#!/usr/bin/env python3
"""Focused tests for the explicit linker-throughput benchmark harness."""

from __future__ import annotations

import json
import os
import runpy
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace


SCRIPT = Path(__file__).with_name("benchmark-linker-throughput")
MODULE = SimpleNamespace(**runpy.run_path(os.fspath(SCRIPT), run_name="linker_benchmark_test"))


class LinkerBenchmarkTests(unittest.TestCase):
    def test_encoded_flags_are_fixed_and_gnu_is_empty(self) -> None:
        self.assertIsNone(MODULE.encoded_rustflags("gnu"))
        self.assertEqual(
            MODULE.encoded_rustflags("lld"),
            "-Clinker=clang\x1f-Clink-arg=-fuse-ld=lld",
        )
        self.assertEqual(
            MODULE.encoded_rustflags("mold"),
            "-Clinker=clang\x1f-Clink-arg=-fuse-ld=mold",
        )
        with self.assertRaises(MODULE.BenchmarkError):
            MODULE.encoded_rustflags("other")

    def test_summary_uses_nearest_rank_p90(self) -> None:
        self.assertEqual(
            MODULE.summarize([6.0, 1.0, 4.0, 2.0, 5.0, 3.0]),
            {"minimum": 1.0, "median": 3.5, "p90": 6.0, "maximum": 6.0},
        )

    def test_stop_workers_settles_exact_process_group(self) -> None:
        process = subprocess.Popen(
            ["/usr/bin/sleep", "60"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        worker = MODULE.Worker(process, Path("receipt"), Path("log"), Path("target"))
        self.assertTrue(MODULE.process_group_exists(process.pid))
        MODULE.stop_workers([worker])
        self.assertIsNotNone(process.returncode)
        self.assertFalse(MODULE.process_group_exists(process.pid))

    def test_target_stats_rejects_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "file").write_bytes(b"payload")
            clean = MODULE.target_stats(root)
            self.assertEqual(clean["entries"], 1)
            (root / "link").symlink_to(root / "file")
            with self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.target_stats(root)
            self.assertEqual(raised.exception.code, "target_symlink")

    def test_atomic_json_is_private(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "receipt.json"
            MODULE.write_json(output, {"schema_version": 1})
            self.assertEqual(json.loads(output.read_text()), {"schema_version": 1})
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            with self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.write_json(output, {"schema_version": 2})
            self.assertEqual(raised.exception.code, "receipt_write")
            self.assertEqual(json.loads(output.read_text()), {"schema_version": 1})

    def test_receipt_validator_accepts_exact_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "target"
            target.mkdir()
            digest = MODULE.hashlib.sha256(
                b"glaeda-local-verification-path-v1\0"
            )
            digest.update(os.fsencode(target.resolve()))
            document = {
                "schema_version": 1,
                "document_type": "glaeda-local-verification-receipt",
                "authority": "performance_observation_only",
                "profile": "fast",
                "source": {"commit": "a" * 40, "tree": "b" * 40, "unchanged": True},
                "environment": {
                    "cargo_build_jobs": {"source": "configured", "value": 4},
                    "cargo_target": {
                        "source": "configured",
                        "identity_digest": f"sha256:{digest.hexdigest()}",
                    },
                },
                "plan": {"declared_phase_count": 7, "executed_phase_count": 7},
                "phases": [
                    {"name": name, "exit_code": 0, "elapsed_seconds": 1.0}
                    for name in MODULE.FAST_PHASES
                ],
                "result": {
                    "exit_code": 0,
                    "elapsed_seconds": 7.0,
                    "child_user_cpu_seconds": 8.0,
                    "child_system_cpu_seconds": 1.0,
                    "process_lifetime_child_max_rss_kib": 100,
                },
            }
            receipt = root / "receipt.json"
            receipt.write_text(json.dumps(document))
            observed = MODULE.load_receipt(
                receipt, MODULE.SourceIdentity("a" * 40, "b" * 40), target
            )
            self.assertEqual(observed["result"]["exit_code"], 0)
            document["source"]["unchanged"] = False
            receipt.write_text(json.dumps(document))
            with self.assertRaises(MODULE.BenchmarkError) as raised:
                MODULE.load_receipt(
                    receipt, MODULE.SourceIdentity("a" * 40, "b" * 40), target
                )
            self.assertEqual(raised.exception.code, "invalid_receipt")


if __name__ == "__main__":
    unittest.main()
