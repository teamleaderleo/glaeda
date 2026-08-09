# Ubuntu 24.04 Podman container-fixture evidence

Status: partial R02/R03 disposable-fixture evidence only. This is not a hostile-repository
execution backend and does not authorize a physical Lima or host mutation.

## Exact evidence

- Fixture commit: `72e92ce64a4c42028fe4431d32c040837b3c81a8`
- Workflow run: [31312693012](https://github.com/teamleaderleo/smolrunner/actions/runs/31312693012)
- ARM64 job: [93242855635](https://github.com/teamleaderleo/smolrunner/actions/runs/31312693012/job/93242855635)
- x86_64 job: [93242855661](https://github.com/teamleaderleo/smolrunner/actions/runs/31312693012/job/93242855661)
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
6. A transient systemd service with `Delegate=yes` exposes CPU, memory, and PID controllers on both
   architectures. The fixture creates sibling supervisor and payload groups, moves the launcher to
   the supervisor, enables and rereads those required controllers, and rereads exact outer limits of
   75% CPU, 96 MiB memory, zero swap, and 64 tasks. It precreates the payload parent with 50% CPU,
   64 MiB memory, zero swap, and 32 PIDs before Podman runs. Stopped inspect preserves that exact parent
   and the requested container limits; the generated OCI spec carries the exact Podman leaf below
   that parent and one pathless private cgroup namespace. The live leaf exposes the same exact limits.
7. Detached `podman start <exact-id>` returns the exact ID. Bounded exact-ID inspection polling
   reaches one stopped exit with code zero, and bounded `podman logs` returns only the expected
   SHA-256 results for the image-owned account files. The single-link log remains runner-owned,
   non-group/world-writable, nonempty, and below its one-MiB ceiling.
8. Root creates one attempt-private executable tmpfs with exact 8-MiB and 64-inode ceilings, runner
   ownership and mode `0700`, `nosuid`, and `nodev`. The unprivileged phase rereads those exact facts
   and an empty root before Podman. The hostile stopped OCI spec binds that exact source read-write at
   `/target` below the exact payload leaf.
9. A fixed image-owned hostile payload exhausts both target ceilings, increments the exact leaf's
   `pids.events max`, and raises both that leaf's `memory.current` and `memory.stat shmem` at least
   7 MiB above their pre-start baselines while the target is full. Its bounded private log crosses
   the 64-KiB abort threshold. The fixture opens the exact leaf's `cgroup.kill` before start, writes
   the kill only after every pressure signal, observes `populated 0`, and then requires one nonzero
   stopped result. Exact removal deletes the leaf; the final single-link runner-owned log remains
   non-group/world-writable and below one MiB.
10. Exact trusted and hostile container removal succeeds, followed by exact image removal. The
    payload parent reports no process and `populated 0`; systemd collection removes the entire exact
    service cgroup; root unmounts the exact target; the two precreated network lock inodes remain the
    only network-directory entries; no process for the disposable UID or mount below the attempt
    root remains.

The successful markers included `hostile_abort=cgroup-kill`,
`target_tmpfs=byte-and-inode-bounded`, `offline_image=exact`, `stopped_create=closed`,
`account_files=image-owned`, `cgroup=bounded`, and `apparmor=rootless-unavailable`, followed by
`podman_container_closure_probe=pass`.

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
- Noble stopped-container inspect preserves the cgroup parent and resource values but reports no
  `CgroupnsMode`; the generated OCI spec is the effective private-cgroup-namespace evidence.
- cgroup-v2 controller pseudo-files are entries below every cgroup. Emptiness checks must count only
  child cgroup directories and separately require an empty `cgroup.procs` plus `populated 0`.
- PID exhaustion must happen after starting the fixed output producer; otherwise the shell cannot
  create the producer it is meant to bound. The 64-KiB abort threshold remains fixed, while its
  bounded ARM64 observation window is twenty seconds.

These are contract corrections, not compatibility relaxations. The trusted fixture remains unable
to authorize hostile repository code until every blocking boundary below is independently closed.

## Still blocking R01/R02/B06

This fixture does **not** prove any of the following and must not be cited as if it did:

- installation, digest verification, and read-only use of the production immutable image store or
  reviewed in-image gate;
- selection and installation of the production seccomp policy, syscall-denial behavior, or any
  outer-service AppArmor confinement;
- descriptor-bound exact Git-tree materialization, read-only source mounting, the production direct
  mount API and held mount identity, verified dependency inputs, Cargo/test command expansion, or
  cache non-authority;
- production sealed cgroup-handle authority, whole-attempt kill across Podman/conmon/pause processes,
  crash recovery, or pause-process/PID-file recovery;
- production timeout, cancellation, output-overflow and cleanup-incomplete classification; memory
  limit/OOM behavior; descendant escape attempts; network syscall denial; or complete mount/device/
  host-file closure;
- durable attempt journaling, crash recovery, final reservation/deadline recheck, ownership
  persistence, or ambiguous-cleanup handling;
- the Rust R01 readiness object, R02 hostile-payload executor, R03 public receipt, B06 service, B07
  CLI, or a physical stopped-VM run-once acceptance.

Those gaps remain fail-closed work under issues #291, #319, #205, and parent #238.
