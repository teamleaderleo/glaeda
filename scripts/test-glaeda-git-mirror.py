#!/usr/bin/env python3
"""glaeda-git-mirror: a full local cmux mirror, seeded from an R2 bundle, refreshed by bounded fetches."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load():
    loader = importlib.machinery.SourceFileLoader("glaeda_git_mirror", os.fspath(ROOT / "scripts/glaeda-git-mirror"))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


gm = load()


def sh(*args: str, cwd: Path | None = None) -> str:
    env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t",
           "GIT_COMMITTER_EMAIL": "t@t"}
    return subprocess.run(args, cwd=cwd, check=True, capture_output=True, text=True, env=env).stdout.strip()


class Mirror(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.home = root / "home"
        (self.home / "actions-runner-glaeda").mkdir(parents=True)
        (self.home / "actions-runner-glaeda/.runner").write_text("{}")
        (self.home / gm.SOURCE_CONFIG).parent.mkdir(parents=True)
        (self.home / gm.SOURCE_CONFIG).write_text('{"schema": 1, "source": "direct"}')
        # The "GitHub" repository: three branches.
        self.upstream = root / "upstream"
        self.upstream.mkdir()
        sh("git", "init", "-q", "-b", "main", cwd=self.upstream)
        self.commit("one")
        for branch in ("feat-cmux-next", "nightly-next"):
            sh("git", "branch", branch, cwd=self.upstream)
        sh("git", "branch", "pr-only", cwd=self.upstream)
        # The bucket: a bundle of upstream, archived as r2-cache.sh saves a directory.
        self.bucket = root / "bucket"
        self.key = "git-mirror-cmux-2026102709." + sh("git", "rev-parse", "HEAD", cwd=self.upstream)[:12]
        staged = root / "staged"
        staged.mkdir()
        sh("git", "bundle", "create", os.fspath(staged / gm.BUNDLE_FILE), "--branches", cwd=self.upstream)
        (self.bucket / "v1/any/objects").mkdir(parents=True)
        (self.bucket / "v1/any/latest").mkdir(parents=True)
        with tarfile.open(self.bucket / f"v1/any/objects/{self.key}.tar.gz", "w:gz") as archive:
            archive.add(staged / gm.BUNDLE_FILE, arcname=f"./{gm.BUNDLE_FILE}")
        (self.bucket / f"v1/any/latest/{gm.BUNDLE_PREFIX}").write_text(self.key + "\n")
        self.downloads: list[str] = []

        git_dir = root / "fleet/git"
        patches = {"GIT_DIR": git_dir, "MIRROR": git_dir / "cmux.git", "RECORD": git_dir / "cmux.json",
                   "REPO_URL": self.upstream.as_uri(), "BUCKET_URL": "https://bucket.test", "SEED_MIN_FREE": 0,
                   "download": self.fake_download, "zstd": lambda: None}
        for name, value in patches.items():
            old = getattr(gm, name)
            setattr(gm, name, value)
            self.addCleanup(setattr, gm, name, old)
        # seed()'s default argument captured the real download at definition time.
        real_seed = gm.seed
        gm.seed = lambda mirror, from_github, fetch_url=self.fake_download: real_seed(mirror, from_github, fetch_url)
        self.addCleanup(setattr, gm, "seed", real_seed)
        self.mirror = git_dir / "cmux.git"

    def commit(self, message: str) -> str:
        (self.upstream / "f").write_text(message)
        sh("git", "add", "f", cwd=self.upstream)
        sh("git", "commit", "-q", "-m", message, cwd=self.upstream)
        return sh("git", "rev-parse", "HEAD", cwd=self.upstream)

    def fake_download(self, url: str, target: Path, timeout: float) -> None:
        self.downloads.append(url)
        source = self.bucket / url.removeprefix("https://bucket.test/")
        if not source.is_file():
            raise subprocess.CalledProcessError(22, "curl")
        target.write_bytes(source.read_bytes())

    def test_no_runner_is_a_no_op(self) -> None:
        empty = Path(self.temp.name) / "nobody"
        empty.mkdir()
        self.assertEqual(gm.run(True, home=empty)["state"], "skipped")
        self.assertFalse(self.mirror.exists())

    def test_seeds_from_the_bundle_then_fetches_new_commits(self) -> None:
        head = self.commit("two")  # newer than the bundle: the fetch after the seed brings it
        result = gm.run(True, home=self.home)
        self.assertEqual(result["state"], "applied", result)
        self.assertIn(f"bundle {self.key}", result["seeded"]["source"])
        self.assertEqual(result["heads"]["main"], head)
        self.assertEqual(set(result["heads"]), set(gm.BRANCHES))  # pr-only is not mirrored
        self.assertEqual(gm.healthy(self.mirror), "")
        self.assertEqual(sh("git", "-C", os.fspath(self.mirror), "config", "uploadpack.allowAnySHA1InWant"), "true")
        self.assertTrue(gm.RECORD.is_file())

    def test_a_job_fetches_any_commit_from_the_mirror(self) -> None:
        old = sh("git", "rev-parse", "HEAD", cwd=self.upstream)
        self.commit("two")
        gm.run(True, home=self.home)
        work = Path(self.temp.name) / "work"
        sh("git", "init", "-q", os.fspath(work))
        sh("git", "-C", os.fspath(work), "fetch", "-q", os.fspath(self.mirror), old)
        self.assertEqual(sh("git", "-C", os.fspath(work), "rev-parse", "FETCH_HEAD"), old)

    def test_the_mirror_refuses_to_become_shallow(self) -> None:
        gm.run(True, home=self.home)
        with self.assertRaises(subprocess.CalledProcessError):
            sh("git", "-C", os.fspath(self.mirror), "fetch", "-q", "--depth=1", self.upstream.as_uri(), "main")
        self.assertEqual(gm.healthy(self.mirror), "")

    def test_a_second_pass_only_fetches(self) -> None:
        gm.run(True, home=self.home)
        self.downloads.clear()
        head = self.commit("three")
        result = gm.run(True, home=self.home)
        self.assertNotIn("seeded", result)
        self.assertEqual(self.downloads, [])
        self.assertEqual(result["heads"]["main"], head)

    def test_a_broken_mirror_is_repaired_in_place_and_keeps_every_object(self) -> None:
        sh("git", "checkout", "-q", "-b", "scratch", cwd=self.upstream)
        forced_away = self.commit("only the old mirror has this")
        sh("git", "branch", "-f", "nightly-next", forced_away, cwd=self.upstream)
        gm.run(True, home=self.home)
        sh("git", "branch", "-f", "nightly-next", "main", cwd=self.upstream)
        sh("git", "checkout", "-q", "main", cwd=self.upstream)
        inode = self.mirror.stat().st_ino
        (self.mirror / "HEAD").unlink()
        self.assertEqual(gm.healthy(self.mirror), "missing")
        result = gm.run(True, home=self.home)
        self.assertEqual(result["state"], "applied", result)
        self.assertEqual(result["seeded"]["mode"], "repaired in place")
        self.assertIn("bundle", result["seeded"]["source"])
        self.assertEqual(self.mirror.stat().st_ino, inode)  # never replaced
        sh("git", "-C", os.fspath(self.mirror), "cat-file", "-e", f"{forced_away}^{{commit}}")

    def test_a_shallow_mirror_is_unshallowed_in_place(self) -> None:
        self.commit("two")
        gm.run(True, home=self.home)
        guard = self.mirror / "shallow.lock"
        guard.rmdir()
        tip = sh("git", "-C", os.fspath(self.mirror), "rev-parse", "main")
        (self.mirror / "shallow").write_text(tip + "\n")
        self.assertEqual(gm.healthy(self.mirror), "shallow")
        result = gm.run(True, home=self.home)
        self.assertEqual(result["state"], "applied", result)
        self.assertEqual(gm.healthy(self.mirror), "")
        self.assertTrue(guard.is_dir())

    def test_nothing_happens_without_a_configured_source(self) -> None:
        (self.home / gm.SOURCE_CONFIG).unlink()
        result = gm.run(True, home=self.home)
        self.assertEqual(result["state"], "skipped")
        self.assertFalse(self.mirror.exists())
        self.assertEqual(self.downloads, [])

    def test_every_partial_marker_and_every_branch_counts(self) -> None:
        gm.run(True, home=self.home)
        self.assertEqual(gm.healthy(self.mirror), "")
        sh("git", "-C", os.fspath(self.mirror), "config", "extensions.partialclone", "origin")
        self.assertEqual(gm.healthy(self.mirror), "partial")
        sh("git", "-C", os.fspath(self.mirror), "config", "--unset", "extensions.partialclone")
        sh("git", "-C", os.fspath(self.mirror), "update-ref", "-d", "refs/heads/nightly-next")
        self.assertEqual(gm.healthy(self.mirror), "no nightly-next")

    def test_git_s_own_shallow_lock_file_never_blocks_the_guard(self) -> None:
        gm.run(True, home=self.home)
        guard = self.mirror / "shallow.lock"
        guard.rmdir()
        guard.write_text("")  # what a fetch killed mid-unshallow leaves
        self.assertEqual(gm.run(True, home=self.home)["state"], "applied")
        self.assertTrue(guard.is_dir())

    def test_the_github_fallback_always_refetches(self) -> None:
        calls = []
        real = gm.fetch
        gm.fetch = lambda repo, url, **kw: calls.append(kw.get("extra")) or {"ok": True, "tries": 1, "seconds": 0}
        try:
            with tempfile.TemporaryDirectory() as temp:
                repo = Path(temp) / "r.git"
                sh("git", "init", "-q", "--bare", os.fspath(repo))
                (self.bucket / f"v1/any/latest/{gm.BUNDLE_PREFIX}").unlink()
                gm.fill(repo, Path(temp), True, gm.download)
        finally:
            gm.fetch = real
        self.assertEqual(calls, [["--refetch"]])

    def test_a_failed_repair_restores_the_shallow_file_exactly(self) -> None:
        gm.run(True, home=self.home)
        (self.bucket / f"v1/any/latest/{gm.BUNDLE_PREFIX}").unlink()
        sh("git", "-C", os.fspath(self.mirror), "update-ref", "-d", "refs/heads/main")
        real = gm.fill

        def writes_shallow(repo, *args):
            (repo / "shallow").write_text("0" * 40 + "\n")
            raise RuntimeError("fill failed")

        gm.fill = writes_shallow
        try:
            self.assertEqual(gm.run(True, home=self.home)["state"], "error")
        finally:
            gm.fill = real
        self.assertFalse((self.mirror / "shallow").exists())

    def test_a_failed_repair_keeps_the_mirror_marked_broken(self) -> None:
        self.commit("two")
        gm.run(True, home=self.home)
        (self.mirror / "shallow.lock").rmdir()
        tip = sh("git", "-C", os.fspath(self.mirror), "rev-parse", "main")
        (self.mirror / "shallow").write_text(tip + "\n")
        sh("git", "-C", os.fspath(self.mirror), "config", "remote.origin.promisor", "true")
        # No bundle and no GitHub fallback: the repair fails, and the markers stay.
        (self.bucket / f"v1/any/latest/{gm.BUNDLE_PREFIX}").unlink()
        self.assertEqual(gm.run(True, home=self.home)["state"], "error")
        self.assertEqual((self.mirror / "shallow").read_text(), tip + "\n")
        self.assertEqual(gm.config_values(self.mirror, "remote.origin.promisor"), ["true"])
        self.assertNotEqual(gm.healthy(self.mirror), "")

    def test_the_github_fallback_unshallows_an_existing_mirror(self) -> None:
        self.commit("two")
        self.commit("three")
        gm.run(True, home=self.home)
        # Make it truly shallow: a fresh depth-1 copy in place of the full one.
        sh("rm", "-rf", os.fspath(self.mirror))
        sh("git", "clone", "-q", "--bare", "--depth=1", self.upstream.as_uri(), os.fspath(self.mirror))
        self.assertEqual(gm.healthy(self.mirror), "shallow")
        (self.bucket / f"v1/any/latest/{gm.BUNDLE_PREFIX}").unlink()
        result = gm.run(True, home=self.home, from_github=True)
        self.assertEqual(result["state"], "applied", result)
        self.assertEqual(result["seeded"]["mode"], "repaired in place")
        self.assertEqual(gm.healthy(self.mirror), "")
        self.assertEqual(sh("git", "-C", os.fspath(self.mirror), "rev-list", "--count", "main"), "3")

    def test_no_bundle_never_clones_from_github_unless_asked(self) -> None:
        (self.bucket / f"v1/any/latest/{gm.BUNDLE_PREFIX}").unlink()
        result = gm.run(True, home=self.home)
        self.assertEqual(result["state"], "error", result)
        self.assertIn("R2 bundle", result["reason"])
        self.assertFalse(self.mirror.exists())
        # The operator's fallback fills the staging repository before it takes the path.
        result = gm.run(True, home=self.home, from_github=True)
        self.assertEqual(result["state"], "applied", result)
        self.assertIn("github", result["seeded"]["source"])
        self.assertEqual(result["seeded"]["mode"], "new")
        self.assertEqual(gm.healthy(self.mirror), "")

    def test_a_failed_seed_leaves_no_half_mirror(self) -> None:
        (self.bucket / f"v1/any/objects/{self.key}.tar.gz").write_bytes(b"not an archive")
        result = gm.run(True, home=self.home)
        self.assertEqual(result["state"], "error", result)
        self.assertFalse(self.mirror.exists())
        self.assertEqual([p.name for p in self.mirror.parent.iterdir() if p.name.startswith(".seed-")], [])

    def test_a_bad_pointer_is_refused(self) -> None:
        (self.bucket / f"v1/any/latest/{gm.BUNDLE_PREFIX}").write_text("../../etc/passwd")
        result = gm.run(True, home=self.home)
        self.assertEqual(result["state"], "error")
        self.assertFalse(self.mirror.exists())

    def test_many_packs_are_folded_and_gc_never_runs_inside_a_fetch(self) -> None:
        gm.run(True, home=self.home)
        self.assertEqual(sh("git", "-C", os.fspath(self.mirror), "config", "gc.auto"), "0")
        self.assertEqual(sh("git", "-C", os.fspath(self.mirror), "config", "gc.pruneExpire"), "never")
        # A commit only a since-forced nightly-next held survives the folds (borrowers may hold it).
        sh("git", "checkout", "-q", "-b", "scratch", cwd=self.upstream)
        orphan = self.commit("forced-away")
        sh("git", "branch", "-f", "nightly-next", orphan, cwd=self.upstream)
        gm.run(True, home=self.home)
        sh("git", "branch", "-f", "nightly-next", "main", cwd=self.upstream)
        sh("git", "checkout", "-q", "main", cwd=self.upstream)
        old = gm.PACK_LIMIT
        gm.PACK_LIMIT = 2
        self.addCleanup(setattr, gm, "PACK_LIMIT", old)
        for n in range(4):
            self.commit(f"c{n}")
            gm.run(True, home=self.home)
        packs = list((self.mirror / "objects/pack").glob("*.pack"))
        self.assertLessEqual(len(packs), 3)
        sh("git", "-C", os.fspath(self.mirror), "cat-file", "-e", f"{orphan}^{{commit}}")
        self.assertEqual(gm.healthy(self.mirror), "")

    def test_plan_changes_nothing(self) -> None:
        result = gm.run(False, home=self.home)
        self.assertEqual((result["state"], result["health"]), ("plan", "missing"))
        self.assertFalse(self.mirror.exists())


class Fetch(unittest.TestCase):
    def test_every_try_is_bounded_and_retried_with_the_low_speed_abort(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            mirror = Path(temp)
            (mirror / "objects/pack").mkdir(parents=True)
            calls, sleeps = [], []

            def runner(command, timeout):
                calls.append((list(command), timeout))
                (mirror / "objects/pack/tmp_pack_x").write_text("partial")
                return (124, "timed out") if len(calls) < 3 else (0, "")

            result = gm.fetch(mirror, "https://example.test/r.git", runner=runner, sleep=sleeps.append)
            self.assertTrue(result["ok"])
            self.assertEqual(result["tries"], 3)
            self.assertEqual(sleeps, list(gm.BACKOFF))
            for command, timeout in calls:
                self.assertEqual(timeout, gm.FETCH_TIMEOUT)
                self.assertEqual(command[1:5], list(gm.LOW_SPEED))
                self.assertNotIn("--depth", " ".join(command))

    def test_run_group_kills_a_hung_command(self) -> None:
        status, why = gm.run_group(["sh", "-c", "sleep 30 & sleep 30"], timeout=1)
        self.assertEqual(status, 124)
        self.assertIn("timed out", why)


if __name__ == "__main__":
    unittest.main()
