#!/usr/bin/env python3
"""Read-only CMUX fleet bootstrap observation for macOS and Linux nodes."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

SCHEMA = "glaeda-cmux-fleet-bootstrap/v1"
MAX_OUTPUT_BYTES = 16 * 1024
ROLE_OS = {
    "cmux_macos_native_build": "macos",
    "cmux_macos_test": "macos",
    "cmux_linux_ci": "linux",
    "cmux_linux_agent": "linux",
    "artifact_cache": None,
    "background_replay": None,
    "benchmark": None,
}


class BootstrapError(RuntimeError):
    pass


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest_bytes(value: bytes) -> str:
    return "sha256:" + hashlib.sha256(value).hexdigest()


def digest_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return "sha256:" + h.hexdigest()


def run(argv: list[str], timeout: int = 10) -> str:
    result = subprocess.run(
        argv,
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env={
            "LC_ALL": "C",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        },
        timeout=timeout,
        text=True,
    )
    if result.returncode != 0:
        raise BootstrapError(f"required command failed: {Path(argv[0]).name}")
    return result.stdout.strip()


def executable(name: str) -> str:
    value = shutil.which(name)
    if value is None:
        raise BootstrapError(f"required command is missing: {name}")
    return os.path.abspath(value)


def normalize_arch(value: str) -> str:
    if value in {"arm64", "aarch64"}:
        return "arm64"
    if value == "x86_64":
        return value
    raise BootstrapError(f"unsupported architecture: {value}")


def disk_free_gib(path: Path) -> int:
    return shutil.disk_usage(path).free // (1024**3)


def mac_sleep_disabled_on_ac(raw: str) -> bool:
    in_ac = False
    for line in raw.splitlines():
        stripped = line.strip()
        if stripped == "AC Power:":
            in_ac = True
            continue
        if stripped.endswith("Power:"):
            in_ac = False
            continue
        if in_ac:
            match = re.fullmatch(r"sleep\s+(\d+)", stripped)
            if match:
                return int(match.group(1)) == 0
    return False


def collect_macos(
    cmux_root: Path,
    glaeda: Path,
    min_free_gib: int,
    hardware_class: str,
) -> dict[str, Any]:
    if platform.system() != "Darwin":
        raise BootstrapError("macOS bootstrap requires Darwin")
    version = platform.mac_ver()[0]
    match = re.fullmatch(r"(\d+)\.\d+(?:\.\d+)?", version)
    if match is None:
        raise BootstrapError("macOS version is unavailable")
    major = int(match.group(1))
    pin = (cmux_root / ".xcode-version").read_text(encoding="utf-8").strip()
    xcode = run([executable("xcodebuild"), "-version"])
    sdk = run([executable("xcrun"), "--sdk", "macosx", "--show-sdk-version"])
    git = run([executable("git"), "--version"])
    pmset = run([executable("pmset"), "-g", "custom"])
    xcode_match = re.search(r"^Xcode\s+(\d+(?:\.\d+)*)$", xcode, re.MULTILINE)
    sdk_match = re.fullmatch(r"(\d+)(?:\.\d+)*", sdk)
    toolchain = {
        "cmuxXcodePin": pin,
        "xcodeVersion": xcode_match.group(1) if xcode_match else "unknown",
        "macosSdkVersion": sdk,
        "gitVersion": git,
    }
    free_gib = disk_free_gib(cmux_root)
    cpus = os.cpu_count() or 0
    memory_gib = mac_total_memory_gib()
    return {
        "platform": "macos",
        "architecture": normalize_arch(platform.machine()),
        "osVersionClass": f"macos-{major}",
        "glaedaGeneration": digest_file(glaeda),
        "toolchainGeneration": digest_bytes(canonical(toolchain)),
        "checks": {
            "supportedOs": major in {15, 26},
            "hardwareCapability": hardware_class_ready(
                "macos",
                hardware_class,
                cpus,
                memory_gib,
            ),
            "cmuxCheckout": (cmux_root / ".git").exists()
            and (cmux_root / ".xcode-version").is_file(),
            "xcodePin": bool(
                xcode_match
                and sdk_match
                and pin == "26.0"
                and xcode_match.group(1).startswith("26")
                and int(sdk_match.group(1)) == 26
            ),
            "git": git.startswith("git version "),
            "glaedaExecutable": glaeda.is_file() and os.access(glaeda, os.X_OK),
            "diskAdmission": free_gib >= min_free_gib,
            "unattendedPower": mac_sleep_disabled_on_ac(pmset),
        },
        "observed": {
            "freeDiskGiBClass": (
                f"ge-{min_free_gib}" if free_gib >= min_free_gib else f"lt-{min_free_gib}"
            ),
            "xcodePin": pin,
            "logicalCpuClass": "ge-8" if cpus >= 8 else "lt-8",
            "totalMemoryGiBClass": (
                "ge-16" if memory_gib >= 16 else "lt-16"
            ),
        },
    }


def read_os_release() -> tuple[str, str]:
    values: dict[str, str] = {}
    for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key] = value.strip().strip('"')
    return values.get("ID", ""), values.get("VERSION_ID", "")


def linux_memory_gib(field: str) -> int:
    for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
        if line.startswith(field + ":"):
            return int(line.split()[1]) // (1024**2)
    raise BootstrapError(f"{field} is unavailable")


def mac_total_memory_gib() -> int:
    raw = run([executable("sysctl"), "-n", "hw.memsize"])
    return int(raw) // (1024**3)


def hardware_class_ready(
    platform_name: str,
    hardware_class: str,
    cpus: int,
    memory_gib: int,
) -> bool:
    minimums = {
        ("macos", "cmux-mac-build-large"): (8, 16),
        ("macos", "cmux-mac-test-large"): (8, 16),
        ("linux", "cmux-linux-ci-medium"): (4, 8),
        ("linux", "cmux-linux-agent-medium"): (4, 8),
    }
    required = minimums.get((platform_name, hardware_class))
    return (
        required is not None
        and cpus >= required[0]
        and memory_gib >= required[1]
    )


def collect_linux(
    cmux_root: Path,
    glaeda: Path,
    min_free_gib: int,
    roles: list[str],
    hardware_class: str,
) -> dict[str, Any]:
    if platform.system() != "Linux":
        raise BootstrapError("Linux bootstrap requires Linux")
    distro, version = read_os_release()
    git = run([executable("git"), "--version"])
    python = run([executable("python3"), "--version"])
    systemd = run([executable("systemctl"), "--version"]).splitlines()[0]
    bwrap = run([executable("bwrap"), "--version"])
    kernel_major = int(platform.release().split(".", 1)[0])
    actions_ok = True
    if "cmux_linux_ci" in roles:
        for name in ("curl", "tar", "gzip", "ldd"):
            actions_ok = actions_ok and shutil.which(name) is not None
    toolchain = {
        "distribution": f"{distro}-{version}",
        "kernelMajor": kernel_major,
        "gitVersion": git,
        "pythonVersion": python,
    }
    supported_distro = (distro, version) in {
        ("ubuntu", "24.04"),
        ("debian", "12"),
    }
    cgroup2 = Path("/sys/fs/cgroup/cgroup.controllers").is_file()
    pressure = all(
        Path(f"/proc/pressure/{name}").is_file()
        for name in ("cpu", "memory", "io")
    )
    available_gib = linux_memory_gib("MemAvailable")
    total_gib = linux_memory_gib("MemTotal")
    cpus = os.cpu_count() or 0
    free_gib = disk_free_gib(cmux_root)
    return {
        "platform": "linux",
        "architecture": normalize_arch(platform.machine()),
        "osVersionClass": f"{distro}-{version}",
        "glaedaGeneration": digest_file(glaeda),
        "toolchainGeneration": digest_bytes(canonical(toolchain)),
        "checks": {
            "supportedOs": supported_distro and kernel_major >= 6,
            "hardwareCapability": hardware_class_ready(
                "linux",
                hardware_class,
                cpus,
                total_gib,
            ),
            "cmuxCheckout": (cmux_root / ".git").exists(),
            "git": git.startswith("git version "),
            "glaedaExecutable": glaeda.is_file() and os.access(glaeda, os.X_OK),
            "systemd": systemd.startswith("systemd "),
            "bubblewrap": bwrap.startswith("bubblewrap "),
            "cgroupV2": cgroup2,
            "pressureSignals": pressure,
            "diskAdmission": free_gib >= min_free_gib,
            "memoryAdmission": available_gib >= 8,
            "actionsPrerequisites": actions_ok,
        },
        "observed": {
            "freeDiskGiBClass": (
                f"ge-{min_free_gib}" if free_gib >= min_free_gib else f"lt-{min_free_gib}"
            ),
            "availableMemoryGiBClass": (
                "ge-8" if available_gib >= 8 else "lt-8"
            ),
            "logicalCpuClass": "ge-4" if cpus >= 4 else "lt-4",
            "totalMemoryGiBClass": (
                "ge-8" if total_gib >= 8 else "lt-8"
            ),
        },
    }


def evaluate(
    observation: dict[str, Any],
    roles: list[str],
    hardware_class: str,
) -> dict[str, Any]:
    platform_name = observation.get("platform")
    if platform_name not in {"macos", "linux"}:
        raise BootstrapError("bootstrap platform is invalid")
    roles = sorted(set(roles))
    if not roles or any(role not in ROLE_OS for role in roles):
        raise BootstrapError("bootstrap roles are invalid")
    for role in roles:
        required = ROLE_OS[role]
        if required is not None and required != platform_name:
            raise BootstrapError(f"role {role} is incompatible with {platform_name}")
    if re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,79}", hardware_class) is None:
        raise BootstrapError("hardware capability class is invalid")
    checks = observation.get("checks")
    if (
        not isinstance(checks, dict)
        or not checks
        or any(type(value) is not bool for value in checks.values())
    ):
        raise BootstrapError("bootstrap checks are invalid")
    failures = sorted(key for key, passed in checks.items() if not passed)
    result = {
        "schema": SCHEMA,
        "platform": platform_name,
        "architecture": observation["architecture"],
        "osVersionClass": observation["osVersionClass"],
        "hardwareCapabilityClass": hardware_class,
        "roles": roles,
        "glaedaGeneration": observation["glaedaGeneration"],
        "toolchainGeneration": observation["toolchainGeneration"],
        "checks": checks,
        "observed": observation.get("observed", {}),
        "eligibleForEnrollment": not failures,
        "blockingChecks": failures,
        "authority": "observation_only",
    }
    if len(canonical(result)) > MAX_OUTPUT_BYTES:
        raise BootstrapError("bootstrap receipt exceeds size ceiling")
    return result


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--platform", required=True, choices=("macos", "linux"))
    p.add_argument("--cmux-root", type=Path, required=True)
    p.add_argument("--glaeda", type=Path, required=True)
    p.add_argument("--hardware-class", required=True)
    p.add_argument("--role", action="append", required=True)
    p.add_argument("--min-free-gib", type=int)
    return p


def main() -> int:
    try:
        args = parser().parse_args()
        cmux_root = args.cmux_root.resolve(strict=True)
        glaeda = args.glaeda.resolve(strict=True)
        minimum = args.min_free_gib or (120 if args.platform == "macos" else 40)
        if minimum <= 0:
            raise BootstrapError("minimum free disk must be positive")
        observation = (
            collect_macos(
                cmux_root,
                glaeda,
                minimum,
                args.hardware_class,
            )
            if args.platform == "macos"
            else collect_linux(
                cmux_root,
                glaeda,
                minimum,
                args.role,
                args.hardware_class,
            )
        )
        sys.stdout.buffer.write(
            canonical(evaluate(observation, args.role, args.hardware_class))
        )
        return 0
    except (
        OSError,
        ValueError,
        BootstrapError,
        subprocess.TimeoutExpired,
    ) as error:
        print(
            json.dumps({"error": str(error)}, sort_keys=True, separators=(",", ":")),
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
