#!/usr/bin/env python3
"""Tests for glaeda-cmux-runner-fleet with fake ssh, scp and gh on PATH (nothing leaves this machine)."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOL = ROOT / "scripts" / "glaeda-cmux-runner-fleet"
TOKEN = "AAAREGTOKEN123"
MANIFEST = {
    "defaults": {"xcode": {"apps": [{"path": "/Applications/Xcode_26.6.app", "version": "26.6", "build": "17F113"}]},
                 "disk": {"min_free_gib": 100}, "people": {"secret": "never copied"}},
    "hosts": {
        "mini-a": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4pro-48"},
        "mini-b": {"class": "std", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4pro-48"},
        "mini-light": {"class": "light", "availability": "dedicated", "roles": ["ci-runner"], "hardware": "m4-16"},
        "cache": {"class": "std", "availability": "dedicated", "roles": ["cache-host"], "hardware": "m4pro-48"},
    },
}

# Each fake appends one JSON line per call: argv, and for ssh the stdin it got (a token or the hook source).
FAKE_SSH = """#!{python}
import json, os, sys, time
log = os.environ["FAKE_LOG"]
args = sys.argv[1:]
while args and args[0] == "-o":
    args = args[2:]
host, remote = args[0], " ".join(args[1:])
data = sys.stdin.read()
json.dump({{"tool": "ssh", "host": host, "remote": remote, "stdin": data[:40]}}, open(log, "a")); open(log, "a").write("\\n")
if " check " in remote:
    if host in os.environ.get("FAKE_ELIGIBLE", "").split(","):
        print("glaeda-cmux-runner-hook: eligible"); sys.exit(0)
    print("glaeda-cmux-runner-hook: refused: node not eligible (no Glaeda enrollment on this mini)"); sys.exit(1)
if "--apply" in remote:
    time.sleep(float(os.environ.get("FAKE_APPLY_S", "0")))
    k = remote.split("--instance ")[1].split()[0]
    ok = data.strip() == "{token}"
    print(json.dumps({{"actions": [{{"kind": "register", "state": "create" if ok else "failed",
                                    "name": host + "-glaeda" + ("" if k == "0" else "-" + k)}},
                                   {{"kind": "verify", "state": "ok" if ok else "failed"}}]}}))
    sys.exit(0 if ok else 1)
sys.exit(0)
"""
FAKE_SCP = """#!{python}
import json, os, sys
json.dump({{"tool": "scp", "argv": sys.argv[1:]}}, open(os.environ["FAKE_LOG"], "a")); open(os.environ["FAKE_LOG"], "a").write("\\n")
dest = sys.argv[-1]
if dest.endswith("mini-fleet.json"):
    src = sys.argv[-2]
    open(os.environ["FAKE_LOG"] + "." + dest.split(":")[0] + ".manifest", "w").write(open(src).read())
"""
FAKE_GH = """#!{python}
import json, os, sys
json.dump({{"tool": "gh", "argv": sys.argv[1:]}}, open(os.environ["FAKE_LOG"], "a")); open(os.environ["FAKE_LOG"], "a").write("\\n")
print("{token}")
"""


class FleetTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        bin_dir = self.dir / "bin"
        bin_dir.mkdir()
        for name, body in (("ssh", FAKE_SSH), ("scp", FAKE_SCP), ("gh", FAKE_GH)):
            path = bin_dir / name
            path.write_text(body.format(python=sys.executable, token=TOKEN))
            path.chmod(0o755)
        self.manifest = self.dir / "mini-fleet.json"
        self.manifest.write_text(json.dumps(MANIFEST))
        self.log = self.dir / "calls.jsonl"
        self.env = {"PATH": f"{bin_dir}:/usr/bin:/bin", "HOME": os.fspath(self.dir), "FAKE_LOG": os.fspath(self.log),
                    "FAKE_ELIGIBLE": "mini-a,mini-b"}

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def fleet(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, os.fspath(TOOL), "--manifest", os.fspath(self.manifest),
                               "--output", "json", *args], capture_output=True, text=True, timeout=120,
                              env={**self.env, **env}, check=False)

    def calls(self) -> list[dict]:
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_plan_is_read_only_and_skips_ineligible(self) -> None:
        result = self.fleet()
        self.assertEqual(result.returncode, 0, result.stderr)
        doc = json.loads(result.stdout)
        rows = {r["member"]: r for r in doc["members"]}
        self.assertEqual(set(rows), {"mini-a", "mini-b", "mini-light"})
        self.assertIn("cache", doc["skipped"])
        self.assertEqual(rows["mini-a"]["action"], "would apply")
        self.assertEqual(rows["mini-light"]["action"], "skip (not eligible)")
        self.assertEqual((rows["mini-a"]["runners"], rows["mini-light"]["runners"]), (4, 2))
        tools = {c["tool"] for c in self.calls()}
        self.assertEqual(tools, {"ssh"}, "a plan only runs the gate")
        checks = [c for c in self.calls() if " check " in c["remote"]]
        self.assertTrue(all(c["stdin"].startswith("#!/usr/bin/env python3") for c in checks), "hook sent on stdin")
        self.assertTrue(all("--fleet-class m4pro-48" in c["remote"] or "m4-16" in c["remote"] for c in checks))

    def test_apply_every_instance_in_parallel_with_piped_tokens(self) -> None:
        start = time.monotonic()
        result = self.fleet("--apply", FAKE_APPLY_S="2")
        elapsed = time.monotonic() - start
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        rows = {r["member"]: r for r in json.loads(result.stdout)["members"]}
        self.assertEqual([i["name"] for i in rows["mini-a"]["instances"]],
                         ["mini-a-glaeda", "mini-a-glaeda-1", "mini-a-glaeda-2", "mini-a-glaeda-3"])
        self.assertTrue(all(i["state"] == "create/ok" for r in ("mini-a", "mini-b") for i in rows[r]["instances"]))
        self.assertNotIn("instances", rows["mini-light"])
        self.assertLess(elapsed, 8, "8 instances of 2 s each must run at once, not one after another")
        calls = self.calls()
        self.assertNotIn(TOKEN, json.dumps([c.get("argv", c.get("remote")) for c in calls]), "token only on stdin")
        applies = [c for c in calls if c["tool"] == "ssh" and "--apply" in c["remote"]]
        self.assertEqual(len(applies), 8)
        self.assertTrue(all(c["stdin"].strip() == TOKEN for c in applies))
        self.assertEqual(sum(1 for c in calls if c["tool"] == "gh"), 8, "one token per instance")
        staged = json.loads((self.dir / "calls.jsonl.mini-a.manifest").read_text())
        self.assertEqual(set(staged["hosts"]), {"mini-a"})
        self.assertEqual(set(staged["defaults"]), {"xcode", "disk"}, "no other hosts, keys or people")

    def test_only_named_hosts_and_include_ineligible_stopped(self) -> None:
        result = self.fleet("--apply", "--hosts", "mini-light", "--include-ineligible", "--skip-launchctl")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        rows = json.loads(result.stdout)["members"]
        self.assertEqual([r["member"] for r in rows], ["mini-light"])
        self.assertEqual(len(rows[0]["instances"]), 2)
        applies = [c for c in self.calls() if c["tool"] == "ssh" and "--apply" in c["remote"]]
        self.assertTrue(applies and all("--skip-launchctl" in c["remote"] for c in applies))
        self.assertFalse(any(c.get("host") in ("mini-a", "mini-b") for c in self.calls()))

    def test_a_failed_instance_fails_the_run(self) -> None:
        bad = self.dir / "bin" / "gh"
        bad.write_text(f"#!{sys.executable}\nprint('not a token!')\n")
        result = self.fleet("--apply", "--hosts", "mini-a")
        self.assertEqual(result.returncode, 1)
        states = [i["state"] for i in json.loads(result.stdout)["members"][0]["instances"]]
        self.assertTrue(all("no registration token" in s for s in states))
        self.assertFalse(any(c["tool"] == "ssh" and "--apply" in c["remote"] for c in self.calls()))


if __name__ == "__main__":
    unittest.main()
