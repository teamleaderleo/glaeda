# Developer-loop benchmark

This benchmark measures one useful Rust edit-to-verification loop across execution arms. It exists to prevent fresh-clone speedups, cache hits, and semantically different test scopes from being compared as though they were the same result.

## Workload contract

The frozen local evidence used Rust source commit `b9fa23462420c13a465d635d9694f0c827c1e685`, tree `edd0b7bb9d3e59305c21c69b721b5278d8aff6da`, pinned Rust/Cargo 1.97.1, and default Cargo concurrency on a 16-logical-CPU, 30-GiB big-red host.

The edit fixture adds one documentation line to `src/lib.rs` without changing behavior. Its tracked-workload diff digest is `sha256:bfdd60e73e8b106c0129d1052310495ae2dbe1ff70bb52b35a9f9ef4911927eb`.

`scripts/benchmark-developer-loop` runs the same locked lib/bin test command in every arm. Receipt
schema v2 derives selected, executed, passed, failed, ignored, measured, and filtered counts from
every Rust test-harness terminal summary; a successful command without a consistent observed
inventory is rejected instead of emitting a stale checked-in count. It excludes exactly 16
host-fact tests that currently observe different `/usr/bin/env` ownership and parent-directory
safety inside `hot-run`'s cross-worktree user/mount namespace. The script names every exclusion
and emits a path-free JSON receipt even when the workload fails. Historical measurements below
retain the exact 1,343-test corpus they observed at those earlier source revisions.

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
reapplying the edit, or replacing task state. `--retained-reuse-windows 3` or `7` extends that into
a bounded stable-lineage sequence; the two options are mutually exclusive and the single-window
flag remains the compatibility spelling for count one. The first edit and retained-reuse windows
have different comparison keys. First use binds `initial_for_source_state`; reuse binds
`retained_after_accepted_edit` plus its one-based ordinal. This prevents a pressure comparator from
treating first-use, reuse one, and reuse three as the same treatment. For `private-copy`, the first
window must prove a positive-time `seeded` preparation and every reuse must prove zero-time
`reused`; either mismatch rejects the observation. Every window still executes and validates the
complete frozen 1,343-test workload. It is therefore a command-reuse measurement, not result
reuse. Results retain `retained_reuse_window` as the first-window compatibility view and add the
complete ordered `retained_reuse_windows` sequence.

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

### Immediate retained-reuse discriminator

Candidate `8fb5232fe38e7dc8d7fe5103c7c28b75b4aed0ad` / tree
`98a60e35cfa651a6d5797ccf97399c561fc926a2` added and physically exercised the distinct
retained-reuse window on big-red. Six runs used ext4 mount 44, fan-out 4, four disjoint four-CPU
sets, 16 total Cargo jobs, resident/uncontrolled page cache, and the same edited source plus complete
1,343-test validator in both measured windows. The two-sample controls were native, private-copy,
and Overlay; run order was native, private-copy A1, Overlay B1/B2, private-copy A2, native.

| Arm | First-use samples (s) | First median (s) | Retained-reuse samples (s) | Reuse median (s) | Reduction from first median |
| --- | ---: | ---: | ---: | ---: | ---: |
| ordinary native | 25.872505, 27.227278 | 26.549892 | 17.996679, 17.846947 | 17.921813 | 32.50% |
| private-copy | 28.038855, 26.975499 | 27.507177 | 18.452164, 20.805015 | 19.628590 | 28.64% |
| Overlay | 29.582546, 29.279602 | 29.431074 | 18.199325, 17.942500 | 18.070913 | 38.60% |

First use reproduced the prior direction: private-copy reached all four results 1.923897 seconds /
6.54% sooner than Overlay. The immediate-reuse samples reversed that ordering: Overlay's median was
1.557677 seconds / 7.94% sooner than private-copy. Ordinary native remained the no-isolation
control; its reuse median was only 0.149100 seconds below Overlay, which this experiment cannot
resolve as a meaningful difference. Two samples do not justify changing the path selector. They do
show that expected lineage lifetime belongs in the next selector experiment rather than assuming
the first-use winner is also the steady-state winner.

Every private-copy first window proved `seeded` preparation in 0.863135–1.033674 seconds; every
reuse window proved `reused` with exactly zero preparation time. Observed allocated file blocks
grew between first and second windows by 16–20 KiB native, 64 KiB private-copy, and zero in both
Overlay samples. All 48 measured task validators were accepted, all windows observed four
simultaneous tasks, all six cleanups removed five of five worktrees, and the scratch root ended
empty.

Host aggregate CPU PSI `some` fractions were 1.24–1.99% during first use and 0.46–0.95% during
reuse. I/O PSI `some` was much larger and variable: 8.67–16.32% first use and 13.08–21.45% reuse.
The receipts therefore support the large within-lineage reuse reduction, but not fine-grained
sub-second ordering. A longer retained sequence with more repetitions should decide whether
Overlay's apparent steady-state advantage survives quieter I/O and later commands.

Receipt SHA-256 digests:

- native A1: `026bd905d9ccb128a244c003a8bbd463ea75cddc224f355476896c6b2c3fa6c9`
- private-copy A1: `8570318d2a946d3da11750b1aecd0e25d27d4b932ccc898bd007409fa8040dc5`
- Overlay B1: `e1bc3b0f50e8ca577a2ce2a6a38af1746fc3c5f8af8492dd82d48b9bc5b57b16`
- Overlay B2: `df4d3130a4a994d6e86333dd2469c6a3a5d887f5042e3a7ff25087f4dee525ec`
- private-copy A2: `2fa4216756108b90c1b35f6bbbd50092cf8e2517a618ee470e8a49a05600ceeb`
- native A2: `ea08811d6e93ef595ab94479457f6f6765bc54abedabdad053a0a3d67484696e`

### Stable-lineage retained sequence

Exact clean candidate `e57c049fe22363393a2b228d6f0fa541bf173328` / tree
`1d16068de71d74070bb24187aa0172f72b70aff6` extended the compatibility window into bounded one-,
three-, or seven-reuse sequences. Every ordinal receives its own comparison key, while the legacy
single-window result remains the exact ordinal-one view. The physical controls kept the same ext4
host, source, edit, validator, fan-out 4, four disjoint four-CPU sets, 16 total Cargo jobs, and
resident/uncontrolled page-cache declaration.

The first A-B-B-A bracket ran three reuses for native, private-copy, and Overlay. Its two-sample
medians were:

| Arm | First use (s) | Reuse 1 (s) | Reuse 2 (s) | Reuse 3 (s) | All-reuse median (s) | Reuse range (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ordinary native | 26.552257 | 18.222615 | 18.577368 | 17.875250 | 18.249267 | 17.700468–18.604845 |
| private-copy | 28.284841 | 18.200026 | 18.152647 | 18.353079 | 18.200026 | 17.902273–18.755589 |
| Overlay | 30.283737 | 18.001930 | 18.102296 | 17.796089 | 17.822915 | 17.748304–18.403516 |

Private-copy reached first use 1.998897 seconds / 6.60% sooner than Overlay. Overlay's median was
0.198096, 0.050352, and 0.556990 seconds lower at the three reuse ordinals, but the ranges overlapped
and private-copy still completed the cumulative first-use-plus-three sequence 1.193459 seconds
sooner. Ordinary native remained the fastest no-isolation first-use control and converged into the
same roughly 18-second reuse band.

Because that result predicted a possible later crossover, a second A-B-B-A bracket compared
private-copy and Overlay through the supported seven-reuse bound. Each entry below is the median of
two complete lineages; ordinal zero is the edited first use.

| Completed through ordinal | Private window (s) | Overlay window (s) | Private cumulative (s) | Overlay cumulative (s) | Private cumulative lead (s) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 27.458494 | 29.609687 | 27.458494 | 29.609687 | 2.151193 |
| 1 | 18.828968 | 18.150454 | 46.287462 | 47.760140 | 1.472679 |
| 2 | 17.975798 | 18.497924 | 64.263260 | 66.258063 | 1.994804 |
| 3 | 18.302575 | 18.223315 | 82.565834 | 84.481378 | 1.915544 |
| 4 | 18.277601 | 18.777487 | 100.843435 | 103.258865 | 2.415430 |
| 5 | 17.876091 | 18.521902 | 118.719526 | 121.780767 | 3.061241 |
| 6 | 18.374546 | 18.274976 | 137.094071 | 140.055743 | 2.961671 |
| 7 | 18.427896 | 18.052837 | 155.521968 | 158.108580 | 2.586612 |

There was no measured crossover. Private-copy stayed cumulatively faster after every ordinal and
finished the eight-command lineage 2.586612 seconds / 1.64% sooner. Its pooled 14-window reuse
median was also slightly lower at 18.224005 versus 18.328246 seconds. Individual lineage totals
overlapped—154.640634 and 156.403301 seconds private-copy versus 156.115406 and 160.101753 seconds
Overlay—so this is a bounded ext4 selector result, not a generic filesystem speed claim.

Overlay allocated a median 11.251150 GiB of task state versus 12.056736 GiB for private-copy, a
0.805586 GiB / 6.68% saving. Overlay showed zero additional allocated state through ordinal seven;
private-copy's median growth was 64 KiB. Host-aggregate I/O `full` PSI fractions remained large and
overlapping: medians were 15.62% Overlay and 16.83% private-copy, with maxima 19.16% and 19.90%.
CPU `some` PSI medians stayed below 0.7%. The cumulative ordering therefore decides only the tested
path policy: retain private-copy as the write-heavy cross-worktree `target` default through the
observed first use plus seven replays; do not introduce a speculative lifetime switch. Overlay
remains preferred for mostly-read source/dependency views and when its measured space advantage is
the controlling constraint.

The three-window bracket accepted all 96 measured task validators; the seven-window bracket
accepted all 128. Every validator executed the same 1,343-test workload, every window observed four
simultaneous tasks, all private-copy first preparations were positive-time `seeded`, all 80
private-copy reuse preparations were exact zero-time `reused`, every cleanup removed five of five
worktrees, and the scratch root ended empty. Receipt SHA-256 digests:

- three-window native A1: `1ce830ed290917674aa0a8e94e32dd411f32ba03358c9af2db17d5ab989ccf61`
- three-window private-copy A1: `603d76244b41c5d68764e8cacd953638eb7efc0f2ccbb7051856483e0c88faa8`
- three-window Overlay B1: `a9b8ea3b1038a9cafa52767157da3d9f617801e554575910e8b9640c74beb870`
- three-window Overlay B2: `bcd4ab981447597a0abb22f73f9122381127ecc150d1e4e001d3720e0ab0b442`
- three-window private-copy A2: `9033f0d9ce639fc4532f41937a789e408ef201fa5cf28f262a2b352424ee7d15`
- three-window native A2: `9186e58df7359c893a7f34c35b17fbb2b7a7c5735016d29c7d30a2f1fd72d315`
- seven-window private-copy A1: `368faf31cc9895268fdc2dc94d805d92db91d5dae8e616409c8537d4723b1b2e`
- seven-window Overlay B1: `5cec1b6b0f6c8693701bbd47f724be7209d7f53226bd6f5b1d3d70c11542b6b1`
- seven-window Overlay B2: `4dcdefef81329972aefbef8b90accfc6e0cea3010a2dcb73687908a24ef44c05`
- seven-window private-copy A2: `55d4b0b155cda1468c971ecec907e352297604fe8904cf4d90dcb10f966420dc`
