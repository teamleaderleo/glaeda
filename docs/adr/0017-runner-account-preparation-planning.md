# ADR 0017: Dependency-aware runner account preparation planning

- Status: Accepted for read-only account preparation planning
- Date: 2026-07-24

## Context

A dedicated runner account depends on several host resources that cannot be planned independently. The primary group must be safe before a user can reference it. The user identity must be safe before SmolRunner can assign a home directory, subordinate UID/GID ranges, or systemd linger. Names alone do not prove identity, and a partially matching account must not be repaired as though it were absent.

The existing command layer already contains reviewed group, system-user, and linger commands. It does not yet encode exact home-directory or subordinate-range changes, nor does it express how unknown or conflicting identity evidence blocks downstream work.

## Decision

### Desired state

One runner-account plan records:

- a validated runner username;
- a validated primary group name;
- a canonical non-root absolute home path;
- one exact subordinate UID range containing at least 65,536 IDs;
- one exact subordinate GID range containing at least 65,536 IDs.

Range allocation remains an explicit caller decision. This planner does not scan for or invent free host ranges.

### Observation states

Each of six resources is classified with bounded public evidence as:

- **matching** — exact identity and policy evidence matches the desired resource;
- **absent** — bounded inspection proves the resource is not present;
- **unknown** — inspection is incomplete or ambiguous;
- **conflicting** — an existing resource is incompatible or belongs elsewhere.

The resources are ordered as group, user, home directory, subordinate UIDs, subordinate GIDs, and linger.

A matching user is valid only with a matching primary group. A matching home, subordinate range, or linger state is valid only with a matching user. Inconsistent classifications fail closed.

### Dependency rule

A matching or proven-absent group is viable: absence may be satisfied by the preceding planned group-creation action. Unknown or conflicting group state blocks the user and all later resources.

A matching or proven-absent user is viable only when the group is viable. Unknown or conflicting user state blocks home, subordinate ranges, and linger.

Individual unknown resources remain inspection-only. Individual conflicts are blocked. No blocked or inspection-only item contains a mutation or command.

### Reviewed commands

Proven absence may produce these root-lane commands:

- `groupadd --system GROUP`;
- `useradd --system --gid GROUP --home-dir HOME --shell /usr/sbin/nologin --no-create-home USER`;
- `install --directory --mode 0750 --owner USER --group GROUP -- HOME`;
- `usermod --add-subuids FIRST-LAST -- USER`;
- `usermod --add-subgids FIRST-LAST -- USER`;
- `loginctl enable-linger USER`.

All arguments are constructed from validated typed names, canonical paths, and checked numeric ranges. Commands use absolute executable paths, an empty environment, and no shell.

### Rollback classification

Every account-preparation action is **compensating**. Removing a group, user, home directory, subordinate range, or linger state automatically cannot honestly promise restoration after later files, services, processes, or configuration have begun using it.

### Scope boundary

This ADR adds a pure plan and reviewed commands only. It does not inspect live passwd/group/subordinate-ID files, allocate ranges, execute commands, change the existing `host plan` CLI, or enable apply.

## Consequences

- Account preparation can be reviewed and unit-tested without root.
- Unknown and conflicting identity evidence cannot become a mutation through dependency ordering.
- A later observation layer must produce exact classifications and evidence rather than raw booleans.
- A later durable reconciliation layer must revalidate every precondition before execution and after uncertain process results.
