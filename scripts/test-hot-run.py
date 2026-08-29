#!/usr/bin/env python3
"""Contract tests for scripts/hot-run."""

from __future__ import annotations

import os
import runpy
import shutil
import subprocess
import sys
import tempfile
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
        default_state_root = runpy.run_path(str(HOT_RUN), run_name="hot_run_test")[
            "default_state_root"
        ]
        resident = Path("/tmp/resident")
        first = default_state_root(resident, Path("/tmp/task-a"))
        self.assertEqual(first, default_state_root(resident, Path("/tmp/task-a")))
        self.assertNotEqual(first, default_state_root(resident, Path("/tmp/task-b")))
        self.assertNotIn("resident", first.name)
        self.assertNotIn("task-a", first.name)

    @unittest.skipUnless(shutil.which("bwrap"), "bubblewrap is unavailable")
    def test_task_sees_stable_path_and_target_writes_stay_private(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            resident = fixture / "resident"
            task = fixture / "task"
            state = fixture / "state"
            resident.mkdir()
            subprocess.run(["git", "init", "--quiet"], cwd=resident, check=True)
            (resident / "payload").write_text("resident\n", encoding="utf-8")
            (resident / "target").mkdir()
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
                "--",
                "/bin/sh",
                "-c",
                f'test "$PWD" = "{resident}" && test "$(cat payload)" = task '
                "&& printf private > target/task-output",
            ]
            result = subprocess.run(command, stdin=subprocess.DEVNULL, check=False)
            self.assertEqual(result.returncode, 0)
            self.assertFalse((resident / "target" / "task-output").exists())
            self.assertFalse((task / "target" / "task-output").exists())
            self.assertTrue((state / "target-upper" / "task-output").is_file())

    def test_direct_resident_execution_preserves_failure_status(self) -> None:
        result = subprocess.run(
            [
                sys.executable,
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(ROOT),
                "--task",
                os.fspath(ROOT),
                "--",
                "/bin/sh",
                "-c",
                "exit 17",
            ],
            stdin=subprocess.DEVNULL,
            check=False,
        )
        self.assertEqual(result.returncode, 17)


if __name__ == "__main__":
    unittest.main()
