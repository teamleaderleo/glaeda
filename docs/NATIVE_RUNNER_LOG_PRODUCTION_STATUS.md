# Native runner-log production status

This status records the primary review of PR #1507's green synthetic matrix. It
does not authorize a merge, promotion, or live fleet deletion.

## Primary review verdict

The exact managed Mac artifact from workflow run `39090421264`, requested for
PR head `66a259947fd4cd4f57c1623326bf309aa5486749`, is bound to merge commit
`b0a545286c095b7625c849176884e48649c862e8` and tree
`e591c76909367a93d279a741c8c72b6fe10c7513`. Its managed-adapter receipt reports
successful build and synthetic matrix outcomes and explicitly records
`live_deletion_enabled=false` and `runner_root=null`.

The benchmark receipt at
`/Users/leoli/Projects/briefs/agentic-skunkworks-20261016/fleet-audit/native-verification/native-python-benchmark-pr1507-green-66a25994-39090421264.json`
contains six valid rows: native and Python at stale counts 1, 16, and 64.
Each row observed the expected stale-file effect and preserved the busy,
open, recent, and warm fixtures. Native rows include complete recovery,
per-item, post-action, and typed busy-refusal evidence. The primary verdict is
`matched_fixture_effects`.

The timing is diagnostic only: native wall times were 0.598s, 1.659s, and
8.596s; Python wall times were 1.779s, 1.388s, and 2.013s. This is one local
trial over temporary fixtures, so it is not a fleet-throughput or production
speed claim.

## Live-apply boundary

The green rows use the CLI's explicit `--synthetic-fixture` mode. That mode is
accepted only for an enrollment receipt whose install ID begins with
`synthetic-`, uses a private temporary root and state directory, and exercises
the descriptor-bound transaction store against those fixtures. Its process
authority is fixture-local: it identifies the synthetic `Runner.Worker` veto
and the held-open fixture log without scanning the host process table.

The ordinary CLI path still returns `reclaim_not_ready` for `--apply`. The
typed production adapter's descriptor-store admission performs recovery and
rejects pending recovery, but `apply_admitted` and `apply_live` return
`live_apply_disabled` for the descriptor-bound backend. No live runner root,
fleet host, or reusable warm state was mutated by this matrix.

## Remaining production gaps

1. **Production apply integration.** Wire a production command through the
   complete observe → recover → typed admission → bounded store transaction →
   fresh post-action observation sequence. Keep the synthetic backend and
   descriptor-bound backend distinct at the type boundary.
2. **Admission binding depth.** Bind the plan to the exact state-root
   generation, journal digest/length, root and diagnostics identities, policy,
   cancellation epoch, expiry, and batch sequence. A source/tree receipt is
   necessary evidence, but it does not authorize a live filesystem mutation.
3. **Durable cancellation and replay.** Define cancellation at each journal
   phase and make a lost response return the existing terminal transaction
   receipt. The store transaction nonce alone must not permit replaying one
   plan as a new batch.
4. **Crash/restart evidence.** Run the production-shaped adapter through
   prepared, staged, delete-intent, open-staged, replacement, alias-change,
   and repeated-recovery scenarios on a managed Mac, with receipts attached to
   the exact source and artifact.
5. **Live authority proof.** Replace the fixture-local process contract with
   fresh libproc evidence for a real enrolled runner, including complete PID
   scope, busy-worker veto, open-vnode identity, and post-action verification.
6. **Operational rollout.** Add an explicit candidate → soak → promotion gate
   for the first non-synthetic run. Until that receipt exists, live deletion,
   fleet rollout, and performance promotion remain disabled.

## Gate

PR #1507 remains open. The synthetic matrix is accepted as evidence for the
benchmark and adapter boundary only. It is not evidence that live apply is
safe, and it does not authorize merge or promotion by itself.
