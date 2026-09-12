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

    def test_explanation_is_read_only_and_tracks_source_without_skipping_work(self):
        plan = self.plan()
        self.assertEqual(apple.explain(plan)["explanation"]["source_observation"], "no_comparable_build")
        self.assertFalse((self.root / ".glaeda").exists())
        receipt = self.run_plan(plan)
        self.assertTrue(all(value >= 0 for value in receipt["timings_seconds"].values()))
        advice = apple.explain(plan)
        self.assertEqual(advice["last_build_timings_seconds"], receipt["timings_seconds"])
        self.assertEqual(advice["explanation"]["source_observation"], "working_tree_requires_native_validation")
        self.assertFalse(advice["result_reuse"])
        (self.root / "Feature.swift").write_text("// edited\n")
        self.assertEqual(self.plan()["key"], plan["key"])
        self.assertEqual(apple.explain(self.plan())["explanation"]["next_action"], "run_native_build")
        subprocess.run(["/usr/bin/git", "-C", str(self.root), "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                        "commit", "--quiet", "--allow-empty", "-m", "new revision"], check=True)
        self.assertEqual(apple.explain(self.plan())["explanation"]["source_observation"], "commit_changed")
        self.assertEqual(apple.explain(self.plan("new"))["explanation"]["source_observation"], "no_comparable_build")

    def test_explanation_filters_untrusted_timing_fields(self):
        plan = self.plan()
        self.run_plan(plan)
        with apple.store(plan) as state:
            receipt = apple.read_json(state, "last-run.json")
            receipt["timings_seconds"] = {"native_command": "private path", "store_and_lock": -1, "unexpected": "secret"}
            apple.write_json(state, "last-run.json", receipt)
        self.assertEqual(apple.explain(plan)["last_build_timings_seconds"], {})

    def test_optional_preparation_reuse_rechecks_inputs_outputs_and_lock(self):
        (self.root / "Package.swift").write_text("// manifest")
        self.tools["swift"] = str(self.script)
        config = {"schema_version": 1, "profiles": {"app": self.profile},
                  "preparations": {"app": {"engine": "swiftpm", "reuse": {
                      "inputs": ["Package.swift", "Package.resolved"],
                      "required_files": [{"cache": "products", "path": "result"}]}}}}
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        def prepare_again(project, name, generation, **options):
            return apple.prepare(project, name, generation, lambda *args: self.tools, **options)
        def plan():
            return prepare_again(self.root, "app", "default", operation="dependencies")
        def run():
            return apple.execute(plan(), prepare_again, reuse_dependencies=True)
        self.assertEqual(run()["state"], "completed")
        self.assertEqual(run()["state"], "preparation_reused")
        (self.root / "Source.swift").write_text("// unrelated edit")
        reused = run()
        self.assertEqual(reused["state"], "preparation_reused")
        self.assertFalse(reused["result_reuse"])
        self.assertTrue(reused["native_build_validation_required"])
        (self.root / "Package.resolved").write_text("changed dependency input")
        self.assertEqual(run()["state"], "completed")
        artifact = Path(plan()["paths"]["products"]) / "result"
        artifact.write_text("damaged")
        self.assertEqual(run()["state"], "completed")
        artifact.unlink()
        self.assertEqual(run()["state"], "completed")
        with apple.store(plan()) as state, apple.lock(state):
            with self.assertRaises(apple.Refusal):
                run()

    def test_swift_driver_keeps_dispatch_name_and_checks_toolchain_identity(self):
        frontend = self.root / "swift-frontend"
        frontend.write_text("fixture")
        driver = self.root / "swift"
        driver.symlink_to(frontend)
        self.assertEqual(apple.swift_driver({"swift": str(frontend)}), str(driver))
        driver.unlink()
        driver.symlink_to(self.script)
        with self.assertRaisesRegex(apple.Refusal, "no longer matches"):
            apple.swift_driver({"swift": str(frontend)})

    def test_dependencies_reuse_paths_and_keep_build_receipt_separate(self):
        build_plan = self.plan()
        build = self.run_plan(build_plan)
        (self.root / "Package.swift").write_text("// fixture")
        self.tools["swift"] = str(self.script)
        self.write_config()
        config_path = self.root / "glaeda.apple.json"
        config = json.loads(config_path.read_text())
        config["preparations"] = {"app": {"engine": "swiftpm"}}
        config_path.write_text(json.dumps(config))
        def prepare_again(project, name, generation, **options):
            return apple.prepare(project, name, generation, lambda *args: self.tools, **options)
        normal = prepare_again(self.root, "app", "default")
        plan = prepare_again(self.root, "app", "default", operation="dependencies")
        self.assertEqual(normal["key"], plan["key"])
        self.assertEqual(normal["paths"], plan["paths"])
        self.assertEqual(plan["argv"][-1], "resolve")
        receipt = apple.execute(plan, prepare_again)
        self.assertEqual(receipt["operation"], "dependencies")
        with apple.store(plan) as state:
            self.assertEqual(apple.read_json(state, "last-run.json")["run_id"], build["run_id"])
            self.assertEqual(apple.read_json(state, "last-dependencies.json")["run_id"], receipt["run_id"])
        config["preparations"]["app"]["package"] = "missing"
        config_path.write_text(json.dumps(config))
        with self.assertRaises((apple.Refusal, FileNotFoundError)):
            apple.execute(plan, prepare_again)

    def test_xcode_dependency_plan_is_read_only_and_recipe_changes_revalidate(self):
        (self.root / "App.xcodeproj").mkdir()
        config = {"schema_version": 1, "profiles": {"app": self.profile},
                  "preparations": {"app": {"engine": "xcode", "project": "App.xcodeproj", "scheme": "App"}}}
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        def prepare_again(project, name, generation, **options):
            return apple.prepare(project, name, generation, lambda *args: self.tools, **options)
        plan = prepare_again(self.root, "app", "default", operation="dependencies")
        self.assertEqual(plan["argv"][-1], "-resolvePackageDependencies")
        self.assertEqual(apple.inspect(plan)["state"], "cold")
        self.assertFalse((self.root / ".glaeda").exists())
        config["preparations"]["app"]["scheme"] = "Other"
        (self.root / "glaeda.apple.json").write_text(json.dumps(config))
        with self.assertRaisesRegex(apple.Refusal, "changed during admission"):
            apple.execute(plan, prepare_again)

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

    def test_dependency_recovery_preserves_build_receipt(self):
        plan = self.plan()
        build = self.run_plan(plan)
        with apple.store(plan) as state:
            apple.write_json(state, "inflight.json", {"run_id": "deps", "pgid": None,
                             "cache_key": plan["key"], "operation": "dependencies"})
        with patch.object(apple, "group_absent", return_value=True):
            recovered = apple.recover(plan, "deps")
        self.assertEqual(recovered["operation"], "dependencies")
        with apple.store(plan) as state:
            self.assertEqual(apple.read_json(state, "last-run.json"), build)
            self.assertEqual(apple.read_json(state, "last-dependencies.json"), recovered)
        with self.assertRaisesRegex(apple.Refusal, "interrupted"):
            apple.inspect(plan)

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
