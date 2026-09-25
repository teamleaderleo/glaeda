#!/usr/bin/env python3
"""Tests for scripts/gh-wait against a fake controller and a fake GitHub API."""

from __future__ import annotations

import contextlib
import http.server
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("gh_wait", os.fspath(ROOT / "scripts" / "gh-wait"))
spec = importlib.util.spec_from_loader("gh_wait", loader)
gw = importlib.util.module_from_spec(spec)
sys.modules["gh_wait"] = gw
loader.exec_module(gw)


class Fake:
    """One HTTP server playing both the controller and GitHub; the test sets `respond`."""

    def __init__(self) -> None:
        self.requests: list[tuple[str, dict]] = []
        self.respond = lambda path, headers: (404, {}, {})
        fake = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self) -> None:  # noqa: N802
                headers = {k.lower(): v for k, v in self.headers.items()}
                fake.requests.append((self.path, headers))
                code, extra, body = fake.respond(self.path, headers)
                data = b"" if code == 304 else json.dumps(body).encode()
                self.send_response(code)
                for k, v in extra.items():
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args) -> None:
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def paths(self, prefix: str) -> list[str]:
        return [p for p, _ in self.requests if p.startswith(prefix)]


class GhWaitTest(unittest.TestCase):
    def setUp(self) -> None:
        self.fake = Fake()
        self.addCleanup(self.fake.server.shutdown)
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        token = Path(tmp.name) / "controller.token"
        token.write_text("cmci_test\n")
        env = {"CMUX_CI_CONTROLLER": self.fake.url, "CMUX_CI_TOKEN_FILE": os.fspath(token), "GH_TOKEN": "ghp_test"}
        for k, v in env.items():
            old = os.environ.get(k)
            os.environ[k] = v
            self.addCleanup(lambda k=k, old=old: os.environ.pop(k, None) if old is None else os.environ.__setitem__(k, old))
        for name, value in {"GITHUB_API": self.fake.url + "/gh", "REST_UNTRACKED": 0.3, "REST_TRACKED": 0.6,
                            "CONTROLLER_RETRY": 0.05, "LONG_POLL": 1, "MIN_PASS": 0.05}.items():
            old = getattr(gw, name)
            setattr(gw, name, value)
            self.addCleanup(setattr, gw, name, old)

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = gw.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    def test_controller_terminal_needs_no_github(self) -> None:
        run = {"id": 5, "repo": "manaflow-ai/cmux", "status": "completed", "conclusion": "success", "attempt": 1,
               "url": "https://github.com/manaflow-ai/cmux/actions/runs/5"}
        self.fake.respond = lambda p, h: (200, {}, {"known": True, "terminal": True, "run": run})
        code, out, _ = self.run_main("run", "5")
        self.assertEqual(code, 0)
        self.assertEqual(out.strip(), "completed success https://github.com/manaflow-ai/cmux/actions/runs/5")
        self.assertEqual(self.fake.paths("/gh"), [])
        path, headers = self.fake.requests[0]
        self.assertEqual(path, "/v1/github/runs/5?attempt=1&wait=0")
        self.assertEqual(headers["authorization"], "Bearer cmci_test")

    def test_long_poll_until_terminal(self) -> None:
        calls = []

        def respond(path, headers):
            calls.append(path)
            done = len(calls) >= 3
            return 200, {}, {"known": True, "terminal": done, "run": {
                "id": 6, "repo": "a/b", "status": "completed" if done else "in_progress",
                "conclusion": "failure" if done else "", "url": "u"}}
        self.fake.respond = respond
        code, out, _ = self.run_main("run", "https://github.com/a/b/actions/runs/6")
        self.assertEqual(code, 1)
        self.assertEqual(out.strip(), "completed failure u")
        self.assertTrue(calls[1].endswith("wait=1"), calls)
        self.assertEqual(self.fake.paths("/gh"), [], "a tracked run must not touch GitHub on the first pass")

    def test_unknown_run_reads_github_once_right_away(self) -> None:
        def respond(path, headers):
            if path.startswith("/gh/"):
                return 200, {"ETag": '"e1"'}, {"id": 7, "name": "seed", "status": "completed", "conclusion": "success",
                                               "run_attempt": 1, "html_url": "https://x/7"}
            return 200, {}, {"known": False, "terminal": False}
        self.fake.respond = respond
        code, out, _ = self.run_main("run", "7", "--repo", "o/r", "--json")
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out)["source"], "github")
        self.assertEqual(self.fake.paths("/gh"), ["/gh/repos/o/r/actions/runs/7"])

    def test_controller_down_uses_etag_and_bounded_cadence(self) -> None:
        state = {"n": 0}

        def respond(path, headers):
            if not path.startswith("/gh/"):
                return 503, {}, {}
            state["n"] += 1
            if state["n"] == 1:
                return 200, {"ETag": '"e1"'}, {"id": 8, "status": "in_progress", "conclusion": None, "run_attempt": 1}
            if state["n"] == 2:
                self.assertEqual(headers.get("if-none-match"), '"e1"')
                return 304, {}, {}
            return 200, {"ETag": '"e2"'}, {"id": 8, "status": "completed", "conclusion": "cancelled", "run_attempt": 1,
                                           "html_url": "https://x/8"}
        self.fake.respond = respond
        start = time.monotonic()
        code, out, _ = self.run_main("run", "8")
        elapsed = time.monotonic() - start
        self.assertEqual(code, 1)
        self.assertEqual(out.strip(), "completed cancelled https://x/8")
        self.assertEqual(state["n"], 3)
        self.assertGreaterEqual(elapsed, 2 * gw.REST_UNTRACKED - 0.05, "GitHub reads must keep the fallback cadence")

    def test_rerun_waits_for_attempt(self) -> None:
        seen = []

        def respond(path, headers):
            seen.append(path)
            return 200, {}, {"known": True, "terminal": len(seen) > 1,
                             "run": {"status": "completed", "conclusion": "success", "attempt": 2, "url": "u"}}
        self.fake.respond = respond
        self.assertEqual(self.run_main("run", "9", "--attempt", "2")[0], 0)
        self.assertIn("attempt=2", seen[0])

    def test_sha_with_check(self) -> None:
        def respond(path, headers):
            return 200, {}, {"terminal": True, "conclusion": "success",
                             "runs": [{"id": 1, "name": "CI", "status": "completed", "conclusion": "success", "url": "https://r/1"}],
                             "suites": []}
        self.fake.respond = respond
        code, out, _ = self.run_main("sha", "abcdef1", "--check", "CI")
        self.assertEqual((code, out.strip()), (0, "completed success https://r/1"))
        self.assertEqual(self.fake.requests[0][0], "/v1/github/commits/abcdef1?repo=manaflow-ai%2Fcmux&check=CI&wait=0")

    def test_sha_rest_fallback_picks_newest_run_per_workflow(self) -> None:
        sha = "a" * 40

        def respond(path, headers):
            if path.startswith("/gh/"):
                return 200, {}, {"workflow_runs": [
                    {"id": 1, "name": "CI", "status": "completed", "conclusion": "failure"},
                    {"id": 2, "name": "CI", "status": "completed", "conclusion": "success", "html_url": "https://r/2"},
                    {"id": 3, "name": "Other", "status": "in_progress", "conclusion": None}]}
            return 503, {}, {}
        self.fake.respond = respond
        code, out, _ = self.run_main("sha", sha, "--check", "ci", "--repo", "o/r")
        self.assertEqual((code, out.strip()), (0, "completed success https://r/2"))

    def test_timeout(self) -> None:
        self.fake.respond = lambda p, h: (200, {}, {"known": True, "terminal": False, "run": {}})
        self.assertEqual(self.run_main("run", "10", "--timeout", "0.5")[0], 3)

    def test_nothing_can_answer(self) -> None:
        os.environ.pop("GH_TOKEN")
        os.environ["CMUX_CI_TOKEN_FILE"] = "/nonexistent/token"
        os.environ["PATH"], old = "/nonexistent", os.environ["PATH"]
        self.addCleanup(os.environ.__setitem__, "PATH", old)
        self.assertEqual(self.run_main("run", "11")[0], 4)

    def test_unknown_repo_is_final(self) -> None:
        self.fake.respond = lambda p, h: (404, {}, {}) if p.startswith("/gh/") else (200, {}, {"known": False})
        code, _, err = self.run_main("run", "12")
        self.assertEqual(code, 4)
        self.assertIn("--repo", err)
        self.assertEqual(len(self.fake.paths("/gh")), 1)

    def test_early_answers_do_not_spin(self) -> None:
        gw.MIN_PASS = 0.2
        self.fake.respond = lambda p, h: (200, {}, {"known": True, "terminal": False, "run": {}})
        self.run_main("run", "13", "--timeout", "1")
        self.assertLessEqual(len(self.fake.paths("/v1/")), 7)

    def test_garbage_is_not_a_failure_verdict(self) -> None:
        self.fake.respond = lambda p, h: (200, {}, ["not", "an", "object"]) if p.startswith("/v1/") else (500, {}, {})
        self.assertEqual(self.run_main("run", "14", "--timeout", "0.5")[0], 3)

    def test_attempt_from_url(self) -> None:
        self.assertEqual(gw.RunTarget("https://github.com/a/b/actions/runs/5/attempts/3", None, 1).attempt, 3)

    def test_usage(self) -> None:
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as cm:
                gw.main(["sha", "xyz"])
        self.assertEqual(cm.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
