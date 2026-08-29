#!/usr/bin/env python3
"""Contract tests for scripts/hot-run."""

from __future__ import annotations

import json
import os
import runpy
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
HOT_RUN = ROOT / "scripts" / "hot-run"


class HotRunTests(unittest.TestCase):
    def test_program_resolution_preserves_dispatch_symlinks(self) -> None:
        resolve_program = runpy.run_path(str(HOT_RUN), run_name="hot_run_test")[
            "resolve_program"
        ]
        cargo = shutil.which("cargo")
        if cargo is None:
            self.skipTest("cargo is unavailable")
        self.assertEqual(resolve_program(["cargo"], ROOT)[0], os.path.abspath(cargo))

    def test_default_state_is_stable_and_separates_tasks(self) -> None:
        namespace = runpy.run_path(str(HOT_RUN), run_name="hot_run_test")
        default_state_root = namespace["default_state_root"]
        resident = Path("/tmp/resident")
        caches = namespace["parse_cache_specs"](["target:overlay"])
        first = default_state_root(resident, Path("/tmp/task-a"), caches)
        self.assertEqual(
            first, default_state_root(resident, Path("/tmp/task-a"), caches)
        )
        self.assertNotEqual(
            first, default_state_root(resident, Path("/tmp/task-b"), caches)
        )
        self.assertNotIn("resident", first.name)
        self.assertNotIn("task-a", first.name)

    @unittest.skipUnless(shutil.which("bwrap"), "bubblewrap is unavailable")
    def test_task_sees_stable_path_and_target_writes_stay_private(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            resident = fixture / "resident"
            task = fixture / "task"
            state = fixture / "state"
            measurement = fixture / "measurement.json"
            resident.mkdir()
            subprocess.run(["git", "init", "--quiet"], cwd=resident, check=True)
            (resident / "payload").write_text("resident\n", encoding="utf-8")
            (resident / "target").mkdir()
            (resident / ".venv").mkdir()
            (resident / ".venv" / "dependency").write_text(
                "resident dependency\n", encoding="utf-8"
            )
            subprocess.run(["git", "add", "payload"], cwd=resident, check=True)
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.name=Glaeda test",
                    "-c",
                    "user.email=glaeda-test@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
                cwd=resident,
                check=True,
            )
            subprocess.run(
                ["git", "worktree", "add", "--quiet", "--detach", os.fspath(task)],
                cwd=resident,
                check=True,
            )
            (task / "payload").write_text("task\n", encoding="utf-8")

            command = [
                sys.executable,
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(resident),
                "--task",
                os.fspath(task),
                "--state",
                os.fspath(state),
                "--cache",
                "target:overlay",
                "--cache",
                ".venv:ro",
                "--measurement",
                os.fspath(measurement),
                "--",
                "/bin/sh",
                "-c",
                f'test "$PWD" = "{resident}" && test "$(cat payload)" = task '
                '&& test "$(cat .venv/dependency)" = "resident dependency" '
                "&& printf private > target/task-output",
            ]
            result = subprocess.run(command, stdin=subprocess.DEVNULL, check=False)
            self.assertEqual(result.returncode, 0)
            self.assertFalse((resident / "target" / "task-output").exists())
            self.assertFalse((task / "target" / "task-output").exists())
            uppers = list(state.glob("upper-*"))
            self.assertEqual(len(uppers), 1)
            self.assertTrue((uppers[0] / "task-output").is_file())
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(report["authority"], "developer_observation_only")
            self.assertEqual(report["exit_code"], 0)
            self.assertEqual(report["completion_reason"], "exited")
            self.assertIsNone(report["timeout_seconds"])
            self.assertIsNone(report["resource_profile"])
            self.assertTrue(report["cross_worktree"])
            self.assertGreaterEqual(report["elapsed_seconds"], 0)
            self.assertGreater(report["max_rss_kib"], 0)
            self.assertEqual(
                report["cache_views"],
                [
                    {"mode": "overlay", "path": "target"},
                    {"mode": "ro", "path": ".venv"},
                ],
            )
            encoded = measurement.read_text(encoding="utf-8")
            self.assertNotIn(os.fspath(resident), encoded)
            self.assertNotIn(os.fspath(task), encoded)

    def test_cache_specs_reject_escape_overlap_and_unknown_modes(self) -> None:
        parse_cache_specs = runpy.run_path(str(HOT_RUN), run_name="hot_run_test")[
            "parse_cache_specs"
        ]
        for values in (["../target"], ["target:shared"], ["target", "target:ro"]):
            with self.assertRaises(RuntimeError):
                parse_cache_specs(values)
        with self.assertRaises(RuntimeError):
            parse_cache_specs(["build", "build/generated"])

    def test_direct_resident_execution_preserves_failure_status(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            measurement = Path(directory) / "measurement.json"
            result = subprocess.run(
                [
                    sys.executable,
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    "/bin/sh",
                    "-c",
                    "exit 17",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(result.returncode, 17)
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(report["exit_code"], 17)
            self.assertEqual(report["completion_reason"], "exited")
            self.assertFalse(report["cross_worktree"])
            self.assertEqual(report["cache_views"], [])
            self.assertIsNone(report["resource_profile"])

    def test_timeout_stops_owned_process_group_and_writes_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            measurement = fixture / "measurement.json"
            child_pid = fixture / "child.pid"
            result = subprocess.run(
                [
                    sys.executable,
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--timeout",
                    "0.2",
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    "/bin/sh",
                    "-c",
                    f"sleep 60 & echo $! > {child_pid}; wait",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(result.returncode, 124)
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(report["exit_code"], 124)
            self.assertEqual(report["completion_reason"], "deadline_exceeded")
            self.assertEqual(report["signal"], signal.SIGTERM)
            self.assertEqual(report["timeout_seconds"], 0.2)
            self.assertEqual(
                report["resource_accounting"],
                "unavailable_after_forced_termination",
            )
            self.assertIsNone(report["max_rss_kib"])
            self.assertLess(report["elapsed_seconds"], 3)
            pid = int(child_pid.read_text(encoding="utf-8"))
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)

    def test_operator_interrupt_is_clean_and_writes_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            measurement = Path(directory) / "measurement.json"
            process = subprocess.Popen(
                [
                    sys.executable,
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    "/bin/sleep",
                    "60",
                ],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            time.sleep(0.1)
            os.killpg(process.pid, signal.SIGINT)
            _, stderr = process.communicate(timeout=3)
            self.assertEqual(process.returncode, 130)
            self.assertNotIn("Traceback", stderr)
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(report["exit_code"], 130)
            self.assertEqual(report["completion_reason"], "operator_interrupt")
            self.assertEqual(report["signal"], signal.SIGINT)
            self.assertIsNone(report["user_cpu_seconds"])

    @unittest.skipUnless(shutil.which("systemd-run"), "systemd-run is unavailable")
    def test_heavy_profile_preserves_status_and_is_recorded(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            measurement = Path(directory) / "measurement.json"
            result = subprocess.run(
                [
                    sys.executable,
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--resource-profile",
                    "big-red-heavy",
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    "/bin/true",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            if result.returncode == 2 and not measurement.exists():
                self.skipTest("user systemd scopes are unavailable")
            self.assertEqual(result.returncode, 0)
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(report["resource_profile"], "big-red-heavy")
            self.assertEqual(report["resource_accounting"], "gnu_time_inside_scope")
            self.assertIsInstance(report["user_cpu_seconds"], float)
            self.assertIsInstance(report["system_cpu_seconds"], float)
            self.assertIsInstance(report["max_rss_kib"], int)


if __name__ == "__main__":
    unittest.main()
