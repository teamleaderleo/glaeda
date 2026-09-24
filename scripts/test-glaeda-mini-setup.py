#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-mini-setup. Runs on Linux CI: host probes are stubbed."""

from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import plistlib
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader(
    "glaeda_mini_setup", os.fspath(ROOT / "scripts" / "glaeda-mini-setup")
)
spec = importlib.util.spec_from_loader("glaeda_mini_setup", loader)
ms = importlib.util.module_from_spec(spec)
sys.modules["glaeda_mini_setup"] = ms
loader.exec_module(ms)
ms.DARWIN_REQUIRED = False

PMSET_SLEEPY = """Battery Power:
 sleep                1
AC Power:
 womp                 0
 sleep                10
 autorestart          0
"""
PMSET_READY = """AC Power:
 womp                 1
 sleep                0
 autorestart          1
"""


def fake_preflight(power: dict | None = None, blocking: list[str] | None = None):
    def probe(ctx):
        checks = {name: ms.check(True, "stub") for name in (
            "macos", "appleSilicon", "macMini", "hardware", "buildLargeClass", "commandLineTools",
            "xcode", "xcodeSelected", "xcodeUsable", "cmuxXcodePin", "diskFree", "tailscale",
            "homebrew", "python", "workloadToolPath", "power")}
        return {
            "checks": checks,
            "hardware": {},
            "tailscale": {"installed": True, "running": True},
            "power": power or {"readable": True, "acSleepDisabled": True, "wakeOnNetwork": True, "autoRestart": True},
            "xcode": {"apps": [], "selected": ms.CMUX_XCODE_APP + "/Contents/Developer", "licenceNeeded": False},
            "blocking": blocking or [],
        }
    return probe


class MiniSetupTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.home = Path(os.path.realpath(self.tmp.name)) / "operator"
        self.home.mkdir()
        self.reclaim = Path(self.tmp.name) / "glaeda-worktree-reclaim"
        self.reclaim.write_bytes(b"\x7fELF fake reclaim binary\n")
        self.calls: list[list[str]] = []
        real_run = ms.run

        def recording_run(argv, *args, **kwargs):
            self.calls.append(list(argv))
            if argv[0] == "/bin/launchctl":
                raise AssertionError("launchctl must never run with a sandbox HOME")
            return real_run(argv, *args, **kwargs)

        self.patches = [
            mock.patch.object(ms, "preflight", fake_preflight()),
            mock.patch.object(ms, "run", recording_run),
            mock.patch.dict(os.environ, {"HOME": os.fspath(self.home)}),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self) -> None:
        for p in reversed(self.patches):
            p.stop()
        self.tmp.cleanup()

    def invoke(self, *args: str) -> dict:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ms.main(["--output", "json", "--python", "/usr/bin/python3",
                            "--reclaim-binary", os.fspath(self.reclaim), *args])
        self.assertEqual(code, 0, out.getvalue()[-2000:])
        return json.loads(out.getvalue())

    def tree(self) -> dict[str, bytes | None]:
        return {
            os.fspath(p.relative_to(self.home)): (p.read_bytes() if p.is_file() else None)
            for p in sorted(self.home.rglob("*"))
        }

    def states(self, receipt: dict) -> set[str]:
        return {a["state"] for a in receipt["actions"]}

    def test_plan_is_default_and_writes_nothing(self) -> None:
        receipt = self.invoke()
        self.assertFalse(receipt["applied"])
        self.assertEqual(self.tree(), {})
        self.assertIn("create", self.states(receipt))
        self.assertTrue(receipt["sandboxHome"])

    def test_apply_installs_templated_for_user_then_is_idempotent(self) -> None:
        first = self.invoke("--apply")
        self.assertTrue(first["applied"])
        bin_dir = self.home / ".local/bin"
        for name in ("glaeda-disk", "glaeda-worktree-reclaim", "glaeda-worktree-reclaim-all"):
            self.assertTrue(os.access(bin_dir / name, os.X_OK), name)
        self.assertEqual((bin_dir / "glaeda-disk").read_bytes(), (ROOT / "scripts/glaeda-disk").read_bytes())
        for label in ("disk-pressure", "disk-dedupe", "worktree-reclaim"):
            path = self.home / f"Library/LaunchAgents/com.teamleaderleo.glaeda.{label}.plist"
            raw = path.read_bytes()
            self.assertNotIn(b"/Users/leoli", raw)
            doc = plistlib.loads(raw)
            self.assertEqual(doc["Label"], f"com.teamleaderleo.glaeda.{label}")
            for arg in doc["ProgramArguments"][1:2]:
                self.assertTrue(arg.startswith(os.fspath(bin_dir)), arg)
            self.assertTrue(doc["StandardOutPath"].startswith(os.fspath(self.home / "Library/Logs")))
        dedupe = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.disk-dedupe.plist").read_bytes())
        self.assertEqual(dedupe["WatchPaths"], [os.fspath(self.home / "Library/Developer/Xcode/DerivedData")])
        self.assertTrue((self.home / "Library/Developer/Xcode/DerivedData").is_dir())
        self.assertEqual((self.home / ".cache/glaeda/cmux-native-cache").stat().st_mode & 0o777, 0o700)
        receipt_path = self.home / ".local/state/glaeda/mini-setup/receipt.json"
        self.assertEqual(receipt_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(json.loads(receipt_path.read_text())["schema"], "glaeda-mini-setup/v1")

        before = self.tree()
        second = self.invoke("--apply")
        self.assertEqual(self.states(second), {"unchanged"})
        after = self.tree()
        after.pop(".local/state/glaeda/mini-setup/receipt.json")
        before.pop(".local/state/glaeda/mini-setup/receipt.json")
        self.assertEqual(before, after)

    def test_hand_written_plist_with_same_content_is_unchanged(self) -> None:
        self.invoke("--apply")
        path = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.disk-pressure.plist"
        path.write_bytes(plistlib.dumps(plistlib.loads(path.read_bytes()), fmt=plistlib.FMT_XML, sort_keys=False))
        path.write_text(path.read_text().replace("<dict>", "<dict>\n  <!-- hand written -->", 1))
        receipt = self.invoke()
        state = {a.get("label"): a["state"] for a in receipt["actions"] if a["kind"] == "agent"}
        self.assertEqual(state["com.teamleaderleo.glaeda.disk-pressure"], "unchanged")

    def test_update_keeps_a_backup(self) -> None:
        (self.home / ".local/bin").mkdir(parents=True)
        old = self.home / ".local/bin/glaeda-disk"
        old.write_text("#!/bin/sh\necho old\n")
        receipt = self.invoke("--apply")
        act = next(a for a in receipt["actions"] if a.get("path") == os.fspath(old))
        self.assertEqual(act["state"], "update")
        self.assertEqual(Path(act["backup"]).read_text(), "#!/bin/sh\necho old\n")

    def test_git_defaults_only_fill_unset_keys_and_uninstall_only_ours(self) -> None:
        (self.home / ".gitconfig").write_text("[fetch]\n\tparallel = 4\n")
        receipt = self.invoke("--apply")
        git = {a["key"]: a for a in receipt["actions"] if a["kind"] == "git"}
        self.assertEqual(git["fetch.parallel"]["state"], "kept")
        self.assertEqual(git["core.untrackedCache"]["state"], "create")
        self.invoke("--apply")  # the second run must not forget what the first one set
        self.invoke("--uninstall", "--apply")
        text = (self.home / ".gitconfig").read_text()
        self.assertIn("parallel = 4", text)
        self.assertNotIn("untrackedCache", text)
        self.assertNotIn("fetchJobs", text)

    def test_uninstall_plan_then_apply_removes_only_unmodified_files(self) -> None:
        self.invoke("--apply")
        edited = self.home / ".local/bin/glaeda-worktree-reclaim-all"
        edited.write_text(edited.read_text() + "# operator edit\n")
        before = self.tree()
        plan = self.invoke("--uninstall")
        self.assertEqual(before, self.tree())
        by_path = {a.get("path"): a["state"] for a in plan["actions"]}
        self.assertEqual(by_path[os.fspath(edited)], "kept")
        self.assertEqual(by_path[os.fspath(self.home / ".local/bin/glaeda-disk")], "remove")
        self.invoke("--uninstall", "--apply")
        self.assertTrue(edited.exists())
        self.assertFalse((self.home / ".local/bin/glaeda-disk").exists())
        self.assertEqual(list((self.home / "Library/LaunchAgents").iterdir()), [])
        self.assertTrue((self.home / ".local/state/glaeda/mini-setup/uninstall-receipt.json").is_file())

    def test_nothing_runs_sudo_and_privileged_steps_are_operator_steps(self) -> None:
        with mock.patch.object(ms, "preflight", fake_preflight(
                power={"readable": True, "acSleepDisabled": False, "wakeOnNetwork": False, "autoRestart": False})):
            receipt = self.invoke("--apply")
        self.assertFalse(any("sudo" in argv for argv in self.calls))
        commands = [s["command"] for s in receipt["operatorSteps"]]
        self.assertIn("sudo pmset -c sleep 0", commands)
        self.assertIn("sudo pmset -c womp 1", commands)
        self.assertIn("sudo pmset -a autorestart 1", commands)
        self.assertTrue(any("persistent-compile up" in c for c in commands))
        source = (ROOT / "scripts/glaeda-mini-setup").read_text()
        self.assertIsNone(re.search(r'\[\s*"sudo"', source))

    def test_reserved_slots_register_nothing(self) -> None:
        receipt = self.invoke("--apply")
        self.assertEqual(receipt["reserved"]["compilationCacheNode"]["state"], "reserved")
        self.assertEqual(receipt["reserved"]["runnerRegistration"]["state"], "not_performed")
        self.assertEqual(receipt["reserved"]["fleetEnrollment"]["state"], "not_run")

    def test_missing_reclaim_source_blocks_that_action_only(self) -> None:
        self.reclaim.unlink()
        receipt = self.invoke()
        act = next(a for a in receipt["actions"] if a.get("path", "").endswith("/glaeda-worktree-reclaim"))
        self.assertEqual(act["state"], "blocked")
        self.assertFalse(receipt["ready"])

    def test_uninstall_without_receipt_removes_nothing(self) -> None:
        self.invoke("--apply")
        (self.home / ".local/state/glaeda/mini-setup/receipt.json").unlink()
        before = self.tree()
        plan = self.invoke("--uninstall", "--apply")
        self.assertEqual({a["state"] for a in plan["actions"] if a["kind"] in {"tool", "agent", "git"}} - {"absent"}, {"kept"})
        self.assertTrue(any("nothing is removed" in a.get("note", "") for a in plan["actions"]))
        after = self.tree()
        after.pop(".local/state/glaeda/mini-setup/uninstall-receipt.json")
        self.assertEqual(before, after)

    def test_preexisting_identical_file_is_unchanged_and_never_removed(self) -> None:
        (self.home / ".local/bin").mkdir(parents=True)
        disk = self.home / ".local/bin/glaeda-disk"
        disk.write_bytes((ROOT / "scripts/glaeda-disk").read_bytes())
        disk.chmod(0o755)
        first = self.invoke("--apply")
        act = next(a for a in first["actions"] if a.get("path") == os.fspath(disk))
        self.assertEqual((act["state"], act["owned"]), ("unchanged", False))
        self.invoke("--apply")
        plan = self.invoke("--uninstall", "--apply")
        by_path = {a.get("path"): a["state"] for a in plan["actions"]}
        self.assertEqual(by_path[os.fspath(disk)], "kept")
        self.assertTrue(disk.exists())
        self.assertFalse((self.home / ".local/bin/glaeda-worktree-reclaim-all").exists())

    def test_blocked_rerun_keeps_ownership_of_earlier_install(self) -> None:
        self.invoke("--apply")
        self.reclaim.unlink()  # the re-run cannot find --reclaim-binary
        rerun = self.invoke("--apply")
        by_path = {a.get("path"): a for a in rerun["actions"]}
        binary = self.home / ".local/bin/glaeda-worktree-reclaim"
        agent = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.worktree-reclaim.plist"
        for path in (binary, agent):
            self.assertEqual(by_path[os.fspath(path)]["state"], "blocked")
            self.assertTrue(by_path[os.fspath(path)]["owned"], path)
        self.invoke("--apply")  # a second blocked run must not lose it either
        plan = self.invoke("--uninstall", "--apply")
        states = {a.get("path"): a["state"] for a in plan["actions"]}
        self.assertEqual(states[os.fspath(binary)], "remove")
        self.assertEqual(states[os.fspath(agent)], "remove")
        self.assertFalse(binary.exists())
        self.assertFalse(agent.exists())

    def test_reclaim_agent_blocked_without_binary(self) -> None:
        self.reclaim.unlink()
        receipt = self.invoke("--apply")
        agent = next(a for a in receipt["actions"] if a.get("label") == "com.teamleaderleo.glaeda.worktree-reclaim")
        self.assertEqual(agent["state"], "blocked")
        self.assertFalse((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.worktree-reclaim.plist").exists())

    def test_cargo_build_skipped_when_binary_unchanged(self) -> None:
        target = Path(self.tmp.name) / "target"
        (target / "release").mkdir(parents=True)
        (target / "release/glaeda-worktree-reclaim").write_bytes(self.reclaim.read_bytes())
        (self.home / ".local/bin").mkdir(parents=True)
        installed = self.home / ".local/bin/glaeda-worktree-reclaim"
        installed.write_bytes(self.reclaim.read_bytes())
        installed.chmod(0o755)
        builds: list[int] = []
        with mock.patch.dict(os.environ, {"CARGO_TARGET_DIR": os.fspath(target)}), \
                mock.patch.object(ms, "find_cargo", lambda ctx: "/bin/false"), \
                mock.patch.object(ms, "build_reclaim", lambda ctx: builds.append(1) or (1, "")):
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.assertEqual(ms.main(["--output", "json", "--python", "/usr/bin/python3", "--apply"]), 0)
        self.assertEqual(builds, [])
        act = next(a for a in json.loads(out.getvalue())["actions"] if a.get("path") == os.fspath(installed))
        self.assertEqual(act["state"], "unchanged")

    def test_fleet_bootstrap_non_json_is_recorded(self) -> None:
        glaeda = Path(self.tmp.name) / "glaeda"
        glaeda.write_text("")
        (self.home / ".cache/glaeda/cmux-native-cache").mkdir(parents=True)
        done = mock.Mock(returncode=0, stdout="warning: not json\n", stderr="")
        with mock.patch.object(ms.subprocess, "run", return_value=done):
            receipt = self.invoke("--cmux-root", self.tmp.name, "--glaeda", os.fspath(glaeda))
        slot = receipt["reserved"]["fleetEnrollment"]
        self.assertEqual(slot["state"], "unparsed")
        self.assertIn("not json", slot["raw"])

    def test_bootout_waits_and_bootstrap_retries_eio(self) -> None:
        calls: list[tuple[str, ...]] = []
        answers = {"print": [(0, ""), (0, ""), (113, "")], "bootstrap": [(5, "Bootstrap failed: 5: Input/output error"), (0, "")]}

        def fake(ctx, *args):
            calls.append(args)
            queue = answers.get(args[0])
            return queue.pop(0) if queue else (0, "")

        ctx = mock.Mock(uid=501)
        with mock.patch.object(ms, "launchctl", fake), mock.patch.object(ms.time, "sleep", lambda s: None):
            self.assertTrue(ms.bootout(ctx, "x"))
            self.assertEqual(ms.bootstrap(ctx, "/p.plist")[0], 0)
        self.assertEqual([c[0] for c in calls], ["bootout", "print", "print", "print", "bootstrap", "bootstrap"])

    def test_power_parser_reuses_fleet_rule(self) -> None:
        with mock.patch.object(ms, "run", lambda argv, **kw: (0, PMSET_SLEEPY)):
            self.assertEqual(ms.power(), {"readable": True, "acSleepDisabled": False,
                                          "wakeOnNetwork": False, "autoRestart": False})
        with mock.patch.object(ms, "run", lambda argv, **kw: (0, PMSET_READY)):
            self.assertEqual(ms.power(), {"readable": True, "acSleepDisabled": True,
                                          "wakeOnNetwork": True, "autoRestart": True})

    def test_no_em_dashes(self) -> None:
        for name in ("glaeda-mini-setup", "glaeda-worktree-reclaim-all", "test-glaeda-mini-setup.py"):
            self.assertNotIn(chr(0x2014), (ROOT / "scripts" / name).read_text(), name)


if __name__ == "__main__":
    unittest.main()
