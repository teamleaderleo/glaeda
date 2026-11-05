# Dev-box disk and Projects hygiene

Glaeda uses one policy surface for fleet minis and developer machines. The role is explicit:

```text
glaeda-disk report --role devbox
glaeda-disk keepup --role devbox --apply
glaeda-projects tidy       # dry run
glaeda-projects tidy --apply
```

`scripts/glaeda-mini-setup --apply` installs these into `~/.local/bin` (`--hygiene-only` on a
developer machine, which defaults to the `devbox` role).

`fleet-mini` retains the existing cmux hot-tier policy. `devbox` adds safe cleanup for stale
developer worktrees, `/tmp`, Xcode DerivedData, SwiftPM `.build`, Ghostty `.zig-cache`, and
other per-checkout build outputs. Developer worktrees and source checkouts remain report-only;
this policy does not remove user source or Git state. Package caches are pressure-only. Codex transcripts and their
archives remain report-only so Codex task history is preserved. Every candidate is rechecked for open files, a live cwd, command-line use,
socket markers, recent activity, Git state, and submodule state immediately before mutation.

New checkouts go through `~/.local/bin/glaeda-worktree add`. `--no-build` creates the source
worktree without initializing submodules. `--build` initializes Ghostty with `--reference` to
the canonical checkout's module objects. Worktrees live under `~/Projects/worktrees/<repo>/<name>`.

`glaeda-projects tidy` reports older linked worktrees, redundant clean pushed clones, and scratch
folders that can move to the canonical layout. It skips live processes, recent paths, and recent
Codex sessions whose cwd names the checkout. Submodule worktrees are status-checked recursively;
if Git refuses to move one, the planner leaves it in place with the metadata and index intact.
The default is report-only;
the compatibility symlink step runs only after every child of a legacy `<repo>-worktrees` folder
has moved successfully.

macOS installs a pressure/idle LaunchAgent and a daily devbox Projects tidy agent. Linux installs
the equivalent user systemd units. Reports and apply receipts remain under the owning user's
Glaeda state directories.

## Read-only checkout evidence

Two compiled tools (`src/bin/`) answer exact questions about a checkout without catalog,
adoption, residency, or mutation authority:

```bash
glaeda-project-observe --checkout /absolute/canonical/checkout --output json
glaeda-repo-query --checkout /absolute/canonical/checkout --project github.com/OWNER/REPO \
  --base COMPLETE_BASE_OID --head COMPLETE_HEAD_OID --tree COMPLETE_HEAD_TREE_OID
```

`glaeda-project-observe` runs the bounded offline checkout observer once, with Git credentials,
hooks, lazy fetch, replacement objects and ambient configuration disabled, and reports exact
commit and tree, the canonical GitHub source when provable, dirty, untracked and local-only work,
and worktree and submodule topology. `glaeda-repo-query` (`repo-query/v1`) returns object and
merge-base identity, ancestry, per-file numstat, a patch digest, and the patch itself only when it
fits the caller's ceiling; optional fields add exact-tree grep, bounded blob reads, path history
and object lookups. Neither accepts refs or arbitrary Git arguments, fetches, mutates, or grants
reuse authority. See the [resident repo-query
result](experiments/resident-repo-query-big-red-2026-08-31.md).

## Developer state never reaches hostile jobs

Developer checkouts and caches are trusted operator state. They are never mounted into a
hostile or disposable CI job, developer credentials are never inherited by one, and a surviving
checkout, cache, alias or directory name never proves project ownership or verification
authority. The older catalog and `glaeda project enter` design (issue #372) was never built and
is retired; recover it from Git history if that work resumes.
