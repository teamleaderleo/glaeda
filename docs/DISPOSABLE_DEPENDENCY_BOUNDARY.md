# Disposable runner dependency and adapter boundary

Status: decision candidate for [#368](https://github.com/teamleaderleo/smolrunner/issues/368), source-reviewed 2026-08-11.

SmolRunner should stay small by owning the lifecycle facts that make an operator-owned Mac safe to use as hostile CI capacity and delegating mature protocol, VM, OS, and packet-processing primitives to projects that already do those jobs well.

This note records the dependency boundary for M2–M4. It also records why the current direct `actions/scaleset` + Lima/VZ direction remains the default after comparing nearby runner managers and VM backends.

## Decision

1. **Keep SmolRunner as the durable lifecycle authority.** SmolRunner owns host-wide admission, attempt identity, job/runner/worker binding, crash recovery, cleanup authority, capacity release, trust policy, and bounded status/recovery debt.
2. **Keep the pinned official `actions/scaleset` client behind the checked-in bridge.** GitHub owns scale-set sessions, long polling, acknowledgement/acquisition semantics, JIT generation, and runner service APIs. SmolRunner persists and reconciles a polled message before explicitly acknowledging it.
3. **Keep Lima/VZ as the first disposable VM backend.** The core reasons in SmolRunner attempt/worker identities and consumes a narrow backend adapter. Lima-specific command and filesystem details remain private to that adapter.
4. **Use a tiny exec-only guest handoff for JIT configuration if direct runner startup would expose the secret in argv.** The helper receives one bounded one-time JIT value over a controller-only channel and starts the exact pinned runner. It is not a shell, scheduler, long-lived agent, or generic RPC service.
5. **Keep hostile-CI network policy intent backend-independent.** The required policy is inbound deny plus egress denial to the Mac host, private/LAN, link-local, metadata, controller, and peer-worker destinations while preserving explicit DNS and ordinary public CI traffic across IPv4 and IPv6. Tart/Softnet is a useful implementation reference and benchmark candidate, but it does not become a required dependency for the Lima backend now.
6. **Prefer mature caches and service supervision.** GitHub Actions cache/artifacts, `sccache`, BuildKit cache backends, and `launchd` should carry their existing responsibilities before SmolRunner grows another cache protocol, CAS, package manager, VM monitor, or supervisor.

The result is deliberately asymmetric: SmolRunner owns fewer mechanisms than the neighboring runner managers, while retaining stronger authority over the exact lifecycle facts that determine safe cleanup and resource release.

## Source/version/license matrix

Versions below are the source-review points used for this decision. Any component promoted into a required distribution dependency needs its own exact release/digest/license pin at that time.

| Component | Source reviewed | License observed | Useful responsibility | Decision |
|---|---|---|---|---|
| GitHub `actions/scaleset` | commit `cb0405b2d874500e75ae34eff8d582ab75956b45` | GitHub upstream | Runner Scale Set sessions, polling, acknowledgement/acquisition, JIT and runner APIs | **Direct dependency through the existing pinned bridge** |
| GARM | v0.2.1; scale-set support stable from v0.2.0 | Apache-2.0 | Full runner manager, providers, durable controller state, scale-to-zero | Borrow provider/recovery ideas; **do not make it SmolRunner's outer controller** |
| GHA Outrunner | v1.2.0 | MIT | Direct scale-set controller with small Docker/libvirt/Tart provisioners | Borrow the thin provisioner idea; keep SmolRunner lifecycle ownership |
| Graftery | `main` inspected 2026-08-11 | Apache-2.0 | Mac-native scale-set controller, Tart clones, prepared images, warm pools | Strong product/design prior art; currently too young to become a core dependency |
| Lima | v2.2.0 | Apache-2.0 | Apple VZ VM lifecycle, templates, cloning, guest provisioning/control | **First VM backend** |
| Tart | v2.35.0 | FSL-1.1-ALv2 | Apple VZ VM lifecycle, OCI image distribution, automation | Benchmark alternative; explicit licensing/product decision required before adoption |
| Softnet | v0.22.1 | FSL-1.1-ALv2 | Tart-specific userspace packet filtering with dynamic policy | M4 reference/benchmark candidate; gaps remain for SmolRunner's complete policy |
| Lume in Cua | Cua `main` inspected 2026-08-11 | Cua repository MIT | Higher-level Apple VZ VM management for macOS/Linux guests | Benchmark only if Lima exposes a material product gap; recheck package-level release/license before adoption |
| vfkit | v0.6.4 | Apache-2.0 | Low-level Apple Virtualization.framework VM runner | Keep as low-level fallback/research option; it would make SmolRunner own too much image/provisioning lifecycle |

### License consequence

Lima, GARM, Graftery, and vfkit are permissive Apache-2.0 projects; Outrunner is MIT. Tart and Softnet currently use the Functional Source License 1.1 with a future Apache-2.0 conversion for each covered release. That license is a real distribution/product boundary, so a switch that makes Tart/Softnet required must be a deliberate release decision rather than an incidental implementation choice.

## GitHub scale-set controller boundary

### Direct pinned `actions/scaleset`

This remains the preferred path and is already implemented as a foundation in `tools/scaleset-bridge`.

The bridge pins the official Go client and intentionally separates polling from acknowledgement:

```text
poll through official client
-> return one bounded normalized message to Rust
-> persist/reconcile under the SmolRunner durable lock
-> explicitly acknowledge/acquire the exact persisted message
```

That ordering preserves SmolRunner's recovery model. GitHub owns the wire/session semantics; SmolRunner owns the durable meaning of a message for local capacity.

The narrow M3 adapter should expose only these capabilities:

```text
validate_scale_set(exact enrolled identity)
poll() -> bounded unacknowledged message + service statistics
ack(exact persisted message, exact acquired request ids)
generate_jit(exact durable runner identity)
observe_runner(exact numeric id/name/scale-set identity)
remove_runner(exact observed runner identity)
```

Keychain access, durable attempt state, capacity reservation, worker creation, actual-job binding, JIT guest delivery, teardown, and capacity release stay outside the Go bridge.

### GARM

GARM is the strongest candidate if the goal were simply to acquire a complete runner manager. Its stable scale-set path already owns GitHub polling, JIT generation, providers, durable controller state, cleanup, and scale-to-zero. Its external provider boundary is useful prior art.

Putting GARM above SmolRunner would create two competing durable authorities: GARM's database/controller would own runner lifecycle while SmolRunner's catalog owns admission, worker identity, cleanup proof, and capacity release. Ambiguous registration or teardown would then require a cross-controller recovery protocol. That is exactly the seam SmolRunner's current attempt model was built to remove.

GARM agent mode also contains broader administrative capabilities, including persistent agent communication and remote-shell functionality. Those are useful in GARM's product and outside SmolRunner's intended authority surface.

**Decision:** borrow provider timeout/recovery ideas; keep the official Scale Set client directly under SmolRunner.

### GHA Outrunner

Outrunner demonstrates that the outer scale-set loop can stay compact while provisioning is delegated to a small backend. Its Docker, libvirt, and Tart implementations are useful evidence that SmolRunner should keep its VM adapter narrow.

Outrunner still owns the overall controller/provisioner lifecycle. Replacing SmolRunner's reconciler with it would discard the host-global reservation ledger and exact crash/replay joins already implemented.

**Decision:** borrow the small provisioner interface idea, especially for future backend benchmarks.

### Graftery

Graftery is the closest product comparison: Mac-native scale-set integration, a fresh Tart VM per job, JIT injection, content-keyed prepared images, warm pools, orphan cleanup, and scale-to-zero. Those are excellent references for M6 image preparation and latency work.

The project was still very young at this review point, so it is stronger as evidence for product direction than as a dependency SmolRunner should build around.

**Decision:** study prepared-image identity, warm-pool accounting, metrics, and orphan cleanup; retain SmolRunner's controller and Lima backend.

## Disposable VM backend boundary

The durable core should never infer ownership from a backend name or generic VM state. It should hand a backend an exact typed identity and receive exact observations back.

A useful backend-independent contract is conceptually:

```text
observe_template(template_identity)
prepare_template(exact prepared-template generation)

observe_worker(attempt_worker_identity)
create_and_start(exact authorized attempt, template_identity, resources, network_policy_identity)
destroy(exact owned worker observation)
list_orphan_candidates(controller_identity)
```

Important semantics live above the backend:

- creation requires a durable capacity reservation and a durable creation authorization;
- observations distinguish absence, exact owned stopped/ready state, unknown, and conflict;
- a stopped partial clone is cleanup debt only inside the exact authorized creation history;
- an existing same-name VM never becomes owned through discovery alone;
- deletion requires exact ownership evidence;
- capacity release follows proven VM/runner cleanup;
- retries/recovery are controller decisions, not hidden backend behavior.

### Lima/VZ — selected first backend

Lima already supplies the mature Apple VZ and guest lifecycle primitives SmolRunner needs, and M2 now has exact pinned template inputs, clone recovery, durable mutation checkpoints, same-lock execution, and ambiguous-outcome handling around it.

Lima plain mode also gives the project a useful starting point for disabling host mounts, forwarding, built-in containerd and guest-agent conveniences while retaining reviewed provisioning.

**Decision:** finish M2 and physical acceptance on Lima before benchmarking a replacement.

### Tart — benchmark alternative

Tart is highly optimized for automated Apple VZ workloads and pairs naturally with OCI-distributed prepared images. Nearby runner projects demonstrate strong CI ergonomics around it.

Its current FSL-1.1-ALv2 license and the large amount of accepted Lima-specific recovery work make an immediate switch expensive. Tart becomes compelling if M4 network enforcement or M6 latency measurements expose a Lima limitation that materially changes the product outcome.

**Decision:** retain as the first serious alternative benchmark, with an explicit license gate before becoming required.

### Lume

Lume provides a higher-level Apple VZ VM surface for macOS and Linux guests. It is closer to the abstraction SmolRunner would want than vfkit, but it currently brings less CI-specific lifecycle prior art than Lima or Tart.

**Decision:** benchmark only after a measured Lima gap. Pin and re-review the exact Lume package/release/license before any switch.

### vfkit

vfkit is a useful low-level Virtualization.framework tool and permissively licensed. Choosing it as the first backend would push image acquisition, provisioning, clone/snapshot conventions, guest control, and more recovery semantics into SmolRunner.

**Decision:** keep as a low-level fallback/research tool. Mature higher-level backends are a better default.

## Guest JIT control channel

GitHub's supported JIT startup documentation shows the encoded JIT configuration passed as a `--jitconfig` command-line argument to the runner startup path. SmolRunner's product boundary keeps one-time JIT material out of argv, public logs, durable public state, and reusable guest storage.

That makes a tiny controller-owned guest handoff appropriate unless the exact pinned runner exposes an equally narrow secret-input mechanism during implementation.

The helper should do exactly this:

```text
receive one bounded JIT configuration over a controller-only private channel
-> validate message framing/size and exact attempt/runner generation
-> make the secret available only to the exact pinned Runner.Listener startup path
-> start/exec that runner as the locked workload account
-> remove/scrub run-private secret material after handoff
```

Required properties:

- fixed executable identity and fixed runner target;
- no arbitrary shell, command, path, environment override, file transfer, or general remote execution;
- one attempt/runner generation per invocation;
- bounded input and output;
- controller-only transport, preferably the existing private Lima control path or a narrowly reviewed vsock path;
- secret absent from process-list-visible argv, inherited long-lived environment, durable SmolRunner documents, and reusable guest filesystem state;
- helper version/identity bound into the prepared-template identity.

A general guest agent would add authority the product does not need.

## Hostile-CI network enforcement

The policy contract belongs in SmolRunner even if enforcement changes backend later.

### Required policy intent

For a hostile disposable worker:

- deny unsolicited inbound traffic;
- deny the Mac host/controller;
- deny RFC1918/private and local LAN destinations;
- deny IPv4/IPv6 link-local destinations;
- deny cloud/container metadata destinations;
- deny peer workers;
- use explicit DNS without inheriting the Mac's local resolver state;
- allow ordinary public GitHub, package, source, browser, and build traffic unless project policy narrows it further;
- cover IPv4 and IPv6;
- fail closed when the enforcement controller/backend loses authority;
- support bounded project exceptions through reviewed policy, never workflow-supplied arbitrary firewall rules.

### Softnet source review

Softnet is interesting because it provides per-Tart-VM userspace packet filtering and a local Unix-socket JSON-RPC policy API. It can install default-deny IPv4 egress policy dynamically.

Its current defaults and product coupling leave several gaps for SmolRunner M4:

- it is Tart-specific;
- it uses FSL-1.1-ALv2;
- initialization requires elevated host authority before privilege drop;
- the documented dynamic policy is IPv4-focused;
- the default policy allows gateway access and incoming traffic, while SmolRunner requires explicit inbound denial and controller/LAN isolation;
- M4 still needs exact fail-closed restart/controller-loss evidence plus a complete IPv6 story.

**Decision:** keep Softnet as a source-reviewed reference and benchmark candidate. Continue looking for a Lima-compatible enforcement path that satisfies the same backend-independent policy. If Lima cannot meet the contract cleanly, benchmark Tart+Softnet as one combined backend option and make the license/distribution decision explicitly.

## Cache and acceleration boundary

M6 should begin with mature cache mechanisms:

1. GitHub Actions cache/artifacts for portable cross-job state;
2. `sccache` for compiler output where producer/consumer trust can be separated;
3. BuildKit's GitHub Actions cache backend for container layers;
4. prepared-template generations for reviewed static toolchains and dependencies.

Any later host-local writable cache needs an explicit namespace, quota, producer authority, consumer authority, generation identity, and discard path. Cache existence never becomes source or verification authority.

## Rejected attractive alternatives

### “Let GARM own GitHub and call SmolRunner as a provider”

This reduces custom GitHub code but creates split durable lifecycle ownership at the exact crash boundaries SmolRunner is designed to own. Keep the direct official client.

### “Use the official convenience listener unchanged”

Its handler/ack ordering does not give SmolRunner the persistence-before-ack boundary its durable reconciler expects. The checked-in bridge already provides explicit acknowledgement after persistence/reconciliation.

### “Switch to Tart now because nearby runner projects use it”

Current M2 Lima work already closes difficult clone/recovery/ownership seams. Switch only from measured evidence or an M4 capability gap, with the FSL distribution decision made first.

### “Build directly on vfkit”

That trades one dependency for a much larger custom image/provisioning/guest-lifecycle surface. SmolRunner should stay above mature VM lifecycle tools.

### “Add a general guest agent”

M3 needs one secret-bearing runner startup handoff and bounded observations. A general command channel creates broader host-to-guest execution authority without product value.

### “Write a generic firewall”

SmolRunner should define network intent and bind enforcement evidence. Packet filtering itself belongs to a mature backend when one satisfies the policy.

## Benchmark and acceptance probes before dependency switches

### VM backend probes

Run the same pinned ARM64 guest/template intent through Lima, Tart, and any serious Lume candidate:

- prepared-template build time;
- clone/create latency;
- boot-to-controller-ready latency;
- destroy latency;
- disk amplification per worker;
- idle host footprint;
- resource-limit fidelity;
- exact orphan discovery after controller kill/reboot;
- controller kill at every create/start/destroy checkpoint;
- same-name foreign VM protection;
- cleanup proof before capacity release.

vfkit needs separate accounting for the image/provisioning work it would force SmolRunner to own.

### JIT secrecy probes

For one generated JIT configuration, prove the secret is absent from:

- host and guest process listings/argv;
- inherited environment visible to unrelated guest processes;
- SmolRunner human/JSON output;
- durable attempt/template documents and journals;
- controller/bridge diagnostics;
- reusable guest files after the attempt;
- host temporary files and crash-recovery debris.

Kill the controller/helper/runner at each handoff point and require a bounded recovery result without silently reusing one-time JIT state.

### Network probes

From a hostile guest fixture verify:

- public GitHub/package/DNS access succeeds according to policy;
- Mac host/controller access fails;
- RFC1918/private LAN access fails;
- IPv4 and IPv6 link-local access fails;
- metadata endpoints fail;
- peer-worker access fails;
- unsolicited inbound access fails;
- IPv6 cannot bypass IPv4 policy;
- enforcement controller/backend restart or failure converges fail-closed;
- nested rootless containers cannot bypass the worker policy.

### Licensing/distribution gate

Before any Tart/Softnet dependency becomes required, record the exact covered release, license text, future-license date semantics, binary redistribution plan, and product-use compatibility. A benchmark dependency can remain optional while that decision is open.

## Upgrade policy

Every required external component should be pinned to an exact reviewed release/commit/digest and upgraded as an explicit generation change:

- `actions/scaleset` bridge source pin;
- Lima binary version;
- Ubuntu guest image digest;
- official Actions runner archive digest;
- prepared-template identity;
- future guest helper identity;
- future network-enforcement backend identity.

An upstream upgrade changes the reviewed dependency identity and triggers the appropriate integration/physical acceptance lane. Moving tags, moving guest images, ambient package-manager resolution, and opportunistic backend fallback never silently widen production authority.

## Sources inspected

Primary upstream sources inspected for this decision:

- GitHub `actions/scaleset`: <https://github.com/actions/scaleset/commit/cb0405b2d874500e75ae34eff8d582ab75956b45>
- GitHub JIT runner documentation: <https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/run-jit-config>
- GARM: <https://github.com/cloudbase/garm> and <https://github.com/cloudbase/garm/releases/tag/v0.2.1>
- GHA Outrunner: <https://github.com/NetzwindHQ/gha-outrunner> and <https://github.com/NetzwindHQ/gha-outrunner/releases/tag/v1.2.0>
- Graftery: <https://github.com/diranged/graftery>
- Lima: <https://github.com/lima-vm/lima/releases/tag/v2.2.0> and <https://lima-vm.io/docs/config/plain/>
- Tart: <https://github.com/cirruslabs/tart/releases/tag/2.35.0> and <https://github.com/cirruslabs/tart/blob/main/LICENSE.md>
- Softnet: <https://github.com/cirruslabs/softnet/releases/tag/0.22.1> and <https://github.com/cirruslabs/softnet>
- Lume/Cua: <https://github.com/trycua/cua/tree/main/libs/lume>
- vfkit: <https://github.com/crc-org/vfkit/releases/tag/v0.6.4>

## Completion signal for #368

This decision satisfies the research gate when accepted:

- direct `actions/scaleset` is selected over GARM/Outrunner/Graftery as the outer controller while preserving useful interface lessons from each;
- Lima/VZ remains the first backend behind a narrow worker adapter;
- the JIT handoff is narrowed to an exec-only secret-bearing helper when required by the runner's supported interface;
- M4 network policy intent is fixed independently of enforcement, with Softnet documented as a candidate plus explicit gaps;
- source/version/license implications are recorded;
- benchmark gates are defined before any backend switch.
