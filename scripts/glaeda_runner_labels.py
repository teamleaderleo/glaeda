#!/usr/bin/env python3
"""No GitHub Actions job here selects a GitHub-hosted runner unless HOSTED_ALLOWED names it, with the reason.

Rulebook: Linux jobs run on Blacksmith ("just put it on blacksmith / 16vcpu blacksmiths do exist", Leo, 2026-10-27),
and no runs-on names a GitHub-hosted macOS label. Blacksmith installs on GitHub organizations only, so it serves
manaflow-ai/glaeda (the fleet fork) but not teamleaderleo/glaeda, a personal-account repository, and no owned runner
serves this repository either. Every job that runs only here therefore stays on a hosted image until the repository
moves under an organization; each is listed below with that reason, and any other job that names a hosted label
fails CI. An entry whose job no longer names a hosted label also fails, so the list only shrinks.

The scan reads workflow text line by line, so it sees labels in `runs-on`, matrix rows (`runner: ubuntu-24.04`),
expression defaults (`vars.X || 'ubuntu-24.04'`) and arrays. Comments and `run:`, `name:` and `description:` text
are prose or shell, not runner selection, and are skipped.

Usage: glaeda_runner_labels.py [workflow files or directories]   (default: .github/workflows)
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

HOSTED = re.compile(
    r"(?<![A-Za-z0-9_-])("
    r"ubuntu-(?:latest|slim|\d{2}\.\d{2}(?:-arm)?)"
    r"|macos-(?:latest|\d+(?:-intel|-large|-xlarge|-arm64)?)"
    r"|windows-(?:latest|\d{4}(?:-arm)?|11-arm)"
    r")(?![A-Za-z0-9_.-])"
)
NON_RUNNER_KEY = re.compile(r"^\s*(?:-\s+)?(?:name|description|run)\s*:")
BLOCK_SCALAR = re.compile(r":\s*[|>][-+0-9]*\s*(?:#.*)?$")
JOB = re.compile(r"^  ([A-Za-z_][A-Za-z0-9_-]*)\s*:\s*(?:#.*)?$")

PERSONAL = ("runs only in teamleaderleo/glaeda, a personal-account repository: Blacksmith installs on organizations "
            "only and no owned runner serves it")
PERSONAL_MACOS = PERSONAL + "; builds and checks the aarch64-apple-darwin target"
PERSONAL_ARM = PERSONAL + "; probes the arm64 Ubuntu package and container closure"

# (workflow file, job id) -> why the job stays on a GitHub-hosted image.
HOSTED_ALLOWED: dict[tuple[str, str], str] = {
    ("big-red-jit-canary.yml", "hosted-baseline"): PERSONAL + "; it is the hosted baseline the canary compares",
    ("bootstrap-compatibility.yml", "verify"): PERSONAL,
    ("ci.yml", "changes"): PERSONAL,
    ("ci.yml", "go"): PERSONAL,
    ("ci.yml", "scripts"): PERSONAL,
    ("ci.yml", "runner-tests"): PERSONAL,
    ("ci.yml", "lint"): PERSONAL,
    ("ci.yml", "lint-macos"): PERSONAL_MACOS,
    ("ci.yml", "test"): PERSONAL,
    ("ci.yml", "cli"): PERSONAL,
    ("ci.yml", "fleet-cas-build"): PERSONAL_MACOS,
    ("ci.yml", "ci-status"): PERSONAL,
    ("control.yml", "control"): PERSONAL,
    ("cultist-advisory.yml", "diff"): PERSONAL,
    ("developer-loop-benchmark.yml", "fresh-hosted"): PERSONAL + "; it measures the hosted developer loop",
    ("developer-loop-benchmark.yml", "prepared-hosted"): PERSONAL + "; it measures the hosted developer loop",
    ("fleet-candidate.yml", "candidate"): PERSONAL_MACOS,
    ("github-resident-snapshot.yml", "contract"): PERSONAL,
    ("linux-acceptance.yml", "disposable-linux"): PERSONAL,
    ("podman-closure-probe.yml", "ubuntu-package"): PERSONAL_ARM,
    ("podman-container-closure-probe.yml", "ubuntu-container"): PERSONAL_ARM,
    ("promote.yml", "promote"): PERSONAL,
    ("release.yml", "build"): PERSONAL_MACOS,
    ("release.yml", "publish"): PERSONAL,
    ("sync-main.yml", "sync"): PERSONAL,
}


def _strip_comment(line: str) -> str:
    match = re.search(r"(^|\s)#", line)
    return line[: match.start()] if match else line


def hosted_jobs(path: Path) -> dict[str, list[str]]:
    """job id -> the lines in that job that name a GitHub-hosted label (a line outside any job is under '')."""
    found: dict[str, list[str]] = {}
    job = ""
    in_jobs = False
    block_indent = -1
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        indent = len(line) - len(line.lstrip())
        if block_indent >= 0:
            if line.strip() and indent <= block_indent:
                block_indent = -1
            else:
                continue
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if indent == 0:
            in_jobs = line.startswith("jobs:")
            job = ""
        elif in_jobs and (match := JOB.match(line)):
            job = match.group(1)
            continue
        if NON_RUNNER_KEY.match(line):
            if BLOCK_SCALAR.search(line):
                block_indent = indent
            continue
        if HOSTED.search(_strip_comment(line)):
            found.setdefault(job, []).append(f"{path.name}:{number}: {line.strip()}")
    return found


def violations(paths: list[Path], allowed: dict[tuple[str, str], str] | None = None) -> list[str]:
    allowed = HOSTED_ALLOWED if allowed is None else allowed
    problems: list[str] = []
    seen: set[tuple[str, str]] = set()
    for path in paths:
        for job, lines in hosted_jobs(path).items():
            key = (path.name, job)
            if key in allowed and allowed[key].strip():
                seen.add(key)
                continue
            problems += [f"{line}  (job {job or '<outside jobs>'} is not in HOSTED_ALLOWED)" for line in lines]
    names = {path.name for path in paths}
    for key in sorted(set(allowed) - seen):
        if key[0] in names:
            problems.append(f"{key[0]}: job {key[1]} is in HOSTED_ALLOWED but names no GitHub-hosted label; "
                            "remove the entry")
    return problems


def workflow_files(args: list[str]) -> list[Path]:
    targets = [Path(a) for a in args] or [Path(__file__).resolve().parent.parent / ".github" / "workflows"]
    files: list[Path] = []
    for target in targets:
        if target.is_dir():
            files += sorted(p for p in target.iterdir() if p.suffix in (".yml", ".yaml"))
        else:
            files.append(target)
    return files


def main(argv: list[str]) -> int:
    problems = violations(workflow_files(argv))
    if problems:
        print("GitHub-hosted runner labels outside HOSTED_ALLOWED (use a blacksmith-<n>vcpu-ubuntu-2404 label):")
        print("\n".join(problems))
        return 1
    print(f"PASS: only the {len(HOSTED_ALLOWED)} allowlisted jobs name a GitHub-hosted runner")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
