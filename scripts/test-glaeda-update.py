#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-update and scripts/glaeda_release.py (over-the-air updates)."""

from __future__ import annotations

import contextlib
import datetime as dt
import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import glaeda_release as gr  # noqa: E402

loader = importlib.machinery.SourceFileLoader("glaeda_update", os.fspath(ROOT / "scripts" / "glaeda-update"))
spec = importlib.util.spec_from_loader("glaeda_update", loader)
gu = importlib.util.module_from_spec(spec)
loader.exec_module(gu)

TARGET = "aarch64-apple-darwin"
SOURCES = ["a" * 40, "b" * 40, "c" * 40]

# A stand-in for a release's glaeda-mini-setup: installs three tools into $GLAEDA_TEST_BIN that exit
# with the release's health code, and records each run.
FAKE_SETUP = """#!/usr/bin/env python3
import os, sys
from pathlib import Path
bin_dir = Path(os.environ["GLAEDA_TEST_BIN"])
bin_dir.mkdir(parents=True, exist_ok=True)
code = int(Path(__file__).with_name("health-code").read_text())
for name in ("glaeda-disk", "glaeda-worktree-reclaim", "glaeda-update"):
    tool = bin_dir / name
    tool.write_text("#!/usr/bin/env python3\\nimport sys\\nprint('{\\\"result\\\": \\\"plan\\\"}')\\nsys.exit(%d)\\n" % code)
    tool.chmod(0o755)
with open(bin_dir / "setup-runs", "a") as log:
    log.write(Path(__file__).parents[2].name + " " + " ".join(sys.argv[1:]) + " " + os.environ.get("GLAEDA_UPDATE_RUNNING", "") + "\\n")
"""


def tar_gz(files: dict[str, tuple[bytes, int]]) -> bytes:
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w:gz") as tar:
        for name, (data, mode) in files.items():
            info = tarfile.TarInfo(name)
            info.size, info.mode = len(data), mode
            tar.addfile(info, io.BytesIO(data))
    return out.getvalue()


class Server:
    """Serves the release files glaeda-update fetches, keyed by URL."""

    def __init__(self) -> None:
        self.files: dict[str, bytes] = {}
        self.paused = False
        self.rings: dict[str, dict] = {}

    def publish(self, source: str, health_code: int = 0, archive: bytes | None = None) -> dict:
        tag = gr.release_tag(source, dt.datetime(2026, 9, 25))
        archive = archive or tar_gz({
            "glaeda/scripts/glaeda-mini-setup": (FAKE_SETUP.encode(), 0o755),
            "glaeda/scripts/health-code": (str(health_code).encode(), 0o644),
            "bin/glaeda-worktree-reclaim": (b"binary", 0o755),
        })
        name = gr.hygiene_asset(TARGET)
        release_raw = gr.canonical(gr.release_manifest(source, tag, {name: archive}))
        self.files[gu.DOWNLOAD.format(tag=tag, name=name)] = archive
        self.files[gu.DOWNLOAD.format(tag=tag, name="release.json")] = release_raw
        entry = gr.channel_entry("canary", json.loads(release_raw), release_raw, "2026-09-25T00:00:00Z")
        self.rings["canary"] = entry
        self.rings["stable"] = {**entry, "ring": "stable"}
        return entry

    def channel(self, ring: str) -> tuple[bytes, bytes | None]:
        entry = self.rings.get(ring)
        return gr.canonical(gr.control(self.paused)), gr.canonical(entry) if entry else None

    def fetch(self, url: str, limit: int) -> bytes:
        return self.files[url]


class UpdateTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.state = self.root / "state"
        self.server = Server()
        self.saved = (gu.fetch, gu.attestation_ok, gu.local_target, os.environ.get("GLAEDA_TEST_BIN"))
        gu.fetch = self.server.fetch
        gu.attestation_ok = lambda path: True
        gu.local_target = lambda: TARGET
        os.environ["GLAEDA_TEST_BIN"] = str(self.bin)
        self.config = {"ring": "canary", "setupArgs": ["--hygiene-only"], "host": "test", "reportStatus": False}

    def tearDown(self) -> None:
        gu.fetch, gu.attestation_ok, gu.local_target, test_bin = self.saved
        if test_bin is None:
            os.environ.pop("GLAEDA_TEST_BIN", None)
        self.tmp.cleanup()

    def run_update(self, apply: bool = True) -> dict:
        return gu.update(apply, self.config, self.state, self.server.channel(self.config["ring"]), self.bin)

    def test_plan_changes_nothing_then_apply_installs_and_is_idempotent(self) -> None:
        entry = self.server.publish(SOURCES[0])
        plan = self.run_update(apply=False)
        self.assertEqual(plan["result"], "plan")
        self.assertFalse(self.bin.exists())
        done = self.run_update()
        self.assertEqual(done["result"], "updated", done)
        self.assertEqual(gu.load_state(self.state)["current"], entry["tag"])
        runs = (self.bin / "setup-runs").read_text().split()
        self.assertEqual(runs[0], entry["tag"])  # ran the release's own setup
        self.assertIn("--hygiene-only", runs)
        self.assertIn("--reclaim-binary", runs)
        self.assertEqual(runs[-1], "1")  # setup knows glaeda-update is running it
        self.assertEqual(self.run_update()["result"], "current")

    def test_unhealthy_release_rolls_back_and_is_quarantined(self) -> None:
        good = self.server.publish(SOURCES[0])
        self.run_update()
        bad = self.server.publish(SOURCES[1], health_code=1)
        result = self.run_update()
        self.assertEqual(result["result"], "rolled-back")
        self.assertEqual(result["rollback"], f"restored {good['tag']}")
        state = gu.load_state(self.state)
        self.assertEqual(state["current"], good["tag"])
        self.assertIn(bad["tag"], state["quarantined"])
        # the restored tools are the good release's again
        self.assertEqual(subprocess.run([sys.executable, str(self.bin / "glaeda-disk")],
                                        capture_output=True).returncode, 0)
        self.assertEqual(self.run_update()["result"], "quarantined")
        # a newer release is still tried
        newer = self.server.publish(SOURCES[2])
        self.assertEqual(self.run_update()["result"], "updated")
        state = gu.load_state(self.state)
        self.assertEqual((state["current"], state["previous"]), (newer["tag"], good["tag"]))
        kept = sorted(p.name for p in (self.state / "generations").iterdir())
        self.assertEqual(kept, sorted([newer["tag"], good["tag"]]))

    def test_tampered_bytes_are_refused(self) -> None:
        entry = self.server.publish(SOURCES[0])
        url = gu.DOWNLOAD.format(tag=entry["tag"], name=gr.hygiene_asset(TARGET))
        self.server.files[url] += b"x"
        with self.assertRaisesRegex(gu.UpdateError, "does not match release.json"):
            self.run_update()
        self.server.publish(SOURCES[0])
        self.server.rings["canary"]["releaseSha256"] = "0" * 64
        with self.assertRaisesRegex(gu.UpdateError, "does not match the channel"):
            self.run_update()
        self.assertFalse(self.bin.exists())

    def test_failed_attestation_is_refused(self) -> None:
        self.server.publish(SOURCES[0])
        gu.attestation_ok = lambda path: False
        with self.assertRaisesRegex(gu.UpdateError, "did not verify"):
            self.run_update()
        # a canary that cannot check refuses; stable, which only names canary-verified releases, installs
        gu.attestation_ok = lambda path: None
        with self.assertRaisesRegex(gu.UpdateError, "canary must verify"):
            self.run_update()
        self.assertFalse(self.bin.exists())
        self.config["ring"] = "stable"
        self.assertEqual(self.run_update()["result"], "updated")

    def test_first_update_rolls_back_to_the_tools_it_replaced(self) -> None:
        self.bin.mkdir()
        (self.bin / "glaeda-disk").write_text("hand-installed\n")
        self.server.publish(SOURCES[0], health_code=1)
        result = self.run_update()
        self.assertEqual(result["result"], "rolled-back")
        self.assertEqual(result["rollback"], "restored the tools installed before the first update")
        self.assertEqual((self.bin / "glaeda-disk").read_text(), "hand-installed\n")

    def test_malformed_channel_names_are_refused(self) -> None:
        entry = self.server.publish(SOURCES[0])
        for tag in ("../../../../cli/cli/releases/download/v2.0.0", "r-20260925-ABC", "latest"):
            with self.subTest(tag=tag):
                self.server.rings["canary"] = {**entry, "tag": tag}
                with self.assertRaisesRegex(gu.UpdateError, "malformed"):
                    self.run_update()
        self.server.rings["canary"] = {**entry, "ring": "stable"}
        with self.assertRaisesRegex(gu.UpdateError, "malformed"):
            self.run_update()

    def test_bad_state_stops_the_run(self) -> None:
        self.server.publish(SOURCES[0])
        self.state.mkdir()
        for text in ("{not json", json.dumps({"current": "../x", "previous": None, "quarantined": []}), "[]"):
            (self.state / "state.json").write_text(text)
            with self.assertRaises(gu.UpdateError):
                self.run_update()

    def test_unsafe_archive_entries_are_refused(self) -> None:
        for name in ("../escape", "/abs/path"):
            with self.subTest(name=name):
                self.server.publish(SOURCES[0], archive=tar_gz({name: (b"x", 0o644)}))
                with self.assertRaisesRegex(gu.UpdateError, "refused"):
                    self.run_update()
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode="w:gz") as tar:
            link = tarfile.TarInfo("glaeda/link")
            link.type, link.linkname = tarfile.SYMTYPE, "/etc/passwd"
            tar.addfile(link)
        self.server.publish(SOURCES[0], archive=out.getvalue())
        with self.assertRaisesRegex(gu.UpdateError, "refused"):
            self.run_update()

    def test_paused_and_empty_rings_do_nothing(self) -> None:
        self.server.publish(SOURCES[0])
        self.server.paused = True
        self.assertEqual(self.run_update()["result"], "idle")
        self.server.paused = False
        del self.server.rings["stable"]
        self.config["ring"] = "stable"
        self.assertEqual(self.run_update()["result"], "idle")
        self.assertFalse(self.bin.exists())

    def test_config_defaults_and_refusals(self) -> None:
        path = self.root / "update.json"
        self.assertEqual(gu.load_config(path)["ring"], "stable")
        path.write_text(json.dumps({"ring": "canary", "host": "air-blue"}))
        config = gu.load_config(path)
        self.assertTrue(config["reportStatus"])
        self.assertEqual(config["setupArgs"], ["--hygiene-only"])
        for bad in ({"ring": "beta"}, {"setupArgs": ["--uninstall"]}, {"setupArgs": ["rm"]}):
            path.write_text(json.dumps(bad))
            with self.assertRaises(gu.UpdateError):
                gu.load_config(path)


class ReleaseTest(unittest.TestCase):
    NOW = dt.datetime(2026, 9, 25, 12, tzinfo=dt.timezone.utc)

    def canary(self, published: str = "2026-09-25T00:00:00Z") -> dict:
        return {"schema": gr.CHANNEL_SCHEMA, "ring": "canary", "tag": "r-20260925-" + "a" * 12,
                "source": "a" * 40, "releaseSha256": "0" * 64, "published": published}

    def status(self, host: str, state: str, at: str) -> dict:
        return {"context": gr.STATUS_PREFIX + host, "state": state, "updated_at": at}

    def test_promotion_needs_soak_success_and_no_failure(self) -> None:
        ok = [self.status("air-blue", "success", "2026-09-25T01:00:00Z")]
        self.assertFalse(gr.promotion(self.canary("2026-09-25T10:00:00Z"), None, ok, self.NOW)[0])
        self.assertEqual(gr.promotion(self.canary(), None, [], self.NOW),
                         (False, "no canary host reported success"))
        failed = ok + [self.status("big-red", "failure", "2026-09-25T02:00:00Z")]
        self.assertFalse(gr.promotion(self.canary(), None, failed, self.NOW)[0])
        # a host's newest status counts: a later success clears its earlier failure
        recovered = failed + [self.status("big-red", "success", "2026-09-25T03:00:00Z")]
        self.assertTrue(gr.promotion(self.canary(), None, recovered, self.NOW)[0])
        # unrelated contexts are ignored
        noise = ok + [{"context": "ci/verify", "state": "failure", "updated_at": "2026-09-25T04:00:00Z"}]
        self.assertTrue(gr.promotion(self.canary(), None, noise, self.NOW)[0])
        self.assertFalse(gr.promotion(self.canary(), {**self.canary(), "ring": "stable"}, ok, self.NOW)[0])
        self.assertEqual(gr.promotion(None, None, ok, self.NOW), (False, "no canary release"))

    def test_cli_promote_writes_stable_or_exits_3(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            canary, statuses = Path(tmp) / "canary.json", Path(tmp) / "statuses.json"
            canary.write_bytes(gr.canonical(self.canary()))
            statuses.write_text(json.dumps([]))
            args = ["promote", "--canary", str(canary), "--statuses", str(statuses), "--soak-hours", "0"]
            out = io.TextIOWrapper(io.BytesIO(), encoding="utf-8")
            with open(os.devnull, "w") as null, contextlib.redirect_stderr(null), contextlib.redirect_stdout(out):
                self.assertEqual(gr.main(args), 3)
                statuses.write_text(json.dumps([self.status("air-blue", "success", "2026-09-25T01:00:00Z")]))
                self.assertEqual(gr.main(args), 0)
            out.flush()
            stable = gr.parse_channel(out.buffer.getvalue(), "stable")
            self.assertEqual(stable["tag"], self.canary()["tag"])
            self.assertIn("promoted", stable)

    def test_manifest_and_channel_round_trip(self) -> None:
        release_raw = gr.canonical(gr.release_manifest("a" * 40, "r-20260925-" + "a" * 12, {"x.tar.gz": b"data"}))
        doc = gr.parse_release(release_raw, gr.sha256(release_raw))
        entry = gr.channel_entry("canary", doc, release_raw, "2026-09-25T00:00:00Z")
        self.assertEqual(gr.parse_channel(gr.canonical(entry), "canary"), entry)
        self.assertEqual(gu.resolve(gr.canonical(gr.control(False)), gr.canonical(entry), "canary"), entry)
        self.assertIsNone(gu.resolve(gr.canonical(gr.control(True)), gr.canonical(entry), "canary"))
        self.assertEqual(gu.check_release(release_raw, entry)["tag"], entry["tag"])
        with self.assertRaises(gr.ReleaseError):
            gr.parse_release(release_raw, "0" * 64)

    def test_hygiene_archive_holds_the_tree_and_the_binary(self) -> None:
        head = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True,
                              text=True, check=True).stdout.strip()
        with tempfile.TemporaryDirectory() as tmp:
            reclaim = Path(tmp) / "reclaim"
            reclaim.write_bytes(b"\x7fELF")
            raw = gr.hygiene_archive(ROOT, head, reclaim)
            self.assertEqual(raw, gr.hygiene_archive(ROOT, head, reclaim))  # reproducible bytes
            with tarfile.open(fileobj=io.BytesIO(raw)) as tar:
                names = set(tar.getnames())
                setup = tar.getmember("glaeda/scripts/glaeda-mini-setup")
                self.assertTrue(setup.mode & 0o111)
                self.assertEqual(tar.extractfile("bin/glaeda-worktree-reclaim").read(), b"\x7fELF")
            self.assertIn("glaeda/scripts/glaeda-update", names)
            self.assertIn("glaeda/ops/systemd/glaeda-update.timer", names)
            # glaeda-update's own extraction accepts it
            archive = Path(tmp) / "a.tar.gz"
            archive.write_bytes(raw)
            gu.safe_extract(archive, Path(tmp) / "out")
            self.assertTrue((Path(tmp) / "out/glaeda/scripts/glaeda_release.py").is_file())


if __name__ == "__main__":
    unittest.main()
