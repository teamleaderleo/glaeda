# SmolRunner

**Disposable GitHub Actions capacity on the Mac you already own.**

SmolRunner, pronounced “small runner,” automatically provisions isolated, one-job GitHub Actions workers on an operator-owned Mac and scales them back to zero. It is aimed at developers who want their own repositories and known open-source projects to use local compute without babysitting runners or exposing the host to potentially hostile CI code.

> [!IMPORTANT]
> SmolRunner is pre-alpha. The current executable has substantial durable state, queue, resource-admission, Lima lifecycle, GitHub observation, and recovery foundations, but it does not yet register a disposable JIT runner or run the unattended control loop. The governing path to that outcome is [Disposable autoscaling CI](docs/DISPOSABLE_AUTOSCALING_CI.md). The older Linux/rootless-Podman path is preserved as optional hardening, not the product critical path.

## The problem

GitHub Actions is easy to use until local capacity becomes another system to operate:

- queued jobs need capacity without ten idle VMs consuming RAM;
- persistent runners retain compromise and state across jobs;
- repository code must not inherit host credentials, files, sockets, or LAN access;
- failed provisioning, crashes, stale registrations, and orphan workers need automatic recovery;
- agents and humans need a trustworthy answer to “what is running, blocked, or being cleaned up?”;
- normal clones, dependency downloads, builds, tests, and nested containers still need to work.

SmolRunner keeps GitHub as the workflow scheduler, status UI, and primary job log store. It focuses on bounded admission, disposable Lima/VZ workers, the official JIT runner lifecycle, hostile-CI network policy, automatic teardown, and recovery.

## Why not just call Lima?

You can create a Linux VM on a Mac with a few Lima commands. SmolRunner starts where those commands stop.

A direct Lima workflow can create, start, stop, clone, and delete virtual machines. SmolRunner adds the durable controller around those operations so GitHub jobs can safely consume local Mac compute without a human remembering which VM belongs to which job, whether a previous operation completed, which resources are still reserved, or what should happen after a crash.

| Problem | Handwritten Lima commands | SmolRunner |
|---|---|---|
| Boot a Linux VM | ✅ | ✅ |
| Clone a prepared VM | ✅ | ✅ Current M2 path |
| Pin the exact guest image | You manage it | ✅ Current |
| Pin the exact Actions runner | You manage it | ✅ Current |
| Bind one VM to one durable attempt | Manual convention | ✅ Current |
| Reserve CPU/RAM/disk before creation | Manual | ✅ Current |
| Prevent two attempts from claiming the same capacity | Manual | ✅ Current |
| Recover after the controller/process crashes | Human recovery | ✅ Current |
| Distinguish an interrupted clone from a valid worker | Human investigation | ✅ Current |
| Prove a VM belongs to the exact attempt before deletion | Usually name-based | ✅ Current model |
| Refuse stale observations before mutation | Manual discipline | ✅ Current |
| Preserve ambiguous clone outcomes for recovery | Human investigation | ✅ Current |
| Recover exactly owned orphan workers after reboot | Script/manual cleanup | M2/M5 |
| Poll GitHub Scale Sets through the official client | DIY integration | ✅ Current M3 foundation |
| Read the controller GitHub App key from macOS Keychain | DIY | ✅ Current M3 foundation |
| Execute one job in one fresh worker | Manual | M3 |
| Bind the actual GitHub job to the exact runner and VM | Manual | M3 |
| Automatically create workers from GitHub demand | DIY controller | M3 |
| Keep long-lived controller credentials outside workers | DIY | M3/M4 |
| Block worker access to the Mac, LAN, controller, and peers | DIY network policy | M4 |
| Run container actions inside the disposable guest | DIY | M4 |
| Scale back to zero while idle | DIY controller | M5 |
| Recover from sleep, reboot, GitHub outage, or failed teardown | DIY controller | M5 |
| Preserve safe hot inputs while destroying job-specific state | DIY cache policy | M6 |
| Let multiple agents treat the Mac like a private build farm | DIY system | M7 |

The difference becomes clearest during failure. A manual sequence might look like:

```text
limactl clone --start template worker-123
→ process dies
→ a VM named worker-123 exists
→ figure out what happened
```

SmolRunner is designed around a durable sequence:

```text
reserve bounded capacity
→ bind attempt + prepared-template + VM identity
→ prove the VM is absent
→ durably authorize creation
→ re-observe immediately before mutation
→ execute one bounded Lima operation under the durable lock
→ observe what actually exists
→ durably checkpoint or preserve ambiguous recovery debt
→ resume safely after interruption
```

The `limactl` invocation is the VM primitive. SmolRunner owns the lifecycle, ownership, admission, recovery, GitHub integration, hostile-job policy, and unattended operation around it.

### Where the disposable path is today

Milestone 1, durable disposable-attempt reconciliation, is complete. M2 now includes pinned Ubuntu/Lima/runner inputs, durable prepared-template generations, clone recovery rules, and live `clone --start` execution under the canonical lock with ambiguous outcomes retained as recovery debt. Physical prepared-template/worker acceptance and the remaining exact cleanup/orphan path still gate M2 acceptance.

M3 has also started: the repository pins GitHub's official Runner Scale Set client behind a bounded bridge, and a private macOS adapter reads the GitHub App key directly from Keychain, validates the installed bridge identity, bounds every exchange, and zeroizes secret-bearing buffers. Durable message consumption, enrollment, guest JIT handoff, one-job execution, and the unattended service loop remain ahead.

The intended end state stays simple:

```text
GitHub job appears
→ SmolRunner reserves Mac capacity
→ fresh Linux worker appears
→ one official GitHub Actions runner executes one job
→ worker is destroyed
→ capacity returns to zero
```

The operator should care about the GitHub job, not the VM lifecycle underneath it. See the [roadmap](docs/ROADMAP.md) for the M1–M7 progression.

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

On Linux, first build SmolRunner as an unprivileged user. Then elevate only the built binary to propose one host-preparation phase and require its exact deterministic confirmation before mutation:

```bash
cargo build --locked
sudo ./target/debug/smolrunner host prepare --file examples/quarry.yml
sudo ./target/debug/smolrunner host prepare --file examples/quarry.yml --confirm EXACT_CONFIRMATION
sudo ./target/debug/smolrunner --output json host prepare --file examples/quarry.yml --confirm EXACT_CONFIRMATION
```

The first elevated command re-observes the host, prints the reviewed proposal and confirmation requirement, and performs no mutation. The confirmed command re-observes and replans in the same elevated process, executes only the matching single phase, checkpoints the journal before and after each action, and stops at any fresh-observation barrier. Initial host mutations are deliberately treated as irreversible; there is no generic `apply`, multi-phase continuation, automatic retry, or unattended repair path.

Do not run Cargo with `sudo`: Cargo may execute build scripts, procedural macros, compiler wrappers, and other build-time code before SmolRunner's own controls start. Only the reviewed, already-built SmolRunner binary should receive elevated privileges.

`doctor` probes Linux support, architecture, systemd, cgroup v2, Podman, and Git. `plan` validates the versioned manifest and describes the runner user, registration, container image, and disposable verification boundary SmolRunner would eventually reconcile. `host plan` additionally reads bounded host state and distinguishes proven absence from facts that still need a privileged or authenticated inspection path. These three commands are read-only. `host prepare` is the narrow mutating exception and requires explicit elevation plus an exact confirmation derived from the immediately preceding public proposal. Human and JSON output come from the same typed reports.

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

The process layer is shell-free, clears ambient environment variables, requires absolute program paths, captures structured results, and redacts explicitly marked secret values. The public mutation path consumes only reviewed typed root or runner-user commands from one confirmed host-preparation phase. It does not accept free-form commands or continue through a fresh-observation barrier. See [host reconciliation](docs/HOST_RECONCILIATION.md).

The execution-journal model assigns every future mutation an immutable ID, execution lane, rollback class, and precondition evidence. Invalid plans never reach an executor, unconfirmed irreversible work blocks the whole batch before its first mutation, and partial failures retain reverse-order rollback, compensation, and rollback-failure outcomes. The accepted architecture is recorded in [ADR 0001](docs/adr/0001-privilege-adoption-and-rollback.md).

The ownership model protects existing infrastructure from name-based adoption. A resource is managed only when its versioned marker, project identity, host installation identity, locator, and required immutable evidence all match. An exact unmarked match is merely adoptable and still requires explicit confirmation; foreign, conflicting, and unknown state remains protected. Durable installation, lease, and execution-journal state is stored beneath the reviewed Linux state root at `/var/lib/smolrunner` through symlink-safe, permission-checked, atomic adapters. See [ADR 0002](docs/adr/0002-durable-ownership-state.md).

Canonical constructors now define exact locators and minimum evidence for Linux users, managed directories, systemd services, official runner installations, rootless Podman images, and GitHub runner registrations. Desired identities cannot be created from names, mutable image tags, or labels alone; partial observations may omit evidence only so ownership classification can return `unknown`. The model also records which execution lane must collect each observation and which evidence survives host restore, repository transfer, or runner re-registration. See [ADR 0003](docs/adr/0003-canonical-resource-evidence.md).

The long-term [reliable control-loop operating model](docs/OPERATING_MODEL.md) keeps the external interface small while separating development, canary, and stable releases; defining drain and rollback; bounding incident evidence; preserving backup and recovery; and requiring host-local vetoes, repair budgets, fresh verification, and circuit breakers before any self-healing or fleet-directed mutation.

## Historical and optional expansion

The Linux runner-steward, rootless-Podman, leased-workspace, and preview work remains useful foundation and optional future scope. It is not the first production path.

The current policy keeps verification separate from deployment and puts a disposable Lima/VZ VM before host containers. Preview or deployment authority remains explicit and later.

The first implementation slice is a platform-independent, revisioned lease lifecycle for runs, retained workspaces, and previews. It plans legal state transitions without persistence or host mutation. See [ADR 0004](docs/adr/0004-lease-lifecycle-core.md).

The next slice adds fail-closed lease documents, an atomic no-replace and compare-and-swap storage contract, stale-revision rejection, and immutable artifact identities. The included memory store proves concurrency semantics for tests. See [ADR 0005](docs/adr/0005-lease-store-and-artifact-identity.md).

Preview-slot planning now coalesces repeated requests for the same artifact, port, and health endpoint into one runtime generation with a renewed lease. Changed runtime inputs produce one checked replacement generation. Bounded typed ports, lifetimes, health paths, and generation counters prevent callers from forging invalid plans. See [ADR 0006](docs/adr/0006-preview-slot-coalescing.md).

The local Podman command-planning slice binds an OCI artifact to bounded runtime limits, loopback-only publication, fixed ownership labels, and reviewed shell-free create/start/inspect/stop/remove vectors. It remains inert until a later executor proves ownership and authorizes each mutation. See [ADR 0007](docs/adr/0007-rootless-podman-preview-command-planning.md).

Podman inspection now decodes one bounded JSON result, classifies exact container ownership, and authorizes existing-container mutations only when the name, image digest, and every required label match the planned preview generation. Authorized commands target the observed full container ID. See [ADR 0008](docs/adr/0008-podman-preview-inspection-authorization.md).

Subprocess execution now captures stdout and stderr concurrently with a one-megabyte limit per stream. Excess output terminates the direct child and fails without producing a successful execution record. See [ADR 0009](docs/adr/0009-bounded-subprocess-output.md).

Podman inspect evidence now binds to the exact reviewed command and successful execution record before decoding. The public mutation gate accepts only that typed receipt, while stderr remains diagnostic and carries no authority. See [ADR 0010](docs/adr/0010-podman-inspect-execution-receipts.md).

Existing-container mutations now require a compatible observed Podman state in addition to exact ownership. Start is limited to inactive startable states, stop to running, and unforced remove to inactive removable states; missing, paused, transitional, dead, and unknown states fail closed. See [ADR 0011](docs/adr/0011-podman-state-aware-mutation-authorization.md).

State reconciliation now distinguishes executable work, an already satisfied goal, and a blocked state before authorization. Running start requests and inactive stop requests become explicit no-ops, while blocked states still cannot produce a subprocess command. See [ADR 0012](docs/adr/0012-podman-state-reconciliation-plans.md).

Lease records now have a process-durable Linux adapter beneath each installation. It validates filesystem ownership and permissions, serializes writers with a persistent lock, and publishes private versioned documents through synchronized atomic rename. See [ADR 0013](docs/adr/0013-durable-linux-lease-store.md).

See [leased execution and previews](docs/LEASED_EXECUTION.md), the [reliable control-loop operating model](docs/OPERATING_MODEL.md), and the updated [roadmap](docs/ROADMAP.md).

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

Later reliability commands such as upgrade, rollback, incident, backup, restore, and quarantine should preserve the same plan-before-mutation and stable-JSON contract rather than exposing the internal machinery directly.

## Design principles

- **Official runner, managed safely.** SmolRunner does not reimplement the GitHub Actions protocol.
- **Persistent listener, disposable execution.** Repository code belongs in bounded rootless containers, not directly on the host.
- **Plan before mutation.** Host changes should be idempotent, inspectable, and reversible.
- **Prove ownership.** Names and labels never authorize adoption or removal.
- **Secure defaults.** Fork execution, host sockets, untracked files, and secret inheritance are denied by default.
- **Boring infrastructure.** Debian or Ubuntu, systemd, cgroup v2, Podman, and one native binary.
- **Human and agent friendly.** Stable JSON is a first-class interface, not terminal output scraped after the fact.
- **Stay smol.** No mandatory daemon, database, dashboard, cloud controller, or Kubernetes cluster; reliability machinery stays behind an explicit compact interface.

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

Live `host prepare` acceptance additionally belongs in disposable Debian and Ubuntu environments because it requires Linux privilege, a reviewed state root, and exact host evidence.

## Project documents

- [Threat model](docs/THREAT_MODEL.md)
- [Manifest reference](docs/MANIFEST.md)
- [Host reconciliation](docs/HOST_RECONCILIATION.md)
- [Reliable control loop and fleet operating model](docs/OPERATING_MODEL.md)
- [Leased execution and previews](docs/LEASED_EXECUTION.md)
- [ADR 0001: privilege, adoption, and rollback](docs/adr/0001-privilege-adoption-and-rollback.md)
- [ADR 0002: durable ownership and state identity](docs/adr/0002-durable-ownership-state.md)
- [ADR 0003: canonical resource evidence](docs/adr/0003-canonical-resource-evidence.md)
- [ADR 0004: lease lifecycle core](docs/adr/0004-lease-lifecycle-core.md)
- [ADR 0005: atomic lease stores and artifact identity](docs/adr/0005-lease-store-and-artifact-identity.md)
- [ADR 0006: preview slot coalescing](docs/adr/0006-preview-slot-coalescing.md)
- [ADR 0007: rootless Podman preview command planning](docs/adr/0007-rootless-podman-preview-command-planning.md)
- [ADR 0008: Podman preview inspection and mutation authorization](docs/adr/0008-podman-preview-inspection-authorization.md)
- [ADR 0009: bounded subprocess output capture](docs/adr/0009-bounded-subprocess-output.md)
- [ADR 0010: Podman inspect execution receipts](docs/adr/0010-podman-inspect-execution-receipts.md)
- [ADR 0011: Podman state-aware mutation authorization](docs/adr/0011-podman-state-aware-mutation-authorization.md)
- [ADR 0012: Podman state reconciliation plans](docs/adr/0012-podman-state-reconciliation-plans.md)
- [ADR 0013: durable Linux lease store](docs/adr/0013-durable-linux-lease-store.md)
- [Roadmap](docs/ROADMAP.md)
- [Agent instructions](AGENTS.md)

## Project status

SmolRunner has a dependable diagnostic and desired-state foundation plus its first explicitly confirmed, one-phase durable host-mutation path. It can prepare reviewed portions of Linux host state and stop at fresh-observation barriers; it does not yet register the official runner, reconcile runner services, enroll projects through the public CLI, or operate an unattended control loop. Runner lifecycle, disposable execution, and small-fleet operations remain the next product milestones. A dashboard and broader distribution support remain deferred until the CLI and security model have proven themselves.

## License

SmolRunner is licensed under the [Apache License, Version 2.0](LICENSE).
