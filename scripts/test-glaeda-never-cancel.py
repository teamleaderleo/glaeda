#!/usr/bin/env python3
"""Tests for glaeda_never_cancel: the predicate's table, cancel_run, and that nothing else in glaeda cancels a run."""
from __future__ import annotations

import base64
import re
import subprocess
import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
import glaeda_never_cancel as nc  # noqa: E402

NOW = 1_800_000_000.0
SHA = "a" * 40
WF = """\
name: x
on: push
jobs:
  build:
    runs-on: x
    timeout-minutes: 90
    steps:
      - run: true
        timeout-minutes: 500
  publish:
    needs: build
    timeout-minutes: 20
    runs-on: x
    steps: [{run: true}]
"""


def run(path="nightly.yml", status="in_progress", event="schedule", name="", attempt=1):
    return {"id": 7, "path": f".github/workflows/{path}", "status": status, "event": event, "name": name,
            "display_title": name, "head_sha": SHA, "run_attempt": attempt}


def job(status="in_progress", started_min_ago=5.0, runner="r1"):
    return {"id": 1, "name": "build", "status": status, "runner_name": runner if status != "queued" else None,
            "started_at": datetime.fromtimestamp(NOW - started_min_ago * 60, timezone.utc)
            .isoformat().replace("+00:00", "Z") if status != "queued" else None}


class DeliveryTable(unittest.TestCase):
    def test_which_runs_are_delivery_runs(self):
        cases = [
            (run("nightly.yml"), True),
            (run("nightly-sign.yml"), True),
            (run("nightly-next-train.yml"), True),
            (run("nightly-next-tick.yml"), True),
            (run("nightly-next-canary.yml"), True),
            (run("cmux-tui-nightly.yml"), True),
            (run("cmux-next-dev-daily.yml", event="workflow_dispatch"), True),
            (run("cmux-next-dev-daily-dispatch.yml"), True),
            (run("cloud-vm-image-promote.yml"), True),
            (run("promote.yml"), True),
            (run("release.yml"), True),
            (run("cmux-next.yml", event="push"), True),
            (run("cmux-next.yml", event="workflow_dispatch", name="cmux NEXT DEV daily · abc123"), True),
            (run("cmux-next.yml", event="pull_request"), False),
            (run("ci.yml", event="pull_request"), False),
            (run("ci.yml", event="push"), False),
            ({**run("nightly.yml"), "path": ".github/workflows/nightly.yml@refs/heads/feature"}, True),
            ({**run("x"), "path": ""}, False),
        ]
        for r, want in cases:
            with self.subTest(path=r["path"], event=r["event"], name=r["name"]):
                self.assertEqual(bool(nc.delivery_reason(r)), want)

    def test_any_branch(self):
        for branch in ("main", "feat-cmux-next", "some-fork-branch"):
            r = {**run("nightly.yml"), "head_branch": branch}
            self.assertFalse(nc.may_cancel(r, [job()], 90, NOW)[0], branch)


class PredicateTable(unittest.TestCase):
    def test_table(self):
        # (run, jobs, timeout minutes, allowed)
        cases = [
            ("non-delivery started run", run("ci.yml", event="pull_request"), [job()], None, True),
            ("non-delivery, jobs unreadable", run("ci.yml", event="pull_request"), None, None, True),
            ("delivery, queued, no job started", run(status="queued"), [job("queued")], None, True),
            ("delivery, started, within timeout", run(), [job(started_min_ago=30)], 90, False),
            ("delivery, started, waiting for a runner", run(), [job("queued")], 90, False),
            ("delivery, started, exactly at timeout", run(), [job(started_min_ago=90)], 90, False),
            ("delivery, stuck past timeout", run(), [job(started_min_ago=91)], 90, True),
            ("delivery, completed job past timeout", run(), [job("completed", started_min_ago=500)], 90, False),
            ("delivery, started, timeout unreadable", run(), [job(started_min_ago=1000)], None, False),
            ("delivery, jobs unreadable", run(), None, 90, False),
            ("delivery, run completed", run(status="completed"), [], 90, False),
            ("DEV daily stuck in setup 19 min of 20", run("cmux-next-dev-daily.yml"),
             [job(started_min_ago=19)], 20, False),
            ("promote past timeout", run("cloud-vm-image-promote.yml"), [job(started_min_ago=400)], 360, True),
        ]
        for label, r, jobs, timeout, want in cases:
            with self.subTest(label):
                ok, why = nc.may_cancel(r, jobs, timeout, NOW)
                self.assertEqual(ok, want, why)
                self.assertTrue(why)

    def test_timeout_reading(self):
        self.assertEqual(nc.job_timeout_minutes(WF), 90)  # job level only; a step's 500 does not count
        self.assertEqual(nc.job_timeout_minutes(WF.replace("    timeout-minutes: 20\n", "")), 360)
        self.assertIsNone(nc.job_timeout_minutes(None))
        self.assertIsNone(nc.job_timeout_minutes("name: x\non: push\n"))
        self.assertIsNone(nc.job_timeout_minutes("jobs:\n  - nope\n"))


class Fake:
    def __init__(self, r, jobs, wf=WF):
        self.r, self.jobs, self.wf, self.posts, self.gets = r, jobs, wf, [], []

    def get(self, path):
        self.gets.append(path)
        if re.search(r"/actions/runs/7/attempts/\d+/jobs", path):
            if self.jobs is None:
                raise OSError("jobs unreadable")
            return {"jobs": self.jobs}
        if "/contents/" in path:
            assert f"ref={SHA}" in path, path
            if self.wf is None:
                raise OSError("404")
            return {"content": base64.b64encode(self.wf.encode()).decode()}
        raise AssertionError(path)

    def post(self, path):
        self.posts.append(path)


class CancelRun(unittest.TestCase):
    def cancel(self, fake, force=False):
        return nc.cancel_run(fake.get, fake.post, "o/r", fake.r, force=force, now=lambda: NOW)

    def test_non_delivery_run_is_cancelled_without_extra_reads(self):
        f = Fake(run("ci.yml", event="pull_request"), [job()])
        self.assertTrue(self.cancel(f)[0])
        self.assertEqual((f.gets, f.posts), ([], ["/repos/o/r/actions/runs/7/cancel"]))
        f = Fake(run("ci.yml", event="pull_request"), [job()])
        self.cancel(f, force=True)
        self.assertEqual(f.posts, ["/repos/o/r/actions/runs/7/force-cancel"])

    def test_started_delivery_run_is_never_cancelled(self):
        for force in (False, True):
            f = Fake(run(), [job(started_min_ago=30)])
            ok, why = self.cancel(f, force)
            self.assertFalse(ok, why)
            self.assertEqual(f.posts, [])

    def test_stuck_delivery_run_past_its_timeout_is_cancelled(self):
        f = Fake(run(), [job(started_min_ago=95)])
        self.assertTrue(self.cancel(f)[0])
        self.assertEqual(f.posts, ["/repos/o/r/actions/runs/7/cancel"])

    def test_unreadable_reads_refuse(self):
        for f in (Fake(run(), None), Fake(run(), [job(started_min_ago=500)], wf=None)):
            self.assertFalse(self.cancel(f)[0])
            self.assertEqual(f.posts, [])


# Ways to cancel a GitHub Actions run or job: the REST endpoints in any spelling, gh's command, and SDK names.
CANCEL_CALL = re.compile(
    r"actions/runs/[^\s\"'`]*?/(?:force-)?cancel(?![\w-])"   # .../actions/runs/{id}/cancel, force-cancel
    r"|[\"'`]/(?:force-)?cancel[\"'`]"                        # a path piece joined on later: "/cancel"
    r"|\bgh\b[^\n]*\brun\s+cancel\b"                          # gh run cancel N
    r"|cancel[_-]?workflow[_-]?run|cancelWorkflowRun",        # octokit and other SDK names
    re.IGNORECASE)
GUARD = "scripts/glaeda_never_cancel.py"


def scanned_files() -> list[str]:
    out = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-z"], capture_output=True, check=True).stdout
    names = [n for n in out.decode().split("\0") if n]
    # Docs, and tests that fake GitHub's endpoints, cancel nothing.
    return [n for n in names if not (n.endswith(".md") or re.search(r"(^|/)test[-_][^/]*$", n) or n.startswith("tests/"))]


class NoOtherCancelPath(unittest.TestCase):
    def test_only_the_guard_cancels_runs(self):
        offenders = []
        for name in scanned_files():
            path = ROOT / name
            try:
                text = path.read_text()
            except (UnicodeDecodeError, OSError):
                continue
            for lineno, line in enumerate(text.splitlines(), 1):
                if CANCEL_CALL.search(line) and name != GUARD:
                    offenders.append(f"{name}:{lineno}: {line.strip()}")
        self.assertEqual(offenders, [], "cancel a run only through glaeda_never_cancel.cancel_run, so a started "
                         "nightly, DEV or promote run is never cancelled:\n" + "\n".join(offenders))

    def test_the_scan_catches_each_spelling(self):
        for line in ('api.ok("POST", f"/repos/{repo}/actions/runs/{run_id}/cancel")',
                     'post(f"/repos/{r}/actions/runs/{i}/force-cancel")',
                     'url = base + "/cancel"',
                     "gh run cancel 123 -R o/r",
                     "subprocess.run(['gh', 'api', '-X', 'POST', 'repos/o/r/actions/runs/1/cancel'])",
                     "octokit.rest.actions.cancelWorkflowRun({owner, repo, run_id})"):
            self.assertTrue(CANCEL_CALL.search(line), line)
        for line in ("cancel-in-progress: false", "future.cancel()", 'conclusion == "cancelled"',
                     "the rescue cancels a job"):
            self.assertFalse(CANCEL_CALL.search(line), line)

    def test_glaeda_route_cancels_through_the_guard(self):
        route = (ROOT / "scripts/glaeda-route").read_text()
        self.assertIn("cancel_run(", route)
        self.assertIn("force=True", route)


if __name__ == "__main__":
    unittest.main()
