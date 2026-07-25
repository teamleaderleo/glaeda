# ADR 0019: Debian package attempt and recovery classification

- Status: Accepted for package reconciliation recovery planning
- Date: 2026-07-25

## Context

`apt-get install` is not an atomic mutation. A nonzero exit may follow unpacking, configuration, trigger execution, or another partial package-manager change. An interrupted process or missing bounded process record is even less conclusive. Treating either case as unchanged host state could authorize an unsafe retry from stale evidence.

Package removal is not a truthful inverse. Removing newly installed packages may also remove dependencies, preserve configuration, or disturb packages and services that began using the new state. Debian package preparation is therefore compensating, not reversible.

## Decision

### Reviewed boundary

Recovery classification accepts only:

- the exact planned action identity;
- the root execution lane;
- rollback class `compensating`;
- the reviewed typed `apt-get install --yes --no-install-recommends ...` command;
- either an exact bounded process record for that command or a typed lane-failure kind.

A process record must match the reviewed argv and child-environment keys exactly and remain within the process capture limits. Process stdout and stderr are not copied into the public recovery report or journal summary.

### Attempt states

One package attempt is classified as:

- **not started** — a lane, command, privilege, or executable check failed before the process boundary;
- **exited successfully** — the exact process record has exit status zero and success true;
- **exited nonzero** — the exact process record has a canonical nonzero exit status and success false;
- **execution uncertain** — the process boundary failed to return a complete record, the process was interrupted, or status and success evidence are inconsistent.

### Observation barrier

A successful process exit is not yet reconciled package state.

Every started attempt—successful, nonzero, or uncertain—requires a fresh bounded package observation before:

- dependent host actions continue;
- the package action is considered satisfied;
- a new package plan is created;
- another installation attempt is made.

A pre-execution refusal does not imply that package state changed, but the boundary must be repaired and the plan regenerated before execution.

### Journal classification

The classifier produces bounded public journal material:

- a successful exit produces a public receipt stating that dependent work remains blocked pending fresh observation;
- a nonzero exit produces failure code `debian-package-install-nonzero`;
- uncertain execution produces failure code `debian-package-install-uncertain`;
- a pre-execution refusal produces failure code `debian-package-install-not-started`.

The durable reconciliation layer must enforce the observation barrier rather than treating the successful receipt as permission to continue directly to dependent mutations.

### Compensation

Automatic package removal is not allowed as rollback. Recovery consists of fresh observation, a newly validated plan, and explicit operator-visible compensation guidance when the observed state remains incompatible.

### Scope boundary

This ADR defines pure classification and public recovery evidence. It does not execute `apt-get`, integrate a package executor with durable journals, remove packages, or add an apply command.

## Consequences

- Stale pre-attempt package observations cannot authorize retry after a potentially partial package operation.
- Public journals can distinguish refusal before execution from uncertain host mutation.
- Process output remains below the public journal boundary.
- Durable package execution still requires an explicit observation barrier and fresh-plan orchestration.
