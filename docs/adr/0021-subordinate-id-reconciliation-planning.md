# ADR 0021: Subordinate-ID reconciliation planning

- Status: Accepted for read-only planning
- Date: 2026-07-25
- Related: issues #3, #12, #103; ADR 0015, ADR 0017, ADR 0018, ADR 0020

## Context

Rootless Podman can be installed and still fail while unpacking ordinary images when the exact runner account lacks usable subordinate UID and GID mappings. The authority is the complete protected `/etc/subuid` or `/etc/subgid` file. A username match alone cannot establish that a range is safe because another record may overlap it, the owner may appear more than once, or unrelated records may conflict globally.

A mapping change also invalidates assumptions held by existing rootless Podman state. The refresh must run as the dedicated runner account through the reviewed sealed runner-user lane after fresh authority observations prove the new mappings.

## Decision

### Complete bounded authority parsing

SmolRunner parses each trusted authority snapshot as a whole. Input is capped at 1 MiB, 16,384 records, and 4,096 bytes per row. Every row must contain one reviewed account name or canonical positive numeric owner, one canonical positive start, and one canonical nonempty nonoverflowing count.

Empty authority files are valid. Empty rows, NUL data, missing final newlines, extra fields, noncanonical numbers and overflowing ranges are malformed. Any owner appearing more than once is conflicting. When the runner UID is freshly proven, its numeric owner form and username form are aliases; multiple records across those aliases are also conflicting. Any pair of ranges that intersects is conflicting. Exactly adjacent ranges are valid.

Malformed authority remains `unknown`. Duplicate-owner and overlap evidence is `conflicting`. Both outcomes block mutation.

### Deterministic allocation

An exact range from account policy or durable ownership evidence is verified exactly. A valid existing single allocation is preserved when an allocation request permits it. A single numeric-owner allocation is adopted only when fresh identity evidence proves that UID belongs to the runner account. An absent owner may receive the lowest free range containing at least 65,536 IDs inside a caller-reviewed allocation window.

The allocation window is explicit input. SmolRunner has no global development range. Exhaustion is a typed blocked outcome. Selection never rewrites an authority file and never adopts an incompatible owner record by username alone.

### Reconciliation actions and barriers

A proven-free exact range produces only the reviewed root-lane command:

- `/usr/sbin/usermod --add-subuids FIRST-LAST -- USER`; or
- `/usr/sbin/usermod --add-subgids FIRST-LAST -- USER`.

Each command carries a mandatory fresh-observation barrier for its complete authority file. Process exit status cannot establish success. Future apply work must re-read the protected file and prove the exact owner and range before continuing.
The read-only host report removes the older generic account-plan copies of subordinate-ID mutations, so this barrier-bearing plan is the sole mapping-action source in `host plan`.

When either mapping changes, the plan requires this runner-user action after all mapping barriers pass:

- `/usr/sbin/runuser --user USER -- /usr/bin/env --ignore-environment HOME=HOME USER=USER LOGNAME=USER XDG_RUNTIME_DIR=/run/user/UID /usr/bin/podman system migrate`.

The command receives no ambient root or operator environment. Root execution is rejected by its typed lane. A missing exact runner identity leaves migration blocked until fresh identity evidence exists.

### Scope boundary

This decision adds parsing, allocation selection, typed reconciliation actions, fresh-observation barriers, runner-user migration planning and read-only `host plan` output. It adds no apply path. Durable journal sequencing, mutation execution, and ADR 0020 static-preflight and first-run verification remain required before host preparation can change a machine. Read-only `host plan` may display the reviewed migration argv but never invokes Podman.

## Security consequences

- Global authority conflicts cannot authorize a mapping mutation.
- Development VM allocations cannot become universal defaults.
- `usermod` success cannot bypass fresh authority evidence.
- Podman namespace migration cannot run as root or inherit root credentials.
- Existing valid mappings remain stable unless exact reviewed evidence requires a different result.

## Verification

The implementation includes fixtures for empty files, exact and multiple owners, adjacent ranges, malformed rows, duplicate owners, global overlaps and exhausted windows. Fake lane execution verifies the exact sealed Podman migration argv and empty outer environment. ARM64 Linux compile coverage remains in the main verification workflow.
