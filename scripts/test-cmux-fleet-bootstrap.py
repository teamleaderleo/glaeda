#!/usr/bin/env python3
import importlib.util
from pathlib import Path
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
        "checks": checks,
        "observed": {},
    }


class Tests(unittest.TestCase):
    def test_mac_ready(self):
        result = b.evaluate(
            observation(),
            ["cmux_macos_native_build", "artifact_cache"],
            "cmux-mac-build-large",
        )
        self.assertTrue(result["eligibleForEnrollment"])
        self.assertEqual(result["authority"], "observation_only")

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
                ["cmux_macos_test"],
                "cmux-linux-ci-medium",
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
