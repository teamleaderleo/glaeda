# General compute runtime

Glaeda is a general execution runtime for compute workloads on operator-controlled machines and fleets.

Coding agents and GitHub Actions are major current workloads and excellent proving grounds. They exercise demanding combinations of isolation, hot state, repeated execution, recovery, capacity pressure, and low-latency feedback. They are one family of consumers of a broader compute runtime.

The top-level product test is:

> **Glaeda finds the quickest trustworthy path from declared work to useful compute results while preserving exact ownership, recovery, and reusable state.**

## The common execution loop

A workload arrives with typed identity, trust, inputs, capability requirements, resource requirements, and an output/evidence contract.

```text
work becomes known
-> identify exact workload intent and inputs
-> identify trust and requested capabilities
-> find eligible compute and the hottest valid reusable state
-> admit capacity
-> materialize only the state that must change
-> execute
-> return useful outputs and evidence
-> retain, reset, migrate, quarantine, or destroy physical state
-> recover safely across interruption
```

This loop should serve many workload families:

- CI, build, test, packaging, and release preparation;
- coding-agent and interactive development work;
- data transformation, indexing, and research jobs;
- simulations and numerical batch computation;
- rendering and media processing;
- model evaluation, preprocessing, training, and inference where hardware and policy permit;
- trusted long-lived services and interactive compute;
- scheduled and background jobs;
- future workloads that should consume Glaeda without adopting repository, GitHub, or coding-agent semantics.

## What the runtime owns

Glaeda owns the compute-side decisions shared across workload families:

- exact workload and execution-attempt identity;
- trust classification and capability admission;
- capacity ownership and contention accounting;
- backend and host eligibility;
- hot-state validity, admission, reset, and reuse;
- execution isolation compatibility;
- lifecycle, quiescence, supersession, and settlement;
- durable recovery and no-replay behavior;
- bounded performance observations and placement decisions;
- physical-state retention, migration, quarantine, and destruction policy.

Workload adapters own domain semantics. A repository-verification adapter can understand commits, test profiles, commands, and verification evidence. A data adapter can understand datasets and transforms. A model adapter can understand model, data, runtime, and accelerator identities. A rendering adapter can understand scenes, assets, renderer generations, and output contracts.

The common runtime should consume exact typed facts from those adapters without assuming that every workload has a repository, PR, test suite, agent, or GitHub job.

## Generic workload seam

The first generic seam should stay small and bounded. A conceptual model is:

```text
ComputeWorkloadIdentity
  workload family
  semantic generation / intent generation
  exact input identity
  trust class
  capability requirements
  resource envelope
  output/evidence contract

ExecutionAttempt
  request / attempt generation
  capacity claim
  selected host/backend
  admitted reusable-state capabilities
  isolation compatibility
  lifecycle / settlement state
```

The exact Rust names can evolve. The semantic split is the important part:

- workload identity describes **what useful computation is requested**;
- execution attempt describes **one physical attempt to perform it**;
- result authority comes from the workload contract;
- physical execution authority comes from runtime admission and ownership.

A successful process exit alone carries only the meaning granted by its workload adapter. Verification evidence, rendered artifacts, datasets, model outputs, benchmarks, and service state each have their own acceptance rules.

## Trust and residency

The existing rule generalizes directly:

> **Disposable is a capability. Trust decides residency.**

Trust controls which physical state can remain hot and which capabilities a workload receives.

Examples:

```text
hostile / unknown workload
-> fresh isolated execution state
-> bounded capability set
-> terminal output
-> exact teardown / absence evidence

trusted repeatable workload
-> reviewed immutable or family-owned reusable state
-> bounded mutable execution state
-> measured retention where useful

ultra-trusted resident workload
-> long-lived compute context
-> exact lease / generation / validity rules
-> retained mutable state and services
-> explicit reset, revalidation, migration, and eviction
```

The same model can serve a hostile CI job, a trusted batch transform, a resident research environment, an interactive coding session, or a long-running local service.

## Hot state is a general compute primitive

Hot state includes every expensive reusable input or intermediate whose validity can be proven exactly enough for its consumer.

Examples include:

- immutable datasets and derived representations;
- compiler and package state;
- model weights and preprocessed shards;
- renderer caches and compiled assets;
- repository object stores and checkouts;
- indexes and search databases;
- prepared VM/container generations;
- resident services;
- task-local mutable views;
- completed reusable computation whose result contract permits reuse.

Each family owns its own validity proof. Glaeda supplies the common admission, lifecycle, accounting, isolation, and placement machinery around those proofs.

## Compute resources

CPU, memory, storage, PIDs, network, accelerators, and backend-specific capabilities belong in the resource/capability model as the implementation grows.

A workload can request only what its adapter and policy allow. The runtime admits work against exact current capacity and preserves resource ownership until the responsible lifecycle owner proves release.

The product-neutral capacity direction in #764 is the right foundation: disposable workers, resident environments, nested task execution, and future workload families should compose against shared host/fleet budgets.

## Universal performance goals

Agent edit/test latency remains an important benchmark. The common runtime optimizes broader compute outcomes:

```text
request -> first useful compute
request -> first useful output
request -> final accepted result
warm-state reuse benefit
queueing and contention cost
recovery time after interruption
throughput per host / fleet / resource budget
CPU / RAM / storage / network / accelerator occupancy
```

Each workload family may add stronger domain-specific metrics:

- coding workloads: edit-to-test, edit-to-build, task completion;
- batch transforms: records/bytes per second and completion latency;
- rendering: frame/scene completion latency;
- model workloads: token/sample throughput, latency, memory residency;
- services: readiness, request latency, idle residency cost.

Glaeda should optimize complete useful-result paths instead of isolated backend microbenchmarks.

## Backend role

Backends are mechanisms competing behind capability and evidence contracts.

Current and future candidates can include:

- Lima/VZ;
- Apple container / Containerization;
- native Linux hosts;
- operator-owned fleet nodes;
- VM/container backends;
- accelerator-equipped hosts;
- selected remote or burst compute where policy permits.

A better backend should improve Glaeda without redefining the workload, trust, ownership, recovery, or hot-state model.

## Current coupling audit

Two existing seams deserve early attention before broader implementation:

1. `ExecutionAdmissionIdentity` is broadly named while currently embedding `VerificationProfileId` and `RunnerProfileId`. Reservation, capacity, queueing, draining, and lifecycle semantics are substantially more general than verification.
2. hot-state admission has reusable family/binding/lease/capability concepts while `HotStateAdmissionTarget` currently embeds `ProjectIdentity` and its semantic inputs assume source/toolchain/profile/validator language.

The preferred direction is a narrow generic workload/owner identity above those kernels, with repository verification mapped through an adapter. Durable/versioned identities stay unchanged until their owning migration explicitly introduces a successor.

## First implementation proof

Before broad refactoring, prove the generic seam with two consumers:

### Repository verification

Bind the generic workload identity to the existing exact repository, source, verification profile, command, trust, and result-evidence identities. Existing correctness semantics remain intact.

### Non-repository compute

Use one bounded example such as an immutable dataset transform:

```text
family: dataset_transform
input generation: exact dataset digest
transform generation: exact reviewed transform identity
runtime generation: exact tool/runtime identity
resources: bounded CPU/RAM/storage
result: immutable output digest + workload-specific validation evidence
```

Run both through the same generic admission/capacity/attempt vocabulary. This demonstrates that the execution kernel serves compute directly while each adapter retains its own semantic authority.

## Boundary

Glaeda should continue to reuse mature domain engines and runtimes. Build systems, data engines, model frameworks, renderers, workflow languages, hypervisors, container runtimes, and schedulers remain integrations unless concrete product evidence justifies deeper ownership.

The durable kernel stays small and exact. Rich physical state can be large, hot, valuable, and replaceable. Losing hot state costs compute and latency; durable ownership and recovery still tell Glaeda what may safely happen next.

## Near-term consequences

Work under #770 should:

1. update root/current product prose so compute is the outer domain;
2. retain agent- and GitHub-specific metrics in the workload sections where they belong;
3. audit general execution/admission/capacity/isolation/hot-state/lifecycle types for accidental repository or verification coupling;
4. specify one bounded workload identity/intent seam before broad type renames;
5. prove repository verification plus one non-repository workload through that seam;
6. preserve exact old schema, digest, receipt, runtime, and recovery identities until explicit successor work owns them.

Related direction: #750, #764, #765, #761, #762, #769, #547, #548.