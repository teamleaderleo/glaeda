# Blazingly hot execution

SmolRunner should feel like an execution runtime that anticipated the next agent action.

> **Disposable is a capability, not a mandate. Trust decides residency.**

This document turns that product direction into an experimental programme.

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

### Baseline

Record the current prepared/resident guest filesystem and virtual-disk behavior:

- worktree creation/removal;
- checkout and Git-object hydration;
- pnpm/npm/Bun install from warm state;
- hardlink-heavy package layouts;
- Cargo incremental build/test;
- Maven/Gradle build trees;
- large small-file deletion;
- index creation/update;
- logical guest bytes;
- Mac-side Lima instance/backing-file growth;
- CPU cost during write/read/delete phases.

### Candidate families

Evaluate mature Linux choices that plausibly improve agent workloads, including:

- XFS with reflink support;
- XFS with a mature block-level dedupe/compression layer when deployment and recovery costs are acceptable;
- ext4 as a simple throughput baseline;
- Btrfs where snapshot/reflink/compression behavior creates a credible advantage;
- another mature Linux option only after a measured workload identifies a missing capability.

The benchmark decides. Filesystem preference carries zero product authority by itself.

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
- immediate reuse versus post-idle reuse.

### SmolRunner

Measure:

- resident checkout;
- Cargo incremental build/test state;
- `sccache` utility;
- worktree/task-fork behavior;
- Linux storage candidates;
- prepared disposable versus resident trusted execution.

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
