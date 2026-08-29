#!/usr/bin/env python3
from __future__ import annotations

import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "benchmark-developer-loop"
FIXTURE = ROOT / "benchmarks" / "developer-loop-doc-edit.patch"


def main() -> None:
    subprocess.run(["bash", "-n", str(SCRIPT)], check=True)
    command = json.loads(
        subprocess.run(
            [str(SCRIPT), "--print-command"],
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        ).stdout
    )
    assert command[:7] == [
        "cargo",
        "test",
        "--quiet",
        "--locked",
        "--lib",
        "--bins",
        "--",
    ]
    assert command[7::2] == ["--skip"] * 16
    excluded = command[8::2]
    assert len(excluded) == 16
    assert len(set(excluded)) == 16
    assert all("::tests::" in test_name for test_name in excluded)
    subprocess.run(
        ["git", "apply", "--check", str(FIXTURE)], cwd=ROOT, check=True
    )
    fixture = FIXTURE.read_text(encoding="utf-8")
    assert fixture.count("Benchmark fixture: one source-only edit") == 1
    print("benchmark developer loop contract tests passed")


if __name__ == "__main__":
    main()
