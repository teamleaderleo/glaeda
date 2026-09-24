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
               worker: str = "running pid=10", node_id: str | None = "cmux-mac-001") -> str:
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
    if node_id is not None:
        lines.append(f"fleet_node_id\t{node_id}")
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

    def test_unknown_role_is_refused(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["build-mini-1"]["roles"] = ["dev-builds", "coffee"]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "unknown roles"):
                mf.load_manifest(path)

    def test_unknown_key_reference_is_refused(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["defaults"]["authorized_keys"]["allow"].append("ghost")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "m.json"
            path.write_text(json.dumps(data))
            with self.assertRaisesRegex(mf.Failure, "unknown key 'ghost'"):
                mf.load_manifest(path)

    def test_node_ids_must_be_opaque_and_unique(self) -> None:
        for host, value, error in (("build-mini-1", "mac-3", "not an opaque cmux-"),
                                   ("build-mini-1", 3, "not an opaque cmux-"),
                                   ("build-mini-2", "cmux-mac-001", "share node_id cmux-mac-001")):
            data = copy.deepcopy(self.manifest)
            data["hosts"][host]["node_id"] = value
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "m.json"
                path.write_text(json.dumps(data))
                with self.assertRaisesRegex(mf.Failure, error):
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

    def test_unenrolled_host_is_pending_with_its_assigned_node_id(self) -> None:
        issues = [i for i in self.issues(probe_text(node_id=None)) if "enrolled" in i["detail"]]
        self.assertEqual([i["area"] for i in issues], ["pending"])
        self.assertIn("--node-id cmux-mac-001", issues[0]["fix"])

    def test_enrolled_under_another_node_id_is_drift(self) -> None:
        issues = [i for i in self.issues(probe_text(node_id="cmux-mac-009")) if i["area"] == "enrollment"]
        self.assertEqual(len(issues), 1)
        self.assertIn("enrolled as cmux-mac-009, manifest assigns cmux-mac-001", issues[0]["detail"])

    def test_host_without_node_id_is_not_checked_for_enrollment(self) -> None:
        issues = self.issues(probe_text(node_id=None), host="small-mini")
        self.assertFalse([i for i in issues if "enrolled" in i["detail"]])

    def test_probe_reads_the_enrollment_node_id_without_python(self) -> None:
        probe = mf.PROBE.read_text()
        self.assertIn("plutil -extract nodeId raw", probe)
        self.assertIn("glaeda/cmux-fleet/enrollment.json", probe)

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

    def test_licence_and_first_launch_come_from_xcodebuild(self) -> None:
        base = probe_text(license_="26.0")
        ok = base + "xcode_ready\t/Applications/Xcode.app|accepted|done\n" \
            + "xcode_ready\t/Applications/Xcode_26.3.app|accepted|done\n"
        self.assertFalse([i for i in self.issues(ok) if "licence" in i["detail"] or "first launch" in i["detail"]])
        bad = base + "xcode_ready\t/Applications/Xcode.app|accepted|done\n" \
            + "xcode_ready\t/Applications/Xcode_26.3.app|needed|needed\n"
        details = [i["detail"] for i in self.issues(bad)]
        self.assertIn("/Applications/Xcode_26.3.app licence not accepted", details)
        self.assertIn("/Applications/Xcode_26.3.app first launch not run", details)
        partial = base + "xcode_ready\t/Applications/Xcode.app|accepted|done\n"
        self.assertTrue([i for i in self.issues(partial) if "no runnable xcodebuild" in i["detail"]])
        self.assertEqual(mf.parse_probe("xcode_ready\t/Applications/X|y.app|accepted|done\n")["xcode_ready"],
                         {"/Applications/X|y.app": {"licence": "accepted", "first_launch": "done"}})

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

    def test_sudo_and_token_drift(self) -> None:
        self.manifest["hosts"]["build-mini-1"]["sudo"] = "password"
        self.manifest["defaults"]["controller_token"] = "present"
        text = probe_text() + "sudo\tnopasswd\ncontroller_token\tmissing\n"
        areas = {i["area"] for i in self.issues(text)}
        self.assertTrue({"sudo", "worker"} <= areas)

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

    def test_probe_never_reads_tokens_or_runs_privileged_commands(self) -> None:
        text = (ROOT / "scripts" / "cmux_mini_probe.sh").read_text()
        secret_lines = [line.strip() for line in text.splitlines() if "secrets/" in line and not line.strip().startswith("#")]
        self.assertEqual(secret_lines, ['[ -f "$F/secrets/controller.token" ] && e controller_token present || e controller_token missing'])
        sudo_lines = [line.strip() for line in text.splitlines() if "sudo" in line and not line.strip().startswith("#")]
        self.assertEqual(sudo_lines, ["if sudo -n -l >/dev/null 2>&1; then e sudo nopasswd; else e sudo password; fi"])



READY_XCODE = ("xcode_ready\t/Applications/Xcode.app|accepted|done\n"
               "xcode_ready\t/Applications/Xcode_26.3.app|accepted|done\n")


def preflight_text(user: str = "builder", brew_owner: str | None = "builder", zig: str = "0.16.0",
                   zig_path: str = "/opt/homebrew/bin/zig", rust: bool = True, metal: bool = True,
                   sdks: str = "ok", python: str = "/opt/homebrew/bin/python3.13|3.13",
                   submodules: tuple[str, ...] = (" a1 ghostty (heads/main)",), artifacts: bool = True,
                   dirty: int = 0, candidate: str | None = "59ca9c9bd1bb|yes|yes", enroll_state: str | None = "eligible",
                   acceptance: str | None = "accepted", update_running: str | None = None, prepared: bool = False,
                   sleep: int = 0, runners: tuple[str, ...] = ("actions-runner-cmux-persistent-compile|mini-1",),
                   **probe: object) -> str:
    """A probe plus preflight section for a host that is ready unless told otherwise."""
    lines = [probe_text(**probe).rstrip("\n").replace("user\tbuilder", f"user\t{user}"), READY_XCODE.rstrip("\n")]
    lines.append(f"pf_sdks\t{sdks}")
    lines.append("pf_metal\t" + ("ok|Apple metal version 32023.883" if metal else
                                 "fail|error: cannot execute tool 'metal' due to missing Metal Toolchain"))
    tools = {"git": ("/usr/bin/git", "git version 2.50.1 (Apple Git-155)"),
             "xcodebuild": ("/usr/bin/xcodebuild", "Xcode 26.3"), "xcrun": ("/usr/bin/xcrun", "xcrun version 72."),
             "zig": (zig_path, zig)}
    if rust:
        tools.update({"cargo": ("/opt/homebrew/bin/cargo", "cargo 1.88.0 (abc 2025-06-23)"),
                      "rustc": ("/opt/homebrew/bin/rustc", "rustc 1.88.0 (abc 2025-06-23)"),
                      "rustup": ("/opt/homebrew/bin/rustup", "rustup 1.29.1 (2026-08-13)")})
    for tool in mf.bootstrap.MACOS_WORKLOAD_TOOLS:
        path, version = tools.get(tool, ("", ""))
        lines.append(f"pf_tool\t{tool}|{path}|{version if path else ''}")
    if rust:
        lines.append("pf_rust_channel\t1.88.0|rustc 1.88.0 (abc 2025-06-23)")
    lines.append(f"pf_python\t{python}")
    lines += ["pf_cmux\tpresent", "pf_cmux_pin\t26", "pf_zig_min\t0.16.0" if submodules[0][0] == " " else "pf_zig_min\t"]
    lines += [f"pf_submodule\t{s}" for s in submodules]
    lines += [f"pf_setup_artifacts\t{'yes' if artifacts else 'no'}", f"pf_cmux_dirty\t{dirty}"]
    if candidate:
        lines.append(f"pf_candidate\t{candidate}")
    lines += ["pf_glaeda\tpresent", "pf_cache_root\tpresent"]
    if enroll_state:
        lines.append(f"pf_enroll_state\t{enroll_state}")
    if acceptance:
        lines.append(f"pf_acceptance\t{acceptance}")
    if brew_owner:
        lines.append(f"pf_brew_owner\t{brew_owner}")
    if update_running:
        lines.append(f"pf_update_running\t{update_running}")
    if prepared:
        lines.append("pf_update_prepared\tyes")
    lines += ["pf_pmset\tAC Power:", f"pf_pmset\t sleep                {sleep}"]
    lines += [f"pf_runner\t{r}" for r in runners]
    return "\n".join(lines) + "\n"


class PreflightTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def result(self, text: str, host: str = "build-mini-1", zig_fallback: str | None = None) -> dict:
        obs = observed(**{host: text})["hosts"][host]
        return mf.preflight_host(self.manifest, host, obs, zig_fallback)

    def states(self, text: str, **kwargs: object) -> dict[str, str]:
        return {k: v["state"] for k, v in self.result(text, **kwargs)["checks"].items()}

    def test_ready_host_passes_every_check(self) -> None:
        result = self.result(preflight_text())
        self.assertTrue(result["ready"], result)
        self.assertEqual({k for k, v in result["checks"].items() if v["state"] not in {"ok", "info"}}, set())
        self.assertEqual(list(result["checks"]), list(mf.PREFLIGHT_CHECKS))

    def test_the_austin_mini_morning_is_found_in_one_pass(self) -> None:
        # 2026-09-24, cmux-austin-mini-1: every one of these showed up only after the previous fix.
        text = preflight_text(user="cmux", brew_owner="admin", zig="0.15.2", zig_path="/usr/local/bin/zig",
                              rust=False, metal=False, python="|3.9", submodules=("-a1 ghostty",),
                              artifacts=False, candidate="59ca9c9bd1bb|no|no", enroll_state=None,
                              acceptance=None, node_id=None)
        text = text.replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                            "xcode_select\t/Applications/Xcode_26.3.app/Contents/Developer")
        result = self.result(text, zig_fallback="0.16.0")
        checks = result["checks"]
        self.assertFalse(result["ready"])
        self.assertEqual({k for k, v in checks.items() if v["state"] == "fail"},
                         {"select", "metal", "rust", "zig", "python", "cmux", "setup"})
        self.assertEqual({k for k, v in checks.items() if v["state"] == "todo"}, {"candidate", "enroll"})
        fixes = mf.grouped_fixes(result)
        self.assertIn("select: sudo xcode-select -s /Applications/Xcode.app", fixes["password"])
        self.assertTrue(any(f.startswith("zig: sudo -u admin brew install zig") for f in fixes["password"]))
        self.assertTrue(any(f.startswith("rust: sudo -u admin brew install rustup") for f in fixes["password"]))
        self.assertTrue(any("-downloadComponent MetalToolchain" in f for f in fixes["self"]))
        self.assertTrue(any("git submodule update --init" in f for f in fixes["self"]))
        self.assertTrue(any("~/.local/bin/python3" in f for f in fixes["self"]))
        self.assertIn("0.15.2 at /usr/local/bin/zig, Ghostty needs 0.16.0", checks["zig"]["detail"])
        self.assertIn("admin", checks["brew"]["detail"])
        # Blockers come before the steps onboarding performs itself.
        self.assertTrue(fixes["self"][-1].startswith("enroll: glaeda-mini-enroll"))
        self.assertIn("--node-id cmux-mac-001", fixes["self"][-1])

    def test_first_launch_is_caught_from_plugin_errors_too(self) -> None:
        checks = self.result(preflight_text(sdks="plugin_error|DVTPlugInLoading: symbol not found"))["checks"]
        self.assertEqual(checks["launch"]["state"], "fail")
        self.assertIn("-runFirstLaunch", checks["launch"]["fix"])
        self.assertEqual(checks["launch"]["group"], "password")
        text = preflight_text().replace("Xcode.app|accepted|done", "Xcode.app|needed|needed")
        checks = self.result(text)["checks"]
        self.assertEqual((checks["licence"]["state"], checks["launch"]["state"]), ("fail", "fail"))

    def test_pending_macos_update_blocks(self) -> None:
        running = self.result(preflight_text(update_running="softwareupdate --install macOS 26.7 --restart"))
        self.assertFalse(running["ready"])
        self.assertEqual(running["checks"]["update"]["group"], "person")
        prepared = self.result(preflight_text(prepared=True))
        self.assertEqual(prepared["checks"]["update"]["state"], "fail")
        self.assertIn("restart", prepared["checks"]["update"]["detail"])

    def test_onboarding_steps_left_do_not_block(self) -> None:
        text = preflight_text(candidate="59ca9c9bd1bb|no|yes", enroll_state=None, acceptance=None, runners=(),
                              node_id=None)
        result = self.result(text)
        self.assertTrue(result["ready"], result)
        self.assertEqual({k for k, v in result["checks"].items() if v["state"] == "todo"},
                         {"candidate", "enroll", "runner"})
        self.assertIn("archive downloaded, not staged", result["checks"]["candidate"]["detail"])

    def test_node_id_is_required_before_enrollment(self) -> None:
        checks = self.result(preflight_text(enroll_state=None, acceptance=None, node_id=None), host="small-mini")["checks"]
        self.assertEqual(checks["node"]["state"], "fail")
        self.assertIn("hosts.small-mini.node_id", checks["node"]["fix"])
        checks = self.result(preflight_text(node_id="cmux-mac-009"))["checks"]
        self.assertIn("enrolled as cmux-mac-009", checks["node"]["detail"])

    def test_power_disk_and_dirty_checkout(self) -> None:
        states = self.states(preflight_text(sleep=10, dirty=3))
        self.assertEqual((states["power"], states["cmux"]), ("fail", "fail"))
        low = preflight_text().replace("disk\t/System/Volumes/Data|460|220", "disk\t/System/Volumes/Data|460|20")
        self.assertEqual(self.states(low)["disk"], "fail")

    def test_zig_minimum_falls_back_to_the_operator_checkout(self) -> None:
        text = preflight_text(zig="0.15.2", submodules=("-a1 ghostty",))
        self.assertEqual(self.states(text)["zig"], "unknown")
        self.assertEqual(self.states(text, zig_fallback="0.16.0")["zig"], "fail")
        self.assertEqual(self.states(preflight_text(zig="0.16.1"))["zig"], "ok")

    def test_missing_homebrew_makes_brew_fixes_a_person_step(self) -> None:
        checks = self.result(preflight_text(brew_owner=None, rust=False))["checks"]
        self.assertEqual((checks["rust"]["group"], checks["brew"]["state"]), ("person", "fail"))
        self.assertIn("install Homebrew", checks["rust"]["fix"])

    def test_unreachable_and_unprobed_hosts_are_not_ready(self) -> None:
        result = mf.preflight_host(self.manifest, "build-mini-1", {"reachable": False, "error": "timeout"})
        self.assertEqual((result["ready"], result["checks"]["reach"]["detail"]), (False, "timeout"))
        plain = observed(**{"build-mini-1": probe_text()})["hosts"]["build-mini-1"]
        self.assertFalse(mf.preflight_host(self.manifest, "build-mini-1", plain)["ready"])

    def run_main(self, texts: dict[str, str], *extra: str) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as tmp:
            obs = Path(tmp) / "obs.json"
            obs.write_text(json.dumps(observed(**texts)))
            with contextlib.redirect_stdout(io.StringIO()) as out, \
                    mock.patch.object(mf, "operator_zig_minimum", return_value=None):
                code = mf.main(["preflight", *texts, "--manifest", os.fspath(EXAMPLE), "--observed", os.fspath(obs), *extra])
        return code, out.getvalue()

    def test_table_groups_fixes_and_exits_nonzero(self) -> None:
        code, out = self.run_main({"build-mini-1": preflight_text(),
                                   "build-mini-2": preflight_text(hostname="Build-Mini-2", metal=False, sleep=1,
                                                                  node_id="cmux-mac-002")})
        self.assertEqual(code, 1, out)
        header, first, second = out.splitlines()[:3]
        self.assertEqual(header.split()[1:], [*mf.PREFLIGHT_CHECKS, "ready"])
        self.assertTrue(first.startswith("build-mini-1") and first.endswith("yes"))
        self.assertTrue(second.endswith("NO"))
        self.assertIn("can do itself (no root):", out)
        self.assertIn("needs a password:\n    power: sudo pmset -c sleep 0", out)
        self.assertIn("2 hosts, 1 ready, 1 not ready", out)
        self.assertNotIn("\u2014", out)  # no em dashes in operator output

    def test_json_output_and_ready_exit(self) -> None:
        code, out = self.run_main({"build-mini-1": preflight_text()}, "--output", "json")
        self.assertEqual(code, 0, out)
        doc = json.loads(out)
        self.assertEqual(doc["schema"], "glaeda-mini-fleet/v1/preflight")
        host = doc["hosts"]["build-mini-1"]
        self.assertTrue(host["ready"])
        self.assertEqual(set(host["fixes"]), {"self", "password", "person"})

    def test_never_touch_host_is_refused_before_any_connection(self) -> None:
        with mock.patch.object(mf, "observe_host") as probe, contextlib.redirect_stderr(io.StringIO()) as err:
            code = mf.main(["preflight", "coordinator-mini", "--manifest", os.fspath(EXAMPLE)])
        self.assertEqual(code, 2)
        self.assertIn("never_touch", err.getvalue())
        probe.assert_not_called()

    def test_preflight_ships_the_owning_scripts_rules(self) -> None:
        script = mf.preflight_script(self.manifest, "build-mini-1").decode()
        self.assertTrue(script.startswith(mf.PROBE.read_text()))
        self.assertIn(f"WORKLOAD_PATH={mf.bootstrap.CMUX_WORKLOAD_TOOL_PATH}\n", script)
        self.assertIn("WORKLOAD_TOOLS='" + " ".join(mf.bootstrap.MACOS_WORKLOAD_TOOLS) + "'", script)
        self.assertIn("XCODE_PIN=/Applications/Xcode.app\n", script)
        self.assertIn(mf.enroll.PYTHON_CANDIDATES[0], script)
        self.assertIn("~/.local/bin/python3", script)
        self.assertEqual(mf.xcode_pin(self.manifest, "build-mini-1"), "/Applications/Xcode.app")


class PreflightScriptTests(unittest.TestCase):
    def test_parses_as_bash(self) -> None:
        subprocess.run(["bash", "-n", os.fspath(mf.PREFLIGHT_PROBE)], check=True)

    def test_never_installs_writes_or_escalates(self) -> None:
        body = [line.strip() for line in mf.PREFLIGHT_PROBE.read_text().splitlines()
                if line.strip() and not line.strip().startswith("#")]
        # The update check reports the softwareupdate command line, not the sudo wrapper around it.
        body = [line.replace("grep -v '^[0-9]* sudo '", "") for line in body]
        for word in ("sudo", "rm ", "mv ", "cp ", "brew ", "install", "downloadComponent", "curl", "> ", ">>"):
            self.assertFalse([line for line in body if word in line.replace("2>", "").replace(">/dev/null", "")],
                             word)
        self.assertIn("export RUSTUP_AUTO_INSTALL=0 GIT_OPTIONAL_LOCKS=0", body)

    @unittest.skipUnless(shutil.which("git"), "needs git")
    def test_runs_against_a_sandbox_home(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            tools = home / "workload-bin"
            tools.mkdir()
            for name, text in (("zig", "0.15.2"), ("cargo", "cargo 1.88.0 (x)"), ("git", None)):
                if text is None:
                    (tools / name).symlink_to(shutil.which("git"))
                    continue
                (tools / name).write_text(f"#!/bin/sh\necho '{text}'\n")
                (tools / name).chmod(0o755)
            cmux = home / "cmux"
            subprocess.run(["git", "init", "-q", os.fspath(cmux)], check=True)
            (cmux / "scripts/ci").mkdir(parents=True)
            (cmux / "scripts/ci/persistent_compile_fleet.py").write_text(f'CANDIDATE_SOURCE = "{"ab" * 20}"\n')
            generation = home / "Projects/glaeda-generations" / ("ab" * 6)
            generation.mkdir(parents=True)
            (generation / "stage-receipt.json").write_text("{}")
            runner = home / "actions-runner-cmux-persistent-compile"
            runner.mkdir()
            (runner / ".runner").write_text('﻿{\n  "agentName": "mini-1",\n  "serverUrl": "https://secret.example/"\n}\n')
            header = (f"CMUX_ROOT='~/cmux'\nXCODE_PIN=''\nWORKLOAD_PATH={tools}\n"
                      "WORKLOAD_TOOLS='cargo git zig rustup'\nPYTHONS=''\nCANDIDATE_FALLBACK=''\n")
            out = subprocess.run(["bash", "-s"], input=header + mf.PREFLIGHT_PROBE.read_text(), capture_output=True,
                                 text=True, env={"HOME": tmp, "PATH": "/usr/bin:/bin"}, timeout=60).stdout
            pf = mf.parse_probe(out)["preflight"]
        self.assertEqual(pf["tools"]["zig"], {"path": f"{tools}/zig", "version": "0.15.2"})
        self.assertEqual(pf["tools"]["cargo"]["version"], "cargo 1.88.0 (x)")
        self.assertIsNone(pf["tools"]["rustup"]["path"])
        self.assertTrue(pf["cmux"])
        self.assertEqual(pf["candidate"], {"source12": "ab" * 6, "staged": True, "archive": False})
        self.assertEqual(pf["runners"], [{"dir": "actions-runner-cmux-persistent-compile", "name": "mini-1"}])
        self.assertNotIn("secret.example", out)
        self.assertEqual(pf["python"], {"path": None, "version": None})


if __name__ == "__main__":
    unittest.main()
