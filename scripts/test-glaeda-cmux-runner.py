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
json.dump({"agentName": name}, open(os.path.join(here, ".runner"), "w"))
doc = runners()
doc["runners"].append({"id": 4242, "name": name, "status": "online", "busy": False,
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
        environ = {"PATH": "/usr/bin:/bin", "HOME": os.fspath(self.dir)}
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

    def invoke(self, *args: str, expect: int = 0, via_setup: bool = False) -> dict:
        out = io.StringIO()
        argv = ["--output", "json", "--gh", os.fspath(self.gh), "--python", sys.executable, *args]
        with contextlib.redirect_stdout(out):
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
                                            "GITHUB_EVENT_NAME": event_name, "GITHUB_EVENT_PATH": os.fspath(path),
                                            "GITHUB_REPOSITORY": (payload.get("repository") or {}).get("full_name", "")})
                self.assertEqual(result.returncode == 0, admitted, result.stdout + result.stderr)
                done = subprocess.run(["/bin/bash", os.fspath(hooks / "job-completed.sh")], capture_output=True,
                                      text=True, timeout=60, check=False, env={"PATH": "/usr/bin:/bin",
                                                                               "HOME": os.fspath(self.home)})
                self.assertEqual(done.returncode, 0)

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
        receipt = self.invoke("--apply")
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
        receipt = self.invoke("--apply")
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

    def test_config_remove_failure_falls_back_to_api_delete(self) -> None:
        self.invoke("--apply")
        (self.state / "fail-config-remove").touch()
        receipt = self.invoke("--uninstall", "--apply")
        dereg = next(a for a in receipt["actions"] if a["kind"] == "deregister")
        self.assertIn("deleted by id", dereg["note"])
        self.assertTrue(any(e["tool"] == "gh" and "DELETE" in e["argv"] for e in self.log()))
        self.assertFalse((self.home / "actions-runner-glaeda").exists())


class NoEmDashTest(unittest.TestCase):
    def test_no_em_dashes(self) -> None:
        for path in (ROOT / "scripts/glaeda-cmux-runner", HOOK, Path(__file__), ROOT / "docs/CMUX_MINI_RUNNER.md"):
            self.assertNotIn(chr(0x2014), path.read_text(encoding="utf-8"), path)


if __name__ == "__main__":
    unittest.main()
