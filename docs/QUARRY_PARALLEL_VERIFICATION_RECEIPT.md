# Quarry parallel verification receipt

Glaeda has a pure, bounded decoder for Quarry's parallel verification receipt schema v2 in
`src/quarry_parallel_verification_receipt.rs`. Quarry PR #1114 landed the reviewed upstream schema
from exact accepted head `f7fa40ed915ef5af689ec78df91d0d738b128a1e` as merge
`d2ff3bd5e4db630a4a99811b967259ff39579cd9`. This decoder remains a pure supplied-byte boundary;
landing the schema does not authorize a durable execution or settlement path.

## Current boundary

The decoder accepts only Quarry's canonical compact JSON followed by one newline. It enforces the
65,536-byte receipt limit, the 32,768-byte embedded-plan limit, exact schema fields, content IDs,
source and toolchain identities, collection and shard inventories, scheduler and isolation policy,
ordered terminal outcomes, cleanup evidence, and success/failure semantics.

A successful decode has `SuppliedReceiptOnly` authority. It proves only that the supplied bytes are
self-consistent under the reviewed v2 wire contract. It does not prove that:

- Quarry or any subprocess ran;
- the named source, tree, toolchain, tests, or outputs existed;
- Glaeda observed the receipt through its pre-opened bounded channel;
- the receipt belongs to a particular Glaeda request, reservation, attempt, resource grant, or
  deadline;
- Glaeda may publish, reuse, settle, cache, attest, or merge anything.

The decoder performs no filesystem, process, network, queue, cache, signal, persistence, or
publication operations. It intentionally does not map the inner Quarry receipt into
`personal_worker_repository_result`; that requires a later design binding the exact attempt and
outer observation independently of attacker-supplied inner bytes.

## Compatibility evidence

`tests/fixtures/quarry_parallel_verification_receipt_v2.json` was regenerated through the merged
Quarry implementation and matched the retained 1,924 bytes exactly, including plan ID
`quarry-parallel-verification-plan-v2:sha256:cfe9b4fbe2e5f0de0de4073aa233a0408536bcd5af989ec0816e3dd97f3a81bd`
and receipt ID
`quarry-parallel-verification-receipt-v2:sha256:b8305b6bb597489de3b32a47716e9e8e7dd5e522e9e0424ae7f7b9fcc5f7725e`.
The Rust tests decode that fixture and fail closed on
noncanonical framing, extra or duplicate fields, byte-limit violations, stale plan or receipt IDs,
unsafe plan identities, invalid shard ordering and measurements, and contradictory terminal or
cleanup claims.

The rebased Glaeda candidate confirmed the schema constants and validation semantics against merged
Quarry, passed all six focused decoder tests, and passed the repository's complete required profile.
Future Quarry schema or validation drift requires a new fixture regeneration and compatibility
review before this decoder can accept it.
