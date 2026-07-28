# Personal worker alpha acceptance contract

Status: W01 contract draft  
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

The final P01/P06 cross-review MUST decide whether `smolrunner worker status` remains an advanced alias after `smolrunner status` exists. This document uses **unified status** and **exact worker snapshot** as distinct concepts until that decision is accepted.

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

Profile state MUST NOT be used as a job state. A profile change or stop MUST be refused while a reservation is active, job outcome is uncertain, drain evidence is incomplete, or observations are stale.

### One-shot action boundary

One `worker run-once` invocation MUST:

1. resolve accepted configuration and durable state;
2. obtain the bounded observations required for one decision;
3. compute one exact next action;
4. perform at most one accepted lifecycle or job action, plus the publication inseparable from that action;
5. re-observe the authority state needed to describe the result;
6. return a typed result or continuation.

It MUST NOT hide an unbounded polling loop. Work that needs new evidence returns a continuation and requires another operator invocation.

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

Acceptance evidence is divided into four classes.

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

Action: routine unified status.

Required predicates:

- supported report schema;
- state classified as uninitialised or blocked on missing state;
- suggested next action is `worker init`;
- no state directory, lock, current document, or staged document is created;
- no Lima, process, repository, or credential action occurs.

Durable expectation: no files created.

#### A02 — first initialisation

Setup: A01 predecessor.

Action: `worker init`.

Required predicates:

- one valid initial durable document is published;
- store revision and queue generation are exact positive initial values;
- queue and active sets are empty;
- initial machine/profile intent matches the accepted P01 contract;
- public receipt contains no private state-root path.

Durable expectation: one canonical initial document and required lock metadata only.

#### A03 — repeated initialisation

Setup: exact A02 successor.

Action: repeat `worker init`.

Required predicates:

- disposition is idempotent/already initialised;
- revision, generation, and durable bytes remain unchanged;
- no Lima or process action occurs.

#### A04 — unified empty status

Setup: exact initialised empty state.

Action: routine unified status.

Required predicates:

- exact configuration identity and current store revision/generation;
- empty queue and no active or terminal job selected as current work;
- current and desired machine/profile state are distinct fields;
- blocker and next action are explicit;
- report is observational and leaves durable bytes unchanged.

### Submission identity, replay, and conflict

#### A05 — first routine submission

Setup: initialised empty state and clean immutable checkout evidence.

Action: `queue submit --profile PROFILE --repository CHECKOUT`.

Required predicates:

- repository, commit, tree, profile, runner profile, resources, cache identity, and request identity are exact;
- one queued request is published;
- store revision and queue generation advance exactly once;
- cancellation defaults active and fallback eligibility follows accepted policy;
- no job execution or Lima lifecycle action occurs.

Durable expectation: one canonical successor.

#### A06 — exact submission replay

Setup: exact A05 successor.

Action: replay semantically identical submission with the accepted idempotency/request identity.

Required predicates:

- disposition is `duplicate` or the exact accepted replay term;
- old and new revision/generation are equal;
- durable bytes are byte-for-byte unchanged;
- no host or process action occurs.

#### A07 — changed semantics under the same request ID

Setup: exact A05 successor.

Action: reuse the request ID while changing at least one immutable source, profile, resource, cache, or deadline field.

Required predicates:

- disposition is `conflict`;
- the changed private value is not echoed;
- durable bytes are unchanged;
- no host or process action occurs.

#### A08 — strict stale revision

Setup: current state newer than the supplied expected revision.

Action: strict advanced submission or cancellation.

Required predicates:

- stale revision is distinguishable from conflict and not-found;
- current state is not disclosed beyond the accepted bounded report;
- durable bytes are unchanged.

#### A09 — strict stale queue generation

Setup and action: as A08, with current revision and stale generation.

Required predicates:

- stale queue generation is distinct from stale revision;
- durable bytes are unchanged;
- no inferred retry silently widens authority.

#### A10 — semantic queue capacity

Setup: one valid document with the maximum accepted live queue entries.

Action: submit one additional exact request.

Required predicates:

- bounded capacity/invalid-mutation result;
- durable bytes unchanged;
- JSON and human output omit request and path sentinels;
- the semantic queue cap remains distinct from the canonical durable-document byte guard.

### Bounded run-once progression

#### A11 — queued work while machine is stopped

Setup: one eligible queued request; Lima observed stopped; no active reservation.

Action: `worker run-once`.

Required predicates:

- exactly one accepted action or continuation is reported;
- no request is reported running;
- the result either records work-profile intent or performs the first accepted lifecycle action according to the final I01 decomposition;
- action count proves no hidden stop/edit/start/readiness polling chain occurred in one invocation unless that chain is one reviewed atomic executor action;
- fresh evidence is required before the next step.

#### A12 — profile transition refusal with active or uncertain work

Setup: active reservation, draining work, uncertain prior attempt, or stale job evidence; requested profile change/downscale/stop.

Action: `worker run-once`.

Required predicates:

- lifecycle mutation is refused or blocked;
- active work is not killed implicitly;
- blocker identifies active, uncertain, drain, or stale-evidence class;
- durable request/attempt evidence remains intact;
- no `limactl edit`, stop, or start adapter action occurs.

#### A13 — reviewed work-profile transition

Setup: eligible queue, no active/uncertain work, exact stopped or interactive observation, fixed accepted work envelope.

Action: repeated run-once invocations as required by the accepted action boundary.

Required predicates across the sequence:

- only reviewed `work` CPU/memory values are selected;
- transition uses the accepted graceful stop/edit/start/verify contract;
- each step is idempotent against exact observation;
- profile generation and instance identity do not drift;
- persistent disk/cache identity is unchanged;
- final current profile is `work` before readiness can pass.

#### A14 — readiness debt

Setup: work profile selected; official runner/listener readiness is offline, starting, stale, or otherwise incomplete.

Action: `worker run-once`.

Required predicates:

- result is blocked/continuation with explicit readiness class;
- no job execution or terminal-success claim occurs;
- no new reservation is created from stale readiness evidence;
- next action requires fresh observation.

#### A15 — reservation and starting transition

Setup: eligible queued request, fresh exact capacity/readiness evidence, no conflicting cache lease or reservation.

Action: run-once.

Required predicates:

- exactly one request is selected by accepted queue policy;
- reservation identity/generation and applied limits match the request;
- cache lease is exact and conflict checked;
- durable revision/generation advance atomically;
- job state is reserved or starting, never prematurely running;
- a replay cannot create a second reservation.

#### A16 — accepted named-profile execution

Setup: exact reserved/starting request and reviewed immutable execution plan.

Action: the accepted run-once execution step.

Required predicates:

- exact request, attempt, source, verification profile, command identity, workspace/cache identity, and resource authority are retained;
- repository-owned bootstrap/verification entry points run only inside the reviewed execution boundary;
- output, timeout, descendant cleanup, and exit evidence are bounded;
- moving refs, dirty or wrong workspace, undeclared arguments, profile drift, cache drift, and authority widening fail closed;
- process start is never represented as terminal completion.

#### A17 — deterministic successful terminal publication

Setup: A16 execution observation proves successful completion and cleanup.

Action: terminal completion transaction.

Required predicates:

- terminal evidence binds request, attempt, profile, immutable source, execution result, and cleanup result;
- active reservation and cache lease release are atomic with terminal retention;
- store revision/generation advance exactly once;
- queue no longer presents the request as live;
- unified status can project the retained terminal result.

#### A18 — deterministic failed terminal publication

Setup: reviewed execution produces a typed compile/test/process/timeout/cleanup or other accepted failure.

Required predicates:

- failure remains typed and bounded;
- unsuccessful execution is never reported as successful because cleanup or publication succeeded;
- terminal evidence and release semantics remain the same identity-safe transaction as A17;
- retry eligibility is explicit and does not silently re-execute.

### Interruption, replay, and recovery

#### A19 — interruption before durable mutation

Inject failure after planning but before any durable write or host action.

Required result: predecessor bytes remain exact; replay recomputes from fresh evidence; no duplicate effect exists.

#### A20 — interruption with a valid staged successor

Inject failure after a canonical successor is staged but before final publication.

Required result: store recovery validates and publishes only the exact successor; the next command observes the recovered revision/generation before deciding; staged residue is removed.

#### A21 — interruption after durable publication but before response

Inject response loss after an exact state mutation commits.

Required result: replay detects the published successor and returns duplicate/already-satisfied/continuation without applying the mutation again.

#### A22 — interruption during execution

Inject runner/process loss while an owned attempt may still be active.

Required result:

- restart classifies exact attempt/process ownership before any new execution;
- uncertain work blocks profile stop/downscale and duplicate execution;
- stale or missing process evidence cannot be converted to success;
- the accepted recovery path either rejoins, drains, or publishes one typed interrupted/runner-lost terminal result.

#### A23 — terminal response loss

Inject response loss after A17 or A18 terminal publication.

Required result: replay returns the already-published terminal result and does not execute again.

#### A24 — process restart

Restart the command process between every accepted continuation in the golden path.

Required result: all authority comes from durable state and fresh injected observations; no required state lives only in process memory.

#### A25 — machine restart

Simulate host restart with persistent durable state and Lima disk identity retained.

Required result: state/store recovery completes before action; active/uncertain evidence is classified; no new job starts until readiness and prior-attempt state are fresh.

#### A26 — cancellation before reservation

Setup: queued request.

Action: cancellation with exact current expectations.

Required result: one exact cancellation successor; request is not selected; replay is idempotent; changed or stale cancellation fails without publication.

#### A27 — cancellation race with reservation or completion

Run cancellation and reservation/completion transactions against the same predecessor expectations.

Required result: exactly one wins; the loser receives bounded stale/duplicate/conflict evidence; no request exists simultaneously as cancelled queued work and active/terminal work.

#### A28 — writer lock contention

Hold the durable writer lock and attempt mutation.

Required result: immediate bounded busy result; no partial file, staged successor, or host action; public output omits lock path and OS prose.

#### A29 — corrupt or unsafe durable state

Inject noncanonical bytes, wrong permissions/ownership, symlink redirection, or invalid history.

Required result: fail closed before mutation or host action; public output contains a stable corruption/unsafe-state class only.

### Status and final idle

#### A30 — terminal unified status

Setup: retained successful or failed terminal result and current Lima/readiness observations.

Action: `smolrunner status`.

Required predicates:

- configuration, exact durable revision/generation, queue summary, active or latest terminal job, current/desired profile, readiness, blocker, and next action are coherent;
- terminal result includes exact public identity and evidence digest;
- human and JSON views agree semantically;
- status remains read-only.

#### A31 — work-to-interactive cooldown

Setup: empty queue, no active/uncertain attempt, cooldown threshold reached according to injected time, current profile work.

Action: repeated run-once invocations.

Required predicates:

- pending downscale is cancelled if new eligible work appears;
- transition occurs only with fresh idle evidence;
- fixed interactive envelope is selected;
- cache/disk identity persists;
- no active job is interrupted.

#### A32 — final stopped idle

Setup: total accepted idle threshold reached; no queue, reservation, active/uncertain work, or operator hold; fresh runner and Lima evidence.

Action: repeated run-once invocations until stopped.

Required predicates:

- graceful stop occurs only after the accepted idle barrier;
- VM runtime memory reservation is released;
- durable worker state, Lima instance disk, checkout, and accepted warm-cache identities persist;
- no Lima delete, factory reset, prune, or cache deletion occurs;
- final status explains stopped current state and next action.

#### A33 — new work cancels pending idle transition

Setup: pending downscale/stop and a newly queued eligible request.

Required result: pending idle transition is cancelled before profile mutation; desired profile becomes work; the request remains exact and is not lost.

## Installed-CLI acceptance

Installed-binary tests MUST cover both human and JSON modes for every public command class used in A01-A33.

At minimum they MUST prove:

- supported schema versions and stable machine classifications;
- exact exit classes for success, continuation, blocked/retryable, conflict/stale, and terminal failure;
- current strict flags preserve no-create, exact expectation, and caller-evidence boundaries;
- routine discovery never guesses among ambiguous configurations, stores, repositories, or profiles;
- `--help` and parse errors contain no private runtime values;
- JSON output is one bounded document and human output does not contradict it;
- no command uses the system clock when its contract requires injected or caller-supplied time;
- no convenience command silently retries a stale mutation with broader authority.

## Privacy acceptance

Each injected and installed-CLI case MUST seed unique sentinels into every private channel available to that case.

The scan covers stdout, stderr, serialized JSON, public receipts, `Debug`, panic/error text captured by the harness, and the physical receipt.

Forbidden output classes:

- absolute state, workspace, checkout, cache, Lima, home, temporary, `/proc`, or cgroup paths;
- repository file contents, arbitrary Git output, patches, or unbounded refs;
- environment variables or dumps;
- GitHub tokens, SSH material, Keychain values, authorization headers, cloud credentials, or credential-shaped sentinels;
- raw child stdout/stderr beyond reviewed bounded diagnostics;
- PIDs, process-group or cgroup names, raw kernel messages, and raw OS error prose;
- generic command lines, shell fragments, or undeclared arguments;
- unrelated hardware, account, application, browser, network, or filesystem facts.

A test MUST assert absence of the exact sentinel, not merely inspect a sample output manually.

## Physical M5 MacBook Air acceptance

### Approval boundary

The physical run is a consequential host effect and requires contemporaneous operator approval. The approval MUST name:

- exact SmolRunner commit and binary digest;
- this acceptance schema/version and exact document digest;
- exact harness commit;
- Lima version and accepted instance identity;
- profiles the harness may select (`interactive`, `work`, `stopped`);
- repository and named verification profile;
- maximum runtime, CPU, memory, PID, and disk-growth bounds;
- whether `caffeinate` may be used while work is active;
- expected final idle state and cache-retention check;
- cleanup actions allowed.

Approval authorises only the named run. It does not authorise credentials, GitHub registration, LaunchAgent/service installation, external publication/contact, paid capacity, signing, deployment, Lima deletion, cache deletion, or unrelated host mutation.

### Physical preconditions

The harness MUST refuse to start unless it proves:

- Apple-silicon host and bounded 24 GiB resource class expected by the contract;
- accepted macOS and Lima compatibility versions;
- exact SmolRunner binary digest and clean acceptance checkout;
- exact persistent Lima instance identity and no host-home mount;
- reviewed plain-mode security settings;
- no forwarded SSH agent, host SSH keys, proxy credentials, or ambient GitHub/cloud tokens in the guest;
- no active or uncertain job and no conflicting operator marker;
- sufficient host and guest disk headroom;
- exact repository and verification-profile identities;
- one-job concurrency and reviewed resource envelope;
- receipt output path is private during construction and public fields are allowlisted.

### Physical sequence

The physical harness MUST perform and record:

1. pre-run host, Lima, durable-state, readiness, and cache identity observations;
2. A01-A10 applicable initialisation/submission/replay/conflict checks on a dedicated acceptance state root;
3. stopped or interactive starting condition;
4. repeated explicit `worker run-once` invocations through work-profile selection and readiness;
5. one named verification-profile execution against the exact immutable source;
6. terminal publication and unified status;
7. selected interruption/recovery cases that are safe on the physical machine, at minimum command-process restart and completion-response replay;
8. cancellation and stale-expectation checks that cannot affect unrelated work;
9. cooldown/downscale/stop sequence using accepted injected time controls or a clearly recorded physical timing method;
10. final proof that durable state and cache/disk identities persist while VM runtime memory is released;
11. privacy scan and receipt finalisation.

The harness MUST stop without claiming acceptance on any identity drift, stale or uncertain job evidence, unapproved prompt for credentials/permissions, unmodelled host mutation, resource-bound breach, cleanup uncertainty, or privacy failure.

### Physical public receipt

The public receipt is versioned and allowlisted. It records only:

- receipt schema and acceptance-contract version/digest;
- exact SmolRunner commit and binary/build digest;
- exact harness commit;
- bounded Apple-silicon host/resource class, not serial numbers or account names;
- Lima version, opaque instance identity digest, guest identity, and selected profiles;
- repository, verification-profile, request, attempt, commit, and tree identities;
- requested and applied resource envelopes;
- lifecycle actions and bounded phase timings;
- terminal classification and evidence digest;
- injected/physical case IDs completed;
- replay, recovery, cancellation, and stale-state assertions passed;
- privacy sentinel classes passed;
- cache identity before/after and retention disposition, without private paths or contents;
- final worker and Lima state;
- operator attestation that the named run occurred under the recorded approval.

The receipt MUST NOT include arbitrary logs. Private diagnostics remain separately retained under the operator-controlled boundary and are referenced only by an opaque local evidence ID when needed.

## W04 acceptance traceability

| W04 gate | Required evidence |
| --- | --- |
| `worker init` creates or reports accepted state idempotently | A01-A04 plus installed CLI |
| routine submission publishes one exact request | A05-A10 plus installed CLI |
| `worker run-once` advances through bounded continuations | A11-A16 and action-count assertions |
| named profile completes durably | A16-A18 |
| unified status explains current and terminal state | A04 and A30 |
| replay, conflict, interruption, restart, and privacy pass | A06-A10, A19-A29, privacy suite |
| physical receipt names exact current-main commit | physical approval, sequence, and public receipt |
| final idle follows policy and preserves caches | A31-A33 plus physical final-state evidence |
| roster records receipt and activates W06 | exact receipt ID/digest and #234 update |
| after-action records friction and timings | W04 issue comment linked from receipt |

## Cross-lane gates

### P01 product vocabulary

Before P01 or P06 merges, the two exact heads MUST agree on:

- routine `status` versus exact `worker status` naming;
- accepted one-shot result terms, including `blocked`, `continuation`, `satisfied`, and terminal result;
- job lifecycle versus machine-profile terms;
- initial profile after `worker init`;
- work-to-interactive and interactive-to-stopped idle policy;
- which physical actions require approval;
- explicit exclusion of W06 service and availability commands.

### P03 configuration

P03 MUST provide typed identities sufficient to express the common fixture and physical approval without public private paths. P06 semantic configuration predicates become exact only after that schema is accepted.

### P04 status

P04 MUST map every status predicate in A01, A04, A11-A18, and A30-A33 into one coherent human/JSON report. It MUST keep current state, desired state, blocker, and next action distinct.

### P05 errors and remediation

P05 MUST provide stable public classes for every blocked/error result in this matrix, including uninitialised, stale revision, stale generation, conflict, busy, unsafe/corrupt state, readiness debt, active/uncertain work, profile/lifecycle failure, execution failure, cleanup failure, approval required, and unsupported version.

## Completion and stop rules

The alpha is accepted only when:

- all applicable deterministic cases pass on one exact integrated head;
- installed CLI tests pass on that same head;
- the approved physical run passes and emits one valid public receipt for that head;
- the final receipt and test results pass privacy scanning;
- the final state is accepted idle/stopped with persistent state and cache identity retained;
- the canonical roster records the exact receipt and commit.

Any failed required case leaves W04 incomplete. A bounded typed failure may itself be the expected result of a negative case, but an unexpected test/harness failure, stale evidence, identity drift, private disclosure, unapproved mutation, or uncertain cleanup invalidates the physical run and requires a new receipt after repair.

## Open terms reserved for cross-review

The following are intentionally unresolved in this P06 draft and MUST be replaced by accepted P01/P03-P05 vocabulary before merge:

1. exact initial profile and status wording after first initialisation;
2. final enum names for one-shot `satisfied`, `blocked`, and `continuation` results;
3. exact JSON field names and exit-code numbers;
4. exact cooldown durations represented by policy rather than hard-coded in acceptance tests;
5. whether exact `worker status` remains a public advanced alias;
6. exact terminal-result selection when multiple retained terminal jobs exist;
7. exact opaque local diagnostic-reference type allowed in the physical receipt.

These open terms do not weaken the required observable behaviour or authority boundaries above.

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
