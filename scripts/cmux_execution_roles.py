#!/usr/bin/env python3
"""Pure CMUX fleet execution-role, capability, and local-admission model."""
from __future__ import annotations

import json
import re
from typing import Any, Iterable

NODE_SCHEMA = "glaeda-cmux-node-capabilities/v1"
ROLE_CANARY_SCHEMA = "glaeda-cmux-role-canary/v1"
CAPACITY_SCHEMA = "glaeda-cmux-role-capacity/v1"
WORKLOAD_SCHEMA = "glaeda-cmux-workload-requirement/v1"
PREFERENCE_SCHEMA = "glaeda-cmux-placement-preference/v1"
STATUS_SCHEMA = "glaeda-cmux-execution-role-status/v1"
DECISION_SCHEMA = "glaeda-cmux-local-admission/v1"

TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9._:-]{0,95}\Z")
SHA256_RE = re.compile(r"sha256:[0-9a-f]{64}\Z")
NODE_RE = re.compile(r"cmux-[a-z0-9][a-z0-9-]{2,59}\Z")

ROLES = (
    "artifact_cache",
    "background_replay",
    "benchmark",
    "cmux_linux_agent",
    "cmux_linux_ci",
    "cmux_macos_native_build",
    "cmux_macos_test",
    "diagnostic",
)

RESOURCE_PROFILES = ("small", "medium", "large", "exclusive")
CPU_CLASSES = ("small", "medium", "large")
MEMORY_CLASSES = ("small", "medium", "large", "xlarge")
DISK_CLASSES = ("small", "medium", "large")
PRESSURE_CLASSES = ("normal", "elevated", "critical", "unknown")
SWAP_CLASSES = ("none", "low", "elevated", "critical", "unknown")
THERMAL_CLASSES = ("stable", "elevated", "critical", "unknown")
ROUTABLE_STATES = {"eligible", "active"}
KNOWN_STATES = {
    "new",
    "discovered",
    "bootstrapping",
    "enrolling",
    "canary",
    "eligible",
    "active",
    "draining",
    "maintenance",
    "quarantined",
    "retired",
}

# Role identity stays small. More precise work belongs in capability and operation vocabulary.
ROLE_REQUIREMENTS = {
    "cmux_macos_native_build": {
        "platform": "macos",
        "architectures": {"arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {
            "apple_silicon",
            "native_glaeda_build",
            "native_xcode_build",
        },
        "requiresToolchainProfile": True,
    },
    "cmux_macos_test": {
        "platform": "macos",
        "architectures": {"arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {"apple_silicon", "app_host_test"},
        "requiresToolchainProfile": True,
    },
    "cmux_linux_ci": {
        "platform": "linux",
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {
            "cgroup_v2",
            "linux_ci_toolchain",
            "systemd_execution",
            "task_isolation",
        },
        "requiresToolchainProfile": True,
    },
    "cmux_linux_agent": {
        "platform": "linux",
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "medium",
        "minimumMemoryClass": "medium",
        "minimumDiskClass": "medium",
        "requiredCapabilities": {"cgroup_v2", "systemd_execution", "task_isolation"},
        "requiresToolchainProfile": True,
    },
    "artifact_cache": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "large",
        "requiredCapabilities": {"artifact_service"},
        "requiresToolchainProfile": False,
    },
    "background_replay": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "small",
        "requiredCapabilities": {"background_replay"},
        "requiresToolchainProfile": False,
    },
    "benchmark": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "small",
        "requiredCapabilities": {"benchmark"},
        "requiresToolchainProfile": False,
    },
    "diagnostic": {
        "platform": None,
        "architectures": {"x86_64", "arm64"},
        "minimumCpuClass": "small",
        "minimumMemoryClass": "small",
        "minimumDiskClass": "small",
        "requiredCapabilities": {"diagnostic"},
        "requiresToolchainProfile": False,
    },
}

# Useful work names semantic requirements. Scarce slot claims stay local.
OPERATION_CONTRACTS = {
    "cmux_macos_compile_admission": {
        "role": "cmux_macos_native_build",
        "requiredCapabilities": {"native_xcode_build"},
        "slotClaims": ("mac_native_build_lane",),
    },
    "cmux_macos_release_admission": {
        "role": "cmux_macos_native_build",
        "requiredCapabilities": {"native_release_build"},
        "slotClaims": ("artifact_publisher_slot", "mac_native_build_lane"),
    },
    "cmux_macos_app_host_test": {
        "role": "cmux_macos_test",
        "requiredCapabilities": {"app_host_test"},
        "slotClaims": ("mac_app_host_test_slot",),
        "largeSlotClaims": ("mac_app_host_test_slot", "mac_native_build_lane"),
    },
    "cmux_linux_ci_admission": {
        "role": "cmux_linux_ci",
        "requiredCapabilities": {"linux_ci"},
        "slotClaims": ("linux_medium_slot",),
        "largeSlotClaims": ("linux_heavy_slot",),
    },
    "cmux_web_ci_admission": {
        "role": "cmux_linux_ci",
        "requiredCapabilities": {"web_ci"},
        "slotClaims": ("browser_ui_test_slot",),
    },
    "cmux_linux_agent_admission": {
        "role": "cmux_linux_agent",
        "requiredCapabilities": {"linux_agent"},
        "slotClaims": ("linux_medium_slot",),
        "largeSlotClaims": ("linux_heavy_slot",),
    },
    "cmux_build_helper_admission": {
        "role": "cmux_linux_agent",
        "requiredCapabilities": {"build_helper"},
        "slotClaims": ("linux_medium_slot",),
        "largeSlotClaims": ("linux_heavy_slot",),
    },
    "background_verification": {
        "role": "cmux_linux_ci",
        "requiredCapabilities": {"background_verification"},
        "slotClaims": ("linux_medium_slot",),
    },
    "artifact_cache_service": {
        "role": "artifact_cache",
        "requiredCapabilities": {"artifact_service"},
        "slotClaims": (),
    },
    "background_replay": {
        "role": "background_replay",
        "requiredCapabilities": {"background_replay"},
        "slotClaims": (),
    },
    "benchmark": {
        "role": "benchmark",
        "requiredCapabilities": {"benchmark"},
        "slotClaims": (),
    },
    "diagnostic": {
        "role": "diagnostic",
        "requiredCapabilities": {"diagnostic"},
        "slotClaims": (),
    },
}


class RoleModelError(RuntimeError):
    """Closed refusal for invalid role/capability evidence."""


def _exact(value: object, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise RoleModelError(f"{label} has unknown or missing fields")
    return value


def _token(value: object, label: str, *, optional: bool = False) -> str | None:
    if value is None and optional:
        return None
    if not isinstance(value, str) or TOKEN_RE.fullmatch(value) is None:
        raise RoleModelError(f"{label} is invalid")
    return value


def _sha(value: object, label: str) -> str:
    if not isinstance(value, str) or SHA256_RE.fullmatch(value) is None:
        raise RoleModelError(f"{label} is invalid")
    return value


def _positive_int(value: object, label: str, *, maximum: int = 2**31 - 1) -> int:
    if type(value) is not int or value < 1 or value > maximum:
        raise RoleModelError(f"{label} is invalid")
    return value


def _nonnegative_int(value: object, label: str, *, maximum: int = 2**31 - 1) -> int:
    if type(value) is not int or value < 0 or value > maximum:
        raise RoleModelError(f"{label} is invalid")
    return value


def _sorted_tokens(
    value: object, label: str, *, allowed: set[str] | None = None
) -> list[str]:
    if (
        not isinstance(value, list)
        or len(value) > 64
        or any(not isinstance(v, str) for v in value)    ):
        raise RoleModelError(f"{label} is invalid")
    if value != sorted(set(value)):
        raise RoleModelError(f"{label} must be sorted and unique")
    for item in value:
        _token(item, label)
        if allowed is not None and item not in allowed:
            raise RoleModelError(f"{label} contains an unsupported value")
    return value


def _sha_map(
    value: object,
    label: str,
    *,
    allowed_keys: set[str] | None = None,
) -> dict[str, str]:
    if not isinstance(value, dict) or len(value) > 64:
        raise RoleModelError(f"{label} is invalid")
    for key, generation in value.items():
        _token(key, label)
        _sha(generation, label)
        if allowed_keys is not None and key not in allowed_keys:
            raise RoleModelError(f"{label} contains an unsupported key")
    return value


def _class_at_least(current: str, required: str, classes: tuple[str, ...]) -> bool:
    return classes.index(current) >= classes.index(required)


def validate_node(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "state",
            "platform",
            "architecture",
            "osVersionClass",
            "enrollmentGeneration",
            "glaedaGeneration",
            "cpuClass",
            "memoryClass",
            "diskClass",
            "capabilityGeneration",
            "toolchainProfiles",
            "roleWorkloadGenerations",
            "capabilities",
            "pressure",
        },
        "node capability",
    )
    if doc["schema"] != NODE_SCHEMA:
        raise RoleModelError("node capability schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("nodeId is invalid")
    if doc["state"] not in KNOWN_STATES:
        raise RoleModelError("node state is unsupported")
    if doc["platform"] not in {"macos", "linux"}:
        raise RoleModelError("node platform is unsupported")
    if doc["architecture"] not in {"arm64", "x86_64"}:
        raise RoleModelError("node architecture is unsupported")
    _token(doc["osVersionClass"], "OS version class")
    _positive_int(doc["enrollmentGeneration"], "enrollment generation")
    _sha(doc["glaedaGeneration"], "Glaeda generation")
    if doc["cpuClass"] not in CPU_CLASSES:
        raise RoleModelError("CPU class is unsupported")
    if doc["memoryClass"] not in MEMORY_CLASSES:
        raise RoleModelError("memory class is unsupported")
    if doc["diskClass"] not in DISK_CLASSES:
        raise RoleModelError("disk class is unsupported")
    _positive_int(doc["capabilityGeneration"], "capability generation")
    _sha_map(doc["toolchainProfiles"], "toolchain profiles")
    workload_generations = _sha_map(
        doc["roleWorkloadGenerations"],
        "role workload generations",
        allowed_keys=set(ROLES),
    )
    if not workload_generations:
        raise RoleModelError("at least one enrolled role workload generation is required")
    _sorted_tokens(doc["capabilities"], "node capabilities")
    pressure = _exact(
        doc["pressure"], {"cpu", "memory", "swap", "thermal"}, "pressure"
    )
    if (
        pressure["cpu"] not in PRESSURE_CLASSES
        or pressure["memory"] not in PRESSURE_CLASSES
    ):
        raise RoleModelError("CPU/memory pressure class is unsupported")
    if pressure["swap"] not in SWAP_CLASSES:
        raise RoleModelError("swap class is unsupported")
    if pressure["thermal"] not in THERMAL_CLASSES:
        raise RoleModelError("thermal class is unsupported")
    return doc


def validate_role_canary(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "enrollmentGeneration",
            "glaedaGeneration",
            "capabilityGeneration",
            "role",
            "toolchainProfile",
            "toolchainGeneration",
            "acceptedCapabilities",
            "acceptedResourceProfiles",
            "workloadGeneration",
            "result",
        },
        "role canary",
    )
    if doc["schema"] != ROLE_CANARY_SCHEMA:
        raise RoleModelError("role canary schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("role canary nodeId is invalid")
    _positive_int(doc["enrollmentGeneration"], "role canary enrollment generation")
    _sha(doc["glaedaGeneration"], "role canary Glaeda generation")
    _positive_int(
        doc["capabilityGeneration"], "role canary capability generation"
    )
    if doc["role"] not in ROLES:
        raise RoleModelError("role canary role is unsupported")
    profile = _token(
        doc["toolchainProfile"], "role canary toolchain profile", optional=True
    )
    generation = doc["toolchainGeneration"]
    if generation is not None:
        _sha(generation, "role canary toolchain generation")
    if (profile is None) != (generation is None):
        raise RoleModelError("role canary toolchain profile/generation must be paired")
    _sorted_tokens(doc["acceptedCapabilities"], "accepted capabilities")
    _sorted_tokens(
        doc["acceptedResourceProfiles"],
        "accepted resource profiles",
        allowed=set(RESOURCE_PROFILES),
    )
    _sha(doc["workloadGeneration"], "role canary workload generation")
    if doc["result"] not in {"accepted", "rejected"}:
        raise RoleModelError("role canary result is invalid")
    return doc


def validate_capacity(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "enrollmentGeneration",
            "glaedaGeneration",
            "capabilityGeneration",
            "role",
            "workloadGeneration",
            "toolchainProfile",
            "toolchainGeneration",
            "resourceProfile",
            "slotClass",
            "maxConcurrent",
            "contentionEvidenceGeneration",
            "measurement",
            "result",
        },
        "capacity evidence",
    )
    if doc["schema"] != CAPACITY_SCHEMA:
        raise RoleModelError("capacity schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("capacity nodeId is invalid")
    _positive_int(doc["enrollmentGeneration"], "capacity enrollment generation")
    _sha(doc["glaedaGeneration"], "capacity Glaeda generation")
    _positive_int(doc["capabilityGeneration"], "capacity generation")
    if doc["role"] not in ROLES:
        raise RoleModelError("capacity role is unsupported")
    _sha(doc["workloadGeneration"], "capacity workload generation")
    profile = _token(
        doc["toolchainProfile"], "capacity toolchain profile", optional=True
    )
    generation = doc["toolchainGeneration"]
    if generation is not None:
        _sha(generation, "capacity toolchain generation")
    if (profile is None) != (generation is None):
        raise RoleModelError("capacity toolchain profile/generation must be paired")
    if doc["resourceProfile"] not in RESOURCE_PROFILES:
        raise RoleModelError("resource profile is unsupported")
    _token(doc["slotClass"], "slot class")
    _positive_int(doc["maxConcurrent"], "max concurrent", maximum=16)
    _sha(doc["contentionEvidenceGeneration"], "contention evidence generation")
    measurement = _exact(
        doc["measurement"],
        {
            "validatedCompletions",
            "p50Millis",
            "p90Millis",
            "cpuPressure",
            "memoryPressure",
            "swapClass",
            "thermalBehavior",
            "unfinishedWork",
        },
        "capacity measurement",
    )
    _nonnegative_int(
        measurement["validatedCompletions"],
        "validated completions",
        maximum=65535,
    )
    _positive_int(measurement["p50Millis"], "p50 millis")
    _positive_int(measurement["p90Millis"], "p90 millis")
    if measurement["p90Millis"] < measurement["p50Millis"]:
        raise RoleModelError("p90 cannot be lower than p50")
    if measurement["cpuPressure"] not in PRESSURE_CLASSES:
        raise RoleModelError("capacity CPU pressure is unsupported")
    if measurement["memoryPressure"] not in PRESSURE_CLASSES:
        raise RoleModelError("capacity memory pressure is unsupported")
    if measurement["swapClass"] not in SWAP_CLASSES:
        raise RoleModelError("capacity swap class is unsupported")
    if measurement["thermalBehavior"] not in THERMAL_CLASSES:
        raise RoleModelError("capacity thermal class is unsupported")
    _nonnegative_int(
        measurement["unfinishedWork"], "unfinished work", maximum=65535
    )
    if doc["result"] not in {"accepted", "rejected"}:
        raise RoleModelError("capacity result is invalid")
    return doc


def validate_workload(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "operation",
            "role",
            "platform",
            "architecture",
            "toolchainProfile",
            "minimumCpuClass",
            "minimumMemoryClass",
            "requiredCapabilities",
            "resourceProfile",
        },
        "workload requirement",
    )
    if doc["schema"] != WORKLOAD_SCHEMA:
        raise RoleModelError("workload schema is unsupported")
    operation = _token(doc["operation"], "operation")
    if operation not in OPERATION_CONTRACTS:
        raise RoleModelError("operation is unsupported")
    contract = OPERATION_CONTRACTS[operation]
    if doc["role"] != contract["role"]:
        raise RoleModelError("operation and role disagree")
    if doc["platform"] not in {"macos", "linux"}:
        raise RoleModelError("workload platform is unsupported")
    if doc["architecture"] not in {"arm64", "x86_64"}:
        raise RoleModelError("workload architecture is unsupported")
    _token(
        doc["toolchainProfile"], "workload toolchain profile", optional=True
    )
    if doc["minimumCpuClass"] not in CPU_CLASSES:
        raise RoleModelError("workload CPU class is unsupported")
    if doc["minimumMemoryClass"] not in MEMORY_CLASSES:
        raise RoleModelError("workload memory class is unsupported")
    required = _sorted_tokens(
        doc["requiredCapabilities"], "required capabilities"
    )
    if not set(contract["requiredCapabilities"]).issubset(required):
        raise RoleModelError("workload omits operation-required capability")
    if doc["resourceProfile"] not in RESOURCE_PROFILES:
        raise RoleModelError("workload resource profile is unsupported")
    return doc


def validate_preference(value: object) -> dict[str, Any]:
    doc = _exact(
        value,
        {
            "schema",
            "nodeId",
            "role",
            "evidenceGeneration",
            "preference",
            "authority",
        },
        "placement preference",
    )
    if doc["schema"] != PREFERENCE_SCHEMA:
        raise RoleModelError("preference schema is unsupported")
    if not isinstance(doc["nodeId"], str) or NODE_RE.fullmatch(doc["nodeId"]) is None:
        raise RoleModelError("preference nodeId is invalid")
    if doc["role"] not in ROLES:
        raise RoleModelError("preference role is unsupported")
    _sha(doc["evidenceGeneration"], "preference evidence generation")    if doc["preference"] not in {"preferred", "neutral"}:
        raise RoleModelError("preference value is unsupported")
    if doc["authority"] != "observation_only":
        raise RoleModelError(
            "preference authority must remain observation_only"
        )
    return doc


def role_eligibility(
    node_value: object, canary_values: Iterable[object]
) -> dict[str, dict[str, Any]]:
    node = validate_node(node_value)
    canaries: dict[str, dict[str, Any]] = {}
    for value in canary_values:
        canary = validate_role_canary(value)
        if canary["nodeId"] != node["nodeId"]:
            raise RoleModelError("role canary belongs to a different node")
        if canary["role"] in canaries:
            raise RoleModelError("duplicate role canary")
        canaries[canary["role"]] = canary

    results: dict[str, dict[str, Any]] = {}
    capabilities = set(node["capabilities"])
    toolchains = node["toolchainProfiles"]
    for role in ROLES:
        requirement = ROLE_REQUIREMENTS[role]
        if node["state"] not in ROUTABLE_STATES:
            results[role] = {
                "eligible": False,
                "reason": f"node_{node['state']}",
            }
            continue
        platform = requirement["platform"]
        if platform is not None and platform != node["platform"]:
            results[role] = {
                "eligible": False,
                "reason": "platform_mismatch",
            }
            continue
        if node["architecture"] not in requirement["architectures"]:
            results[role] = {
                "eligible": False,
                "reason": "architecture_mismatch",
            }
            continue
        expected_workload = node["roleWorkloadGenerations"].get(role)
        if expected_workload is None:
            results[role] = {
                "eligible": False,
                "reason": "role_not_enrolled",
            }
            continue
        if not _class_at_least(
            node["cpuClass"],
            requirement["minimumCpuClass"],
            CPU_CLASSES,
        ):
            results[role] = {
                "eligible": False,
                "reason": "insufficient_cpu_class",
            }
            continue
        if not _class_at_least(
            node["memoryClass"],
            requirement["minimumMemoryClass"],
            MEMORY_CLASSES,
        ):
            results[role] = {
                "eligible": False,
                "reason": "insufficient_memory_class",
            }
            continue
        if not _class_at_least(
            node["diskClass"],
            requirement["minimumDiskClass"],
            DISK_CLASSES,
        ):
            results[role] = {
                "eligible": False,
                "reason": "insufficient_disk_class",
            }
            continue
        if not requirement["requiredCapabilities"].issubset(capabilities):
            results[role] = {
                "eligible": False,
                "reason": "node_capability_missing",
            }
            continue
        canary = canaries.get(role)
        if canary is None:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_pending",
            }
            continue
        if canary["result"] != "accepted":
            results[role] = {
                "eligible": False,
                "reason": "role_canary_failed",
            }
            continue
        if canary["enrollmentGeneration"] != node["enrollmentGeneration"]:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_enrollment_stale",
            }
            continue
        if canary["glaedaGeneration"] != node["glaedaGeneration"]:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_glaeda_stale",
            }
            continue
        if canary["workloadGeneration"] != expected_workload:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_workload_stale",
            }
            continue
        if canary["capabilityGeneration"] != node["capabilityGeneration"]:
            results[role] = {
                "eligible": False,
                "reason": "role_canary_pending",
            }
            continue
        if not set(requirement["requiredCapabilities"]).issubset(
            canary["acceptedCapabilities"]
        ):
            results[role] = {
                "eligible": False,
                "reason": "role_canary_capability_incomplete",
            }
            continue
        if requirement["requiresToolchainProfile"]:
            profile = canary["toolchainProfile"]
            generation = canary["toolchainGeneration"]
            if (
                profile is None
                or generation is None
                or toolchains.get(profile) != generation
            ):
                results[role] = {
                    "eligible": False,
                    "reason": "toolchain_canary_pending",
                }
                continue
        elif (
            canary["toolchainProfile"] is not None
            or canary["toolchainGeneration"] is not None
        ):
            results[role] = {
                "eligible": False,
                "reason": "role_canary_context_mismatch",
            }
            continue
        results[role] = {
            "eligible": True,
            "reason": "accepted",
            "glaedaGeneration": canary["glaedaGeneration"],
            "workloadGeneration": canary["workloadGeneration"],
            "toolchainProfile": canary["toolchainProfile"],
            "toolchainGeneration": canary["toolchainGeneration"],
            "acceptedCapabilities": canary["acceptedCapabilities"],
            "acceptedResourceProfiles": canary[
                "acceptedResourceProfiles"
            ],
        }
    return results


def _capacity_matches_context(
    node: dict[str, Any],
    receipt: dict[str, Any],
    role_status: dict[str, Any],
) -> bool:
    return (
        role_status.get("eligible") is True
        and receipt["nodeId"] == node["nodeId"]
        and receipt["enrollmentGeneration"] == node["enrollmentGeneration"]
        and receipt["glaedaGeneration"] == node["glaedaGeneration"]
        and receipt["capabilityGeneration"] == node["capabilityGeneration"]
        and receipt["workloadGeneration"] == role_status["workloadGeneration"]
        and receipt["toolchainProfile"] == role_status["toolchainProfile"]
        and receipt["toolchainGeneration"] == role_status["toolchainGeneration"]
    )


def measured_slot_capacity(
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
) -> dict[str, int]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    node = validate_node(node_value)
    eligibility = role_eligibility(node, canary_values)
    slots: dict[str, int] = {}
    for value in capacity_values:
        receipt = validate_capacity(value)
        if receipt["nodeId"] != node["nodeId"]:
            raise RoleModelError(
                "capacity evidence belongs to a different node"
            )
        role = receipt["role"]
        if not _capacity_matches_context(node, receipt, eligibility[role]):
            continue
        if receipt["result"] != "accepted":
            continue
        if (
            receipt["resourceProfile"]
            not in eligibility[role]["acceptedResourceProfiles"]
        ):
            continue
        slots[receipt["slotClass"]] = max(
            slots.get(receipt["slotClass"], 0), receipt["maxConcurrent"]
        )
    return dict(sorted(slots.items()))


def _slot_claims(workload: dict[str, Any]) -> tuple[str, ...]:
    contract = OPERATION_CONTRACTS[workload["operation"]]
    if (
        workload["resourceProfile"] in {"large", "exclusive"}
        and "largeSlotClaims" in contract
    ):
        return contract["largeSlotClaims"]
    return contract["slotClaims"]


def _profile_has_capacity(
    node: dict[str, Any],
    workload: dict[str, Any],
    eligibility: dict[str, dict[str, Any]],
    capacity_values: Iterable[object],
) -> bool:
    role = workload["role"]
    if (
        workload["resourceProfile"]
        not in eligibility[role].get("acceptedResourceProfiles", [])
    ):
        return False
    claims = set(_slot_claims(workload))
    matched = set()
    measured_profile = False
    for value in capacity_values:
        receipt = validate_capacity(value)
        if (
            receipt["role"] != role
            or not _capacity_matches_context(node, receipt, eligibility[role])
            or receipt["resourceProfile"]
            != workload["resourceProfile"]
            or receipt["result"] != "accepted"
        ):
            continue
        measured_profile = True
        if receipt["slotClass"] in claims:
            matched.add(receipt["slotClass"])
    if not measured_profile:
        return False
    return matched == claims


def select_eligible(
    workload_value: object,
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
) -> tuple[bool, str]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    workload = validate_workload(workload_value)
    node = validate_node(node_value)
    eligibility = role_eligibility(node, canary_values)
    role = workload["role"]
    if not eligibility[role]["eligible"]:
        return False, eligibility[role]["reason"]
    if workload["platform"] != node["platform"]:
        return False, "platform_mismatch"
    if workload["architecture"] != node["architecture"]:
        return False, "architecture_mismatch"
    if not _class_at_least(
        node["cpuClass"],
        workload["minimumCpuClass"],
        CPU_CLASSES,
    ):
        return False, "insufficient_cpu_class"
    if not _class_at_least(
        node["memoryClass"],
        workload["minimumMemoryClass"],
        MEMORY_CLASSES,
    ):
        return False, "insufficient_memory_class"
    if not set(workload["requiredCapabilities"]).issubset(
        node["capabilities"]
    ):        return False, "node_capability_missing"
    if not set(workload["requiredCapabilities"]).issubset(
        eligibility[role].get("acceptedCapabilities", [])
    ):
        return False, "role_canary_capability_incomplete"
    requested_toolchain = workload["toolchainProfile"]
    if requested_toolchain is not None:
        if requested_toolchain not in node["toolchainProfiles"]:
            return False, "toolchain_profile_missing"
        if eligibility[role].get("toolchainProfile") != requested_toolchain:
            return False, "toolchain_profile_ineligible"
    if not _profile_has_capacity(
        node, workload, eligibility, capacity_values
    ):
        return False, "resource_profile_unmeasured"
    return True, "eligible"


def _workload_slot_capacities(
    node: dict[str, Any],
    workload: dict[str, Any],
    eligibility: dict[str, dict[str, Any]],
    capacity_values: Iterable[object],
) -> dict[str, int]:
    capacities: dict[str, int] = {}
    for value in capacity_values:
        receipt = validate_capacity(value)
        if (
            receipt["role"] != workload["role"]
            or not _capacity_matches_context(
                node, receipt, eligibility[workload["role"]]
            )
            or receipt["resourceProfile"]
            != workload["resourceProfile"]
            or receipt["result"] != "accepted"
        ):
            continue
        capacities[receipt["slotClass"]] = max(
            capacities.get(receipt["slotClass"], 0),
            receipt["maxConcurrent"],
        )
    return capacities


def local_admission(
    workload_value: object,
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
    held_slots: dict[str, int],
) -> dict[str, Any]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    workload = validate_workload(workload_value)
    node = validate_node(node_value)
    ok, reason = select_eligible(
        workload, node, canary_values, capacity_values
    )
    if not ok:
        return {
            "schema": DECISION_SCHEMA,
            "authority": "admission_only",
            "leaseBoundary": None,
            "nodeId": node["nodeId"],
            "operation": workload["operation"],
            "accepted": False,
            "reason": reason,
            "resourceProfile": workload["resourceProfile"],
            "physicalLeaseRequired": False,
            "slotClaims": [],
        }

    pressure = node["pressure"]
    if (
        pressure["cpu"] == "critical"
        or pressure["memory"] == "critical"
        or pressure["swap"] == "critical"
        or pressure["thermal"] == "critical"
    ):
        return {
            "schema": DECISION_SCHEMA,
            "authority": "admission_only",
            "leaseBoundary": None,
            "nodeId": node["nodeId"],
            "operation": workload["operation"],
            "accepted": False,
            "reason": "node_pressured",
            "resourceProfile": workload["resourceProfile"],
            "physicalLeaseRequired": False,
            "slotClaims": [],
        }

    eligibility = role_eligibility(node, canary_values)
    capacities = _workload_slot_capacities(
        node, workload, eligibility, capacity_values
    )
    claims = list(_slot_claims(workload))
    for slot in claims:
        if capacities.get(slot, 0) <= held_slots.get(slot, 0):
            return {
                "schema": DECISION_SCHEMA,
                "authority": "admission_only",
                "leaseBoundary": "physical_execution_lease",
                "nodeId": node["nodeId"],
                "operation": workload["operation"],
                "accepted": False,
                "reason": "physical_slot_unavailable",
                "resourceProfile": workload["resourceProfile"],
                "physicalLeaseRequired": True,
                "slotClaims": claims,
            }
    return {
        "schema": DECISION_SCHEMA,
        "authority": "admission_only",
        "leaseBoundary": "physical_execution_lease" if claims else None,
        "nodeId": node["nodeId"],
        "operation": workload["operation"],
        "accepted": True,
        "reason": "admitted",
        "resourceProfile": workload["resourceProfile"],
        "physicalLeaseRequired": bool(claims),
        "slotClaims": claims,
    }


def operator_status(
    node_value: object,
    canary_values: Iterable[object],
    capacity_values: Iterable[object],
    preference_values: Iterable[object] = (),
) -> dict[str, Any]:
    canary_values = list(canary_values)
    capacity_values = list(capacity_values)
    preference_values = list(preference_values)
    node = validate_node(node_value)
    eligibility = role_eligibility(node, canary_values)
    preferred: list[str] = []
    for value in preference_values:
        preference = validate_preference(value)
        if preference["nodeId"] != node["nodeId"]:
            raise RoleModelError(
                "preference belongs to a different node"
            )
        if preference["preference"] == "preferred":
            preferred.append(preference["role"])
    preferred = sorted(set(preferred))
    eligible = [
        role for role in ROLES if eligibility[role]["eligible"]
    ]
    unavailable = [
        {"role": role, "reason": eligibility[role]["reason"]}
        for role in ROLES
        if not eligibility[role]["eligible"]
        and eligibility[role]["reason"]
        not in {"platform_mismatch", "architecture_mismatch"}
    ]
    return {
        "schema": STATUS_SCHEMA,
        "node": node["nodeId"],
        "eligible": eligible,
        "temporarilyUnavailable": unavailable,
        "capacity": measured_slot_capacity(
            node, canary_values, capacity_values
        ),
        "preferred": preferred,
        "state": node["state"],
    }


def render_status(status_value: object) -> str:
    status = _exact(
        status_value,
        {
            "schema",
            "node",
            "eligible",
            "temporarilyUnavailable",
            "capacity",
            "preferred",
            "state",
        },
        "operator status",
    )
    if status["schema"] != STATUS_SCHEMA:
        raise RoleModelError("operator status schema is unsupported")
    lines = [f"node {status['node']}", "eligible:"]
    lines.extend(f"  {role}" for role in status["eligible"])
    lines.append("temporarily unavailable:")
    for item in status["temporarilyUnavailable"]:
        lines.append(f"  {item['role']}: {item['reason']}")
    lines.append("capacity:")
    lines.extend(
        f"  {slot}: {count}"
        for slot, count in status["capacity"].items()
    )
    lines.append("preferred:")
    lines.extend(f"  {role}" for role in status["preferred"])
    lines.append(f"state: {status['state']}")
    return "\n".join(lines) + "\n"


def render_json(value: object) -> str:
    return (
        json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        )
        + "\n"
    )