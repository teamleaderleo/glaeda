#!/usr/bin/env python3
"""Publish and consume bounded advisory resident-node snapshots through GitHub."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import stat
import tempfile
from typing import Any


NODE_DOCUMENT = "glaeda-github-resident-node-snapshot"
FLEET_DOCUMENT = "glaeda-github-resident-fleet"
TRUST_DOCUMENT = "glaeda-github-resident-trust"
VIEW_DOCUMENT = "glaeda-github-resident-view"
PUBLICATION_DOCUMENT = "glaeda-github-resident-publication-receipt"
SCHEMA_VERSION = 1
SIGNING_NAMESPACE = "glaeda-github-resident-node-snapshot-v1"
STATUS_BRANCH = "glaeda-status/v1"
STATUS_FILE = "resident-nodes.json"
MAX_NODE_BYTES = 16 * 1024
MAX_FLEET_BYTES = 128 * 1024
MAX_NODES = 8
MAX_PROJECTS = 8
MAX_REQUESTS = 16
MAX_PROFILES = 8
MIN_USEFUL_AGE_SECONDS = 60
MAX_USEFUL_AGE_SECONDS = 600
DEFAULT_USEFUL_AGE_SECONDS = 300
DEFAULT_REFRESH_INTERVAL_SECONDS = 240
MAX_CLOCK_SKEW_SECONDS = 30
MAX_GIT_OUTPUT_BYTES = 64 * 1024
OID_RE = re.compile(r"[0-9a-f]{40}\Z")
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
NODE_RE = re.compile(r"node-[0-9a-f]{16}\Z")
KEY_RE = re.compile(r"key-[0-9a-f]{16}\Z")
REQUEST_RE = re.compile(r"req-[0-9a-f]{16,64}\Z")
REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]{1,64}/[A-Za-z0-9_.-]{1,100}\Z")
PROFILE_RE = re.compile(r"[a-z0-9][a-z0-9_.-]{0,62}/v[1-9][0-9]{0,5}\Z")
VERIFICATION_PROFILE_RE = re.compile(r"[a-z0-9][a-z0-9_.-]{0,79}(?:/v[1-9][0-9]{0,5})?\Z")
SSH_PUBLIC_KEY_RE = re.compile(r"ssh-ed25519 [A-Za-z0-9+/=]{40,200}(?: [^\r\n]{0,80})?\Z")
HEAT_CLASSES = {"cold", "warm", "resident_hot", "revalidation_required"}
BUILD_STATE_CLASSES = {"cold", "warm", "resident_generation", "revalidation_required", "unknown"}
REQUEST_STATES = {"queued", "preparing", "running", "terminal", "superseded"}
DURATION_CLASSES = {"under_10s", "10s_to_1m", "1m_to_5m", "over_5m", "unknown"}
AVAILABILITY_CLASSES = {"available", "held", "draining", "unknown"}
PRESSURE_CLASSES = {"low", "high", "unknown"}
CAPACITY_CLASSES = {"available", "reserved", "insufficient", "unknown"}


class SnapshotError(RuntimeError):
    """A bounded resident-snapshot refusal."""


class PublicationSuppressed(SnapshotError):
    """The snapshot is valid but no GitHub write is useful yet."""


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")


def digest(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def exact_object(value: object, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise SnapshotError(f"{label} must be an object")
    return value


def exact_list(value: object, label: str, maximum: int) -> list[Any]:
    if not isinstance(value, list) or len(value) > maximum:
        raise SnapshotError(f"{label} must be a bounded array")
    return value


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise SnapshotError(f"{label} has unsupported fields")


def integer(value: object, label: str, minimum: int, maximum: int) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or not minimum <= value <= maximum:
        raise SnapshotError(f"{label} is invalid")
    return value


def bounded_string(value: object, pattern: re.Pattern[str], label: str) -> str:
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise SnapshotError(f"{label} is invalid")
    return value


def parse_time(value: object, label: str) -> dt.datetime:
    if not isinstance(value, str) or len(value) > 32 or not value.endswith("Z"):
        raise SnapshotError(f"{label} is invalid")
    try:
        parsed = dt.datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as error:
        raise SnapshotError(f"{label} is invalid") from error
    if parsed.utcoffset() != dt.timedelta(0):
        raise SnapshotError(f"{label} must be UTC")
    return parsed


def format_time(value: dt.datetime) -> str:
    if value.tzinfo is None or value.utcoffset() != dt.timedelta(0):
        raise SnapshotError("time must be UTC")
    return value.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def load_json(path: Path, maximum_bytes: int = MAX_FLEET_BYTES) -> object:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise SnapshotError("bounded JSON input is unavailable") from error
    if len(raw) > maximum_bytes:
        raise SnapshotError("bounded JSON input is oversized")
    try:
        return json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SnapshotError("bounded JSON input is invalid") from error


def validate_trust(value: object) -> dict[str, Any]:
    trust = exact_object(value, "trust document")
    exact_keys(trust, {"document_type", "schema_version", "repositories", "nodes"}, "trust document")
    if trust["document_type"] != TRUST_DOCUMENT or trust["schema_version"] != SCHEMA_VERSION:
        raise SnapshotError("trust document version is unsupported")
    repositories = exact_list(trust["repositories"], "trusted repositories", MAX_PROJECTS)
    if not repositories or any(not isinstance(item, str) or REPOSITORY_RE.fullmatch(item) is None for item in repositories):
        raise SnapshotError("trusted repositories are invalid")
    if len(set(repositories)) != len(repositories):
        raise SnapshotError("trusted repositories contain duplicates")
    nodes = exact_list(trust["nodes"], "trusted nodes", MAX_NODES)
    seen_nodes: set[str] = set()
    seen_keys: set[str] = set()
    for entry_raw in nodes:
        entry = exact_object(entry_raw, "trusted node")
        exact_keys(entry, {"id", "key_id", "ssh_public_key", "os_class", "architecture_class"}, "trusted node")
        node_id = bounded_string(entry["id"], NODE_RE, "trusted node id")
        key_id = bounded_string(entry["key_id"], KEY_RE, "trusted key id")
        if node_id in seen_nodes or key_id in seen_keys:
            raise SnapshotError("trusted node identities must be unique")
        seen_nodes.add(node_id)
        seen_keys.add(key_id)
        if not isinstance(entry["ssh_public_key"], str) or SSH_PUBLIC_KEY_RE.fullmatch(entry["ssh_public_key"]) is None:
            raise SnapshotError("trusted SSH public key is invalid")
        if entry["os_class"] not in {"linux", "macos"}:
            raise SnapshotError("trusted OS class is invalid")
        if entry["architecture_class"] not in {"x86_64", "arm64"}:
            raise SnapshotError("trusted architecture class is invalid")
    return trust


def trust_node(trust: dict[str, Any], node_id: str) -> dict[str, Any]:
    for entry in trust["nodes"]:
        if entry["id"] == node_id:
            return entry
    raise SnapshotError("node is absent from the reviewed trust set")


def admission_projection(value: object) -> dict[str, object]:
    admission = exact_object(value, "admission observation")
    exact_keys(
        admission,
        {"document_type", "schema_version", "outcome", "reason", "grants_authority", "authorizes_execution", "authorizes_redispatch"},
        "admission observation",
    )
    if admission["document_type"] != "glaeda-owned-admission-observation" or admission["schema_version"] != 1:
        raise SnapshotError("admission observation version is unsupported")
    if admission["grants_authority"] is not False or admission["authorizes_execution"] is not False or admission["authorizes_redispatch"] is not False:
        raise SnapshotError("admission observation carries authority")
    pair = (admission["outcome"], admission["reason"])
    mapping: dict[tuple[object, object], tuple[str, str, str, int]] = {
        ("ready", "compatible"): ("available", "low", "available", 0),
        ("wait", "reserved"): ("available", "unknown", "reserved", 1),
        ("wait", "node_draining"): ("draining", "unknown", "unknown", 0),
        ("wait", "pressure_high"): ("available", "high", "insufficient", 0),
        ("wait", "capacity_unavailable"): ("available", "unknown", "insufficient", 0),
        ("wait", "node_held"): ("held", "unknown", "unknown", 0),
        ("refused", "observation_unavailable"): ("unknown", "unknown", "unknown", 0),
    }
    if pair not in mapping:
        raise SnapshotError("admission observation disposition is unsupported")
    availability, pressure, capacity, active = mapping[pair]
    return {
        "availability_class": availability,
        "pressure_class": pressure,
        "capacity_class": capacity,
        "active_work_count": active,
    }


def capability_projection(
    value: object,
    *,
    trust: dict[str, Any],
    public_node_id: str,
    observed_at: dt.datetime,
    published_at: dt.datetime,
) -> tuple[dict[str, object], list[dict[str, object]], list[dict[str, object]], str, dt.datetime]:
    capability = exact_object(value, "owned workstation capability")
    if capability.get("schema") != "glaeda-owned-workstation-capability/v1":
        raise SnapshotError("owned workstation capability version is unsupported")
    if capability.get("advisoryOnly") is not True or capability.get("authorizesDispatch") is not False or capability.get("authorizesExecution") is not False:
        raise SnapshotError("owned workstation capability carries unexpected authority")
    cap_observed = parse_time(capability.get("observedAt"), "capability observedAt")
    cap_expires = parse_time(capability.get("expiresAt"), "capability expiresAt")
    if cap_observed > observed_at + dt.timedelta(seconds=MAX_CLOCK_SKEW_SECONDS) or cap_expires <= published_at:
        raise SnapshotError("owned workstation capability is stale")

    node = exact_object(capability.get("node"), "capability node")
    if node.get("osClass") not in {"linux", "macos"} or node.get("architectureClass") not in {"x86_64", "arm64"}:
        raise SnapshotError("capability node class is invalid")
    integer(node.get("generation"), "capability node generation", 1, 2**31 - 1)
    reviewed = trust_node(trust, public_node_id)
    if node["osClass"] != reviewed["os_class"] or node["architectureClass"] != reviewed["architecture_class"]:
        raise SnapshotError("capability node class disagrees with reviewed trust")
    public_node = {
        "id": public_node_id,
        "os_class": node["osClass"],
        "architecture_class": node["architectureClass"],
        "glaeda_node_generation": node["generation"],
    }

    profiles_raw = exact_list(capability.get("profiles"), "capability profiles", MAX_PROFILES)
    profiles: list[dict[str, object]] = []
    profile_ids: set[str] = set()
    for item_raw in profiles_raw:
        item = exact_object(item_raw, "capability profile")
        profile_id = bounded_string(item.get("id"), PROFILE_RE, "profile id")
        profile_class = item.get("class")
        generation = item.get("versionSha256")
        if not isinstance(profile_class, str) or len(profile_class) > 48 or not re.fullmatch(r"[a-z0-9_]+", profile_class):
            raise SnapshotError("profile class is invalid")
        bounded_string(generation, SHA256_RE, "profile generation")
        if profile_id in profile_ids:
            raise SnapshotError("profile identities must be unique")
        profile_ids.add(profile_id)
        profiles.append({"id": profile_id, "class": profile_class, "generation": generation})

    repositories = set(trust["repositories"])
    projects_raw = exact_list(capability.get("projects"), "capability projects", MAX_PROJECTS)
    projects: list[dict[str, object]] = []
    for item_raw in projects_raw:
        item = exact_object(item_raw, "capability project")
        repository = item.get("repository")
        if repository not in repositories:
            raise SnapshotError("project is absent from the reviewed repository allowlist")
        source = exact_object(item.get("source"), "project source")
        commit = bounded_string(source.get("commitOid"), OID_RE, "project commit")
        tree = bounded_string(source.get("treeOid"), OID_RE, "project tree")
        heat = item.get("heatClass")
        heat_map = {"resident_hot": "resident_hot", "resident_cold": "cold"}
        if heat not in heat_map:
            raise SnapshotError("project heat class is unsupported")
        verification_profiles = exact_list(item.get("verificationProfiles"), "project verification profiles", MAX_PROFILES)
        if any(not isinstance(profile, str) or len(profile) > 80 for profile in verification_profiles):
            raise SnapshotError("project verification profiles are invalid")
        projects.append({
            "repository": repository,
            "source": {"commit_oid": commit, "tree_oid": tree},
            "heat_class": heat_map[heat],
            "verification_profiles": sorted(set(verification_profiles)),
            "dependency_build_state_class": "resident_generation" if heat == "resident_hot" else "cold",
            "active_task_count": 0,
            "recent_compatible_receipt_ref": None,
        })

    producer = exact_object(capability.get("producer"), "capability producer")
    glaeda_generation = bounded_string(producer.get("glaedaRuntimeSha256"), SHA256_RE, "Glaeda generation")
    return public_node, profiles, projects, glaeda_generation, cap_observed


def validate_project_state(value: object, trust: dict[str, Any], profile_ids: set[str]) -> list[dict[str, object]]:
    doc = exact_object(value, "project state input")
    exact_keys(doc, {"document_type", "schema_version", "projects"}, "project state input")
    if doc["document_type"] != "glaeda-github-project-heat-input" or doc["schema_version"] != 1:
        raise SnapshotError("project state input version is unsupported")
    repositories = set(trust["repositories"])
    projects: list[dict[str, object]] = []
    seen: set[str] = set()
    for raw in exact_list(doc["projects"], "project state", MAX_PROJECTS):
        item = exact_object(raw, "project state")
        exact_keys(
            item,
            {"repository", "source", "heat_class", "verification_profiles", "dependency_build_state_class", "active_task_count", "recent_compatible_receipt_ref"},
            "project state",
        )
        repository = item["repository"]
        if repository not in repositories or repository in seen:
            raise SnapshotError("project state repository is invalid")
        seen.add(repository)
        source = exact_object(item["source"], "project state source")
        exact_keys(source, {"commit_oid", "tree_oid"}, "project state source")
        commit = bounded_string(source["commit_oid"], OID_RE, "project state commit")
        tree = bounded_string(source["tree_oid"], OID_RE, "project state tree")
        if item["heat_class"] not in HEAT_CLASSES or item["dependency_build_state_class"] not in BUILD_STATE_CLASSES:
            raise SnapshotError("project state class is invalid")
        active = integer(item["active_task_count"], "project active task count", 0, 32)
        verification_profiles = exact_list(item["verification_profiles"], "project verification profiles", MAX_PROFILES)
        if any(not isinstance(profile, str) or VERIFICATION_PROFILE_RE.fullmatch(profile) is None for profile in verification_profiles):
            raise SnapshotError("project verification profile is invalid")
        receipt = item["recent_compatible_receipt_ref"]
        if receipt is not None:
            bounded_string(receipt, SHA256_RE, "recent receipt reference")
        projects.append({
            "repository": repository,
            "source": {"commit_oid": commit, "tree_oid": tree},
            "heat_class": item["heat_class"],
            "verification_profiles": sorted(set(verification_profiles)),
            "dependency_build_state_class": item["dependency_build_state_class"],
            "active_task_count": active,
            "recent_compatible_receipt_ref": receipt,
        })
    return sorted(projects, key=lambda item: str(item["repository"]))


def validate_request_state(value: object, trust: dict[str, Any], profile_ids: set[str]) -> list[dict[str, object]]:
    doc = exact_object(value, "request state input")
    exact_keys(doc, {"document_type", "schema_version", "requests"}, "request state input")
    if doc["document_type"] != "glaeda-github-request-state-input" or doc["schema_version"] != 1:
        raise SnapshotError("request state input version is unsupported")
    repositories = set(trust["repositories"])
    requests: list[dict[str, object]] = []
    seen: set[str] = set()
    for raw in exact_list(doc["requests"], "request state", MAX_REQUESTS):
        item = exact_object(raw, "request state")
        exact_keys(
            item,
            {"request_id", "source", "profile", "state", "terminal_receipt_ref", "elapsed_class", "estimate_class"},
            "request state",
        )
        request_id = bounded_string(item["request_id"], REQUEST_RE, "request id")
        if request_id in seen:
            raise SnapshotError("request ids must be unique")
        seen.add(request_id)
        source = exact_object(item["source"], "request source")
        exact_keys(source, {"repository", "commit_oid", "tree_oid"}, "request source")
        if source["repository"] not in repositories:
            raise SnapshotError("request repository is absent from the reviewed allowlist")
        commit = bounded_string(source["commit_oid"], OID_RE, "request commit")
        tree = bounded_string(source["tree_oid"], OID_RE, "request tree")
        if item["profile"] not in profile_ids or item["state"] not in REQUEST_STATES:
            raise SnapshotError("request profile or state is invalid")
        if item["elapsed_class"] not in DURATION_CLASSES or item["estimate_class"] not in DURATION_CLASSES:
            raise SnapshotError("request duration class is invalid")
        receipt = item["terminal_receipt_ref"]
        if item["state"] == "terminal":
            bounded_string(receipt, SHA256_RE, "terminal receipt reference")
        elif receipt is not None:
            raise SnapshotError("non-terminal request cannot claim a terminal receipt")
        requests.append({
            "request_id": request_id,
            "source": {"repository": source["repository"], "commit_oid": commit, "tree_oid": tree},
            "profile": item["profile"],
            "state": item["state"],
            "terminal_receipt_ref": receipt,
            "elapsed_class": item["elapsed_class"],
            "estimate_class": item["estimate_class"],
        })
    return sorted(requests, key=lambda item: str(item["request_id"]))


def build_unsigned_snapshot(
    capability: object,
    admission: object,
    trust_value: object,
    *,
    public_node_id: str,
    producer_generation: int,
    snapshot_sequence: int,
    observed_at: dt.datetime,
    published_at: dt.datetime,
    maximum_useful_age_seconds: int = DEFAULT_USEFUL_AGE_SECONDS,
    project_state: object | None = None,
    request_state: object | None = None,
) -> dict[str, object]:
    trust = validate_trust(trust_value)
    public_node_id = bounded_string(public_node_id, NODE_RE, "public node id")
    reviewed = trust_node(trust, public_node_id)
    producer_generation = integer(producer_generation, "producer generation", 1, 2**31 - 1)
    snapshot_sequence = integer(snapshot_sequence, "snapshot sequence", 1, 2**63 - 1)
    maximum_useful_age_seconds = integer(
        maximum_useful_age_seconds, "maximum useful age", MIN_USEFUL_AGE_SECONDS, MAX_USEFUL_AGE_SECONDS
    )
    if published_at < observed_at:
        raise SnapshotError("published_at precedes observed_at")
    node, profiles, projects, glaeda_generation, capability_observed_at = capability_projection(
        capability,
        trust=trust,
        public_node_id=public_node_id,
        observed_at=observed_at,
        published_at=published_at,
    )
    effective_observed_at = min(observed_at, capability_observed_at)
    if published_at < effective_observed_at:
        raise SnapshotError("published_at precedes observed evidence")
    node.update(admission_projection(admission))
    profile_ids = {str(profile["id"]) for profile in profiles}
    if project_state is not None:
        projects = validate_project_state(project_state, trust, profile_ids)
    else:
        active = integer(node["active_work_count"], "node active work count", 0, 32)
        if len(projects) == 1:
            projects[0]["active_task_count"] = active
    requests = [] if request_state is None else validate_request_state(request_state, trust, profile_ids)
    active_work_count = integer(node["active_work_count"], "node active work count", 0, 32)
    active_request_count = sum(item["state"] in {"preparing", "running"} for item in requests)
    active_project_count = sum(int(item["active_task_count"]) for item in projects)
    if active_request_count > active_work_count or active_project_count > active_work_count:
        raise SnapshotError("published active work disagrees with local admission evidence")
    unsigned = {
        "document_type": NODE_DOCUMENT,
        "schema_version": SCHEMA_VERSION,
        "payload": {
            "authority": {
                "advisory_only": True,
                "authorizes_dispatch": False,
                "authorizes_execution": False,
                "authorizes_host_selection": False,
                "authorizes_cleanup": False,
            },
            "freshness": {
                "snapshot_sequence": snapshot_sequence,
                "observed_at": format_time(effective_observed_at),
                "published_at": format_time(published_at),
                "producer_generation": producer_generation,
                "maximum_useful_age_seconds": maximum_useful_age_seconds,
            },
            "producer": {
                "glaeda_generation": glaeda_generation,
                "key_id": reviewed["key_id"],
            },
            "node": node,
            "profiles": sorted(profiles, key=lambda item: str(item["id"])),
            "projects": sorted(projects, key=lambda item: str(item["repository"])),
            "requests": requests,
        },
    }
    validate_unsigned_snapshot(unsigned, trust, check_freshness=False)
    return unsigned


def validate_unsigned_snapshot(
    value: object,
    trust_value: object,
    *,
    now: dt.datetime | None = None,
    check_freshness: bool = True,
) -> dict[str, Any]:
    trust = validate_trust(trust_value)
    value = exact_object(value, "node snapshot")
    exact_keys(value, {"document_type", "schema_version", "payload"}, "node snapshot")
    if value["document_type"] != NODE_DOCUMENT or value["schema_version"] != SCHEMA_VERSION:
        raise SnapshotError("node snapshot version is unsupported")
    payload = exact_object(value["payload"], "node payload")
    exact_keys(payload, {"authority", "freshness", "producer", "node", "profiles", "projects", "requests"}, "node payload")
    authority = exact_object(payload["authority"], "snapshot authority")
    exact_keys(authority, {"advisory_only", "authorizes_dispatch", "authorizes_execution", "authorizes_host_selection", "authorizes_cleanup"}, "snapshot authority")
    if authority != {
        "advisory_only": True,
        "authorizes_dispatch": False,
        "authorizes_execution": False,
        "authorizes_host_selection": False,
        "authorizes_cleanup": False,
    }:
        raise SnapshotError("node snapshot carries authority")
    freshness = exact_object(payload["freshness"], "snapshot freshness")
    exact_keys(freshness, {"snapshot_sequence", "observed_at", "published_at", "producer_generation", "maximum_useful_age_seconds"}, "snapshot freshness")
    integer(freshness["snapshot_sequence"], "snapshot sequence", 1, 2**63 - 1)
    integer(freshness["producer_generation"], "producer generation", 1, 2**31 - 1)
    max_age = integer(freshness["maximum_useful_age_seconds"], "maximum useful age", MIN_USEFUL_AGE_SECONDS, MAX_USEFUL_AGE_SECONDS)
    observed = parse_time(freshness["observed_at"], "snapshot observed_at")
    published = parse_time(freshness["published_at"], "snapshot published_at")
    if published < observed:
        raise SnapshotError("snapshot publication time is invalid")
    if check_freshness:
        now = now or dt.datetime.now(dt.UTC)
        if now.tzinfo is None or now.utcoffset() != dt.timedelta(0):
            raise SnapshotError("freshness clock must be UTC")
        if observed > now + dt.timedelta(seconds=MAX_CLOCK_SKEW_SECONDS) or published > now + dt.timedelta(seconds=MAX_CLOCK_SKEW_SECONDS):
            raise SnapshotError("snapshot timestamp is in the future")
        if now - observed > dt.timedelta(seconds=max_age):
            raise SnapshotError("snapshot is stale")

    producer = exact_object(payload["producer"], "snapshot producer")
    exact_keys(producer, {"glaeda_generation", "key_id"}, "snapshot producer")
    bounded_string(producer["glaeda_generation"], SHA256_RE, "Glaeda generation")
    key_id = bounded_string(producer["key_id"], KEY_RE, "producer key id")

    node = exact_object(payload["node"], "snapshot node")
    exact_keys(node, {"id", "os_class", "architecture_class", "glaeda_node_generation", "availability_class", "pressure_class", "capacity_class", "active_work_count"}, "snapshot node")
    node_id = bounded_string(node["id"], NODE_RE, "snapshot node id")
    reviewed = trust_node(trust, node_id)
    if key_id != reviewed["key_id"]:
        raise SnapshotError("snapshot key id disagrees with reviewed trust")
    if node["os_class"] != reviewed["os_class"] or node["architecture_class"] != reviewed["architecture_class"]:
        raise SnapshotError("snapshot node class disagrees with reviewed trust")
    integer(node["glaeda_node_generation"], "Glaeda node generation", 1, 2**31 - 1)
    if node["availability_class"] not in AVAILABILITY_CLASSES or node["pressure_class"] not in PRESSURE_CLASSES or node["capacity_class"] not in CAPACITY_CLASSES:
        raise SnapshotError("snapshot node state class is invalid")
    integer(node["active_work_count"], "active work count", 0, 32)

    profiles = exact_list(payload["profiles"], "snapshot profiles", MAX_PROFILES)
    profile_ids: set[str] = set()
    for raw in profiles:
        item = exact_object(raw, "snapshot profile")
        exact_keys(item, {"id", "class", "generation"}, "snapshot profile")
        profile_id = bounded_string(item["id"], PROFILE_RE, "snapshot profile id")
        if profile_id in profile_ids:
            raise SnapshotError("snapshot profile ids must be unique")
        profile_ids.add(profile_id)
        if not isinstance(item["class"], str) or len(item["class"]) > 48 or re.fullmatch(r"[a-z0-9_]+", item["class"]) is None:
            raise SnapshotError("snapshot profile class is invalid")
        bounded_string(item["generation"], SHA256_RE, "snapshot profile generation")

    repositories = set(trust["repositories"])
    projects = exact_list(payload["projects"], "snapshot projects", MAX_PROJECTS)
    seen_projects: set[str] = set()
    for raw in projects:
        item = exact_object(raw, "snapshot project")
        exact_keys(item, {"repository", "source", "heat_class", "verification_profiles", "dependency_build_state_class", "active_task_count", "recent_compatible_receipt_ref"}, "snapshot project")
        repository = item["repository"]
        if repository not in repositories or repository in seen_projects:
            raise SnapshotError("snapshot project repository is invalid")
        seen_projects.add(repository)
        source = exact_object(item["source"], "snapshot project source")
        exact_keys(source, {"commit_oid", "tree_oid"}, "snapshot project source")
        bounded_string(source["commit_oid"], OID_RE, "snapshot project commit")
        bounded_string(source["tree_oid"], OID_RE, "snapshot project tree")
        if item["heat_class"] not in HEAT_CLASSES or item["dependency_build_state_class"] not in BUILD_STATE_CLASSES:
            raise SnapshotError("snapshot project state class is invalid")
        verification_profiles = exact_list(item["verification_profiles"], "snapshot project profiles", MAX_PROFILES)
        if any(not isinstance(profile, str) or VERIFICATION_PROFILE_RE.fullmatch(profile) is None for profile in verification_profiles):
            raise SnapshotError("snapshot project verification profile is invalid")
        integer(item["active_task_count"], "snapshot project active task count", 0, 32)
        if item["recent_compatible_receipt_ref"] is not None:
            bounded_string(item["recent_compatible_receipt_ref"], SHA256_RE, "snapshot recent receipt")

    requests = exact_list(payload["requests"], "snapshot requests", MAX_REQUESTS)
    seen_requests: set[str] = set()
    for raw in requests:
        item = exact_object(raw, "snapshot request")
        exact_keys(item, {"request_id", "source", "profile", "state", "terminal_receipt_ref", "elapsed_class", "estimate_class"}, "snapshot request")
        request_id = bounded_string(item["request_id"], REQUEST_RE, "snapshot request id")
        if request_id in seen_requests:
            raise SnapshotError("snapshot request ids must be unique")
        seen_requests.add(request_id)
        source = exact_object(item["source"], "snapshot request source")
        exact_keys(source, {"repository", "commit_oid", "tree_oid"}, "snapshot request source")
        if source["repository"] not in repositories:
            raise SnapshotError("snapshot request repository is invalid")
        bounded_string(source["commit_oid"], OID_RE, "snapshot request commit")
        bounded_string(source["tree_oid"], OID_RE, "snapshot request tree")
        if item["profile"] not in profile_ids or item["state"] not in REQUEST_STATES:
            raise SnapshotError("snapshot request state is invalid")
        if item["elapsed_class"] not in DURATION_CLASSES or item["estimate_class"] not in DURATION_CLASSES:
            raise SnapshotError("snapshot request duration class is invalid")
        if item["state"] == "terminal":
            bounded_string(item["terminal_receipt_ref"], SHA256_RE, "snapshot terminal receipt")
        elif item["terminal_receipt_ref"] is not None:
            raise SnapshotError("non-terminal snapshot request claims a terminal receipt")

    if len(canonical_json(value)) > MAX_NODE_BYTES:
        raise SnapshotError("node snapshot exceeds the byte ceiling")
    return value


def signing_environment() -> dict[str, str]:
    return {"LC_ALL": "C"}


def require_executable(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise SnapshotError(f"{label} must be an absolute path")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise SnapshotError(f"{label} is unavailable") from error
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise SnapshotError(f"{label} is unavailable")
    return resolved


def run_bounded(
    argv: list[str],
    *,
    cwd: Path | None = None,
    input_bytes: bytes | None = None,
    env: dict[str, str] | None = None,
    timeout: int = 10,
) -> subprocess.CompletedProcess[bytes]:
    try:
        options: dict[str, object] = {
            "cwd": cwd,
            "stdout": subprocess.PIPE,
            "stderr": subprocess.PIPE,
            "check": False,
            "timeout": timeout,
            "env": env or {"LC_ALL": "C"},
        }
        if input_bytes is None:
            options["stdin"] = subprocess.DEVNULL
        else:
            options["input"] = input_bytes
        completed = subprocess.run(argv, **options)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise SnapshotError("bounded subprocess failed") from error
    if len(completed.stdout) > MAX_GIT_OUTPUT_BYTES or len(completed.stderr) > MAX_GIT_OUTPUT_BYTES:
        raise SnapshotError("bounded subprocess output exceeded its ceiling")
    return completed


def sign_snapshot(
    unsigned_value: object,
    trust_value: object,
    *,
    private_key: Path,
    ssh_keygen: Path = Path("/usr/bin/ssh-keygen"),
) -> dict[str, object]:
    trust = validate_trust(trust_value)
    unsigned = validate_unsigned_snapshot(unsigned_value, trust, check_freshness=False)
    node_id = unsigned["payload"]["node"]["id"]
    reviewed = trust_node(trust, node_id)
    key_id = reviewed["key_id"]
    ssh_keygen = require_executable(ssh_keygen, "ssh-keygen")
    private_key = private_key.resolve(strict=True)
    if not private_key.is_file():
        raise SnapshotError("snapshot signing key is unavailable")
    if stat.S_IMODE(private_key.stat().st_mode) & 0o077:
        raise SnapshotError("snapshot signing key permissions are too broad")
    public = run_bounded(
        [str(ssh_keygen), "-y", "-f", str(private_key)],
        env=signing_environment(),
 