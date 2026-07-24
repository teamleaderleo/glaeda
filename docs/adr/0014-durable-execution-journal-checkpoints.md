# ADR 0014: Durable execution-journal checkpoints

- Status: Accepted
- Date: 2026-07-25

## Context

An execution journal written only after a mutation batch finishes cannot support honest crash recovery. If the process or host fails while an action is running, a persisted `pending` record is ambiguous: the action may never have started, or it may have changed the host before the process disappeared.

The existing state layer already provides descriptor-relative, private, atomic replacement beneath one installation. The missing boundary is an execution protocol that publishes a complete journal snapshot before and after every operation whose result can become uncertain.

## Decision

SmolRunner durable execution uses one canonical journal document beneath an installation and replaces that document atomically at each transition.

The protocol is:

1. Validate the complete plan before persistence or mutation.
2. Persist the initial all-pending journal before the first executor call.
3. Mark one action `executing` and persist that snapshot before invoking its executor.
4. Persist the completed or failed outcome immediately after the executor returns.
5. After a failure, mark each eligible completed action `rollback_in_progress` and persist before invoking its inverse or compensation.
6. Persist `rolled_back`, `compensated`, or `rollback_failed` immediately after the rollback executor returns.

Only one action may be `executing` or `rollback_in_progress` in a valid document. `executing` retains no message. `rollback_in_progress` retains the public completion receipt needed to explain what is being recovered; it does not claim that the rollback has completed.

A checkpoint failure stops execution immediately. No later action or rollback is attempted. The public error retains:

- the checkpoint phase;
- the relevant action ID, when present;
- the last snapshot known to have been persisted;
- the attempted next snapshot; and
- one bounded, redacted persistence failure.

The state-store adapter rebuilds and validates the complete journal document for every checkpoint. Its first publication is create-only: an existing journal ID is a conflict and its recovery evidence is not replaced. Later checkpoints replace only the journal created by that adapter. Linux performs create-only publication with the installation-local lock and `RENAME_NOREPLACE`; both creation and replacement retain private temporary files, file synchronization, atomic rename, and parent-directory synchronization.

An unconfirmed irreversible action blocks the whole batch before mutation. Its record is `skipped`; all other records remain `pending`, including records that appear earlier in plan order.

## Recovery interpretation

An interrupted state is evidence of uncertainty, not permission to retry blindly.

- `executing` means the action may or may not have changed the world.
- `rollback_in_progress` means the inverse or compensation may or may not have completed.
- Recovery must re-observe ownership and preconditions before resuming, retrying, or compensating.
- A journal remains explanatory state and is not proof that the current host still matches it.

## Consequences

- A host crash cannot make an invoked action look definitively unattempted.
- Every mutation adds multiple durable writes; correctness is preferred over write minimization for the first apply implementation.
- Persistence failure after an executor returns can leave the durable snapshot intentionally conservative (`executing` or `rollback_in_progress`).
- A duplicate journal ID fails before existing recovery evidence can be replaced.
- The same protocol is testable with fake executors and in-memory stores before root or runner-user lanes exist.
- Real Linux fault injection remains required before enabling host mutation commands.
