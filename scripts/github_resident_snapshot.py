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
            raise SnapshotError("project state