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
MAX_COMMAND_OUTPUT_BYTES = 16 * 1024
ENROLLABLE_ROLES = {
    "cmux_macos_native_build",
    "cmux_linux_ci",
}
ROLE_OS = {
    "cmux_macos_native_build": "macos",
    "cmux_macos_test": "macos",
    "cmux_linux_ci": "linux",
    "cmux_linux_agent": "linux",
    "artifact_cache": None,
    "background_replay": None,
    "benchmark": None,
    "diagnostic": None,
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


def run(
    argv: list[str],
    timeout: int = 10,
    cwd: Path | None = None,
) -> str:
    environment = {
        "LC_ALL": "C",
        "PATH": os.environ.get("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
    }
    for name in ("HOME", "CARGO_HOME", "RUSTUP_HOME", "DEVELOPER_DIR"):
        value = os.environ.get(name)
        if value:
            environment[name] = value
    result = subprocess.run(
        argv,
        check=False,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=environment,
        timeout=timeout,
        text=True,
    )
    if len(result.stdout.encode("utf-8", errors="replace")) > MAX_COMMAND_OUTPUT_BYTES:
        raise BootstrapError(
            f"required command output is too large: {Path(argv[0]).name}"
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


def role_workload_generations(cmux_root: Path, roles: list[str]) -> dict[str, str]:
    workload = cmux_root / "scripts/fleet_acceptance.py"
    if not workload.is_file():
        raise BootstrapError("CMUX fleet acceptance workload is unavailable")
    generation = digest_file(workload)
    return {role: generation for role in sorted(set(roles))}


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


def cmux_checkout_clean(root: Path) -> bool:
    return (
        run(
            [
                executable("git"),
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
            ],
            cwd=root,
        )
        == ""
    )


def cmux_submodules_ready(root: Path) -> bool:
    output = run(
        [executable("git"), "submodule", "status", "--recursive"],
        cwd=root,
    )
    lines = [line for line in output.splitlines() if line]
    return bool(lines) and all(line[0] == " " for line in lines)


def cmux_required_zig_version(root: Path) -> str:
    manifest = root / "ghostty/build.zig.zon"
    match = re.search(
        r'^\s*\.minimum_zig_version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"',
        manifest.read_text(encoding="utf-8"),
        re.MULTILINE,
    )
    if match is None:
        raise BootstrapError("Ghostty minimum Zig version is unavailable")
    return match.group(1)


def zig_version_compatible(actual: str, required: str) -> bool:
    def parse(value: str) -> tuple[int, int, int] | None:
        core = re.split(r"[-+]", value, maxsplit=1)[0]
        match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", core)
        if match is None:
            return None
        return tuple(int(part) for part in match.groups())

    actual_parts = parse(actual)
    required_parts = parse(required)
    return bool(
        actual_parts
        and required_parts
        and actual_parts[:2] == required_parts[:2]
        and actual_parts[2] >= required_parts[2]
    )


def cmux_diff_rust_toolchain(root: Path) -> str:
    content = (root / "Native/DiffSidecar/rust-toolchain.toml").read_text(
        encoding="utf-8"
    )
    match = re.search(
        r'^\s*channel\s*=\s*"([^"]+)"',
        content,
        re.MULTILINE,
    )
    if match is None:
        raise BootstrapError("CMUX DiffSidecar Rust toolchain is unavailable")
    return match.group(1)


def collect_macos(
    cmux_root: Path,
    glaeda: Path,
    min_free_gib: int,
    hardware_class: str,
    cache_root: Path | None,
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
    xcrun = executable("xcrun")
    sdk = run([xcrun, "--sdk", "macosx", "--show-sdk-version"])
    metal = run([xcrun, "metal", "--version"])
    git = run([executable("git"), "--version"])
    zig = run([executable("zig"), "version"])
    zig_required = cmux_required_zig_version(cmux_root)
    rustup = executable("rustup")
    rustup_version = run([rustup, "--version"])
    cargo = run([executable("cargo"), "--version"])
    rustc = run([executable("rustc"), "--version"])
    diff_rust = cmux_diff_rust_toolchain(cmux_root)
    diff_cargo = run([rustup, "run", diff_rust, "cargo", "--version"])
    diff_rustc = run([rustup, "run", diff_rust, "rustc", "--version"])
    pmset = run([executable("pmset"), "-g", "custom"])
    xcode_match = re.search(r"^Xcode\s+(\d+(?:\.\d+)*)$", xcode, re.MULTILINE)
    sdk_match = re.fullmatch(r"(\d+)(?:\.\d+)*", sdk)
    toolchain = {
        "cmuxXcodePin": pin,
        "xcodeVersion": xcode_match.group(1) if xcode_match else "unknown",
        "macosSdkVersion": sdk,
        "gitVersion": git,
        "metalVersion": metal,
        "zigVersion": zig,
        "zigRequired": zig_required,
        "rustupVersion": rustup_version,
        "cargoVersion": cargo,
        "rustcVersion": rustc,
        "diffRustToolchain": diff_rust,
        "diffCargoVersion": diff_cargo,
        "diffRustcVersion": diff_rustc,
    }
    free_gib = disk_free_gib(cmux_root)
    cache_required = cache_root is not None
    cache_ready = (
        cache_root is not None
        and cache_root.is_dir()
        and os.access(cache_root, os.W_OK | os.X_OK)
    )
    cache_free_gib = disk_free_gib(cache_root) if cache_ready and cache_root else 0
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
            "canonicalCheckoutClean": cmux_checkout_clean(cmux_root),
            "submodulesReady": cmux_submodules_ready(cmux_root),
            "cmuxSetupArtifacts": (
                (cmux_root / "ghostty/include/ghostty.h").is_file()
                and (
                    cmux_root / "ghostty/macos/GhosttyKit.xcframework"
                ).is_dir()
            ),
            "xcodePin": bool(
                xcode_match
                and sdk_match
                and pin == "26.0"
                and xcode_match.group(1).startswith("26")
                and int(sdk_match.group(1)) == 26
            ),
            "git": git.startswith("git version "),
            "zig": zig_version_compatible(zig, zig_required),
            "rust": (
                rustup_version.startswith("rustup ")
                and cargo.startswith("cargo ")
                and rustc.startswith("rustc ")
                and diff_cargo.startswith("cargo ")
                and diff_rustc.startswith("rustc ")
            ),
            "metalToolchain": bool(metal),
            "glaedaExecutable": glaeda.is_file() and os.access(glaeda, os.X_OK),
            "diskAdmission": free_gib >= min_free_gib,
            "nativeCacheRoot": (not cache_required) or cache_ready,
            "nativeCacheDiskAdmission": (
                (not cache_required)
                or (cache_ready and cache_free_gib >= min_free_gib)
            ),
            "unattendedPower": mac_sleep_disabled_on_ac(pmset),
        },
        "observed": {
            "freeDiskGiBClass": (
                f"ge-{min_free_gib}" if free_gib >= min_free_gib else f"lt-{min_free_gib}"
            ),
            "xcodePin": pin,
            "nativeCacheFreeDiskGiBClass": (
                "unconfigured"
                if not cache_required
                else (
                    f"ge-{min_free_gib}"
                    if cache_ready and cache_free_gib >= min_free_gib
                    else f"lt-{min_free_gib}"
                )
            ),
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
            "canonicalCheckoutClean": cmux_checkout_clean(cmux_root),
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
    unreviewed = [role for role in roles if role not in ENROLLABLE_ROLES]
    if unreviewed:
        raise BootstrapError(
            "bootstrap role lacks a reviewed v1 acceptance workload: "
            + ",".join(unreviewed)
        )
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
    workload_generations = observation.get("roleWorkloadGenerations")
    if (
        not isinstance(workload_generations, dict)
        or set(workload_generations) != set(roles)
        or any(
            not isinstance(value, str)
            or re.fullmatch(r"sha256:[0-9a-f]{64}", value) is None
            for value in workload_generations.values()
        )
    ):
        raise BootstrapError("role workload generations are invalid")
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
        "roleWorkloadGenerations": workload_generations,
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
    p.add_argument(
        "--cache-root",
        type=Path,
        help="Existing operator-owned native build/cache root; path is never emitted",
    )
    p.add_argument("--min-free-gib", type=int)
    return p


def main() -> int:
    try:
        args = parser().parse_args()
        cmux_root = args.cmux_root.resolve(strict=True)
        glaeda = args.glaeda.resolve(strict=True)
        cache_root = (
            args.cache_root.resolve(strict=True)
            if args.cache_root is not None
            else None
        )
        if (
            args.platform == "macos"
            and any(
                role == "cmux_macos_native_build"
                for role in args.role
            )
            and cache_root is None
        ):
            raise BootstrapError(
                "macOS native-build role requires --cache-root"
            )
        minimum = args.min_free_gib or (120 if args.platform == "macos" else 40)
        if minimum <= 0:
            raise BootstrapError("minimum free disk must be positive")
        observation = (
            collect_macos(
                cmux_root,
                glaeda,
                minimum,
                args.hardware_class,
                cache_root,
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
        observation["roleWorkloadGenerations"] = role_workload_generations(
            cmux_root,
            args.role,
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
