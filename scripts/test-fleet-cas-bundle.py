#!/usr/bin/env python3
import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("fleet_cas_bundle", ROOT / "scripts/fleet_cas_bundle.py")
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FleetCasBundleTest(unittest.TestCase):
    def make_archive(self) -> tuple[bytes, str]:
        files = {name: f"#!/bin/sh\necho {name}\n".encode() for name in module.FILES}
        source = "a" * 40
        manifest = {
            "schema": module.SCHEMA,
            "source": {"repository": "teamleaderleo/glaeda", "commit": source, "tree": "b" * 40},
            "target": "x86_64-unknown-linux-gnu",
            "toolchain": "rustc 1.97.1",
            "files": {name: {"sha256": hashlib.sha256(data).hexdigest(), "size": len(data)} for name, data in files.items()},
        }
        return module.archive_bytes(files, manifest), source

    def test_verify_and_stage_are_bounded_and_no_overwrite(self) -> None:
        raw, source = self.make_archive()
        manifest, files = module.verified_contents(raw, hashlib.sha256(raw).hexdigest(), source, "x86_64-unknown-linux-gnu")
        self.assertEqual(set(files), set(module.FILES))
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "bundle"
            archive = Path(temporary) / "candidate.tar.gz"
            archive.write_bytes(raw)
            self.assertEqual(module.stage(archive, destination, source, "x86_64-unknown-linux-gnu", False)["state"], "planned")
            self.assertFalse(destination.exists())
            self.assertEqual(module.stage(archive, destination, source, "x86_64-unknown-linux-gnu", True)["state"], "staged")
            self.assertEqual((destination / "fleet-cas").read_bytes(), files["fleet-cas"])
            self.assertEqual((destination / "fleet-cas").stat().st_mode & 0o777, 0o755)
            with self.assertRaises(module.BundleError):
                module.stage(archive, destination, source, "x86_64-unknown-linux-gnu", True)
            dangling = Path(temporary) / "dangling"
            dangling.symlink_to("missing")
            with self.assertRaises(module.BundleError):
                module.stage(archive, dangling, source, "x86_64-unknown-linux-gnu", True)

    def test_tampered_checksum_and_identity_are_rejected(self) -> None:
        raw, source = self.make_archive()
        with self.assertRaises(module.BundleError):
            module.verified_contents(raw[:-1], hashlib.sha256(raw).hexdigest(), source, "x86_64-unknown-linux-gnu")
        with self.assertRaises(module.BundleError):
            module.verified_contents(raw, hashlib.sha256(raw).hexdigest(), "c" * 40, "x86_64-unknown-linux-gnu")


if __name__ == "__main__":
    unittest.main()
