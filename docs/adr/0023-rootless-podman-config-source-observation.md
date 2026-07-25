# ADR 0023: Observe trusted rootless Podman configuration sources

- Status: Accepted
- Date: 2026-07-25
- Related: issue #3, issue #77, issue #103, issue #122, issue #128, ADR 0018, ADR 0020, ADR 0021

## Context

ADR 0020 separates non-mutating rootless Podman static preflight from the later journaled first-run smoke verification. PR #109 added bounded parsing for the relevant `containers.conf` and `storage.conf` fields, and PR #124 added pure precedence resolution and explicit-policy assessment.

Those layers intentionally accept typed source states and do not read the host filesystem. Static preflight still needs trustworthy evidence for the exact reviewed configuration files. Treating an unreadable, unsafe, or malformed higher-precedence source as absent could incorrectly expose a lower-precedence value as effective and authorize an unsafe first-run check.

## Decision

Add a Linux-only, read-only source-observation layer for these exact paths:

- `/usr/share/containers/containers.conf`;
- `/etc/containers/containers.conf`;
- `<runner-home>/.config/containers/containers.conf`;
- `/etc/containers/storage.conf`;
- `<runner-home>/.config/containers/storage.conf`.

System paths may be relocated only through an explicitly constructed observation-path value for tests or a separately reviewed host root. Runner paths are never supplied independently. They are derived from the canonical reviewed home and its `.config` XDG root.

### Runner identity gate

Runner-specific sources are inspected only when:

- the exact non-root runner identity has been resolved by the runner-account observer;
- the observed primary group matches the resolved group;
- the reviewed home observation is `matching`.

Otherwise both runner-specific sources are represented as `unknown`. They are not read, and they are not treated as missing.

### File trust contract

Every source is opened one path component at a time from `/` with `O_NOFOLLOW` and `O_CLOEXEC`. Intermediate components must be directories. The final object must satisfy all of the following:

- regular file;
- exactly one hard link;
- size no greater than the bounded parser input limit;
- not writable by group or other users;
- UID and GID `0` for vendor and system sources;
- exact runner UID and primary GID for runner sources.

A missing path is `missing`. Symlinks, permission failures, metadata failures, incompatible type, wrong ownership, multiple links, unsafe modes, oversized input, short or failed reads, and other traversal failures are `unknown`.

### Parsing and evidence

A trusted file is read through its already verified descriptor with a hard byte limit. Invalid UTF-8 and bounded parser failures become `unknown` source evidence. Reports may contain only:

- source role and canonical path;
- `missing`, `present`, or `unknown`;
- short reviewed evidence;
- normalized resolved fields and policy assessments already defined by the resolution layer.

Raw file contents, arbitrary operating-system errors, environment values, and unbounded parser messages are never serialized or rendered.

### Composition

The observer constructs the five typed source values, invokes the pure precedence resolver, invokes explicit-policy assessment, and exposes one composition entrypoint into `RootlessPodmanStaticPreflightReport`.

The composition function accepts no command executor. It does not invoke Podman, `podman unshare`, user services, a shell, or any child process. A matching static report authorizes only planning of the later durable first-run smoke verification described by ADR 0020.

## Consequences

- An unreadable or unsafe higher-precedence source continues to hide lower-precedence values.
- Static preflight gains identity-aware configuration evidence without weakening the no-mutation contract of `host plan`.
- Runner configuration cannot be redirected through an independently supplied XDG path.
- Public human and JSON reports remain useful to agents without becoming a configuration-content or operating-system-error exfiltration path.
- The source observer is reusable by future host-plan composition without coupling it to CLI rendering or mutation execution.

## Deferred work

This decision does not wire the observer into the current host-readiness CLI, create or rewrite configuration files, initialize Podman storage, or perform the first-run smoke action. Those steps remain blocked on their existing host-planning, durable-journal, privilege-lane, and recovery contracts.

## Verification

Implementation must cover:

- all sources missing;
- safe root-owned and runner-owned files;
- runner identity or home not proven;
- symlink traversal;
- wrong owner;
- group- or world-writable mode;
- multiple hard links;
- oversized input;
- invalid UTF-8;
- malformed relevant values;
- unreadable higher-precedence sources;
- raw configuration and arbitrary OS error exclusion from human and JSON reports;
- zero subprocess execution in source observation and static-preflight composition;
- Debian 12 and Ubuntu 24.04 acceptance.
