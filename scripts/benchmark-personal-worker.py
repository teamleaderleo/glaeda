#!/usr/bin/env python3
"""Measure bounded personal-worker CLI and durable-store paths."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable, Sequence

SCHEMA_VERSION = 1
REPORT_TYPE = "smolrunner-personal-worker-benchmark-report"
BASE_EPOCH_MILLIS = 50_000_000
QUEUE_SIZES = (10, 100, 256)
GIB = 1024 * 1024 * 1024
MAX_ERROR_BYTES = 4096


class BenchmarkError(RuntimeError):
    """A bounded benchmark setup or execution failure."""


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Measure installed SmolRunner personal-worker CLI paths with "
            "deterministic private fixtures."
        )
    )
    parser.add_argument(
        "--profile",
        choices=("debug", "release"),
        default="release",
        help="Cargo profile used for the installed binary (default: release).",
    )
    parser.add_argument(
        "--binary",
        type=Path,
        help="Use this already-built SmolRunner binary instead of Cargo's target path.",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="Do not run cargo build before measuring.",
    )
    parser.add_argument(
        "--samples",
        type=int,
        default=15,
        help="Recorded warm samples per benchmark (default: 15).",
    )
    parser.add_argument(
        "--warmups",
        type=int,
        default=3,
        help="Unrecorded warm-up runs per benchmark (default: 3).",
    )
    parser.add_argument(
        "--smoke",
        action="store_true",
        help="Use two samples and one warm-up while retaining the complete suite.",
    )
    parser.add_argument(
        "--output",
        default="-",
        help="Write JSON to this file, or '-' for stdout (default: '-').",
    )
    args = parser.parse_args(argv)
    if args.samples < 1 or args.samples > 10_000:
        parser.error("--samples must be between 1 and 10000")
    if args.warmups < 0 or args.warmups > 1_000:
        parser.error("--warmups must be between 0 and 1000")
    if args.smoke:
        args.samples = 2
        args.warmups = 1
    return args


def run_process(
    command: Sequence[os.PathLike[str] | str],
    *,
    cwd: Path,
) -> tuple[float, bytes, bytes]:
    started = time.perf_counter_ns()
    completed = subprocess.run(
        [os.fspath(part) for part in command],
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if completed.returncode != 0:
        stderr = completed.stderr[-MAX_ERROR_BYTES:].decode("utf-8", errors="replace")
        raise BenchmarkError(
            f"command failed with status {completed.returncode}: "
            f"{Path(os.fspath(command[0])).name}: {stderr}"
        )
    return elapsed_ms, completed.stdout, completed.stderr


def run_metadata(command: Sequence[str], *, cwd: Path) -> str:
    completed = subprocess.run(
        command,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
    )
    if completed.returncode != 0:
        raise BenchmarkError(f"metadata command failed: {command[0]}")
    return completed.stdout.strip()


def target_directory(repo_root: Path) -> Path:
    configured = os.environ.get("CARGO_TARGET_DIR")
    if not configured:
        return repo_root / "target"
    path = Path(configured)
    return path if path.is_absolute() else (repo_root / path).resolve()


def installed_binary(args: argparse.Namespace, repo_root: Path) -> Path:
    if args.binary is not None:
        binary = args.binary.expanduser().resolve()
    else:
        profile_dir = "release" if args.profile == "release" else "debug"
        suffix = ".exe" if os.name == "nt" else ""
        binary = target_directory(repo_root) / profile_dir / f"smolrunner{suffix}"
    if not args.skip_build and args.binary is None:
        command = ["cargo", "build", "--locked", "--bin", "smolrunner"]
        if args.profile == "release":
            command.append("--release")
        subprocess.run(command, cwd=repo_root, check=True)
    if not binary.is_file():
        raise BenchmarkError(f"installed binary is missing: {binary.name}")
    return binary


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return f"sha256:{digest.hexdigest()}"


def request_document(index: int) -> dict[str, Any]:
    submitted_at = BASE_EPOCH_MILLIS - 20_000 + index
    return {
        "identity": {
            "request_id": f"bench-{index:04d}",
            "verification_profile_id": "smolrunner.required",
            "runner_profile_id": "personal-lima-work",
        },
        "source": {
            "repository": "example/benchmark",
            "commit": "a" * 40,
            "tree": "b" * 40,
        },
        "priority": "normal",
        "requested_limits": {
            "cpu_millis": 2_000,
            "memory_bytes": 2 * GIB,
            "pids": 2_048,
        },
        "cache_namespace": {
            "class": "repository_build",
            "cache_id": "build-cache",
            "repository": "example/benchmark",
            "namespace_digest": f"sha256:{'ab' * 32}",
        },
        "cache_access": "write",
        "submitted_at": submitted_at,
        "operator_deadline": None,
        "cancellation": {"state": "active"},
        "fallback_eligibility": {"status": "ineligible"},
    }


def store_document(queue_size: int) -> dict[str, Any]:
    if queue_size < 0 or queue_size > 256:
        raise BenchmarkError("fixture queue size is outside the current accepted limit")
    return {
        "schema_version": 1,
        "revision": 1,
        "queue": {
            "generation": 1,
            "observed_at": BASE_EPOCH_MILLIS,
            "current_profile": "interactive",
            "last_activity_at": BASE_EPOCH_MILLIS - 1_000,
            "queued": [request_document(index) for index in range(queue_size)],
            "active": [],
            "pending_profile_change": None,
        },
        "cache_leases": [],
        "history": [],
    }


def canonical_store_bytes(queue_size: int) -> bytes:
    encoded = json.dumps(
        store_document(queue_size),
        ensure_ascii=True,
        indent=2,
        separators=(",", ": "),
    )
    return f"{encoded}\n".encode("utf-8")


def create_store_fixture(parent: Path, label: str, queue_size: int) -> tuple[Path, str]:
    root = parent / label
    root.mkdir(mode=0o750)
    os.chmod(root, 0o750)
    store = root / "personal-worker"
    store.mkdir(mode=0o750)
    os.chmod(store, 0o750)
    lock = store / "store.lock"
    lock.write_bytes(b"")
    os.chmod(lock, 0o600)
    current = store / "current.json"
    payload = canonical_store_bytes(queue_size)
    current.write_bytes(payload)
    os.chmod(current, 0o600)
    return root, f"sha256:{hashlib.sha256(payload).hexdigest()}"


def parse_json_output(stdout: bytes) -> dict[str, Any]:
    value = json.loads(stdout)
    if not isinstance(value, dict):
        raise BenchmarkError("command returned a non-object JSON document")
    return value


def percentile_nearest_rank(values: Sequence[float], percentile: float) -> float:
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[rank - 1]


def statistics_document(values: Sequence[float]) -> dict[str, float | int]:
    if not values:
        raise BenchmarkError("benchmark produced no samples")
    return {
        "sample_count": len(values),
        "min_ms": round(min(values), 3),
        "median_ms": round(statistics.median(values), 3),
        "p95_ms": round(percentile_nearest_rank(values, 0.95), 3),
        "max_ms": round(max(values), 3),
    }


def measure_read_only(
    *,
    benchmark_id: str,
    operation: str,
    command: Sequence[os.PathLike[str] | str],
    repo_root: Path,
    samples: int,
    warmups: int,
    validator: Callable[[bytes], None],
    queue_size: int | None = None,
    note: str,
) -> dict[str, Any]:
    first_ms, first_stdout, _ = run_process(command, cwd=repo_root)
    validator(first_stdout)
    for _ in range(warmups):
        _, stdout, _ = run_process(command, cwd=repo_root)
        validator(stdout)
    timings: list[float] = []
    for _ in range(samples):
        elapsed_ms, stdout, _ = run_process(command, cwd=repo_root)
        validator(stdout)
        timings.append(elapsed_ms)
    result: dict[str, Any] = {
        "id": benchmark_id,
        "operation": operation,
        "first_observation_ms": round(first_ms, 3),
        "warmup_count": warmups,
        **statistics_document(timings),
        "note": note,
    }
    if queue_size is not None:
        result["queue_size"] = queue_size
    return result


def measure_mutation(
    *,
    benchmark_id: str,
    operation: str,
    fixture_parent: Path,
    fixture_size: int,
    command_factory: Callable[[Path], Sequence[os.PathLike[str] | str]],
    repo_root: Path,
    samples: int,
    warmups: int,
    validator: Callable[[bytes], None],
    note: str,
) -> dict[str, Any]:
    counter = 0

    def one() -> float:
        nonlocal counter
        root, _ = create_store_fixture(
            fixture_parent,
            f"{benchmark_id}-{counter:05d}",
            fixture_size,
        )
        counter += 1
        try:
            elapsed_ms, stdout, _ = run_process(command_factory(root), cwd=repo_root)
            validator(stdout)
            return elapsed_ms
        finally:
            shutil.rmtree(root, ignore_errors=True)

    first_ms = one()
    for _ in range(warmups):
        one()
    timings = [one() for _ in range(samples)]
    return {
        "id": benchmark_id,
        "operation": operation,
        "fixture_queue_size": fixture_size,
        "first_observation_ms": round(first_ms, 3),
        "warmup_count": warmups,
        **statistics_document(timings),
        "note": note,
    }


def machine_class() -> dict[str, str]:
    cpu_count = os.cpu_count() or 1
    if cpu_count <= 4:
        cpu_class = "1-4-logical-cpus"
    elif cpu_count <= 8:
        cpu_class = "5-8-logical-cpus"
    elif cpu_count <= 16:
        cpu_class = "9-16-logical-cpus"
    else:
        cpu_class = "17-plus-logical-cpus"

    memory_class = "unknown"
    if hasattr(os, "sysconf"):
        try:
            pages = int(os.sysconf("SC_PHYS_PAGES"))
            page_size = int(os.sysconf("SC_PAGE_SIZE"))
            gib = (pages * page_size) / GIB
            if gib <= 8:
                memory_class = "up-to-8-gib"
            elif gib <= 16:
                memory_class = "over-8-to-16-gib"
            elif gib <= 32:
                memory_class = "over-16-to-32-gib"
            elif gib <= 64:
                memory_class = "over-32-to-64-gib"
            else:
                memory_class = "over-64-gib"
        except (OSError, ValueError):
            pass
    return {
        "os_family": platform.system().lower() or "unknown",
        "architecture": platform.machine().lower() or "unknown",
        "cpu_class": cpu_class,
        "memory_class": memory_class,
    }


def toolchain_document(repo_root: Path) -> dict[str, str]:
    verbose = run_metadata(["rustc", "-vV"], cwd=repo_root)
    fields: dict[str, str] = {}
    for line in verbose.splitlines():
        key, separator, value = line.partition(": ")
        if separator:
            fields[key] = value
    return {
        "rustc_release": fields.get("release", "unknown"),
        "host_target": fields.get("host", "unknown"),
    }


def source_document(repo_root: Path) -> dict[str, Any]:
    commit = run_metadata(["git", "rev-parse", "HEAD"], cwd=repo_root)
    dirty = bool(run_metadata(["git", "status", "--porcelain=v1"], cwd=repo_root))
    if len(commit) != 40 or any(character not in "0123456789abcdef" for character in commit):
        raise BenchmarkError("Git did not return an exact lowercase commit identity")
    return {"commit": commit, "dirty": dirty}


def submit_command(binary: Path, root: Path) -> list[str]:
    return [
        os.fspath(binary),
        "--output",
        "json",
        "queue",
        "submit",
        "--store-root",
        os.fspath(root),
        "--revision",
        "1",
        "--generation",
        "1",
        "--observed-at",
        str(BASE_EPOCH_MILLIS + 1_000),
        "--request-id",
        "bench-submit",
        "--verification-profile",
        "smolrunner.required",
        "--runner-profile",
        "personal-lima-work",
        "--repository",
        "example/benchmark-submit",
        "--commit",
        "c" * 40,
        "--tree",
        "d" * 40,
        "--priority",
        "normal",
        "--cpu-millis",
        "2000",
        "--memory-bytes",
        str(2 * GIB),
        "--pids",
        "2048",
        "--cache-id",
        "build-cache",
        "--cache-namespace-digest",
        f"sha256:{'cd' * 32}",
        "--cache-access",
        "write",
        "--submitted-at",
        str(BASE_EPOCH_MILLIS - 500),
    ]


def cancel_command(binary: Path, root: Path) -> list[str]:
    return [
        os.fspath(binary),
        "--output",
        "json",
        "job",
        "cancel",
        "--store-root",
        os.fspath(root),
        "--revision",
        "1",
        "--generation",
        "1",
        "--cancelled-at",
        str(BASE_EPOCH_MILLIS + 1_000),
        "bench-0000",
    ]


def produce_report(args: argparse.Namespace) -> dict[str, Any]:
    if os.name != "posix":
        raise BenchmarkError("personal-worker durable-store benchmarks require a Unix host")
    repo_root = Path(__file__).resolve().parent.parent
    if not (repo_root / "Cargo.toml").is_file():
        raise BenchmarkError("benchmark runner must execute from a SmolRunner checkout")
    binary = installed_binary(args, repo_root)

    with tempfile.TemporaryDirectory(prefix="smolrunner-q03-private-") as temporary:
        fixture_parent = Path(temporary)
        fixture_roots: dict[int, Path] = {}
        fixture_digests: dict[str, str] = {}
        for queue_size in QUEUE_SIZES:
            root, digest = create_store_fixture(
                fixture_parent,
                f"readonly-{queue_size}",
                queue_size,
            )
            fixture_roots[queue_size] = root
            fixture_digests[str(queue_size)] = digest

        benchmarks: list[dict[str, Any]] = []

        def validate_help(stdout: bytes) -> None:
            if b"Usage" not in stdout:
                raise BenchmarkError("help output is missing Usage")

        benchmarks.append(
            measure_read_only(
                benchmark_id="cli-startup-help",
                operation="installed CLI startup and argument rendering",
                command=[binary, "--help"],
                repo_root=repo_root,
                samples=args.samples,
                warmups=args.warmups,
                validator=validate_help,
                note="Includes process creation and bounded help rendering.",
            )
        )

        for queue_size in QUEUE_SIZES:
            root = fixture_roots[queue_size]

            def validate_status(stdout: bytes, expected: int = queue_size) -> None:
                value = parse_json_output(stdout)
                if value.get("store_revision") != 1 or value.get("queue_generation") != 1:
                    raise BenchmarkError("status output drifted from the exact fixture")
                queued = value.get("queued_count")
                if queued is not None and queued != expected:
                    raise BenchmarkError("status queued count drifted from the fixture")

            benchmarks.append(
                measure_read_only(
                    benchmark_id=f"worker-status-{queue_size}",
                    operation="store open, decode, canonical validation, and status projection",
                    command=[
                        binary,
                        "--output",
                        "json",
                        "worker",
                        "status",
                        "--store-root",
                        root,
                    ],
                    repo_root=repo_root,
                    samples=args.samples,
                    warmups=args.warmups,
                    validator=validate_status,
                    queue_size=queue_size,
                    note=(
                        "This is the current public proxy for durable decode/validation; "
                        "it also includes read-model projection and JSON rendering."
                    ),
                )
            )

            offsets = {
                "first": 0,
                "middle": queue_size // 2,
                "final": queue_size - 1,
            }
            for position, offset in offsets.items():

                def validate_page(
                    stdout: bytes,
                    expected_total: int = queue_size,
                    expected_offset: int = offset,
                ) -> None:
                    value = parse_json_output(stdout)
                    if value.get("total") != expected_total:
                        raise BenchmarkError("queue-page total drifted from the fixture")
                    items = value.get("items")
                    if not isinstance(items, list) or len(items) != 1:
                        raise BenchmarkError("queue-page result is not exactly one item")
                    actual_offset = value.get("offset")
                    if actual_offset is not None and actual_offset != expected_offset:
                        raise BenchmarkError("queue-page offset drifted from the request")

                benchmarks.append(
                    measure_read_only(
                        benchmark_id=f"queue-page-{position}-{queue_size}",
                        operation=f"{position} one-item queue page",
                        command=[
                            binary,
                            "--output",
                            "json",
                            "queue",
                            "list",
                            "--store-root",
                            root,
                            "--revision",
                            "1",
                            "--generation",
                            "1",
                            "--offset",
                            str(offset),
                            "--limit",
                            "1",
                        ],
                        repo_root=repo_root,
                        samples=args.samples,
                        warmups=args.warmups,
                        validator=validate_page,
                        queue_size=queue_size,
                        note=(
                            "Includes process creation, exact store load, pagination, "
                            "and JSON rendering."
                        ),
                    )
                )

        benchmarks.append(
            measure_mutation(
                benchmark_id="queue-submit-transaction",
                operation="exact submission durable transaction",
                fixture_parent=fixture_parent,
                fixture_size=10,
                command_factory=lambda root: submit_command(binary, root),
                repo_root=repo_root,
                samples=args.samples,
                warmups=args.warmups,
                validator=lambda stdout: parse_json_output(stdout),
                note=(
                    "Fixture creation is excluded. Timing includes process creation, "
                    "locking, validation, staged write, fsync, publication, and JSON rendering."
                ),
            )
        )
        benchmarks.append(
            measure_mutation(
                benchmark_id="job-cancel-transaction",
                operation="exact cancellation durable transaction",
                fixture_parent=fixture_parent,
                fixture_size=1,
                command_factory=lambda root: cancel_command(binary, root),
                repo_root=repo_root,
                samples=args.samples,
                warmups=args.warmups,
                validator=lambda stdout: parse_json_output(stdout),
                note=(
                    "Fixture creation is excluded. Timing includes process creation, "
                    "locking, validation, staged write, fsync, publication, and JSON rendering."
                ),
            )
        )

        report: dict[str, Any] = {
            "schema_version": SCHEMA_VERSION,
            "receipt_type": REPORT_TYPE,
            "source": source_document(repo_root),
            "binary": {
                "profile": args.profile if args.binary is None else "external",
                "sha256": sha256_file(binary),
            },
            "toolchain": toolchain_document(repo_root),
            "machine_class": machine_class(),
            "settings": {
                "sample_count": args.samples,
                "warmup_count": args.warmups,
                "queue_sizes": list(QUEUE_SIZES),
                "timings_are_correctness_gates": False,
            },
            "fixture_digests": fixture_digests,
            "benchmarks": benchmarks,
            "blocked_measurements": [
                {
                    "id": "status-1000-jobs",
                    "dependency": "current queue contract",
                    "reason": (
                        "The accepted queue limit is 256 entries; a 1000-job fixture "
                        "would benchmark an invalid document."
                    ),
                },
                {
                    "id": "terminal-completion-transaction",
                    "dependency": "W03 B07",
                    "reason": "No accepted terminal-completion operator path is integrated yet.",
                },
                {
                    "id": "broker-tick",
                    "dependency": "W03 B01",
                    "reason": (
                        "The existing broker foundation has no accepted Q03 installed-binary "
                        "benchmark entrypoint; product runtime files remain outside this lane."
                    ),
                },
                {
                    "id": "named-profile-cold-warm-execution",
                    "dependency": "W03 B05/B06 and W04",
                    "reason": "The bounded profile planner/executor journey is not integrated yet.",
                },
                {
                    "id": "lima-stopped-ready-and-work-idle",
                    "dependency": "W04 physical acceptance",
                    "reason": (
                        "These measurements require an approved physical Mac/Lima harness "
                        "and are not inferred from CI."
                    ),
                },
            ],
            "privacy": {
                "private_path_exposed": False,
                "environment_dumped": False,
                "credentials_read": False,
                "hostname_reported": False,
                "raw_command_output_reported": False,
            },
        }
        serialized = json.dumps(report, ensure_ascii=True, sort_keys=True)
        forbidden = [
            os.fspath(repo_root),
            os.fspath(Path.home()),
            os.fspath(fixture_parent),
        ]
        for sentinel in forbidden:
            if len(sentinel) > 1 and sentinel in serialized:
                raise BenchmarkError("public report contains a private path sentinel")
        return report


def write_report(report: dict[str, Any], output: str) -> None:
    encoded = f"{json.dumps(report, ensure_ascii=True, indent=2)}\n"
    if output == "-":
        sys.stdout.write(encoded)
        return
    destination = Path(output)
    destination.write_text(encoded, encoding="utf-8")


def main(argv: Sequence[str]) -> int:
    try:
        args = parse_args(argv)
        report = produce_report(args)
        write_report(report, args.output)
        return 0
    except (BenchmarkError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
