# Glaeda agent instructions

Use **Glaeda** / `glaeda` for current product and command surfaces. Historical sources may still say **SmolRunner**; preserve that name when it is part of truthful old evidence, an exact v1 identity, or an explicit migration note.

## Product boundary

Glaeda is a Rust execution runtime for **blazingly hot, trust-tiered Linux work on compute the operator controls**.

The product has three intentional execution classes:

- **hostile / unknown** — one fresh isolated Linux worker, one bounded job, exact teardown, proven absence;
- **trusted CI** — prepared workers, repository seeds, reviewed reusable generations, compiler/package caches, and warm pools while preserving a clean job boundary;
- **ultra-trusted agent/project work** — resident project sandboxes, task-local worktrees, incremental compiler/build state, package state, indexes, and selected services under an explicit project lease and reset/revalidation policy.

Disposable execution is a capability. Trust decides residency.

All three classes share one durable correctness kernel: exact execution/ownership identity, bounded authority, fresh external observation, crash recovery, and physical state that can be destroyed without losing the ability to decide the next safe action.

GitHub Actions remains the ordinary scheduler and workflow language for GitHub jobs. Glaeda owns local admission, execution policy, durable identity, worker/project lifecycle, recovery, hot-state lifecycle, and measured execution decisions around that interface.

The first hostile-workload boundary is a disposable Lima/VZ virtual machine with no host integration, a one-job just-in-time official runner, controlled network egress, and hard host-wide resource limits. The first hot trusted lane is Linux project residency on the operator Mac, with Lima/VZ as the current baseline and alternative mature backends selected from measurement.

Do not turn Glaeda into a new pipeline language, runner protocol, Kubernetes controller, public multi-tenant platform, custom hypervisor/container runtime, generic cloud scheduler, or automatic production deployment system.

## Current priorities

1. **Finish the strict disposable lifecycle.** Complete the end-to-end installed-service GitHub job path in `docs/DISPOSABLE_AUTOSCALING_CI.md` / #365 so hostile work has a dependable fresh-worker capability.
2. **Close the hostile boundary.** Preserve the threat-model invariants in `docs/THREAT_MODEL.md`: hostile jobs stay inside a disposable VM, receive no host integrations or unrelated secrets, have controlled network access and hard resource ceilings, and leave no useful hostile writable persistence after destruction.
3. **Make Glaeda blazingly hot.** Follow #557/#556 and `docs/BLAZINGLY_HOT.md`. Optimize agent wall-clock latency and choose residency from trust, workload, validity, and measured value.
4. **Measure before promoting optimizations.** #563 owns the bounded common performance receipt. #560 owns resident project storage. #562 owns cheap task/worktree materialization. #561 owns resident backend comparison. Use comparable receipts before selecting defaults.
5. **Keep valuable trusted state resident.** Ultra-trusted projects may intentionally retain project-local mutable checkout, dependency, build, index, and service state when exact lease/validity/reset rules exist.
6. **Keep hostile reuse separately reviewed.** Cross-job state consumed by hostile/unknown work must use explicit immutable/read-only or separately reviewed cache-generation contracts. Consumer authority and publisher authority remain distinct.
7. **Preserve rebuildability.** Every hot physical state family must have canonical reconstruction inputs plus reset/revalidation/eviction behavior. Losing hot state costs latency and compute, never execution truth.
8. **Reuse mature components.** Prefer Lima/VZ, Apple `container` where measurements justify it, Linux filesystems, Git, package-manager primitives, compiler caches, launchd/systemd, and existing networking/storage mechanisms over bespoke equivalents.
9. **Make unattended recovery boring.** Provisioning, JIT registration, execution, teardown, project revalidation, cache reset, stale-runner cleanup, reboot recovery, and scale-to-zero/idle convergence should become automatic within accepted authority.
10. **Feed measurements into adaptation later.** #21/#546/#547/#548 may learn from bounded observations after the underlying execution/storage/residency primitives produce trustworthy receipts.

## Hot-state rules

- Resident state is useful working state. It carries zero independent workflow-result, source, ownership, merge, or cleanup authority.
- Bind reusable/resident state to explicit trust class, project identity, lease/generation, source state, toolchain/prepared generation, credential/network capability generation, and reset/revalidation policy where relevant.
- A directory, VM name, disk name, PID, process name, mount point, cache hit, or surviving service never proves ownership by itself.
- Ambiguous residency becomes `revalidate_required`, `reset_required`, quarantine, or cold reconstruction. Never guess continuity.
- Keep project-local mutable state separate across trust/project boundaries. Shared writable state needs an explicit poisoning, ownership, quota, and publication design.
- Performance observations are evidence only. They may recommend a storage/backend/residency policy and grant zero mutation authority.
- Compare complete agent loops: queue/task-known -> first useful command -> first relevant result -> final trustworthy result, plus fleet throughput and idle CPU/RAM/disk cost.
- Separate optimization levers while benchmarking. Filesystem, VM backend, cache policy, test partitioning, and resource profile should change independently until evidence justifies composition.
- Prefer hotness in this order unless measurements say otherwise: retain valuable trusted state; remove repeated semantic work; reuse exact completed work; overlap independent preparation; share immutable inputs; parallelize independent work; optimize storage/hot kernels; add hardware or paid burst capacity.

## Workspace bootstrap

Run `./scripts/bootstrap` from the repository root before selecting a verification profile. Use `./scripts/bootstrap --output json` for the versioned capability receipt. The command is read-only: it observes the checkout, toolchain, resource envelope, cache classes, and operation-specific Git readiness while preserving source, lockfiles, Git configuration, credentials, and host state. See `docs/WORKSPACE_BOOTSTRAP.md`.

## Required checks

For a quick non-authoritative edit/test loop, run:

```bash
./scripts/verify fast
```

This runs library/binary tests first, formatting, one CLI build, and the doctor/reference-plan smoke
checks with per-phase timings. It deliberately skips Clippy and integration/acceptance targets, so
it is developer feedback only and never replaces final verification.

Add `--receipt PATH` to either profile to atomically retain a path-free performance observation
outside the source worktree. The receipt records the exact source and fixed plan identity, each
executed phase's wall/child-CPU timing, the process-lifetime maximum waited-child RSS observation
(not concurrent aggregate RSS), terminal exit status, and whether source remained unchanged. Opaque
Cargo target/home identities and the build-job setting make cold/warm samples comparable without
publishing private paths. It retains no command output and grants no result-reuse or publication
authority.

`./scripts/verify required` runs the exact eight commands below in order and streams their output
without retaining a log. The explicit commands remain listed as the canonical contract.

Before declaring a code change ready:

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

Documentation-only changes may use the repository's existing docs-only verification policy. Record when the GitHub workflow intentionally produces no run for an ignored docs path.

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
- Names, labels, mutable tags, preview slots, release channels, path basenames, VM names, disk names, and cache names never prove ownership or exact release identity.
- Production planning and probing must use kind-specific canonical resource constructors; do not build free-form locators or fingerprints.
- Desired identities require their kind's minimum immutable evidence. Observations may omit evidence only so classification can report `unknown`; present evidence must validate canonically.
- An unmarked exact-evidence match is adoptable only after explicit confirmation; it is never automatically managed.
- Foreign, conflicting, and unknown resources are protected from mutation.
- A lease ID alone never proves ownership of a container, workspace, route, disk, cache, artifact, or resident project sandbox.
- Lease transitions must follow `docs/adr/0004-lease-lifecycle-core.md`; accepted transitions advance their revision and terminal leases remain terminal.
- Do not write durable state or authority-bearing markers until atomic persistence, permissions, symlink defense, locking, crash recovery, migrations, and installation/generation identity are concretely implemented for that state family.
- Do not add an apply path until durable ownership persistence, root elevation, runner-user execution, journal persistence, GitHub credential acquisition, and package-operation rollback classes are concretely implemented.
- Generated subprocesses must use explicit absolute program paths and argument vectors; do not introduce `sh -c` or equivalent implicit shells.
- Child-process environments must start empty and receive only explicit allowlisted values.
- Treat output redaction as defense in depth, not proof that a child process cannot transform or leak a secret.
- Use stable system interfaces and invoke existing tools where that is safer than recreating package-manager, filesystem, systemd, Git, container-runtime, proxy, or TLS behavior.
- Avoid adding dependencies without a concrete need and maintenance rationale.
- Keep Linux-specific code behind a narrow host/backend abstraction so unsupported platforms fail clearly.
- Tests must not require root, systemd, Podman, filesystem formatting, VM creation, a reverse proxy, or live GitHub credentials unless explicitly marked as physical/integration tests with an exact opt-in.
- Keep manifests limited to host and execution policy. Language-specific build behavior belongs in repository-owned scripts, package-manager configuration, Containerfiles, and GitHub workflows.
- A performance optimization must identify its baseline, candidate identity, comparable-work definition, semantic validator where needed, primary latency metric, secondary CPU/RAM/disk effects, and fallback/reset behavior.
- Repository builders or benchmark workloads cannot grant themselves stronger cache/artifact/residency authority by emitting metadata.
- Exact input mismatch, corrupt reusable state, invalid project lease, or stale toolchain generation produces a miss/reset/cold path rather than optimistic reuse.
- No self-update may replace privileged control logic while a job or reconciliation journal is active. Exact binary digest, state compatibility, drain state, previous verified version, and post-switch health evidence are required before an upgrade can complete.
- No automatic repair path may exist without an explicit action-class policy, repair budget, circuit breaker, exact ownership, durable checkpoint, fresh post-action observation, and defined rollback or compensation class.
- Fleet directives and optimizer outputs are desired-state/recommendation inputs, not remote-shell or mutation authority. Host-local ownership conflicts, unknown state, active work, recovery mode, and operator holds remain vetoes.
- Incident and performance evidence is local-first, bounded, versioned, and content-minimised. It must exclude raw repository contents, arbitrary logs, environment dumps, tokens, credentials, and unrelated machine data.
- Agent diagnosis and recommendations remain separate from observed facts. Agents may propose issues, tests, patches, and pull requests but may not expand their own authority, approve their own privileged mutation, or treat a generated fix as verified before reproducing and clearing the original failure.
- Restore begins quarantined. Restored names and documents do not authorise adoption until fresh host/GitHub/backend observations satisfy the canonical ownership contract.

## Physical performance experiments

Physical hot-execution benchmarks mutate local test/project state and require explicit experiment boundaries even when the workload is ultra-trusted.

Before one experiment:

1. record exact Glaeda head, macOS/hardware identity class, backend version, guest/kernel/filesystem identity, project/revision, toolchain/package-manager versions, resource profile, and experiment candidate;
2. keep one semantic workload constant while changing one optimization dimension;
3. record cold and warm/reuse paths separately;
4. record physical allocated bytes and host backing growth separately from logical file sizes;
5. retain exact cleanup/rebuild evidence for experiment-created VMs, disks, mounts, worktrees, caches, and services;
6. leave ambiguous physical state in a named recovery/quarantine state instead of broad cleanup;
7. publish only bounded results and opaque/canonical identities; private paths and arbitrary command output stay private.

Do not run filesystem formatting, VDO creation, VM replacement, disk deletion, or operator-machine network mutation as ordinary hosted CI.

## Multi-agent coordination

- Follow `docs/AGENT_COORDINATION.md` whenever work is delegated between agents.
- Repository implementation must finish in the active work session with an observable artifact or an explicit blocked or failed result.
- Do not create scheduled tasks, reminders, recurring checks, or condition watches to wake, poll, or coordinate implementation agents.
- Define the exact base SHA, owned scope, deliverable, checks, completion signal, and recovery rule before delegation.
- Git branches, exact commit SHAs, pull requests, comments, and workflow conclusions are valid signals. Hidden chat state and silence are not.
- Check a delegated completion signal at most twice during one coordination pass. After the second miss, classify the work as stalled and take over, reassign, or reduce the scope.
- A dependent agent completes all independent work, reports the exact dependency, and stops. It must not remain indefinitely in a waiting or listening state.

## Pull requests

Keep changes small enough to review. State the security impact, commands run, and any host assumptions. Do not claim a filesystem/backend benchmark, GitHub runner, VM lifecycle, cache hit, resident project path, provider path, automatic repair, rollback, restore, incident upload, or fleet directive passed unless the exact tested commit and result are available.

## Merge authority

- Repository merge is an ordinary reversible repository action. Agents may merge an exact pull-request head once required checks pass, declared review requirements are satisfied, the complete final diff remains within scope, and GitHub reports the candidate mergeable.
- A separate human merge approval is never required.
- Routine low-risk changes may be self-reviewed and merged by their author after the required checks and complete-final-diff review.
- Privilege, ownership, adoption, durable persistence and recovery, rollback or compensation, secret handling, concurrency, destructive-operation, or comparable high-risk changes require an independent exact-head acceptance. After acceptance, an eligible agent may merge the accepted head. The acceptance reviewer does not author the final repair it accepts.
- Merge with an expected head SHA. Any head movement expires prior acceptance and requires the applicable review again.
- Update canonical trackers and dependent lanes immediately after merge.
- Human approval may still be required for effects beyond repository merge, including credential configuration or rotation, operator-machine service installation or removal, paid-capacity changes, physical filesystem/network mutation, external publication or contact, release signing, destructive non-test data changes, and irreversible migrations without a proven recovery path.

## Review workflow

- Review your own diff continuously while implementing. Before declaring a change ready, read the complete final diff once for correctness, accidental scope, stale comments, duplicated logic, missing failure cases, and violations of the accepted ADRs/product direction.
- Treat that final once-over plus the required checks as the normal review path. Avoid spawning extra review agents or external review services for routine changes.
- CodeDriver usage is exhausted. Do not contact CodeDriver, request work from it, or make repository progress depend on it.
- CodeRabbit is unavailable because the operator has no remaining usage. Do not request `@coderabbitai review`, wait for CodeRabbit, or treat a stale CodeRabbit status as acceptance.
- For privilege-boundary, ownership or adoption, durable persistence and recovery, rollback or compensation, secret handling, concurrency or race-sensitive, destructive, or comparable high-risk changes, use an implementation-independent Codex agent to review the complete exact-head diff. Record its evidence-based verdict on the pull request.
- Do not request GitHub-hosted Codex reviews or mention `@codex review`. Use locally coordinated Codex review agents only when the repository's independent-review rule requires them or the human operator explicitly asks for them.
- A large diff alone does not require an independent agent review. Documentation, mechanical configuration, formatting, temporary verification work, focused tests, and ordinary typed refactors stay self-reviewed.
- Verify every automated finding against the current code and accepted ADRs. Prioritize demonstrated privilege, ownership, persistence, rollback, race, security, data-loss, and correctness failures.
- Ignore or explain away speculative style, blanket documentation, duplication, and refactoring suggestions that do not improve behavior or reduce a concrete maintenance risk.
- Use the `review-exempt` label or `[skip review]` in the title for temporary verification pull requests, mechanical configuration changes, and other changes where automated review would add little value.
