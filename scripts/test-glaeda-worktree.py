#!/usr/bin/env python3
"""Fixture contracts for canonical worktree creation."""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class WorktreeTest(unittest.TestCase):
    def test_no_build_adds_under_canonical_root(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            home = Path(td) / "home"
            projects = home / "Projects"
            repo = projects / "demo"
            repo.mkdir(parents=True)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.invalid"], check=True)
            subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
            (repo / "README").write_text("ok\n")
            subprocess.run(["git", "-C", str(repo), "add", "README"], check=True)
            subprocess.run(["git", "-C", str(repo), "commit", "-qm", "initial"], check=True)
            result = subprocess.run([str(ROOT / "scripts/glaeda-worktree"), "add", "--no-build",
                                     str(repo), "feature"], env={**os.environ, "HOME": str(home)},
                                    capture_output=True, text=True, check=True)
            target = Path(result.stdout.strip())
            self.assertEqual(target, projects / "worktrees/demo/feature")
            self.assertTrue((target / "README").is_file())


if __name__ == "__main__":
    unittest.main()
