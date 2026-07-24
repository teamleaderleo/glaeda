# ADR 0018: Conservative runner account preparation observation

- Status: Accepted for read-only host observation
- Date: 2026-07-24

## Context

The dependency-aware runner account plan needs more than name existence. It must distinguish a safely matching primary group and runner user from missing, ambiguous, malformed, stale, or conflicting state. It also needs exact evidence for the desired home directory, subordinate UID/GID ranges, and systemd linger marker.

Reading only `/etc/passwd` and `/etc/group` is not sufficient because Debian and Ubuntu may resolve accounts through NSS. Conversely, a failed NSS lookup must never be treated as proof that an account is absent. Subordinate-ID files and filesystem markers also require bounded, no-follow inspection so symlinks or writable authority files cannot authorize mutation.

## Decision

### NSS lookups

The observer uses only these absolute, shell-free commands with an empty environment:

- `/usr/bin/getent group GROUP`;
- `/usr/bin/getent passwd USER`.

A receipt is accepted only when its argv and environment match the reviewed command exactly.

- exit status `0`, empty stderr, bounded newline-terminated stdout: parse one exact record;
- exit status `2`, empty stdout, and empty stderr: absent;
- execution failure, any other status, stderr, oversized output, NUL data, malformed records, or receipt mismatch: unknown.

A matching group has the exact desired name and a nonzero canonical GID. A matching user has the exact desired name, nonzero UID and primary GID, the desired home, `/usr/sbin/nologin`, and a primary GID equal to the matching group GID. An existing incompatible user is conflicting.

### Home directory

The desired home path is inspected without following a final symlink.

- missing: absent;
- inspection failure: unknown;
- exact directory, mode `0750`, and owner/group equal to the matching user identity: matching;
- any other existing object or metadata: conflicting.

An existing home cannot be considered matching until the group and user identities are both matching.

### Subordinate ID authorities

The configured subordinate UID and GID files are opened with `O_NOFOLLOW`, bounded to one mebibyte, and accepted only as single-link regular files owned by root:root and not writable by group or others. Missing or unsafe authority files are unknown, not absent.

Matching entries are parsed through the existing strict subordinate-range parser.

- no entry for the desired user in a trusted authority: absent;
- exactly one range equal to the desired allocation, with a matching user: matching;
- malformed matching entries: unknown;
- additional, stale, or different ranges: conflicting.

### Linger marker

The marker at `/var/lib/systemd/linger/USER` is inspected without following a final symlink.

- missing: absent;
- inspection failure: unknown;
- protected empty root-owned regular file with a matching user: matching;
- any other existing state: conflicting.

### Partial observation

The observer returns the existing six-resource `RunnerAccountObservations` contract. Unsafe evidence for one resource does not erase safe evidence for another, but dependencies in ADR 0017 still block downstream mutations. The observer also returns a sealed UID/GID identity only when both the group and user observations match.

### Scope boundary

This ADR adds read-only observation only. It does not allocate subordinate ranges, change `host plan`, execute account commands, or enable apply.

## Consequences

- NSS-aware account absence can be distinguished from lookup failure.
- Unsafe authority files, symlinks, residual allocations, and stale linger markers cannot become absence.
- The account planner can consume observations directly without weakening its dependency rules.
- Host-plan integration and durable execution remain separate reviewed slices.
