#!/usr/bin/env python3
"""CMUX-owned fleet enrollment, role acceptance, and bounded status projection."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

ENROLLMENT_SCHEMA = "glaeda-cmux-fleet-enrollment/v1"
ACCEPTANCE_EVIDENCE_SCHEMA = "glaeda-cmux-fleet-acceptance-evidence/v1"
ACCEPTANCE_SCHEMA = "glaeda-cmux-fleet-acceptance/v1"
STATUS_SCHEMA = "glaeda-cmux-fleet-node-status/v1"
BOOTSTRAP_SCHEMA = "glaeda-cmux-fleet-bootstrap/v1"
MAX_DOCUMENT_BYTES = 64 * 1024
MAX_STATUS_BYTES = 16 * 1024
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\\Z")
COMMIT_RE = re.compile(r"[0-9a-f]{40}\\Z")
TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9._-]{0,79}\\Z")
NODE_RE = re.compile(r"cmux-[a-z0-9][a-z0-9-]{2,59}\\Z")
REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}/[A-Za-z0-9_.-]{1,100}\\Z")

ROLES = (
    "artifact_cache",
    "background_replay",
    "benchmark",
    "cmux_linux_agent",
    "cmux_linux_ci",
    "cmux_macos_native_build",
    "cmux_macos_test",
)
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
    "supportedToolchainGenerations", "allowedExecutionRoles", "operatorFleetScope",
    "enrollmentGeneration", "glaedaGeneration", "state", "quarantineReason",
}
ACCEPTANCE_EVIDENCE_KEYS = {
    "schema", "nodeId", "enrollmentGeneration", "role", "source",
    "toolchainGeneration", "glaedaGeneration", "workloadGeneration", "checks",
}
CHECK_KEYS = {"workload", "semanticVerifier", "artifact", "processSettlement"}


class FleetError(RuntimeError):
    """A closed fleet enrollment/status refusal."""


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest(value: object) -> str:
    return "sha256:" + hashlib.sha256(canonical(value)).hexdigest()


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
    for role in roles:
        required_os = ROLE_OS.get(role)
        if required_os is not None and required_os != os_doc["family"]:
            raise FleetError(f"role {role} is incompatible with OS family {os_doc['family']}")
    positive_int(doc["enrollmentGeneration"], "enrollment generation")
    sha256(doc["glaedaGeneration"], "Glaeda generation", optional=True)
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
        "allowedExecutionRoles": bootstrap_value.get("roles"),
        "operatorFleetScope": operator_fleet_scope,
        "enrollmentGeneration": enrollment_generation,
        "glaedaGeneration": bootstrap_value.get("glaedaGeneration"),
        "state": "enrolling",
        "quarantineReason": None,
    }
    return validate_enrollment(enrollment)


def validate_acceptance_evidence(value: object) -> dict[str, Any]:
    doc = exact_keys(value, ACCEPTANCE_EVIDENCE_KEYS, "acceptance evidence")
    if doc["schema"] != ACCEPTANCE_EVIDENCE_SCHEMA:
        raise FleetError("acceptance evidence schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise FleetError("acceptance nodeId is invalid")
    positive_int(doc["enrollmentGeneration"], "acceptance enrollment generation")
    if doc["role"] not in ROLES:
        raise FleetError("acceptance role is unsupported")
    source = exact_keys(doc["source"], {"repository", "commit"}, "source")
    if not isinstance(source["repository"], str) or REPOSITORY_RE.fullmatch(source["repository"]) is None:
        raise FleetError("source repository is invalid")
    if not isinstance(source["commit"], str) or COMMIT_RE.fullmatch(source["commit"]) is None:
        raise FleetError("source commit is invalid")
    sha256(doc["toolchainGeneration"], "acceptance toolchain generation")
    sha256(doc["glaedaGeneration"], "acceptance Glaeda generation", optional=True)
    sha256(doc["workloadGeneration"], "acceptance workload generation")
    checks = exact_keys(doc["checks"], CHECK_KEYS, "acceptance checks")
    if any(v not in {"pass", "fail"} for v in checks.values()):
        raise FleetError("acceptance checks must be pass or fail")
    return doc


ACCEPTANCE_RECEIPT_KEYS = {
    "schema", "nodeId", "enrollmentGeneration", "role", "source",
    "toolchainGeneration", "glaedaGeneration", "workloadGeneration", "checks",
    "result", "evidenceSha256",
}


def validate_acceptance_receipt(value: object) -> dict[str, Any]:
    doc = exact_keys(value, ACCEPTANCE_RECEIPT_KEYS, "acceptance receipt")
    if doc["schema"] != ACCEPTANCE_SCHEMA:
        raise FleetError("acceptance receipt schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise FleetError("acceptance receipt nodeId is invalid")
    positive_int(
        doc["enrollmentGeneration"],
        "acceptance receipt enrollment generation",
    )
    if doc["role"] not in ROLES:
        raise FleetError("acceptance receipt role is unsupported")
    source = exact_keys(
        doc["source"],
        {"repository", "commit"},
        "acceptance receipt source",
    )
    if (
        not isinstance(source["repository"], str)
        or REPOSITORY_RE.fullmatch(source["repository"]) is None
    ):
        raise FleetError("acceptance receipt source repository is invalid")
    if (
        not isinstance(source["commit"], str)
        or COMMIT_RE.fullmatch(source["commit"]) is None
    ):
        raise FleetError("acceptance receipt source commit is invalid")
    sha256(
        doc["toolchainGeneration"],
        "acceptance receipt toolchain generation",
    )
    sha256(
        doc["glaedaGeneration"],
        "acceptance receipt Glaeda generation",
        optional=True,
    )
    sha256(
        doc["workloadGeneration"],
        "acceptance receipt workload generation",
    )
    sha256(doc["evidenceSha256"], "acceptance evidence digest")
    checks = exact_keys(
        doc["checks"],
        CHECK_KEYS,
        "acceptance receipt checks",
    )
    if any(value not in {"pass", "fail"} for value in checks.values()):
        raise FleetError("acceptance receipt checks must be pass or fail")
    if doc["result"] not in {"accepted", "rejected"}:
        raise FleetError("acceptance receipt result is invalid")
    all_pass = all(value == "pass" for value in checks.values())
    if (doc["result"] == "accepted") != all_pass:
        raise FleetError("acceptance receipt result disagrees with checks")
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
    if receipt.get("toolchainGeneration") not in enrollment["supportedToolchainGenerations"]:
        return False, "acceptance_toolchain_stale"
    return True, "accepted"


def finalize_acceptance(enrollment_value: object, evidence_value: object) -> dict[str, Any]:
    enrollment = validate_enrollment(enrollment_value)
    evidence = validate_acceptance_evidence(evidence_value)
    if evidence["nodeId"] != enrollment["nodeId"]:
        raise FleetError("acceptance node identity differs from enrollment")
    if evidence["enrollmentGeneration"] != enrollment["enrollmentGeneration"]:
        raise FleetError("acceptance enrollment generation differs from current enrollment")
    if evidence["role"] not in enrollment["allowedExecutionRoles"]:
        raise FleetError("acceptance role is outside the enrollment allowlist")
    if evidence["toolchainGeneration"] not in enrollment["supportedToolchainGenerations"]:
        raise FleetError("acceptance toolchain generation is outside the enrollment allowlist")
    if evidence["glaedaGeneration"] != enrollment["glaedaGeneration"]:
        raise FleetError("acceptance Glaeda generation differs from current enrollment")
    result = "accepted" if all(v == "pass" for v in evidence["checks"].values()) else "rejected"
    receipt = {
        "schema": ACCEPTANCE_SCHEMA,
        "nodeId": evidence["nodeId"],
        "enrollmentGeneration": evidence["enrollmentGeneration"],
        "role": evidence["role"],
        "source": evidence["source"],
        "toolchainGeneration": evidence["toolchainGeneration"],
        "glaedaGeneration": evidence["glaedaGeneration"],
        "workloadGeneration": evidence["workloadGeneration"],
        "checks": evidence["checks"],
        "result": result,
        "evidenceSha256": digest(evidence),
    }
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
        "automaticRoutingEligible": any(r["eligible"] for r in role_status),
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


def transition(enrollment_value: object, target: str, reason: str | None) -> dict[str, Any]:
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
    return validate_enrollment(enrollment)


def load(path: Path) -> object:
    raw = path.read_bytes()
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise FleetError("fleet document exceeds size ceiling")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise FleetError("fleet document is invalid JSON") from error
    return value


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
    a = sub.add_parser("finalize-acceptance")
    a.add_argument("enrollment", type=Path)
    a.add_argument("evidence", type=Path)
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
            emit(transition(enrollment, args.to, args.reason))
        elif args.command == "finalize-acceptance":
            emit(finalize_acceptance(enrollment, load(args.evidence)))
        else:
            raise FleetError("unsupported command")
        return 0
    except (OSError, FleetError) as error:
        print(json.dumps({"error": str(error)}, sort_keys=True, separators=(",", ":")), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
