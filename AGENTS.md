# SmolRunner agent instructions

## Product boundary

SmolRunner is a Rust-based steward for a small number of self-hosted GitHub Actions runners and an exploratory leased execution host for ordinary Linux servers. It manages desired host state, official runner lifecycle, project isolation, diagnostics, and later optional runs, retained workspaces, and temporary previews.

GitHub Actions remains the first scheduler and workflow language. SmolRunner may eventually broker execution and preview targets, but it should begin with local rootless Podman and an explicit verify-often, deploy-deliberately policy.

Do not turn SmolRunner into a new pipeline language, runner protocol, Kubernetes controller, public multi-tenant platform, custom container runtime, custom reverse proxy, or automatic production deployment system.

## Current priorities

1. Preserve the threat-model invariants in `docs/THREAT_MODEL.md`.
2. Follow the privilege, adoption, rollback, ownership, canonical-evidence, and lease-lifecycle decisions in `docs/adr/`.
3. Build a dependable CLI and structured state model before adding a daemon, TUI, or web dashboard.
4. Prefer idempotent plans and explicit reconciliation over one-shot shell setup.
5. Keep project-specific build and test behavior inside each enrolled repository.
6. Unknown manifest, ownership-marker, fingerprint, lease, and artifact fields or versions must fail closed.
7. Distinguish proven absence from unknown state; never mutate based on an unproven assumption.
8. Keep frequent verification separate from live preview creation. A successful check does not imply a deployment.
9. Prove one local execution and preview backend before worker selection or external provider adapters.

## Required checks

Before declaring a change ready:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo run --locked --quiet -- --output json doctor
cargo run --locked --quiet -- plan --file examples/quarry.yml
cargo run --locked --quiet -- --output json plan --file examples/glossless.yml
cargo run --locked --quiet -- --output json host plan --file examples/quarry.yml
```

A doctor warning is acceptable on a development machine that lacks Podman or systemd. A doctor failure must be understood and documented. Planning must never mutate the filesystem, users, services, containers, routes, leases, or GitHub state.

## Implementation rules

- Unsafe Rust is forbidden.
- Human output and JSON output must be derived from the same typed report.
- Never print registration tokens, app keys, repository credentials, provider credentials, or secret environment values.
- Commit `Cargo.lock` and use locked Cargo operations for this binary application.
- Pin third-party GitHub Actions to reviewed commit SHAs.
- Every host mutation must eventually support plan/dry-run behavior and a clear rollback path.
- Invalid mutation plans must fail before the first executor call.
- Irreversible actions must block the entire batch before the first mutation unless explicitly confirmed.
- Rollback and compensation run in reverse completion order; do not describe compensation as restoration.
- Public journals may contain only public receipts and public failures.
- Names, labels, mutable tags, preview slots, and path basenames never prove ownership.
- Production planning and probing must use kind-specific canonical resource constructors; do not build free-form locators or fingerprints.
- Desired identities require their kind's minimum immutable evidence. Observations may omit evidence only so classification can report `unknown`; present evidence must validate canonically.
- An unmarked exact-evidence match is adoptable only after explicit confirmation; it is never automatically managed.
- Foreign, conflicting, and unknown resources are protected from mutation.
- A lease ID alone never proves ownership of a container, workspace, route, or artifact.
- Lease transitions must follow `docs/adr/0004-lease-lifecycle-core.md`; accepted transitions advance their revision and terminal leases remain terminal.
- Do not write state or markers until atomic persistence, permissions, symlink defense, locking, crash recovery, migrations, and installation-ID generation are concretely implemented.
- Do not add an apply path until durable ownership persistence, root elevation, runner-user execution, journal persistence, GitHub credential acquisition, and package-operation rollback classes are concretely implemented.
- Generated subprocesses must use explicit absolute program paths and argument vectors; do not introduce `sh -c` or equivalent implicit shells.
- Child-process environments must start empty and receive only explicit allowlisted values.
- Treat output redaction as defense in depth, not proof that a child process cannot transform or leak a secret.
- Use stable system interfaces and invoke existing tools where that is safer than recreating package-manager, systemd, Git, container-runtime, proxy, or TLS behavior.
- Avoid adding dependencies without a concrete need and maintenance rationale.
- Keep Linux-specific code behind a narrow host abstraction so unsupported platforms fail clearly.
- Tests must not require root, systemd, Podman, a reverse proxy, or live GitHub credentials unless explicitly marked as integration tests.
- Keep manifests limited to host and execution policy. Language-specific build behavior belongs in repository-owned scripts and Containerfiles.
- Keep the initial target local and explicit. Provider routing belongs behind a narrow adapter after local lifecycle and cleanup prove dependable.

## Pull requests

Keep changes small enough to review. State the security impact, commands run, and any host assumptions. Do not claim a VPS, GitHub runner, Podman preview, route, or provider path passed unless the exact tested commit and result are available.
