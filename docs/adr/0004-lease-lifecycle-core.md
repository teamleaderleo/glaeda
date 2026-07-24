# ADR 0004: lease lifecycle core

- Status: accepted for the exploratory leased-execution direction
- Date: 2026-07-24

## Context

SmolRunner's existing runner stewardship and disposable verification work needs a small lifecycle model before it can safely retain workspaces, keep previews alive, renew ownership, or clean up expired resources.

A lease may eventually carry time bounds, resource reservations, source and artifact provenance, route ownership, and cleanup receipts. Those concerns should share one state vocabulary without requiring an early daemon, database, provider adapter, or scheduler.

The first model must remain useful to the current CLI-first codebase:

- transitions are planned as pure data;
- no transition mutates durable state by itself;
- every accepted transition advances an optimistic revision;
- terminal records reject further changes;
- a run cannot enter a sleeping state;
- source and artifact contracts remain a separate decision;
- persistence and clock semantics remain a separate decision.

## Decision

Introduce a platform-independent `lease` module with the following concepts.

### Identity

A lease identity contains:

- a validated opaque lease ID;
- the durable SmolRunner installation ID that owns it;
- a lease kind: `run`, `workspace`, or `preview`.

The installation ID prevents a lease name from authorizing adoption across restored, copied, or unrelated SmolRunner installations. Later source, artifact, slot, route, and worker evidence will extend ownership classification; they do not belong in this initial lifecycle primitive.

### States

| State | Meaning |
| --- | --- |
| `pending` | Desired or reserved, with live execution still absent. |
| `active` | Execution or a retained live service currently owns resources. |
| `sleeping` | A workspace or preview retains recoverable state while its live process is stopped. |
| `releasing` | Cleanup has begun and may require several journaled operations. |
| `released` | Operator-requested cleanup completed. |
| `expired` | Lifetime policy ended the lease. |
| `failed` | Activation, supervision, or cleanup reached a terminal failure outcome. |

`released`, `expired`, and `failed` are terminal. A new attempt receives a new lease identity instead of reviving a terminal record.

### Actions

| Current state | Action | Result |
| --- | --- | --- |
| `pending` | `activate` | `active` |
| `pending`, `active`, `sleeping` | `renew` | same state, next revision |
| `active` workspace or preview | `sleep` | `sleeping` |
| `sleeping` | `wake` | `active` |
| `pending`, `active`, `sleeping` | `begin_release` | `releasing` |
| `releasing` | `finish_release` | `released` |
| any nonterminal state | `expire` | `expired` |
| any nonterminal state | `fail` | `failed` |

All other pairs fail closed. Run leases reject `sleep` because a completed or interrupted run should release resources and produce a separate retained artifact or workspace lease when continuity is required.

### Revisions

Every accepted action increments a `u64` revision, including renewal. The revision supports later compare-and-swap persistence and stale-writer rejection. Revision overflow rejects the transition before a transition record is emitted.

A transition records:

- identity;
- previous and next state;
- action;
- previous and next revision;
- schema version.

The lifecycle module returns typed transition data. Persistence, journals, clocks, container execution, and routing consume that data later.

## Consequences

### Benefits

- CLI plans, future durable state, and a future supervisor can share one transition table.
- Renewal and sleeping become explicit operations instead of inferred process behavior.
- Resource cleanup can enter `releasing` before individual reversible mutations run.
- Stale clients can eventually be rejected with a revision precondition.
- The model is testable on every development platform without Linux services or credentials.

### Costs

- Terminal failure currently requires creating a new lease for retries.
- Time and expiry deadlines remain absent from the core record.
- The initial identity remains incomplete for adoption or removal of containers, routes, workspaces, and artifacts.
- Durable compare-and-swap persistence still needs a separate design and implementation.

## Deferred decisions

The following remain open:

- persisted timestamps, monotonic deadlines, renewal windows, and clock recovery;
- source commit and artifact digest contracts;
- preview slots and supersession;
- worker assignment and resource reservation evidence;
- container and route ownership markers;
- crash-safe lease persistence and compare-and-swap updates;
- systemd timer versus daemon supervision;
- retry semantics after a terminal failure;
- integration with Stensibly claims.

## Security notes

- A lease ID alone never proves ownership of a runtime resource.
- Repository code must never receive the durable lease store, Podman control socket, or proxy administration endpoint.
- Persistence must reject stale revisions and foreign installation IDs.
- Cleanup must classify concrete containers, workspaces, routes, and artifacts through kind-specific immutable evidence before mutation.
- Expiry is an authorization to plan cleanup, followed by the same ownership checks required for operator-requested release.
