# SmolRunner

**GitHub Actions on the Linux boxes you already own.**

SmolRunner is a Rust-based steward for small fleets of self-hosted GitHub Actions runners. It is aimed at solo developers and small teams who have ordinary Linux servers, several repositories, and no desire to operate Kubernetes or inherit a full platform-engineering stack.

> [!IMPORTANT]
> SmolRunner is pre-alpha. The current executable provides host diagnostics and read-only desired-state and host-state planning; runner installation and reconciliation are roadmap work.

## The problem

The official GitHub Actions runner is straightforward to install once. The operational burden appears afterward:

- several repositories and organizations share one server;
- persistent runner services need upgrades and repair;
- repository code should not inherit host credentials or state;
- container limits, cgroup delegation, users, and systemd units drift;
- agents and humans need a trustworthy answer to “what is broken?”;
- setup should be repeatable on the next boring VPS.

SmolRunner keeps GitHub as the workflow scheduler, status UI, and log store. It focuses on host desired state, official runner lifecycle, disposable project execution, and diagnostics.

## Current commands

Inspect whether the current machine has the basic SmolRunner prerequisites:

```bash
cargo run --locked -- doctor
cargo run --locked -- --output json doctor
cargo run --locked -- doctor --strict
```

Validate a project manifest and print its deterministic desired-state plan:

```bash
cargo run --locked -- plan --file examples/quarry.yml
cargo run --locked -- --output json plan --file examples/glossless.yml
```

Compare the manifest with bounded observations from the current Linux host:

```bash
cargo run --locked -- host plan --file examples/quarry.yml
cargo run --locked -- --output json host plan --file examples/glossless.yml
```

`doctor` probes Linux support, architecture, systemd, cgroup v2, Podman, and Git. `plan` validates the versioned manifest and describes the runner user, registration, container image, and disposable verification boundary SmolRunner would eventually reconcile. `host plan` additionally reads bounded host state and distinguishes proven absence from facts that still need a privileged or authenticated inspection path. All commands are read-only, and human and JSON output come from the same typed reports.

## Manifest boundary

A SmolRunner manifest describes host and execution policy, not build steps:

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

Individual repositories continue to own their Containerfiles, dependency installation, test commands, and GitHub workflow YAML. SmolRunner will not introduce another pipeline language. Unknown fields and future schema versions fail closed.

See the [manifest reference](docs/MANIFEST.md) and the redacted [Quarry](examples/quarry.yml) and [Glossless](examples/glossless.yml) fixtures.

## Reconciliation boundary

SmolRunner models desired state, current state, proposed actions, execution, and ownership separately. Current observations are reported as `present`, `absent`, or `unknown`; unknown facts produce inspection actions rather than speculative mutations.

The process layer is shell-free, clears ambient environment variables, requires absolute program paths, captures structured results, and redacts explicitly marked secret values. It is not yet connected to any mutation command. See [host reconciliation](docs/HOST_RECONCILIATION.md).

The execution-journal model assigns every future mutation an immutable ID, execution lane, rollback class, and precondition evidence. Invalid plans never reach an executor, unconfirmed irreversible work blocks the whole batch before its first mutation, and partial failures retain reverse-order rollback, compensation, and rollback-failure outcomes. The accepted architecture is recorded in [ADR 0001](docs/adr/0001-privilege-adoption-and-rollback.md).

The ownership model protects existing infrastructure from name-based adoption. A resource is managed only when its versioned marker, project identity, host installation identity, locator, and required immutable evidence all match. An exact unmarked match is merely adoptable and still requires explicit confirmation; foreign, conflicting, and unknown state remains protected. The planned system state root is `/var/lib/smolrunner`, but no state-writing path exists yet. See [ADR 0002](docs/adr/0002-durable-ownership-state.md).

Canonical constructors now define exact locators and minimum evidence for Linux users, managed directories, systemd services, official runner installations, rootless Podman images, and GitHub runner registrations. Desired identities cannot be created from names, mutable image tags, or labels alone; partial observations may omit evidence only so ownership classification can return `unknown`. The model also records which execution lane must collect each observation and which evidence survives host restore, repository transfer, or runner re-registration. See [ADR 0003](docs/adr/0003-canonical-resource-evidence.md).

## Exploratory expansion

The runner-steward work remains SmolRunner's foundation. A later optional layer may use the same host ownership and isolation model for leased workspaces and temporary previews.

The proposed policy keeps frequent verification separate from live deployment: GitHub Actions may verify each eligible push, while a preview starts only after an explicit request or repository policy grants a bounded lease. The local rootless Podman path comes first; worker selection and external deployment targets come later only when useful.

The first implementation slice is a platform-independent, revisioned lease lifecycle for runs, retained workspaces, and previews. It plans legal state transitions without persistence or host mutation. See [ADR 0004](docs/adr/0004-lease-lifecycle-core.md).

See [leased execution and previews](docs/LEASED_EXECUTION.md) and the updated [roadmap](docs/ROADMAP.md).

## Intended workflow

The planned interface is deliberately small:

```text
smolrunner doctor
smolrunner plan
smolrunner host plan
smolrunner host prepare
smolrunner runner add
smolrunner project enroll
smolrunner status
smolrunner remove
```

## Design principles

- **Official runner, managed safely.** SmolRunner does not reimplement the GitHub Actions protocol.
- **Persistent listener, disposable execution.** Repository code belongs in bounded rootless containers, not directly on the host.
- **Plan before mutation.** Host changes should be idempotent, inspectable, and reversible.
- **Prove ownership.** Names and labels never authorize adoption or removal.
- **Secure defaults.** Fork execution, host sockets, untracked files, and secret inheritance are denied by default.
- **Boring infrastructure.** Debian or Ubuntu, systemd, cgroup v2, Podman, and one native binary.
- **Human and agent friendly.** Stable JSON is a first-class interface, not terminal output scraped after the fact.
- **Stay smol.** No mandatory daemon, database, dashboard, cloud controller, or Kubernetes cluster.

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

- [Threat model](docs/THREAT_MODEL.md)
- [Manifest reference](docs/MANIFEST.md)
- [Host reconciliation](docs/HOST_RECONCILIATION.md)
- [Leased execution and previews](docs/LEASED_EXECUTION.md)
- [ADR 0001: privilege, adoption, and rollback](docs/adr/0001-privilege-adoption-and-rollback.md)
- [ADR 0002: durable ownership and state identity](docs/adr/0002-durable-ownership-state.md)
- [ADR 0003: canonical resource evidence](docs/adr/0003-canonical-resource-evidence.md)
- [ADR 0004: lease lifecycle core](docs/adr/0004-lease-lifecycle-core.md)
- [Roadmap](docs/ROADMAP.md)
- [Agent instructions](AGENTS.md)

## Project status

The first milestone is a dependable diagnostic and desired-state foundation. Runner lifecycle, disposable execution, and small-fleet operations come before leased previews or worker-pool coordination. A dashboard and broader distribution support remain deferred until the CLI and security model have proven themselves.
