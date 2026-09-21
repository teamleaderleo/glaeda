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


def reusable_state_summary(
    *,
    cache_class="incremental_build_state",
    generation="7",
    heat="hot",
    size="medium",
    recent_hit="within_hour",
    revalidation_required=False,
):
    return {
        "schema_version": 1,
        "cache_class": cache_class,
        "generation": "sha256:" + generation * 64,
        "heat": heat,
        "size": size,
        "recent_hit": recent_hit,
        "revalidation_required": revalidation_required,
    }


def project_state(*states):
    return {
        "document_type": "glaeda-github-project-heat-input",
        "schema_version": 1,
        "projects": [
            {
                "repository": "teamleaderleo/glaeda",
                "source": {"commit_oid": "1" * 40, "tree_oid": "2" * 40},
                "heat_class": "resident_hot",
                "verification_profiles": ["glaeda.required", "verify-focused/v1"],
                "dependency_build_state_class": "resident_generation",
                "reusable_states": list(states),
                "active_task_count": 0,
                "recent_compatible_receipt_ref": None,
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
        self.assertEqual(payload["projects"][0]["reusable_states"], [])
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

    def test_reusable_state_summary_matches_lifecycle_public_contract(self):
        summary = reusable_state_summary()
        snapshot = build_unsigned(project_state=project_state(summary))
        published = snapshot["payload"]["projects"][0]["reusable_states"]
        self.assertEqual(published, [summary])
        raw = MODULE.canonical_json(snapshot).decode()
        for forbidden in ("/home/", "/Users/", "target/", "node_modules", "credential", "command_output"):
            self.assertNotIn(forbidden.lower(), raw.lower())

        duplicate = reusable_state_summary()
        with self.assertRaisesRegex(MODULE.SnapshotError, "identities must be unique"):
            build_unsigned(project_state=project_state(summary, duplicate))

        requires_reset = reusable_state_summary(revalidation_required=True)
        with self.assertRaisesRegex(MODULE.SnapshotError, "revalidation requires cold heat"):
            build_unsigned(project_state=project_state(requires_reset))

        cold_reset = reusable_state_summary(
            heat="cold",
            recent_hit="within_day",
            revalidation_required=True,
        )
        accepted = build_unsigned(project_state=project_state(cold_reset))
        self.assertTrue(
            accepted["payload"]["projects"][0]["reusable_states"][0]["revalidation_required"]
        )

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
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [current]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            updated, reason = MODULE.upsert_fleet(fleet, reboot, trust(), now=BASE_TIME + dt.timedelta(seconds=30))
        self.assertEqual(reason, "transition")
        self.assertEqual(updated["nodes"][0]["payload"]["freshness"]["snapshot_sequence"], 1)
        self.assertEqual(updated["nodes"][0]["payload"]["producer"]["glaeda_generation"], "sha256:" + "f" * 64)

    def test_unchanged_refresh_is_suppressed_until_interval(self):
        current = fake_signed(build_unsigned(snapshot_sequence=8))
        candidate = fake_signed(build_unsigned(
            snapshot_sequence=9,
            observed_at=BASE_TIME + dt.timedelta(seconds=20),
            published_at=BASE_TIME + dt.timedelta(seconds=20),
            capability=capability(observed=BASE_TIME + dt.timedelta(seconds=20)),
        ))
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [current]}
        with mock.patch.object(MODULE, "validate_signed_snapshot", side_effect=lambda value, *_a, **_kw: value):
            with self.assertRaises(MODULE.PublicationSuppressed):
                MODULE.upsert_fleet(fleet, candidate, trust(), now=BASE_TIME + dt.timedelta(seconds=20))

    def test_running_request_requires_matching_local_active_work(self):
        with self.assertRaisesRegex(MODULE.SnapshotError, "active work"):
            build_unsigned(request_state=request_state(state="running"), admission=admission())
        value = build_unsigned(
            request_state=request_state(state="running"),
            admission=admission("wait", "reserved"),
        )
        self.assertEqual(value["payload"]["node"]["active_work_count"], 1)
        self.assertEqual(value["payload"]["requests"][0]["state"], "running")

    def test_terminal_state_requires_bounded_receipt(self):
        with self.assertRaisesRegex(MODULE.SnapshotError, "terminal receipt"):
            build_unsigned(request_state=request_state(state="terminal", receipt=None))
        value = build_unsigned(request_state=request_state(state="terminal", receipt="sha256:" + "9" * 64))
        self.assertEqual(value["payload"]["requests"][0]["state"], "terminal")


@unittest.skipUnless(GIT.exists() and SSH_KEYGEN.exists(), "OpenSSH signing integration unavailable")
class ResidentSnapshotSigningAndTransportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="glaeda-status-test-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def key(self, name):
        path = self.root / name
        subprocess.run(
            [str(SSH_KEYGEN), "-q", "-t", "ed25519", "-N", "", "-f", str(path)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        public = Path(str(path) + ".pub").read_text(encoding="ascii").strip()
        return path, public

    def one_signed(
        self,
        *,
        now=BASE_TIME,
        sequence=1,
        generation=1,
        hot=True,
        request=None,
        admission_value=None,
        project=None,
    ):
        key, public = self.key(f"key-{sequence}-{generation}")
        trust_value = trust(nodes=[{
            "id": "node-1111111111111111",
            "key_id": "key-aaaaaaaaaaaaaaaa",
            "ssh_public_key": public,
            "os_class": "linux",
            "architecture_class": "x86_64",
        }])
        unsigned = build_unsigned(
            capability=capability(observed=now, hot=hot),
            admission=admission_value or admission(),
            trust_value=trust_value,
            producer_generation=generation,
            snapshot_sequence=sequence,
            observed_at=now,
            published_at=now,
            project_state=project,
            request_state=request if request is not None else request_state(state="queued"),
        )
        return MODULE.sign_snapshot(unsigned, trust_value, private_key=key, ssh_keygen=SSH_KEYGEN), trust_value, key

    def test_signature_round_trip_and_manual_forgery_degrades_to_unknown(self):
        signed, trust_value, _ = self.one_signed()
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        view = MODULE.consume_fleet(fleet, trust_value, now=BASE_TIME + dt.timedelta(seconds=10), ssh_keygen=SSH_KEYGEN)
        self.assertEqual(view["nodes"][0]["freshness_class"], "fresh")
        forged = json.loads(json.dumps(signed))
        forged["payload"]["projects"][0]["heat_class"] = "cold"
        view = MODULE.consume_fleet(
            {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [forged]},
            trust_value,
            now=BASE_TIME + dt.timedelta(seconds=10),
            ssh_keygen=SSH_KEYGEN,
        )
        self.assertEqual(view["nodes"][0]["freshness_class"], "unknown")
        self.assertEqual(view["nodes"][0]["reason"], "untrusted")
        self.assertEqual(view["nodes"][0]["projects"], [])

    def test_reusable_state_summary_round_trips_to_agent_view(self):
        summary = reusable_state_summary(
            cache_class="package_manager_state",
            generation="8",
            heat="warm",
            size="small",
            recent_hit="within_day",
        )
        signed, trust_value, _ = self.one_signed(project=project_state(summary))
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        view = MODULE.consume_fleet(
            fleet,
            trust_value,
            now=BASE_TIME + dt.timedelta(seconds=10),
            ssh_keygen=SSH_KEYGEN,
        )
        project = view["nodes"][0]["projects"][0]
        self.assertEqual(project["reusable_states"], [summary])

    def test_node_dies_after_available_and_lost_terminal_update_degrade_to_unknown(self):
        signed, trust_value, key = self.one_signed(
            request=request_state(state="running"),
            admission_value=admission("wait", "reserved"),
        )
        terminal_time = BASE_TIME + dt.timedelta(seconds=30)
        terminal_unsigned = MODULE.build_unsigned_snapshot(
            capability(observed=terminal_time),
            admission(),
            trust_value,
            public_node_id="node-1111111111111111",
            producer_generation=1,
            snapshot_sequence=2,
            observed_at=terminal_time,
            published_at=terminal_time,
            request_state=request_state(state="terminal", receipt="sha256:" + "9" * 64),
        )
        terminal = MODULE.sign_snapshot(terminal_unsigned, trust_value, private_key=key, ssh_keygen=SSH_KEYGEN)
        self.assertEqual(terminal["payload"]["requests"][0]["state"], "terminal")
        # Simulate the terminal GitHub update being lost: the remote fleet still contains sequence 1.
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        stale = MODULE.consume_fleet(fleet, trust_value, now=BASE_TIME + dt.timedelta(seconds=301), ssh_keygen=SSH_KEYGEN)
        self.assertEqual(stale["nodes"][0]["freshness_class"], "unknown")
        self.assertEqual(stale["nodes"][0]["reason"], "stale")
        self.assertEqual(stale["nodes"][0]["requests"], [])

    def test_signed_snapshot_and_fleet_byte_measurements(self):
        signed, trust_value, _ = self.one_signed(
            project=project_state(reusable_state_summary())
        )
        fleet = {"document_type": MODULE.FLEET_DOCUMENT, "schema_version": 1, "nodes": [signed]}
        node_bytes = len(MODULE.canonical_json(signed))
        fleet_bytes = len(MODULE.canonical_json(fleet))
        self.assertLessEqual(node_bytes, MODULE.MAX_NODE_BYTES)
        self.assertLessEqual(fleet_bytes, MODULE.MAX_FLEET_BYTES)
        print(f"MEASURE github_resident_snapshot node_bytes={node_bytes} fleet_bytes={fleet_bytes} reusable_state_count=1 agent_reads=2 successful_write_remote_round_trips=2 idle_refresh_seconds={MODULE.DEFAULT_REFRESH_INTERVAL_SECONDS}")

    def test_two_nodes_can_publish_concurrently_and_converge(self):
        remote = self.root / "remote.git"
        subprocess.run([str(GIT), "init", "--bare", "--quiet", str(remote)], check=True)
        repos = []
        for index in (1, 2):
            repo = self.root / f"repo-{index}"
            subprocess.run([str(GIT), "init", "--quiet", str(repo)], check=True)
            subprocess.run([str(GIT), "-C", str(repo), "remote", "add", "origin", str(remote)], check=True)
            repos.append(repo)

        key1, pub1 = self.key("node1")
        key2, pub2 = self.key("node2")
        trust_value = trust(nodes=[
            {
                "id": "node-1111111111111111",
                "key_id": "key-aaaaaaaaaaaaaaaa",
                "ssh_public_key": pub1,
                "os_class": "linux",
                "architecture_class": "x86_64",
            },
            {
                "id": "node-2222222222222222",
                "key_id": "key-bbbbbbbbbbbbbbbb",
                "ssh_public_key": pub2,
                "os_class": "macos",
                "architecture_class": "arm64",
            },
        ])
        snap1 = MODULE.sign_snapshot(
            MODULE.build_unsigned_snapshot(
                capability(observed=BASE_TIME), admission(), trust_value,
                public_node_id="node-1111111111111111", producer_generation=1, snapshot_sequence=1,
                observed_at=BASE_TIME, published_at=BASE_TIME,
            ),
            trust_value, private_key=key1, ssh_keygen=SSH_KEYGEN,
        )
        snap2 = MODULE.sign_snapshot(
            MODULE.build_unsigned_snapshot(
                capability(observed=BASE_TIME, os_class="macos", architecture="arm64"), admission(), trust_value,
                public_node_id="node-2222222222222222", producer_generation=1, snapshot_sequence=1,
                observed_at=BASE_TIME, published_at=BASE_TIME,
            ),
            trust_value, private_key=key2, ssh_keygen=SSH_KEYGEN,
        )
        def publish(args):
            snapshot, repo = args
            return MODULE.publish_snapshot(
                snapshot, trust_value, repository_root=repo, now=BASE_TIME,
                git=GIT, ssh_keygen=SSH_KEYGEN, retries=3,
            )
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            receipts = list(pool.map(publish, [(snap1, repos[0]), (snap2, repos[1])]))
        self.assertTrue(all(item["state"] == "published" for item in receipts))
        head = subprocess.check_output([str(GIT), "--git-dir", str(remote), "rev-parse", f"refs/heads/{MODULE.STATUS_BRANCH}"], text=True).strip()
        raw = subprocess.check_output([str(GIT), "--git-dir", str(remote), "show", f"{head}:{MODULE.STATUS_FILE}"])
        fleet = json.loads(raw)
        view = MODULE.consume_fleet(fleet, trust_value, now=BASE_TIME + dt.timedelta(seconds=5), ssh_keygen=SSH_KEYGEN)
        self.assertEqual([item["id"] for item in view["nodes"]], ["node-1111111111111111", "node-2222222222222222"])
        self.assertTrue(all(item["freshness_class"] == "fresh" for item in view["nodes"]))


if __name__ == "__main__":
    unittest.main(verbosity=2)
