#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-local-guard, on fake process tables."""

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
loader = importlib.machinery.SourceFileLoader("glaeda_local_guard", os.fspath(ROOT / "scripts" / "glaeda-local-guard"))
spec = importlib.util.spec_from_loader("glaeda_local_guard", loader)
lg = importlib.util.module_from_spec(spec)
sys.modules["glaeda_local_guard"] = lg
loader.exec_module(lg)

UID = 501
START = "Sun Sep 27 09:00:00 2026"
HOME = Path("/Users/leo")
PROJECT = "/Users/leo/Projects/cmux"
CLAUDE = "/Users/leo/Library/Application Support/Claude/claude-code/2.1.281/claude.app/Contents/MacOS/claude"


def proc(pid, ppid, command, path=None, elapsed=600, uid=UID, stat="S"):
    exe_path = path if path is not None else command.split(" ", 1)[0]
    return lg.Proc(pid, ppid, uid, elapsed, 1024, 0.0, stat, START, command, exe_path)


def cwds(mapping):
    return lambda pids: {p: mapping[p] for p in pids if p in mapping}


def session(pid=10):
    """launchd -> Claude desktop helper -> claude CLI -> zsh tool shell."""
    return [
        proc(1, 0, "/sbin/launchd"),
        proc(5, 1, "/Applications/Claude.app/Contents/MacOS/Claude"),
        proc(pid, 5, CLAUDE + " --session-id 0123abcd-0000-0000-0000-000000000000", path=CLAUDE),
        proc(pid + 1, pid, "/bin/zsh -c source snapshot && eval 'swift test --filter X'"),
    ]


def survey(procs, mapping=None, grace=30):
    return lg.survey(procs, UID, cwds(mapping or {}), grace, HOME)


class RuleTest(unittest.TestCase):
    def rule(self, command, path=None):
        return lg.rule(proc(2, 1, command, path=path))

    def test_heavy_runs_match(self) -> None:
        cases = {
            "/usr/bin/swift-build --package-path x": "swift-build",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/swift-test": "swift-test",
            "/usr/bin/swift test --filter Foo": "swift-test",
            "swift build -c release": "swift-build",
            "/usr/bin/xcodebuild -scheme cmux build": "xcodebuild",
            "xcodebuild -project a.xcodeproj build-for-testing": "xcodebuild",
            "/Users/leo/.cargo/bin/cargo +nightly build --release": "cargo-build",
            "cargo test -p foo": "cargo-test",
            "python3 tests/test_socket.py": "python-tests",
            "/opt/homebrew/bin/python3 -u /Users/leo/Projects/cmux/tests/test_x.py": "python-tests",
            "bash ./tests/test_ci_self_hosted_guard.sh": "shell-tests",
            "/bin/bash scripts/merge-main.sh": "merge-main",
            "python3 scripts/ci/merge_main.py --guards": "merge-main",
            "python3 scripts/verify-local.py": "verify-local",
        }
        for command, want in cases.items():
            self.assertEqual(self.rule(command), want, command)

    def test_framework_python_is_python(self) -> None:
        path = "/opt/homebrew/Cellar/python@3.13/3.13.1/Frameworks/Python.framework/Versions/3.13/Resources/Python.app/Contents/MacOS/Python"
        self.assertEqual(self.rule(f"{path} tests/test_a.py", path=path), "python-tests")

    def test_light_and_look_alike_runs_do_not_match(self) -> None:
        for command in (
            "/bin/zsh -c swift test --filter X",  # a tool shell; its child is the run
            "bash -c bash tests/test_x.sh",
            "grep swift test file",
            "python3 -m py_compile tests/test_x.py",
            "python3 -c import tests",
            "python3 scripts/test-glaeda-procs.py",  # not in a tests directory
            "python3 tests/helper.py",
            "xcodebuild -showBuildSettings",
            "xcodebuild -list",
            "cargo fmt",
            "cargo metadata --format-version 1",
            "swift --version",
            "swift package resolve",
            "bash tests/lib.sh",
        ):
            self.assertIsNone(self.rule(command), command)


class SurveyTest(unittest.TestCase):
    def test_agent_session_run_is_stopped_with_its_tree(self) -> None:
        procs = session() + [
            proc(20, 11, "/usr/bin/swift test --filter X"),
            proc(21, 20, "/usr/bin/swift-test --filter X"),  # nested: the topmost run speaks
            proc(22, 21, "/usr/bin/swift-frontend -c a.swift"),
        ]
        s = survey(procs, {20: PROJECT})
        [e] = s["stop"]
        self.assertEqual((e["pid"], e["rule"], e["owner"], e["processes"]), (20, "swift-test", "agent-session", 3))
        self.assertEqual(e["session"], {"agent": "claude", "pid": 10, "session_id": "0123abcd-0000-0000-0000-000000000000"})
        self.assertEqual(e["cwd"], PROJECT)
        self.assertEqual({q.pid for q in e["_tree"]}, {20, 21, 22})
        self.assertEqual(s["keep"], [])

    def test_merge_main_chain_stops_at_the_script(self) -> None:
        procs = session() + [
            proc(30, 11, "/bin/bash scripts/merge-main.sh"),
            proc(31, 30, "python3 scripts/ci/merge_main.py"),
            proc(32, 31, "python3 tests/test_a.py"),
        ]
        [e] = survey(procs)["stop"]
        self.assertEqual((e["pid"], e["rule"], e["processes"]), (30, "merge-main", 3))

    def test_codex_counts_as_an_agent(self) -> None:
        procs = [proc(1, 0, "/sbin/launchd"),
                 proc(40, 1, "/Users/leo/.codex/bin/codex app-server"),
                 proc(41, 40, "cargo build")]
        [e] = survey(procs)["stop"]
        self.assertEqual(e["session"]["agent"], "codex")

    def test_short_runs_get_grace(self) -> None:
        procs = session() + [proc(20, 11, "python3 tests/test_fast.py", elapsed=12)]
        s = survey(procs)
        self.assertEqual(s["stop"], [])
        self.assertEqual(s["keep"][0]["reason"], "young")

    def test_orphan_in_projects_is_stopped(self) -> None:
        procs = [proc(1, 0, "/sbin/launchd"),
                 proc(50, 1, "/bin/zsh -c eval 'bash tests/test_a.sh'"),
                 proc(51, 50, "bash tests/test_a.sh"),
                 proc(52, 1, "/usr/bin/xcodebuild -scheme cmux test"),
                 proc(53, 1, "/usr/bin/swift build")]
        s = survey(procs, {51: PROJECT + "/tests", 52: "/private/tmp/claude-501/x/scratch", 53: "/Users/leo/Downloads"})
        self.assertEqual(sorted((e["pid"], e["owner"]) for e in s["stop"]), [(51, "orphan"), (52, "orphan")])
        self.assertEqual([(e["pid"], e["reason"]) for e in s["keep"]], [(53, "orphan-outside-projects")])

    def test_orphan_without_cwd_evidence_is_kept(self) -> None:
        procs = [proc(1, 0, "/sbin/launchd"), proc(53, 1, "/usr/bin/swift build")]
        self.assertEqual(survey(procs)["keep"][0]["reason"], "orphan-cwd-unknown")

    def test_leos_interactive_runs_are_kept(self) -> None:
        procs = [proc(1, 0, "/sbin/launchd"),
                 proc(60, 1, "/Applications/cmux.app/Contents/MacOS/cmux"),
                 proc(61, 60, "/usr/bin/login -flp leo"),
                 proc(62, 61, "-zsh", path="/bin/zsh"),
                 proc(63, 62, "swift test"),
                 proc(70, 1, "tmux new -s work", path="/opt/homebrew/bin/tmux"),
                 proc(71, 70, "-zsh", path="/bin/zsh"),
                 proc(72, 71, "cargo build"),
                 proc(80, 1, "/Applications/Xcode.app/Contents/MacOS/Xcode"),
                 proc(81, 80, "/usr/bin/xcodebuild -scheme cmux build")]
        s = survey(procs, {63: PROJECT, 72: PROJECT, 81: PROJECT})
        self.assertEqual(s["stop"], [])
        self.assertEqual({e["pid"]: e["reason"] for e in s["keep"]}, {63: "not-agent", 72: "not-agent", 81: "not-agent"})

    def test_ci_runner_and_glaeda_apple_are_kept(self) -> None:
        runner = "/Users/leo/Projects/cmux/.local/runner"
        procs = session() + [
            proc(90, 1, f"{runner}/bin/Runner.Listener run", path=f"{runner}/bin/Runner.Listener"),
            proc(91, 90, f"{runner}/bin/Runner.Worker spawnclient", path=f"{runner}/bin/Runner.Worker"),
            proc(92, 91, "/bin/bash -e /Users/leo/Projects/cmux/.local/runner/_work/_temp/x.sh"),
            proc(93, 92, "xcodebuild -scheme cmux test"),
            proc(94, 1, "swift test"),  # a job's orphan: it runs inside the runner
            proc(95, 11, "python3 scripts/apple_build.py warm"),
            proc(96, 95, "xcodebuild -scheme cmux build"),
        ]
        s = survey(procs, {93: runner + "/_work/cmux", 94: runner + "/_work/cmux", 96: PROJECT})
        self.assertEqual(s["stop"], [])
        self.assertEqual({e["pid"]: e["reason"] for e in s["keep"]},
                         {93: "ci-runner", 94: "ci-runner", 96: "glaeda-apple"})

    def test_other_users_and_zombies_are_ignored(self) -> None:
        procs = session() + [proc(20, 11, "swift test", uid=0), proc(21, 11, "swift build", stat="Z")]
        s = survey(procs)
        self.assertEqual((s["stop"], s["keep"]), ([], []))


class ScopeTest(unittest.TestCase):
    def test_only_a_hygiene_only_mac(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            config = Path(tmp) / "update.json"
            self.assertFalse(lg.laptop(config))
            config.write_text(json.dumps({"setupArgs": []}))
            self.assertFalse(lg.laptop(config))
            config.write_text(json.dumps({"setupArgs": ["--hygiene-only"]}))
            self.assertEqual(lg.laptop(config), sys.platform == "darwin")
            config.write_text("{not json")
            self.assertFalse(lg.laptop(config))

    def test_override_file_and_durations(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "allow"
            self.assertIsNone(lg.allowed_until(path))
            path.write_text("1790000000\n")
            self.assertEqual(lg.allowed_until(path), 1790000000.0)
            path.write_text("garbage")
            self.assertIsNone(lg.allowed_until(path))
        self.assertEqual([lg.parse_duration(t) for t in ("90s", "45m", "2h", "10", "0")], [90, 2700, 7200, 600, 0])
        with self.assertRaises(Exception):
            lg.parse_duration("soon")

    def test_parse_lsof(self) -> None:
        text = "p20\nfcwd\nn/Users/leo/Projects/cmux\np21\nfcwd\nn/private/tmp/claude-501/a b\n"
        self.assertEqual(lg.parse_lsof(text), {20: "/Users/leo/Projects/cmux", 21: "/private/tmp/claude-501/a b"})


class ApplyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.log = Path(self.tmp.name) / "guard.jsonl"
        self.procs = session() + [proc(20, 11, "swift test"), proc(21, 20, "swift-test"), proc(22, 21, "swift-frontend")]
        self.stop = survey(self.procs, {20: PROJECT})["stop"]
        self.sent: list[tuple[int, int]] = []

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def kill(self, pid, sig):
        self.sent.append((pid, sig))

    def records(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def run_apply(self, look, fresh):
        by_pid = {p.pid: p for p in self.procs}
        return lg.apply(self.stop, self.log, look=look or (lambda pid: by_pid.get(pid)), kill=self.kill,
                        fresh=fresh, sleep=lambda s: None)

    def test_term_then_kill_survivors(self) -> None:
        survivor = next(p for p in self.procs if p.pid == 22)
        [r] = self.run_apply(None, lambda: [survivor])
        self.assertEqual(sorted(pid for pid, sig in self.sent if sig == signal.SIGTERM), [20, 21, 22])
        self.assertEqual(self.sent[-1], (22, signal.SIGKILL))
        self.assertEqual(r["outcome"], "killed")
        outcomes = [x["outcome"] for x in self.records()]
        self.assertEqual(outcomes, ["signalling", "killed"])
        self.assertEqual(self.records()[1]["session"]["agent"], "claude")
        self.assertNotIn("_proc", self.records()[1])

    def test_clean_exit(self) -> None:
        [r] = self.run_apply(None, lambda: [])
        self.assertEqual(r["outcome"], "terminated")
        self.assertNotIn(signal.SIGKILL, {s for _, s in self.sent})

    def test_changed_or_gone_root_is_not_signalled(self) -> None:
        other = proc(20, 11, "swift test --other")
        [r] = self.run_apply(lambda pid: other, lambda: [])
        self.assertEqual((r["outcome"], self.sent), ("skipped-changed", []))
        [r] = self.run_apply(lambda pid: None, lambda: [])
        self.assertEqual((r["outcome"], self.sent), ("gone", []))

    def test_recent_skips_intent_records(self) -> None:
        self.run_apply(None, lambda: [])
        self.assertEqual([r["outcome"] for r in lg.recent(self.log, 10)], ["terminated"])
        text = lg.render_status(0, None, lg.recent(self.log, 10), True)
        self.assertIn("swift-test", text)
        self.assertIn("claude 0123abcd", text)


if __name__ == "__main__":
    unittest.main()
