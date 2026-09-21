#!/usr/bin/env python3
"""Fixed owner-trusted GitHub Actions JIT runner adapter for Big Red.

This is an internal physical adapter, not a remote command surface. The caller
owns GitHub Scale Set assignment/JIT/terminal evidence and passes only exact,
non-secret identities plus the one-time JIT value on stdin. The adapter owns the
Big Red physical reservation, task-private runner root, bounded process lifetime,
and cleanup/release reconciliation.
"""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import sys
import tarfile
import time
from typing import IO, NoReturn

import owned_linux_admission as owned_admission
import owned_linux_task as owned_task
from owned_linux_task import Refusal


SCHEMA_VERSION = 1
PROFILE_ID = "github-actions-big-red-trusted/v1"
RESOURCE_CLASS = "big-red-github-actions-trusted"
NETWORK_CLASS = owned_task.TaskNetwork.GITHUB_ACTIONS_TRUSTED_EGRESS.value
DEADLINE_SECONDS = 1800
RUNNER_VERSION = "2.336.0"
RUNNER_ARCHIVE = Path(
    "/opt/glaeda/payloads/actions-runner-linux-x64-2.336.0.tar.gz"
)
RUNNER_ARCHIVE_BYTES = 226_035_903
RUNNER_ARCHIVE_SHA256 = (
    "sha256:04cf0be1aff4c3ec3554466c39124ca250e3effd8873bb7e8d68535aa9505d5d"
)
JIT_LAUNCHER_SHA256 = (
    "sha256:6f096fb518b6d40d45ca1a9923e423da436b6269fceec5a5989566abbc93a76a"
)
TRUSTED_REPOSITORY = "teamleaderleo/quarry"
TRUSTED_RUNNER_LABEL = "glaeda-big-red-trusted"
MAX_JIT_BYTES = 64 * 1024
MAX_RUNNER_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_RUNNER_ENTRIES = 50_000
MAX_RUNNER_EXPANDED_BYTES = 2 * 1024 * 1024 * 1024
MAX_STATE_BYTES = 32 * 1024
TOKEN = re.compile(r"^[A-Za-z0-9_.:/@+-]{1,240}$")
RUNNER_NAME = re.compile(r"^[A-Za-z0-9_.-]{1,100}$")
SHA256 = re.compile(r"^sha256:[a-f0-9]{64}$")

SYSTEMD_PROPERTIES = [
    "CPUQuota=400%",
    "MemoryHigh=6G",
    "MemoryMax=8G",
    "TasksMax=512",
    f"RuntimeMaxSec={DEADLINE_SECONDS}",
    "KillMode=mixed",
    "NoNewPrivileges=yes",
    "RestrictSUIDSGID=yes",
]

PROFILE_SPEC = {
    "schema_version": 1,
    "profile_id": PROFILE_ID,
    "trust_class": "owner_trusted_internal_repository",
    "repository": TRUSTED_REPOSITORY,
    "runner_label": TRUSTED_RUNNER_LABEL,
    "runner_version": RUNNER_VERSION,
    "runner_archive_bytes": RUNNER_ARCHIVE_BYTES,
    "runner_archive_sha256": RUNNER_ARCHIVE_SHA256,
    "resource_class": RESOURCE_CLASS,
    "deadline_seconds": DEADLINE_SECONDS,
    "network_class": NETWORK_CLASS,
    "network_enforcement": "shared_host_namespace_trusted_jobs_only",
    "systemd_properties": SYSTEMD_PROPERTIES,
}

ISOLATION = {
    "runner_root": "task_private",
    "runner_work": "task_private",
    "ambient_environment": "cleared",
    "network_class": NETWORK_CLASS,
    "network_enforcement": "shared_host_namespace_trusted_jobs_only",
    "glaeda_control_credentials": "absent",
    "publisher_credentials": "absent",
    "ssh_agent": "absent",
    "sudo_admin": "absent",
    "unrelated_writable_projects": "absent",
    "docker_podman_host_socket": "absent",
}


@dataclass(frozen=True)
class Request:
    attempt_id: str
    assignment_id: str
    repository: str
    runner_label: str
    workflow_run_id: int
    job_id: str
    runner_id: int
    runner_name: str
    runner_generation: str
    expires_at_unix_ms: int

    def assignment_fingerprint(self) -> str:
        """Stable durable owner for one exact GitHub assignment.

        Local retry/attempt identities deliberately do not change this key, so
        the same GitHub assignment cannot acquire a second physical runner root.
        """
        return sha256(
            canonical_bytes(
                {
                    "assignment_id": self.assignment_id,
                    "repository": self.repository,
                    "runner_label": self.runner_label,
                    "workflow_run_id": self.workflow_run_id,
                    "job_id": self.job_id,
                    "profile_id": PROFILE_ID,
                }
            )
        )

    def fingerprint(self) -> str:
        return sha256(
            canonical_bytes(
                {
                    "attempt_id": self.attempt_id,
                    "assignment_id": self.assignment_id,
                    "repository": self.repository,
                    "runner_label": self.runner_label,
                    "workflow_run_id": self.workflow_run_id,
                    "job_id": self.job_id,
                    "runner_id": self.runner_id,
                    "runner_name": self.runner_name,
                    "runner_generation": self.runner_generation,
                    "expires_at_unix_ms": self.expires_at_unix_ms,
                    "profile_id": PROFILE_ID,
                }
            )
        )


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def profile_generation() -> str:
    return sha256(canonical_bytes(PROFILE_SPEC))


def normalize(arguments: argparse.Namespace) -> Request:
    fields = (
        arguments.attempt_id,
        arguments.assignment_id,
        arguments.job_id,
        arguments.runner_generation,
    )
    if any(TOKEN.fullmatch(value) is None for value in fields):
        raise Refusal("runner transaction identity is invalid")
    if arguments.repository != TRUSTED_REPOSITORY:
        raise Refusal("repository is outside the reviewed owner-trusted canary")
    if arguments.runner_label != TRUSTED_RUNNER_LABEL:
        raise Refusal("runner label is outside the reviewed owner-trusted canary")
    if type(arguments.workflow_run_id) is not int or arguments.workflow_run_id <= 0:
        raise Refusal("workflow run identity is invalid")
    if type(arguments.runner_id) is not int or arguments.runner_id <= 0:
        raise Refusal("runner identity is invalid")
    if RUNNER_NAME.fullmatch(arguments.runner_name) is None:
        raise Refusal("runner name is invalid")
    if type(arguments.expires_at_unix_ms) is not int or arguments.expires_at_unix_ms <= 0:
        raise Refusal("attempt expiry is invalid")
    request = Request(
        arguments.attempt_id,
        arguments.assignment_id,
        arguments.repository,
        arguments.runner_label,
        arguments.workflow_run_id,
        arguments.job_id,
        arguments.runner_id,
        arguments.runner_name,
        arguments.runner_generation,
        arguments.expires_at_unix_ms,
    )
    return request


def private_directory(raw: str) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        raise Refusal("state root must be absolute")
    parent = path.parent.resolve(strict=True)
    if parent != path.parent or path.parent.is_symlink():
        raise Refusal("state root parent is unavailable")
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    metadata = path.lstat()
    if (
        path.resolve(strict=True) != path
        or not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise Refusal("state root is not a private current-user directory")
    return path


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
            raise Refusal("runner state contains an unsafe filesystem object")
    return path


def existing_private_child(parent: Path, name: str) -> Path:
    path = parent / name
    try:
        metadata = path.lstat()
    except FileNotFoundError as error:
        raise Refusal("exact runner assignment state is unavailable") from error
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
        or path.resolve(strict=True) != path
    ):
        raise Refusal("runner state contains an unsafe assignment directory")
    return path


@contextmanager
def launch_fence(directory: Path):
    path = directory / "launch.lock"
    descriptor = os.open(
        path,
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
        raise Refusal("runner launch fence is unsafe")
    lock = os.fdopen(descriptor, "r+b", buffering=0)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(lock, fcntl.LOCK_UN)
        lock.close()


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
        raise Refusal("runner lock is unsafe")
    lock = os.fdopen(descriptor, "r+b", buffering=0)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        lock.close()
        raise Refusal("exact runner attempt is already active") from error
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
        or metadata.st_size > MAX_STATE_BYTES
    ):
        raise Refusal("runner state contains an unsafe document")
    raw = path.read_bytes()
    try:
        value = json.loads(raw)
    except (UnicodeError, ValueError) as error:
        raise Refusal("runner state is corrupt") from error
    if not isinstance(value, dict) or canonical_bytes(value) + b"\n" != raw:
        raise Refusal("runner state is noncanonical")
    return value


def publish(path: Path, value: dict[str, object], *, replace: bool) -> None:
    raw = canonical_bytes(value) + b"\n"
    if len(raw) > MAX_STATE_BYTES:
        raise Refusal("runner state document exceeds its ceiling")
    temporary = path.parent / f".{path.name}.creating-{os.getpid()}"
    fd = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        with os.fdopen(fd, "wb", closefd=True) as output:
            output.write(raw)
            output.flush()
            os.fsync(output.fileno())
        if replace:
            os.replace(temporary, path)
        else:
            os.link(temporary, path, follow_symlinks=False)
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


def sync_directory(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def read_jit_secret() -> bytearray:
    buffer = bytearray(MAX_JIT_BYTES + 2)
    view = memoryview(buffer)
    offset = 0
    try:
        while offset < len(buffer):
            count = sys.stdin.buffer.readinto(view[offset:])
            if count is None or count == 0:
                break
            offset += count
        if offset == 0 or offset > MAX_JIT_BYTES + 1:
            raise Refusal("JIT input is missing or exceeds its fixed ceiling")
        if buffer[offset - 1] != 10:
            raise Refusal("JIT input must be exactly one newline-terminated value")
        if buffer.find(b"\n", 0, offset - 1) != -1:
            raise Refusal("JIT input contains more than one line")
        if offset == 1:
            raise Refusal("JIT input is empty")
        allowed = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=_-"
        allowed_set = set(allowed)
        if any(buffer[index] not in allowed_set for index in range(offset - 1)):
            raise Refusal("JIT input contains invalid characters")
        view.release()
        view = None
        del buffer[offset:]
        return buffer
    except BaseException:
        buffer[:] = b"\x00" * len(buffer)
        raise
    finally:
        if view is not None:
            view.release()


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while True:
            chunk = source.read(1024 * 1024)
            if not chunk:
                break
            size += len(chunk)
            if size > MAX_RUNNER_ARCHIVE_BYTES:
                raise Refusal("reviewed Actions runner archive exceeds its ceiling")
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def extract_reviewed_runner(task_root: Path) -> Path:
    if not RUNNER_ARCHIVE.is_file() or RUNNER_ARCHIVE.is_symlink():
        raise Refusal("reviewed Actions runner archive is unavailable")
    if RUNNER_ARCHIVE.stat().st_size != RUNNER_ARCHIVE_BYTES:
        raise Refusal("reviewed Actions runner archive size changed")
    if file_sha256(RUNNER_ARCHIVE) != RUNNER_ARCHIVE_SHA256:
        raise Refusal("reviewed Actions runner archive digest changed")
    runner_root = task_root / "runner"
    runner_root.mkdir(mode=0o700)
    with tarfile.open(RUNNER_ARCHIVE, "r:gz") as archive:
        members = archive.getmembers()
        if len(members) > MAX_RUNNER_ENTRIES:
            raise Refusal("Actions runner archive exceeds its entry ceiling")
        expanded = 0
        for member in members:
            if member.ischr() or member.isblk() or member.isfifo():
                raise Refusal("Actions runner archive contains an unsafe object")
            expanded += max(member.size, 0)
            if expanded > MAX_RUNNER_EXPANDED_BYTES:
                raise Refusal("Actions runner archive exceeds its expanded ceiling")
        archive.extractall(runner_root, filter="data")
    listener = runner_root / "bin" / "Runner.Listener"
    if not listener.is_file() or listener.is_symlink():
        raise Refusal("reviewed Actions runner payload is incomplete")
    (runner_root / "_work").mkdir(mode=0o700, exist_ok=True)
    (runner_root / "_diag").mkdir(mode=0o700, exist_ok=True)
    return runner_root


def reviewed_launcher(task_root: Path) -> Path:
    source = Path(__file__).resolve(strict=True).parents[1] / "examples" / "lima" / "glaeda-jit-launcher"
    if source.is_symlink() or not source.is_file():
        raise Refusal("reviewed JIT launcher is unavailable")
    if file_sha256(source) != JIT_LAUNCHER_SHA256:
        raise Refusal("reviewed JIT launcher digest changed")
    target = task_root / "jit-launcher"
    shutil.copyfile(source, target, follow_symlinks=False)
    target.chmod(0o555)
    return target


def unit_name(request: Request) -> str:
    return f"glaeda-gha-{request.assignment_fingerprint()[7:39]}.service"


def admission_binding(request: Request, state_root: Path) -> str:
    """Bind the shared physical reservation before assignment-private state exists."""
    info = state_root.stat()
    return sha256(
        canonical_bytes(
            {
                "attempt_id": request.attempt_id,
                "assignment_id": request.assignment_id,
                "assignment_fingerprint": request.assignment_fingerprint(),
                "runner_id": request.runner_id,
                "runner_name": request.runner_name,
                "runner_generation": request.runner_generation,
                "state_root": os.fspath(state_root),
                "device": info.st_dev,
                "inode": info.st_ino,
                "profile_generation": profile_generation(),
            }
        )
    )


def sandbox_command(
    task_root: Path, runner_root: Path, launcher: Path, request: Request
) -> list[str]:
    source = task_root / "source"
    source.mkdir(mode=0o700)
    (source / "target").mkdir(mode=0o700)
    cargo = task_root / "cargo"
    (cargo / "bin").mkdir(parents=True, mode=0o700)
    rustup = task_root / "rustup"
    rustup.mkdir(mode=0o700)
    identity = task_root / "identity"
    identity.mkdir(mode=0o700)
    passwd = identity / "passwd"
    group = identity / "group"
    passwd.write_text(
        "glaeda-runner:x:65534:65534:Glaeda runner:/home/project:/usr/sbin/nologin\n",
        encoding="utf-8",
    )
    group.write_text("glaeda-runner:x:65534:\n", encoding="utf-8")
    passwd.chmod(0o444)
    group.chmod(0o444)
    mounts = [
        "--dir", "/opt",
        "--dir", "/opt/smolrunner",
        "--dir", "/opt/smolrunner/bin",
        "--bind", os.fspath(runner_root), "/opt/smolrunner/actions-runner",
        "--ro-bind", os.fspath(launcher), "/opt/smolrunner/bin/smolrunner-jit-launcher",
        "--ro-bind", os.fspath(passwd), "/etc/passwd",
        "--ro-bind", os.fspath(group), "/etc/group",
        "--ro-bind-try", "/etc/os-release", "/etc/os-release",
        "--ro-bind-try", "/etc/debian_version", "/etc/debian_version",
    ]
    recipe = [
        "--chdir", "/opt/smolrunner/actions-runner",
        "--clearenv",
        "--setenv", "HOME", "/home/project",
        "--setenv", "PATH", "/usr/bin:/bin",
        "--setenv", "GIT_CONFIG_GLOBAL", "/dev/null",
        "--setenv", "GIT_CONFIG_NOSYSTEM", "1",
        "--setenv", "LC_ALL", "C",
        "--",
        "/opt/smolrunner/bin/smolrunner-jit-launcher",
    ]
    return owned_task.sandbox_command(
        source,
        cargo,
        rustup,
        unit_name(request),
        systemd_properties=SYSTEMD_PROPERTIES,
        build_tmpfs_bytes=64 * 1024 * 1024,
        mount_arguments=mounts,
        recipe_arguments=recipe,
        network=owned_task.TaskNetwork.GITHUB_ACTIONS_TRUSTED_EGRESS,
        source_read_only=True,
    )


def base_identity(request: Request) -> dict[str, object]:
    return {
        "attempt_id": request.attempt_id,
        "assignment_id": request.assignment_id,
        "repository": request.repository,
        "runner_label": request.runner_label,
        "workflow_run_id": request.workflow_run_id,
        "job_id": request.job_id,
        "runner_id": request.runner_id,
        "runner_name": request.runner_name,
        "runner_generation": request.runner_generation,
        "expires_at_unix_ms": request.expires_at_unix_ms,
    }


def intent_document(request: Request) -> dict[str, object]:
    return {
        "document_type": "glaeda-owned-linux-jit-runner-intent",
        "schema_version": SCHEMA_VERSION,
        "phase": "runner_start_checkpointed",
        "identity": base_identity(request),
        "command_fingerprint": request.fingerprint(),
        "profile_generation": profile_generation(),
    }


def cancellation_document(request: Request) -> dict[str, object]:
    return {
        "document_type": "glaeda-owned-linux-jit-runner-cancellation",
        "schema_version": SCHEMA_VERSION,
        "phase": "github_cancellation_observed",
        "identity": base_identity(request),
        "command_fingerprint": request.fingerprint(),
        "authorizes_redispatch": False,
    }


def exit_document(
    request: Request,
    terminal: str,
    exit_code: int,
    elapsed: float,
    settled: bool,
    output_bytes: int,
    output_digest: str,
) -> dict[str, object]:
    return {
        "document_type": "glaeda-owned-linux-jit-runner-exit",
        "schema_version": SCHEMA_VERSION,
        "authority": "physical_runner_process_observation",
        "identity": base_identity(request),
        "command_fingerprint": request.fingerprint(),
        "profile": {**PROFILE_SPEC, "generation": profile_generation()},
        "isolation": dict(ISOLATION),
        "result": {
            "terminal_class": terminal,
            "exit_code": exit_code,
            "elapsed_seconds": round(elapsed, 6),
            "process_tree_settled": settled,
            "output_bytes": output_bytes,
            "output_sha256": output_digest,
            "raw_output": "not_published",
            "task_cleanup_complete": False,
            "capacity_released": False,
            "github_terminal_observed": False,
            "runner_retired": False,
        },
        "contains_jit_secret": False,
        "contains_credentials": False,
        "authorizes_redispatch": False,
    }


def terminal_document(
    exit_receipt: dict[str, object], github_terminal: str
) -> dict[str, object]:
    result = dict(exit_receipt["result"])
    result.update(
        {
            "task_cleanup_complete": True,
            "capacity_released": True,
            "github_terminal_observed": True,
            "github_terminal": github_terminal,
            "runner_retired": True,
        }
    )
    return {
        **exit_receipt,
        "document_type": "glaeda-owned-linux-jit-runner-receipt",
        "authority": "settled_github_and_physical_observation",
        "result": result,
    }


def matches_request(document: dict[str, object], request: Request) -> bool:
    return (
        document.get("schema_version") == SCHEMA_VERSION
        and document.get("identity") == base_identity(request)
        and document.get("command_fingerprint") == request.fingerprint()
    )


def emit(document: dict[str, object]) -> None:
    sys.stdout.buffer.write(canonical_bytes(document) + b"\n")


def inspect_existing_assignment(
    command_root: Path, request: Request
) -> bool:
    """Replay a terminal receipt or refuse any ambiguous prior physical state.

    An empty assignment directory is safe pre-launch bookkeeping from a prior
    refusal whose physical reservation was released. It grants no replay.
    """
    with open_lock(command_root):
        final = read_document(command_root / "receipt.json")
        if final is not None:
            if not matches_request(final, request):
                raise Refusal("terminal receipt conflicts with exact runner attempt")
            emit(final)
            return True
        existing_exit = read_document(command_root / "runner-exit.json")
        if existing_exit is not None:
            if not matches_request(existing_exit, request):
                raise Refusal("runner exit receipt conflicts with exact attempt")
            raise Refusal(
                "runner exited without settled GitHub evidence; redispatch refused"
            )
        intent = read_document(command_root / "intent.json")
        if intent is not None:
            if not matches_request(intent, request):
                raise Refusal("runner intent conflicts with exact attempt")
            raise Refusal("previous runner start is ambiguous; redispatch refused")
        cancellation = read_document(command_root / "cancellation.json")
        if cancellation is not None:
            if not matches_request(cancellation, request):
                raise Refusal("runner cancellation conflicts with exact attempt")
            raise Refusal("cancelled assignment cannot be redispatched")
    return False


def run_once(arguments: argparse.Namespace) -> int:
    request = normalize(arguments)
    state_root = private_directory(arguments.state_root)
    assignment_name = request.assignment_fingerprint()[7:]
    candidate = state_root / assignment_name
    try:
        candidate.lstat()
    except FileNotFoundError:
        command_root = None
    else:
        command_root = existing_private_child(state_root, assignment_name)
        if inspect_existing_assignment(command_root, request):
            return 0

    if time.time_ns() // 1_000_000 >= request.expires_at_unix_ms:
        raise Refusal("runner attempt is expired")

    unit = unit_name(request)
    binding = admission_binding(request, state_root)
    gate = owned_admission.Reservation(
        owned_admission.CANONICAL_ROOT,
        request.assignment_fingerprint(),
        unit,
        binding,
    )
    with gate as admission:
        if command_root is None:
            command_root = ensure_private_child(state_root, assignment_name)
        with open_lock(command_root):
            final_path = command_root / "receipt.json"
            exit_path = command_root / "runner-exit.json"
            intent_path = command_root / "intent.json"
            cancellation_path = command_root / "cancellation.json"
            # A concurrent controller may have populated an already-empty assignment
            # while this process waited for the shared physical slot. Refuse and
            # release pre-launch capacity; the next invocation reconciles that state.
            if any(
                read_document(path) is not None
                for path in (final_path, exit_path, intent_path, cancellation_path)
            ):
                raise Refusal("runner assignment changed while acquiring physical capacity")

            task_root = command_root / "task"
            try:
                owned_task.prepare_task(task_root)
                runner_root = extract_reviewed_runner(task_root)
                launcher = reviewed_launcher(task_root)
                secret = read_jit_secret()
                publish(intent_path, intent_document(request), replace=False)
                started = time.monotonic()

                @contextmanager
                def guarded_launch():
                    with launch_fence(command_root):
                        cancellation = read_document(cancellation_path)
                        if cancellation is not None:
                            if not matches_request(cancellation, request):
                                raise Refusal(
                                    "runner cancellation conflicts with exact attempt"
                                )
                            raise Refusal("assignment was cancelled before runner start")
                        with admission.launch():
                            yield

                terminal, code, elapsed, settled, output_bytes, output_digest = (
                    owned_task.execute_secret_stdin(
                        sandbox_command(task_root, runner_root, launcher, request),
                        unit=unit,
                        deadline_seconds=DEADLINE_SECONDS,
                        label="github-actions-jit-runner",
                        secret_stdin=secret,
                        launch_guard=guarded_launch,
                    )
                )
                if not settled:
                    raise Refusal(
                        "runner process tree remains unsettled; capacity stays reserved"
                    )
                document = exit_document(
                    request,
                    terminal,
                    code,
                    elapsed if elapsed >= 0 else time.monotonic() - started,
                    settled,
                    output_bytes,
                    output_digest,
                )
                publish(exit_path, document, replace=False)
                emit(document)
                return 0
            finally:
                # Before Popen, refusal can clean the exact task and release the
                # physical reservation. After launch begins, ambiguity stays durable.
                if admission.owned and not admission.launch_attempted:
                    owned_task.remove_task(task_root)
                    if intent_path.exists():
                        intent_path.unlink()
                    sync_directory(command_root)
                    admission.release()


def cancel(arguments: argparse.Namespace) -> int:
    request = normalize(arguments)
    state_root = private_directory(arguments.state_root)
    command_root = existing_private_child(
        state_root, request.assignment_fingerprint()[7:]
    )
    intent_path = command_root / "intent.json"
    exit_path = command_root / "runner-exit.json"
    final_path = command_root / "receipt.json"
    cancellation_path = command_root / "cancellation.json"

    final = read_document(final_path)
    if final is not None:
        if not matches_request(final, request):
            raise Refusal("terminal receipt conflicts with exact runner attempt")
        raise Refusal("runner attempt is already terminal")
    existing_exit = read_document(exit_path)
    if existing_exit is not None:
        if not matches_request(existing_exit, request):
            raise Refusal("runner exit receipt conflicts with exact attempt")
        raise Refusal("runner already exited; continue authoritative settlement")
    intent = read_document(intent_path)
    if intent is None or not matches_request(intent, request):
        raise Refusal("exact runner-start checkpoint is unavailable")

    cancellation = read_document(cancellation_path)
    if cancellation is None:
        publish(cancellation_path, cancellation_document(request), replace=False)
    elif not matches_request(cancellation, request):
        raise Refusal("runner cancellation conflicts with exact attempt")

    unit = unit_name(request)
    with launch_fence(command_root):
        owned_task.stop_unit(unit)
        if not owned_task.unit_absent(unit):
            raise Refusal("cancelled runner process tree remains alive")
    emit(cancellation_document(request))
    return 0


def settle(arguments: argparse.Namespace) -> int:
    request = normalize(arguments)
    if arguments.github_terminal not in {"success", "failure", "cancelled"}:
        raise Refusal("GitHub terminal class is invalid")
    if arguments.retired_runner_id != request.runner_id:
        raise Refusal("retired runner identity does not match exact bound runner")
    state_root = private_directory(arguments.state_root)
    command_root = ensure_private_child(state_root, request.assignment_fingerprint()[7:])
    with open_lock(command_root):
        final_path = command_root / "receipt.json"
        exit_path = command_root / "runner-exit.json"
        intent_path = command_root / "intent.json"
        final = read_document(final_path)
        if final is not None:
            if not matches_request(final, request):
                raise Refusal("terminal receipt conflicts with exact runner attempt")
            emit(final)
            return 0
        exit_receipt = read_document(exit_path)
        if exit_receipt is None or not matches_request(exit_receipt, request):
            raise Refusal("exact physical runner exit evidence is unavailable")
        unit = unit_name(request)
        task_root = command_root / "task"
        if not owned_task.unit_absent(unit):
            raise Refusal("runner process tree remains alive")
        owned_task.remove_task(task_root)
        if task_root.exists() or task_root.is_symlink():
            raise Refusal("task-private runner state cleanup is incomplete")
        if intent_path.exists():
            intent_path.unlink()
        sync_directory(command_root)

        def observe_settled() -> None:
            if not owned_task.unit_absent(unit):
                raise Refusal("runner process tree remains alive")
            if task_root.exists() or task_root.is_symlink():
                raise Refusal("task cleanup is incomplete")

        owned_admission.recover(
            owned_admission.CANONICAL_ROOT,
            request.assignment_fingerprint(),
            unit,
            admission_binding(request, state_root),
            observe_settled,
        )
        document = terminal_document(exit_receipt, arguments.github_terminal)
        publish(final_path, document, replace=False)
        emit(document)
        return 0


def add_identity_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--state-root", required=True)
    parser.add_argument("--attempt-id", required=True)
    parser.add_argument("--assignment-id", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--runner-label", required=True)
    parser.add_argument("--workflow-run-id", type=int, required=True)
    parser.add_argument("--job-id", required=True)
    parser.add_argument("--runner-id", type=int, required=True)
    parser.add_argument("--runner-name", required=True)
    parser.add_argument("--runner-generation", required=True)
    parser.add_argument("--expires-at-unix-ms", type=int, required=True)


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    commands.add_parser("profile")
    run = commands.add_parser("run")
    add_identity_arguments(run)
    cancelled = commands.add_parser("cancel")
    add_identity_arguments(cancelled)
    settled = commands.add_parser("settle")
    add_identity_arguments(settled)
    settled.add_argument(
        "--github-terminal", choices=("success", "failure", "cancelled"), required=True
    )
    settled.add_argument("--retired-runner-id", type=int, required=True)
    return root


def refuse(error: BaseException) -> NoReturn:
    message = str(error) if isinstance(error, Refusal) else "owned Linux JIT runner failed"
    print(
        json.dumps(
            {
                "document_type": "glaeda-owned-linux-jit-runner-error",
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


def main() -> int:
    arguments = parser().parse_args()
    try:
        if arguments.command == "profile":
            emit({**PROFILE_SPEC, "profile_generation": profile_generation()})
            return 0
        if arguments.command == "run":
            return run_once(arguments)
        if arguments.command == "cancel":
            return cancel(arguments)
        return settle(arguments)
    except (OSError, RuntimeError, tarfile.TarError) as error:
        refuse(error)


if __name__ == "__main__":
    raise SystemExit(main())
