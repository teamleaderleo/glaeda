#!/usr/bin/env python3
"""Contract tests for scripts/hot-run."""

from __future__ import annotations

import fcntl
import hashlib
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
from unittest import mock


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
        runtime_state_root = namespace["runtime_state_root"]
        RuntimeContract = namespace["RuntimeContract"]
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
        node_22 = RuntimeContract("node-22", f"sha256:{'1' * 64}")
        node_24 = RuntimeContract("node-24", f"sha256:{'2' * 64}")
        self.assertNotEqual(
            runtime_state_root(first, node_22), runtime_state_root(first, node_24)
        )
        self.assertNotIn("node-22", runtime_state_root(first, node_22).name)

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
                f'&& test "$(git rev-parse --show-toplevel)" = "{resident}" '
                "&& ! git diff --quiet -- payload "
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
            self.assertIsNone(report["runtime"])
            self.assertTrue(report["cross_worktree"])
            self.assertGreaterEqual(report["elapsed_seconds"], 0)
            self.assertGreater(report["max_rss_kib"], 0)
            self.assertEqual(report["state_preparation"], [])
            self.assertEqual(report["preparation_elapsed_seconds"], 0.0)
            self.assertEqual(
                report["command_plus_preparation_elapsed_seconds"],
                report["elapsed_seconds"],
            )
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
            self.assertEqual(list(state.glob(".git-view-*")), [])

    @unittest.skipUnless(shutil.which("bwrap"), "bubblewrap is unavailable")
    def test_private_cache_is_empty_then_persists_at_the_stable_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            resident = fixture / "resident"
            task = fixture / "task"
            state = fixture / "state"
            first_measurement = fixture / "first.json"
            second_measurement = fixture / "second.json"
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

            base_command = [
                sys.executable,
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(resident),
                "--task",
                os.fspath(task),
                "--state",
                os.fspath(state),
                "--cache",
                "target:private",
            ]
            first = subprocess.run(
                [
                    *base_command,
                    "--measurement",
                    os.fspath(first_measurement),
                    "--",
                    "/bin/sh",
                    "-c",
                    f'test "$PWD" = "{resident}" '
                    "&& test ! -e target/generation "
                    "&& printf 1 > target/generation",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(first.returncode, 0)
            second = subprocess.run(
                [
                    *base_command,
                    "--measurement",
                    os.fspath(second_measurement),
                    "--",
                    "/bin/sh",
                    "-c",
                    'test "$(cat target/generation)" = 1 '
                    "&& printf 2 > target/generation",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(second.returncode, 0)
            self.assertFalse((resident / "target" / "generation").exists())
            self.assertFalse((task / "target" / "generation").exists())
            private_directories = list(state.glob("private-*"))
            self.assertEqual(len(private_directories), 1)
            self.assertEqual(
                (private_directories[0] / "generation").read_text(encoding="utf-8"),
                "2",
            )
            for measurement in (first_measurement, second_measurement):
                report = json.loads(measurement.read_text(encoding="utf-8"))
                self.assertEqual(
                    report["cache_views"],
                    [{"mode": "private", "path": "target"}],
                )
                self.assertTrue(report["cross_worktree"])
            self.assertEqual(list(state.glob(".git-view-*")), [])

    @unittest.skipUnless(shutil.which("bwrap"), "bubblewrap is unavailable")
    def test_private_copy_seeds_once_then_reuses_the_lineage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            resident = fixture / "resident"
            task = fixture / "task"
            state = fixture / "state"
            first_measurement = fixture / "first.json"
            second_measurement = fixture / "second.json"
            resident.mkdir()
            subprocess.run(["git", "init", "--quiet"], cwd=resident, check=True)
            (resident / "payload").write_text("resident\n", encoding="utf-8")
            (resident / "target").mkdir()
            (resident / "target" / "parent").write_text(
                "exact warm parent\n", encoding="utf-8"
            )
            (resident / "target").chmod(0o555)
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

            base_command = [
                sys.executable,
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(resident),
                "--task",
                os.fspath(task),
                "--state",
                os.fspath(state),
                "--cache",
                "target:private-copy",
            ]
            first = subprocess.run(
                [
                    *base_command,
                    "--measurement",
                    os.fspath(first_measurement),
                    "--",
                    "/bin/sh",
                    "-c",
                    'test "$(cat target/parent)" = "exact warm parent" '
                    "&& test ! -e target/generation "
                    "&& printf 1 > target/generation",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(first.returncode, 0)
            (resident / "target" / "parent").write_text(
                "changed resident parent\n", encoding="utf-8"
            )
            second = subprocess.run(
                [
                    *base_command,
                    "--measurement",
                    os.fspath(second_measurement),
                    "--",
                    "/bin/sh",
                    "-c",
                    'test "$(cat target/parent)" = "exact warm parent" '
                    '&& test "$(cat target/generation)" = 1 '
                    "&& printf 2 > target/generation",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(second.returncode, 0)

            private_directories = list(state.glob("private-*"))
            self.assertEqual(len(private_directories), 1)
            private = private_directories[0]
            self.assertEqual(private.stat().st_mode & 0o777, 0o700)
            self.assertEqual(
                (private / "parent").read_text(encoding="utf-8"),
                "exact warm parent\n",
            )
            self.assertEqual(
                (private / "generation").read_text(encoding="utf-8"), "2"
            )
            first_report = json.loads(first_measurement.read_text(encoding="utf-8"))
            second_report = json.loads(
                second_measurement.read_text(encoding="utf-8")
            )
            self.assertEqual(
                first_report["cache_views"],
                [{"mode": "private-copy", "path": "target"}],
            )
            self.assertEqual(
                first_report["state_preparation"][0]["disposition"], "seeded"
            )
            self.assertGreater(
                first_report["state_preparation"][0]["elapsed_seconds"], 0
            )
            self.assertGreaterEqual(
                first_report["command_plus_preparation_elapsed_seconds"],
                first_report["elapsed_seconds"],
            )
            self.assertEqual(
                second_report["state_preparation"],
                [
                    {
                        "disposition": "reused",
                        "elapsed_seconds": 0.0,
                        "mode": "private-copy",
                        "path": "target",
                    }
                ],
            )
            self.assertEqual(second_report["preparation_elapsed_seconds"], 0.0)
            self.assertEqual(list(state.glob(".private-*")), [])
            self.assertEqual(list(state.glob(".git-view-*")), [])

    def test_cache_specs_reject_escape_overlap_and_unknown_modes(self) -> None:
        parse_cache_specs = runpy.run_path(str(HOT_RUN), run_name="hot_run_test")[
            "parse_cache_specs"
        ]
        for values in (["../target"], ["target:shared"], ["target", "target:ro"]):
            with self.assertRaises(RuntimeError):
                parse_cache_specs(values)
        self.assertEqual(parse_cache_specs(["target:private"])[0].mode, "private")
        self.assertEqual(
            parse_cache_specs(["target:private-copy"])[0].mode, "private-copy"
        )
        with self.assertRaises(RuntimeError):
            parse_cache_specs(["build", "build/generated"])

    def test_private_copy_failure_never_publishes_candidate(self) -> None:
        namespace = runpy.run_path(str(HOT_RUN), run_name="hot_run_test")
        prepare_private_copy = namespace["prepare_private_copy"]
        CacheSpec = namespace["CacheSpec"]
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            resident = fixture / "resident"
            state = fixture / "state"
            destination = state / "private-target"
            resident.mkdir()
            state.mkdir()
            (resident / "parent").write_text("warm\n", encoding="utf-8")
            failed_copy = mock.Mock(returncode=1)
            with mock.patch.object(
                namespace["subprocess"], "run", return_value=failed_copy
            ):
                with self.assertRaisesRegex(
                    RuntimeError, "private-copy preparation failed"
                ):
                    prepare_private_copy(
                        CacheSpec(Path("target"), "private-copy"),
                        resident,
                        destination,
                    )
            self.assertFalse(destination.exists())
            self.assertEqual(list(state.glob(".private-target.*")), [])

    @unittest.skipUnless(shutil.which("bwrap"), "bubblewrap is unavailable")
    def test_private_copy_refuses_contention_before_seeding(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            resident = fixture / "resident"
            task = fixture / "task"
            state = fixture / "state"
            executed = fixture / "executed"
            resident.mkdir()
            state.mkdir()
            subprocess.run(["git", "init", "--quiet"], cwd=resident, check=True)
            (resident / "payload").write_text("resident\n", encoding="utf-8")
            (resident / "target").mkdir()
            (resident / "target" / "parent").write_text(
                "exact warm parent\n", encoding="utf-8"
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
            lock_fd = os.open(state / "lock", os.O_CREAT | os.O_RDWR, 0o600)
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                result = subprocess.run(
                    [
                        sys.executable,
                        os.fspath(HOT_RUN),
                        "--resident",
                        os.fspath(resident),
                        "--task",
                        os.fspath(task),
                        "--state",
                        os.fspath(state),
                        "--cache",
                        "target:private-copy",
                        "--",
                        "/bin/sh",
                        "-c",
                        f"printf executed > {executed}",
                    ],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    check=False,
                )
            finally:
                os.close(lock_fd)
            self.assertEqual(result.returncode, 2)
            self.assertIn("hot state is already in use", result.stderr)
            self.assertFalse(executed.exists())
            self.assertEqual(list(state.glob("private-copy-*")), [])
            self.assertEqual(list(state.glob(".private-copy-*")), [])

    def test_runtime_contract_verifies_before_execution_and_is_recorded(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            program = fixture / "runtime"
            output = fixture / "output"
            measurement = fixture / "measurement.json"
            program.write_text(
                '#!/bin/sh\nprintf verified > "$1"\n', encoding="utf-8"
            )
            program.chmod(0o700)
            digest = hashlib.sha256(program.read_bytes()).hexdigest()
            base_command = [
                sys.executable,
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(ROOT),
                "--task",
                os.fspath(ROOT),
                "--runtime-id",
                "test-runtime-v1",
                "--runtime-sha256",
            ]
            accepted = subprocess.run(
                [
                    *base_command,
                    f"sha256:{digest}",
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    os.fspath(program),
                    os.fspath(output),
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(accepted.returncode, 0)
            self.assertEqual(output.read_text(encoding="utf-8"), "verified")
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(
                report["runtime"],
                {
                    "id": "test-runtime-v1",
                    "program_sha256": f"sha256:{digest}",
                },
            )
            self.assertNotIn(os.fspath(fixture), measurement.read_text(encoding="utf-8"))

            output.unlink()
            refused = subprocess.run(
                [
                    *base_command,
                    f"sha256:{'0' * 64}",
                    "--",
                    os.fspath(program),
                    os.fspath(output),
                ],
                stdin=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )
            self.assertEqual(refused.returncode, 2)
            self.assertFalse(output.exists())
            self.assertIn("does not match declared digest", refused.stderr)
            self.assertNotIn(os.fspath(program), refused.stderr)

    def test_runtime_id_alone_observes_exact_executable_and_records_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            program = fixture / "runtime"
            output = fixture / "output"
            measurement = fixture / "measurement.json"
            program.write_text(
                '#!/bin/sh\nprintf observed > "$1"\n', encoding="utf-8"
            )
            program.chmod(0o700)
            digest = hashlib.sha256(program.read_bytes()).hexdigest()

            result = subprocess.run(
                [
                    sys.executable,
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--runtime-id",
                    "test-runtime-current",
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    os.fspath(program),
                    os.fspath(output),
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(result.returncode, 0)
            self.assertEqual(output.read_text(encoding="utf-8"), "observed")
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(
                report["runtime"],
                {
                    "id": "test-runtime-current",
                    "program_sha256": f"sha256:{digest}",
                },
            )
            self.assertNotIn(os.fspath(fixture), measurement.read_text(encoding="utf-8"))

    def test_runtime_contract_rejects_digest_without_id_and_noncanonical_values(
        self,
    ) -> None:
        parse_runtime_contract = runpy.run_path(
            str(HOT_RUN), run_name="hot_run_test"
        )["parse_runtime_contract"]
        for runtime_id, digest in (
            (None, f"sha256:{'1' * 64}"),
            ("../node", f"sha256:{'1' * 64}"),
            ("../node", None),
            ("node-22", "1" * 64),
            ("node-22", f"sha256:{'A' * 64}"),
        ):
            with self.assertRaises(RuntimeError):
                parse_runtime_contract(runtime_id, digest)

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
            fixture = Path(directory)
            measurement = fixture / "measurement.json"
            ready = fixture / "ready"
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
                    "/bin/sh",
                    "-c",
                    f"printf ready > {ready}; exec /bin/sleep 60",
                ],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            readiness_deadline = time.monotonic() + 3
            while not ready.exists() and time.monotonic() < readiness_deadline:
                time.sleep(0.01)
            self.assertTrue(ready.exists(), "child command did not become ready")
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
