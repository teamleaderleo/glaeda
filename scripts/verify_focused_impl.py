#!/usr/bin/env python3
"""Execute one fixed Glaeda verification profile without source credentials.

The caller supplies identities, never a command, argv, environment, executable, remote URL, or
mutable ref. Local workstation configuration supplies the exact resident repository, state,
Cargo, and rustup roots. Repository code runs only inside the closed bubblewrap/systemd boundary.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import math
import os
import re
import selectors
import shutil
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import IO, NoReturn


SCHEMA_VERSION = 1
EXECUTION_IDENTITY_CLASS = "credentialless_project"
RUST_TOOLCHAIN = "1.97.1-x86_64-unknown-linux-gnu"
MAX_CONTROL_OUTPUT_BYTES = 64 * 1024
MAX_RECEIPT_BYTES = 32 * 1024
MAX_SOURCE_OUTPUT_BYTES = 1024 * 1024
FAILURE_TAIL_BYTES = 8 * 1024
MAX_MATERIALIZED_SOURCE_BYTES = 512 * 1024 * 1024
MAX_MATERIALIZED_SOURCE_ENTRIES = 100_000
TARGET_TMPFS_BYTES = 6 * 1024 * 1024 * 1024
CARGO_HOME_TMPFS_BYTES = 512 * 1024 * 1024
TEMP_TMPFS_BYTES = 512 * 1024 * 1024
PROJECT_HOME_TMPFS_BYTES = 64 * 1024 * 1024
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
OID_PATTERN = re.compile(r"^[a-f0-9]{40}$")
REPOSITORY_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
@dataclass(frozen=True)
class Profile:
    profile_id: str
    profile_class: str
    resource_class: str
    deadline_seconds: int
    recipe_name: str
    document_slug: str


def fixed_profile(
    profile_id: str,
    profile_class: str,
    resource_class: str,
    deadline_seconds: int,
    recipe_name: str,
    document_slug: str,
) -> Profile:
    return Profile(
        profile_id,
        profile_class,
        resource_class,
        deadline_seconds,
        recipe_name,
        document_slug,
    )


def profile_spec(profile: Profile) -> dict[str, object]:
    return {
        "schema_version": 1,
        "profile_id": profile.profile_id,
        "profile_class": profile.profile_class,
        "execution_identity_class": EXECUTION_IDENTITY_CLASS,
        "recipe": ["scripts/verify", profile.recipe_name],
        "rust_toolchain": RUST_TOOLCHAIN,
        "cargo_build_jobs": 4,
        "cargo_network": "offline",
        "git_optional_locks": False,
        "source_network": "none",
        "source_output": "controller_private_bounded_digest",
        "source_output_bytes": MAX_SOURCE_OUTPUT_BYTES,
        "source_state": "exact_read_only",
        "materialized_source_bytes_max": MAX_MATERIALIZED_SOURCE_BYTES,
        "build_state": "task_private_tmpfs",
        "build_tmpfs_bytes": TARGET_TMPFS_BYTES,
        "package_cache": "host_public_crates_io_read_only",
        "resource_class": profile.resource_class,
        "deadline_seconds": profile.deadline_seconds,
        "systemd_properties": [
            "CPUQuota=400%",
            "MemoryHigh=6G",
            "MemoryMax=8G",
            "TasksMax=512",
            f"RuntimeMaxSec={profile.deadline_seconds}",
            "KillMode=mixed",
            "NoNewPrivileges=yes",
            "RestrictSUIDSGID=yes",
        ],
    }


FOCUSED_PROFILE = fixed_profile(
    "verify-focused/v1", "verify_focused", "big-red-focused", 600, "focused", "verify-focused"
)
REQUIRED_PROFILE = fixed_profile(
    "verify-required/v1", "verify_required", "big-red-required", 1200, "required", "verify-required"
)

# Compatibility aliases for the focused profile and its existing tests/consumers.
PROFILE_ID = FOCUSED_PROFILE.profile_id
PROFILE_CLASS = FOCUSED_PROFILE.profile_class
RESOURCE_CLASS = FOCUSED_PROFILE.resource_class
DEADLINE_SECONDS = FOCUSED_PROFILE.deadline_seconds
PROFILE_SPEC = profile_spec(FOCUSED_PROFILE)
REQUIRED_PROFILE_SPEC = profile_spec(REQUIRED_PROFILE)

ISOLATION_SPEC = {
    "filesystem": "read_only_source_task_private_tmpfs",
    "network": "none",
    "ambient_environment": "cleared",
    "publisher_credentials": "absent",
    "control_credentials": "absent",
    "ssh_agent": "absent",
    "sudo_admin": "absent",
    "unrelated_writable_projects": "absent",
    "build_state": "task_private_tmpfs",
    "package_cache": "host_public_crates_io_read_only",
}
RECEIPT_KEYS = (
    "document_type",
    "schema_version",
    "authority",
    "command_fingerprint",
    "source",
    "profile",
    "execution_identity_class",
    "isolation",
    "result",
    "contains_private_content",
    "contains_credentials",
    "authorizes_work",
    "authorizes_effects",
    "authorizes_redispatch",
)
RESULT_KEYS = (
    "terminal_class",
    "exit_code",
    "elapsed_seconds",
    "started_at_unix_millis",
    "settled_at_unix_millis",
    "process_tree_settled",
    "task_cleanup_complete",
    "raw_output",
    "output_bytes",
    "output_sha256",
    "cpu_seconds",
    "max_rss_kib",
    "resource_accounting",
)


@dataclass(frozen=True)
class Request:
    repository: str
    commit: str
    tree: str
    profile_generation: str
    command_fingerprint: str
    profile: Profile = FOCUSED_PROFILE


class Refusal(RuntimeError):
    pass


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def exact_keys(value: dict[str, object], expected: tuple[str, ...]) -> bool:
    return set(value) == set(expected)


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def profile_generation(profile: Profile = FOCUSED_PROFILE) -> str:
    return sha256(canonical_bytes(profile_spec(profile)))


def exact_directory(raw: str, label: str) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        raise Refusal(f"{label} must be an absolute directory")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise Refusal(f"{label} is unavailable") from error
    if resolved != path or not path.is_dir() or path.is_symlink():
        raise Refusal(f"{label} must be one canonical plain directory")
    return path


def private_state_directory(raw: str) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        raise Refusal("state root must be absolute")
    parent = path.parent.resolve(strict=True)
    if parent != path.parent or not parent.is_dir() or parent.is_symlink():
        raise Refusal("state root parent is unavailable")
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    resolved = path.resolve(strict=True)
    metadata = path.lstat()
    if (
        resolved != path
        or not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise Refusal("state root is not a private current-user directory")
    return path


def normalize_request(
    arguments: argparse.Namespace, profile: Profile = FOCUSED_PROFILE
) -> Request:
    repository = arguments.repository
    commit = arguments.commit
    tree = arguments.tree
    generation = arguments.profile_generation
    fingerprint = arguments.command_fingerprint
    if not REPOSITORY_PATTERN.fullmatch(repository):
        raise Refusal("source repository identity is invalid")
    if not OID_PATTERN.fullmatch(commit) or not OID_PATTERN.fullmatch(tree):
        raise Refusal("source commit/tree identity is invalid")
    if generation != profile_generation(profile):
        raise Refusal(f"profile generation does not match {profile.profile_id}")
    if not SHA256_PATTERN.fullmatch(fingerprint):
        raise Refusal("command fingerprint is invalid")
    return Request(repository, commit, tree, generation, fingerprint, profile)


def closed_environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    environment = {
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
    }
    if extra:
        environment.update(extra)
    return environment


def run_control(argv: list[str], cwd: Path | None = None) -> bytes:
    completed = subprocess.run(
        argv,
        cwd=cwd,
        env=closed_environment(),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=60,
        check=False,
    )
    if len(completed.stdout) > MAX_CONTROL_OUTPUT_BYTES:
        raise Refusal("control command output exceeded its fixed ceiling")
    if completed.returncode != 0:
        raise Refusal("exact source materialization or observation failed")
    return completed.stdout


def git_text(repository_root: Path, *arguments: str) -> str:
    return run_control(["/usr/bin/git", *arguments], repository_root).decode(
        "ascii", errors="strict"
    ).strip()


def verify_resident_source(repository_root: Path, request: Request) -> None:
    commit, tree = git_text(
        repository_root, "rev-parse", f"{request.commit}^{{commit}}", f"{request.commit}^{{tree}}"
    ).splitlines()
    if commit != request.commit or tree != request.tree:
        raise Refusal("resident Git source does not match the exact request")
    remotes = git_text(repository_root, "remote", "get-url", "--all", "origin").splitlines()
    admitted = {
        f"git@github.com:{request.repository}.git",
        f"https://github.com/{request.repository}.git",
        f"https://github.com/{request.repository}",
    }
    if len(remotes) != 1 or remotes[0] not in admitted:
        raise Refusal("resident Git origin does not match the exact repository")


def ensure_private_child(parent: Path, name: str) -> Path:
    path = parent / name
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        metadata = path.lstat()
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or metadata.st_uid != os.getuid()
            or stat.S_IMODE(metadata.st_mode) != 0o700
        ):
            raise Refusal("command state contains an unsafe filesystem object")
    return path


def open_lock(directory: Path) -> IO[bytes]:
    descriptor = os.open(
        directory / "lock",
        os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size != 0
    ):
        os.close(descriptor)
        raise Refusal("command lock is not one private stable empty file")
    lock = os.fdopen(descriptor, "r+b", buffering=0)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        lock.close()
        raise Refusal("the exact verification command is already active") from error
    return lock


def read_document(path: Path) -> dict[str, object] | None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return None
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size > MAX_RECEIPT_BYTES
    ):
        raise Refusal("verification state contains an unsafe document")
    raw = path.read_bytes()
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise Refusal("verification state is corrupt") from error
    if not isinstance(value, dict) or canonical_bytes(value) + b"\n" != raw:
        raise Refusal("verification state is noncanonical")
    return value


def publish_document(path: Path, value: dict[str, object], *, replace: bool) -> None:
    raw = canonical_bytes(value) + b"\n"
    if len(raw) > MAX_RECEIPT_BYTES:
        raise Refusal("verification document exceeds its fixed ceiling")
    temporary = path.parent / f".{path.name}.creating-{os.getpid()}"
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb", closefd=True) as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        if replace:
            os.replace(temporary, path)
        else:
            try:
                os.link(temporary, path, follow_symlinks=False)
            except FileExistsError as error:
                raise Refusal("terminal verification document already exists") from error
            temporary.unlink()
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def matches_request(document: dict[str, object], request: Request) -> bool:
    source = document.get("source")
    profile = document.get("profile")
    return (
        document.get("schema_version") == SCHEMA_VERSION
        and document.get("command_fingerprint") == request.command_fingerprint
        and isinstance(source, dict)
        and source.get("repository") == request.repository
        and source.get("commit") == request.commit
        and source.get("tree") == request.tree
        and isinstance(profile, dict)
        and profile.get("id") == request.profile.profile_id
        and profile.get("generation") == request.profile_generation
    )


def valid_terminal_receipt(document: dict[str, object], request: Request) -> bool:
    source = document.get("source")
    profile = document.get("profile")
    result = document.get("result")
    isolation = document.get("isolation")
    terminal = result.get("terminal_class") if isinstance(result, dict) else None
    cleanup_complete = (
        result.get("task_cleanup_complete") if isinstance(result, dict) else None
    )
    return (
        exact_keys(document, RECEIPT_KEYS)
        and matches_request(document, request)
        and document.get("document_type")
        == f"glaeda-{request.profile.document_slug}-receipt"
        and document.get("authority") == "physical_execution_observation"
        and document.get("execution_identity_class") == EXECUTION_IDENTITY_CLASS
        and document.get("contains_private_content") is False
        and document.get("contains_credentials") is False
        and document.get("authorizes_work") is False
        and document.get("authorizes_effects") is False
        and document.get("authorizes_redispatch") is False
        and source
        == {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        }
        and profile
        == {
            "id": request.profile.profile_id,
            "class": request.profile.profile_class,
            "generation": request.profile_generation,
            "resource_class": request.profile.resource_class,
            "deadline_seconds": request.profile.deadline_seconds,
        }
        and isinstance(result, dict)
        and exact_keys(result, RESULT_KEYS)
        and terminal in {"succeeded", "failed", "timed_out", "cleanup_incomplete"}
        and isinstance(result.get("exit_code"), int)
        and not isinstance(result.get("exit_code"), bool)
        and (terminal != "succeeded" or result["exit_code"] == 0)
        and isinstance(result.get("elapsed_seconds"), (int, float))
        and not isinstance(result.get("elapsed_seconds"), bool)
        and math.isfinite(result["elapsed_seconds"])
        and result["elapsed_seconds"] >= 0
        and isinstance(result.get("started_at_unix_millis"), int)
        and not isinstance(result.get("started_at_unix_millis"), bool)
        and isinstance(result.get("settled_at_unix_millis"), int)
        and not isinstance(result.get("settled_at_unix_millis"), bool)
        and result["settled_at_unix_millis"] >= result["started_at_unix_millis"] >= 0
        and result.get("process_tree_settled") is True
        and isinstance(cleanup_complete, bool)
        and (terminal == "cleanup_incomplete") == (cleanup_complete is False)
        and result.get("raw_output") == "not_published"
        and isinstance(result.get("output_bytes"), int)
        and not isinstance(result.get("output_bytes"), bool)
        and result["output_bytes"] >= 0
        and isinstance(result.get("output_sha256"), str)
        and SHA256_PATTERN.fullmatch(result["output_sha256"]) is not None
        and result.get("cpu_seconds") is None
        and result.get("max_rss_kib") is None
        and result.get("resource_accounting")
        == "cgroup_enforced_metrics_not_exported_v1"
        and isolation == ISOLATION_SPEC
    )


def valid_intent(document: dict[str, object], request: Request) -> bool:
    return (
        matches_request(document, request)
        and document.get("document_type")
        == f"glaeda-{request.profile.document_slug}-intent"
        and document.get("phase") in {"prepared", "executing"}
    )


def remove_task(task_root: Path) -> None:
    try:
        shutil.rmtree(task_root)
    except FileNotFoundError:
        return
    if task_root.exists() or task_root.is_symlink():
        raise Refusal("task-private source/build state cleanup is incomplete")


def materialize(repository_root: Path, task_root: Path, request: Request) -> Path:
    source = task_root / "source"
    source.mkdir(mode=0o700)
    template = task_root / "git-template"
    template.mkdir(mode=0o700)
    environment = closed_environment({"GIT_TEMPLATE_DIR": os.fspath(template)})
    commands = (
        ["/usr/bin/git", "init", "--quiet", os.fspath(source)],
        [
            "/usr/bin/git",
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            "--depth=1",
            os.fspath(repository_root),
            request.commit,
        ],
        ["/usr/bin/git", "checkout", "--quiet", "--detach", request.commit],
    )
    for index, command in enumerate(commands):
        cwd = None if index == 0 else source
        completed = subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=60,
            check=False,
        )
        if completed.returncode != 0 or len(completed.stdout) > MAX_CONTROL_OUTPUT_BYTES:
            raise Refusal("exact task-private source materialization failed")
    try:
        (source / ".git" / "FETCH_HEAD").unlink()
    except FileNotFoundError:
        pass
    observed = run_control(
        ["/usr/bin/git", "rev-parse", "HEAD", "HEAD^{tree}"], source
    ).decode("ascii").splitlines()
    status = run_control(
        ["/usr/bin/git", "status", "--porcelain=v1", "-z", "--untracked-files=all"], source
    )
    if observed != [request.commit, request.tree] or status:
        raise Refusal("task-private source does not match the exact clean commit/tree")
    entries = 0
    bytes_seen = 0
    for parent, directories, files in os.walk(source, followlinks=False):
        entries += len(directories) + len(files)
        if entries > MAX_MATERIALIZED_SOURCE_ENTRIES:
            raise Refusal("task-private source exceeds the entry ceiling")
        for name in files:
            bytes_seen += (Path(parent) / name).lstat().st_size
            if bytes_seen > MAX_MATERIALIZED_SOURCE_BYTES:
                raise Refusal("task-private source exceeds the byte ceiling")
    return source


def public_crates_io_cache_arguments(cargo_root: Path) -> list[str]:
    """Expose public crates.io cache entries without unrelated Git/private-registry state."""
    arguments = ["--dir", "/cargo-home/registry"]
    for kind in ("cache", "index", "src"):
        arguments.extend(["--dir", f"/cargo-home/registry/{kind}"])
        parent = cargo_root / "registry" / kind
        if not parent.is_dir() or parent.is_symlink():
            continue
        for source in sorted(parent.glob("index.crates.io-*")):
            if source.is_dir() and not source.is_symlink() and source.parent == parent:
                arguments.extend(
                    [
                        "--ro-bind",
                        os.fspath(source),
                        f"/cargo-home/registry/{kind}/{source.name}",
                    ]
                )
    return arguments


def sandbox_command(
    source: Path,
    task_root: Path,
    cargo_root: Path,
    rustup_root: Path,
    unit_name: str,
    profile: Profile = FOCUSED_PROFILE,
) -> list[str]:
    spec = profile_spec(profile)
    systemd_properties = [f"--property={value}" for value in spec["systemd_properties"]]
    bubblewrap = [
        "/usr/bin/bwrap",
        "--unshare-all",
        "--unshare-user",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--disable-userns",
        "--uid",
        "65534",
        "--gid",
        "65534",
        "--hostname",
        "glaeda-task",
        "--ro-bind",
        "/usr",
        "/usr",
        "--symlink",
        "usr/bin",
        "/bin",
        "--symlink",
        "usr/lib",
        "/lib",
        "--symlink",
        "usr/lib64",
        "/lib64",
        "--dir",
        "/etc",
        "--ro-bind-try",
        "/etc/ld.so.cache",
        "/etc/ld.so.cache",
        "--ro-bind-try",
        "/etc/alternatives",
        "/etc/alternatives",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--size",
        str(TEMP_TMPFS_BYTES),
        "--tmpfs",
        "/tmp",
        "--dir",
        "/workspace",
        "--ro-bind",
        os.fspath(source),
        "/workspace/source",
        "--size",
        str(spec["build_tmpfs_bytes"]),
        "--tmpfs",
        "/workspace/target",
        "--size",
        str(CARGO_HOME_TMPFS_BYTES),
        "--tmpfs",
        "/cargo-home",
        "--dir",
        "/home",
        "--size",
        str(PROJECT_HOME_TMPFS_BYTES),
        "--tmpfs",
        "/home/project",
        "--ro-bind",
        os.fspath(cargo_root / "bin"),
        "/cargo/bin",
        "--ro-bind",
        os.fspath(rustup_root),
        "/rustup",
    ]
    bubblewrap.extend(public_crates_io_cache_arguments(cargo_root))
    bubblewrap.extend(
        [
            "--chdir",
            "/workspace/source",
            "--clearenv",
            "--setenv",
            "PATH",
            "/cargo/bin:/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/home/project",
            "--setenv",
            "CARGO_HOME",
            "/cargo-home",
            "--setenv",
            "CARGO_TARGET_DIR",
            "/workspace/target",
            "--setenv",
            "CARGO_NET_OFFLINE",
            "true",
            "--setenv",
            "CARGO_BUILD_JOBS",
            "4",
            "--setenv",
            "RUSTUP_HOME",
            "/rustup",
            "--setenv",
            "RUSTUP_TOOLCHAIN",
            RUST_TOOLCHAIN,
            "--setenv",
            "GIT_CONFIG_NOSYSTEM",
            "1",
            "--setenv",
            "GIT_CONFIG_GLOBAL",
            "/dev/null",
            "--setenv",
            "LC_ALL",
            "C",
            "--setenv",
            "GIT_OPTIONAL_LOCKS",
            "0",
            "--",
            "/workspace/source/scripts/verify",
            profile.recipe_name,
        ]
    )
    return [
        "/usr/bin/systemd-run",
        "--user",
        "--wait",
        "--pipe",
        "--quiet",
        "--collect",
        "--service-type=exec",
        f"--unit={unit_name}",
        *systemd_properties,
        *bubblewrap,
    ]


def unit_absent(unit_name: str) -> bool:
    completed = subprocess.run(
        ["/usr/bin/systemctl", "--user", "show", unit_name, "--property=LoadState", "--value"],
        env=closed_environment({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"}),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        timeout=10,
        check=False,
    )
    return completed.returncode == 0 and completed.stdout.strip() in (b"", b"not-found")


def stop_unit(unit_name: str) -> None:
    subprocess.run(
        ["/usr/bin/systemctl", "--user", "stop", unit_name],
        env=closed_environment({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"}),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=15,
        check=False,
    )


def execute_profile(
    source: Path,
    task_root: Path,
    cargo_root: Path,
    rustup_root: Path,
    request: Request,
) -> tuple[str, int, float, bool, int, str]:
    profile = request.profile
    unit = f"glaeda-{profile.document_slug}-{request.command_fingerprint[7:39]}.service"
    started = time.monotonic()
    process = subprocess.Popen(
        sandbox_command(source, task_root, cargo_root, rustup_root, unit, profile),
        env=closed_environment({"XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}"}),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    digest = hashlib.sha256()
    output_bytes = 0
    tail = bytearray()
    output_exceeded = False
    forced_timeout = False
    deadline = started + profile.deadline_seconds + 30
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            forced_timeout = True
            stop_unit(unit)
            try:
                os.killpg(process.pid, 9)
            except ProcessLookupError:
                pass
            break
        events = selector.select(min(0.1, remaining))
        for key, _ in events:
            chunk = os.read(key.fd, 64 * 1024)
            if not chunk:
                selector.unregister(process.stdout)
                continue
            digest.update(chunk)
            output_bytes += len(chunk)
            tail.extend(chunk)
            if len(tail) > FAILURE_TAIL_BYTES:
                del tail[:-FAILURE_TAIL_BYTES]
            if output_bytes > MAX_SOURCE_OUTPUT_BYTES and not output_exceeded:
                output_exceeded = True
                stop_unit(unit)
        if process.poll() is not None and not selector.get_map():
            break
    try:
        returncode = process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        stop_unit(unit)
        try:
            os.killpg(process.pid, 9)
        except ProcessLookupError:
            pass
        returncode = process.wait()
    selector.close()
    elapsed = time.monotonic() - started
    settled = unit_absent(unit)
    if not settled:
        stop_unit(unit)
        settled = unit_absent(unit)
    terminal = "succeeded" if returncode == 0 else "failed"
    if forced_timeout or elapsed >= profile.deadline_seconds:
        terminal = "timed_out"
    if output_exceeded:
        terminal = "failed"
    if not settled:
        terminal = "cleanup_incomplete"
    if terminal != "succeeded" and tail:
        omitted = output_bytes - len(tail)
        print(
            f"{profile.document_slug} failure output: tail_bytes={len(tail)} omitted_bytes={omitted}",
            file=sys.stderr,
        )
        sys.stderr.flush()
        sys.stderr.buffer.write(bytes(tail))
        if not tail.endswith(b"\n"):
            sys.stderr.buffer.write(b"\n")
        sys.stderr.buffer.flush()
    return terminal, returncode, elapsed, settled, output_bytes, f"sha256:{digest.hexdigest()}"


def receipt(
    request: Request,
    terminal: str,
    exit_code: int,
    elapsed: float,
    process_tree_settled: bool,
    cleanup_complete: bool,
    output_bytes: int,
    output_sha256: str,
    started_at_ms: int,
    settled_at_ms: int,
) -> dict[str, object]:
    return {
        "document_type": f"glaeda-{request.profile.document_slug}-receipt",
        "schema_version": SCHEMA_VERSION,
        "authority": "physical_execution_observation",
        "command_fingerprint": request.command_fingerprint,
        "source": {
            "repository": request.repository,
            "commit": request.commit,
            "tree": request.tree,
        },
        "profile": {
            "id": request.profile.profile_id,
            "class": request.profile.profile_class,
            "generation": request.profile_generation,
            "resource_class": request.profile.resource_class,
            "deadline_seconds": request.profile.deadline_seconds,
        },
        "execution_identity_class": EXECUTION_IDENTITY_CLASS,
        "isolation": dict(ISOLATION_SPEC),
        "result": {
            "terminal_class": terminal,
            "exit_code": exit_code,
            "elapsed_seconds": round(elapsed, 6),
            "started_at_unix_millis": started_at_ms,
            "settled_at_unix_millis": settled_at_ms,
            "process_tree_settled": process_tree_settled,
            "task_cleanup_complete": cleanup_complete,
            "raw_output": "not_published",
            "output_bytes": output_bytes,
            "output_sha256": output_sha256,
            "cpu_seconds": None,
            "max_rss_kib": None,
            "resource_accounting": "cgroup_enforced_metrics_not_exported_v1",
        },
        "contains_private_content": False,
        "contains_credentials": False,
        "authorizes_work": False,
        "authorizes_effects": False,
        "authorizes_redispatch": False,
    }


def emit(document: dict[str, object]) -> None:
    sys.stdout.buffer.write(canonical_bytes(document) + b"\n")


def run(arguments: argparse.Namespace, profile: Profile = FOCUSED_PROFILE) -> int:
    request = normalize_request(arguments, profile)
    repository_root = exact_directory(arguments.repository_root, "resident repository")
    cargo_root = exact_directory(arguments.cargo_root, "Cargo root")
    rustup_root = exact_directory(arguments.rustup_root, "rustup root")
    state_root = private_state_directory(arguments.state_root)
    verify_resident_source(repository_root, request)
    command_root = ensure_private_child(state_root, request.command_fingerprint[7:])
    with open_lock(command_root):
        receipt_path = command_root / "receipt.json"
        intent_path = command_root / "intent.json"
        existing = read_document(receipt_path)
        if existing is not None:
            if not valid_terminal_receipt(existing, request):
                raise Refusal("durable receipt conflicts with the exact command")
            emit(existing)
            return 0
        intent = read_document(intent_path)
        if arguments.reconcile_only:
            raise Refusal("no terminal receipt exists for exact reconciliation")
        if intent is not None:
            if not valid_intent(intent, request):
                raise Refusal("durable intent conflicts with the exact command")
            raise Refusal("previous physical execution is ambiguous; redispatch refused")

        task_root = command_root / "task"
        remove_task(task_root)
        task_root.mkdir(mode=0o700)
        source = materialize(repository_root, task_root, request)
        base = {
            "document_type": f"glaeda-{profile.document_slug}-intent",
            "schema_version": SCHEMA_VERSION,
            "command_fingerprint": request.command_fingerprint,
            "source": {
                "repository": request.repository,
                "commit": request.commit,
                "tree": request.tree,
            },
            "profile": {"id": profile.profile_id, "generation": request.profile_generation},
        }
        publish_document(intent_path, {**base, "phase": "prepared"}, replace=False)
        publish_document(intent_path, {**base, "phase": "executing"}, replace=True)
        started_at_ms = time.time_ns() // 1_000_000
        terminal, exit_code, elapsed, settled, output_bytes, output_sha256 = execute_profile(
            source, task_root, cargo_root, rustup_root, request
        )
        if not settled:
            raise Refusal("physical process-tree settlement is incomplete; redispatch refused")
        cleanup_complete = False
        try:
            remove_task(task_root)
            cleanup_complete = True
        except (Refusal, OSError):
            terminal = "cleanup_incomplete"
        settled_at_ms = time.time_ns() // 1_000_000
        document = receipt(
            request,
            terminal,
            exit_code,
            elapsed,
            settled,
            cleanup_complete,
            output_bytes,
            output_sha256,
            started_at_ms,
            settled_at_ms,
        )
        publish_document(receipt_path, document, replace=False)
        intent_path.unlink()
        emit(document)
        return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subcommands = root.add_subparsers(dest="command", required=True)
    subcommands.add_parser("profile", help="emit the fixed path-free profile identity")
    execute = subcommands.add_parser("run", help="execute or reconcile one exact command")
    execute.add_argument("--repository-root", required=True)
    execute.add_argument("--state-root", required=True)
    execute.add_argument("--cargo-root", required=True)
    execute.add_argument("--rustup-root", required=True)
    execute.add_argument("--repository", required=True)
    execute.add_argument("--commit", required=True)
    execute.add_argument("--tree", required=True)
    execute.add_argument("--profile-generation", required=True)
    execute.add_argument("--command-fingerprint", required=True)
    execute.add_argument("--reconcile-only", action="store_true")
    return root


def refuse(error: BaseException, profile: Profile = FOCUSED_PROFILE) -> NoReturn:
    message = str(error) if isinstance(error, Refusal) else f"{profile.document_slug} execution failed"
    print(
        json.dumps(
            {
                "document_type": f"glaeda-{profile.document_slug}-error",
                "schema_version": SCHEMA_VERSION,
                "authority": "none",
                "problem": message[:500],
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
        file=sys.stderr,
    )
    raise SystemExit(75)


def main(profile: Profile = FOCUSED_PROFILE) -> int:
    arguments = parser().parse_args()
    try:
        if arguments.command == "profile":
            emit({**profile_spec(profile), "profile_generation": profile_generation(profile)})
            return 0
        return run(arguments, profile)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        refuse(error, profile)
