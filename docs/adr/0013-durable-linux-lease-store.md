# ADR 0013: Durable Linux lease-store publication

## Status

Accepted for process-durable lease records beneath one trusted installation directory.

## Context

ADR 0005 defined an atomic lease-store contract and supplied an in-memory implementation. That proves no-replace creation, optimistic revisions, and legal lifecycle transitions, but it does not survive process or host restart.

The first durable adapter must preserve the same contract without introducing a database or a daemon. It must also follow SmolRunner's existing filesystem policy: bounded documents, validated path components, restrictive permissions, descriptor-relative operations, explicit locking, atomic publication, and fail-closed handling of foreign or malformed state.

## Decision

On Linux, `LinuxLeaseStore` binds to one validated installation identity and one already existing installation directory. It opens and retains the exact installation directory descriptor, then creates or validates:

- `leases/`, a real directory with mode `0750` and the installation owner;
- `leases.lock`, a persistent empty regular file with mode `0600`, one link, and the installation owner;
- `leases/LEASE_ID.json`, one private validated lease document per lease selector.

Loads open the final lease file with `O_NOFOLLOW`, verify type, link count, mode, owner, and bounded size, decode the versioned document, and require the persisted identity to match the selector and store installation.

Creates and replacements encode the complete next document before locking. Mutations then acquire a nonblocking exclusive advisory lock on `leases.lock`, re-read authoritative state while holding that lock, and stage a private file in the same directory. The staged file is written completely, synchronized, and inspected before publication.

Creation publishes with `RENAME_NOREPLACE`. Replacement verifies the expected revision and exact one-step revision advance, then atomically renames over the existing final file. The lease directory is synchronized after publication.

## Crash behavior

Before rename, the prior authoritative document remains unchanged. A process death may leave an unreferenced hidden temporary file, but that file is never selected as authoritative state.

After rename, the new final file is authoritative. A subsequent directory-sync failure means durability is uncertain and returns an I/O error; callers must reload before retrying rather than assuming the publication did or did not persist.

Temporary files created by a live failed operation are removed by a guard. Scavenging temporary files left by process or host death is deferred to explicit recovery work.

## Security consequences

- Lease creation cannot replace an existing selector.
- Revision-checked replacement is serialized across cooperating processes.
- Symlinks, hard links, broad modes, wrong owners, malformed documents, oversized documents, and selector mismatches fail closed.
- Retained directory descriptors prevent later path replacement from redirecting an opened store.
- Repository code receives neither the state-directory descriptor nor the mutation lock.

The advisory lock coordinates SmolRunner writers. An actor with direct write access to the installation directory remains inside the trusted host-administration boundary.

## Deferred work

- Enumeration and bounded catalog scans.
- Orphan temporary-file recovery.
- Expiry deadlines and clock recovery.
- Cleanup-journal integration.
- CLI read and transition commands.
- Installation deletion and archival policy.
