#!/usr/bin/env python3
"""Reject direct ``gh auth token`` callers unless their login fallback is documented.

GitHub App tokens are the default for reads.  A small number of write paths still
need the host login; those paths carry an explicit marker immediately above the
call so a new login consumer cannot slip into the tree unnoticed.
"""
from __future__ import annotations

import argparse
import ast
import subprocess
import sys
from pathlib import Path

MARKER = "gh-auth-token-lint: allow"
SKIP_SUFFIXES = {".md", ".rst", ".txt", ".json", ".lock", ".svg", ".png", ".jpg"}


def allowed(lines: list[str], line: int) -> bool:
    lo = max(0, line - 4)
    return any(MARKER in lines[i] for i in range(lo, min(len(lines), line + 1)))


def literal(node: ast.AST) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.Name):
        return node.id
    return None


def python_hits(path: Path, text: str) -> list[tuple[int, str]]:
    try:
        tree = ast.parse(text, filename=str(path))
    except SyntaxError:
        return []
    lines = text.splitlines()
    hits: list[tuple[int, str]] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        for arg in node.args:
            if isinstance(arg, (ast.List, ast.Tuple)):
                values = [literal(item) for item in arg.elts]
                first = (values[0] if values else "") or ""
                if len(values) >= 3 and values[1:3] == ["auth", "token"] and (
                        first in {"gh", "gh_path", "gh_bin"} or first.endswith("/gh")):
                    if not allowed(lines, node.lineno - 1):
                        hits.append((node.lineno, "Python argv invokes gh auth token"))
    return hits


def shell_hits(path: Path, text: str) -> list[tuple[int, str]]:
    hits: list[tuple[int, str]] = []
    lines = text.splitlines()
    for number, raw in enumerate(lines, 1):
        code = raw.split("#", 1)[0]
        if "gh" not in code or "auth" not in code or "token" not in code:
            continue
        if "gh auth token" in code and not allowed(lines, number - 1):
            hits.append((number, "Shell command invokes gh auth token"))
    return hits


def tracked(root: Path) -> list[Path]:
    result = subprocess.run(["git", "ls-files", "-z"], cwd=root, check=True, capture_output=True)
    return [root / item for item in result.stdout.decode().split("\0") if item]


def scan(root: Path) -> list[str]:
    findings: list[str] = []
    for path in tracked(root):
        if path.suffix.lower() in SKIP_SUFFIXES or path.name == Path(__file__).name:
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        is_python = path.suffix == ".py" or text.startswith("#!") and "python" in text.splitlines()[0]
        hits = python_hits(path, text) if is_python else shell_hits(path, text)
        findings.extend(f"{path.relative_to(root)}:{line}: {why}" for line, why in hits)
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, default=Path.cwd())
    args = parser.parse_args()
    findings = scan(args.root.resolve())
    if findings:
        print("gh_auth_token_lint: direct login-token callers are not allowed:", file=sys.stderr)
        print("\n".join(findings), file=sys.stderr)
        return 1
    print("gh_auth_token_lint: clean")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
