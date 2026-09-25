#!/usr/bin/env python3
"""Tests for scripts/glaeda-seed-prefetch."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_seed_prefetch",
                                              os.fspath(ROOT / "scripts" / "glaeda-seed-prefetch"))
spec = importlib.util.spec_from_loader("glaeda_seed_prefetch", loader)
sp = importlib.util.module_from_spec(spec)
sys.modules["glaeda_seed_prefetch"] = sp
loader.exec_module(sp)

# Stands in for cmux's scripts/ci/seed_derived_data.py: logs each call and answers with the
# distance the test put in FAKE_DISTANCE.
FAKE_SEED = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["FAKE_CALLS"], "a") as log:
    log.write(" ".join(sys.argv[1:]) + " url=" + os.environ.get("CI_CACHE_R2_PUBLIC_URL", "")
              + " git=" + os.environ.get("CMUX_SEED_GIT_DIR", "") + "\\n")
distance = int(os.environ.get("FAKE_DISTANCE", "0"))
print("some progress line")
print(json.dumps({"fetched": "true", "key": "k", "distance": distance}))
"""


class SeedPrefetchTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.state = base / "ci"
        self.state.mkdir()
        self.repo = base / "cmux"
        (self.repo / "scripts/ci").mkdir(parents=True)
        (self.repo / "scripts/ci/seed_derived_data.py").write_text(FAKE_SEED)
        self.git = ["git", "-C", os.fspath(self.repo), "-c", "user.name=t", "-c", "user.email=t@t"]
        subprocess.run(["git", "init", "-q", "-b", "main", os.fspath(self.repo)], check=True)
        self.commit()
        self.calls = base / "calls"
        self.saved = (sp.REPO_URL, sp.running_commands, dict(os.environ))
        sp.REPO_URL = self.repo.as_uri()
        sp.running_commands = lambda: ["/usr/libexec/something", "zsh"]
        os.environ["FAKE_CALLS"] = os.fspath(self.calls)

    def tearDown(self) -> None:
        sp.REPO_URL, sp.running_commands, env = self.saved
        os.environ.clear()
        os.environ.update(env)
        self.tmp.cleanup()

    def commit(self) -> str:
        subprocess.run([*self.git, "add", "-A"], check=True)
        subprocess.run([*self.git, "commit", "-q", "--allow-empty", "-m", "c"], check=True)
        return subprocess.run([*self.git, "rev-parse", "HEAD"], check=True, capture_output=True,
                              text=True).stdout.strip()

    def record(self, store: Path) -> None:
        store.mkdir(parents=True, exist_ok=True)
        (store / sp.SOURCE).write_text(json.dumps({"prefix": "admission-derived-data-v1-macOS-ARM64-fp-"}))

    def call_lines(self) -> list[str]:
        return self.calls.read_text().splitlines() if self.calls.exists() else []

    def test_a_mini_no_owned_job_has_recorded_is_skipped(self):
        result = sp.run(True, self.state)
        self.assertEqual(result["state"], "skip")
        self.assertFalse((self.state / ".prefetch").exists())

    def test_a_busy_mini_defers(self):
        self.record(self.state)
        sp.running_commands = lambda: ["/Users/cmux/actions-runner-glaeda/bin/Runner.Worker spawnclient 1 2"]
        self.assertEqual(sp.run(True, self.state)["state"], "deferred")
        sp.running_commands = lambda: None
        self.assertEqual(sp.run(True, self.state)["state"], "deferred")
        self.assertEqual(self.call_lines(), [])

    def test_plan_changes_nothing(self):
        self.record(self.state)
        result = sp.run(False, self.state)
        self.assertEqual((result["state"], result["would_fetch"]), ("plan", [os.fspath(self.state)]))
        self.assertEqual(self.call_lines(), [])

    def test_every_recorded_root_fetches_main_head_once(self):
        self.record(self.state)
        self.record(self.state / "cmux-ci-2")
        (self.state / "cmux-ci-3").mkdir()  # no job has recorded this root
        head = self.commit()
        result = sp.run(True, self.state)
        self.assertEqual((result["state"], result["head"]), ("applied", head))
        calls = self.call_lines()
        self.assertEqual([line.split()[:3] for line in calls],
                         [["prefetch", os.fspath(self.state), head],
                          ["prefetch", os.fspath(self.state / "cmux-ci-2"), head]])
        self.assertTrue(calls[0].endswith("git=" + os.fspath(self.state / ".prefetch/cmux.git")))
        self.assertIn(" url=https://ci-cache.cmux.com ", calls[0])
        # Both roots have HEAD's own seed: the next run only fetches git.
        self.assertEqual(sp.run(True, self.state)["state"], "current")
        self.assertEqual(len(self.call_lines()), 2)
        # A new main commit fetches again, from freshly extracted scripts.
        newer = self.commit()
        self.assertEqual(sp.run(True, self.state)["head"], newer)
        self.assertEqual(len(self.call_lines()), 4)
        self.assertEqual(sorted(p.name for p in (self.state / ".prefetch").glob("scripts-*")),
                         sorted({f"scripts-{head[:12]}", f"scripts-{newer[:12]}"}))

    def test_a_seed_behind_head_is_retried_until_heads_own_lands(self):
        self.record(self.state)
        os.environ["FAKE_DISTANCE"] = "1"
        sp.run(True, self.state)
        sp.run(True, self.state)
        self.assertEqual(len(self.call_lines()), 2)
        os.environ["FAKE_DISTANCE"] = "0"
        sp.run(True, self.state)
        self.assertEqual(sp.run(True, self.state)["state"], "current")
        self.assertEqual(len(self.call_lines()), 3)

    def test_a_broken_mirror_is_rebuilt_on_the_next_run(self):
        self.record(self.state)
        sp.run(True, self.state)
        # A fetch killed mid-way leaves a lock that makes every later fetch fail.
        (self.state / ".prefetch/cmux.git/shallow.lock").write_text("")
        self.commit()
        failed = sp.run(True, self.state)
        self.assertEqual(failed["state"], "error")
        self.assertIn("shallow.lock", failed["reason"])
        self.assertEqual(sp.run(True, self.state)["state"], "applied")

    def test_a_failed_prefetch_is_reported_and_retried(self):
        self.record(self.state)
        (self.repo / "scripts/ci/seed_derived_data.py").write_text("import sys\nsys.exit('boom')\n")
        self.commit()
        result = sp.run(True, self.state)
        outcome = result["results"][os.fspath(self.state)]
        self.assertEqual(outcome["fetched"], "false")
        self.assertIn("boom", outcome["reason"])
        self.assertNotEqual(sp.run(True, self.state)["state"], "current")


    def test_a_killed_run_stops_its_download(self):
        # launchd's bootout signals only the job's own process group; the download runs in another.
        base = Path(self.tmp.name)
        scripts = base / "fake-scripts"
        (scripts / "scripts/ci").mkdir(parents=True)
        pidfile = base / "download.pid"
        (scripts / "scripts/ci/seed_derived_data.py").write_text(
            "import os, subprocess, sys, time\n"
            "d = subprocess.Popen(['sleep', '300'])\n"
            f"open({os.fspath(pidfile)!r}, 'w').write(str(d.pid))\n"
            "time.sleep(300)\n")
        runner = subprocess.Popen([sys.executable, "-c",
                                   "import importlib.machinery, sys; from pathlib import Path; "
                                   f"m = importlib.machinery.SourceFileLoader('p', {os.fspath(ROOT / 'scripts/glaeda-seed-prefetch')!r}).load_module(); "
                                   f"m.prefetch(Path({os.fspath(scripts)!r}), Path({os.fspath(base)!r}), 'x', Path({os.fspath(base / 'm')!r}))"])
        deadline = time.time() + 20
        while not (pidfile.exists() and pidfile.read_text()) and time.time() < deadline:
            time.sleep(0.1)
        download = int(pidfile.read_text())
        runner.terminate()
        self.assertEqual(runner.wait(timeout=20), 128 + signal.SIGTERM)
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                os.kill(download, 0)
            except ProcessLookupError:
                break
            time.sleep(0.1)
        else:
            os.kill(download, signal.SIGKILL)
            self.fail("the download outlived the killed run")


if __name__ == "__main__":
    unittest.main()
