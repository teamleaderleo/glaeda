# M6 hot-runtime handoff

Status snapshot: 2026-08-25

This is the durable continuation point for SmolRunner's resident trusted-agent runtime work. Read it together with `BLAZINGLY_HOT.md`, current `main`, and the linked issues. Current accepted code and later reviewed issue/PR decisions win over this snapshot.

> **Disposable is a capability. Trust decides residency.**

## Current answer

The leading trusted-agent path is a path-class-specific resident Linux design:

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

Preferred mechanisms:

- source and mostly-read project/dependency state: immutable resident lower state plus task-local OverlayFS upper/work;
- Git task identity: task-private Git metadata backed by one exact immutable object-generation alternate;
- write-heavy compiler/build output: task-private CoW/reflink lineage where supported;
- fallback: ordinary private Git/materialized state whenever hot-path identity or authority is unavailable;
- hostile or unknown work: the strict fresh disposable worker lane remains separate.

Hot state is acceleration. Durable execution state and fresh exact observation remain authoritative.

## Current `main`

At this snapshot, `main` is `47c4586f2a68acd1e101d0fa80bdc5b7996fc632`, merge of PR #626.

Important M6 capabilities already landed:

| Capability | Issue / PR | Current role |
| --- | --- | --- |
| bounded hot-execution performance receipts | #563 / #564 | comparable latency/resource/storage observations |
| single-writer project-disk lease core | #565 / #567 | logical disk/attachment generations; physical Lima correlation still pending |
| Git index-v2 stat-cache patcher | #568 / #569 | pure verified index-byte handling |
| source-anchor/task-view lifecycle | #570 / #571 | exact leases and ready/run/cleanup ordering |
| trusted OverlayFS mount plan | #572 / #575 | exact lower/upper/work/merged identities and fixed mount policy |
| hot-state path policy | #573 / #579 | overlay/private-CoW/private-empty/reviewed-share selection |
| immutable Git object-pool lease core | #581 / #583 | pool generation identity, consumer leases, draining and retirement |
| mount-plan reconfirmation | #576 / #582 | fresh authority/evidence before privileged mount work |
| exact OverlayFS role descriptor lease | #584 / #587 | retained lower/upper/work/merged descriptors |
| immutable Git-pool marker | #586 / #590 | strict 64-byte binding digest + nonzero generation nonce |
| merged-parent descriptor + basename | #593 / #594 | descriptor-relative visible-target reopen after attach |
| generic Git worktree observation vocabulary | #591 / #601 | `OverlayGitWorktreeObservation` primary, compatibility alias retained |
| sealed all-FD OverlayFS transaction | #589 / #607 | safe-rustix FD-only mount execution; production correlation proof remains sealed |
| immutable Git-pool ownership observer | #585 / #609 | root-owned frozen generation, exact marker, retained `objects/`, race-safe nested-alternates absence |
| one-shot guest-control protocol envelope | #588 / #612 | exact attachment/sandbox/binary binding, closed operations/debt, canonical digests; pure protocol |
| sealed one-shot Lima invocation plan | #588 / #615 + #623 | fixed exact instance/root guest argv; host `shell <instance> -- ...` and guest `sudo -- ...` boundaries; execution adapter pending |
| immutable-pool publication guest operation | #592 / #626 | distinct closed protocol tag for pool publication; no handler or mutation authority |

#607 contains physical mount mechanics, while normal product code still cannot mint the required project-filesystem correlation proof. Physical activation waits for accepted #565 P2 evidence.

## Performance and kernel evidence

### Ordinary Git worktree metadata is already cheap

Owned hosted-ARM measurements put `git worktree add --no-checkout` around 1.8–2 ms on a representative SmolRunner checkout. Full tree materialization was roughly 26–31 ms. The main source-view cost is writing the tree, not worktree registration metadata.

### OverlayFS is the leading multi-agent source view

A representative hosted-ARM/XFS fan-out using an exact resident source, task Git metadata, OverlayFS lower sharing, and inherited clean source index measured approximately:

| Tasks | Ordinary Git materialization | OverlayFS + inherited source index | Task-view growth |
| ---: | ---: | ---: | ---: |
| 1 | 76.2 ms | 15.9 ms | ~8 MiB -> ~4 KiB |
| 8 | 224.3 ms | 52.4 ms | ~64 MiB -> ~32 KiB |
| 32 | 695.6 ms | 175.2 ms | ~257 MiB -> ~160 KiB |

All 41 views passed non-mutating Git proof. The same source-view model also worked on ext4, removing XFS as a prerequisite for resident source fan-out.

### Private CoW remains better for write-heavy build output

OverlayFS lower sharing performed poorly when warmed compiler output lived under the lower tree and Cargo copied up write-heavy `target/` data. Giving each task a private XFS reflink lineage for build output cut one eight-task synthetic run from roughly 5.70 s to 2.77 s.

### OverlayFS mutation boundary is settled

Owned #597 timing evidence showed:

```text
initial                 workdir entries = 0
after lower SET_FD      workdir entries = 0
after upper SET_FD      workdir entries = 0
after work SET_FD       workdir entries = 0
after FSCONFIG_CREATE   workdir entries = 1
after fsmount           workdir entries = 1
after move_mount        workdir entries = 1
```

The final safe empty-workdir second confirmation belongs after all `fsconfig_set_fd` calls and immediately before `FSCONFIG_CMD_CREATE`.

Pinned `rustix 1.1.4` exposes the required safe APIs (`fsopen`, `fsconfig_set_fd`, `fsconfig_create`, `fsmount`, `move_mount`). Project unsafe code remains forbidden. Ordinary mount ID is transaction-local observation only.

## Immutable Git object-pool findings

### Private task Git metadata is preferred

Linked worktrees remain an excellent fallback/comparator because registration is extremely cheap. Multiple linked worktrees still share common repository metadata. Task-private Git directories isolate refs, config, index, and new objects with modest overhead, so private metadata is the preferred hot-path direction when object sharing is proven.

### Active pool ownership belongs outside the task UID

A task-UID-owned seed with mode `0555` is weak against a hostile owner because the owner can chmod it writable. Active immutable generations are owned outside `smolrunner-runner`; the current guest design uses root-owned frozen generations.

The useful account split is:

```text
root              control / publication / mount authority
smolrunner-admin  trusted non-task Git producer/preparer
smolrunner-runner agent/workflow execution
```

### Producer generations require inode independence

Local Git clone can hardlink producer objects. Changing ownership of a newly cloned seed can therefore affect the producer repository through shared inodes.

V1 publication requires distinct regular-file inodes before root takes ownership. The initial producer command is deliberately boring:

```text
git clone --bare --local --no-hardlinks <exact immutable source> <exact staging candidate>
```

A later reflink-aware producer is acceptable only after distinct inode/ownership semantics are proven.

### Admin Git gets a completely scrubbed environment

An earlier `sudo -u <admin> -H` probe still attempted to read the runner user's Git attributes configuration. `-H` alone is insufficient.

Admin Git production uses the semantic equivalent of:

```text
env -i
HOME=<fixed reviewed value>
PATH=/usr/bin:/bin
GIT_CONFIG_NOSYSTEM=1
GIT_CONFIG_GLOBAL=/dev/null
XDG_CONFIG_HOME=<exact empty/reviewed config root>
```

plus fixed absolute Git argv, bounded input/output/deadline, disabled credentials/network where appropriate, and no project-sensitive Git preparation as root.

### #590 marker is generation identity

The fixed marker binds the exact #583 pool identity and a nonzero generation nonce. Git refs, HEAD, config, and objects provide content evidence. They never substitute for ownership/generation identity.

### Active generations stay frozen

Fetch/repack/GC/thaw never modify an active pool in place. Build and promote a successor generation, then drain old #583 consumers before retirement.

## Cross-owner private Git alternates are proven

The first security-correct private-clone experiment proved runner checkout/add/commit/GC and seed immutability, but the expected alternates file was empty. That result was recorded as incomplete evidence.

A dedicated non-shallow owned ARM run `32748702131` / job `97500255676` settled the question. A root-owned frozen bare seed was produced with `git clone --bare --local --no-hardlinks`; admin and runner commands used scrubbed environments.

Both forms created the exact seed `objects` alternate:

```text
git clone --shared --no-checkout ...
git clone --reference <seed> --no-local --no-checkout ...
```

Both also worked with `--separate-git-dir`. After handoff to the runner account, checkout, edit, add, commit, `git gc --prune=now`, and object lookup succeeded. The alternate survived GC and the root seed digest/ownership/frozen modes stayed unchanged.

V1 task admission should prefer:

```text
git clone
  --reference <exact-accepted-generation>
  --no-local
  --no-checkout
  --separate-git-dir <exact-private-task-gitdir>
  --template <reviewed-empty-template>
  <exact-accepted-generation>
  <exact-task-target>
```

The explicit reference expresses the shared-object dependency in argv and disables local hardlink optimization. `--shared` remains a measured comparator.

## #585 hot-path observer is complete

PR #609 completed the intentionally narrow #585 contract. It keeps per-task admission O(1)-ish with respect to object count:

- exact protected generation parent + basename;
- retained generation root and `objects/` descriptors;
- root-owned frozen ownership/mode checks;
- root/`objects` same-filesystem relation;
- exact #590 marker verification;
- race-safe `objects/info/alternates` absence;
- descriptor/name rebinding and final revalidation;
- no Git command, recursive object walk, byte accounting, publication, or mutation.

The retained `objects/` handle is intentionally `O_PATH` identity authority.

## #592 publication audit status

The first recursive candidate audit, PR #608, had useful walker mechanics but an unacceptable authority seam: caller-selected paths could mint a positive publication receipt, and the result lacked exact #583/#585 source identity plus staging-transaction identity. #608 was closed as diagnostic evidence.

PR #614 rebuilds that audit around retained authority. Exact head `3b2996d9292f25454ed8dbceef797bcb850677a9` currently has green repository Verify (`32759201142`) and Linux acceptance (`32759201261`) and is ready for implementation-independent acceptance.

#614's accepted design target:

- source comes from the retained #609 observation, never a caller-selected source path;
- #609's `O_PATH` `objects/` handle is upgraded only through descriptor-relative `openat(fd, ".")`, followed by exact identity equality;
- candidate comes from a sealed retained staging descriptor lease with no public constructor;
- candidate lease binds exact candidate #583 identity plus staging-transaction identity;
- receipt binds source/candidate #590 binding digests, staging transaction identity, and opaque physical input digests;
- source and candidate are reconfirmed after the O(N) walk;
- candidate regular files must have one link;
- source/candidate object `(st_dev, st_ino)` sets must be disjoint;
- symlinks/special entries, nested alternates, count/depth overflow, and drift fail closed;
- repeated audit on the same retained leases must yield the same receipt;
- logical bytes are reported; `st_blocks` is never presented as unique physical allocation.

#614 remains publication evidence only. It grants no root mutation, Git execution, ownership transfer, marker write, rename, or cleanup authority.

## #592 next transaction

After #614 receives independent acceptance, continue the privileged guest-local transaction in this order:

```text
root: exact private staging envelope + retained parent authority
  -> delegate only exact producer candidate to smolrunner-admin
smolrunner-admin: scrubbed absolute-Git clone --bare --local --no-hardlinks
  -> exit
root: prove producer absence + exact candidate descriptor
  -> #614 candidate audit
  -> reviewed Git/object content/reachability proof
  -> recursive ownership/mode normalization
  -> root:root, dirs 0555, regular files 0444
  -> write exact #590 marker from the same transaction nonce
  -> #609 observe frozen staged generation
  -> no-replace descriptor-relative promotion
  -> fsync publication parents
  -> #609 observe final published generation
```

Crash recovery uses the exact staging transaction identity and retained parent evidence. Directory names alone never authorize adoption or deletion.

## #580 task-private Git lane

#580 owns task Git metadata, private `.git` identity, inherited reviewed source index, final Git/source proof, and ready composition around the landed #607 mount transaction.

V1 requirements include:

- exact #571 anchor/task leases;
- exact #583 pool consumer lease;
- fresh #609 pool confirmation immediately before private Git preparation;
- `--reference <accepted-generation> --no-local` private clone under the trusted admin account;
- exact post-clone alternate = accepted pool `objects`;
- bounded reviewed local config and reviewed empty template;
- remove clone-created local `origin` before ready publication;
- atomic exact task `.git` pointer publication into the OverlayFS upper;
- exact inherited clean source-index digest and task-private index destination;
- no successful-path checkout/reset that rewrites the shared source view;
- final non-mutating Git/source proof with hooks/fsmonitor/credentials/network disabled;
- cleanup only after workflow process absence and exact task Git/mount evidence.

## #588 guest-control status

PR #612 landed the pure canonical one-shot request/receipt envelope. It binds exact project-disk revision/generation, attachment generation, resident sandbox generation, guest-control binary generation/digest, one closed operation tag, and canonical payload/request digests.

#615 landed the pure sealed Mac invocation plan. #623 then hardened the host CLI boundary so the exact plan is `limactl --tty=false shell <instance> -- <guest-command...>` while retaining the guest `sudo -- /usr/bin/env -i ...` separator and scrubbed environment.

#626 landed a distinct closed `PublishImmutableGitPoolGeneration` protocol operation. Pool publication therefore has its own canonical one-shot tag instead of being hidden under `PrepareTrustedTaskView`. The execution adapter is still pending and should consume the bounded plain-stdin process mode only after #619 receives its required independent concurrency acceptance.

## #565 physical blocker

The main production physical blocker remains #565 P2: exact operator-Mac evidence tying the standalone Lima disk generation/attachment to the guest filesystem identity. #618 is the current observation-only receipt lane on hardened head `a0bdf00fdcf98e7fd3484d8d8630904a1db37f64`; exact Verify `32760003780` is green. The collector now opens the supplied disk-directory path component-by-component with no-follow semantics and requires decoded Linux guest `st_dev` major/minor to equal the single exact project `mountinfo` row. It still leaves physical ownership unresolved and adds no writable #589 proof constructor. Because the final repair is ownership-adjacent, #618 remains gated on implementation-independent exact-head acceptance.

#607 stays sealed until accepted #565 P2 evidence can mint the project-filesystem correlation proof.

## Failed experiments and corrected assumptions

Keep these visible so the lane does not repeat them.

### Reflink worktree v1 rewrote the CoW win

Pre-populated reflinked files were followed by `git reset --hard`, which rewrote most of the tree. Git correctness survived; the physical sharing benefit did not.

**Correction:** successful optimized population must avoid a forced checkout/reset that rewrites already-proven files.

### Reflink worktree v2 started with an empty index

`git worktree add --no-checkout` leaves the target index empty. `update-index --refresh` cannot make an empty index represent the target tree.

**Correction:** populate the exact target index first (`read-tree` or equivalent reviewed path) or inherit the exact reviewed clean source index when the commit/tree relation permits it.

### Python reflink fan-out overstated scaling cost

Hundreds of Python-launched clone operations polluted the 32-task result with interpreter/process/thread overhead.

**Correction:** judge kernel primitives with native/bulk implementations.

### `git update-index --refresh` moved whole-tree work

Refresh shifted the cost earlier instead of eliminating it.

**Correction:** direct index stat publication and inherited clean source index are the preferred hot paths.

### Early XFS allocation deltas were noisy

Delayed allocation/free caused noisy and occasionally negative filesystem-used deltas.

**Correction:** synchronize allocation samples and avoid treating summed per-file `st_blocks` as unique physical space on reflink-capable filesystems.

### Early pool read-only result overstated immutability

The seed was task-UID-owned with write bits removed.

**Correction:** active generation ownership belongs outside the task UID; current V1 uses root-owned frozen generations.

### Early local pool clone risked hardlink aliasing

Local clone could share producer object inodes.

**Correction:** `--no-hardlinks` plus explicit inode-independence proof before ownership transfer.

### `sudo -u -H` leaked Git environment state

The admin probe still attempted the runner user's attributes config.

**Correction:** fully cleared environment with fixed HOME/PATH/config roots and fixed absolute Git argv.

### First private clone did not prove object sharing

Task Git operations worked and the root seed stayed unchanged, but the expected alternates file was empty.

**Correction:** the later non-shallow scrubbed run `32748702131` established exact cross-owner alternates for both `--shared` and `--reference --no-local`.

### #597 initially failed in its C harness

The first workdir-timing C program ignored `system()`'s return under `-Werror`, so later steps were skipped.

**Correction:** fix the harness, rerun, and cite only the successful evidence. The corrected run established CREATE as the first workdir mutation.

### A combined #597 Git fixture used a shallow source

`--reference` correctly refused the shallow reference repository.

**Correction:** the dedicated non-shallow follow-up settled the behavior.

### Push-only research branches sometimes produced ambiguous absence

Several result branches failed to carry evidence cleanly or queued long enough that branch absence said little.

**Correction:** use owned draft PRs/normal Actions for critical physical evidence, and treat missing result branches as missing evidence.

## Current active lanes

- #580 — task-private Git metadata/index/Git-source proof and ready composition;
- #581 — immutable pool generation lifecycle and private task Git direction;
- #588 — one-shot guest-control execution path; #612/#615/#623/#626 landed; the process adapter waits on #619 independent concurrency acceptance;
- #592 — root/admin immutable-pool publication; #614 candidate audit still waits on implementation-independent exact-head acceptance;
- #565 P2 — operator-Mac project-disk physical evidence; #618 hardened head is Verify-green and waits on implementation-independent exact-head acceptance;
- #619 / #617 — bounded non-secret stdin process primitive is Verify/Linux-green on `9907b0a5df525170606937a653e23e02826f3cf2` and waits on implementation-independent concurrency acceptance;
- #566 — owned performance/research receipts.

Completed adjacent slices: #585/#609, #589/#607 mechanics, #591/#601, #593/#594.

## Next implementation sequence

1. Accept #614 independently on its exact head; keep the O(N) walk outside hot task admission.
2. Build #592's root-owned staging envelope and narrow admin-producer execution boundary around fixed absolute Git + cleared environment.
3. Compose producer absence, #614 audit, content/reachability proof, root freeze/marker, no-replace promotion, and final #609 observation.
4. Accept #619 independently on its exact concurrency/process-lifecycle head, then build the one-shot execution adapter from the merged #612/#626 protocol plus #615/#623 invocation plan instead of reconstructing Lima argv.
5. Implement #580 task-private Git creation using `--reference <accepted-generation> --no-local`, exact alternate validation, origin removal, exact index publication, and final non-mutating Git/source proof.
6. Finish #565 P2 real operator-Mac evidence through #618 and mint #607's first production correlation proof only from accepted evidence.
7. Compose the one-shot resident task transaction: current authority -> descriptors -> correlation -> #607 mount -> #580 Git/index proof -> bounded receipt -> Mac publishes ready.
8. Run a real resident trusted-agent edit/test loop and compare complete-loop receipts against the #563 baseline.

## Rules to preserve

- Hostile or unknown work uses fresh disposable workers.
- Names, PIDs, paths, disk names, mount presence, Git refs/config, cache presence, and surviving hot bytes carry zero independent ownership authority.
- One mutable project filesystem has one writable sandbox attachment owner at a time.
- Private task upper/work state may be runner-owned; immutable lower/pool/control state stays outside task ownership.
- Project-sensitive Git preparation runs as the trusted non-task admin account, never as root or the workflow UID.
- Admin Git receives a cleared, fixed environment.
- Active immutable Git pool generations are never fetched/repacked/GC'd/thawed in place.
- #590 marker is generation identity; Git data is content evidence.
- Destructive recovery requires exact retained/durable authority; same-name survival never authorizes adoption.
- `unsafe_code = "forbid"` remains intact.
- Performance observations grant no execution or cleanup authority.
- Benchmark complete cold/warm agent loops, not only micro-operations.

## Next major milestone

```text
accepted resident project/sandbox/disk generation
-> independently accepted immutable pool publication
-> exact private task Git identity + immutable alternate
-> exact all-FD OverlayFS task view
-> final non-mutating Git/source proof
-> agent edit/test command
-> retain valid hot state
-> exact cleanup or residency transition
```

with an ordinary cold/private fallback and the same durable authority/recovery discipline.
