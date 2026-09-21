#!/usr/bin/env python3
from __future__ import annotations

import copy
import json
import fcntl
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]

MODULE_PATH = Path(__file__).with_name("cmux_fleet.py")
SPEC = importlib.util.spec_from_file_location("cmux_fleet", MODULE_PATH)
assert SPEC and SPEC.loader
f = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(f)

A = "sha256:" + "a" * 64
B = "sha256:" + "b" * 64
C = "sha256:" + "c" * 64
D = "sha256:" + "d" * 64
COMMIT = "1" * 40


def enrollment(os_family="macos", state="eligible", roles=None):
    if roles is None:
        roles = (
            ["cmux_macos_native_build"]
            if os_family == "macos"
            else ["cmux_linux_ci"]
        )
    return {
        "schema": f.ENROLLMENT_SCHEMA,
        "nodeId": "cmux-fixture-001",
        "architecture": "arm64" if os_family == "macos" else "x86_64",
        "os": {
            "family": os_family,
            "versionClass": "macos-26" if os_family == "macos" else "ubuntu-24.04",
        },
        "hardwareCapabilityClass": (
            "cmux-mac-build-large" if os_family == "macos" else "cmux-linux-ci-medium"
        ),
        "supportedToolchainGenerations": [A, B],
        "roleWorkloadGenerations": {role: B for role in sorted(roles)},
        "allowedExecutionRoles": sorted(roles),
        "operatorFleetScope": "cmux-founders",
        "enrollmentGeneration": 3,
        "glaedaGeneration": C,
        "state": state,
        "quarantineReason": None,
    }


def evidence(role="cmux_macos_native_build", toolchain=A, checks=None):
    return {
        "schema": f.ACCEPTANCE_EVIDENCE_SCHEMA,
        "nodeId": "cmux-fixture-001",
        "enrollmentGeneration": 3,
        "role": role,
        "source": {"repository": "manaflow-ai/cmux", "commit": COMMIT},
        "toolchainGeneration": toolchain,
        "glaedaGeneration": C,
        "workloadGeneration": B,
        "checks": checks or {
            "workload": "pass",
            "semanticVerifier": "pass",
            "artifact": "pass",
            "processSettlement": "pass",
        },
    }


class FleetTests(unittest.TestCase):
    def test_current_accepted_role_is_eligible(self):
        e = enrollment()
        r = f.finalize_acceptance(e, evidence())
        status = f.node_status(e, [r])
        by_role = {v["role"]: v for v in status["roles"]}
        self.assertTrue(by_role["cmux_macos_native_build"]["eligible"])
        self.assertEqual(set(by_role), {"cmux_macos_native_build"})
        self.assertTrue(status["routingCandidateEligible"])
        self.assertFalse(status["automaticDispatchAuthorized"])

    def test_enrollment_presence_does_not_grant_role(self):
        status = f.node_status(enrollment(), [])
        self.assertFalse(status["routingCandidateEligible"])
        self.assertTrue(all(not r["eligible"] for r in status["roles"]))

    def test_non_routable_states_exclude_every_role(self):
        for state in ("draining", "quarantined", "retired"):
            with self.subTest(state=state):
                e = enrollment(state=state)
                if state == "quarantined":
                    e["quarantineReason"] = "disk_pressure"
                receipt = f.finalize_acceptance(
                    {**e, "state": "eligible", "quarantineReason": None},
                    evidence(),
                )
                status = f.node_status(e, [receipt])
                self.assertFalse(status["routingCandidateEligible"])
                self.assertTrue(
                    all(r["reason"] == f"node_{state}" for r in status["roles"])
                )

    def test_null_glaeda_generation_is_rejected(self):
        e = enrollment()
        e["glaedaGeneration"] = None
        with self.assertRaisesRegex(f.FleetError, "Glaeda generation is invalid"):
            f.validate_enrollment(e)

        bad_evidence = evidence()
        bad_evidence["glaedaGeneration"] = None
        with self.assertRaisesRegex(f.FleetError, "acceptance Glaeda generation is invalid"):
            f.validate_acceptance_evidence(bad_evidence)

        receipt = f.finalize_acceptance(enrollment(), evidence())
        receipt["glaedaGeneration"] = None
        with self.assertRaisesRegex(
            f.FleetError, "acceptance receipt Glaeda generation is invalid"
        ):
            f.validate_acceptance_receipt(receipt)

    def test_stale_acceptance_generation_is_ineligible(self):
        e = enrollment()
        r = f.finalize_acceptance(e, evidence())
        r["enrollmentGeneration"] = 2
        status = f.node_status(e, [r])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertEqual(native["reason"], "acceptance_enrollment_stale")

    def test_stale_acceptance_workload_generation_is_ineligible(self):
        e = enrollment()
        r = f.finalize_acceptance(e, evidence())
        e["roleWorkloadGenerations"]["cmux_macos_native_build"] = D
        status = f.node_status(e, [r])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertFalse(native["eligible"])
        self.assertEqual(native["reason"], "acceptance_workload_stale")

    def test_private_identity_fields_are_rejected(self):
        for field in ("hostname", "serialNumber", "privateIp", "username"):
            with self.subTest(field=field):
                e = enrollment()
                e[field] = "secret-ish"
                with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
                    f.validate_enrollment(e)

    def test_role_without_reviewed_workload_is_rejected(self):
        with self.assertRaisesRegex(f.FleetError, "reviewed v1 acceptance workload"):
            f.validate_enrollment(
                enrollment(roles=["artifact_cache"])
            )

    def test_os_role_mismatch_is_rejected(self):
        with self.assertRaisesRegex(f.FleetError, "incompatible"):
            f.validate_enrollment(
                enrollment("linux", roles=["cmux_macos_native_build"])
            )

    def test_eligible_transition_requires_current_acceptance(self):
        e = enrollment(state="enrolling")
        with self.assertRaisesRegex(f.FleetError, "accepted role receipt"):
            f.transition(e, "eligible", None)
        receipt = f.finalize_acceptance(e, evidence())
        eligible = f.transition(e, "eligible", None, [receipt])
        self.assertEqual(eligible["state"], "eligible")

    def test_quarantine_and_reenroll_path(self):
        e = enrollment()
        q = f.transition(e, "quarantined", "service_mismatch")
        self.assertEqual(q["quarantineReason"], "service_mismatch")
        enrolling = f.transition(q, "enrolling", None)
        self.assertIsNone(enrolling["quarantineReason"])
        self.assertEqual(enrolling["state"], "enrolling")
        self.assertEqual(
            enrolling["enrollmentGeneration"],
            e["enrollmentGeneration"] + 1,
        )

    def test_acceptance_binds_current_identity(self):
        e = enrollment()
        bad = evidence(toolchain="sha256:" + "d" * 64)
        with self.assertRaisesRegex(f.FleetError, "toolchain generation"):
            f.finalize_acceptance(e, bad)
        bad = evidence()
        bad["glaedaGeneration"] = "sha256:" + "d" * 64
        with self.assertRaisesRegex(f.FleetError, "Glaeda generation"):
            f.finalize_acceptance(e, bad)
        bad = evidence()
        bad["enrollmentGeneration"] = 4
        with self.assertRaisesRegex(f.FleetError, "enrollment generation"):
            f.finalize_acceptance(e, bad)
        bad = evidence()
        bad["workloadGeneration"] = D
        with self.assertRaisesRegex(f.FleetError, "workload generation"):
            f.finalize_acceptance(e, bad)

    def test_failed_settlement_rejects_role(self):
        checks = {
            "workload": "pass",
            "semanticVerifier": "pass",
            "artifact": "pass",
            "processSettlement": "fail",
        }
        e = enrollment()
        r = f.finalize_acceptance(e, evidence(checks=checks))
        self.assertEqual(r["result"], "rejected")
        status = f.node_status(e, [r])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertFalse(native["eligible"])

    def test_linux_fixture_can_be_eligible(self):
        e = enrollment("linux")
        r = f.finalize_acceptance(e, evidence(role="cmux_linux_ci"))
        status = f.node_status(e, [r])
        linux = next(v for v in status["roles"] if v["role"] == "cmux_linux_ci")
        self.assertTrue(linux["eligible"])

    def test_status_rejects_forged_acceptance_receipt(self):
        e = enrollment()
        forged = f.finalize_acceptance(e, evidence())
        forged["checks"]["processSettlement"] = "fail"
        with self.assertRaisesRegex(f.FleetError, "disagrees"):
            f.node_status(e, [forged])

    def test_status_rejects_unknown_acceptance_fields(self):
        e = enrollment()
        receipt = f.finalize_acceptance(e, evidence())
        receipt["hostname"] = "hidden-host"
        with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
            f.node_status(e, [receipt])

    def test_enrollment_is_derived_from_accepted_bootstrap(self):
        bootstrap = {
            "schema": f.BOOTSTRAP_SCHEMA,
            "platform": "macos",
            "architecture": "arm64",
            "osVersionClass": "macos-26",
            "hardwareCapabilityClass": "cmux-mac-build-large",
            "roles": ["cmux_macos_native_build"],
            "glaedaGeneration": C,
            "toolchainGeneration": A,
            "roleWorkloadGenerations": {"cmux_macos_native_build": B},
            "checks": {"ready": True},
            "observed": {},
            "eligibleForEnrollment": True,
            "blockingChecks": [],
            "authority": "observation_only",
        }
        result = f.enrollment_from_bootstrap(
            bootstrap,
            node_id="cmux-fixture-002",
            operator_fleet_scope="cmux-founders",
            enrollment_generation=1,
        )
        self.assertEqual(result["state"], "enrolling")
        self.assertEqual(result["supportedToolchainGenerations"], [A])
        self.assertEqual(
            result["roleWorkloadGenerations"],
            {"cmux_macos_native_build": B},
        )

    def test_blocked_bootstrap_cannot_enroll(self):
        bootstrap = {
            "schema": f.BOOTSTRAP_SCHEMA,
            "authority": "observation_only",
            "eligibleForEnrollment": False,
        }
        with self.assertRaisesRegex(f.FleetError, "blocking checks"):
            f.enrollment_from_bootstrap(
                bootstrap,
                node_id="cmux-fixture-002",
                operator_fleet_scope="cmux-founders",
                enrollment_generation=1,
            )

    def test_versioned_schema_and_examples_parse(self):
        schema = json.loads(
            (ROOT / "examples/cmux-fleet/enrollment-v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertFalse(schema["additionalProperties"])
        for name in (
            "macos-enrollment.example.json",
            "linux-enrollment.example.json",
        ):
            document = json.loads(
                (ROOT / "examples/cmux-fleet" / name).read_text(
                    encoding="utf-8"
                )
            )
            self.assertEqual(f.validate_enrollment(document), document)

    def test_load_requires_private_owned_regular_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            document = root / "enrollment.json"
            document.write_bytes(f.canonical(enrollment()))
            document.chmod(0o600)
            self.assertEqual(f.load(document), enrollment())

            document.chmod(0o644)
            with self.assertRaisesRegex(f.FleetError, "ownership or mode"):
                f.load(document)
            document.chmod(0o600)

            alias = root / "alias.json"
            alias.symlink_to(document)
            with self.assertRaisesRegex(f.FleetError, "unavailable"):
                f.load(alias)

    def test_load_rejects_file_identity_change_during_read(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "enrollment.json"
            path.write_bytes(f.canonical(enrollment()))
            path.chmod(0o600)
            original_fstat = os.fstat
            calls = 0

            def changed_fstat(fd):
                nonlocal calls
                calls += 1
                value = original_fstat(fd)
                if calls == 2:
                    values = list(value)
                    values[8] += 1
                    return os.stat_result(values)
                return value

            with mock.patch.object(f.os, "fstat", side_effect=changed_fstat):
                with self.assertRaisesRegex(f.FleetError, "changed while reading"):
                    f.load(path)

    def test_transition_apply_publishes_private_durable_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            enrollment_path = root / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(enrollment()))
            enrollment_path.chmod(0o600)

            updated = f.apply_transition(
                enrollment_path,
                "draining",
                None,
                [],
            )

            self.assertEqual(updated["state"], "draining")
            self.assertEqual(f.load(enrollment_path), updated)
            self.assertEqual(enrollment_path.stat().st_mode & 0o777, 0o600)
            lock_path = root / ".mutation.lock"
            self.assertTrue(lock_path.is_file())
            self.assertEqual(lock_path.stat().st_mode & 0o777, 0o600)
            self.assertFalse(list(root.glob(".enrollment.next.*")))

    def test_transition_apply_refuses_concurrent_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            enrollment_path = root / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(enrollment()))
            enrollment_path.chmod(0o600)

            with f.FleetMutationLock(enrollment_path):
                with self.assertRaisesRegex(f.FleetError, "mutation is busy"):
                    f.apply_transition(
                        enrollment_path,
                        "draining",
                        None,
                        [],
                    )

            self.assertEqual(f.load(enrollment_path)["state"], "eligible")

    def test_transition_apply_refuses_parent_directory_rebind(self):
        with tempfile.TemporaryDirectory() as temporary:
            outer = Path(temporary).resolve()
            state = outer / "state"
            state.mkdir(mode=0o700)
            enrollment_path = state / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(enrollment()))
            enrollment_path.chmod(0o600)
            detached = outer / "detached"

            original_transition = f.transition

            def rebind(current, target, reason, acceptances):
                result = original_transition(current, target, reason, acceptances)
                state.rename(detached)
                state.mkdir(mode=0o700)
                decoy = state / "enrollment.json"
                decoy.write_bytes(f.canonical(enrollment()))
                decoy.chmod(0o600)
                return result

            with mock.patch.object(f, "transition", side_effect=rebind):
                with self.assertRaisesRegex(
                    f.FleetError,
                    "directory identity changed",
                ):
                    f.apply_transition(
                        enrollment_path,
                        "draining",
                        None,
                        [],
                    )

            self.assertEqual(
                f.load(detached / "enrollment.json")["state"],
                "eligible",
            )
            self.assertEqual(
                f.load(state / "enrollment.json")["state"],
                "eligible",
            )

    def test_fingerprint_is_canonical(self):
        e = enrollment()
        self.assertEqual(f.digest(copy.deepcopy(e)), f.digest(e))
        self.assertLess(len(f.canonical(f.node_status(e, []))), f.MAX_STATUS_BYTES)


if __name__ == "__main__":
    unittest.main()
