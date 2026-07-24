# ADR 0005: Atomic lease stores and immutable artifact identity

## Status

Accepted for the exploratory leased-execution layer.

## Context

Lease transitions need to survive concurrent callers without accepting stale updates. Preview requests also need immutable build evidence so repeated requests can reuse one verified result instead of rebuilding or deploying every agent push.

The existing Linux state writer already serializes installation-local mutations and publishes synchronized temporary files through atomic rename. The lease model should expose the concurrency requirement before binding itself to that implementation.

## Decision: atomic lease-store contract

SmolRunner defines a narrow `LeaseStore` contract with three operations:

- load a lease by installation and lease ID;
- create a lease with atomic no-replace semantics;
- replace a lease only when its persisted revision matches the caller's expected revision.

`replace_if_revision` must compare the current revision and publish the replacement while holding one exclusive write boundary. A read followed by an unlocked write violates the contract.

Lease revisions advance exactly once per accepted transition. Stale callers receive a conflict and must reload current state before planning another transition.

The first `MemoryLeaseStore` implementation proves the contract and supports deterministic tests. It provides no process durability. A Linux implementation will perform decode, comparison, temporary-file publication, rename, and directory synchronization while holding the existing installation-local lock.

## Decision: versioned lease documents

Persisted leases use bounded, newline-terminated JSON with schema version 1. Decoding:

- rejects oversized documents;
- rejects unknown fields and schema versions;
- revalidates lease and installation identifiers through canonical constructors;
- rejects unknown lease kinds and states.

Serialized strings cannot bypass identifier validation.

## Decision: immutable artifact identity

An artifact identity consists of:

- a validated `owner/name` repository reference;
- a complete 40-character SHA-1 or 64-character SHA-256 Git object ID;
- an artifact kind;
- a canonical SHA-256 content digest.

Initial artifact kinds are:

- OCI image;
- static archive;
- committed-source archive.

Locations, registry names, workflow run IDs, preview URLs, and mutable tags are metadata or retrieval hints. They do not establish artifact identity.

One verified artifact may later be promoted to several execution targets without rebuilding. Provider adapters must consume immutable identity and separately authorized retrieval information.

## Security consequences

- Lease IDs remain locators, never ownership evidence by themselves.
- Atomic revision checks prevent lost updates from concurrent agents or workflow jobs.
- Unknown or corrupt persisted documents fail closed.
- Full Git object IDs prevent branch or abbreviated-SHA substitution.
- Content digests prevent mutable tag or filename substitution.
- Repository code receives neither lease-store write access nor container-control sockets.

## Deferred work

- Linux lease-store implementation under the installation lock.
- Lease expiry timestamps and clock policy.
- Artifact retrieval records and authorization.
- Static archive format and extraction limits.
- OCI registry authentication.
- Preview-slot supersession policy.
