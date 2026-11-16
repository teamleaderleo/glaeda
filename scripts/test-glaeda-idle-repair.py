#!/usr/bin/env python3
"""Focused contract tests for the idle repair queue."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("idle_repair", str(ROOT / "scripts/glaeda-idle-repair"))
spec = importlib.util.spec_from_loader("idle_repair", loader)
idle = importlib.util.module_from_spec(spec)
loader.exec_module(idle)


class IdleRepairTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        home = Path(self.tmp.name)
        idle.HOME = home
        idle.STATE_DIR = home / ".local/state/glaeda/idle-repair"
        idle.STATE = idle.STATE_DIR / "queue.json"
        idle.RECEIPT = idle.STATE_DIR / "receipts.jsonl"
        idle.LOCK = idle.STATE_DIR / "run.lock"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def test_busy_host_is_queued_without_running_repairs(self) -> None:
        disk = mock.Mock()
        disk.host_busy.return_value = "a build is running"
        with mock.patch.object(idle, "load_disk_module", return_value=disk), \
             mock.patch.object(idle, "run") as run:
            self.assertEqual(idle.main(["--apply"]), idle.QUEUED_EXIT)
        run.assert_not_called()
        state = json.loads(idle.STATE.read_text())
        self.assertEqual(state["state"], "queued")
        self.assertEqual(state["reason"], "a build is running")

    def test_idle_host_runs_disk_then_health_and_records_receipt(self) -> None:
        disk = mock.Mock()
        disk.host_busy.return_value = ""
        with mock.patch.dict(idle.os.environ, {"GLAEDA_ROLE": "devbox"}), \
             mock.patch.object(idle, "load_disk_module", return_value=disk), \
             mock.patch.object(idle, "run", side_effect=[(0, "disk ok", ""), (0, "health ok", "")]) as run:
            self.assertEqual(idle.main(["--apply"]), 0)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(run.call_args_list[0].args[0][2:4], ["--role", "devbox"])
        state = json.loads(idle.STATE.read_text())
        self.assertEqual(state["state"], "idle")
        self.assertEqual([a["exit"] for a in state["actions"]], [0, 0])
        self.assertEqual([call.args[1] for call in run.call_args_list], [660, 180])

    def test_successful_repair_waits_until_next_attempt(self) -> None:
        disk = mock.Mock()
        disk.host_busy.return_value = ""
        with mock.patch.object(idle, "load_disk_module", return_value=disk), \
             mock.patch.object(idle, "run", side_effect=[(0, "disk ok", ""), (0, "health ok", "")]) as run, \
             mock.patch.object(idle.time, "time", side_effect=[1000, 1001]):
            self.assertEqual(idle.main(["--apply"]), 0)
            self.assertEqual(idle.main(["--apply"]), 0)
        self.assertEqual(run.call_count, 2)

    def test_status_is_read_only(self) -> None:
        idle.write_state({"state": "queued", "reason": "reservation"})
        with mock.patch.object(idle, "load_disk_module") as load:
            self.assertEqual(idle.main(["--status"]), 0)
        load.assert_not_called()

    def test_idle_runs_worktree_reclaim_after_disk_and_health(self) -> None:
        disk = mock.Mock()
        disk.host_busy.return_value = ""
        reclaim = self.tmp.name + "/reclaim"
        Path(reclaim).write_text("#!/bin/sh\n")
        with mock.patch.object(idle, "load_disk_module", return_value=disk), \
             mock.patch.dict(idle.os.environ, {"GLAEDA_WORKTREE_RECLAIM_ALL": reclaim}), \
             mock.patch.object(idle, "run", side_effect=[(0, "disk", ""), (0, "health", ""), (0, "reclaim", "")]) as run:
            self.assertEqual(idle.main(["--apply"]), 0)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(run.call_args_list[-1].args[0][0:4], ["/bin/bash", reclaim, "--apply", "--output"])


class LoadDiskModuleTest(unittest.TestCase):
    # The tests above mock load_disk_module, so none of them saw it fail on Python 3.15 (SourceFileLoader.load_module
    # was removed): from 10-18 every run failed there, 70,782 tracebacks on one host and nothing alarmed.
    def test_it_loads_an_extensionless_script(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            disk = Path(tmp) / "glaeda-disk"
            disk.write_text("def host_busy():\n    return None\n")
            with mock.patch.dict("os.environ", {"GLAEDA_DISK": str(disk)}):
                module = idle.load_disk_module()
        self.assertIsNone(module.host_busy())

    def test_no_tracked_file_calls_the_removed_load_module(self) -> None:
        import subprocess
        removed = ".load" + "_module("
        hits = subprocess.run(["git", "-C", str(ROOT), "grep", "-n", "-F", removed, "--", "scripts", ".github"],
                              capture_output=True, text=True).stdout.strip()
        self.assertEqual(hits, "", "SourceFileLoader's load_module is gone in Python 3.15; use "
                         "importlib.util.module_from_spec(spec_from_loader(name, loader)) and loader.exec_module:\n"
                         + hits)


if __name__ == "__main__":
    unittest.main()
