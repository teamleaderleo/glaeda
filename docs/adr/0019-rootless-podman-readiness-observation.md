# ADR 0019: Rootless Podman readiness observation

- Status: Superseded by ADR 0020
- Date: 2026-07-25
- Related: issue #77, ADR 0017, ADR 0018

## Supersession note

ADR 0020 supersedes this decision because invoking `podman info` is not reliably read-only: Podman may initialize graph-root state, lock files, and the libpod database while answering the query. This document remains as the historical design that exposed the requirement, but its command sequence must not be integrated into `host plan`.

## Context

Package installation and runner-account preparation are necessary but not sufficient evidence that a dedicated runner user can execute rootless Podman safely. SmolRunner must not build an image, register a runner, or create a preview merely because `/usr/bin/podman` exists.

Rootless readiness depends on facts owned by several boundaries: the exact runner identity, subordinate-ID allocations, the user runtime directory, Podman storage configuration, user-namespace support, networking helpers, and the user systemd session. These facts can be absent, unknown, or conflicting even after packages have been prepared.

The readiness check must remain read-only, run through the reviewed runner-user lane, avoid ambient credentials and configuration, and preserve uncertainty rather than converting probe failures into authorization.

## Decision

SmolRunner will add one typed, read-only rootless Podman readiness report after package and runner-account planning. The report is not a mutation plan and cannot authorize image build, runner registration, or preview creation by itself.

### Preconditions

The observer runs only when all of the following are proven:

1. the exact `/usr/bin/podman`, `/usr/sbin/runuser`, `/usr/bin/env`, `/usr/bin/systemctl`, and `/usr/bin/cat` executables pass reviewed ownership and file-type verification;
2. the runner account, primary group, home directory, subordinate UID range, subordinate GID range, and linger state are matching;
3. the runner UID and primary GID are nonzero;
4. `/run/user/<uid>` exists, is a directory, is owned by the runner UID, and is not group- or world-writable;
5. the package plan contains no unknown or conflicting prerequisite state.

If any precondition is absent, unknown, or conflicting, the readiness report is blocked and no runner-user command executes.

### Sealed execution context

Every probe runs through the reviewed runner-user lane using `/usr/sbin/runuser` and `/usr/bin/env -i`. The only child environment keys are:

- `HOME=<reviewed runner home>`
- `USER=<reviewed runner name>`
- `LOGNAME=<reviewed runner name>`
- `XDG_RUNTIME_DIR=/run/user/<uid>`

No root home, `PATH`, SSH agent, Git configuration, cloud variables, GitHub token, container environment, or caller-provided variable is inherited. Every executable path is absolute and every argument is selected by SmolRunner rather than the manifest.

### Probe sequence

The observer executes the following bounded sequence in order and stops when a dependency is not proven:

1. **Podman engine and storage**
   - `/usr/bin/podman info --format json`
   - Parse only bounded JSON fields needed to establish rootless mode, graph root, run root, storage driver, cgroup manager, and network backend.
   - Require the report to identify rootless operation. The run root must be beneath the reviewed runtime directory. The graph root must be beneath the reviewed runner home or another separately reviewed runner-owned storage root.

2. **User namespace mapping**
   - `/usr/bin/podman unshare /usr/bin/cat /proc/self/uid_map`
   - `/usr/bin/podman unshare /usr/bin/cat /proc/self/gid_map`
   - Accept only mappings consistent with the exact runner UID/GID and reviewed subordinate ranges. Extra, overlapping, truncated, or unparsable mappings are conflicting.

3. **Networking helpers**
   - Verify exact executable metadata for the distribution-selected rootless networking helpers before use.
   - Debian 12 and Ubuntu 24.04 initially accept `/usr/bin/slirp4netns` and `/usr/bin/pasta` only when installed from the reviewed package set.
   - `podman info` must name a backend whose required helper is proven present. An unrecognized backend is unknown, not ready.

4. **User systemd session**
   - `/usr/bin/systemctl --user is-system-running`
   - Accept `running` or `degraded` only when the command reaches the exact user manager through the reviewed runtime directory. Do not inspect or serialize the user manager environment. Do not require systemd to be PID 1 in container-only tests.

5. **Read-only container smoke check**
   - Deferred from the initial observer. Pulling an image, creating a container, writing storage, or starting a service is mutation and belongs to a later explicit reconciliation action.

### Typed outcomes

Each check returns one of:

- `matching`: positive evidence satisfies the reviewed contract;
- `absent`: a required path, helper, mapping, or session is proven missing;
- `unknown`: bounded observation could not establish the fact;
- `conflicting`: evidence contradicts the reviewed runner identity or policy;
- `blocked`: an earlier dependency was not matching, so this check did not execute.

Overall readiness is `ready` only when every required check is `matching`. Any other state blocks image builds, runner registration, and preview execution.

Command nonzero exits are evidence, not transport errors. Spawn failure, output-limit failure, invalid UTF-8 where text is required, oversized JSON, or schema mismatch becomes `unknown` unless stronger conflicting evidence is available.

### Output and evidence

Human and JSON output derive from the same typed report. Evidence includes only bounded, non-secret facts:

- reviewed command argv;
- exit status;
- normalized readiness state;
- selected storage driver and network backend;
- canonicalized non-secret paths;
- normalized UID/GID mappings;
- short recovery guidance.

Raw environment dumps, full Podman configuration, tokens, registry credentials, and unbounded stderr are never serialized.

## Security consequences

This decision prevents executable presence from being mistaken for usable rootless isolation. It preserves the existing plan-before-mutation boundary, keeps repository code away from the Podman socket, and prevents root credentials or ambient operator state from entering runner-user probes.

The observer does not prove that arbitrary repository workloads are safe. It only proves that the host-side rootless Podman prerequisites match the reviewed runner identity. Workload capability, network, filesystem, and resource policy remain separate authorization layers.

## Verification

Implementation must include:

- fake-executor tests for exact argv and the four allowed environment keys;
- fixtures for matching, absent, unknown, conflicting, and dependency-blocked outcomes;
- bounded JSON and mapping parsers with adversarial-size tests;
- Debian 12 and Ubuntu 24.04 acceptance coverage for package/helper selection;
- a real MacBook Linux-VM check for user-systemd and rootless storage behavior;
- proof that no probe writes Podman storage or modifies the user session.

## Follow-up

ADR 0020 replaces this follow-up with a non-mutating static preflight plus a separately journaled first-run Podman smoke verification. No Podman command may be added to `host plan`.
