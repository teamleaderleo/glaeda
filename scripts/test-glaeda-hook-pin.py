#!/usr/bin/env python3
"""Tests for glaeda_hook_pin.py: the runner hook's sha is appended to glaeda-disk's drain allowlist, and nothing
else changes."""
from __future__ import annotations

import hashlib
import io
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, os.fspath(ROOT / "scripts"))
import glaeda_hook_pin as hp  # noqa: E402

OLD = "a" * 64
DISK = f'''X = 1
CMUX_NEXT_DRAIN_HOOK_SHA256 = frozenset({{
    "{OLD}",
}})
Y = frozenset({{
    "{"c" * 64}",
}})
'''


class PinTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.hook, self.disk = Path(tmp.name) / "hook", Path(tmp.name) / "glaeda-disk"
        self.hook.write_bytes(b"#!/usr/bin/env python3\nprint('hook')\n")
        self.sha = hashlib.sha256(self.hook.read_bytes()).hexdigest()
        self.disk.write_text(DISK)

    def test_appends_only_the_hook_sha_and_keeps_earlier_pins(self) -> None:
        self.assertIsNone(hp.ensure(check=False, hook=self.hook, disk=self.disk))
        text = self.disk.read_text()
        self.assertEqual(hp.pinned(text), [OLD, self.sha])
        self.assertEqual(text, DISK.replace(f'    "{OLD}",\n', f'    "{OLD}",\n    "{self.sha}",\n', 1))
        self.assertIsNone(hp.ensure(check=False, hook=self.hook, disk=self.disk))
        self.assertEqual(self.disk.read_text(), text)  # idempotent

    def test_check_names_the_sha_and_writes_nothing(self) -> None:
        problem = hp.ensure(check=True, hook=self.hook, disk=self.disk)
        self.assertIn(self.sha, problem)
        self.assertIn(hp.COMMAND, problem)
        self.assertEqual(self.disk.read_text(), DISK)

    def test_a_reshaped_block_is_an_error_not_a_guess(self) -> None:
        self.disk.write_text(DISK.replace("frozenset({\n", "frozenset({  # pins\n", 1))
        with self.assertRaises(ValueError):
            hp.ensure(check=False, hook=self.hook, disk=self.disk)

    def test_cli(self) -> None:
        with mock.patch.object(hp, "HOOK", self.hook), mock.patch.object(hp, "DISK", self.disk), \
                redirect_stderr(io.StringIO()) as err:
            self.assertEqual(hp.main(["--check"]), 1)
            self.assertIn(self.sha, err.getvalue())
            self.assertEqual(hp.main([]), 0)
            self.assertEqual(hp.main(["--check"]), 0)
            self.assertEqual(hp.main(["--bogus"]), 2)

    def test_the_repo_block_parses(self) -> None:
        pins = hp.pinned(hp.DISK.read_text())
        self.assertGreater(len(pins), 10)
        self.assertEqual(len(pins), len(set(pins)))


if __name__ == "__main__":
    unittest.main()
