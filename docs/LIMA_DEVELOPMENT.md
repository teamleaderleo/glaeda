# Lima development environment

The Lima integration is an optional macOS development and live-integration convenience. It is not SmolRunner's production installation contract. SmolRunner itself continues to target an ordinary Debian or Ubuntu host with systemd, cgroup v2, rootless Podman, and a native SmolRunner binary.

## Commands

From the repository root:

```bash
make vm-create
make vm-bootstrap
make vm-check
make vm
make vm-stop
```

`vm-create` installs Lima with Homebrew when necessary and creates the `smolrunner` instance from `examples/lima/smolrunner-interactive.yaml`. It preserves an existing instance and never deletes or replaces one.

`vm-bootstrap` starts the instance and idempotently prepares a development guest. It installs Ubuntu build and rootless-Podman prerequisites, disables unused rootful Podman socket and automatic-update units, installs the stable Rust toolchain with rustup when absent, creates or safely fast-forwards the guest checkout, builds with the committed lockfile, and runs `smolrunner doctor`.

`vm-check` is read-only. It reports host and guest resource use, rejects broad host mounts and an active rootful Podman socket, prints the rootless Podman execution mode, checks the guest checkout, and runs `smolrunner doctor`.

The existing `vm`, `vm-up`, `vm-tmux`, `vm-status`, `vm-sync`, `vm-doctor`, `vm-observe`, and `vm-stop` commands remain available for normal operation.

## Safety boundary

The checked-in template deliberately uses native Apple Virtualization (`vz`) with an ARM64 Ubuntu guest, no host directory mounts, no port forwarding, no Rosetta, no containerd, no SSH-agent forwarding, and no inherited proxy environment.

The bootstrap does not:

- register a GitHub Actions runner;
- request or store GitHub credentials;
- install Caddy or publish preview ports;
- expose either the rootful or rootless Podman API socket;
- delete, reset, or resize a Lima instance;
- prepare a production host.

The Lima login user has passwordless sudo for VM administration. Repository jobs must not run directly as that administrative user once a real GitHub Actions runner is introduced. Runner registration belongs behind SmolRunner's dedicated non-sudo account and rootless-container execution boundary.

## Overrides

The helpers accept:

```text
SMOLRUNNER_VM
SMOLRUNNER_GUEST_REPO
SMOLRUNNER_REPO_URL
SMOLRUNNER_REPO_REF
SMOLRUNNER_LIMA_CONFIG
```

The defaults are the `smolrunner` instance, `/home/lima/smolrunner`, this repository's HTTPS URL, the `main` branch, and the checked-in Lima template.

## Future convergence

The guest bootstrap is development scaffolding, not a second host-management implementation. As durable host reconciliation becomes executable, the intended flow is:

```text
Lima helper
  -> create a plain supported Linux guest
  -> install a checksum-verified SmolRunner release
  -> smolrunner host plan
  -> smolrunner host prepare
  -> live integration checks
```

At that point, package, account, subordinate-ID, state-directory, Podman, and systemd mutations should move out of the shell helper and into SmolRunner's typed, journaled, plan-before-mutation implementation. The helper should remain responsible only for the macOS-to-Linux VM boundary and development checkout.

Destructive reset/delete commands should be added only with an explicit confirmation value and a bounded pre-destruction observation report. Automatic GitHub runner registration should remain a separate command with short-lived token handling.
