# Developer-loop benchmark

This benchmark measures one useful Rust edit-to-verification loop across execution arms. It exists to prevent fresh-clone speedups, cache hits, and semantically different test scopes from being compared as though they were the same result.

## Workload contract

The frozen local evidence used Rust source commit `b9fa23462420c13a465d635d9694f0c827c1e685`, tree `edd0b7bb9d3e59305c21c69b721b5278d8aff6da`, pinned Rust/Cargo 1.97.1, and default Cargo concurrency on a 16-logical-CPU, 30-GiB big-red host.

The edit fixture adds one documentation line to `src/lib.rs` without changing behavior. Its tracked-workload diff digest is `sha256:bfdd60e73e8b106c0129d1052310495ae2dbe1ff70bb52b35a9f9ef4911927eb`.

`scripts/benchmark-developer-loop` runs the same locked lib/bin test command in every arm. The profile executes 1,343 tests and leaves one existing ignored test ignored. It excludes exactly 16 host-fact tests that currently observe different `/usr/bin/env` ownership and parent-directory safety inside `hot-run`'s cross-worktree user/mount namespace. The script names every exclusion and emits a path-free JSON receipt even when the workload fails.

This profile is a resident-eligible developer-loop result, not a replacement for full host verification. The full suite remains a required distinct verifier.

## First big-red control matrix

All three-sample rows report every sample and the median. Three samples are not enough to claim p90.

| Arm | Setup state | Wall seconds | Median | Peak RSS KiB | Result |
| --- | --- | ---: | ---: | ---: | --- |
| Fresh local | no target tree; shared Cargo download cache | 43.45, 44.16, 43.67 | 43.67 | 2,515,488–2,525,164 | green |
| Ordinary native prime | no target tree; unedited base | 43.39, 42.03, 43.26 | 43.26 | 2,491,872–2,518,568 | green |
| Ordinary native edit | private target primed on unedited base | 10.14, 10.62, 10.31 | 10.31 | 1,638,324–1,639,292 | green |
| Glaeda resident edit | shared resident lower target plus fresh private overlay state | 10.92, 11.43, 11.06 | 11.06 | 1,631,712–1,632,296 | green |

The current Glaeda resident path is 3.95 times faster than fresh local at the median, but ordinary Cargo incremental is 4.24 times faster than fresh local and beats Glaeda by 0.75 seconds, or 7.3%. For this edit shape, current stable-path isolation adds semantics and private writable cache state but does not yet improve latency over a normal warm worktree.

## Big-red path-class follow-up

### Superseded storage attribution

The first private-lineage follow-up used an explicit state root below `/tmp`. On big-red `/tmp` is
`tmpfs`, not the host's ext4 filesystem. Its `43.98, 41.98, 42.43`-second primes and
`9.87, 10.50, 10.78`-second edits are valid tmpfs observations, but they do not support an ext4
mechanism claim. The original state was removed in 0.49 seconds after recording its receipts. The
storage attribution and any ext4 conclusion derived from those samples are superseded by the
same-filesystem rerun below.

### Physical ext4 rerun

The correction kept the same frozen commit, tree, fixture, toolchain, command, 16-logical-CPU host,
and default Cargo concurrency. The resident `target`, every Overlay upper/work directory, and every
private lineage were freshly proven on the same ext4 filesystem. Three independent empty-private
lineages each ran the complete unedited workload once, received the exact fixture, and ran the
complete workload once more. Three separate edited tasks compared a warmed resident Overlay lower
with a full ordinary-copy private seed from that exact lower.

| Phase | Wall seconds | Median | User CPU seconds | System CPU seconds | Peak RSS KiB | Result |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Empty-private cold prime | 41.76, 42.53, 43.67 | 42.53 | 104.76, 109.09, 109.05 | 9.43, 9.81, 9.78 | 2,512,408, 2,503,152, 2,502,760 | green |
| Empty-private edit | 10.65, 10.38, 10.37 | 10.38 | 17.41, 16.88, 16.73 | 3.88, 3.91, 4.14 | 1,634,556, 1,635,252, 1,630,944 | green |
| Warmed Overlay edit | 11.85, 11.94, 12.68 | 11.94 | 16.30, 16.51, 17.87 | 3.90, 4.32, 4.40 | 1,638,348, 1,638,040, 1,637,116 | green |
| Ordinary-copy private seed | 0.43, 0.47, 0.59 | 0.47 | 0.00, 0.00, 0.00 | 0.42, 0.47, 0.58 | 2,380, 2,492, 2,512 | green |
| Copy-seeded private edit | 10.04, 10.79, 10.32 | 10.32 | 16.69, 17.37, 17.58 | 3.79, 3.69, 3.71 | 1,622,412, 1,633,428, 1,632,676 | green |

Every edit receipt carried the exact fixture digest and every run completed 1,343 executed tests
with one existing ignored test. Outer `hot-run` medians were 10.470375 seconds for empty-private,
12.074703 seconds for Overlay, and 10.450082 seconds for the already-seeded private command. Pairing
each copy with its edit produced inner request-to-result samples of 10.47, 11.26, and 10.91 seconds
(median 10.91); the corresponding outer totals were 10.603277, 11.357405, and 11.040082 seconds
(median 11.040082).

The same-filesystem `cp --reflink=always` control refused in 0.01 seconds with `Operation not
supported`. The ordinary copy therefore allocated the full exact parent: 1,960,813,503 logical
bytes and 1,963,872,256–1,963,876,352 allocated bytes. After the edit, the copy-seeded private
states occupied 2,716,748,969–2,716,749,002 logical and 2,720,026,624–2,720,038,912 allocated bytes.
The empty-private final allocation was 2,720,018,432–2,720,022,528 bytes. Overlay state occupied
2,537,993,803–2,537,993,825 logical and 2,539,700,224–2,539,720,704 allocated bytes, so it saved
only about 172 MiB versus a complete private lineage for this real workload.

After recording the receipts, the three task worktrees removed in 0.04, 0.00, and 0.00 seconds;
the warmed resident worktree and its target removed in 0.12 seconds. The three copy-seeded,
Overlay, and empty-private state families removed in 0.22, 0.11, and 0.33 seconds respectively;
receipt cleanup was below 0.01 seconds. No benchmark process or mount survived.

On this ext4 host, an empty private lineage pays the full cold compile once. Copy-seeding avoids
that prime: its 10.91-second preparation-plus-result median was 1.03 seconds, or 8.6%, faster than
the 11.94-second Overlay result, while remaining 0.60 seconds behind the earlier 10.31-second
ordinary-native median. That makes ordinary-copy seeding a measured retained-lineage candidate,
not a universal default: it consumes the full parent allocation, depends on an exact warm parent,
and still needs concurrent fan-out and cold-page-cache controls. Reflink-capable storage remains
the preferred cheap-seeding candidate. Source fan-out, Git metadata, dependencies, compiler output,
and resident services remain separate mechanism decisions.

### Executable `private-copy` candidate

The first implementation dogfood repeated the same exact edited workload through the actual
`target:private-copy` mode, with three new task/state lineages physically on ext4. The mode copied
the exact warm parent into a hidden candidate, atomically published the private directory, mounted
it at the stable resident pathname, ran the validator, and emitted one preparation-plus-command
receipt:

| Measurement | Samples (s) | Median (s) |
| --- | ---: | ---: |
| Atomic private-copy preparation | 0.434903, 0.407321, 0.418423 | 0.418423 |
| `hot-run` command | 9.520338, 10.214857, 10.198427 | 10.198427 |
| Receipt copy + command | 9.955241, 10.622178, 10.616850 | 10.616850 |
| Inner workload | 9.42, 10.11, 10.10 | 10.10 |

All three receipts reported `seeded`; all validators were green, with 1,630,948–1,632,464 KiB
peak RSS for the workload. Final lineages each allocated 2,720,026,624 bytes. One immediate reuse
reported `reused`, zero preparation time, 2.732046 seconds command time, and 2.79 seconds outside
the whole process; its inner workload was 2.63 seconds. The receipt deliberately keeps copy CPU/RSS
outside command resource accounting while exposing copy wall time and command-plus-copy wall time.
The three task worktrees removed below 0.01 seconds each, the warmed resident worktree removed in
0.12 seconds, all three private-copy states removed in 0.45 seconds, and receipt cleanup was below
0.01 seconds. No process or mount survived.

The executable candidate's 10.616850-second measured median is 1.323 seconds, or 11.1%, faster than
the corrected 11.94-second ext4 Overlay median and 0.307 seconds, or 3.0%, slower than the earlier
10.31-second ordinary-native median. This validates the mechanism and its observation surface; it
does not yet settle concurrent fan-out, cold-page-cache copy behavior, or automatic selection.

## Correctness failure retained as evidence

Running the full lib/bin command through the same Glaeda resident path took 11.50 seconds, used 1,631,808 KiB peak RSS, and exited 101 after 1,326 passed, 16 failed, and one ignored library test. Fifteen failures could not select a reviewed environment executable after namespace ownership remapping; one rootless-Podman observation saw an unsafe parent directory instead of a missing source.

That sample is not a speed result. It establishes a routing/compatibility requirement: Glaeda must either preserve those host facts, route those tests to an equivalent host verifier, or refuse to claim full verification.

## Hosted controls

`.github/workflows/developer-loop-benchmark.yml` measures two GitHub-hosted Ubuntu 24.04 controls from the same edit fixture:

1. a fresh runner applies the edit before any build;
2. a cache-prepared runner restores an exact immutable target generation compiled from the unedited base, applies the edit, and runs the same workload.

The base cache key binds runner OS/architecture, Rust 1.97.1, and the exact Actions source SHA. On a miss, the prepared job materializes and saves the unedited base before applying the fixture; later unchanged runs restore that immutable generation directly. Receipts are uploaded for both workload results. Queue, checkout, toolchain installation, cache transfer, workload, and end-to-end job times remain separately observable in the Actions run.

The first hosted distributions should be appended here after repeated unchanged runs. Node 22/24 is not a benchmark dimension for this Rust workload; earlier Node 22 evidence only discriminated one Scrapbook environment incident.

## Next decisions

- Close or explain the remaining 0.307-second seeded-private versus ordinary-native gap before
  claiming a warm Rust-loop win.
- Measure a leaf-function behavior edit and a public-type edit; the documentation edit mostly measures crate/test relink and namespace overhead.
- Add Python and current-project Node fixtures with repository-owned validators instead of pinning the product direction to legacy Node versions.
- Repeat each promoted arm enough times to publish p50 and p90, then add 1/4/8 concurrent-task fan-out with physical disk growth and cleanup latency.
- Keep semantic test selection and exact reusable verification receipts separate from filesystem/cache residency; they can compose, but neither authorizes the other.

## Controlled big-red fan-out harness

`scripts/benchmark-hot-state-fanout` runs one fixed physical window for the frozen Rust workload.
It has four closed arms—`ordinary-native`, `private`, `overlay`, and `private-copy`—and accepts only
fan-out widths 1, 4, or 8. It assigns disjoint Linux CPU-affinity sets from the harness's current
affinity, configures Cargo jobs to match each set, primes the state required by the selected
treatment, applies the exact checked-in edit to every task, starts every validator in one bounded
window, and always attempts to remove its exact worktrees and state. This is CPU enforcement, not a
memory reservation; per-task memory remains host-default and is reported from the child receipts.
The harness cannot accept an arbitrary command, repository, commit, fixture, cache path, or
validator.

After an unprivileged OverlayFS mount is gone, the kernel may leave its internal work directory at
mode `000`. Cleanup restores owner traversal only on owned, non-symlink directories inside the
exact disposable experiment tree, then removes that tree. It does not follow links or alter an
external target. Any ownership mismatch or remaining removal error still retains the tree as
`recovery_required` evidence.

Inspect the exact plan without mutation:

```bash
scripts/benchmark-hot-state-fanout --plan --arm private-copy --fanout 4
```

Run one physical ext4 window with the aggregate receipt outside the disposable scratch root:

```bash
scripts/benchmark-hot-state-fanout \
  --arm private-copy \
  --fanout 4 \
  --scratch-root /path/to/owned/ext4-scratch \
  --output /path/to/receipt.json
```

Every arm now executes through a schema-v4 `hot-run` measurement. The ordinary-native control uses
the task worktree directly with an explicit `target:native` observation; it receives no mount,
copy, state, or isolation treatment. Prime and edited work get different caller-owned comparison
keys. Each key deterministically binds the frozen source/tree/diff, the three producer-program
content digests, exact Rust and Cargo versions and executable digests, arm, fan-out, Cargo
concurrency, CPU-affinity sets, memory treatment, offline/incremental settings, page-cache
declaration, and creation umask. This lets repeated same-treatment receipts feed
`hot-pressure-shadow` while mixed work or treatment refuses.

The aggregate observation binds the harness commit/tree, frozen source/tree/edit, arm, fan-out,
exact CPU-affinity sets, per-task Cargo concurrency, setup and complete-window latency, every
semantic and `hot-run` receipt, mount/device/filesystem identity, backing-filesystem free bytes,
per-tree logical bytes and summed allocated file blocks before/after the window, and cleanup
disposition. Summed `st_blocks` are not unique physical usage on reflink-capable filesystems; the
filesystem-level observation keeps that distinction visible. A failed task cancels the remaining
process groups. The child environment is a closed allowlist: caller target overrides, compiler
wrappers/flags, and toolchain overrides are excluded; the accepted Cargo home is offline and held
constant. Setup and byte-observation time remain outside the primary request-to-all-results window
and are reported separately. The default page-cache state remains uncontrolled/resident. For
`overlay` and `private-copy`, `--page-cache-treatment resident-target-dontneed` adds the bounded
cold-read discriminator: after the resident prime and byte observation, it fsyncs every exact owned
regular file in resident `target` and issues `POSIX_FADV_DONTNEED` immediately before the edit
window. The receipt records the file/byte scope and elapsed setup time, while naming this as advice
rather than claiming a globally cold cache; it never writes `/proc/sys/vm/drop_caches` or touches
unrelated trees. The harness grants no policy, cache, result-reuse, cleanup, or shared-mutation
authority.

`--retained-reuse` adds one immediate second validation window without recreating worktrees,
reapplying the edit, or replacing task state. The first edit and retained-reuse windows have
different comparison keys: the former binds `initial_for_source_state`, while the latter binds
`retained_after_accepted_edit`. This prevents a pressure comparator from treating first-use and
already-retained work as the same treatment. For `private-copy`, the first window must prove a
positive-time `seeded` preparation and the second must prove zero-time `reused`; either mismatch
rejects the observation. The second window still executes and validates the complete frozen
1,343-test workload. It is therefore a command-reuse measurement, not result reuse.

### First exact-head matched fan-out control

The first physical use of the harness ran `A-B-B-A` at fan-out 1 on big-red, where `A` was
`ordinary-native` and `B` was `private-copy`. All four observations bound exact clean harness commit
`e911d69947ba62661f085b51235ef304f8d1d250`, ext4 mount ID 44, device 259:2, the frozen workload
above, Rust/Cargo 1.97.1, and one 16-CPU affinity set. The harness held the child environment and
Cargo concurrency constant. Page cache remained resident/uncontrolled and memory remained
host-default unbounded as declared by the experiment. No competing build or test process was seen
at the initial observation; initial host load averages were 0.71, 1.03, and 1.30.

| Order | Arm | Complete edit window (s) | Inner workload (s) | Private-copy preparation (s) | Command after preparation (s) | Peak RSS KiB | Result |
| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| A1 | ordinary-native | 13.841862 | 13.74 | n/a | n/a | 1,637,572 | green |
| B1 | private-copy | 12.933524 | 12.26 | 0.513091 | 12.362230 | 1,633,464 | green |
| B2 | private-copy | 17.180191 | 14.79 | 1.403091 | 15.245866 | 1,633,792 | green |
| A2 | ordinary-native | 13.992653 | 13.86 | n/a | n/a | 1,636,552 | green |

The two-sample complete-window medians were 13.917258 seconds for ordinary native and 15.056858
seconds for private-copy. Private-copy was therefore 1.139600 seconds, or 8.19%, slower in this
small matched control. Its inner-workload median was 13.525 seconds versus 13.800 seconds native,
but its median `hot-run` command time was 13.804048 seconds and median first-use preparation was
0.958091 seconds. One candidate window beat the native median by 7.07%; the other missed it by
23.45%. That spread is evidence against a default change from two samples, not evidence for picking
the favorable sample.

The native tasks grew from about 2.042 GB allocated after prime to about 3.247 GB after the edit.
Each private-copy task kept the roughly 2.042 GB resident parent and allocated another roughly
3.236 GB private state because same-filesystem ext4 reflinks are unavailable here. Summed file
blocks are not a unique-filesystem accounting claim, but the result confirms that ordinary-copy
seeding is neither a latency nor a space default on this host at fan-out 1. It remains a useful
fallback mechanism, while reflink-capable private lineage, OverlayFS source views, read-only
dependency state, private-empty state, and ordinary native worktrees remain distinct path-class
choices.

Every run reported semantic acceptance, 1,343 executed tests plus one existing ignored test, and
cleanup disposition `removed` with two of two owned worktrees removed. The scratch root was empty
after the sequence. Raw receipt SHA-256 digests, in run order, were:

- A1: `302569edb2166941fb6da2f0ebdc8b58a2614c440a9a8b87692a97835d15d53d`
- B1: `8ff52deaaa6303070fc7595f375bc2d7f69d5b824f4a382ad2d01c5dcdea9ad5`
- B2: `318840c763dca386da533a3e32bf74759fd9257cf7189c47f479e8b24f2170c7`
- A2: `291694d81fe49c7d2d5dac962724508567e093991990fed2239a6e4a8f1c78a7`

This result closes only the single-task warm-page-cache discriminator. Fan-out 4/8, cold-read
behavior, and composed Python/current-Node path policies remain separate experiments.

### Fan-out-4 warm and cold-read selection

The next bounded big-red sequence closed the fan-out-4 write-heavy `target` choice. Every window
used ext4 mount 44, four disjoint four-CPU sets, 16 total Cargo jobs, the same frozen edit and
1,343-test validator, and complete cleanup. Warm receipts used accepted head `384388e`; the
cold-read treatment itself used exact clean implementation commit `e6923237e0acbf24fc39a4e606a51a508f7c4d83`
and tree `aa8369e4711bc1c79f84d41f9c6a07c32f60f674` in A-B-B-A order. The producer-program change and
page-cache treatment intentionally give warm and cold receipts different comparison keys.

| Treatment | Complete-window samples (s) | Median (s) | Median peak temporary growth |
| --- | ---: | ---: | ---: |
| ordinary native, resident/warm | 26.474802 | 26.474802 | 10,934,538,240 bytes |
| private-copy, resident/warm | 27.928636, 28.288224 | 28.108430 | 12,890,456,064 bytes |
| Overlay, resident/warm | 30.295675, 29.632275 | 29.963975 | 12,178,305,024 bytes |
| private-copy, resident-target cold-advised | 28.340464, 27.281826 | 27.811145 | 13,028,327,424 bytes |
| Overlay, resident-target cold-advised | 32.096016, 30.938088 | 31.517052 | 12,179,146,752 bytes |

Each cold setup fsynced and advised away 1,119 exact resident-target files representing about
2.028 GB logical / 2.031 GB allocated. Advice took 2.750–2.769 seconds outside the primary window.
Private-copy first-use preparation rose from a 0.955072-second warm median to 1.681559 seconds
cold-advised, evidence that the treatment removed a meaningful warm-read advantage. Its complete
window remained within ordinary run variance, while Overlay's median grew 1.553077 seconds / 5.18%.

Under cold advice, private-copy reached all four useful results 3.705907 seconds / 11.76% sooner
than Overlay. Under warm pages it was 1.855545 seconds / 6.19% sooner. Overlay saved 849,180,672
bytes / 6.52% of private-copy's cold peak temporary growth, but that capacity difference does not
outweigh the latency result for the default write-heavy `target` path. Ordinary native remains the
fastest same-worktree observation when isolation is unnecessary; source and suitable dependency
paths remain separate Overlay/read-only decisions. The implicit cross-worktree `target` selector
therefore moves to `private-copy`, which also becomes cheap CoW seeding on reflink-capable XFS.

All four cold receipts succeeded, accepted all semantic validators, observed four simultaneous
tasks, removed five of five worktrees, and left the scratch root empty. Receipt SHA-256 digests in
run order:

- private-copy A1: `b37e2f1223750c5aa9e81a7dc3ecbbafbdaf9b40093dbe0277d1697f6b028cf0`
- Overlay B1: `9f8ed0484a2311c57d24a83c95783564ea8b7bf67683653c03e352d94e8da2b4`
- Overlay B2: `cbf8ee5adb63d42670b5fc78fa933fe778ac7891e23254be64c87a9f259c2db6`
- private-copy A2: `dcf52a4e16de43bbf3e8714eebcea703933eb6fa4252dd13d23df4a4c2d7de1c`
