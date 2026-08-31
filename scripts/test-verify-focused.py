#!/usr/bin/env python3
"""Deterministic tests for the fixed credentialless verification profiles."""

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

    def test_required_profile_is_fixed_and_uses_repository_required_recipe(self) -> None:
        completed = subprocess.run(
            [sys.executable, ROOT / "scripts" / "verify-required", "profile"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        profile = json.loads(completed.stdout)
        self.assertEqual(profile["profile_id"], "verify-required/v1")
        self.assertEqual(profile["profile_class"], "verify_required")
        self.assertEqual(profile["resource_class"], "big-red-required")
        self.assertEqual(profile["deadline_seconds"], 1200)
        self.assertEqual(profile["build_tmpfs_bytes"], 8 * 1024 * 1024 * 1024)
        self.assertEqual(profile["recipe"], ["scripts/verify", "required"])
        self.assertIn("MemoryHigh=10G", profile["systemd_properties"])
        self.assertIn("MemoryMax=12G", profile["systemd_properties"])
        self.assertEqual(
            profile["profile_generation"],
            MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
        )
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
        cache_target = "/cargo-home/registry/cache/index.crates.io-public"
        cache_index = command.index(cache_target)
        self.assertEqual(command[cache_index - 2], "--ro-bind")
        self.assertNotIn("/cargo-home/git", joined)
        self.assertNotIn("unrelated-project", joined)
        self.assertNotIn("SSH_AUTH_SOCK", joined)
        self.assertNotIn("GITHUB_TOKEN", joined)
        for forbidden_host_state in (
            "/etc/shadow",
            "/etc/gshadow",
            "/etc/ssh",
            "/run",
            "/var/run",
        ):
            self.assertNotIn(forbidden_host_state, command)
        self.assertEqual(
            [
                argument.removeprefix("--property=")
                for argument in command
                if argument.startswith("--property=")
            ],
            MODULE.PROFILE_SPEC["systemd_properties"],
        )

    def test_required_sandbox_selects_only_the_named_required_recipe(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("source", "cargo", "rustup"):
                (root / name).mkdir()
            (root / "cargo" / "bin").mkdir()
            command = MODULE.sandbox_command(
                root / "source",
                root,
                root / "cargo",
                root / "rustup",
                "exact.service",
                MODULE.REQUIRED_PROFILE,
            )
        joined = "\0".join(command)
        self.assertIn("/workspace/source/scripts/verify\0required", joined)
        self.assertIn("--tmpfs\0/workspace/source/target", joined)
        self.assertIn("CARGO_TARGET_DIR\0/workspace/source/target", joined)
        self.assertNotIn("/workspace/target", command)
        self.assertNotIn("/workspace/source/scripts/verify\0focused", joined)
        self.assertIn("--property=RuntimeMaxSec=1200", command)
        self.assertIn("--property=MemoryHigh=10G", command)
        self.assertIn("--property=MemoryMax=12G", command)
        self.assertIn(str(MODULE.REQUIRED_TARGET_TMPFS_BYTES), command)
        for safe_host_fact in (
            "/etc/os-release",
            "/etc/nsswitch.conf",
            "/etc/passwd",
            "/etc/group",
            "/etc/subuid",
            "/etc/subgid",
            "/etc/containers",
            "/var/lib/dpkg",
        ):
            self.assertIn(safe_host_fact, command)

    def test_focused_sandbox_does_not_receive_required_host_plan_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("source", "cargo", "rustup"):
                (root / name).mkdir()
            (root / "cargo" / "bin").mkdir()
            command = MODULE.sandbox_command(
                root / "source",
                root,
                root / "cargo",
                root / "rustup",
                "exact.service",
            )
        for required_only_fact in ("/etc/os-release", "/etc/passwd", "/var/lib/dpkg"):
            self.assertNotIn(required_only_fact, command)

    def test_materialized_source_prepares_only_the_ignored_target_mountpoint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            repository = root / "repository"
            repository.mkdir()
            subprocess.run(["git", "init", "--quiet"], cwd=repository, check=True)
            subprocess.run(
                ["git", "config", "user.email", "verify@example.invalid"],
                cwd=repository,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "Verify Fixture"],
                cwd=repository,
                check=True,
            )
            (repository / ".gitignore").write_text("/target/\n", encoding="utf-8")
            (repository / "tracked").write_text("exact\n", encoding="utf-8")
            subprocess.run(["git", "add", "."], cwd=repository, check=True)
            subprocess.run(["git", "commit", "--quiet", "-m", "fixture"], cwd=repository, check=True)
            commit, tree = subprocess.run(
                ["git", "rev-parse", "HEAD", "HEAD^{tree}"],
                cwd=repository,
                check=True,
                text=True,
                stdout=subprocess.PIPE,
            ).stdout.splitlines()
            request = MODULE.Request(
                repository="teamleaderleo/glaeda",
                commit=commit,
                tree=tree,
                profile_generation=MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
                command_fingerprint="sha256:" + "a" * 64,
                profile=MODULE.REQUIRED_PROFILE,
            )
            task = root / "task"
            task.mkdir()
            source = MODULE.materialize(repository, task, request)
            self.assertTrue((source / "target").is_dir())
            self.assertEqual(
                subprocess.run(
                    ["git", "status", "--porcelain=v1", "-z", "--untracked-files=all"],
                    cwd=source,
                    check=True,
                    stdout=subprocess.PIPE,
                ).stdout,
                b"",
            )
    def test_required_receipt_has_distinct_profile_identity(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
            "sha256:" + "e" * 64,
            MODULE.REQUIRED_PROFILE,
        )
        document = MODULE.receipt(
            request, "succeeded", 0, 1.0, True, True, 0, MODULE.sha256(b""), 1, 2
        )
        self.assertEqual(document["document_type"], "glaeda-verify-required-receipt")
        self.assertEqual(document["profile"]["id"], "verify-required/v1")
        self.assertTrue(MODULE.valid_terminal_receipt(document, request))

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
            root = Path(temporary).resolve(strict=True)
            os.chmod(root, 0o700)
            path = root / "receipt.json"
            MODULE.publish_document(path, document, replace=False)
            self.assertEqual(MODULE.read_document(path), document)
            self.assertTrue(MODULE.matches_request(document, request))
            self.assertTrue(MODULE.valid_terminal_receipt(document, request))

    def test_receipt_replay_rejects_incomplete_security_claims(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(),
            "sha256:" + "c" * 64,
        )
        original = MODULE.receipt(
            request,
            "succeeded",
            0,
            1.0,
            True,
            True,
            1,
            MODULE.sha256(b"x"),
            1,
            2,
        )
        mutations = (
            ("result", "task_cleanup_complete", False),
            ("result", "output_sha256", "not-a-digest"),
            ("result", "resource_accounting", "none"),
            ("isolation", "filesystem", "host_writable"),
            ("isolation", "ambient_environment", "inherited"),
            ("isolation", "unrelated_writable_projects", "present"),
            ("isolation", "build_state", "host_shared"),
            ("isolation", "package_cache", "host_private_writable"),
        )
        for section, key, value in mutations:
            with self.subTest(section=section, key=key):
                document = json.loads(json.dumps(original))
                document[section][key] = value
                self.assertFalse(MODULE.valid_terminal_receipt(document, request))

        document = json.loads(json.dumps(original))
        document["unexpected"] = True
        self.assertFalse(MODULE.valid_terminal_receipt(document, request))

    def test_required_replay_rejects_impossible_terminal_evidence(self) -> None:
        request = MODULE.Request(
            "teamleaderleo/glaeda",
            "a" * 40,
            "b" * 40,
            MODULE.profile_generation(MODULE.REQUIRED_PROFILE),
            "sha256:" + "f" * 64,
            MODULE.REQUIRED_PROFILE,
        )
        original = MODULE.receipt(
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
        for key, value in (
            ("exit_code", 9),
            ("elapsed_seconds", float("inf")),
            ("elapsed_seconds", float("nan")),
        ):
            with self.subTest(key=key, value=value):
                document = json.loads(json.dumps(original))
                document["result"][key] = value
                self.assertFalse(MODULE.valid_terminal_receipt(document, request))

    def test_non_finite_json_state_is_corrupt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary).resolve(strict=True) / "receipt.json"
            path.write_bytes(b'{"elapsed_seconds":Infinity}\n')
            os.chmod(path, 0o600)
            with self.assertRaisesRegex(MODULE.Refusal, "state is corrupt"):
                MODULE.read_document(path)

    def test_reconcile_without_receipt_never_executes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(strict=True)
            os.chmod(root, 0o700)
            for name in ("repository", "cargo", "rustup"):
                (root / name).mkdir()
            arguments = mock.Mock(
                repository_root=str(root / "repository"),
                state_root=str(root / "state"),
                cargo_root=str(root / "cargo"),
                rustup_root=str(root / "rustup"),
                repository="teamleaderleo/glaeda",
                commit="a" * 40,
                tree="b" * 40,
                profile_generation=MODULE.profile_generation(),
                command_fingerprint="sha256:" + "c" * 64,
                reconcile_only=True,
            )
            with (
                mock.patch.object(MODULE, "verify_resident_source"),
                mock.patch.object(MODULE, "materialize") as materialize,
                mock.patch.object(MODULE, "execute_profile") as execute,
            ):
                with self.assertRaisesRegex(MODULE.Refusal, "no terminal receipt"):
                    MODULE.run(arguments)
                materialize.assert_not_called()
                execute.assert_not_called()

    def test_failed_unit_observation_is_not_settled(self) -> None:
        failed = subprocess.CompletedProcess([], 1, stdout=b"", stderr=b"")
        absent = subprocess.CompletedProcess([], 0, stdout=b"not-found\n", stderr=b"")
        with mock.patch.object(MODULE.subprocess, "run", return_value=failed):
            self.assertFalse(MODULE.unit_absent("exact.service"))
        with mock.patch.object(MODULE.subprocess, "run", return_value=absent):
            self.assertTrue(MODULE.unit_absent("exact.service"))

    def test_cleanup_oserror_publishes_terminal_receipt(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve(strict=True)
            os.chmod(root, 0o700)
            for name in ("repository", "cargo", "rustup"):
                (root / name).mkdir()
            arguments = mock.Mock(
                repository_root=str(root / "repository"),
                state_root=str(root / "state"),
                cargo_root=str(root / "cargo"),
                rustup_root=str(root / "rustup"),
                repository="teamleaderleo/glaeda",
                commit="a" * 40,
                tree="b" * 40,
                profile_generation=MODULE.profile_generation(),
                command_fingerprint="sha256:" + "d" * 64,
                reconcile_only=False,
            )
            with (
                mock.patch.object(MODULE, "verify_resident_source"),
                mock.patch.object(MODULE, "materialize", return_value=root / "source"),
                mock.patch.object(
                    MODULE,
                    "execute_profile",
                    return_value=("succeeded", 0, 1.0, True, 0, MODULE.sha256(b"")),
                ),
                mock.patch.object(MODULE, "remove_task", side_effect=[None, OSError("busy")]),
                mock.patch.object(MODULE, "emit"),
            ):
                self.assertEqual(MODULE.run(arguments), 0)
            receipt = MODULE.read_document(
                root / "state" / ("d" * 64) / "receipt.json"
            )
            self.assertEqual(receipt["result"]["terminal_class"], "cleanup_incomplete")
            self.assertFalse(receipt["result"]["task_cleanup_complete"])


if __name__ == "__main__":
    unittest.main()
