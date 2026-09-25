#!/usr/bin/env python3
"""Durable glaeda releases and their update channels (#525), consumed by scripts/glaeda-update (#149).

Every push to main builds one release in CI (.github/workflows/release.yml):

    glaeda-hygiene-<target>.tar.gz   the source tree at that commit under glaeda/, plus a prebuilt
                                     bin/glaeda-worktree-reclaim; glaeda-update installs it with the
                                     tree's own scripts/glaeda-mini-setup --reclaim-binary
    glaeda-<source>-<target>.tar.gz  the fleet candidate bundle (scripts/fleet_bundle.py), carried
                                     for the operator's glaeda-mini-fleet upgrade; not auto-applied
    release.json                     source commit and the SHA-256 and size of every asset

The release is tagged r-<UTC date>-<source[:12]> and each asset carries a build-provenance
attestation from release.yml on main. One more release, tag `ota-channels`, holds the only mutable
files, each with exactly one writer so concurrent workflow runs never overwrite each other:

    canary.json   {"schema": "glaeda-ota-channel/v1", "ring": "canary", "tag", "source",
                   "releaseSha256", "published"}                      written by release.yml
    stable.json   the same, ring "stable", plus "promoted"            written by promote.yml
    control.json  {"schema": "glaeda-ota-control/v1", "paused": false} written by control.yml

Hosts read them anonymously (the repository is public) and trust nothing by name: release.json
must match `releaseSha256`, and every asset must match release.json. Canary only moves forward
(release.yml checks ancestry). Stable moves to the canary once it has soaked (`SOAK_HOURS`) and
canary hosts have reported success and none failure, as commit statuses `glaeda-ota/<host>`.
"""
from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
from pathlib import Path

REPOSITORY = "teamleaderleo/glaeda"
CHANNELS_TAG = "ota-channels"
CHANNEL_SCHEMA = "glaeda-ota-channel/v1"
CONTROL_SCHEMA = "glaeda-ota-control/v1"
RELEASE_SCHEMA = "glaeda-release/v1"
TARGETS = ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
STATUS_PREFIX = "glaeda-ota/"
SOAK_HOURS = 6
MAX_ASSET = 200 * 1024 * 1024
TAG_RE = re.compile(r"r-\d{8}-[0-9a-f]{12}")
SHA_RE = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")


class ReleaseError(RuntimeError):
    pass


def canonical(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False) + "\n").encode()


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def hygiene_asset(target: str) -> str:
    return f"glaeda-hygiene-{target}.tar.gz"


def release_tag(source: str, when: dt.datetime) -> str:
    if not SHA_RE.fullmatch(source):
        raise ReleaseError("source must be a full commit id")
    return f"r-{when.strftime('%Y%m%d')}-{source[:12]}"


def _add(tar: tarfile.TarFile, name: str, data: bytes, mode: int) -> None:
    info = tarfile.TarInfo(name)
    info.size, info.mode, info.mtime = len(data), mode, 0
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    tar.addfile(info, io.BytesIO(data))


def hygiene_archive(root: Path, source: str, reclaim: Path) -> bytes:
    """The committed tree at `source` under glaeda/, plus the reclaim binary, as a stable tar.gz."""
    tree = subprocess.run(["git", "-C", str(root), "archive", "--format=tar", "--prefix=glaeda/", source],
                          check=True, capture_output=True).stdout
    out = io.BytesIO()
    with tarfile.open(fileobj=io.BytesIO(tree)) as src, \
            gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as gz, \
            tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for member in src.getmembers():
            if member.isfile():
                _add(tar, member.name, src.extractfile(member).read(), 0o755 if member.mode & 0o111 else 0o644)
            elif member.isdir():
                info = tarfile.TarInfo(member.name)
                info.type, info.mode, info.mtime = tarfile.DIRTYPE, 0o755, 0
                tar.addfile(info)
            # symlinks and anything else are left out; extraction refuses them anyway
        _add(tar, "bin/glaeda-worktree-reclaim", reclaim.read_bytes(), 0o755)
    return out.getvalue()


def release_manifest(source: str, tag: str, assets: dict[str, bytes]) -> dict:
    return {
        "schema": RELEASE_SCHEMA, "repository": REPOSITORY, "tag": tag, "source": source,
        "assets": {name: {"sha256": sha256(raw), "size": len(raw)} for name, raw in sorted(assets.items())},
    }


def parse_release(raw: bytes, expected_sha256: str) -> dict:
    """release.json, checked against the digest the channel names."""
    if sha256(raw) != expected_sha256:
        raise ReleaseError("release.json does not match the channel's digest")
    doc = json.loads(raw)
    if (doc.get("schema") != RELEASE_SCHEMA or doc.get("repository") != REPOSITORY
            or not TAG_RE.fullmatch(str(doc.get("tag"))) or not SHA_RE.fullmatch(str(doc.get("source")))):
        raise ReleaseError("release.json is malformed")
    for name, meta in doc.get("assets", {}).items():
        if ("/" in name or not HEX64.fullmatch(str(meta.get("sha256")))
                or not isinstance(meta.get("size"), int) or not 0 < meta["size"] <= MAX_ASSET):
            raise ReleaseError(f"release.json lists a malformed asset: {name!r}")
    return doc


def channel_entry(ring: str, doc: dict, release_raw: bytes, published: str) -> dict:
    return {"schema": CHANNEL_SCHEMA, "ring": ring, "tag": doc["tag"], "source": doc["source"],
            "releaseSha256": sha256(release_raw), "published": published}


def parse_channel(raw: bytes, ring: str) -> dict:
    doc = json.loads(raw)
    if (doc.get("schema") != CHANNEL_SCHEMA or doc.get("ring") != ring
            or not TAG_RE.fullmatch(str(doc.get("tag"))) or not SHA_RE.fullmatch(str(doc.get("source")))
            or not HEX64.fullmatch(str(doc.get("releaseSha256")))):
        raise ReleaseError(f"{ring}.json is malformed")
    return doc


def control(paused: bool) -> dict:
    return {"schema": CONTROL_SCHEMA, "paused": paused}


def parse_control(raw: bytes) -> dict:
    doc = json.loads(raw)
    if doc.get("schema") != CONTROL_SCHEMA or not isinstance(doc.get("paused"), bool):
        raise ReleaseError("control.json is malformed")
    return doc


def canary_verdict(statuses: list[dict]) -> tuple[int, int]:
    """(successes, failures) among the newest status of each glaeda-ota/<host> context."""
    newest: dict[str, dict] = {}
    for status in statuses:
        context = str(status.get("context", ""))
        if not context.startswith(STATUS_PREFIX):
            continue
        seen = newest.get(context)
        if seen is None or str(status.get("updated_at", "")) > str(seen.get("updated_at", "")):
            newest[context] = status
    states = [s.get("state") for s in newest.values()]
    return states.count("success"), sum(state in ("failure", "error") for state in states)


def promotion(canary: dict | None, stable: dict | None, statuses: list[dict], now: dt.datetime,
              soak_hours: float = SOAK_HOURS) -> tuple[bool, str]:
    """Whether `canary` may become `stable` now, and why. Pausing stops hosts, not this."""
    if canary is None:
        return False, "no canary release"
    if stable is not None and stable["tag"] == canary["tag"]:
        return False, "stable is already the canary"
    published = dt.datetime.fromisoformat(canary["published"].replace("Z", "+00:00"))
    age = (now - published).total_seconds() / 3600
    if age < soak_hours:
        return False, f"canary soaking: {age:.1f} h of {soak_hours} h"
    ok, failed = canary_verdict(statuses)
    if failed:
        return False, f"{failed} canary host(s) reported failure"
    if not ok:
        return False, "no canary host reported success"
    return True, f"{ok} canary host(s) healthy after {age:.1f} h"


def now_utc() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0)


def iso(when: dt.datetime) -> str:
    return when.isoformat().replace("+00:00", "Z")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="command", required=True)
    h = sub.add_parser("hygiene", help="write glaeda-hygiene-<target>.tar.gz")
    h.add_argument("--source", required=True)
    h.add_argument("--target", required=True, choices=TARGETS)
    h.add_argument("--reclaim", type=Path, required=True)
    h.add_argument("--output", type=Path, required=True)
    t = sub.add_parser("tag", help="print the release tag for a source commit")
    t.add_argument("--source", required=True)
    m = sub.add_parser("manifest", help="write release.json over the assets in a directory")
    m.add_argument("--source", required=True)
    m.add_argument("--tag", required=True)
    m.add_argument("--assets", type=Path, required=True)
    c = sub.add_parser("canary", help="print canary.json for a release")
    c.add_argument("--release", type=Path, required=True)
    p = sub.add_parser("promote", help="print stable.json moved to the canary, if allowed (exit 3 if not)")
    p.add_argument("--canary", type=Path, required=True)
    p.add_argument("--stable", type=Path, help="current stable.json; absent means none yet")
    p.add_argument("--statuses", type=Path, required=True, help="the canary commit's statuses (API JSON)")
    p.add_argument("--soak-hours", type=float, default=SOAK_HOURS)
    k = sub.add_parser("control", help="print control.json")
    k.add_argument("--paused", choices=("true", "false"), required=True)
    a = ap.parse_args(argv)
    try:
        if a.command == "hygiene":
            raw = hygiene_archive(Path(__file__).resolve().parents[1], a.source, a.reclaim)
            a.output.mkdir(parents=True, exist_ok=True)
            (a.output / hygiene_asset(a.target)).write_bytes(raw)
            print(json.dumps({"asset": hygiene_asset(a.target), "sha256": sha256(raw), "size": len(raw)}))
        elif a.command == "tag":
            print(release_tag(a.source, now_utc()))
        elif a.command == "manifest":
            assets = {p.name: p.read_bytes() for p in sorted(a.assets.iterdir())
                      if p.is_file() and p.name != "release.json"}
            if not assets:
                raise ReleaseError("no assets")
            (a.assets / "release.json").write_bytes(canonical(release_manifest(a.source, a.tag, assets)))
        elif a.command == "canary":
            release_raw = a.release.read_bytes()
            doc = parse_release(release_raw, sha256(release_raw))
            sys.stdout.buffer.write(canonical(channel_entry("canary", doc, release_raw, iso(now_utc()))))
        elif a.command == "promote":
            canary = parse_channel(a.canary.read_bytes(), "canary")
            stable = parse_channel(a.stable.read_bytes(), "stable") if a.stable else None
            statuses = json.loads(a.statuses.read_bytes())
            if not isinstance(statuses, list):
                raise ReleaseError("statuses must be the API's JSON list")
            ok, why = promotion(canary, stable, statuses, now_utc(), a.soak_hours)
            print(why, file=sys.stderr)
            if not ok:
                return 3
            sys.stdout.buffer.write(canonical({**canary, "ring": "stable", "promoted": iso(now_utc())}))
        elif a.command == "control":
            sys.stdout.buffer.write(canonical(control(a.paused == "true")))
        return 0
    except (ReleaseError, OSError, ValueError, subprocess.CalledProcessError) as e:
        print(f"glaeda_release: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
