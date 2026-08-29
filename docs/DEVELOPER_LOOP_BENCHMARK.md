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

The first path-class follow-up kept the same frozen commit, tree, fixture, toolchain, command,
16-logical-CPU host, and default Cargo concurrency. Three independent task lineages each started
with an empty `target:private` directory mounted at the resident project's stable pathname. Each
lineage ran the complete unedited workload once, then received the exact fixture and ran the
complete workload once more. This prevents later samples from becoming no-edit cache hits.

| Phase | Wall seconds | Median | User CPU seconds | System CPU seconds | Peak RSS KiB | Result |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| Private-lineage cold prime | 43.98, 41.98, 42.43 | 42.43 | 110.01, 106.96, 105.55 | 9.31, 9.51, 9.06 | 2,513,796, 2,534,772, 2,510,176 | green |
| Private-lineage edit | 9.87, 10.50, 10.78 | 10.50 | 16.96, 17.97, 18.65 | 3.27, 3.21, 3.60 | 1,631,732, 1,633,072, 1,632,096 | green |

The outer `hot-run` edit measurements were 9.971136, 10.600774, and 10.898277 seconds. Every edit
receipt carried the exact fixture digest and every run completed 1,343 executed tests with one
existing ignored test.

The `10.50`-second private-lineage median is `0.56` seconds, or about 5.1%, faster than the prior
`11.06`-second private-Overlay median. It recovers about 75% of the measured `0.75`-second gap, but
remains `0.19` seconds, or about 1.8%, behind the `10.31`-second ordinary-native median. One
immediate no-edit repeat completed the inner workload in 3.20 seconds and the whole `hot-run`
invocation in 3.374774 seconds; that is a warm-suite floor, not an edit result.

For sample one, the private state occupied 1,960,765,984 logical bytes and 1,963,511,808 allocated
bytes after the base prime. After the edit it occupied 2,716,690,227 logical bytes and
2,719,531,008 allocated bytes, growth of 755,924,243 logical and 756,019,200 allocated bytes. All
three final lineages had 2,716,690,227–2,716,690,270 logical bytes and exactly 2,719,531,008
allocated bytes.

After recording the receipts, all three route-owned worktrees removed in less than 0.01 seconds
each at the timer's precision. Removing the external state-and-receipt root, including about
8,158,593,024 allocated bytes across the three final lineages, took 0.49 seconds of wall time,
0.00 seconds user CPU, 0.48 seconds system CPU, and 2,004 KiB peak RSS. No mount survived cleanup.

This settles the ext4 choice more narrowly than “private is faster.” An empty private lineage pays
the full cold compile once and is appropriate only when a task/agent will retain it across enough
useful edits. A short-lived task should inherit a warmed Overlay lower. Reflink-capable storage can
change that break-even by seeding the private lineage cheaply; big-red's current ext4 filesystem
cannot claim that result. Source fan-out, Git metadata, dependencies, and compiler output therefore
remain separate mechanism decisions.

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

- Recover at least the measured 0.75-second resident overhead before claiming a warm Rust-loop win.
- Measure a leaf-function behavior edit and a public-type edit; the documentation edit mostly measures crate/test relink and namespace overhead.
- Add Python and current-project Node fixtures with repository-owned validators instead of pinning the product direction to legacy Node versions.
- Repeat each promoted arm enough times to publish p50 and p90, then add 1/4/8 concurrent-task fan-out with physical disk growth and cleanup latency.
- Keep semantic test selection and exact reusable verification receipts separate from filesystem/cache residency; they can compose, but neither authorizes the other.
