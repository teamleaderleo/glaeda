# Trusted pnpm task dependency benchmark

This benchmark keeps the pnpm dependency-import mechanism separate from Git source materialization and compiler/build-state reuse.

It is for ultra-trusted Linux project work only. Hostile or unknown work stays on the fresh disposable lane.

## Question

Given an exact clean repository, an already-resident pnpm store, and a task-private Git worktree, which pnpm import method gets the task to useful work with the least wall time and physical write amplification?

The closed treatments are:

- `auto`: observe which physical mechanism pnpm selected;
- `hardlink`: measurement control only;
- `clone`: task-private CoW candidate and the preferred treatment to test on XFS `reflink=1`.

On current pnpm 12, Linux `auto` tries hardlink before clone, so `auto` is an observation arm rather than a task-execution policy. `hardlink` can couple package bytes through a shared inode. Its per-task directory root does not make those package bytes task-private. The harness therefore refuses repository probe scripts in the explicit hardlink arm. If `auto` physically resolves to hardlinks, the harness also refuses the probe after observing that evidence. `clone` must prove shared extents through FIEMAP in every task or the sample fails.

## What stays fixed

Each sample requires:

- one exact clean source commit/tree;
- plain `package.json` and `pnpm-lock.yaml`, with their SHA-256 identities recorded;
- one exact Git executable digest/version, pnpm executable digest/version, Node executable digest/version, and effective pnpm configuration digest;
- one already-present pnpm store;
- `pnpm install --frozen-store --offline --frozen-lockfile --ignore-scripts`, so the measured shared store is dependency input rather than writable task state;
- one unique detached Git worktree, unique `node_modules` root, and task-local `node_modules/.pnpm` virtual store per task;\n- exact Git proof before the first useful command;\n- repository probe execution only after dependency bytes are proven non-hardlinked;
- a 600-second default cohort deadline with TERM/KILL process-group ownership;
- ordinary Git proof that tracked task state still equals the source commit/tree.

The task workspace itself never becomes a shared mutable cache. The install pins pnpm's virtual store to the task-local project layout rather than the machine-global virtual-store mode. The resident pnpm store is dependency state and remains a separate mechanism from task source and build output. The measured install enables pnpm's frozen-store mode so the shared store receives no writes during the task-import window.

## Matrix

Run `auto`, `hardlink`, and `clone` at widths 1, 8, and 32 on the same selected project filesystem. Use repeated samples sufficient to publish p50/p90/p99.

The first filesystem matrix remains bounded to the #560 candidates:

1. ordinary resident ext4;
2. dedicated ext4 project disk;
3. dedicated XFS project disk with `reflink=1`.

Do not add another filesystem until these results leave a specific unanswered question.

Example XFS run:

```bash
scripts/benchmark-pnpm-task-dependencies \
  --source /path/to/exact-clean-project \
  --scratch-root /path/to/owned-xfs-scratch \
  --pnpm /absolute/path/to/pnpm \
  --fanout 8 \
  --import-method clone \
  --repetitions 20 \
  --probe-script test \
  --output /path/outside/scratch/pnpm-clone-8.json
```

For the hardlink control, omit `--probe-script`.

## Receipt

Each sample records the whole task preparation loop and keeps the timing reservoirs distinct:

- ordinary Git worktree creation;
- dependency install;
- readiness verification and task-known to workspace-ready;
- direct task-ready storage observation cost;
- task-known to first command start and completion;
- optional task-known to repository probe completion, with the selected script name receipt-bound by digest;
- cleanup;
- child CPU time.

Storage evidence includes:

- signed filesystem physical-byte and inode deltas at workspace-ready time;
- exact `node_modules` logical/allocated bytes and inode counts;
- exhaustive regular-file and multiply-linked regular-file counts;
- sampled FIEMAP shared-extent observations;
- signed bytes/inodes reclaimed by cleanup;
- signed residual bytes/inodes after cleanup.

The filesystem deltas stay signed. Negative task-ready consumption or negative cleanup reclaim remains visible as accounting noise, concurrent filesystem activity, or cleanup debt instead of being clamped to zero.

The benchmark binds the full scratch mount identity before materialization. At task-ready time it reads capacity with direct `stat`/`statvfs` calls against that already-bound device and receipts the observation cost separately, avoiding a `findmnt` child in front of the first useful command. Later full observations must still agree with the bound filesystem identity.

Summary distributions publish p50/p90/p99 over successful samples. Explicit `clone` and `hardlink` samples fail unless their requested physical mechanism is observed in every task. Any multiply-linked regular dependency file makes the task hardlink-observed and blocks repository probe execution. `auto` reports the selected physical mechanism without granting it policy authority.

## Interruption boundary

The live benchmark generation owns only materialization attempts it actually starts. A partial `git worktree add` attempt enters cleanup ownership before the Git command is invoked, so an error after registration can still be checked against Git's worktree registry and cleaned exactly. Dependency/command cohorts run in owned process groups with deadline TERM/KILL cleanup.

Controller death remains a different authority class. After restart, a directory or Git registration alone grants no ownership. Resume, cleanup, quarantine, or rebuild must come from the durable Glaeda task-view lease/generation plus fresh exact process/worktree evidence. The current benchmark deliberately does not mint that authority; the resident-project task-view lifecycle owns the physical restart composition.

## Composition policy

Use this experiment beside the existing #562 source-materialization and #560 Rust build-state receipts:

- source: ordinary Git at width 1 where it wins; Glaeda ordinary/reflink fan-out when measured fan-out gains justify it;
- Node dependencies: resident read-only generations when the workload can consume them directly; otherwise prefer task-private pnpm `clone` on proven reflink storage when this matrix shows a complete-loop win or meaningful write-amplification reduction;
- Rust build output: private identity-bound state, with CoW/reflink seeding on capable filesystems;
- Python: resident environment through its separate reviewed project path;
- native Apple/Xcode: separate reviewed Glaeda path.

Directory presence grants no task ownership or recovery authority. Controller-death adoption/cleanup still requires the Glaeda task/lease identity owned by the resident-project lifecycle; this benchmark creates only disposable experiment generations and reports cleanup debt as failure.
