#!/usr/bin/env python3
"""Tests for glaeda_runner_labels: what it refuses and accepts, and that this repository's workflows pass."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import glaeda_runner_labels as rl  # noqa: E402

HEAD = "name: x\non: push\njobs:\n"


def check(body: str, allowed: dict[tuple[str, str], str] | None = None) -> list[str]:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "w.yml"
        path.write_text(HEAD + body, encoding="utf-8")
        return rl.violations([path], allowed or {})


class Refuses(unittest.TestCase):
    def test_hosted_labels_in_every_spelling(self) -> None:
        for runs_on in (
            "    runs-on: ubuntu-latest\n",
            "    runs-on: ubuntu-24.04\n",
            "    runs-on: ubuntu-22.04\n",
            "    runs-on: [ubuntu-24.04-arm]\n",
            "    runs-on: ${{ vars.LINUX_RUNNER || 'ubuntu-24.04' }}\n",
            "    runs-on: macos-15\n",
            "    runs-on: macos-latest\n",
            "    runs-on: windows-2025\n",
            "    runs-on: ubuntu-slim\n",
        ):
            with self.subTest(runs_on=runs_on):
                self.assertTrue(check("  a:\n" + runs_on + "    steps: [{run: true}]\n"))

    def test_matrix_row_counts_for_its_job(self) -> None:
        body = ("  a:\n    strategy:\n      matrix:\n        include:\n          - runner: ubuntu-24.04\n"
                "    runs-on: ${{ matrix.runner }}\n    steps: [{run: true}]\n")
        self.assertIn("job a", check(body)[0])

    def test_allowlist_is_per_job_and_needs_a_reason(self) -> None:
        body = "  a:\n    runs-on: ubuntu-24.04\n  b:\n    runs-on: ubuntu-24.04\n"
        problems = check(body, {("w.yml", "a"): "a reason"})
        self.assertEqual(len(problems), 1)
        self.assertIn("job b", problems[0])
        self.assertTrue(check("  a:\n    runs-on: ubuntu-24.04\n", {("w.yml", "a"): " "}))

    def test_stale_allowlist_entry(self) -> None:
        problems = check("  a:\n    runs-on: blacksmith-4vcpu-ubuntu-2404\n", {("w.yml", "a"): "a reason"})
        self.assertEqual(len(problems), 1)
        self.assertIn("names no GitHub-hosted label", problems[0])


class Accepts(unittest.TestCase):
    def test_blacksmith_owned_comments_and_shell(self) -> None:
        body = (
            "  a:\n"
            "    runs-on: blacksmith-4vcpu-ubuntu-2404\n"
            "  b:\n"
            "    runs-on: blacksmith-16vcpu-ubuntu-2404-arm  # was ubuntu-24.04\n"
            "  c:\n"
            "    name: compare with ubuntu-24.04\n"
            "    runs-on: glaeda-owned-linux\n"
            "    steps:\n"
            "      - run: echo ubuntu-latest\n"
            "      - run: |\n"
            "          echo macos-15\n"
            "          echo ubuntu-24.04\n"
            "      # runs-on: ubuntu-latest was the old label\n"
        )
        self.assertEqual(check(body), [])


class Repository(unittest.TestCase):
    def test_workflows_pass(self) -> None:
        self.assertEqual(rl.violations(rl.workflow_files([])), [])

    def test_every_allowlist_entry_has_a_reason(self) -> None:
        for key, reason in rl.HOSTED_ALLOWED.items():
            self.assertTrue(reason.strip(), key)

    def test_jobs_in_the_fleet_fork_run_on_blacksmith(self) -> None:
        # manaflow-ai/glaeda is an organization repository with Blacksmith installed, so a job that runs only
        # there never takes a hosted image.
        for path in rl.workflow_files([]):
            text = path.read_text(encoding="utf-8")
            if "github.repository == 'manaflow-ai/glaeda'" in text:
                self.assertEqual(rl.hosted_jobs(path), {}, path.name)


if __name__ == "__main__":
    unittest.main()
