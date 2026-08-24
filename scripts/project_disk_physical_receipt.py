#!/usr/bin/env python3
"""Collect and strictly parse an observation-only physical Lima project-disk receipt.

This program deliberately contains no Lima standalone-disk layout or lock semantics. The operator
supplies the exact standalone-disk directory and pre-captured Lima/guest evidence. Direct entries
stay opaque until an operator explicitly labels exact observed entries after inspecting a real
receipt.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
RECEIPT_TYPE = "smolrunner-project-disk-physical-observation"
HOST_IDENTITY_RECEIPT_TYPE = "smolrunner-lima-host-identity-observation"
MAX_JSON_BYTES = 65_536
MAX_MOUNTINFO_BYTES = 1_048_576
MAX_RECEIPT_BYTES = 262_144
MAX_DIRECTORY_ENTRIES = 64
MAX_ENTRY_NAME_BYTES = 255
MAX_EXPLICIT_SMALL_FILE_BYTES = 4_096
MAX_LABEL_BYTES = 256
ALLOCATED_BLOCK_BYTES = 512

SNAPSHOT_KEYS = {
    "device",
    "inode",
    "mode",
    "uid",
    "gid",
    "link_count",
    "logical_bytes",
    "allocated_bytes",
    "mtime_ns",
    "ctime_ns",
    "birthtime_ns",
    "flags",
    "generation",
}


class ReceiptError(ValueError):
    pass


def _sha256(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def _validate_sha256(value: Any, field: str) -> str:
    if not isinstance(value, str) or len(value) != 71 or not value.startswith("sha256:"):
        raise ReceiptError(f"{field} must be canonical sha256:<64 lowercase hex>")
    suffix = value[7:]
    if any(ch not in "0123456789abcdef" for ch in suffix):
        raise ReceiptError(f"{field} must be canonical sha256:<64 lowercase hex>")
    return value


def _validate_label(value: Any, field: str) -> str:
    if not isinstance(value, str):
        raise ReceiptError(f"{field} must be one bounded ASCII label")
    encoded = value.encode("utf-8")
    if not encoded or len(encoded) > MAX_LABEL_BYTES:
        raise ReceiptError(f"{field} must be one bounded ASCII label")
    if any(byte < 0x21 or byte > 0x7E for byte in encoded):
        raise ReceiptError(f"{field} must be one bounded ASCII label")
    return value


def _positive(value: Any, field: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ReceiptError(f"{field} must be a positive integer")
    return value


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ReceiptError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _read_bounded(path: Path, maximum: int, label: str) -> bytes:
    with path.open("rb") as handle:
        data = handle.read(maximum + 1)
    if len(data) > maximum:
        raise ReceiptError(f"{label} exceeds the reviewed byte bound")
    return data


def _decode_json_bytes(data: bytes, label: str) -> Any:
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ReceiptError(f"{label} is not UTF-8 JSON") from exc
    try:
        return json.loads(text, object_pairs_hook=_reject_duplicate_keys)
    except ReceiptError:
        raise
    except json.JSONDecodeError as exc:
        raise ReceiptError(f"{label} is malformed JSON") from exc


def _read_json_evidence(path: Path, label: str) -> dict[str, Any]:
    data = _read_bounded(path, MAX_JSON_BYTES, label)
    return {"sha256": _sha256(data), "value": _decode_json_bytes(data, label)}


def _bytes_view(value: bytes) -> dict[str, Any]:
    try:
        utf8 = value.decode("utf-8")
    except UnicodeDecodeError:
        utf8 = None
    return {"hex": value.hex(), "utf8": utf8}


def _validate_bytes_view(value: Any, field: str) -> bytes:
    if not isinstance(value, dict) or set(value) != {"hex", "utf8"}:
        raise ReceiptError(f"{field} byte view is invalid")
    raw_hex = value["hex"]
    if not isinstance(raw_hex, str) or len(raw_hex) % 2:
        raise ReceiptError(f"{field} byte view is invalid")
    try:
        raw = bytes.fromhex(raw_hex)
    except ValueError as exc:
        raise ReceiptError(f"{field} byte view is invalid") from exc
    utf8 = value["utf8"]
    if utf8 is not None:
        if not isinstance(utf8, str) or utf8.encode("utf-8") != raw:
            raise ReceiptError(f"{field} UTF-8 view disagrees with exact bytes")
    return raw


def _os_bytes(value: str | bytes | os.PathLike[str] | os.PathLike[bytes]) -> bytes:
    if isinstance(value, bytes):
        return value
    return os.fsencode(value)


def _entry_kind(mode: int) -> str:
    if stat.S_ISREG(mode):
        return "regular_file"
    if stat.S_ISDIR(mode):
        return "directory"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISBLK(mode):
        return "block_device"
    if stat.S_ISCHR(mode):
        return "character_device"
    if stat.S_ISFIFO(mode):
        return "fifo"
    if stat.S_ISSOCK(mode):
        return "socket"
    return "other"


def _birthtime_ns(observation: os.stat_result) -> int | None:
    direct = getattr(observation, "st_birthtime_ns", None)
    if direct is not None:
        return int(direct)
    seconds = getattr(observation, "st_birthtime", None)
    if seconds is None:
        return None
    return int(seconds * 1_000_000_000)


def _snapshot(observation: os.stat_result) -> dict[str, Any]:
    blocks = getattr(observation, "st_blocks", None)
    allocated_bytes = None if blocks is None else int(blocks) * ALLOCATED_BLOCK_BYTES
    return {
        "device": int(observation.st_dev),
        "inode": int(observation.st_ino),
        "mode": int(observation.st_mode),
        "uid": int(observation.st_uid),
        "gid": int(observation.st_gid),
        "link_count": int(observation.st_nlink),
        "logical_bytes": int(observation.st_size),
        "allocated_bytes": allocated_bytes,
        "mtime_ns": int(observation.st_mtime_ns),
        "ctime_ns": int(observation.st_ctime_ns),
        "birthtime_ns": _birthtime_ns(observation),
        "flags": getattr(observation, "st_flags", None),
        "generation": getattr(observation, "st_gen", None),
    }


def _validate_snapshot(value: Any, field: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != SNAPSHOT_KEYS:
        raise ReceiptError(f"{field} metadata field set is invalid")
    for key in (
        "device",
        "inode",
        "mode",
        "uid",
        "gid",
        "link_count",
        "logical_bytes",
        "mtime_ns",
        "ctime_ns",
    ):
        if isinstance(value[key], bool) or not isinstance(value[key], int) or value[key] < 0:
            raise ReceiptError(f"{field}.{key} is invalid")
    for key in ("allocated_bytes", "birthtime_ns", "flags", "generation"):
        current = value[key]
        if current is not None and (isinstance(current, bool) or not isinstance(current, int) or current < 0):
            raise ReceiptError(f"{field}.{key} is invalid")
    if value["inode"] == 0 or value["link_count"] == 0:
        raise ReceiptError(f"{field} lacks a usable exact entry identity")
    return value


def _stability_key(snapshot: dict[str, Any]) -> tuple[Any, ...]:
    return tuple(
        snapshot[key]
        for key in (
            "device",
            "inode",
            "mode",
            "uid",
            "gid",
            "link_count",
            "logical_bytes",
            "mtime_ns",
            "ctime_ns",
            "birthtime_ns",
            "generation",
        )
    )


def _same_observation(left: dict[str, Any], right: dict[str, Any]) -> bool:
    return _stability_key(left) == _stability_key(right)


def _validate_exact_absolute_path(path: Path, label: str) -> Path:
    raw = os.fspath(path)
    if not isinstance(raw, str) or not os.path.isabs(raw) or os.path.normpath(raw) != raw:
        raise ReceiptError(f"{label} must be one exact normalized absolute UTF-8 path")
    if "\x00" in raw:
        raise ReceiptError(f"{label} must be one exact normalized absolute UTF-8 path")
    return path


def _path_from_json(value: Any, label: str) -> Path:
    if not isinstance(value, str):
        raise ReceiptError(f"{label} must be one exact normalized absolute UTF-8 path")
    return _validate_exact_absolute_path(Path(value), label)


def _directory_snapshot_from_fd(directory_fd: int) -> dict[str, Any]:
    observed = os.fstat(directory_fd)
    if not stat.S_ISDIR(observed.st_mode):
        raise ReceiptError("project disk directory descriptor is not a directory")
    return _snapshot(observed)


def _entry_snapshot(directory_fd: int, name: str) -> tuple[dict[str, Any], str]:
    observed = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    return _snapshot(observed), _entry_kind(observed.st_mode)


def _read_fd_bounded(file_fd: int, maximum: int) -> bytes:
    chunks: list[bytes] = []
    total = 0
    while total <= maximum:
        chunk = os.read(file_fd, maximum + 1 - total)
        if not chunk:
            break
        chunks.append(chunk)
        total += len(chunk)
    data = b"".join(chunks)
    if len(data) > maximum:
        raise ReceiptError("explicit small-file read exceeds the reviewed bound")
    return data


def _read_explicit_small_file(
    directory_fd: int,
    name: str,
    expected: dict[str, Any],
) -> dict[str, Any]:
    if expected["logical_bytes"] > MAX_EXPLICIT_SMALL_FILE_BYTES:
        raise ReceiptError(f"explicit small-file read exceeds bound: {name}")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    file_fd = os.open(name, flags, dir_fd=directory_fd)
    try:
        before = _snapshot(os.fstat(file_fd))
        if not _same_observation(before, expected) or not stat.S_ISREG(before["mode"]):
            raise ReceiptError(f"explicit small-file entry changed before read: {name}")
        data = _read_fd_bounded(file_fd, MAX_EXPLICIT_SMALL_FILE_BYTES)
        if len(data) != before["logical_bytes"]:
            raise ReceiptError(f"explicit small-file length changed during read: {name}")
        after = _snapshot(os.fstat(file_fd))
        if not _same_observation(before, after):
            raise ReceiptError(f"explicit small-file entry changed during read: {name}")
    finally:
        os.close(file_fd)
    view = _bytes_view(data)
    view["sha256"] = _sha256(data)
    return view


def _capture_directory(disk_directory: Path, explicit_small_reads: set[str]) -> dict[str, Any]:
    disk_directory = _validate_exact_absolute_path(disk_directory, "disk directory")
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    directory_fd = os.open(disk_directory, flags)
    try:
        before_directory = _directory_snapshot_from_fd(directory_fd)
        names = os.listdir(directory_fd)
        if len(names) > MAX_DIRECTORY_ENTRIES:
            raise ReceiptError("project disk directory exceeds the reviewed direct-entry bound")

        entries: list[dict[str, Any]] = []
        seen: set[str] = set()
        for name in names:
            name_bytes = _os_bytes(name)
            if not name_bytes or len(name_bytes) > MAX_ENTRY_NAME_BYTES:
                raise ReceiptError("project disk directory contains an invalid entry name length")
            if b"/" in name_bytes or name_bytes in (b".", b".."):
                raise ReceiptError("project disk directory contains a non-direct entry name")
            name_hex = name_bytes.hex()
            if name_hex in seen:
                raise ReceiptError("project disk directory contains duplicate encoded entry names")
            seen.add(name_hex)

            snapshot, kind = _entry_snapshot(directory_fd, name)
            entry: dict[str, Any] = {
                "name": _bytes_view(name_bytes),
                "kind": kind,
                "metadata": snapshot,
            }
            if kind == "symlink":
                target = os.readlink(name, dir_fd=directory_fd)
                target_bytes = _os_bytes(target)
                if len(target_bytes) > MAX_EXPLICIT_SMALL_FILE_BYTES:
                    raise ReceiptError(f"symlink target exceeds reviewed bound: {name}")
                entry["symlink_target"] = _bytes_view(target_bytes)
            if name in explicit_small_reads:
                if kind != "regular_file":
                    raise ReceiptError(f"explicit small-file read is not a regular file: {name}")
                entry["explicit_small_file_bytes"] = _read_explicit_small_file(
                    directory_fd, name, snapshot
                )
            after_snapshot, after_kind = _entry_snapshot(directory_fd, name)
            if after_kind != kind or not _same_observation(snapshot, after_snapshot):
                raise ReceiptError(f"project disk directory entry changed during observation: {name}")
            entries.append(entry)

        missing_reads = explicit_small_reads.difference(names)
        if missing_reads:
            raise ReceiptError(
                "explicit small-file entry is absent: " + ", ".join(sorted(missing_reads))
            )

        after_directory = _directory_snapshot_from_fd(directory_fd)
        rebound_fd = os.open(disk_directory, flags)
        try:
            rebound_directory = _directory_snapshot_from_fd(rebound_fd)
        finally:
            os.close(rebound_fd)
        if not _same_observation(before_directory, after_directory) or not _same_observation(
            after_directory, rebound_directory
        ):
            raise ReceiptError("project disk directory changed or was rebound during observation")
    finally:
        os.close(directory_fd)

    entries.sort(key=lambda item: item["name"]["hex"])
    return {"path": os.fspath(disk_directory), "metadata": before_directory, "entries": entries}


def _parse_host_identity(path: Path, expected_instance: str) -> dict[str, Any]:
    data = _read_bounded(path, MAX_JSON_BYTES, "Lima host identity receipt")
    value = _decode_json_bytes(data, "Lima host identity receipt")
    expected_keys = {
        "schema_version",
        "receipt_type",
        "instance",
        "lima_host_identity",
        "lima_request_identity",
    }
    if not isinstance(value, dict) or set(value) != expected_keys:
        raise ReceiptError("Lima host identity receipt has an unexpected field set")
    if value["schema_version"] != SCHEMA_VERSION or value["receipt_type"] != HOST_IDENTITY_RECEIPT_TYPE:
        raise ReceiptError("Lima host identity receipt schema/type is unsupported")
    if value["instance"] != expected_instance:
        raise ReceiptError("Lima host identity receipt names another resident instance")
    _validate_label(value["instance"], "host_identity.instance")
    _validate_sha256(value["lima_host_identity"], "lima_host_identity")
    _validate_sha256(value["lima_request_identity"], "lima_request_identity")
    value["source_sha256"] = _sha256(data)
    return value


def _parse_guest_stat(path: Path) -> dict[str, int]:
    data = _read_bounded(path, 256, "guest project stat")
    try:
        line = data.decode("ascii").strip()
    except UnicodeDecodeError as exc:
        raise ReceiptError("guest project stat must be canonical ASCII") from exc
    fields = line.split(":")
    if len(fields) != 2 or any(not field.isdigit() for field in fields):
        raise ReceiptError("guest project stat must be one canonical %d:%i line")
    device, inode = (int(field) for field in fields)
    if device < 0 or inode <= 0:
        raise ReceiptError("guest project stat contains an invalid device/inode")
    return {"device": device, "inode": inode}


def _decode_mountinfo_field(field: bytes) -> bytes:
    result = bytearray()
    index = 0
    while index < len(field):
        if field[index] != 0x5C:
            result.append(field[index])
            index += 1
            continue
        if index + 3 >= len(field):
            raise ReceiptError("guest mountinfo contains a truncated escape")
        octal = field[index + 1 : index + 4]
        if any(byte < ord("0") or byte > ord("7") for byte in octal):
            raise ReceiptError("guest mountinfo contains an invalid escape")
        decoded = int(octal.decode("ascii"), 8)
        if decoded not in (9, 10, 32, 92):
            raise ReceiptError("guest mountinfo contains an unreviewed escape")
        result.append(decoded)
        index += 4
    return bytes(result)


def _parse_mountinfo(path: Path, expected_mountpoint: str) -> dict[str, Any]:
    expected = _os_bytes(expected_mountpoint)
    data = _read_bounded(path, MAX_MOUNTINFO_BYTES, "guest mountinfo")
    matches: list[dict[str, Any]] = []
    for raw_line in data.splitlines():
        if not raw_line:
            continue
        if b" - " not in raw_line:
            raise ReceiptError("guest mountinfo contains a malformed line")
        left, right = raw_line.split(b" - ", 1)
        left_fields = left.split(b" ")
        right_fields = right.split(b" ")
        if len(left_fields) < 6 or len(right_fields) < 3:
            raise ReceiptError("guest mountinfo contains an incomplete line")
        mountpoint = _decode_mountinfo_field(left_fields[4])
        if mountpoint != expected:
            continue
        device_fields = left_fields[2].split(b":", 1)
        if len(device_fields) != 2 or any(not field.isdigit() for field in device_fields):
            raise ReceiptError("guest mountinfo contains an invalid major:minor device")
        try:
            mount_id = int(left_fields[0])
            parent_id = int(left_fields[1])
        except ValueError as exc:
            raise ReceiptError("guest mountinfo contains an invalid mount id") from exc
        try:
            mount_options = left_fields[5].decode("ascii")
            filesystem_type = right_fields[0].decode("ascii")
            super_options = b" ".join(right_fields[2:]).decode("ascii")
        except UnicodeDecodeError as exc:
            raise ReceiptError("guest mountinfo contains non-ASCII option/type evidence") from exc
        matches.append(
            {
                "mount_id": mount_id,
                "parent_id": parent_id,
                "device_major": int(device_fields[0]),
                "device_minor": int(device_fields[1]),
                "root": _bytes_view(_decode_mountinfo_field(left_fields[3])),
                "mountpoint": _bytes_view(mountpoint),
                "mount_options": mount_options,
                "filesystem_type": filesystem_type,
                "mount_source": _bytes_view(_decode_mountinfo_field(right_fields[1])),
                "super_options": super_options,
                "raw_line_sha256": _sha256(raw_line),
            }
        )
    if len(matches) != 1:
        raise ReceiptError("guest mountinfo must contain exactly one exact project mountpoint")
    return matches[0]


def _declared_binding(args: argparse.Namespace) -> dict[str, Any]:
    return {
        "source": "declared_from_exact_p1_lease_state",
        "project_identity": _validate_label(args.project_identity, "project_identity"),
        "project_disk_id": _validate_label(args.project_disk_id, "project_disk_id"),
        "project_disk_generation": _positive(args.project_disk_generation, "project_disk_generation"),
        "project_disk_revision": _positive(args.project_disk_revision, "project_disk_revision"),
        "attachment_generation": _positive(args.attachment_generation, "attachment_generation"),
        "resident_sandbox_id": _validate_label(args.resident_sandbox_id, "resident_sandbox_id"),
        "resident_sandbox_generation": _positive(
            args.resident_sandbox_generation, "resident_sandbox_generation"
        ),
    }


def _entry_lookup(directory: dict[str, Any]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    for entry in directory["entries"]:
        utf8 = entry["name"]["utf8"]
        if utf8 is not None:
            result[utf8] = entry
    return result


def _explicit_role_labels(
    directory: dict[str, Any], backing: str | None, lock: str | None
) -> dict[str, Any] | None:
    if (backing is None) != (lock is None):
        raise ReceiptError("observed backing and lock entry labels must be supplied together")
    if backing is None:
        return None
    entries = _entry_lookup(directory)
    if backing not in entries or lock not in entries:
        raise ReceiptError("an explicit observed role label does not name a captured direct entry")
    if backing == lock:
        raise ReceiptError("observed backing and lock entry labels must name distinct entries")
    return {
        "source": "explicit_operator_observation",
        "observed_backing_entry": backing,
        "observed_lock_entry": lock,
    }


def capture(args: argparse.Namespace) -> dict[str, Any]:
    instance = _validate_label(args.resident_sandbox_instance, "resident_sandbox_instance")
    host_identity = _parse_host_identity(Path(args.host_identity_json), instance)
    directory = _capture_directory(Path(args.disk_directory), set(args.read_small_entry))
    guest_mountpoint = os.fspath(
        _validate_exact_absolute_path(Path(args.guest_project_mountpoint), "guest project mountpoint")
    )
    receipt = {
        "schema_version": SCHEMA_VERSION,
        "receipt_type": RECEIPT_TYPE,
        "authority": {
            "class": "read_only_physical_observation",
            "mutation_authority": "none",
            "smolrunner_ownership_authority": "unresolved",
            "production_project_filesystem_proof": "blocked_on_565_p2",
        },
        "declared_binding": _declared_binding(args),
        "lima_host_identity": host_identity,
        "disk_directory": directory,
        "correlation": {
            "resident_sandbox_instance": instance,
            "lima_disk_json": _read_json_evidence(Path(args.lima_disk_json), "Lima disk JSON"),
            "resident_instance_json": _read_json_evidence(
                Path(args.resident_instance_json), "resident Lima instance JSON"
            ),
            "guest_project_filesystem": {
                "mountpoint": guest_mountpoint,
                "stat": _parse_guest_stat(Path(args.guest_project_stat)),
                "mountinfo": _parse_mountinfo(Path(args.guest_mountinfo), guest_mountpoint),
            },
        },
        "operator_role_labels": _explicit_role_labels(
            directory, args.observed_backing_entry, args.observed_lock_entry
        ),
    }
    validate_receipt(receipt)
    return receipt


def _require_exact_keys(value: Any, keys: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise ReceiptError(f"{label} has an unexpected field set")
    return value


def _validate_declared_binding(value: Any) -> None:
    binding = _require_exact_keys(
        value,
        {
            "source",
            "project_identity",
            "project_disk_id",
            "project_disk_generation",
            "project_disk_revision",
            "attachment_generation",
            "resident_sandbox_id",
            "resident_sandbox_generation",
        },
        "declared_binding",
    )
    if binding["source"] != "declared_from_exact_p1_lease_state":
        raise ReceiptError("declared binding source is invalid")
    _validate_label(binding["project_identity"], "declared_binding.project_identity")
    _validate_label(binding["project_disk_id"], "declared_binding.project_disk_id")
    _positive(binding["project_disk_generation"], "declared_binding.project_disk_generation")
    _positive(binding["project_disk_revision"], "declared_binding.project_disk_revision")
    _positive(binding["attachment_generation"], "declared_binding.attachment_generation")
    _validate_label(binding["resident_sandbox_id"], "declared_binding.resident_sandbox_id")
    _positive(binding["resident_sandbox_generation"], "declared_binding.resident_sandbox_generation")


def _validate_entry(entry: Any, index: int) -> str | None:
    if not isinstance(entry, dict):
        raise ReceiptError("disk directory entry is invalid")
    allowed = {"name", "kind", "metadata", "symlink_target", "explicit_small_file_bytes"}
    if set(entry).difference(allowed) or not {"name", "kind", "metadata"}.issubset(entry):
        raise ReceiptError("disk directory entry has an unexpected field set")
    raw_name = _validate_bytes_view(entry["name"], f"disk_directory.entries[{index}].name")
    if not raw_name or len(raw_name) > MAX_ENTRY_NAME_BYTES or b"/" in raw_name:
        raise ReceiptError("disk directory entry name is invalid")
    kind = entry["kind"]
    if kind not in {
        "regular_file",
        "directory",
        "symlink",
        "block_device",
        "character_device",
        "fifo",
        "socket",
        "other",
    }:
        raise ReceiptError("disk directory entry kind is invalid")
    _validate_snapshot(entry["metadata"], f"disk_directory.entries[{index}].metadata")
    if "symlink_target" in entry:
        if kind != "symlink":
            raise ReceiptError("symlink target appears on a non-symlink entry")
        if len(_validate_bytes_view(entry["symlink_target"], "symlink_target")) > MAX_EXPLICIT_SMALL_FILE_BYTES:
            raise ReceiptError("symlink target exceeds reviewed bound")
    if kind == "symlink" and "symlink_target" not in entry:
        raise ReceiptError("symlink entry lacks exact target evidence")
    if "explicit_small_file_bytes" in entry:
        if kind != "regular_file":
            raise ReceiptError("explicit small-file bytes appear on a non-regular entry")
        evidence = _require_exact_keys(
            entry["explicit_small_file_bytes"], {"hex", "utf8", "sha256"}, "small file evidence"
        )
        raw = _validate_bytes_view({"hex": evidence["hex"], "utf8": evidence["utf8"]}, "small file")
        if len(raw) > MAX_EXPLICIT_SMALL_FILE_BYTES:
            raise ReceiptError("explicit small-file evidence exceeds reviewed bound")
        _validate_sha256(evidence["sha256"], "small_file.sha256")
        if _sha256(raw) != evidence["sha256"]:
            raise ReceiptError("explicit small-file digest disagrees with captured bytes")
    return entry["name"]["utf8"]


def _validate_mountinfo(value: Any) -> None:
    evidence = _require_exact_keys(
        value,
        {
            "mount_id",
            "parent_id",
            "device_major",
            "device_minor",
            "root",
            "mountpoint",
            "mount_options",
            "filesystem_type",
            "mount_source",
            "super_options",
            "raw_line_sha256",
        },
        "guest mountinfo evidence",
    )
    for key in ("mount_id", "parent_id", "device_major", "device_minor"):
        if isinstance(evidence[key], bool) or not isinstance(evidence[key], int) or evidence[key] < 0:
            raise ReceiptError(f"guest mountinfo {key} is invalid")
    if evidence["mount_id"] == 0:
        raise ReceiptError("guest mountinfo mount id is invalid")
    _validate_bytes_view(evidence["root"], "guest mountinfo root")
    _validate_bytes_view(evidence["mountpoint"], "guest mountinfo mountpoint")
    _validate_bytes_view(evidence["mount_source"], "guest mountinfo source")
    for key in ("mount_options", "filesystem_type", "super_options"):
        if not isinstance(evidence[key], str) or not evidence[key].isascii():
            raise ReceiptError(f"guest mountinfo {key} is invalid")
    _validate_sha256(evidence["raw_line_sha256"], "guest mountinfo row digest")


def validate_receipt(receipt: Any) -> dict[str, Any]:
    root = _require_exact_keys(
        receipt,
        {
            "schema_version",
            "receipt_type",
            "authority",
            "declared_binding",
            "lima_host_identity",
            "disk_directory",
            "correlation",
            "operator_role_labels",
        },
        "project disk physical receipt",
    )
    if root["schema_version"] != SCHEMA_VERSION or root["receipt_type"] != RECEIPT_TYPE:
        raise ReceiptError("project disk physical receipt schema/type is unsupported")

    authority = _require_exact_keys(
        root["authority"],
        {
            "class",
            "mutation_authority",
            "smolrunner_ownership_authority",
            "production_project_filesystem_proof",
        },
        "authority",
    )
    if authority != {
        "class": "read_only_physical_observation",
        "mutation_authority": "none",
        "smolrunner_ownership_authority": "unresolved",
        "production_project_filesystem_proof": "blocked_on_565_p2",
    }:
        raise ReceiptError("physical receipt authority fields must remain observation-only")
    _validate_declared_binding(root["declared_binding"])

    host = _require_exact_keys(
        root["lima_host_identity"],
        {
            "schema_version",
            "receipt_type",
            "instance",
            "lima_host_identity",
            "lima_request_identity",
            "source_sha256",
        },
        "lima_host_identity",
    )
    if host["schema_version"] != SCHEMA_VERSION or host["receipt_type"] != HOST_IDENTITY_RECEIPT_TYPE:
        raise ReceiptError("embedded Lima host identity schema/type is unsupported")
    _validate_label(host["instance"], "lima_host_identity.instance")
    _validate_sha256(host["lima_host_identity"], "lima_host_identity.digest")
    _validate_sha256(host["lima_request_identity"], "lima_host_identity.request_digest")
    _validate_sha256(host["source_sha256"], "lima_host_identity.source_sha256")

    directory = _require_exact_keys(
        root["disk_directory"], {"path", "metadata", "entries"}, "disk_directory"
    )
    _path_from_json(directory["path"], "disk directory")
    _validate_snapshot(directory["metadata"], "disk_directory.metadata")
    entries = directory["entries"]
    if not isinstance(entries, list) or len(entries) > MAX_DIRECTORY_ENTRIES:
        raise ReceiptError("disk directory entry list exceeds the reviewed bound")
    encoded_names: set[str] = set()
    utf8_names: set[str] = set()
    for index, entry in enumerate(entries):
        utf8 = _validate_entry(entry, index)
        name_hex = entry["name"]["hex"]
        if name_hex in encoded_names:
            raise ReceiptError("disk directory entry names must be unique")
        encoded_names.add(name_hex)
        if utf8 is not None:
            utf8_names.add(utf8)

    correlation = _require_exact_keys(
        root["correlation"],
        {
            "resident_sandbox_instance",
            "lima_disk_json",
            "resident_instance_json",
            "guest_project_filesystem",
        },
        "correlation",
    )
    instance = _validate_label(correlation["resident_sandbox_instance"], "resident_sandbox_instance")
    if instance != host["instance"]:
        raise ReceiptError("correlation resident instance disagrees with host identity receipt")
    for field in ("lima_disk_json", "resident_instance_json"):
        evidence = _require_exact_keys(correlation[field], {"sha256", "value"}, field)
        _validate_sha256(evidence["sha256"], f"{field}.sha256")

    guest = _require_exact_keys(
        correlation["guest_project_filesystem"], {"mountpoint", "stat", "mountinfo"}, "guest"
    )
    guest_path = _path_from_json(guest["mountpoint"], "guest project mountpoint")
    guest_stat = _require_exact_keys(guest["stat"], {"device", "inode"}, "guest stat")
    if (
        isinstance(guest_stat["device"], bool)
        or not isinstance(guest_stat["device"], int)
        or guest_stat["device"] < 0
        or isinstance(guest_stat["inode"], bool)
        or not isinstance(guest_stat["inode"], int)
        or guest_stat["inode"] <= 0
    ):
        raise ReceiptError("guest stat device/inode is invalid")
    _validate_mountinfo(guest["mountinfo"])
    observed_mountpoint = _validate_bytes_view(guest["mountinfo"]["mountpoint"], "guest mountpoint row")
    if observed_mountpoint != os.fsencode(os.fspath(guest_path)):
        raise ReceiptError("guest mountinfo row disagrees with declared exact project mountpoint")

    labels = root["operator_role_labels"]
    if labels is not None:
        labels = _require_exact_keys(
            labels,
            {"source", "observed_backing_entry", "observed_lock_entry"},
            "operator_role_labels",
        )
        if labels["source"] != "explicit_operator_observation":
            raise ReceiptError("operator role labels must remain explicitly observed")
        backing = labels["observed_backing_entry"]
        lock = labels["observed_lock_entry"]
        if not isinstance(backing, str) or not isinstance(lock, str):
            raise ReceiptError("operator role labels must be UTF-8 direct entry names")
        if backing not in utf8_names or lock not in utf8_names or backing == lock:
            raise ReceiptError("operator role labels must name distinct captured direct entries")

    return root


def validate_file(path: Path) -> dict[str, Any]:
    data = _read_bounded(path, MAX_RECEIPT_BYTES, "project disk physical receipt")
    receipt = _decode_json_bytes(data, "project disk physical receipt")
    return validate_receipt(receipt)


def _encoded_json(value: Any) -> bytes:
    try:
        data = (json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
    except (TypeError, UnicodeError) as exc:
        raise ReceiptError("receipt could not be encoded as bounded UTF-8 JSON") from exc
    if len(data) > MAX_RECEIPT_BYTES:
        raise ReceiptError("project disk physical receipt exceeds the reviewed output bound")
    return data


def _write_json(value: Any, output: str | None) -> None:
    data = _encoded_json(value)
    if output is None:
        sys.stdout.buffer.write(data)
        return
    output_path = _validate_exact_absolute_path(Path(output), "output path")
    file_fd = os.open(output_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(file_fd, "wb", closefd=False) as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        os.close(file_fd)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    capture_parser = subparsers.add_parser("capture", help="capture one read-only physical receipt")
    capture_parser.add_argument("--host-identity-json", required=True)
    capture_parser.add_argument("--disk-directory", required=True)
    capture_parser.add_argument("--lima-disk-json", required=True)
    capture_parser.add_argument("--resident-instance-json", required=True)
    capture_parser.add_argument("--resident-sandbox-instance", required=True)
    capture_parser.add_argument("--guest-project-stat", required=True)
    capture_parser.add_argument("--guest-mountinfo", required=True)
    capture_parser.add_argument("--guest-project-mountpoint", required=True)
    capture_parser.add_argument("--project-identity", required=True)
    capture_parser.add_argument("--project-disk-id", required=True)
    capture_parser.add_argument("--project-disk-generation", required=True, type=int)
    capture_parser.add_argument("--project-disk-revision", required=True, type=int)
    capture_parser.add_argument("--attachment-generation", required=True, type=int)
    capture_parser.add_argument("--resident-sandbox-id", required=True)
    capture_parser.add_argument("--resident-sandbox-generation", required=True, type=int)
    capture_parser.add_argument("--read-small-entry", action="append", default=[])
    capture_parser.add_argument("--observed-backing-entry")
    capture_parser.add_argument("--observed-lock-entry")
    capture_parser.add_argument("--output")

    validate_parser = subparsers.add_parser("validate", help="strictly parse an existing receipt")
    validate_parser.add_argument("receipt")
    validate_parser.add_argument("--output")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "capture":
            value = capture(args)
        else:
            value = validate_file(Path(args.receipt))
        _write_json(value, args.output)
        return 0
    except (OSError, ReceiptError, UnicodeError, TypeError) as exc:
        print(f"project-disk physical receipt refused: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
