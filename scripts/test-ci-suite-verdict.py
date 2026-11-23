#!/usr/bin/env python3
"""CI's `suite` helper reads a verdict line, not only the exit code.

A test that exec's, or calls os._exit, replaces the test runner: the step sees exit 0 with no summary
line and reads green while every test after that one never ran. Measured 2026-11-22 on
scripts/test-glaeda-gh.py, whose combined-short-flag case exec'd a stand-in guard (`exit 0`): `-k flag`
and `-k main_` each ended at exit 0 with no `Ran N tests` line, 22 tests unrun.

These tests run the helper exactly as .github/workflows/ci.yml ships it: the block is lifted out of the
workflow rather than copied here, so a change to one is a change to both.
"""
from __future__ import annotations

import re
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CI = ROOT / ".github" / "workflows" / "ci.yml"
# The helper and the two patterns it reads, as one run of lines ending at the function's closing brace.
BLOCK = re.compile(r"^ *VERDICT=.*?^ *\}\n", re.S | re.M)


def helper() -> str:
    """The shipped `suite` helper, dedented out of the workflow."""
    found = BLOCK.search(CI.read_text(encoding="utf-8"))
    assert found, f"no `suite` helper block in {CI}: the test cannot check what it cannot find"
    return textwrap.dedent(found.group(0))


def run_suites(*commands: str) -> subprocess.CompletedProcess[str]:
    """Run the helper over `commands`, each a shell line, the way the workflow's step does (bash -e)."""
    body = "\n".join(f"suite bash -c {command!r}" for command in commands)
    script = ("set -e\nfailed=()\n" + helper() + body
              + '\nprintf "failed=%s\\n" "${#failed[@]}"\nfor f in "${failed[@]}"; do echo "- $f"; done\n')
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp, "step.sh")
        path.write_text(script, encoding="utf-8")
        return subprocess.run(["bash", "-e", str(path)], capture_output=True, text=True, check=False)


class CiSuiteVerdict(unittest.TestCase):
    def test_the_workflow_still_carries_the_helper_the_tests_check(self) -> None:
        text = helper()
        self.assertIn("suite() {", text)
        self.assertIn("no verdict printed", text)
        self.assertIn("PIPESTATUS", text, "the suite's own exit code, not tee's, decides a failure")

    def test_a_suite_that_prints_its_verdict_passes(self) -> None:
        out = run_suites('echo ..; echo "Ran 3 tests in 0.1s"; echo OK',
                         'echo "workspace bootstrap tests passed"')
        self.assertIn("failed=0", out.stdout, out.stdout + out.stderr)

    def test_a_verdict_on_stderr_counts(self) -> None:
        """unittest writes `Ran N tests` and `OK` to stderr, not stdout. The helper merges the suite's streams
        (`2>&1`) before it reads them; drop that and every unittest suite the workflow runs -- most of the 77 --
        would read as verdictless at once. This pins the merge, not only the patterns."""
        out = run_suites('echo ..; { echo "Ran 3 tests in 0.1s"; echo OK; } >&2',
                         'echo "Ran 7 tests in 1.1s" >&2; echo FAILED >&2; exit 0')
        self.assertIn("failed=0", out.stdout, out.stdout + out.stderr)
        self.assertIn("2>&1", helper(), "the suite's streams are merged before the verdict is read")

    def test_a_nonzero_exit_is_still_a_failure_and_keeps_its_code(self) -> None:
        out = run_suites('echo boom; exit 3')
        self.assertIn("failed=1", out.stdout, out.stdout)
        self.assertIn("Suite failed (exit 3)", out.stdout)
        self.assertIn("boom", out.stdout, "the suite's own output still reaches the log")

    def test_exit_zero_with_no_verdict_is_a_failure(self) -> None:
        out = run_suites('echo "replaced the runner"; exit 0')
        self.assertIn("failed=1", out.stdout, out.stdout)
        self.assertIn("no verdict printed", out.stdout)

    def test_the_refusal_names_the_last_error_line(self) -> None:
        out = run_suites('echo started; echo "Traceback (most recent call last)"; exit 0')
        self.assertIn("no verdict printed", out.stdout)
        self.assertIn("last error: Traceback (most recent call last)", out.stdout)

    def test_every_suite_the_workflow_runs_can_print_a_verdict(self) -> None:
        """A check that goes red must be a real break, never the wording of a suite we ship."""
        lines = [line.strip()[len("suite "):] for line in CI.read_text(encoding="utf-8").splitlines()
                 if line.strip().startswith("suite ")]
        self.assertGreater(len(lines), 50, "the suite list reads short: the parse, not the workflow, is wrong")
        silent = []
        for command in lines:
            path = ROOT / command.split()[-1]
            if not path.exists():
                silent.append((command, "no such file"))
                continue
            text = path.read_text(encoding="utf-8", errors="replace")
            if "unittest.main(" in text or re.search(r"tests? passed", text):
                continue
            silent.append((command, "prints neither unittest's summary nor `... tests passed`"))
        self.assertEqual(silent, [])


if __name__ == "__main__":
    unittest.main()
