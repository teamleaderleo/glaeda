# Roadmap

SmolRunner should remain useful while it is still small. The roadmap favors a dependable CLI and explicit host state over a control-plane service or dashboard.

The runner-steward design remains the foundation. Later milestones may extend the same ownership, isolation, and planning model into leased workspaces, temporary previews, and a small pool of execution workers. GitHub Actions remains the first scheduler and workflow language.

## Milestone 0 — foundation

- [x] Rust CLI with human and JSON output.
- [x] Host `doctor` checks for Linux, systemd, cgroup v2, Podman, and required commands.
- [x] Threat model and non-goals.
- [x] Continuous formatting, linting, and test verification.

## Milestone 1 — desired state

- [x] Versioned `smolrunner.yml` manifest.
- [x] Typed host, runner, project, and resource-limit models.
- [x] `smolrunner plan` that makes no changes.
- [x] Typed current-host observations with present, absent, and unknown state.
- [x] Shell-free command execution records with an empty child environment and explicit secret redaction.
- [x] Typed execution lanes, precondition evidence, rollback classes, and partial-failure journals.
- [x] ADR for privilege separation, adoption, token handling, and rollback semantics.
- [x] Pure ownership identity and managed/adoptable/foreign/conflicting/unknown classification.
- [x] ADR for `/var/lib/smolrunner`, marker identity, and name-safe adoption policy.
- [x] Canonical locators, immutable evidence, observation lanes, and survival policy for each resource kind.
- [ ] Atomic ownership persistence with crash recovery.
- [x] Atomic execution-journal checkpoints with explicit interrupted states.
- [x] Typed root and runner-user lane executors with sealed command and environment boundaries.
- [ ] Integrate lane executors with durable reconciliation journals.
- [ ] Debian and Ubuntu host preparation.
  - [x] Conservative prerequisite package planning with exact distribution identity, package observations, rollback class, and reviewed `apt-get` argv.
  - [x] Bounded package-state probing and host-plan CLI integration.
  - [x] Classify successful, nonzero, refused, and uncertain package attempts with mandatory fresh-observation recovery barriers.
  - [x] Dependency-aware runner account, subordinate-ID, home-directory, and linger preparation planning.
  - [x] Bounded account/group/home/subordinate-ID/linger observation.
  - [x] Runner account observation integration with read-only host plans.
  - [x] Define the read-only rootless Podman readiness contract and fail-closed probe sequence.
  - [ ] Implement rootless Podman readiness observation in `host plan`.
  - [ ] Durable package and account reconciliation execution.

## Milestone 2 — runner lifecycle

- [ ] Install a checksum-verified official GitHub Actions runner.
- [ ] Repository and organization registration scopes.
- [ ] Dedicated Linux user and systemd service management.
- [ ] Runner status, version inspection, update, disable, and removal.
- [ ] Short-lived registration-token handling without persistent plaintext storage.

## Milestone 3 — project execution

- [ ] Project-owned Containerfile and verification command.
- [ ] Rootless Podman image build and digest recording.
- [ ] Immutable committed-source archives.
- [ ] Separate network policy for dependency installation and verification.
- [ ] Capability dropping, no-new-privileges, and resource limits.
- [ ] Focused and full suite conventions without inventing a pipeline language.
- [ ] Explicit artifact references for successful verification runs.

## Milestone 4 — small-fleet operations

- [ ] Multi-host inventory over SSH.
- [ ] Fleet-wide `doctor`, status, and upgrade planning.
- [ ] Disk-pressure and stale-image diagnostics.
- [ ] Machine-readable remediation suggestions.
- [ ] Optional terminal UI backed by the same core library.

## Milestone 5 — leased execution foundation

- [x] Record the optional leased-execution and preview direction.
- [x] Define platform-independent lease kinds, states, legal transitions, terminal behavior, and optimistic revisions.
- [x] Record the initial lifecycle decision in ADR 0004.
- [x] Define crash-safe lease persistence and stale-revision rejection.
- [ ] Define source, artifact, preview-slot, and route ownership evidence.
- [ ] Define expiry deadlines, renewal windows, and clock-recovery behavior.
- [ ] Map lease cleanup into typed execution-journal actions.
- [ ] Add read-only lease planning and inspection commands.

## Milestone 6 — local previews

- [x] Accept a verified immutable OCI image digest or static artifact.
- [x] Plan one local preview without mutation.
- [ ] Start a bounded rootless Podman preview.
- [ ] Reconcile a temporary route through a narrow reverse-proxy adapter.
- [x] Keep a stable preview slot while verified artifacts supersede one another.
- [ ] Expire previews automatically and recover cleanup after host restart.
- [ ] Measure startup time, idle memory, and retained disk use on a small VPS.

## Milestone 7 — retained workspaces

- [ ] Create one writable worktree and container boundary per active claim or actor.
- [ ] Retain selected workspace state under an explicit lease.
- [ ] Sleep and wake eligible workspace or preview processes.
- [ ] Share only explicitly scoped package and build caches.
- [ ] Record commit, log, screenshot, and preview artifact references.
- [ ] Support optional Stensibly references without making Stensibly a dependency.

## Milestone 8 — worker selection

- [ ] Advertise bounded worker capabilities and current pressure.
- [ ] Distinguish continuous workers from opportunistic laptop workers.
- [ ] Select workers by architecture, capacity, cached artifacts, and routing capability.
- [ ] Preserve explicit operator policy and explain every placement decision.
- [ ] Avoid promising a general-purpose or multi-tenant scheduler.

## Later, only with evidence

- External deployment-target adapters for selected static or preview workloads.
- Per-target budgets and an explicit local-first fallback policy.
- Web dashboard.
- Background daemon for lease supervision, heartbeats, and asynchronous cleanup.
- GitHub App authentication.
- Ephemeral machine provisioning.
- Additional Linux distributions and service managers.

## Non-goals

- Replacing GitHub Actions workflow YAML.
- Reimplementing the GitHub Actions runner protocol.
- Kubernetes runner scale sets.
- Public-fork execution on persistent personal hosts.
- Becoming a generic public deployment platform.
- Automatically deploying every successful verification run.
- Building a custom container runtime, reverse proxy, or TLS stack.
