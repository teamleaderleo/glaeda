#!/usr/bin/env python3
"""Contract tests for scripts/hot-run."""

from __future__ import annotations

import fcntl
import hashlib
import json
import os
import resource
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
HOT_RUN_IMPLEMENTATION = ROOT / "scripts" / "hot-run-python"


def load_hot_run() -> dict[str, object]:
    return runpy.run_path(str(HOT_RUN_IMPLEMENTATION), run_name="hot_run_test")


class HotRunTests(unittest.TestCase):
    def test_front_door_fast_path_avoids_python_and_preserves_context(self) -> None:
        environment = os.environ.copy()
        environment["PYTHONHOME"] = "/definitely-absent-glaeda-python-home"
        environment["GLAEDA_FAST_PATH_TEST"] = "expected"
        result = subprocess.run(
            [
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(ROOT),
                "--task",
                os.fspath(ROOT),
                "--cache",
                "target:native",
                "--",
                "/bin/sh",
                "-c",
                'test "$PWD" = "$1" && test "$GLAEDA_FAST_PATH_TEST" = expected && exit 17',
                "glaeda-fast-path",
                os.fspath(ROOT),
            ],
            env=environment,
            stdin=subprocess.DEVNULL,
            check=False,
        )
        self.assertEqual(result.returncode, 17)

    def test_front_door_falls_back_for_observed_work(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            fake_bin = fixture / "bin"
            fake_bin.mkdir()
            marker = fixture / "fallback"
            fake_python = fake_bin / "python3"
            fake_python.write_text(
                '#!/bin/sh\nprintf fallback > "$GLAEDA_FALLBACK_MARKER"\nexit 23\n',
                encoding="utf-8",
            )
            fake_python.chmod(0o700)
            environment = os.environ.copy()
            environment["PATH"] = os.pathsep.join(
                (os.fspath(fake_bin), "/usr/bin", "/bin")
            )
            environment["GLAEDA_FALLBACK_MARKER"] = os.fspath(marker)
            result = subprocess.run(
                [
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--measurement",
                    os.fspath(fixture / "measurement.json"),
                    "--",
                    "/bin/true",
                ],
                env=environment,
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(result.returncode, 23)
            self.assertEqual(marker.read_text(encoding="utf-8"), "fallback")

            marker.unlink()
            environment["GIT_DIR"] = "/definitely-foreign-git-directory"
            result = subprocess.run(
                [
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--",
                    "/bin/true",
                ],
                env=environment,
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(result.returncode, 23)
            self.assertEqual(marker.read_text(encoding="utf-8"), "fallback")

        invalid_cache = subprocess.run(
            [
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(ROOT),
                "--task",
                os.fspath(ROOT),
                "--cache",
                "target:overlay",
                "--",
                "/bin/true",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(invalid_cache.returncode, 2)
        self.assertIn("same-worktree cache observations", invalid_cache.stderr)

    def test_unmeasured_command_does_not_observe_machine(self) -> None:
        namespace = load_hot_run()
        execute = namespace["execute"]
        observer = mock.Mock(side_effect=AssertionError("unexpected observation"))
        resolver = mock.Mock(side_effect=AssertionError("unexpected resolution"))
        with (
            mock.patch.dict(execute.__globals__, {"observe_machine": observer}),
            mock.patch.object(execute.__globals__["shutil"], "which", resolver),
        ):
            result = execute(
                ["/bin/true"],
                None,
                None,
                None,
                (),
                (),
                None,
                None,
                False,
                None,
                None,
                None,
            )
        self.assertEqual(result, 0)
        observer.assert_not_called()
        resolver.assert_not_called()

    def test_measured_command_rss_excludes_prior_children(self) -> None:
        namespace = load_hot_run()
        execute = namespace["execute"]
        with tempfile.TemporaryDirectory() as directory:
            measurement = Path(directory) / "measurement.json"
            subprocess.run(
                [
                    sys.executable,
                    "-c",
                    "payload = bytearray(128 * 1024 * 1024); print(payload[-1])",
                ],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                check=True,
            )
            prior_child_peak = resource.getrusage(
                resource.RUSAGE_CHILDREN
            ).ru_maxrss
            result = execute(
                ["/bin/true"],
                None,
                None,
                measurement,
                (),
                (),
                None,
                None,
                False,
                None,
                None,
                None,
            )

            self.assertEqual(result, 0)
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(report["schema_version"], 6)
            self.assertEqual(report["resource_accounting"], "gnu_time_command_tree")
            self.assertGreater(prior_child_peak, 100 * 1024)
            self.assertGreater(report["max_rss_kib"], 0)
            self.assertLess(report["max_rss_kib"], prior_child_peak)

    def test_measured_command_distinguishes_exit_143_from_sigterm(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            for name, command, completion_reason, terminating_signal in (
                ("exit", "exit 143", "exited", None),
                ("signal", "kill -TERM $$", "signaled", signal.SIGTERM),
            ):
                measurement = fixture / f"{name}.json"
                result = subprocess.run(
                    [
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
                        command,
                    ],
                    stdin=subprocess.DEVNULL,
                    check=False,
                )
                self.assertEqual(result.returncode, 143)
                report = json.loads(measurement.read_text(encoding="utf-8"))
                self.assertEqual(report["exit_code"], 143)
                self.assertEqual(report["completion_reason"], completion_reason)
                self.assertEqual(report["signal"], terminating_signal)
                self.assertEqual(
                    report["resource_accounting"], "gnu_time_command_tree"
                )

    def test_linux_machine_observation_parsers_are_strict(self) -> None:
        namespace = load_hot_run()
        parse_load_average = namespace["parse_load_average"]
        parse_meminfo = namespace["parse_meminfo"]
        parse_pressure = namespace["parse_pressure"]

        self.assertEqual(
            parse_load_average("1.25 2.50 3.75 2/100 42\n"),
            {
                "one_minute": 1.25,
                "five_minutes": 2.5,
                "fifteen_minutes": 3.75,
            },
        )
        self.assertEqual(
            parse_meminfo(
                "MemAvailable: 1024 kB\n"
                "SwapTotal: 512 kB\n"
                "SwapFree: 128 kB\n"
                "HugePages_Total: 0\n"
            ),
            {
                "available_bytes": 1024 * 1024,
                "swap_total_bytes": 512 * 1024,
                "swap_used_bytes": 384 * 1024,
            },
        )
        self.assertEqual(
            parse_pressure(
                "some avg10=0.25 avg60=1.50 avg300=2.75 total=12345\n"
                "full avg10=0.00 avg60=0.10 avg300=0.20 total=678\n"
            )["some"],
            {
                "avg10": 0.25,
                "avg60": 1.5,
                "avg300": 2.75,
                "total_microseconds": 12345,
            },
        )

        for invalid in ("", "nan 1 1", "-1 1 1"):
            with self.assertRaises(ValueError):
                parse_load_average(invalid)
        for invalid in (
            "MemAvailable: 1 kB\nSwapTotal: 1 kB\n",
            "MemAvailable: 1 kB\nMemAvailable: 2 kB\nSwapTotal: 1 kB\nSwapFree: 1 kB\n",
            "MemAvailable: 1 kB\nSwapTotal: 1 kB\nSwapFree: 2 kB\n",
        ):
            with self.assertRaises(ValueError):
                parse_meminfo(invalid)
        for invalid in (
            "full avg10=0 avg60=0 avg300=0 total=0\n",
            "some avg10=nan avg60=0 avg300=0 total=0\n",
            "some avg10=0 avg60=0 avg300=0 total=-1\n",
            "some avg10=0 avg60=0 avg300=0 total=0\n"
            "some avg10=0 avg60=0 avg300=0 total=1\n",
        ):
            with self.assertRaises(ValueError):
                parse_pressure(invalid)

    def test_machine_interval_derives_only_complete_monotonic_evidence(self) -> None:
        namespace = load_hot_run()
        derive = namespace["pressure_observation_interval"]
        before = {
            "memory": {"available_bytes": 1_000, "swap_used_bytes": 200},
            "pressure": {
                "cpu": {
                    "some": {"total_microseconds": 1_000_000},
                    "full": {"total_microseconds": 0},
                },
                "memory": {"some": {"total_microseconds": 50}},
                "io": {"some": {"total_microseconds": 500}},
            },
        }
        after = {
            "memory": {"available_bytes": 900, "swap_used_bytes": 230},
            "pressure": {
                "cpu": {
                    "some": {"total_microseconds": 1_500_000},
                    "full": {"total_microseconds": 0},
                },
                "memory": {"some": {"total_microseconds": 75}},
                "io": {"some": {"total_microseconds": 400}},
            },
        }

        interval = derive(before, after, 2.0)
        self.assertEqual(interval["duration_basis"], "command_elapsed")
        self.assertEqual(
            interval["memory"],
            {"available_bytes_delta": -100, "swap_used_bytes_delta": 30},
        )
        self.assertEqual(
            interval["pressure"]["cpu"]["some"],
            {
                "total_microseconds_delta": 500_000,
                "stall_fraction_of_command_elapsed": 0.25,
            },
        )
        self.assertEqual(
            interval["pressure"]["memory"]["some"]["total_microseconds_delta"],
            25,
        )
        self.assertIsNone(
            interval["pressure"]["io"]["some"]["total_microseconds_delta"]
        )
        self.assertIsNone(
            interval["pressure"]["memory"]["full"]["total_microseconds_delta"]
        )
        zero_duration = derive(before, after, 0.0)
        self.assertEqual(
            zero_duration["pressure"]["cpu"]["some"]["total_microseconds_delta"],
            500_000,
        )
        self.assertIsNone(
            zero_duration["pressure"]["cpu"]["some"]
            ["stall_fraction_of_command_elapsed"]
        )

    def test_program_resolution_preserves_dispatch_symlinks(self) -> None:
        resolve_program = load_hot_run()[
            "resolve_program"
        ]
        cargo = shutil.which("cargo")
        if cargo is None:
            self.skipTest("cargo is unavailable")
        self.assertEqual(resolve_program(["cargo"], ROOT)[0], os.path.abspath(cargo))

    def test_default_state_is_stable_and_separates_tasks(self) -> None:
        namespace = load_hot_run()
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

    def test_default_target_cache_uses_private_copy(self) -> None:
        namespace = load_hot_run()
        default_cache_specs = namespace["default_cache_specs"]
        with tempfile.TemporaryDirectory() as directory:
            resident = Path(directory)
            self.assertEqual(default_cache_specs(resident), ())
            (resident / "target").mkdir()
            specs = default_cache_specs(resident)
        self.assertEqual(len(specs), 1)
        self.assertEqual(specs[0].path, Path("target"))
        self.assertEqual(specs[0].mode, "private-copy")

    def test_seed_source_metadata_normalizes_only_matching_regular_files(self) -> None:
        namespace = load_hot_run()
        prepare = namespace["prepare_seed_source_metadata"]
        CachePreparation = namespace["CachePreparation"]
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            resident = fixture / "resident"
            task = fixture / "task"
            resident.mkdir()
            task.mkdir()
            subprocess.run(["git", "init", "--quiet"], cwd=task, check=True)
            (resident / "matching").write_text("same\n", encoding="utf-8")
            (task / "matching").write_text("same\n", encoding="utf-8")
            (resident / "different").write_text("resident\n", encoding="utf-8")
            (task / "different").write_text("task\n", encoding="utf-8")
            (task / "missing-resident").write_text("task only\n", encoding="utf-8")
            (resident / "link").symlink_to("matching")
            (task / "link").symlink_to("matching")
            (resident / "nested").mkdir()
            (resident / "nested" / "redirected").write_text("same\n", encoding="utf-8")
            (task / "nested").mkdir()
            (task / "nested" / "redirected").write_text("same\n", encoding="utf-8")
            (resident / "hardlinked").write_text("same\n", encoding="utf-8")
            (task / "hardlinked").write_text("same\n", encoding="utf-8")
            subprocess.run(
                [
                    "git",
                    "add",
                    "matching",
                    "different",
                    "missing-resident",
                    "link",
                    "nested/redirected",
                    "hardlinked",
                ],
                cwd=task,
                check=True,
            )
            resident_mtime = 1_700_000_000_000_000_000
            task_mtime = resident_mtime + 5_000_000_000
            os.utime(resident / "matching", ns=(resident_mtime, resident_mtime))
            os.utime(task / "matching", ns=(task_mtime, task_mtime))
            os.utime(task / "different", ns=(task_mtime, task_mtime))
            outside = fixture / "outside"
            outside.mkdir()
            redirected = outside / "redirected"
            redirected.write_text("same\n", encoding="utf-8")
            os.utime(redirected, ns=(task_mtime, task_mtime))
            shutil.rmtree(task / "nested")
            (task / "nested").symlink_to(outside, target_is_directory=True)
            external_hardlink = outside / "hardlinked"
            external_hardlink.write_text("same\n", encoding="utf-8")
            (task / "hardlinked").unlink()
            os.link(external_hardlink, task / "hardlinked")
            os.utime(external_hardlink, ns=(task_mtime, task_mtime))

            seeded = (
                CachePreparation(Path("target"), "private-copy", "seeded", 0.25),
            )
            result = prepare(resident, task, seeded)
            self.assertIsNotNone(result)
            assert result is not None
            self.assertEqual(result.disposition, "normalized_on_seed")
            self.assertEqual(result.tracked_path_count, 6)
            self.assertEqual(result.normalized_regular_file_count, 1)
            self.assertEqual(result.differing_regular_file_count, 1)
            self.assertEqual(result.skipped_path_count, 4)
            self.assertEqual((task / "matching").stat().st_mtime_ns, resident_mtime)
            self.assertEqual((task / "different").stat().st_mtime_ns, task_mtime)
            self.assertEqual(redirected.stat().st_mtime_ns, task_mtime)
            self.assertEqual(external_hardlink.stat().st_mtime_ns, task_mtime)

            retained_mtime = resident_mtime + 10_000_000_000
            os.utime(task / "matching", ns=(retained_mtime, retained_mtime))
            reused = (
                CachePreparation(Path("target"), "private-copy", "reused", 0.0),
            )
            retained = prepare(resident, task, reused)
            self.assertIsNotNone(retained)
            assert retained is not None
            self.assertEqual(retained.disposition, "retained_state_unchanged")
            self.assertEqual(retained.tracked_path_count, 0)
            self.assertEqual((task / "matching").stat().st_mtime_ns, retained_mtime)

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
            self.assertEqual(report["schema_version"], 6)
            self.assertEqual(report["authority"], "developer_observation_only")
            self.assertIsNone(report["comparison_key"])
            self.assertEqual(report["exit_code"], 0)
            self.assertEqual(report["completion_reason"], "exited")
            self.assertIsNone(report["timeout_seconds"])
            self.assertIsNone(report["resource_profile"])
            self.assertIsNone(report["runtime"])
            machine = report["machine_observation"]
            self.assertEqual(machine["scope"], "host_aggregate")
            self.assertIn(machine["before"]["status"], ("observed", "partial"))
            self.assertIn(machine["after"]["status"], ("observed", "partial"))
            self.assertGreater(machine["before"]["online_logical_cpus"], 0)
            self.assertGreaterEqual(
                machine["before"]["observation_elapsed_seconds"], 0
            )
            interval = machine["interval"]
            self.assertEqual(interval["duration_basis"], "command_elapsed")
            self.assertEqual(interval["elapsed_seconds"], report["elapsed_seconds"])
            self.assertIsInstance(
                interval["pressure"]["cpu"]["some"]
                ["total_microseconds_delta"],
                int,
            )
            self.assertTrue(report["cross_worktree"])
            self.assertGreaterEqual(report["elapsed_seconds"], 0)
            self.assertGreater(report["max_rss_kib"], 0)
            self.assertEqual(report["resource_accounting"], "gnu_time_command_tree")
            self.assertEqual(report["state_preparation"], [])
            self.assertIsNone(report["source_preparation"])
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
            resident_payload_mtime = (resident / "payload").stat().st_mtime_ns
            first_task_mtime = resident_payload_mtime + 5_000_000_000
            os.utime(
                task / "payload",
                ns=((task / "payload").stat().st_atime_ns, first_task_mtime),
            )

            base_command = [
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(resident),
                "--task",
                os.fspath(task),
                "--state",
                os.fspath(state),
                "--cache",
                "target:private-copy",
                "--seed-source-mtimes",
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
            self.assertEqual((task / "payload").stat().st_mtime_ns, resident_payload_mtime)
            (resident / "target" / "parent").write_text(
                "changed resident parent\n", encoding="utf-8"
            )
            retained_task_mtime = resident_payload_mtime + 10_000_000_000
            os.utime(
                task / "payload",
                ns=((task / "payload").stat().st_atime_ns, retained_task_mtime),
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
            self.assertEqual((task / "payload").stat().st_mtime_ns, retained_task_mtime)

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
            self.assertEqual(
                first_report["source_preparation"]["mode"],
                "resident_mtime_for_identical_tracked_regular_files_v1",
            )
            self.assertEqual(
                first_report["source_preparation"]["disposition"],
                "normalized_on_seed",
            )
            self.assertEqual(
                first_report["source_preparation"]["tracked_path_count"], 1
            )
            self.assertEqual(
                first_report["source_preparation"]["normalized_regular_file_count"],
                1,
            )
            self.assertEqual(
                first_report["source_preparation"]["differing_regular_file_count"], 0
            )
            self.assertEqual(first_report["source_preparation"]["skipped_path_count"], 0)
            self.assertGreaterEqual(
                first_report["source_preparation"]["elapsed_seconds"], 0
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
            self.assertEqual(
                second_report["source_preparation"],
                {
                    "mode": "resident_mtime_for_identical_tracked_regular_files_v1",
                    "disposition": "retained_state_unchanged",
                    "tracked_path_count": 0,
                    "normalized_regular_file_count": 0,
                    "differing_regular_file_count": 0,
                    "skipped_path_count": 0,
                    "elapsed_seconds": 0.0,
                },
            )
            self.assertEqual(second_report["preparation_elapsed_seconds"], 0.0)
            self.assertEqual(list(state.glob(".private-*")), [])
            self.assertEqual(list(state.glob(".git-view-*")), [])

    def test_cache_specs_reject_escape_overlap_and_unknown_modes(self) -> None:
        parse_cache_specs = load_hot_run()[
            "parse_cache_specs"
        ]
        for values in (["../target"], ["target:shared"], ["target", "target:ro"]):
            with self.assertRaises(RuntimeError):
                parse_cache_specs(values)
        self.assertEqual(parse_cache_specs(["target"])[0].mode, "overlay")
        self.assertEqual(parse_cache_specs(["target:private"])[0].mode, "private")
        self.assertEqual(
            parse_cache_specs(["target:private-copy"])[0].mode, "private-copy"
        )
        with self.assertRaises(RuntimeError):
            parse_cache_specs(["build", "build/generated"])

    def test_private_copy_failure_never_publishes_candidate(self) -> None:
        namespace = load_hot_run()
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

    def test_runtime_bin_binding_closes_descendant_path_and_is_path_free(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            runtime_bin = fixture / "toolchain" / "bin"
            fallback_bin = fixture / "fallback"
            runtime_bin.mkdir(parents=True)
            fallback_bin.mkdir()
            runtime = runtime_bin / "runtime"
            descendant = runtime_bin / "runtime-child"
            fallback_descendant = fallback_bin / "runtime-child"
            output = fixture / "output"
            measurement = fixture / "measurement.json"
            runtime.write_text(
                '#!/bin/sh\nexec /usr/bin/env runtime-child "$1"\n',
                encoding="utf-8",
            )
            descendant.write_text(
                '#!/bin/sh\nprintf bound > "$1"\n', encoding="utf-8"
            )
            fallback_descendant.write_text(
                '#!/bin/sh\nprintf fallback > "$1"\n', encoding="utf-8"
            )
            for program in (runtime, descendant, fallback_descendant):
                program.chmod(0o700)
            digest = hashlib.sha256(runtime.read_bytes()).hexdigest()
            environment = os.environ.copy()
            environment["PATH"] = os.pathsep.join(
                (os.fspath(fallback_bin), "/usr/bin", "/bin")
            )

            bound = subprocess.run(
                [
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--runtime-id",
                    "test-runtime-bound",
                    "--runtime-sha256",
                    f"sha256:{digest}",
                    "--runtime-bin",
                    os.fspath(runtime_bin),
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    "runtime",
                    os.fspath(output),
                ],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )
            self.assertEqual(bound.returncode, 0, bound.stderr)
            self.assertEqual(output.read_text(encoding="utf-8"), "bound")
            report_text = measurement.read_text(encoding="utf-8")
            report = json.loads(report_text)
            self.assertEqual(report["runtime"]["id"], "test-runtime-bound")
            self.assertEqual(
                report["runtime"]["program_sha256"], f"sha256:{digest}"
            )
            self.assertEqual(
                report["runtime"]["descendant_path"], "runtime_bin_first"
            )
            self.assertRegex(
                report["runtime"]["runtime_bin_binding_sha256"],
                r"^sha256:[0-9a-f]{64}$",
            )
            self.assertNotIn(os.fspath(fixture), report_text)

            output.unlink()
            unbound = subprocess.run(
                [
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--runtime-id",
                    "test-runtime-bound",
                    "--runtime-sha256",
                    f"sha256:{digest}",
                    "--",
                    os.fspath(runtime),
                    os.fspath(output),
                ],
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )
            self.assertEqual(unbound.returncode, 0, unbound.stderr)
            self.assertEqual(output.read_text(encoding="utf-8"), "fallback")

            namespace = load_hot_run()
            binding = namespace["observe_runtime_bin"](
                runtime_bin, "test-runtime-bound"
            )
            self.assertIsNotNone(binding)
            RuntimeContract = namespace["RuntimeContract"]
            state = Path("/tmp/glaeda-runtime-state-test")
            unbound_contract = RuntimeContract(
                "test-runtime-bound", f"sha256:{digest}"
            )
            bound_contract = RuntimeContract(
                "test-runtime-bound",
                f"sha256:{digest}",
                binding.identity_sha256,
            )
            self.assertNotEqual(
                namespace["runtime_state_root"](state, unbound_contract),
                namespace["runtime_state_root"](state, bound_contract),
            )

    def test_runtime_bin_binding_refuses_invalid_or_changed_directories(self) -> None:
        namespace = load_hot_run()
        observe = namespace["observe_runtime_bin"]
        revalidate = namespace["revalidate_runtime_bin"]
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            runtime_bin = fixture / "bin"
            runtime_bin.mkdir()
            program = runtime_bin / "runtime"
            program.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            program.chmod(0o700)

            with self.assertRaisesRegex(RuntimeError, "requires a runtime ID"):
                observe(runtime_bin, None)
            with self.assertRaisesRegex(RuntimeError, "absolute canonical path"):
                observe(Path("relative/bin"), "runtime")
            regular_file = fixture / "not-a-directory"
            regular_file.write_text("not a directory", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "not a plain directory"):
                observe(regular_file, "runtime")
            alias = fixture / "alias"
            alias.symlink_to(runtime_bin, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "not a plain directory"):
                observe(alias, "runtime")

            outside = subprocess.run(
                [
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--runtime-id",
                    "test-runtime-bound",
                    "--runtime-bin",
                    os.fspath(runtime_bin),
                    "--",
                    "/bin/true",
                ],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
            )
            self.assertEqual(outside.returncode, 2)
            self.assertIn("outside the bound runtime bin", outside.stderr)

            binding = observe(runtime_bin, "runtime")
            moved = fixture / "old-bin"
            runtime_bin.rename(moved)
            runtime_bin.mkdir()
            with self.assertRaisesRegex(RuntimeError, "changed during preflight"):
                revalidate(binding)

    def test_runtime_contract_rejects_digest_without_id_and_noncanonical_values(
        self,
    ) -> None:
        parse_runtime_contract = load_hot_run()["parse_runtime_contract"]
        for runtime_id, digest in (
            (None, f"sha256:{'1' * 64}"),
            ("../node", f"sha256:{'1' * 64}"),
            ("../node", None),
            ("node-22", "1" * 64),
            ("node-22", f"sha256:{'A' * 64}"),
        ):
            with self.assertRaises(RuntimeError):
                parse_runtime_contract(runtime_id, digest)

    def test_comparison_key_requires_measurement_and_is_recorded(self) -> None:
        comparison_key = f"sha256:{'3' * 64}"
        without_measurement = subprocess.run(
            [
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(ROOT),
                "--task",
                os.fspath(ROOT),
                "--comparison-key",
                comparison_key,
                "--",
                "/bin/true",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(without_measurement.returncode, 2)
        self.assertIn("--comparison-key requires --measurement", without_measurement.stderr)

        with tempfile.TemporaryDirectory() as directory:
            measurement = Path(directory) / "measurement.json"
            measured = subprocess.run(
                [
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--measurement",
                    os.fspath(measurement),
                    "--comparison-key",
                    comparison_key,
                    "--",
                    "/bin/true",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            self.assertEqual(measured.returncode, 0)
            report = json.loads(measurement.read_text(encoding="utf-8"))
            self.assertEqual(report["schema_version"], 6)
            self.assertEqual(report["comparison_key"], comparison_key)

        invalid = subprocess.run(
            [
                os.fspath(HOT_RUN),
                "--comparison-key",
                f"sha256:{'A' * 64}",
                "--measurement",
                "/tmp/unused-glaeda-measurement",
                "--",
                "/bin/true",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(invalid.returncode, 2)
        self.assertIn("comparison key must be canonical SHA-256", invalid.stderr)

    def test_direct_resident_execution_preserves_failure_status(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            measurement = Path(directory) / "measurement.json"
            result = subprocess.run(
                [
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

    def test_direct_native_cache_declaration_is_recorded_without_isolation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            measurement = Path(directory) / "measurement.json"
            result = subprocess.run(
                [
                    os.fspath(HOT_RUN),
                    "--resident",
                    os.fspath(ROOT),
                    "--task",
                    os.fspath(ROOT),
                    "--cache",
                    "target:native",
                    "--measurement",
                    os.fspath(measurement),
                    "--",
                    "/bin/true",
                ],
                stdin=subprocess.DEVNULL,
                check=False,
            )
            report = json.loads(measurement.read_text(encoding="utf-8"))
        self.assertEqual(result.returncode, 0)
        self.assertFalse(report["cross_worktree"])
        self.assertEqual(
            report["cache_views"], [{"mode": "native", "path": "target"}]
        )
        self.assertEqual(report["state_preparation"], [])

    def test_native_cache_mode_refuses_cross_worktree_execution(self) -> None:
        namespace = load_hot_run()
        run = namespace["run"]
        with mock.patch.dict(
            run.__globals__,
            {
                "worktree_root": mock.Mock(
                    side_effect=[Path("/resident"), Path("/task")]
                ),
                "common_git_directory": mock.Mock(return_value=Path("/common")),
                "task_git_directory": mock.Mock(
                    return_value=Path("/common/worktrees/task")
                ),
                "resolve_program": mock.Mock(return_value=["/bin/true"]),
                "verify_runtime_contract": mock.Mock(return_value=None),
            },
        ):
            with self.assertRaisesRegex(
                RuntimeError, "native cache mode requires the same worktree"
            ):
                run(
                    Path("/resident"),
                    Path("/task"),
                    None,
                    ["target:native"],
                    False,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    ["/bin/true"],
                    False,
                )

    def test_seed_source_mtimes_requires_a_private_target_copy(self) -> None:
        result = subprocess.run(
            [
                os.fspath(HOT_RUN),
                "--resident",
                os.fspath(ROOT),
                "--task",
                os.fspath(ROOT),
                "--cache",
                "target:native",
                "--seed-source-mtimes",
                "--",
                "/bin/true",
            ],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("require a task-private target copy", result.stderr)

    def test_timeout_stops_owned_process_group_and_writes_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            measurement = fixture / "measurement.json"
            child_pid = fixture / "child.pid"
            result = subprocess.run(
                [
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
