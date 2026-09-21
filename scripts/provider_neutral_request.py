#!/usr/bin/env python3
"""Provider-neutral semantic request/receipt contract for owner-local Glaeda work.

This module owns semantic identity and typed result projection only. It performs no Git, network,
filesystem, process, scheduler, credential, or host mutation. Transport adapters authenticate a
caller and compile into this contract; reviewed Glaeda adapters retain all physical authority.
"""
from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import re
from typing import NoReturn

import verify_focused_impl as focused


REQUEST_DOCUMENT_TYPE = "glaeda-semantic-request"
RECEIPT_DOCUMENT_TYPE = "glaeda-semantic-receipt"
CAPABILITIES_DOCUMENT_TYPE = "glaeda-semantic-capabilities"
REQUEST_SCHEMA_VERSION = 1
RECEIPT_SCHEMA_VERSION = 1
CAPABILITIES_SCHEMA_VERSION = 1
MAX_REQUEST_BYTES = 16 * 1024
MAX_RECEIPT_BYTES = 192 * 1024
MAX_REPO_QUERY_RESULT_BYTES = 128 * 1024
MAX_REFERENCE_BYTES = 128
MAX_REPO_QUERY_PATCH_BYTES = 8 * 1024

OP_CAPABILITIES = "capabilities"
OP_STATUS = "status"
OP_REPO_QUERY = "repo_query"
OP_VERIFY_NAMED = "verify_named"
OPERATIONS = {OP_CAPABILITIES, OP_STATUS, OP_REPO_QUERY, OP_VERIFY_NAMED}

REQUEST_ID_PATTERN = re.compile(r"^[a-z0-9][a-z0-9-]{7,63}$")
REPOSITORY_PATTERN = re.compile(r"^[a-z0-9_.-]+/[a-z0-9_.-]+$")
OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
PROFILE_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,159}$")
REASON_PATTERN = re.compile(r"^[a-z0-9][a-z0-9_]{0,63}$")

REQUEST_KEYS = {"document_type", "schema_version", "request_id", "operation", "source", "parameters"}
SOURCE_KEYS = {"repository", "commit", "tree"}
RECEIPT_KEYS = {
    "document_type",
    "schema_version",
    "request_id",
    "request_sha256",
    "operation",
    "source",
    "state",
    "resolved_operation",
    "result",
    "result_sha256",
    "refusal_code",
    "authority",
}
AUTHORITY = {
    "authorizes_execution": False,
    "authorizes_redispatch": False,
    "authorizes_host_selection": False,
    "authorizes_cleanup": False,
}
RECEIPT_STATES = {
    "planned",
    "waiting",
    "refused",
    "ambiguous",
    "succeeded",
    "failed",
    "timed_out",
    "cleanup_incomplete",
}


class ContractRefusal(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class SourceIdentity:
    repository: str
    commit: str
    tree: str


@dataclass(frozen=True)
class SemanticRequest:
    request_id: str
    operation: str
    source: SourceIdentity | None
    parameters: dict[str, object]


@dataclass(frozen=True)
class CompiledRequest:
    request: SemanticRequest
    request_sha256: str
    resolved_operation: dict[str, object]
    internal: focused.Request | None = None


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def parse_request_id(value: object) -> str:
    if not isinstance(value, str) or not REQUEST_ID_PATTERN.fullmatch(value):
        raise ContractRefusal("invalid_request", "semantic request identity is invalid")
    return value


def _source(value: object) -> SourceIdentity:
    if not isinstance(value, dict) or set(value) != SOURCE_KEYS:
        raise ContractRefusal("invalid_request", "semantic source identity is invalid")
    repository = value["repository"]
    commit = value["commit"]
    tree = value["tree"]
    if not isinstance(repository, str) or not REPOSITORY_PATTERN.fullmatch(repository):
        raise ContractRefusal("invalid_request", "semantic source repository is invalid")
    if (
        not isinstance(commit, str)
        or not OID_PATTERN.fullmatch(commit)
        or not isinstance(tree, str)
        or not OID_PATTERN.fullmatch(tree)
    ):
        raise ContractRefusal("invalid_request", "semantic source commit/tree is invalid")
    return SourceIdentity(repository, commit, tree)


def _source_document(source: SourceIdentity | None) -> dict[str, str] | None:
    if source is None:
        return None
    return {"repository": source.repository, "commit": source.commit, "tree": source.tree}


def _profile_id(value: object) -> str:
    if not isinstance(value, str) or not PROFILE_PATTERN.fullmatch(value):
        raise ContractRefusal("invalid_request", "verification profile identity is invalid")
    return value


def request_document(request: SemanticRequest) -> dict[str, object]:
    return {
        "document_type": REQUEST_DOCUMENT_TYPE,
        "schema_version": REQUEST_SCHEMA_VERSION,
        "request_id": request.request_id,
        "operation": request.operation,
        "source": _source_document(request.source),
        "parameters": dict(request.parameters),
    }


def request_sha256(request: SemanticRequest) -> str:
    return sha256(canonical_bytes(request_document(request)))


def decode_request(raw: bytes) -> SemanticRequest:
    if len(raw) > MAX_REQUEST_BYTES:
        raise ContractRefusal("invalid_request", "semantic request exceeds its fixed ceiling")
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise ContractRefusal("invalid_request", "semantic request is not valid JSON") from error
    if (
        not isinstance(value, dict)
        or set(value) != REQUEST_KEYS
        or canonical_bytes(value) + b"\n" != raw
    ):
        raise ContractRefusal("invalid_request", "semantic request is not canonical closed-schema JSON")
    if (
        value["document_type"] != REQUEST_DOCUMENT_TYPE
        or type(value["schema_version"]) is not int
        or value["schema_version"] != REQUEST_SCHEMA_VERSION
    ):
        raise ContractRefusal("unsupported_schema", "semantic request schema is unsupported")
    request_id = parse_request_id(value["request_id"])
    operation = value["operation"]
    if not isinstance(operation, str) or operation not in OPERATIONS:
        raise ContractRefusal("unsupported_operation", "semantic operation is unsupported")
    parameters = value["parameters"]
    if not isinstance(parameters, dict):
        raise ContractRefusal("invalid_request", "semantic parameters must be an object")

    if operation in {OP_CAPABILITIES, OP_STATUS}:
        if value["source"] is not None or parameters:
            raise ContractRefusal("invalid_request", "read-only node operation carries unexpected source or parameters")
        return SemanticRequest(request_id, operation, None, {})

    source = _source(value["source"])
    if operation == OP_VERIFY_NAMED:
        if set(parameters) != {"profile_id"}:
            raise ContractRefusal("invalid_request", "verify_named parameters are invalid")
        return SemanticRequest(
            request_id,
            operation,
            source,
            {"profile_id": _profile_id(parameters["profile_id"])},
        )

    if set(parameters) != {"base_commit", "max_patch_bytes"}:
        raise ContractRefusal("invalid_request", "repo_query parameters are invalid")
    base_commit = parameters["base_commit"]
    patch_bytes = parameters["max_patch_bytes"]
    if not isinstance(base_commit, str) or not OID_PATTERN.fullmatch(base_commit):
        raise ContractRefusal("invalid_request", "repo_query base commit is invalid")
    if (
        isinstance(patch_bytes, bool)
        or not isinstance(patch_bytes, int)
        or not 0 <= patch_bytes <= MAX_REPO_QUERY_PATCH_BYTES
    ):
        raise ContractRefusal("invalid_request", "repo_query patch ceiling is invalid")
    return SemanticRequest(
        request_id,
        operation,
        source,
        {"base_commit": base_commit, "max_patch_bytes": patch_bytes},
    )


def make_verify_named_request(
    request_id: str,
    repository: str,
    commit: str,
    tree: str,
    profile_id: str,
) -> SemanticRequest:
    document = {
        "document_type": REQUEST_DOCUMENT_TYPE,
        "schema_version": REQUEST_SCHEMA_VERSION,
        "request_id": request_id,
        "operation": OP_VERIFY_NAMED,
        "source": {"repository": repository, "commit": commit, "tree": tree},
        "parameters": {"profile_id": profile_id},
    }
    return decode_request(canonical_bytes(document) + b"\n")


def _verify_fingerprint(request: SemanticRequest, generation: str) -> str:
    assert request.source is not None
    binding = {
        "domain": "glaeda-semantic-verify-named-binding-v1",
        "request_id": request.request_id,
        "operation": OP_VERIFY_NAMED,
        "source": _source_document(request.source),
        "resolved_profile": {
            "id": focused.FOCUSED_PROFILE.profile_id,
            "generation": generation,
            "capability_class": focused.EXECUTION_IDENTITY_CLASS,
        },
    }
    return sha256(canonical_bytes(binding))


def _contract_spec() -> dict[str, object]:
    verify_generation = focused.profile_generation(focused.FOCUSED_PROFILE)
    return {
        "document_type": "glaeda-semantic-contract",
        "schema_version": 1,
        "operations": [
            {"kind": OP_CAPABILITIES, "authority": "observation_only"},
            {
                "kind": OP_STATUS,
                "authority": "observation_only",
                "observation_contract": "glaeda-owned-admission-observation/v1",
            },
            {
                "kind": OP_REPO_QUERY,
                "authority": "observation_only",
                "profile_id": "repo-query/v1",
                "source_class": "resident_exact",
                "max_patch_bytes": MAX_REPO_QUERY_PATCH_BYTES,
            },
            {
                "kind": OP_VERIFY_NAMED,
                "authority": "bounded_execution_request",
                "profiles": [
                    {
                        "id": focused.FOCUSED_PROFILE.profile_id,
                        "generation": verify_generation,
                        "capability_class": focused.EXECUTION_IDENTITY_CLASS,
                    }
                ],
            },
        ],
        "command_argv": {
            "available": False,
            "reason": "no_reviewed_glaeda_command_profile",
        },
    }


def contract_generation() -> str:
    return sha256(canonical_bytes(_contract_spec()))


def capabilities_payload() -> dict[str, object]:
    return {
        "document_type": CAPABILITIES_DOCUMENT_TYPE,
        "schema_version": CAPABILITIES_SCHEMA_VERSION,
        "contract_generation": contract_generation(),
        **{key: value for key, value in _contract_spec().items() if key not in {"document_type", "schema_version"}},
        "authority": "observation_only",
    }


def compile_request(request: SemanticRequest) -> CompiledRequest:
    digest = request_sha256(request)
    if request.operation == OP_CAPABILITIES:
        return CompiledRequest(
            request,
            digest,
            {"kind": OP_CAPABILITIES, "contract_generation": contract_generation()},
        )
    if request.operation == OP_STATUS:
        return CompiledRequest(
            request,
            digest,
            {
                "kind": OP_STATUS,
                "observation_contract": "glaeda-owned-admission-observation/v1",
                "authority": "observation_only",
            },
        )
    if request.operation == OP_REPO_QUERY:
        assert request.source is not None
        return CompiledRequest(
            request,
            digest,
            {
                "kind": OP_REPO_QUERY,
                "profile_id": "repo-query/v1",
                "source_class": "resident_exact",
                "base_commit": request.parameters["base_commit"],
                "max_patch_bytes": request.parameters["max_patch_bytes"],
                "result_ceiling_bytes": MAX_REPO_QUERY_RESULT_BYTES,
                "authority": "observation_only",
            },
        )

    profile_id = request.parameters["profile_id"]
    if profile_id != focused.FOCUSED_PROFILE.profile_id:
        raise ContractRefusal("unsupported_profile", "verification profile is not admitted by semantic v1")
    assert request.source is not None
    generation = focused.profile_generation(focused.FOCUSED_PROFILE)
    if not SHA256_PATTERN.fullmatch(generation):
        raise ContractRefusal("internal_contract_error", "resolved profile generation is invalid")
    internal = focused.Request(
        repository=request.source.repository,
        commit=request.source.commit,
        tree=request.source.tree,
        profile_generation=generation,
        command_fingerprint=_verify_fingerprint(request, generation),
        profile=focused.FOCUSED_PROFILE,
    )
    return CompiledRequest(
        request,
        digest,
        {
            "kind": OP_VERIFY_NAMED,
            "profile_id": focused.FOCUSED_PROFILE.profile_id,
            "profile_generation": generation,
            "capability_class": focused.EXECUTION_IDENTITY_CLASS,
            "resource_class": focused.FOCUSED_PROFILE.resource_class,
            "network_class": "none",
            "environment_class": "minimal",
            "deadline_seconds": focused.FOCUSED_PROFILE.deadline_seconds,
            "output_ceiling_bytes": focused.MAX_SOURCE_OUTPUT_BYTES,
        },
        internal,
    )


def _result_digest(result: object | None) -> str | None:
    if result is None:
        return None
    return sha256(canonical_bytes(result) + b"\n")


def _receipt(
    request: SemanticRequest,
    digest: str,
    *,
    state: str,
    resolved_operation: dict[str, object] | None,
    result: object | None = None,
    refusal_code: str | None = None,
) -> dict[str, object]:
    if state not in RECEIPT_STATES:
        raise ContractRefusal("internal_contract_error", "semantic receipt state is invalid")
    value = {
        "document_type": RECEIPT_DOCUMENT_TYPE,
        "schema_version": RECEIPT_SCHEMA_VERSION,
        "request_id": request.request_id,
        "request_sha256": digest,
        "operation": request.operation,
        "source": _source_document(request.source),
        "state": state,
        "resolved_operation": resolved_operation,
        "result": result,
        "result_sha256": _result_digest(result),
        "refusal_code": refusal_code,
        "authority": dict(AUTHORITY),
    }
    if len(canonical_bytes(value) + b"\n") > MAX_RECEIPT_BYTES:
        raise ContractRefusal("internal_contract_error", "semantic receipt exceeds its fixed ceiling")
    return value


def planned_receipt(compiled: CompiledRequest) -> dict[str, object]:
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="planned",
        resolved_operation=compiled.resolved_operation,
    )


def refused_receipt(request: SemanticRequest, refusal: ContractRefusal) -> dict[str, object]:
    return _receipt(
        request,
        request_sha256(request),
        state="refused",
        resolved_operation=None,
        refusal_code=refusal.code,
    )


def waiting_receipt(compiled: CompiledRequest, reason: str) -> dict[str, object]:
    if not REASON_PATTERN.fullmatch(reason):
        raise ContractRefusal("internal_contract_error", "semantic waiting reason is invalid")
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="waiting",
        resolved_operation=compiled.resolved_operation,
        result={"reason": reason},
    )


def capabilities_receipt(compiled: CompiledRequest) -> dict[str, object]:
    if compiled.request.operation != OP_CAPABILITIES:
        raise ContractRefusal("internal_contract_error", "request is not capabilities")
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="succeeded",
        resolved_operation=compiled.resolved_operation,
        result=capabilities_payload(),
    )


def status_receipt(compiled: CompiledRequest, observation: dict[str, object]) -> dict[str, object]:
    if compiled.request.operation != OP_STATUS:
        raise ContractRefusal("internal_contract_error", "request is not status")
    expected = {
        "document_type",
        "schema_version",
        "outcome",
        "reason",
        "grants_authority",
        "authorizes_execution",
        "authorizes_redispatch",
    }
    if (
        not isinstance(observation, dict)
        or set(observation) != expected
        or observation.get("document_type") != "glaeda-owned-admission-observation"
        or type(observation.get("schema_version")) is not int
        or observation.get("schema_version") != 1
        or observation.get("outcome") not in {"ready", "wait", "refused"}
        or not isinstance(observation.get("reason"), str)
        or not REASON_PATTERN.fullmatch(observation["reason"])
        or observation.get("grants_authority") is not False
        or observation.get("authorizes_execution") is not False
        or observation.get("authorizes_redispatch") is not False
    ):
        raise ContractRefusal("invalid_status_observation", "status observation is outside the reviewed contract")
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="succeeded",
        resolved_operation=compiled.resolved_operation,
        result=observation,
    )


def repo_query_receipt(compiled: CompiledRequest, report: dict[str, object]) -> dict[str, object]:
    if compiled.request.operation != OP_REPO_QUERY or compiled.request.source is None:
        raise ContractRefusal("internal_contract_error", "request is not repo_query")
    raw = canonical_bytes(report) + b"\n"
    source = compiled.request.source
    if (
        len(raw) > MAX_REPO_QUERY_RESULT_BYTES
        or not isinstance(report, dict)
        or report.get("document_type") != "glaeda-resident-repo-query"
        or type(report.get("schema_version")) is not int
        or report.get("schema_version") != 1
        or report.get("profile_id") != "repo-query/v1"
        or report.get("authority") != "observation_only"
        or not isinstance(report.get("profile_generation"), str)
        or not SHA256_PATTERN.fullmatch(report["profile_generation"])
        or report.get("repository") != source.repository
        or report.get("requested_base") != compiled.request.parameters["base_commit"]
        or report.get("head") != source.commit
        or report.get("head_tree") != source.tree
    ):
        raise ContractRefusal("invalid_repo_query_result", "repo query result does not match the semantic request")
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="succeeded",
        resolved_operation=compiled.resolved_operation,
        result=report,
    )


def ambiguous_receipt(compiled: CompiledRequest) -> dict[str, object]:
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state="ambiguous",
        resolved_operation=compiled.resolved_operation,
        refusal_code="ambiguous_execution",
    )


def terminal_verify_receipt(
    compiled: CompiledRequest, workload_receipt: dict[str, object]
) -> dict[str, object]:
    if compiled.request.operation != OP_VERIFY_NAMED or compiled.internal is None:
        raise ContractRefusal("internal_contract_error", "request is not verify_named")
    if not focused.valid_terminal_receipt(workload_receipt, compiled.internal):
        raise ContractRefusal("invalid_workload_receipt", "verification receipt does not match the semantic request")
    result = workload_receipt["result"]
    terminal = result["terminal_class"]
    projection = {
        "workload_receipt_sha256": sha256(canonical_bytes(workload_receipt) + b"\n"),
        "terminal_class": terminal,
        "started_at_unix_millis": result["started_at_unix_millis"],
        "settled_at_unix_millis": result["settled_at_unix_millis"],
        "output_bytes": result["output_bytes"],
        "output_sha256": result["output_sha256"],
        "process_tree_settled": result["process_tree_settled"],
        "task_cleanup_complete": result["task_cleanup_complete"],
    }
    return _receipt(
        compiled.request,
        compiled.request_sha256,
        state=terminal,
        resolved_operation=compiled.resolved_operation,
        result=projection,
    )


def inspect_receipt(raw: bytes) -> dict[str, object]:
    if len(raw) > MAX_RECEIPT_BYTES:
        raise ContractRefusal("invalid_receipt", "semantic receipt exceeds its fixed ceiling")
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise ContractRefusal("invalid_receipt", "semantic receipt is not valid JSON") from error
    if (
        not isinstance(value, dict)
        or set(value) != RECEIPT_KEYS
        or canonical_bytes(value) + b"\n" != raw
        or value.get("document_type") != RECEIPT_DOCUMENT_TYPE
        or type(value.get("schema_version")) is not int
        or value.get("schema_version") != RECEIPT_SCHEMA_VERSION
        or value.get("authority") != AUTHORITY
        or value.get("state") not in RECEIPT_STATES
    ):
        raise ContractRefusal("invalid_receipt", "semantic receipt is outside the closed schema")
    parse_request_id(value.get("request_id"))
    digest = value.get("request_sha256")
    if not isinstance(digest, str) or not SHA256_PATTERN.fullmatch(digest):
        raise ContractRefusal("invalid_receipt", "semantic receipt request digest is invalid")
    result = value.get("result")
    expected_result_digest = _result_digest(result)
    if value.get("result_sha256") != expected_result_digest:
        raise ContractRefusal("invalid_receipt", "semantic receipt result digest is invalid")
    refusal = value.get("refusal_code")
    if refusal is not None and (
        not isinstance(refusal, str) or not REASON_PATTERN.fullmatch(refusal)
    ):
        raise ContractRefusal("invalid_receipt", "semantic receipt refusal code is invalid")
    return value


def validate_replay(
    existing_receipt: dict[str, object], request: SemanticRequest
) -> dict[str, object]:
    validated = inspect_receipt(canonical_bytes(existing_receipt) + b"\n")
    if validated["request_id"] != request.request_id:
        raise ContractRefusal("request_mismatch", "semantic receipt belongs to another request identity")
    if validated["request_sha256"] != request_sha256(request):
        raise ContractRefusal(
            "request_conflict",
            "semantic request identity was reused with different semantics",
        )
    if (
        validated["operation"] != request.operation
        or validated["source"] != _source_document(request.source)
    ):
        raise ContractRefusal("invalid_receipt", "semantic receipt request binding is inconsistent")
    return existing_receipt
