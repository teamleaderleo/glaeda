#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import json
import platform
import os
import subprocess
import tempfile
import unittest
from unittest import mock

MODULE_PATH = Path(__file__).with_name("cmux_fleet_bootstrap.py")
SPEC = importlib.util.spec_from_file_location("cmux_fleet_bootstrap", MODULE_PATH)
assert SPEC and SPEC.loader
b = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(b)

A = "sha256:" + "a" * 64
B = "sha256:" + "b" * 64


def observation(platform="macos", failed=()):
    checks = {
        "supportedOs": True,
        "hardwareCapability": True,
        "git": True,
        "glaedaExecutable": True,
        "diskAdmission": True,
        "profileRunnerInterpreter": True,
        "workloadToolPath": True,
    }
    if platform == "macos":
        checks.update(
            {"cmuxCheckout": True, "xcodePin": True, "unattendedPower": True}
        )
    else:
        checks.update(
            {
                "cmuxCheckout": True,
                "systemd": True,
                "bubblewrap": True,
                "cgroupV2": True,
                "pressureSignals": True,
                "memoryAdmission": True,
                "actionsPrerequisites": True,
            }
        )
    for key in failed:
        checks[key] = False
    return {
        "platform": platform,
        "architecture": "arm64" if platform == "macos" else "x86_64",
        "osVersionClass": "macos-26" if platform == "macos" else "ubuntu-24.04",
        "glaedaGeneration": A,
        "toolchainGeneration": B,
        "roleProfiles": {
            "cmux_macos_native_build" if platform == "macos" else "cmux_linux_ci":
                dict(
                    b.ROLE_PROFILES[
                        "cmux_macos_native_build" if platform == "macos" else "cmux_linux_ci"
                    ]
                )
        },
        "checks": checks,
        "observed": {},
    }


class Tests(unittest.TestCase):
    def test_mac_ready(self):
        result = b.evaluate(
            observation(),
            ["cmux_macos_native_build"],
            "cmux-mac-build-large",
        )
        self.assertTrue(result["eligibleForEnrollment"])
        self.assertEqual(result["authority"], "observation_only")
        self.assertEqual(
            result["roleProfiles"],
            {"cmux_macos_native_build": dict(b.ROLE_PROFILES["cmux_macos_native_build"])},
        )

    def test_linux_ready(self):
        result = b.evaluate(
            observation("linux"),
            ["cmux_linux_ci"],
            "cmux-linux-ci-medium",
        )
        self.assertTrue(result["eligibleForEnrollment"])

    def test_failures_block_enrollment(self):
        result = b.evaluate(
            observation(failed=("xcodePin", "diskAdmission")),
            ["cmux_macos_native_build"],
            "cmux-mac-build-large",
        )
        self.assertFalse(result["eligibleForEnrollment"])
        self.assertEqual(result["blockingChecks"], ["diskAdmission", "xcodePin"])

    def test_wrong_os_role_refused(self):
        with self.assertRaisesRegex(b.BootstrapError, "incompatible"):
            b.evaluate(
                observation("linux"),
                ["cmux_macos_native_build"],
                "cmux-linux-ci-medium",
            )

    def test_reviewed_hardware_classes_have_minimums(self):
        self.assertTrue(
            b.hardware_class_ready(
                "macos",
                "cmux-mac-build-large",
                8,
                16,
            )
        )
        self.assertFalse(
            b.hardware_class_ready(
                "macos",
                "cmux-mac-build-large",
                4,
                16,
            )
        )
        self.assertTrue(
            b.hardware_class_ready(
                "linux",
                "cmux-linux-ci-medium",
                4,
                8,
            )
        )
        self.assertFalse(
            b.hardware_class_ready(
                "linux",
                "unknown",
                64,
                256,
            )
        )

    def test_role_without_workload_refused(self):
        with self.assertRaisesRegex(b.BootstrapError, "reviewed v1 acceptance workload"):
            b.evaluate(
                observation(),
                ["artifact_cache"],
                "cmux-mac-build-large",
            )

    def test_zig_version_compatibility_matches_cmux_policy(self):
        self.assertTrue(b.zig_version_compatible("0.16.0", "0.16.0"))
        self.assertTrue(b.zig_version_compatible("0.16.4", "0.16.0"))
        self.assertFalse(b.zig_version_compatible("0.15.9", "0.16.0"))
        self.assertFalse(b.zig_version_compatible("0.17.0", "0.16.0"))
        self.assertFalse(b.zig_version_compatible("nightly", "0.16.0"))

    def test_role_profiles_must_cover_exact_roles(self):
        observed = observation()
        observed["roleProfiles"] = {}
        with self.assertRaisesRegex(b.BootstrapError, "role profiles"):
            b.evaluate(
                observed,
                ["cmux_macos_native_build"],
                "cmux-mac-build-large",
            )

    def test_profile_registry_binds_role_generation_and_architecture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            registry = root / b.CMUX_PROFILE_REGISTRY
            registry.parent.mkdir(parents=True)
            registry.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "repository": b.CMUX_REPOSITORY,
                        "result_contract": b.CMUX_RESULT_CONTRACT,
                        "profiles": [
                            {
                                "id": "cmux.macos.dev-check",
                                "generation": 1,
                                "platform": {
                                    "os": "macos",
                                    "architectures": ["arm64"],
                                },
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            self.assertEqual(
                b.role_profiles(
                    root,
                    ["cmux_macos_native_build"],
                    "macos",
                    "arm64",
                ),
                {
                    "cmux_macos_native_build": dict(
                        b.ROLE_PROFILES["cmux_macos_native_build"]
                    )
                },
            )
            with self.assertRaisesRegex(b.BootstrapError, "incompatible"):
                b.role_profiles(
                    root,
                    ["cmux_macos_native_build"],
                    "macos",
                    "x86_64",
                )

    def test_command_output_keeps_column_zero(self):
        # `git submodule status` marks a checked-out submodule with a leading
        # space, so trimming it made every clean checkout look unready.
        output = b.run(["/bin/sh", "-c", "printf ' clean sub\\n-absent sub\\n'"])
        self.assertEqual(output.splitlines(), [" clean sub", "-absent sub"])

    def test_submodules_ready_on_a_real_checked_out_submodule(self):
        # Mocking `run` here would step over the bug: the leading space that
        # marks a checked-out submodule only survives if `run` leaves it.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            inner, outer = root / "inner", root / "outer"

            def git(*args, cwd):
                subprocess.run(
                    ["git", "-c", "protocol.file.allow=always",
                     "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                     *args],
                    cwd=cwd, check=True,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                         "HOME": str(root), "GIT_CONFIG_GLOBAL": "/dev/null",
                         "GIT_CONFIG_NOSYSTEM": "1"},
                )

            for path in (inner, outer):
                path.mkdir()
                git("init", "-q", "-b", "main", cwd=path)
                (path / "file").write_text("x", encoding="utf-8")
                git("add", "-A", cwd=path)
                git("commit", "-qm", "seed", cwd=path)
            git("submodule", "add", "-q", str(inner), "vendor", cwd=outer)
            git("commit", "-qm", "vendor", cwd=outer)

            self.assertTrue(b.cmux_submodules_ready(outer))
            self.assertTrue(b.cmux_checkout_clean(outer))

    def test_setup_artifacts_accept_either_ghosttykit_location(self):
        for location in b.CMUX_GHOSTTYKIT_LOCATIONS:
            with self.subTest(location=location), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                header = root / "ghostty/include/ghostty.h"
                header.parent.mkdir(parents=True)
                header.write_text("/* fixture */\n", encoding="utf-8")
                self.assertFalse(b.cmux_setup_artifacts_present(root))
                (root / location).mkdir(parents=True)
                self.assertTrue(b.cmux_setup_artifacts_present(root))

    def test_tools_only_on_the_operator_path_are_reported_invisible(self):
        # The build rebuilds PATH from a fixed list of system directories plus
        # an empty Cargo home, so a tool under the operator's home is one the
        # build cannot spend, however well the operator's shell resolves it.
        with tempfile.TemporaryDirectory() as temporary:
            operator_only = Path(temporary)
            tool = operator_only / "cmux-fixture-tool"
            tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            tool.chmod(0o755)
            with mock.patch.dict(os.environ, {"PATH": str(operator_only)}):
                # Still resolvable for probing: a node with a misplaced tool
                # must still produce a receipt naming everything else wrong.
                self.assertTrue(b.executable("cmux-fixture-tool").startswith("/"))
                self.assertEqual(
                    b.missing_workload_tools(("cmux-fixture-tool", "sh")),
                    ["cmux-fixture-tool"],
                )
            self.assertEqual(b.missing_workload_tools(("sh", "cat")), [])

    def test_linux_observation_reports_the_new_checks(self):
        # evaluate() is key-agnostic, so asserting on a hand-built fixture
        # proves nothing about what the collectors actually emit.
        if platform.system() != "Linux":
            self.skipTest("collect_linux observes a Linux host")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / ".git").mkdir()
            glaeda = root / "glaeda"
            glaeda.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            glaeda.chmod(0o755)
            outputs = {
                "git": "git version 2.51.0",
                "python3": "Python 3.14.0",
                "systemctl": "systemd 257 (257)\n+PAM",
                "bwrap": "bubblewrap 0.11.0",
            }
            with (
                mock.patch.object(b, "executable", side_effect=lambda name: f"/usr/bin/{name}"),
                mock.patch.object(
                    b, "run", side_effect=lambda argv, **kw: outputs[Path(argv[0]).name]
                ),
                mock.patch.object(b, "read_os_release", return_value=("ubuntu", "24.04")),
                mock.patch.object(b, "linux_memory_gib", return_value=64),
            ):
                observed = b.collect_linux(
                    root, glaeda, 1, ["cmux_linux_ci"], "cmux-linux-ci-medium"
                )
        self.assertIn("profileRunnerInterpreter", observed["checks"])
        self.assertIn("workloadToolPath", observed["checks"])
        self.assertEqual(
            observed["checks"]["profileRunnerInterpreter"], hasattr(os, "waitid")
        )
        self.assertEqual(
            observed["checks"]["workloadToolPath"],
            not observed["observed"]["toolsMissingFromWorkloadPath"],
        )

    def test_power_posture_parser(self):
        raw = (
            "Battery Power:\n"
            " sleep              1\n"
            "AC Power:\n"
            " sleep              0\n"
            " displaysleep       10\n"
        )
        self.assertTrue(b.mac_sleep_disabled_on_ac(raw))
        self.assertFalse(
            b.mac_sleep_disabled_on_ac(
                raw.replace("sleep              0", "sleep              1")
            )
        )


if __name__ == "__main__":
    unittest.main()
