#!/usr/bin/env python3
import importlib.util
import io
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("bundle", Path(__file__).with_name("fleet_bundle.py"))
b = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(b)
SOURCE = "a" * 40
TARGET = "aarch64-apple-darwin"


def fixture():
    payload = {name: b"fixture\n" for name in b.PAYLOAD}
    manifest = {
        "schema": b.SCHEMA,
        "source": {"repository": "teamleaderleo/glaeda", "commit": SOURCE, "tree": "b" * 40},
        "target": TARGET, "version": "0.1.0", "toolchain": "rustc fixture",
        "channel": "candidate", "automaticUpdateAuthorized": False,
        "files": {name: {"sha256": b.sha256(raw), "size": len(raw)} for name, raw in payload.items()},
    }
    return payload, manifest


class BundleTests(unittest.TestCase):
    def test_round_trip_is_deterministic_and_contains_complete_fleet_tools(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        self.assertEqual(raw, b.archive_bytes(payload, manifest))
        self.assertEqual(b.verify(raw, b.sha256(raw), SOURCE, TARGET), manifest)
        self.assertIn("scripts/cmux_fleet_bootstrap.py", manifest["files"])
        self.assertIn("bin/glaeda", manifest["files"])

    def test_digest_source_target_and_inventory_must_match(self):
        payload, manifest = fixture()
        raw = b.archive_bytes(payload, manifest)
        for digest, source, target in (
            ("0" * 64, SOURCE, TARGET), (b.sha256(raw), "c" * 40, TARGET),
            (b.sha256(raw), SOURCE, "x86_64-unknown-linux-gnu"),
        ):
            with self.subTest(source=source, target=target, digest=digest):
                with self.assertRaises(b.BundleError):
                    b.verify(raw, digest, source, target)
        payload["bin/glaeda"] = b"tampered binary"
        raw = b.archive_bytes(payload, manifest)
        with self.assertRaisesRegex(b.BundleError, "inventory"):
            b.verify(raw, b.sha256(raw), SOURCE, TARGET)

    def test_manifest_cannot_grant_update_authority(self):
        for key, value in (("automaticUpdateAuthorized", True), ("channel", "stable"),
                           ("source", []), ("version", {}), ("unexpected", "field")):
            payload, manifest = fixture()
            manifest[key] = value
            raw = b.archive_bytes(payload, manifest)
            with self.subTest(key=key), self.assertRaises(b.BundleError):
                b.verify(raw, b.sha256(raw), SOURCE, TARGET)

    def test_archive_rejects_missing_foreign_duplicate_and_link_entries(self):
        for kind in ("missing", "foreign", "duplicate", "symlink", "hardlink", "mode"):
            payload, manifest = fixture()
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w:gz") as archive:
                members = list({**payload, "manifest.json": b.canonical(manifest)}.items())
                if kind == "missing":
                    members.pop()
                if kind == "foreign":
                    members.append(("../escape", b"bad"))
                if kind == "duplicate":
                    members.append(members[0])
                for index, (name, raw) in enumerate(members):
                    entry = tarfile.TarInfo(name)
                    entry.size = len(raw)
                    entry.mode = 0o755 if name == "bin/glaeda" else 0o644
                    if index == 0 and kind in ("symlink", "hardlink"):
                        entry.type = tarfile.SYMTYPE if kind == "symlink" else tarfile.LNKTYPE
                        entry.linkname = "../escape"
                        entry.size = 0
                    if index == 0 and kind == "mode":
                        entry.mode = 0o4777
                    archive.addfile(entry, io.BytesIO(raw))
            raw = output.getvalue()
            with self.subTest(kind=kind), self.assertRaises(b.BundleError):
                b.verify(raw, b.sha256(raw), SOURCE, TARGET)

    def test_source_requires_exact_clean_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.DEVNULL).decode().strip()
            git("init", "-q")
            (root / "source").write_text("source")
            git("add", "source")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
            source = git("rev-parse", "HEAD")
            self.assertEqual(b.source_identity(root, source)["commit"], source)
            with self.assertRaises(b.BundleError):
                b.source_identity(root, SOURCE)
            (root / "source").write_text("dirty")
            with self.assertRaisesRegex(b.BundleError, "clean"):
                b.source_identity(root, source)


if __name__ == "__main__":
    unittest.main()
