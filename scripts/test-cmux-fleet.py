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

BOOTSTRAP_PATH = Path(__file__).with_name("cmux_fleet_bootstrap.py")
BOOTSTRAP_SPEC = importlib.util.spec_from_file_location(
    "cmux_fleet_bootstrap", BOOTSTRAP_PATH
)
assert BOOTSTRAP_SPEC and BOOTSTRAP_SPEC.loader
bootstrap = importlib.util.module_from_spec(BOOTSTRAP_SPEC)
BOOTSTRAP_SPEC.loader.exec_module(bootstrap)

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
    def setUp(self):
        notices = mock.patch.object(f, "_notice")
        self.notices = notices.start()
        self.addCleanup(notices.stop)

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

    def test_fleet_contract_change_stales_acceptance(self):
        e = enrollment()
        receipt = finalized(e)
        self.assertEqual(
            receipt["glaedaFleetContractGeneration"],
            f.fleet_contract_generation(),
        )
        with mock.patch.object(
            f,
            "fleet_contract_generation",
            return_value=D,
        ):
            status = f.node_status(e, [receipt])
        native = next(
            value
            for value in status["roles"]
            if value["role"] == "cmux_macos_native_build"
        )
        self.assertFalse(native["eligible"])
        self.assertEqual(
            native["reason"],
            "acceptance_glaeda_contract_stale",
        )

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
        self.assertEqual(
            receipt["glaedaFleetContractGeneration"],
            f.fleet_contract_generation(),
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

    def test_git_oid_uses_closed_environment(self):
        observed = {}

        def fake_run(argv, **kwargs):
            observed.update(kwargs)
            return __import__("subprocess").CompletedProcess(
                argv,
                0,
                stdout=COMMIT + "\n",
                stderr="",
            )

        with (
            mock.patch.dict(
                f.os.environ,
                {
                    "PATH": "/attacker/bin",
                    "HOME": "/attacker/home",
                    "PYTHONPATH": "/attacker/python",
                    "SSH_AUTH_SOCK": "/private/agent.sock",
                    "SECRET_SENTINEL": "do-not-forward",
                    "LD_PRELOAD": "/attacker/lib.so",
                    "GIT_DIR": "/attacker/git",
                    "GIT_CONFIG_GLOBAL": "/attacker/config",
                },
                clear=True,
            ),
            mock.patch.object(f.subprocess, "run", side_effect=fake_run),
        ):
            value = f._git_oid(Path("/cmux"), "HEAD^{commit}")

        self.assertEqual(value, COMMIT)
        self.assertEqual(
            observed["env"],
            {
                "LC_ALL": "C",
                "LANG": "C",
                "GIT_CONFIG_GLOBAL": "/dev/null",
                "GIT_CONFIG_NOSYSTEM": "1",
            },
        )
        for forbidden in (
            "PATH",
            "HOME",
            "PYTHONPATH",
            "SSH_AUTH_SOCK",
            "SECRET_SENTINEL",
            "LD_PRELOAD",
            "GIT_DIR",
        ):
            self.assertNotIn(forbidden, observed["env"])

    def test_acceptance_child_environment_is_explicit_allowlist(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            with mock.patch.dict(
                f.os.environ,
                {
                    "PATH": "/reviewed/bin:/usr/bin:/bin",
                    "HOME": str(root / "home"),
                    "CARGO_HOME": str(root / "cargo"),
                    "RUSTUP_HOME": str(root / "rustup"),
                    "DEVELOPER_DIR": "/Applications/Xcode.app/Contents/Developer",
                    "PYTHONPATH": "/attacker/python",
                    "PYTHONHOME": "/attacker/home",
                    "SSH_AUTH_SOCK": "/private/agent.sock",
                    "SECRET_SENTINEL": "secret",
                    "GIT_CONFIG_GLOBAL": "/attacker/gitconfig",
                },
                clear=True,
            ):
                environment = f.acceptance_child_environment(root / "tmp")

        self.assertEqual(
            set(environment),
            {
                "LC_ALL",
                "LANG",
                "TMPDIR",
                "PATH",
                "HOME",
                "CARGO_HOME",
                "RUSTUP_HOME",
                "DEVELOPER_DIR",
            },
        )
        self.assertEqual(environment["LC_ALL"], "C")
        self.assertEqual(environment["LANG"], "C")
        # The operator's PATH is an input the build never sees, so forwarding it
        # would let bootstrap and acceptance disagree about which tools exist.
        self.assertEqual(environment["PATH"], f.CMUX_WORKLOAD_TOOL_PATH)
        self.assertNotIn("PYTHONPATH", environment)
        self.assertNotIn("PYTHONHOME", environment)
        self.assertNotIn("SSH_AUTH_SOCK", environment)
        self.assertNotIn("SECRET_SENTINEL", environment)
        self.assertNotIn("GIT_CONFIG_GLOBAL", environment)

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
            child_environments = []

            def fake_run(argv, **kwargs):
                joined = " ".join(str(item) for item in argv)
                self.assertGreaterEqual(len(argv), 3)
                self.assertEqual(argv[1], "-I")
                environment = dict(kwargs["env"])
                child_environments.append(environment)
                for forbidden in (
                    "PYTHONPATH",
                    "PYTHONHOME",
                    "SSH_AUTH_SOCK",
                    "SECRET_SENTINEL",
                    "GIT_DIR",
                    "GIT_WORK_TREE",
                ):
                    self.assertNotIn(forbidden, environment)
                self.assertEqual(environment["LC_ALL"], "C")
                self.assertEqual(environment["LANG"], "C")
                self.assertEqual(environment["PATH"], f.CMUX_WORKLOAD_TOOL_PATH)
                self.assertEqual(environment["HOME"], str(root / "home"))
                self.assertEqual(environment["CARGO_HOME"], str(root / "cargo"))
                self.assertEqual(environment["RUSTUP_HOME"], str(root / "rustup"))
                self.assertEqual(
                    environment["DEVELOPER_DIR"],
                    "/Applications/Xcode.app/Contents/Developer",
                )
                tmpdir = Path(environment["TMPDIR"])
                self.assertEqual(tmpdir.name, "tmp")
                self.assertEqual(tmpdir.stat().st_mode & 0o777, 0o700)

                if "cmux_workload_profile.py" in joined:
                    self.assertIn("--state-class", argv)
                    self.assertEqual(argv[argv.index("--state-class") + 1], "cold")
                    self.assertNotIn("--state-root", argv)
                    result_path = Path(argv[argv.index("--result") + 1])
                    result_path.write_bytes(f.canonical(result))
                    result_path.chmod(0o600)
                    return __import__("subprocess").CompletedProcess(argv, 0)
                if "cmux_fleet_bootstrap.py" in joined:
                    return __import__("subprocess").CompletedProcess(
                        argv,
                        0,
                        stdout=f.canonical(post),
                        stderr=b"",
                    )
                raise AssertionError(argv)

            environment = {
                "PATH": "/reviewed/bin:/usr/bin:/bin",
                "HOME": str(root / "home"),
                "CARGO_HOME": str(root / "cargo"),
                "RUSTUP_HOME": str(root / "rustup"),
                "DEVELOPER_DIR": "/Applications/Xcode.app/Contents/Developer",
                "LANG": "en_US.UTF-8",
                "LC_ALL": "en_US.UTF-8",
                "TMPDIR": str(root / "ambient-tmp"),
                "PYTHONPATH": "/attacker/python",
                "PYTHONHOME": "/attacker/home",
                "SSH_AUTH_SOCK": "/private/agent.sock",
                "SECRET_SENTINEL": "do-not-forward",
                "GIT_DIR": "/attacker/git",
                "GIT_WORK_TREE": "/attacker/tree",
            }
            with (
                mock.patch.dict(f.os.environ, environment, clear=True),
                mock.patch.object(
                    f,
                    "_git_oid",
                    side_effect=[COMMIT, "2" * 40, COMMIT, "2" * 40],
                ),
                mock.patch.object(f.subprocess, "run", side_effect=fake_run),
            ):
                receipt = f.accept_local(
                    enrollment_path,
                    cmux_root,
                    glaeda,
                    "cmux_linux_ci",
                )

            self.assertEqual(len(child_environments), 2)
            self.assertEqual(child_environments[0], child_environments[1])

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

    def test_local_acceptance_rejects_different_result_or_changed_checkout_source(self):
        for change in ("result_commit", "result_tree", "checkout_commit", "checkout_tree"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                e = enrollment("linux", state="enrolling")
                enrollment_path = root / "enrollment.json"
                enrollment_path.write_bytes(f.canonical(e))
                enrollment_path.chmod(0o600)
                cmux_root = root / "cmux"
                runner = cmux_root / "scripts/ci/cmux_workload_profile.py"
                runner.parent.mkdir(parents=True)
                runner.write_text("# fixture\n")
                glaeda = root / "glaeda"
                glaeda.write_text("#!/bin/sh\nexit 0\n")
                glaeda.chmod(0o755)
                result = cmux_result("cmux_linux_ci")
                if change.startswith("result_"):
                    result["source"][change.removeprefix("result_")] = "3" * 40
                    semantic = f._cmux_semantic_key(result)
                    result["benchmark"]["semantic_comparison_key"] = semantic
                    result["benchmark"]["comparison_context_key"] = f._cmux_context_key(
                        semantic, "cold", result["toolchain"]["identity"])
                observations = [COMMIT, "2" * 40, COMMIT, "2" * 40]
                if change == "checkout_commit":
                    observations[2] = "3" * 40
                if change == "checkout_tree":
                    observations[3] = "3" * 40

                def run(argv, **kwargs):
                    if str(runner) in argv:
                        path = Path(argv[argv.index("--result") + 1])
                        path.write_bytes(f.canonical(result))
                        path.chmod(0o600)
                        return f.subprocess.CompletedProcess(argv, 0)
                    return f.subprocess.CompletedProcess(
                        argv, 0, stdout=f.canonical(bootstrap_for(e)), stderr=b"")

                with mock.patch.object(f, "_git_oid", side_effect=observations), \
                        mock.patch.object(f.subprocess, "run", side_effect=run):
                    with self.assertRaisesRegex(f.FleetError, "source"):
                        f.accept_local(enrollment_path, cmux_root, glaeda, "cmux_linux_ci")

    def _local_acceptance_fixture(self, root):
        e = enrollment("linux", state="enrolling")
        enrollment_path = root / "enrollment.json"
        enrollment_path.write_bytes(f.canonical(e))
        enrollment_path.chmod(0o600)
        cmux_root = root / "cmux"
        runner = cmux_root / "scripts/ci/cmux_workload_profile.py"
        runner.parent.mkdir(parents=True)
        runner.write_text("# fixture\n")
        glaeda = root / "glaeda"
        glaeda.write_text("#!/bin/sh\nexit 0\n")
        glaeda.chmod(0o755)
        return e, enrollment_path, cmux_root, runner, glaeda

    def test_workload_tool_path_matches_bootstrap_observation(self):
        # Bootstrap's readiness verdict means something only if it searched the
        # directories acceptance will hand the build.
        self.assertEqual(
            f.CMUX_WORKLOAD_TOOL_PATH, bootstrap.CMUX_WORKLOAD_TOOL_PATH
        )

    def test_rejected_attempt_keeps_the_evidence_an_acceptance_discards(self):
        # A rejected receipt carries a verdict and no cause, so deleting the
        # runner log with the attempt leaves the operator nothing to read.
        for state, retained in (("failed", True), ("passed", False)):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                e, enrollment_path, cmux_root, runner, glaeda = (
                    self._local_acceptance_fixture(root)
                )
                result = cmux_result("cmux_linux_ci", state=state)

                def run(argv, **kwargs):
                    if str(runner) in argv:
                        path = Path(argv[argv.index("--result") + 1])
                        (path.parent / "tmp/scratch").mkdir(parents=True, exist_ok=True)
                        kwargs["stdout"].write(b"zig 0.16.0 is required\n")
                        path.write_bytes(f.canonical(result))
                        path.chmod(0o600)
                        return f.subprocess.CompletedProcess(
                            argv, 0 if state == "passed" else 1
                        )
                    return f.subprocess.CompletedProcess(
                        argv, 0, stdout=f.canonical(bootstrap_for(e)), stderr=b"")

                with (
                    mock.patch.object(
                        f, "_git_oid", side_effect=[COMMIT, "2" * 40, COMMIT, "2" * 40]
                    ),
                    mock.patch.object(f.subprocess, "run", side_effect=run),
                ):
                    receipt = f.accept_local(
                        enrollment_path, cmux_root, glaeda, "cmux_linux_ci"
                    )

                self.assertEqual(receipt["result"] == "accepted", not retained)
                self.assertEqual(list(root.glob(f.ATTEMPT_PREFIX + "*")), [])
                kept = list(root.glob(f.RETAINED_ATTEMPT_PREFIX + "*"))
                self.assertEqual(len(kept), 1 if retained else 0)
                if retained:
                    self.notices.assert_called_once()
                    self.assertIn(
                        str(kept[0]), self.notices.call_args.args[0]
                    )
                    self.assertIn(
                        "zig 0.16.0 is required",
                        (kept[0] / "cmux-runner.log").read_text(encoding="utf-8"),
                    )
                    self.assertTrue((kept[0] / "result.json").is_file())
                    # The child's scratch tree is the large part and rebuilds.
                    self.assertFalse((kept[0] / "tmp").exists())

    def test_retained_attempts_stay_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            for index in range(f.RETAINED_ATTEMPT_LIMIT + 2):
                attempt = root / f"{f.ATTEMPT_PREFIX}{index:04d}"
                (attempt / "tmp").mkdir(parents=True)
                (attempt / "cmux-runner.log").write_text(str(index), encoding="utf-8")
                os.utime(attempt, (index + 1, index + 1))
                f._retain_attempt(attempt, root)
            kept = sorted(path.name for path in root.glob(f.RETAINED_ATTEMPT_PREFIX + "*"))
            self.assertEqual(len(kept), f.RETAINED_ATTEMPT_LIMIT)
            self.assertEqual(kept[-1], f"{f.RETAINED_ATTEMPT_PREFIX}0004")

    def test_interpreter_without_waitid_refuses_before_the_attempt(self):
        # CPython exposes os.waitid on macOS only from 3.13; the CMUX profile
        # runner cannot wait on its child without it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            root.chmod(0o700)
            _, enrollment_path, cmux_root, _, glaeda = (
                self._local_acceptance_fixture(root)
            )
            with mock.patch.object(
                f, "profile_runner_interpreter_ready", return_value=False
            ):
                with self.assertRaisesRegex(f.FleetError, "os.waitid"):
                    f.accept_local(enrollment_path, cmux_root, glaeda, "cmux_linux_ci")
            self.assertEqual(list(root.glob(f.ATTEMPT_PREFIX + "*")), [])

    def test_accept_local_refuses_fleet_contract_replacement(self):
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
                joined = " ".join(str(item) for item in argv)
                if "cmux_workload_profile.py" in joined:
                    result_path = Path(argv[argv.index("--result") + 1])
                    result_path.write_bytes(f.canonical(result))
                    result_path.chmod(0o600)
                    return __import__("subprocess").CompletedProcess(argv, 0)
                if "cmux_fleet_bootstrap.py" in joined:
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
                    side_effect=[COMMIT, "2" * 40, COMMIT, "2" * 40],
                ),
                mock.patch.object(f.subprocess, "run", side_effect=fake_run),
                mock.patch.object(
                    f,
                    "fleet_contract_generation",
                    side_effect=[A, D],
                ),
            ):
                with self.assertRaisesRegex(
                    f.FleetError,
                    "fleet contract changed during local acceptance",
                ):
                    f.accept_local(
                        enrollment_path,
                        cmux_root,
                        glaeda,
                        "cmux_linux_ci",
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

    def test_renewal_refreshes_capabilities_and_requires_new_acceptance(self):
        for family in ("macos", "linux"):
            with self.subTest(family=family):
                original = enrollment(family)
                old_receipt = finalized(original)
                current = f.transition(original, "quarantined", "toolchain_mismatch")
                bootstrap = bootstrap_for(current, D)
                bootstrap["glaedaGeneration"] = E
                bootstrap["osVersionClass"] = "updated-os"
                before = copy.deepcopy(current)
                plan = f.renewal_plan(current, bootstrap)
                self.assertEqual(current, before)
                renewed = plan["replacement"]
                self.assertEqual(renewed["nodeId"], current["nodeId"])
                self.assertEqual(renewed["operatorFleetScope"], current["operatorFleetScope"])
                self.assertEqual(renewed["enrollmentGeneration"], current["enrollmentGeneration"] + 1)
                self.assertEqual(renewed["supportedToolchainGenerations"], [D])
                self.assertEqual(renewed["glaedaGeneration"], E)
                self.assertEqual(renewed["os"]["versionClass"], "updated-os")
                self.assertEqual(renewed["state"], "enrolling")
                self.assertIsNone(renewed["quarantineReason"])
                with self.assertRaisesRegex(f.FleetError, "current accepted role"):
                    f.transition(renewed, "eligible", None, [old_receipt])
                fresh_receipt = finalized(renewed, toolchain=D)
                eligible = f.transition(renewed, "eligible", None, [fresh_receipt])
                self.assertTrue(f.node_status(eligible, [fresh_receipt])["routingCandidateEligible"])
                self.assertFalse(f.node_status(eligible, [fresh_receipt])["automaticDispatchAuthorized"])

    def test_renewal_refuses_live_terminal_blocked_and_exhausted_inputs(self):
        for state in f.STATES:
            if state == "quarantined":
                continue
            with self.subTest(state=state):
                current = enrollment(state=state)
                with self.assertRaisesRegex(f.FleetError, "quarantined"):
                    f.renewal_plan(current, bootstrap_for(current))
        current = f.transition(enrollment(), "quarantined", "service_mismatch")
        bootstrap = bootstrap_for(current)
        bootstrap["eligibleForEnrollment"] = False
        with self.assertRaisesRegex(f.FleetError, "blocking checks"):
            f.renewal_plan(current, bootstrap)
        current["enrollmentGeneration"] = 2**31 - 1
        with self.assertRaisesRegex(f.FleetError, "exhausted"):
            f.renewal_plan(current, bootstrap_for(current))

    def test_apply_renewal_binds_plan_and_publishes_once(self):
        for changed in (None, "enrollment", "bootstrap", "digest"):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                path = root / "enrollment.json"
                bootstrap_path = root / "bootstrap.json"
                current = f.transition(enrollment(), "quarantined", "toolchain_mismatch")
                bootstrap = bootstrap_for(current, D)
                plan = f.renewal_plan(current, bootstrap)
                if changed == "enrollment":
                    current["quarantineReason"] = "hardware_failure"
                if changed == "bootstrap":
                    bootstrap["toolchainGeneration"] = E
                for target, value in ((path, current), (bootstrap_path, bootstrap)):
                    target.write_bytes(f.canonical(value))
                    target.chmod(0o600)
                before = path.read_bytes()
                self.assertEqual(f.renewal_plan(current, bootstrap)["replacement"]["state"], "enrolling")
                self.assertFalse((root / ".mutation.lock").exists())
                if changed:
                    with self.assertRaisesRegex(f.FleetError, "plan changed"):
                        f.apply_renewal(path, bootstrap_path, A if changed == "digest" else plan["planSha256"])
                    self.assertEqual(path.read_bytes(), before)
                else:
                    result = f.apply_renewal(path, bootstrap_path, plan["planSha256"])
                    self.assertEqual(result, plan["replacement"])
                    self.assertEqual(f.load(path), result)
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                    with self.assertRaisesRegex(f.FleetError, "quarantined"):
                        f.apply_renewal(path, bootstrap_path, plan["planSha256"])
                    self.assertEqual(f.load(path), result)
                self.assertFalse(list(root.glob(".enrollment.next.*")))

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

    def test_transition_refuses_replaced_or_changed_lock_before_publication(self):
        for change in ("replace", "mode", "unlink", "symlink"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                root.chmod(0o700)
                path = root / "enrollment.json"
                original = f.canonical(enrollment())
                path.write_bytes(original)
                path.chmod(0o600)
                transition = f.transition

                def changed_lock(*args):
                    result = transition(*args)
                    lock = root / ".mutation.lock"
                    if change == "mode":
                        lock.chmod(0o644)
                    else:
                        lock.rename(root / "old-lock")
                        if change == "replace":
                            lock.touch(mode=0o600)
                        elif change == "symlink":
                            lock.symlink_to(root / "old-lock")
                    return result

                with mock.patch.object(f, "transition", side_effect=changed_lock):
                    with self.assertRaisesRegex(f.FleetError, "lock"):
                        f.apply_transition(path, "draining", None, [])
                self.assertEqual(path.read_bytes(), original)
                self.assertFalse(list(root.glob(".enrollment.next.*")))

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
