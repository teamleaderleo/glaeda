# SmolRunner

**Blazingly hot Linux execution for coding agents and GitHub Actions on the Mac you already own.**

SmolRunner turns operator-owned Apple-silicon compute into a trust-tiered Linux execution layer. It keeps GitHub as the ordinary workflow surface, uses Lima/VZ for Linux execution, persists the minimum durable truth required for recovery, and chooses worker residency from trust and measured value.

> **Disposable is a capability, not a mandate. Trust decides residency.**

For hostile or unknown repository work, SmolRunner targets one fresh isolated worker, one bounded job, exact teardown, and proven absence. For trusted CI, it can reuse prepared workers, repository seeds, reviewed cache generations, and warm pools. For ultra-trusted agent work, it can keep project sandboxes, worktrees, compiler state, package state, indexes, and selected services resident across edit/test/build loops.

The target experience is simple:

```text
agent work appears
-> SmolRunner selects the hottest valid execution path
-> Linux environment is ready
-> repo / dependencies / build state are already warm where trust permits
-> useful command starts
-> useful result returns
-> state is retained, reset, or destroyed according to trust + validity + value
```

## Status

SmolRunner is pre-alpha and in live systems acceptance on Apple silicon.

The strict disposable path already includes a substantial durable controller: prepared Lima/VZ generations, official GitHub Runner Scale Set integration, Keychain credential acquisition, durable assignment and no-replay handling, clone/JIT/teardown composition, LaunchAgent supervision, controller-death evidence, exact worker ownership, and repeated physical Quarry pilots.

Recent work has also established the first trusted persistent runner lane, warm pause/resume, auto-idle behavior, and the new top-level product direction: **make SmolRunner blazingly hot**.

The immediate production work remains:

- finish the installed-service one-job disposable journey;
- close restart-safe ownership for in-flight Lima mutations;
- extend sleep/reboot/outage/teardown recovery;
- prove and implement the hostile-worker network boundary.

Hot trusted execution progresses in parallel where it preserves those guarantees. See the [roadmap](docs/ROADMAP.md), [#557](https://github.com/teamleaderleo/smolrunner/issues/557), and the strict disposable programme [#365](https://github.com/teamleaderleo/smolrunner/issues/365).

## What SmolRunner is optimizing

The headline metric is **agent wall-clock latency**.

Measure the whole path:

```text
work becomes known
-> target / resident sandbox selected
-> capacity admitted
-> environment ready
-> repo/revision usable
-> dependency/build state usable
-> first useful command
-> first useful test/build result
-> final trustworthy result
-> teardown or residency transition
```

Useful product metrics include:

- queue-to-first-useful-command;
- edit-to-first-test-result;
- edit-to-final-relevant-verification;
- task completion wall time;
- fleet throughput under concurrent agents;
- disk/RAM/CPU residency cost;
- hot-state hit/miss/reset behavior.

## Trust-tiered execution

### Hostile / unknown

```text
fresh prepared Linux worker
-> one bounded job
-> terminal result
-> runner removal
-> VM teardown
-> prove absence
```

This lane carries the strongest isolation and cleanup contract. Job-specific writable state dies with the worker. Reusable inputs come from separately reviewed immutable/read-only generations where policy permits.

### Trusted CI

```text
job arrives
-> prepared worker / warm pool
-> exact repo seed or already-present Git objects
-> eligible dependency/compiler/artifact generations
-> execute
-> destroy or retire according to policy
```

The job can remain disposable while expensive preparation stays hot around it.

### Ultra-trusted agent/project work

```text
resident project sandbox
-> revalidate project/source/toolchain lease
-> edit
-> test
-> inspect
-> build
-> test
-> keep valuable incremental state resident
```

Useful resident state may include:

- project checkout and task-local worktrees;
- Rust incremental build trees and `sccache` state;
- npm/pnpm/Bun package state;
- Python environments;
- Maven/Gradle state;
- language-server and code-search indexes;
- container/build caches;
- test daemons/watchers and selected development services.

Resident state remains working state. Durable execution truth, source identity, trust class, project lease, toolchain generation, credential/network capability generation, and reset policy remain authoritative.

## Why Linux on a Mac?

Agentic development creates intense filesystem churn: Git worktrees, package stores, `node_modules`, compiler trees, indexes, test outputs, caches, and large cleanup operations.

SmolRunner keeps those repository filesystem operations inside Linux. macOS remains the trusted control plane while the guest handles the small-file-heavy developer workload using Linux filesystem semantics.

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

M6 makes the Linux storage layer itself a benchmark target: XFS/reflink and other credible Linux storage choices, project volumes, cheap task-local forks, package-manager behavior, compiler-tree reuse, and host backing-file growth.

## Why SmolRunner instead of a few Lima commands?

Lima supplies excellent VM primitives. SmolRunner supplies the durable execution runtime around them.

| Capability | Direct Lima | SmolRunner |
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

The distinction becomes clearest around crashes and ambiguity. SmolRunner persists exact authority before external mutations, freshly observes external state during recovery, and preserves debt when ownership or completion remains ambiguous.

## Hot execution programme

The roadmap now treats hot execution as a primary programme.

### Hot environments

Compare:

```text
cold disposable
prepared disposable
warm-pool disposable
resident project after idle
resident project immediate reuse
resident task loop
```

### Hot repository state

Explore local Git object seeds, resident trusted checkouts, cheap task-local worktrees, read-only shared bases, and repository hydration overlapped with admission.

### Hot Linux storage

Benchmark real agent workloads across credible Linux filesystem/storage choices:

- Git worktree add/remove;
- pnpm/npm/Bun install from warm state;
- hardlink/reflink behavior;
- Cargo incremental builds;
- Maven/Gradle trees;
- large small-file creation/deletion;
- compiler/index state reuse;
- many concurrent agent task forks;
- guest logical bytes versus Mac-side Lima disk growth.

The product question is simple: **which storage path gives the quickest repeated agent loop at an acceptable CPU/disk cost?**

### Hot build and dependency state

Treat package state, compiler caches, build outputs, container layers, prepared dependency environments, indexes, and derived verification artifacts as typed state with explicit validity and utility accounting.

### Hot services

For ultra-trusted projects, keep expensive dev services resident when their lifecycle is explicit: language servers, test watchers, compiler servers, local fixtures, builders, and repository-specific daemons.

## Durable execution truth

SmolRunner persists the minimum facts required to decide the next safe action after restart. Physical execution state stays replaceable.

Useful durable facts include:

- job / attempt / execution identity;
- capacity and ownership generations;
- exact mutation intent and restart-safe execution ownership;
- template/toolchain/input generation identities;
- GitHub delivery/acquisition/runner identities;
- VM/sandbox/project-lease bindings needed for reconciliation;
- terminal outcome and teardown receipts;
- explicit recovery debt.

VMs, workspaces, caches, indexes, compiler trees, and resident services can be destroyed and reconstructed from canonical inputs. That destroyability is what makes aggressive trusted residency safe.

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
sudo ./target/debug/smolrunner host prepare --file examples/quarry.yml
sudo ./target/debug/smolrunner host prepare --file examples/quarry.yml --confirm EXACT_CONFIRMATION
```

Cargo can execute build-time code. Give elevation to the already-built reviewed SmolRunner binary.

## Manifest boundary

A SmolRunner manifest describes host and execution policy while repositories continue to own build/test semantics and GitHub workflow YAML.

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
- **Trust decides residency.** Hostile work evaporates; trusted projects may stay warm.
- **Destroyability preserves freedom.** Physical state can disappear without losing execution truth.
- **Official GitHub protocol.** Use the official runner and Scale Set client.
- **Plan before mutation.** External side effects follow exact reviewed authority.
- **Prove ownership.** Names, PIDs, labels, and directory presence carry zero cleanup authority by themselves.
- **Linux executes; Mac controls.** Repository filesystem churn lives in Linux while secrets and durable control stay outside the worker.
- **Measure complete loops.** Optimize queue/edit-to-useful-result and fleet throughput.
- **Explain reuse.** Reports should say what stayed resident, what hit, what reset, and why.
- **Stay smol.** Prefer mature components and a compact explicit control surface.

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

- [Roadmap](docs/ROADMAP.md)
- [Product evolution](docs/PRODUCT_EVOLUTION.md)
- [Disposable autoscaling CI](docs/DISPOSABLE_AUTOSCALING_CI.md)
- [Threat model](docs/THREAT_MODEL.md)
- [Manifest reference](docs/MANIFEST.md)
- [Project workspaces](docs/PROJECT_WORKSPACES.md)
- [Host reconciliation](docs/HOST_RECONCILIATION.md)
- [Reliable control loop and fleet operating model](docs/OPERATING_MODEL.md)
- [Leased execution and previews](docs/LEASED_EXECUTION.md)
- [Agent instructions](AGENTS.md)

## License

SmolRunner is licensed under the [Apache License, Version 2.0](LICENSE).
