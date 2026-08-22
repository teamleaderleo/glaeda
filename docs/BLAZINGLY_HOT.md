# Blazingly hot execution

SmolRunner should feel like an execution runtime that anticipated the next agent action.

> **Disposable is a capability. Trust decides residency.**

This document turns that product direction into an experimental programme.

## Current experiment owners

Keep the first implementation lanes separate enough that each experiment answers one question:

- [#563](https://github.com/teamleaderleo/smolrunner/issues/563) — one bounded hot-execution performance receipt shared by every experiment;
- [#560](https://github.com/teamleaderleo/smolrunner/issues/560) — resident Linux project storage and persistent project-disk layout;
- [#562](https://github.com/teamleaderleo/smolrunner/issues/562) — nearly-free task/worktree materialization on a hot project;
- [#561](https://github.com/teamleaderleo/smolrunner/issues/561) — Lima/VZ versus Apple `container machine` for resident trusted projects.

The intended proving order is:

```text
one comparable receipt vocabulary
-> choose useful project-storage primitives
-> make task fan-out cheap on those primitives
-> compare resident VM backends with the workload held constant
-> feed measured history into routing/verification optimization
```

## North-star experience

For an ultra-trusted active project:

```text
agent edits
-> relevant verification starts almost immediately
-> incremental compiler/package/index state is already warm
-> result returns
-> useful state remains ready for the next iteration
```

For hostile or unknown work:

```text
job arrives
-> fresh isolated prepared worker
-> one bounded execution
-> terminal result
-> exact teardown
-> prove absence
```

Both paths use the same durable execution truth and recovery discipline.

## Benchmark the complete loop

Measure:

- queue-to-first-useful-command;
- edit-to-first-test-result;
- edit-to-final-relevant-verification;
- total task wall time;
- fleet throughput under 1 / 2 / 4 / N concurrent agents;
- CPU/RAM/disk residency cost;
- hot-state hit/miss/reset events;
- cold reconstruction time.

Track phase-level reservoirs:

```text
routing / admission
sandbox materialization or resident reuse
repository ready
package/dependency ready
build/compiler state ready
index/service ready
first command
first relevant result
final relevant result
teardown or residency transition
```

## First execution matrix

For one representative trusted project compare:

```text
cold disposable
prepared disposable
warm-pool disposable
resident project after idle
resident project immediate reuse
resident task loop
```

Repeat under concurrent agent load.

Use Quarry first for Python/project residency and SmolRunner itself for Rust incremental build state.

## Linux storage programme

SmolRunner already places repository filesystem churn inside Linux. The host Mac sees Lima/VZ backing storage while Git, package managers, compilers, indexes, and cleanup operate in the guest.

Make that Linux storage path a deliberate benchmark target.

### Useful primitives already available

Current mature components already expose several pieces SmolRunner can compose and measure:

- Lima supports [standalone additional disks](https://lima-vm.io/docs/config/disk/) that can persist independently of a VM instance, giving a resident project disk its own lifecycle;
- Linux/XFS supports reflink/CoW file cloning, giving task materialization a cheap unchanged-file primitive;
- pnpm supports [`packageImportMethod=clone`](https://pnpm.io/settings#packageimportmethod), allowing a reflink-capable filesystem to CoW-import package files from the content-addressable store;
- Git supports `worktree add --no-checkout`, giving SmolRunner a clean point to pre-populate exact unchanged bytes before ordinary checkout completes and verifies the task tree.

Treat each primitive as a benchmark candidate. Compose them only after the individual value is visible.

### Baseline

Record the current prepared/resident guest filesystem and virtual-disk behavior:

- worktree creation/removal;
- checkout and Git-object hydration;
- pnpm/npm/Bun install from warm state;
- hardlink/reflink-heavy package layouts;
- Cargo incremental build/test;
- Maven/Gradle build trees;
- large small-file deletion;
- index creation/update;
- logical guest bytes;
- allocated guest blocks;
- Mac-side Lima instance/project-disk backing-file growth;
- CPU cost during write/read/delete phases.

### First bounded storage matrix

Start with three configurations:

```text
A. current resident root/ext4
B. dedicated persistent ext4 project disk
C. dedicated persistent XFS project disk + explicit reflink task materialization
```

This isolates two questions first: whether separating project residency from VM residency helps, and whether reflinks make task fan-out materially cheaper.

A block-level dedupe/compression layer such as dm-vdo becomes the next experiment only when A-C leave a measured disk-growth reservoir worth attacking. Its fixed metadata, memory, CPU, recovery, and discard costs belong in the result.

Btrfs or another mature filesystem enters only when a measured workload exposes a capability gap the first matrix cannot answer.

### Cheap task forks

Compare:

```text
many independent worker/project copies
```

with:

```text
one resident project base
-> cheap task-local writable fork
-> agent edits/tests
-> retain task state or discard fork
```

Measure 1 / 8 / 32 / N task forks for:

- creation latency;
- first command latency;
- install/build/test latency;
- delete/reset latency;
- physical disk growth;
- CPU overhead;
- concurrent contention.

For the reflink candidate, prefer Git-owned semantics:

```text
git worktree add --no-checkout
-> reflink exact matching tracked regular files
-> ordinary git checkout fills changed/missing bytes
-> hardened Git observation proves the final target state
```

For pnpm workloads, measure `auto`, `hardlink`, and `clone` import methods separately.

OverlayFS remains a later task-view comparator when reflink-aware worktrees leave a meaningful measured gap.

## Repository heat

Explore exact reusable repository state:

- local Git object/repository seeds;
- resident trusted checkout;
- project-local worktrees;
- shared read-only object/source bases;
- task-local writable layers;
- background hydration/prefetch while admission runs;
- explicit stale/dirty detection before cross-task reuse.

Git/source identity remains canonical. A surviving checkout carries working state only.

## Dependency and compiler heat

Treat these as distinct state families instead of one generic cache:

- package-manager stores;
- resolved dependency environments;
- `sccache` / `ccache` objects;
- repository-native incremental build trees;
- container/build layers;
- generated derived artifacts;
- language/toolchain generations.

For each family record:

```text
bytes retained
build/restore cost
hit/miss count
observed time saved
last useful hit
invalidation cause
reset cost
```

Keep state that earns its residency cost.

## Service and index heat

For ultra-trusted projects, benchmark persistent:

- language servers;
- code-search indexes;
- test watchers/daemons;
- compiler servers;
- buildkit/container builders;
- local databases/fixtures;
- repository-specific dev services.

Each service needs an explicit project lease, validity parents, resource accounting, and reset path.

## Resident backend programme

Once the project workload and storage baseline are stable, compare resident execution backends without changing the project semantics at the same time.

The first local comparison is Lima/VZ against current Apple `container machine`.

For Apple `container machine`, use `home-mount=none`: keep project source, package state, build output, and indexes on Linux storage instead of feeding the Mac checkout directly into the agent loop.

Measure:

- cold project-machine creation;
- stopped-to-first-command resume;
- immediate resident command;
- edit-to-test/build;
- idle and peak host RAM;
- host RAM retained after a memory-heavy workload exits;
- project disk growth;
- stop/start preservation;
- full destruction and cold reconstruction;
- exact ownership/recovery quality after controller or host restart.

The selected backend should minimize total agent latency under fleet concurrency while retaining a clean recovery story.

## Parallel preparation

When a resident sandbox is unavailable, overlap every independent preparation step:

```text
work observed
├─ choose target / resident project
├─ reserve capacity
├─ fork/materialize sandbox
├─ hydrate missing Git objects
├─ resolve eligible cache/artifact generations
├─ prepare JIT handoff when applicable
└─ compile verification plan
```

Join only where one step truly depends on another.

## Correctness rule

Hot state carries performance value. Durable owned facts and fresh exact observation carry execution authority.

When a validity parent changes or residency becomes ambiguous:

```text
revalidate
-> reset if required
-> quarantine when ownership/validity is unclear
-> destroy and reconstruct when that is cheaper or safer
```

Every hot state family needs a cold reconstruction path that produces the same repository-defined semantics.

## First proving programme

### Quarry

Measure:

- resident checkout and Python environment;
- edit-to-focused-pytest latency;
- progressive verification receipt reuse;
- deterministic test partitioning;
- project-local indexes/derived data;
- immediate reuse versus post-idle reuse;
- task/worktree fan-out on the selected project storage;
- Lima versus Apple container-machine residency after the storage workload is held constant.

### SmolRunner

Measure:

- resident checkout;
- Cargo incremental build/test state;
- `sccache` utility;
- worktree/task-fork behavior;
- Linux storage candidates;
- prepared disposable versus resident trusted execution;
- backend memory/resume behavior under concurrent projects.

## Promotion rule

A hot-path optimization becomes preferred when controlled evidence shows a useful end-to-end improvement and its validity/reset contract is explicit.

A useful promotion receipt records:

```text
workload class
baseline identity
candidate identity
exact validity inputs
p50 / p90 useful-result latency
CPU/RAM/disk effect
concurrency effect
reset/invalidation behavior
fallback path
```

SmolRunner should explain why a run was hot, cold, reset, or resident in ordinary language and typed JSON.
