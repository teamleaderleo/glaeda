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
    "supportedToolchainGenerations", "roleWorkloadGenerations",
    "allowedExecutionRoles", "operatorFleetScope",
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
    workload_generations = doc["roleWorkloadGenerations"]
    if (
        not isinstance(workload_generations, dict)
        or set(workload_generations) != set(roles)
        or any(
            not isinstance(value, str) or SHA256_RE.fullmatch(value) is None
            for value in workload_generations.values()
        )
    ):
        raise FleetError("role workload generations must exactly cover allowed roles")
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
        "roleWorkloadGenerations": bootstrap_value.get("roleWorkloadGenerations"),
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
    if doc["role"] not in ENROLLABLE_ROLES:
        raise FleetError("acceptance role lacks a reviewed v1 workload")
    source = exact_keys(doc["source"], {"repository", "commit"}, "source")
    if not isinstance(source["repository"], str) or REPOSITORY_RE.fullmatch(source["repository"]) is None:
        raise FleetError("source repository is invalid")
    if not isinstance(source["commit"], str) or COMMIT_RE.fullmatch(source["commit"]) is None:
        raise FleetError("source commit is invalid")
    sha256(doc["toolchainGeneration"], "acceptance toolchain generation")
    sha256(doc["glaedaGeneration"], "acceptance Glaeda generation")
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
    if doc["role"] not in ENROLLABLE_ROLES:
        raise FleetError("acceptance receipt role lacks a reviewed v1 workload")
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
    if receipt.get("workloadGeneration") != enrollment["roleWorkloadGenerations"].get(role):
        return False, "acceptance_workload_stale"
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
    if evidence["workloadGeneration"] != enrollment["roleWorkloadGenerations"].get(evidence["role"]):
        raise FleetError("acceptance workload generation differs from current enrollment")
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
            "roleWorkloadGenerations": enrollment["roleWorkloadGenerations"],
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
