# SmolRunner agent instructions

SmolRunner is pronounced “small runner.” Treat “SmallRunner” and “small runner” as references to this project when context points here.

## Product boundary

SmolRunner is a Rust-based steward for automatically provisioned, disposable GitHub Actions workers on an operator-owned Mac. It admits bounded capacity, provisions one isolated worker per job, manages the official runner lifecycle, destroys workers after use, and reconciles crashes and orphaned state without routine operator intervention. An exploratory Linux runner steward and leased execution model remain secondary tracks.

GitHub Actions remains the scheduler and workflow language. The first hostile-workload boundary is a disposable Lima/VZ virtual machine with no host integration, a one-job just-in-time official runner, controlled network egress, and hard host-wide resource limits. Rootless Podman on the host is not the critical path; nested containers may run inside the disposable VM.

Do not turn SmolRunner into a new pipeline language, runner protocol, Kubernetes controller, public multi-tenant platform, custom container runtime, custom reverse proxy, or automatic production deployment system.

## Current priorities

1. Deliver the end-to-end disposable autoscaling path in `docs/DISPOSABLE_AUTOSCALING_CI.md` before additional narrow R01 proof slices.
2. Preserve the practical threat-model invariants in `docs/THREAT_MODEL.md`: hostile jobs stay inside a disposable VM, receive no host integrations or unrelated secrets, have controlled network access and hard resource ceilings, and leave no useful persistence after destruction.
3. Reuse mature components for workflow semantics, VM isolation, guest boot, networking, and service supervision. Add custom proof only for a realistic escape, persistence, secret, lateral-movement, network-abuse, resource-exhaustion, or recovery path that those boundaries do not own.
4. Preserve and integrate the existing durable store, queue/admission, Lima lifecycle, GitHub observation, journal, and recovery work.
5. Prefer idempotent reconciliation over one-shot setup. Provisioning, JIT registration, execution, teardown, stale-runner cleanup, reboot recovery, and scale-to-zero must become automatic.
6. Keep project-specific build and test behavior in GitHub Actions and enrolled repositories. Do not invent a pipeline language or runner protocol.
7. Unknown durable schemas, ownership evidence, attempt identities, or external state remain fail-closed. Distinguish proven absence from unknown state.
8. Enforce one job per disposable VM, least-privilege ephemeral credentials, host-wide concurrency/resource budgets, and a hostile-CI network policy before production acceptance.
9. Use GitHub Actions cache/artifact services first. Reusable host-local writable build outputs are deferred until poisoning, quota, and authority boundaries are proven.
10. Defer previews, deployment, multi-host placement, providers, Kubernetes, retained workspaces, and dashboards until the single-Mac unattended lifecycle is dependable.

## Workspace bootstrap

Run `./scripts/bootstrap` from the repository root before selecting a verification profile. Use `./scripts/bootstrap --output json` for the versioned capability receipt. The command is read-only: it observes the checkout, toolchain, resource envelope, cache classes, and operation-specific Git readiness while preserving source, lockfiles, Git configuration, credentials, and host state. See `docs/WORKSPACE_BOOTSTRAP.md`.

## Required checks

Before declaring a change ready:

```bash
./scripts/bootstrap --output json
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo run --locked --quiet -- --output json doctor
cargo run --locked --quiet -- plan --file examples/quarry.yml
cargo run --locked --quiet -- --output json plan --file examples/glossless.yml
cargo run --locked --quiet -- --output json host plan --file examples/quarry.yml
```

A bootstrap result of `ready_with_declared_deviations` is acceptable when every deviation is recorded and irrelevant to the selected repository verification profile. `blocked` must be resolved before verification. A doctor warning is acceptable on a development machine that lacks Podman or systemd. A doctor failure must be understood and documented. Planning must never mutate the filesystem, users, services, containers, routes, leases, GitHub state, release state, incident stores, backup stores, or fleet policy.

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
- Names, labels, mutable tags, preview slots, release channels, and path basenames never prove ownership or exact release identity.
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
- No self-update may replace privileged control logic while a job or reconciliation journal is active. Exact binary digest, state compatibility, drain state, previous verified version, and post-switch health evidence are required before an upgrade can complete.
- No automatic repair path may exist without an explicit action-class policy, repair budget, circuit breaker, exact ownership, durable checkpoint, fresh post-action observation, and defined rollback or compensation class.
- Fleet directives are desired-state inputs, not remote-shell authority. Host-local ownership conflicts, unknown state, active work, recovery mode, and operator holds remain vetoes.
- Incident evidence is local-first, bounded, versioned, and redacted. It must exclude raw repository contents, arbitrary logs, environment dumps, tokens, credentials, and unrelated machine data.
- Agent diagnosis and recommendations remain separate from observed facts. Agents may propose issues, tests, patches, and pull requests but may not expand their own authority, approve their own privileged mutation, or treat a generated fix as verified before reproducing and clearing the original failure.
- Restore begins quarantined. Restored names and documents do not authorise adoption until fresh host and GitHub observations satisfy the canonical ownership contract.

## Multi-agent coordination

- Follow `docs/AGENT_COORDINATION.md` whenever work is delegated between agents.
- Repository implementation must finish in the active work session with an observable artifact or an explicit blocked or failed result.
- Do not create scheduled tasks, reminders, recurring checks, or condition watches to wake, poll, or coordinate implementation agents.
- Define the exact base SHA, owned scope, deliverable, checks, completion signal, and recovery rule before delegation.
- Git branches, exact commit SHAs, pull requests, comments, and workflow conclusions are valid signals. Hidden chat state and silence are not.
- Check a delegated completion signal at most twice during one coordination pass. After the second miss, classify the work as stalled and take over, reassign, or reduce the scope.
- A dependent agent completes all independent work, reports the exact dependency, and stops. It must not remain indefinitely in a waiting or listening state.

## Pull requests

Keep changes small enough to review. State the security impact, commands run, and any host assumptions. Do not claim a VPS, GitHub runner, Podman preview, route, provider path, automatic repair, rollback, restore, incident upload, or fleet directive passed unless the exact tested commit and result are available.

## Merge authority

- Repository merge is an ordinary reversible repository action. Agents may merge an exact pull-request head once required checks pass, declared review requirements are satisfied, the complete final diff remains within scope, and GitHub reports the candidate mergeable.
- A separate human merge approval is never required.
- Routine low-risk changes may be self-reviewed and merged by their author after the required checks and complete-final-diff review.
- Privilege, ownership, adoption, durable persistence and recovery, rollback or compensation, secret handling, concurrency, destructive-operation, or comparable high-risk changes require an independent exact-head acceptance. After acceptance, an eligible agent may merge the accepted head. The acceptance reviewer does not author the final repair it accepts.
- Merge with an expected head SHA. Any head movement expires prior acceptance and requires the applicable review again.
- Update canonical trackers and dependent lanes immediately after merge.
- Human approval may still be required for effects beyond repository merge, including credential configuration or rotation, operator-machine service installation or removal, paid-capacity changes, external publication or contact, release signing, destructive non-test data changes, and irreversible migrations without a proven recovery path.

## Review workflow

- Review your own diff continuously while implementing. Before declaring a change ready, read the complete final diff once for correctness, accidental scope, stale comments, duplicated logic, missing failure cases, and violations of the accepted ADRs.
- Treat that final once-over plus the required checks as the normal review path. Avoid spawning extra review agents or external review services for routine changes.
- CodeDriver usage is exhausted. Do not contact CodeDriver, request work from it, or make repository progress depend on it.
- CodeRabbit is unavailable because the operator has no remaining usage. Do not request `@coderabbitai review`, wait for CodeRabbit, or treat a stale CodeRabbit status as acceptance.
- For privilege-boundary, ownership or adoption, durable persistence and recovery, rollback or compensation, secret handling, concurrency or race-sensitive, destructive, or comparable high-risk changes, use an implementation-independent Codex agent to review the complete exact-head diff. Record its evidence-based verdict on the pull request.
- Do not request GitHub-hosted Codex reviews or mention `@codex review`. Use locally coordinated Codex review agents only when the repository's independent-review rule requires them or the human operator explicitly asks for them.
- A large diff alone does not require an independent agent review. Documentation, mechanical configuration, formatting, temporary verification work, focused tests, and ordinary typed refactors stay self-reviewed.
- Verify every automated finding against the current code and accepted ADRs. Prioritize demonstrated privilege, ownership, persistence, rollback, race, security, data-loss, and correctness failures.
- Ignore or explain away speculative style, blanket documentation, duplication, and refactoring suggestions that do not improve behavior or reduce a concrete maintenance risk.
- Use the `review-exempt` label or `[skip review]` in the title for temporary verification pull requests, mechanical configuration changes, and other changes where automated review would add little value.
