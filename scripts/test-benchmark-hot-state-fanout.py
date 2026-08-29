#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import runpy
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "benchmark-hot-state-fanout"
NAMESPACE = runpy.run_path(str(SCRIPT), run_name="hot_state_fanout_test")


class HotStateFanoutTests(unittest.TestCase):
    def test_closed_plans_divide_the_host_grant(self) -> None:
        build_plan = NAMESPACE["build_plan"]
        for fanout, jobs in ((1, 16), (4, 4), (8, 2)):
            plan = build_plan("private-copy", fanout, 16)
            self.assertEqual(plan.cargo_jobs_per_task, jobs)
            value = plan.to_json()
            self.assertEqual(value["resources"]["total_declared_cargo_jobs"], 16)
            self.assertEqual(value["workload"]["expected_executed_test_count"], 1343)

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
            self.assertGreaterEqual(observation["logical"], 4097)
            self.assertGreater(observation["allocated"], 0)

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

    def test_receipt_validation_binds_source_edit_and_result(self) -> None:
        validate = NAMESPACE["validate_benchmark_receipt"]
        ExperimentError = NAMESPACE["ExperimentError"]
        receipt = {
            "document_type": "glaeda-developer-loop-benchmark",
            "benchmark_id": "resident-eligible-rust-edit-v1",
            "source": {
                "commit": NAMESPACE["SOURCE_COMMIT"],
                "tree": NAMESPACE["SOURCE_TREE"],
                "tracked_workload_dirty": True,
                "tracked_workload_diff_digest": NAMESPACE["FIXTURE_DIGEST"],
            },
            "toolchain": {
                "rustc": "rustc 1.97.1 (fixture)",
                "cargo": "cargo 1.97.1 (fixture)",
            },
            "resources": {"cargo_build_jobs": "4"},
            "workload": {
                "expected_executed_test_count": 1343,
                "excluded_host_fact_test_count": 16,
            },
            "result": {"exit_code": 0},
        }
        validate(receipt, True, 4)
        receipt["result"]["exit_code"] = 1
        with self.assertRaises(ExperimentError):
            validate(receipt, True, 4)


if __name__ == "__main__":
    unittest.main()
