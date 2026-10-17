#!/usr/bin/env python3
"""Deterministic tests for the internal task-private patch mutation transaction."""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import owned_linux_task as owned_task
import task_private_patch as patching


REPOSITORY = "teamleaderleo/glaeda"


class Fixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve(strict=True)
        os.chmod(self.root, 0o700)
        self.repository = self.root / "repository"
        self.repository.mkdir(mode=0o700)
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        run(["git", "init", "--quiet"], self.repository)
        run(["git", "config", "user.email", "patch@example.invalid"], self.repository)
        run(["git", "config", "user.name", "Patch Fixture"], self.repository)
        run(["git", "remote", "add", "origin", f"https://github.com/{REPOSITORY}.git"], self.repository)
        (self.repository / ".gitignore").write_text("/target/\n", encoding="utf-8")
        (self.repository / "tracked.txt").write_text("old\n", encoding="utf-8")
        run(["git", "add", "."], self.repository)
        run(["git", "commit", "--quiet", "-m", "fixture"], self.repository)
        self.head, self.tree = run(
            ["git", "rev-parse", "HEAD", "HEAD^{tree}"], self.repository
        ).stdout.decode("ascii").splitlines()

    def close(self) -> None:
        self.temporary.cleanup()

    def identity(self, patch: bytes) -> dict[str, object]:
        framed = f"blob {len(patch)}\0".encode("ascii") + patch
        return {
            "git_blob_sha1": hashlib.sha1(framed).hexdigest(),
            "sha256": f"sha256:{hashlib.sha256(patch).hexdigest()}",
            "bytes_": len(patch),
        }

    def apply(self, patch: bytes, **extra):
        return patching.apply_task_private_patch(
            repository_root=self.repository,
            state_root=self.state,
            repository=REPOSITORY,
            expected_head=self.head,
            expected_tree=self.tree,
            patch=patch,
            **self.identity(patch),
            **extra,
        )

    def recover(self, patch: bytes):
        return patching.recover_ambiguous_task_private_patch(
            repository_root=self.repository,
            state_root=self.state,
            repository=REPOSITORY,
            expected_head=self.head,
            expected_tree=self.tree,
            patch=patch,
            **self.identity(patch),
        )

    def operation_root(self, patch: bytes) -> Path:
        request = patching.normalize_request(
            REPOSITORY,
            self.head,
            self.tree,
            self.identity(patch)["git_blob_sha1"],
            self.identity(patch)["sha256"],
            self.identity(patch)["bytes_"],
            patch,
        )
        return self.state / request.operation_id.removeprefix("sha256:")


def run(argv: list[str], cwd: Path) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        argv,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )


def edit_patch() -> bytes:
    return (
        b"diff --git a/tracked.txt b/tracked.txt\n"
        b"--- a/tracked.txt\n"
        b"+++ b/tracked.txt\n"
        b"@@ -1 +1 @@\n"
        b"-old\n"
        b"+new\n"
    )


class TaskPrivatePatchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_exact_patch_mutates_only_fresh_task_checkout(self) -> None:
        patch = edit_patch()
        result = self.fixture.apply(patch)
        self.assertEqual(result.receipt["terminal_class"], "applied")
        self.assertEqual(result.receipt["changed_paths"], ["tracked.txt"])
        self.assertTrue(result.receipt["head_unchanged"])
        self.assertFalse(result.receipt["authorizes_commit"])
        self.assertFalse(result.receipt["authorizes_publication"])
        assert result.source is not None
        self.assertEqual((result.source / "tracked.txt").read_text(), "new\n")
        self.assertEqual((self.fixture.repository / "tracked.txt").read_text(), "old\n")
        self.assertEqual(
            run(["git", "rev-parse", "HEAD"], result.source).stdout.decode().strip(),
            self.fixture.head,
        )
        self.assertEqual(
            run(["git", "diff", "--cached", "--name-only", "HEAD"], result.source).stdout,
            b"tracked.txt\n",
        )

    def test_terminal_replay_returns_same_receipt_without_second_apply(self) -> None:
        patch = edit_patch()
        first = self.fixture.apply(patch)
        receipt_path = self.fixture.operation_root(patch) / "receipt.json"
        before = receipt_path.read_bytes()
        mtime = receipt_path.stat().st_mtime_ns
        second = self.fixture.apply(patch, apply_runner=lambda *_: self.fail("replay applied twice"))
        self.assertEqual(first.receipt, second.receipt)
        self.assertEqual(receipt_path.read_bytes(), before)
        self.assertEqual(receipt_path.stat().st_mtime_ns, mtime)

    def test_sibling_task_and_resident_repository_stay_unchanged(self) -> None:
        sibling = self.fixture.root / "sibling"
        sibling.mkdir(mode=0o700)
        sibling_source = owned_task.materialize(
            self.fixture.repository,
            sibling,
            self.fixture.head,
            self.fixture.tree,
        )
        patch = edit_patch()
        self.fixture.apply(patch)
        self.assertEqual((self.fixture.repository / "tracked.txt").read_text(), "old\n")
        self.assertEqual((sibling_source / "tracked.txt").read_text(), "old\n")
        self.assertEqual(
            run(
                ["git", "status", "--porcelain=v1", "--untracked-files=all"],
                sibling_source,
            ).stdout,
            b"",
        )

    def test_git_executable_configuration_refuses_before_apply(self) -> None:
        patch = edit_patch()

        def poison(source: Path) -> None:
            run(["git", "config", "--local", "filter.evil.clean", "/bin/true"], source)

        with self.assertRaisesRegex(patching.Refusal, "configuration contains executable"):
            self.fixture.apply(patch, after_materialize=poison)
        operation = self.fixture.operation_root(patch)
        self.assertFalse((operation / "task").exists())
        self.assertFalse((operation / "intent.json").exists())
        self.assertFalse((operation / "receipt.json").exists())

    def test_final_applicability_drift_refuses_without_retaining_task(self) -> None:
        patch = edit_patch()

        def drift(source: Path) -> None:
            (source / "tracked.txt").write_text("drift\n", encoding="utf-8")

        with self.assertRaisesRegex(patching.Refusal, "applicability changed"):
            self.fixture.apply(patch, before_apply_process=drift)
        operation = self.fixture.operation_root(patch)
        self.assertFalse((operation / "task").exists())
        self.assertFalse((operation / "intent.json").exists())

    def test_process_failure_discards_task_and_records_rebuild(self) -> None:
        patch = edit_patch()
        result = self.fixture.apply(patch, apply_runner=lambda *_: 1)
        self.assertEqual(result.receipt["terminal_class"], "ambiguous_rebuild_required")
        self.assertEqual(result.receipt["changed_paths"], [])
        self.assertIsNone(result.receipt["result_tree"])
        self.assertIsNone(result.source)
        self.assertFalse((self.fixture.operation_root(patch) / "task").exists())

    def test_crash_after_applying_intent_never_redispatches(self) -> None:
        patch = edit_patch()

        def crash(*_):
            raise patching.InjectedCrash()

        with self.assertRaises(patching.InjectedCrash):
            self.fixture.apply(patch, apply_runner=crash)
        operation = self.fixture.operation_root(patch)
        self.assertTrue((operation / "intent.json").is_file())
        with self.assertRaisesRegex(patching.AmbiguousMutation, "rebuild recovery"):
            self.fixture.apply(patch)
        recovered = self.fixture.recover(patch)
        self.assertEqual(recovered.receipt["terminal_class"], "ambiguous_rebuild_required")
        self.assertFalse((operation / "task").exists())
        self.assertFalse((operation / "intent.json").exists())
        replay = self.fixture.recover(patch)
        self.assertEqual(replay.receipt, recovered.receipt)

    def test_git_metadata_path_is_rejected_before_state_creation(self) -> None:
        patch = (
            b"diff --git a/.git/config b/.git/config\n"
            b"--- a/.git/config\n"
            b"+++ b/.git/config\n"
            b"@@ -0,0 +1 @@\n"
            b"+bad\n"
        )
        with self.assertRaisesRegex(patching.Refusal, "path is unsafe"):
            self.fixture.apply(patch)
        self.assertEqual(list(self.fixture.state.iterdir()), [])

    def test_parent_traversal_path_is_rejected_before_state_creation(self) -> None:
        patch = (
            b"diff --git a/../escape b/../escape\n"
            b"--- a/../escape\n"
            b"+++ b/../escape\n"
            b"@@ -0,0 +1 @@\n"
            b"+bad\n"
        )
        with self.assertRaisesRegex(patching.Refusal, "path is unsafe"):
            self.fixture.apply(patch)
        self.assertEqual(list(self.fixture.state.iterdir()), [])

    def test_patch_identity_mismatch_refuses_before_state_creation(self) -> None:
        patch = edit_patch()
        identity = self.fixture.identity(patch)
        with self.assertRaisesRegex(patching.Refusal, "SHA-256 does not match"):
            patching.apply_task_private_patch(
                repository_root=self.fixture.repository,
                state_root=self.fixture.state,
                repository=REPOSITORY,
                expected_head=self.fixture.head,
                expected_tree=self.fixture.tree,
                git_blob_sha1=identity["git_blob_sha1"],
                sha256="sha256:" + "0" * 64,
                bytes_=identity["bytes_"],
                patch=patch,
            )
        self.assertEqual(list(self.fixture.state.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
