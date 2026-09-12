#!/usr/bin/env python3
"""Native Apple build policy tests; no Xcode, signing, or external services required."""

import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("apple_build", Path(__file__).with_name("apple_build.py"))
apple = importlib.util.module_from_spec(spec)
spec.loader.exec_module(apple)


class AppleBuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        subprocess.run(["/usr/bin/git", "init", "--quiet", str(self.root)], check=True)
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "--quiet", "--allow-empty", "-m", "fixture"], check=True)
        self.script = self.root / "build.sh"
        self.script.write_text('#!/bin/sh\nmkdir -p "$BUILD_DIR"\nprintf ok > "$BUILD_DIR/result"\nprintf private-output\n')
        self.script.chmod(0o755)
        self.profile = {"engine": "script", "executable": "build.sh", "environment": {"BUILD_DIR": "{products}"}}
        self.tools = {"developer": "/test/Xcode", "swift": "/test/swift", "xcodebuild": "/test/xcodebuild",
                      "swift_version": "Swift test", "xcode_version": "Xcode test", "sdk": "/test/SDK", "sdk_build": "test1", "host": "arm64"}
        self.write_config()

    def tearDown(self):
        self.temp.cleanup()

    def write_config(self):
        (self.root / "glaeda.apple.json").write_text(json.dumps({"schema_version": 1, "profiles": {"app": self.profile}}))

    def plan(self, generation="default"):
        return apple.prepare(self.root, "app", generation, lambda environment, sdk: self.tools)

    def run_plan(self, plan):
        return apple.execute(plan, lambda *args: self.plan(args[2]))

    def test_plan_is_read_only_and_source_edits_preserve_incremental_paths(self):
        plan = self.plan()
        self.assertEqual(apple.inspect(plan)["state"], "cold")
        self.assertFalse((self.root / ".glaeda").exists())
        (self.root / "Feature.swift").write_text("// changed source\n")
        self.assertEqual(plan["key"], self.plan()["key"])
        self.assertNotEqual(plan["key"], self.plan("cold-reset")["key"])

    def test_settings_sdk_toolchain_and_recipe_separate_generations(self):
        key = self.plan()["key"]
        self.tools["sdk_build"] = "test2"
        self.assertNotEqual(key, self.plan()["key"])
        key = self.plan()["key"]
        self.profile["environment"]["CONFIG"] = "release"
        self.write_config()
        self.assertNotEqual(key, self.plan()["key"])
        key = self.plan()["key"]
        self.script.write_text(self.script.read_text() + "# new build recipe\n")
        self.assertNotEqual(key, self.plan()["key"])

    def test_repeated_build_executes_and_retains_native_state(self):
        plan = self.plan()
        first = self.run_plan(plan)
        self.assertEqual(first["exit_code"], 0)
        product = Path(plan["paths"]["products"])
        (product / "native-cache").write_text("warm")
        second = self.run_plan(self.plan())
        self.assertEqual(second["exit_code"], 0)
        self.assertEqual((product / "native-cache").read_text(), "warm")
        self.assertNotEqual(first["run_id"], second["run_id"])
        self.assertFalse(second["result_reuse"])
        self.assertEqual(apple.inspect(plan)["state"], "prepared")
        log = self.root / ".glaeda/apple-build" / ("run-" + second["run_id"] + ".log")
        self.assertEqual(log.read_text(), "private-output")
        self.assertNotIn(str(self.root), json.dumps(second))
        self.assertEqual(log.stat().st_mode & 0o777, 0o600)

    def test_parent_secrets_and_build_overrides_do_not_leak(self):
        with patch.dict(os.environ, {"SECRET_TOKEN": "secret", "CONFIG": "ambient-release", "SDKROOT": "/wrong"}):
            plan = self.plan()
        for key in ("SECRET_TOKEN", "CONFIG", "SDKROOT"):
            self.assertNotIn(key, plan["environment"])

    def test_project_lock_refuses_second_builder(self):
        plan = self.plan()
        with apple.store(plan, True) as state, apple.lock(state):
            with self.assertRaisesRegex(apple.Refusal, "another Glaeda"):
                self.run_plan(plan)
        self.assertFalse(Path(plan["paths"]["products"]).exists())

    def test_revalidation_precedes_cache_mutation(self):
        plan = self.plan()
        self.tools["swift_version"] = "changed"
        with self.assertRaisesRegex(apple.Refusal, "changed during admission"):
            self.run_plan(plan)
        self.assertFalse(Path(plan["paths"]["products"]).exists())

    def test_quarantine_published_before_lock_admission_prevents_execution(self):
        plan = self.plan()
        original_lock = apple.lock

        @contextlib.contextmanager
        def prior_owner_quarantines(state):
            with original_lock(state):
                apple.write_json(state, "quarantine-" + plan["key"] + ".json", {"state": "interrupted"})
                yield

        with patch.object(apple, "lock", prior_owner_quarantines):
            with self.assertRaisesRegex(apple.Refusal, "interrupted"):
                self.run_plan(plan)
        self.assertFalse(Path(plan["paths"]["products"]).exists())

    def test_unmarked_state_and_symlinks_are_not_adopted(self):
        plan = self.plan()
        (self.root / ".glaeda").mkdir()
        (self.root / ".glaeda/apple-build").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(OSError):
            apple.inspect(plan)
        (self.root / ".glaeda/apple-build").unlink()
        (self.root / ".glaeda/apple-build").mkdir()
        with self.assertRaisesRegex(apple.Refusal, "incomplete"):
            self.run_plan(plan)

    def test_interrupted_build_blocks_then_quarantines_after_exact_recovery(self):
        plan = self.plan()
        child = subprocess.Popen(["/usr/bin/true"], start_new_session=True)
        child.wait()
        marker = {"schema_version": 1, "run_id": "interrupted", "pgid": child.pid, "cache_key": plan["key"]}
        with apple.store(plan, True) as state:
            apple.write_json(state, "inflight.json", marker)
        with self.assertRaisesRegex(apple.Refusal, "unfinished"):
            self.run_plan(plan)
        with self.assertRaises(apple.Refusal):
            apple.recover(plan, "wrong-id")
        recovered = apple.recover(plan, "interrupted")
        self.assertEqual(recovered["state"], "interruption_recovered")
        with self.assertRaisesRegex(apple.Refusal, "interrupted"):
            self.run_plan(plan)
        self.assertEqual(self.run_plan(self.plan("rebuild"))["exit_code"], 0)

    def test_recovery_does_not_release_a_present_process_group(self):
        plan = self.plan()
        with apple.store(plan, True) as state:
            apple.write_json(state, "inflight.json", {"run_id": "live", "pgid": os.getpgrp(), "cache_key": plan["key"]})
        with self.assertRaises(apple.Refusal):
            apple.recover(plan, "live")

    def test_native_adapters_bind_their_cache_locations(self):
        (self.root / "Package.swift").write_text("// fixture")
        self.profile = {"engine": "swiftpm", "product": "Example", "configuration": "release"}
        self.write_config()
        plan = self.plan()
        self.assertIn(plan["paths"]["scratch"], plan["argv"])
        self.assertIn("release", plan["argv"])
        (self.root / "Example.xcodeproj").mkdir()
        self.profile = {"engine": "xcode", "project": "Example.xcodeproj", "scheme": "Example", "configuration": "Debug",
                        "destination": "platform=macOS", "settings": {"CODE_SIGNING_ALLOWED": "NO"}}
        self.write_config()
        plan = self.plan()
        self.assertIn(plan["paths"]["derived_data"], plan["argv"])
        self.assertIn("CODE_SIGNING_ALLOWED=NO", plan["argv"])

    def test_invalid_configuration_has_no_state_effect(self):
        self.profile["arguments"] = ["{unknown}"]
        self.write_config()
        with self.assertRaises(apple.Refusal):
            self.plan()
        self.assertFalse((self.root / ".glaeda").exists())

    def test_malformed_interruption_record_has_a_bounded_public_refusal(self):
        plan = self.plan()
        with apple.store(plan, True) as state:
            apple.write_json(state, "inflight.json", {})
        output = io.StringIO()
        with patch.object(apple, "prepare", return_value=plan), patch("sys.argv", ["apple-build", "plan"]), contextlib.redirect_stderr(output):
            self.assertEqual(apple.main(), 2)
        refusal = json.loads(output.getvalue())
        self.assertEqual(refusal["state"], "refused")
        self.assertNotIn(str(self.root), output.getvalue())
        self.assertFalse(Path(plan["paths"]["products"]).exists())


if __name__ == "__main__":
    unittest.main()
