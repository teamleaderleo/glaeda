#!/usr/bin/env python3

import concurrent.futures
import datetime as dt
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


MODULE_PATH = Path(__file__).with_name("github_resident_snapshot.py")
SPEC = importlib.util.spec_from_file_location("github_resident_snapshot", MODULE_PATH)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

BASE_TIME = dt.datetime(2026, 9, 21, 10, 0, tzinfo=dt.UTC)
GIT = Path("/usr/bin/git")
SSH_KEYGEN = Path("/usr/bin/ssh-keygen")


def fake_public_key(seed: str) -> str:
    return "ssh-ed25519 " + (seed * 68)[:68]


def trust(*, nodes=None):
    return {
        "document_type": MODULE.TRUST_DOCUMENT,
        "schema_version": 1,
        "repositories": ["teamleaderleo/glaeda"],
        "nodes": nodes
        or [
            {
                "id": "node-1111111111111111",
                "key_id": "key-aaaaaaaaaaaaaaaa",
                "ssh_public_key": fake_public_key("A"),
                "os_class": "linux",
                "architecture_class": "x86_64",
            }
        ],
    }


def capability(*, node_generation=7, glaeda="a", observed=BASE_TIME, expires=None, hot=True, os_class="linux", architecture="x86_64"):
    expires = expires or (observed + dt.timedelta(minutes=10))
    return {
        "schema": "glaeda-owned-workstation-capability/v1",
        "advisoryOnly": True,
        "authorizesDispatch": False,
        "authorizesExecution": False,
        "expiresAt": MODULE.format_time(expires),
        "node": {
            "architectureClass": architecture,
            "generation": node_generation,
            "id": "private-hostname-that-must-never-publish",
            "osClass": os_class,
        },
        "observedAt": MODULE.format_time(observed),
        "producer": {
            "glaedaRuntimeSha256": "sha256:" + glaeda * 64,
            "python": {"version": "3.14.7", "executableSha256": "sha256:" + "b" * 64},
            "workspaceCapabilitySha256": "sha256:" + "c" * 64,
        },
        "profiles": [
            {"class": "repo_query", "id": "repo-query/v1", "versionSha256": "sha256:" + "d" * 64},
            {"class": "verify_focused", "id": "verify-focused/v1", "versionSha256": "sha256:" + "e" * 64},
        ],
        "projects": [
            {
                "heatClass": "resident_hot" if hot else "resident_cold",
                "repository": "teamleaderleo/glaeda",
                "source": {"commitOid": "1" * 40, "treeOid": "2" * 40},
                "sourceObjectClass": "exact_commit_and_tree_present",
                "verificationProfiles": ["glaeda.required", "verify-focused/v1"],
            }
        ],
    }


def admission(outcome="ready", reason="compatible"):
    return {
        "document_type": "glaeda-owned-admission-observation",
        "schema_version": 1,
        "outcome": outcome,
        "reason": reason,
        "grants_authority": False,
        "authorizes_execution": False,
        "authorizes_redispatch": False,
    }


def request_state(state="running", receipt=None):
    return {
        "document_type": "glaeda-github-request-state-input",
        "schema_version": 1,
        "requests": [
            {
                "request_id": "req-1234567890abcdef",
                "source": {
                    "repository": "teamleaderleo/glaeda",
                    "commit_oid": "1" * 40,
                    "tree_oid": "2" * 40,
                },
                "profile": "verify-focused/v1",
                "state": state,
                "terminal_receipt_ref": receipt,
                "elapsed_class": "10s_to_1m",
                "estimate_class": "under_10s" if state == "running" else "unknown",
            }
        ],
    }


def build_unsigned(**changes):
    values = {
        "capability": capability(),
        "admission": admission(),
        "trust_value": trust(),
        "public_node_id": "node-1111111111111111",
        "producer_generation": 4,
        "snapshot_sequence": 8,
        "observed_at": BASE_TIME + dt.timedelta(seconds=5),
        "published_at": BASE_TIME + dt.timedelta(seconds=6),
        "maximum_useful_age_seconds": 300,
        "request_state": request_state(state="queued"),
    }
    values.update(changes)
    return MODULE.build_unsigned_snapshot(**values)


def fake_signed(unsigned):
    value = dict(unsigned)
    value["signature"] = {
        "algorithm": "sshsig-ed25519",
        "key_id": unsigned["payload"]["producer"]["key_id"],
        "namespace": MODULE.SIGNING_NAMESPACE,
        "value": "-----BEGIN SSH SIGNATURE-----\nfake\n-----END SSH SIGNATURE-----\n",
    }
    return value


class ResidentSnapshotPureTests(unittest.TestCase):
    def test_compose_is_bounded_sanitized_and_zero_authority(self):
        snapshot = build_unsigned()
        payload = snapshot["payload"]
        self.assertEqual(payload["freshness"]["observed_at"], MODULE.format_time(BASE_TIME))
        self.assertEqual(payload["node"]["id"], "node-1111111111111111")
        self.assertEqual(payload["node"]["availability_class"], "available")
        self.assertEqual(payload["node"]["pressure_class"], "low")
        self.assertEqual(payload["node"]["capacity_class"], "available")
        self.assertEqual(payload["projects"][0]["heat_class"], "resident_hot")
        self.assertEqual(payload["requests"][0]["state"], "queued")
        self.assertEqual(payload["authority"], {
            "advisory_only": True,
            "authorizes_dispatch": False,
            "authorizes_execution": False,
            "authorizes_host_selection": False,
            "authorizes_cleanup": False,
        })
        raw = MODULE.canonical_json(snapshot)
        self.assertLessEqual(len(raw), MODULE.MAX_NODE_BYTES)
        text = raw.decode()
        for forbidden in ("private-hostname", "/home/", "/Users/", "argv", "environment", "pid", "command"):
            self.assertNotIn(forbidden, text.lower())

    def test_admission_reduces_only_to_bounded_classes(self):
        expected = {
            ("ready", "compatible"): ("available", "low", "available", 0),
            ("wait", "reserved"): ("available", "unknown", "reserved", 1),
            ("wait", "node_held"): ("held", "unknown", "unknown", 0),
            ("wait", "node_draining"): ("draining", "unknown", "unknown", 0),
            ("wait", "pressure_high"): ("available", "high", "insufficient", 0),
            ("wait", "capacity_unavailable"): ("available", "unknown", "insufficient", 0),
            ("refused", "observation_unavailable"): ("unknown", "unknown", "unknown", 0),
        }
        for pair, classes in expected.items():
            with self.subTest(pair=pair):
                value = MODULE.admission_projection(admission(*pair))
                self.assertEqual(
                    (value["availability_class"], value["pressure_class"], value["capacity_class"], value["active_work_count"]),
                    classes,
                )

    def test_old_project_observation_ages_entire_snapshot(self):
        old = BASE_TIME - dt.timedelta(seconds=301)
        value = build_unsigned(
            capability=capability(observed=old, expires=BASE_TIME + dt.timedelta(minutes=5)),
            observed_at=BASE_TIME,
            published_at=BASE_TIME,
        )
        self.assertEqual(value["payload"]["freshness"]["observed_at"], MODULE.format_time(old))
        with self.assertRaisesRegex(MODULE.SnapshotError, "stale"):
            MODULE.validate_unsigned_snapshot(value, trust(), now=BASE_TIME)

    def test_delayed_publication_is_stale_on_arrival(self):
        value = build_unsigned(
            capability=capability(observed=BASE_TIME),
            observed_at=BASE_TIME,
            published_at=BASE_TIME + dt.timedelta(seconds=301),
        )
        with self.assertRaisesRegex(MODULE.SnapshotError, "stale"):
            MODULE.validate_unsigned_snapshot(value, trust(), now=BASE_TIME + dt.timedelta(seconds=301))

    def test_project_heat_disappearance_is_a_semantic_transition(self):
        hot = fake_signed(build_unsigned(snapshot_sequence=8))
        cold_unsigned = build_unsigned(
            capability=capability(hot=False),
            snapshot_sequence=9,
            published_at=BASE_TIME + dt.timedelta(seconds=20),
        )
        cold = fake_signed(cold_unsigned)
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [hot]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            updated, reason = MODULE.upsert_fleet(fleet, cold, trust(), now=BASE_TIME + dt.timedelta(seconds=20))
        self.assertEqual(reason, "transition")
        self.assertEqual(updated["nodes"][0]["payload"]["projects"][0]["heat_class"], "cold")

    def test_old_producer_cannot_overwrite_newer_state(self):
        current = fake_signed(build_unsigned(producer_generation=5, snapshot_sequence=1))
        old = fake_signed(build_unsigned(producer_generation=4, snapshot_sequence=99))
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [current]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            with self.assertRaisesRegex(MODULE.SnapshotError, "older producer"):
                MODULE.upsert_fleet(fleet, old, trust(), now=BASE_TIME + dt.timedelta(seconds=10))

    def test_reboot_generation_restarts_sequence_and_changes_glaeda_generation(self):
        current = fake_signed(build_unsigned(producer_generation=5, snapshot_sequence=9))
        reboot = fake_signed(build_unsigned(
            capability=capability(node_generation=8, glaeda="f"),
            producer_generation=6,
            snapshot_sequence=1,
            published_at=BASE_TIME + dt.timedelta(seconds=30),
        ))
        fleet = {"document_type": MODUL