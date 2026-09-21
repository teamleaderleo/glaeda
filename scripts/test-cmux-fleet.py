#!/usr/bin/env python3
from __future__ import annotations

import copy
import importlib.util
from pathlib import Path
import unittest

MODULE_PATH = Path(__file__).with_name("cmux_fleet.py")
SPEC = importlib.util.spec_from_file_location("cmux_fleet", MODULE_PATH)
assert SPEC and SPEC.loader
f = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(f)

A = "sha256:" + "a" * 64
B = "sha256:" + "b" * 64
C = "sha256:" + "c" * 64
COMMIT = "1" * 40


def enrollment(os_family="macos", state="eligible", roles=None):
    if roles is None:
        roles = (
            ["artifact_cache", "cmux_macos_native_build"]
            if os_family == "macos"
            else ["artifact_cache", "cmux_linux_ci"]
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
        self.assertEqual(by_role["artifact_cache"]["reason"], "acceptance_missing_or_rejected")
        self.assertTrue(status["automaticRoutingEligible"])

    def test_enrollment_presence_does_not_grant_role(self):
        status = f.node_status(enrollment(), [])
        self.assertFalse(status["automaticRoutingEligible"])
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
                self.assertFalse(status["automaticRoutingEligible"])
                self.assertTrue(
                    all(r["reason"] == f"node_{state}" for r in status["roles"])
                )

    def test_stale_acceptance_generation_is_ineligible(self):
        e = enrollment()
        r = f.finalize_acceptance(e, evidence())
        r["enrollmentGeneration"] = 2
        status = f.node_status(e, [r])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertEqual(native["reason"], "acceptance_enrollment_stale")

    def test_private_identity_fields_are_rejected(self):
        for field in ("hostname", "serialNumber", "privateIp", "username"):
            with self.subTest(field=field):
                e = enrollment()
                e[field] = "secret-ish"
                with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
                    f.validate_enrollment(e)

    def test_os_role_mismatch_is_rejected(self):
        with self.assertRaisesRegex(f.FleetError, "incompatible"):
            f.validate_enrollment(
                enrollment("linux", roles=["cmux_macos_native_build"])
            )

    def test_quarantine_and_reenroll_path(self):
        e = enrollment()
        q = f.transition(e, "quarantined", "service_mismatch")
        self.assertEqual(q["quarantineReason"], "service_mismatch")
        enrolling = f.transition(q, "enrolling", None)
        self.assertIsNone(enrolling["quarantineReason"])
        self.assertEqual(enrolling["state"], "enrolling")

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

    def test_fingerprint_is_canonical(self):
        e = enrollment()
        self.assertEqual(f.digest(copy.deepcopy(e)), f.digest(e))
        self.assertLess(len(f.canonical(f.node_status(e, []))), f.MAX_STATUS_BYTES)


if __name__ == "__main__":
    unittest.main()
