# Disposable autoscaling CI

This document is the product direction for SmolRunner. It supersedes the persistent personal-worker sequence in `PERSONAL_WORKER_ALPHA.md` and the rootless-Podman-first sequence in the older roadmap.

## Product outcome

An operator enrolls a repository and leaves SmolRunner running. For each eligible GitHub Actions job, SmolRunner admits capacity, creates one isolated worker on the operator's Mac, registers a just-in-time runner, waits for exactly one job, records the result, destroys the worker, and returns the reserved capacity. Crashes, reboots, stale GitHub registrations, and partial provisioning are reconciled automatically.

The first production path is:

```text
queued GitHub job assigned to the SmolRunner scale set
-> durable capacity reservation
-> fresh Lima/VZ VM
-> just-in-time GitHub runner registration
-> one job
-> terminal observation and bounded diagnostics
-> VM destruction and stale-runner cleanup
-> reservation release and scale to zero
```

GitHub Actions remains the scheduler and workflow language. SmolRunner does not interpret workflow steps or reimplement the runner protocol.

## Actual threat model

Repository content, workflow steps, actions, build scripts, dependencies, test code, and nested containers are hostile. This includes the operator's repositories and known open-source repositories. GitHub, the pinned official runner distribution, Lima, Apple Virtualization Framework, the pinned guest image, macOS, and SmolRunner's controller are trusted components whose security updates must be applied deliberately.

The protected assets are the Mac host, host credentials and personal data, other workers, other network services, the controller's GitHub App credential, and the availability of the host and surrounding network.

The realistic attacker goals are host escape, persistence after a job, theft of host or unrelated credentials, access to other workers, lateral movement to the LAN, abuse of outbound connectivity, and resource exhaustion. A job is allowed to corrupt its own guest completely. Destroying that guest is the recovery mechanism.

The first release does not promise protection from a vulnerability in macOS or Apple Virtualization Framework, a malicious pinned guest image or official runner distribution, a compromised SmolRunner controller, or secrets intentionally supplied to that job by its GitHub workflow. Those are explicit trusted-computing-base and workflow-policy risks, not facts SmolRunner can prove away.

## Smallest credible security boundary

The following controls are release blockers.

1. **One fresh VM per job.** Potentially hostile repository code never runs in the Mac host namespace or in a long-lived worker. The VM is destroyed after one job, timeout, cancellation, runner loss, or controller recovery.
2. **No host integration.** The VM has no host filesystem mount, SSH-agent forwarding, credential socket, dynamic port forwarding, host environment inheritance, or container-control socket. Lima plain mode is the preferred starting configuration because it disables mounts, dynamic forwarding, built-in containerd, guest agent, Rosetta, and SSH-agent forwarding. Any required static control channel is explicit and controller-owned.
3. **One just-in-time runner.** SmolRunner uses GitHub's supported runner scale-set and JIT/ephemeral interfaces with a unique runner identity. The runner receives at most one job. GitHub owns label matching and assignment; SmolRunner binds the actual assigned job from the scale-set lifecycle message before accepting terminal evidence. The official runner archive and guest template are pinned and verified outside the workload.
4. **Credentials stay in the control plane.** A least-privilege GitHub App credential is stored on the Mac. Only the short-lived JIT configuration needed by the one runner enters the VM. It is never logged, persisted in public journals, exposed in argv, or reused. Workflow-provided job secrets remain GitHub/workflow responsibility.
5. **Hostile-CI network policy.** Inbound connectivity is denied. Outbound internet needed for ordinary builds is allowed, while host, private, link-local, metadata, peer-worker, and controller networks are denied. The policy is enforced outside workload authority by a mature firewall or egress gateway. Connection/rate limits and project-specific exceptions follow after the base deny policy works.
6. **Hard resource and concurrency ceilings.** Admission reserves CPU, memory, disk, and one concurrency slot before provisioning. The VM has fixed CPU, RAM, and disk limits plus a wall deadline. A host-wide budget prevents concurrent workers from exhausting the Mac. Zero admitted jobs means zero running worker VMs.
7. **Durable lifecycle and recovery.** Every externally visible mutation has a durable attempt identity and checkpoint. Reconciliation is idempotent: it may finish cleanup, remove a stale runner, destroy an orphan VM, or retry a bounded provisioning failure, but never silently widens authority or starts a second worker for the same job.
8. **Unprivileged guest workload.** The runner user has no sudo or equivalent guest administrative authority. Nested containers, when enabled, are rootless inside the disposable VM. Guest compromise is expected; the VM boundary, network policy, credentials policy, resource limits, and destruction contain it.
9. **Bounded external diagnostics.** Runner lifecycle logs needed to diagnose ephemeral runners are copied to bounded controller-owned storage. Raw repository contents, environment dumps, job secrets, and arbitrary logs are not durable SmolRunner state.

These controls are stronger and simpler than treating a rootless host container as the primary hostile-code boundary. A container backend can remain available later for lower-risk or high-throughput work, but it is not the default hostile-CI backend.

## Existing work to keep

The following implementation is directly useful:

- canonical configuration, identities, public error vocabulary, and human/JSON report discipline;
- the durable personal-worker store, locking, atomic publication, recovery, revision, and queue-generation rules;
- queue admission, reservations, concurrency/resource policy, cancellation, and terminal tombstones;
- the broker tick and read models;
- Mac capacity observation and the Lima observation, ownership, lifecycle authority, and bounded command executor;
- GitHub workflow-job mapping and snapshot reconciliation foundations;
- typed execution receipts, journals, bounded subprocess handling, and privacy rules;
- the reviewed R01 modules and evidence as optional hardening or a future Linux/container backend.

Existing proofs do not become false because they leave the critical path. They stop blocking the first complete product.

## Work to drop or defer

Do not add more ELF, glibc, loader-cache, package-layout, account-authority, or descriptor-attestation slices merely to complete R01. Defer the remaining 40-class runtime-readiness proof, the host-rootless-Podman execution path, custom checked-out-source materialization, reusable writable build-output caches, and custom descendant/cgroup proof for hostile jobs.

Also defer multi-host placement, Kubernetes, cloud providers, public multi-tenancy, previews, deployment, retained workspaces, hot profile resizing, automatic self-update, and a dashboard until the single-Mac disposable path is dependable.

The initial cache policy is intentionally conservative: use GitHub Actions cache/artifact services and normal upstream dependency registries. Do not retain a writable Cargo target directory or equivalent compiled output across hostile jobs. Later cache work must separate non-authoritative dependency inputs from executable build outputs and have explicit poisoning and quota tests.

## Delegated mature components

SmolRunner deliberately delegates:

- workflow semantics, job routing, scale-set demand, message acknowledgement, JIT configuration, job tokens, and one-job runner behavior to GitHub Actions, the official [Runner Scale Set Client](https://github.com/actions/scaleset), and the official runner;
- VM isolation and lifecycle to Apple Virtualization Framework through Lima;
- guest bootstrapping to a pinned Ubuntu cloud image and cloud-init/provisioning;
- guest package/container behavior to Ubuntu and a rootless container engine inside the VM when required;
- signed Ubuntu Noble repositories to install the official runner's declared dependencies during
  template construction; this is a versioned update-stream policy, not a claim of byte-reproducible
  package output, and APT's automatic services/timers are stopped and masked before readiness so a
  published template or disposable clone cannot drift in the background;
- network enforcement to a reviewed host-side firewall/egress gateway or protected guest firewall configuration, rather than executable-by-executable network attestation;
- Mac service supervision to `launchd`;
- initial cache/artifact transport to GitHub Actions.

SmolRunner still owns the exact configuration of those components, admission, scoped credential acquisition, durable orchestration, drift handling, cleanup, and truthful reporting.

## Shortest milestones

### M1 — disposable-attempt control contract

Add the small durable lifecycle that joins one scale-set capacity claim, one capacity reservation,
one VM identity, one runner registration, and the actual GitHub job when GitHub assigns it. Clone
authorization and clone start are separate durable checkpoints so a controller crash cannot replay
creation. The pure reconciler never learns ownership from a VM that merely appears under the
selected name: an unbound `CloneStarted` attempt is recovery debt regardless of whether one
observation says present or absent, and cannot adopt, delete, release capacity, or accept late job
evidence. The private clone transaction holds the canonical store lock while it reopens the current
reservation and prepared-template generation, freshly proves the exact source ready and stopped
plus the target absent, and takes one live revision/claim/time-bound zero-capacity poll at the final
admission barrier. A returned message is durably published instead of consuming clone authority;
only a bounded idle result can reach the immediately following `CloneStarted` publication and
fixed command. An earlier idle long poll remains advisory. Tests prove one bounded clone can bind
only the finally reconfirmed post-command host observation. The identity is only a durable drift
token: every later mutation service must still freshly reconfirm matching descriptor-bound Lima
host evidence immediately before a command. A command failure or controller death before that
binding deliberately leaves unbound recovery debt and is never replayed. Automatic proof that a
controller-killed clone process is quiescent remains required before that debt can be cleaned.
Enforce host-wide concurrency and memory/disk ceilings before provisioning.

Acceptance: deterministic tests cover duplicate observations, crash-after-each-checkpoint,
cancellation, expiry, runner loss, orphan VM, stale GitHub runner, and scale-to-zero. The current
coordinator drives durable Scale Set reservation, cancellation, release, replay, the fresh
absent-target authorization checkpoint, and the live admission transaction for an already
`CloneAuthorized` attempt. Supervisor composition still needs to select and drive those steps. It
does not yet register or execute a GitHub runner and intentionally leaves an unbound ambiguous
clone as recovery debt.

### M2 — disposable Lima backend

Create, start, observe, and destroy a uniquely named pinned Lima/VZ VM with fixed resources and no host integrations. Provision the pinned runner and minimal build tools into the image/template, not from repository-controlled input.

Current construction input is checked in at `examples/lima/smolrunner-prepared-template.yaml`. Its exact bytes are part of the canonical prepared-template identity. Lima 2.2.0 owns cloud-init execution in plain mode; the fixed system provision downloads the pinned official runner, verifies its byte count and SHA-256 before extraction, runs that pinned archive's official dependency installer against Ubuntu's signed repositories, creates a locked non-sudo workload account, and emits a root-owned readiness marker. It consumes no repository input. Ubuntu 24.04 LTS remains the first production guest because the dated image is pinned and already exercised; moving to 26.04 is an update decision, not a prerequisite for the first end-to-end path.

The canonical source-generation document and pure recovery planner require revision-bound sealed
observations and produce only opaque advisory candidates before create, stop, or discard. Canonical
Unix persistence reuses the existing no-follow private store, canonical lock, staged file,
exact-successor validation, file and directory fsync, and restart recovery. The private runtime
supervisor now derives one exact source identity bound to the configured Lima program path and
required 2.2.0 version, atomically materializes only the checked-in Lima bytes into the private
store, binds the held store objects to the pathname handed to Lima, double-observes advisory
decisions, retains descriptor-held host identity for destructive commands, publishes the exact
Started checkpoint, and runs fixed `limactl start|stop|delete` argument vectors with empty
allowlisted environments and bounded deadlines while retaining that lock. It verifies the exact
Lima version before observation and again immediately before each mutation. Readiness delegates
Lima semantics back to that pinned binary: the supervisor compares `limactl validate --fill` for
the exact input with the complete realized `limactl list --all-fields` config across the guest
probes, re-observes the full final Lima state and guest identity, then verifies the pinned runner
marker, root-owned non-writable installation closure, exact file/entry manifest, account, and the
still-disabled automatic-update policy. The complete composite
observation has one 30-second freshness budget. The clone-attempt transaction composes that exact
stopped source with a durable acquired attempt, a fresh absent target, a live Scale Set veto, one
fixed clone-with-start command, and the finally reconfirmed descriptor-bound post-clone disk
identity while retaining the same canonical lock. Its live source persists a newly arrived message
before returning control. The preceding authorization transaction uses the same live veto and
freshly brackets an exact absent target before it publishes `CloneAuthorized`; it never runs the
clone command. Command completion intentionally does not make the target controller-ready; a
subsequent fresh complete target observation must advance registration. A Started operation whose
externally visible result is still ambiguous is never replayed. Orphan-process quiescence after
controller death, wiring the coordinator into the supervised loop, and the explicit physical-Mac
acceptance run remain before M2 is complete.

Acceptance: fake/injected executor tests first; then an explicit physical-Mac test proves create, boot, bounded control access, forced failure, destroy, orphan discovery, and zero residual VM/process/disk allocation outside the documented image cache.

### M3 — GitHub JIT job path

Integrate a pinned build of GitHub's Runner Scale Set Client behind a narrow local adapter. Add Keychain-backed GitHub App authentication, scale-set session recovery, current-capacity reporting, JIT configuration generation, exact runner/VM binding, actual-job lifecycle binding, one-job execution, stale-runner deletion, and bounded log collection. Its long-polling session avoids an inbound webhook and avoids recreating GitHub's assignment semantics in Rust.

The checked-in `tools/scaleset-bridge` foundation pins the official Go client and exposes an
empty-environment, stdin/stdout-only protocol for exact scale-set validation, long polling,
explicit post-persistence acknowledgement, JIT generation, and exact runner observation/removal.
It deliberately does not use the official convenience listener's acknowledge-before-handler
ordering. A private Rust adapter now starts that no-argument process with an empty environment,
reads the GitHub App key directly through macOS Security.framework, strictly decodes bounded
responses, maps validated statistics into the existing typed demand contract, and retains JIT
configuration only in a redacted guaranteed-zeroization value. Before reading the key it requires
the bridge at the one fixed root-owned, non-writable installation path with exact SHA-256 content,
and it reconfirms that held file around spawn. Every exchange has a finite deadline; malformed
decoded or semantic responses poison and terminate the session. The personal-Mac adapter fixes
advertised Scale Set capacity at one and derives its current zero-or-one availability from the
durable attempt catalog. Capacity derivation, the bounded long poll, and exact response
publication retain one canonical store lock, so a cooperating writer cannot consume the
advertised slot between those operations. A private persistence-before-ack inbox now binds the
exact bridge configuration identity, applies normalized events to exact capacity claims under the
canonical store lock, and reconciles GitHub's acquired subset before another message or lifecycle
mutation may proceed. Its one-transition coordinator covers poll, durable record, event application,
acknowledgement checkpoint, and acquired/unacquired capacity reconciliation with crash replay.
An acquired offer is now prebound to its exact GitHub job before clone authority exists; exact
runnerless cancellation can release it without manufacturing VM cleanup authority. A settled
zero-capacity poll is retained as short-lived advisory evidence only. It does not prove that
cancellation and capacity vetoes remain clear at a later mutation barrier. The clone transaction
therefore performs its own fresh zero-capacity poll: a newly arrived message is durably retained
before return, while only idle can authorize the next checkpoint.
The 30-second message-admission window is separate from the deterministic six-hour hard ceiling on
the resulting one-job attempt, so replay is exact without expiring ordinary builds after one poll.
If a controller outage outlives that hard ceiling, the exact redelivered message is deleted and
acknowledged without acquiring its available job; the expired local reservation then follows the
ordinary unprovisioned-release path instead of accepting work that can no longer start safely.
The acquisition deadline is sampled only after the durable acknowledgement checkpoint while the
canonical store lock still binds the exact message and catalog. If GitHub nevertheless confirms an
acquisition before that boundary, the durable job binding cannot enter unprovisioned release after
the deadline; capacity stays closed while the service polls until the exact upstream cancellation
settles the obligation.
Unacquired or pre-clone-canceled work completes its durable release and moves immediately into
bounded replay history, so repeated cancellations restore advertised capacity instead of filling
the active catalog.
A private guest-handoff foundation consumes the bridge's guaranteed-zeroization JIT value as one
secret standard-input line to a fixed template-bound guest launcher, without placing the value in
argv, the inherited environment, logs, Debug, serialization, or an ordinary copied `String`.
Secret-bearing command output is discarded at the host operating-system boundary rather than
captured and redacted. The
launcher sets only the official runner's JIT input and hosted-result switch immediately before
`exec`. The plan fixes the Lima shell, guest `sudo`, launcher, work directory, and empty inherited
environment, and binds the candidate to the exact
durable attempt, cloned VM identity, runner name, runner ID, and deadline. It intentionally exposes
no free-standing execution path. A private canonical-lock transaction now distinguishes proven
registration absence from observation failure, repeatedly rechecks the running clone's exact host
identity, resources, realized Lima isolation policy, guest identity, root-owned checksum-verified
runner closure and launcher, and disabled automatic updates, then durably records the GitHub
registration and a no-replay
runner-start checkpoint before consuming the one command. A discovered registration is persisted
for exact cleanup without issuing another JIT value; command failure leaves Started recovery debt
and cannot replay. A final retained host-object check withholds a successful receipt if the Lima
instance or disk path was rebound while the runner command was active. The initial transaction
deliberately retains the single controller lock while
the one-job runner command is active.

Terminal cleanup now has an observation-first canonical-lock transaction over the existing durable
`Destroying -> Deregistering -> Releasing -> Complete` graph. It compares the current descriptor-
bound VZ/disk identity before Lima deletion, requires both Lima and the host identity path to prove
absence, deletes only the exact service-assigned runner ID and prechosen name, and releases host
capacity only after both VM and runner are absent. Each tick performs at most one external deletion;
an ambiguous result leaves the cleanup phase unchanged so restart observes before retrying. A
complete attempt contributes zero host usage and is then retired into bounded replay history.
The private coordinator now has one phase dispatcher that drains persisted Scale Set messages first
and then selects exactly one existing same-lock transaction from the current durable phase. A bound
ready clone is freshly re-observed and checkpointed into `Registering` without replaying clone; a
fresh cancellation, lost-capacity observation, or attempt expiry checkpoints it into `Destroying`;
a final descriptor-held VM identity check precedes registration publication. A runner whose Started
marker is already durable is routed back to message observation rather than receiving another JIT
value. The long-running process wrapper, retry/circuit-breaker policy, orphan enumeration, operator
enrollment, and physical acceptance run remain, so this is not production capacity yet.

Acceptance: an enrolled test repository targets the SmolRunner scale-set label, queues a job, and receives its result without operator commands; the JIT runner cannot accept a second job and its credential is absent after VM destruction.

### M4 — hostile-CI network and nested-container policy

Enforce denial of host, LAN, link-local, metadata, control-plane, and peer-worker destinations while preserving DNS and ordinary outbound build access. Enable rootless nested containers inside the guest only after this policy and guest resource limits are verified.

Acceptance: ordinary clone/download/build/test fixtures pass; hostile fixtures cannot reach denied destinations, listen inbound, exceed resource ceilings, or leave a reachable process after teardown.

### M5 — supervised reconciliation and autoscaling

Add `smolrunner worker serve` under `launchd`: observe demand, reserve bounded capacity, advance attempts, back off transient failures, enforce circuit breakers and operator holds, clean orphans, reconcile after reboot, remove stale runners, and scale to zero. Status must explain current capacity, attempts, blockers, retries, and cleanup debt without exposing secrets.

The private supervisor loop now drives the existing one-transition dispatcher: durable progress
continues immediately, an idle poll receives a fixed delay, failures use bounded exponential
backoff, and repeated failures open a five-minute cool-down circuit that closes only after a
successful step. `launchd` remains responsible for process lifetime and crash restart. This loop is
not production-facing until enrolled configuration, operator holds, bounded status, and the
`worker serve`/launchd installation path consume it.

The enrollment boundary is one canonical, bounded, secret-free document. It binds the private
state root, Lima program/home/source instance, exact installed bridge digest, GitHub App identity
and Keychain reference, exact Scale Set/repository/labels, and the single worker's CPU, memory, and
disk reservation. The App private key remains in Keychain. Decoding delegates to the existing
bridge, current prepared-template, resource, consumer, clone, and runner constructors so the
service entry point cannot invent a weaker parallel configuration path.

The private service composition now consumes that enrollment and performs bounded joint recovery
of the template, attempt catalog, and Scale Set inbox before initialization. A fresh bridge session
restores the durable acknowledged-message cursor and re-fetches any pending message from GitHub;
its exact normalized event bundle must match the durable inbox before acknowledgement authority is
restored. One dedicated no-follow private lock permits only one process-lifetime bridge/controller
session; short durable mutations continue to use the canonical store lock. A missing inbox may be
created only beside the exact pristine catalog, so cursor loss cannot be spliced onto active or
historical attempt authority. Source-template readiness gates only new polling and clone execution.
An expired Reserved or CloneAuthorized attempt is checkpointed into unprovisioned release before
that template gate, so a permanently broken source cannot retain unused capacity. Registration, job
event handling, teardown, capacity release, and retirement continue even when that source is
refused or needs rebuilding, so a broken source cannot strand an existing hostile worker. The
single supervisor retry/circuit policy drives template maintenance and the production clock,
bounded executor, clone/runner runtimes, and durable coordinator without exposing a second command
path. `worker serve --enrollment <absolute-canonical-document>` is now the sole production entry
point; it boundedly loads the existing secret-free enrollment and then enters that supervisor.
The enrollment must be an owner-matched mode-0600 single-link regular file beneath an
owner-matched non-writable directory path; startup opens the path without following links and
requires stable descriptor, pathname, metadata, and bytes before accepting its authority.
The launchd installation/removal contract and comprehensive bounded status remain incomplete.

The first operator-control surface is deliberately small: a private empty marker under the
canonical store lock represents an admission hold. `worker admission hold` makes every demand poll
advertise zero capacity and vetoes both clone authorization and the final clone command before any
executor call. It does not pause lifecycle polling, job-result observation, VM destruction, stale
runner removal, capacity release, or recovery. `worker admission status` and `resume` use the same
versioned typed report for human and JSON output. A held idle poll is a successful advisory service
step paced by the normal idle delay, not a failure that can open the supervisor circuit.
Process-lifetime control remains delegated to `launchd`; this marker is not a second service
supervisor.

Acceptance: sleep/wake, controller kill, Mac reboot, network loss, GitHub outage, failed provisioning, stuck job, and failed teardown tests all converge automatically or stop behind a precise durable blocker.

### M6 — production acceptance and safe optimization

Run a known repository plus intentionally hostile fixtures repeatedly. Measure cold-start latency, queue-to-start time, RAM/disk ceilings, teardown time, and idle footprint. Only then optimize template cloning, image warming, polling, or safe dependency caches. Every optimization must preserve one-job disposal and the security boundary above.

## Decision rule

New proof or infrastructure belongs on the critical path only when it closes a realistic route from hostile CI to host compromise, persistence, secret theft, cross-worker access, dangerous network activity, resource exhaustion, or unrecoverable operation. If a mature boundary can own the property more simply, use it. The release criterion is an unattended, recoverable job lifecycle—not the number of host facts proven.

## Supported-interface basis

- GitHub [recommends ephemeral runners for autoscaling](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#ephemeral-runners-for-autoscaling) because each runner receives one job and can then be wiped.
- GitHub's [Runner Scale Set Client](https://github.com/actions/scaleset) is the supported standalone Go client for non-Kubernetes autoscalers. It supplies current assigned-job statistics, long-polling sessions, acknowledgement, and JIT configuration while leaving VM creation and destruction to SmolRunner.
- Lima [plain mode](https://lima-vm.io/docs/config/plain/) disables filesystem mounts, dynamic port forwarding, built-in containerd, the guest agent, Rosetta, and SSH-agent forwarding. SmolRunner still needs an independently enforced outbound network policy because Lima's default user-mode network exposes the host gateway to the guest.
