#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-mini-fleet. Runs on Linux CI: SSH and observation are stubbed."""

from __future__ import annotations

import contextlib
import copy
import importlib.machinery
import importlib.util
import io
import json
import re
import os
import pwd
import shutil
import subprocess
import sys
import tempfile
import time
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


class ClassAndPoolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def write(self, data: dict) -> Path:
        tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, tmp)
        path = Path(tmp) / "m.json"
        path.write_text(json.dumps(data))
        return path

    def test_example_splits_hardware_from_class(self) -> None:
        host = self.manifest["hosts"]["build-mini-1"]
        self.assertEqual((host["hardware"], host["class"], host["availability"]), ("m4pro-48", "std", "dedicated"))
        self.assertIn("m4pro-48", self.manifest["hardware"])

    def test_legacy_classes_table_still_loads_as_hardware(self) -> None:
        data = json.loads(EXAMPLE.read_text())
        data["classes"] = data.pop("hardware")
        for host in data["hosts"].values():
            host["class"] = host.pop("hardware")
            host.pop("availability", None)
        loaded = mf.load_manifest(self.write(data))
        self.assertEqual(loaded["hosts"]["small-mini"]["hardware"], "m4-16")
        self.assertNotIn("class", loaded["hosts"]["small-mini"])

    def test_unknown_class_availability_and_hardware_are_refused(self) -> None:
        for field, value, message in (("class", "huge", "unknown class"),
                                      ("availability", "sometimes", "unknown availability"),
                                      ("hardware", "m9-1", "unknown hardware")):
            data = json.loads(EXAMPLE.read_text())
            data["hosts"]["build-mini-1"][field] = value
            with self.assertRaisesRegex(mf.Failure, message):
                mf.load_manifest(self.write(data))

    def test_hardware_drift_names_the_hardware(self) -> None:
        obs = observed(**{"build-mini-1": probe_text().replace("cpus\t14", "cpus\t12")})
        issues = mf.check(self.manifest, obs, ["build-mini-1"])
        self.assertTrue(any(i["area"] == "hardware" and "hardware m4pro-48" in i["detail"] for i in issues))

    def test_pools_count_declared_and_conforming_once_per_version(self) -> None:
        obs = observed(**{"build-mini-1": probe_text()})
        result = mf.pools(self.manifest, obs, list(self.manifest["hosts"]))
        # build-mini-2 lacks ci-runner and small-mini has no runner role, so only build-mini-1 counts.
        self.assertEqual(list(result), ["glaeda-std-xcode-26.3"])
        self.assertEqual(result["glaeda-std-xcode-26.3"]["declared"], ["build-mini-1"])
        self.assertEqual(result["glaeda-std-xcode-26.3"]["conforming_count"], 1)

    def test_pools_need_an_exact_real_xcode_to_conform(self) -> None:
        for text in (probe_text().replace("|dir|26.3|17C529", "|symlink:/x|26.3|17C529"),
                     probe_text().replace("|dir|26.3|17C529", "|dir|26.3|17C528")):
            result = mf.pools(self.manifest, observed(**{"build-mini-1": text}), ["build-mini-1"])
            self.assertEqual(result["glaeda-std-xcode-26.3"]["conforming"], [])
        self.assertEqual(mf.pools(self.manifest, None, ["build-mini-1"])["glaeda-std-xcode-26.3"]["conforming_count"], 0)

    def test_opportunistic_and_dev_members_are_not_pooled(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["build-mini-1"]["availability"] = "opportunistic"
        self.assertEqual(mf.pools(data, None, ["build-mini-1"]), {})
        data["hosts"]["build-mini-1"].update(availability="dedicated", **{"class": "dev"})
        self.assertEqual(mf.pools(data, None, ["build-mini-1"]), {})

    def test_classed_host_must_state_availability(self) -> None:
        data = json.loads(EXAMPLE.read_text())
        del data["hosts"]["build-mini-1"]["availability"]
        with self.assertRaisesRegex(mf.Failure, "class but no availability"):
            mf.load_manifest(self.write(data))

    def test_host_xcode_override_sets_its_pool(self) -> None:
        data = copy.deepcopy(self.manifest)
        data["hosts"]["build-mini-1"]["overrides"] = {"xcode": {"apps": [
            {"path": "/Applications/Xcode_26.6.app", "version": "26.6", "build": "17F113"}]}}
        text = probe_text() + "xcode_app\t/Applications/Xcode_26.6.app|dir|26.6|17F113\n"
        result = mf.pools(data, observed(**{"build-mini-1": text}), ["build-mini-1"])
        self.assertEqual(list(result), ["glaeda-std-xcode-26.6"])
        self.assertEqual(result["glaeda-std-xcode-26.6"]["conforming"], ["build-mini-1"])

    def test_unreachable_host_is_declared_not_conforming(self) -> None:
        obs = {"hosts": {"build-mini-1": {"host": "build-mini-1", "reachable": False, "error": "timeout"}}}
        result = mf.pools(self.manifest, obs, ["build-mini-1"])
        self.assertEqual((result["glaeda-std-xcode-26.3"]["declared_count"],
                          result["glaeda-std-xcode-26.3"]["conforming_count"]), (1, 0))

    def test_pool_label_matches_the_runner_rule(self) -> None:
        self.assertEqual(mf.pool_label("std", "26.6"), "glaeda-std-xcode-26.6")


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
                   acceptance: str | None = "accepted", update_running: str | None = None, prepared: str | None = None,
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
        lines.append("pf_rust_channel\t1.88.0|rustc 1.88.0 (abc 2025-06-23)|cargo 1.88.0 (abc 2025-06-23)")
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
        lines.append(f"pf_update_prepared\t{prepared}")
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
        prepared = self.result(preflight_text(prepared="pending|26.7"))
        self.assertEqual(prepared["checks"]["update"]["state"], "fail")
        self.assertIn("macOS 26.7 is prepared and waits for a restart", prepared["checks"]["update"]["detail"])
        # cmux14 and cmux15 carry a 26.6.2 prepared before a reboot that never applied; that is not pending.
        suspended = self.result(preflight_text(prepared="suspended|26.6.2"))
        self.assertEqual(suspended["checks"]["update"]["state"], "ok")

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

    def test_bootstrap_refusals_are_predicted(self) -> None:
        # Each of these passes every tool check yet the fleet bootstrap would refuse the host.
        pin16 = preflight_text().replace("pf_cmux_pin\t26", "pf_cmux_pin\t16")
        self.assertEqual(self.states(pin16)["xcode"], "fail")
        small = preflight_text().replace("cpus\t14", "cpus\t4")
        self.assertEqual(self.states(small)["os"], "fail")
        bare = preflight_text().replace("pf_cache_root\tpresent", "pf_cache_root\tmissing")
        self.assertIn("native cache root", self.result(bare)["checks"]["glaeda"]["detail"])
        no_cargo = preflight_text().replace("|cargo 1.88.0 (abc 2025-06-23)", "|error: toolchain '1.88.0' is not installed")
        self.assertEqual(self.states(no_cargo)["rust"], "fail")
        dubious = preflight_text(submodules=("fatal: detected dubious ownership in repository",))
        checks = self.result(dubious)["checks"]
        self.assertIn("dubious ownership", checks["cmux"]["detail"])
        self.assertEqual(checks["cmux"]["group"], "person")

    def test_unknowns_that_hide_a_refusal_block(self) -> None:
        del self.manifest["defaults"]["xcode"]
        del self.manifest["defaults"]["toolchain"]["xcode"]
        result = self.result(preflight_text())
        self.assertEqual(result["checks"]["xcode"]["state"], "unknown")
        self.assertFalse(result["ready"])

    def test_metal_is_not_blamed_for_an_unaccepted_licence(self) -> None:
        text = preflight_text(metal=False).replace("Xcode.app|accepted|done", "Xcode.app|needed|done")
        checks = self.result(text)["checks"]
        self.assertEqual((checks["licence"]["state"], checks["metal"]["state"]), ("fail", "unknown"))

    def test_zig_minimum_falls_back_to_the_operator_checkout(self) -> None:
        del self.manifest["defaults"]["toolchain"]["zig_min"]
        text = preflight_text(zig="0.15.2", submodules=("-a1 ghostty",))
        self.assertEqual(self.states(text)["zig"], "unknown")
        self.assertEqual(self.states(text, zig_fallback="0.16.0")["zig"], "fail")
        self.assertEqual(self.states(preflight_text(zig="0.16.1"))["zig"], "ok")

    def test_missing_homebrew_is_installed_by_sudo_plan_then_used_by_fix(self) -> None:
        # Most Manaflow minis had no Homebrew at all on 2026-09-24.
        result = self.result(preflight_text(user="cmux", brew_owner=None, rust=False, zig="0.15.2"), zig_fallback="0.16.0")
        checks = result["checks"]
        self.assertEqual((checks["rust"]["group"], checks["brew"]["state"], checks["brew"]["group"]),
                         ("self", "fail", "password"))
        self.assertIn("after sudo-plan installs Homebrew for cmux", checks["rust"]["fix"])
        mine, root = mf.planned_actions(result)
        self.assertEqual([a["kind"] for a in root], ["homebrew"])
        self.assertEqual([(a["kind"], a.get("formulas") or a.get("toolchain")) for a in mine],
                         [("brew", ["rustup", "zig"]), ("rust_default", "stable")])
        self.assertEqual(mine[0]["owner"], "cmux")
        out = mf.fix_host(self.manifest, "build-mini-1", result, False, None)
        self.assertEqual(len(out["waiting"]), 2)
        self.assertIn("waits for brew (sudo-plan)", out["waiting"][0])
        self.assertIn("waits for rust", out["waiting"][1])
        script = mf.sudo_plan_script(self.manifest, "build-mini-1", result)
        self.assertIn(f'  curl -fsSL -o "$installer" {mf.HOMEBREW_INSTALLER}\n  NONINTERACTIVE=1 /bin/bash "$installer"\n',
                      script)
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        self.assertNotIn("brew install", script)
        # Once Homebrew exists and the login user owns it, fix installs the formulas without root.
        checks["brew"] = {"state": "info", "detail": "cmux", "fix": "", "group": ""}
        out = mf.fix_host(self.manifest, "build-mini-1", result, False, None)
        self.assertEqual([p.split(":")[0] for p in out["planned"]], ["rust/zig", "rust"])

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
        body = [line.replace("grep -v '^[0-9]* sudo '", "") for line in body if not line.startswith("applying='")]
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
            # The whole SSH payload, probe first, as observe_host sends it.
            script = mf.PROBE.read_text() + "\n" + header + mf.PREFLIGHT_PROBE.read_text()
            out = subprocess.run(["bash", "-s"], input=script, capture_output=True,
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

    def test_empty_homebrew_prefix_is_no_homebrew(self) -> None:
        # A brew.sh run that stops early leaves bin and Cellar behind. Counting those as Homebrew hid the missing
        # brew from preflight, and fix then ran /opt/homebrew/bin/brew on four minis (2026-09-24).
        def owner(prefix: Path) -> str | None:
            script = mf.PROBE.read_text() + "\nCMUX_ROOT='~/cmux'\nXCODE_PIN=''\nWORKLOAD_PATH=''\n" \
                "WORKLOAD_TOOLS=''\nPYTHONS=''\nCANDIDATE_FALLBACK=''\n" + mf.PREFLIGHT_PROBE.read_text()
            out = subprocess.run(["bash", "-s"], input=script, capture_output=True, text=True, timeout=60,
                                 env={"HOME": os.fspath(prefix.parent), "PATH": "/usr/bin:/bin",
                                      "GLAEDA_PREFLIGHT_BREW_PREFIX": os.fspath(prefix)}).stdout
            return mf.parse_probe(out)["preflight"]["brew_owner"]
        with tempfile.TemporaryDirectory() as tmp:
            prefix = Path(tmp) / "homebrew"
            for name in ("bin", "Cellar", "lib"):
                (prefix / name).mkdir(parents=True)
            self.assertIsNone(owner(prefix))
            (prefix / "bin/brew").write_text("#!/bin/sh\n")
            self.assertIsNone(owner(prefix))  # present but not runnable
            (prefix / "bin/brew").chmod(0o755)
            self.assertEqual(owner(prefix), pwd.getpwuid(os.getuid()).pw_name)


def morning_text() -> str:
    """cmux-austin-mini-1 on 2026-09-24 (see PreflightTests), as a preflight probe."""
    text = preflight_text(user="cmux", brew_owner="admin", zig="0.15.2", zig_path="/usr/local/bin/zig",
                          rust=False, metal=False, python="|3.9", submodules=("-a1 ghostty",),
                          artifacts=False, candidate="59ca9c9bd1bb|no|no", enroll_state=None,
                          acceptance=None, node_id=None)
    return text.replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                        "xcode_select\t/Applications/Xcode_26.3.app/Contents/Developer")


def write_manifest(tmp: str, data: dict) -> Path:
    path = Path(tmp) / "m.json"
    path.write_text(json.dumps(data))
    return path


class ToolchainTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def result(self, text: str, host: str = "build-mini-1") -> dict:
        return mf.preflight_host(self.manifest, host, observed(**{host: text})["hosts"][host])

    def test_bad_declarations_are_refused(self) -> None:
        cases = (({"python_min": "3.12"}, "below 3.13"),
                 ({"xcode": {"app": "/Applications/My Xcode.app", "version": "26.3", "build": "x"}}, "without spaces"),
                 ({"xcode": {"app": "/Applications/Xcode.app"}}, "needs app, version and build"),
                 ({"zig_min": "latest"}, "not a version"),
                 ({"rustup_default": "stable; rm -rf ~"}, "not a toolchain name"),
                 ({"cmux": {"ref": "-o ProxyCommand"}}, "branch or tag"),
                 ({"cmux": {"submodule_depth": -1}}, "submodule_depth"),
                 ({"ruby": "3"}, "unknown keys"))
        for override, error in cases:
            data = copy.deepcopy(self.manifest)
            data["hosts"]["build-mini-1"]["overrides"] = {"toolchain": override}
            with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(mf.Failure, error):
                mf.load_manifest(write_manifest(tmp, data))

    def test_declared_xcode_is_the_pin(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"]["app"] = "/Applications/Xcode_26.3.app"
        self.assertEqual(mf.xcode_pin(self.manifest, "build-mini-1"), "/Applications/Xcode_26.3.app")
        self.manifest["defaults"]["toolchain"]["xcode"]["build"] = "17C999"
        checks = self.result(preflight_text())["checks"]
        self.assertEqual(checks["xcode"]["state"], "fail")
        self.assertIn("want 26.3 (17C999)", checks["xcode"]["detail"])

    def test_selection_and_metal_can_be_declared_optional(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"]["select"] = False
        self.manifest["defaults"]["toolchain"]["metal"] = False
        text = preflight_text(metal=False).replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                                                   "xcode_select\t/Library/Developer/CommandLineTools")
        result = self.result(text)
        self.assertEqual((result["checks"]["select"]["state"], result["checks"]["metal"]["state"]), ("info", "info"))
        self.assertTrue(result["ready"], result)

    def test_declared_minimums_raise_the_bar(self) -> None:
        self.manifest["defaults"]["toolchain"]["zig_min"] = "0.17.0"
        self.manifest["defaults"]["toolchain"]["python_min"] = "3.14"
        checks = self.result(preflight_text(zig="0.16.0"))["checks"]
        self.assertIn("Ghostty needs 0.17.0", checks["zig"]["detail"])
        self.assertEqual(checks["python"]["state"], "fail")
        self.assertIn("enroll would use 3.13", checks["python"]["detail"])
        self.assertEqual(checks["python"]["action"], {"kind": "python", "min": "3.14"})

    def test_rustup_default_must_match(self) -> None:
        text = preflight_text() + "pf_rust_default\tnightly-aarch64-apple-darwin (default)\n"
        checks = self.result(text)["checks"]
        self.assertEqual(checks["rust"]["state"], "fail")
        self.assertEqual(checks["rust"]["action"], {"kind": "rust_default", "toolchain": "stable"})
        ok = preflight_text() + "pf_rust_default\tstable-aarch64-apple-darwin (default)\n"
        self.assertEqual(self.result(ok)["checks"]["rust"]["state"], "ok")
        self.manifest["defaults"]["toolchain"]["rustup_default"] = "1.8"
        near = preflight_text() + "pf_rust_default\t1.80.0-aarch64-apple-darwin (default)\n"
        self.assertEqual(self.result(near)["checks"]["rust"]["state"], "fail")

    def test_cmux_checkout_comes_from_the_toolchain(self) -> None:
        self.manifest["defaults"]["toolchain"]["cmux"] = {"repo": "example/cmux", "ref": "release", "root": "~/src/cmux",
                                                         "submodule_depth": 0}
        text = preflight_text().replace("pf_cmux\tpresent", "pf_cmux\tmissing")
        check = self.result(text)["checks"]["cmux"]
        self.assertEqual(check["action"], {"kind": "cmux_clone", "repo": "example/cmux", "ref": "release",
                                           "root": "~/src/cmux", "depth": 0})
        self.assertIn("--branch release https://github.com/example/cmux.git ~/src/cmux", check["fix"])
        self.assertIn("CMUX_ROOT='~/src/cmux'", mf.preflight_script(self.manifest, "build-mini-1").decode())

    def test_check_reports_toolchain_drift(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"] = {"app": "/Applications/Xcode_26.6.app", "version": "26.6",
                                                           "build": "17F113"}
        obs = observed(**{"build-mini-1": preflight_text(zig="0.15.2", metal=False)})
        issues = [i for i in mf.check(self.manifest, obs, ["build-mini-1"]) if i["area"] == "toolchain"]
        details = [i["detail"] for i in issues]
        self.assertIn("Xcode /Applications/Xcode_26.6.app (26.6 17F113) missing", details)
        self.assertTrue(any(d.startswith("zig: 0.15.2") for d in details), details)
        # Without a preflight section only the Xcode part can be judged.
        plain = observed(**{"build-mini-1": probe_text()})
        self.assertEqual([i["detail"] for i in mf.check(self.manifest, plain, ["build-mini-1"]) if i["area"] == "toolchain"],
                         ["Xcode /Applications/Xcode_26.6.app (26.6 17F113) missing"])

    def test_check_probes_preflight_when_a_toolchain_is_declared(self) -> None:
        with mock.patch.object(mf, "observe", return_value=observed(**{"build-mini-1": preflight_text()})) as probe, \
                contextlib.redirect_stdout(io.StringIO()):
            mf.main(["check", "build-mini-1", "--manifest", os.fspath(EXAMPLE)])
        self.assertTrue(probe.call_args.kwargs["preflight"])


class FixPlanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        obs = observed(**{"build-mini-1": morning_text()})["hosts"]["build-mini-1"]
        self.result = mf.preflight_host(self.manifest, "build-mini-1", obs, "0.16.0")

    def test_the_morning_splits_into_fix_and_sudo_plan(self) -> None:
        mine, root = mf.planned_actions(self.result)
        self.assertEqual([a["kind"] for a in mine], ["python", "submodules", "metal", "setup", "candidate"])
        self.assertEqual([a["kind"] for a in root], ["select", "brew"])
        self.assertEqual((root[1]["formulas"], root[1]["checks"], root[1]["owner"]), (["rustup", "zig"], ["rust", "zig"], "admin"))

    def test_dry_run_plans_waits_and_runs_nothing(self) -> None:
        with mock.patch.object(mf, "run_repair") as run:
            out = mf.fix_host(self.manifest, "build-mini-1", self.result, False, None)
        run.assert_not_called()
        self.assertEqual(len(out["planned"]), 4)
        self.assertEqual(out["waiting"], ["setup: cmux scripts/setup.sh in ~/cmux (builds GhosttyKit) (waits for zig (sudo-plan))"])
        self.assertEqual(out["sudo"], ["select", "rust", "zig"])
        text = mf.render_fix([out], False)
        self.assertIn("dry run; pass --yes", text)
        self.assertNotIn("\u2014", text)

    def test_runs_in_order_logs_and_a_failure_holds_back_its_dependents(self) -> None:
        self.result["checks"]["zig"] = {"state": "ok", "detail": "", "fix": "", "group": ""}
        calls = []

        def repair(manifest, name, action, log, seed=None):
            calls.append(action["kind"])
            return action["kind"] != "metal", "boom" if action["kind"] == "metal" else ""

        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(mf, "run_repair", side_effect=repair), \
                contextlib.redirect_stdout(io.StringIO()):
            out = mf.fix_host(self.manifest, "build-mini-1", self.result, True, Path(tmp))
            log = Path(out["log"]).read_text()
        self.assertEqual(calls, ["python", "submodules", "metal", "candidate"])
        self.assertEqual(len(out["failed"]), 1)
        self.assertIn("waits for metal (fix)", out["waiting"][0])
        self.assertIn("FAILED boom", log)

    def test_one_host_raising_is_a_failure_not_a_crash(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "run_repair", side_effect=OSError("disk full")):
            out = mf.fix_host(self.manifest, "build-mini-1", self.result, True, Path(tmp))
        self.assertTrue(out["failed"])
        self.assertIn("OSError: disk full", out["failed"][0])

    def test_unknown_prerequisites_name_their_cause(self) -> None:
        self.result["checks"]["licence"] = {"state": "unknown", "detail": "no runnable xcodebuild in the pinned Xcode",
                                            "fix": "", "group": ""}
        waiting = mf.fix_host(self.manifest, "build-mini-1", self.result, False, None)["waiting"]
        self.assertIn("waits for licence (unknown: no runnable xcodebuild in the pinned Xcode)", waiting[0])

    def test_glaeda_runs_with_the_python_enroll_will_use_and_waits_for_cargo(self) -> None:
        self.result["checks"]["glaeda"] = {"state": "fail", "detail": "", "fix": "", "group": "self", "action": {"kind": "glaeda"}}
        waiting = mf.fix_host(self.manifest, "build-mini-1", self.result, False, None)["waiting"]
        self.assertTrue(any(w.startswith("glaeda:") and "waits for rust (sudo-plan)" in w for w in waiting), waiting)
        self.result["checks"]["glaeda"] = {"state": "fail", "detail": "", "fix": "", "group": "self", "action": {"kind": "glaeda"}}
        self.result["checks"]["rust"] = {"state": "ok", "detail": "", "fix": "", "group": ""}
        seen = {}
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "run_repair", side_effect=lambda m, n, a, log, seed=None: (seen.setdefault(a["kind"], a), (True, ""))[1]):
            mf.fix_host(self.manifest, "build-mini-1", self.result, True, Path(tmp))
        self.assertEqual(seen["glaeda"]["python"], "~/.local/bin/python3")

    def test_fix_command_skips_record_only_hosts_and_probes_once(self) -> None:
        obs = observed(**{"build-mini-1": morning_text()})
        with mock.patch.object(mf, "observe", return_value=obs) as probe, mock.patch.object(mf, "run_repair") as run, \
                contextlib.redirect_stdout(io.StringIO()) as out, \
                mock.patch.object(mf, "operator_zig_minimum", return_value="0.16.0"):
            code = mf.main(["fix", "build-mini-1", "small-mini", "--manifest", os.fspath(EXAMPLE)])
        self.assertEqual(code, 0)
        run.assert_not_called()
        self.assertEqual(probe.call_args.args[1], ["build-mini-1"])
        self.assertIn("small-mini: skipped (apply: false)", out.getvalue())
        self.assertIn("would run", out.getvalue())


class SudoPlanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)

    def plan(self, text: str, sudoers: bool = False) -> str | None:
        obs = observed(**{"build-mini-1": text})["hosts"]["build-mini-1"]
        return mf.sudo_plan_script(self.manifest, "build-mini-1", mf.preflight_host(self.manifest, "build-mini-1", obs, "0.16.0"),
                                   sudoers)

    def test_every_root_step_once_in_order(self) -> None:
        text = morning_text().replace("Xcode.app|accepted|done", "Xcode.app|needed|needed").replace(
            "pf_pmset\t sleep                0", "pf_pmset\t sleep                1\npf_pmset\t autorestart          0")
        script = self.plan(text)
        order = [script.index(s) for s in ("sudo -v", "sudo xcode-select -s /Applications/Xcode.app",
                                           "xcodebuild -license accept", "xcodebuild -runFirstLaunch",
                                           "sudo pmset -c sleep 0", "sudo pmset -a autorestart 1",
                                           "sudo -H -u admin /opt/homebrew/bin/brew", "for f in rustup zig",
                                           'sudo -H -u admin ln -s "/opt/homebrew/opt/rustup/bin/$t"')]
        self.assertEqual(order, sorted(order))
        self.assertEqual(script.count("sudo -v\n"), 1)
        self.assertNotIn(mf.SUDOERS_FILE, script)
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)
        self.assertNotIn("\u2014", script)

    def test_ready_host_has_no_plan_and_autorestart_alone_does_not_block(self) -> None:
        self.assertIsNone(self.plan(preflight_text()))
        text = preflight_text().replace("pf_pmset\t sleep                0",
                                        "pf_pmset\t sleep                0\npf_pmset\t autorestart          0")
        obs = observed(**{"build-mini-1": text})["hosts"]["build-mini-1"]
        result = mf.preflight_host(self.manifest, "build-mini-1", obs)
        self.assertTrue(result["ready"])
        self.assertIn("sudo pmset -a autorestart 1", mf.sudo_plan_script(self.manifest, "build-mini-1", result))

    def test_sudoers_rule_is_narrow_and_checked_before_install(self) -> None:
        script = self.plan(preflight_text(), sudoers=True)
        rule = next(line for line in script.splitlines() if line.startswith("builder ALL="))
        # Never a path rule for Xcode tools: /Applications is admin-writable, so that would be root.
        self.assertEqual(rule, "builder ALL=(root) NOPASSWD: /bin/launchctl kickstart -k system/com.example.build-worker")
        del self.manifest["defaults"]["launchd"]
        self.assertIn("no rule to install", self.plan(preflight_text(), sudoers=True))
        self.assertLess(script.index('sudo visudo -cf "$rule"'), script.index(f"sudo install -m 0440"))
        subprocess.run(["bash", "-n"], input=script, text=True, check=True)

    def test_pending_restart_is_noted_not_run(self) -> None:
        script = self.plan(morning_text() + "pf_update_prepared\tpending|26.7\n")
        self.assertIn("# Not run here: macOS 26.7 is prepared", script)
        self.assertNotIn("shutdown", script.replace("# Not run here", ""))

    def test_homebrew_owner_is_validated(self) -> None:
        with self.assertRaises(mf.Failure):
            mf.brew_commands("admin; rm -rf /", ["zig"], True)
        with self.assertRaises(mf.Failure):
            mf.brew_commands("admin", ["zig", "openssl"], True)

    def test_run_needs_a_terminal_and_plans_are_saved(self) -> None:
        obs = observed(**{"build-mini-1": morning_text()})
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(mf, "observe", return_value=obs), \
                contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err, \
                mock.patch.object(mf, "operator_zig_minimum", return_value="0.16.0"):
            with mock.patch.object(sys.stdin, "isatty", return_value=False):
                self.assertEqual(mf.main(["sudo-plan", "build-mini-1", "--run", "--manifest", os.fspath(EXAMPLE)]), 2)
            self.assertIn("from a terminal", err.getvalue())
            code = mf.main(["sudo-plan", "build-mini-1", "--manifest", os.fspath(EXAMPLE), "--log-dir", tmp])
            saved = (Path(tmp) / "build-mini-1.sudo-plan.sh")
            self.assertEqual(code, 0)
            self.assertEqual(saved.stat().st_mode & 0o777, 0o600)
            self.assertIn("sudo-plan build-mini-1 --run", out.getvalue())


class FixLibraryTests(unittest.TestCase):
    """cmux_mini_fix.sh functions, run locally the way fix_call sends them, against a sandbox HOME."""

    def call(self, home: Path, func: str, *args: str) -> subprocess.CompletedProcess:
        with mock.patch.object(mf, "ssh_stream", return_value=0) as ssh:
            mf.fix_call("h", "u", func, list(args), None, 10)
        command = ssh.call_args.args[2]
        self.assertTrue(command.startswith("/bin/bash -c "))
        return subprocess.run(["bash", "-c", command], capture_output=True, text=True, timeout=60,
                              env={"HOME": os.fspath(home), "PATH": "/usr/bin:/bin"})

    def test_parses_as_bash_and_never_escalates(self) -> None:
        subprocess.run(["bash", "-n", os.fspath(mf.FIX_LIBRARY)], check=True)
        body = [line for line in mf.FIX_LIBRARY.read_text().splitlines() if not line.lstrip().startswith("#")]
        for word in ("curl", "--force"):
            self.assertFalse([line for line in body if word in line], word)
        # Recursive removal only of this file's own staging directories.
        self.assertEqual([line.strip() for line in body if "rm -r" in line],
                         ['case "$stage" in */.glaeda-share.partial) rm -rf "$stage" ;; esac',
                          'rm -rf "$stage"  # what is left are copies of toolchains this host already had'])
        self.assertFalse([line for line in body if re.search(r"(^|[;&|(]\s*)sudo\b", line.strip())])
        # The only removal is the step lock's own pid file and directory.
        self.assertEqual([line.strip() for line in body if "rm " in line and "rm -r" not in line],
                         ["""trap 'rm -f "$HOME/.local/state/glaeda/mini-fleet/step.lock/pid"; rmdir "$HOME/.local/state/glaeda/mini-fleet/step.lock" 2>/dev/null || true' EXIT"""])

    @unittest.skipUnless(shutil.which("shasum") or shutil.which("sha256sum"), "needs shasum")
    def test_candidate_is_placed_only_when_its_digest_matches(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            bin_dir = home / "bin"
            bin_dir.mkdir()
            if not shutil.which("shasum"):  # Linux CI: shasum -a 256 as sha256sum
                (bin_dir / "shasum").write_text('#!/bin/sh\nshift 2\nexec sha256sum "$@"\n')
                (bin_dir / "shasum").chmod(0o755)
            with mock.patch.object(mf.bootstrap, "CMUX_WORKLOAD_TOOL_PATH", f"{bin_dir}:/usr/bin:/bin"):
                self.assertEqual(self.call(home, "candidate_check", "abc", "a.tar.gz", "0" * 64).returncode, 10)
                self.assertEqual(self.call(home, "candidate_dir", "abc").returncode, 0)
                directory = home / "Library/Caches/cmux-fleet/glaeda-candidate-abc"
                (directory / "a.tar.gz.partial").write_bytes(b"candidate")
                import hashlib
                good = hashlib.sha256(b"candidate").hexdigest()
                bad = self.call(home, "candidate_place", "abc", "a.tar.gz", "f" * 64)
                self.assertEqual(bad.returncode, 3)
                self.assertFalse((directory / "a.tar.gz").exists())
                self.assertEqual(self.call(home, "candidate_place", "abc", "a.tar.gz", good).returncode, 0)
                self.assertEqual((directory / "a.tar.gz").read_bytes(), b"candidate")
                self.assertEqual(self.call(home, "candidate_check", "abc", "a.tar.gz", good).returncode, 0)

    def test_python_link_refuses_a_foreign_python3(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            dist = home / ".local/python-3.14/bin"
            dist.mkdir(parents=True)
            (dist / "python3").write_text('#!/bin/sh\n[ "$1" = --version ] && echo "Python 3.14.0"\nexit 0\n')
            (dist / "python3").chmod(0o755)
            (home / ".local/bin").mkdir()
            (home / ".local/bin/python3").symlink_to("/usr/bin/python3")
            refused = self.call(home, "python_link", "~/.local/python-3.14", "3.13")
            self.assertEqual(refused.returncode, 3, refused.stderr)
            (home / ".local/bin/python3").unlink()
            linked = self.call(home, "python_link", "~/.local/python-3.14", "3.13")
            self.assertEqual(linked.returncode, 0, linked.stderr)
            self.assertEqual(os.readlink(home / ".local/bin/python3"), f"{dist}/python3")
            # Idempotent: its own link is replaced, not refused.
            self.assertEqual(self.call(home, "python_link", "~/.local/python-3.14", "3.13").returncode, 0)

    def test_a_running_step_holds_the_host_lock(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            lock = home / ".local/state/glaeda/mini-fleet/step.lock"
            lock.mkdir(parents=True)
            (lock / "pid").write_text(str(os.getpid()))  # a live process holds it
            held = self.call(home, "candidate_dir", "abc")
            self.assertEqual(held.returncode, 3)
            self.assertIn("still running", held.stderr)
            finished = subprocess.Popen(["true"])
            finished.wait()
            (lock / "pid").write_text(str(finished.pid))  # its process is gone: taken over, then released
            self.assertEqual(self.call(home, "candidate_dir", "abc").returncode, 0)
            self.assertFalse(lock.exists())

    @unittest.skipUnless(shutil.which("git"), "needs git")
    def test_an_unfinished_clone_is_not_built_on(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            subprocess.run(["git", "init", "-q", os.fspath(home / "cmux")], check=True)
            out = self.call(home, "cmux_clone", "example/cmux", "main", "~/cmux", "1")
            self.assertEqual(out.returncode, 3)
            self.assertIn("unfinished clone", out.stderr)

    def test_clone_refuses_a_directory_that_is_not_a_checkout(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            (home / "cmux").mkdir()
            (home / "cmux/notes.txt").write_text("mine")
            out = self.call(home, "cmux_clone", "example/cmux", "main", "~/cmux", "1")
            self.assertEqual(out.returncode, 3)
            self.assertIn("not a git checkout", out.stderr)
            self.assertEqual((home / "cmux/notes.txt").read_text(), "mine")


SOURCE = "59ca9c9bd1bb" + "0" * 28


class OperatorSideTests(unittest.TestCase):
    CONSTANTS = ('CANDIDATE_RUN = "35991372390"\nCANDIDATE_ARTIFACT = "glaeda-candidate-aarch64-apple-darwin"\n'
                 f'CANDIDATE_SOURCE = "{SOURCE}"\nCANDIDATE_SHA256 = "{{sha}}"\n'
                 'CANDIDATE_REPO = "teamleaderleo/glaeda"\n')

    def test_candidate_downloads_once_and_is_digest_checked(self) -> None:
        import hashlib
        payload = b"archive"
        sha = hashlib.sha256(payload).hexdigest()
        with tempfile.TemporaryDirectory() as tmp:
            cmux = Path(tmp) / "cmux/scripts/ci"
            cmux.mkdir(parents=True)
            (cmux / "persistent_compile_fleet.py").write_text(self.CONSTANTS.replace("{sha}", sha))
            downloads = []

            def gh(argv, log, stdin=b"", timeout=None):
                downloads.append(argv)
                target = Path(argv[argv.index("--dir") + 1])
                (target / f"glaeda-{SOURCE}-aarch64-apple-darwin.tar.gz").write_bytes(payload)
                return 0

            with mock.patch.dict(os.environ, {"GLAEDA_CMUX_ROOT": os.fspath(Path(tmp) / "cmux")}), \
                    mock.patch.object(mf, "OPERATOR_CACHE", Path(tmp) / "cache"), \
                    mock.patch.object(mf, "run_logged", side_effect=gh):
                candidate = mf.operator_candidate()
                self.assertEqual(candidate["run"], "35991372390")
                self.assertEqual(mf.operator_candidate_source(), candidate["source"])
                first = mf.fetch_candidate(candidate, None)
                second = mf.fetch_candidate(candidate, None)
                self.assertEqual(first, second)
                self.assertEqual(len(downloads), 1)
                self.assertEqual(downloads[0][:3], ["gh", "run", "download"])
                with self.assertRaisesRegex(mf.Failure, "sha256"):
                    mf.fetch_candidate({**candidate, "sha256": "f" * 64}, None)

    def test_python_dist_is_the_newest_that_meets_the_minimum(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp) / ".local/share/uv/python"
            for name in ("cpython-3.12.9-macos-aarch64-none", "cpython-3.13.5-macos-aarch64-none",
                         "cpython-3.14.0-macos-aarch64-none"):
                (base / name / "bin").mkdir(parents=True)
                (base / name / "bin/python3").write_text("")
            with mock.patch.dict(os.environ, {"HOME": tmp}):
                os.environ.pop("GLAEDA_MINI_FLEET_PYTHON", None)
                self.assertEqual(mf.operator_python_dist("3.13")[1], "3.14.0")
                self.assertIsNone(mf.operator_python_dist("3.15"))


SSH_SHIM = """#!/bin/bash
# Test stand-in for ssh: each host is a directory under $SHIM_ROOT used as that host's HOME.
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-i|-l) shift 2 ;;
    -A|-t|-T) shift ;;
    *) break ;;
  esac
done
host=$1; shift
exec env HOME="$SHIM_ROOT/$host" bash -c "$*"
"""


class ShareTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        self.candidate = {"source": SOURCE, "sha256": "a" * 64, "run": "1", "artifact": "x", "repo": "a/b"}

    def test_share_arguments_come_from_the_manifest_and_this_mac(self) -> None:
        with mock.patch.object(mf, "operator_candidate", return_value=self.candidate):
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "xcode"), ["/Applications/Xcode.app", "26.3", "17C529"])
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "metal"), ["/Applications/Xcode.app/Contents/Developer"])
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "candidate"),
                             [SOURCE[:12], f"glaeda-{SOURCE}-aarch64-apple-darwin.tar.gz", "a" * 64])
            self.assertEqual(mf.share_args(self.manifest, "build-mini-1", "python"), ["3.13"])

    def test_lan_address_must_be_on_the_fleet_lan(self) -> None:
        self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "10.0.8.7"
        self.assertEqual(mf.lan_address(self.manifest, "build-mini-2", None), ("10.0.8.7", "10.0.8.7"))
        self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "100.64.1.2"  # a tailnet address is not the LAN
        address, why = mf.lan_address(self.manifest, "build-mini-2", None)
        self.assertIsNone(address)
        self.assertIn("outside the fleet LAN 10.0.8.", why)

    def test_dry_run_checks_the_peer_and_copies_nothing(self) -> None:
        self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "10.0.8.7"
        with mock.patch.object(mf, "ssh_stream", return_value=1) as ssh:
            ok, detail = mf.share_one(self.manifest, "xcode", "build-mini-1", "build-mini-2",
                                      ["/Applications/Xcode.app", "26.3", "17C529"], None, False)
        self.assertTrue(ok)
        self.assertEqual(detail, "would copy over build-mini-1 -> 10.0.8.7 (ssh -A from this Mac)")
        self.assertEqual(ssh.call_count, 1)
        self.assertIn("share_have xcode", ssh.call_args.args[2])

    @unittest.skipUnless(shutil.which("shasum") or shutil.which("sha256sum"), "needs shasum")
    def test_candidate_goes_from_seed_to_peer_over_the_lan_and_verifies(self) -> None:
        import hashlib
        payload = b"candidate bytes"
        sha = hashlib.sha256(payload).hexdigest()
        name = f"glaeda-{SOURCE}-aarch64-apple-darwin.tar.gz"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            shim = root / "ssh"
            shim.write_text(SSH_SHIM)
            shim.chmod(0o755)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            if not shutil.which("shasum"):
                (bin_dir / "shasum").write_text('#!/bin/sh\nshift 2\nexec sha256sum "$@"\n')
                (bin_dir / "shasum").chmod(0o755)
            seed = root / "build-mini-1/Library/Caches/cmux-fleet" / f"glaeda-candidate-{SOURCE[:12]}"
            seed.mkdir(parents=True)
            (seed / name).write_bytes(payload)
            (root / "build-mini-2").mkdir()
            (root / "10.0.8.7").symlink_to(root / "build-mini-2")
            self.manifest["hosts"]["build-mini-2"]["lan_ip"] = "10.0.8.7"
            args = [SOURCE[:12], name, sha]
            with mock.patch.object(mf, "SSH", os.fspath(shim)), \
                    mock.patch.object(mf.bootstrap, "CMUX_WORKLOAD_TOOL_PATH", f"{bin_dir}:/usr/bin:/bin"), \
                    mock.patch.dict(os.environ, {"SHIM_ROOT": tmp}), \
                    mock.patch.object(mf, "operator_candidate", return_value={"source": SOURCE, "sha256": sha}):
                self.assertIsNone(mf.prepare_seed(self.manifest, "build-mini-1", "candidate", None, True))
                ok, detail = mf.share_one(self.manifest, "candidate", "build-mini-1", "build-mini-2", args, None, True)
                self.assertTrue(ok, detail)
                placed = root / "build-mini-2/Library/Caches/cmux-fleet" / f"glaeda-candidate-{SOURCE[:12]}" / name
                self.assertEqual(placed.read_bytes(), payload)
                self.assertFalse((placed.parent / ".glaeda-share.partial").exists())
                # A rerun finds it in place and copies nothing.
                self.assertEqual(mf.share_one(self.manifest, "candidate", "build-mini-1", "build-mini-2", args, None, True),
                                 (True, "already there"))
                # A peer holding a different archive under that name is refused, not overwritten.
                placed.write_bytes(b"something else")
                ok, _ = mf.share_one(self.manifest, "candidate", "build-mini-1", "build-mini-2", args, None, True)
                self.assertFalse(ok)
                self.assertEqual(placed.read_bytes(), b"something else")

    def test_share_command_needs_a_source_and_refuses_never_touch(self) -> None:
        for argv, error in ((["share", "xcode", "build-mini-2"], "--from HOST"),
                            (["share", "xcode", "build-mini-2", "--from", "coordinator-mini"], "never_touch"),
                            (["share", "xcode", "--from", "build-mini-1"], "destination"),
                            (["share", "teapot", "build-mini-2", "--from", "build-mini-1"], "share what")):
            with contextlib.redirect_stderr(io.StringIO()) as err, mock.patch.object(mf, "ssh_stream") as ssh:
                self.assertEqual(mf.main([*argv, "--manifest", os.fspath(EXAMPLE)]), 2, argv)
            self.assertIn(error, err.getvalue())
            ssh.assert_not_called()

    def test_fix_from_a_seed_copies_over_the_lan(self) -> None:
        obs = observed(**{"build-mini-1": morning_text()})["hosts"]["build-mini-1"]
        result = mf.preflight_host(self.manifest, "build-mini-1", obs, "0.16.0")
        out = mf.fix_host(self.manifest, "build-mini-1", result, False, None, seed="build-mini-2")
        self.assertIn("python: copy a relocatable CPython 3.13+ from this Mac into ~/.local [from build-mini-2 over the LAN]",
                      out["planned"])
        calls = []
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "prepare_seed", return_value=None), \
                mock.patch.object(mf, "share_one", side_effect=lambda m, thing, src, dst, a, log, yes: (calls.append((thing, src)), (True, ""))[1]), \
                mock.patch.object(mf, "fix_call", return_value=0), mock.patch.object(mf, "operator_candidate", return_value=self.candidate):
            mf.fix_host(self.manifest, "build-mini-1", result, True, Path(tmp), seed="build-mini-2")
        self.assertEqual(calls, [("python", "build-mini-2"), ("metal", "build-mini-2"), ("candidate", "build-mini-2")])
        # A seed that cannot provide Python (a Homebrew one, say) falls back to this Mac's copy.
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(mf, "prepare_seed", return_value="share_source python exit 3"), \
                mock.patch.object(mf, "share_one") as share, mock.patch.object(mf, "ship_python", return_value=(True, "")) as ship, \
                mock.patch.object(mf, "fix_call", return_value=0), mock.patch.object(mf, "stage_candidate", return_value=(True, "")):
            out = mf.fix_host(self.manifest, "build-mini-1", result, True, Path(tmp), seed="build-mini-2")
        share.assert_not_called()
        ship.assert_called_once()
        self.assertEqual(out["failed"], [])

    def test_a_missing_xcode_waits_for_a_seed(self) -> None:
        self.manifest["defaults"]["toolchain"]["xcode"] = {"app": "/Applications/Xcode_26.6.app", "version": "26.6",
                                                           "build": "17F113"}
        obs = observed(**{"build-mini-1": preflight_text()})["hosts"]["build-mini-1"]
        result = mf.preflight_host(self.manifest, "build-mini-1", obs)
        self.assertEqual(result["checks"]["xcode"]["action"]["kind"], "xcode_share")
        waiting = mf.fix_host(self.manifest, "build-mini-1", result, False, None)["waiting"]
        self.assertIn("waits for --from HOST, a host that has it", waiting[0])
        planned = mf.fix_host(self.manifest, "build-mini-1", result, False, None, seed="build-mini-2")["planned"]
        self.assertTrue(planned[0].startswith("xcode: copy /Applications/Xcode_26.6.app"))


class OnboardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.manifest = mf.load_manifest(EXAMPLE)
        self.candidate = {"source": "59ca9c9bd1bb" + "0" * 28, "sha256": "a" * 64, "run": "1", "artifact": "x", "repo": "a/b"}
        patcher = mock.patch.object(mf, "operator_candidate", return_value=self.candidate)
        patcher.start()
        self.addCleanup(patcher.stop)

    def fresh(self, **kwargs: object) -> str:
        """A host that passes every check and is not enrolled or registered yet."""
        return preflight_text(enroll_state=None, acceptance=None, runners=(), node_id=None, **kwargs)

    def test_default_hosts_are_node_ids_on_compile_lane_classes(self) -> None:
        # By default, hosts that could never join (no node id, a light class) are left out silently.
        self.assertEqual(mf.onboard_hosts(self.manifest, None), (["build-mini-1", "build-mini-2"], {}))
        targets, skipped = mf.onboard_hosts(self.manifest, ["small-mini"])
        self.assertEqual((targets, skipped), ([], {"small-mini": "no node_id in the manifest"}))
        self.manifest["hosts"]["build-mini-2"]["hardware"] = "m4-16"  # 16 GB hardware never takes the app compile
        targets, skipped = mf.onboard_hosts(self.manifest, ["build-mini-1", "build-mini-2"])
        self.assertEqual(targets, ["build-mini-1"])
        self.assertIn("compile_lane: false", skipped["build-mini-2"])
        with self.assertRaisesRegex(mf.Failure, "never_touch"):
            mf.onboard_hosts(self.manifest, ["coordinator-mini"])

    def test_class_acceptance_must_match_xcode_build_and_candidate(self) -> None:
        recorded, why = mf.class_acceptance(self.manifest, "build-mini-1")
        self.assertEqual(recorded["node"], "cmux-mac-001")
        self.manifest["defaults"]["toolchain"]["xcode"]["build"] = "17C999"
        recorded, why = mf.class_acceptance(self.manifest, "build-mini-1")
        self.assertIsNone(recorded)
        self.assertIn("not 17C999", why)

    def run_onboard(self, texts: dict[str, str], *extra: str, tokens: list[str] | None = None,
                    manifest: Path = EXAMPLE) -> tuple[int, str, list]:
        calls: list = []

        def ssh(name, user, command, log, stdin=b"", timeout=None, options=()):
            calls.append((name, command, stdin))
            return 0

        obs = observed(**texts)
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()) as out, \
                mock.patch.object(mf, "observe", return_value=obs), mock.patch.object(mf, "ssh_stream", side_effect=ssh), \
                mock.patch.object(mf, "mint_runner_token", return_value="AAAATOKENTOKENTOKENTOKEN") as mint, \
                mock.patch.object(mf, "operator_zig_minimum", return_value="0.16.0"):
            code = mf.main(["onboard", *texts, "--manifest", os.fspath(manifest), "--log-dir", tmp, *extra])
        if tokens is not None:
            tokens.append(mint.call_count)
        return code, out.getvalue(), calls

    def test_dry_run_prints_the_plan_and_touches_nothing(self) -> None:
        code, out, calls = self.run_onboard({"build-mini-1": self.fresh(), "build-mini-2": morning_text()})
        self.assertEqual((code, calls), (0, []))
        self.assertIn("would: enroll without acceptance (class)", out)
        self.assertIn("sudo-plan select, rust, zig", out)
        self.assertIn("dry run; pass --yes", out)
        self.assertEqual(out.splitlines()[0].split(), ["host", "node", "id", "step", "result", "log"])
        self.assertNotIn("—", out)

    def test_class_mode_enrolls_without_acceptance_and_does_not_register(self) -> None:
        minted: list = []
        code, out, calls = self.run_onboard({"build-mini-1": self.fresh()}, "--yes", tokens=minted)
        enroll = [c for c in calls if "glaeda-mini-enroll" in c[1]]
        self.assertEqual(len(enroll), 1)
        self.assertIn("--no-accept", enroll[0][1])
        self.assertIn("--node-id cmux-mac-001", enroll[0][1])
        self.assertIn("~/'glaeda/scripts/glaeda-mini-enroll'", enroll[0][1])
        self.assertEqual(minted, [0])
        self.assertIn("enrolled without node acceptance", out)
        self.assertEqual(code, 1)

    def test_class_receipt_goes_over_stdin_and_the_host_registers(self) -> None:
        receipt = {"schema": "glaeda-cmux-fleet-class-acceptance/v1", "fleetClass": "m4pro-48",
                   "receiptSha256": "sha256:" + "c" * 64}
        with tempfile.TemporaryDirectory() as tmp:
            receipt_path = Path(tmp) / "m4pro-48.json"
            receipt_path.write_text(json.dumps(receipt))
            data = copy.deepcopy(self.manifest)
            data["hardware"]["m4pro-48"]["acceptance"].update(receipt=os.fspath(receipt_path),
                                                             receipt_sha256=receipt["receiptSha256"])
            path = write_manifest(tmp, data)
            code, out, calls = self.run_onboard({"build-mini-1": self.fresh()}, "--yes", manifest=path)
            self.assertEqual(code, 0, out)
            enroll = [c for c in calls if "glaeda-mini-enroll" in c[1]]
            self.assertEqual(len(enroll), 1)
            self.assertNotIn("--no-accept", enroll[0][1])
            self.assertIn(f"--class-receipt - --class-receipt-sha256 {receipt['receiptSha256']} --fleet-class m4pro-48",
                          enroll[0][1])
            self.assertEqual(enroll[0][2], receipt_path.read_bytes())
            self.assertTrue([c for c in calls if "persistent-compile up" in c[1]])
            # A receipt that is not the recorded one blocks the host before anything runs on it.
            data["hardware"]["m4pro-48"]["acceptance"]["receipt_sha256"] = "sha256:" + "d" * 64
            path = write_manifest(tmp, data)
            code, out, calls = self.run_onboard({"build-mini-1": self.fresh()}, "--yes", manifest=path)
        self.assertEqual(code, 1)
        self.assertIn("is not the recorded", out)
        self.assertFalse([c for c in calls if "glaeda-mini-enroll" in c[1]])

    def test_node_mode_accepts_then_registers_with_the_token_on_stdin(self) -> None:
        minted: list = []
        code, out, calls = self.run_onboard({"build-mini-1": self.fresh(), "build-mini-2": self.fresh(hostname="Build-Mini-2")},
                                            "--yes", "--acceptance", "node", tokens=minted)
        self.assertEqual(code, 0, out)
        self.assertEqual(minted, [1])  # one token for the whole run
        enroll = [c for c in calls if "glaeda-mini-enroll" in c[1]]
        self.assertTrue(enroll and all("--no-accept" not in c[1] for c in enroll))
        register = [c for c in calls if "persistent-compile up" in c[1]]
        self.assertEqual(sorted(c[0] for c in register), ["build-mini-1", "build-mini-2"])
        for _, command, stdin in register:
            self.assertEqual(stdin, b"AAAATOKENTOKENTOKENTOKEN\n")
            self.assertNotIn("AAAATOKEN", command)
            self.assertIn("IFS= read -r CMUX_RUNNER_TOKEN", command)
        self.assertNotIn("AAAATOKEN", out)
        rows = [line for line in out.splitlines() if line.startswith("build-mini-")]
        self.assertTrue(all(" register " in line and " ok " in line for line in rows), rows)

    def test_a_host_needing_a_person_stops_at_preflight(self) -> None:
        text = self.fresh(update_running="softwareupdate --install macOS 26.7 --restart")
        code, out, calls = self.run_onboard({"build-mini-1": text}, "--yes")
        self.assertEqual(code, 1)
        self.assertEqual(calls, [])
        self.assertIn("needs a person: update", out)

    def test_sudo_plans_stop_the_host_without_a_terminal(self) -> None:
        text = self.fresh().replace("xcode_select\t/Applications/Xcode.app/Contents/Developer",
                                    "xcode_select\t/Library/Developer/CommandLineTools")
        with mock.patch.object(sys.stdin, "isatty", return_value=False):
            code, out, _ = self.run_onboard({"build-mini-1": text}, "--yes")
        self.assertEqual(code, 1)
        self.assertIn("waiting: glaeda-mini-fleet sudo-plan build-mini-1 --run", out)
        with mock.patch.object(sys.stdin, "isatty", return_value=False), contextlib.redirect_stderr(io.StringIO()) as err:
            code, _, calls = self.run_onboard({"build-mini-1": text}, "--yes", "--sudo", "run")
        self.assertEqual((code, calls), (2, []))
        self.assertIn("from a terminal", err.getvalue())

    def test_heartbeat_reports_each_running_host(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stdout(io.StringIO()) as out:
            log = Path(tmp) / "a.log"
            log.write_text("== enroll\naccept-local: cold cmux build\n")
            results = mf.run_with_heartbeat({"a": lambda: (time.sleep(0.3), 0)[1]}, {"a": log}, "enroll", every=0.1)
        self.assertEqual(results, {"a": 0})
        self.assertIn("heartbeat enroll 0m00s: a: accept-local: cold cmux build", out.getvalue())

    def test_onboarding_fields_are_validated(self) -> None:
        cases = ((("lan",), {"auth": "password"}, "lan.auth"),
                 (("runner",), {"org": "a b"}, "runner.org"),
                 (("hardware", "m4-16", "compile_lane"), "no", "compile_lane"),
                 (("hardware", "m4pro-48", "acceptance"), {"xcode_build": "17C529", "candidate": "x", "node": "cmux-mac-001"},
                  "acceptance needs"),
                 (("hosts", "build-mini-1", "lan_ip"), "mini-1.lan", "lan_ip"))
        for keys, value, error in cases:
            data = copy.deepcopy(self.manifest)
            target = data
            for key in keys[:-1]:
                target = target[key]
            target[keys[-1]] = value
            with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(mf.Failure, error):
                mf.load_manifest(write_manifest(tmp, data))


if __name__ == "__main__":
    unittest.main()
