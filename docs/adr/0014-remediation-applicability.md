# ADR 0014: remediation applicability is separate from diagnosis and authority

## Status

Proposed, plan-only first slice.

This ADR progresses #112. It defines a pure public vocabulary and grants the current executable no new mutation authority.

## Context

SmolRunner already has two useful pieces that should remain distinct:

1. the public operator error catalogue classifies failures by retry, remediation, dependency, approval, and a small allowlist of suggested commands;
2. the reliable control-loop model requires exact ownership, repair budgets, durable checkpoints, fresh verification, rollback or compensation, circuit breakers, local vetoes, and explicit action-class policy before automatic repair.

A missing seam remains between them. `remediation: repair` says what broad family of response may be needed. It does not answer whether one concrete repair proposal is supported by exact evidence, whether it is reversible, whether it may only be displayed as a plan, or whether a later policy layer may consider executing it automatically.

Encoding those questions only in prose would invite callers to treat a diagnostic label or suggested command as authority.

## Decision

Add a pure `OperatorRemediationCandidate` contract with three independent axes.

### Diagnostic confidence

```text
exact
conditional
insufficient
```

`exact` means the accepted evidence identifies the failure family and the proposed response. `conditional` means the response depends on an explicit condition that still needs review. `insufficient` means the evidence cannot support an executable response.

### Operational safety

```text
read_only
reversible
compensating
irreversible
```

This is independent from confidence. An action can be diagnostically exact and still destructive.

Mutating candidates derive the existing journal `RollbackClass` and require exact managed ownership. Read-only candidates require neither rollback nor managed-resource ownership.

### Applicability

```text
advisory_only
plan_only
policy_eligible
```

`policy_eligible` means only that a later policy layer may consider the action. It never means authorised.

A policy-eligible mutating candidate requires:

- exact diagnostic confidence;
- explicit evidence preconditions;
- exact managed ownership;
- a positive repair-budget cost;
- a durable checkpoint;
- fresh post-action verification;
- circuit-breaker participation;
- a reversible or compensating action class.

Irreversible actions can never be policy-eligible under this version.

### Non-authority invariant

Every remediation candidate serializes:

```json
{ "authorizes_mutation": false }
```

The candidate contains no command vector, credential, filesystem path, provider token, executor handle, or capability grant.

Before any future executor receives a repair plan, a separate policy/authority layer must still evaluate current ownership, exact observations, active work, repair budget, circuit-breaker state, directives, operator holds, approval requirements, and the repository's existing mutation/journal contracts.

## Why confidence and safety are separate

Conflating them creates two common failure modes.

A highly certain diagnosis can yield an unsafe automatic response:

```text
"this state file is corrupt" -> exact diagnosis
"delete the state root"      -> destructive response
```

A less certain diagnosis can yield a useful reversible plan:

```text
"this service may be wedged" -> conditional diagnosis
"show a restart plan"        -> reversible, but plan-only
```

Automation needs both facts.

## Why this is not a repair implementation

This slice intentionally does not:

- choose remediation candidates for every `OperatorErrorCode`;
- add `smolrunner repair`;
- execute a suggested command;
- consume repair budgets;
- persist circuit-breaker state;
- alter ownership classification;
- create a fleet directive;
- add a daemon or unattended mutation loop.

Those remain later steps under #112 and the reliable control-loop sequence.

## Initial examples

An exactly owned expired disposable worker may eventually produce a reversible candidate that is policy-eligible when the accepted evidence also proves no active job.

A stale durable revision can produce a read-only refresh candidate.

A conditional Lima diagnosis may produce a restart plan, but cannot become policy-eligible until the diagnosis is exact.

An irreversible migration remains outside automatic policy even when its need is certain.

## Consequences

Positive:

- public diagnostics can propose richer recovery without implying permission;
- future `repair --dry-run` output can expose confidence and safety directly;
- automation policy gets a closed typed input instead of parsing prose;
- the contract reuses existing rollback classes and existing control-loop requirements;
- tests can reject accidental promotion of conditional or irreversible proposals into automatic work.

Cost:

- a later candidate-selection layer must map concrete observed findings into this vocabulary;
- the public error catalogue and remediation candidates remain separate concepts callers must compose deliberately.

That separation is intentional.

## Verification boundary

The first implementation is pure Rust data and validation with serialization tests. It has no filesystem, process, Lima, GitHub, credential, durable-state, or host mutation access.
