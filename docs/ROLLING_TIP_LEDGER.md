# Rolling-tip ledger MVP

The `rolling_tip_ledger` module is the first Glaeda integration boundary for a
continuously building development branch. It is scoped to one stream, such as
`manaflow-ai/cmux@feat-cmux-next`, and is intentionally path-free and
transport-independent.

Each `BuildKey` includes the repository and branch, complete commit and tree
IDs, toolchain, target, platform, architecture, configuration, and an optional
bundle key. A published artifact carries that key plus content digest and
producer run provenance. The ledger therefore reuses only an exact build and
does not treat a mutable `latest` label as identity. Publication also records
the execution attempt that produced the bytes, and Glaeda accepts the artifact
only when that exact attempt is currently `succeeded`.

The event envelope is append-only and typed. It covers tip observation, build
execution state, host leases, artifact publication, reuse hit/miss/in-flight
deduplication, promotion requests and decisions, backpressure, and closed
receipts. Execution state is separate from promotion state: a newer tip can be
delayed or rejected while an older build continues running, and no operation
automatically cancels work.

Promotion uses a compare-and-swap revision. `accept`, `delay`, and `reject`
replace the candidate pointer only when the caller supplies its current
revision. `accept` additionally requires a published artifact bound to the
current successful execution attempt; `delay` and `reject` can record an
operator decision for a requested tip before a successful artifact exists. A
stale caller receives a conflict and cannot clobber a newer operator decision.
The in-memory ledger is the MVP kernel; a later Glaeda
adapter can persist envelopes and publish idempotent events to fleet
intelligence without changing these identities or transitions.

The first consumer should ingest the existing CI receipts for the
`feat-cmux-next` stream. Scheduler changes, host selection, cancellation, and
workflow edits remain outside this tranche.
