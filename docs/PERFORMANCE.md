# Personal-worker performance baselines

This document defines the reproducible Q03 benchmark method and records the first bounded personal-worker baseline. Performance evidence informs later engineering and release decisions; timings are not correctness assertions and do not weaken any existing verification, privacy, durability, ARM64, installed-CLI, or disposable-Linux gate.

## Benchmark runner

The repository-owned runner is:

```text
scripts/benchmark-personal-worker.py
```

Typical release measurement:

```text
python3 scripts/benchmark-personal-worker.py \
  --profile release \
  --samples 30 \
  --warmups 5 \
  --output q03-report.json
```

Bounded smoke execution:

```text
python3 scripts/benchmark-personal-worker.py --smoke --output -
```

The runner uses Python's standard library only. Unless `--binary` is supplied, it builds the installed `smolrunner` binary with the selected Cargo profile. `--skip-build` may be used only when the named binary already exists and its identity is understood.

The emitted JSON report contains:

- exact checkout commit and dirty-state verdict;
- release/debug/external binary class and SHA-256 digest;
- Rust release and host target;
- bounded operating-system, architecture, logical-CPU, and memory classes;
- sample and warm-up counts;
- deterministic fixture digests;
- first observation, minimum, median, nearest-rank p95, and maximum duration;
- explicit dependency blocks for measurements the current product cannot honestly perform;
- privacy assertions.

It does not report the checkout path, fixture path, home directory, hostname, environment dump, credentials, or raw command output.

## What each timing includes

Every CLI timing includes process creation and output rendering. Read benchmarks also include exact durable file open, bounded read, decode, canonical re-encoding validation, read-model projection, and JSON output.

Submission and cancellation timings exclude deterministic fixture creation. They include process creation, store locking, exact validation, staged write, file synchronisation, atomic publication, directory synchronisation, and JSON rendering. Therefore they are end-to-end durable transaction measurements, not an in-memory codec microbenchmark.

The first observation is reported separately. Recorded warm samples run after the configured warm-ups. The runner does not attempt privileged filesystem-cache eviction, CPU pinning, frequency control, or scheduler isolation. Results should be compared only with reports that describe a compatible machine class and method.

## Deterministic fixtures

The runner creates private temporary Unix durable stores with the same canonical wire schema and permissions used by the reviewed adapter:

- state root and managed directory: mode `0750`;
- `store.lock` and `current.json`: mode `0600`;
- schema version, revision, and queue generation: `1`;
- queue sizes: `10`, `100`, and `256`;
- immutable repository, commit, tree, profile, runner-profile, resource, cache, request, and time identities.

The current accepted queue limit is 256 entries. A 1,000-job fixture would be invalid and is not used to create an impressive but meaningless number.

## Initial hosted-Linux baseline

This baseline was collected by disposable diagnostic PR #278, workflow `Q03 benchmark diagnostic` run #3, job `benchmark`.

Evidence identity:

- diagnostic checkout/merge commit: `6b415aa0c6d510f0e926792fd8d38470cb160827`;
- canonical benchmark-runner head contained in that merge: `361a01986466b7a4c58666d9756ced17e3bea221`;
- release binary SHA-256: `c778247fb0edf57161aa51721308c872f44a1d594f5f570af7b4079059582d38`;
- Rust: `1.97.1`, host `x86_64-unknown-linux-gnu`;
- host class: Linux x86-64, 1–4 logical CPUs, over 8 through 16 GiB memory;
- seven recorded samples after two warm-ups;
- source checkout clean;
- report privacy validation passed.

Times are milliseconds.

| Benchmark | Queue size | First | Median | p95 |
| --- | ---: | ---: | ---: | ---: |
| Installed CLI startup and help rendering | — | 1.438 | 1.063 | 1.071 |
| Store decode/validation and status projection | 10 | 1.271 | 1.235 | 1.320 |
| First one-item queue page | 10 | 1.254 | 1.240 | 1.254 |
| Middle one-item queue page | 10 | 1.247 | 1.239 | 1.379 |
| Final one-item queue page | 10 | 1.226 | 1.238 | 1.269 |
| Store decode/validation and status projection | 100 | 2.096 | 2.065 | 2.168 |
| First one-item queue page | 100 | 2.080 | 2.086 | 2.122 |
| Middle one-item queue page | 100 | 2.222 | 2.089 | 2.101 |
| Final one-item queue page | 100 | 2.029 | 2.071 | 2.149 |
| Store decode/validation and status projection | 256 | 3.698 | 3.665 | 3.709 |
| First one-item queue page | 256 | 3.675 | 3.610 | 3.653 |
| Middle one-item queue page | 256 | 3.585 | 3.598 | 3.671 |
| Final one-item queue page | 256 | 3.926 | 3.629 | 3.704 |
| Exact durable submission transaction | 10 before submit | 4.502 | 2.486 | 2.616 |
| Exact durable cancellation transaction | 1 before cancel | 2.009 | 2.001 | 2.154 |

Fixture document digests for this run:

| Queue size | SHA-256 |
| ---: | --- |
| 10 | `5b7fa188d18eaf5b6b0efdc17b94090ea819f299cdc216848d1f3da5de385729` |
| 100 | `1e342c9e44ae3ca92af475540c142e9c3c0e50ab40884565441f2490c1cdc57c` |
| 256 | `1580ca48c936d88d08e5bd758d26658cd7806a80ef705121dd2e87c30ed2d257` |

### Interpretation

On this hosted Linux class, process startup is a substantial part of the smallest measurements. Status and one-item pagination scale from roughly 1.2 ms at 10 jobs to roughly 3.6 ms at the valid 256-job cap. First, middle, and final page positions are effectively equivalent at each fixture size, so this baseline does not indicate position-dependent pagination work.

The durable submission and cancellation paths remain below 3 ms at the median in this environment even though the timing includes locking and synchronised publication. The slower first submission observation is consistent with an initially colder filesystem/process path, but seven samples are not sufficient to assign a cause.

The initial budgets proposed in W05 were hypotheses: 40 ms-class CLI startup, 20 ms-class status at 100 jobs, 50 ms-class large queue pagination, and 25 ms-class mutation excluding unavoidable sync cost. This one Linux run is comfortably below those classes even with process and durable synchronisation costs included. It does not establish universal thresholds, Mac results, or permission to fail correctness tests on timing variance.

## Measurements intentionally blocked

The report records these as explicit dependency blocks rather than fabricating stand-ins:

| Measurement | Dependency | Reason |
| --- | --- | --- |
| Status over 1,000 jobs | Current queue contract | The accepted queue limit is 256; 1,000 jobs is an invalid document. |
| Terminal completion transaction | W03 B07 | No accepted terminal-completion operator path is integrated. |
| Broker tick | W03 B01 | No accepted Q03 installed-binary benchmark entry point exists, and Q03 does not modify product runtime files. |
| Cold/warm named-profile execution | W03 B05/B06 and W04 | The bounded planner/executor journey is not integrated. |
| Lima stopped-to-ready and work-to-idle transitions | W04 physical acceptance | These require the separately approved physical Mac/Lima harness. CI data is not a substitute. |

When those product paths become accepted, extend the runner or the approved physical harness without changing the meaning of existing benchmark IDs.

## Comparing future reports

A useful comparison names both report identities and checks:

1. exact source commit and dirty state;
2. binary digest and build profile;
3. Rust release and host target;
4. machine class;
5. fixture digests;
6. sample and warm-up counts;
7. identical benchmark ID and timing boundary;
8. median and p95, not a single fastest sample.

Investigate a regression before optimising. Product runtime changes belong in a separate bounded issue with profiling evidence, exact ownership, correctness checks, and privacy/authority review. Q03 itself does not authorise runtime refactors.
