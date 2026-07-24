# Roadmap

SmolRunner should remain useful while it is still small. The roadmap favors a dependable CLI and explicit host state before adding long-running services.

The runner-steward work remains the foundation. Later milestones may extend the same ownership, isolation, and reconciliation model to leased workspaces and temporary previews. GitHub Actions remains the first scheduler and workflow interface. See [leased execution and previews](LEASED_EXECUTION.md).

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
- [ ] Atomic ownership and journal persistence with crash recovery.
- [ ] Root and runner-user lane implementations.
- [ ] Debian and Ubuntu host preparation.

## Milestone 2 — runner lifecycle

- [ ] Install a checksum-verified official GitHub Actions runner.
- [ ] Repository and organization registration scopes.
- [ ] Dedicated Linux user and systemd service management.
- [ ] Runner status, version inspection, update, disable, and removal.
- [ ] Short-lived registration-token handling without persistent plaintext storage.

## Milestone 3 — disposable project execution

- [ ] Project-owned Containerfile and verification command.
- [ ] Rootless Podman image build and digest recording.
- [ ] Immutable committed-source archives.
- [ ] Separate network policy for dependency installation and verification.
- [ ] Capability dropping, no-new-privileges, and resource limits.
- [ ] Focused and full suite conventions without inventing a pipeline language.
- [ ] Typed run identity, status, exit result, log reference, and artifact references.
- [ ] Explicit cache policy separated from writable workspace state.

## Milestone 4 — small-fleet operations

- [ ] Multi-host inventory over SSH.
- [ ] Fleet-wide `doctor`, status, and upgrade planning.
- [ ] Worker capability reporting for architecture, CPU, memory, storage pressure, and optional features.
- [ ] Disk-pressure and stale-image diagnostics.
- [ ] Machine-readable remediation suggestions.
- [ ] Optional terminal UI backed by the same core library.

## Milestone 5 — leased workspaces and previews

This milestone is an optional extension of disposable execution. Verification can continue to run on each eligible push while live previews require an explicit lease or repository policy.

- [ ] Typed lease model with owner, expiry, renewal, resource request, and cleanup policy.
- [ ] Workspace lifecycle with independent Git worktrees and bounded writable state.
- [ ] Minimal artifact contract for OCI image digests and static output.
- [ ] Explicit preview request from CLI and GitHub Actions.
- [ ] Rootless Podman preview lifecycle with health checks and resource limits.
- [ ] Reverse-proxy adapter with temporary routes and TLS handled by an existing proxy.
- [ ] Automatic TTL cleanup and crash-safe reconciliation.
- [ ] Distinguish warm cache, preserved workspace, and live process policy.
- [ ] Attach preview, run, log, and artifact references to external tools without requiring them.

## Milestone 6 — execution pool and target selection

Only pursue this after one-host leased execution proves useful.

- [ ] Worker heartbeats and availability for always-on and opportunistic machines.
- [ ] Capacity reservations and bounded concurrent execution.
- [ ] Selection by architecture, declared resources, cached artifacts, and routing capability.
- [ ] Remote execution through enrolled SmolRunner workers.
- [ ] Narrow deployment-target interface with local Podman as the reference backend.
- [ ] Evaluate one external provider adapter using the same artifact and lease semantics.
- [ ] Optional Stensibly linkage for claims, handoffs, workspaces, previews, and artifact references.

## Later, only with evidence

- Background daemon for lease expiry, service supervision, route reconciliation, and worker heartbeats.
- Web dashboard.
- GitHub App authentication.
- Ephemeral machine provisioning.
- Additional Linux distributions and service managers.
- Idle preview suspension and restart.
- More deployment-target adapters.

## Non-goals

- Replacing GitHub Actions workflow YAML.
- Reimplementing the GitHub Actions runner protocol.
- Kubernetes runner scale sets.
- Public-fork execution on persistent personal hosts.
- Hostile multi-tenant execution.
- Becoming a generic public deployment platform.
- Reimplementing a container runtime, reverse proxy, TLS stack, or provider-specific build system.
- Deploying every successful agent iteration by default.
