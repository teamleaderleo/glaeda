# ADR 0006: Preview slot coalescing

## Status

Accepted for the exploratory preview layer.

## Context

Agents may push many verified commits during one task. Creating a new external deployment for every push consumes deployment quotas and keeps services alive that nobody needs to inspect.

SmolRunner needs a deterministic decision before any container, route, or provider mutation occurs.

## Decision

A preview request names one explicit slot and one immutable artifact identity. It also declares:

- a nonzero container port;
- a bounded lease lifetime between one minute and seven days;
- an optional bounded path-only health endpoint.

The planner compares the request with current state for the same slot and produces one of three actions:

- `create`: no current preview exists; generation 1 begins;
- `reuse_and_renew`: artifact, port, and health path match; the current generation remains and its lease may be renewed;
- `replace`: runtime inputs changed; the generation advances exactly once and the old artifact is retained in the plan for cleanup and audit.

A changed TTL alone renews the existing preview. It never creates another runtime generation.

Current state from another slot fails closed. Generation overflow fails closed. The planner performs no mutation.

## Consequences

- Repeated agent requests for the same verified output consume one preview slot.
- A stable URL may remain attached to the slot while generations change behind it.
- Verification frequency stays independent from deployment frequency.
- Provider choice remains downstream from the same deterministic plan.
- The lease layer owns expiry and renewal; the preview planner owns runtime equivalence and supersession.

## Deferred work

- Durable preview-state documents.
- Authorization for creating or replacing slots.
- Route identity and ownership evidence.
- Health-check execution and readiness timeouts.
- Cleanup ordering across routes, containers, and workspaces.
- Local Podman and external provider adapters.
