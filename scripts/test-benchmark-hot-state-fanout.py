#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import runpy
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "benchmark-hot-state-fanout"
NAMESPACE = runpy.run_path(str(SCRIPT), run_name="hot_state_fanout_test")


def valid_benchmark_receipt(jobs: int = 4) -> dict[str, object]:
    return {
        "document_type": "glaeda-developer-loop-benchmark",
        "benchmark_id": "resident-eligible-rust-edit-v1",
        "source": {
            "commit": NAMESPACE["SOURCE_COMMIT"],
            "tree": NAMESPACE["SOURCE_TREE"],
            "tracked_workload_dirty": True,
            "tracked_workload_diff_digest": NAMESPACE["FIXTURE_DIGEST"],
        },
        "toolchain": {
            "rustc": NAMESPACE["RUSTC_VERSION"],
            "cargo": NAMESPACE["CARGO_VERSION"],
        },
        "resources": {"cargo_build_jobs": str(jobs)},
        "workload": {
            "expected_executed_test_count": 1343,
            "excluded_host_fact_test_count": 16,
        },
        "result": {"exit_code": 0},
    }


class HotStateFanoutTests(unittest.TestCase):
    def test_closed_plans_assign_disjoint_cpu_affinity(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        for fanout, jobs in ((1, 16), (4, 4), (8, 2)):
            plan = build_plan("private-copy", fanout, 16)
            self.assertEqual(plan.cargo_jobs_per_task, jobs)
            value = plan.to_json()
            self.assertEqual(value["resources"]["configured_total_cargo_jobs"], 16)
            self.assertEqual(value["environment"]["child_filesystem_creation_umask"], "0022")
            flattened = [
                cpu
                for cpu_set in value["resources"]["task_cpu_sets"]
                for cpu in cpu_set
            ]
            self.assertEqual(len(flattened), len(set(flattened)))
            self.assertEqual(value["workload"]["expected_executed_test_count"], 1343)
            self.assertRegex(
                value["comparison"]["edit_key"], r"^sha256:[0-9a-f]{64}$"
            )
            self.assertNotEqual(
                value["comparison"]["prime_key"],
                value["comparison"]["edit_key"],
            )
            self.assertFalse(value["retained_reuse_window"])
            self.assertNotIn("retained_reuse_key", value["comparison"])

    def test_retained_reuse_has_distinct_state_phase_and_key(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        comparison_basis = NAMESPACE["comparison_basis"]
        comparison_key = NAMESPACE["comparison_key"]
        ExperimentError = NAMESPACE["ExperimentError"]
        plan = build_plan("private-copy", 4, 16, retained_reuse=True)
        value = plan.to_json()
        initial_key = comparison_key(plan, True)
        reuse_key = comparison_key(plan, True, retained_reuse=True)
        self.assertTrue(value["retained_reuse_window"])
        self.assertEqual(value["comparison"]["retained_reuse_key"], reuse_key)
        self.assertNotEqual(initial_key, reuse_key)
        self.assertEqual(
            comparison_basis(plan, True)["treatment"]["state_phase"],
            "initial_for_source_state",
        )
        self.assertEqual(
            comparison_basis(plan, True, retained_reuse=True)["treatment"][
                "state_phase"
            ],
            "retained_after_accepted_edit",
        )
        with self.assertRaises(ExperimentError):
            comparison_basis(plan, False, retained_reuse=True)

    def test_comparison_keys_bind_treatment_and_remain_path_free(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        comparison_basis = NAMESPACE["comparison_basis"]
        comparison_key = NAMESPACE["comparison_key"]
        first = build_plan("ordinary-native", 4, 16)
        repeat = build_plan("ordinary-native", 4, 16)
        overlay = build_plan("overlay", 4, 16)
        wider = build_plan("ordinary-native", 8, 16)
        cold_overlay = build_plan(
            "overlay",
            4,
            16,
            page_cache_treatment=NAMESPACE[
                "PAGE_CACHE_RESIDENT_TARGET_DONTNEED"
            ],
        )
        self.assertEqual(
            comparison_key(first, True), comparison_key(repeat, True)
        )
        self.assertNotEqual(
            comparison_key(first, True), comparison_key(overlay, True)
        )
        self.assertNotEqual(
            comparison_key(first, True), comparison_key(wider, True)
        )
        self.assertNotEqual(
            comparison_key(overlay, True), comparison_key(cold_overlay, True)
        )
        self.assertEqual(
            cold_overlay.to_json()["environment"]["page_cache_treatment"],
            "resident_target_fsync_fadvise_dontneed",
        )
        programs = comparison_basis(first, True)["producer_programs"]
        self.assertEqual(
            set(programs),
            {
                "fanout_harness_sha256",
                "hot_run_sha256",
                "semantic_benchmark_sha256",
            },
        )
        self.assertTrue(
            all(
                isinstance(value, str)
                and value.startswith("sha256:")
                and len(value) == 71
                for value in programs.values()
            )
        )
        self.assertNotIn(os.fspath(ROOT), json.dumps(first.to_json()))

    def test_unknown_arms_fanout_and_deadlines_refuse(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        ExperimentError = NAMESPACE["ExperimentError"]
        for arguments in (
            ("shared", 1, 16, 900),
            ("overlay", 2, 16, 900),
            ("overlay", 8, 4, 900),
            ("overlay", 1, 16, 0),
            ("overlay", 1, 16, 3601),
        ):
            with self.assertRaises(ExperimentError):
                build_plan(*arguments)
        with self.assertRaises(ExperimentError):
            build_plan(
                "ordinary-native",
                4,
                16,
                page_cache_treatment=NAMESPACE[
                    "PAGE_CACHE_RESIDENT_TARGET_DONTNEED"
                ],
            )
        with self.assertRaises(ExperimentError):
            build_plan(
                "overlay", 4, 16, page_cache_treatment="global_drop_caches"
            )

    def test_plan_cli_is_read_only_and_path_free(self) -> None:
        result = subprocess.run(
            [str(SCRIPT), "--plan", "--arm", "overlay", "--fanout", "4"],
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertEqual(plan["arm"], "overlay")
        self.assertEqual(plan["fanout"], 4)
        self.assertNotIn(os.fspath(ROOT), result.stdout)

    def test_retained_reuse_plan_cli_declares_second_window(self) -> None:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--plan",
                "--arm",
                "private-copy",
                "--fanout",
                "4",
                "--retained-reuse",
            ],
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)
        self.assertTrue(plan["retained_reuse_window"])
        self.assertRegex(
            plan["comparison"]["retained_reuse_key"],
            r"^sha256:[0-9a-f]{64}$",
        )

    def test_tree_bytes_observes_but_does_not_follow_symlinks(self) -> None:
        tree_bytes = NAMESPACE["tree_bytes"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "payload").write_bytes(b"x" * 4097)
            (root / "link").symlink_to("payload")
            (root / "nested").mkdir()
            (root / "nested" / "small").write_bytes(b"x")
            observation = tree_bytes(root)
            self.assertEqual(observation["entries"], 5)
            self.assertGreaterEqual(observation["logical_file_bytes"], 4097)
            self.assertGreater(observation["allocated_file_blocks_bytes"], 0)

    def test_resident_target_cold_advice_is_bounded_and_skips_symlinks(
        self,
    ) -> None:
        advise = NAMESPACE["advise_resident_target_dontneed"]
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            target = fixture / "target"
            target.mkdir()
            (target / "payload").write_bytes(b"x" * 4097)
            (target / "nested").mkdir()
            (target / "nested" / "small").write_bytes(b"y")
            external = fixture / "external"
            external.write_bytes(b"external")
            (target / "external-link").symlink_to(external)
            result = advise(target)
        self.assertEqual(result["disposition"], "advised")
        self.assertEqual(result["writeback"], "per_file_fsync")
        self.assertEqual(result["advice"], "POSIX_FADV_DONTNEED")
        self.assertEqual(result["directories"], 2)
        self.assertEqual(result["regular_files"], 2)
        self.assertEqual(result["symlinks_skipped"], 1)
        self.assertEqual(result["logical_file_bytes"], 4098)

    def test_filesystem_observation_binds_mount_and_device(self) -> None:
        filesystem_observation = NAMESPACE["filesystem_observation"]
        same_filesystem_identity = NAMESPACE["same_filesystem_identity"]
        with tempfile.TemporaryDirectory() as directory:
            first = filesystem_observation(Path(directory))
            second = filesystem_observation(Path(directory))
        self.assertTrue(same_filesystem_identity(first, second))
        self.assertTrue(first["filesystem_type"])
        self.assertEqual(
            first["findmnt_device"],
            f"{first['device_major']}:{first['device_minor']}",
        )

    def test_owned_cleanup_repairs_mode_zero_directories_without_following_links(
        self,
    ) -> None:
        remove_owned_experiment_tree = NAMESPACE["remove_owned_experiment_tree"]
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory)
            experiment = fixture / "experiment"
            opaque = experiment / "state" / "work"
            external = fixture / "external"
            opaque.mkdir(parents=True)
            external.mkdir()
            (opaque / "payload").write_text("owned\n", encoding="utf-8")
            (experiment / "external-link").symlink_to(external, target_is_directory=True)
            opaque.chmod(0)
            external.chmod(0)
            try:
                remove_owned_experiment_tree(experiment)
                self.assertFalse(experiment.exists())
                self.assertEqual(external.stat().st_mode & 0o777, 0)
            finally:
                external.chmod(0o700)

    def test_closed_environment_excludes_caller_build_injection(self) -> None:
        closed_environment = NAMESPACE["closed_environment"]
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(
            os.environ,
            {
                "CARGO_TARGET_DIR": "/foreign-target",
                "RUSTC_WRAPPER": "/foreign-wrapper",
                "RUSTFLAGS": "--cfg foreign",
                "RUSTUP_TOOLCHAIN": "foreign",
            },
        ):
            environment = closed_environment(4, Path(directory) / "tmp")
        for variable in (
            "CARGO_TARGET_DIR",
            "RUSTC_WRAPPER",
            "RUSTFLAGS",
            "RUSTUP_TOOLCHAIN",
        ):
            self.assertNotIn(variable, environment)
        self.assertEqual(environment["CARGO_BUILD_JOBS"], "4")
        self.assertEqual(environment["CARGO_INCREMENTAL"], "1")
        self.assertEqual(environment["CARGO_NET_OFFLINE"], "true")

    def test_ambient_cargo_configuration_refuses(self) -> None:
        validate = NAMESPACE["validate_cargo_config_absence"]
        ExperimentError = NAMESPACE["ExperimentError"]
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            with mock.patch.dict(
                validate.__globals__,
                {"git_output": mock.Mock(return_value="")},
            ):
                validate(ROOT, scratch)
                cargo = scratch / ".cargo"
                cargo.mkdir()
                (cargo / "config.toml").write_text(
                    '[build]\ntarget-dir = "/foreign"\n', encoding="utf-8"
                )
                with self.assertRaises(ExperimentError):
                    validate(ROOT, scratch)

    def test_hot_run_command_uses_supported_unscoped_mode(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        command_for_task = NAMESPACE["command_for_task"]
        plan = build_plan("private-copy", 4, 16)
        command, receipt = command_for_task(
            plan,
            Path("/resident"),
            Path("/task"),
            Path("/state"),
            Path("/benchmark.json"),
            Path("/hot-run.json"),
            True,
        )
        self.assertNotIn("--resource-profile", command)
        self.assertEqual(command[command.index("--cache") + 1], "target:private-copy")
        self.assertEqual(
            command[command.index("--comparison-key") + 1],
            NAMESPACE["comparison_key"](plan, True),
        )
        self.assertEqual(receipt, Path("/hot-run.json"))

        reuse_command, _ = command_for_task(
            plan,
            Path("/resident"),
            Path("/task"),
            Path("/state"),
            Path("/benchmark.json"),
            Path("/hot-run.json"),
            True,
            retained_reuse=True,
        )
        self.assertEqual(
            reuse_command[reuse_command.index("--comparison-key") + 1],
            NAMESPACE["comparison_key"](plan, True, retained_reuse=True),
        )

    def test_ordinary_control_uses_measured_same_worktree_native_mode(self) -> None:
        plan = NAMESPACE["build_plan"]("ordinary-native", 4, 16)
        command, receipt = NAMESPACE["command_for_task"](
            plan,
            Path("/unused-resident"),
            Path("/task"),
            Path("/unused-state"),
            Path("/benchmark.json"),
            Path("/hot-run.json"),
            True,
        )
        self.assertEqual(command[command.index("--resident") + 1], "/task")
        self.assertEqual(command[command.index("--task") + 1], "/task")
        self.assertEqual(command[command.index("--cache") + 1], "target:native")
        self.assertEqual(receipt, Path("/hot-run.json"))

    def test_failed_member_cancels_a_running_sibling(self) -> None:
        TaskProcess = NAMESPACE["TaskProcess"]
        wait_for_tasks = NAMESPACE["wait_for_tasks"]
        failed = subprocess.Popen(
            ["/usr/bin/false"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        sibling = subprocess.Popen(
            ["/usr/bin/sleep", "30"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        tasks = [
            TaskProcess("failed", failed, Path("missing-a"), None),
            TaskProcess("sibling", sibling, Path("missing-b"), None),
        ]
        elapsed, failure, maximum_running = wait_for_tasks(tasks, 5)
        self.assertEqual(failure, "one or more fan-out tasks failed")
        self.assertLess(elapsed, 2)
        self.assertGreaterEqual(maximum_running, 1)
        self.assertIsNotNone(sibling.returncode)

    def test_started_task_runs_inside_declared_affinity(self) -> None:
        start_task = NAMESPACE["start_task"]
        wait_for_tasks = NAMESPACE["wait_for_tasks"]
        available = tuple(sorted(os.sched_getaffinity(0)))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            task = start_task(
                "task-01",
                ["/usr/bin/true"],
                root,
                root / "unused.json",
                None,
                1,
                root / "tmp",
                (available[0],),
            )
            _, failure, maximum_running = wait_for_tasks([task], 5)
        self.assertIsNone(failure)
        self.assertEqual(task.process.returncode, 0)
        self.assertIn(maximum_running, (0, 1))

    def test_started_task_uses_private_safe_creation_umask(self) -> None:
        start_task = NAMESPACE["start_task"]
        wait_for_tasks = NAMESPACE["wait_for_tasks"]
        available = tuple(sorted(os.sched_getaffinity(0)))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            previous_umask = os.umask(0o002)
            try:
                task = start_task(
                    "task-01",
                    ["/usr/bin/mkdir", "created"],
                    root,
                    root / "unused.json",
                    None,
                    1,
                    root / "tmp",
                    (available[0],),
                )
            finally:
                os.umask(previous_umask)
            _, failure, _ = wait_for_tasks([task], 5)
            created_mode = (root / "created").stat().st_mode & 0o777
        self.assertIsNone(failure)
        self.assertEqual(task.process.returncode, 0)
        self.assertEqual(created_mode, 0o755)

    def test_receipt_validation_binds_source_edit_and_result(self) -> None:
        validate = NAMESPACE["validate_benchmark_receipt"]
        ExperimentError = NAMESPACE["ExperimentError"]
        receipt = valid_benchmark_receipt()
        validate(receipt, True, 4)
        result = receipt["result"]
        assert isinstance(result, dict)
        result["exit_code"] = 1
        with self.assertRaises(ExperimentError):
            validate(receipt, True, 4)

    def test_private_copy_acceptance_requires_first_use_seed(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        aggregate_task = NAMESPACE["aggregate_task"]
        TaskProcess = NAMESPACE["TaskProcess"]
        ExperimentError = NAMESPACE["ExperimentError"]
        plan = build_plan("private-copy", 4, 16)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            benchmark_path = root / "benchmark.json"
            hot_run_path = root / "hot-run.json"
            benchmark_path.write_text(
                json.dumps(valid_benchmark_receipt()), encoding="utf-8"
            )
            hot_run = {
                "schema_version": 4,
                "comparison_key": NAMESPACE["comparison_key"](plan, True),
                "document_type": "glaeda-hot-run-measurement",
                "exit_code": 0,
                "completion_reason": "exited",
                "cross_worktree": True,
                "resource_profile": None,
                "machine_observation": {
                    "scope": "host_aggregate",
                    "before": {"status": "observed"},
                    "after": {"status": "observed"},
                    "interval": {
                        "duration_basis": "command_elapsed",
                        "elapsed_seconds": 1.0,
                        "memory": {
                            "available_bytes_delta": -1024,
                            "swap_used_bytes_delta": 0,
                        },
                        "pressure": {
                            kind: {
                                pressure_class: {
                                    "total_microseconds_delta": 0,
                                    "stall_fraction_of_command_elapsed": 0.0,
                                }
                                for pressure_class in ("some", "full")
                            }
                            for kind in ("cpu", "memory", "io")
                        },
                    },
                },
                "elapsed_seconds": 1.0,
                "cache_views": [{"path": "target", "mode": "private-copy"}],
                "state_preparation": [
                    {
                        "path": "target",
                        "mode": "private-copy",
                        "disposition": "seeded",
                        "elapsed_seconds": 0.25,
                    }
                ],
            }
            hot_run_path.write_text(json.dumps(hot_run), encoding="utf-8")
            task = TaskProcess("task-01", None, benchmark_path, hot_run_path)
            aggregate_task(task, plan, True)
            hot_run["state_preparation"][0]["disposition"] = "reused"
            hot_run["state_preparation"][0]["elapsed_seconds"] = 0
            hot_run["comparison_key"] = NAMESPACE["comparison_key"](
                plan, True, retained_reuse=True
            )
            hot_run_path.write_text(json.dumps(hot_run), encoding="utf-8")
            with self.assertRaises(ExperimentError):
                aggregate_task(task, plan, True)
            aggregate_task(task, plan, True, retained_reuse=True)
            hot_run["state_preparation"][0]["disposition"] = "seeded"
            hot_run_path.write_text(json.dumps(hot_run), encoding="utf-8")
            with self.assertRaises(ExperimentError):
                aggregate_task(task, plan, True, retained_reuse=True)
            hot_run["state_preparation"][0]["disposition"] = "reused"
            hot_run["state_preparation"][0]["elapsed_seconds"] = 0.25
            hot_run_path.write_text(json.dumps(hot_run), encoding="utf-8")
            with self.assertRaises(ExperimentError):
                aggregate_task(task, plan, True, retained_reuse=True)
            hot_run["state_preparation"][0]["disposition"] = "seeded"
            hot_run["state_preparation"][0]["elapsed_seconds"] = 0.25
            hot_run["comparison_key"] = NAMESPACE["comparison_key"](plan, True)
            hot_run["machine_observation"]["interval"]["pressure"]["cpu"][
                "some"
            ]["stall_fraction_of_command_elapsed"] = -0.1
            hot_run_path.write_text(json.dumps(hot_run), encoding="utf-8")
            with self.assertRaises(ExperimentError):
                aggregate_task(task, plan, True)

    def test_failed_resident_prime_retains_bounded_attempt_evidence(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        prime_resident = NAMESPACE["prime_resident"]
        TaskProcess = NAMESPACE["TaskProcess"]
        plan = build_plan("private-copy", 1, 16)
        failed_task = TaskProcess(
            "resident",
            SimpleNamespace(returncode=7),
            Path("missing-benchmark.json"),
            None,
        )
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(
            prime_resident.__globals__,
            {
                "start_task": mock.Mock(return_value=failed_task),
                "wait_for_tasks": mock.Mock(
                    return_value=(0.25, "one or more fan-out tasks failed", 1)
                ),
            },
        ):
            result = prime_resident(
                plan,
                Path(directory),
                Path(directory) / "missing-benchmark.json",
                Path(directory) / "tmp",
            )
        self.assertEqual(result["process_exit_code"], 7)
        self.assertEqual(result["semantic_validation"], "unobserved")
        self.assertEqual(result["failure"], "one or more fan-out tasks failed")
        self.assertIsNone(result["benchmark"])

    def test_rejected_resident_prime_retains_parsed_receipt(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        prime_resident = NAMESPACE["prime_resident"]
        TaskProcess = NAMESPACE["TaskProcess"]
        plan = build_plan("private-copy", 1, 16)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = root / "benchmark.json"
            rejected = valid_benchmark_receipt(16)
            source = rejected["source"]
            assert isinstance(source, dict)
            source["commit"] = "0" * 40
            receipt.write_text(json.dumps(rejected), encoding="utf-8")
            task = TaskProcess(
                "resident", SimpleNamespace(returncode=0), receipt, None
            )
            with mock.patch.dict(
                prime_resident.__globals__,
                {
                    "start_task": mock.Mock(return_value=task),
                    "wait_for_tasks": mock.Mock(return_value=(0.25, None, 1)),
                },
            ):
                result = prime_resident(
                    plan, root, receipt, root / "tmp"
                )
        self.assertEqual(result["process_exit_code"], 0)
        self.assertEqual(result["semantic_validation"], "rejected")
        self.assertIsNotNone(result["benchmark"])
        self.assertEqual(
            result["failure"],
            "task semantic receipt does not match the frozen workload",
        )

    def test_pre_creation_failure_reports_no_state_created(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        run_experiment = NAMESPACE["run_experiment"]
        plan = build_plan("private-copy", 1, 16)
        filesystem = {
            "mount_id": "1",
            "device_major": 1,
            "device_minor": 2,
            "findmnt_device": "1:2",
            "filesystem_type": "fixture",
            "fragment_size_bytes": 4096,
            "total_bytes": 4096,
            "available_bytes": 4096,
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scratch = root / "scratch"
            scratch.mkdir()
            output = root / "result.json"
            with mock.patch.dict(
                run_experiment.__globals__,
                {
                    "validate_static_contract": mock.Mock(),
                    "validate_scratch_root": mock.Mock(return_value=scratch),
                    "validate_cargo_config_absence": mock.Mock(),
                    "filesystem_observation": mock.Mock(return_value=filesystem),
                    "git_output": mock.Mock(return_value="0" * 40),
                },
            ), mock.patch.object(
                run_experiment.__globals__["tempfile"],
                "mkdtemp",
                side_effect=OSError("fixture creation failure"),
            ):
                exit_code = run_experiment(ROOT, scratch, output, plan)
            report = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(exit_code, 1)
        self.assertEqual(report["cleanup"]["disposition"], "no_state_created")
        self.assertEqual(report["cleanup"]["worktrees_expected"], 0)
        self.assertEqual(report["cleanup"]["failure_count"], 0)


if __name__ == "__main__":
    unittest.main()
