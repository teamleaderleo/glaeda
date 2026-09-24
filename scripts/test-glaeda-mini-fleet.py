#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-mini-fleet. Runs on Linux CI: SSH and observation are stubbed."""

from __future__ import annotations

import contextlib
import copy
import importlib.machinery
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_mini_fleet", os.fspath(ROOT / "scripts" / "glaeda-mini-fleet"))
spec = importlib.util.spec_from_loader("glaeda_mini_fleet", loader)
mf = importlib.util.module_from_spec(spec)
sys.modules["glaeda_mini_fleet"] = mf
loader.exec_module(mf)

EXAMPLE = ROOT / "examples" / "mini-fleet" / "manifest.example.json"
FP = {
    "coordinator": "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    "operator": "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
    "service-cache": "SHA256:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
}
STRAY = "SHA256:DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD"


def probe_text(hostname: str = "build-mini-1", keys: tuple[str, ...] = ("coordinator", "operator"),
               extra_keys: tuple[str, ...] = (), clone: bool = True, license_: str = "26.3",
               worker: str = "running pid=10") -> str:
    lines = [
        "user\tbuilder", "uid\t501", "admin_group\tyes", f"hostname\t{hostname}", "computer_name\tMini",
        "model\tMac16,11", "chip\tApple M4 Pro", "cpus\t14", f"memory_bytes\t{48 * 2**30}",
        "macos\t26.5.1", "macos_build\t25F80",
        "xcode_app\t/Applications/Xcode.app|dir|26.3|17C529",
        "xcode_select\t/Applications/Xcode.app/Contents/Developer",
        f"xcode_license_accepted\t{license_}",
        "disk\t/System/Volumes/Data|460|220",
        f"launchd\t/Library/LaunchDaemons|com.example.build-worker|{worker}",
        "fleet_root\tpresent", "fleet_worker_proc\trunning",
        "ak_file\tauthorized_keys|600",
    ]
    if clone:
        lines.append("xcode_app\t/Applications/Xcode_26.3.app|dir|26.3|17C529")
    for key in keys:
        lines.append(f"ak_key\tauthorized_keys|no|256 {FP[key]} {key} comment (ED25519)")
    for fp in extra_keys:
        lines.append(f"ak_key\tauthorized_keys|no|256 {fp} someone@laptop (ED25519)")
    return "\n".join(lines) + "\n"


def observed(**hosts: str) -> dict:
    return {"hosts": {name: {"host": name, "reachable": True, **mf.parse_probe(text)} for name, text in hosts.items()}}


class ManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def test_example_loads(self) -> None:
        self.assertEqual(set(self.manifest["hosts"]), {"build-mini-1", "build-mini-2", "small-mini"})

    def test_never_touch_host_is_refused(self) -> None:
        with self.assertRaisesRegex(mf.Failure, "never_touch"):
            mf.select_hosts(self.manifest, ["coordinator-mini"])

    def test_never_touch_cannot_also_be_a_host(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["coordinator-mini"] = {"class": "m4-16"}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "also in never_touch"):
                mf.load_manifest(path)

    def test_never_touch_matches_case_and_local_suffix(self) -> None:
        for name in ("Coordinator-Mini", "coordinator-mini.local"):
            with self.assertRaisesRegex(mf.Failure, "never_touch"):
                mf.select_hosts(self.manifest, [name])
        data = copy.deepcopy(self.manifest)
        data["hosts"]["alias"] = {"hostname": "Coordinator-Mini.local", "class": "m4-16"}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "also in never_touch"):
                mf.load_manifest(path)

    def test_public_key_must_be_one_bare_line(self) -> None:
        for bad in ("ssh-ed25519 AAAA a\nssh-ed25519 BBBB b", 'command="x" ssh-ed25519 AAAA a'):
            data = copy.deepcopy(self.manifest)
            data["keys"]["coordinator"]["public_key"] = bad
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "m.json"
                path.write_text(json.dumps(data))
                with self.assertRaisesRegex(mf.Failure, "one bare key line"):
                    mf.load_manifest(path)

    def test_bad_observation_exits_2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            obs = Path(tmp) / "obs.json"
            for text in ("not json", "{}", json.dumps({"hosts": {"build-mini-1": {"reachable": True}}})):
                obs.write_text(text)
                with contextlib.redirect_stderr(io.StringIO()):
                    code = mf.main(["check", "--manifest", os.fspath(EXAMPLE), "--host", "build-mini-1",
                                    "--observed", os.fspath(obs)])
                self.assertEqual(code, 2, text)

    def test_unknown_key_reference_is_refused(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["defaults"]["authorized_keys"]["allow"].append("ghost")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "unknown key 'ghost'"):
                mf.load_manifest(path)

    def test_overrides_merge_over_defaults(self) -> None:
        policy = mf.host_policy(self.manifest, "small-mini")
        self.assertEqual(policy["disk"]["min_free_gib"], 40)
        self.assertEqual(policy["macos"]["major"], 26)


class CheckTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def issues(self, text: str, host: str = "build-mini-1") -> list[dict]:
        return mf.check(self.manifest, observed(**{host: text}), [host])

    def test_conforming_host_has_only_pending(self) -> None:
        issues = self.issues(probe_text())
        self.assertEqual([i["area"] for i in issues], ["pending"])

    def test_missing_required_key_is_drift_with_add_action(self) -> None:
        issues = self.issues(probe_text(keys=("operator",)))
        add = [i for i in issues if i.get("action", {}).get("kind") == "add_key"]
        self.assertEqual(add[0]["action"]["key"], "coordinator")

    def test_unlisted_key_is_drift_but_never_an_action(self) -> None:
        issues = self.issues(probe_text(extra_keys=(STRAY,)))
        stray = [i for i in issues if "not allowed" in i["detail"]]
        self.assertEqual(len(stray), 1)
        self.assertNotIn("action", stray[0])

    def test_allowed_review_key_is_not_drift(self) -> None:
        issues = self.issues(probe_text(keys=("coordinator", "operator", "service-cache")))
        self.assertFalse([i for i in issues if i["area"] == "ssh"])

    def test_missing_clone_carries_exact_source_identity(self) -> None:
        issues = self.issues(probe_text(clone=False))
        action = next(i["action"] for i in issues if i.get("action", {}).get("kind") == "clone_xcode")
        self.assertEqual(action, {"kind": "clone_xcode", "path": "/Applications/Xcode_26.3.app",
                                  "source": "/Applications/Xcode.app", "version": "26.3", "build": "17C529"})

    def test_newer_licence_covers_older_xcode(self) -> None:
        self.assertFalse([i for i in self.issues(probe_text(license_="26.5")) if "licence" in i["detail"]])
        self.assertTrue([i for i in self.issues(probe_text(license_="")) if "licence" in i["detail"]])

    def test_hostname_and_launchd_drift(self) -> None:
        issues = self.issues(probe_text(hostname="other", worker="not-loaded"))
        self.assertTrue(any(i["area"] == "identity" for i in issues))
        self.assertTrue(any(i["area"] == "launchd" and "not-loaded" in i["detail"] for i in issues))

    def test_key_in_authorized_keys2_is_live(self) -> None:
        text = probe_text() + f"ak_key\tauthorized_keys2|no|256 {STRAY} hidden (ED25519)\n"
        details = [i["detail"] for i in self.issues(text)]
        self.assertTrue(any("not allowed" in d for d in details))
        self.assertTrue(any("authorized_keys2" in d for d in details))

    def test_backup_copies_are_not_live(self) -> None:
        text = probe_text() + f"ak_key\tauthorized_keys.bak|no|256 {STRAY} old (ED25519)\n"
        self.assertFalse([i for i in self.issues(text) if "not allowed" in i["detail"]])

    def test_symlinked_xcode_is_drift(self) -> None:
        text = probe_text(clone=False) + "xcode_app\t/Applications/Xcode_26.3.app|symlink:Xcode.app|26.3|17C529\n"
        self.assertTrue([i for i in self.issues(text) if "real directory" in i["detail"]])

    def test_versions_compare_padded(self) -> None:
        self.assertEqual(mf.version_tuple("26.3"), mf.version_tuple("26.3.0"))
        self.assertLess(mf.version_tuple("26.4"), mf.version_tuple("26.5"))
        self.assertEqual(mf.version_tuple(""), (0, 0, 0))

    def test_unreachable_host_is_one_finding(self) -> None:
        issues = mf.check(self.manifest, {"hosts": {"build-mini-1": {"reachable": False, "error": "timeout"}}},
                          ["build-mini-1"])
        self.assertEqual([(i["area"], i["detail"]) for i in issues], [("reach", "timeout")])

    def test_exit_code_ignores_pending(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            obs = Path(tmp) / "obs.json"
            obs.write_text(json.dumps(observed(**{"build-mini-1": probe_text()})))
            with contextlib.redirect_stdout(io.StringIO()) as out:
                code = mf.main(["check", "--manifest", os.fspath(EXAMPLE), "--host", "build-mini-1",
                                "--observed", os.fspath(obs)])
        self.assertEqual(code, 0, out.getvalue())
        self.assertIn("ok (1 pending)", out.getvalue())


class ApplyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def run_apply(self, hosts: list[str], yes: bool, texts: dict[str, str]) -> tuple[int, str, list]:
        calls: list = []
        with mock.patch.object(mf, "observe", lambda manifest, names: observed(**{n: texts[n] for n in names})), \
                mock.patch.object(mf, "remote", lambda *a: calls.append(a) or (0, "done")), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            code = mf.apply(self.manifest, hosts, yes)
        return code, out.getvalue(), calls

    def test_dry_run_runs_nothing(self) -> None:
        code, out, calls = self.run_apply(["build-mini-1"], False, {"build-mini-1": probe_text(keys=("operator",), clone=False)})
        self.assertEqual((code, calls), (0, []))
        self.assertIn("would add key coordinator", out)
        self.assertIn("would clone", out)

    def test_apply_false_host_is_skipped(self) -> None:
        code, out, calls = self.run_apply(["small-mini"], True, {"small-mini": probe_text(hostname="small-mini", clone=False)})
        self.assertEqual(calls, [])
        self.assertIn("skipped", out)

    def test_add_key_sends_public_key_and_fingerprint(self) -> None:
        text = probe_text(keys=("operator",))
        _code, _out, calls = self.run_apply(["build-mini-1"], True, {"build-mini-1": text})
        (_name, _user, script, args, stdin), = calls
        self.assertIs(script, mf.ADD_KEY)
        self.assertEqual(args, [FP["coordinator"]])
        self.assertTrue(stdin.startswith("ssh-ed25519 "))

    def test_key_without_public_key_is_not_added(self) -> None:
        self.manifest["keys"]["coordinator"].pop("public_key")
        code, out, calls = self.run_apply(["build-mini-1"], True, {"build-mini-1": probe_text(keys=("operator",))})
        self.assertEqual(calls, [])
        self.assertEqual(code, 1)
        self.assertIn("no public_key", out)

    def test_unreachable_host_fails_apply(self) -> None:
        with mock.patch.object(mf, "observe", lambda m, n: {"hosts": {x: {"reachable": False, "error": "down"} for x in n}}), \
                mock.patch.object(mf, "remote") as remote, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(mf.apply(self.manifest, ["build-mini-1"], True), 1)
        remote.assert_not_called()

    def test_unverified_effect_fails_apply(self) -> None:
        texts = iter([observed(**{"build-mini-1": probe_text(keys=("operator",))}),
                      {"hosts": {"build-mini-1": {"reachable": False, "error": "gone"}}}])
        with mock.patch.object(mf, "observe", lambda m, n: next(texts)), \
                mock.patch.object(mf, "remote", return_value=(0, "added")), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(mf.apply(self.manifest, ["build-mini-1"], True), 1)

    def test_stray_keys_are_never_removed(self) -> None:
        _code, _out, calls = self.run_apply(["build-mini-1"], True, {"build-mini-1": probe_text(extra_keys=(STRAY,))})
        self.assertEqual(calls, [])


@unittest.skipUnless(shutil.which("ssh-keygen") and shutil.which("bash"), "needs ssh-keygen and bash")
class AddKeyScriptTests(unittest.TestCase):
    def test_append_is_idempotent_backed_up_and_fingerprint_bound(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / ".ssh").mkdir()
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "old", "-f", os.fspath(home / "old")], check=True)
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "new", "-f", os.fspath(home / "new")], check=True)
            ak = home / ".ssh" / "authorized_keys"
            ak.write_text((home / "old.pub").read_text().rstrip("\n"))  # no trailing newline
            ak.chmod(0o600)
            new = (home / "new.pub").read_text()
            fp = subprocess.run(["ssh-keygen", "-lf", os.fspath(home / "new.pub")], capture_output=True, text=True).stdout.split()[1]

            def run(fingerprint: str) -> subprocess.CompletedProcess:
                return subprocess.run(["bash", "-c", mf.ADD_KEY, "glaeda", fingerprint], input=new, text=True,
                                      capture_output=True, env={"HOME": tmp, "PATH": os.environ["PATH"]})

            self.assertEqual(run("SHA256:wrong").returncode, 3)
            first = run(fp)
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertIn("added", first.stdout)
            self.assertIn("unchanged", run(fp).stdout)
            self.assertEqual(len(ak.read_text().splitlines()), 2)
            self.assertEqual(ak.stat().st_mode & 0o777, 0o600)
            self.assertEqual(sorted(p.name for p in (home / ".ssh").iterdir()), ["authorized_keys"])
            self.assertEqual(len(list((home / ".local/state/glaeda/mini-fleet").glob("authorized_keys.*"))), 1)

    def test_missing_file_is_created_private(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "new", "-f", os.fspath(home / "new")], check=True)
            fp = subprocess.run(["ssh-keygen", "-lf", os.fspath(home / "new.pub")], capture_output=True, text=True).stdout.split()[1]
            proc = subprocess.run(["bash", "-c", mf.ADD_KEY, "glaeda", fp], input=(home / "new.pub").read_text(), text=True,
                                  capture_output=True, env={"HOME": tmp, "PATH": os.environ["PATH"]})
            self.assertEqual(proc.returncode, 0, proc.stderr)
            ak = home / ".ssh" / "authorized_keys"
            self.assertEqual(len(ak.read_text().splitlines()), 1)
            self.assertEqual((ak.stat().st_mode & 0o777, (home / ".ssh").stat().st_mode & 0o777), (0o600, 0o700))


@unittest.skipUnless(sys.platform == "darwin", "cp -c, plutil and stat -f are macOS")
class CloneScriptTests(unittest.TestCase):
    def fake_app(self, root: Path, version: str, build: str) -> Path:
        app = root / "X.app" / "Contents"
        app.mkdir(parents=True)
        subprocess.run(["plutil", "-create", "xml1", os.fspath(app / "Info.plist")], check=True)
        subprocess.run(["plutil", "-insert", "CFBundleShortVersionString", "-string", version, os.fspath(app / "Info.plist")], check=True)
        subprocess.run(["plutil", "-create", "xml1", os.fspath(app / "version.plist")], check=True)
        subprocess.run(["plutil", "-insert", "ProductBuildVersion", "-string", build, os.fspath(app / "version.plist")], check=True)
        return root / "X.app"

    def run_clone(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(["bash", "-c", mf.CLONE_XCODE, "glaeda", *args], capture_output=True, text=True)

    def test_clone_matches_identity_and_is_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            src = self.fake_app(Path(tmp), "26.3", "17C529")
            dst = os.fspath(Path(tmp) / "Y.app")
            self.assertEqual(self.run_clone(os.fspath(src), dst, "26.6", "17F113").returncode, 3)
            first = self.run_clone(os.fspath(src), dst, "26.3", "17C529")
            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertTrue(Path(dst).is_dir() and not Path(dst).is_symlink())
            self.assertIn("unchanged", self.run_clone(os.fspath(src), dst, "26.3", "17C529").stdout)

    def test_dangling_symlink_destination_is_left_alone(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            src = self.fake_app(Path(tmp), "26.3", "17C529")
            dst = Path(tmp) / "Y.app"
            dst.symlink_to(Path(tmp) / "gone")
            self.assertIn("unchanged", self.run_clone(os.fspath(src), os.fspath(dst), "26.3", "17C529").stdout)
            self.assertFalse((Path(tmp) / "Y.app.glaeda-partial").exists())


class ProbeScriptTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("ssh-keygen"), "needs ssh-keygen")
    def test_probe_reports_every_live_key_including_an_unterminated_last_line(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / ".ssh").mkdir()
            pubs = []
            for name in ("a", "b", "c"):
                subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", name, "-f", os.fspath(home / name)], check=True)
                pubs.append((home / f"{name}.pub").read_text().strip())
            (home / ".ssh" / "authorized_keys").write_text(pubs[0] + "\n" + pubs[1])  # no trailing newline
            (home / ".ssh" / "authorized_keys2").write_text(pubs[2] + "\n")
            out = subprocess.run(["bash", os.fspath(ROOT / "scripts" / "cmux_mini_probe.sh")], capture_output=True,
                                 text=True, env={"HOME": tmp, "PATH": "/usr/bin:/bin"}).stdout
            parsed = mf.parse_probe(out)
            self.assertEqual(sorted((k["file"], k["comment"]) for k in parsed["authorized_keys"]),
                             [("authorized_keys", "a"), ("authorized_keys", "b"), ("authorized_keys2", "c")])
            self.assertNotIn("AAAA", out)  # fingerprints only, never key material

    def test_probe_parses_as_bash(self) -> None:
        subprocess.run(["bash", "-n", os.fspath(ROOT / "scripts" / "cmux_mini_probe.sh")], check=True)

    def test_probe_never_reads_key_material_or_tokens(self) -> None:
        text = (ROOT / "scripts" / "cmux_mini_probe.sh").read_text()
        self.assertNotIn("secrets/", text)
        self.assertNotIn("sudo", text.replace("no sudo", ""))


if __name__ == "__main__":
    unittest.main()
