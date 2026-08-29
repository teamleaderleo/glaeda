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

The aggregate observation binds the harness commit/tree, frozen source/tree/edit, arm, fan-out,
exact CPU-affinity sets, per-task Cargo concurrency, setup and complete-window latency, every
semantic and `hot-run` receipt, mount/device/filesystem identity, backing-filesystem free bytes,
per-tree logical bytes and summed allocated file blocks before/after the window, and cleanup
disposition. Summed `st_blocks` are not unique physical usage on reflink-capable filesystems; the
filesystem-level observation keeps that distinction visible. A failed task cancels the remaining
process groups. The child environment is a closed allowlist: caller target overrides, compiler
wrappers/flags, and toolchain overrides are excluded; the accepted Cargo home is offline and held
constant. Setup and byte-observation time remain outside the primary request-to-all-results window
and are reported separately. Page-cache state is explicitly uncontrolled/resident in this harness;
the cold-read discriminator is a separate experiment. The harness grants no policy, cache,
result-reuse, cleanup, or shared-mutation authority.
