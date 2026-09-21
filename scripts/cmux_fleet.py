#!/usr/bin/env python3
"""CMUX-owned fleet enrollment, role acceptance, and bounded status projection."""
from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import stat
import subprocess
import tempfile
import sys
from pathlib import Path
from typing import Any

ENROLLMENT_SCHEMA = "glaeda-cmux-fleet-enrollment/v1"
ACCEPTANCE_SCHEMA = "glaeda-cmux-fleet-acceptance/v2"
STATUS_SCHEMA = "glaeda-cmux-fleet-node-status/v1"
BOOTSTRAP_SCHEMA = "glaeda-cmux-fleet-bootstrap/v1"
MAX_DOCUMENT_BYTES = 64 * 1024
MAX_STATUS_BYTES = 16 * 1024
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
COMMIT_RE = re.compile(r"[0-9a-f]{40}\Z")
TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9._-]{0,79}\Z")
NODE_RE = re.compile(r"cmux-[a-z0-9][a-z0-9-]{2,59}\Z")
REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}/[A-Za-z0-9_.-]{1,100}\Z")

ROLES = (
    "artifact_cache",
    "background_replay",
    "benchmark",
    "cmux_linux_agent",
    "cmux_linux_ci",
    "cmux_macos_native_build",
    "cmux_macos_test",
)
ENROLLABLE_ROLES = {
    "cmux_linux_ci",
    "cmux_macos_native_build",
}
CMUX_REPOSITORY = "manaflow-ai/cmux"
CMUX_RESULT_DOCUMENT_TYPE = "cmux-workload-result"
CMUX_RESULT_SCHEMA_VERSION = 1
CMUX_RESULT_STATES = {"passed", "failed", "timed_out", "ambiguous"}
CMUX_PROFILE_RUNNER = "scripts/ci/cmux_workload_profile.py"
LOCAL_EXECUTION_CLASS = "glaeda-local-profile/v1"
EXTERNAL_EVIDENCE_CLASS = "external-evidence/v1"
ACCEPTANCE_CHILD_ENV_KEYS = (
    "PATH",
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "DEVELOPER_DIR",
)
ROLE_PROFILES = {
    "cmux_linux_ci": {"id": "cmux.ci.guard", "generation": 1},
    "cmux_macos_native_build": {"id": "cmux.macos.dev-check", "generation": 1},
}
STATES = ("discovered", "enrolling", "eligible", "draining", "quarantined", "retired")
QUARANTINE_REASONS = (
    "dirty_canonical_checkout",
    "disk_pressure",
    "failed_acceptance",
    "hardware_failure",
    "service_mismatch",
    "stale_glaeda_generation",
    "toolchain_mismatch",
    "unexplained_process_settlement",
)
ROLE_OS = {
    "cmux_macos_native_build": "macos",
    "cmux_macos_test": "macos",
    "cmux_linux_ci": "linux",
    "cmux_linux_agent": "linux",
}
TRANSITIONS = {
    "discovered": {"enrolling", "retired"},
    "enrolling": {"eligible", "quarantined", "retired"},
    "eligible": {"draining", "quarantined", "retired"},
    "draining": {"eligible", "quarantined", "retired"},
    "quarantined": {"enrolling", "retired"},
    "retired": set(),
}
ENROLLMENT_KEYS = {
    "schema", "nodeId", "architecture", "os", "hardwareCapabilityClass",
    "supportedToolchainGenerations", "roleProfiles",
    "allowedExecutionRoles", "operatorFleetScope",
    "enrollmentGeneration", "glaedaGeneration", "state", "quarantineReason",
}

class FleetError(RuntimeError):
    """A closed fleet enrollment/status refusal."""


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(canonical(value)).hexdigest()


def cmux_canonical_bytes(value: object) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode()


def cmux_digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(cmux_canonical_bytes(value)).hexdigest()


def _file_sha256(path: Path) -> str:
    digest_value = hashlib.sha256()
    try:
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                digest_value.update(chunk)
    except OSError as error:
        raise FleetError("Glaeda fleet contract file is unavailable") from error
    return "sha256:" + digest_value.hexdigest()


def fleet_contract_generation() -> str:
    root = Path(__file__).resolve().parent
    files = {
        "cmux_fleet.py": _file_sha256(root / "cmux_fleet.py"),
        "cmux_fleet_bootstrap.py": _file_sha256(root / "cmux_fleet_bootstrap.py"),
    }
    return digest(files)


def exact_keys(value: object, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise FleetError(f"{label} has unknown or missing fields")
    return value


def token(value: object, label: str) -> str:
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise FleetError(f"{label} is invalid")
    return value


def sha256(value: object, label: str, *, optional: bool = False) -> str | None:
    if value is None and optional:
        return None
    if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
        raise FleetError(f"{label} is invalid")
    return value


def positive_int(value: object, label: str) -> int:
    if type(value) is not int or not 1 <= value <= 2**31 - 1:
        raise FleetError(f"{label} is invalid")
    return value


def sorted_unique_strings(value: object, label: str, *, allowed: set[str] | None = None) -> list[str]:
    if not isinstance(value, list) or len(value) > 32 or any(not isinstance(v, str) for v in value):
        raise FleetError(f"{label} is invalid")
    if value != sorted(set(value)):
        raise FleetError(f"{label} must be sorted and unique")
    if allowed is not None and any(v not in allowed for v in value):
        raise FleetError(f"{label} contains an unsupported value")
    return value


def validate_enrollment(value: object) -> dict[str, Any]:
    doc = exact_keys(value, ENROLLMENT_KEYS, "enrollment")
    if doc["schema"] != ENROLLMENT_SCHEMA:
        raise FleetError("enrollment schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise FleetError("nodeId must be an opaque cmux-* identifier")
    if doc["architecture"] not in {"arm64", "x86_64"}:
        raise FleetError("architecture is unsupported")
    os_doc = exact_keys(doc["os"], {"family", "versionClass"}, "os")
    if os_doc["family"] not in {"macos", "linux"}:
        raise FleetError("OS family is unsupported")
    token(os_doc["versionClass"], "OS version class")
    token(doc["hardwareCapabilityClass"], "hardware capability class")
    token(doc["operatorFleetScope"], "operator/fleet scope")
    generations = sorted_unique_strings(doc["supportedToolchainGenerations"], "supported toolchain generations")
    if not generations or any(SHA256_RE.fullmatch(v) is None for v in generations):
        raise FleetError("supported toolchain generations are invalid")
    roles = sorted_unique_strings(doc["allowedExecutionRoles"], "allowed execution roles", allowed=set(ROLES))
    if not roles:
        raise FleetError("at least one execution role is required")
    role_profiles = doc["roleProfiles"]
    if not isinstance(role_profiles, dict) or set(role_profiles) != set(roles):
        raise FleetError("role profiles must exactly cover allowed roles")
    for role in roles:
        profile = exact_keys(role_profiles[role], {"id", "generation"}, "role profile")
        token(profile["id"], "role profile id")
        positive_int(profile["generation"], "role profile generation")
        if profile != ROLE_PROFILES.get(role):
            raise FleetError(f"role {role} profile is not the reviewed v1 profile")
    unreviewed = [role for role in roles if role not in ENROLLABLE_ROLES]
    if unreviewed:
        raise FleetError(
            "execution role lacks a reviewed v1 acceptance workload: "
            + ",".join(unreviewed)
        )
    for role in roles:
        required_os = ROLE_OS.get(role)
        if required_os is not None and required_os != os_doc["family"]:
            raise FleetError(f"role {role} is incompatible with OS family {os_doc['family']}")
    positive_int(doc["enrollmentGeneration"], "enrollment generation")
    sha256(doc["glaedaGeneration"], "Glaeda generation")
    if doc["state"] not in STATES:
        raise FleetError("node state is unsupported")
    reason = doc["quarantineReason"]
    if doc["state"] == "quarantined":
        if reason not in QUARANTINE_REASONS:
            raise FleetError("quarantine reason is required and must be reviewed")
    elif reason is not None:
        raise FleetError("quarantine reason is only valid for quarantined nodes")
    return doc


def enrollment_from_bootstrap(
    bootstrap_value: object,
    *,
    node_id: str,
    operator_fleet_scope: str,
    enrollment_generation: int,
) -> dict[str, Any]:
    if (
        not isinstance(bootstrap_value, dict)
        or bootstrap_value.get("schema") != BOOTSTRAP_SCHEMA
    ):
        raise FleetError("bootstrap receipt schema is unsupported")
    if bootstrap_value.get("authority") != "observation_only":
        raise FleetError("bootstrap receipt authority is invalid")
    if bootstrap_value.get("eligibleForEnrollment") is not True:
        raise FleetError("bootstrap receipt has blocking checks")
    enrollment = {
        "schema": ENROLLMENT_SCHEMA,
        "nodeId": node_id,
        "architecture": bootstrap_value.get("architecture"),
        "os": {
            "family": bootstrap_value.get("platform"),
            "versionClass": bootstrap_value.get("osVersionClass"),
        },
        "hardwareCapabilityClass": bootstrap_value.get("hardwareCapabilityClass"),
        "supportedToolchainGenerations": [
            bootstrap_value.get("toolchainGeneration")
        ],
        "roleProfiles": bootstrap_value.get("roleProfiles"),
        "allowedExecutionRoles": bootstrap_value.get("roles"),
        "operatorFleetScope": operator_fleet_scope,
        "enrollmentGeneration": enrollment_generation,
        "glaedaGeneration": bootstrap_value.get("glaedaGeneration"),
        "state": "enrolling",
        "quarantineReason": None,
    }
    return validate_enrollment(enrollment)


def _cmux_semantic_key(value: dict[str, Any]) -> str:
    source = value["source"]
    profile = value["profile"]
    runtime_inputs = value["runtime_input_identities"]
    return cmux_digest(
        {
            "source": {
                "repository": source["repository"],
                "tree": source["tree"],
            },
            "profile": {
                "id": profile["id"],
                "generation": profile["generation"],
            },
            "semantic_validator": value["semantic_validator"],
            "environment_class": value["environment_class"],
            "parameters": value["parameters"],
            "runtime_inputs": [
                {
                    "name": item["name"],
                    "class": item["class"],
                    "identity": item["identity"],
                    "sha256": item["sha256"],
                }
                for item in runtime_inputs
            ],
        }
    )


def _cmux_context_key(
    semantic_key: str,
    state_class: str,
    toolchain_identity: str,
) -> str:
    return cmux_digest(
        {
            "semantic_key": semantic_key,
            "state_class": state_class,
            "toolchain_identity": toolchain_identity,
        }
    )


def validate_cmux_semantic_result(
    value: object,
    expected_profile: dict[str, object],
) -> dict[str, Any]:
    doc = exact_keys(
        value,
        {
            "document_type",
            "schema_version",
            "source",
            "profile",
            "semantic_validator",
            "environment_class",
            "expected_result_class",
            "result",
            "parameters",
            "runtime_input_identities",
            "artifact_identities",
            "validation",
            "stage_timings",
            "resource_summary",
            "toolchain",
            "benchmark",
            "network_class",
            "timeout_class",
            "cleanup",
            "exit_code",
            "started_at_unix_millis",
            "ended_at_unix_millis",
        },
        "CMUX semantic result",
    )
    if (
        doc["document_type"] != CMUX_RESULT_DOCUMENT_TYPE
        or type(doc["schema_version"]) is not int
        or doc["schema_version"] != CMUX_RESULT_SCHEMA_VERSION
        or doc["result"] not in CMUX_RESULT_STATES
    ):
        raise FleetError("CMUX semantic result contract is unsupported")

    source = exact_keys(
        doc["source"],
        {"repository", "commit", "tree"},
        "CMUX semantic source",
    )
    if (
        source["repository"] != CMUX_REPOSITORY
        or not isinstance(source["commit"], str)
        or COMMIT_RE.fullmatch(source["commit"]) is None
        or not isinstance(source["tree"], str)
        or COMMIT_RE.fullmatch(source["tree"]) is None
    ):
        raise FleetError("CMUX semantic source commit/tree is invalid")

    profile = exact_keys(
        doc["profile"],
        {"id", "generation"},
        "CMUX semantic profile",
    )
    token(profile["id"], "CMUX semantic profile id")
    positive_int(profile["generation"], "CMUX semantic profile generation")
    if profile != expected_profile:
        raise FleetError("CMUX semantic profile differs from enrolled role profile")
    if doc["parameters"] != {}:
        raise FleetError("CMUX fleet acceptance profile parameters must be empty")
    if doc["runtime_input_identities"] != []:
        raise FleetError("CMUX fleet acceptance profile runtime inputs must be empty")

    if (
        not isinstance(doc["semantic_validator"], str)
        or not doc["semantic_validator"]
        or not isinstance(doc["environment_class"], str)
        or TOKEN_RE.fullmatch(doc["environment_class"]) is None
        or not isinstance(doc["expected_result_class"], str)
        or not doc["expected_result_class"]
        or not isinstance(doc["network_class"], str)
        or not doc["network_class"]
        or not isinstance(doc["timeout_class"], str)
        or not doc["timeout_class"]
        or type(doc["exit_code"]) is not int
        or type(doc["started_at_unix_millis"]) is not int
        or type(doc["ended_at_unix_millis"]) is not int
        or not 0
        <= doc["started_at_unix_millis"]
        <= doc["ended_at_unix_millis"]
        < 253402300800000
    ):
        raise FleetError("CMUX semantic result state is invalid")

    validation = exact_keys(
        doc["validation"],
        {"missing_required_artifact_classes"},
        "CMUX semantic validation",
    )
    missing = validation["missing_required_artifact_classes"]
    if (
        not isinstance(missing, list)
        or any(not isinstance(item, str) or not item for item in missing)
    ):
        raise FleetError("CMUX semantic artifact validation is invalid")

    artifacts = doc["artifact_identities"]
    if not isinstance(artifacts, list):
        raise FleetError("CMUX semantic artifact identities are invalid")
    for item in artifacts:
        artifact = exact_keys(
            item,
            {"class", "path_class", "sha256", "bytes"},
            "CMUX semantic artifact identity",
        )
        if (
            not isinstance(artifact["class"], str)
            or not artifact["class"]
            or artifact["path_class"] != "repository_output"
            or not isinstance(artifact["sha256"], str)
            or SHA256_RE.fullmatch(artifact["sha256"]) is None
            or type(artifact["bytes"]) is not int
            or artifact["bytes"] < 0
        ):
            raise FleetError("CMUX semantic artifact identity is invalid")

    benchmark = exact_keys(
        doc["benchmark"],
        {"state_class", "semantic_comparison_key", "comparison_context_key"},
        "CMUX semantic benchmark",
    )
    if (
        benchmark["state_class"] != "cold"
        or not isinstance(benchmark["semantic_comparison_key"], str)
        or SHA256_RE.fullmatch(benchmark["semantic_comparison_key"]) is None
        or not isinstance(benchmark["comparison_context_key"], str)
        or SHA256_RE.fullmatch(benchmark["comparison_context_key"]) is None
    ):
        raise FleetError("CMUX fleet acceptance benchmark identity is invalid")

    toolchain = exact_keys(
        doc["toolchain"],
        {"identity", "observations"},
        "CMUX semantic toolchain",
    )
    observations = toolchain["observations"]
    if (
        not isinstance(toolchain["identity"], str)
        or SHA256_RE.fullmatch(toolchain["identity"]) is None
        or not isinstance(observations, dict)
        or any(
            not isinstance(name, str)
            or not name
            or not isinstance(observation, str)
            for name, observation in observations.items()
        )
        or toolchain["identity"] != cmux_digest(observations)
    ):
        raise FleetError("CMUX semantic toolchain identity is inconsistent")

    semantic_key = _cmux_semantic_key(doc)
    if benchmark["semantic_comparison_key"] != semantic_key:
        raise FleetError("CMUX semantic comparison identity is inconsistent")
    if benchmark["comparison_context_key"] != _cmux_context_key(
        semantic_key,
        benchmark["state_class"],
        toolchain["identity"],
    ):
        raise FleetError("CMUX comparison context identity is inconsistent")

    cleanup = exact_keys(
        doc["cleanup"],
        {"state", "process_group_settled"},
        "CMUX semantic cleanup",
    )
    if (
        cleanup["state"] not in {"complete", "forced", "incomplete"}
        or type(cleanup["process_group_settled"]) is not bool
    ):
        raise FleetError("CMUX semantic cleanup evidence is invalid")
    if doc["result"] == "ambiguous":
        if cleanup != {"state": "forced", "process_group_settled": False}:
            raise FleetError("ambiguous CMUX result lacks forced cleanup evidence")
    elif cleanup != {"state": "complete", "process_group_settled": True}:
        raise FleetError("terminal CMUX result lacks complete cleanup")

    if doc["result"] == "passed" and (doc["exit_code"] != 0 or missing):
        raise FleetError("passed CMUX result is inconsistent")
    if doc["result"] == "failed" and doc["exit_code"] == 0 and not missing:
        raise FleetError("failed CMUX result is inconsistent")
    timings = doc["stage_timings"]
    if (
        not isinstance(timings, list)
        or not timings
        or any(
            not isinstance(item, dict)
            or set(item) != {"stage", "seconds"}
            or not isinstance(item["stage"], str)
            or not item["stage"]
            or isinstance(item["seconds"], bool)
            or not isinstance(item["seconds"], (int, float))
            or item["seconds"] < 0
            for item in timings
        )
    ):
        raise FleetError("CMUX semantic stage timings are invalid")

    resource = exact_keys(
        doc["resource_summary"],
        {"resource_class", "cpu_count", "memory_bytes", "architecture"},
        "CMUX semantic resources",
    )
    if (
        not isinstance(resource["resource_class"], str)
        or not resource["resource_class"]
        or type(resource["cpu_count"]) is not int
        or resource["cpu_count"] <= 0
        or (
            resource["memory_bytes"] is not None
            and (
                type(resource["memory_bytes"]) is not int
                or resource["memory_bytes"] <= 0
            )
        )
        or not isinstance(resource["architecture"], str)
        or not resource["architecture"]
    ):
        raise FleetError("CMUX semantic resource summary is invalid")
    return doc


ACCEPTANCE_RECEIPT_KEYS = {
    "schema", "nodeId", "enrollmentGeneration", "role", "source", "profile",
    "toolchainGeneration", "glaedaGeneration", "glaedaFleetContractGeneration",
    "cmuxSemanticResultSha256",
    "cmuxSemanticResultState", "cmuxEnvironmentClass", "cmuxToolchainIdentity",
    "postBootstrapSha256", "executionClass", "localExecutionAttemptSha256",
    "processSettlement", "result",
}


def validate_acceptance_receipt(value: object) -> dict[str, Any]:
    doc = exact_keys(value, ACCEPTANCE_RECEIPT_KEYS, "acceptance receipt")
    if doc["schema"] != ACCEPTANCE_SCHEMA:
        raise FleetError("acceptance receipt schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise FleetError("acceptance receipt nodeId is invalid")
    positive_int(doc["enrollmentGeneration"], "acceptance receipt enrollment generation")
    role = doc["role"]
    if role not in ENROLLABLE_ROLES:
        raise FleetError("acceptance receipt role lacks a reviewed v1 profile")
    source = exact_keys(
        doc["source"],
        {"repository", "commit", "tree"},
        "acceptance receipt source",
    )
    if source["repository"] != CMUX_REPOSITORY:
        raise FleetError("acceptance receipt source repository is invalid")
    if (
        not isinstance(source["commit"], str)
        or COMMIT_RE.fullmatch(source["commit"]) is None
        or not isinstance(source["tree"], str)
        or COMMIT_RE.fullmatch(source["tree"]) is None
    ):
        raise FleetError("acceptance receipt source commit/tree is invalid")
    profile = exact_keys(doc["profile"], {"id", "generation"}, "acceptance receipt profile")
    token(profile["id"], "acceptance receipt profile id")
    positive_int(profile["generation"], "acceptance receipt profile generation")
    if profile != ROLE_PROFILES.get(role):
        raise FleetError("acceptance receipt profile is not the reviewed role profile")
    sha256(doc["toolchainGeneration"], "acceptance receipt toolchain generation")
    sha256(doc["glaedaGeneration"], "acceptance receipt Glaeda generation")
    sha256(
        doc["glaedaFleetContractGeneration"],
        "acceptance receipt Glaeda fleet contract generation",
    )
    sha256(doc["cmuxSemanticResultSha256"], "CMUX semantic result digest")
    sha256(doc["cmuxToolchainIdentity"], "CMUX semantic toolchain identity")
    sha256(doc["postBootstrapSha256"], "post-acceptance bootstrap digest")
    token(doc["cmuxEnvironmentClass"], "CMUX environment class")
    if doc["cmuxSemanticResultState"] not in CMUX_RESULT_STATES:
        raise FleetError("CMUX semantic result state is invalid")
    if doc["executionClass"] not in {
        LOCAL_EXECUTION_CLASS,
        EXTERNAL_EVIDENCE_CLASS,
    }:
        raise FleetError("acceptance execution class is invalid")
    local_attempt = sha256(
        doc["localExecutionAttemptSha256"],
        "local execution attempt",
        optional=True,
    )
    if (
        (doc["executionClass"] == LOCAL_EXECUTION_CLASS)
        != (local_attempt is not None)
    ):
        raise FleetError("acceptance local execution evidence is inconsistent")
    if doc["processSettlement"] not in {"complete", "incomplete"}:
        raise FleetError("acceptance process settlement is invalid")
    if doc["result"] not in {"accepted", "rejected"}:
        raise FleetError("acceptance receipt result is invalid")
    accepted = (
        doc["cmuxSemanticResultState"] == "passed"
        and doc["processSettlement"] == "complete"
        and doc["executionClass"] == LOCAL_EXECUTION_CLASS
        and local_attempt is not None
    )
    if (doc["result"] == "accepted") != accepted:
        raise FleetError("acceptance receipt result disagrees with semantic result")
    return doc


def acceptance_matches_enrollment(enrollment: dict[str, Any], receipt: dict[str, Any], role: str) -> tuple[bool, str]:
    if receipt.get("schema") != ACCEPTANCE_SCHEMA or receipt.get("result") != "accepted":
        return False, "acceptance_missing_or_rejected"
    if receipt.get("nodeId") != enrollment["nodeId"] or receipt.get("role") != role:
        return False, "acceptance_identity_mismatch"
    if receipt.get("enrollmentGeneration") != enrollment["enrollmentGeneration"]:
        return False, "acceptance_enrollment_stale"
    if receipt.get("glaedaGeneration") != enrollment["glaedaGeneration"]:
        return False, "acceptance_glaeda_stale"
    if receipt.get("glaedaFleetContractGeneration") != fleet_contract_generation():
        return False, "acceptance_glaeda_contract_stale"
    if receipt.get("toolchainGeneration") not in enrollment["supportedToolchainGenerations"]:
        return False, "acceptance_toolchain_stale"
    if receipt.get("profile") != enrollment["roleProfiles"].get(role):
        return False, "acceptance_profile_stale"
    return True, "accepted"


def validate_post_acceptance_bootstrap(
    enrollment: dict[str, Any],
    bootstrap_value: object,
    toolchain_generation: str,
) -> str:
    observed = enrollment_from_bootstrap(
        bootstrap_value,
        node_id=enrollment["nodeId"],
        operator_fleet_scope=enrollment["operatorFleetScope"],
        enrollment_generation=enrollment["enrollmentGeneration"],
    )
    stable_fields = ENROLLMENT_KEYS - {
        "state",
        "quarantineReason",
        "supportedToolchainGenerations",
    }
    expected_stable = {name: enrollment[name] for name in stable_fields}
    observed_stable = {name: observed[name] for name in stable_fields}
    if observed_stable != expected_stable:
        raise FleetError(
            "post-acceptance bootstrap differs from enrolled machine capability"
        )
    if observed["supportedToolchainGenerations"] != [toolchain_generation]:
        raise FleetError(
            "post-acceptance bootstrap toolchain differs from acceptance"
        )
    return digest(bootstrap_value)


def finalize_acceptance(
    enrollment_value: object,
    role: str,
    toolchain_generation: str,
    cmux_result_value: object,
    post_bootstrap_value: object,
    cmux_result_sha256: str | None = None,
    *,
    execution_class: str = EXTERNAL_EVIDENCE_CLASS,
    local_execution_attempt_sha256: str | None = None,
) -> dict[str, Any]:
    enrollment = validate_enrollment(enrollment_value)
    if role not in enrollment["allowedExecutionRoles"]:
        raise FleetError("acceptance role is outside the enrollment allowlist")
    if toolchain_generation not in enrollment["supportedToolchainGenerations"]:
        raise FleetError("acceptance toolchain generation is outside the enrollment allowlist")
    post_bootstrap_sha256 = validate_post_acceptance_bootstrap(
        enrollment,
        post_bootstrap_value,
        toolchain_generation,
    )
    expected_profile = enrollment["roleProfiles"][role]
    semantic = validate_cmux_semantic_result(cmux_result_value, expected_profile)
    actual_cmux_result_sha256 = digest(semantic)
    if cmux_result_sha256 is None:
        cmux_result_sha256 = actual_cmux_result_sha256
    sha256(cmux_result_sha256, "CMUX semantic result digest")
    if cmux_result_sha256 != actual_cmux_result_sha256:
        raise FleetError("CMUX semantic result digest does not match validated result")
    cleanup = semantic["cleanup"]
    settlement = (
        "complete"
        if cleanup["state"] == "complete" and cleanup["process_group_settled"] is True
        else "incomplete"
    )
    locally_bound = (
        execution_class == LOCAL_EXECUTION_CLASS
        and local_execution_attempt_sha256 is not None
    )
    if local_execution_attempt_sha256 is not None:
        sha256(local_execution_attempt_sha256, "local execution attempt")
    result = (
        "accepted"
        if semantic["result"] == "passed"
        and settlement == "complete"
        and locally_bound
        else "rejected"
    )
    receipt = {
        "schema": ACCEPTANCE_SCHEMA,
        "nodeId": enrollment["nodeId"],
        "enrollmentGeneration": enrollment["enrollmentGeneration"],
        "role": role,
        "source": semantic["source"],
        "profile": semantic["profile"],
        "toolchainGeneration": toolchain_generation,
        "glaedaGeneration": enrollment["glaedaGeneration"],
        "glaedaFleetContractGeneration": fleet_contract_generation(),
        "cmuxSemanticResultSha256": cmux_result_sha256,
        "cmuxSemanticResultState": semantic["result"],
        "cmuxEnvironmentClass": semantic["environment_class"],
        "cmuxToolchainIdentity": semantic["toolchain"]["identity"],
        "postBootstrapSha256": post_bootstrap_sha256,
        "executionClass": execution_class,
        "localExecutionAttemptSha256": local_execution_attempt_sha256,
        "processSettlement": settlement,
        "result": result,
    }
    validate_acceptance_receipt(receipt)
    if len(canonical(receipt)) > MAX_STATUS_BYTES:
        raise FleetError("acceptance receipt exceeds size ceiling")
    return receipt


def node_status(enrollment_value: object, acceptance_values: list[object]) -> dict[str, Any]:
    enrollment = validate_enrollment(enrollment_value)
    receipts: dict[str, dict[str, Any]] = {}
    for value in acceptance_values:
        receipt = validate_acceptance_receipt(value)
        role = receipt["role"]
        if role in receipts:
            raise FleetError("duplicate acceptance receipt for role")
        receipts[role] = receipt
    role_status = []
    for role in enrollment["allowedExecutionRoles"]:
        if enrollment["state"] != "eligible":
            eligible, reason = False, f"node_{enrollment['state']}"
        else:
            receipt = receipts.get(role)
            if receipt is None:
                eligible, reason = False, "acceptance_missing_or_rejected"
            else:
                eligible, reason = acceptance_matches_enrollment(enrollment, receipt, role)
        role_status.append({"role": role, "eligible": eligible, "reason": reason})
    status = {
        "schema": STATUS_SCHEMA,
        "nodeId": enrollment["nodeId"],
        "enrollmentGeneration": enrollment["enrollmentGeneration"],
        "state": enrollment["state"],
        "routingCandidateEligible": any(r["eligible"] for r in role_status),
        "automaticDispatchAuthorized": False,
        "capability": {
            "architecture": enrollment["architecture"],
            "osFamily": enrollment["os"]["family"],
            "osVersionClass": enrollment["os"]["versionClass"],
            "hardwareCapabilityClass": enrollment["hardwareCapabilityClass"],
            "supportedToolchainGenerations": enrollment[
                "supportedToolchainGenerations"
            ],
            "roleProfiles": enrollment["roleProfiles"],
            "glaedaGeneration": enrollment["glaedaGeneration"],
            "operatorFleetScope": enrollment["operatorFleetScope"],
        },
        "roles": role_status,
        "privacy": {
            "containsSerialNumber": False,
            "containsPrivateNetworking": False,
            "containsUsername": False,
        },
    }
    if len(canonical(status)) > MAX_STATUS_BYTES:
        raise FleetError("status document exceeds size ceiling")
    return status


def transition(
    enrollment_value: object,
    target: str,
    reason: str | None,
    acceptance_values: list[object] | None = None,
) -> dict[str, Any]:
    enrollment = dict(validate_enrollment(enrollment_value))
    current = enrollment["state"]
    if target not in STATES or target not in TRANSITIONS[current]:
        raise FleetError(f"transition {current}->{target} is unsupported")
    if target == "quarantined":
        if reason not in QUARANTINE_REASONS:
            raise FleetError("a reviewed quarantine reason is required")
        enrollment["quarantineReason"] = reason
    else:
        if reason is not None:
            raise FleetError("transition reason is only accepted for quarantine")
        enrollment["quarantineReason"] = None
    if current == "quarantined" and target == "enrolling":
        if enrollment["enrollmentGeneration"] == 2**31 - 1:
            raise FleetError("enrollment generation is exhausted")
        enrollment["enrollmentGeneration"] += 1
    enrollment["state"] = target
    enrollment = validate_enrollment(enrollment)
    if target == "eligible":
        status = node_status(enrollment, acceptance_values or [])
        if not status["routingCandidateEligible"]:
            raise FleetError(
                "eligible transition requires a current accepted role receipt"
            )
    return enrollment


def _read_private_document(descriptor: int) -> object:
    before = os.fstat(descriptor)
    if (
        not stat.S_ISREG(before.st_mode)
        or before.st_uid != os.geteuid()
        or before.st_nlink != 1
        or stat.S_IMODE(before.st_mode) != 0o600
        or before.st_size < 0
        or before.st_size > MAX_DOCUMENT_BYTES
    ):
        raise FleetError("fleet document ownership or mode is unsafe")
    chunks: list[bytes] = []
    total = 0
    while True:
        chunk = os.read(descriptor, min(8192, MAX_DOCUMENT_BYTES + 1 - total))
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
        if total > MAX_DOCUMENT_BYTES:
            raise FleetError("fleet document exceeds size ceiling")
    after = os.fstat(descriptor)
    if (
        before.st_dev != after.st_dev
        or before.st_ino != after.st_ino
        or before.st_uid != after.st_uid
        or before.st_gid != after.st_gid
        or before.st_mode != after.st_mode
        or before.st_nlink != after.st_nlink
        or before.st_size != after.st_size
        or before.st_mtime_ns != after.st_mtime_ns
        or before.st_ctime_ns != after.st_ctime_ns
        or total != after.st_size
    ):
        raise FleetError("fleet document changed while reading")
    try:
        return json.loads(b"".join(chunks))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("fleet document is invalid JSON") from error


def load(path: Path) -> object:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        raise FleetError("fleet document is unavailable") from error
    try:
        return _read_private_document(descriptor)
    finally:
        os.close(descriptor)


def load_cmux_semantic_result(path: Path) -> tuple[dict[str, Any], str]:
    try:
        descriptor = os.open(
            path,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as error:
        raise FleetError("CMUX semantic result is unavailable") from error
    try:
        info = os.fstat(descriptor)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.geteuid()
            or info.st_nlink != 1
            or stat.S_IMODE(info.st_mode) != 0o600
            or info.st_size <= 0
            or info.st_size > MAX_DOCUMENT_BYTES
        ):
            raise FleetError("CMUX semantic result file is unsafe")
        raw = b""
        while len(raw) <= MAX_DOCUMENT_BYTES:
            chunk = os.read(descriptor, min(8192, MAX_DOCUMENT_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
        if len(raw) > MAX_DOCUMENT_BYTES:
            raise FleetError("CMUX semantic result exceeds size ceiling")
        after = os.fstat(descriptor)
        if (
            info.st_dev != after.st_dev
            or info.st_ino != after.st_ino
            or info.st_uid != after.st_uid
            or info.st_gid != after.st_gid
            or info.st_mode != after.st_mode
            or info.st_nlink != after.st_nlink
            or info.st_size != after.st_size
            or info.st_mtime_ns != after.st_mtime_ns
            or info.st_ctime_ns != after.st_ctime_ns
            or len(raw) != after.st_size
        ):
            raise FleetError("CMUX semantic result changed while reading")
    finally:
        os.close(descriptor)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("CMUX semantic result is invalid JSON") from error
    canonical_raw = canonical(value)
    if raw != canonical_raw:
        raise FleetError("CMUX semantic result is not canonical")
    if not isinstance(value, dict):
        raise FleetError("CMUX semantic result is not an object")
    return value, "sha256:" + hashlib.sha256(raw).hexdigest()


def load_at(parent_fd: int, name: str) -> object:
    if "/" in name or name in {"", ".", ".."}:
        raise FleetError("fleet document name is invalid")
    try:
        descriptor = os.open(
            name,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
            dir_fd=parent_fd,
        )
    except OSError as error:
        raise FleetError("fleet document is unavailable") from error
    try:
        return _read_private_document(descriptor)
    finally:
        os.close(descriptor)


class FleetMutationLock:
    def __init__(self, enrollment_path: Path):
        if not enrollment_path.is_absolute() or enrollment_path.name != "enrollment.json":
            raise FleetError("mutable enrollment path must be explicit and canonical")
        self.enrollment_path = enrollment_path
        self.parent_path = enrollment_path.parent
        try:
            resolved_parent = self.parent_path.resolve(strict=True)
        except OSError as error:
            raise FleetError("fleet state directory is unavailable") from error
        if resolved_parent != self.parent_path:
            raise FleetError("fleet state directory is not canonical")
        try:
            self.parent_fd = os.open(
                self.parent_path,
                os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW,
            )
        except OSError as error:
            raise FleetError("fleet state directory is unavailable") from error
        try:
            info = os.fstat(self.parent_fd)
            if (
                not stat.S_ISDIR(info.st_mode)
                or info.st_uid != os.geteuid()
                or stat.S_IMODE(info.st_mode) & 0o077
            ):
                raise FleetError("fleet state directory ownership or mode is unsafe")
            self.parent_identity = (info.st_dev, info.st_ino)
            self.lock_fd = os.open(
                ".mutation.lock",
                os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW,
                0o600,
                dir_fd=self.parent_fd,
            )
            lock_info = os.fstat(self.lock_fd)
            if (
                not stat.S_ISREG(lock_info.st_mode)
                or lock_info.st_uid != os.geteuid()
                or lock_info.st_nlink != 1
                or stat.S_IMODE(lock_info.st_mode) != 0o600
                or lock_info.st_size != 0
            ):
                raise FleetError("fleet mutation lock is unsafe")
            try:
                fcntl.flock(self.lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise FleetError("fleet state mutation is busy") from error
            self.revalidate_parent_path()
        except Exception:
            if hasattr(self, "lock_fd"):
                os.close(self.lock_fd)
            os.close(self.parent_fd)
            raise

    def revalidate_parent_path(self) -> None:
        try:
            current = os.stat(self.parent_path, follow_symlinks=False)
        except OSError as error:
            raise FleetError("fleet state directory identity changed") from error
        held = os.fstat(self.parent_fd)
        if (
            not stat.S_ISDIR(current.st_mode)
            or current.st_uid != os.geteuid()
            or stat.S_IMODE(current.st_mode) & 0o077
            or (current.st_dev, current.st_ino) != self.parent_identity
            or (held.st_dev, held.st_ino) != self.parent_identity
        ):
            raise FleetError("fleet state directory identity changed")

    def load_enrollment(self) -> object:
        self.revalidate_parent_path()
        return load_at(self.parent_fd, self.enrollment_path.name)

    def __enter__(self) -> "FleetMutationLock":
        return self

    def __exit__(self, exc_type, exc, traceback) -> None:
        del exc_type, exc, traceback
        fcntl.flock(self.lock_fd, fcntl.LOCK_UN)
        os.close(self.lock_fd)
        os.close(self.parent_fd)


def durable_replace_enrollment(
    expected: object,
    replacement: object,
    mutation: FleetMutationLock,
) -> None:
    raw = canonical(replacement)
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise FleetError("fleet document exceeds size ceiling")
    if canonical(mutation.load_enrollment()) != canonical(expected):
        raise FleetError("fleet enrollment changed before mutation")

    temporary = None
    descriptor = None
    try:
        for attempt in range(32):
            candidate = f".enrollment.next.{os.getpid()}.{attempt}"
            try:
                descriptor = os.open(
                    candidate,
                    os.O_WRONLY
                    | os.O_CREAT
                    | os.O_EXCL
                    | os.O_CLOEXEC
                    | os.O_NOFOLLOW,
                    0o600,
                    dir_fd=mutation.parent_fd,
                )
                temporary = candidate
                break
            except FileExistsError:
                continue
        if descriptor is None or temporary is None:
            raise FleetError("fleet enrollment staging namespace is exhausted")
        with os.fdopen(descriptor, "wb", closefd=True) as stream:
            descriptor = None
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())

        if canonical(mutation.load_enrollment()) != canonical(expected):
            raise FleetError("fleet enrollment changed before publication")
        mutation.revalidate_parent_path()
        os.replace(
            temporary,
            mutation.enrollment_path.name,
            src_dir_fd=mutation.parent_fd,
            dst_dir_fd=mutation.parent_fd,
        )
        temporary = None
        os.fsync(mutation.parent_fd)
        mutation.revalidate_parent_path()
    finally:
        if descriptor is not None:
            os.close(descriptor)
        if temporary is not None:
            try:
                os.unlink(temporary, dir_fd=mutation.parent_fd)
            except FileNotFoundError:
                pass

    if canonical(mutation.load_enrollment()) != raw:
        raise FleetError("published fleet enrollment did not revalidate")


def acceptance_child_environment(temporary_root: Path) -> dict[str, str]:
    temporary_root.mkdir(parents=True, exist_ok=True)
    temporary_root.chmod(0o700)
    environment = {
        "LC_ALL": "C",
        "LANG": "C",
        "TMPDIR": str(temporary_root),
        "PATH": os.environ.get(
            "PATH",
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        ),
    }
    for name in ACCEPTANCE_CHILD_ENV_KEYS:
        if name == "PATH":
            continue
        value = os.environ.get(name)
        if value:
            environment[name] = value
    return environment


def _bounded_tail(path: Path, ceiling: int = 4096) -> str:
    try:
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - ceiling))
            return stream.read(ceiling).decode("utf-8", errors="replace").strip()
    except OSError:
        return ""


def _git_oid(cmux_root: Path, expression: str) -> str:
    environment = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith("GIT_")
    }
    environment["LC_ALL"] = "C"
    try:
        completed = subprocess.run(
            ["/usr/bin/git", "-C", str(cmux_root), "rev-parse", expression],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=environment,
            text=True,
            check=False,
            timeout=15,
        )
    except subprocess.TimeoutExpired as error:
        raise FleetError("CMUX source identity probe timed out") from error
    value = completed.stdout.strip()
    if completed.returncode != 0 or COMMIT_RE.fullmatch(value) is None:
        raise FleetError("CMUX source identity is unavailable")
    return value


def _decode_bootstrap_output(raw: bytes) -> object:
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise FleetError("post-acceptance bootstrap exceeds size ceiling")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("post-acceptance bootstrap is invalid JSON") from error
    if raw != canonical(value):
        raise FleetError("post-acceptance bootstrap is not canonical")
    return value


def accept_local(
    enrollment_path: Path,
    cmux_root_value: Path,
    glaeda: Path,
    role: str,
    cache_root_value: Path | None = None,
    min_free_gib: int | None = None,
) -> dict[str, Any]:
    enrollment = validate_enrollment(load(enrollment_path))
    if enrollment["state"] != "enrolling":
        raise FleetError("local acceptance requires an enrolling node")
    if role not in enrollment["allowedExecutionRoles"]:
        raise FleetError("local acceptance role is outside the enrollment allowlist")

    cmux_root = cmux_root_value.resolve(strict=True)
    runner = cmux_root / CMUX_PROFILE_RUNNER
    if not runner.is_file() or runner.is_symlink():
        raise FleetError("CMUX workload profile runner is unavailable")
    glaeda = glaeda.resolve(strict=True)
    if not glaeda.is_file() or not os.access(glaeda, os.X_OK):
        raise FleetError("Glaeda executable is unavailable")

    family = enrollment["os"]["family"]
    cache_root = (
        cache_root_value.resolve(strict=True)
        if cache_root_value is not None
        else None
    )
    if family == "macos" and cache_root is None:
        raise FleetError("macOS local acceptance requires --cache-root")
    if min_free_gib is not None and min_free_gib <= 0:
        raise FleetError("minimum free disk must be positive")

    source_commit = _git_oid(cmux_root, "HEAD^{commit}")
    source_tree = _git_oid(cmux_root, "HEAD^{tree}")
    profile = enrollment["roleProfiles"][role]
    fleet_contract_before = fleet_contract_generation()

    fleet_root = enrollment_path.resolve(strict=True).parent
    root_info = fleet_root.stat(follow_symlinks=False)
    if (
        not stat.S_ISDIR(root_info.st_mode)
        or root_info.st_uid != os.geteuid()
        or stat.S_IMODE(root_info.st_mode) & 0o077
    ):
        raise FleetError("fleet state directory ownership or mode is unsafe")

    bootstrap_script = Path(__file__).with_name("cmux_fleet_bootstrap.py")
    if not bootstrap_script.is_file() or bootstrap_script.is_symlink():
        raise FleetError("fleet bootstrap implementation is unavailable")

    with tempfile.TemporaryDirectory(
        prefix=".acceptance-run.",
        dir=fleet_root,
    ) as raw_state:
        state_root = Path(raw_state).resolve(strict=True)
        state_root.chmod(0o700)
        result_path = state_root / "result.json"
        log_path = state_root / "cmux-runner.log"
        child_environment = acceptance_child_environment(state_root / "tmp")
        log_fd = os.open(
            log_path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
            0o600,
        )
        try:
            with os.fdopen(log_fd, "wb", closefd=True) as log:
                log_fd = -1
                completed = subprocess.run(
                    [
                        sys.executable,
                        "-I",
                        str(runner),
                        "run",
                        profile["id"],
                        "--generation",
                        str(profile["generation"]),
                        "--commit",
                        source_commit,
                        "--tree",
                        source_tree,
                        "--state-class",
                        "cold",
                        "--result",
                        str(result_path),
                    ],
                    cwd=cmux_root,
                    stdin=subprocess.DEVNULL,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    env=child_environment,
                    check=False,
                )
        finally:
            if log_fd >= 0:
                os.close(log_fd)

        if not result_path.is_file():
            detail = _bounded_tail(log_path)
            suffix = f": {detail}" if detail else ""
            raise FleetError(
                f"CMUX local acceptance produced no semantic result{suffix}"
            )
        cmux_result, cmux_result_sha256 = load_cmux_semantic_result(result_path)

        bootstrap_argv = [
            sys.executable,
            "-I",
            str(bootstrap_script),
            "--platform",
            family,
            "--cmux-root",
            str(cmux_root),
            "--glaeda",
            str(glaeda),
            "--hardware-class",
            enrollment["hardwareCapabilityClass"],
            "--role",
            role,
        ]
        if cache_root is not None:
            bootstrap_argv.extend(["--cache-root", str(cache_root)])
        if min_free_gib is not None:
            bootstrap_argv.extend(["--min-free-gib", str(min_free_gib)])
        try:
            bootstrap = subprocess.run(
                bootstrap_argv,
                cwd=Path(__file__).resolve().parents[1],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=child_environment,
                check=False,
                timeout=180,
            )
        except subprocess.TimeoutExpired as error:
            raise FleetError("post-acceptance bootstrap timed out") from error
        if bootstrap.returncode != 0:
            detail = bootstrap.stderr.decode("utf-8", errors="replace").strip()
            raise FleetError(
                "post-acceptance bootstrap refused"
                + (f": {detail[:1024]}" if detail else "")
            )
        post_bootstrap = _decode_bootstrap_output(bootstrap.stdout)
        if not isinstance(post_bootstrap, dict):
            raise FleetError("post-acceptance bootstrap is not an object")
        toolchain_generation = post_bootstrap.get("toolchainGeneration")
        sha256(toolchain_generation, "post-acceptance toolchain generation")
        if toolchain_generation not in enrollment["supportedToolchainGenerations"]:
            raise FleetError(
                "post-acceptance toolchain is outside enrollment allowlist"
            )

        if fleet_contract_generation() != fleet_contract_before:
            raise FleetError(
                "Glaeda fleet contract changed during local acceptance"
            )
        local_attempt = "sha256:" + hashlib.sha256(os.urandom(32)).hexdigest()
        receipt = finalize_acceptance(
            enrollment,
            role,
            toolchain_generation,
            cmux_result,
            post_bootstrap,
            cmux_result_sha256,
            execution_class=LOCAL_EXECUTION_CLASS,
            local_execution_attempt_sha256=local_attempt,
        )
        if completed.returncode == 0 and receipt["result"] != "accepted":
            raise FleetError("successful CMUX local run did not produce acceptance")
        if completed.returncode != 0 and receipt["result"] == "accepted":
            raise FleetError("failed CMUX local run produced acceptance")
        return receipt


def apply_transition(
    enrollment_path: Path,
    target: str,
    reason: str | None,
    acceptance_paths: list[Path],
) -> dict[str, Any]:
    with FleetMutationLock(enrollment_path) as mutation:
        current = validate_enrollment(mutation.load_enrollment())
        acceptances = [load(path) for path in acceptance_paths]
        replacement = transition(current, target, reason, acceptances)
        durable_replace_enrollment(
            current,
            replacement,
            mutation,
        )
        return replacement

def emit(value: object) -> None:
    sys.stdout.buffer.write(canonical(value))


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="command", required=True)
    e = sub.add_parser("enroll")
    e.add_argument("bootstrap", type=Path)
    e.add_argument("--node-id", required=True)
    e.add_argument("--scope", required=True)
    e.add_argument("--generation", required=True, type=int)
    v = sub.add_parser("validate")
    v.add_argument("enrollment", type=Path)
    f = sub.add_parser("fingerprint")
    f.add_argument("enrollment", type=Path)
    s = sub.add_parser("status")
    s.add_argument("enrollment", type=Path)
    s.add_argument("--acceptance", action="append", type=Path, default=[])
    t = sub.add_parser("transition")
    t.add_argument("enrollment", type=Path)
    t.add_argument("--to", required=True, choices=STATES)
    t.add_argument("--reason", choices=QUARANTINE_REASONS)
    t.add_argument("--acceptance", action="append", type=Path, default=[])
    ta = sub.add_parser("transition-apply")
    ta.add_argument("enrollment", type=Path)
    ta.add_argument("--to", required=True, choices=STATES)
    ta.add_argument("--reason", choices=QUARANTINE_REASONS)
    ta.add_argument("--acceptance", action="append", type=Path, default=[])
    al = sub.add_parser("accept-local")
    al.add_argument("enrollment", type=Path)
    al.add_argument("--cmux-root", type=Path, required=True)
    al.add_argument("--glaeda", type=Path, required=True)
    al.add_argument("--role", required=True, choices=sorted(ENROLLABLE_ROLES))
    al.add_argument("--cache-root", type=Path)
    al.add_argument("--min-free-gib", type=int)
    a = sub.add_parser("finalize-acceptance")
    a.add_argument("enrollment", type=Path)
    a.add_argument("cmux_result", type=Path)
    a.add_argument("post_bootstrap", type=Path)
    a.add_argument("--role", required=True, choices=sorted(ENROLLABLE_ROLES))
    a.add_argument("--toolchain-generation", required=True)
    return p


def main() -> int:
    try:
        args = parser().parse_args()
        if args.command == "enroll":
            emit(
                enrollment_from_bootstrap(
                    load(args.bootstrap),
                    node_id=args.node_id,
                    operator_fleet_scope=args.scope,
                    enrollment_generation=args.generation,
                )
            )
            return 0
        enrollment = load(args.enrollment)
        if args.command == "validate":
            emit(validate_enrollment(enrollment))
        elif args.command == "fingerprint":
            emit({"schema": ENROLLMENT_SCHEMA, "enrollmentSha256": digest(validate_enrollment(enrollment))})
        elif args.command == "status":
            emit(node_status(enrollment, [load(path) for path in args.acceptance]))
        elif args.command == "transition":
            emit(
                transition(
                    enrollment,
                    args.to,
                    args.reason,
                    [load(path) for path in args.acceptance],
                )
            )
        elif args.command == "transition-apply":
            emit(
                apply_transition(
                    args.enrollment,
                    args.to,
                    args.reason,
                    args.acceptance,
                )
            )
        elif args.command == "accept-local":
            receipt = accept_local(
                args.enrollment,
                args.cmux_root,
                args.glaeda,
                args.role,
                args.cache_root,
                args.min_free_gib,
            )
            emit(receipt)
            return 0 if receipt["result"] == "accepted" else 1
        elif args.command == "finalize-acceptance":
            cmux_result, cmux_result_sha256 = load_cmux_semantic_result(args.cmux_result)
            post_bootstrap = load(args.post_bootstrap)
            emit(
                finalize_acceptance(
                    enrollment,
                    args.role,
                    args.toolchain_generation,
                    cmux_result,
                    post_bootstrap,
                    cmux_result_sha256,
                )
            )
        else:
            raise FleetError("unsupported command")
        return 0
    except (OSError, FleetError) as error:
        print(json.dumps({"error": str(error)}, sort_keys=True, separators=(",", ":")), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
