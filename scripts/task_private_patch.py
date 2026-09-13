"""Internal exact patch mutation for one freshly materialized task-private checkout.

This module has no remote CLI or request deserializer. A checked-in local adapter supplies the
resident repository, private state root, exact source/patch identities, and admitted patch bytes.
The module creates its own isolated checkout through owned_linux_task before any source edit. It
never mutates the resident repository, creates a commit, contacts a provider, or grants publication
or execution authority.
"""
from __future__ import annotations

from dataclasses import dataclass
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
from typing import Callable, IO, NoReturn

import owned_linux_task as owned_task


GIT = "/usr/bin/git"
SCHEMA_VERSION = 1
POLICY_GENERATION = 1
MAX_PATCH_BYTES = 16 * 1024
MAX_CHANGED_PATHS = 256
MAX_CHANGED_PATH_BYTES = 512
MAX_DOCUMENT_BYTES = 64 * 1024
MAX_GIT_OUTPUT_BYTES = 64 * 1024
GIT_TIMEOUT_SECONDS = 15
SHA1_PATTERN = re.compile(r"^[a-f0-9]{40}$")
SHA256_PATTERN = re.compile(r"^sha256:[a-f0-9]{64}$")
REPOSITORY_PATTERN = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")


class Refusal(RuntimeError):
    pass


class AmbiguousMutation(Refusal):
    pass


class InjectedCrash(BaseException):
    """Test-only process-loss discriminator; ordinary adapters never raise this."""


@dataclass(frozen=True)
class PatchIdentity:
    git_blob_sha1: str
    sha256: str
    bytes: int

    @classmethod
    def validate(cls, git_blob_sha1: str, sha256: str, bytes_: int) -> "PatchIdentity":
        if not SHA1_PATTERN.fullmatch(git_blob_sha1):
            raise Refusal("patch Git blob identity is invalid")
        if not SHA256_PATTERN.fullmatch(sha256):
            raise Refusal("patch SHA-256 identity is invalid")
        if isinstance(bytes_, bool) or not isinstance(bytes_, int) or not 0 < bytes_ <= MAX_PATCH_BYTES:
            raise Refusal("patch byte count is outside the reviewed boundary")
        return cls(git_blob_sha1, sha256, bytes_)


@dataclass(frozen=True)
class TaskPrivatePatchRequest:
    repository: str
    expected_head: str
    expected_tree: str
    patch: PatchIdentity
    changed_paths: tuple[str, ...]
    operation_id: str


@dataclass(frozen=True)
class TaskPrivatePatchResult:
    receipt: dict[str, object]
    source: Path | None


def apply_task_private_patch(
    *,
    repository_root: Path,
    state_root: Path,
    repository: str,
    expected_head: str,
    expected_tree: str,
    git_blob_sha1: str,
    sha256: str,
    bytes_: int,
    patch: bytes,
    after_materialize: Callable[[Path], None] | None = None,
    before_apply_process: Callable[[Path], None] | None = None,
    apply_runner: Callable[[Path, bytes], int] | None = None,
) -> TaskPrivatePatchResult:
    """Apply one exact patch to a newly created task-private checkout.

    `after_materialize`, `before_apply_process`, and `apply_runner` are dependency seams for focused
    tests. Production adapters omit them. The function publishes a durable applying intent before
    process creation. Once that phase exists, any missing terminal receipt blocks redispatch.
    """
    repository_root = exact_directory(repository_root, "resident repository")
    state_root = exact_private_directory(state_root, "task patch state root")
    require_disjoint_roots(repository_root, state_root)
    request = normalize_request(
        repository,
        expected_head,
        expected_tree,
        git_blob_sha1,
        sha256,
        bytes_,
        patch,
    )
    verify_resident_repository(repository_root, request)

    command_root = ensure_private_child(state_root, request.operation_id.removeprefix("sha256:"))
    with open_lock(command_root):
        receipt_path = command_root / "receipt.json"
        intent_path = command_root / "intent.json"
        receipt = read_document(receipt_path)
        if receipt is not None:
            validate_receipt(receipt, request)
            source = command_root / "task" / "source"
            if receipt.get("terminal_class") == "applied":
                validate_retained_result(source, request, receipt)
                return TaskPrivatePatchResult(receipt=receipt, source=source)
            return TaskPrivatePatchResult(receipt=receipt, source=None)

        if read_document(intent_path) is not None:
            raise AmbiguousMutation(
                "task-private patch mutation is ambiguous; rebuild recovery is required"
            )

        task_root = command_root / "task"
        owned_task.prepare_task(task_root)
        try:
            source = owned_task.materialize(
                repository_root,
                task_root,
                request.expected_head,
                request.expected_tree,
            )
            if after_materialize is not None:
                after_materialize(source)
            validate_task_source(source, request)
            validate_git_mutation_boundary(source)

            expected_result_tree = expected_result_tree_for_patch(source, request, patch)
            validate_task_source(source, request)
            intent = intent_document(request, expected_result_tree, "prepared")
            publish_document(intent_path, intent, replace=False)

            # The applicability proof is repeated after durable intent and immediately before the
            # applying phase. A refused final check has not attempted source mutation.
            check_patch_applicable(source, patch)
            validate_task_source(source, request)
            if before_apply_process is not None:
                before_apply_process(source)
            check_patch_applicable(source, patch)
            validate_task_source(source, request)

            publish_document(
                intent_path,
                intent_document(request, expected_result_tree, "applying"),
                replace=True,
            )
            if apply_runner is None:
                apply_status = run_apply_process(source, patch)
            else:
                apply_status = apply_runner(source, patch)
            if apply_status != 0:
                return settle_ambiguous(command_root, intent_path, receipt_path, request)

            receipt = observe_applied_result(source, request, expected_result_tree)
            publish_document(receipt_path, receipt, replace=False)
            intent_path.unlink()
            sync_directory(command_root)
            verify_resident_repository(repository_root, request)
            return TaskPrivatePatchResult(receipt=receipt, source=source)
        except InjectedCrash:
            raise
        except AmbiguousMutation:
            raise
        except BaseException:
            intent = read_document(intent_path)
            if intent is not None and intent.get("phase") == "applying":
                return settle_ambiguous(command_root, intent_path, receipt_path, request)
            cleanup_pre_apply(command_root, intent_path)
            raise


def recover_ambiguous_task_private_patch(
    *,
    repository_root: Path,
    state_root: Path,
    repository: str,
    expected_head: str,
    expected_tree: str,
    git_blob_sha1: str,
    sha256: str,
    bytes_: int,
    patch: bytes,
) -> TaskPrivatePatchResult:
    """Discard one exact ambiguous task workspace without re-applying its patch."""
    repository_root = exact_directory(repository_root, "resident repository")
    state_root = exact_private_directory(state_root, "task patch state root")
    require_disjoint_roots(repository_root, state_root)
    request = normalize_request(
        repository,
        expected_head,
        expected_tree,
        git_blob_sha1,
        sha256,
        bytes_,
        patch,
    )
    verify_resident_repository(repository_root, request)
    command_root = ensure_private_child(state_root, request.operation_id.removeprefix("sha256:"))
    with open_lock(command_root):
        receipt_path = command_root / "receipt.json"
        receipt = read_document(receipt_path)
        if receipt is not None:
            validate_receipt(receipt, request)
            return TaskPrivatePatchResult(receipt=receipt, source=None)
        intent_path = command_root / "intent.json"
        intent = read_document(intent_path)
        if intent is None:
            raise Refusal("no ambiguous task-private patch mutation exists")
        validate_intent(intent, request)
        return settle_ambiguous(command_root, intent_path, receipt_path, request)


def normalize_request(
    repository: str,
    expected_head: str,
    expected_tree: str,
    git_blob_sha1: str,
    sha256: str,
    bytes_: int,
    patch: bytes,
) -> TaskPrivatePatchRequest:
    if not REPOSITORY_PATTERN.fullmatch(repository):
        raise Refusal("repository identity is invalid")
    if not SHA1_PATTERN.fullmatch(expected_head) or not SHA1_PATTERN.fullmatch(expected_tree):
        raise Refusal("source commit/tree identity is invalid")
    identity = PatchIdentity.validate(git_blob_sha1, sha256, bytes_)
    validate_patch_identity(identity, patch)
    changed_paths = parse_changed_paths(patch)
    document = {
        "schema_version": SCHEMA_VERSION,
        "policy_generation": POLICY_GENERATION,
        "repository": repository,
        "expected_head": expected_head,
        "expected_tree": expected_tree,
        "patch": {
            "git_blob_sha1": identity.git_blob_sha1,
            "sha256": identity.sha256,
            "bytes": identity.bytes,
        },
        "changed_paths": list(changed_paths),
    }
    operation_id = sha256_bytes(canonical_bytes(document))
    return TaskPrivatePatchRequest(
        repository=repository,
        expected_head=expected_head,
        expected_tree=expected_tree,
        patch=identity,
        changed_paths=changed_paths,
        operation_id=operation_id,
    )


def validate_patch_identity(identity: PatchIdentity, patch: bytes) -> None:
    if len(patch) != identity.bytes:
        raise Refusal("patch byte count does not match its expected identity")
    if sha256_bytes(patch) != identity.sha256:
        raise Refusal("patch SHA-256 does not match its expected identity")
    framed = f"blob {len(patch)}\0".encode("ascii") + patch
    if hashlib.sha1(framed).hexdigest() != identity.git_blob_sha1:
        raise Refusal("patch Git blob identity does not match its expected identity")
    try:
        text = patch.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise Refusal("patch is not valid UTF-8") from error
    if b"\0" in patch:
        raise Refusal("patch contains a NUL byte")
    if "GIT binary patch" in text or "Binary files " in text:
        raise Refusal("binary patch forms are outside the reviewed boundary")


def parse_changed_paths(patch: bytes) -> tuple[str, ...]:
    text = patch.decode("utf-8", errors="strict")
    old_path: str | None | object = _UNSET
    paths: set[str] = set()
    saw_hunk = False
    for line in text.splitlines():
        if line.startswith("--- "):
            if old_path is not _UNSET:
                raise Refusal("patch contains an incomplete file header")
            old_path = parse_header_path(line[4:], "a/")
        elif line.startswith("+++ "):
            if old_path is _UNSET:
                raise Refusal("patch new-file header has no matching old-file header")
            new_path = parse_header_path(line[4:], "b/")
            if old_path is None and new_path is None:
                raise Refusal("patch file header has no repository path")
            if old_path is not None and new_path is not None and old_path != new_path:
                raise Refusal("rename/copy patches are outside the reviewed boundary")
            paths.add(new_path if new_path is not None else old_path)
            old_path = _UNSET
        elif line.startswith("@@ "):
            saw_hunk = True
    if old_path is not _UNSET or not saw_hunk or not paths:
        raise Refusal("patch is not an ordinary bounded unified diff")
    if len(paths) > MAX_CHANGED_PATHS:
        raise Refusal("patch changes too many paths")
    return tuple(sorted(paths))


_UNSET = object()


def parse_header_path(raw: str, prefix: str) -> str | None:
    raw = raw.split("\t", 1)[0]
    if raw == "/dev/null":
        return None
    if raw.startswith('"') or not raw.startswith(prefix):
        raise Refusal("patch path is outside the reviewed Git-style boundary")
    path = raw[len(prefix) :]
    if (
        not path
        or len(path.encode("utf-8")) > MAX_CHANGED_PATH_BYTES
        or path.startswith("/")
        or "\\" in path
        or any(ord(character) < 32 or ord(character) == 127 for character in path)
    ):
        raise Refusal("patch path is unsafe")
    components = path.split("/")
    if any(component in {"", ".", "..", ".git"} for component in components):
        raise Refusal("patch path is unsafe")
    return path


def verify_resident_repository(repository_root: Path, request: TaskPrivatePatchRequest) -> None:
    values = git_output(
        repository_root,
        ["rev-parse", f"{request.expected_head}^{{commit}}", f"{request.expected_head}^{{tree}}"],
    ).decode("ascii", errors="strict").splitlines()
    if values != [request.expected_head, request.expected_tree]:
        raise Refusal("resident repository no longer contains the exact source identity")
    remotes = git_output(repository_root, ["remote", "get-url", "--all", "origin"]).decode(
        "utf-8", errors="strict"
    ).splitlines()
    admitted = {
        f"git@github.com:{request.repository}.git",
        f"https://github.com/{request.repository}.git",
        f"https://github.com/{request.repository}",
    }
    if len(remotes) != 1 or remotes[0] not in admitted:
        raise Refusal("resident repository origin does not match the exact repository identity")


def validate_task_source(source: Path, request: TaskPrivatePatchRequest) -> None:
    source = exact_directory(source, "task-private source")
    git_dir = source / ".git"
    metadata = git_dir.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or git_dir.is_symlink():
        raise Refusal("task-private Git metadata is unavailable or unsafe")
    values = git_output(source, ["rev-parse", "HEAD", "HEAD^{tree}"]).decode(
        "ascii", errors="strict"
    ).splitlines()
    if values != [request.expected_head, request.expected_tree]:
        raise Refusal("task-private source no longer matches the exact base identity")
    status = git_output(source, ["status", "--porcelain=v1", "-z", "--untracked-files=all"])
    if status:
        raise Refusal("task-private source is not clean at the mutation boundary")


def validate_git_mutation_boundary(source: Path) -> None:
    dangerous = git_command(
        source,
        [
            "config",
            "--includes",
            "--name-only",
            "--null",
            "--get-regexp",
            r"^(filter\..*\.(clean|process)|core\.hooksPath|credential\.helper|diff\..*\.command|merge\..*\.driver|alias\..*|core\.fsmonitor)$",
        ],
        check=False,
    )
    if dangerous.returncode not in {0, 1}:
        raise Refusal("task-private Git configuration could not be inspected")
    if dangerous.returncode == 0 or dangerous.stdout:
        raise Refusal("task-private Git configuration contains executable or credential behavior")


def expected_result_tree_for_patch(
    source: Path,
    request: TaskPrivatePatchRequest,
    patch: bytes,
) -> str:
    index_root = source.parent / "patch-index"
    try:
        index_root.mkdir(mode=0o700)
    except FileExistsError as error:
        raise Refusal("task-private expected-result index already exists") from error
    index_path = index_root / "index"
    environment = {"GIT_INDEX_FILE": os.fspath(index_path)}
    try:
        git_command(source, ["read-tree", request.expected_tree], extra_environment=environment)
        checked = git_command(
            source,
            ["-c", "apply.ignoreWhitespace=false", "apply", "--check", "--cached", "--whitespace=nowarn", "-"],
            input_bytes=patch,
            extra_environment=environment,
            check=False,
        )
        if checked.returncode != 0:
            raise Refusal("patch applicability changed before mutation")
        applied = git_command(
            source,
            ["-c", "apply.ignoreWhitespace=false", "apply", "--cached", "--whitespace=nowarn", "-"],
            input_bytes=patch,
            extra_environment=environment,
            check=False,
        )
        if applied.returncode != 0:
            raise Refusal("patch could not produce an exact expected result tree")
        result = git_output(source, ["write-tree"], extra_environment=environment).decode(
            "ascii", errors="strict"
        ).strip()
        if not SHA1_PATTERN.fullmatch(result):
            raise Refusal("expected result tree identity is invalid")
        return result
    finally:
        shutil.rmtree(index_root, ignore_errors=True)


def check_patch_applicable(source: Path, patch: bytes) -> None:
    completed = git_command(
        source,
        ["-c", "apply.ignoreWhitespace=false", "apply", "--check", "--index", "--whitespace=nowarn", "-"],
        input_bytes=patch,
        check=False,
    )
    if completed.returncode != 0:
        raise Refusal("patch applicability changed before mutation")


def run_apply_process(source: Path, patch: bytes) -> int:
    argv = git_argv(
        ["-c", "apply.ignoreWhitespace=false", "apply", "--index", "--whitespace=nowarn", "-"]
    )
    process = subprocess.Popen(
        argv,
        cwd=source,
        env=git_environment(),
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        process.communicate(patch, timeout=GIT_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, 9)
        except ProcessLookupError:
            pass
        process.wait()
        return 124
    return process.returncode


def observe_applied_result(
    source: Path,
    request: TaskPrivatePatchRequest,
    expected_result_tree: str,
) -> dict[str, object]:
    head = git_output(source, ["rev-parse", "HEAD"]).decode("ascii", errors="strict").strip()
    if head != request.expected_head:
        raise AmbiguousMutation("task-private HEAD changed during patch mutation")
    result_tree = git_output(source, ["write-tree"]).decode("ascii", errors="strict").strip()
    if result_tree != expected_result_tree:
        raise AmbiguousMutation("task-private index does not match the exact patch result")
    unstaged = git_output(source, ["diff", "--name-only", "-z", "--"])
    if unstaged:
        raise AmbiguousMutation("task-private worktree does not match its patched index")
    untracked = git_output(source, ["ls-files", "--others", "--exclude-standard", "-z"])
    if untracked:
        raise AmbiguousMutation("task-private patch produced unexpected untracked paths")
    changed = decode_path_list(
        git_output(source, ["diff", "--cached", "--name-only", "-z", "HEAD", "--"])
    )
    if tuple(changed) != request.changed_paths:
        raise AmbiguousMutation("task-private changed paths do not match the admitted patch")
    return receipt_document(
        request,
        terminal_class="applied",
        result_tree=result_tree,
        retention_state="task_private_retained_for_verification",
        working_copy="index_and_worktree_exact_patch_result",
    )


def validate_retained_result(
    source: Path,
    request: TaskPrivatePatchRequest,
    receipt: dict[str, object],
) -> None:
    if not source.is_dir() or source.is_symlink():
        raise AmbiguousMutation("terminal task-private patch workspace is no longer retained")
    observed = observe_applied_result(source, request, str(receipt["result_tree"]))
    if canonical_bytes(observed) != canonical_bytes(receipt):
        raise AmbiguousMutation("terminal task-private patch receipt no longer matches retained state")


def settle_ambiguous(
    command_root: Path,
    intent_path: Path,
    receipt_path: Path,
    request: TaskPrivatePatchRequest,
) -> TaskPrivatePatchResult:
    try:
        owned_task.remove_task(command_root / "task")
    except (owned_task.Refusal, OSError) as error:
        raise AmbiguousMutation(
            "task-private patch mutation is ambiguous and cleanup is incomplete"
        ) from error
    receipt = receipt_document(
        request,
        terminal_class="ambiguous_rebuild_required",
        result_tree=None,
        retention_state="discarded_rebuild_required",
        working_copy="discarded_after_ambiguous_mutation",
    )
    publish_document(receipt_path, receipt, replace=False)
    intent_path.unlink(missing_ok=True)
    sync_directory(command_root)
    return TaskPrivatePatchResult(receipt=receipt, source=None)


def cleanup_pre_apply(command_root: Path, intent_path: Path) -> None:
    owned_task.remove_task(command_root / "task")
    intent_path.unlink(missing_ok=True)
    sync_directory(command_root)


def intent_document(
    request: TaskPrivatePatchRequest,
    expected_result_tree: str,
    phase: str,
) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "document_type": "glaeda-task-private-patch-intent",
        "policy_generation": POLICY_GENERATION,
        "operation_id": request.operation_id,
        "phase": phase,
        "source": {
            "repository": request.repository,
            "base_commit": request.expected_head,
            "base_tree": request.expected_tree,
        },
        "patch": patch_document(request.patch),
        "changed_paths": list(request.changed_paths),
        "expected_result_tree": expected_result_tree,
    }


def receipt_document(
    request: TaskPrivatePatchRequest,
    *,
    terminal_class: str,
    result_tree: str | None,
    retention_state: str,
    working_copy: str,
) -> dict[str, object]:
    return {
        "schema_version": SCHEMA_VERSION,
        "document_type": "glaeda-task-private-patch-receipt",
        "policy_generation": POLICY_GENERATION,
        "operation_id": request.operation_id,
        "terminal_class": terminal_class,
        "source": {
            "repository": request.repository,
            "base_commit": request.expected_head,
            "base_tree": request.expected_tree,
        },
        "patch": patch_document(request.patch),
        "changed_path_count": len(request.changed_paths),
        "changed_paths": list(request.changed_paths) if terminal_class == "applied" else [],
        "result_tree": result_tree,
        "head_unchanged": terminal_class == "applied",
        "working_copy": working_copy,
        "retention_state": retention_state,
        "contains_patch_content": False,
        "contains_private_path": False,
        "authorizes_execution": False,
        "authorizes_commit": False,
        "authorizes_publication": False,
        "authorizes_merge": False,
        "authorizes_deploy": False,
    }


def patch_document(identity: PatchIdentity) -> dict[str, object]:
    return {
        "git_blob_sha1": identity.git_blob_sha1,
        "sha256": identity.sha256,
        "bytes": identity.bytes,
    }


def validate_intent(document: dict[str, object], request: TaskPrivatePatchRequest) -> None:
    if (
        document.get("document_type") != "glaeda-task-private-patch-intent"
        or document.get("schema_version") != SCHEMA_VERSION
        or document.get("policy_generation") != POLICY_GENERATION
        or document.get("operation_id") != request.operation_id
        or document.get("source")
        != {
            "repository": request.repository,
            "base_commit": request.expected_head,
            "base_tree": request.expected_tree,
        }
        or document.get("patch") != patch_document(request.patch)
        or document.get("changed_paths") != list(request.changed_paths)
        or document.get("phase") not in {"prepared", "applying"}
        or not isinstance(document.get("expected_result_tree"), str)
        or not SHA1_PATTERN.fullmatch(str(document.get("expected_result_tree")))
    ):
        raise Refusal("task-private patch intent conflicts with the exact request")


def validate_receipt(document: dict[str, object], request: TaskPrivatePatchRequest) -> None:
    terminal = document.get("terminal_class")
    if (
        document.get("document_type") != "glaeda-task-private-patch-receipt"
        or document.get("schema_version") != SCHEMA_VERSION
        or document.get("policy_generation") != POLICY_GENERATION
        or document.get("operation_id") != request.operation_id
        or document.get("source")
        != {
            "repository": request.repository,
            "base_commit": request.expected_head,
            "base_tree": request.expected_tree,
        }
        or document.get("patch") != patch_document(request.patch)
        or terminal not in {"applied", "ambiguous_rebuild_required"}
        or document.get("contains_patch_content") is not False
        or document.get("contains_private_path") is not False
        or any(
            document.get(field) is not False
            for field in (
                "authorizes_execution",
                "authorizes_commit",
                "authorizes_publication",
                "authorizes_merge",
                "authorizes_deploy",
            )
        )
    ):
        raise Refusal("task-private patch receipt conflicts with the exact request")
    if terminal == "applied":
        if (
            document.get("changed_paths") != list(request.changed_paths)
            or document.get("changed_path_count") != len(request.changed_paths)
            or document.get("head_unchanged") is not True
            or document.get("retention_state") != "task_private_retained_for_verification"
            or document.get("working_copy") != "index_and_worktree_exact_patch_result"
            or not isinstance(document.get("result_tree"), str)
            or not SHA1_PATTERN.fullmatch(str(document.get("result_tree")))
        ):
            raise Refusal("applied task-private patch receipt is incomplete")
    else:
        if (
            document.get("changed_paths") != []
            or document.get("result_tree") is not None
            or document.get("head_unchanged") is not False
            or document.get("retention_state") != "discarded_rebuild_required"
        ):
            raise Refusal("ambiguous task-private patch receipt is incomplete")


def decode_path_list(raw: bytes) -> list[str]:
    paths = [os.fsdecode(value) for value in raw.split(b"\0") if value]
    if len(paths) > MAX_CHANGED_PATHS:
        raise AmbiguousMutation("task-private result changed too many paths")
    for path in paths:
        if parse_header_path(f"b/{path}", "b/") != path:
            raise AmbiguousMutation("task-private result contains an unsafe path")
    return sorted(paths)


def git_argv(arguments: list[str]) -> list[str]:
    return [
        GIT,
        "--no-optional-locks",
        "-c",
        "credential.helper=",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "diff.external=",
        *arguments,
    ]


def git_environment(extra: dict[str, str] | None = None) -> dict[str, str]:
    environment = owned_task.closed_environment(
        {
            "GIT_ALLOW_PROTOCOL": "",
            "GIT_ASKPASS": "/bin/false",
            "GIT_ATTR_NOSYSTEM": "1",
            "GIT_NO_LAZY_FETCH": "1",
            "GIT_NO_REPLACE_OBJECTS": "1",
            "GIT_OPTIONAL_LOCKS": "0",
            "GIT_PROTOCOL_FROM_USER": "0",
            "GIT_TERMINAL_PROMPT": "0",
        }
    )
    if extra:
        environment.update(extra)
    return environment


def git_command(
    cwd: Path,
    arguments: list[str],
    *,
    input_bytes: bytes | None = None,
    extra_environment: dict[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    completed = subprocess.run(
        git_argv(arguments),
        cwd=cwd,
        env=git_environment(extra_environment),
        input=input_bytes,
        stdin=None if input_bytes is not None else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=GIT_TIMEOUT_SECONDS,
        check=False,
    )
    if len(completed.stdout) > MAX_GIT_OUTPUT_BYTES or len(completed.stderr) > MAX_GIT_OUTPUT_BYTES:
        raise Refusal("bounded Git operation exceeded its output ceiling")
    if check and completed.returncode != 0:
        raise Refusal("bounded Git operation failed")
    return completed


def git_output(
    cwd: Path,
    arguments: list[str],
    *,
    extra_environment: dict[str, str] | None = None,
) -> bytes:
    return git_command(cwd, arguments, extra_environment=extra_environment).stdout


def exact_directory(path: Path, label: str) -> Path:
    if not path.is_absolute():
        raise Refusal(f"{label} must be absolute")
    try:
        metadata = path.lstat()
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise Refusal(f"{label} is unavailable") from error
    if resolved != path or not stat.S_ISDIR(metadata.st_mode) or path.is_symlink():
        raise Refusal(f"{label} must be one canonical plain directory")
    return path


def exact_private_directory(path: Path, label: str) -> Path:
    path = exact_directory(path, label)
    metadata = path.lstat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise Refusal(f"{label} must be a private current-user directory")
    return path


def require_disjoint_roots(repository_root: Path, state_root: Path) -> None:
    if (
        repository_root == state_root
        or repository_root.is_relative_to(state_root)
        or state_root.is_relative_to(repository_root)
    ):
        raise Refusal("resident repository and task patch state root must be disjoint")


def ensure_private_child(parent: Path, name: str) -> Path:
    path = parent / name
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        pass
    metadata = path.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or path.is_symlink()
        or metadata.st_uid != os.getuid()
        or stat.S_IMODE(metadata.st_mode) != 0o700
    ):
        raise Refusal("task patch state contains an unsafe filesystem object")
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
        raise Refusal("task patch lock is not one private stable empty file")
    lock = os.fdopen(descriptor, "r+b", buffering=0)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError as error:
        lock.close()
        raise Refusal("the exact task-private patch operation is already active") from error
    return lock


def canonical_bytes(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def reject_json_constant(value: str) -> NoReturn:
    raise ValueError(f"non-finite JSON number: {value}")


def read_document(path: Path) -> dict[str, object] | None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return None
    if (
        not stat.S_ISREG(metadata.st_mode)
        or path.is_symlink()
        or metadata.st_uid != os.getuid()
        or metadata.st_nlink != 1
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_size > MAX_DOCUMENT_BYTES
    ):
        raise Refusal("task patch state contains an unsafe document")
    raw = path.read_bytes()
    try:
        value = json.loads(raw, parse_constant=reject_json_constant)
    except (UnicodeError, ValueError) as error:
        raise Refusal("task patch state document is corrupt") from error
    if not isinstance(value, dict) or canonical_bytes(value) + b"\n" != raw:
        raise Refusal("task patch state document is noncanonical")
    return value


def publish_document(path: Path, value: dict[str, object], *, replace: bool) -> None:
    raw = canonical_bytes(value) + b"\n"
    if len(raw) > MAX_DOCUMENT_BYTES:
        raise Refusal("task patch document exceeds its fixed ceiling")
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
                raise Refusal("task patch document already exists") from error
            temporary.unlink()
        sync_directory(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
