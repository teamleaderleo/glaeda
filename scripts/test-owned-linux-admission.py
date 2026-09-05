#!/usr/bin/env python3
"""Admission persistence and actual launch-boundary tests; no systemd or credentials."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import owned_linux_admission as gate
import owned_linux_task as task
import verify_focused_impl as verifier


class AdmissionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.admission = self.root / "admission"
        self.admission.mkdir(mode=0o700)
        self.policy = {"schema_version": 1, "generation": "a" * 64, "revision": 1,
                       "node_control": "available", "memory_reserve_bytes": 4 * 1024**3,
                       "host_executable": {"path": "/host", "sha256": "sha256:" + "b" * 64},
                       "policy_executable": {"path": "/policy", "sha256": "sha256:" + "c" * 64}}
        self.write_policy()
        self.pressure = 0
        self.query_patch = mock.patch.object(gate, "query", side_effect=self.query)
        self.query_mock = self.query_patch.start()
        self.addCleanup(self.query_patch.stop)
        self.fingerprint = "sha256:" + "d" * 64
        self.unit = "glaeda-verify-focused-" + "d" * 32 + ".service"

    def write_policy(self):
        (self.admission / "policy.json").write_bytes(gate.canonical(self.policy))
        (self.admission / "policy.json").chmod(0o600)

    def query(self, entry, arguments, raw=b""):
        if entry["path"] == "/host":
            return {"document_type": "glaeda-linux-host-observation", "schema_version": 3,
                    "authority": "observation_only", "scope": "current_execution_context",
                    "observed_at_unix_millis": time.time_ns() // 1_000_000,
                    "memory": {"available_bytes": 24 * 1024**3}, "cpu": {"logical_cpus": 16},
                    "pressure": {kind: {"avg10_micros": self.pressure}
                                 for kind in ("cpu", "memory", "io")}}
        observation = json.loads(raw)["observation"]
        reason = {"held": "node_held", "draining": "node_draining"}.get(observation["node_control"])
        reason = reason or ("pressure_high" if observation["pressure"] == "high" else "compatible")
        disposition = {"node_held": "refuse", "compatible": "admit_now"}.get(reason, "wait")
        return {"document_type": "glaeda-local-admission-decision", "schema_version": 1,
                "input_sha256": gate.digest(raw), "grants_authority": False, "authorizes_execution": False,
                "decision": {"disposition": disposition,
                             "reason": reason,
                             "active_quiet_lease_generation": None, "requires_new_quiet_lease": False,
                             "requires_yieldable_drain": False, "grants_authority": False,
                             "authorizes_preemption": False, "authorizes_execution": False}}

    def test_observation_has_no_persistent_effects(self):
        def snapshot():
            return {p.name: (p.read_bytes(), p.stat().st_mtime_ns) for p in self.admission.iterdir()}
        before = snapshot()
        answer = gate.observe(self.admission)
        self.assertEqual((answer["outcome"], answer["reason"]), ("ready", "compatible"))
        self.assertFalse(answer["authorizes_execution"])
        self.assertFalse(answer["grants_authority"])
        self.assertFalse(answer["authorizes_redispatch"])
        self.assertEqual(snapshot(), before)
        for state, reason in (("held", "node_held"), ("draining", "node_draining")):
            self.policy["node_control"] = state
            self.write_policy()
            before = snapshot()
            answer = gate.observe(self.admission)
            self.assertEqual((answer["outcome"], answer["reason"]), ("wait", reason))
            self.assertEqual(snapshot(), before)

    def test_observation_defers_pressure_capacity_and_owned_reservations(self):
        self.pressure = 50_000_000
        self.assertEqual(gate.observe(self.admission)["reason"], "pressure_high")
        self.pressure = 0
        self.policy["memory_reserve_bytes"] = 32 * 1024**3
        self.write_policy()
        self.assertEqual(gate.observe(self.admission)["reason"], "capacity_unavailable")
        self.policy["memory_reserve_bytes"] = 4 * 1024**3
        self.write_policy()
        with self.reservation() as reservation:
            raw = (self.admission / "reservation.json").read_bytes()
            self.assertEqual(gate.observe(self.admission)["reason"], "reserved")
            self.assertEqual((self.admission / "reservation.json").read_bytes(), raw)
            reservation.release()

    def test_observation_refuses_malformed_reducer_and_policy_change(self):
        self.query_mock.side_effect = lambda *args: {}
        self.assertEqual(gate.observe(self.admission)["outcome"], "refused")
        def changing(entry, arguments, raw=b""):
            result = self.query(entry, arguments, raw)
            if entry["path"] == "/policy":
                self.policy["revision"] += 1
                self.write_policy()
            return result
        self.query_mock.side_effect = changing
        self.assertEqual(gate.observe(self.admission)["outcome"], "refused")

    def test_wait_observation_revalidates_policy_root_and_freshness(self):
        self.pressure = 50_000_000
        def changing(entry, arguments, raw=b""):
            result = self.query(entry, arguments, raw)
            if entry["path"] == "/policy":
                self.policy["revision"] += 1
                self.write_policy()
            return result
        self.query_mock.side_effect = changing
        self.assertEqual(gate.observe(self.admission)["outcome"], "refused")
        self.query_mock.side_effect = self.query
        with mock.patch.object(gate.time, "monotonic", side_effect=[0, 0, 4]):
            self.assertEqual(gate.observe(self.admission)["outcome"], "refused")
        self.policy["memory_reserve_bytes"] = 32 * 1024**3
        self.write_policy()
        with mock.patch.object(gate.time, "monotonic", side_effect=[0, 0, 4]):
            self.assertEqual(gate.observe(self.admission)["outcome"], "refused")

    def test_wait_observation_rejects_replaced_root(self):
        self.pressure = 50_000_000
        def replaced(entry, arguments, raw=b""):
            result = self.query(entry, arguments, raw)
            if entry["path"] == "/policy":
                self.admission.rename(self.root / "original-admission")
                self.admission.mkdir(mode=0o700)
                self.write_policy()
            return result
        self.query_mock.side_effect = replaced
        self.assertEqual(gate.observe(self.admission)["outcome"], "refused")

    def test_observation_closes_expected_helper_and_decoder_failures(self):
        for error in (subprocess.TimeoutExpired("private command", 3), RecursionError("private data")):
            with self.subTest(error=type(error).__name__):
                self.query_mock.side_effect = error
                answer = gate.observe(self.admission)
                self.assertEqual((answer["outcome"], answer["reason"]),
                                 ("refused", "observation_unavailable"))
                self.assertNotIn(b"private", gate.canonical(answer))

    def test_observation_cli_missing_root_does_not_install(self):
        missing = self.root / "missing"
        completed = subprocess.run([sys.executable, str(Path(__file__).with_name("owned-admission-observe")),
                                    "--root", str(missing)], capture_output=True, check=True)
        self.assertEqual(json.loads(completed.stdout)["reason"], "observation_unavailable")
        self.assertFalse(missing.exists())
        self.assertNotIn(str(missing).encode(), completed.stdout)

    def binding(self):
        command = self.root / "state" / ("d" * 64)
        if command.is_dir():
            request = verifier.normalize_request(self.arguments())
            return verifier.admission_binding(request, command)
        return "sha256:" + "f" * 64

    def reservation(self):
        return gate.Reservation(self.admission, self.fingerprint, self.unit, self.binding())

    def test_serial_slot_and_crash_reservation_are_not_reclaimed(self):
        with self.reservation() as first:
            with self.assertRaisesRegex(task.Refusal, "busy"):
                with self.reservation():
                    self.fail("second reservation entered")
            with first.launch():
                pass  # Controller loss after launch attempt, before settlement.
        with self.assertRaisesRegex(task.Refusal, "exact recovery"):
            with self.reservation():
                self.fail("crash reservation was reclaimed")
        settled = mock.Mock()
        with self.assertRaisesRegex(task.Refusal, "exact terminal"):
            gate.recover(self.admission, "sha256:" + "e" * 64, self.unit, self.binding(), settled)
        settled.assert_not_called()
        gate.recover(self.admission, self.fingerprint, self.unit, self.binding(), settled)
        settled.assert_called_once_with()
        self.assertFalse((self.admission / "reservation.json").exists())

    def test_recovery_refuses_another_source_or_state_binding(self):
        with self.reservation() as first:
            with first.launch():
                pass
        before = (self.admission / "reservation.json").read_bytes()
        settled = mock.Mock()
        with self.assertRaisesRegex(task.Refusal, "exact terminal"):
            gate.recover(self.admission, self.fingerprint, self.unit,
                         "sha256:" + "0" * 64, settled)
        settled.assert_not_called()
        self.assertEqual((self.admission / "reservation.json").read_bytes(), before)

    def test_recovery_does_not_release_unsettled_unit(self):
        with self.reservation() as first:
            with first.launch():
                pass
        with self.assertRaisesRegex(task.Refusal, "still active"):
            gate.recover(self.admission, self.fingerprint, self.unit, self.binding(),
                         mock.Mock(side_effect=task.Refusal("still active")))
        self.assertTrue((self.admission / "reservation.json").exists())

    def test_control_can_change_while_reserved_but_not_during_spawn(self):
        with self.reservation() as first:
            gate.set_control(self.admission, "draining")
            with self.assertRaises(task.Refusal):
                with first.launch():
                    self.fail("drain admitted")
            gate.set_control(self.admission, "available")
            with first.launch():
                with self.assertRaisesRegex(task.Refusal, "busy"):
                    gate.set_control(self.admission, "held")
            gate.set_control(self.admission, "held")
            first.release()
        self.assertEqual(json.loads((self.admission / "policy.json").read_bytes())["node_control"], "held")

    def arguments(self):
        for name in ("repository", "cargo", "rustup"):
            (self.root / name).mkdir(exist_ok=True)
        return verifier.parser().parse_args([
            "run", "--repository-root", str(self.root / "repository"),
            "--state-root", str(self.root / "state"), "--cargo-root", str(self.root / "cargo"),
            "--rustup-root", str(self.root / "rustup"), "--repository", "teamleaderleo/glaeda",
            "--commit", "a" * 40, "--tree", "b" * 40,
            "--profile-generation", verifier.profile_generation(),
            "--command-fingerprint", self.fingerprint, "--admission-root", str(self.admission)])

    def run_verifier(self, arguments, materialize=None):
        marker = self.root / "child-started"
        def make_source(*unused):
            source = self.root / "source"
            source.mkdir(exist_ok=True)
            if materialize:
                materialize()
            return source
        with (mock.patch.object(verifier, "verify_resident_source"),
              mock.patch.object(verifier, "materialize", side_effect=make_source),
              mock.patch.object(verifier, "sandbox_command", return_value=[
                  sys.executable, "-c", f"from pathlib import Path; Path({str(marker)!r}).write_text('started')"]),
              mock.patch.object(task, "unit_absent", return_value=True),
              mock.patch.object(verifier, "emit")):
            return verifier.run(arguments)

    def test_fresh_hold_drain_pressure_refuse_actual_popen_and_clean_attempt(self):
        arguments = self.arguments()
        for change in (lambda: gate.set_control(self.admission, "held"),
                       lambda: gate.set_control(self.admission, "draining"),
                       lambda: setattr(self, "pressure", 1_000_000)):
            with self.subTest(change=change):
                gate.set_control(self.admission, "available")
                self.pressure = 0
                with self.assertRaisesRegex(task.Refusal, "interference"):
                    self.run_verifier(arguments, change)
                self.assertFalse((self.root / "child-started").exists())
                command = self.root / "state" / ("d" * 64)
                self.assertFalse((command / "task").exists())
                self.assertFalse((command / "intent.json").exists())
                self.assertFalse((command / "receipt.json").exists())
                self.assertFalse((self.admission / "reservation.json").exists())

    def test_real_child_settlement_releases_slot_and_exact_replay_skips_gate(self):
        arguments = self.arguments()
        self.assertEqual(self.run_verifier(arguments), 0)
        self.assertEqual((self.root / "child-started").read_text(), "started")
        self.assertFalse((self.admission / "reservation.json").exists())
        receipt = self.root / "state" / ("d" * 64) / "receipt.json"
        before = (receipt.read_bytes(), receipt.stat().st_mtime_ns)
        gate.set_control(self.admission, "held")
        with mock.patch.object(gate, "Reservation", side_effect=AssertionError("replay entered gate")):
            self.assertEqual(self.run_verifier(arguments), 0)
        self.assertEqual((receipt.read_bytes(), receipt.stat().st_mtime_ns), before)

    def test_exact_terminal_recovery_releases_only_after_fresh_absence(self):
        arguments = self.arguments()
        self.run_verifier(arguments)
        with self.reservation() as first:
            with first.launch():
                pass
        arguments.reconcile_only = True
        with mock.patch.object(verifier, "unit_absent", return_value=False):
            with self.assertRaisesRegex(task.Refusal, "cleanup"):
                self.run_verifier(arguments)
        self.assertTrue((self.admission / "reservation.json").exists())
        with mock.patch.object(verifier, "unit_absent", return_value=True):
            self.run_verifier(arguments)
        self.assertFalse((self.admission / "reservation.json").exists())

    def test_symlink_fifo_hardlink_and_replaced_root_refuse(self):
        for kind in ("symlink", "fifo", "hardlink"):
            path = self.admission / "reservation.json"
            if kind == "symlink":
                path.symlink_to(self.admission / "policy.json")
            elif kind == "fifo":
                os.mkfifo(path, 0o600)
            else:
                os.link(self.admission / "policy.json", path)
            with self.assertRaises((task.Refusal, OSError)):
                with self.reservation():
                    self.fail("unsafe reservation entered")
            path.unlink()
        store = gate.Store(self.admission)
        try:
            self.admission.rename(self.root / "old-admission")
            self.admission.mkdir(mode=0o700)
            with self.assertRaisesRegex(task.Refusal, "root changed"):
                store.read("policy.json")
        finally:
            store.close()

    def test_policy_duplicate_keys_bool_revision_and_wrong_digest_refuse(self):
        with self.assertRaises(task.Refusal):
            gate.decode(b'{"a":1,"a":2}')
        self.policy["revision"] = True
        self.write_policy()
        with self.assertRaisesRegex(task.Refusal, "policy"):
            with self.reservation():
                self.fail("boolean revision accepted")
        self.policy["revision"] = 1
        self.write_policy()
        original = self.query
        def wrong_digest(entry, args, raw=b""):
            result = original(entry, args, raw)
            if raw:
                result["input_sha256"] = "sha256:" + "0" * 64
            return result
        self.query_mock.side_effect = wrong_digest
        with self.assertRaisesRegex(task.Refusal, "interference"):
            with self.reservation():
                self.fail("unbound response admitted")

    def test_stale_missing_capacity_and_expired_observations_refuse(self):
        original = self.query
        for mutate in (lambda host: host["memory"].update(available_bytes=1),
                       lambda host: host.update(observed_at_unix_millis=1),
                       lambda host: host.pop("pressure")):
            def invalid(entry, arguments, raw=b""):
                answer = original(entry, arguments, raw)
                if not raw:
                    mutate(answer)
                return answer
            self.query_mock.side_effect = invalid
            with self.assertRaises(task.Refusal):
                with self.reservation():
                    self.fail("invalid observation admitted")
            self.assertFalse((self.admission / "reservation.json").exists())
        self.query_mock.side_effect = original
        with mock.patch.object(gate.time, "monotonic", side_effect=[0, 4]):
            with self.assertRaisesRegex(task.Refusal, "expired"):
                gate.check(self.policy)

    def test_held_lock_substitution_refuses_launch(self):
        with self.reservation() as first:
            path = self.admission / "slot.lock"
            path.unlink()
            path.touch(mode=0o600)
            with self.assertRaisesRegex(task.Refusal, "lock changed"):
                with first.launch():
                    self.fail("substituted lock admitted")
            self.assertFalse(first.launch_attempted)
        self.assertTrue((self.admission / "reservation.json").exists())

    def test_observer_timeout_settles_its_test_child(self):
        self.query_patch.stop()
        executable = self.root / "observer"
        executable.write_text("#!/usr/bin/python3\nimport time\ntime.sleep(5)\n")
        executable.chmod(0o700)
        entry = {"path": str(executable), "sha256": gate.digest(executable.read_bytes())}
        with mock.patch.object(gate, "FRESH_SECONDS", 0.05):
            with self.assertRaisesRegex(task.Refusal, "timed out"):
                gate.query(entry, [])

    def test_observer_output_is_bounded_and_executable_identity_checked(self):
        self.query_patch.stop()
        executable = self.root / "observer"
        executable.write_text("#!/usr/bin/python3\nprint('x' * 40000)\n")
        executable.chmod(0o700)
        entry = {"path": str(executable), "sha256": gate.digest(executable.read_bytes())}
        with self.assertRaisesRegex(task.Refusal, "output exceeds"):
            gate.query(entry, [])
        entry["sha256"] = "sha256:" + "0" * 64
        with self.assertRaisesRegex(task.Refusal, "executable changed"):
            gate.query(entry, [])


if __name__ == "__main__":
    unittest.main()
