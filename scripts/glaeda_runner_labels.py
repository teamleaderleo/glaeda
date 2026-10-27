#!/usr/bin/env python3
"""No GitHub Actions job here selects a GitHub-hosted runner unless HOSTED_ALLOWED names it, with the reason.

Rulebook: Linux jobs run on Blacksmith ("just put it on blacksmith / 16vcpu blacksmiths do exist", Leo, 2026-10-27),
and no runs-on names a GitHub-hosted macOS label. Blacksmith installs on GitHub organizations only, so it serves
manaflow-ai/glaeda (the fleet fork) but not teamleaderleo/glaeda, a personal-account repository, and no owned runner
serves this repository either. So:

- A job that can also run in the fork picks by owner, `${{ github.repository_owner == 'manaflow-ai' &&
  'blacksmith-<n>vcpu-ubuntu-2404' || 'ubuntu-24.04' }}` (macOS rows likewise, `blacksmith-6vcpu-macos-15`): Blacksmith
  there, the hosted image only here. That form, in runs-on or a matrix row, is the one hosted spelling accepted
  without an entry.
- A job gated to this repository (`github.repository == 'teamleaderleo/glaeda'`), or one that measures the hosted
  image itself, is listed in HOSTED_ALLOWED with its reason until the repository moves under an organization.

Any other job that names a hosted label fails CI. An entry whose job no longer names one fails too, so the list only
shrinks. A runs-on or matrix this scan cannot read (`vars`, `needs`, `inputs` or `steps`, in any case or bracket
form, or `fromJSON`), on its line or nested under it, fails as well unless its job is listed: a variable or picker output could hold a hosted label the text never shows.

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
# Blacksmith in the manaflow-ai fork, the hosted image only in this personal-account repository. The whole value
# must be exactly this expression: a condition added in front of it (`github.event_name == 'push' && ...`) would
# send the fork to the hosted image too.
OWNER_FORM = re.compile(
    r"^\s*(?:-\s+)?(?:runs-on|runner)\s*:\s*(?P<q>[\"']?)\$\{\{\s*"
    r"github\.repository_owner\s*==\s*'manaflow-ai'\s*&&\s*'blacksmith-[0-9]+vcpu-[a-z0-9.-]+'\s*\|\|\s*'[^']+'"
    r"\s*\}\}(?P=q)\s*$")
# A key whose value, and every line nested under it, selects a runner.
SELECTOR_KEY = re.compile(r"^\s*(?:-\s+)?(?:runs-on|matrix)\s*:")
UNREADABLE = re.compile(r"\b(?:vars|needs|inputs|steps)\s*[.\[]|fromJSON", re.IGNORECASE)

PERSONAL = ("runs only in teamleaderleo/glaeda, a personal-account repository: Blacksmith installs on organizations "
            "only and no owned runner serves it")
HOSTED_ON_PURPOSE = ("measures the developer loop on a GitHub-hosted runner, so the hosted image is the thing "
                     "measured; it is dispatched or runs on pull requests that change the benchmark")
HOSTED_BASELINE = ("the GitHub-hosted baseline the owned-runner JIT canary is compared with, so the hosted image is "
                   "the thing measured; dispatched by hand")
PERSONAL_MACOS = PERSONAL + "; builds the aarch64-apple-darwin target"

# (workflow file, job id) -> why the job stays on a GitHub-hosted image.
HOSTED_ALLOWED: dict[tuple[str, str], str] = {
    ("big-red-jit-canary.yml", "hosted-baseline"): HOSTED_BASELINE,
    ("control.yml", "control"): PERSONAL,
    ("developer-loop-benchmark.yml", "fresh-hosted"): HOSTED_ON_PURPOSE,
    ("developer-loop-benchmark.yml", "prepared-hosted"): HOSTED_ON_PURPOSE,
    ("promote.yml", "promote"): PERSONAL,
    ("release.yml", "build"): PERSONAL_MACOS,
    ("release.yml", "publish"): PERSONAL,
    ("sync-main.yml", "sync"): PERSONAL,
}


def _strip_comment(line: str) -> str:
    match = re.search(r"(^|\s)#", line)
    return line[: match.start()] if match else line


def hosted_jobs(path: Path) -> dict[str, list[str]]:
    """job id -> lines in that job that name a hosted label or a runner this scan cannot read ('' = outside jobs)."""
    found: dict[str, list[str]] = {}
    job = ""
    in_jobs = False
    block_indent = -1
    selector_indent = -1  # indent of the runs-on or matrix key whose nested lines are being read
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        indent = len(line) - len(line.lstrip())
        if selector_indent >= 0 and line.strip() and indent <= selector_indent:
            selector_indent = -1
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
        code = _strip_comment(line)
        if SELECTOR_KEY.match(code):
            selector_indent = indent
        if OWNER_FORM.match(code):
            continue
        # Inside runs-on or matrix (a block value, a list, or a matrix axis that runs-on reads as `matrix.x`), any
        # value the text cannot show could be a hosted label.
        if HOSTED.search(code) or (selector_indent >= 0 and UNREADABLE.search(code)):
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
        print("GitHub-hosted or unreadable runners outside HOSTED_ALLOWED (use a blacksmith-<n>vcpu-ubuntu-2404 label,")
        print("or the owner form for a job that also runs in teamleaderleo/glaeda):")
        print("\n".join(problems))
        return 1
    print(f"PASS: only the {len(HOSTED_ALLOWED)} allowlisted jobs name a GitHub-hosted runner")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
