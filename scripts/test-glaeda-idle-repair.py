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
        with mock.patch.object(idle, "load_disk_module", return_value=disk) as load, \
             mock.patch.object(idle, "run", side_effect=[(0, "disk ok", ""), (0, "health ok", "")]) as run, \
             mock.patch.object(idle.time, "time", side_effect=[1000, 1001]):
            self.assertEqual(idle.main(["--apply"]), 0)
            self.assertEqual(idle.main(["--apply"]), 0)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(load.call_count, 1, "the not-due tick never loads glaeda-disk")

    def test_tick_before_next_attempt_loads_nothing_and_keeps_the_schedule(self) -> None:
        """14 of 15 ticks are not due: they must not import glaeda-disk (~730 KB) or run its host gate, and a
        busy host must not turn the 15-minute schedule into "queued, due in 60 s"."""
        idle.write_state({"state": "idle", "lastAttemptAt": 1000, "nextAttemptAt": 1900})
        before = idle.STATE.read_bytes()
        with mock.patch.object(idle, "load_disk_module") as load, \
             mock.patch.object(idle, "run") as run, \
             mock.patch.object(idle.time, "time", return_value=1500):
            self.assertEqual(idle.main(["--apply"]), 0)
        load.assert_not_called()
        run.assert_not_called()
        self.assertEqual(idle.STATE.read_bytes(), before)
        self.assertFalse(idle.RECEIPT.exists())

    def test_queued_repair_is_due_on_the_next_tick_despite_launchd_jitter(self) -> None:
        disk = mock.Mock()
        disk.host_busy.side_effect = ["a build is running", ""]
        with mock.patch.object(idle, "load_disk_module", return_value=disk), \
             mock.patch.object(idle, "run", side_effect=[(0, "disk ok", ""), (0, "health ok", "")]) as run, \
             mock.patch.object(idle.time, "time", side_effect=[1000, 1059]):
            self.assertEqual(idle.main(["--apply"]), idle.QUEUED_EXIT)
            self.assertEqual(idle.main(["--apply"]), 0)
        self.assertEqual(disk.host_busy.call_count, 2)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(json.loads(idle.STATE.read_text())["nextAttemptAt"], 1059 + 900)

    def test_timed_out_repair_waits_a_full_interval_after_it_was_stopped(self) -> None:
        disk = mock.Mock()
        disk.host_busy.return_value = ""
        with mock.patch.object(idle, "load_disk_module", return_value=disk), \
             mock.patch.object(idle, "run", return_value=(idle.TIMED_OUT, "", "timed out")), \
             mock.patch.object(idle.time, "time", return_value=1000):
            self.assertEqual(idle.main(["--apply"]), 1)
        state = json.loads(idle.STATE.read_text())
        self.assertEqual((state["state"], state["nextAttemptAt"]), ("retry", 1000 + 660 + idle.REPAIR_EVERY_S))

    def test_other_failure_retries_on_the_next_tick(self) -> None:
        disk = mock.Mock()
        disk.host_busy.return_value = ""
        with mock.patch.object(idle, "load_disk_module", return_value=disk), \
             mock.patch.object(idle, "run", return_value=(1, "", "boom")), \
             mock.patch.object(idle.time, "time", return_value=1000):
            self.assertEqual(idle.main(["--apply"]), 1)
        self.assertEqual(json.loads(idle.STATE.read_text())["nextAttemptAt"], 1000 + idle.RETRY_AFTER_S)

    def test_status_is_read_only(self) -> None:
        idle.write_state({"state": "queued", "reason": "reservation"})
        with mock.patch.object(idle, "load_disk_module") as load:
            self.assertEqual(idle.main(["--status"]), 0)
        load.assert_not_called()


class RunTest(unittest.TestCase):
    def test_timeout_stops_the_whole_process_group(self) -> None:
        """A timed-out repair's children (glaeda-disk's git and du) stop with it; only the group run() made."""
        import os
        import sys
        import time
        with tempfile.TemporaryDirectory() as tmp:
            pid_file = Path(tmp) / "child.pid"
            script = ("import subprocess, sys, time\n"
                      "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
                      f"open({str(pid_file)!r}, 'w').write(str(child.pid))\n"
                      "time.sleep(60)\n")
            started = time.monotonic()
            code, _, err = idle.run([sys.executable, "-c", script], timeout=2)
            self.assertEqual((code, err), (idle.TIMED_OUT, "timed out"))
            self.assertLess(time.monotonic() - started, 20)
            child = int(pid_file.read_text())
            import subprocess
            for _ in range(50):
                try:
                    os.kill(child, 0)
                except ProcessLookupError:
                    break
                stat = subprocess.run(["ps", "-o", "stat=", "-p", str(child)], capture_output=True, text=True).stdout
                if not stat.strip() or stat.strip().startswith("Z"):  # a zombie a container's pid 1 never reaps
                    break
                time.sleep(0.1)
            else:
                self.fail(f"grandchild {child} outlived the timed-out repair")

    def test_output_and_exit_are_returned(self) -> None:
        import sys
        code, out, err = idle.run([sys.executable, "-c", "import sys; print('ok'); sys.stderr.write('e'); sys.exit(3)"], 30)
        self.assertEqual((code, out, err), (3, "ok\n", "e"))


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
