# M6 hot-runtime handoff

Status snapshot: 2026-08-24

This document is the durable continuation point for SmolRunner's "blazingly hot" trusted-agent work. It records the current product conclusion, what is already on `main`, owned GitHub Actions evidence, failed/corrected experiments, unresolved questions, and the next implementation sequence.

Use this document together with [`BLAZINGLY_HOT.md`](BLAZINGLY_HOT.md), the current source on `main`, and the canonical issues linked below. When this document conflicts with current code or a later accepted issue/PR, current accepted code wins.

> **Disposable is a capability. Trust decides residency.**

## Current answer

The leading trusted-agent runtime is no longer "put everything on XFS" or "make Git worktrees cheaper".

The measured design is path-class specific:

```text
Mac durable control plane
        |
        v
resident Linux sandbox
        |
single-writer project disk
        |
        +------------------------------+
        |                              |
immutable source / dependency state   write-heavy build state
        |                              |
immutable Git object generation       private CoW lineage
        |                              |
private task Git metadata              |
        |                              |
        +-------- OverlayFS task view--+
                       |
                     agent
                       |
                edit -> test -> edit
```

The current preferred mechanisms are:

- **source and mostly-read project/dependency state:** immutable resident lower state + task-local OverlayFS upper/work;
- **Git task identity:** private task Git metadata backed by an immutable resident object generation, once the secure alternates handoff is fully proven;
- **write-heavy compiler/build output:** task-private CoW/reflink lineage where the filesystem supports it;
- **fallback:** ordinary Git/private-empty state whenever reuse identity, capability, or authority is missing;
- **hostile/unknown work:** the strict fresh disposable worker path remains separate.

Hot state is acceleration only. Durable execution truth and fresh exact observation remain authoritative.

## Current `main`

At this snapshot, current `main` is `1934b4c5f5e15151c68ed9429770fa56bdcc3357`, merge of PR #594.

The M6 substrate already landed includes:

| Capability | Owner / landed PR | Current role |
| --- | --- | --- |
| bounded hot-execution performance receipt | #563 / #564 | comparable source/candidate/backend/heat/milestone/storage/resource observations |
| crash-safe single-writer project-disk lease core | #565 / #567 | logical project-disk and attachment generations; stale/foreign/unknown lock policy; no physical Lima disk mutation yet |
| Git index-v2 stat patcher | #568 / #569 | pure byte handling for verified stat-cache publication |
| trusted source-anchor/task-view lifecycle | #570 / #571 | exact anchor/task leases and create/mount/ready/run/cleanup ordering |
| sealed trusted OverlayFS mount plan | #572 / #575 | exact lower/upper/work/merged identities, mount namespace, OverlayFS capability, fixed `nodev,nosuid` policy |
| hot-state path-class policy | #573 / #579 | overlay / private-CoW / private-empty / reviewed-shared selection from exact reuse identity |
| immutable Git object-pool generation lease core | #581 / #583 | pool generation identity, consumer leases, draining and retirement |
| pre-mutation mount-plan reconfirmation | #576 / #582 | current anchor/task authority + current filesystem/kernel evidence before privileged work |
| exact OverlayFS role descriptor lease | #584 / #587 | retained lower/upper/work/merged `OwnedFd`s, no-follow traversal, second confirmation |
| fixed immutable Git-pool marker | #586 / #590 | strict 64-byte binding-digest + nonzero generation-nonce marker |
| merged-parent descriptor + basename | #593 / #594 | descriptor-relative post-attach reopen of the visible mount target |

PR #595 also refreshed the README to reflect the landed M6 primitives.

None of the landed OverlayFS modules currently gives normal product code authority to mount a trusted task view.

## Owned GitHub Actions evidence

All results below came from repositories/resources owned by `teamleaderleo`; the research lane deliberately avoided contacting upstream projects for help and generally avoided installing extra packages.

### GitHub-hosted ARM is a useful benchmark lab

The owned FEX fork already proved native GitHub-hosted `ubuntu-24.04-arm` execution. SmolRunner then used small owned workflows to compare hosted ARM and x64 and to exercise local loopback ext4/XFS/OverlayFS behavior.

One representative hosted ARM assignment was native aarch64 Neoverse-N2 with four cores. Workspace/temp were ext4 and ordinary reflink cloning was unsupported there until a local XFS loopback volume was created.

Architecture conclusions are workload-dependent. A generated dependency-free Rust crate showed large cold-runner variance across fresh VMs, while warm/incremental phases clustered much more tightly. Residency was the more universal win than ARM-vs-x64 placement.

### Ordinary Git worktree metadata is already cheap

On hosted ARM/ext4, a representative SmolRunner checkout measured roughly:

```text
git worktree add --no-checkout metadata: ~1.8-2 ms
full worktree materialization:             ~26-31 ms
```

This changed the #562 question. Git's administrative worktree operation is not the main latency reservoir; writing the working tree is.

Two-phase fan-out on ordinary ext4 already helped under concurrency:

```text
1 task:  roughly parity
8 tasks:  about 23% lower wall time
32 tasks: about 11% lower wall time
```

The benefit came from creating task metadata cheaply, then allowing tree writes to fan out.

### XFS/reflink is valuable, but for specific state

On a local hosted-ARM XFS volume with `reflink=1`:

```text
64 MiB reflink:       ~1.6 ms, ~0 new used bytes before modification
64 MiB ordinary copy: ~12.5 ms, +64 MiB
```

A warmed SmolRunner tree reflink also used dramatically less additional filesystem space than an ordinary copy.

Plain Git worktree materialization itself was approximately the same on ext4 and XFS. The XFS win was the CoW primitive, not universal small-file superiority.

This is why XFS remains a strong candidate for private write-heavy lineages while OverlayFS can still be the main source-view mechanism on ext4.

### OverlayFS is the leading multi-agent source-view primitive

The strongest source-view experiment used:

```text
exact resident source
-> task Git metadata
-> OverlayFS lower=resident source
-> unique task upper/work
-> inherited clean source index
-> non-mutating Git proof
```

On hosted ARM/XFS, stripped-down fan-out measured approximately:

| Tasks | Ordinary Git | OverlayFS + inherited source index | Task-view growth |
| ---: | ---: | ---: | ---: |
| 1 | 76.2 ms | **15.9 ms** | ~8 MiB -> **4 KiB** |
| 8 | 224.3 ms | **52.4 ms** | ~64 MiB -> **32 KiB** |
| 32 | 695.6 ms | **175.2 ms** | ~257 MiB -> **160 KiB** |

All 41 views passed the non-mutating Git proof.

The same model also worked on ext4. That removed XFS as a prerequisite for resident source fan-out.

Mutation semantics were also exercised: tracked edits/deletes/mode changes, rename behavior, untracked files/symlinks, whiteouts, `git clean -ffd`, and `git reset --hard`. The resident lower state stayed unchanged.

### OverlayFS alone is weaker for write-heavy compiler output

A generated offline Rust workload exposed the path-class split clearly.

With warmed compiler state inside an OverlayFS lower, Cargo's write-heavy `target/` activity caused enough copy-up that pure overlay lost under concurrency.

Giving each task a private XFS reflink lineage for `target/` improved the eight-task experiment substantially:

```text
pure overlay, 8 tasks: ~5.70 s
hybrid private-CoW target: ~2.77 s
```

A long-lived single task with a private CoW target lineage paid roughly tens of milliseconds once for preparation, then edit/test cycles settled around ~300 ms in that synthetic workload, with a no-edit invocation around ~55 ms.

This is the basis for the current path-class policy: overlay mostly-read state; private-CoW write-heavy state.

### OverlayFS mount policy and kernel API

Owned ARM and x64 probes established that `nodev,nosuid` works for the task view without interfering with normal build execution. `noexec` is intentionally excluded because agents need to run freshly built binaries.

The Linux new-mount API works for this design:

```text
fsopen("overlay")
fsconfig(FSCONFIG_SET_FD, "lowerdir+", lower_fd)
fsconfig(FSCONFIG_SET_FD, "upperdir", upper_fd)
fsconfig(FSCONFIG_SET_FD, "workdir", work_fd)
FSCONFIG_CMD_CREATE
fsmount(nodev | nosuid)
move_mount(mount_fd -> merged_fd)
```

Owned ARM proof also established that all lower/upper/work roles can be supplied by file descriptor and final publication can be FD-to-FD.

Fan-out of the FD-based mount path was small enough to support resident multi-agent use in the hosted experiment: roughly single-digit milliseconds for one mount and tens of milliseconds for 8/32 concurrent mounts.

A pre-opened merged leaf FD remains pinned to the underlay after `move_mount`; it does **not** magically see the new mount. The accepted post-attach observation is:

```text
retained merged-parent FD + exact basename
-> openat(parent, basename, O_PATH|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC)
-> observe the visible mounted target
```

An owned hosted-ARM probe showed the reopened visible FD had the same kernel mount identity as the attached mount object and could read the expected lower-view file. #594 landed the descriptor prerequisite for this.

### Safe Rust mount implementation is viable

`Cargo.toml` forbids project unsafe code. A disposable compile PR (#596, closed without merge) proved that the already-locked `rustix 1.1.4` exposes safe wrappers for the needed APIs and exact flag vocabulary.

The compile probe passed ARM Linux cross-build, Apple Silicon cross-build, Clippy, and the normal verification tail after correcting rustix's kernel-style bitflag names.

The relevant public safe APIs include:

```text
fsopen
fsconfig_set_fd
fsconfig_create
fsmount
move_mount
```

No new dependency and no unsafe escape hatch are required for the basic all-FD mount transaction.

Pinned rustix exposes ordinary `STATX_MNT_ID`; it does not expose `STATX_MNT_ID_UNIQUE`. Do not bypass `unsafe_code = "forbid"` just to obtain the unique variant. A mount ID is observation only and never standalone cleanup authority.

## Git object-pool findings

### Private task Git metadata is preferred over a shared common Git dir

Linked worktrees are very cheap, but multiple tasks share common repository metadata. Owned experiments showed that a task-private Git directory costs only a modest amount more while isolating refs, config, and new objects.

This changed the likely #580 default toward private task Git metadata backed by a frozen resident object generation, with linked worktrees retained as a fallback/research comparator.

Issue #591 exists to rename the lifecycle observation vocabulary from `OverlayLinkedWorktreeObservation` to generic `OverlayGitWorktreeObservation` while retaining a compatibility alias. This is naming only; it must not change lifecycle semantics.

### `chmod 0555` is not enough when the task UID owns the seed

An early "read-only seed" experiment removed write bits but left the seed owned by the runner UID. That does not provide adversarial immutability because the owner can chmod it writable again.

Accepted direction: active immutable pool generations are owned outside the workflow/task UID, initially root-owned in the trusted guest.

### Local clone hardlinks can invalidate ownership isolation

Another early pool experiment used a local clone and then changed ownership on the resulting seed. Local Git cloning may hardlink object files to the producer repository. Changing ownership of the "new" seed can therefore affect shared source inodes.

Accepted direction for v1 publication: the pool generation must have independent regular-file inodes before root takes ownership. `git clone --bare --local --no-hardlinks` is the first boring candidate; later reflink publication is allowed only after proving distinct inode/ownership semantics.

### `sudo -u ... -H` did not fully isolate Git configuration

A cross-owner probe showed an admin-side Git command still attempting to read the runner user's Git attributes path.

Accepted command discipline for admin/root-controlled Git preparation:

```text
env -i
fixed HOME
fixed PATH
system/global Git config disabled
fixed Git executable
fixed argv
bounded stdin/stdout/stderr/deadline
```

Project-controlled Git preparation should not execute as root.

### Three-account guest split

The prepared guest already has a useful privilege split:

```text
root              control / publication / mount authority
smolrunner-admin  trusted non-task Git producer/preparer
smolrunner-runner agent/workflow execution
```

Issue #592 owns the intended immutable-pool publication transaction:

```text
root creates private staging envelope
-> smolrunner-admin creates independent Git generation
-> root proves exact generation / no aliasing
-> root writes #590 marker and freezes/publishes
-> smolrunner-runner consumes read-only
```

Once active, pool generations are never thawed/repacked in place; create a successor generation and drain old consumers instead.

### Still-open Git question

The security-correct cross-owner task clone experiment proved that the task could checkout, add, commit, GC, and could not mutate the root-owned seed. The seed remained byte/mode/ownership identical and `git fsck --full` passed.

However, the first cross-owner `git clone --shared` attempt did not leave the expected task alternates file. Therefore **do not claim the secure private task path is sharing immutable object bytes yet**. A scrubbed `--shared` versus `--reference` probe is still needed to decide whether mature Git creates the desired alternates relationship under the accepted ownership/config boundary or whether #580 should publish a reviewed alternates file explicitly.

## Failed experiments and corrected assumptions

Keep these failures visible so future work does not repeat them.

### Reflink worktree v1: forced checkout/reset erased the CoW win

A verified reflink prototype successfully shared task file extents, but finishing with `git reset --hard` rewrote almost the whole tree. Final Git cleanliness was correct, but the reset destroyed the physical-space advantage.

Correction: successful CoW population must not finish with a forced checkout/reset that rewrites already-proven files.

### Reflink worktree v2: empty index

`git worktree add --no-checkout` leaves the target index empty. Cloning working-tree files and calling `update-index --refresh` cannot make the task clean because there are no index entries to refresh.

Correction: use `git read-tree HEAD` (or equivalent reviewed target-index population) before stat-cache publication.

### Reflink worktree Python scaling was misleading

A Python prototype issuing hundreds of clone operations per task saved large amounts of disk but scaled poorly at 32 tasks. Process/thread launch and Python/GIL overhead polluted the result.

Correction: measure native/bulk implementations before judging the kernel primitive. Native bulk reflink cloning performed much better.

### Git index refresh moved cost instead of removing it

`git update-index --refresh` before verification did not eliminate whole-tree work; it mostly moved it earlier.

This motivated direct index stat publication and later the simpler inherited-source-index OverlayFS path.

### First XFS allocation counters were noisy/negative

Filesystem-used deltas crossed delayed allocation/free boundaries and occasionally went negative.

Correction: synchronize around allocation samples and avoid summing per-file blocks as unique physical space on reflink filesystems.

### Early pool "read-only" result overstated immutability

The first frozen-seed result was task-UID-owned. It proved ordinary Git writes failed, but not that a hostile owner could not chmod it writable.

Correction: root-owned active generation, task UID different from owner.

### Early pool publication risked hardlink aliasing

Local Git clone may hardlink producer objects.

Correction: independent inodes are a publication acceptance requirement.

### Push-only research result branches were sometimes silent

Several workflows wrote a result inside the checkout and then switched to an orphan branch, losing or colliding with the result handoff. Other push-only jobs sat queued long enough that branch absence was ambiguous.

Correction: for critical diagnostics, use failure-tolerant result publication or disposable draft PRs with the normal Actions matrix. Treat missing result branches as missing evidence, not as success/failure.

### Physical probe #597 failed in the harness

Draft research PR #597's first workdir-timing C probe failed compilation because `system()`'s return value was ignored under `-Werror`. The safe-rustix and Git-alternates probes in later steps were skipped.

This is a harness defect, not an OverlayFS result. Fix/re-run or supersede it; never cite that run as evidence about workdir mutation timing.

## Open questions

### 1. Exactly when does OverlayFS first mutate `workdir`?

`TrustedOverlayMountPlan::confirm()` currently requires the sealed workdir to remain empty/exact. The executor wants a second confirmation as late as possible before mount publication.

The likely safe boundary is after `fsconfig_set_fd` and before `FSCONFIG_CMD_CREATE`, but the owned timing probe must confirm when the kernel first changes workdir metadata/content. #597 attempted this but its C harness failed before running.

Do not freeze the executor's second-confirmation stage until this is physically observed.

### 2. Can the accepted cross-owner private task Git dir use mature Git alternates directly?

Need a scrubbed, ownership-safe comparison of at least:

```text
git clone --shared
git clone --reference <frozen seed>
```

under the accepted root/admin/runner account split and fixed environment.

Record whether the task actually uses an alternates file, task-local object bytes, creation latency/space, and whether the frozen seed remains unchanged.

### 3. What is the exact Lima standalone-disk layout on the operator Mac?

This is the main production physical blocker.

#565 P2 needs an owned read-only receipt of the real `${LIMA_HOME}/_disks/<name>` directory/backing/lock layout from the operator Mac running the pinned Lima generation.

Hosted macOS does not have `limactl`, and the repository currently has no self-hosted operator-Mac Actions label to assume. Do not guess the backing filename or lock semantics in authority code.

### 4. Mount identity token

Pinned rustix exposes ordinary `STATX_MNT_ID`, not `STATX_MNT_ID_UNIQUE`. Keep unsafe forbidden.

The executor should bind any mount-ID token to task/anchor/mount-namespace/correlation generation and use it as observation only. Cleanup must freshly prove the exact owned task mount through the accepted role/task evidence; mount ID alone grants no unmount authority.

### 5. How much generation auditing belongs in the hot path?

Current answer: almost none.

#585 P1 should prove exact ownership/generation/marker/descriptors and no nested alternates. A recursive frozen-inventory/accounting audit is a separate generation-promotion/reconciliation operation, not a per-task command prerequisite.

## Current active owners

The important current issues are:

- **#565** — crash-safe single-writer project-disk leases; P1 landed, P2 physical Lima disk observation still required;
- **#576** — descriptor-only trusted OverlayFS mount executor programme;
- **#580** — agent-ready Git/source task-view publication;
- **#581** — immutable resident Git object-pool generations and private task Git metadata;
- **#585** — read-only physical immutable Git-pool observation;
- **#588** — exact one-shot Linux guest-control transactions from the Mac controller;
- **#589** — sealed all-FD OverlayFS mount transaction behind project-filesystem correlation proof;
- **#591** — generic Git-worktree observation vocabulary;
- **#592** — immutable Git-pool publication through root/admin staging;
- **#593 / #594** — merged-parent descriptor prerequisite; completed/merged;
- **#566** — GitHub Actions benchmark lab and owned research receipts.

At this snapshot, open PRs #597 and #598 are **research drafts only** and must not be merged as product changes.

## #589 implementation status

The all-FD executor is the leading next runtime code slice.

A research candidate exists on the #598 branch. The intended sequence is:

```text
sealed mount plan
-> current plan.confirm(...)
-> exact descriptor-lease confirm
-> sealed project-filesystem correlation proof
-> fsopen("overlay")
-> fsconfig_set_fd lower/upper/work
-> second pre-create confirmation at the latest physically valid boundary
-> fsconfig_create
-> fsmount(nodev,nosuid)
-> observe mount ID
-> move_mount FD-to-FD
-> reopen visible target from merged-parent FD + basename
-> require expected mount/OverlayFS observation
-> return bounded receipt
```

Before attach, failures drop unattached FDs and publish no mount. After attach, ambiguity becomes explicit cleanup/revalidation debt; never report `ready` from syscall success alone.

The required project-filesystem correlation proof must have no normal production constructor until #565 P2 can prove the exact project-disk generation + resident sandbox + filesystem device relationship.

## #588 guest-control integration

The Mac cannot hold Linux guest FDs. The first integration should use one-shot exact guest transactions, not a daemon:

```text
Mac durable controller
-> exact resident Lima sandbox
-> fixed limactl shell invocation
-> sudo --non-interactive
-> scrubbed environment
-> pinned root-owned Linux SmolRunner binary
-> one closed protocol operation
-> bounded typed receipt
-> helper exits
```

Mount/data state may remain resident after the helper exits. Durable semantic authority remains on the Mac.

The repository already has reviewed command-style precedents for fixed Lima shell + non-interactive sudo + scrubbed environment in the disposable runner/runtime work. Reuse that discipline.

## Next implementation sequence

Keep this sequence unless new evidence invalidates a prerequisite:

1. **Land the minimal #585 ownership observer.** Descriptor-bound exact marker/root/objects ownership generation; no recursive O(N) audit in P1.
2. **Finish #591** so task lifecycle vocabulary no longer implies a shared linked-worktree implementation.
3. **Finish #589 executor composition** behind an unmintable production correlation proof; close research #598 and rebuild one clean exact-head PR.
4. **Re-run/supersede #597** to settle workdir mutation timing and the cross-owner Git alternates question.
5. **Implement #592 publication** only after #585's read-only acceptance contract is stable.
6. **Obtain the operator-Mac #565 P2 Lima disk receipt** and implement descriptor-bound physical project-disk observation.
7. **Mint the first real correlation proof** only from #565 P2 accepted evidence.
8. **Compose #588 one-shot guest prepare transaction**: #582 confirm -> #587/#594 descriptors -> #565 correlation -> #589 mount -> #580 Git/index proof -> bounded receipt -> Mac publishes task ready.
9. Run the first real resident trusted agent edit/test loop and compare it with the #563 baseline receipts.

## Do not regress these rules

- Hostile/unknown work still uses strict disposable workers.
- Names, PIDs, paths, disk names, mount presence, cache presence, Git refs/config, and Lima lock files carry zero independent ownership authority.
- A surviving hot state has zero independent workflow/source/result/merge/cleanup authority.
- Shared writable state requires explicit ownership/poisoning/quota/publication policy.
- `unsafe_code = "forbid"` remains intact; do not bypass it for mount-ID convenience.
- Never format/unlock/delete a project disk from name-only evidence.
- One mutable ext4/XFS project filesystem has one writable sandbox attachment owner at a time.
- Private task upper/work state may be runner-owned; immutable lower/pool/control state remains outside task ownership.
- Admin/root Git commands use a scrubbed fixed environment and do not execute project-controlled Git preparation as root.
- Active immutable Git pool generations are never mutated/repacked/GC'd in place.
- Performance observations do not grant execution authority.
- Benchmark full agent loops and preserve cold/warm distinction.
- When evidence is ambiguous: revalidate, reset/quarantine, or reconstruct. Do not name-adopt.

## Definition of the next major milestone

The next meaningful product milestone is not another micro-benchmark. It is:

```text
accepted resident project/sandbox/disk generation
-> exact immutable source/Git generation
-> task-private Git identity
-> exact all-FD OverlayFS task view
-> final non-mutating Git/source proof
-> agent command
-> edit/test loop
-> retain valid hot state
-> exact cleanup or residency transition
```

with an ordinary cold/private fallback and the same durable authority/recovery discipline.
