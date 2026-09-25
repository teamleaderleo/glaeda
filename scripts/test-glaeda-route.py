#!/usr/bin/env python3
"""Tests for scripts/glaeda-route and scripts/glaeda_route_probe.sh. No network, no SSH, no GitHub."""
from __future__ import annotations

import base64
import contextlib
import fcntl
import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "glaeda-route"
PROBE = Path(__file__).resolve().parent / "glaeda_route_probe.sh"
loader = importlib.machinery.SourceFileLoader("glaeda_route", str(SCRIPT))
spec = importlib.util.spec_from_loader("glaeda_route", loader)
gr = importlib.util.module_from_spec(spec)
sys.modules["glaeda_route"] = gr
loader.exec_module(gr)

NOW = 1_800_000_000.0
STD = "glaeda-std-xcode-26.6"
LIGHT = "glaeda-light-xcode-26.6"
REPO = "manaflow-ai/cmux"
SECRET = "ghs_supersecretvalue123"
XCODE = {"apps": [{"path": "/Applications/Xcode_26.6.app", "version": "26.6", "build": "17F113"}]}


def manifest(**extra_hosts):
    hosts = {
        "m1": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4pro-48",
               "hostname": "m1-Mac-mini"},
        "m2": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4pro-48",
               "lan_ip": "172.20.21.9"},
        "l1": {"class": "light", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4-16"},
        "cache": {"class": "std", "availability": "dedicated", "roles": ["cache-host"], "hardware": "m4pro-48"},
        "opp": {"class": "std", "availability": "opportunistic", "roles": ["ci-runner"], "hardware": "m4pro-48"},
        **extra_hosts,
    }
    return {"ssh_user": "cmux", "defaults": {"xcode": XCODE}, "hosts": hosts}


def probe_text(units=("free",) * 4, dd="free", gui="free", lock="free", eligible=None, done=True, extra=""):
    lines = [f"units_total\t{len(units)}"] + [f"unit\t{i}|{s}" for i, s in enumerate(units)]
    lines += [f"token\tpersistent-dd|{dd}", f"token\tgui|{gui}", f"host_lock_exclusive\t{lock}"]
    if eligible is not None:
        lines.append(f"eligible\t{'yes|' if eligible else 'no|node not eligible (drift)'}")
    if extra:
        lines.append(extra)
    if done:
        lines.append("probe\tdone")
    return "\n".join(lines) + "\n"


def runner(name, label, status="online", busy=False):
    return {"name": name, "status": status, "busy": busy,
            "labels": [{"name": n} for n in ("self-hosted", "macOS", "glaeda-mini", label)]}


def runners():
    return {"runners": [runner("m1-glaeda", STD), runner("m1-glaeda-1", STD, busy=True),
                        runner("m2-glaeda", STD), runner("m2-glaeda-1", STD, status="offline"),
                        runner("l1-glaeda", LIGHT, status="offline"),
                        {"name": "tart-x", "status": "online", "busy": False, "labels": [{"name": "tart-canary"}]}]}


ELIGIBLE = {"m1": {"eligible": True, "at": NOW}, "m2": {"eligible": True, "at": NOW}, "l1": {"eligible": True, "at": NOW}}


def state(probes=None, eligibility=None, runner_list=None, members=None):
    members = members or gr.fleet_members(manifest())
    probes = probes if probes is not None else {m: gr.parse_probe(probe_text()) for m in members}
    return gr.build_state(members, runner_list or runners(), probes, ELIGIBLE if eligibility is None else eligibility,
                          repo=REPO, now=NOW)


class FakeGh:
    """A subprocess.run stand-in for gh: records argv and stdin, answers per subcommand."""

    def __init__(self, runners_rows=None, fail=None):
        self.calls, self.fail = [], fail
        self.rows = runners_rows if runners_rows is not None else runners()["runners"]

    def __call__(self, argv, **kw):
        self.calls.append((argv, kw.get("input")))
        if self.fail:
            return subprocess.CompletedProcess(argv, 1, "", self.fail)
        if argv[1] == "api":
            lines = [json.dumps({"name": r["name"], "status": r["status"], "busy": r["busy"],
                                 "labels": [l["name"] for l in r["labels"]]}) for r in self.rows]
            return subprocess.CompletedProcess(argv, 0, "\n".join(lines) + "\n", "")
        if argv[1:3] == ["variable", "set"]:
            return subprocess.CompletedProcess(argv, 0, "", "")
        raise AssertionError(argv)


def run_main(argv, run=None, probe=None):
    out, err = io.StringIO(), io.StringIO()
    kwargs = {"probe": probe} if probe else {}
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = gr.main(argv, run=run or FakeGh(), now=lambda: NOW, **kwargs)
    return code, out.getvalue(), err.getvalue()


class MemberTests(unittest.TestCase):
    def test_runner_members_with_pool_units_and_address(self):
        members = gr.fleet_members(manifest())
        self.assertEqual(sorted(members), ["l1", "m1", "m2"])  # no cache host, no opportunistic member
        self.assertEqual((members["m1"]["pool"], members["m1"]["units"]), (STD, 4))
        self.assertEqual((members["l1"]["pool"], members["l1"]["units"]), (LIGHT, 2))
        self.assertEqual(members["m1"]["lan"], "m1-Mac-mini.local")
        self.assertEqual(members["m2"]["lan"], "172.20.21.9")
        self.assertEqual(members["l1"]["lan"], "l1")
        self.assertEqual((members["m1"]["fleet_class"], members["m1"]["xcode_app"]),
                         ("m4pro-48", "/Applications/Xcode_26.6.app"))

    def test_member_with_two_xcodes_counts_in_the_first_pool_only(self):
        two = {"class": "std", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4pro-48",
               "overrides": {"xcode": {"apps": [{"path": "/Applications/Xcode_26.4.app", "version": "26.4"},
                                                {"path": "/Applications/Xcode_26.6.app", "version": "26.6"}]}}}
        members = gr.fleet_members(manifest(two=two))
        self.assertEqual(members["two"]["pool"], STD)

    def test_runner_instances_map_to_their_member(self):
        self.assertEqual(gr.member_of("cmux15-glaeda"), "cmux15")
        self.assertEqual(gr.member_of("cmux15-glaeda-3"), "cmux15")
        self.assertIsNone(gr.member_of("tart-cmux-aws-1"))


class CapacityTests(unittest.TestCase):
    member = {"units": 4}

    def cap(self, text, eligible=True, online=1, now=NOW):
        return gr.member_capacity(self.member, gr.parse_probe(text), eligible, online, now)

    def test_free_units_and_tokens(self):
        self.assertEqual(self.cap(probe_text()), {"units": 4, "compile": 1, "gui": 1, "why": ""})
        got = self.cap(probe_text(units=("held", "held", "held", "free"), dd="free", gui="free"))
        self.assertEqual((got["units"], got["compile"], got["gui"]), (1, 0, 1))  # a compile needs 2 units
        got = self.cap(probe_text(dd="held", gui="held"))
        self.assertEqual((got["units"], got["compile"], got["gui"]), (4, 0, 0))
        got = self.cap(probe_text(units=("unknown",) * 4))
        self.assertEqual(got["units"], 0)

    def test_nothing_is_offered_unless_everything_agrees(self):
        cases = {
            "unreachable": (probe_text(done=False), True, 1),
            "not eligible": (probe_text(), False, 1),
            "not eligible (never checked)": (probe_text(), None, 1),
            "fleet build": (probe_text(lock="held"), True, 1),
            "no runner online": (probe_text(), True, 0),
        }
        for why, (text, eligible, online) in cases.items():
            got = self.cap(text, eligible, online)
            self.assertEqual((got["units"], got["compile"], got["gui"], got["why"]), (0, 0, 0, why.split(" (")[0]), why)
        self.assertEqual(self.cap(probe_text(lock="unknown"))["why"], "fleet build")

    def test_reservations(self):
        def marker(until):
            raw = json.dumps({"schema": "glaeda-reservation/v1", "owner": "a", "purpose": "b", "since": 1, "until": until})
            return "reservation_raw\t" + base64.b64encode(raw.encode()).decode()

        self.assertEqual(self.cap(probe_text(extra=marker(int(NOW) + 60)))["why"], "reserved")
        self.assertEqual(self.cap(probe_text(extra=marker(int(NOW) - 60)))["units"], 4)
        self.assertEqual(self.cap(probe_text(extra="reservation\tinvalid"))["why"], "reserved")
        self.assertEqual(self.cap(probe_text(extra="reservation_raw\tbm90IGpzb24="))["why"], "reserved")


class StateTests(unittest.TestCase):
    def test_counts(self):
        doc = state()
        self.assertEqual(doc["schema"], gr.STATE_SCHEMA)
        self.assertEqual(doc["order"], [STD, LIGHT])
        std = doc["pools"][STD]
        self.assertEqual((std["declared"], std["eligible"], std["runners"], std["online"], std["busy"]), (2, 2, 4, 3, 1))
        self.assertEqual(std["free"], {"units": 8, "compile": 2, "gui": 2})
        self.assertEqual(doc["pools"][LIGHT]["free"], {"units": 0, "compile": 0, "gui": 0})  # its runner is offline
        self.assertTrue(gr.validate_state(doc, now=NOW, repo=REPO)[0])

    def test_no_host_names_leave_the_document(self):
        text = json.dumps(state())
        for name in ('"m1', '"m2', '"l1', "tart", "172.20", "Mac-mini"):
            self.assertNotIn(name, text)

    def test_unknown_eligibility_offers_nothing(self):
        self.assertEqual(state(eligibility={})["pools"][STD]["free"]["units"], 0)


class ValidateTests(unittest.TestCase):
    def check(self, doc, **kw):
        return gr.validate_state(doc, now=kw.pop("now", NOW), repo=kw.pop("repo", REPO), **kw)

    def test_fresh_is_usable_and_ages_out_at_90_seconds(self):
        doc = state()
        self.assertTrue(self.check(doc)[0])
        self.assertTrue(self.check(doc, now=NOW + 89)[0])
        self.assertFalse(self.check(doc, now=NOW + 91)[0])

    def test_rejections(self):
        doc = state()
        self.assertFalse(self.check(None)[0])
        self.assertFalse(self.check(doc, repo="x/y")[0])
        self.assertFalse(self.check(doc, now=NOW - 120)[0])
        for mutate in (
            lambda d: d.update(schema="glaeda-pool-state/v1"),
            lambda d: d.update(generatedAt="2027-01-15"),
            lambda d: d.pop("observedAt"),
            lambda d: d.update(order=[STD]),
            lambda d: d.update(order=[STD, 3]),
            lambda d: d["pools"][STD]["free"].update(units=-1),
            lambda d: d["pools"][STD]["free"].update(gui=True),
            lambda d: d["pools"][STD].pop("free"),
            lambda d: d["pools"][STD].update(busy=99),
        ):
            bad = json.loads(json.dumps(doc))
            mutate(bad)
            self.assertFalse(self.check(bad)[0], bad)

    def test_check_command(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "state.json"
            path.write_text(json.dumps(state()))
            self.assertEqual(run_main(["check", "--state", str(path), "--repo", REPO])[0], 0)
            path.write_text(json.dumps({**state(), "observedAt": gr.utc(NOW - 3600)}))
            code, out, _ = run_main(["check", "--state", str(path)])
            self.assertEqual(code, 1)
            self.assertIn("stale", out)


class ObserveTests(unittest.TestCase):
    def test_every_never_checked_member_then_the_oldest_stale_few(self):
        members = {f"m{i}": {} for i in range(10)}
        self.assertEqual(len(gr.due_checks(members, {}, NOW)), 10)
        cache = {f"m{i}": {"eligible": True, "at": NOW - 1000 - i} for i in range(10)}
        self.assertEqual(gr.due_checks(members, cache, NOW), ["m9", "m8", "m7", "m6"])
        fresh = {m: {"eligible": True, "at": NOW} for m in members}
        self.assertEqual(gr.due_checks(members, fresh, NOW), [])

    def test_probes_run_in_parallel_and_only_due_members_are_checked(self):
        members = gr.fleet_members(manifest())
        calls = []

        def fake(user, address, units, check):
            calls.append((user, address, units, check))
            return gr.parse_probe(probe_text(eligible=True if check else None))

        cache = {"m1": {"eligible": False, "reason": "x", "at": NOW}}
        got = gr.observe_fleet(manifest(), members, cache, now=NOW, address="lan", probe=fake)
        self.assertEqual(sorted(got), ["l1", "m1", "m2"])
        checked = {address: check for _, address, _, check in calls}
        self.assertIsNone(checked["m1-Mac-mini.local"])
        self.assertEqual(checked["172.20.21.9"], ("m4pro-48", "/Applications/Xcode_26.6.app"))
        self.assertEqual(cache["m2"]["eligible"], True)
        self.assertEqual(cache["m1"]["eligible"], False)  # fresh, not re-checked
        self.assertTrue(all(user == "cmux" for user, *_ in calls))

    def test_a_failed_check_keeps_the_old_verdict_for_a_while_then_forgets_it(self):
        members = {"m1": gr.fleet_members(manifest())["m1"]}

        def dead(*args):
            return {"units": {}, "tokens": {}, "complete": False}

        cache = {"m1": {"eligible": True, "at": NOW - gr.ELIGIBILITY_TTL}}
        gr.observe_fleet(manifest(), members, cache, now=NOW, address="name", probe=dead)
        self.assertTrue(cache["m1"]["eligible"])
        cache = {"m1": {"eligible": True, "at": NOW - 2 * gr.ELIGIBILITY_TTL}}
        gr.observe_fleet(manifest(), members, cache, now=NOW, address="name", probe=dead)
        self.assertIsNone(cache["m1"]["eligible"])

    def test_probe_member_ssh_argv_and_failures(self):
        seen = {}

        def ok(argv, **kw):
            seen["argv"], seen["kw"] = argv, kw
            return subprocess.CompletedProcess(argv, 0, probe_text().encode(), b"")

        got = gr.probe_member("cmux", "m1.local", 4, ("m4pro-48", "/Applications/Xcode_26.6.app"), run=ok)
        self.assertTrue(got["complete"])
        self.assertEqual(seen["argv"][:7], [gr.SSH, "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-l", "cmux"])
        self.assertEqual(seen["argv"][-4:], ["4", "1", "m4pro-48", "/Applications/Xcode_26.6.app"])
        self.assertEqual(seen["kw"]["input"], PROBE.read_bytes())
        self.assertEqual(seen["kw"]["timeout"], gr.CHECK_TIMEOUT)

        def slow(argv, **kw):
            raise subprocess.TimeoutExpired(argv, kw["timeout"])

        self.assertFalse(gr.probe_member("cmux", "m1", 4, None, run=slow)["complete"])
        fail = lambda argv, **kw: subprocess.CompletedProcess(argv, 255, b"", b"denied")  # noqa: E731
        self.assertFalse(gr.probe_member("cmux", "m1", 4, None, run=fail)["complete"])


class ProbeScriptTests(unittest.TestCase):
    """The real probe, run locally against a temporary fleet directory."""

    def run_probe(self, fleet, *args, home=None):
        env = {"GLAEDA_FLEET_DIR": str(fleet), "HOME": str(home or fleet), "PATH": "/usr/bin:/bin"}
        out = subprocess.run(["bash", str(PROBE), *args], capture_output=True, text=True, env=env, timeout=60, check=True)
        return gr.parse_probe(out.stdout)

    def test_reads_locks_without_creating_anything(self):
        with tempfile.TemporaryDirectory() as tmp:
            fleet = Path(tmp)
            (fleet / "capacity").mkdir()
            for name in ("unit-0", "unit-1", "gui.token", "persistent-dd.token"):
                (fleet / "capacity" / name).touch()
            (fleet / "host.lock").touch()
            holds = []
            for name in ("capacity/unit-1", "capacity/gui.token"):
                fd = os.open(fleet / name, os.O_RDONLY)
                fcntl.flock(fd, fcntl.LOCK_EX)
                holds.append(fd)
            shared = os.open(fleet / "host.lock", os.O_RDONLY)
            fcntl.flock(shared, fcntl.LOCK_SH)  # a PR job's share is not a fleet build
            before = sorted(p.name for p in fleet.rglob("*"))
            try:
                got = self.run_probe(fleet, "4")
            finally:
                for fd in [*holds, shared]:
                    os.close(fd)
            self.assertTrue(got["complete"])
            self.assertEqual(got["units"], {0: "free", 1: "held", 2: "free", 3: "free"})
            self.assertEqual(got["tokens"], {"persistent-dd": "free", "gui": "held"})
            self.assertEqual(got["host_lock_exclusive"], "free")
            self.assertEqual(sorted(p.name for p in fleet.rglob("*")), before)
            self.assertNotIn("eligible", got)

    def test_fleet_build_holds_the_host_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            fleet = Path(tmp)
            (fleet / "host.lock").touch()
            fd = os.open(fleet / "host.lock", os.O_RDONLY)
            fcntl.flock(fd, fcntl.LOCK_EX)
            try:
                got = self.run_probe(fleet, "2")
            finally:
                os.close(fd)
            self.assertEqual(got["host_lock_exclusive"], "held")

    def test_check_without_a_runner_hook_is_not_eligible(self):
        with tempfile.TemporaryDirectory() as tmp:
            got = self.run_probe(Path(tmp), "2", "1", "m4pro-48", "/Applications/Xcode_26.6.app", home=Path(tmp))
            self.assertIs(got["eligible"], False)
            self.assertTrue(got["complete"])

    def test_reservation_is_shipped_raw(self):
        with tempfile.TemporaryDirectory() as tmp:
            fleet = Path(tmp)
            (fleet / "reservation.json").write_text('{"x": 1}')
            got = self.run_probe(fleet, "0")
            self.assertEqual(base64.b64decode(got["reservation_raw"]), b'{"x": 1}')


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        (self.dir / "manifest.json").write_text(json.dumps(manifest()))
        (self.dir / "probes.json").write_text(json.dumps({m: probe_text(eligible=True) for m in ("m1", "m2", "l1")}))
        self.gh = self.dir / "gh"
        self.gh.write_text("#!/bin/sh\n")
        os.chmod(self.gh, 0o755)

    def tearDown(self):
        self.tmp.cleanup()

    def args(self, *extra):
        return ["publish", "--manifest", str(self.dir / "manifest.json"), "--probes", str(self.dir / "probes.json"),
                "--cache", str(self.dir / "cache.json"), "--gh", str(self.gh), *extra]

    def test_dry_run_only_reads(self):
        gh = FakeGh()
        code, out, _ = run_main(self.args(), run=gh)
        self.assertEqual(code, 0)
        self.assertIn("would write GLAEDA_POOL_STATE", out)
        self.assertEqual([argv[1] for argv, _ in gh.calls], ["api"])
        self.assertEqual(gh.calls[0][0][:3], [str(self.gh), "api", "--paginate"])

    def test_publish_sets_the_variable_from_stdin(self):
        gh = FakeGh()
        code, out, err = run_main(self.args("--yes"), run=gh)
        self.assertEqual(code, 0, err)
        argv, stdin = gh.calls[-1]
        self.assertEqual(argv[1:], ["variable", "set", "GLAEDA_POOL_STATE", "--repo", REPO])
        doc = json.loads(stdin)
        self.assertTrue(gr.validate_state(doc, now=NOW, repo=REPO)[0])
        self.assertEqual(doc["pools"][STD]["free"]["units"], 8)
        self.assertEqual(doc["pools"][STD]["busy"], 1)
        self.assertIn("wrote GLAEDA_POOL_STATE", out)

    def test_gh_failure_exits_2_with_its_last_line(self):
        code, _, err = run_main(self.args("--yes"), run=FakeGh(fail="HTTP 403: Must have admin rights\n"))
        self.assertEqual(code, 2)
        self.assertIn("admin rights", err)

    def test_missing_gh_is_named(self):
        with unittest.mock.patch.object(gr.shutil, "which", return_value=None), \
                unittest.mock.patch.object(gr.os.path, "isfile", return_value=False):
            with self.assertRaises(gr.Failure):
                gr.find_gh(None)


if __name__ == "__main__":
    unittest.main()
