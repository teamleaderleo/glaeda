#!/usr/bin/env python3
"""Build and verify exact-source fleet candidate bundles; never install or execute them."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import tomllib

SCHEMA = "glaeda-fleet-candidate/v1"
TARGETS = {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"}
FILES = (
    "LICENSE",
    "scripts/cmux-fleet",
    "scripts/cmux-fleet-bootstrap-macos",
    "scripts/cmux-fleet-bootstrap-linux",
    "scripts/cmux_fleet.py",
    "scripts/cmux_fleet_bootstrap.py",
    "scripts/cmux_fleet_operator_summary.py",
    "scripts/cmux_execution_roles.py",
    "scripts/cmux_workload_request.py",
    "docs/CMUX_FLEET_ENROLLMENT.md",
)
PAYLOAD = {*FILES, "bin/glaeda", "THIRD_PARTY_NOTICES.txt"}
MAX_ARCHIVE = 100 * 1024 * 1024
MAX_FILE = 100 * 1024 * 1024
MAX_TOTAL = 200 * 1024 * 1024


class BundleError(RuntimeError):
    pass


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def file_bytes(path: Path) -> bytes:
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_FILE:
        raise BundleError("bundle input must be a bounded regular file")
    with path.open("rb") as stream:
        raw = stream.read(MAX_FILE + 1)
    if len(raw) > MAX_FILE:
        raise BundleError("bundle input exceeds size limit")
    return raw


def child_environment() -> dict[str, str]:
    return {key: os.environ[key] for key in (
        "PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR", "TMP", "TEMP",
    ) if key in os.environ}


def command(root: Path, argv: list[str], *, capture: bool = True) -> bytes:
    executable = shutil.which(argv[0])
    if executable is None:
        raise BundleError(f"required tool unavailable: {argv[0]}")
    result = subprocess.run(
        [executable, *argv[1:]], cwd=root, env=child_environment(),
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE if capture else None,
        check=False,
    )
    if result.returncode:
        raise BundleError(f"{argv[0]} failed with exit code {result.returncode}")
    return result.stdout if capture else b""


def source_identity(root: Path, expected: str) -> dict[str, str]:
    if re.fullmatch(r"[0-9a-f]{40}", expected) is None:
        raise BundleError("expected source must be a full commit SHA")
    if command(root, ["git", "status", "--porcelain", "--untracked-files=normal"]):
        raise BundleError("candidate source checkout must be clean")
    commit = command(root, ["git", "rev-parse", "HEAD^{commit}"]).decode().strip()
    if commit != expected:
        raise BundleError("candidate source commit does not match request")
    tree = command(root, ["git", "rev-parse", "HEAD^{tree}"]).decode().strip()
    return {"repository": "teamleaderleo/glaeda", "commit": commit, "tree": tree}


def notices(root: Path, target: str) -> bytes:
    metadata = json.loads(command(root, [
        "cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target,
    ]))
    sections = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        directory = Path(package["manifest_path"]).parent
        paths = {path for pattern in ("LICENSE*", "COPYING*", "NOTICE*")
                 for path in directory.glob(pattern) if path.is_file()}
        if package.get("license_file"):
            path = directory / package["license_file"]
            if not path.resolve().is_relative_to(directory.resolve()):
                raise BundleError("dependency license path escapes its package")
            paths.add(path)
        if not paths:
            raise BundleError(f"dependency license text unavailable: {package['name']}")
        sections.append(f"\n{package['name']} {package['version']} ({package.get('license') or 'custom'})\n".encode())
        for path in sorted(paths):
            sections.extend([f"\n{path.name}\n".encode(), file_bytes(path), b"\n"])
    raw = b"".join(sections)
    if len(raw) > MAX_FILE:
        raise BundleError("dependency notices exceed size limit")
    return raw


def archive_bytes(payload: dict[str, bytes], manifest: dict) -> bytes:
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", mtime=0, filename="") as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for name, raw in sorted({**payload, "manifest.json": canonical(manifest)}.items()):
                entry = tarfile.TarInfo(name)
                entry.size = len(raw)
                entry.mode = 0o755 if name == "bin/glaeda" else 0o644
                archive.addfile(entry, io.BytesIO(raw))
    return output.getvalue()


def verify(raw: bytes, expected_sha256: str, source: str, target: str) -> dict:
    if len(raw) > MAX_ARCHIVE or sha256(raw) != expected_sha256:
        raise BundleError("candidate archive digest or size mismatch")
    contents = {}
    total = 0
    try:
        with tarfile.open(fileobj=io.BytesIO(raw), mode="r|gz") as archive:
            for entry in archive:
                if (entry.name not in PAYLOAD | {"manifest.json"}
                        or entry.name in contents or not entry.isfile()
                        or entry.mode != (0o755 if entry.name == "bin/glaeda" else 0o644)
                        or entry.size < 0 or entry.size > MAX_FILE):
                    raise BundleError("candidate archive contains an unsafe or unexpected entry")
                total += entry.size
                if total > MAX_TOTAL:
                    raise BundleError("candidate archive expands beyond size limit")
                stream = archive.extractfile(entry)
                if stream is None:
                    raise BundleError("candidate archive entry is unreadable")
                contents[entry.name] = stream.read(MAX_FILE + 1)
        if set(contents) != PAYLOAD | {"manifest.json"}:
            raise BundleError("candidate archive is incomplete")
        manifest = json.loads(contents.pop("manifest.json"))
        if not isinstance(manifest, dict) or set(manifest) != {
            "schema", "source", "target", "version", "toolchain", "channel",
            "automaticUpdateAuthorized", "files",
        }:
            raise BundleError("candidate manifest fields are invalid")
        if (manifest["schema"] != SCHEMA or manifest["channel"] != "candidate"
                or manifest["automaticUpdateAuthorized"] is not False
                or target not in TARGETS or manifest["target"] != target
                or re.fullmatch(r"[0-9a-f]{40}", source) is None
                or not isinstance(manifest["source"], dict)
                or set(manifest["source"]) != {"repository", "commit", "tree"}
                or manifest["source"]["repository"] != "teamleaderleo/glaeda"
                or manifest["source"]["commit"] != source
                or not isinstance(manifest["source"]["tree"], str)
                or re.fullmatch(r"[0-9a-f]{40}", manifest["source"]["tree"]) is None):
            raise BundleError("candidate source, target or authority mismatch")
        for field in ("version", "toolchain"):
            if not isinstance(manifest[field], str) or not 1 <= len(manifest[field]) <= 512:
                raise BundleError("candidate build metadata is invalid")
        expected_files = {name: {"sha256": sha256(data), "size": len(data)}
                          for name, data in contents.items()}
        if manifest["files"] != expected_files:
            raise BundleError("candidate file inventory mismatch")
        return manifest
    except (tarfile.TarError, EOFError, ValueError, TypeError, KeyError) as error:
        raise BundleError("candidate archive or manifest is malformed") from error


def build(root: Path, expected_source: str, target: str, output: Path) -> dict:
    source = source_identity(root, expected_source)
    toolchain = command(root, ["rustc", "-Vv"]).decode().strip()
    if f"host: {target}" not in toolchain.splitlines() or target not in TARGETS:
        raise BundleError("candidate build requires a supported native target")
    build_root = root / "target/fleet-candidate-build"
    command(root, ["cargo", "build", "--locked", "--release", "--bin", "glaeda", "--target", target, "--target-dir", str(build_root)], capture=False)
    payload = {name: file_bytes(root / name) for name in FILES}
    payload["bin/glaeda"] = file_bytes(build_root / target / "release/glaeda")
    payload["THIRD_PARTY_NOTICES.txt"] = notices(root, target)
    manifest = {
        "schema": SCHEMA, "source": source, "target": target,
        "version": tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"],
        "toolchain": toolchain, "channel": "candidate", "automaticUpdateAuthorized": False,
        "files": {name: {"sha256": sha256(raw), "size": len(raw)} for name, raw in payload.items()},
    }
    if source_identity(root, expected_source) != source:
        raise BundleError("candidate source changed during build")
    raw = archive_bytes(payload, manifest)
    archive_sha256 = sha256(raw)
    verify(raw, archive_sha256, expected_source, target)
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"glaeda-{expected_source}-{target}.tar.gz"
    with tempfile.NamedTemporaryFile(dir=output, prefix=".candidate.") as stage:
        stage.write(raw)
        stage.flush()
        os.fsync(stage.fileno())
        os.link(stage.name, destination)  # Refuse overwrite, including a dangling symlink.
    return {"archive": destination.name, "sha256": archive_sha256, "source": source, "target": target}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("build")
    b.add_argument("--output", type=Path, required=True)
    v = sub.add_parser("verify")
    v.add_argument("archive", type=Path)
    v.add_argument("--sha256", required=True)
    for p in (b, v):
        p.add_argument("--source", required=True)
        p.add_argument("--target", required=True, choices=sorted(TARGETS))
    args = parser.parse_args()
    try:
        if args.command == "build":
            result = build(Path(__file__).resolve().parents[1], args.source, args.target, args.output)
        else:
            result = verify(file_bytes(args.archive), args.sha256, args.source, args.target)
        print(canonical(result).decode(), end="")
        return 0
    except (BundleError, OSError) as error:
        print(json.dumps({"error": str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
