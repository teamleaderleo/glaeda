# Personal worker alpha contract

> **Superseded product direction.** This persistent-worker, local named-verification alpha is retained as design history and for reusable typed contracts. The current product path is the one-job disposable VM autoscaler in [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md). It must not block JIT registration, disposable execution, teardown, or supervised reconciliation.

This document defines the first supported SmolRunner product journey: one operator-owned Apple-silicon Mac running one persistent Lima worker for trusted, named verification profiles.

It is the user-facing contract for W01 through W04 of programme #233. Later implementation must preserve the command vocabulary, lifecycle distinctions, authority boundaries, persistence promises, and acceptance requirements defined here. Current implementation status is stated separately from the target alpha experience so documentation never presents planned behaviour as already available.

## Alpha outcome

The supported journey is:

```text
install SmolRunner
→ initialise durable personal-worker state
→ submit one named verification request from an immutable source
→ select or start the Lima work profile
→ prove guest and runner readiness
→ execute the checked-in verification profile
→ persist one bounded terminal result
→ explain the result and current machine state
→ return Lima to the applicable idle state while preserving caches
```

The routine operator sequence converges on:

```text
smolrunner worker init
smolrunner queue submit --profile smolrunner.required --repository .
smolrunner worker run-once
smolrunner status
```

`worker run-once` is deliberately bounded. One invocation observes accepted state, performs at most one accepted lifecycle or job action, re-observes the relevant authority, persists any exact result, and returns. When further progress requires fresh evidence, it returns a continuation. The operator or a later supervised service may invoke it again; the command does not hide an unbounded polling loop.

## Supported boundary

The first alpha supports:

- one operator-owned Apple-silicon Mac, with the M5 MacBook Air with 24 GiB unified memory as the physical acceptance machine;
- one persistent Lima instance, conventionally named `smolrunner`;
- Lima 2.1.1 or newer after explicit compatibility review;
- an ARM64 Ubuntu 24.04 guest using Apple Virtualization Framework through Lima `vz`;
- one dedicated Linux runner account and one job at a time;
- repository code executed only through reviewed verification profiles and bounded execution adapters;
- immutable repository, commit, Git tree, command, resource, cache, request, attempt, and receipt identities;
- human and JSON output derived from the same bounded typed reports.

The Lima instance uses persistent guest storage. The Mac home directory, SSH agent, browser data, password-manager sockets, personal Git configuration, cloud credentials, and unrelated host paths remain outside the guest. Repository code is treated as untrusted even when the repository itself is allowlisted.

The Linux runner-steward track remains the shared installation, ownership, durable-state, execution, and recovery foundation. The personal Mac worker is the first complete product journey built from that foundation, not a replacement for it.

## Resource and idle policy

The alpha uses one persistent Lima instance with three machine/profile states:

| Machine/profile state | Reviewed envelope | Meaning |
| --- | --- | --- |
| `stopped` | No VM runtime memory reservation | Durable host state and the Lima disk remain; guest caches persist. |
| `interactive` | 3 GiB memory, 4 vCPU | Lightweight operator use while the queue is empty and the worker is within the first idle interval. |
| `work` | 10 GiB memory, 8 vCPU | Builds, tests, and other accepted named verification work. |

The initial job concurrency is one. A job receives its own reviewed CPU, memory, PID, timeout, cache, workspace, and command envelope; the VM envelope is not an invitation to consume the entire guest.

Any eligible queued or active job requests `work`. When the queue is empty and no reservation remains:

1. after 10 minutes of accepted idle evidence, the desired profile becomes `interactive`;
2. after 30 total idle minutes, the desired profile becomes `stopped`.

New work cancels a pending downscale. SmolRunner never changes profile or stops the VM while a reservation is active, a job is active or uncertain, drain evidence is incomplete, or required observations are stale. Resource changes use a reviewed stop, edit, start, and verify sequence; the alpha makes no claim of hot memory resizing.

Stopping or changing profile preserves the persistent Lima disk and approved caches. It does not prove that cached source passed verification, and it does not authorise cache deletion.

Availability modes such as `active`, `away`, `off`, and `auto` belong to W06. Before that work lands, the run-once alpha follows the fixed queue and idle policy above and exposes blockers rather than inferring operator absence from a single noisy signal.

## Command contract

### Current exact surfaces at the W01 base

At W01 activation base `3aa7c8f0341c6dd9138f7b0b5f2e2470140430ae`, these commands are CLI-reachable:

```text
smolrunner doctor [--strict]
smolrunner plan [--file FILE]
smolrunner host plan [--file FILE] [--account-file FILE]
smolrunner host prepare [--file FILE] [--account-file FILE] [--confirm TOKEN]
smolrunner worker status --store-root ROOT
smolrunner queue list --store-root ROOT --revision REVISION --generation GENERATION
smolrunner queue submit --store-root ROOT --revision REVISION --generation GENERATION ...
smolrunner job show --store-root ROOT --revision REVISION --generation GENERATION REQUEST_ID
smolrunner job cancel --store-root ROOT --revision REVISION --generation GENERATION --cancelled-at TIME REQUEST_ID
```

These are exact and advanced surfaces. They require caller-supplied durable expectations and, for submission, complete source, resource, cache, and time evidence. They are valuable for agents, tests, recovery, and strict automation, but they are not yet the complete routine alpha journey.

### Target alpha surfaces

| Current exact command | Routine alpha command | Owning wave | Contract |
| --- | --- | --- | --- |
| No initialisation command | `smolrunner worker init` | W02/W04 | Explicitly create or report accepted durable personal-worker state. Repeated invocation is idempotent. |
| `smolrunner worker status --store-root ...` | `smolrunner status` | W02/W04 | Resolve accepted configuration and present one unified operator report. |
| `smolrunner worker status --store-root ...` | `smolrunner worker status --store-root ...` | Already reachable; retained as strict surface | Read one exact durable snapshot without platform observation or mutation. |
| `smolrunner queue submit` with complete exact flags | `smolrunner queue submit --profile PROFILE --repository PATH` | W02/W04 | Discover current state and immutable repository identity, then publish one exact request. Strict flags remain available. |
| `smolrunner queue list` with exact expectations | `smolrunner queue list` | W02/W04 | Read the current bounded queue; optional strict expectations preserve stale-snapshot detection. |
| `smolrunner job show` with exact expectations | `smolrunner job show REQUEST_ID` | W02/W04 | Read one current queued, active, or retained-terminal identity. |
| `smolrunner job cancel` with exact expectations and time | `smolrunner job cancel REQUEST_ID` | W02/W04 | Discover current state and time, then publish one exact cancellation without hiding conflict. |
| No orchestration command | `smolrunner worker run-once` | W03/W04 | Perform at most one accepted lifecycle or job action and return a bounded result or continuation. |

`smolrunner status` means the routine unified operator view: configuration identity, durable worker state, queue, active work, Lima state, runner readiness, latest terminal result, blockers, and suggested next action.

`smolrunner worker status` means a strict exact durable-snapshot view. It does not silently inspect Lima, processes, GitHub, the checkout, or the host. Implementations may retain it as an advanced command or alias, but they must not blur it with unified operator status.

### Reserved later commands

These names belong to W06 and are outside the first run-once alpha:

```text
smolrunner worker serve
smolrunner service install
smolrunner service status
smolrunner service stop
smolrunner service remove
smolrunner availability active
smolrunner availability away
smolrunner availability off
smolrunner availability auto
```

Ordinary alpha setup does not install a LaunchAgent, configure GitHub credentials, begin background polling, or enable automatic availability changes.

## Lifecycle vocabulary

Machine state, durable work state, command result, and terminal outcome are separate concepts.

### Durable work states

| Term | Definition | Source of authority |
| --- | --- | --- |
| `queued` | An exact accepted request is durable and has no active reservation. | Personal-worker durable store. |
| `reserved` | Exact capacity, cache, request, and generation evidence has been assigned, but execution has not begun. | Durable admission and reservation record. |
| `starting` | An accepted lifecycle or execution start is underway and requires fresh readiness evidence. | Durable transition plus exact platform observation. |
| `running` | The exact attempt is active under its reviewed execution envelope. | Durable attempt identity plus fresh runner/process evidence. |
| `draining` | No new work may enter while current work, cancellation, cleanup, or shutdown evidence is being completed. | Durable drain intent and acknowledgement evidence. |
| `terminal` | The request has one retained bounded completion or failure result and cannot be re-created under the same identity. | Durable terminal tombstone or receipt. |

A queued request is never described as running. A machine profile is never used as a job state. An uncertain interrupted attempt is never reported as passed.

### Run-once results

| Result | Meaning |
| --- | --- |
| `satisfied` | The accepted desired state already holds; no mutation was required. |
| `action_applied` | One exact accepted action completed and its result was re-observed or durably recorded. |
| `continuation` | Progress was made or an observation was gathered, but a later invocation must use fresh evidence before another action. |
| `blocked` | No safe action can proceed until a named dependency, approval, resource, recovery step, or operator decision is supplied. |
| `failed` | The attempted bounded action ended in a typed failure and no additional hidden retry is running. |

A continuation is not background work. A blocked result names the exact blocker and suggested safe next action. Automatic retries, where allowed at all, remain explicitly bounded by the checked-in verification profile and preserve source, scope, command, cache, and authority.

### Terminal outcomes

A terminal result distinguishes at least:

- successful completion;
- compile, link, or test failure from typed phase evidence;
- memory-pressure refusal before execution;
- corroborated memory exhaustion;
- timeout;
- runner loss;
- cancellation or drain completion;
- cleanup incomplete;
- diagnostic inconclusive;
- other bounded execution failure.

Signal 9 alone is not proof of memory exhaustion. A memory-exhausted result requires compatible trusted memory-event evidence for the exact attempt and process-group generation.

## Operator actions and bounded results

| Operator action | Required evidence | Possible bounded result |
| --- | --- | --- |
| Initialise | Accepted configuration identity; safe state root; no conflicting recovery state | Created, already initialised, blocked by unsafe/missing dependency, or failed. |
| Submit | Current durable revision/generation; immutable source; named profile; resource/cache/time evidence | Applied, exact duplicate, stale snapshot, changed-input conflict, capacity refusal, or failed. |
| Inspect status | Accepted configuration plus available typed worker, Lima, readiness, and terminal evidence | Unified report with blockers and suggested next action; unavailable inputs remain explicit. |
| Run once | Current durable state; bounded host/Lima/readiness observations; one accepted broker decision | Satisfied, one action applied, continuation, blocked, or failed. |
| Cancel | Current durable expectations; exact request identity; cancellation observation | Applied, exact duplicate, stale snapshot, not found, terminal conflict, or failed. |
| Change profile | Exact Lima identity; no active/uncertain work; accepted resource plan; fresh observation | Applied and verified, already satisfied, continuation, blocked, or failed. |
| Execute profile | Immutable source and command; reservation; readiness; workspace/cache receipts; execution envelope | Terminal result, continuation for fresh evidence, blocked, or failed. |
| Stop for idle | Empty queue; no reservation; complete drain; accepted cooldown; fresh Lima/runner evidence | Applied and verified, already stopped, continuation, blocked, or failed. |

Convenience commands may discover current state internally and may perform one bounded fresh-read retry for a genuine stale conflict. They never convert changed-input conflict into success or conceal the exact durable outcome.

## Named verification contract

Routine submission names a checked-in verification profile rather than accepting an arbitrary shell string. A profile binds:

- repository identity and immutable commit/tree;
- repository-owned command identity and digest;
- exact package, target, feature, toolchain, timeout, and capability scope where applicable;
- requested and applied CPU, memory, PID, concurrency, and guest-headroom policy;
- workspace and cache identity classes;
- clean/dirty and trigger policy;
- allowed lower-concurrency retry, normally none;
- bounded output, cleanup, and receipt policy.

Unknown profiles, aliases, moving refs, undeclared arguments, scope widening, resource widening, cache drift, workspace drift, or authority widening fail closed. SmolRunner orchestrates repository-owned commands; it does not invent a general build language.

## Persistence and recovery promises

The durable personal-worker store is the source of truth for queue generation, requests, cancellation, admission, reservation, active work, cache leases, profile intent, terminal identity, and recovery state.

The alpha promises:

- single-writer mutation with cooperative locking;
- exact revision and queue-generation expectations;
- canonical bounded documents and atomic publication;
- staged-successor recovery before a new mutation is evaluated;
- byte-stable exact replay;
- changed-input conflict under a reused identity;
- retained terminal identity that prevents accidental re-creation;
- no implicit initialisation by read or mutation commands;
- explicit corruption, unsafe-filesystem, busy, stale, and recovery blockers;
- interruption never becomes success without a durable terminal receipt;
- process or machine restart resumes from durable evidence rather than in-memory assumptions.

A Mac sleep, process crash, VM loss, or network interruption may leave an attempt uncertain. The next run observes current state, refuses unsafe reuse, completes bounded recovery or cleanup, and either continues the exact attempt contract or records a terminal interruption. It does not silently claim completion or create a replacement request with broader authority.

## Public failure and remediation classes

W01 P05 defines the final stable schema. The product contract requires coverage for:

- invalid or incompatible configuration;
- missing, unsafe, corrupt, busy, or recovery-required state;
- stale revision or queue generation;
- submission, replay, identity, cancellation, or capacity conflict;
- repository identity, source, or verification-profile refusal;
- Lima missing, stopped, changing, mismatched, unavailable, or stale;
- runner offline, starting, busy, draining, mismatched, or stale;
- insufficient host/guest capacity or memory pressure;
- execution, timeout, runner-loss, terminal-classification, or cleanup failure;
- credential, service, publication, signing, or release approval required;
- unsupported platform or incompatible binary/state/config schema.

Every public error has a stable machine code, concise bounded summary, retryability, optional suggested command, and optional dependency or approval class. Public output excludes raw operating-system prose and private diagnostic material.

## Privacy and credential boundary

Human output, JSON, receipts, errors, and debug representations must not expose:

- private host, guest, workspace, cache, state, runtime, or cgroup paths;
- arbitrary commands, arguments, environment variables, stdout, stderr, or kernel logs;
- credentials, tokens, SSH agents, browser data, password-manager material, cloud configuration, or GitHub secrets;
- unrelated process IDs, host facts, repository contents, cache contents, or machine inventory;
- generic executable, shell, mount, Lima, Podman, GitHub, credential, deployment, publication, or destructive-cleanup authority.

The first run-once alpha needs no GitHub credential. Source discovery is local and credentialless. GitHub observation, reconciliation, Keychain-backed authentication, runner registration, and service operation belong to separately reviewed later waves.

Public identities are bounded and purpose-specific. Private evidence may be retained internally only when required to prove ownership, source, process, filesystem, or cleanup state.

## Physical acceptance evidence

W04 completion requires both deterministic injected acceptance and one operator-approved physical run on the M5 MacBook Air. Code merge alone is insufficient.

The versioned public physical receipt records:

- exact SmolRunner commit and binary identity;
- binary target and schema versions;
- bounded host architecture and resource class, not unrelated machine inventory;
- Lima version and opaque instance identity digest;
- guest architecture, operating-system class, and accepted profile;
- configuration and durable state identities;
- repository, profile, request, reservation, attempt, command, commit, and tree identities;
- requested and applied resource/concurrency envelope;
- lifecycle actions and bounded timings;
- readiness evidence class;
- cache state and opaque identity, without paths or contents;
- terminal classification and evidence digest;
- replay, stale-conflict, interruption, restart, recovery, and privacy checks performed;
- final queue, reservation, worker, runner, Lima profile, and idle state;
- operator approval for consequential host actions performed during the run.

The receipt excludes private paths, repository contents, arbitrary logs, environment dumps, credentials, tokens, and unrelated machine facts.

Acceptance must demonstrate:

1. clean or explicitly reconciled configuration and state;
2. idempotent initialisation;
3. exact submission and exact replay;
4. changed-input and stale-snapshot conflict;
5. stopped-to-work transition and readiness;
6. named profile execution and durable terminal completion;
7. status projection explaining the result and next action;
8. interruption and restart recovery before and after durable publication;
9. cancellation race and cleanup evidence;
10. return to the accepted final idle state with cache identity preserved.

## Alpha non-goals

The first alpha does not provide:

- a general-purpose workflow language or multi-tenant scheduler;
- public-fork or other untrusted remote job admission;
- arbitrary host, guest, shell, executable, mount, or container authority;
- automatic GitHub webhook handling, queue polling, runner registration, or credential setup;
- `worker serve`, LaunchAgent installation, or automatic availability modes;
- automatic merge, deployment, spending, external publication, signing, or credential changes;
- preemption, hidden retries, unbounded polling, or silent authority/resource widening;
- host-home mounts, ambient credential propagation, cache deletion, or whole-VM destructive cleanup;
- proof that a warm cache is equivalent to a verification receipt;
- support for every repository without a checked-in reviewed verification contract;
- broad fleet/provider expansion before the personal-worker journey passes physical acceptance.

## Downstream compatibility requirements

P03 through P06 and W02 through W04 must use these distinctions consistently:

- **operator status** is the unified routine report; **exact worker snapshot** is a strict durable read;
- **work state**, **machine/profile state**, **run-once result**, and **terminal outcome** are separate types;
- **continuation** requests another bounded invocation with fresh evidence and never implies background polling;
- **blocked** identifies a concrete dependency or decision and never degrades to an indefinite wait;
- convenience discovery preserves exact replay, stale, conflict, and authority semantics;
- the host broker owns queue and lifecycle policy; Lima is a bounded executor;
- named profiles own repository command scope; SmolRunner does not accept generic commands;
- durable evidence, not process memory or GitHub event order, decides recovery and terminal identity.

P06 must cross-review its command sequence and JSON predicates against this document before either documentation lane merges. P03 and P05 may freeze their public schemas after that terminology gate; P04 consumes their accepted types.
