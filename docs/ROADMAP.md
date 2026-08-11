# Roadmap

SmolRunner's product goal is a **hot, autoscaling Linux build server on an operator-owned Mac where every individual execution is disposable**.

Trusted agents and humans stay outside the worker boundary. They use GitHub as the scheduler, workflow engine, status surface, and canonical job-log store. SmolRunner supplies bounded local compute underneath GitHub: it admits capacity, creates one fresh Linux VM for one job, runs the official GitHub Actions runner, destroys the worker, proves cleanup, and releases the reserved capacity.

The operator experience should converge on three properties:

- **hot** — reviewed VM images, toolchains, dependency inputs, compiler caches, and container-build caches stay locally reusable where their trust model permits it;
- **disposable** — checked-out source, writable build state, job credentials, services, processes, and uncertain cache writes die with the worker;
- **quiet** — queueing, capacity, crashes, stale registrations, orphan workers, retries, and cleanup normally reconcile without operator work.

The governing security boundary and end-to-end acceptance criteria are in [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md). Cache lifecycle and trust research belongs in [#21](https://github.com/teamleaderleo/smolrunner/issues/21). Dependency/backend selection is tracked in [#368](https://github.com/teamleaderleo/smolrunner/issues/368).

## Product boundary

GitHub Actions remains the orchestrator. SmolRunner does not need another workflow language, job scheduler, or generic agent execution protocol.

The first production backend remains Lima with Apple Virtualization Framework. The intended stack is deliberately composed from mature components:

```text
trusted agent / human
        |
        v
GitHub Actions
        |
        v
GitHub Runner Scale Set Client
        |
        v
SmolRunner
  admission + durable attempts + policy + recovery
        |
        v
Lima / Apple VZ
        |
        v
pinned Ubuntu worker clone
        |
        v
official one-job GitHub Actions runner
        |
        v
hostile build/test code
```

Prefer mature components for GitHub protocol semantics, VM isolation, guest OS, networking enforcement, compiler/dependency caching, container builds, and macOS service supervision. SmolRunner should own the Mac-specific policy and reconciliation layer that those components intentionally leave to the infrastructure provider.

## What SmolRunner should own

- exact host-wide CPU, memory, disk, wall-time, and concurrency admission;
- one durable attempt identity joining capacity, worker, GitHub runner, actual job, and cleanup;
- crash recovery across ambiguous GitHub and VM mutations;
- exact worker/resource ownership before mutation, deletion, reuse, or capacity release;
- trust classification for jobs and cache publication;
- network-policy intent for hostile CI;
- least-privilege credential boundaries and secret lifetime;
- cleanup proof, retry debt, holds, quarantine, and agent-readable status;
- backend-independent worker contracts so implementation details can evolve without changing durable lifecycle semantics.

## Useful foundation already built

- [x] Rust CLI with shared typed human/JSON reports.
- [x] Canonical configuration, host observations, ownership classifications, and public error vocabulary.
- [x] Bounded shell-free process execution with an empty explicit environment.
- [x] Crash-safe durable stores, atomic journals, recovery classification, revisions, and queue generations.
- [x] Typed personal-worker queue, admission, reservations, resource limits, cancellation, and terminal identities.
- [x] Mac capacity observation and Lima observation/lifecycle authority.
- [x] GitHub workflow-job mapping and snapshot reconciliation foundations.
- [x] Pure disposable-worker capacity and lifecycle reconciler on `main`.
- [x] Extensive optional Linux/rootless-Podman R01 prerequisite and closure evidence.

The R01 implementation remains useful defense-in-depth and future Linux/container work. No additional narrow R01 proof slice blocks the disposable VM product path.

## Adjacent operator lane — developer namespace and blank-Mac recovery

The disposable worker path is the production critical path. A separate operator-experience lane may make the **development machine itself reproducible** without turning persistent developer state into CI execution state. The governing design is [Developer project namespace and workstation recovery](PROJECT_WORKSPACES.md), tracked in [#372](https://github.com/teamleaderleo/smolrunner/issues/372).

The target experience is:

```text
replacement Mac
-> restore reviewed operator catalog
-> converge developer environment
-> smolrunner project enter PROJECT
-> work
```

This lane owns logical project names, a portable secret-free catalog, adoption of existing Mac checkouts, lazy repository materialization, one trusted persistent developer Lima environment, generation-based publication/recovery, and eventual blank-Mac convergence. It borrows Nix/OSTree-style prepare/prove/switch semantics plus SmolRunner's existing compare-and-swap and reconciliation discipline.

It must preserve the production boundary:

- persistent developer checkouts never mount into hostile CI workers;
- developer credentials never flow into disposable workers;
- persistent writable developer caches carry no verification authority merely by existing;
- exact source identity remains separate from materialization and execution identity;
- destructive project cleanup still requires exact ownership evidence and separate authority.

Initial sequence:

- [x] P1 design and canonical identity contract in `docs/PROJECT_WORKSPACES.md` and #372.
- [ ] P2 read-only catalog parsing, alias resolution, and explicit-root checkout discovery.
- [ ] P3 in-place adoption of existing `~/Projects` checkouts plus durable catalog generations and local-only-risk reporting.
- [ ] P4 one persistent developer Lima environment plus `project ensure` / `project enter` for a public repository.
- [ ] P5 reviewed blank-Mac convergence with eager essential projects, lazy remainder, and explicit credential blockers.

P2/P3 can progress as bounded read-only/durable-state work when they do not collide with disposable-worker milestones. P4/P5 must reuse mature host/VM/dependency tooling and may not divert the hostile-CI backend into a persistent worker model.

## Gate A — dependency and adapter boundary

Before broad M2-M4 implementation, finish the research in [#368](https://github.com/teamleaderleo/smolrunner/issues/368) far enough to keep the core from duplicating mature projects.

- [ ] Compare direct pinned `actions/scaleset` integration with GARM, GHA Outrunner, Graftery, and related autoscaler/provider contracts.
- [ ] Keep Lima/VZ as the first backend while defining a small backend-independent disposable-worker contract.
- [ ] Decide whether the one-time JIT secret can be delivered through direct runner execution over the existing Lima control path or needs a tiny exec-only guest launcher.
- [ ] Select or bound the M4 network-enforcement backend; keep network policy intent independent of that implementation.
- [ ] Record source/version/license implications for candidate dependencies before they become required distribution components.

This gate is meant to shrink custom code, not delay the first worker indefinitely. The default remains direct `actions/scaleset` + Lima/VZ unless research finds a clearly better boundary.

## Milestone 1 — durable disposable-attempt reconciliation

The pure reconciler now consumes the bounded Scale Set protocol and canonical durable attempt
catalog directly. Restart-at-every-checkpoint coverage exercises the real crash-safe Unix store.

- [x] Define a small phase graph from reservation through provisioning, registration, assignment, execution, teardown, deregistration, release, and completion.
- [x] Emit one idempotent next action per reconciliation tick.
- [x] Enforce concurrency, CPU, memory, and disk budgets before additional workers are admitted.
- [x] Model cancellation, expiry, reservation loss, runner loss, missing/orphan VM state, duplicate/out-of-order job events, and cleanup ordering.
- [x] Persist the attempt through the existing crash-safe state machinery with exact revisions and recovery semantics.
- [x] Bind an acquired Scale Set offer to its exact GitHub job before clone authority; runnerless pre-clone cancellation releases capacity without manufacturing VM cleanup authority.
- [ ] Add a live persistence-aware Scale Set clone-admission source; an earlier zero-capacity idle poll is advisory and cannot authorize a later mutation.
- [x] Complete and retire unacquired or pre-clone-canceled attempts one durable transition at a time so capacity returns and bounded replay history replaces active-state accumulation.
- [x] Split prechosen runner name from GitHub-assigned runner ID so ambiguous JIT registration can recover by stable identity.
- [x] Preserve Scale Set job identity and result as bounded protocol values instead of assuming narrower REST/enum forms.
- [x] Cover crash-after-every-checkpoint, exact late-event binding, refusal of unbound runnerless completion, unknown completion strings, exact stale-registration cleanup, and scale-to-zero in deterministic persistence tests.

**M1 acceptance:** killing the controller at every lifecycle boundary and restarting it never creates a second worker for the same attempt, loses owned cleanup debt, or releases capacity before cleanup is proven.

## Milestone 2 — prepared disposable Lima/VZ worker

Build a worker factory that makes **fresh writable state cheap**.

- [x] Distinguish absent, exact stopped, and exact ready workers so clone-with-start success and interrupted-clone cleanup are independently recoverable across controller crashes.
- [x] Define sealed fixed shell-free clone and force-delete Lima command plans with an empty explicit environment, fixed deadlines, and exact durable attempt/resource inputs; omit standalone start, and keep all execution private until the same-lock durable authority exists.
- [x] Persist the controller's observed-absent decision as versioned `CloneAuthorized` state before cloning; use clone-with-start and discard only a stopped partial clone inside that authorized creation window, avoiding Lima's create-or-start `start NAME` behavior. Schema-v1 attempts and the legacy `Provisioning` phase fail closed. The future mutation boundary must still revalidate sealed absence under the catalog lock.
- [x] Remove moving Ubuntu fallbacks from checked-in Lima inputs, pin one dated ARM64 cloud image by its published SHA-256, and bind every durable reservation and Lima command plan to an exact prepared-template digest. Template construction and post-clone provenance observation remain required before live execution.
- [x] Pin Lima 2.2.0 and the official Linux ARM64 runner 2.336.0 archive by its GitHub-published and independently verified SHA-256; derive one canonical prepared-template identity from that archive, the pinned guest image, the provisioning recipe/account split, and the fail-closed no-host-integration policy. Durable reservations and Lima command plans consume the typed identity.
- [x] Define the canonical prepared-template source-generation history and pure recovery planner: sealed observations are exact-generation/revision-bound, mutation candidates are opaque advisory values that require a second fresh private reconfirmation before future execution, started external operations are never blindly replayed, existing names remain protected, and incomplete construction becomes destroy-and-rebuild debt rather than adoption.
- [x] Persist prepared-template generation under the existing canonical Unix lock with strict current/staged recovery, exact-successor checks, cross-document recovery refusal, and a plan-plus-second-sealed-observation transition API that rejects VM command candidates.
- [ ] Produce a controller-owned prepared template containing the runner, required guest account separation, selected toolchains, and static provisioning inputs.
- [ ] Evaluate Ubuntu 26.04 ARM64 first where it simplifies VZ/vsock control; retain the final distro/version as an explicit reviewed input.
- [ ] Clone the stopped template into a unique attempt-bound worker through Lima's copy-on-write-capable clone path.
- [ ] Create, start, observe, stop, force-stop, and destroy the exact worker through bounded Lima commands.
- [ ] Use Lima plain mode or an equivalent exact configuration with no host mounts, SSH-agent forwarding, dynamic port forwarding, guest agent, Rosetta, or inherited host environment.
- [ ] Keep the Actions workload account distinct from any provisioning/admin account and remove sudo/equivalent authority from the workload identity.
- [ ] Apply exact CPU, memory, disk, and wall-time ceilings.
- [ ] Discover and destroy exactly owned orphan workers after controller crash or reboot.
- [ ] Invoke the official runner's supported warmup path where measurements show it improves ready-to-job latency.
- [ ] Prove the lifecycle first with injected/fake executors, then on the physical acceptance Mac.

**M2 acceptance:** a fresh worker clone reaches bounded controller-ready state quickly, has no Mac integration visible to the workload identity, and leaves no attempt-specific VM/process/disk allocation after deletion outside documented shared image/cache state.

## Milestone 3 — GitHub-native one-job execution

Make GitHub the normal control path for agents and humans.

- [ ] Store the least-privilege controller GitHub App credential in the Mac Keychain.
- [x] Read the pre-enrolled GitHub App credential directly through macOS Security.framework without
  a secret-bearing subprocess, argv, environment, or public execution record.
- [x] Refuse secret-bearing bridge startup unless the fixed protected installation path matches the
  reviewed SHA-256 identity, and bound every bridge operation with a finite deadline.
- [x] Integrate a pinned Runner Scale Set Client behind a narrow adapter for demand, sessions, acknowledgement, JIT generation, job-start, and job-completion observations.
  - [x] Pin the official Go client behind a bounded empty-environment bridge whose messages require
    an explicit post-persistence acknowledgement.
  - [x] Persist each normalized message before acknowledgement, apply its events under the canonical
    store lock, and reconcile the exact acquired subset before admitting another message.
- [x] Validate every upstream response before it can advance durable state; malformed or internally inconsistent responses retain the prior attempt and fail closed.
- [ ] Persist a unique runner name before JIT creation; bind the service-assigned runner ID only after exact scale-set identity is observed.
- [ ] Transfer JIT configuration without argv, public logs, public journals, reusable guest storage, or a long-lived parent environment carrying the secret.
- [ ] Prefer direct execution of the official `Runner.Listener`; add only a tiny bounded exec-only guest launcher if the existing control path cannot deliver the secret cleanly.
- [ ] Bind the actual started job identity to the exact runner and VM rather than inferring assignment from demand.
- [ ] Run the pinned official runner for exactly one job and collect only bounded external lifecycle diagnostics.
- [ ] Observe terminal GitHub evidence, destroy the VM, remove stale runner registration, and release capacity automatically.
- [ ] Demonstrate the complete benign path against an enrolled test repository without routine operator commands.

**M3 acceptance:** an agent can submit ordinary GitHub work and receive the normal GitHub result while SmolRunner automatically creates and removes the required local worker.

## Milestone 4 — hostile-CI credential and network boundary

M3 proves functionality. M4 makes arbitrary repository code an intended workload.

- [ ] Keep durable agent/API credentials in the trusted agent/control plane and outside workers entirely.
- [ ] Give each worker only the short-lived JIT/Actions authority required for its exact job; assume every credential visible in the guest may be stolen.
- [ ] Deny inbound access and outbound Mac host, private/LAN, link-local, metadata, controller, and peer-worker destinations outside explicit workload authority.
- [ ] Preserve explicit DNS and ordinary outbound source/package/build access without depending on or exposing the Mac's local resolver state.
- [ ] Add bounded connection/rate/byte policy where the selected mature enforcement backend supports it.
- [ ] Allow explicit project network exceptions as reviewed policy rather than arbitrary workflow expansion.
- [ ] Enable rootless nested containers inside the disposable VM for container actions, service containers, and ordinary container builds.
- [ ] Verify benign CI plus hostile credential, network, process, disk, CPU, memory, nested-container, and persistence fixtures.

**M4 acceptance:** complete compromise of the guest yields no durable Mac/agent credential, useful host persistence, LAN/controller/peer-worker reachability, or reusable execution state after worker destruction.

## Milestone 5 — supervised autoscaling and recovery

Make the service disappear from the operator's attention during normal use.

- [ ] Add `smolrunner worker serve` as a bounded reconciler supervised by `launchd`.
- [ ] Run scale-set long polling with durable acknowledgement/session recovery; keep the first path outbound-only rather than adding an inbound webhook.
- [ ] Scale within exact host-wide resource budgets and return to zero running workers when idle.
- [ ] Let multiple trusted agents submit work concurrently without giving them direct host/VM control.
- [ ] Add backoff, retry budgets, circuit breakers, operator holds, quarantine, and precise status/remediation.
- [ ] Reconcile controller kill, sleep/wake, reboot, GitHub outage, failed provisioning, lost runner, stuck job, failed teardown, and partial stale-registration cleanup.
- [ ] Keep secrets, arbitrary job logs, and raw repository data out of durable SmolRunner diagnostics.

**M5 acceptance:** agents may stop paying attention after submitting work; queueing and host contention may delay execution, but ordinary completion and failure converge without routine operator cleanup.

## Milestone 6 — hot disposable performance

Recover as much of a persistent runner's speed as possible without carrying its compromise persistence forward.

Optimization rule:

> Keep state across jobs only when reusing it does not make compromise recovery materially harder.

- [ ] Measure queue-to-start, VM clone/create, boot-to-control-ready, JIT-to-job-start, job overhead, teardown, RAM/disk, failure convergence, and idle footprint.
- [ ] Benchmark representative Maven, Node/npm/pnpm, Rust, container-build, Git, and browser workloads on native macOS versus cold and prepared Linux/VZ workers on the same Mac.
- [ ] Keep reviewed toolchains and common static dependencies inside a pinned prepared VM template where rebuild cost justifies it.
- [ ] Use GitHub Actions cache/artifacts as the first cross-job dependency/cache path.
- [ ] Evaluate `sccache` before retaining whole writable compiler output trees; use read-only consumption for lower-trust jobs where appropriate.
- [ ] Use BuildKit's GitHub Actions cache backend for container-layer reuse before introducing a host cache service.
- [ ] Separate cache **consumer** authority from **publisher** authority so low-trust jobs cannot poison reusable cache state for trusted work.
- [ ] Namespace and quota any host-local cache that later proves worthwhile; record cold/warm state and cache identity in performance evidence.
- [ ] Keep checkouts, mutable build trees, temporary services, job credentials, and uncertain cache writes in the disposable worker layer.
- [ ] Add cache-poisoning fixtures and prove a suspected-compromise response can discard the affected writable/cache generation and continue from known reusable state.

**M6 acceptance:** repeated common builds approach warm-runner latency while the operator can still respond to suspected worker compromise primarily by discarding attempt-specific state rather than rebuilding the Mac or a pet runner.

## Milestone 7 — trusted-agent build-farm experience

Turn the implementation into the simple experience that motivated it.

- [ ] Make ordinary repository-owned GitHub workflows the default agent interface: push/PR/dispatch, wait for GitHub result, continue work.
- [ ] Provide a narrow optional path for testing reputable external open-source repositories without granting their workflow YAML infrastructure authority; the trusted harness may check out an exact repository/commit and run an explicit reviewed command inside the same hostile worker boundary.
- [ ] Keep agent identities/API keys outside workers and avoid direct SSH/shell authority to SmolRunner hosts as a routine interface.
- [ ] Report capacity, queue delay, worker phase, blockers, retries, cleanup debt, and result through stable human/JSON status suitable for unattended agents.
- [ ] Measure multiple concurrent agent workloads and tune resource classes/admission from actual host behavior rather than fixed runner counts alone.
- [ ] Preserve backend and capability identity so later experiments such as GPU-capable Linux workers or additional Mac VM engines do not rewrite the core lifecycle.

**M7 acceptance:** the Mac behaves like a small private build farm: trusted agents can continuously submit varied work through GitHub, useful build inputs remain hot, hostile execution stays disposable, and the operator usually forgets the runner service exists.

## Suspected-compromise recovery target

Security exists primarily to make recovery cheap and boring.

For a dependency-worm or hostile-repository event, the desired routine response is:

```text
mark attempt/cache generation suspect
-> stop admitting derived reusable writes
-> destroy the worker
-> remove stale GitHub runner state
-> allow job-scoped authority to expire or revoke narrow controller authority if evidence requires it
-> discard affected writable/cache generation
-> continue from pinned template + trusted reusable inputs
```

The Mac host, trusted-agent credentials, unrelated repositories, LAN, and future workers should remain outside the blast radius. A worker compromise may fail jobs or invalidate a bounded cache generation; it should rarely imply rebuilding the operator machine.

## Deferred

- Completion of the custom 40-class R01 runtime-readiness graph and host-rootless-Podman hostile-code backend.
- Persistent workers and retained writable workspaces as the default execution model.
- Shared authoritative compiled-output trees without a proven trust/content/producer model.
- Linux fleet stewardship as the primary product path.
- Multi-host selection, cloud providers, Kubernetes, and public multi-tenancy.
- Previews, routing, deployment, and production credentials.
- Automatic self-update, broad fleet policy, dashboard, and broad automatic repair.
- A custom cache server, CAS, image registry, VM monitor, firewall implementation, or workflow scheduler before measurements demonstrate a missing mature component.

## Non-goals

- Replacing GitHub Actions workflow YAML or the official runner protocol.
- Giving agents unrestricted host or worker shell authority as the normal submission path.
- Building a custom hypervisor, container runtime, package manager, init system, or service supervisor.
- Building another general-purpose CI scheduler or Kubernetes-like placement system for the single-Mac product.
- Weakening the isolation or credential boundary merely to produce a demo.
- Treating the operator's repositories or reputable open-source repositories as inherently safe code.
- Treating successful verification as deployment authority.
- Treating worker disposal alone as a defense for long-lived credentials intentionally exposed to hostile code.
