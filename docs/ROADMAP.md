# Roadmap

Glaeda's product goal is a **blazingly hot Linux execution substrate for coding agents and GitHub Actions on compute the operator controls**.

The core rule is:

> **Disposable is a capability, not a mandate. Trust decides residency.**

Glaeda should do every reusable, trustworthy piece of work before the next task asks for it. Hostile work receives a fresh isolated worker and exact teardown. Trusted CI can use prepared workers, repository seeds, reviewed caches, and warm pools. Ultra-trusted agent work can keep project sandboxes, incremental compiler state, package state, indexes, and selected services resident across edit/test/build loops.

The shared correctness requirement across all of those modes is durable execution truth: destroying physical state must preserve enough exact evidence to recover safely, and retaining physical state must never make correctness depend on that state surviving.

The operator experience should converge on four properties:

- **blazingly hot** — queue/edit-to-first-useful-command and edit-to-first-test-result approach the cost of the useful work itself;
- **trust-tiered** — worker lifetime, writable-state reuse, cache publication, credentials, and networking follow explicit trust policy;
- **recoverable** — VMs, sandboxes, workspaces, caches, indexes, and prepared materializations can disappear without losing execution truth;
- **quiet** — queueing, capacity, crashes, stale registrations, retries, cleanup, revalidation, and eviction normally converge without operator work.

The strict hostile-work lifecycle remains governed by [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md) and [#365](https://github.com/teamleaderleo/smolrunner/issues/365). The top-level hot execution direction is tracked in [#557](https://github.com/teamleaderleo/smolrunner/issues/557) and the destroyability/residency invariant in [#556](https://github.com/teamleaderleo/smolrunner/issues/556). Observations, adaptive placement, verification compilation, and diagnostics are tracked in [#21](https://github.com/teamleaderleo/smolrunner/issues/21), [#546](https://github.com/teamleaderleo/smolrunner/issues/546), [#547](https://github.com/teamleaderleo/smolrunner/issues/547), and [#548](https://github.com/teamleaderleo/smolrunner/issues/548).

## Product boundary

GitHub Actions remains the ordinary workflow scheduler, check/status surface, and canonical hosted job-log surface for GitHub jobs. Glaeda owns execution admission, trust policy, durable identity, local lifecycle, recovery, hot-state lifecycle, and measured execution decisions around that interface.

The first production host/backend remains an operator-owned Apple-silicon Mac running Lima with Apple Virtualization Framework and pinned ARM64 Linux guests. Linux carries the repository filesystem workload; macOS remains the trusted control plane.

The product intentionally supports two very different excellent paths:

```text
hostile / unknown work
    -> fresh prepared Linux worker
    -> one bounded job
    -> exact teardown
    -> prove absence

ultra-trusted agent work
    -> resident Linux project sandbox
    -> revalidate exact project/source/toolchain lease
    -> edit / test / inspect / build
    -> retain useful incremental state
```

Trusted CI sits between those poles and may combine prepared disposable workers with reviewed reusable generations, repository seeds, warm pools, compiler caches, and derived verification artifacts.

Prefer mature components for GitHub protocol semantics, VM isolation, guest OS, networking enforcement, filesystems, compiler/dependency caching, container builds, and macOS service supervision. Glaeda should own the policy and recovery layer that joins them.

## Durable execution kernel

Persist only the facts a restarted controller genuinely needs and cannot safely infer again, including where applicable:

- job / attempt / execution identity;
- capacity reservation and ownership generations;
- exact external mutation intent and restart-safe execution ownership;
- immutable template / toolchain / input generation identities;
- GitHub delivery, acquisition, runner, and no-replay identities;
- VM / sandbox / project-lease bindings required for reconciliation;
- terminal outcome and teardown receipts;
- causation / idempotency identities;
- explicit recovery debt when ownership or external state remains ambiguous.

The durable kernel stays bounded and non-executable. Workspaces, process lists, logs, arbitrary shell state, compiler trees, indexes, package stores, and resident services remain physical execution state with separately reviewed lifetimes.

## Trust-tiered hot state

### Tier 1 — hostile / unknown

Use the strict disposable contract:

- fresh isolated worker;
- one bounded job;
- no inherited mutable workspace;
- short-lived job authority only;
- reviewed immutable/read-only reusable generations where policy permits;
- exact runner/VM teardown and proven absence.

### Tier 2 — trusted CI

Allow much hotter preparation while preserving a clean job boundary:

- prepared worker generations and snapshots;
- local Git object/repository seeds;
- dependency and compiler cache generations;
- derived verification artifacts;
- deterministic test plans and shard plans;
- warm pools;
- overlapping admission, repository hydration, JIT preparation, and verification planning.

### Tier 3 — ultra-trusted agent/project work

Persistence becomes a first-class performance tool:

- resident project sandboxes;
- project-local and task-local worktrees;
- incremental compiler/build trees;
- language-server and code-search indexes;
- warmed package/dependency state;
- resident containers and selected development services;
- long-lived project-local caches;
- reusable checkout/object data;
- prewarmed verification/test daemons.

Residency requires exact project/lease identity, source state, toolchain generation, dirty/task-local state classification, credential/network capability generation, validity parents, and explicit reset/revalidation/expiry policy.

## Useful foundation already built

- [x] Rust CLI with shared typed human/JSON reports.
- [x] Canonical configuration, host observations, ownership classifications, and bounded public error vocabulary.
- [x] Bounded shell-free process execution with an empty explicit environment.
- [x] Crash-safe durable stores, atomic journals, recovery classification, revisions, and queue generations.
- [x] Mac capacity observation and Lima observation/lifecycle authority.
- [x] Durable disposable-worker capacity and lifecycle reconciliation.
- [x] Pinned prepared Lima/VZ worker inputs and exact template identity.
- [x] Physical prepared-template create/provision/ready/stop/delete acceptance on Apple silicon.
- [x] Official Runner Scale Set bridge, Keychain credential acquisition, durable delivery recovery, and JIT handoff composition.
- [x] LaunchAgent plan/apply/status and service supervision path.
- [x] Physical controller-death evidence and explicit recovery debt for in-flight Lima mutation gaps.
- [x] Trusted persistent Quarry runner lane with warm workspace/toolchain/cache retention.
- [x] Warm pause/resume and auto-idle controls for the trusted lane.
- [x] Recent security hardening around runner JIT secrecy, immutable readiness identity, Git filter execution, teardown authority, wrapper provenance, and exact elevated-command confirmation.

## Current sequencing

The strict disposable lifecycle remains the immediate production prerequisite for hostile work. Hot trusted execution should progress in parallel whenever it does not weaken that lane.

Current priorities:

1. complete the installed-service one-job disposable path and converge back to runner/VM absence plus zero reserved capacity;
2. close restart-safe ownership for in-flight Lima mutations and extend controller-death/reboot/sleep/outage recovery;
3. prove and implement the hostile-worker network boundary;
4. benchmark resident trusted execution as a first-class product path;
5. instrument queue/edit-to-useful-result latency and hot-state hit/miss/reset behavior;
6. evaluate Linux-native project storage, cheap task forks, repository seeds, compiler caches, and warm services from measured agent loops;
7. feed those measurements into adaptive execution placement, verification compilation, and diagnostics.

## Milestone 1 — durable disposable-attempt reconciliation

The durable reconciler establishes the correctness kernel for fresh one-job workers.

- [x] Durable attempt identity from reservation through provisioning, registration, assignment, execution, teardown, deregistration, release, and completion.
- [x] Exact resource budgets and no-duplicate capacity ownership.
- [x] Cancellation, expiry, runner loss, missing/orphan VM state, duplicate/out-of-order events, and cleanup ordering.
- [x] Crash-at-checkpoint persistence tests and exact stale-registration cleanup semantics.

**M1 acceptance:** restart at any durable lifecycle boundary preserves exact ownership and prevents duplicate worker authority or premature capacity release.

## Milestone 2 — prepared disposable Lima/VZ worker

Build a worker factory that makes fresh hostile-writable state cheap and exact.

- [x] Pin Lima, guest image, Actions runner archive, provisioning recipe, account separation, and no-host-integration policy into one prepared-template identity.
- [x] Bound clone/delete commands, exact resource inputs, filesystem-space admission, and post-command observation.
- [x] Physically prove prepared-template lifecycle and create-failure cleanup.
- [x] Keep repository code inside Linux with no host mounts, SSH-agent forwarding, proxy inheritance, Rosetta, or host filesystem sharing.
- [ ] Finish restart-safe ownership/quiescence for in-flight Lima mutations.
- [ ] Extend the physical crash matrix through clone/start/delete and ambiguous response cases.

**M2 acceptance:** a fresh worker reaches controller-ready state from exact prepared inputs and can be removed after any supported failure without adopting foreign state.

## Milestone 3 — GitHub-native one-job execution

Make ordinary GitHub demand the normal strict-disposable control path.

- [x] Pin the official Runner Scale Set client behind a bounded private bridge.
- [x] Keep long-lived GitHub App credentials in the Mac Keychain/control plane.
- [x] Durably consume assignment state and preserve explicit no-replay recovery.
- [x] Transfer one-time JIT material through the reviewed secret-safe guest launcher.
- [x] Compose clone, JIT, runner start, terminal teardown, and capacity release behind `worker serve`.
- [ ] Complete the installed LaunchAgent -> queued job -> disposable VM -> one job -> runner removal -> VM deletion -> zero-capacity physical journey.
- [ ] Extend reassignment/backlog and graceful shutdown behavior from physical evidence.

**M3 acceptance:** one ordinary GitHub job executes exactly once in one disposable Linux worker and converges to zero worker-specific state automatically.

## Milestone 4 — hostile-CI credential and network boundary

Make arbitrary repository code an intended workload.

- [x] Keep durable controller credentials outside the worker.
- [x] Harden JIT/runner process secrecy against same-UID workflow reads.
- [ ] Deny worker access to the Mac host, LAN/private ranges, link-local, metadata-style destinations, controller, and peer workers while preserving ordinary public build egress.
- [ ] Physically identify and prove the VZ networking enforcement point before selecting the policy backend.
- [ ] Add least-privilege nested-container behavior and hostile fixtures.
- [ ] Preserve exact teardown even when the guest mutates its own writable disk state.

**M4 acceptance:** complete guest compromise yields no durable Mac/agent credential, useful host persistence, controller/LAN/peer reachability, or reusable hostile writable state after teardown.

## Milestone 5 — supervised autoscaling and recovery

Make execution disappear from the operator's attention.

- [x] Bounded long-poll supervisor, pacing, retry budget, and circuit breaker.
- [x] LaunchAgent-supervised production service path.
- [x] Signal handling, exact service installation identity, and read-only status vocabulary.
- [x] First physical controller-SIGKILL proof.
- [ ] Restart-safe execution ownership for in-flight mutations.
- [ ] Sleep/wake, reboot, GitHub outage, failed provisioning, lost runner, stuck job, failed teardown, and stale-registration recovery.
- [ ] Bounded private diagnostic/failure receipts that preserve useful evidence without turning logs into authority.

**M5 acceptance:** ordinary success, failure, cancellation, host restart, and service restart converge with bounded operator attention and exact recovery semantics.

## Milestone 6 — blazingly hot execution

M6 is a primary product programme. The target is **agent wall-clock latency**, across both prepared-disposable and resident trusted execution.

Measure the full path:

```text
work becomes known
-> execution target / resident sandbox selected
-> capacity admitted
-> environment ready
-> repo/revision usable
-> dependency/build state usable
-> first useful command
-> first useful test/build result
-> final trustworthy result
-> teardown or residency transition
```

Headline metrics:

- queue-to-first-useful-command;
- edit-to-first-test-result;
- edit-to-final-relevant-verification;
- task completion wall time;
- fleet throughput under 1 / 2 / 4 / N concurrent agents;
- disk/RAM/CPU residency cost per active project;
- reset/invalidation frequency and time lost to cold reconstruction.

### M6A — hot execution environments

- [ ] Benchmark cold disposable, prepared disposable, warm-pool disposable, resident project after idle, immediate resident reuse, and resident task loop.
- [ ] Benchmark Lima/VZ clone/start and snapshot/fork alternatives using the same workload receipts.
- [ ] Evaluate Apple `container` and other mature local backends only where measurements show a credible win.
- [ ] Make project residency an explicit leased state with revalidate/reset/quarantine/retire semantics.
- [ ] Keep active ultra-trusted projects resident when expected reuse value exceeds idle resource cost.

### M6B — hot repository state

- [ ] Keep Git object/repository seeds locally reusable under exact project identity.
- [ ] Reuse persistent trusted project checkouts/worktrees under exact source and dirty-state rules.
- [ ] Explore shared read-only source/object bases with cheap task-local writable views.
- [ ] Hydrate/fetch missing objects while admission and environment preparation run in parallel.
- [ ] Benchmark 1 / 8 / 32 / N concurrent agent worktrees and branch/task forks.

### M6C — Linux storage and cheap task forks

The repository workload already runs inside Linux. Make that Linux storage layer an explicit performance lever.

- [ ] Establish the current guest filesystem/virtual-disk path as the baseline.
- [ ] Benchmark representative Git, pnpm/npm/Bun, Cargo, Maven, test-output, and cleanup workloads against XFS/reflink, XFS with a mature dedupe/compression layer where viable, and other credible Linux candidates.
- [ ] Measure small-file creation/deletion, hardlink/reflink behavior, worktree creation, package installs from warm stores, compiler-tree reuse, disk growth, and CPU overhead.
- [ ] Measure host-side Lima backing-file growth alongside guest-visible logical bytes.
- [ ] Evaluate one resident project volume plus cheap task-local forks against many fully independent worker clones.
- [ ] Keep the winning filesystem/storage choice behind a backend contract so execution semantics stay independent of one format.

### M6D — hot dependency/build state

- [ ] Make package-manager state, compiler caches, build outputs, container layers, and prepared dependency environments first-class typed hot-state classes.
- [ ] Benchmark `sccache`/`ccache` and repository-native incremental build state from real edit/test loops.
- [ ] Separate project-local mutable state from cross-job immutable generations.
- [ ] Track hit/miss, bytes, restore/build cost, observed time saved, last useful hit, and invalidation causes.
- [ ] Reclaim state that fails to earn its disk/RAM cost.

### M6E — hot services and indexes

- [ ] Keep language servers, test daemons/watchers, compiler servers, local fixtures, builders, and repository-specific services resident for ultra-trusted projects when their lifecycle is explicit.
- [ ] Preserve/revalidate language and code-search indexes across related tasks.
- [ ] Measure daemon/index warmup savings separately from repository and compiler-cache savings.

### M6F — overlap and anticipate

- [ ] Overlap target selection, capacity reservation, environment fork/materialization, repository hydration, cache/artifact selection, JIT preparation, and verification-plan compilation whenever their inputs permit it.
- [ ] Prefetch likely next inputs from explicit project activity and queue evidence within bounded policy.
- [ ] Expose which preparation hit, missed, reset, or ran in parallel for each execution.

**M6 acceptance:** active trusted agent loops feel like local development: common iterations begin with useful work almost immediately, repeated setup disappears, fleet throughput remains efficient, and every hot state family has a proven reset/cold-reconstruction path.

## Milestone 7 — adaptive agent execution runtime

Turn the hot execution substrate into a system that improves from repeated work.

- [ ] Use #21 observations to record bounded comparable-run timing/resource/cache evidence.
- [ ] Use #546 to select execution target, resource profile, concurrency, and included/owned/burst capacity from predicted completion cost.
- [ ] Use #547 to compile repeated verification work into exact reusable plans, derived artifacts, test selection, and deterministic partitioning.
- [ ] Use #548 to classify recurring failures, propose discriminating probes, rank remedies, and promote known recurring causes into preflight checks.
- [ ] Route trusted agent work toward already-hot project state when that beats a theoretically quicker cold machine.
- [ ] Explain every decision through stable human/JSON reports: residency, source validity, cache hits, reset reasons, expected first-test latency, and selected target.
- [ ] Preserve ordinary GitHub workflows as the default interface while allowing explicit trusted agent dispatch to consume routing/planning recommendations.

**M7 acceptance:** Glaeda behaves like an execution runtime that gets quicker and easier to operate as it sees the same projects repeatedly, while every optimization remains inspectable, reversible, and bounded by trust.

## Hotness hierarchy

Optimize in this order unless measurements say otherwise:

1. keep valuable trusted state resident;
2. remove avoidable semantic work;
3. reuse exact work already completed;
4. overlap independent preparation;
5. share immutable inputs;
6. parallelize genuinely independent work;
7. optimize hot kernels and storage behavior;
8. buy/rent additional compute.

A giant runner rebuilding the world every iteration is cold.

## Suspected-compromise recovery target

For hostile or compromised execution, the desired routine response remains small and exact:

```text
mark affected attempt/generation suspect
-> stop admitting derived reusable writes
-> destroy or quarantine the worker/sandbox
-> remove stale GitHub runner state where exact authority exists
-> revoke or expire narrow affected authority
-> discard affected reusable generation when policy requires it
-> continue from canonical durable truth + reviewed inputs
```

For ultra-trusted resident projects, ambiguity in project/source/toolchain/credential validity triggers revalidation, reset, quarantine, or destruction. Residency never gains authority merely by surviving.

## Deferred

- Public multi-tenancy and generic fleet management.
- A replacement workflow language or general CI scheduler.
- Cross-project writable-state sharing without an explicit portability and trust model.
- Broad autonomous source modification from diagnostic guesses.
- Custom hypervisors, container runtimes, package managers, filesystems, or cache servers before mature components fail a measured requirement.
- Deployment authority inferred from verification success.

## Non-goals

- Replacing GitHub Actions workflow YAML or the official runner protocol.
- Giving agents unrestricted Mac host authority as the normal submission path.
- Treating resident state, cache hits, process survival, filenames, PIDs, or directory presence as execution truth.
- Letting performance preferences weaken credential, network, ownership, teardown, or recovery gates.
- Treating one worker-lifetime policy as universally optimal.