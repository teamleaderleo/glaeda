#!/usr/bin/env python3
"""Deterministic tests for verify-focused/v1."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "verify_focused_impl", ROOT / "scripts" / "verify_focused_impl.py"
)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class VerifyFocusedTests(unittest.TestCase):
    def test_profile_is_fixed_compact_and_credentialless(self) -> None:
        completed = subprocess.run(
            [sys.executable, ROOT / "scripts" / "verify-focused", "profile"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        profile = json.loads(completed.stdout)
        self.assertEqual(profile["profile_id"], "verify-focused/v1")
        self.assertEqual(profile["execution_identity_class"], "credentialless_project")
        self.assertEqual(profile["recipe"], ["scripts/verify", "focused"])
        self.assertEqual(profile["source_network"], "none")
        self.assertEqual(profile["profile_generation"], MODULE.profile_generation())
        self.assertLess(len(completed.stdout), 2_000)

    def test_cli_has_no_remote_command_environment_or_url(self) -> None:
        help_text = subprocess.run(
            [sys.executable, ROOT / "scripts" / "verify-focused", "run", "--help"],
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        ).stdout
        for forbidden in ("--shell", "--argv", "--environment", "--remote-url", "--executable"):
            self.assertNotIn(forbidden, help_text)
        self.assertIn("--repository", help_text)
        self.assertIn("--commit", help_text)
        self.assertIn("--tree", help_text)

    def test_sandbox_clears_authority_and_owns_recipe(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("source", "cargo", "rustup"):
                (root / name).mkdir()
            (root / "cargo" / "bin").mkdir()
            for kind in ("cache", "index", "src"):
                cache = root / "cargo" / "registry" / kind / "index.crates.io-public"
                cache.mkdir(parents=True)
            unrelated = root / "cargo" / "git" / "checkouts" / "unrelated-project"
            unrelated.mkdir(parents=True)
            command = MODULE.sandbox_command(
                root / "source",
                root,
                root / "cargo",
                root / "rustup",
                "exact.service",
            )
        joined = "\0".join(command)
        self.assertIn("--unshare-all", command)
        self.assertIn("--disable-userns", command)
        self.assertIn("--clearenv", command)
        self.assertIn(str(MODULE.TARGET_TMPFS_BYTES), command)
        self.assertIn("--property=NoNewPrivileges=yes", command)
        self.assertIn("--property=RestrictSUIDSGID=yes", command)
        self.assertIn("--pipe", command)
        self.assertNotIn("--property=StandardOutput=journal", command)
        self.assertIn("/workspace/source/scripts/verify\0focused", joined)
        self.assertIn("/cargo-home/registry/cache/index.crates.io-public", command)
        self.assertNotIn("/cargo-home/git", joined)
        self.assertNotIn("unrelated-project", joined)
        self.assertNotIn("SSH_AUTH_SOCK", joined)
        self.assertNotIn("GITHUB_TOKEN", joined)
        self.assertNotIn("STENSIBLY", joined)

    def test_exact_receipt_replays_without_execution(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(),
            "sha256:" + "c" * 64,
        )
        document = MODULE.receipt(
            request,
            "succeeded",
            0,
            1.0,
            True,
            True,
            0,
            MODULE.sha256(b""),
            1,
            2,
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            os.chmod(root, 0o700)
            path = root / "receipt.json"
            MODULE.publish_document(path, document, replace=False)
            self.assertEqual(MODULE.read_document(path), document)
            self.assertTrue(MODULE.matches_request(document, request))
            self.assertTrue(MODULE.valid_terminal_receipt(document, request))

    def test_reconcile_without_receipt_never_executes(self) -> None:
        with mock.patch.object(MODULE, "execute_profile") as execute:
            arguments = mock.Mock(
                repository="teamleaderleo/glaeda",
                commit="a" * 40,
                tree="b" * 40,
                profile_generation=MODULE.profile_generation(),
                command_fingerprint="sha256:" + "c" * 64,
                reconcile_only=True,
            )
            request = MODULE.normalize_request(arguments)
            self.assertEqual(request.repository, "teamleaderleo/glaeda")
            execute.assert_not_called()


if __name__ == "__main__":
    unittest.main()
