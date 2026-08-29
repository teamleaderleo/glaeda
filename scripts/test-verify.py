#!/usr/bin/env python3
"""Deterministic contract tests for scripts/verify."""

from __future__ import annotations

import json
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
VERIFY = ROOT / "scripts" / "verify"


def read_plan(profile: str) -> dict[str, object]:
    result = subprocess.run(
        [sys.executable, str(VERIFY), profile, "--plan-json"],
        cwd=ROOT,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=10,
        check=True,
    )
    return json.loads(result.stdout)


class VerifyPlanTests(unittest.TestCase):
    def test_required_profile_is_the_exact_eight_step_agents_sequence(self) -> None:
        plan = read_plan("required")
        self.assertEqual(plan["authority"], "repository_required_checks")
        self.assertEqual(
            [phase["argv"] for phase in plan["phases"]],
            [
                ["scripts/bootstrap", "--output", "json"],
                ["cargo", "fmt", "--all", "--", "--check"],
                [
                    "cargo",
                    "clippy",
                    "--locked",
                    "--all-targets",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ],
                ["cargo", "test", "--locked", "--all-targets", "--all-features"],
                [
                    "cargo",
                    "run",
                    "--locked",
                    "--quiet",
                    "--",
                    "--output",
                    "json",
                    "doctor",
                ],
                [
                    "cargo",
                    "run",
                    "--locked",
                    "--quiet",
                    "--",
                    "plan",
                    "--file",
                    "examples/quarry.yml",
                ],
                [
                    "cargo",
                    "run",
                    "--locked",
                    "--quiet",
                    "--",
                    "--output",
                    "json",
                    "plan",
                    "--file",
                    "examples/glossless.yml",
                ],
                [
                    "cargo",
                    "run",
                    "--locked",
                    "--quiet",
                    "--",
                    "--output",
                    "json",
                    "host",
                    "plan",
                    "--file",
                    "examples/quarry.yml",
                ],
            ],
        )

    def test_fast_profile_is_explicitly_non_authoritative_and_uses_one_cli_build(self) -> None:
        plan = read_plan("fast")
        self.assertEqual(plan["authority"], "developer_feedback_only")
        phases = plan["phases"]
        self.assertEqual(
            [phase["name"] for phase in phases],
            [
                "unit-and-binary-tests",
                "format",
                "build-cli",
                "doctor",
                "plan-quarry",
                "plan-glossless",
                "host-plan-quarry",
            ],
        )
        self.assertEqual(
            phases[0]["argv"], ["cargo", "test", "--locked", "--lib", "--bins"]
        )
        self.assertEqual(
            sum(phase["argv"][:2] == ["cargo", "build"] for phase in phases), 1
        )
        self.assertTrue(all("sh" not in phase["argv"][:1] for phase in phases))

    def test_plans_are_path_free_and_retain_no_logs(self) -> None:
        for profile in ("fast", "required"):
            plan = read_plan(profile)
            encoded = json.dumps(plan, sort_keys=True)
            self.assertNotIn(str(ROOT), encoded)
            self.assertFalse(plan["retained_logs"])
            self.assertTrue(plan["source_must_remain_unchanged"])

    def test_source_state_detects_content_changes_with_the_same_git_status(self) -> None:
        source_state = runpy.run_path(str(VERIFY), run_name="glaeda_verify_test")[
            "source_state"
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
            tracked = root / "tracked.txt"
            tracked.write_text("base\n", encoding="utf-8")
            subprocess.run(["git", "add", "tracked.txt"], cwd=root, check=True)
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
                cwd=root,
                check=True,
            )

            tracked.write_text("first dirty value\n", encoding="utf-8")
            first_tracked = source_state(root)
            tracked.write_text("second dirty value\n", encoding="utf-8")
            self.assertNotEqual(first_tracked, source_state(root))

            untracked = root / "untracked.txt"
            untracked.write_text("first untracked value\n", encoding="utf-8")
            first_untracked = source_state(root)
            untracked.write_text("second untracked value\n", encoding="utf-8")
            self.assertNotEqual(first_untracked, source_state(root))


if __name__ == "__main__":
    unittest.main()
