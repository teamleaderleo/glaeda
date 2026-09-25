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
import threading
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
        self.saved = (gg.BASE, gg.WAIT_POLL, gg.DOWN_GRACE)
        gg.BASE = Path(self.tmp.name) / "gh"
        gg.WAIT_POLL = 0.05
        gg.DOWN_GRACE = 0.3
        self.gh = FakeGitHub()
        self.daemon = gg.Daemon(transport=self.gh, token=lambda: TOKEN, controller=None)

    def tearDown(self) -> None:
        gg.BASE, gg.WAIT_POLL, gg.DOWN_GRACE = self.saved
        self.tmp.cleanup()

    def age_watch(self, key: str, seconds: float) -> None:
        path = gg.watch_dir() / gg.file_name(key)
        os.utime(path, (NOW - seconds, NOW - seconds))


class FakeController:
    """Stands in for the cmux build controller's /v1/github/runs/<id> endpoint."""

    def __init__(self) -> None:
        self.runs: dict[int, dict] = {}
        self.calls: list[int] = []
        self.down = False

    def run(self, run_id: int) -> dict | None:
        self.calls.append(run_id)
        if self.down:
            raise gg.WatchError("controller: URLError")
        return self.runs.get(run_id)


def ctl_run(run_id: int, status: str, conclusion: str = "", updated: str = "2026-09-25T10:00:00Z", repo: str = "o/r") -> dict:
    return {"id": run_id, "repo": repo, "name": "seed", "event": "workflow_dispatch", "head_sha": "a" * 40,
            "attempt": 1, "status": status, "conclusion": conclusion, "url": f"https://x/{run_id}", "updated_at": updated}


class ControllerTest(Base):
    def setUp(self) -> None:
        super().setUp()
        self.ctl = FakeController()
        self.daemon = gg.Daemon(transport=self.gh, token=lambda: TOKEN, controller=self.ctl)

    def test_tracked_run_costs_no_quota(self) -> None:
        self.ctl.runs[21] = ctl_run(21, "in_progress")
        key = gg.parse_key("run", "o/r/21")
        gg.register(key)
        for i in range(5):
            self.daemon.tick(NOW + i * gg.CYCLE)
        self.assertEqual(self.gh.rest_calls(), [])
        self.ctl.runs[21] = ctl_run(21, "completed", "failure", "2026-09-25T10:05:00Z")
        self.daemon.tick(NOW + 5 * gg.CYCLE + gg.TICK)
        entry = gg.read_cache(key)
        self.assertEqual((entry["source"], entry["data"]["conclusion"]), ("controller", "failure"))
        self.assertEqual(gg.verdict("run", entry["data"]), gg.EXIT_FAIL)
        self.assertEqual(self.gh.rest_calls(), [])

    def test_safety_net_reads_rest_every_15_minutes(self) -> None:
        self.ctl.runs[22] = ctl_run(22, "in_progress")
        self.gh.runs[22] = {"id": 22, "status": "in_progress", "updated_at": "2026-09-25T10:00:00Z"}
        gg.register("run:o/r/22")
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.REST_SAFETY - gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 0)
        self.daemon.tick(NOW + gg.REST_SAFETY + 1)
        self.assertEqual(len(self.gh.rest_calls()), 1)

    def test_lost_delivery_is_not_undone(self) -> None:
        self.ctl.runs[23] = ctl_run(23, "in_progress")
        self.gh.runs[23] = {"id": 23, "status": "completed", "conclusion": "success", "updated_at": "2026-09-25T10:09:00Z"}
        key = gg.parse_key("run", "o/r/23")
        gg.register(key)
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.REST_SAFETY + 1)
        self.daemon.tick(NOW + gg.REST_SAFETY + 1 + gg.TICK)
        self.assertEqual(gg.read_cache(key)["data"]["conclusion"], "success")

    def test_unknown_run_uses_rest(self) -> None:
        self.gh.runs[24] = {"id": 24, "status": "completed", "conclusion": "success"}
        gg.register("run:o/r/24")
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.assertEqual(gg.read_cache("run:o/r/24")["source"], "rest")

    def test_controller_down_backs_off(self) -> None:
        self.ctl.down = True
        self.gh.runs[25] = {"id": 25, "status": "in_progress"}
        gg.register("run:o/r/25")
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.TICK)
        self.assertEqual(len(self.ctl.calls), 1)
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.ctl.down = False
        self.daemon.tick(NOW + gg.CONTROLLER_RETRY + 1)
        self.assertEqual(len(self.ctl.calls), 2)

    def test_job_lists_and_other_repos_stay_on_rest(self) -> None:
        self.ctl.runs[26] = ctl_run(26, "in_progress", repo="other/repo")
        self.gh.runs[26] = {"id": 26, "status": "in_progress"}
        gg.register("run:o/r/26")
        self.gh.runs[27] = {"id": 27, "status": "in_progress"}
        self.ctl.runs[27] = ctl_run(27, "in_progress")
        gg.register("run:o/r/27", jobs=True)
        self.daemon.tick(NOW)
        self.assertEqual(self.ctl.calls, [26])
        self.assertEqual(gg.read_cache("run:o/r/26")["source"], "rest")
        self.assertEqual(gg.read_cache("run:o/r/27")["source"], "rest")

    def test_new_reader_of_a_finished_run_gets_one_rest_confirmation(self) -> None:
        self.ctl.runs[28] = ctl_run(28, "completed", "success")
        self.gh.runs[28] = {"id": 28, "status": "completed", "conclusion": "success", "updated_at": "2026-09-25T10:00:00Z"}
        key = "run:o/r/28"
        gg.register(key)
        self.age_watch(key, 200)
        self.daemon.tick(NOW - 100)
        self.assertEqual(self.gh.rest_calls(), [])
        stale = gg.read_cache(key)["dataAt"]
        self.daemon.tick(NOW - 98)
        self.assertEqual(gg.read_cache(key)["dataAt"], stale, "unchanged controller data keeps its time")
        self.age_watch(key, 0)  # a new reader
        self.daemon.tick(NOW)
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.assertEqual(gg.read_cache(key)["dataAt"], NOW)
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 1, "confirmed once, not every cycle")

    def test_rerun_before_its_webhook_is_not_answered_with_the_old_attempt(self) -> None:
        self.ctl.runs[29] = ctl_run(29, "completed", "failure")
        self.gh.runs[29] = {"id": 29, "status": "queued", "conclusion": None, "run_attempt": 2,
                            "updated_at": "2026-09-25T10:30:00Z"}
        key = "run:o/r/29"
        gg.register(key)
        self.age_watch(key, 200)
        self.daemon.tick(NOW - 100)
        self.age_watch(key, 0)
        self.daemon.tick(NOW)
        entry = gg.read_cache(key)
        self.assertEqual((entry["source"], entry["data"]["status"]), ("rest", "queued"))
        self.daemon.tick(NOW + gg.TICK)  # the controller has not heard of the rerun yet
        self.assertEqual(gg.read_cache(key)["data"]["status"], "queued")
        self.ctl.runs[29] = {**ctl_run(29, "completed", "success", "2026-09-25T10:40:00Z"), "attempt": 2}
        self.daemon.tick(NOW + 2 * gg.TICK)
        entry = gg.read_cache(key)
        self.assertEqual((entry["source"], entry["data"]["conclusion"], entry["data"]["run_attempt"]), ("controller", "success", 2))

    def test_controller_down_after_tracking_resumes_rest(self) -> None:
        self.ctl.runs[30] = ctl_run(30, "in_progress")
        self.gh.runs[30] = {"id": 30, "status": "in_progress"}
        gg.register("run:o/r/30")
        self.daemon.tick(NOW)
        self.assertEqual(self.gh.rest_calls(), [])
        self.ctl.down = True
        self.daemon.tick(NOW + gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 1, "no 15-minute blackout while the controller is down")

    def test_controller_config_needs_a_token(self) -> None:
        old = os.environ.get("CMUX_CI_TOKEN_FILE")
        os.environ["CMUX_CI_TOKEN_FILE"] = "/nonexistent/token"
        try:
            self.assertIsNone(gg.Controller.from_env())
        finally:
            os.environ.pop("CMUX_CI_TOKEN_FILE") if old is None else os.environ.__setitem__("CMUX_CI_TOKEN_FILE", old)


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
        self.assertEqual([c["name"] for c in gg.failing(one["checks"])], ["b", "legacy"])
        query_vars = json.loads(self.gh.gql_calls()[0][3])["query"]
        self.assertIn("isRequired(pullRequestNumber: $n2)", query_vars)
        self.assertEqual(one["checksTotal"], 3)
        self.assertEqual(gg.read_cache(keys[1])["data"]["state"], "MERGED")
        missing = gg.read_cache(keys[2])
        self.assertIsNone(missing["data"])
        self.assertIn("Could not resolve", missing["error"])
        self.assertTrue(missing["missing"])
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
            self.age_watch("pr:o/r#2", 1)  # the reader was already there before the last fetch
            self.daemon.tick(NOW + i * gg.CYCLE)
        self.assertEqual(len(self.gh.gql_calls()), 1)

    def test_rerun_of_a_completed_run_is_seen_on_the_next_cycle(self) -> None:
        self.gh.runs[3] = {"id": 3, "status": "completed", "conclusion": "failure"}
        gg.register("run:o/r/3")
        self.age_watch("run:o/r/3", 1)  # registered before the first fetch
        self.daemon.tick(NOW)
        self.daemon.tick(NOW + gg.CYCLE)  # nobody new is reading: a completed run is not re-read
        self.assertEqual(len(self.gh.rest_calls()), 1)
        self.gh.runs[3] = {"id": 3, "status": "queued", "conclusion": None}  # gh run rerun 3
        path = gg.watch_dir() / gg.file_name("run:o/r/3")
        gg.register("run:o/r/3")  # a new wait
        os.utime(path, (NOW + gg.CYCLE + 1,) * 2)
        self.daemon.tick(NOW + 2 * gg.CYCLE)
        self.assertEqual(len(self.gh.rest_calls()), 2)
        self.assertEqual(gg.read_cache("run:o/r/3")["data"]["status"], "queued")

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

    def test_failures_are_retried_once_per_cycle(self) -> None:
        def broken(method, url, headers, body):
            raise gg.WatchError("request failed: RemoteDisconnected")

        self.daemon.transport = broken
        gg.register("pr:o/r#1")
        gg.register("run:o/r/2")
        attempts = []
        self.daemon.transport = lambda *a: (attempts.append(a[1]), broken(*a))[1]
        for i in range(20):  # 40 s of ticks
            self.daemon.tick(NOW + i * gg.TICK)
        self.assertEqual(len(attempts), 2)
        self.assertIn("RemoteDisconnected", gg.read_cache("run:o/r/2")["error"])

        def explodes(method, url, headers, body):
            raise KeyError("boom")  # an unexpected bug still costs one attempt per cycle

        self.daemon.transport = lambda *a: (attempts.append(a[1]), explodes(*a))[1]
        for i in range(20):
            self.daemon.tick(NOW + gg.CYCLE + i * gg.TICK)
        self.assertEqual(len(attempts), 4)

    def test_redirects_and_foreign_hosts_are_refused(self) -> None:
        with self.assertRaises(gg.WatchError):
            gg.http("GET", "https://example.com/x", {"Authorization": "Bearer x"})
        handler = gg.NoRedirect()
        self.assertIsNone(handler.redirect_request(None, None, 301, "Moved", {}, "https://evil.example/"))

    def test_token_format_is_checked(self) -> None:
        self.assertTrue(gg.TOKEN_RE.fullmatch("gho_" + "a" * 36))
        self.assertTrue(gg.TOKEN_RE.fullmatch("github_pat_" + "A1_" * 20))
        self.assertFalse(gg.TOKEN_RE.fullmatch("To get started with GitHub CLI, please run: gh auth login"))

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

    def put(self, key: str, data: dict | None, at: float | None = None, **extra) -> None:
        at = time.time() if at is None else at
        gg.write_json(gg.cache_dir() / gg.file_name(key), {"key": key, "fetchedAt": at,
                                                          "dataAt": at if data else None, "data": data, "error": None, **extra})

    def wait(self, kind: str, target: str, until: str = "green", timeout: float = 0.5, fill: tuple | None = None,
             sha: str | None = None) -> tuple[int, str]:
        """Wait while a stand-in daemon writes `fill` (data, extra) shortly after the wait begins."""
        key = gg.parse_key(kind, target)
        timer = threading.Timer(0.1, lambda: self.put(key, fill[0], **fill[1])) if fill else None
        if timer:
            timer.start()
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            code = gg.cmd_wait(kind, target, until, timeout, False, False, sha)
        if timer:
            timer.join()
        return code, out.getvalue()

    def test_wait_exit_codes(self) -> None:
        green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))
        self.assertEqual(self.wait("pr", "o/r#1", fill=(green, {}))[0], 0)

        red = gg.parse_pr(pr_node(2, rollup="PENDING", checks=[check_run("a", conclusion="FAILURE"),
                                                                check_run("b", "IN_PROGRESS", None)]))
        code, text = self.wait("pr", "o/r#2", fill=(red, {}))
        self.assertEqual(code, 1)  # green fails fast
        self.assertIn("failed: a  https://ci/a", text)
        self.assertEqual(self.wait("pr", "o/r#2", until="done", fill=(red, {}))[0], 2)  # done waits for b

        closed = gg.parse_pr(pr_node(3, state="CLOSED", rollup="SUCCESS"))
        self.assertEqual(self.wait("pr", "o/r#3", until="merged", fill=(closed, {}))[0], 1)
        merged = gg.parse_pr(pr_node(4, state="MERGED", rollup=None))
        self.assertEqual(self.wait("pr", "o/r#4", until="merged", fill=(merged, {}))[0], 0)

        self.assertEqual(self.wait("run", "o/r/5", fill=({"status": "completed", "conclusion": "failure"}, {}))[0], 1)
        self.assertEqual(self.wait("run", "o/r/6", fill=({"status": "completed", "conclusion": "success"}, {}))[0], 0)
        code, text = self.wait("run", "o/r/7", timeout=0.1)  # nothing cached yet
        self.assertEqual(code, 2)
        self.assertIn("result: timeout", text)
        # waiting registered interest for the daemon to pick up
        self.assertIsNotNone(gg.read_json(gg.watch_dir() / gg.file_name("run:o/r/7")))

    def test_wait_ignores_cache_from_before_it_began(self) -> None:
        old_green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))
        self.put("pr:o/r#1", old_green, at=time.time() - 30)
        self.assertEqual(self.wait("pr", "o/r#1", timeout=0.3)[0], 2)

    def test_wait_sha_mismatch_is_pending(self) -> None:
        green = gg.parse_pr(pr_node(1, rollup="SUCCESS", checks=[check_run("a")]))  # head is aaaa...
        code, text = self.wait("pr", "o/r#1", fill=(green, {}), sha="b" * 40, timeout=0.4)
        self.assertEqual(code, 2)
        self.assertIn("GitHub still shows head aaaaaaaaaa", text)
        self.assertEqual(self.wait("pr", "o/r#1", fill=(green, {}), sha="aaaaaaa")[0], 0)

    def test_missing_pr_fails(self) -> None:
        code, text = self.wait("pr", "o/r#404", fill=(None, {"error": "Could not resolve", "missing": True}))
        self.assertEqual(code, 1)
        self.assertIn("Could not resolve", text)

    def test_rerun_after_cancel_counts_the_latest_attempt(self) -> None:
        old = {**check_run("Web complexity", conclusion="CANCELLED"), "databaseId": 1, "startedAt": "2026-09-25T10:00:00Z"}
        new = {**check_run("Web complexity"), "databaseId": 2, "startedAt": "2026-09-25T11:00:00Z"}
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[new, old]))
        self.assertEqual([c["conclusion"] for c in data["checks"]], ["SUCCESS"])
        self.assertEqual(gg.verdict("pr", data), 0)

    def test_queued_rerun_outranks_the_cancelled_attempt(self) -> None:
        old = {**check_run("e2e", conclusion="CANCELLED"), "databaseId": 1, "startedAt": "2026-09-25T10:00:00Z"}
        queued = {**check_run("e2e", "QUEUED", None), "databaseId": 2, "startedAt": None}
        data = gg.parse_pr(pr_node(1, rollup="PENDING", checks=[old, queued]))
        self.assertEqual([c["status"] for c in data["checks"]], ["QUEUED"])
        self.assertIsNone(gg.verdict("pr", data))

    def test_same_name_in_two_workflows_stays_separate(self) -> None:
        def in_workflow(check: dict, workflow: str, db: int) -> dict:
            return {**check, "databaseId": db, "checkSuite": {"workflowRun": {"workflow": {"name": workflow}}}}
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[
            in_workflow(check_run("build", conclusion="FAILURE"), "CI", 1),
            in_workflow(check_run("build"), "Release", 2)]))
        self.assertEqual(len(data["checks"]), 2)
        self.assertEqual(gg.verdict("pr", data), 1)
        self.assertIn("failed: CI / build", gg.summary("pr:o/r#1", {"data": data}))

    def test_finished_things_answer_at_once_from_an_older_fetch(self) -> None:
        self.put("pr:o/r#1", gg.parse_pr(pr_node(1, state="MERGED", rollup="SUCCESS")), at=time.time() - 300)
        self.assertEqual(self.wait("pr", "o/r#1", until="merged", timeout=0.3)[0], 0)
        # a completed run may have been rerun since: only data fetched after the wait began decides
        self.put("run:o/r/2", {"status": "completed", "conclusion": "failure"}, at=time.time() - 300)
        self.assertEqual(self.wait("run", "o/r/2", timeout=0.3)[0], 2)
        rerun = ({"status": "queued", "conclusion": None}, {})
        self.assertEqual(self.wait("run", "o/r/2", timeout=0.4, fill=rerun)[0], 2)
        passed = ({"status": "completed", "conclusion": "success"}, {})
        self.assertEqual(self.wait("run", "o/r/2", timeout=0.5, fill=passed)[0], 0)
        self.put("pr:o/r#4", gg.parse_pr(pr_node(4, state="CLOSED", rollup="SUCCESS")), at=time.time() - 300)
        self.assertEqual(self.wait("pr", "o/r#4", until="merged", timeout=0.3)[0], 2)  # may be reopened
        self.put("run:o/r/3", {"status": "in_progress"}, at=time.time() - 300)
        self.assertEqual(self.wait("run", "o/r/3", timeout=0.3)[0], 2)  # live and stale: not trusted

    def test_only_not_found_fails_a_missing_pr(self) -> None:
        code, _ = self.wait("pr", "o/r#5", fill=(None, {"error": "graphql: HTTP 502", "missing": False}), timeout=0.4)
        self.assertEqual(code, 2)  # a transport error or 5xx stays pending
        code, _ = self.wait("pr", "o/r#6", fill=(None, {"error": "gh auth token exited 1; is gh signed in?"}), timeout=0.4)
        self.assertEqual(code, 2)

    def test_only_required_checks_decide_when_marked(self) -> None:
        req = {**check_run("build"), "isRequired": True}
        optional = {**check_run("lint", conclusion="FAILURE"), "isRequired": False}
        data = gg.parse_pr(pr_node(1, rollup="FAILURE", checks=[req, optional]))
        self.assertEqual(gg.verdict("pr", data, "green"), 0)
        self.assertEqual(gg.verdict("pr", data, "done"), 0)
        self.assertIn("lint (not required)", gg.summary("pr:o/r#1", {"data": data}))
        many = {**data, "checksTruncated": True, "checkState": "PENDING"}
        self.assertIsNone(gg.verdict("pr", many))  # over 100 checks: the rollup decides
        self.assertIn("none reported yet", gg.summary("pr:o/r#2", {"data": gg.parse_pr(pr_node(2, rollup=None))}))

    def test_daemon_down_waits_out_a_reexec(self) -> None:
        self.lock.close()
        holder = open(gg.BASE / "daemon.lock", "a+")
        relock = threading.Timer(0.1, lambda: fcntl.flock(holder, fcntl.LOCK_EX))
        relock.start()
        self.assertTrue(gg.daemon_running(grace=0.5))
        relock.join()
        holder.close()

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
