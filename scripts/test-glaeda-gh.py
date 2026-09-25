#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-gh, with a fake GitHub transport."""

from __future__ import annotations

import contextlib
import fcntl
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_gh", os.fspath(ROOT / "scripts" / "glaeda-gh"))
spec = importlib.util.spec_from_loader("glaeda_gh", loader)
gg = importlib.util.module_from_spec(spec)
sys.modules["glaeda_gh"] = gg
loader.exec_module(gg)

TOKEN = "ghp_faketoken_must_never_touch_disk"
NOW = float(int(time.time()))  # watch files carry real mtimes


def pr_node(number: int, state: str = "OPEN", rollup: str | None = "PENDING", checks: list | None = None) -> dict:
    return {
        "number": number, "url": f"https://github.com/o/r/pull/{number}", "title": "t", "state": state,
        "isDraft": False, "merged": state == "MERGED", "mergedAt": None, "mergeable": "MERGEABLE",
        "mergeStateStatus": "BLOCKED", "headRefOid": "a" * 40, "reviewDecision": None, "updatedAt": "x",
        "commits": {"nodes": [{"commit": {"statusCheckRollup": None if rollup is None else {
            "state": rollup,
            "contexts": {"totalCount": len(checks or []), "pageInfo": {"hasNextPage": False}, "nodes": checks or []},
        }}}]},
    }


def check_run(name: str, status: str = "COMPLETED", conclusion: str | None = "SUCCESS") -> dict:
    return {"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion,
            "detailsUrl": f"https://ci/{name}"}


class FakeGitHub:
    """Answers GraphQL and REST like api.github.com; records every request."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str, dict, bytes | None]] = []
        self.prs: dict[tuple[str, str, int], dict] = {}
        self.runs: dict[int, dict] = {}
        self.jobs: dict[int, list] = {}
        self.rest_remaining = 4000
        self.graphql_remaining = 4000

    def rate(self, resource: str, remaining: int) -> dict:
        return {"x-ratelimit-limit": "5000", "x-ratelimit-remaining": str(remaining),
                "x-ratelimit-reset": str(int(NOW + 1800)), "x-ratelimit-resource": resource}

    def __call__(self, method: str, url: str, headers: dict, body: bytes | None) -> tuple[int, dict, bytes]:
        self.calls.append((method, url, dict(headers), body))
        assert headers["Authorization"] == f"Bearer {TOKEN}"
        if url.endswith("/graphql"):
            doc = json.loads(body)
            v, data, errors = doc["variables"], {}, []
            self.graphql_remaining -= 1
            data["rateLimit"] = {"limit": 5000, "remaining": self.graphql_remaining, "resetAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(NOW + 1800)), "cost": 1}
            i = 0
            while f"o{i}" in v:
                node = self.prs.get((v[f"o{i}"], v[f"r{i}"], v[f"n{i}"]))
                data[f"p{i}"] = {"pullRequest": node}
                if node is None:
                    errors.append({"type": "NOT_FOUND", "path": [f"p{i}", "pullRequest"], "message": "Could not resolve"})
                i += 1
            out = {"data": data, **({"errors": errors} if errors else {})}
            return 200, self.rate("graphql", self.graphql_remaining), json.dumps(out).encode()
        path = url.split("api.github.com", 1)[1]
        parts = path.split("?")[0].split("/")
        run_id = int(parts[6])
        payload = {"jobs": self.jobs.get(run_id, [])} if parts[-1] == "jobs" else self.runs[run_id]
        etag = '"' + str(abs(hash(json.dumps(payload, sort_keys=True)))) + '"'
        if headers.get("If-None-Match") == etag:
            return 304, self.rate("core", self.rest_remaining), b""
        self.rest_remaining -= 1
        return 200, {**self.rate("core", self.rest_remaining), "etag": etag}, json.dumps(payload).encode()

    def rest_calls(self) -> list:
        return [c for c in self.calls if not c[1].endswith("/graphql")]

    def gql_calls(self) -> list:
        return [c for c in self.calls if c[1].endswith("/graphql")]


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.saved = (gg.BASE, gg.WAIT_POLL)
        gg.BASE = Path(self.tmp.name) / "gh"
        gg.WAIT_POLL = 0.05
        self.gh = FakeGitHub()
        self.daemon = gg.Daemon(transport=self.gh, token=lambda: TOKEN)

    def tearDown(self) -> None:
        gg.BASE, gg.WAIT_POLL = self.saved
        self.tmp.cleanup()

    def age_watch(self, key: str, seconds: float) -> None:
        path = gg.watch_dir() / gg.file_name(key)
        os.utime(path, (NOW - seconds, NOW - seconds))


class DaemonTest(Base):
    def test_run_etag_304_reuses_cached_body(self) -> None:
        self.gh.runs[7] = {"id": 7, "name": "CI", "status": "in_progress", "conclusion": None, "html_url": "u"}
        key = gg.parse_key("run", "O/R/7")
        gg.register(key)
        self.daemon.tick(NOW)
        first = gg.read_cache(key)
        self.assertEqual(first["data"]["status"], "in_progress")
        self.assertTrue(first["etag"])
        self.daemon.tick(NOW + gg.CYCLE)
        calls = self.gh.rest_calls()
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1][2]["If-None-Match"], first["etag"])
        second = gg.read_cache(key)
        self.assertEqual(second["data"], first["data"])
        self.assertEqual(second["etag"], first["etag"])
        self.assertEqual(self.gh.rest_remaining, 3999)  # the 304 cost nothing
        self.assertEqual(self.daemon.budget["rest"]["remaining"], 3999)
        # a change comes through as fresh data
        self.gh.runs[7] = {**self.gh.runs[7], "status": "completed", "conclusion": "success"}
        self.daemon.tick(NOW + 2 * gg.CYCLE)
        self.assertEqual(gg.read_cache(key)["data"]["conclusion"], "success")

    def test_jobs_only_when_asked(self) -> None:
        self.gh.runs[8] = {"id": 8, "status": "in_progress"}
        self.gh.jobs[8] = [{"id": 1, "name": "build", "status": "completed", "conclusion": "failure", "html_url": "j"}]
        key = gg.parse_key("run", "https://github.com/o/r/actions/runs/8/job/99")
        gg.register(key)
        self.daemon.tick(NOW)
        self.assertFalse(any("/jobs" in c[1] for c in self.gh.calls))
        gg.register(key, jobs=True)
        gg.register(key)  # another reader without --jobs does not turn it off
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(gg.read_cache(key)["jobs"][0]["name"], "build")

    def test_batched_graphql_query_and_parse(self) -> None:
        keys = [gg.parse_key("pr", "o/r#1"), gg.parse_key("pr", "https://github.com/O/R/pull/2"), gg.parse_key("pr", "o/r#3")]
        query, variables = gg.pr_query(keys)
        self.assertIn("p0: repository(owner: $o0, name: $r0)", query)
        self.assertIn("p2: repository(owner: $o2", query)
        self.assertIn("rateLimit", query)
        self.assertEqual(variables, {"o0": "o", "r0": "r", "n0": 1, "o1": "o", "r1": "r", "n1": 2,
                                     "o2": "o", "r2": "r", "n2": 3})
        status_ctx = {"__typename": "StatusContext", "context": "legacy", "state": "ERROR", "targetUrl": "s"}
        self.gh.prs[("o", "r", 1)] = pr_node(1, rollup="FAILURE", checks=[check_run("a"), check_run("b", conclusion="FAILURE"), status_ctx])
        self.gh.prs[("o", "r", 2)] = pr_node(2, state="MERGED", rollup="SUCCESS", checks=[check_run("a")])
        for key in keys:
            gg.register(key)
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.gql_calls()), 1)  # one query for all three
        one = gg.read_cache(keys[0])["data"]
        self.assertEqual([c["name"] for c in gg.failing(one)], ["b", "legacy"])
        self.assertEqual(one["checksTotal"], 3)
        self.assertEqual(gg.read_cache(keys[1])["data"]["state"], "MERGED")
        missing = gg.read_cache(keys[2])
        self.assertIsNone(missing["data"])
        self.assertIn("Could not resolve", missing["error"])
        self.assertEqual(self.daemon.budget["graphql"]["remaining"], 3999)

    def test_new_entry_fills_at_once_then_waits_for_the_cycle(self) -> None:
        self.gh.prs[("o", "r", 1)] = pr_node(1)
        self.gh.prs[("o", "r", 2)] = pr_node(2)
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        gg.register("pr:o/r#2")
        self.daemon.tick(NOW + gg.TICK)  # only the new PR is fetched
        self.assertEqual(len(self.gh.gql_calls()), 2)
        self.assertEqual(json.loads(self.gh.gql_calls()[1][3])["variables"], {"o0": "o", "r0": "r", "n0": 2})
        self.daemon.tick(NOW + 2 * gg.TICK)
        self.assertEqual(len(self.gh.gql_calls()), 2)
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(len(self.gh.gql_calls()), 3)

    def test_interest_expiry(self) -> None:
        self.gh.prs[("o", "r", 1)] = pr_node(1)
        self.gh.prs[("o", "r", 2)] = pr_node(2, state="MERGED", rollup="SUCCESS")
        self.gh.prs[("o", "r", 3)] = pr_node(3)
        for n in (1, 2, 3):
            gg.register(f"pr:o/r#{n}")
        self.daemon.tick(NOW)
        self.age_watch("pr:o/r#1", gg.IDLE_EXPIRY + 1)  # nobody read it for 15 minutes
        self.age_watch("pr:o/r#2", gg.TERMINAL_GRACE + 1)  # merged, last reader gone
        self.age_watch("pr:o/r#3", gg.TERMINAL_GRACE + 1)  # open: still within the idle window
        status = self.daemon.tick(NOW + 1)
        self.assertEqual(status["watching"], 1)
        left = [gg.read_json(p)["key"] for p in gg.watch_dir().glob("*.json")]
        self.assertEqual(left, ["pr:o/r#3"])

    def test_terminal_entries_are_not_repolled_every_cycle(self) -> None:
        self.gh.prs[("o", "r", 2)] = pr_node(2, state="MERGED", rollup="SUCCESS")
        gg.register("pr:o/r#2")
        self.daemon.tick(NOW)
        for i in range(1, 5):
            gg.register("pr:o/r#2")
            self.daemon.tick(NOW + i * gg.CYCLE)
        self.assertEqual(len(self.gh.gql_calls()), 1)

    def test_budget_floor_goes_lean_then_stops(self) -> None:
        self.gh.runs[9] = {"id": 9, "status": "in_progress"}
        key = "run:o/r/9"
        gg.register(key, jobs=True)
        self.gh.rest_remaining = gg.FLOORS["rest"][0]  # the run fetch leaves it just under the floor
        self.daemon.tick(NOW)  # first fill still happens, but without the job list
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.assertEqual(self.daemon.mode("rest"), "lean")
        self.assertNotIn("jobs", gg.read_cache(key))
        self.gh.runs[9] = {"id": 9, "status": "queued"}
        self.daemon.tick(NOW + gg.CYCLE)  # lean: one normal cycle is not enough
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.daemon.tick(NOW + gg.CYCLE * gg.LEAN_FACTOR)
        self.assertEqual(len(self.gh.rest_calls()), 2)
        self.daemon.budget["rest"]["remaining"] = gg.FLOORS["rest"][1] - 1  # below the reserve
        gg.register("run:o/r/10")
        self.daemon.tick(NOW + 8 * gg.CYCLE)  # before the reset
        self.assertEqual(len(self.gh.rest_calls()), 2)
        self.assertIsNone(gg.read_cache("run:o/r/10"))
        self.assertEqual(self.daemon.mode("rest"), "stop")
        self.daemon.budget["rest"]["reset"] = NOW  # the quota reset: back to normal
        self.daemon.now = NOW + 1
        self.assertEqual(self.daemon.mode("rest"), "normal")

    def test_graphql_reserve_and_rate_limit_backoff(self) -> None:
        self.daemon.budget["graphql"] = {"limit": 5000, "remaining": 10, "reset": NOW + 600}
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        self.assertEqual(self.gh.calls, [])

        def limited(method, url, headers, body):
            return 403, {"retry-after": "120", "x-ratelimit-remaining": "0", "x-ratelimit-resource": "core",
                         "x-ratelimit-limit": "5000", "x-ratelimit-reset": str(int(NOW + 900))}, b'{"message":"rate limited"}'

        self.daemon.transport = limited
        gg.register("run:o/r/5")
        self.daemon.tick(NOW)
        self.assertEqual(self.daemon.mode("rest"), "stop")
        self.assertIn("HTTP 403", gg.read_cache("run:o/r/5")["error"])

    def test_token_never_written(self) -> None:
        self.gh.runs[7] = {"id": 7, "status": "queued"}
        self.gh.prs[("o", "r", 1)] = pr_node(1)
        gg.register("run:o/r/7")
        gg.register("pr:o/r#1")
        gg.register("pr:o/r#404")
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.daemon.tick(NOW)
        for path in gg.BASE.rglob("*"):
            if path.is_file():
                self.assertNotIn(TOKEN, path.read_text(), path)
        self.assertNotIn(TOKEN, out.getvalue())

    def test_token_failure_is_reported_without_output(self) -> None:
        self.daemon.token_source = lambda: (_ for _ in ()).throw(gg.WatchError("gh auth token exited 1; is gh signed in?"))
        gg.register("pr:o/r#1")
        self.daemon.tick(NOW)
        self.assertIn("gh signed in", gg.read_cache("pr:o/r#1")["error"])

    def test_bad_keys_are_refused(self) -> None:
        for kind, text in (("pr", "o/r"), ("pr", "o/r#x"), ("run", "o/r"), ("pr", "o/r#1) { x }")):
            with self.assertRaises(gg.WatchError):
                gg.parse_key(kind, text)
        self.assertEqual(gg.parse_key("pr", "Manaflow-AI/cmux#14542"), "pr:manaflow-ai/cmux#14542")
        self.assertEqual(gg.parse_key("run", "o/r/actions/runs/12/attempts/2"), "run:o/r/12")


class ClientTest(Base):
    def setUp(self) -> None:
        super().setUp()
        gg.BASE.mkdir(parents=True)
        self.lock = open(gg.BASE / "daemon.lock", "a+")
        fcntl.flock(self.lock, fcntl.LOCK_EX)  # stands in for a running daemon

    def tearDown(self) -> None:
        self.lock.close()
        super().tearDown()

    def put(self, key: str, data: dict | None, **extra) -> None:
        gg.write_json(gg.cache_dir() / gg.file_name(key), {"key": key, "fetchedAt": time.time(),
                                                          "dataAt": time.time(), "data": data, "error": None, **extra})

    def wait(self, kind: str, target: str, until: str = "green", timeout: float = 0.3) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            code = gg.cmd_wait(kind, target, until, timeout, False, False)
        return code, out.getvalue()

    def test_wait_exit_codes(self) -> None:
        green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))
        self.put("pr:o/r#1", green)
        self.assertEqual(self.wait("pr", "o/r#1")[0], 0)

        red = gg.parse_pr(pr_node(2, rollup="PENDING", checks=[check_run("a", conclusion="FAILURE"),
                                                                check_run("b", "IN_PROGRESS", None)]))
        self.put("pr:o/r#2", red)
        code, text = self.wait("pr", "o/r#2")
        self.assertEqual(code, 1)  # green fails fast
        self.assertIn("failed: a  https://ci/a", text)
        self.assertEqual(self.wait("pr", "o/r#2", until="done")[0], 2)  # done waits for b

        closed = gg.parse_pr(pr_node(3, state="CLOSED", rollup="SUCCESS"))
        self.put("pr:o/r#3", closed)
        self.assertEqual(self.wait("pr", "o/r#3", until="merged")[0], 1)
        self.put("pr:o/r#4", gg.parse_pr(pr_node(4, state="MERGED", rollup=None)))
        self.assertEqual(self.wait("pr", "o/r#4", until="merged")[0], 0)

        self.put("run:o/r/5", {"status": "completed", "conclusion": "failure"})
        self.assertEqual(self.wait("run", "o/r/5")[0], 1)
        self.put("run:o/r/6", {"status": "completed", "conclusion": "success"})
        self.assertEqual(self.wait("run", "o/r/6")[0], 0)
        code, text = self.wait("run", "o/r/7", timeout=0.1)  # nothing cached yet
        self.assertEqual(code, 2)
        self.assertIn("result: timeout", text)
        # waiting registered interest for the daemon to pick up
        self.assertIsNotNone(gg.read_json(gg.watch_dir() / gg.file_name("run:o/r/7")))

    def test_done_uses_every_check(self) -> None:
        both = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[check_run("a"), check_run("b", conclusion="CANCELLED")]))
        self.assertEqual(gg.verdict("pr", both, "done"), 1)
        ok = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a"), check_run("b", conclusion="SKIPPED")]))
        self.assertEqual(gg.verdict("pr", ok, "done"), 0)
        self.assertIsNone(gg.verdict("pr", gg.parse_pr(pr_node(1, rollup=None)), "done"))

    def test_status_prints_cached_entry(self) -> None:
        self.put("pr:o/r#1", gg.parse_pr(pr_node(1, rollup="PENDING", checks=[check_run("a")])))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            self.assertEqual(gg.cmd_status("pr", "o/r#1", False, True, first_wait=0), 0)
        self.assertEqual(json.loads(out.getvalue())["data"]["checkState"], "PENDING")

    def test_daemon_down(self) -> None:
        self.lock.close()
        err = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(gg.cmd_wait("pr", "o/r#1", "green", 5, False, False), 3)
            self.assertEqual(gg.cmd_status("pr", "o/r#1", False, False), 3)
            self.assertEqual(gg.cmd_budget(False), 3)
        self.assertIn("daemon is not running", err.getvalue())
        self.assertFalse(gg.watch_dir().exists())  # no interest registered, nothing polled


if __name__ == "__main__":
    unittest.main()
