#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import json
import tempfile
import unittest

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
