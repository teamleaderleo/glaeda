# Blazingly hot execution

Glaeda should make useful compute begin with as little repeated setup as possible.

> **Disposable is a capability. Trust decides residency.**

This document is the current high-level map for hot execution. The general product boundary lives in [`COMPUTE_RUNTIME.md`](COMPUTE_RUNTIME.md). The detailed resident project/runtime continuation lives in [`M6_HOT_RUNTIME_HANDOFF.md`](M6_HOT_RUNTIME_HANDOFF.md). Current code and accepted issue/PR decisions win over either document when they move ahead.

## Product objective

The universal objective is **time from declared work to a useful accepted result**, together with throughput and resource cost under exact trust and correctness constraints.

Useful common metrics include:

- request-to-first-useful-compute;
- request-to-first-useful-output;
- request-to-final-accepted-result;
- workload completion wall time;
- fleet throughput under concurrency;
- CPU/RAM/storage/network/accelerator occupancy where relevant;
- hot-state hit/miss/reset benefit;
- recovery time after interruption.

Coding agents, GitHub Actions, edit/test/build loops, and repository verification are major current proving workloads. They add important workload-specific metrics such as edit-to-first-test, edit-to-build, verification completion, and multi-agent fan-out. Those workloads do not define the outer product boundary.

## Current execution model

Glaeda tries to do every reusable, trustworthy piece of preparation before the workload needs it.

```text
work becomes known
-> identify exact workload, trust, inputs, and capabilities
-> select eligible compute and the hottest valid reusable state
-> admit capacity
-> materialize only what must change
-> execute
-> return useful outputs / evidence
-> retain, reset, migrate, quarantine, or destroy state
-> recover from exact durable truth after interruption
```

For hostile or unknown work, the quickest safe path may still be a fresh worker. For trusted repeated work, the quickest path may be a prepared environment or warm pool. For ultra-trusted work, the winning optimization is often to keep valuable state resident.

## Current experiment owners

Keep the implementation lanes separate enough that measurements remain interpretable:

- [#563](https://github.com/teamleaderleo/smolrunner/issues/563) — bounded comparable hot-execution performance receipts;
- [#560](https://github.com/teamleaderleo/smolrunner/issues/560) — resident Linux project storage and project-disk layout;
- [#562](https://github.com/teamleaderleo/smolrunner/issues/562) — cheap task/workspace materialization;
- [#570](https://github.com/teamleaderleo/smolrunner/issues/570) — trusted OverlayFS task views and source anchors;
- [#580](https://github.com/teamleaderleo/smolrunner/issues/580) — task-private Git metadata, inherited source index, OverlayFS source view, and final Git/source readiness proof;
- [#573](https://github.com/teamleaderleo/smolrunner/issues/573) — path-class-specific hot-state mechanism selection;
- [#565](https://github.com/teamleaderleo/smolrunner/issues/565) — crash-safe single-writer project-disk lifecycle;
- [#588](https://github.com/teamleaderleo/smolrunner/issues/588) — exact one-shot Mac-to-Linux guest control transactions;
- [#561](https://github.com/teamleaderleo/smolrunner/issues/561) — resident backend comparison;
- [#770](https://github.com/teamleaderleo/smolrunner/issues/770) — general compute-workload boundary and generic workload seam.

## The leading resident Linux path

For the current development/CI workload family, the leading trusted path is already more specific than “try a faster filesystem.”

```text
macOS durable control plane
        |
        v
resident Linux sandbox
        |
single-writer persistent project filesystem
        |
        +-----------------------------------+
        |                                   |
immutable / mostly-read state              write-heavy mutable state
        |                                   |
clean source anchor                        private CoW / reflink lineage
immutable Git object pool                  compiler/build output
private task Git metadata                  other mutation-heavy state
inherited reviewed source index            |
        |                                   |
        +---------- task execution --------+
                     |
              OverlayFS source view
                     |
                 useful work
```

This is not a custom general-purpose filesystem. Glaeda composes mature Linux filesystem primitives according to the mutation behavior of each state family.

### Mostly-read source state: OverlayFS

OverlayFS is the current leading task-view mechanism for clean resident source fan-out, not a hypothetical later comparator.

The accepted model is roughly:

```text
exact immutable source anchor
+ exact immutable Git object-pool generation
+ task-private Git directory
+ inherited reviewed clean source index
+ unique task upper/work directories
-> exact OverlayFS mount
-> hardened non-mutating Git/source proof
-> task ready
```

The lower source remains immutable for the lifetime of child task leases. Task edits and whiteouts live in the private upper layer. Git refs/config/index/new objects are task-private rather than shared writable metadata.

Owned hosted-ARM evidence recorded in #570/#580 found, for one representative source-view comparison:

```text
tasks    ordinary materialization    OverlayFS + inherited index    ready-state growth
1        ~76.2 ms                    ~15.9 ms                       ~8.05 MiB -> ~4 KiB
8        ~224.3 ms                   ~52.4 ms                       ~64.3 MiB -> ~32 KiB
32       ~695.6 ms                   ~175.2 ms                      ~257.3 MiB -> ~160 KiB
```

All compared views passed the non-mutating Git proof. The source-view model also worked on ext4, so XFS is not a prerequisite for OverlayFS source fan-out.

### Git metadata: private, objects shared immutably

The preferred task path does not rely on one shared writable common Git directory.

A resident immutable object-pool generation supplies base objects while each task gets private refs, config, index, HEAD, and new objects. The current V1 direction uses an explicit reviewed equivalent of:

```text
git clone
  --reference <exact-immutable-pool-generation>
  --no-local
  --no-checkout
  --separate-git-dir <exact-private-task-gitdir>
  --template <reviewed-empty-template>
  <exact-immutable-pool-generation>
  <exact-task-target>
```

The active pool remains frozen and task-unwritable. New/updated pool state is published as a successor generation instead of mutating the active generation in place.

### Write-heavy state: private CoW/reflink lineage

OverlayFS is not the universal answer.

Putting warmed compiler output behind a shared lower can create expensive copy-up. Current measurements showed that clearly: one synthetic eight-task Rust comparison was roughly **5.70 s** with the write-heavy target state behind OverlayFS versus roughly **2.77 s** with task-private reflink/CoW build-state lineage.

The resulting policy direction is path-class-specific:

```text
immutable / mostly-read source and suitable dependency state
-> immutable resident base + OverlayFS task view

write-heavy compiler/build state
-> task-private CoW/reflink lineage where supported

state with no worthwhile reusable parent
-> private empty state

shared mutable cache
-> only under an explicit producer/consumer authority model
```

Do not promote a filesystem primitive because its microbenchmark is attractive. Promote it for the state family where complete useful-result measurements show it wins.

### Ultra-trusted local hot-run prototype

`scripts/hot-run` is a deliberately small Linux developer-loop prototype. It binds a task Git
worktree onto the pathname of a warmed resident worktree and gives the task a persistent private
OverlayFS upper over the resident Cargo target:

```bash
/path/to/resident/scripts/hot-run \
  --task /path/to/task-worktree \
  -- cargo test --locked --lib --bins
```

The task and resident must be worktrees of the same Git repository. The resident worktree remains
the stable compiler pathname, while task source changes remain in the ordinary task worktree and
compiler writes land in a task-private state directory. One non-blocking lock prevents concurrent
mounts of the same upper/work pair. The command receives the caller's terminal, environment, host
filesystem, devices, processes, and network and returns the child's status. This is explicitly an
ultra-trusted performance tool, not a security boundary or result-authority mechanism.

Unprivileged bubblewrap maps host identities outside the caller's user namespace to the overflow
identity. Commands that deliberately validate host ownership, mount identity, or other physical
namespace facts must use the ordinary host path and final verifier. The hot path is for compilation,
language tools, repository scripts, and tests whose semantics do not depend on those host facts.

On Ubuntu 26.04.1, Linux 7.0, ext4, Rust 1.97.1, and Glaeda `7f40597`, one bounded G0 probe used the
same `cargo test --locked --lib --bins --no-run` command at four Cargo jobs. A fresh ordinary
worktree/path took about 39–42 seconds to compile. An exact task over a resident target started in
0.03 seconds; after a one-line task edit, the private target upper completed in 9.21 seconds. A
direct unmodified full library/binary test execution was 2.63 seconds. These observations promote
the stable-path/private-upper experiment, not OverlayFS as the final write-heavy storage default.
The resulting script measured 0.05 seconds for no-op Bash and Python commands, 0.13 seconds for an
exact Rust no-run check, and 10.06 seconds after the same one-line edit versus a 37.56-second
path-cold rebuild.

The default task state is an opaque path under the user's cache directory. It is expendable:
discarding it or selecting a new empty `--state` path produces a private cold upper and a normal
compiler rebuild. Bubblewrap and kernel OverlayFS are required for cross-worktree mode; running
directly in the resident worktree does not require either.

## Linux mount path

The privileged OverlayFS mount machinery is also concrete.

The landed executor uses safe Rust through the Linux new mount API with held descriptors:

```text
fsopen("overlay")
-> fsconfig_set_fd(lower)
-> fsconfig_set_fd(upper)
-> fsconfig_set_fd(work)
-> exact second confirmation
-> FSCONFIG_CMD_CREATE
-> fsmount
-> move_mount onto exact merged target
-> reopen visible target
-> exact post-mount observation
```

No free-form `/bin/mount` option string is the product interface. Lower/upper/work/merged roles are descriptor-bound. The final safe empty-workdir confirmation is immediately before `FSCONFIG_CMD_CREATE`, which physical probes identified as the first observed operation that mutates OverlayFS work state.

Owned ARM measurements recorded mount-only fan-out around **7.7 ms / 23 ms / 62 ms for 1 / 8 / 32 mounts** in the measured path.

The syscall machinery is deliberately separate from production authority. Physical task-view activation still depends on the exact project-disk/filesystem/resident-sandbox correlation and guest-control composition owned by #565/#640/#588/#707/#708.

## Persistent project filesystem

The useful resident project filesystem is Linux-native state, not a macOS checkout projected into the guest hot path.

Current direction:

```text
Mac
  durable control / backend ownership
  Lima/VZ backing storage
        |
        v
Linux guest
  persistent project disk
  ext4/XFS as measured
  native Linux page cache
  source anchors
  Git pools
  package/compiler state
  indexes/services
  task-local OverlayFS/CoW views
```

This keeps Git, package-manager, compiler, index, cleanup, and other filesystem-heavy workload operations on Linux filesystem semantics.

The project disk itself remains working/acceleration state. Its lifecycle is single-writer, generation-bound, revalidated, and recoverable. Disk presence, path names, filesystem UUIDs, mount IDs, and surviving bytes never create ownership or result authority by themselves.

## Storage experiments still worth running

The existence of the current OverlayFS path does not settle the physical storage choice underneath it.

Continue comparing complete workloads across bounded candidates such as:

```text
resident root/ext4
dedicated persistent ext4 project disk
dedicated persistent XFS/reflink project disk
internal storage vs suitable TB5 external storage
```

Measure:

- request/edit-to-first-useful-result;
- source/task-view creation;
- compiler/build-state reuse;
- package/dependency behavior;
- cleanup/reset latency;
- guest allocated blocks;
- Mac backing-file growth;
- CPU/RAM cost;
- concurrency and tail latency;
- recovery after VM/controller interruption.

Block-level dedupe/compression or another filesystem earns a place only when current measurements expose a remaining problem it can plausibly solve.

## Backend programme

Keep project/workload semantics constant while comparing resident execution backends.

Current candidates include Lima/VZ and Apple `container` / Containerization. Native Linux hosts and additional owned fleet nodes remain important reference/target classes.

The meaningful comparison is the complete result path, not blank-environment startup alone:

```text
work becomes known
-> eligible host/backend selected
-> hot state admitted
-> required view/state available
-> useful work starts
-> useful output returns
```

A backend that boots a blank environment quicker can still lose to a resident backend holding valid project/compiler/data state. A better underlying backend should improve Glaeda without redefining Glaeda.

## Parallel preparation

When useful state is not already resident, overlap independent preparation:

```text
work observed
├─ choose host/backend
├─ reserve capacity
├─ materialize/resume execution environment
├─ hydrate missing immutable inputs
├─ resolve reusable state generations
├─ prepare integration-specific handoff where needed
└─ compile workload-specific execution/verification plan
```

Join only at real dependency boundaries.

## Hot-state classes

Do not call every reusable thing “cache.” Distinguish at least:

- **immutable reusable generation** — source/object pools, prepared images, datasets, model assets, derived artifacts;
- **lease-scoped mutable resident state** — project checkout, compiler trees, indexes, databases, resident services;
- **task-local mutable state** — edits, debugging state, private build output, task upper layers;
- **shared mutable state** — only with an explicit producer/consumer and poisoning model;
- **disposable physical cache** — freely evictable optimization state.

Each family needs exact validity parents, resource accounting, reset/revalidation behavior, and a cold reacquisition route when it is classified as rebuildable.

## Correctness rule

Hot state carries performance value. Durable owned facts and fresh exact observation carry execution authority.

When validity or ownership becomes ambiguous:

```text
revalidate
-> reset if required
-> quarantine when current ownership is unclear
-> destroy and reconstruct when cheaper or safer
```

Losing hot state should cost latency and compute, not the ability to decide the next safe action.

## First proving workloads

### Quarry

Use Quarry to exercise:

- persistent Python/research environments;
- edit-to-focused-pytest loops;
- repeated task fan-out;
- data/index residency;
- concurrent independent research jobs;
- host economics across Apple VZ and native Linux references.

### Glaeda

Use Glaeda itself to exercise:

- resident Rust source/task views;
- private Cargo incremental build state;
- `sccache` usefulness;
- immutable Git pools;
- exact one-shot guest-control transactions;
- project-disk and mount recovery;
- cold versus prepared versus resident execution.

### Non-repository compute

The generic workload seam under #770/#773 should also prove that hot admission, capacity, lifecycle, and placement are not repository-only. A bounded dataset transform, render, or similar non-repository workload can use the same execution kernel while its adapter owns result semantics.

## Promotion rule

A hot optimization becomes preferred only when controlled evidence shows a useful complete-loop improvement and its validity/reset contract is explicit.

A promotion receipt should record at least:

```text
workload class
baseline identity
candidate identity
exact validity inputs
p50 / p90 useful-result latency
CPU/RAM/storage effect
concurrency effect
reset/invalidation behavior
fallback path
```

The end state is simple to describe even if the internals are exacting:

> **Keep the useful world hot, materialize only what changed, and let trust decide what may survive.**
