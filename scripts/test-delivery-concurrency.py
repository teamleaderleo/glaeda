#!/usr/bin/env python3
"""glaeda's release, promote and mirror workflows never evict or cancel a waiting delivery run.

GitHub keeps one pending run per concurrency group and replaces it with the next: the old shared
ota-canary group let a newer push replace an older pending publish, so that commit never got its
durable release. scripts/delivery_concurrency.py is a byte-identical copy of manaflow-ai/cmux
scripts/ci/delivery_concurrency.py (the same check runs there and in manaflow-ai/cmuxterm-hq).
"""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"
sys.path.insert(0, str(Path(__file__).resolve().parent))
from delivery_concurrency import can_be_shared, findings, stale_allowances  # noqa: E402

DELIVERY_GLOBS = ("release*.yml", "*promote*.yml", "mirror*.yml", "nightly*.yml", "*-nightly*.yml")
REQUIRED = ("release.yml", "promote.yml", "mirror.yml")
ALLOWED_SHARED: dict[tuple[str, str], str] = {
    ("mirror.yml", ""): (
        "a schedule-only copy of upstream's tags, releases and channel files as they are when it starts; "
        "the waiting run copies everything a replaced one would have"),
}


def delivery_workflows() -> list[Path]:
    return sorted({p for pattern in DELIVERY_GLOBS for p in WORKFLOWS.glob(pattern)})


class DeliveryConcurrencyTest(unittest.TestCase):
    def test_required_workflows_are_in_scope(self) -> None:
        names = {p.name for p in delivery_workflows()}
        for name in REQUIRED:
            self.assertIn(name, names)

    def test_delivery_groups_are_keyed_per_run_or_payload(self) -> None:
        workflows = delivery_workflows()
        problems = [p for path in workflows for p in findings(path, ALLOWED_SHARED)]
        problems += stale_allowances(workflows, ALLOWED_SHARED)
        self.assertEqual(problems, [], "\n".join(problems))

    def test_the_check_fails_on_a_fixed_group_and_cancel_in_progress(self) -> None:
        self.assertTrue(can_be_shared("ota-canary"))
        self.assertFalse(can_be_shared("ota-canary-${{ github.sha }}"))
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "release-sample.yml"
            path.write_text("on: push\njobs:\n  publish:\n    runs-on: x\n    concurrency:\n"
                            "      group: fixed\n      cancel-in-progress: true\n")
            found = findings(path, {})
        self.assertEqual(len(found), 2, found)


if __name__ == "__main__":
    unittest.main()
