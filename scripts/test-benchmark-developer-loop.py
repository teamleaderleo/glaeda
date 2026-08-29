#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import subprocess
import tempfile
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

    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        fake_cargo = root / "cargo"
        fake_cargo.write_text(
            "#!/bin/sh\n"
            'if [ "${1:-}" = "--version" ]; then\n'
            '  printf "cargo 1.97.1 (fixture)\\n"\n'
            "  exit 0\n"
            "fi\n"
            "exit 23\n",
            encoding="utf-8",
        )
        fake_cargo.chmod(0o700)
        output = root / "failure.json"
        environment = os.environ.copy()
        environment["PATH"] = f"{root}:{environment['PATH']}"
        failed = subprocess.run(
            [str(SCRIPT), "--output", str(output)],
            cwd=ROOT,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        assert failed.returncode == 23, failed.stderr
        receipt = json.loads(output.read_text(encoding="utf-8"))
        assert receipt["result"]["exit_code"] == 23
        assert "command/time exit-code mismatch" not in failed.stderr
    print("benchmark developer loop contract tests passed")


if __name__ == "__main__":
    main()
