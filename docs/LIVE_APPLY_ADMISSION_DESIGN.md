# Typed live-apply admission design

This is the next implementation contract after the receipt-backed synthetic
matrix. It defines the production-shaped admission and replay boundary without
enabling live deletion. The current `StoreAdmission` remains the only minted
capability, and `apply_live` continues to return `live_apply_disabled` until
this contract has an implementation and managed-host evidence.

## What exists today

`ProductionRecoveryAdapter::admit_descriptor_store` already binds an observer
identity, store generation, plan digest, enrollment, root, policy, and clean
recovery result. `RunnerLogStore` already holds the descriptor-bound lock and
uses monotonic `prepared → staged → delete_intent → projected|aborted` rows.
The durable adapter journal supports acknowledgement, cancellation, and
restart of the synthetic projection.

The missing production pieces are a durable admission lease, cancellation
epochs tied to the store journal, and a terminal apply receipt that replay can
return after a lost response. A random transaction nonce alone is insufficient:
retrying the same plan after a timeout must find the existing transaction rather
than mint another one.

## Typed contract

The production-only capability should be an opaque `LiveApplyAdmission` whose
private fields bind:

```text
admission_id       opaque stable ID
plan_digest        exact observer plan digest
root_generation    root, diagnostics, marker, UID identity tuple
store_generation   descriptor-bound store generation
policy_digest      idle window, batch limit, and schema
journal_digest     digest and byte length observed under the store lock
cancel_epoch       monotonic cancellation epoch
expires_at         bounded deadline
batch_sequence     monotonic sequence within the admission
```

Its public `AdmissionReceipt` is path-free and contains the same binding plus
the recovery receipt and candidate snapshots. Callers cannot construct it from
a CLI boolean or an arbitrary `LogItem` list.

The live result should be a typed `LiveApplyReceipt`:

```text
admission_id
plan_digest
recovery           pending-before, repaired, pending-after, disposition
items[]            transaction ID, candidate identity, phase, outcome
post_action        fresh root/process/journal identity and pending count
terminal           applied | cancelled | refused | replayed
```

`replayed` means the store found a durable terminal receipt for the same
`admission_id`, `plan_digest`, and batch sequence. It is not a second filesystem
effect. Every item outcome is bounded to `reclaimed`, `already_reclaimed`,
`skipped`, `cancelled`, or a typed refusal.

## Durable journal additions

Extend the state journal with admission metadata and terminal receipts. Each
row remains bound to the enrolled root/install tuple:

```text
admission_opened(admission_id, plan_digest, cancel_epoch, expires_at, batch_sequence,
                 root_generation, store_generation, policy_digest, journal_digest)
admission_cancelled(admission_id, cancel_epoch, phase)
item_prepared(admission_id, transaction_id, candidate_identity)
item_staged(admission_id, transaction_id, staged_identity)
item_delete_intent(admission_id, transaction_id)
item_projected(admission_id, transaction_id, outcome)
admission_terminal(admission_id, terminal_receipt_digest, terminal)
```

Rows must reject phase regression, conflicting duplicates, foreign admission
IDs, changed root/store generations, and a cancellation epoch lower than the
latest durable epoch. The terminal receipt is written and synced after the
final item projection and before the operation returns. Reopening the store
first reads that terminal row and returns the stored receipt byte-for-byte.

## Cancellation semantics

Cancellation is a durable epoch bump, not a process flag. The store checks the
epoch and deadline while holding the exclusive lock:

| Current phase | Cancellation result |
| --- | --- |
| admission opened, no item prepared | append `admission_cancelled`; terminal `cancelled`; no file effect |
| item prepared, source not staged | leave the prepared row for recovery; terminal `cancelled` only after recovery proves no pending row |
| item staged or delete-intent | do not unlink; return a recovery-pending receipt unless fresh identity/open checks prove safe recovery |
| item projected | preserve the projection; cancellation applies before the next item |
| terminal | replay the existing terminal receipt; cancellation is idempotent |

Cancellation must be checked before each new `prepared` row and before each
delete. A cancellation observed after `prepared` never hides that row or reports
completion while recovery debt remains.

## Replay algorithm

1. Open the descriptor-bound store and acquire its non-blocking lock.
2. Read and validate the journal binding, terminal receipt, current cancellation
   epoch, root/store generations, and journal digest.
3. If a matching terminal receipt exists, return it with `terminal=replayed`;
   perform no filesystem mutation.
4. Recover pending rows under the lock. Any unresolved row is a hard admission
   veto and is returned in the recovery receipt.
5. Validate the fresh observer plan, lease deadline, cancellation epoch, and
   candidate identities.
6. For each candidate, re-observe immediately before stage, after stage, before
   delete, and after delete. Append and sync each journal phase before the next
   filesystem effect.
7. Append the terminal receipt, sync the journal and parent, then return the
   exact receipt. A retry follows step 3.

The adapter should expose one replay-safe operation, not separate public
`stage`, `delete`, or journal-writing methods. The store owns the lock and
durability; the adapter owns the typed admission and cancellation policy.

## Acceptance matrix before enabling live apply

- unit tests for epoch monotonicity, deadline expiry, duplicate terminal replay,
  phase regression, foreign admission rows, and journal digest drift;
- managed Mac crash/restart runs at prepared, staged, delete-intent, and
  projected boundaries;
- cancellation tests for every phase, including an open staged file;
- exact source/tree and binary receipts for each managed run;
- fresh post-action process/root/journal evidence with no pending recovery;
- a candidate → soak → promotion receipt for one explicitly enrolled runner;
- an independent review confirming that `apply_live` is still closed until all
  rows are green.

Until those receipts exist, the synthetic matrix remains the accepted evidence
for benchmark ergonomics only; it is not production deletion authorization.
