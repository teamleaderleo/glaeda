# Glaeda

**Blazingly hot, trust-tiered compute execution on the machines and fleets you control.**

Glaeda turns operator-controlled compute into a general execution runtime. It finds the quickest trustworthy path from declared work to useful compute results, keeps valuable state hot when policy permits, and preserves the exact durable truth needed to recover when physical execution state disappears or becomes ambiguous.

Coding agents and GitHub Actions are major current proving workloads. The first production backend is operator-owned Apple-silicon compute running Linux through Lima/VZ, with GitHub as an important workflow integration. The execution, capacity, hot-state, recovery, and placement model is intended to serve broader compute workloads too.

> **Disposable is a capability. Trust decides residency.**

Hostile or unknown work can receive fresh isolated execution state, one bounded capability set, exact teardown, and proven absence. Trusted repeatable work can reuse prepared environments, immutable inputs, reviewed cache generations, and warm pools. Ultra-trusted work can keep long-lived compute contexts, mutable working state, indexes, accelerators, and selected services resident when exact validity and ownership rules permit it.

The target experience is simple:

```text
work appears
-> Glaeda identifies exact workload intent, trust, inputs, and required capabilities
-> Glaeda selects eligible compute and the hottest valid reusable state
-> capacity is admitted
-> only the state that must change is materialized
-> useful compute begins
-> useful outputs and evidence return
-> state is retained, reset, migrated, quarantined, or destroyed by policy
-> interruption converges through exact durable recovery
```

## Status

Glaeda is pre-alpha and in live systems acceptance on Apple silicon.

The strict disposable path already includes a substantial durable controller: prepared Lima/VZ generations, official GitHub Runner Scale Set integration, Keychain credential acquisition, durable assignment and no-replay handling, clone/JIT/teardown composition, LaunchAgent supervision, controller-death evidence, exact worker ownership, and repeated physical Quarry pilots.

Recent work has also established the first trusted persistent runner lane, warm pause/resume, auto-idle behavior, and the new top-level product direction: **make Glaeda blazingly hot**.

Current M6 main now carries the first concrete hot-state substrate rather than only a roadmap: bounded hot-execution performance receipts, a pure path-class policy for deciding which state may be shared, crash-safe single-writer project-disk leases, trusted OverlayFS task-view and mount-plan cores, descriptor-bound mount prerequisites, immutable Git object-pool generations with consumer leases and fixed markers, and Git index stat patching for copy-on-write task materialization. These are landed reusable primitives; the end-to-end routing and benchmarked task loop is still being composed around them.

The immediate production work remains:

- finish the installed-service one-job disposable journey;
- close restart-safe ownership for in-flight Lima mutations;
- extend sleep/reboot/outage/teardown recovery;
- prove and implement the hostile-worker network boundary;
- compose the landed M6 OverlayFS, Git-pool, lease, and performance primitives into measured trusted task loops.

Hot trusted execution progresses in parallel where it preserves those guarantees. See the [general compute runtime](docs/COMPUTE_RUNTIME.md), [roadmap](docs/ROADMAP.md), [#770](https://github.com/teamleaderleo/smolrunner/issues/770), [#557](https://github.com/teamleaderleo/smolrunner/issues/557), and the strict disposable programme [#365](https://github.com/teamleaderleo/smolrunner/issues/365).

## What Glaeda is optimizing

The universal objective is **time to a useful accepted compute result, plus throughput and resource cost under exact trust and correctness constraints**.

Measure the whole path:

```text
work becomes known
-> workload / trust / input identity resolved
-> target and reusable state selected
-> capacity admitted
-> environment ready
-> required inputs and state usable
-> first useful compute
-> first useful output
-> final accepted result
-> teardown or residency transition
```

Useful product metrics include:

- request-to-first-useful-compute;
- request-to-first-useful-output;
- request-to-final-accepted-result;
- queueing and contention cost;
- workload completion wall time;
- fleet throughput under concurrent workloads;
- CPU/RAM/storage/network/accelerator occupancy where relevant;
- warm-state hit/miss/reset benefit;
- recovery time after interruption.

Workload families can add stronger domain metrics. Current coding-agent and CI proving workloads care deeply about edit-to-first-test, edit-to-build, verification completion, and multi-agent throughput. Data, rendering, model, service, and batch workloads can define their own useful-result metrics without adopting agent or repository semantics.

## Trust-tiered execution

### Hostile / unknown

```text
fresh prepared execution state
-> one bounded workload
-> terminal output/evidence
-> exact teardown
-> prove absence
```

This lane carries the strongest isolation and cleanup contract. Workload-specific writable state dies with the execution environment. Reusable inputs come from separately reviewed immutable/read-only generations where policy permits.

The current hostile proving workload is arbitrary repository/CI code inside a fresh Linux worker.

### Trusted repeatable compute

```text
work arrives
-> prepared environment / warm pool
-> exact reusable inputs and eligible hot state
-> execute
-> destroy, reset, or retain according to policy
```

The physical execution can remain disposable while expensive preparation stays hot around it. Current examples include trusted CI, repository seeds, dependency/compiler caches, immutable derived artifacts, and prepared workers. The same pattern can serve data transforms, rendering inputs, model assets, or other repeatable compute families with their own exact validity contracts.

### Ultra-trusted resident compute

```text
resident compute context
-> revalidate exact owner / inputs / runtime / capability generations
-> perform useful work
-> retain valuable mutable state and services
-> reset, migrate, stop, or evict when policy changes
```

Current agent/project work is one strong example: a resident project sandbox can retain worktrees, compiler state, package state, indexes, and selected services across edit/test/build loops.

Useful resident state may include:

- project checkout and task-local worktrees;
- Rust incremental build trees and `sccache` state;
- npm/pnpm/Bun package state;
- Python environments;
- Maven/Gradle state;
- language-server and code-search indexes;
- container/build caches;
- immutable datasets and derived representations;
- model weights and preprocessed shards;
- renderer caches and compiled assets;
- test daemons/watchers and selected workload services.

Resident state remains working state. Durable execution truth, workload/input identity, trust class, ownership/lease generations, runtime/toolchain identity, credential/network capability generations, and reset policy remain authoritative.

## Why Linux on a Mac?

The current proving workloads create intense filesystem churn: Git worktrees, package stores, `node_modules`, compiler trees, indexes, test outputs, caches, data preparation, and large cleanup operations.

Glaeda keeps those Linux-native workload operations inside Linux. macOS remains the trusted control plane while the guest handles the small-file-heavy and Linux-specific execution path using Linux filesystem semantics.

The current prepared worker uses:

- Apple Virtualization Framework through Lima/VZ;
- ARM64 Ubuntu;
- Lima plain mode;
- no host mounts;
- no SSH-agent/X11 forwarding;
- no proxy-environment inheritance;
- no Rosetta;
- a separate workload account;
- exact pinned runner/template identities.

M6 makes the Linux storage layer itself a benchmark target: XFS/reflink and other credible Linux storage choices, project volumes, cheap task-local forks, package-manager behavior, compiler-tree reuse, data representations, and host backing-file growth.

## Why Glaeda instead of a few Lima commands?

Lima supplies excellent VM primitives. Glaeda supplies the durable execution runtime around them.

| Capability | Direct Lima | Glaeda |
|---|---|---|
| Create/start/stop/delete Linux VMs | ✅ | ✅ |
| Clone a prepared VM | ✅ | ✅ |
| Pin exact guest + runner inputs | Manual | ✅ |
| Bind one VM to one durable execution identity | Manual | ✅ |
| Reserve CPU/RAM/disk before admission | Manual | ✅ |
| Recover across controller death | Human work | ✅ durable recovery model |
| Preserve ambiguous mutation outcomes without replay | Human work | ✅ |
| Poll GitHub Runner Scale Sets | DIY | ✅ official client bridge |
| Keep controller GitHub credentials in Keychain | DIY | ✅ |
| JIT one ephemeral GitHub runner | DIY | ✅ composed |
| Exact runner/VM teardown and capacity release | DIY | ✅ composed |
| LaunchAgent supervision | DIY | ✅ |
| Hostile-CI network policy | DIY | Active M4 work |
| Persistent trusted project residency | DIY | Active hot-execution programme |
| Adaptive cache/verification/routing decisions | DIY | Planned #21/#546/#547 |
| Agent-readable diagnosis and recovery hints | DIY | Planned #548 |

The distinction becomes clearest around crashes and ambiguity. Glaeda persists exact authority before external mutations, freshly observes external state during recovery, and preserves debt when ownership or completion remains ambiguous.

The same runtime boundary can sit above additional backends over time: Apple container / Containerization, native Linux hosts, operator-owned fleet nodes, accelerator-equipped hosts, and selected remote or burst compute where policy permits.

## Hot execution programme

The roadmap now treats hot execution as a primary programme.

### Hot environments

Compare:

```text
cold disposable
prepared disposable
warm-pool disposable
resident context after idle
resident immediate reuse
resident repeated-work loop
```

### Hot repository state

For the current development/CI workload family, explore local Git object seeds, resident trusted checkouts, cheap task-local worktrees, read-only shared bases, and repository hydration overlapped with admission.

### Hot Linux storage

Benchmark real compute workloads, beginning with current agent/dev loops, across credible Linux filesystem/storage choices:

- Git worktree add/remove;
- pnpm/npm/Bun install from warm state;
- hardlink/reflink behavior;
- Cargo incremental builds;
- Maven/Gradle trees;
- dataset preparation and derived-representation reuse;
- large small-file creation/deletion;
- compiler/index state reuse;
- many concurrent task forks;
- guest logical bytes versus Mac-side Lima disk growth.

The product question is simple: **which storage path gets repeated useful computation to its next accepted output soonest at an acceptable resource cost?**

### Hot build and dependency state

Treat package state, compiler caches, build outputs, container layers, prepared dependency environments, indexes, datasets, model assets, renderer artifacts, and derived verification outputs as typed state with explicit validity and utility accounting.

### Hot services

For ultra-trusted workloads, keep expensive services resident when their lifecycle is explicit: language servers, test watchers, compiler servers, local fixtures, builders, databases, model servers, render helpers, and workload-specific daemons.

## Durable execution truth

Glaeda persists the minimum facts required to decide the next safe action after restart. Physical execution state stays replaceable.

Useful durable facts include:

- workload / attempt / execution identity;
- capacity and ownership generations;
- exact mutation intent and restart-safe execution ownership;
- template/runtime/toolchain/input generation identities;
- external delivery/acquisition identities where integrations require them;
- VM/sandbox/resident-context bindings needed for reconciliation;
- terminal outcome and teardown receipts;
- explicit recovery debt.

VMs, workspaces, caches, indexes, compiler trees, datasets, model state, and resident services can be destroyed and reconstructed from canonical inputs when their family contract says they are rebuildable. That destroyability is what makes aggressive trusted residency safe.

## Current commands

Read-only machine/project inspection:

```bash
cargo run --locked -- doctor
cargo run --locked -- --output json doctor
cargo run --locked -- plan --file examples/quarry.yml
cargo run --locked -- --output json plan --file examples/glossless.yml
cargo run --locked -- host plan --file examples/quarry.yml
```

For the legacy Linux host-preparation lane, build unprivileged and elevate only the reviewed binary:

```bash
cargo build --locked
sudo ./target/debug/glaeda host prepare --file examples/quarry.yml
sudo ./target/debug/glaeda host prepare --file examples/quarry.yml --confirm EXACT_CONFIRMATION
```

Cargo can execute build-time code. Give elevation to the already-built reviewed Glaeda binary.

## Manifest boundary

The current Glaeda repository manifest describes host and repository-execution policy while repositories continue to own build/test semantics and GitHub workflow YAML. Broader compute workload adapters can carry their own exact typed input and output contracts instead of being forced through repository configuration.

```yaml
version: 1
repository: example/project

runner:
  scope: repository
  user: project-runner
  labels: [project-ci]

container:
  image: localhost/project-ci:1
  file: build/ci/Containerfile

verify:
  command: scripts/run-vps-verification.sh
  suites:
    focused: focused
    full: full

limits:
  memory: 2GiB
  cpus: 1.5
  pids: 768

trust:
  forks: deny
  trigger: operator
```

See the [manifest reference](docs/MANIFEST.md) and example [Quarry](examples/quarry.yml) / [Glossless](examples/glossless.yml) manifests.

## Design principles

- **Blazingly hot.** Remove repeated work until the useful operation dominates wall time.
- **Compute is the product domain.** Current workload families are proving grounds, not ceilings.
- **Trust decides residency.** Hostile work evaporates; trusted compute may stay warm.
- **Destroyability preserves freedom.** Physical state can disappear without losing execution truth.
- **Execution authority and result authority stay separate.** A process, backend, cache hit, benchmark, dataset, model output, render, or verification receipt carries only the authority its owning contract grants.
- **Backends are mechanisms.** Adopt better execution primitives without redefining workload semantics or recovery.
- **Plan before mutation.** External side effects follow exact reviewed authority.
- **Prove ownership.** Names, PIDs, labels, and directory presence carry zero cleanup authority by themselves.
- **Linux executes; Mac controls today.** Linux is the current workload execution environment while secrets and durable control stay outside workers.
- **Measure complete loops.** Optimize request-to-useful-result, throughput, and resource cost.
- **Explain reuse.** Reports should say what stayed resident, what hit, what reset, and why.
- **Stay lean.** Prefer mature components and a compact explicit control surface.

## Development

Rust 2024 stable is used. The repository commits `Cargo.lock` and checks formatting, locked dependency resolution, Clippy, tests, doctor output, reference plans, and read-only host planning:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo run --locked --quiet -- --output json doctor
cargo run --locked --quiet -- plan --file examples/quarry.yml
cargo run --locked --quiet -- --output json plan --file examples/glossless.yml
cargo run --locked --quiet -- --output json host plan --file examples/quarry.yml
```

## Project documents

- [General compute runtime](docs/COMPUTE_RUNTIME.md)
- [Roadmap](docs/ROADMAP.md)
- [Product evolution](docs/PRODUCT_EVOLUTION.md)
- [Blazingly hot execution](docs/BLAZINGLY_HOT.md)
- [Disposable autoscaling CI](docs/DISPOSABLE_AUTOSCALING_CI.md)
- [Threat model](docs/THREAT_MODEL.md)
- [Manifest reference](docs/MANIFEST.md)
- [Project workspaces](docs/PROJECT_WORKSPACES.md)
- [Host reconciliation](docs/HOST_RECONCILIATION.md)
- [Reliable control loop and fleet operating model](docs/OPERATING_MODEL.md)
- [Leased execution and previews](docs/LEASED_EXECUTION.md)
- [Agent instructions](AGENTS.md)

## License

Glaeda is licensed under the [Apache License, Version 2.0](LICENSE).
