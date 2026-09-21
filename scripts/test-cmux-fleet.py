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
E = "sha256:" + "e" * 64
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
        "roleProfiles": {
            role: dict(
                f.ROLE_PROFILES.get(
                    role,
                    {"id": "reserved.placeholder", "generation": 1},
                )
            )
            for role in sorted(roles)
        },
        "allowedExecutionRoles": sorted(roles),
        "operatorFleetScope": "cmux-founders",
        "enrollmentGeneration": 3,
        "glaedaGeneration": C,
        "state": state,
        "quarantineReason": None,
    }


def bootstrap_for(enrollment_value, toolchain=A):
    return {
        "schema": f.BOOTSTRAP_SCHEMA,
        "platform": enrollment_value["os"]["family"],
        "architecture": enrollment_value["architecture"],
        "osVersionClass": enrollment_value["os"]["versionClass"],
        "hardwareCapabilityClass": enrollment_value["hardwareCapabilityClass"],
        "roles": list(enrollment_value["allowedExecutionRoles"]),
        "glaedaGeneration": enrollment_value["glaedaGeneration"],
        "toolchainGeneration": toolchain,
        "roleProfiles": copy.deepcopy(enrollment_value["roleProfiles"]),
        "checks": {"ready": True},
        "observed": {},
        "eligibleForEnrollment": True,
        "blockingChecks": [],
        "authority": "observation_only",
    }


def cmux_result(
    role="cmux_macos_native_build",
    *,
    state="passed",
    cleanup_state=None,
    process_group_settled=None,
    profile=None,
):
    chosen_profile = dict(profile or f.ROLE_PROFILES[role])
    if cleanup_state is None:
        cleanup_state = "forced" if state == "ambiguous" else "complete"
    if process_group_settled is None:
        process_group_settled = state != "ambiguous"
    exit_code = {
        "passed": 0,
        "failed": 1,
        "timed_out": 124,
        "ambiguous": 0,
    }[state]
    observations = {"fixture": "cmux-test"}
    result = {
        "document_type": f.CMUX_RESULT_DOCUMENT_TYPE,
        "schema_version": f.CMUX_RESULT_SCHEMA_VERSION,
        "source": {
            "repository": f.CMUX_REPOSITORY,
            "commit": COMMIT,
            "tree": "2" * 40,
        },
        "profile": chosen_profile,
        "semantic_validator": "cmux.fixture/v1",
        "environment_class": (
            "isolated-build"
            if role == "cmux_macos_native_build"
            else "isolated-portable"
        ),
        "expected_result_class": "cmux.fixture-result/v1",
        "result": state,
        "parameters": {},
        "runtime_input_identities": [],
        "artifact_identities": [],
        "validation": {
            "missing_required_artifact_classes": [],
        },
        "stage_timings": [{"stage": "test", "seconds": 1.25}],
        "resource_summary": {
            "resource_class": "cmux-fixture",
            "cpu_count": 4,
            "memory_bytes": 16 * 1024**3,
            "architecture": "arm64" if role == "cmux_macos_native_build" else "x86_64",
        },
        "toolchain": {
            "identity": f.cmux_digest(observations),
            "observations": observations,
        },
        "benchmark": {
            "state_class": "cold",
            "semantic_comparison_key": A,
            "comparison_context_key": B,
        },
        "network_class": "none",
        "timeout_class": "fixture",
        "cleanup": {
            "state": cleanup_state,
            "process_group_settled": process_group_settled,
        },
        "exit_code": exit_code,
        "started_at_unix_millis": 1000,
        "ended_at_unix_millis": 2250,
    }
    semantic = f._cmux_semantic_key(result)
    result["benchmark"]["semantic_comparison_key"] = semantic
    result["benchmark"]["comparison_context_key"] = f._cmux_context_key(
        semantic,
        "cold",
        result["toolchain"]["identity"],
    )
    return result


def finalized(
    enrollment_value,
    role=None,
    *,
    result=None,
    toolchain=A,
    post_bootstrap=None,
):
    role = role or enrollment_value["allowedExecutionRoles"][0]
    return f.finalize_acceptance(
        enrollment_value,
        role,
        toolchain,
        result or cmux_result(role),
        post_bootstrap or bootstrap_for(enrollment_value, toolchain),
        execution_class=f.LOCAL_EXECUTION_CLASS,
        local_execution_attempt_sha256=E,
    )


class FleetTests(unittest.TestCase):
    def test_current_accepted_role_is_eligible(self):
        e = enrollment()
        r = finalized(e)
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
                receipt = finalized(
                    {**e, "state": "eligible", "quarantineReason": None}
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

        receipt = finalized(enrollment())
        receipt["glaedaGeneration"] = None
        with self.assertRaisesRegex(
            f.FleetError, "acceptance receipt Glaeda generation is invalid"
        ):
            f.validate_acceptance_receipt(receipt)

    def test_stale_acceptance_generation_is_ineligible(self):
        e = enrollment()
        r = finalized(e)
        r["enrollmentGeneration"] = 2
        status = f.node_status(e, [r])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertEqual(native["reason"], "acceptance_enrollment_stale")

    def test_profile_policy_change_requires_fresh_enrollment(self):
        e = enrollment()
        receipt = finalized(e)
        with mock.patch.dict(
            f.ROLE_PROFILES,
            {
                "cmux_macos_native_build": {
                    "id": "cmux.macos.dev-check",
                    "generation": 2,
                },
                "cmux_linux_ci": dict(f.ROLE_PROFILES["cmux_linux_ci"]),
            },
            clear=True,
        ):
            with self.assertRaisesRegex(f.FleetError, "not the reviewed v1 profile"):
                f.node_status(e, [receipt])

    def test_private_identity_fields_are_rejected(self):
        for field in ("hostname", "serialNumber", "privateIp", "username"):
            with self.subTest(field=field):
                e = enrollment()
                e[field] = "secret-ish"
                with self.assertRaisesRegex(f.FleetError, "unknown or missing"):
                    f.validate_enrollment(e)

    def test_role_without_reviewed_workload_is_rejected(self):
        with self.assertRaisesRegex(f.FleetError, "reviewed v1 profile"):
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
        receipt = finalized(e)
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
        with self.assertRaisesRegex(f.FleetError, "toolchain generation"):
            f.finalize_acceptance(
                e,
                "cmux_macos_native_build",
                D,
                cmux_result(),
                bootstrap_for(e, D),
            )

        wrong_profile = {
            "id": "cmux.macos.dev-check",
            "generation": 2,
        }
        with self.assertRaisesRegex(f.FleetError, "differs from enrolled role profile"):
            finalized(e, result=cmux_result(profile=wrong_profile))

    def test_acceptance_rejects_detached_semantic_result_digest(self):
        e = enrollment()
        with self.assertRaisesRegex(
            f.FleetError, "semantic result digest does not match"
        ):
            f.finalize_acceptance(
                e,
                "cmux_macos_native_build",
                A,
                cmux_result(),
                bootstrap_for(e),
                D,
                execution_class=f.LOCAL_EXECUTION_CLASS,
                local_execution_attempt_sha256=E,
            )

    def test_acceptance_requires_fresh_matching_post_bootstrap(self):
        e = enrollment()

        changed_toolchain = bootstrap_for(e, B)
        with self.assertRaisesRegex(
            f.FleetError,
            "post-acceptance bootstrap toolchain differs",
        ):
            finalized(e, post_bootstrap=changed_toolchain)

        changed_glaeda = bootstrap_for(e)
        changed_glaeda["glaedaGeneration"] = D
        with self.assertRaisesRegex(
            f.FleetError,
            "post-acceptance bootstrap differs",
        ):
            finalized(e, post_bootstrap=changed_glaeda)

        changed_hardware = bootstrap_for(e)
        changed_hardware["hardwareCapabilityClass"] = "cmux-mac-other"
        with self.assertRaisesRegex(
            f.FleetError,
            "post-acceptance bootstrap differs",
        ):
            finalized(e, post_bootstrap=changed_hardware)

    def test_acceptance_receipt_binds_post_bootstrap_and_cmux_context(self):
        e = enrollment()
        result = cmux_result()
        receipt = finalized(e, result=result)
        self.assertEqual(
            receipt["postBootstrapSha256"],
            f.digest(bootstrap_for(e)),
        )
        self.assertEqual(
            receipt["cmuxToolchainIdentity"],
            result["toolchain"]["identity"],
        )
        self.assertEqual(
            receipt["cmuxEnvironmentClass"],
            result["environment_class"],
        )

    def test_acceptance_receipt_execution_binding_is_closed(self):
        e = enrollment()
        receipt = finalized(e)

        missing_attempt = copy.deepcopy(receipt)
        missing_attempt["localExecutionAttemptSha256"] = None
        with self.assertRaisesRegex(
            f.FleetError,
            "local execution evidence is inconsistent",
        ):
            f.validate_acceptance_receipt(missing_attempt)

        external_with_attempt = copy.deepcopy(receipt)
        external_with_attempt["executionClass"] = f.EXTERNAL_EVIDENCE_CLASS
        with self.assertRaisesRegex(
            f.FleetError,
            "local execution evidence is inconsistent",
        ):
            f.validate_acceptance_receipt(external_with_attempt)

    def test_external_semantic_evidence_cannot_mint_accepted_receipt(self):
        e = enrollment()
        receipt = f.finalize_acceptance(
            e,
            "cmux_macos_native_build",
            A,
            cmux_result(),
            bootstrap_for(e),
        )
        self.assertEqual(receipt["executionClass"], f.EXTERNAL_EVIDENCE_CLASS)
        self.assertIsNone(receipt["localExecutionAttemptSha256"])
        self.assertEqual(receipt["result"], "rejected")
        self.assertFalse(f.node_status(e, [receipt])["routingCandidateEligible"])

    def test_accept_local_binds_profile_run_to_this_node(self):
        e = enrollment("linux", state="enrolling")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            enrollment_path = root / "enrollment.json"
            enrollment_path.write_bytes(f.canonical(e))
            enrollment_path.chmod(0o600)

            cmux_root = root / "cmux"
            runner = cmux_root / "scripts/ci/cmux_workload_profile.py"
            runner.parent.mkdir(parents=True)
            runner.write_text("# fixture\n", encoding="utf-8")

            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)

            result = cmux_result("cmux_linux_ci")
            post = bootstrap_for(e)

            def fake_run(argv, **kwargs):
                if "cmux_workload_profile.py" in str(argv[1]):
                    self.assertIn("--state-class", argv)
                    self.assertEqual(argv[argv.index("--state-class") + 1], "cold")
                    self.assertNotIn("--state-root", argv)
                    result_path = Path(argv[argv.index("--result") + 1])
                    result_path.write_bytes(f.canonical(result))
                    result_path.chmod(0o600)
                    return __import__("subprocess").CompletedProcess(argv, 0)
                if "cmux_fleet_bootstrap.py" in str(argv[1]):
                    return __import__("subprocess").CompletedProcess(
                        argv,
                        0,
                        stdout=f.canonical(post),
                        stderr=b"",
                    )
                raise AssertionError(argv)

            with (
                mock.patch.object(
                    f,
                    "_git_oid",
                    side_effect=[COMMIT, "2" * 40],
                ),
                mock.patch.object(f.subprocess, "run", side_effect=fake_run),
            ):
                receipt = f.accept_local(
                    enrollment_path,
                    cmux_root,
                    glaeda,
                    "cmux_linux_ci",
                )

        self.assertEqual(receipt["result"], "accepted")
        self.assertEqual(receipt["executionClass"], f.LOCAL_EXECUTION_CLASS)
        self.assertRegex(
            receipt["localExecutionAttemptSha256"],
            r"^sha256:[0-9a-f]{64}$",
        )
        self.assertEqual(
            receipt["cmuxSemanticResultSha256"],
            f.digest(result),
        )

    def test_failed_settlement_rejects_role(self):
        e = enrollment()
        receipt = finalized(
            e,
            result=cmux_result(state="ambiguous"),
        )
        self.assertEqual(receipt["result"], "rejected")
        self.assertEqual(receipt["processSettlement"], "incomplete")
        status = f.node_status(e, [receipt])
        native = next(
            v for v in status["roles"] if v["role"] == "cmux_macos_native_build"
        )
        self.assertFalse(native["eligible"])

    def test_cmux_semantic_failure_never_accepts_role(self):
        for state in ("failed", "timed_out", "ambiguous"):
            with self.subTest(state=state):
                e = enrollment()
                receipt = finalized(e, result=cmux_result(state=state))
                self.assertEqual(receipt["result"], "rejected")
                self.assertEqual(receipt["cmuxSemanticResultState"], state)
                self.assertFalse(f.node_status(e, [receipt])["routingCandidateEligible"])

    def test_timed_out_result_preserves_actual_child_exit_code(self):
        e = enrollment()
        result = cmux_result(state="timed_out")
        result["exit_code"] = -15
        receipt = finalized(e, result=result)
        self.assertEqual(receipt["result"], "rejected")
        self.assertEqual(receipt["cmuxSemanticResultState"], "timed_out")

    def test_cmux_semantic_result_requires_positive_cpu_count(self):
        e = enrollment()
        result = cmux_result()
        result["resource_summary"]["cpu_count"] = None
        with self.assertRaisesRegex(f.FleetError, "resource summary"):
            finalized(e, result=result)

    def test_cmux_semantic_result_rejects_inconsistent_evidence(self):
        e = enrollment()
        cases = []

        extra = cmux_result()
        extra["unexpected"] = True
        cases.append(("unknown or missing", extra))

        toolchain = cmux_result()
        toolchain["toolchain"]["observations"]["fixture"] = "changed"
        cases.append(("toolchain identity", toolchain))

        semantic = cmux_result()
        semantic["benchmark"]["semantic_comparison_key"] = D
        cases.append(("semantic comparison", semantic))

        environment = cmux_result()
        environment["environment_class"] = "isolated-portable"
        cases.append(("semantic comparison", environment))

        passed_forced = cmux_result()
        passed_forced["cleanup"] = {
            "state": "forced",
            "process_group_settled": False,
        }
        cases.append(("terminal CMUX result", passed_forced))

        missing = cmux_result()
        missing["validation"]["missing_required_artifact_classes"] = [
            "cmux.required/v1"
        ]
        cases.append(("passed CMUX result", missing))

        for message, result in cases:
            with self.subTest(message=message):
                with self.assertRaisesRegex(f.FleetError, message):
                    finalized(e, result=result)

    def test_linux_fixture_can_be_eligible(self):
        e = enrollment("linux")
        r = finalized(e, "cmux_linux_ci")
        status = f.node_status(e, [r])
        linux = next(v for v in status["roles"] if v["role"] == "cmux_linux_ci")
        self.assertTrue(linux["eligible"])

    def test_status_rejects_forged_acceptance_receipt(self):
        e = enrollment()
        forged = finalized(e)
        forged["cmuxSemanticResultState"] = "failed"
        with self.assertRaisesRegex(f.FleetError, "disagrees"):
            f.node_status(e, [forged])

    def test_status_rejects_unknown_acceptance_fields(self):
        e = enrollment()
        receipt = finalized(e)
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
            "roleProfiles": {"cmux_macos_native_build": dict(f.ROLE_PROFILES["cmux_macos_native_build"])},
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
            result["roleProfiles"],
            {"cmux_macos_native_build": dict(f.ROLE_PROFILES["cmux_macos_native_build"])},
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

    def test_cmux_semantic_result_loader_requires_canonical_exact_bytes(self):
        result = cmux_result()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / "cmux-result.json"
            raw = f.canonical(result)
            path.write_bytes(raw)
            path.chmod(0o600)
            loaded, exact_digest = f.load_cmux_semantic_result(path)
            self.assertEqual(loaded, result)
            self.assertEqual(
                exact_digest,
                "sha256:" + __import__("hashlib").sha256(raw).hexdigest(),
            )
            e = enrollment()
            receipt = f.finalize_acceptance(
                e,
                "cmux_macos_native_build",
                A,
                loaded,
                bootstrap_for(e),
                exact_digest,
                execution_class=f.LOCAL_EXECUTION_CLASS,
                local_execution_attempt_sha256=E,
            )
            self.assertEqual(receipt["cmuxSemanticResultSha256"], exact_digest)

            path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(f.FleetError, "not canonical"):
                f.load_cmux_semantic_result(path)

    def test_cmux_semantic_result_loader_requires_private_mode(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "result.json"
            path.write_bytes(f.canonical(cmux_result()))
            path.chmod(0o644)
            with self.assertRaisesRegex(f.FleetError, "unsafe"):
                f.load_cmux_semantic_result(path)

    def test_cmux_semantic_result_loader_refuses_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "result.json"
            target.write_bytes(f.canonical(cmux_result()))
            target.chmod(0o600)
            alias = root / "alias.json"
            alias.symlink_to(target)
            with self.assertRaisesRegex(f.FleetError, "unavailable"):
                f.load_cmux_semantic_result(alias)

    def test_fingerprint_is_canonical(self):
        e = enrollment()
        self.assertEqual(f.digest(copy.deepcopy(e)), f.digest(e))
        self.assertLess(len(f.canonical(f.node_status(e, []))), f.MAX_STATUS_BYTES)


if __name__ == "__main__":
    unittest.main()
