#!/usr/bin/env python3
"""No Python script under scripts/ holds a top-level name defined twice.

Python keeps the last top-level def, so a second `def f` silently replaces the first for every caller in the file,
including callers written against the first one's signature. 2026-11-27: leo-workspace tools/wt held two
`def fleet_seed` from two PRs that each passed alone, and `wt new` raised TypeError for every brokered mirror
(leo-workspace #12768, detector #12778). The same scan found two here: scripts/glaeda-disk's `_pid_alive` and
`_sha256`, each defined twice with different bodies. An upper-case constant set twice is the same shadow.
2026-11-27: scripts/glaeda-gh set LAND_FORWARD_READ_WAIT to 300.0 (with a comment saying what 300 meant) and later to
900.0, so 900 ruled and the comment was false. The scan never read it: glaeda-gh starts `#!/bin/sh` and re-execs
itself as Python from its second line (three double quotes, then `exec" python3 ...`), so the shebang test skipped it. A file with that
trampoline is Python too. A constant rebuilt from itself (`X = X.replace(...)`) reads the value before it, so it
shadows nothing and is not counted.
"""
from __future__ import annotations

import ast
from concurrent.futures import ProcessPoolExecutor
import os
from pathlib import Path
import unittest

SCRIPTS = Path(__file__).resolve().parent


def duplicates(source: str) -> list[str]:
    seen: dict[str, int] = {}
    out = []
    for node in ast.parse(source).body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            names = [node.name]
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            names = [t.id for t in targets if isinstance(t, ast.Name) and t.id.isupper()]
            # X = X.replace(...) builds on the value before it: that one is read, not shadowed
            reads = {n.id for n in ast.walk(node.value) if isinstance(n, ast.Name)} if node.value else set()
            names = [n for n in names if n not in reads]
        else:
            continue
        for name in names:
            if name in seen:
                out.append(f"{name}: line {seen[name]} and line {node.lineno}")
            else:
                seen[name] = node.lineno
    return out


def python_scripts() -> list[Path]:
    """Every regular file in scripts/ (and its subfolders) that is a .py or has a python shebang; symlinks are
    read once, through their target."""
    out = []
    for p in sorted(SCRIPTS.rglob("*")):
        if not p.is_file() or p.is_symlink():
            continue
        if p.suffix == ".py":
            out.append(p)
            continue
        try:
            with p.open("rb") as f:
                first = f.readline(200)
        except OSError:
            continue
        if first.startswith(b"#!") and b"python" in first:
            out.append(p)
            continue
        try:
            with p.open("rb") as f:
                f.readline(200)
                second = f.readline(200)
        except OSError:
            continue
        if first.startswith(b"#!") and second.startswith(b'"""exec" python'):
            out.append(p)  # a sh trampoline whose second line re-execs the file as Python (scripts/glaeda-gh)
    return out


def scan(path: Path) -> list[str]:
    try:
        return duplicates(path.read_text(encoding="utf-8"))
    except (UnicodeDecodeError, SyntaxError):
        return []  # not this test's question


class NoShadowTest(unittest.TestCase):
    def test_no_top_level_name_is_defined_twice(self):
        scripts = python_scripts()
        self.assertIn(SCRIPTS / "glaeda-disk", scripts)  # the scan reaches the shebang tools, not only *.py
        self.assertIn(SCRIPTS / "glaeda-gh", scripts)  # and the `#!/bin/sh` trampoline that re-execs as Python
        # One parse per file, on every core: the 223 scripts hold multi-MB files (test-glaeda-gh.py, 3 MB, parses in
        # about 8 s alone), so a serial scan took about 27 s.
        with ProcessPoolExecutor(max_workers=os.cpu_count() or 1) as pool:
            found = dict(zip(scripts, pool.map(scan, scripts)))
        for script, dups in found.items():
            with self.subTest(script=str(script.relative_to(SCRIPTS))):
                self.assertEqual(dups, [])

    def test_it_finds_a_shadowed_def(self):
        self.assertEqual(duplicates("def f(a):\n    pass\nX = 1\ndef f(a, b):\n    pass\nX = 2\n"),
                         ["f: line 1 and line 4", "X: line 3 and line 6"])

    def test_a_constant_set_twice_is_found_and_one_built_from_itself_is_not(self):
        self.assertEqual(duplicates("WAIT = 300.0\nF = 'a B'\nF = F.replace('B', 'b')\nWAIT = 900.0\n"),
                         ["WAIT: line 1 and line 4"])


if __name__ == "__main__":
    unittest.main()
