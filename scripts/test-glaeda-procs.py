#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-procs."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import signal
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_procs", os.fspath(ROOT / "scripts" / "glaeda-procs"))
spec = importlib.util.spec_from_loader("glaeda_procs", loader)
gp = importlib.util.module_from_spec(spec)
sys.modules["glaeda_procs"] = gp
loader.exec_module(gp)

UID = 501
PAUSE = "/opt/homebrew/bin/python3 -c import signal; signal.pause()"
START = "Sat Sep 26 04:21:22 2026"


def proc(pid, ppid=1, command=PAUSE, elapsed=7200, rss=4096, cpu=0.0, stat="S", uid=UID,
         path="", start=START):
    return gp.Proc(pid, ppid, uid, elapsed, rss, cpu, stat, start, command, path)


class ParseTest(unittest.TestCase):
    def test_etime(self) -> None:
        self.assertEqual(gp.parse_etime("05"), 5)
        self.assertEqual(gp.parse_etime("01:05"), 65)
        self.assertEqual(gp.parse_etime("02:01:05"), 7265)
        self.assertEqual(gp.parse_etime("3-02:01:05"), 3 * 86400 + 7265)

    def test_ps_row_keeps_command_with_spaces(self) -> None:
        row = f"  42     1   501  1-00:00:01  4704   0.0 SNs  {START} {PAUSE}\n"
        [p] = gp.parse_ps(row + "garbage\n")
        self.assertEqual((p.pid, p.ppid, p.uid, p.elapsed, p.rss_kib), (42, 1, 501, 86401, 4704))
        self.assertEqual(p.start, START)
        self.assertEqual(p.command, PAUSE)

    def test_comm_paths_with_spaces(self) -> None:
        paths = gp.parse_comm("  7 /Applications/Claude.app/Contents/MacOS/Claude Helper\n")
        self.assertEqual(paths, {7: "/Applications/Claude.app/Contents/MacOS/Claude Helper"})


class FixtureTest(unittest.TestCase):
    def kind(self, p, min_age=3600):
        return gp.fixture_kind(p, UID, min_age)

    def test_orphaned_idle_pause_is_a_fixture(self) -> None:
        self.assertEqual(self.kind(proc(10)), "python-signal-pause")
        self.assertEqual(self.kind(proc(11, command="sleep infinity")), "sleep-forever")

    def test_protected_shapes_are_not_fixtures(self) -> None:
        self.assertIsNone(self.kind(proc(10, ppid=500)), "still has its parent")
        self.assertIsNone(self.kind(proc(10, uid=0)), "another user")
        self.assertIsNone(self.kind(proc(10, elapsed=60)), "younger than min age")
        self.assertIsNone(self.kind(proc(10, cpu=5.0)), "doing work")
        self.assertIsNone(self.kind(proc(10, stat="Z")), "already dead")
        self.assertIsNone(self.kind(proc(10, command=PAUSE + "; import os")), "more than the fixture")
        self.assertIsNone(self.kind(proc(10, command="python3 ci_dash.py")), "a real program")
        self.assertIsNone(self.kind(proc(10, command="sleep 30")), "a bounded sleep")


class SurveyTest(unittest.TestCase):
    def test_attribution(self) -> None:
        procs = [
            proc(100, command="/Users/u/.local/bin/claude --session-id 655bc8de-fb1f-4296", path="/Users/u/.local/bin/claude", rss=300_000),
            proc(101, ppid=100, command="/bin/zsh -c swift test", path="/bin/zsh", rss=1_000),
            proc(102, ppid=101, command="swift-test --package-path P", path="/X/usr/bin/swift-test", rss=50_000),
            proc(103, ppid=102, command="swift-frontend -frontend", path="/X/usr/bin/swift-frontend", rss=200_000),
            proc(200, command="Runner.Listener run", path="/r/bin/Runner.Listener"),
            proc(201, ppid=200, command="Runner.Worker", path="/r/bin/Runner.Worker"),
            proc(202, ppid=201, command="xcodebuild test", path="/usr/bin/xcodebuild"),
            proc(300, command="python3 ci_dash.py", path="/usr/bin/python3"),
            proc(301, command="python3 ci_dash.py", path="/usr/bin/python3"),
            proc(400),
            proc(401, command="/Users/u/.local/bin/codex app-server --listen unix://", path="/Users/u/.local/bin/codex"),
        ]
        s = gp.survey(procs, UID, 1)
        [session] = s["sessions"]
        self.assertEqual((session["pid"], session["session_id"], session["processes"]), (100, "655bc8de-fb1f-4296", 4))
        self.assertEqual(session["rss_bytes"], 551_000 * 1024)
        builds = {b["pid"]: b for b in s["builds"]}
        self.assertEqual(set(builds), {102, 202}, "the frontend under swift-test is not its own build")
        self.assertTrue(builds[102]["direct"])
        self.assertEqual(builds[102]["session"]["pid"], 100)
        self.assertEqual(builds[102]["rss_bytes"], 250_000 * 1024)
        self.assertEqual(builds[202]["origin"], "ci-runner")
        self.assertFalse(builds[202]["direct"])
        self.assertEqual([f["pid"] for f in s["fixtures"]], [400])
        self.assertEqual([(d["count"], d["pids"]) for d in s["duplicate_orphans"]], [(2, [300, 301])])

    def test_json_omits_command_lines(self) -> None:
        s = gp.survey([proc(400), proc(300, command="python3 secret_tool.py --token abc")], UID, 1)
        text = json.dumps(gp.public(s))
        self.assertNotIn("signal.pause", text)
        self.assertNotIn("--token", text)
        self.assertNotIn("_proc", text)


class ApplyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.receipt = Path(self.tmp.name) / "r" / "receipts.jsonl"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def fixtures(self, *procs):
        return [{"pid": p.pid, "kind": "python-signal-pause", "elapsed_seconds": p.elapsed,
                 "rss_bytes": p.rss_kib * 1024, "_proc": p} for p in procs]

    def test_signals_only_unchanged_identity(self) -> None:
        live = {10: proc(10), 11: proc(11), 12: proc(12)}
        seen = {10: proc(10),
                11: proc(11, start="Sat Sep 26 09:00:00 2026"),  # PID reused
                12: proc(12, ppid=77)}  # re-parented
        sent = []

        def kill(pid, sig):
            sent.append((pid, sig))
            seen.pop(pid)

        out = gp.apply(self.fixtures(*live.values()), self.receipt, look=seen.get, kill=kill, wait_seconds=0.5)
        self.assertEqual(sent, [(10, signal.SIGTERM)])
        self.assertEqual({r["pid"]: r["outcome"] for r in out},
                         {10: "terminated", 11: "skipped-changed", 12: "skipped-changed"})
        records = [json.loads(line) for line in self.receipt.read_text().splitlines()]
        self.assertEqual([r["pid"] for r in records], [10, 11, 12])
        self.assertTrue(all("command" not in r for r in records))

    def test_gone_and_zombie_and_survivor(self) -> None:
        p20, p21, p22 = proc(20), proc(21), proc(22)
        seen = {21: p21, 22: p22}

        def kill(pid, sig):
            if pid == 21:
                seen[21] = proc(21, stat="Z")

        out = gp.apply(self.fixtures(p20, p21, p22), self.receipt, look=seen.get, kill=kill, wait_seconds=0.2)
        self.assertEqual([r["outcome"] for r in out], ["gone", "terminated", "survived"])


class RenderTest(unittest.TestCase):
    def test_report_names_the_pressure_and_the_fix(self) -> None:
        h = {"load": [298.4, 250.0, 200.0], "cpus": 10, "pressure": "normal", "memory_bytes": 24 * gp.GIB,
             "swap_total_bytes": 5 * gp.GIB, "swap_used_bytes": 4 * gp.GIB}
        self.assertEqual(gp.verdict(h), "elevated")
        s = gp.survey([proc(400)], UID, 1)
        text = gp.render(h, s, None, 10)
        self.assertIn("host elevated", text)
        self.assertIn("glaeda-procs --apply", text)
        self.assertNotIn("signal.pause", text)


if __name__ == "__main__":
    unittest.main()
