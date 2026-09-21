#!/usr/bin/env python3
from __future__ import annotations

from contextlib import contextmanager, redirect_stderr
import argparse
import io
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock

import github_actions_owned_linux_runner as runner
import owned_linux_task as owned_task
from owned_linux_task import Refusal


class FakeStdin:
    def __init__(self, value: bytes):
        self.buffer = io.BytesIO(value)


class FakeAdmission:
    def __init__(self):
        self.owned = True
        self.launch_attempted = False
        self.released = False

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False

    @contextmanager
    def launch(self):
        self.launch_attempted = True
        yield

    def release(self):
        self.released = True
        self.owned = False


def arguments(root: Path, **overrides) -> argparse.Namespace:
    values = {
        "state_root": str(root / "state"),
        "admission_root": str(root / "admission"),
        "attempt_id": "attempt-1010",
        "assignment_id": "assignment-41",
        "repository": runner.TRUSTED_REPOSITORY,
        "runner_label": runner.TRUSTED_RUNNER_LABEL,
        "workflow_run_id": 1234,
        "job_id": "job-55",
        "runner_id": 77,
        "runner_name": "glaeda-big-red-77",
        "runner_generation": "generation-9",
        "expires_at_unix_ms": time.time_ns() // 1_000_000 + 600_000,
        "github_terminal": "success",
        "retired_runner_id": 77,
    }
    values.update(overrides)
    return argparse.Namespace(**values)


class OwnedLinuxJitRunnerTests(unittest.TestCase):
    def test_profile_is_closed_to_one_trusted_repository_label_and_named_boundary(self):
        self.assertEqual(runner.PROFILE_SPEC["repository"], "teamleaderleo/quarry")
        self.assertEqual(
            runner.PROFILE_SPEC["runner_label"], "glaeda-big-red-trusted"
        )
        self.assertEqual(
            runner.PROFILE_SPEC["network_class"],
            "github_actions_trusted_host_egress_v1",
        )
        self.assertEqual(runner.PROFILE_SPEC["resource_class"], runner.RESOURCE_CLASS)
        self.assertIn("CPUQuota=400%", runner.SYSTEMD_PROPERTIES)
        self.assertIn("MemoryMax=8G", runner.SYSTEMD_PROPERTIES)
        self.assertIn("TasksMax=512", runner.SYSTEMD_PROPERTIES)
        self.assertEqual(runner.ISOLATION["glaeda_control_credentials"], "absent")
        self.assertEqual(runner.ISOLATION["publisher_credentials"], "absent")
        self.assertEqual(runner.ISOLATION["ssh_agent"], "absent")
        self.assertEqual(runner.ISOLATION["sudo_admin"], "absent")
        self.assertEqual(runner.ISOLATION["docker_podman_host_socket"], "absent")

    def test_wrong_repository_or_runner_label_refuses(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            with self.assertRaisesRegex(Refusal, "repository"):
                runner.normalize(arguments(root, repository="other/repo"))
            with self.assertRaisesRegex(Refusal, "runner label"):
                runner.normalize(arguments(root, runner_label="self-hosted"))

    def test_jit_reader_accepts_one_line_and_refuses_multiline_without_string_copy_contract(self):
        secret_value = b"abcDEF0123+/_=-\n"
        with mock.patch.object(runner.sys, "stdin", FakeStdin(secret_value)):
            secret = runner.read_jit_secret()
        self.assertIsInstance(secret, bytearray)
        self.assertEqual(secret, secret_value)
        secret[:] = b"\x00" * len(secret)

        with mock.patch.object(runner.sys, "stdin", FakeStdin(b"one\ntwo\n")):
            with self.assertRaisesRegex(Refusal, "more than one line"):
                runner.read_jit_secret()

    def test_shared_secret_stdin_executor_zeroizes_and_suppresses_failure_tails(self):
        secret = bytearray(b"super-secret-jit\n")
        observed = {}

        def fake_run(command, **kwargs):
            observed.update(kwargs)
            self.assertEqual(kwargs["secret_stdin"], secret)
            return (
                "failed",
                1,
                0.1,
                True,
                10,
                "sha256:" + "0" * 64,
                b"",
                False,
                0,
                "sha256:" + "0" * 64,
            )

        with mock.patch.object(owned_task, "_run_bounded", side_effect=fake_run):
            result = owned_task.execute_secret_stdin(
                ["/fixed/program"],
                unit="fixed.service",
                deadline_seconds=10,
                label="secret-test",
                secret_stdin=secret,
            )
        self.assertEqual(result[0], "failed")
        self.assertEqual(secret, bytearray(len(secret)))
        self.assertIs(observed["emit_failure_tail"], False)
        self.assertEqual(observed["retain_limit"], 0)

    def test_reflected_jit_bytes_never_reach_failure_output(self):
        python = Path("/usr/bin/python3")
        if not python.is_file():
            self.skipTest("system Python is unavailable")
        secret_value = b"JIT-REFLECTION-SENTINEL-1010\n"
        secret = bytearray(secret_value)
        captured = io.StringIO()
        script = (
            "import os,sys; data=sys.stdin.buffer.read(); "
            "os.write(1,data); os.write(2,data); raise SystemExit(7)"
        )
        with redirect_stderr(captured):
            terminal, code, _, _, output_bytes, _ = owned_task.execute_secret_stdin(
                [str(python), "-c", script],
                unit="glaeda-jit-secret-reflection-test-absent.service",
                deadline_seconds=10,
                label="jit-reflection-test",
                secret_stdin=secret,
            )
        self.assertEqual((terminal, code), ("failed", 7))
        self.assertEqual(output_bytes, len(secret_value) * 2)
        self.assertEqual(secret, bytearray(len(secret)))
        self.assertNotIn("JIT-REFLECTION-SENTINEL-1010", captured.getvalue())

    def test_sandbox_uses_trusted_egress_and_only_task_private_runner_mounts(self):
        with tempfile.TemporaryDirectory() as raw:
            task = Path(raw)
            runner_root = task / "runner"
            runner_root.mkdir()
            launcher = task / "launcher"
            launcher.write_text("#!/bin/sh\n", encoding="utf-8")
            request = runner.normalize(arguments(task))
            command = runner.sandbox_command(task, runner_root, launcher, request)
        joined = "\n".join(command)
        self.assertIn("--share-net", command)
        self.assertIn("/opt/smolrunner/actions-runner", command)
        self.assertIn("/opt/smolrunner/bin/smolrunner-jit-launcher", command)
        self.assertIn("--clearenv", command)
        self.assertIn("/etc/passwd", command)
        self.assertIn("/etc/group", command)
        self.assertNotIn("SSH_AUTH_SOCK", joined)
        self.assertNotIn("/var/run/docker.sock", joined)
        self.assertNotIn("/run/podman", joined)
        self.assertNotIn("super-secret", joined)
        self.assertNotIn("/home/", joined.replace("/home/project", ""))

    def test_expired_attempt_refuses_before_physical_admission_or_launch(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args = arguments(
                root, expires_at_unix_ms=time.time_ns() // 1_000_000 - 1
            )
            with mock.patch.object(runner.owned_admission, "Reservation") as reservation:
                with mock.patch.object(
                    runner.owned_task, "execute_secret_stdin"
                ) as execute:
                    with self.assertRaisesRegex(Refusal, "expired"):
                        runner.run_once(args)
            reservation.assert_not_called()
            execute.assert_not_called()

    def test_hold_drain_pressure_and_capacity_refusals_launch_nothing(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for reason in (
                "node_held",
                "node_draining",
                "pressure_high",
                "capacity_unavailable",
            ):
                with self.subTest(reason=reason):
                    case_root = root / reason
                    case_root.mkdir()
                    args = arguments(case_root)

                    class Refusing:
                        def __enter__(self):
                            raise Refusal(reason)

                        def __exit__(self, *_):
                            return False

                    with mock.patch.object(
                        runner.owned_admission, "Reservation", return_value=Refusing()
                    ):
                        with mock.patch.object(
                            runner.owned_task, "execute_secret_stdin"
                        ) as execute:
                            with self.assertRaisesRegex(Refusal, reason):
                                runner.run_once(args)
                    execute.assert_not_called()

    def _run_injected_exit(
        self,
        root: Path,
        secret_value: bytes = b"JITSECRET123\n",
        terminal: str = "succeeded",
        exit_code: int = 0,
    ):
        args = arguments(root)
        admission = FakeAdmission()

        def extract(task_root):
            path = task_root / "runner"
            path.mkdir()
            return path

        def launcher(task_root):
            path = task_root / "launcher"
            path.write_text("#!/bin/sh\n", encoding="utf-8")
            return path

        def execute(*_, secret_stdin, launch_guard, **__):
            self.assertEqual(secret_stdin, bytearray(secret_value))
            with launch_guard():
                pass
            secret_stdin[:] = b"\x00" * len(secret_stdin)
            return (
                terminal,
                exit_code,
                1.25,
                True,
                0,
                "sha256:" + "e" * 64,
            )

        patches = [
            mock.patch.object(
                runner.owned_admission, "Reservation", return_value=admission
            ),
            mock.patch.object(runner, "extract_reviewed_runner", side_effect=extract),
            mock.patch.object(runner, "reviewed_launcher", side_effect=launcher),
            mock.patch.object(
                runner, "read_jit_secret", return_value=bytearray(secret_value)
            ),
            mock.patch.object(
                runner.owned_task, "execute_secret_stdin", side_effect=execute
            ),
            mock.patch.object(runner, "sandbox_command", return_value=["fixed-runner"]),
        ]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        self.assertEqual(runner.run_once(args), 0)
        return args, admission

    def test_runner_exit_without_github_terminal_is_durable_and_replay_refuses(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, admission = self._run_injected_exit(root)
            self.assertTrue(admission.launch_attempted)
            self.assertFalse(admission.released)
            command_root = (
                Path(args.state_root) / runner.normalize(args).assignment_fingerprint()[7:]
            )
            receipt = runner.read_document(command_root / "runner-exit.json")
            self.assertIsNotNone(receipt)
            self.assertFalse(receipt["result"]["github_terminal_observed"])
            serialized = json.dumps(receipt, sort_keys=True)
            self.assertNotIn("JITSECRET123", serialized)

            with self.assertRaisesRegex(Refusal, "redispatch refused"):
                runner.run_once(args)

    def test_duplicate_assignment_with_new_local_attempt_cannot_launch_twice(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, _ = self._run_injected_exit(root)
            duplicate = arguments(root, attempt_id="attempt-1010-retry")
            self.assertEqual(
                runner.normalize(args).assignment_fingerprint(),
                runner.normalize(duplicate).assignment_fingerprint(),
            )
            self.assertNotEqual(
                runner.normalize(args).fingerprint(),
                runner.normalize(duplicate).fingerprint(),
            )
            with mock.patch.object(
                runner.owned_task, "execute_secret_stdin"
            ) as execute:
                with self.assertRaisesRegex(Refusal, "conflicts with exact attempt"):
                    runner.run_once(duplicate)
            execute.assert_not_called()

    def test_deadline_expiry_is_durable_and_keeps_capacity_reserved(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, admission = self._run_injected_exit(
                root, terminal="timed_out", exit_code=-15
            )
            self.assertTrue(admission.launch_attempted)
            self.assertFalse(admission.released)
            command_root = (
                Path(args.state_root)
                / runner.normalize(args).assignment_fingerprint()[7:]
            )
            receipt = runner.read_document(command_root / "runner-exit.json")
            self.assertEqual(receipt["result"]["terminal_class"], "timed_out")
            self.assertFalse(receipt["result"]["capacity_released"])
            self.assertFalse(receipt["result"]["github_terminal_observed"])

    def test_cancellation_stops_only_checkpointed_exact_unit_and_blocks_replay(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args = arguments(root)
            request = runner.normalize(args)
            state_root = runner.private_directory(args.state_root)
            command_root = runner.ensure_private_child(
                state_root, request.assignment_fingerprint()[7:]
            )
            runner.publish(
                command_root / "intent.json",
                runner.intent_document(request),
                replace=False,
            )
            stopped = []

            def stop(unit):
                stopped.append(unit)

            with mock.patch.object(runner.owned_task, "stop_unit", side_effect=stop):
                with mock.patch.object(
                    runner.owned_task, "unit_absent", return_value=True
                ):
                    self.assertEqual(runner.cancel(args), 0)
            self.assertEqual(stopped, [runner.unit_name(request)])
            cancellation = runner.read_document(command_root / "cancellation.json")
            self.assertTrue(runner.matches_request(cancellation, request))
            with mock.patch.object(
                runner.owned_task, "execute_secret_stdin"
            ) as execute:
                with self.assertRaisesRegex(
                    Refusal, "ambiguous|cancelled assignment"
                ):
                    runner.run_once(args)
            execute.assert_not_called()

    def test_wrong_runner_generation_cannot_settle_existing_attempt(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, _ = self._run_injected_exit(root)
            wrong = arguments(root, runner_generation="generation-10")
            with self.assertRaisesRegex(Refusal, "exit evidence"):
                runner.settle(wrong)
            self.assertNotEqual(
                runner.normalize(args).fingerprint(),
                runner.normalize(wrong).fingerprint(),
            )

    def test_controller_restart_settles_after_exact_terminal_and_runner_retirement(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, _ = self._run_injected_exit(root)
            recovered = []

            def recover(admission_root, fingerprint, unit, binding, observe):
                observe()
                recovered.append((admission_root, fingerprint, unit, binding))

            with mock.patch.object(
                runner.owned_admission, "recover", side_effect=recover
            ):
                self.assertEqual(runner.settle(args), 0)
            self.assertEqual(len(recovered), 1)
            command_root = (
                Path(args.state_root) / runner.normalize(args).assignment_fingerprint()[7:]
            )
            final = runner.read_document(command_root / "receipt.json")
            self.assertTrue(final["result"]["github_terminal_observed"])
            self.assertTrue(final["result"]["runner_retired"])
            self.assertTrue(final["result"]["task_cleanup_complete"])
            self.assertTrue(final["result"]["capacity_released"])
            self.assertFalse((command_root / "task").exists())

    def test_settlement_after_deadline_is_allowed_for_recovery(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, _ = self._run_injected_exit(root)
            expired = argparse.Namespace(
                **{
                    **vars(args),
                    "expires_at_unix_ms": runner.normalize(args).expires_at_unix_ms,
                }
            )
            # The request identity includes the original expiry; move the clock
            # past it instead of mutating the identity.
            future = expired.expires_at_unix_ms + 1
            with mock.patch.object(runner.time, "time_ns", return_value=future * 1_000_000):
                with mock.patch.object(runner.owned_admission, "recover") as recover:
                    recover.side_effect = lambda *call: call[-1]()
                    self.assertEqual(runner.settle(expired), 0)

    def test_live_process_tree_blocks_cleanup_and_capacity_release(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, _ = self._run_injected_exit(root)
            with mock.patch.object(runner.owned_task, "unit_absent", return_value=False):
                with mock.patch.object(runner.owned_admission, "recover") as recover:
                    with self.assertRaisesRegex(Refusal, "remains alive"):
                        runner.settle(args)
            recover.assert_not_called()

    def test_cleanup_failure_preserves_capacity_reservation(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, _ = self._run_injected_exit(root)
            with mock.patch.object(
                runner.owned_task, "remove_task", side_effect=Refusal("cleanup failed")
            ):
                with mock.patch.object(runner.owned_admission, "recover") as recover:
                    with self.assertRaisesRegex(Refusal, "cleanup failed"):
                        runner.settle(args)
            recover.assert_not_called()

    def test_retired_runner_id_must_match_exact_bound_identity(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            args, _ = self._run_injected_exit(root)
            wrong = argparse.Namespace(**{**vars(args), "retired_runner_id": 78})
            with mock.patch.object(runner.owned_admission, "recover") as recover:
                with self.assertRaisesRegex(Refusal, "retired runner identity"):
                    runner.settle(wrong)
            recover.assert_not_called()


if __name__ == "__main__":
    unittest.main()
