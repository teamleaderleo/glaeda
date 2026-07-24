# ADR 0018: Conservative runner account preparation observation

- Status: Accepted for read-only host observation
- Date: 2026-07-24

## Context

The dependency-aware runner account plan needs more than name existence. It must distinguish a safely matching primary group and runner user from missing, ambiguous, malformed, stale, or conflicting state. It also needs exact evidence for the desired home directory, subordinate UID/GID ranges, and systemd linger marker.

Reading only `/etc/passwd` and `/etc/group` is not sufficient because Debian and Ubuntu may resolve accounts through NSS. Conversely, a failed NSS lookup must never be treated as proof that an account is absent. Subordinate-ID files and filesystem markers also require bounded, no-follow inspection so symlinks, hard links, writable authority files, or ranges owned by another account cannot authorize mutation.

## Decision

### NSS lookups

The observer uses only these absolute, shell-free commands with an empty environment:

- `/usr/bin/getent group GROUP`;
- `/usr/bin/getent passwd USER`.

A receipt is accepted only when its argv and environment match the reviewed command exactly.

- exit status `0`, empty stderr, bounded newline-terminated stdout: parse one exact record;
- exit status `2`, empty stdout, and empty stderr: absent;
- execution failure, any other status, stderr, oversized output, NUL data, malformed records, or receipt mismatch: unknown.

A matching group has the exact desired name, a nonzero canonical GID, and no supplementary members. A group containing other users is conflicting because it is not dedicated to the runner. A matching user has the exact desired name, nonzero UID and primary GID, the desired home, `/usr/sbin/nologin`, and a primary GID equal to the matching group GID. An existing incompatible user is conflicting. When the user fields match but the primary-group lookup is unknown, the user remains unknown rather than being mislabeled as conflicting.

### Home directory

The desired home path is inspected without following a final symlink.

- missing: absent;
- inspection failure: unknown;
- exact directory, mode `0750`, and owner/group equal to the matching user identity: matching;
- any other existing object or metadata: conflicting.

An existing home cannot be considered matching until the group and user identities are both matching.

### Subordinate ID authorities

The configured subordinate UID and GID files are opened with `O_NOFOLLOW`, bounded to one mebibyte, and accepted only as single-link regular files owned by root:root and not writable by group or others. Missing or unsafe authority files are unknown, not absent.

The whole trusted authority is parsed, not only lines naming the desired user. Every nonempty entry must have a bounded valid owner and canonical, nonempty, nonoverflowing range. This is required to prove that the desired allocation does not overlap another account.

- no entry for the desired user and no cross-owner overlap: absent;
- exactly one desired-user range equal to the desired allocation, no additional desired-user ranges, no cross-owner overlap, and a matching user: matching;
- malformed data anywhere in the authority: unknown;
- cross-owner overlap, additional desired-user ranges, stale ranges, or different ranges: conflicting.

### Linger marker

The marker at `/var/lib/systemd/linger/USER` is inspected without following a final symlink.

- missing: absent;
- inspection failure: unknown;
- protected empty single-link root-owned regular file with a matching user: matching;
- any other existing state, including a hard link: conflicting.

### Partial observation

The observer returns the existing six-resource `RunnerAccountObservations` contract. Unsafe evidence for one resource does not erase safe evidence for another, but dependencies in ADR 0017 still block downstream mutations. The observer also returns a sealed UID/GID identity only when both the group and user observations match.

### Scope boundary

This ADR adds read-only observation only. It does not allocate subordinate ranges, change `host plan`, execute account commands, or enable apply.

## Consequences

- NSS-aware account absence can be distinguished from lookup failure.
- Unsafe authority files, symlinks, hard links, residual allocations, cross-owner overlaps, and stale linger markers cannot become absence.
- The account planner can consume observations directly without weakening its dependency rules.
- Host-plan integration and durable execution remain separate reviewed slices.
