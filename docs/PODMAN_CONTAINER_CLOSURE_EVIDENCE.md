# Ubuntu 24.04 Podman container-fixture evidence

Status: partial R02 trusted-fixture evidence only. This is not a hostile-repository execution
backend and does not authorize a physical Lima or host mutation.

## Exact evidence

- Fixture commit: `a3381f37bd24c82d8542d88a64e762f7c6b67068`
- Workflow run: [31310912737](https://github.com/teamleaderleo/smolrunner/actions/runs/31310912737)
- ARM64 job: [93238494231](https://github.com/teamleaderleo/smolrunner/actions/runs/31310912737/job/93238494231)
- x86_64 job: [93238494193](https://github.com/teamleaderleo/smolrunner/actions/runs/31310912737/job/93238494193)
- Date: 2026-08-09
- Result: both jobs passed on fresh GitHub-hosted Ubuntu 24.04 VMs.

The workflow has read-only repository permission, persists no checkout credential, and receives no
repository or organization secret. The fixture mutates only its disposable hosted VM. It neither
contacts nor mutates the personal Mac/Lima worker.

## Proven facts

The exact fixture proves only these facts on both admitted architectures:

1. An offline root filesystem made from the installed `busybox-static` executable plus fixed
   numeric image-owned account files can be imported into one fresh transient rootless Podman store.
   The returned image identity and the bounded image-inspection result agree exactly.
2. Noble Podman 4.9.3 accepts the contract's stopped-create controls exercised by this fixture:
   `--pull=never`, fixed init/runtime/conmon paths, no network or hosts generation, private IPC/PID/UTS
   namespaces, read-only rootfs, ignored image volumes, all capabilities dropped, no-new-privileges,
   explicit seccomp, bounded PID/memory/swap/CPU values, no host/proxy environment, bounded private
   logging, no privilege/systemd/restart/healthcheck, numeric keep-id mapping, fixed user/workdir/
   entrypoint, and a bounded noexec/nosuid/nodev `/tmp`.
3. The packaged seccomp source has SHA-256
   `cc374cf23846ce1f62f4dc807a8e2b8673c783c6f56cb475467621035d281e6c` on both architectures.
   Because that packaged pathname is mode `0777` on the hosted images, the fixture never uses it as
   authority directly. Before creating the runner user, root requires the pinned bytes and copies
   them to an attempt-private, root-owned, single-link, `0444` file. The unprivileged phase rechecks
   the digest immediately before passing that exact path to Podman, and the generated OCI spec
   contains a seccomp policy.
4. `podman container init` produces one exact non-running `initialized` object. The generated OCI
   spec is uniquely found below the attempt root before start and preserves UID/GID 1000, umask
   `0022`, supplementary GID 1000, no-new-privileges, an empty capability object, no runtime
   `/etc/passwd` or `/etc/group` mount, and no AppArmor profile. The spec contains exactly one
   pathless IPC, PID, and UTS namespace and no runner-control, proxy, or attempt-private-path process
   environment entry.
5. The stopped-container inspection preserves the exact image/container IDs, numeric user,
   entrypoint, workdir, no-network/read-only/unprivileged state, PID and memory limits, and bounded
   attempt-private log configuration. It contains no host account name.
6. Detached `podman start <exact-id>` returns the exact ID. Bounded exact-ID inspection polling
   reaches one stopped exit with code zero, and bounded `podman logs` returns only the expected
   SHA-256 results for the image-owned account files. The single-link log remains runner-owned,
   non-group/world-writable, nonempty, and below its one-MiB ceiling.
7. Exact container and image removal succeeds. The two precreated network lock inodes remain the only
   network-directory entries, no process for the disposable UID remains, and no mount remains below
   the attempt root.

The successful marker was
`offline_image=exact stopped_create=closed account_files=image-owned apparmor=rootless-unavailable`,
followed by `podman_container_closure_probe=pass`.

## Corrections learned from fail-closed probes

Earlier disposable runs were intentionally allowed to fail and corrected these assumptions:

- `podman import` prints `sha256:<hex>`, while `podman image inspect` reports the bare hexadecimal ID.
- Before explicit initialization, the stopped object reports `created`; after `container init`, it
  reports `initialized`.
- Noble `podman start --attach` and `podman wait` can remain blocked after this short payload has
  exited. The admitted sequence therefore uses detached start, bounded exact-ID inspection polling,
  then bounded log retrieval; neither hanging command is admitted.
- `container init` launches conmon. Its output must go to attempt-private regular files rather than
  command-substitution pipes, and failure cleanup must already own the exact container ID.
- The generated OCI spec is available only after initialization and only while that initialized
  object exists; its package path must be discovered below the exact protected attempt root rather
  than assumed.
- With all capabilities dropped, this Podman/crun pair serializes `.process.capabilities` as an empty
  object, not five empty arrays.
- The packaged seccomp source is mode `0777`; exact bytes must be pinned and copied to a protected
  root-owned snapshot before unprivileged use. Its pathname or package ownership is not authority.
- containers/common 0.57.4 rejects every named AppArmor profile in rootless mode. Failed run
  [31310381494](https://github.com/teamleaderleo/smolrunner/actions/runs/31310381494) confirmed the
  packaged behavior on both architectures. The initial backend makes no AppArmor claim; a future
  outer-service profile requires a separate root-installed and inherited-policy proof.

These are contract corrections, not compatibility relaxations. The trusted fixture remains unable
to authorize hostile repository code until every blocking boundary below is independently closed.

## Still blocking R01/R02/B06

This fixture does **not** prove any of the following and must not be cited as if it did:

- installation, digest verification, and read-only use of the production immutable image store or
  reviewed in-image gate;
- selection and installation of the production seccomp policy, syscall-denial behavior, or any
  outer-service AppArmor confinement;
- descriptor-bound exact Git-tree materialization, read-only source mounting, bounded target tmpfs,
  verified dependency inputs, Cargo/test command expansion, or cache non-authority;
- the final delegated outer/payload cgroup hierarchy, exact aggregate and child CPU/memory/swap/PID
  controls, control rereads, tmpfs charging, group-empty cleanup, or pause-process/PID-file recovery;
- hostile payload behavior, timeout, cancellation, output overflow, descendant escape attempts,
  network syscall denial, mount/device/host-file closure, or log-ceiling failure classification;
- durable attempt journaling, crash recovery, final reservation/deadline recheck, ownership
  persistence, or ambiguous-cleanup handling;
- the Rust R01 readiness object, R02 hostile-payload executor, R03 public receipt, B06 service, B07
  CLI, or a physical stopped-VM run-once acceptance.

Those gaps remain fail-closed work under issues #291, #319, #205, and parent #238.
