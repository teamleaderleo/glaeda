#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-cmux-runner and scripts/glaeda-cmux-runner-hook.

Runs on Linux CI. gh, curl, launchctl and the runner's config.sh are fakes that log every argv
and environment flag to a JSON-lines file, so the tests can prove the registration token never
reaches an argv, a file, or the output.
"""

from __future__ import annotations

import contextlib
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import plistlib
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
HOOK = ROOT / "scripts" / "glaeda-cmux-runner-hook"


def load(name: str, path: Path):
    loader = importlib.machinery.SourceFileLoader(name, os.fspath(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    loader.exec_module(module)
    return module


cr = load("glaeda_cmux_runner", ROOT / "scripts" / "glaeda-cmux-runner")
cr.DARWIN_REQUIRED = False
cr.ONLINE_WAIT_S = 0
hook = load("glaeda_cmux_runner_hook", HOOK)
setup = load("glaeda_mini_setup_for_runner", ROOT / "scripts" / "glaeda-mini-setup")
setup.DARWIN_REQUIRED = False

REG_TOKEN = "AREGTOKENSECRET0123456789"
REMOVE_TOKEN = "AREMOVETOKENSECRET9876543"
VERSION = "2.399.0"

FAKE_LOG_HEADER = r'''#!/usr/bin/env python3
import json, os, sys
state = os.environ["FAKE_STATE"]
def log(tool, argv):
    with open(os.path.join(state, "log.jsonl"), "a") as f:
        f.write(json.dumps({"tool": tool, "argv": argv,
                            "envToken": os.environ.get("ACTIONS_RUNNER_INPUT_TOKEN")}) + "\n")
def runners():
    try:
        return json.load(open(os.path.join(state, "runners.json")))
    except OSError:
        return {"runners": []}
def save(doc):
    json.dump(doc, open(os.path.join(state, "runners.json"), "w"))
'''

FAKE_GH = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("gh", argv)
if argv[:2] == ["auth", "status"]:
    sys.exit(0)
if argv[0] != "api":
    sys.exit(2)
if "-X" in argv:
    i = argv.index("-X"); method, endpoint = argv[i + 1], argv[i + 2]
else:
    method, endpoint = "GET", argv[1]
if endpoint.startswith("repos/actions/runner/releases"):
    sys.stdout.write(open(os.path.join(state, "release.json")).read())
elif endpoint.endswith("/registration-token") and method == "POST":
    print(os.environ["FAKE_REG_TOKEN"])
elif endpoint.endswith("/remove-token") and method == "POST":
    if os.path.exists(os.path.join(state, "fail-remove-token")):
        sys.exit(1)
    print(os.environ["FAKE_REMOVE_TOKEN"])
elif "/actions/runners" in endpoint and method == "GET":
    print(json.dumps(runners()))
elif "/actions/runners/" in endpoint and method == "DELETE":
    if os.path.exists(os.path.join(state, "fail-delete")):
        sys.exit(1)
    rid = int(endpoint.rsplit("/", 1)[1])
    save({"runners": [r for r in runners()["runners"] if r["id"] != rid]})
else:
    sys.exit(3)
'''

FAKE_CURL = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("curl", argv)
if "-o" not in argv:  # the gh-free path reads the public release API
    sys.stdout.write(open(os.path.join(state, "release.json")).read()); sys.exit(0)
dest = argv[argv.index("-o") + 1]
open(dest, "wb").write(open(os.path.join(state, "runner.tar.gz"), "rb").read())
'''

FAKE_LAUNCHCTL = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("launchctl", argv)
sys.exit(1 if argv[0] == "print" else 0)
'''

FAKE_CONFIG = FAKE_LOG_HEADER + r'''
argv = sys.argv[1:]
log("config.sh", argv)
here = os.path.dirname(os.path.abspath(__file__))
token = os.environ.get("ACTIONS_RUNNER_INPUT_TOKEN")
if argv[:1] == ["remove"]:
    if token != os.environ["FAKE_REMOVE_TOKEN"] or os.path.exists(os.path.join(state, "fail-config-remove")):
        print("remove failed"); sys.exit(1)
    name = json.load(open(os.path.join(here, ".runner")))["agentName"]
    save({"runners": [r for r in runners()["runners"] if r["name"] != name]})
    os.remove(os.path.join(here, ".runner"))
    sys.exit(0)
if token != os.environ["FAKE_REG_TOKEN"]:
    print("bad token " + str(token)); sys.exit(1)
name = argv[argv.index("--name") + 1]
labels = ["self-hosted", "macOS", "ARM64"] + argv[argv.index("--labels") + 1].split(",")
doc = runners()
if "--replace" in argv:
    doc["runners"] = [r for r in doc["runners"] if r["name"] != name]
elif any(r["name"] == name for r in doc["runners"]):
    print("A runner exists with the same name"); sys.exit(1)
count_path = os.path.join(state, "registrations")
count = int(open(count_path).read()) if os.path.exists(count_path) else 0
open(count_path, "w").write(str(count + 1))
rid = 4242 + count
json.dump({"agentId": rid, "agentName": name, "gitHubUrl": argv[argv.index("--url") + 1]},
          open(os.path.join(here, ".runner"), "w"))
for extra in (".credentials", ".credentials_rsaparams"):
    open(os.path.join(here, extra), "w").write("{}")
doc["runners"].append({"id": rid, "name": name, "status": "online", "busy": False,
                       "labels": [{"name": l} for l in labels]})
save(doc)
print("registered with token " + token)  # a leaky runner must still not leak it through us
'''


def make_executable(path: Path, text: str) -> Path:
    path.write_text(text)
    path.chmod(0o755)
    return path


def event(tmp: Path, name: str, payload: dict) -> Path:
    path = tmp / f"{name}.json"
    path.write_text(json.dumps(payload))
    return path


CMUX = {"full_name": "manaflow-ai/cmux", "fork": False}
FORK = {"full_name": "someone/cmux", "fork": True}
SAMPLE_EVENTS = {
    # name: (GITHUB_EVENT_NAME, payload, admitted)
    "same-repo-pr": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": CMUX}, "base": {"repo": CMUX}}}, True),
    "push": ("push", {"repository": CMUX, "ref": "refs/heads/main"}, True),
    "workflow-dispatch": ("workflow_dispatch", {"repository": CMUX, "inputs": {}}, True),
    "merge-group": ("merge_group", {"repository": CMUX, "merge_group": {"head_ref": "gh-readonly-queue/main/x"}}, True),
    "fork-pr": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": FORK}, "base": {"repo": CMUX}}}, False),
    "cross-repo-pr-not-flagged-fork": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": {"full_name": "other/cmux", "fork": False}}, "base": {"repo": CMUX}}}, False),
    "deleted-fork-pr": ("pull_request", {"repository": CMUX, "pull_request": {
        "head": {"repo": None}, "base": {"repo": CMUX}}}, False),
    "pull-request-target-same-repo": ("pull_request_target", {"repository": CMUX, "pull_request": {
        "head": {"repo": CMUX}, "base": {"repo": CMUX}}}, False),
    "fork-pr-review": ("pull_request_review", {"repository": CMUX, "pull_request": {
        "head": {"repo": FORK}, "base": {"repo": CMUX}}}, False),
    "pr-event-without-payload": ("pull_request", {"repository": CMUX}, False),
    "workflow-run-from-fork": ("workflow_run", {"repository": CMUX, "workflow_run": {
        "head_repository": FORK, "event": "pull_request"}}, False),
    "workflow-run-same-repo": ("workflow_run", {"repository": CMUX, "workflow_run": {
        "head_repository": CMUX, "event": "push"}}, True),
    "other-repository": ("push", {"repository": {"full_name": "evil/cmux"}}, False),
    "issue-comment-on-pr": ("issue_comment", {"repository": CMUX, "issue": {"pull_request": {"url": "x"}}}, False),
    "check-run": ("check_run", {"repository": CMUX, "check_run": {}}, False),
    "schedule": ("schedule", {"repository": CMUX}, True),
}


class HookTest(unittest.TestCase):
    """Runs the hook script itself, as the runner would, against sample event files."""

    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def run_hook(self, phase: str, event_name: str | None, event_path: Path | None,
                 *extra: str, repo: str | None = None, env: dict | None = None) -> subprocess.CompletedProcess:
        environ = {"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.dir),
                   "GLAEDA_FLEET_DIR": os.fspath(self.dir / "fleet")}
        if event_name is not None:
            environ["GITHUB_EVENT_NAME"] = event_name
        if event_path is not None:
            environ["GITHUB_EVENT_PATH"] = os.fspath(event_path)
        if repo:
            environ["GITHUB_REPOSITORY"] = repo
        environ.update(env or {})
        return subprocess.run([sys.executable, os.fspath(HOOK), phase, *extra], env=environ,
                              capture_output=True, text=True, timeout=30, check=False)

    def test_sample_events(self) -> None:
        for name, (event_name, payload, admitted) in SAMPLE_EVENTS.items():
            with self.subTest(name):
                path = event(self.dir, name, payload)
                repo = (payload.get("repository") or {}).get("full_name")
                result = self.run_hook("job-started", event_name, path, "--allowed-repo", "manaflow-ai/cmux",
                                       "--no-disk", repo=repo)
                self.assertEqual(result.returncode == 0, admitted, result.stdout + result.stderr)
                self.assertIn("admitted" if admitted else "refused", result.stdout)

    def test_fails_closed_without_event(self) -> None:
        self.assertEqual(self.run_hook("job-started", None, None, "--no-disk").returncode, 1)
        self.assertEqual(self.run_hook("job-started", "push", self.dir / "missing.json", "--no-disk").returncode, 1)
        bad = self.dir / "bad.json"
        bad.write_text("{not json")
        self.assertEqual(self.run_hook("job-started", "push", bad, "--no-disk").returncode, 1)

    def test_org_scope_uses_owner(self) -> None:
        path = event(self.dir, "push", {"repository": CMUX})
        self.assertEqual(self.run_hook("job-started", "push", path, "--allowed-owner", "manaflow-ai",
                                       "--no-disk", repo="manaflow-ai/cmux").returncode, 0)
        self.assertEqual(self.run_hook("job-started", "push", path, "--allowed-owner", "manaflow-ai",
                                       "--no-disk", repo="other/cmux").returncode, 1)

    def test_disk_pressure_never_fails_and_is_bounded(self) -> None:
        marker = self.dir / "disk-ran"
        slow = make_executable(self.dir / "glaeda-disk-slow", "import time, pathlib, sys\n"
                               f"pathlib.Path({os.fspath(marker)!r}).write_text(' '.join(sys.argv[1:]))\n"
                               "time.sleep(20)\n")
        broken = make_executable(self.dir / "glaeda-disk-broken", "import sys\nsys.exit(3)\n")
        push = event(self.dir, "push", {"repository": CMUX})
        start = time.monotonic()
        result = self.run_hook("job-started", "push", push, "--allowed-repo", "manaflow-ai/cmux",
                               "--disk", os.fspath(slow), repo="manaflow-ai/cmux",
                               env={"GLAEDA_RUNNER_DISK_TIMEOUT": "1"})
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertLess(time.monotonic() - start, 10)
        self.assertIn("timed out", result.stdout)
        self.assertEqual(marker.read_text(), "--pressure --apply --top 0")
        for phase in ("job-started", "job-completed"):
            result = self.run_hook(phase, "push", push, "--allowed-repo", "manaflow-ai/cmux",
                                   "--disk", os.fspath(broken), repo="manaflow-ai/cmux")
            self.assertEqual(result.returncode, 0, result.stdout)
        # job-completed never fails, even with no event at all
        self.assertEqual(self.run_hook("job-completed", None, None, "--disk", os.fspath(broken)).returncode, 0)

    def test_refused_job_does_not_touch_disk(self) -> None:
        marker = self.dir / "disk-ran"
        disk = make_executable(self.dir / "glaeda-disk", f"import pathlib\npathlib.Path({os.fspath(marker)!r}).touch()\n")
        name, payload, _ = SAMPLE_EVENTS["fork-pr"]
        result = self.run_hook("job-started", name, event(self.dir, "fork", payload), "--allowed-repo",
                               "manaflow-ai/cmux", "--disk", os.fspath(disk), repo="manaflow-ai/cmux")
        self.assertEqual(result.returncode, 1)
        self.assertFalse(marker.exists())

    # ------------------------------------------------------------ host lock, reservation, disk floor

    def fleet(self) -> Path:
        fleet = self.dir / "fleet"
        fleet.mkdir(exist_ok=True)
        (fleet / "host.lock").touch()
        return fleet

    def started(self, *extra: str, watch: int | None = None) -> subprocess.CompletedProcess:
        push = event(self.dir, "push", {"repository": CMUX})
        args = ["--allowed-repo", "manaflow-ai/cmux", "--no-disk", "--state-dir", os.fspath(self.dir / "state"),
                "--watch-pid", str(watch or os.getpid()), *extra]
        return self.run_hook("job-started", "push", push, *args, repo="manaflow-ai/cmux")

    def lock_free(self) -> bool:
        import fcntl
        fd = os.open(self.fleet() / "host.lock", os.O_RDWR)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            fcntl.flock(fd, fcntl.LOCK_UN)
            return True
        except OSError:
            return False
        finally:
            os.close(fd)

    def test_reservation_markers(self) -> None:
        fleet = self.fleet()
        now = int(time.time())
        v1 = {"schema": "glaeda-reservation/v1", "owner": "fleet-session", "purpose": "chromium campaign",
              "since": now - 60, "until": now + 3600}
        cases = {
            "active": (v1, False, "host reserved by fleet-session for chromium campaign until"),
            "expired": ({**v1, "until": now - 1}, True, "admitted"),
            "iso-string-until": ({**v1, "until": "2099-01-01T00:00:00Z"}, False, "until is not integer"),
            "float-until": ({**v1, "until": now + 3600.5}, False, "until is not integer"),
            "bool-until": ({**v1, "until": True}, False, "until is not integer"),
            "millisecond-until": ({**v1, "until": now * 1000}, False, "until is outside"),
            "negative-since": ({**v1, "since": -1}, False, "since is outside"),
            "missing-since": ({k: v for k, v in v1.items() if k != "since"}, False, "since is not integer"),
            "wrong-schema": ({**v1, "schema": "other/v1"}, False, "schema is not"),
            "not-json": ("{nope", False, "not JSON"),
        }
        for name, (doc, admitted, text) in cases.items():
            with self.subTest(name):
                (fleet / "reservation.json").write_text(doc if isinstance(doc, str) else json.dumps(doc))
                result = self.started()
                self.assertEqual(result.returncode == 0, admitted, result.stdout)
                self.assertIn(text, result.stdout)
                self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))
        (fleet / "reservation.json").unlink()
        self.assertEqual(self.started().returncode, 0)
        self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))

    def test_marker_without_the_parser_refuses(self) -> None:
        fleet = self.fleet()
        (fleet / "reservation.json").write_text("{}")
        lonely = self.dir / "lonely"
        lonely.mkdir()
        (lonely / "glaeda-cmux-runner-hook").write_bytes(HOOK.read_bytes())
        push = event(self.dir, "push", {"repository": CMUX})
        result = subprocess.run([sys.executable, os.fspath(lonely / "glaeda-cmux-runner-hook"), "job-started",
                                 "--allowed-repo", "manaflow-ai/cmux", "--no-disk"], capture_output=True, text=True,
                                timeout=30, env={"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.dir),
                                                 "GLAEDA_FLEET_DIR": os.fspath(fleet), "GITHUB_EVENT_NAME": "push",
                                                 "GITHUB_EVENT_PATH": os.fspath(push),
                                                 "GITHUB_REPOSITORY": "manaflow-ai/cmux"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("glaeda_reservation.py is missing", result.stdout)

    def test_disk_floor_refuses(self) -> None:
        self.fleet()
        result = self.started("--min-free-gib", "999999999")
        self.assertEqual(result.returncode, 1)
        self.assertIn("GiB required", result.stdout)
        self.assertTrue(self.lock_free())

    def test_lock_held_by_another_build_refuses_fast(self) -> None:
        fleet = self.fleet()
        holder = subprocess.Popen([sys.executable, "-c",
                                   "import fcntl,os,sys,time\n"
                                   f"fd=os.open({os.fspath(fleet / 'host.lock')!r},os.O_RDWR)\n"
                                   "fcntl.flock(fd,fcntl.LOCK_EX)\nprint('held',flush=True)\ntime.sleep(30)\n"],
                                  stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), "held")
            start = time.monotonic()
            result = self.started()
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("another build holds the fleet host lock", result.stdout)
            self.assertLess(time.monotonic() - start, 10)
        finally:
            holder.kill()
            holder.wait()
            holder.stdout.close()

    def test_job_holds_the_lock_until_completed(self) -> None:
        self.fleet()
        start = time.monotonic()
        result = self.started()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("holding the fleet host lock", result.stdout)
        self.assertLess(time.monotonic() - start, 10, "the holder must not keep the hook's output open")
        self.assertFalse(self.lock_free())
        done = self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))
        self.assertIn("released the fleet host lock", done.stdout)
        self.assertTrue(self.lock_free())

    def test_lock_frees_itself_when_the_job_dies(self) -> None:
        self.fleet()
        worker = subprocess.Popen(["/bin/sleep", "30"])
        try:
            self.assertEqual(self.started(watch=worker.pid).returncode, 0)
            self.assertFalse(self.lock_free())
        finally:
            worker.kill()
            worker.wait()
        deadline = time.monotonic() + 10
        while not self.lock_free() and time.monotonic() < deadline:
            time.sleep(0.2)
        self.assertTrue(self.lock_free())

    def test_stale_holder_file_is_not_success(self) -> None:
        self.fleet()
        state = self.dir / "state"
        state.mkdir()
        stale = hook.holder_file(state)  # keyed by RUNNER_NAME, which CI's own runner sets
        stale.write_text(f"{os.getpid()}\n")  # a live pid, but not a holder
        with mock.patch.object(hook.os, "fork", side_effect=OSError("no fork")):
            held, note = hook.take_host_lock(os.fspath(self.dir / "fleet/host.lock"), os.getpid(), state)
        self.assertFalse(held)
        self.assertIn("cannot start the host lock holder", note)
        self.assertFalse(stale.exists())
        self.assertTrue(self.lock_free())

    def test_huge_until_is_described_not_raised(self) -> None:
        reservation = load("glaeda_reservation_under_test", ROOT / "scripts" / "glaeda_reservation.py")
        text = reservation.describe({"owner": "o", "purpose": "p", "since": 0, "until": 10**30})
        self.assertIn("not a representable time", text)

    def test_no_fleet_lock_file_admits(self) -> None:
        result = self.started()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("no fleet host lock", result.stdout)

    # ------------------------------------------------------------ eligibility gate

    COMMIT = "3809eed51fdd9f1464452f2cd37dbf3148a831fd"
    TOOLCHAIN = {"rustcVersion": "rustc 1.98.1 (48a229cea 2026-09-01)", "cargoVersion": "cargo 1.98.1 (797e8a9bc 2026-08-05)",
                 "zigVersion": "0.16.0", "xcodeVersion": "26.6", "xcodeBuild": "17F113", "macosSdkVersion": "26.5"}

    def node(self, state: str = "eligible", routing: bool = True, receipt_sha: str = "sha256:aa",
             enrolled_sha: str = "sha256:aa", tools: dict | None = None, generation: bool = True) -> None:
        """A fake Glaeda node under HOME (self.dir): enrollment, class receipt, staged CLI, toolchain."""
        config = self.dir / ".config/glaeda/cmux-fleet"
        (config / "class-acceptance").mkdir(parents=True, exist_ok=True)
        (config / "acceptance").mkdir(exist_ok=True)
        (config / "enrollment.json").write_text(json.dumps({"state": state, "classAcceptanceSha256": enrolled_sha}))
        (config / "acceptance/cmux_macos_native_build.json").write_text("{}")
        (config / "class-acceptance/m4pro-48.json").write_text(json.dumps(
            {"receiptSha256": receipt_sha, "glaedaCandidate": {"commit": self.COMMIT}, "toolchain": self.TOOLCHAIN}))
        gen = self.dir / "Projects/glaeda-generations" / self.COMMIT[:12] / "scripts"
        if generation:
            gen.mkdir(parents=True, exist_ok=True)
            (gen / "cmux_fleet.py").write_text("import json\nprint(json.dumps(" + repr({
                "schema": "glaeda-cmux-fleet-node-status/v1", "state": state, "routingCandidateEligible": routing,
                "roles": [{"role": "cmux_macos_native_build", "eligible": routing,
                           "reason": "ok" if routing else "acceptance_missing_or_rejected"}]}) + "))\n")
        cargo = self.dir / ".cargo/bin"
        cargo.mkdir(parents=True, exist_ok=True)
        have = {**self.TOOLCHAIN, **(tools or {})}
        outputs = {"rustc": have["rustcVersion"], "cargo": have["cargoVersion"], "zig": have["zigVersion"],
                   "xcrun": have["macosSdkVersion"],
                   "xcodebuild": f"Xcode {have['xcodeVersion']}\nBuild version {have['xcodeBuild']}"}
        for name, text in outputs.items():
            if text is None:
                (cargo / name).unlink(missing_ok=True)
                continue
            make_executable(cargo / name, f"#!/bin/sh\ncat <<'OUT'\n{text}\nOUT\n")

    def eligible_start(self) -> subprocess.CompletedProcess:
        return self.started("--require-eligible", "--fleet-class", "m4pro-48", "--toolchain-xcode", "/Applications/Xcode_26.6.app")

    def done(self) -> None:
        self.run_hook("job-completed", None, None, "--no-disk", "--state-dir", os.fspath(self.dir / "state"))

    def test_eligible_node_on_its_toolchain_is_admitted(self) -> None:
        self.fleet()
        self.node()
        result = self.eligible_start()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.done()

    def test_ineligible_nodes_refuse(self) -> None:
        self.fleet()
        cases = {
            "enrolling": (dict(state="enrolling", routing=False), "state enrolling"),
            "routing-false": (dict(routing=False), "acceptance_missing_or_rejected"),
            "other-receipt": (dict(enrolled_sha="sha256:bb"), "does not reference the m4pro-48 class receipt"),
            "rustc-drift": (dict(tools={"rustcVersion": "rustc 1.99.0"}), "rustcVersion 'rustc 1.99.0'"),
            "xcode-drift": (dict(tools={"xcodeBuild": "17F999"}), "xcodeBuild '17F999' != '17F113'"),
            "zig-missing": (dict(tools={"zigVersion": None}), "zigVersion '' != '0.16.0'"),
            "no-generation": (dict(generation=False), "no node status from generation 3809eed51fdd"),
        }
        for name, (kwargs, text) in cases.items():
            with self.subTest(name):
                import shutil as _sh
                _sh.rmtree(self.dir / "Projects", ignore_errors=True)
                self.node(**kwargs)
                result = self.eligible_start()
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("refused: node not eligible", result.stdout)
                self.assertIn(text, result.stdout)
                self.assertTrue(self.lock_free(), "a refused job never takes the host lock")

    def test_no_enrollment_refuses(self) -> None:
        self.fleet()
        result = self.eligible_start()
        self.assertEqual(result.returncode, 1)
        self.assertIn("no Glaeda enrollment", result.stdout)

    def test_decide_is_pure_table(self) -> None:
        for name, (event_name, payload, admitted) in SAMPLE_EVENTS.items():
            with self.subTest(name):
                ok, _ = hook.decide(event_name, payload, (payload.get("repository") or {}).get("full_name"),
                                    ["manaflow-ai/cmux"], None)
                self.assertEqual(ok, admitted)


class RunnerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(os.path.realpath(self.tmp.name))
        self.home = base / "operator"
        self.home.mkdir()
        self.state = base / "fake"
        self.state.mkdir()
        self.bin = base / "bin"
        self.bin.mkdir()
        self.gh = make_executable(self.bin / "gh", FAKE_GH)
        self.curl = make_executable(self.bin / "curl", FAKE_CURL)
        self.launchctl = make_executable(self.bin / "launchctl", FAKE_LAUNCHCTL)
        self.build_release()
        self.patches = [
            mock.patch.dict(os.environ, {"HOME": os.fspath(self.home), "FAKE_STATE": os.fspath(self.state),
                                         "FAKE_REG_TOKEN": REG_TOKEN, "FAKE_REMOVE_TOKEN": REMOVE_TOKEN}),
            mock.patch.object(cr, "CURL", os.fspath(self.curl)),
            mock.patch.object(cr, "LAUNCHCTL", os.fspath(self.launchctl)),
            mock.patch.object(cr, "default_name", lambda: "mini-test-glaeda"),
        ]
        for p in self.patches:
            p.start()

    def tearDown(self) -> None:
        for p in reversed(self.patches):
            p.stop()
        self.tmp.cleanup()

    def build_release(self, sha_override: str | None = None) -> None:
        stage = self.state / "stage"
        stage.mkdir(exist_ok=True)
        make_executable(stage / "config.sh", FAKE_CONFIG)
        make_executable(stage / "run.sh", "#!/bin/bash\nexit 0\n")
        tarball = self.state / "runner.tar.gz"
        with tarfile.open(tarball, "w:gz") as tar:
            for name in ("config.sh", "run.sh"):
                tar.add(stage / name, arcname=name)
        sha = sha_override or hashlib.sha256(tarball.read_bytes()).hexdigest()
        asset = f"actions-runner-osx-arm64-{VERSION}.tar.gz"
        (self.state / "release.json").write_text(json.dumps({
            "tag_name": f"v{VERSION}",
            "body": f"notes\n<!-- BEGIN SHA osx-arm64 -->{sha}<!-- END SHA osx-arm64 -->\n",
            "assets": [{"name": asset, "browser_download_url": f"https://github.com/actions/runner/releases/download/v{VERSION}/{asset}"}],
        }))

    def invoke(self, *args: str, expect: int = 0, via_setup: bool = False, gh: bool = True,
               stdin: str = "") -> dict:
        out = io.StringIO()
        argv = ["--output", "json", *(["--gh", os.fspath(self.gh)] if gh else []), "--python", sys.executable, *args]
        with contextlib.redirect_stdout(out), mock.patch.object(sys, "stdin", io.StringIO(stdin)):
            code = setup.main(["--runner", *argv]) if via_setup else cr.main(argv)
        self.assertEqual(code, expect, out.getvalue()[-3000:])
        self.last_output = out.getvalue()
        return json.loads(out.getvalue())

    def log(self) -> list[dict]:
        path = self.state / "log.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def tree(self) -> dict[str, bytes | None]:
        return {os.fspath(p.relative_to(self.home)): (p.read_bytes() if p.is_file() else None)
                for p in sorted(self.home.rglob("*"))}

    def by_kind(self, receipt: dict) -> dict[str, dict]:
        return {a["kind"] + (":" + Path(a["path"]).name if a["kind"] == "hook" else ""): a for a in receipt["actions"]}

    # ------------------------------------------------------------ plan

    def test_plan_is_side_effect_free_and_prints_routing(self) -> None:
        receipt = self.invoke()
        self.assertFalse(receipt["applied"])
        self.assertEqual(self.tree(), {})
        for entry in self.log():
            self.assertEqual(entry["tool"], "gh", entry)
            self.assertNotIn("-X", entry["argv"], entry)
        kinds = self.by_kind(receipt)
        self.assertEqual(kinds["download"]["state"], "create")
        self.assertEqual(kinds["download"]["version"], VERSION)
        self.assertEqual(kinds["register"]["labels"], ["self-hosted", "macOS", "ARM64", "glaeda-mini"])
        self.assertFalse(kinds["register"]["ephemeral"])
        self.assertIn("gh variable set MACOS_RUNNER_15 --body glaeda-mini --repo manaflow-ai/cmux",
                      receipt["routing"]["route"])
        self.assertIn("gh variable delete MACOS_RUNNER_15 --repo manaflow-ai/cmux", receipt["routing"]["rollback"])
        human = io.StringIO()
        with contextlib.redirect_stdout(human):
            cr.main(["--gh", os.fspath(self.gh), "--python", sys.executable])
        self.assertIn("gh variable set MACOS_RUNNER_26 --body glaeda-mini --repo manaflow-ai/cmux", human.getvalue())
        self.assertEqual(self.tree(), {})

    def test_mini_setup_runner_flag_delegates(self) -> None:
        receipt = self.invoke(via_setup=True)
        self.assertEqual(receipt["schema"], "glaeda-cmux-runner/v1")
        self.assertEqual(self.tree(), {})

    # ------------------------------------------------------------ apply

    def test_apply_installs_registers_and_is_idempotent(self) -> None:
        receipt = self.invoke("--apply", "--labels", "cmux-extra")
        runner = self.home / "actions-runner-glaeda"
        self.assertTrue((runner / ".runner").is_file())
        self.assertTrue((runner / ".glaeda-cmux-runner").is_file())
        kinds = self.by_kind(receipt)
        self.assertEqual(kinds["verify"]["state"], "ok", kinds["verify"])
        self.assertEqual(kinds["verify"]["runner"]["id"], 4242)
        config = [e for e in self.log() if e["tool"] == "config.sh"][0]
        self.assertIn("--unattended", config["argv"])
        self.assertNotIn("--ephemeral", config["argv"])
        self.assertEqual(config["argv"][config["argv"].index("--labels") + 1], "glaeda-mini,cmux-extra")
        self.assertEqual(config["argv"][config["argv"].index("--url") + 1], "https://github.com/manaflow-ai/cmux")
        plist = plistlib.loads((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist").read_bytes())
        self.assertEqual(plist["ProgramArguments"], [os.fspath(runner / "run.sh")])
        env = plist["EnvironmentVariables"]
        started = Path(env["ACTIONS_RUNNER_HOOK_JOB_STARTED"])
        self.assertTrue(os.access(started, os.X_OK))
        self.assertTrue(os.access(env["ACTIONS_RUNNER_HOOK_JOB_COMPLETED"], os.X_OK))
        self.assertIn("--allowed-repo manaflow-ai/cmux", started.read_text())
        receipt_path = self.home / ".local/state/glaeda/cmux-runner/receipt.json"
        self.assertEqual(receipt_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(json.loads(receipt_path.read_text())["registration"]["runnerId"], 4242)
        self.assertFalse(any(e["tool"] == "launchctl" for e in self.log()), "sandbox HOME never calls launchctl")

        before = self.tree()
        calls = len(self.log())
        second = self.invoke("--apply", "--labels", "cmux-extra")
        states = {a["state"] for a in second["actions"] if a["kind"] != "verify"}
        self.assertEqual(states, {"unchanged"})
        self.assertFalse(any(e["tool"] in {"curl", "config.sh"} or "-X" in e["argv"] for e in self.log()[calls:]))
        after = self.tree()
        for tree in (before, after):
            tree.pop(".local/state/glaeda/cmux-runner/receipt.json")
        self.assertEqual(before, after)

    def test_installed_wrapper_hooks_refuse_fork_prs(self) -> None:
        self.invoke("--apply")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        for name, (event_name, payload, admitted) in SAMPLE_EVENTS.items():
            with self.subTest(name):
                path = event(self.state, name, payload)
                result = subprocess.run(["/bin/bash", os.fspath(hooks / "job-started.sh")], capture_output=True,
                                        text=True, timeout=60, check=False, env={
                                            "PATH": "/usr/bin:/bin", "HOME": os.fspath(self.home),
                                            "GLAEDA_FLEET_DIR": os.fspath(self.home / "fleet"),
                                            "GITHUB_EVENT_NAME": event_name, "GITHUB_EVENT_PATH": os.fspath(path),
                                            "GITHUB_REPOSITORY": (payload.get("repository") or {}).get("full_name", "")})
                self.assertEqual(result.returncode == 0, admitted, result.stdout + result.stderr)
                done = subprocess.run(["/bin/bash", os.fspath(hooks / "job-completed.sh")], capture_output=True,
                                      text=True, timeout=60, check=False, env={"PATH": "/usr/bin:/bin",
                                                                               "HOME": os.fspath(self.home)})
                self.assertEqual(done.returncode, 0)

    def test_installed_wrapper_holds_the_lock_for_its_real_parent(self) -> None:
        self.invoke("--apply")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        fleet = self.home / "fleet"
        fleet.mkdir()
        (fleet / "host.lock").touch()
        env = {"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.home), "GLAEDA_FLEET_DIR": os.fspath(fleet),
               "GITHUB_EVENT_NAME": "push", "GITHUB_EVENT_PATH": os.fspath(event(self.state, "push", SAMPLE_EVENTS["push"][1])),
               "GITHUB_REPOSITORY": "manaflow-ai/cmux"}
        # No --watch-pid: the wrapper execs python, so the holder watches this test process, which lives on.
        started = subprocess.run(["/bin/bash", os.fspath(hooks / "job-started.sh")], capture_output=True,
                                 text=True, timeout=60, check=False, env=env)
        self.assertEqual(started.returncode, 0, started.stdout + started.stderr)
        self.assertIn(f"watching pid {os.getpid()}", started.stdout)
        time.sleep(3)  # longer than one holder poll: a holder watching a dead parent would have let go
        import fcntl
        fd = os.open(fleet / "host.lock", os.O_RDONLY)
        try:
            with self.assertRaises(OSError):
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            os.close(fd)
        subprocess.run(["/bin/bash", os.fspath(hooks / "job-completed.sh")], capture_output=True, timeout=60,
                       check=False, env=env)
        fd = os.open(fleet / "host.lock", os.O_RDONLY)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            os.close(fd)

    def test_missing_interpreter_fails_closed(self) -> None:
        self.invoke("--apply", "--python", "/nonexistent/python3")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        path = event(self.state, "push", SAMPLE_EVENTS["push"][1])
        started = subprocess.run(["/bin/bash", os.fspath(hooks / "job-started.sh")], capture_output=True, timeout=30,
                                 check=False, env={"PATH": "/usr/bin:/bin", "GITHUB_EVENT_NAME": "push",
                                                   "GITHUB_EVENT_PATH": os.fspath(path)})
        self.assertNotEqual(started.returncode, 0)
        done = subprocess.run(["/bin/bash", os.fspath(hooks / "job-completed.sh")], capture_output=True, timeout=30,
                              check=False, env={"PATH": "/usr/bin:/bin"})
        self.assertEqual(done.returncode, 0)

    def test_token_never_in_argv_files_or_output(self) -> None:
        self.invoke("--apply")
        self.invoke("--uninstall", "--apply")
        entries = self.log()
        self.assertTrue(any(e["tool"] == "config.sh" and e["envToken"] == REG_TOKEN for e in entries))
        self.assertTrue(any(e["tool"] == "config.sh" and e["envToken"] == REMOVE_TOKEN for e in entries))
        for entry in entries:
            joined = " ".join(entry["argv"])
            self.assertNotIn(REG_TOKEN, joined)
            self.assertNotIn(REMOVE_TOKEN, joined)
            if entry["tool"] != "config.sh":
                self.assertIsNone(entry["envToken"], entry)
        self.assertNotIn(REG_TOKEN, self.last_output)
        for path in list(self.home.rglob("*")):
            if path.is_file():
                data = path.read_bytes()
                self.assertNotIn(REG_TOKEN.encode(), data, path)
                self.assertNotIn(REMOVE_TOKEN.encode(), data, path)
        self.assertNotIn("ACTIONS_RUNNER_INPUT_TOKEN", os.environ)

    def test_token_scrubbed_from_config_output_on_failure(self) -> None:
        # config.sh rejects this token and echoes it back; the receipt and output must not carry it.
        out = io.StringIO()
        with mock.patch.object(cr, "read_token", lambda gh, ep: REG_TOKEN + "X"), contextlib.redirect_stdout(out):
            code = cr.main(["--apply", "--output", "json", "--gh", os.fspath(self.gh), "--python", sys.executable])
        self.assertEqual(code, 1)
        self.assertNotIn(REG_TOKEN + "X", out.getvalue())
        receipt = json.loads(out.getvalue())
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "failed")
        self.assertEqual(self.by_kind(receipt)["agent"]["state"], "skipped")

    def test_sha_mismatch_refuses_install(self) -> None:
        self.build_release(sha_override="0" * 64)
        receipt = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(receipt)["download"]["state"], "failed")
        self.assertIn("SHA-256 mismatch", self.by_kind(receipt)["download"]["note"])
        self.assertFalse((self.home / "actions-runner-glaeda").exists())
        self.assertFalse(any(e["tool"] == "config.sh" or "-X" in e["argv"] for e in self.log()))

    def test_existing_runner_name_blocks_without_replace(self) -> None:
        (self.state / "runners.json").write_text(json.dumps({"runners": [
            {"id": 7, "name": "mini-test-glaeda", "status": "offline", "labels": []}]}))
        receipt = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "blocked")
        self.assertFalse(receipt["ready"])
        self.assertFalse((self.home / "actions-runner-glaeda").exists())

    def test_launchctl_is_managed_for_the_real_home(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            receipt = self.invoke("--apply")
            self.assertTrue(self.by_kind(receipt)["agent"]["bootstrapped"])
            self.invoke("--uninstall", "--apply")
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"]
        self.assertIn("bootstrap", verbs)
        self.assertIn("bootout", verbs)
        self.assertLess(verbs.index("bootstrap"), verbs.index("bootout"))

    # ------------------------------------------------------------ uninstall

    def test_uninstall_plan_is_side_effect_free_then_apply_removes_owned(self) -> None:
        self.invoke("--apply", "--name", "custom-mini")
        before, calls = self.tree(), len(self.log())
        plan = self.invoke("--uninstall")  # no --name: the receipt decides what to remove
        self.assertEqual(before, self.tree())
        self.assertFalse(any("-X" in e["argv"] or e["tool"] == "config.sh" for e in self.log()[calls:]))
        states = {a["kind"]: a["state"] for a in plan["actions"]}
        self.assertEqual(states, {"agent": "remove", "deregister": "remove", "dir": "remove"})
        self.invoke("--uninstall", "--apply")
        self.assertFalse((self.home / "actions-runner-glaeda").exists())
        self.assertFalse((self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist").exists())
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])
        remove = [e for e in self.log() if e["tool"] == "config.sh" and e["argv"][:1] == ["remove"]]
        self.assertEqual(len(remove), 1)
        state_dir = self.home / ".local/state/glaeda/cmux-runner"
        self.assertTrue((state_dir / "uninstall-receipt.json").is_file())
        self.assertFalse((state_dir / "receipt.json").exists())

    def test_uninstall_without_receipt_removes_nothing(self) -> None:
        runner = self.home / "actions-runner-glaeda"
        runner.mkdir()
        (runner / "config.sh").write_text("#!/bin/sh\n")
        before = self.tree()
        receipt = self.invoke("--uninstall", "--apply")
        self.assertTrue(any("nothing is removed" in a.get("note", "") for a in receipt["actions"]))
        after = {k: v for k, v in self.tree().items() if not k.startswith(".local")}
        self.assertEqual(before, after)
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/uninstall-receipt.json").is_file())
        self.assertFalse(any(e["tool"] == "config.sh" or "-X" in e["argv"] for e in self.log()))

    def test_preexisting_runner_dir_is_never_adopted(self) -> None:
        runner = self.home / "actions-runner-glaeda"
        runner.mkdir()
        (runner / "keep.txt").write_text("operator data")
        receipt = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(receipt)["dir"]["state"], "blocked")
        self.assertEqual(sorted(p.name for p in runner.iterdir()), ["keep.txt"])
        self.assertFalse(any(e["tool"] in {"curl", "config.sh"} for e in self.log()))
        self.invoke("--uninstall", "--apply")
        self.assertTrue((runner / "keep.txt").exists())

    def test_marker_mismatch_keeps_dir_and_registration(self) -> None:
        self.invoke("--apply")
        runner = self.home / "actions-runner-glaeda"
        (runner / ".glaeda-cmux-runner").write_text("someone-else\n")
        plan = self.invoke("--uninstall", "--apply")
        states = {a["kind"]: a["state"] for a in plan["actions"]}
        self.assertEqual(states["dir"], "kept")
        self.assertEqual(states["deregister"], "kept")
        self.assertTrue((runner / ".runner").exists())
        self.assertFalse(any(e["tool"] == "config.sh" and e["argv"][:1] == ["remove"] for e in self.log()))

    def test_operator_edited_plist_is_kept(self) -> None:
        self.invoke("--apply")
        plist = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
        plist.write_bytes(plist.read_bytes() + b"\n<!-- operator edit -->\n")
        plan = self.invoke("--uninstall", "--apply")
        self.assertEqual({a["kind"]: a["state"] for a in plan["actions"]}["agent"], "kept")
        self.assertTrue(plist.exists())

    def test_failed_deregistration_keeps_dir_and_receipt(self) -> None:
        self.invoke("--apply")
        (self.state / "fail-config-remove").touch()
        (self.state / "fail-delete").touch()
        receipt = self.invoke("--uninstall", "--apply", expect=1)
        states = {a["kind"]: a["state"] for a in receipt["actions"]}
        self.assertEqual(states["deregister"], "failed")
        self.assertEqual(states["dir"], "kept")
        self.assertTrue((self.home / "actions-runner-glaeda/.runner").exists())
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/receipt.json").exists())
        (self.state / "fail-config-remove").unlink()
        (self.state / "fail-delete").unlink()
        # The plist was already removed by the failed run; the retry still deregisters and cleans up.
        retry = self.invoke("--uninstall", "--apply")
        self.assertEqual({a["kind"]: a["state"] for a in retry["actions"]}["dir"], "remove")
        self.assertFalse((self.home / "actions-runner-glaeda").exists())

    def test_api_fallback_never_deletes_another_machines_runner(self) -> None:
        self.invoke("--apply")
        # the name was re-registered elsewhere with --replace: same name, new id
        (self.state / "runners.json").write_text(json.dumps({"runners": [
            {"id": 9999, "name": "mini-test-glaeda", "status": "online", "labels": []}]}))
        (self.state / "fail-config-remove").touch()
        receipt = self.invoke("--uninstall", "--apply", expect=1)
        dereg = next(a for a in receipt["actions"] if a["kind"] == "deregister")
        self.assertEqual(dereg["state"], "failed")
        self.assertIn("not deleted", dereg["note"])
        self.assertFalse(any(e["tool"] == "gh" and "DELETE" in e["argv"] for e in self.log()))
        self.assertTrue((self.home / "actions-runner-glaeda").is_dir())
        self.assertTrue((self.home / ".local/state/glaeda/cmux-runner/receipt.json").is_file())

    def test_a_second_install_elsewhere_is_blocked_and_keeps_the_receipt(self) -> None:
        self.invoke("--apply")
        receipt_path = self.home / ".local/state/glaeda/cmux-runner/receipt.json"
        before = receipt_path.read_bytes()
        (self.home / "elsewhere").mkdir()
        for args in (("--runner-dir", os.fspath(self.home / "elsewhere")),
                     ("--runner-dir", os.fspath(self.home / "fresh")),
                     ("--name", "other-glaeda"), ("--repo", "teamleaderleo/cmux")):
            with self.subTest(args=args):
                blocked = self.invoke("--apply", *args, expect=1)
                self.assertEqual([a["state"] for a in blocked["actions"]], ["blocked"])
                self.assertEqual(receipt_path.read_bytes(), before)
                self.assertFalse((self.home / "fresh").exists())
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(all(a["applied"] for a in removed["actions"] if a["state"] == "remove"))
        self.assertFalse((self.home / "actions-runner-glaeda").exists())

    def test_rerun_after_failed_unpack_recovers(self) -> None:
        self.invoke("--apply")
        runner = self.home / "actions-runner-glaeda"
        for name in ("config.sh", ".runner"):
            (runner / name).unlink()
        (self.state / "runners.json").write_text(json.dumps({"runners": []}))
        receipt = self.invoke("--apply")
        self.assertTrue(receipt["ready"], receipt["blocking"])
        self.assertTrue((runner / ".runner").is_file())

    def test_registration_finished_after_timeout_is_adopted_then_removed(self) -> None:
        # config.sh outlived our timeout: .runner exists but the receipt recorded no registration
        with mock.patch.object(cr, "run_with_token", side_effect=lambda argv, token, cwd, timeout=900: (
                subprocess.run(argv, env={**os.environ, "ACTIONS_RUNNER_INPUT_TOKEN": token}, cwd=cwd,
                               capture_output=True, check=False), (127, "timed out after 900 seconds"))[1]):
            first = self.invoke("--apply", expect=1)
        self.assertEqual(self.by_kind(first)["register"]["state"], "failed")
        runner = self.home / "actions-runner-glaeda"
        self.assertTrue((runner / ".runner").is_file())
        second = self.invoke("--apply")
        self.assertTrue(second["ready"], second["blocking"])
        self.assertIn("adopted", self.by_kind(second)["register"]["note"])
        receipt = json.loads((self.home / ".local/state/glaeda/cmux-runner/receipt.json").read_text())
        self.assertEqual(receipt["registration"]["runnerId"], 4242)
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])
        self.assertFalse(runner.exists())

    def test_uninstall_deregisters_an_unadopted_interrupted_registration(self) -> None:
        with mock.patch.object(cr, "run_with_token", side_effect=lambda argv, token, cwd, timeout=900: (
                subprocess.run(argv, env={**os.environ, "ACTIONS_RUNNER_INPUT_TOKEN": token}, cwd=cwd,
                               capture_output=True, check=False), (127, "timed out"))[1]):
            self.invoke("--apply", expect=1)
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])

    def test_interrupted_registration_uninstall_uses_the_runners_own_name_and_scope(self) -> None:
        with mock.patch.object(cr, "run_with_token", side_effect=lambda argv, token, cwd, timeout=900: (
                subprocess.run(argv, env={**os.environ, "ACTIONS_RUNNER_INPUT_TOKEN": token}, cwd=cwd,
                               capture_output=True, check=False), (124, "timed out"))[1]):
            self.invoke("--apply", "--repo", "teamleaderleo/cmux", "--name", "custom-glaeda", expect=1)
        removed = self.invoke("--uninstall", "--apply")
        self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
        tokens = [e["argv"] for e in self.log() if e["tool"] == "gh" and "-X" in e["argv"]]
        self.assertIn("repos/teamleaderleo/cmux/actions/runners/remove-token", tokens[-1])
        self.assertEqual(json.loads((self.state / "runners.json").read_text())["runners"], [])

    def test_timeout_kills_the_whole_process_group(self) -> None:
        marker = self.home / "grandchild-survived"
        script = make_executable(self.home / "slow-config.sh",
                                 "#!/bin/bash\n(sleep 3; touch " + os.fspath(marker) + ") &\nsleep 30\n")
        start = time.monotonic()
        code, out = cr.run_with_token([os.fspath(script)], REG_TOKEN, self.home, timeout=1)
        self.assertEqual(code, 124)
        self.assertLess(time.monotonic() - start, 10)
        self.assertNotIn(REG_TOKEN, out)
        time.sleep(4)
        self.assertFalse(marker.exists())

    def manifest(self) -> str:
        path = self.state / "mini-fleet.json"
        path.write_text(json.dumps(MANIFEST))
        return os.fspath(path)

    def config_argvs(self) -> list[list[str]]:
        return [e["argv"] for e in self.log() if e["tool"] == "config.sh" and e["argv"][:1] != ["remove"]]

    def test_manifest_install_derives_labels_and_name(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std")
        argv = self.config_argvs()[0]
        self.assertEqual(argv[argv.index("--name") + 1], "mini-std-glaeda")
        self.assertEqual(argv[argv.index("--labels") + 1], "glaeda-mini,glaeda-class-std,glaeda-dedicated,xcode-26.6,glaeda-std-xcode-26.6")
        self.assertEqual(receipt["member"]["class"], "std")
        self.assertEqual(self.by_kind(receipt)["verify"]["state"], "ok")
        # a plain re-run keeps the registered labels and never asks for a relabel
        again = self.invoke("--apply")
        self.assertEqual(self.by_kind(again)["register"]["state"], "unchanged")
        self.assertEqual(len(self.config_argvs()), 1)

    def test_manifest_disk_floor_is_baked_into_the_hook(self) -> None:
        manifest = json.loads(json.dumps(MANIFEST))
        manifest["defaults"]["disk"] = {"min_free_gib": 100}
        path = self.state / "floor.json"
        path.write_text(json.dumps(manifest))
        with mock.patch.object(cr, "xcode_present", return_value=True):
            self.invoke("--apply", "--manifest", os.fspath(path), "--member", "mini-std")
        hooks = self.home / "actions-runner-glaeda/glaeda-hooks"
        self.assertIn("--min-free-gib 100", (hooks / "job-started.sh").read_text())
        self.assertIn("--require-eligible --fleet-class m4pro-48 --toolchain-xcode /Applications/Xcode_26.6.app",
                      (hooks / "job-started.sh").read_text())
        self.assertTrue((hooks / "glaeda_reservation.py").is_file())

    def test_manifest_refusals_and_exclusive_flags(self) -> None:
        for args in (("--manifest", self.manifest(), "--member", "laptop"),
                     ("--manifest", self.manifest(), "--member", "mini-std", "--labels", "x"),
                     ("--manifest", self.manifest()), ("--member", "mini-std")):
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(cr.main(["--gh", os.fspath(self.gh), *args]), 2)
        self.assertEqual(self.tree(), {})

    def test_relabel_reregisters_in_place_and_keeps_work(self) -> None:
        self.invoke("--apply", "--labels", "ram48")  # the pre-manifest fleet install
        runner = self.home / "actions-runner-glaeda"
        (runner / "_work").mkdir(exist_ok=True)
        (runner / "_work" / "hot").write_text("derived data")
        for stale in (".runner_migrated", ".credentials_migrated"):
            (runner / stale).write_text("{}")
        with mock.patch.object(cr, "xcode_present", return_value=True):
            plan = self.invoke("--manifest", self.manifest(), "--member", "mini-std", "--name", "mini-test-glaeda")
            self.assertEqual(self.by_kind(plan)["register"]["state"], "relabel")
            self.assertEqual(self.by_kind(plan)["register"]["previousLabels"],
                             ["self-hosted", "macOS", "ARM64", "glaeda-mini", "ram48"])
            receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                  "--name", "mini-test-glaeda")
        reg = self.by_kind(receipt)["register"]
        self.assertEqual(reg["state"], "updated", reg)
        argv = self.config_argvs()[-1]
        self.assertIn("--replace", argv)
        self.assertEqual(argv[argv.index("--labels") + 1], "glaeda-mini,glaeda-class-std,glaeda-dedicated,xcode-26.6,glaeda-std-xcode-26.6")
        runners = json.loads((self.state / "runners.json").read_text())["runners"]
        self.assertEqual([(r["name"], r["id"]) for r in runners], [("mini-test-glaeda", 4243)])
        self.assertEqual((runner / "_work" / "hot").read_text(), "derived data")
        self.assertFalse((runner / ".runner_migrated").exists())
        self.assertFalse((runner / ".credentials_migrated").exists())
        saved = json.loads((self.home / ".local/state/glaeda/cmux-runner/receipt.json").read_text())
        self.assertEqual(saved["registration"]["runnerId"], 4243)
        self.assertIn("glaeda-class-std", saved["registration"]["labels"])
        self.assertEqual(self.by_kind(receipt)["verify"]["state"], "ok")

    def test_relabel_starts_a_stopped_agent_exactly_once(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply", "--labels", "ram48")
            with mock.patch.object(cr, "xcode_present", return_value=True):
                receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                      "--name", "mini-test-glaeda")
        self.assertEqual(self.by_kind(receipt)["register"]["state"], "updated")
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl"]
        self.assertEqual(verbs.count("bootstrap"), 2, verbs)  # install, then relabel; never a third

    def test_relabel_with_a_plist_change_restarts_on_the_new_plist(self) -> None:
        fake_pw = mock.Mock(pw_dir=os.fspath(self.home))
        with mock.patch.object(cr.pwd, "getpwuid", return_value=fake_pw):
            self.invoke("--apply", "--labels", "ram48")
            plist = self.home / "Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
            receipt_path = self.home / ".local/state/glaeda/cmux-runner/receipt.json"
            doc = json.loads(receipt_path.read_text())
            data = plist.read_bytes().replace(b"</dict>\n</plist>", b"<key>X</key><string>old</string></dict>\n</plist>")
            plist.write_bytes(data)  # an older plist this install wrote
            for act in doc["actions"]:
                if act.get("kind") == "agent":
                    act["ownedSha256"] = cr.sha256_bytes(data)
            receipt_path.write_text(json.dumps(doc))
            with mock.patch.object(cr, "xcode_present", return_value=True):
                receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                      "--name", "mini-test-glaeda")
        self.assertEqual(self.by_kind(receipt)["agent"]["state"], "update")
        self.assertTrue(receipt["ready"], receipt["blocking"])
        verbs = [e["argv"][0] for e in self.log() if e["tool"] == "launchctl" and e["argv"][0] != "print"]
        self.assertEqual(verbs[-3:], ["bootstrap", "bootout", "bootstrap"], verbs)

    def test_relabel_without_a_token_blocks_and_changes_nothing(self) -> None:
        self.invoke("--apply", "--labels", "ram48")
        before = self.tree()
        real_which = cr.which
        with mock.patch.object(cr, "which", lambda name, extra=(): None if name == "gh" else real_which(name, extra)), \
                mock.patch.object(cr, "xcode_present", return_value=False):
            blocked = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                  "--name", "mini-test-glaeda", gh=False, expect=1)
        self.assertIn("--token-stdin", self.by_kind(blocked)["register"]["note"])
        after = self.tree()
        for tree in (before, after):
            tree.pop(".local/state/glaeda/cmux-runner/receipt.json", None)
        self.assertEqual(before, after)

    def test_interrupted_relabel_reregisters_its_own_name(self) -> None:
        self.invoke("--apply", "--labels", "ram48")
        (self.state / "fail-register").touch()
        real = cr.register

        def failing(ctx, act):  # config.sh fails after the local registration was cleared
            act["state"], act["note"] = "failed", "config.sh failed: network"
        with mock.patch.object(cr, "register", failing), mock.patch.object(cr, "xcode_present", return_value=False):
            self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                        "--name", "mini-test-glaeda", expect=1)
        runner = self.home / "actions-runner-glaeda"
        self.assertFalse((runner / ".runner").exists())
        with mock.patch.object(cr, "register", real), mock.patch.object(cr, "xcode_present", return_value=False):
            receipt = self.invoke("--apply", "--manifest", self.manifest(), "--member", "mini-std",
                                  "--name", "mini-test-glaeda")
        self.assertTrue(receipt["ready"], receipt["blocking"])
        self.assertIn("--replace", self.config_argvs()[-1])
        runners = json.loads((self.state / "runners.json").read_text())["runners"]
        self.assertEqual(len(runners), 1)

    def test_config_remove_failure_falls_back_to_api_delete(self) -> None:
        self.invoke("--apply")
        (self.state / "fail-config-remove").touch()
        receipt = self.invoke("--uninstall", "--apply")
        dereg = next(a for a in receipt["actions"] if a["kind"] == "deregister")
        self.assertIn("deleted by id", dereg["note"])
        self.assertTrue(any(e["tool"] == "gh" and "DELETE" in e["argv"] for e in self.log()))
        self.assertFalse((self.home / "actions-runner-glaeda").exists())


    # ------------------------------------------------------------ --token-stdin, no gh on the mini

    def test_token_stdin_without_gh_installs_and_uninstalls(self) -> None:
        real_which = cr.which
        with mock.patch.object(cr, "which", lambda name, extra=(): None if name == "gh" else real_which(name, extra)):
            plan = self.invoke(gh=False)
            self.assertEqual(self.by_kind(plan)["register"]["state"], "blocked")
            preview = self.invoke("--token-stdin", gh=False, stdin=REG_TOKEN + "\n")
            self.assertTrue(preview["ready"], preview["blocking"])
            self.assertEqual(self.by_kind(preview)["register"]["state"], "create")
            self.assertEqual(self.tree(), {})
            for entry in self.log():  # plans only read the public release API
                self.assertEqual(entry["tool"], "curl", entry)
                self.assertNotIn("-o", entry["argv"], entry)
            receipt = self.invoke("--apply", "--token-stdin", gh=False, stdin=REG_TOKEN + "\n")
            kinds = self.by_kind(receipt)
            self.assertTrue(receipt["ready"], receipt["blocking"])
            self.assertEqual(kinds["download"]["version"], VERSION)
            self.assertEqual(kinds["verify"]["state"], "ok", kinds["verify"])
            self.assertEqual(kinds["verify"]["runner"]["id"], 4242)
            self.assertFalse(any(e["tool"] == "gh" for e in self.log()))
            config = [e for e in self.log() if e["tool"] == "config.sh"][0]
            self.assertEqual(config["envToken"], REG_TOKEN)
            for entry in self.log():
                self.assertNotIn(REG_TOKEN, " ".join(entry["argv"]))
            self.assertNotIn(REG_TOKEN, self.last_output)
            for name, data in self.tree().items():
                self.assertNotIn(REG_TOKEN.encode(), data or b"", name)

            again = self.invoke("--apply", gh=False)  # an installed mini re-applies without gh or a token
            self.assertTrue(again["ready"], again["blocking"])
            self.assertEqual({a["state"] for a in again["actions"] if a["kind"] != "verify"}, {"unchanged"})

            before = self.tree()
            stuck = self.invoke("--uninstall", "--apply", gh=False, expect=1)  # no way to deregister: touch nothing
            self.assertFalse(stuck["ready"])
            self.assertTrue(all(a["state"] == "blocked" for a in stuck["actions"]), stuck["actions"])
            after = self.tree()
            after.pop(".local/state/glaeda/cmux-runner/uninstall-receipt.json")
            self.assertEqual(before, after)

            removed = self.invoke("--uninstall", "--apply", "--token-stdin", gh=False, stdin=REMOVE_TOKEN + "\n")
            self.assertTrue(next(a for a in removed["actions"] if a["kind"] == "deregister")["applied"])
            self.assertFalse((self.home / "actions-runner-glaeda").exists())
            self.assertNotIn(REMOVE_TOKEN, self.last_output)

    def test_xcode_app_check_warns_and_never_blocks(self) -> None:
        missing = self.invoke("--xcode-app", os.fspath(self.home / "Xcode_26.6.app"))
        self.assertEqual(missing["preflight"]["checks"]["xcodeApp"]["level"], "warn")
        self.assertNotIn("xcodeApp", missing["blocking"])
        real = self.home / "Real.app"
        real.mkdir()
        (self.home / "Link.app").symlink_to(real)
        linked = self.invoke("--xcode-app", os.fspath(self.home / "Link.app"))
        self.assertIn("symlink", linked["preflight"]["checks"]["xcodeApp"]["value"])
        self.assertNotIn("xcodeApp", self.invoke()["preflight"]["checks"])

    def test_token_stdin_apply_needs_a_token(self) -> None:
        for args, stdin in ((("--apply", "--token-stdin"), ""),
                            (("--apply", "--token-stdin"), "not a token; rm -rf /\n")):
            with self.subTest(args=args, stdin=stdin[:8]), contextlib.redirect_stderr(io.StringIO()) as err, \
                    mock.patch.object(sys, "stdin", io.StringIO(stdin)):
                self.assertEqual(cr.main(["--gh", os.fspath(self.gh), *args]), 2)
                self.assertNotIn("rm -rf", err.getvalue())
        self.assertEqual(self.tree(), {})
        self.assertEqual(self.log(), [])


MANIFEST = {
    "defaults": {"xcode": {"apps": [{"path": "/Applications/Xcode_26.6.app", "version": "26.6", "build": "17F113"}]}},
    "hosts": {
        "mini-std": {"class": "std", "availability": "dedicated", "roles": ["dev-builds", "ci-runner"],
                     "hardware": "m4pro-48"},
        "mini-light": {"class": "light", "availability": "opportunistic", "roles": ["ci-runner"], "owner": "x",
                       "hardware": "m4-16"},
        "no-hardware": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"]},
        "mini-no-role": {"class": "std", "availability": "dedicated", "roles": ["dev-builds"]},
        "laptop": {"class": "dev", "availability": "opportunistic", "roles": ["ci-runner"]},
        "borrowed": {"class": "borrowed", "availability": "opportunistic", "roles": ["ci-runner"]},
        "old-shape": {"class": "m4pro-48", "roles": ["ci-runner"]},
        "bad-avail": {"class": "std", "availability": "sometimes", "roles": ["ci-runner"]},
        "override": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4pro-48",
                     "overrides": {"xcode": {"apps": [{"path": "/Applications/Xcode.app", "version": "26.3",
                                                       "build": "17C529"}]}}},
    },
}


class ManifestLabelsTest(unittest.TestCase):
    def test_member_labels_table(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            std, _ = cr.member_labels(MANIFEST, "mini-std")
            self.assertEqual(std["labels"], ["glaeda-mini", "glaeda-class-std", "glaeda-dedicated", "xcode-26.6",
                                             "glaeda-std-xcode-26.6"])
            light, _ = cr.member_labels(MANIFEST, "mini-light")
            self.assertEqual(light["labels"], ["glaeda-mini", "glaeda-class-light", "glaeda-opportunistic", "xcode-26.6"])
            self.assertEqual(cr.member_labels(MANIFEST, "override")[0]["labels"][-2:],
                             ["xcode-26.3", "glaeda-std-xcode-26.3"])
        with mock.patch.object(cr, "xcode_present", return_value=False):
            self.assertEqual(cr.member_labels(MANIFEST, "mini-std")[0]["labels"],
                             ["glaeda-mini", "glaeda-class-std", "glaeda-dedicated"])
        for name, why in (("mini-no-role", "ci-runner"), ("laptop", "never runs"), ("borrowed", "never runs"),
                          ("old-shape", "m4pro-48"), ("bad-avail", "availability"), ("absent", "not a member"),
                          ("no-hardware", "no hardware class")):
            with self.subTest(name):
                member, reason = cr.member_labels(MANIFEST, name)
                self.assertIsNone(member)
                self.assertIn(why, reason)

    def test_xcode_present_needs_the_exact_build(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            app = Path(tmp) / "Xcode_26.6.app"
            app.mkdir()
            for out, want in (("Xcode 26.6\nBuild version 17F113", True), ("Xcode 26.6\nBuild version 17F1134", False),
                              ("Xcode 26.6\nBuild version 17F11", False), ("Xcode 26.3\nBuild version 17F113", False)):
                with self.subTest(out=out), mock.patch.object(cr, "run", return_value=(0, out)):
                    got = cr.xcode_present({"path": os.fspath(app), "version": "26.6", "build": "17F113"})
                    self.assertEqual(got, want)
            link = Path(tmp) / "Link.app"
            link.symlink_to(app)
            with mock.patch.object(cr, "run", return_value=(0, "Xcode 26.6\nBuild version 17F113")):
                self.assertFalse(cr.xcode_present({"path": os.fspath(link), "version": "26.6", "build": "17F113"}))

    def test_malformed_manifest_is_refused_not_a_traceback(self) -> None:
        for manifest in ({"hosts": {"m": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"],
                                          "overrides": ["x"]}}},
                         {"defaults": {"xcode": "26.6"},
                          "hosts": {"m": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"]}}},
                         {"hosts": {"m": "std"}}, {"hosts": []}, []):
            with self.subTest(manifest=manifest):
                member, why = cr.member_labels(manifest, "m")
                if member is not None:  # a non-object xcode block just means no Xcode labels
                    self.assertEqual(member["labels"], ["glaeda-mini", "glaeda-class-std", "glaeda-dedicated"])
                else:
                    self.assertTrue(why)


fleet_labels = load("glaeda_fleet_labels_under_test", ROOT / "scripts" / "glaeda_fleet_labels.py")


class FleetLabelsModuleTest(unittest.TestCase):
    """The shared rule glaeda-mini-fleet imports: pin its strings and its declared/conforming views."""

    def test_pool_label_string(self) -> None:
        self.assertEqual(fleet_labels.pool_label("std", "26.6"), "glaeda-std-xcode-26.6")
        self.assertEqual(fleet_labels.pool_label("light", "26.6"), "glaeda-light-xcode-26.6")

    def test_runner_and_module_agree(self) -> None:
        with mock.patch.object(cr, "xcode_present", return_value=True):
            for member in MANIFEST["hosts"]:
                with self.subTest(member):
                    self.assertEqual(cr.member_labels(MANIFEST, member),
                                     fleet_labels.member_labels(MANIFEST, member, lambda app: True))

    def test_declared_and_conforming_pools(self) -> None:
        declared = fleet_labels.declared_pools(MANIFEST)
        self.assertEqual(declared, {"glaeda-std-xcode-26.3": ["override"], "glaeda-std-xcode-26.6": ["mini-std"]})
        conforming = fleet_labels.declared_pools(MANIFEST, lambda member, app: member != "mini-std")
        self.assertEqual(conforming, {"glaeda-std-xcode-26.3": ["override"]})
        self.assertEqual(fleet_labels.declared_pools({"hosts": []}), {})

    def test_runner_without_the_module_explains(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(cr, "SCRIPT_DIR", Path(tmp)):
            member, why = cr.member_labels(MANIFEST, "mini-std")
        self.assertIsNone(member)
        self.assertIn("glaeda_fleet_labels.py must sit next to this script", why)


class NoEmDashTest(unittest.TestCase):
    def test_no_em_dashes(self) -> None:
        for path in (ROOT / "scripts/glaeda-cmux-runner", HOOK, Path(__file__), ROOT / "docs/CMUX_MINI_RUNNER.md",
                     ROOT / "scripts/glaeda_fleet_labels.py", ROOT / "scripts/glaeda_reservation.py"):
            self.assertNotIn(chr(0x2014), path.read_text(encoding="utf-8"), path)


if __name__ == "__main__":
    unittest.main()
