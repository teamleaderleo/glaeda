#!/usr/bin/env python3
"""Observation-only physical Lima project-disk receipt collector and parser.

The collector deliberately has no Lima disk-layout knowledge. The operator supplies the exact
standalone-disk directory plus already-captured Lima/guest evidence. Direct directory entries are
recorded as opaque observations. Backing/lock roles appear only when the operator explicitly labels
exact observed entry names.
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
MAX_DIRECTORY_ENTRIES = 64
MAX_ENTRY_NAME_BYTES = 255
MAX_EXPLICIT_SMALL_FILE_BYTES = 4_096
ALLOCATED_BLOCK_BYTES = 512


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
    if not os.path.isabs(raw) or os.path.normpath(raw) != raw:
        raise ReceiptError(f"{label} must be one exact normalized absolute path")
    return path


def _directory_snapshot_from_fd(directory_fd: int) -> dict[str, Any]:
    observed = os.fstat(directory_fd)
    if not stat.S_ISDIR(observed.st_mode):
        raise ReceiptError("project disk directory descriptor is not a directory")
    return _snapshot(observed)


def _entry_snapshot(directory_fd: int, name: str) -> tuple[dict[str, Any], str]:
    observed = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    return _snapshot(observed), _entry_kind(observed.st_mode)


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
        data = os.read(file_fd, MAX_EXPLICIT_SMALL_FILE_BYTES + 1)
        if len(data) > MAX_EXPLICIT_SMALL_FILE_BYTES:
            raise ReceiptError(f"explicit small-file read exceeds bound: {name}")
        after = _snapshot(os.fstat(file_fd))
        if not _same_observation(before, after):
            raise ReceiptError(f"explicit small-file entry changed during read: {name}")
    finally:
        os.close(file_fd)
    view = _bytes_view(data)
    view["sha256"] = _sha256(data)
    return view


def _capture_directory(
    disk_directory: Path,
    explicit_small_reads: set[str],
) -> dict[str, Any]:
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
            if len(name_bytes) == 0 or len(name_bytes) > MAX_ENTRY_NAME_BYTES:
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
    return {
        "path": os.fspath(disk_directory),
        "metadata": before_directory,
        "entries": entries,
    }


def _parse_host_identity(path: Path) -> dict[str, Any]:
    data = _read_bounded(path, MAX_JSON_BYTES, "Lima host identity receipt")
    value = _decode_json_bytes(data, "Lima host identity receipt")
    if not isinstance(value, dict):
        raise ReceiptError("Lima host identity receipt must be one JSON object")
    expected_keys = {
        "schema_version",
        "receipt_type",
        "instance",
        "lima_host_identity",
        "lima_request_identity",
    }
    if set(value) != expected_keys:
        raise ReceiptError("Lima host identity receipt has an unexpected field set")
    if value["schema_version"] != SCHEMA_VERSION or value["receipt_type"] != HOST_IDENTITY_RECEIPT_TYPE:
        raise ReceiptError("Lima host identity receipt schema/type is unsupported")
    if not isinstance(value["instance"], str) or not value["instance"]:
        raise ReceiptError("Lima host identity receipt instance is invalid")
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
        matches.append(
            {
                "mount_id": mount_id,
                "parent_id": parent_id,
                "device_major": int(device_fields[0]),
                "device_minor": int(device_fields[1]),
                "root": _bytes_view(_decode_mountinfo_field(left_fields[3])),
                "mountpoint": _bytes_view(mountpoint),
                "mount_options": left_fields[5].decode("ascii", "strict"),
                "filesystem_type": right_fields[0].decode("ascii", "strict"),
                "mount_source": _bytes_view(_decode_mountinfo_field(right_fields[1])),
                "super_options": b" ".join(right_fields[2:]).decode("ascii", "strict"),
                "raw_line_sha256": _sha256(raw_line),
            }
        )
    if len(matches) != 1:
        raise ReceiptError("guest mountinfo must contain exactly one exact project mountpoint")
    return matches[0]


def _entry_lookup(directory: dict[str, Any]) -> dict[str, dict[str, Any]]:
    result: dict[str, dict[str, Any]] = {}
    for entry in directory["entries"]:
        utf8 = entry["name"]["utf8"]
        if utf8 is not None:
            result[utf8] = entry
    return result


def _explicit_role_labels(
    directory: dict[str, Any],
    backing: str | None,
    lock: str | None,
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
    host_identity = _parse_host_identity(Path(args.host_identity_json))
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
        "lima_host_identity": host_identity,
        "disk_directory": directory,
        "correlation": {
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


def validate_receipt(receipt: Any) -> dict[str, Any]:
    root = _require_exact_keys(
        receipt,
        {
            "schema_version",
            "receipt_type",
            "authority",
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

    host = root["lima_host_identity"]
    if not isinstance(host, dict):
        raise ReceiptError("Lima host identity receipt is invalid")
    _validate_sha256(host.get("lima_host_identity"), "lima_host_identity")
    _validate_sha256(host.get("lima_request_identity"), "lima_request_identity")
    _validate_sha256(host.get("source_sha256"), "host_identity.source_sha256")

    directory = _require_exact_keys(
        root["disk_directory"], {"path", "metadata", "entries"}, "disk_directory"
    )
    _validate_exact_absolute_path(Path(directory["path"]), "disk directory")
    entries = directory["entries"]
    if not isinstance(entries, list) or len(entries) > MAX_DIRECTORY_ENTRIES:
        raise ReceiptError("disk directory entry list exceeds the reviewed bound")
    names: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict) or set(entry).difference(
            {"name", "kind", "metadata", "symlink_target", "explicit_small_file_bytes"}
        ):
            raise ReceiptError("disk directory entry has an unexpected field")
        name = entry.get("name")
        if not isinstance(name, dict) or set(name) != {"hex", "utf8"}:
            raise ReceiptError("disk directory entry name encoding is invalid")
        name_hex = name["hex"]
        if not isinstance(name_hex, str) or len(name_hex) % 2 != 0:
            raise ReceiptError("disk directory entry name hex is invalid")
        try:
            name_bytes = bytes.fromhex(name_hex)
        except ValueError as exc:
            raise ReceiptError("disk directory entry name hex is invalid") from exc
        if len(name_bytes) == 0 or len(name_bytes) > MAX_ENTRY_NAME_BYTES:
            raise ReceiptError("disk directory entry name length is invalid")
        if name_hex in names:
            raise ReceiptError("disk directory entry names must be unique")
        names.add(name_hex)
        if name["utf8"] is not None:
            if not isinstance(name["utf8"], str) or name["utf8"].encode("utf-8") != name_bytes:
                raise ReceiptError("disk directory entry UTF-8 name disagrees with exact bytes")

    correlation = _require_exact_keys(
        root["correlation"],
        {"lima_disk_json", "resident_instance_json", "guest_project_filesystem"},
        "correlation",
    )
    for field in ("lima_disk_json", "resident_instance_json"):
        evidence = _require_exact_keys(correlation[field], {"sha256", "value"}, field)
        _validate_sha256(evidence["sha256"], f"{field}.sha256")
    guest = _require_exact_keys(
        correlation["guest_project_filesystem"], {"mountpoint", "stat", "mountinfo"}, "guest"
    )
    _validate_exact_absolute_path(Path(guest["mountpoint"]), "guest project mountpoint")
    if not isinstance(guest["stat"], dict) or set(guest["stat"]) != {"device", "inode"}:
        raise ReceiptError("guest stat field set is invalid")
    if not isinstance(guest["stat"]["device"], int) or not isinstance(guest["stat"]["inode"], int):
        raise ReceiptError("guest stat device/inode types are invalid")
    mountinfo = guest["mountinfo"]
    if not isinstance(mountinfo, dict):
        raise ReceiptError("guest mountinfo evidence is invalid")
    if mountinfo.get("device_major", -1) < 0 or mountinfo.get("device_minor", -1) < 0:
        raise ReceiptError("guest mountinfo device is invalid")

    labels = root["operator_role_labels"]
    if labels is not None:
        labels = _require_exact_keys(
            labels,
            {"source", "observed_backing_entry", "observed_lock_entry"},
            "operator_role_labels",
        )
        if labels["source"] != "explicit_operator_observation":
            raise ReceiptError("operator role labels must remain explicitly observed")
        utf8_names = {
            entry["name"]["utf8"] for entry in entries if entry["name"]["utf8"] is not None
        }
        if labels["observed_backing_entry"] not in utf8_names or labels["observed_lock_entry"] not in utf8_names:
            raise ReceiptError("operator role labels must name exact captured entries")
        if labels["observed_backing_entry"] == labels["observed_lock_entry"]:
            raise ReceiptError("operator role labels must name distinct entries")

    return root


def validate_file(path: Path) -> dict[str, Any]:
    data = _read_bounded(path, MAX_MOUNTINFO_BYTES, "project disk physical receipt")
    receipt = _decode_json_bytes(data, "project disk physical receipt")
    return validate_receipt(receipt)


def _write_json(value: Any, output: str | None) -> None:
    encoded = json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False) + "\n"
    if output is None:
        sys.stdout.write(encoded)
        return
    Path(output).write_text(encoded, encoding="utf-8")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    capture_parser = subparsers.add_parser("capture", help="capture one read-only physical receipt")
    capture_parser.add_argument("--host-identity-json", required=True)
    capture_parser.add_argument("--disk-directory", required=True)
    capture_parser.add_argument("--lima-disk-json", required=True)
    capture_parser.add_argument("--resident-instance-json", required=True)
    capture_parser.add_argument("--guest-project-stat", required=True)
    capture_parser.add_argument("--guest-mountinfo", required=True)
    capture_parser.add_argument("--guest-project-mountpoint", required=True)
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
    except (OSError, ReceiptError, UnicodeError) as exc:
        print(f"project-disk physical receipt refused: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
