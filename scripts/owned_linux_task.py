"""Internal owned-Linux task mechanics; no remote CLI or request deserializer.

Only checked-in adapters supply commands, mounts, and resource properties. This
module grants no caller or execution authority. The verifier is its sole production
consumer until physical parity is accepted.
"""
from __future__ import annotations

from contextlib import nullcontext
from enum import Enum
import hashlib
import os
from pathlib import Path
import selectors
import shutil
import subprocess
import sys
import time


class TaskNetwork(Enum):
    NONE = "none"


MAX_CONTROL_OUTPUT_BYTES = 64 * 1024
MAX_SOURCE_OUTPUT_BYTES = 1024 * 1024
FAILURE_TAIL_BYTES = 8 * 1024
MAX_MATERIALIZED_SOURCE_BYTES = 512 * 1024 * 1024
MAX_MATERIALIZED_SOURCE_ENTRIES = 100_000
CARGO_HOME_TMPFS_BYTES = 512 * 1024 * 1024
TEMP_TMPFS_BYTES = 512 * 1024 * 1024
PROJECT_HOME_TMPFS_BYTES = 64 * 1024 * 1024


class Refusal(RuntimeError):
    pass


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


def remove_task(task_root: Path) -> None:
    try:
        shutil.rmtree(task_root)
    except FileNotFoundError:
        return
    if task_root.exists() or task_root.is_symlink():
        raise Refusal("task-private source/build state cleanup is incomplete")


def materialize(repository_root: Path, task_root: Path, commit: str, tree: str) -> Path:
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
            commit,
        ],
        ["/usr/bin/git", "checkout", "--quiet", "--detach", commit],
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
    if observed != [commit, tree] or status:
        raise Refusal("task-private source does not match the exact clean commit/tree")
    # Bubblewrap cannot create a child mountpoint beneath a read-only bind. Create
    # the ignored target mountpoint only after exact source admission; the tmpfs
    # mounted over it remains the sole writable build surface seen by the recipe.
    (source / "target").mkdir(mode=0o700)
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
    source: Path, cargo_root: Path, rustup_root: Path, unit_name: str, *,
    systemd_properties: list[str], build_tmpfs_bytes: int,
    mount_arguments: list[str], recipe_arguments: list[str],
    network: TaskNetwork,
) -> list[str]:
    if network is not TaskNetwork.NONE:
        raise Refusal("owned task network class is unsupported")
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
        str(build_tmpfs_bytes),
        "--tmpfs",
        "/workspace/source/target",
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
    bubblewrap.extend(mount_arguments)
    bubblewrap.extend(recipe_arguments)
    return [
        "/usr/bin/systemd-run",
        "--user",
        "--wait",
        "--pipe",
        "--quiet",
        "--collect",
        "--service-type=exec",
        f"--unit={unit_name}",
        *(f"--property={value}" for value in systemd_properties),
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


def execute(
    command: list[str], *, unit: str, deadline_seconds: int, label: str,
    launch_guard=None,
) -> tuple[str, int, float, bool, int, str]:
    started = time.monotonic()
    with launch_guard() if launch_guard is not None else nullcontext():
        process = subprocess.Popen(
            command,
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
    deadline = started + deadline_seconds + 30
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
    process.stdout.close()
    elapsed = time.monotonic() - started
    settled = unit_absent(unit)
    if not settled:
        stop_unit(unit)
        settled = unit_absent(unit)
    terminal = "succeeded" if returncode == 0 else "failed"
    if forced_timeout or elapsed >= deadline_seconds:
        terminal = "timed_out"
    if output_exceeded:
        terminal = "failed"
    if not settled:
        terminal = "cleanup_incomplete"
    if terminal != "succeeded" and tail:
        omitted = output_bytes - len(tail)
        print(
            f"{label} failure output: tail_bytes={len(tail)} omitted_bytes={omitted}",
            file=sys.stderr,
        )
        sys.stderr.flush()
        sys.stderr.buffer.write(bytes(tail))
        if not tail.endswith(b"\n"):
            sys.stderr.buffer.write(b"\n")
        sys.stderr.buffer.flush()
    return terminal, returncode, elapsed, settled, output_bytes, f"sha256:{digest.hexdigest()}"


def prepare_task(task_root: Path) -> None:
    remove_task(task_root)
    task_root.mkdir(mode=0o700)
