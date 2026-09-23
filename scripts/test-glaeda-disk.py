#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-disk."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_disk", os.fspath(ROOT / "scripts" / "glaeda-disk"))
spec = importlib.util.spec_from_loader("glaeda_disk", loader)
gd = importlib.util.module_from_spec(spec)
sys.modules["glaeda_disk"] = gd
loader.exec_module(gd)


def make(path: Path, mib: int = 1, age_hours: float = 48) -> Path:
    path.mkdir(parents=True)
    (path / "blob").write_bytes(b"\0" * (mib * 1024 * 1024))
    t = time.time() - age_hours * 3600
    for p in (path / "blob", path):
        os.utime(p, (t, t))
    return path


class GlaedaDiskTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(os.path.realpath(self.tmp.name))
        self.fam = gd.Family("xcode-derived-data", self.root, True, "rebuild")
        self.gd_evidence = gd.process_evidence
        gd.process_evidence = lambda: ([], "")

    def tearDown(self) -> None:
        gd.process_evidence = self.gd_evidence
        self.tmp.cleanup()

    def verdicts(self, idle: float = 24) -> dict[str, str]:
        return {Path(i.path).name: i.verdict for i in gd.survey([self.fam], idle, 0)}

    def test_idle_is_reclaimable_recent_is_not(self) -> None:
        make(self.root / "old")
        make(self.root / "new", age_hours=1)
        self.assertEqual(self.verdicts(), {"old": "reclaimable", "new": "recent"})

    def test_process_cwd_and_command_line_veto(self) -> None:
        a, b = make(self.root / "a"), make(self.root / "b")
        make(self.root / "a-sibling")
        gd.process_evidence = lambda: ([str(a / "sub")], f"xcodebuild -derivedDataPath {b} build\n")
        v = self.verdicts()
        self.assertEqual(v["a"], "in-use")
        self.assertEqual(v["b"], "in-use")
        # a prefix of another path must not veto it
        self.assertEqual(v["a-sibling"], "reclaimable")

    def test_git_checkout_is_never_reclaimable_outside_derived_data(self) -> None:
        self.fam = gd.Family("tmp", self.root, True, "scratch")
        repo = make(self.root / "repo")
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        os.utime(repo, (time.time() - 48 * 3600,) * 2)
        os.utime(repo / ".git", (time.time() - 48 * 3600,) * 2)
        self.assertEqual(gd.survey([self.fam], 24, 0)[0].verdict, "git-checkout")

    def test_report_only_family_is_never_deleted(self) -> None:
        self.fam = gd.Family("user-cache", self.root, False, "report")
        make(self.root / "cache")
        items = gd.survey([self.fam], 24, 0)
        self.assertEqual(items[0].verdict, "report-only")
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        gd.apply(items, {"user-cache": self.fam}, receipt, None, self.root, 24)
        self.assertTrue((self.root / "cache").exists())
        self.assertFalse(receipt.exists())

    def test_apply_deletes_and_writes_receipt(self) -> None:
        make(self.root / "old")
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        try:
            items = gd.survey([self.fam], 24, 0)
            gd.apply(items, {self.fam.id: self.fam}, receipt, None, self.root, 24)
            self.assertFalse((self.root / "old").exists())
            self.assertIn('"outcome": "reclaimed"', receipt.read_text())
        finally:
            receipt.unlink(missing_ok=True)

    def test_apply_rechecks_and_skips_item_touched_since_survey(self) -> None:
        d = make(self.root / "old")
        items = gd.survey([self.fam], 24, 0)
        (d / "fresh").write_text("x")
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        try:
            gd.apply(items, {self.fam.id: self.fam}, receipt, None, self.root, 24)
            self.assertTrue(d.exists())
            self.assertIn("changed:modified", receipt.read_text())
        finally:
            receipt.unlink(missing_ok=True)

    def test_git_clone_two_levels_down_is_a_checkout(self) -> None:
        self.fam = gd.Family("claude-scratchpad", self.root, True, "scratch")
        session = make(self.root / "session")
        (session / "scratchpad" / "repo" / ".git").mkdir(parents=True)
        for p in (session / "scratchpad" / "repo" / ".git", session / "scratchpad" / "repo",
                  session / "scratchpad", session):
            os.utime(p, (time.time() - 48 * 3600,) * 2)
        self.assertEqual(gd.survey([self.fam], 24, 0)[0].verdict, "git-checkout")

    def test_deep_tree_is_unchecked_not_git_and_build_dirs_are_skipped(self) -> None:
        deep = self.root / "deep"
        (deep / "a/b/c/d/e/f/g").mkdir(parents=True)
        self.assertEqual(gd.git_state(deep), "unchecked")
        mods = self.root / "mods"
        (mods / "node_modules/x/y/z/w/v/u").mkdir(parents=True)
        self.assertEqual(gd.git_state(mods), "none")
        (self.root / "clone/target/.git").mkdir(parents=True)
        self.assertEqual(gd.git_state(self.root / "clone"), "git")
        (self.root / "repo/sub/.git").mkdir(parents=True)
        self.assertEqual(gd.git_state(self.root / "repo"), "git")

    def test_tmp_alias_and_comma_lists_count_as_named(self) -> None:
        self.assertTrue(gd.named_by("/private/tmp/foo", "tool --out /tmp/foo/dist\n"))
        self.assertTrue(gd.named_by("/private/tmp/foo", "tool --dirs=/private/tmp/foo,/x\n"))
        self.assertFalse(gd.named_by("/private/tmp/foo", "tool /tmp/foobar\n"))
        self.assertTrue(gd.in_cwd("/private/tmp/foo", ["/tmp/foo/a.log"]))

    def test_missing_process_evidence_fails_closed(self) -> None:
        make(self.root / "old")

        def blind():
            raise gd.NoEvidence("lsof missing")
        gd.process_evidence = blind
        items = gd.survey([self.fam], 24, 0)
        self.assertEqual(items[0].verdict, "in-use")
        forced = [gd.Item(i.family, i.path, i.bytes, i.idle_hours, "reclaimable") for i in items]
        receipt = self.root.parent / f"{self.root.name}-receipt.jsonl"
        gd.apply(forced, {self.fam.id: self.fam}, receipt, None, self.root, 24)
        self.assertTrue((self.root / "old").exists())

    def test_remove_never_chmods_through_symlinks(self) -> None:
        outside = self.root.parent / f"{self.root.name}-outside"
        outside.write_text("x")
        os.chmod(outside, 0o400)
        try:
            tree = self.root / "tree"
            (tree / "ro").mkdir(parents=True)
            os.symlink(outside, tree / "ro" / "link")
            os.chmod(tree / "ro", 0o500)
            gd.remove(tree)
            self.assertFalse(tree.exists())
            self.assertEqual(outside.stat().st_mode & 0o777, 0o400)
        finally:
            os.chmod(outside, 0o600)
            outside.unlink()

    def test_known_sizes_are_reused_and_new_items_measured(self) -> None:
        old, new = make(self.root / "old"), make(self.root / "new")
        known = {str(old): 7 * 1024**3}
        sizes = {Path(i.path).name: i.bytes for i in gd.survey([self.fam], 24, 0, known)}
        self.assertEqual(sizes["old"], 7 * 1024**3)
        self.assertGreater(sizes["new"], 0)
        self.assertIn(str(new), known)  # measured size is kept for the next snapshot

    def test_snapshot_round_trip_and_corrupt_snapshot_is_empty(self) -> None:
        snap = self.root / "sizes.json"
        gd.save_snapshot({"/a": 1}, snap)
        self.assertEqual(gd.load_snapshot(snap)[1], {"/a": 1})
        snap.write_text("{not json")
        self.assertEqual(gd.load_snapshot(snap), (0.0, {}))


@unittest.skipUnless(sys.platform == "darwin", "APFS clones are macOS only")
class DedupeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(dir=os.path.expanduser("~"))
        self.root = Path(self.tmp.name)
        self.state = self.root / "state.json"

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def blob(self, rel: str, data: bytes, mode: int = 0o644, mtime: float = 1_000_000) -> Path:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        os.chmod(p, mode)
        os.utime(p, (mtime, mtime))
        return p

    def test_identical_files_are_cloned_keeping_mode_and_mtime(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        a = self.blob("a/SourcePackages/x.bin", data)
        b = self.blob("b/SourcePackages/x.bin", data, mode=0o755, mtime=2_000_000)
        c = self.blob("c/SourcePackages/x.bin", os.urandom(len(data)))
        ino_b = b.stat().st_ino
        r = gd.dedupe([self.root / "a", self.root / "b", self.root / "c"], self.state)
        self.assertEqual(r["cloned_bytes"], len(data))
        self.assertEqual(b.read_bytes(), data)
        self.assertNotEqual(b.stat().st_ino, ino_b)
        self.assertEqual(oct(b.stat().st_mode & 0o777), oct(0o755))
        self.assertEqual(int(b.stat().st_mtime), 2_000_000)
        self.assertNotEqual(c.read_bytes(), data)
        self.assertEqual(a.read_bytes(), data)
        again = gd.dedupe([self.root / "a", self.root / "b", self.root / "c"], self.state)
        self.assertEqual((again["hashed_bytes"], again["cloned_bytes"]), (0, 0))
        self.assertEqual([p.name for p in self.root.rglob("*glaeda-clone*")], [])

    def test_target_changed_after_hash_is_refused(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        a = self.blob("a/x.bin", data)
        b = self.blob("b/x.bin", data)
        key = gd._file_key(os.lstat(b))
        os.utime(b, (3_000_000, 3_000_000))
        self.assertFalse(gd.clone_over(str(a), str(b), key))
        self.assertEqual(int(b.stat().st_mtime), 3_000_000)

    def test_canonical_changed_after_hash_is_refused(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        a = self.blob("a/x.bin", data)
        b = self.blob("b/x.bin", data)
        akey, bkey = gd._file_key(os.lstat(a)), gd._file_key(os.lstat(b))
        a.write_bytes(os.urandom(len(data)))
        self.assertFalse(gd.clone_over(str(a), str(b), bkey, akey))
        self.assertEqual(b.read_bytes(), data)

    def test_recently_written_files_wait_for_a_later_pass(self) -> None:
        data = os.urandom(2 * 1024 * 1024)
        self.blob("a/x.bin", data)
        self.blob("b/x.bin", data, mtime=time.time())
        r = gd.dedupe([self.root / "a", self.root / "b"], self.state, settle_s=600)
        self.assertEqual((r["young"], r["cloned_bytes"]), (1, 0))

    def test_small_and_unique_files_are_ignored(self) -> None:
        self.blob("a/small", b"x" * 10)
        self.blob("b/small", b"x" * 10)
        r = gd.dedupe([self.root / "a", self.root / "b"], self.state)
        self.assertEqual((r["files"], r["cloned_bytes"]), (0, 0))


if __name__ == "__main__":
    unittest.main()
