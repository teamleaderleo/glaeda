# Protected cache replacement equivalence

The protected cache-generation catalog cannot treat “a command exited successfully” or “a target
directory exists” as proof that one reconstructed generation may replace another. Replacement
equivalence needs a closed, path-free identity contract before a physical producer can be reviewed.

`protected_cache_replacement_equivalence` defines that first contract for `cargo_target_v1`. One
canonical receipt binds:

- the protected namespace, cache-state identity, and newly materialized generation identity;
- the exact repository, commit, and Git tree;
- a canonical reconstruction-input digest;
- exact reconstruction-plan, validator, and toolchain-envelope generation digests;
- the declared Cargo-target family semantic digest; and
- the fixed `equivalent` outcome vocabulary.

All fields participate in exact correlation. The codec is bounded, rejects unknown or noncanonical
JSON, and carries no filesystem paths, command arguments, output, logs, environment, credentials,
or lease data.

## Authority boundary

Decoding a document reports `supplied_receipt_only`. Even an exact expected-field correlation
returns only `exact_supplied_receipt`. Neither result can:

- construct the protected catalog's current-transition authorization;
- adopt or publish a physical cache generation;
- prove a filesystem path, producer, validator execution, or successful personal-worker attempt;
- authorize cache reuse, reset, quarantine, eviction, deletion, or cleanup; or
- weaken catalog-wide recovery or namespace-wide lease vetoes.

There is intentionally no public or naked crate-wide success constructor. The Unix physical
producer is the only reviewed construction path: it accepts only an absent caller-named candidate
beneath an exact owner-private root, keeps its receipt root distinct and non-nested, executes
content-digest-bound direct ELF materializer and validator plans with exact argv, empty environment,
bounded output, and wall-clock limits, derives the materialized tree identity, synchronizes the
output, and atomically persists the canonical receipt. The receipt constructor accepts only the
producer's unconstructible seal. Caller-authored JSON and reloaded receipt bytes never become
physical-production authority.

Raw program and production-plan constructors additionally require an unforgeable authority token
that has no production constructor. There is no external call path that can nominate an arbitrary
program (for example, a trivial always-successful executable) as the Cargo semantic validator. A
later Cargo-specific adapter must be reviewed separately to add a module-owned token construction
path and bind the plan to canonical reconstruction inputs and an observed toolchain envelope.

The derived tree identity covers sorted raw entry names, object kinds, modes, regular-file bytes,
sizes, and hard-link topology. Symlinks and special files fail closed. Hard links are accepted only
when every link reported by the inode is observed beneath the fresh candidate; a link outside the
candidate remains unsafe. Traversal is bounded to 2,000,000 entries, 64 GiB of logical regular-file
bytes, and 64 levels. Every opened object is revalidated around hashing. Files and directories are
`fsync`ed before receipt publication.

Receipt persistence uses one owner-private stage and no-replace final name per cache-state ID. Its
durability order is stage-file `fsync`, receipt-directory `fsync`, no-replace rename, then a second
receipt-directory `fsync`. Exact duplicates are allowed; a different receipt for the same state is
a conflict. A complete canonical abandoned stage can be recovered, while incomplete or conflicting
staging debt fails closed. Producer failure never removes or adopts the caller-owned candidate.

## Remaining gates

This vocabulary completes only the schema/equality portion of the replacement-equivalence gate.
Current Big Red Cargo targets remain unmanaged and unknown. Before any generation can be current,
reused, or reclaimed, Glaeda still needs:

1. independent exact-head acceptance of the descriptor-bound physical producer;
2. a Cargo-specific sealed plan adapter binding canonical inputs and the observed toolchain
   envelope to the physical producer;
3. fresh namespace-wide personal-worker lease visibility;
4. a sealed adapter correlating live producer authority with the independently accepted protected
   store transition from PR #884; and
5. a read-only adapter joining catalog, equivalence, lease, and live lock/mount/open/process evidence
   into cache inventory.

Missing or conflicting evidence remains a cold reconstruction or `unknown`, never an optimistic hit.
