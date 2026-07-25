# ADR 0020: Split rootless Podman preflight from first-run smoke verification

- Status: Accepted
- Date: 2026-07-25
- Supersedes: ADR 0019 rootless Podman readiness observation
- Related: issue #77, issue #103, ADR 0017, ADR 0018

## Context

SmolRunner needs evidence that a dedicated runner account can use rootless Podman before it builds project images, registers a GitHub Actions runner, or starts a preview. ADR 0019 attempted to collect that evidence inside the read-only `host plan` command by invoking `podman info` and `podman unshare` through the sealed runner-user lane.

That boundary is incorrect. Podman commands that appear observational can initialize container storage and libpod state. Upstream reports show `podman info` creating graph-root directories, lock files, and a libpod database when no prior state exists. A command with that behavior cannot run inside `host plan`, whose contract forbids filesystem, user, service, container, route, lease, and GitHub mutations.

Static host facts remain observable without starting Podman. Actual rootless-engine validation remains necessary, but it must be treated as a first-run reconciliation action with explicit journal and recovery semantics.

The boundary correction is grounded in upstream Podman evidence:

- `podman info` recreated storage directories, locks, and libpod state in [containers/podman discussion #18295](https://github.com/containers/podman/discussions/18295);
- debug output from [containers/podman issue #11539](https://github.com/containers/podman/issues/11539) shows `podman info` initializing the libpod state database.

## Decision

SmolRunner will split rootless Podman readiness into two stages.

### Stage 1: non-mutating static preflight

`host plan` may inspect only bounded host state that does not invoke Podman or enter a Podman-created namespace:

1. exact executable metadata for `/usr/bin/podman`, `/usr/sbin/runuser`, `/usr/bin/env`, `/usr/bin/systemctl`, `/usr/bin/newuidmap`, `/usr/bin/newgidmap`, `/usr/bin/slirp4netns`, and any later distribution-selected helper;
2. the exact runner account, primary group, home directory, subordinate UID range, subordinate GID range, and linger state;
3. `/run/user/<uid>` existence, directory type, ownership by the runner UID, and rejection of group- or world-writable modes;
4. cgroup v2 and systemd host prerequisites already represented by bounded host observations;
5. bounded parsing of reviewed system and runner-specific containers configuration files without executing the runtime;
6. storage and runtime paths derived from explicit policy, not from a first invocation of Podman;
7. package state for Podman, `uidmap`, the selected rootless networking helper, `fuse-overlayfs`, and `dbus-user-session`.

Static preflight outcomes remain `matching`, `absent`, `unknown`, `conflicting`, and `blocked`. Static preflight can prove that prerequisites are suitable for a first-run check, but it cannot claim that Podman itself is operational.

No `podman`, `podman unshare`, image, container, storage migration, or user-service command may execute from `host plan`.

### Stage 2: explicit first-run smoke verification

After durable host reconciliation has satisfied the static preflight, SmolRunner may plan a runner-user smoke verification as an explicit mutation-capable action. The action:

1. runs through the reviewed runner-user lane with an empty environment and only `HOME`, `USER`, `LOGNAME`, and `XDG_RUNTIME_DIR` supplied;
2. verifies all required executables immediately before execution;
3. records that Podman may create graph-root and run-root directories, locks, databases, and related runtime state;
4. invokes a bounded sequence such as `podman info --format json`, namespace-map inspection, and a later explicitly approved local-image or no-network smoke check;
5. captures only bounded, normalized, non-secret evidence rather than raw Podman configuration;
6. publishes attempted and durable journal checkpoints before and after each command;
7. requires a fresh observation barrier after interruption, nonzero exit, output-limit failure, or conflicting state;
8. never automatically deletes pre-existing Podman storage during rollback or compensation.

The first-run smoke action is successful only when rootless identity, storage roots, namespace mappings, networking helper selection, and user-systemd behavior match the reviewed policy. Success authorizes later planning; it does not itself authorize repository workload execution.

### Existing storage

If the runner account already has Podman state, SmolRunner must classify ownership and compatibility before any smoke action. Unknown, foreign, conflicting, or unexpectedly populated storage blocks execution. Name, path, or account ownership alone does not prove SmolRunner ownership.

SmolRunner must not run `podman system reset`, delete graph-root contents, or rewrite storage configuration as an automatic recovery step.

## Security consequences

This split restores the no-mutation guarantee of `host plan`. It also prevents a supposedly observational command from silently publishing durable runtime state before the reconciliation journal exists.

The smoke verification becomes more operationally expensive because it requires durable execution and recovery infrastructure. That cost is intentional: first-use Podman initialization is a host change and must receive the same review, evidence, and interruption treatment as other host changes.

Static preflight still cannot prove workload isolation. Container capability, filesystem, network, source, artifact, and resource-limit authorization remain separate layers.

## Verification

Implementation must include:

- tests proving `host plan` never executes a program whose inner executable is Podman;
- bounded parsers for reviewed containers configuration files and explicit storage policy;
- exact metadata tests for namespace and networking helpers;
- durable-journal interruption tests around the first-run smoke sequence;
- fixtures for fresh, matching, pre-existing unknown, conflicting, and interrupted Podman state;
- Debian 12 and Ubuntu 24.04 acceptance checks;
- a real MacBook Linux-VM check that records every path created by first-run `podman info` and namespace inspection;
- proof that no automatic recovery deletes operator-owned or unclassified Podman data.

## Follow-up

The next implementation slice should add the non-mutating static preflight without touching the active host-readiness-verdict and subordinate-ID reconciliation lanes. The first-run smoke action remains blocked until durable package and account reconciliation execution can journal runner-user commands and recover after interruption.
