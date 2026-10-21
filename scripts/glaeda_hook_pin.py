#!/usr/bin/env python3
"""Pin the runner hook's sha256 in glaeda-disk's drain allowlist.

    scripts/glaeda_hook_pin.py           add the sha of scripts/glaeda-cmux-runner-hook to CMUX_NEXT_DRAIN_HOOK_SHA256
    scripts/glaeda_hook_pin.py --check   exit 1 (and name the sha) when it is missing; change nothing

glaeda-disk's cmux-next drain trusts only runner hooks whose bytes are a reviewed release
(CMUX_NEXT_DRAIN_HOOK_SHA256), so every change to the hook must append its new sha. A person had to
remember, and #1550 and #1561 did not: main's drain tests went red with "runner hook enrollment is not
proven". The tests now call this: test-glaeda-cmux-runner.py and test-glaeda-disk.py pin the hook when they
run on a checkout (so changing the hook and running its tests leaves the pin in the diff), and only check
under CI, where a missing pin fails with the command to run.

It only ever appends the current hook's sha: earlier pins stay (runners that have not refreshed their hook
still drain), and nothing else is allowed.
"""
from __future__ import annotations

import hashlib
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HOOK = ROOT / "scripts" / "glaeda-cmux-runner-hook"
DISK = ROOT / "scripts" / "glaeda-disk"
BLOCK = re.compile(r"^CMUX_NEXT_DRAIN_HOOK_SHA256 = frozenset\(\{\n(?P<body>(?:    \"[0-9a-f]{64}\",\n)*)\}\)\n", re.M)
COMMAND = "python3 scripts/glaeda_hook_pin.py"


def hook_sha(hook: Path | None = None) -> str:
    return hashlib.sha256((hook or HOOK).read_bytes()).hexdigest()


def pinned(text: str) -> list[str]:
    match = BLOCK.search(text)
    if not match:
        raise ValueError("scripts/glaeda-disk has no CMUX_NEXT_DRAIN_HOOK_SHA256 block in the expected form")
    return re.findall(r"[0-9a-f]{64}", match["body"])


def pin(text: str, sha: str) -> str:
    """text with sha appended to the allowlist; unchanged when it is already there."""
    if sha in pinned(text):
        return text
    match = BLOCK.search(text)
    return text[:match.end("body")] + f'    "{sha}",\n' + text[match.end("body"):]


def ensure(check: bool, hook: Path | None = None, disk: Path | None = None) -> str | None:
    """None when the hook's sha is pinned (after pinning it, unless check); else what is wrong."""
    hook, disk = hook or HOOK, disk or DISK
    sha, text = hook_sha(hook), disk.read_text()
    if sha in pinned(text):
        return None
    if check:
        return f"scripts/glaeda-cmux-runner-hook ({sha}) is not in CMUX_NEXT_DRAIN_HOOK_SHA256; run {COMMAND}"
    disk.write_text(pin(text, sha))
    return None


def under_ci() -> bool:
    return bool(os.environ.get("CI") or os.environ.get("GITHUB_ACTIONS"))


def main(argv: list[str]) -> int:
    if argv not in ([], ["--check"]):
        print(__doc__, file=sys.stderr)
        return 2
    problem = ensure(check=bool(argv))
    if problem:
        print(problem, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
