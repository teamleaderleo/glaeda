# ADR 0016: Conservative Debian-family prerequisite package planning

- Status: Accepted for read-only host preparation planning
- Date: 2026-07-24

## Context

SmolRunner needs Git, Podman, subordinate-ID helpers, rootless storage and networking helpers, and a usable systemd user session before it can prepare a dedicated runner account. The existing host plan reports missing command names but does not identify a supported package-management boundary, distinguish package presence from command presence, attach rollback semantics, or retain the evidence that justified an installation action.

Package installation is not a reversible file copy. Debian maintainer scripts, dependency installation, service activation, and package-manager recovery may change host state beyond the named packages. Unknown package state must therefore never be treated as absence, and a failed or interrupted install must be followed by fresh observation rather than a blind retry.

## Decision

### Supported distribution identity

This slice supports only exact `ID=debian` and `ID=ubuntu` identities with a bounded `VERSION_ID` read from `os-release`. The parser reads only the fields it needs, rejects duplicates and malformed quoting, does not evaluate shell syntax, and fails closed for all other distribution IDs.

`ID_LIKE` is not sufficient authorization for package mutation. A derivative may use different repositories, package names, defaults, or service behavior even when it declares Debian compatibility.

### Fixed prerequisite bundle

The reviewed package bundle is:

- `git`;
- `podman`;
- `uidmap` for `newuidmap` and `newgidmap`;
- `slirp4netns` for broadly available rootless networking;
- `fuse-overlayfs` for a reviewed rootless storage helper;
- `dbus-user-session` for systemd user-session integration.

The bundle is versionless. Repository and security policy remain under the operator's configured Debian or Ubuntu package sources.

### Observation and mutation rule

Each fixed package is observed as present, absent, or unknown. Missing observation keys are unknown.

- all present: the package plan is ready and contains no mutation;
- any unknown: the plan is inspection-only and contains no mutation, even when another package is proven absent;
- all known with at least one absent: the plan produces one root-lane mutation and one reviewed `apt-get install --yes --no-install-recommends` command containing only the proven-absent fixed packages.

The mutation records exact distribution and package-state evidence. Package installation is classified as **compensating**, not reversible. Automatic package removal is not claimed to restore the previous dependency, configuration, service, or maintainer-script state.

### Scope boundary

This ADR does not add a package-state probe, connect the plan to the CLI, execute `apt-get`, prepare accounts or subordinate IDs, or enable any apply command. Those remain separate slices requiring bounded observation and durable reconciliation integration.

## Consequences

- Debian and Ubuntu package planning can be tested without root or a live package manager.
- Unsupported derivatives and incomplete package observations remain protected from mutation.
- The exact package bundle and rollback class are reviewable before execution integration.
- A later package probe must populate the same present, absent, and unknown contract rather than bypass it.
- A later executor must re-observe package state after nonzero, interrupted, or uncertain process results.
