# Personal worker alpha acceptance contract

> **Superseded acceptance path:** this persistent-worker acceptance plan is retained for its reusable lifecycle and failure fixtures. Current production acceptance is defined by [Disposable autoscaling CI](DISPOSABLE_AUTOSCALING_CI.md).

Status: W01 cross-reviewed contract candidate  
Schema: `personal_worker_acceptance/v1`  
Exact drafting base: `3aa7c8f0341c6dd9138f7b0b5f2e2470140430ae`  
Owners: P06 issue #249; product vocabulary cross-review with P01 issue #244

## Purpose

This document defines the evidence required to claim that the first personal-worker run-once alpha works. It is an acceptance contract, not an implementation design and not permission to mutate an operator machine.

The accepted journey starts with no personal-worker state, creates or discovers one durable worker, submits one immutable named verification request, advances it through bounded repeated `worker run-once` invocations, publishes one durable terminal result, explains that result through unified status, and reaches the accepted final idle state while persistent guest caches remain intact.

Code merge, a green unit test, or one successful command is insufficient by itself. Acceptance requires the deterministic injected matrix and one separately approved physical Mac receipt described below.

The normative words **MUST**, **MUST NOT**, **SHOULD**, and **MAY** are used as requirements.

## Scope and exclusions

The first alpha covers one operator-owned Apple-silicon Mac, one persistent Lima instance, one admitted job at a time, reviewed named verification profiles, bounded repeated run-once invocations, durable replay/recovery, and privacy-safe human and JSON reports.

The first alpha excludes:

- a continuously running broker or hidden polling loop;
- LaunchAgent or other service installation;
- GitHub credentials, webhook ingress, queue polling, or runner registration mutation;
- automatic availability-mode changes;
- untrusted fork pull-request execution;
- arbitrary host commands, generic shell strings, or user-selected Lima resource values;
- deployment, merge, publication, signing, or publisher credentials;
- cache deletion, Lima instance deletion, factory reset, or unrelated host cleanup;
- release-package acceptance, which belongs to W07;
- continuous-service acceptance, which belongs to W06.

## Command classes

Every acceptance step MUST identify the command class so current implementation evidence is not confused with the target product contract.

| Class | Meaning |
| --- | --- |
| Current exact surface | Reachable on the exact named commit and normally requires explicit store, revision, generation, immutable source, resource, cache, and observation evidence. |
| Accepted alpha target | Required for the run-once alpha but implemented by later W02-W04 lanes. |
| Later service surface | Deliberately absent from run-once alpha acceptance and owned by W06 or W07. |

### Command inventory on the drafting base

| Operator intent | Current exact surface | Accepted alpha target | Owner |
| --- | --- | --- | --- |
| Unified status | `smolrunner worker status --store-root ROOT` reads one exact durable snapshot | `smolrunner status` discovers accepted configuration and presents the unified operator report | W02/W04 |
| Initialise durable worker state | none | `smolrunner worker init` | W02/W04 |
| Submit work | `smolrunner queue submit` with explicit revision, generation, observation time, request/profile/source/resource/cache evidence | `smolrunner queue submit --profile PROFILE --repository CHECKOUT`, with strict flags retained as advanced controls | W02/W04 |
| List queue | `smolrunner queue list --store-root ROOT --revision R --generation G` | routine discovery plus strict advanced controls | W02/W04 |
| Inspect job | `smolrunner job show --store-root ROOT --revision R --generation G REQUEST_ID` | routine discovery plus strict advanced controls | W02/W04 |
| Cancel job | `smolrunner job cancel --store-root ROOT --revision R --generation G --cancelled-at T REQUEST_ID` | routine discovery plus strict advanced controls | W02/W04 |
| Advance one bounded step | none | `smolrunner worker run-once` | W03/W04 |
| Continuous supervision | none | `smolrunner worker serve` and service commands | W06; excluded here |
| Availability controls | none | `smolrunner availability active|away|off|auto` | W06; excluded here |

`smolrunner status` is the routine unified operator report. `smolrunner worker status --store-root ...` remains the strict exact durable-snapshot surface and MUST NOT silently observe Lima, processes, GitHub, a checkout, or unrelated host state.

## System model and invariants

### Durable work state

The work lifecycle uses distinct states:

- `queued`;
- `reserved`;
- `starting`;
- `running`;
- `draining`;
- `terminal`.

A queued request MUST NOT be described as running. A terminal result MUST identify its exact request and attempt and MUST NOT be inferred only from process exit or disappearance.

### Machine profile state

The persistent Lima instance uses separate machine/profile states:

- `stopped`: no VM runtime memory reservation; durable disk and caches retained;
- `interactive`: reviewed 3 GiB / 4 vCPU envelope;
- `work`: reviewed 10 GiB / 8 vCPU envelope.

Any eligible queued or active job requests `work`. When the queue is empty and no reservation remains, the desired profile becomes `interactive` after 10 minutes of accepted idle evidence and `stopped` after 30 total idle minutes.

New work cancels a pending downscale. A profile change or stop MUST be refused while a reservation is active, job outcome is uncertain, drain evidence is incomplete, or observations are stale.

Profile state MUST NOT be used as a job state.

### Run-once result

Every `worker run-once` invocation returns exactly one accepted result class:

| Result | Meaning |
| --- | --- |
| `satisfied` | The accepted desired state already holds and no mutation was required. |
| `action_applied` | One exact accepted action completed and was re-observed or durably recorded. |
| `continuation` | Progress or observation occurred, but another invocation with fresh evidence is required before the next action. |
| `blocked` | No safe action can proceed until a named dependency, approval, resource, recovery step, or operator decision is supplied. |
| `failed` | The attempted bounded action ended in a typed failure and no hidden retry is running. |

A continuation is not background work. A blocked result names the blocker and suggested next action.

### One-shot action boundary

One `worker run-once` invocation MUST:

1. resolve accepted configuration and durable state;
2. obtain the bounded observations required for one decision;
3. compute one exact next action;
4. perform at most one accepted lifecycle or job action, plus the publication inseparable from that action;
5. re-observe the authority state needed to describe the result;
6. return one accepted result class.

It MUST NOT hide an unbounded polling loop. Work that needs new evidence returns `continuation` and requires another operator invocation.

### Immutable identity boundary

A submitted or executed request MUST bind:

- request identity;
- verification-profile identity;
- repository identity;
- exact commit object identity;
- exact Git tree identity;
- runner/profile identity;
- requested resource limits;
- cache identity and access mode;
- submission and observation evidence.

Execution and terminal evidence additionally bind one exact attempt identity. Mutable references or a dirty checkout cannot substitute for immutable source evidence.

### Cache boundary

Stopping or changing the Lima profile MUST preserve the persistent instance disk and reviewed caches. Cache identity is acceleration evidence only and MUST NOT be treated as proof that source passed verification.

### Public-output boundary

Human and JSON output MUST derive from the same typed report. Public output MUST remain bounded and MUST NOT include private paths, credentials, environment dumps, arbitrary repository contents, raw child output, unreviewed command lines, PIDs, cgroup paths, or raw operating-system prose.

## Evidence classes

| Evidence class | Purpose | Required producer |
| --- | --- | --- |
| Pure/model tests | Prove deterministic validation, policy, replay, and classification without I/O | W01-W03 modules |
| Injected integration | Prove the complete run-once sequence with deterministic adapters and durable bytes | W04 I04 |
| Installed CLI | Prove parsing, exit classes, human/JSON agreement, privacy, and exact command composition | W04 I02/I04 |
| Physical receipt | Prove the accepted path on the named M5 MacBook Air and Lima instance | W04 I05 after explicit operator approval |

Evidence expires when the tested SmolRunner commit, accepted schema, command contract, relevant Lima profile, or physical harness changes. A later commit cannot inherit acceptance merely because an earlier commit passed.

## Common deterministic fixture

All injected cases use a fixture with these properties unless a case overrides them:

- an isolated temporary state root inaccessible outside the test;
- deterministic caller-supplied time and no wall-clock reads;
- one accepted personal-worker configuration identity;
- one persistent Lima-instance identity with injected observations;
- one reviewed named verification profile;
- one immutable repository, commit, and tree;
- one request ID and one attempt ID;
- bounded resource and cache identities;
- deterministic process/readiness/lifecycle adapters;
- captured durable bytes before and after each action;
- unique private sentinels for paths, credentials, environment values, commands, process output, PIDs, cgroup names, and unrelated host facts.

Every case MUST record:

- exact setup state and revision/generation;
- operator command or direct typed input;
- exit class;
- semantic JSON predicates;
- expected durable-byte relation: unchanged, one exact successor, or named recovery publication;
- expected host/process action count;
- next accepted operator action;
- privacy scan result.

## Deterministic injected acceptance matrix

Exact JSON field names remain owned by P03-P05. Until those schemas merge, predicates below are semantic and MUST later be translated one-for-one into accepted fields rather than weakened.

### Initialisation and status

#### A01 — status before initialisation

Setup: valid accepted configuration; no managed state directory or durable document.

Required result:

- supported report schema;
- uninitialised or missing-state blocker;
- suggested next action `worker init`;
- no state directory, lock, current document, or staged document created;
- no Lima, process, repository, or credential action.

#### A02 — first initialisation

Action: `worker init` from A01.

Required result:

- one valid initial canonical durable document;
- exact positive initial revision and queue generation;
- empty queued and active sets;
- initial machine/profile intent matches the accepted configuration contract;
- bounded receipt with no private state-root path.

#### A03 — repeated initialisation

Action: repeat `worker init` on A02.

Required result: `satisfied` or the accepted idempotent init disposition; revision, generation, and durable bytes unchanged; no Lima or process action.

#### A04 — unified empty status

Required result:

- exact configuration identity and durable revision/generation;
- empty queue and no active work;
- current and desired profile represented separately;
- blocker and next action explicit;
- read-only durable bytes.

### Submission identity, replay, and conflict

#### A05 — first routine submission

Action: `queue submit --profile PROFILE --repository CHECKOUT` from initialised empty state.

Required result:

- exact repository, commit, tree, verification profile, runner profile, resources, cache, and request identity;
- one queued request;
- revision and generation advance exactly once;
- cancellation active and fallback eligibility per accepted policy;
- no execution or Lima action.

#### A06 — exact submission replay

Replay A05 with identical semantics.

Required result: exact duplicate disposition; old/new revision and generation equal; byte-for-byte unchanged durable state; no host or process action.

#### A07 — changed semantics under the same request ID

Change immutable source, profile, resource, cache, or deadline evidence.

Required result: `conflict`; changed private value not echoed; durable bytes unchanged; no host or process action.

#### A08 — strict stale revision

Use a stale expected revision.

Required result: stale revision distinguishable from conflict and not-found; no automatic widened retry; durable bytes unchanged.

#### A09 — strict stale queue generation

Use current revision and stale generation.

Required result: stale generation distinct from stale revision; durable bytes unchanged; no inferred retry.

#### A10 — semantic queue capacity

Start from a valid document at the accepted live queue limit and submit one additional request.

Required result: bounded capacity/invalid-mutation result; durable bytes unchanged; human/JSON omit request and path sentinels; semantic queue cap distinguished from the canonical document byte guard.

### Bounded run-once progression

#### A11 — queued work while machine is stopped

Required result:

- exactly one `action_applied` or `continuation` outcome;
- no request reported running;
- work-profile intent or the first accepted lifecycle action only;
- action-count proof against a hidden lifecycle/readiness loop;
- fresh evidence required before another action.

#### A12 — profile transition refusal with active or uncertain work

Setup: active reservation, draining work, uncertain prior attempt, or stale evidence.

Required result: `blocked`; no job killed; blocker class exact; request/attempt evidence retained; no stop/edit/start action.

#### A13 — reviewed work-profile transition

Repeated run-once invocations transition stopped/interactive to `work`.

Required result across the sequence:

- only reviewed 10 GiB / 8 vCPU values;
- accepted graceful stop/edit/start/verify contract;
- each step idempotent against exact observation;
- instance/profile generation stable;
- persistent disk/cache identity unchanged;
- current profile `work` before readiness succeeds.

#### A14 — readiness debt

Setup: work profile selected; runner readiness offline, starting, stale, or incomplete.

Required result: `continuation` or `blocked` with exact readiness class; no reservation from stale evidence; no execution or success claim.

#### A15 — reservation and starting transition

Setup: eligible request, fresh capacity/readiness, no conflicting lease/reservation.

Required result:

- exactly one selected request;
- reservation identity/generation and applied limits exact;
- cache lease exact and conflict checked;
- atomic durable advance;
- state reserved or starting, never prematurely running;
- replay cannot create a second reservation.

#### A16 — accepted named-profile execution

Required result:

- exact request, attempt, source, profile, command, workspace/cache, and resource authority retained;
- repository-owned bootstrap/verification commands run only in the reviewed execution boundary;
- bounded output, timeout, descendant cleanup, and exit evidence;
- moving refs, dirty/wrong workspace, undeclared arguments, profile/cache drift, and authority widening fail closed;
- process start never represented as terminal completion.

#### A17 — successful terminal publication

Required result:

- terminal evidence binds request, attempt, profile, source, execution, and cleanup;
- active reservation/cache lease release atomic with terminal retention;
- one exact durable advance;
- request no longer live;
- unified status projects retained success.

#### A18 — failed terminal publication

Required result:

- typed bounded compile/link/test/process/timeout/runner-loss/cleanup or other accepted failure;
- failed execution never converted to success by cleanup or publication;
- identity-safe terminal/release transaction;
- retry eligibility explicit and no hidden re-execution.

### Interruption, replay, and recovery

#### A19 — interruption before durable mutation

Failure after planning but before write/action leaves predecessor bytes exact. Replay recomputes from fresh evidence with no duplicate effect.

#### A20 — interruption with a valid staged successor

Recovery validates and publishes only the exact successor, observes recovered revision/generation before deciding, and removes staged residue.

#### A21 — interruption after durable publication but before response

Replay detects the published successor and returns duplicate/`satisfied`/`continuation` without repeating the effect.

#### A22 — interruption during execution

Restart classifies exact attempt/process ownership before new execution; uncertain work blocks downscale and duplicate execution; stale/missing process evidence cannot become success; recovery rejoins, drains, or publishes one typed interrupted/runner-lost terminal result.

#### A23 — terminal response loss

Replay returns the already-published terminal result without re-execution.

#### A24 — process restart

Restart the command process between each accepted continuation. All authority comes from durable state and fresh observations, not process memory.

#### A25 — machine restart

Persistent state and Lima disk identity survive. Recovery completes before action; active/uncertain evidence is classified; no new job starts before readiness and prior-attempt state are fresh.

#### A26 — cancellation before reservation

Exact cancellation publishes once; request is not selected; replay is idempotent; changed or stale cancellation fails without publication.

#### A27 — cancellation race with reservation or completion

Exactly one transaction wins. The loser receives bounded stale/duplicate/conflict evidence. A request cannot be simultaneously cancelled queued work and active/terminal work.

#### A28 — writer lock contention

Immediate bounded busy result; no partial/staged file or host action; public output omits lock path and OS prose.

#### A29 — corrupt or unsafe durable state

Noncanonical bytes, wrong permissions/ownership, symlink redirection, or invalid history fail closed before mutation/action. Public output uses a stable corruption/unsafe-state class.

### Status and final idle

#### A30 — terminal unified status

Required result:

- coherent configuration, revision/generation, queue, active/latest terminal job, current/desired profile, readiness, blocker, and next action;
- terminal public identity and evidence digest;
- human/JSON semantic agreement;
- read-only state.

#### A31 — work-to-interactive after 10 idle minutes

Setup: queue empty, no active/uncertain attempt, exactly 10 accepted idle minutes, current profile `work`.

Required result across repeated run-once invocations:

- pending downscale cancelled if new work appears;
- transition only with fresh idle evidence;
- fixed 3 GiB / 4 vCPU interactive envelope;
- cache/disk identity persists;
- no active job interrupted.

#### A32 — stopped after 30 total idle minutes

Setup: exactly 30 total accepted idle minutes, no queue/reservation/active/uncertain work/operator hold, and fresh runner/Lima evidence.

Required result:

- graceful stop only after the idle barrier;
- VM runtime memory released;
- durable state, Lima disk, checkout, and accepted warm-cache identities persist;
- no delete, factory reset, prune, or cache deletion;
- final status explains stopped state and next action.

#### A33 — new work cancels pending idle transition

New eligible work cancels pending downscale/stop before profile mutation; desired profile becomes `work`; request remains exact and is not lost.

## Installed-CLI acceptance

Installed-binary tests MUST cover human and JSON modes for every public command class used in A01-A33.

At minimum they MUST prove:

- supported schema versions and stable machine classifications;
- exact exit classes for success, `satisfied`, `action_applied`, `continuation`, `blocked`, conflict/stale, and terminal failure;
- strict flags preserve no-create, exact-expectation, and caller-evidence boundaries;
- routine discovery never guesses among ambiguous configurations, stores, repositories, or profiles;
- help/parse errors contain no private runtime values;
- JSON is one bounded document and human output does not contradict it;
- no clock read when the contract requires injected/caller time;
- no stale mutation is silently retried with broader authority.

## Privacy acceptance

Each case MUST seed unique sentinels into every available private channel. Scan stdout, stderr, JSON, public receipts, `Debug`, captured panic/error text, and the physical receipt.

Forbidden output classes:

- absolute state, workspace, checkout, cache, Lima, home, temporary, `/proc`, or cgroup paths;
- repository contents, arbitrary Git output, patches, or unbounded refs;
- environment variables or dumps;
- tokens, SSH material, Keychain values, authorization headers, cloud credentials, or credential-shaped sentinels;
- raw child stdout/stderr beyond reviewed diagnostics;
- PIDs, process-group/cgroup names, kernel messages, or raw OS prose;
- generic command lines, shell fragments, or undeclared arguments;
- unrelated hardware, account, application, browser, network, or filesystem facts.

Tests MUST assert absence of exact sentinels, not rely on manual inspection.

## Physical M5 MacBook Air acceptance

### Approval boundary

The physical run is a consequential host effect and requires contemporaneous operator approval naming:

- exact SmolRunner commit and binary digest;
- acceptance schema/version and document digest;
- exact harness commit;
- Lima version and accepted instance identity;
- profiles the harness may select (`interactive`, `work`, `stopped`);
- repository and named verification profile;
- maximum runtime, CPU, memory, PID, and disk-growth bounds;
- whether `caffeinate` may be used while work is active;
- expected final idle/cache-retention check;
- allowed cleanup actions.

Approval authorises only the named run. It does not authorise credentials, GitHub registration, LaunchAgent/service installation, external publication/contact, paid capacity, signing, deployment, Lima deletion, cache deletion, or unrelated host mutation.

### Physical preconditions

The harness MUST refuse unless it proves:

- Apple-silicon host and bounded 24 GiB resource class;
- accepted macOS/Lima compatibility;
- exact binary digest and clean acceptance checkout;
- exact persistent Lima instance identity and no host-home mount;
- reviewed plain-mode security settings;
- no forwarded SSH agent, host keys, proxy credentials, or ambient GitHub/cloud tokens in the guest;
- no active/uncertain job or conflicting marker;
- sufficient host/guest disk headroom;
- exact repository/profile identities;
- one-job concurrency and reviewed resource envelope;
- private receipt construction path and allowlisted public fields.

### Physical sequence

The harness MUST record:

1. pre-run host, Lima, durable-state, readiness, and cache observations;
2. applicable A01-A10 initialisation/submission/replay/conflict checks on a dedicated state root;
3. stopped or interactive starting condition;
4. repeated explicit run-once invocations through work-profile selection/readiness;
5. one named profile execution against exact immutable source;
6. terminal publication and unified status;
7. safe interruption/recovery cases, at minimum process restart and terminal-response replay;
8. cancellation/stale checks isolated from unrelated work;
9. 10-minute interactive and 30-total-minute stopped idle sequence, using accepted injected controls or recorded physical timing;
10. final proof that durable state and cache/disk identities persist while VM runtime memory is released;
11. privacy scan and receipt finalisation.

The harness MUST stop without claiming acceptance on identity drift, stale/uncertain work, unapproved credential/permission prompts, unmodelled host mutation, resource breach, cleanup uncertainty, or privacy failure.

### Physical public receipt

The versioned allowlisted receipt records only:

- receipt schema and acceptance-contract version/digest;
- exact SmolRunner commit, binary/build digest, and harness commit;
- bounded Apple-silicon host/resource class;
- Lima version, opaque instance identity digest, guest identity, and selected profiles;
- repository, profile, request, reservation, attempt, command, commit, and tree identities;
- requested/applied resource and concurrency envelopes;
- lifecycle actions and bounded phase timings;
- readiness evidence class;
- terminal classification and evidence digest;
- injected/physical case IDs completed;
- replay, recovery, cancellation, stale-state, and privacy assertions;
- cache identity before/after and retention disposition, without paths/contents;
- final queue, reservation, worker, runner, Lima profile, and idle state;
- operator attestation that the named run occurred under the recorded approval.

The receipt MUST NOT include arbitrary logs. Private diagnostics remain operator-controlled and may be referenced only by an opaque local evidence ID if the accepted schema permits it.

## W04 acceptance traceability

| W04 gate | Required evidence |
| --- | --- |
| `worker init` creates or reports accepted state idempotently | A01-A04 plus installed CLI |
| routine submission publishes one exact request | A05-A10 plus installed CLI |
| run-once advances through bounded continuations | A11-A16 and action-count assertions |
| named profile completes durably | A16-A18 |
| unified status explains current/terminal state | A04 and A30 |
| replay, conflict, interruption, restart, and privacy pass | A06-A10, A19-A29, privacy suite |
| physical receipt names exact current-main commit | physical approval, sequence, receipt |
| final idle follows policy and preserves caches | A31-A33 plus physical evidence |
| roster records receipt and activates W06 | exact receipt ID/digest and #234 update |
| after-action records friction and timings | W04 comment linked from receipt |

## Cross-lane gates

### P01 product vocabulary

P01 exact candidate `90d043a0d79114f9ce79f577b2f0cefaa820cc49` and this contract agree on:

- routine unified `status` and strict exact `worker status`;
- five run-once result classes;
- separate work/profile/result/outcome types;
- 10-minute interactive and 30-total-minute stopped idle policy;
- approval boundaries and W06 exclusions.

A moved P01 or P06 head requires a fresh terminology cross-review.

### P03 configuration

P03 MUST provide typed identities sufficient for the fixture and physical approval without public private paths. Semantic configuration predicates become exact after that schema is accepted.

### P04 status

P04 MUST map A01, A04, A11-A18, and A30-A33 into one coherent human/JSON report, keeping current state, desired state, blocker, and next action distinct.

### P05 errors and remediation

P05 MUST provide stable public classes for all blocked/error results, including uninitialised, stale revision/generation, conflict, busy, unsafe/corrupt state, readiness debt, active/uncertain work, lifecycle/execution/cleanup failure, approval required, and unsupported version.

## Completion and stop rules

The alpha is accepted only when:

- all applicable deterministic cases pass on one exact integrated head;
- installed CLI tests pass on that head;
- the approved physical run passes and emits one valid public receipt for that head;
- receipt and test results pass privacy scanning;
- final state is stopped after 30 total idle minutes with persistent state/cache identity retained;
- the canonical roster records the exact receipt and commit.

Any unexpected harness/test failure, stale evidence, identity drift, disclosure, unapproved mutation, or uncertain cleanup invalidates the physical run and requires a new receipt after repair.

## Open terms reserved for downstream schemas

The following remain intentionally unresolved until their owning lanes merge:

1. exact initial profile/intent after first initialisation;
2. exact JSON field names and exit-code numbers;
3. exact terminal-result selection when multiple retained terminal jobs exist;
4. exact opaque local diagnostic-reference type allowed in the physical receipt.

These open terms do not weaken the required observable behaviour or authority boundaries.

## Related contracts

- #117 — repository-owned bootstrap boundary;
- #120 — availability modes and active/uncertain-work safety;
- #148 — named reusable verification profiles;
- #150 — exact credentialless handoff, outside this run-once acceptance path;
- #157 — warm-runner queue, resource, cache, receipt, and privacy requirements;
- #171 — persistent Lima profiles, idle shutdown, and cache retention;
- #187 — personal-worker queue and host-broker direction;
- #199 — memory-aware Rust verification evidence;
- #233-#239 — programme and wave dependencies.
