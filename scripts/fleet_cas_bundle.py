#!/usr/bin/env python3
"""Build, verify, and stage an explicit fleet-CAS prebuilt candidate."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tarfile
import tempfile

SCHEMA = "glaeda-fleet-cas-prebuilt/v1"
TARGETS = {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"}
FILES = (
    "fleet-cas", "fleet-cas-run", "fleet-cas-mount", "fleet-cas-settings.sh",
    "fleet-cas-marker.sh", "fleet-cas-warm.sh", "fleet-cas-prewarm.sh",
    "fleet-cas-writer-build.sh",
)
MAX_FILE = 100 * 1024 * 1024
MAX_TOTAL = 200 * 1024 * 1024
MAX_ARCHIVE = MAX_TOTAL


class BundleError(RuntimeError):
    pass


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def digest(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def read_file(path: Path, limit: int = MAX_FILE) -> bytes:
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
        raise BundleError(f"bundle input must be a bounded regular file: {path}")
    raw = path.read_bytes()
    if len(raw) > limit:
        raise BundleError(f"bundle input exceeds size limit: {path}")
    return raw


def command(root: Path, argv: list[str], capture: bool = True) -> bytes:
    executable = shutil.which(argv[0])
    if executable is None:
        raise BundleError(f"required tool unavailable: {argv[0]}")
    env = {k: os.environ[k] for k in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "TMPDIR", "TMP", "TEMP") if k in os.environ}
    result = subprocess.run([executable, *argv[1:]], cwd=root, env=env, stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE if capture else None, check=False)
    if result.returncode:
        raise BundleError(f"{argv[0]} failed with exit code {result.returncode}")
    return result.stdout if capture else b""


def source_identity(root: Path, expected: str) -> dict[str, str]:
    if len(expected) != 40 or any(c not in "0123456789abcdef" for c in expected):
        raise BundleError("expected source must be a full commit SHA")
    if command(root, ["git", "status", "--porcelain", "--untracked-files=normal"]):
        raise BundleError("candidate source checkout must be clean")
    commit = command(root, ["git", "rev-parse", "HEAD^{commit}"]).decode().strip()
    if commit != expected:
        raise BundleError("candidate source commit does not match request")
    tree = command(root, ["git", "rev-parse", "HEAD^{tree}"]).decode().strip()
    return {"repository": "teamleaderleo/glaeda", "commit": commit, "tree": tree}


def archive_bytes(files: dict[str, bytes], manifest: dict) -> bytes:
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as gz, tarfile.open(fileobj=gz, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        entries = {**files, "manifest.json": canonical(manifest)}
        checksums = "".join(f"{digest(entries[name])}  {name}\n" for name in sorted(entries))
        entries["SHA256SUMS"] = checksums.encode()
        for name, data in sorted(entries.items()):
            info = tarfile.TarInfo(name)
            info.size, info.mode, info.mtime = len(data), (0o755 if name in FILES else 0o644), 0
            tar.addfile(info, io.BytesIO(data))
    return out.getvalue()


def verified_contents(raw: bytes, expected_digest: str, source: str, target: str) -> tuple[dict, dict[str, bytes]]:
    if len(raw) > MAX_ARCHIVE or digest(raw) != expected_digest:
        raise BundleError("fleet-CAS archive digest mismatch or size limit exceeded")
    contents: dict[str, bytes] = {}
    total = 0
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(raw), mode="rb") as gz:
            tar_bytes = gz.read(MAX_TOTAL + 1)
        if len(tar_bytes) > MAX_TOTAL:
            raise BundleError("fleet-CAS archive expands beyond size limit")
        with tarfile.open(fileobj=io.BytesIO(tar_bytes), mode="r:") as tar:
            for entry in tar:
                if entry.name in contents or entry.name not in set(FILES) | {"manifest.json", "SHA256SUMS"} or not entry.isfile():
                    raise BundleError("fleet-CAS archive contains an unsafe or unexpected entry")
                expected_mode = 0o755 if entry.name in FILES else 0o644
                if entry.mode != expected_mode:
                    raise BundleError("fleet-CAS archive entry has an unexpected mode")
                if entry.size < 0 or entry.size > MAX_FILE:
                    raise BundleError("fleet-CAS archive file exceeds size limit")
                total += entry.size
                if total > MAX_TOTAL:
                    raise BundleError("fleet-CAS archive expands beyond size limit")
                stream = tar.extractfile(entry)
                if stream is None:
                    raise BundleError("fleet-CAS archive entry is unreadable")
                contents[entry.name] = stream.read(MAX_FILE + 1)
        if set(contents) != set(FILES) | {"manifest.json", "SHA256SUMS"}:
            raise BundleError("fleet-CAS archive is incomplete")
        manifest = json.loads(contents["manifest.json"])
        source_doc = manifest.get("source", {}) if isinstance(manifest, dict) else {}
        if (not isinstance(manifest, dict) or manifest.get("schema") != SCHEMA
                or target not in TARGETS or manifest.get("target") != target or not isinstance(source_doc, dict)
                or source_doc.get("commit") != source
                or source_doc.get("repository") != "teamleaderleo/glaeda"
                or not isinstance(source_doc.get("tree"), str)
                or len(source_doc["tree"]) != 40
                or any(c not in "0123456789abcdef" for c in source_doc["tree"])
                or not isinstance(manifest.get("toolchain"), str)
                or set(manifest.get("files", {})) != set(FILES)):
            raise BundleError("fleet-CAS manifest identity is invalid")
        expected = {name: {"sha256": digest(contents[name]), "size": len(contents[name])} for name in FILES}
        if manifest["files"] != expected:
            raise BundleError("fleet-CAS manifest file inventory mismatch")
        lines = contents["SHA256SUMS"].decode().splitlines()
        if lines != [f"{digest(contents[name])}  {name}" for name in sorted(contents) if name != "SHA256SUMS"]:
            raise BundleError("fleet-CAS checksum inventory mismatch")
        return manifest, {name: contents[name] for name in FILES}
    except (OSError, UnicodeError, json.JSONDecodeError, tarfile.TarError, EOFError, KeyError, TypeError) as error:
        raise BundleError("fleet-CAS archive or manifest is malformed") from error


def build(root: Path, expected_source: str, target: str, output: Path) -> dict:
    source = source_identity(root, expected_source)
    if target not in TARGETS:
        raise BundleError("unsupported fleet-CAS target")
    toolchain = command(root, ["rustc", "-Vv"]).decode().strip()
    if f"host: {target}" not in toolchain.splitlines():
        raise BundleError("fleet-CAS build requires a native supported target")
    build_root = root / "target/fleet-cas-bundle-build"
    command(root, ["cargo", "build", "--locked", "--release", "--manifest-path", "tools/fleet-cas-prototype/Cargo.toml", "--target", target, "--target-dir", str(build_root)], capture=False)
    files = {"fleet-cas": read_file(build_root / target / "release/fleet-cas")}
    for name in FILES[1:]:
        files[name] = read_file(root / "tools/fleet-cas-prototype/scripts" / name)
    manifest = {"schema": SCHEMA, "source": source, "target": target, "toolchain": toolchain,
                "files": {name: {"sha256": digest(data), "size": len(data)} for name, data in files.items()}}
    raw = archive_bytes(files, manifest)
    verified_contents(raw, digest(raw), expected_source, target)
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"glaeda-fleet-cas-{expected_source}-{target}.tar.gz"
    with tempfile.NamedTemporaryFile(dir=output, prefix=".fleet-cas.") as temporary:
        temporary.write(raw); temporary.flush(); os.fsync(temporary.fileno())
        os.link(temporary.name, destination)
    return {"archive": destination.name, "sha256": digest(raw), "source": source, "target": target}


def stage(archive: Path, destination: Path, source: str, target: str, apply: bool) -> dict:
    raw = read_file(archive, MAX_ARCHIVE)
    manifest, files = verified_contents(raw, digest(raw), source, target)
    if os.path.lexists(destination) or destination.name in ("", ".", ".."):
        raise BundleError("staging destination already exists")
    receipt = {"schema": "glaeda-fleet-cas-stage/v1", "state": "planned" if not apply else "staged",
               "archiveSha256": digest(raw), "source": manifest["source"], "target": target,
               "automaticUpdateAuthorized": False}
    if not apply:
        return receipt
    destination.mkdir(mode=0o700)
    try:
        for name, data in files.items():
            path = destination / name
            with path.open("xb") as stream:
                stream.write(data); stream.flush(); os.fsync(stream.fileno())
            path.chmod(0o755)
        (destination / "manifest.json").write_bytes(canonical(manifest))
        (destination / "SHA256SUMS").write_bytes(("".join(f"{digest(data)}  {name}\n" for name, data in sorted({**files, "manifest.json": canonical(manifest)}.items()))).encode())
        return receipt
    except BaseException:
        shutil.rmtree(destination, ignore_errors=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    b = sub.add_parser("build"); b.add_argument("--source", required=True); b.add_argument("--target", required=True); b.add_argument("--output", type=Path, required=True); b.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    v = sub.add_parser("verify"); v.add_argument("archive", type=Path); v.add_argument("--source", required=True); v.add_argument("--target", required=True)
    s = sub.add_parser("stage"); s.add_argument("archive", type=Path); s.add_argument("--source", required=True); s.add_argument("--target", required=True); s.add_argument("--destination", type=Path, required=True); s.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "build": result = build(args.root, args.source, args.target, args.output)
        elif args.command == "verify": result = {"state": "verified", **verified_contents(read_file(args.archive, MAX_ARCHIVE), digest(read_file(args.archive, MAX_ARCHIVE)), args.source, args.target)[0]}
        else: result = stage(args.archive, args.destination, args.source, args.target, args.apply)
        print(json.dumps(result, sort_keys=True)); return 0
    except BundleError as error:
        print(f"fleet-cas bundle: {error}", file=os.sys.stderr); return 2


if __name__ == "__main__":
    raise SystemExit(main())
