#!/usr/bin/env python3

import argparse
import json
import os
import tempfile
import unittest
from pathlib import Path

import project_disk_physical_receipt as receipt


DIGEST_A = "sha256:" + "a" * 64
DIGEST_B = "sha256:" + "b" * 64


class Fixture:
    def __init__(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="smolrunner-project-disk-receipt-")
        self.root = Path(self.temp.name)
        self.disk = self.root / "opaque-disk-directory"
        self.disk.mkdir()

        self.backing = self.disk / "alpha"
        with self.backing.open("wb") as handle:
            handle.truncate(1024 * 1024)
        self.lock = self.disk / "beta"
        self.lock.symlink_to("../opaque-resident-instance")
        self.metadata = self.disk / "gamma"
        self.metadata.write_text("opaque-lock-metadata\n", encoding="utf-8")

        self.host = self.root / "host.json"
        self.host.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "receipt_type": "smolrunner-lima-host-identity-observation",
                    "instance": "resident-a",
                    "lima_host_identity": DIGEST_A,
                    "lima_request_identity": DIGEST_B,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        self.disk_json = self.root / "disk.json"
        self.disk_json.write_text(
            json.dumps({"opaque_disk": "disk-a", "opaque_attachment": "resident-a"}) + "\n",
            encoding="utf-8",
        )
        self.instance_json = self.root / "instance.json"
        self.instance_json.write_text(
            json.dumps({"name": "resident-a", "status": "Running"}) + "\n",
            encoding="utf-8",
        )
        self.guest_stat = self.root / "guest-stat.txt"
        self.guest_stat.write_text("2049:12345\n", encoding="ascii")
        self.mountinfo = self.root / "mountinfo.txt"
        self.mountinfo.write_bytes(
            b"41 30 8:1 / / rw,relatime - ext4 /dev/root rw\n"
            b"77 41 8:16 / /srv/project rw,nosuid,nodev - xfs /dev/vdb rw,attr2,inode64\n"
        )

    def args(self, **overrides):
        values = {
            "host_identity_json": str(self.host),
            "disk_directory": str(self.disk),
            "lima_disk_json": str(self.disk_json),
            "resident_instance_json": str(self.instance_json),
            "resident_sandbox_instance": "resident-a",
            "guest_project_stat": str(self.guest_stat),
            "guest_mountinfo": str(self.mountinfo),
            "guest_project_mountpoint": "/srv/project",
            "project_identity": "github.com/teamleaderleo/smolrunner",
            "project_disk_id": "project-disk-a",
            "project_disk_generation": 7,
            "project_disk_revision": 11,
            "attachment_generation": 5,
            "resident_sandbox_id": "sandbox-a",
            "resident_sandbox_generation": 3,
            "read_small_entry": [],
            "observed_backing_entry": None,
            "observed_lock_entry": None,
            "output": None,
        }
        values.update(overrides)
        return argparse.Namespace(**values)

    def close(self) -> None:
        self.temp.cleanup()


class ProjectDiskPhysicalReceiptTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()

    def tearDown(self) -> None:
        self.fixture.close()

    def test_first_capture_is_opaque_and_carries_exact_declared_generations(self) -> None:
        captured = receipt.capture(self.fixture.args())
        self.assertEqual(captured["authority"]["class"], "read_only_physical_observation")
        self.assertEqual(captured["authority"]["mutation_authority"], "none")
        self.assertEqual(captured["authority"]["smolrunner_ownership_authority"], "unresolved")
        self.assertIsNone(captured["operator_role_labels"])
        self.assertEqual(
            captured["declared_binding"],
            {
                "source": "declared_from_exact_p1_lease_state",
                "project_identity": "github.com/teamleaderleo/smolrunner",
                "project_disk_id": "project-disk-a",
                "project_disk_generation": 7,
                "project_disk_revision": 11,
                "attachment_generation": 5,
                "resident_sandbox_id": "sandbox-a",
                "resident_sandbox_generation": 3,
            },
        )

        entries = {
            entry["name"]["utf8"]: entry for entry in captured["disk_directory"]["entries"]
        }
        self.assertEqual(set(entries), {"alpha", "beta", "gamma"})
        self.assertEqual(entries["alpha"]["kind"], "regular_file")
        self.assertEqual(entries["alpha"]["metadata"]["logical_bytes"], 1024 * 1024)
        self.assertIsInstance(entries["alpha"]["metadata"]["allocated_bytes"], int)
        self.assertEqual(entries["beta"]["kind"], "symlink")
        self.assertEqual(entries["beta"]["symlink_target"]["utf8"], "../opaque-resident-instance")
        self.assertNotIn("explicit_small_file_bytes", entries["alpha"])
        self.assertNotIn("explicit_small_file_bytes", entries["gamma"])

        guest = captured["correlation"]["guest_project_filesystem"]
        self.assertEqual(guest["stat"], {"device": 2049, "inode": 12345})
        self.assertEqual(guest["mountinfo"]["device_major"], 8)
        self.assertEqual(guest["mountinfo"]["device_minor"], 16)
        self.assertEqual(guest["mountinfo"]["filesystem_type"], "xfs")

    def test_small_file_contents_require_explicit_exact_entry_request(self) -> None:
        captured = receipt.capture(self.fixture.args(read_small_entry=["gamma"]))
        entries = {
            entry["name"]["utf8"]: entry for entry in captured["disk_directory"]["entries"]
        }
        evidence = entries["gamma"]["explicit_small_file_bytes"]
        self.assertEqual(evidence["utf8"], "opaque-lock-metadata\n")
        self.assertTrue(evidence["sha256"].startswith("sha256:"))
        self.assertNotIn("explicit_small_file_bytes", entries["alpha"])

    def test_role_labels_are_only_explicit_and_must_name_captured_entries(self) -> None:
        captured = receipt.capture(
            self.fixture.args(observed_backing_entry="alpha", observed_lock_entry="beta")
        )
        self.assertEqual(
            captured["operator_role_labels"],
            {
                "source": "explicit_operator_observation",
                "observed_backing_entry": "alpha",
                "observed_lock_entry": "beta",
            },
        )
        with self.assertRaises(receipt.ReceiptError):
            receipt.capture(
                self.fixture.args(observed_backing_entry="invented", observed_lock_entry="beta")
            )

    def test_same_name_replacement_changes_exact_entry_observation(self) -> None:
        directory_fd = os.open(self.fixture.disk, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            before, _ = receipt._entry_snapshot(directory_fd, "alpha")
            held_fd = os.open("alpha", os.O_RDONLY, dir_fd=directory_fd)
            try:
                os.rename(self.fixture.backing, self.fixture.disk / "alpha-old")
                self.fixture.backing.write_bytes(b"replacement")
                after, _ = receipt._entry_snapshot(directory_fd, "alpha")
                held = receipt._snapshot(os.fstat(held_fd))
                self.assertFalse(receipt._same_observation(before, after))
                self.assertEqual(
                    (before["device"], before["inode"]),
                    (held["device"], held["inode"]),
                )
                self.assertNotEqual(
                    (after["device"], after["inode"]),
                    (held["device"], held["inode"]),
                )
            finally:
                os.close(held_fd)
        finally:
            os.close(directory_fd)

    def test_duplicate_keys_unknown_fields_and_instance_mismatch_are_refused(self) -> None:
        duplicate = self.fixture.root / "duplicate.json"
        duplicate.write_text('{"name":"a","name":"b"}\n', encoding="utf-8")
        with self.assertRaises(receipt.ReceiptError):
            receipt._read_json_evidence(duplicate, "duplicate fixture")

        captured = receipt.capture(self.fixture.args())
        captured["guessed_backing_file"] = "alpha"
        with self.assertRaises(receipt.ReceiptError):
            receipt.validate_receipt(captured)

        with self.assertRaises(receipt.ReceiptError):
            receipt.capture(self.fixture.args(resident_sandbox_instance="resident-b"))

    def test_mountinfo_exact_mountpoint_decodes_reviewed_escapes(self) -> None:
        escaped = self.fixture.root / "escaped-mountinfo.txt"
        escaped.write_bytes(
            b"77 41 8:16 / /srv/project\\040space rw - xfs /dev/vdb rw\n"
        )
        parsed = receipt._parse_mountinfo(escaped, "/srv/project space")
        self.assertEqual(parsed["mountpoint"]["utf8"], "/srv/project space")
        self.assertEqual((parsed["device_major"], parsed["device_minor"]), (8, 16))

    def test_receipt_round_trip_parser_preserves_observations(self) -> None:
        captured = receipt.capture(self.fixture.args())
        path = self.fixture.root / "receipt.json"
        path.write_bytes(receipt._encoded_json(captured))
        decoded = receipt.validate_file(path)
        self.assertEqual(decoded, captured)

    def test_private_output_is_create_new_and_mode_0600(self) -> None:
        captured = receipt.capture(self.fixture.args())
        output = self.fixture.root / "private-receipt.json"
        receipt._write_json(captured, str(output))
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)
        with self.assertRaises(FileExistsError):
            receipt._write_json(captured, str(output))

    def test_parser_refuses_tampered_explicit_small_file_digest(self) -> None:
        captured = receipt.capture(self.fixture.args(read_small_entry=["gamma"]))
        entry = next(
            item for item in captured["disk_directory"]["entries"] if item["name"]["utf8"] == "gamma"
        )
        entry["explicit_small_file_bytes"]["sha256"] = DIGEST_A
        with self.assertRaises(receipt.ReceiptError):
            receipt.validate_receipt(captured)


if __name__ == "__main__":
    unittest.main()
