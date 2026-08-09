# Ubuntu 24.04 Podman closure evidence

Status: partial R01 package/first-use evidence only. This is not a runnable verification backend
and does not authorize a physical Lima or host mutation.

## Exact evidence

- Fixture commit: `81bac8c2672ae04e6750feeb4fa896d3f320dbaa`
- Workflow run: [31313212518](https://github.com/teamleaderleo/smolrunner/actions/runs/31313212518)
- ARM64 job: [93244186699](https://github.com/teamleaderleo/smolrunner/actions/runs/31313212518/job/93244186699)
- x86_64 job: [93244186729](https://github.com/teamleaderleo/smolrunner/actions/runs/31313212518/job/93244186729)
- Date: 2026-08-09
- Result: both jobs passed on fresh GitHub-hosted Ubuntu 24.04 VMs.

The workflow has read-only repository permission and persists no checkout credential. The checkout
action receives only GitHub's ephemeral read-only workflow token; no repository or organization
secret is passed to the probe. Probe cleanup deletes only the exact disposable user, systemd unit,
mount, and temporary root that it creates. It does not contact or mutate the personal Mac/Lima
worker.

## Package baseline

Both architectures reported the same Ubuntu Noble package versions:

| Package | Version |
| --- | --- |
| aardvark-dns | `1.4.0-5` |
| busybox-static | `1:1.36.1-6ubuntu3.1` |
| catatonit | `0.1.7-1` |
| conmon | `2.1.10+ds1-1build2` |
| crun | `1.14.1-1` |
| fuse-overlayfs | `1.13-1` |
| Git | `1:2.43.0-1ubuntu7.3` |
| netavark | `1.4.0-4` |
| Podman | `4.9.3+ds1-1ubuntu0.2` |
| uidmap | `1:4.13+dfsg1-4ubuntu3.2` |

The fixture deliberately disables the GitHub runner image's Git PPA and downgrades Git/Git-man to
the stock Noble 2.43 package. A pass therefore does not accidentally rely on a newer Git lazy-fetch
control.

## Proven facts

The exact fixture proves only these facts on both admitted architectures:

1. The named Podman, Git, conmon, crun, catatonit, fuse-overlayfs, Netavark, Aardvark, and subordinate
   ID helper package paths are root-owned, single-link regular files without group/world write
   access. The subordinate-ID helpers retain setuid, and Podman's packaged catatonit fallback is the
   exact symlink to `/usr/bin/catatonit`.
2. The known Podman pre-exec indicator is absent and the known compiled pre-exec directories are
   absent or empty.
3. Stock Git 2.43 can read a held exact object directory through an inherited descriptor with an
   empty transport allowlist, lazy fetch/replacements disabled, no system/global config, no
   credential or prompt environment, and a protected synthetic bare directory. The fixture
   recomputes the raw tree object ID. It also proves that explicit bare-directory selection does not
   enforce owner safety itself, so SmolRunner must enforce the directory binding and owner gate.
4. A steward-created tmpfs with `size=1048576,nr_inodes=64,nosuid,nodev` reports no more than 1 MiB
   and 64 inodes on either architecture. This proves the primitive, not its future cgroup charging
   or use by Cargo.
5. Noble Podman exposes the fixed global/create/start CLI options required by the contract,
   including the Boolean `podman start --attach` form.
6. Rootless effective storage uses the exact `rootless_storage_path` graph root and derives the run
   root as `<XDG_RUNTIME_DIR>/containers`; generic `graphroot`/`runroot` alone are not authority.
7. `podman info` opens both `cni.lock` and `netavark.lock`. It succeeds when those two exact empty
   `0600` runner-owned inodes are precreated in a root-owned non-writable directory, and it neither
   replaces/resizes them nor creates another network entry.
8. First use reaches the rootless pause-process path. The deliberately absent per-attempt user D-Bus
   socket makes the systemd scope move fail, and the pause process remains inside the exact
   disposable service cgroup. The fixture records the exact runner-owned single-link `0600`
   `pause.pid` inode and canonical numeric contents, then deliberately SIGKILLs the service main.
   Packaged `systemd-run --wait --collect --pipe` reports that signalled unit as status 255 on both
   architectures; collection removes the exact service cgroup and unit and leaves no process for
   the probe UID. Only then does the root fixture revalidate the fixed run-private hierarchy and
   the same stale PID-file inode, remove that fixed file without signalling its numeric PID, fsync
   the exact parent directory, and prove that no PID file or mount beneath the probe root remains.

The successful log markers were `git_materializer=closed`, `target_tmpfs=bounded`,
`podman_cli_surface=expected`, `network_state=precreated-and-stable`,
`rootless_info=success pause=contained crash=armed`,
`pause_crash_recovery=stale-pid-removed-and-synced`, and
`podman_closure_package_probe=pass`.

## Corrections learned from fail-closed probes

Earlier disposable runs were intentionally allowed to fail and corrected these false assumptions:

- Ubuntu installs `runuser` at `/usr/sbin/runuser`, not `/usr/bin/runuser`.
- The GitHub runner image's Git PPA is not the Noble baseline.
- `/usr/libexec/podman/catatonit` is a symlink; the executable is `/usr/bin/catatonit`.
- Git 2.43 does not supply the assumed explicit-bare-directory owner refusal.
- Noble rootless storage requires `rootless_storage_path`; generic `graphroot` is ignored.
- Noble rootless `runroot` is derived from `XDG_RUNTIME_DIR`, not the generic config field.
- First use needs both exact network lock files even when no workload network is admitted.
- Noble Podman 4.9.3 exposes `--passwd` only on `run`, not `create`; the stopped-container flow
  must instead use explicit numeric keep-id mapping and fail closed on generated passwd/group
  state.
- When a transient unit's main is deliberately killed by SIGKILL, packaged
  `systemd-run --wait --collect --pipe` returns status 255 rather than the shell-style status 137;
  the journal/service result must remain the authoritative crash classification.

These are contract changes, not compatibility relaxations: each ambient or mutable edge is now
either made exact or remains blocking.

## Still blocking R01/B06

This fixture does **not** prove any of the following and must not be cited as if it did:

- installation and digest verification of the immutable read-only image store;
- `image inspect`, stopped `create`, generated OCI-spec inspection, the pre-start gate,
  `start --attach`, bounded output capture, exit receipt, or exact container removal;
- AppArmor, seccomp, capability, device, host-file, hostname, resolver, timezone, or `/dev/shm`
  closure in the generated and running container;
- proof that Netavark/Aardvark, fuse-overlayfs, remote transports, auth helpers, hooks, CDI, plugins,
  or alternate runtimes remain unexecuted through a complete container attempt;
- the final delegated cgroup hierarchy, exact CPU/memory/swap/PID controls, control rereads,
  aggregate tmpfs charging, or group-empty cleanup implementation;
- production durable attempt journaling, crash recovery, cancellation, reservation/deadline
  recheck, ownership persistence, authoritative cgroup/file handles, or ambiguous-cleanup handling;
- the Rust R01 readiness object, R02 hostile-payload executor, R03 public receipt, B06 service, B07
  CLI, or a physical stopped-VM run-once acceptance.

Those gaps remain fail-closed work under issues #291, #319, #205, and parent #238.
