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

### ELF dependency parser evidence

- Parser implementation commit: `eb602552ca483dc62595f135c7238a30a6fc7e32`
- Workflow run: [31327938385](https://github.com/teamleaderleo/smolrunner/actions/runs/31327938385)
- ARM64 job: [93281301455](https://github.com/teamleaderleo/smolrunner/actions/runs/31327938385/job/93281301455)
- x86_64 job: [93281301434](https://github.com/teamleaderleo/smolrunner/actions/runs/31327938385/job/93281301434)
- Date: 2026-08-09
- Result: both jobs parsed every admitted top-level executable from the exact package baseline
  below.

The pure parser accepted the fixed GNU interpreter, exact ordered `DT_NEEDED` basenames, and either
default search or Noble systemd's exact architecture-specific private runpath for `/usr/bin/systemctl`
and `/usr/bin/systemd-run`. It rejects `RPATH`, every other `RUNPATH`, loader audit/filter/config
authority, path-bearing dependencies, executable stacks, writable-executable load segments, and
ambiguous runtime mappings. This proves compatibility of the parser with those package bytes only;
it is not path, package, loader, library, configuration, cache, or transitive dependency evidence.

### Dynamic-loader object parser evidence

- Parser implementation commit: `f95b74f0318f7d1f061df2e1c610ec0ad8240481`
- Workflow run: [31337151663](https://github.com/teamleaderleo/smolrunner/actions/runs/31337151663)
- ARM64 job: [93304773282](https://github.com/teamleaderleo/smolrunner/actions/runs/31337151663/job/93304773282)
- x86_64 job: [93304773257](https://github.com/teamleaderleo/smolrunner/actions/runs/31337151663/job/93304773257)
- Date: 2026-08-10
- Result: both jobs parsed the exact architecture-specific GNU dynamic-loader object installed
  from the Noble package baseline below.

The pure parser shares the bounded ELF64 safety envelope used for the admitted top-level
executables and additionally requires `ET_DYN`, no interpreter, one dynamic segment, no external
dependency, and default dynamic search. It rejects loader-side `RPATH`, `RUNPATH`, audit, filter,
configuration, text-relocation, and nodefaultlib authority. This proves compatibility with those
loader bytes only. It follows the fixed loader pathname in a disposable package fixture and does
not prove path, symlink, inode, owner, mode, package, configuration, cache, transitive-library, or
revalidation evidence; it constructs no R01 class or runtime readiness.

### Dynamic-loader configuration parser evidence

- Parser implementation commit: `8805b36169ec5c56afc0dacdb41fc56b149b7131`
- Workflow run: [31329004262](https://github.com/teamleaderleo/smolrunner/actions/runs/31329004262)
- ARM64 job: [93284062710](https://github.com/teamleaderleo/smolrunner/actions/runs/31329004262/job/93284062710)
- x86_64 job: [93284062672](https://github.com/teamleaderleo/smolrunner/actions/runs/31329004262/job/93284062672)
- Date: 2026-08-09
- Result: both jobs parsed the exact root and fragment loader-configuration bytes installed from
  the Noble package baseline below.

The pure parser accepted exactly the root system-fragment include and the closed ordered Noble
search-directory identities for the selected architecture. It rejects oversized or noncanonical
syntax, duplicate directories, wrong-role directives, wrong-architecture paths, and every other
include or search directory. This proves compatibility with those package bytes only; it does not
expand includes, enumerate configuration files, open or revalidate a path, prove file or directory
ownership, parse `ld.so.cache`, resolve a library, construct evidence, or seal runtime readiness.

### Dynamic-loader cache parser evidence

- Parser implementation commit: `75e7a317e66bbeb25a4a8c8626d9ccc0ce27b035`
- Workflow run: [31332267685](https://github.com/teamleaderleo/smolrunner/actions/runs/31332267685)
- ARM64 job: [93292289028](https://github.com/teamleaderleo/smolrunner/actions/runs/31332267685/job/93292289028)
- x86_64 job: [93292288968](https://github.com/teamleaderleo/smolrunner/actions/runs/31332267685/job/93292288968)
- Date: 2026-08-09
- Result: both jobs parsed the complete live `/etc/ld.so.cache` from the exact Noble package
  baseline below.

The pure parser accepted only the current little-endian glibc 1.1 layout, bounded every table and
string, validated cache ordering and the closed extension/hwcap representation, and rejected
numeric comparison aliases, unknown cache IDs, and unsafe library identities or paths. The x86
capability vocabulary is exactly `x86-64-v2`, `x86-64-v3`, and `x86-64-v4` with ISA levels 0–3;
the AArch64 model admits no named hwcap or nonzero ISA level. The Noble x86_64 cache also carries
generic ELF/libc6 compatibility entries that the admitted 64-bit loader does not select; the parser
fully validates those known entries but omits them from its compatible-entry view. This proves
compatibility with those live cache bytes only. It does not open or revalidate the cache, prove its
owner or package, enumerate or open search directories or libraries, resolve a dependency,
construct evidence, or seal runtime readiness.

### Top-level executable prerequisite evidence

- Observer implementation commit: `7f903fa84912ccec5ea21579201c99900947f83a`
- Workflow run: [31333397603](https://github.com/teamleaderleo/smolrunner/actions/runs/31333397603)
- ARM64 job: [93295096762](https://github.com/teamleaderleo/smolrunner/actions/runs/31333397603/job/93295096762)
- x86_64 job: [93295096748](https://github.com/teamleaderleo/smolrunner/actions/runs/31333397603/job/93295096748)
- Date: 2026-08-09
- Result: both jobs descriptor-opened, parsed, and revalidated all eleven fixed top-level
  executable prerequisites from the exact Noble package baseline below.

The observer requires protected root-owned directory chains and exact root/group, mode, regular
file, single-link, per-file, and aggregate byte policy before parsing held inodes. It double-reads
the bounded bytes, binds the path-specific ELF/search semantics, and revalidates held descriptors,
bytes, pathname entries, and every directory chain around the complete observation. The fixture
also proves refusal of hardlinks, symlinks, writable mode, wrong architecture before filesystem
access, and mid-observation metadata drift. Catatonit is the one exact static executable; every
other admitted top-level executable is dynamic, and only systemctl/systemd-run use the reviewed
systemd-private runpath. This is an opaque current prerequisite only. It does not resolve or open
the interpreter or any transitive library, inspect loader configuration/cache ownership, prove
package identity, construct classes 8–18, execute a command, or seal runtime readiness.

### Dynamic-loader state prerequisite evidence

- Observer implementation commit: `0d57507bbea68ed558bc02eca9bb2f48ff4bb9d1`
- Workflow run: [31336046923](https://github.com/teamleaderleo/smolrunner/actions/runs/31336046923)
- ARM64 job: [93301971865](https://github.com/teamleaderleo/smolrunner/actions/runs/31336046923/job/93301971865)
- x86_64 job: [93301971848](https://github.com/teamleaderleo/smolrunner/actions/runs/31336046923/job/93301971848)
- Date: 2026-08-09
- Result: both jobs descriptor-opened, parsed, and revalidated the fixed Noble root loader
  configuration, every included non-hidden `.conf` fragment, and the current loader cache while
  proving `ld.so.preload` absent before and after observation.

The observer bounds the complete fragment enumeration before opening every matched file, requires
protected root-owned directory chains and exact root/group, mode, regular-file, single-link, and
byte policy, and uses independent directory descriptions so the confirming enumeration starts at
the beginning. It double-reads every held file, binds project/architecture/config/cache semantics,
and revalidates held descriptors, bytes, pathname entries, included names, preload absence, and
every directory chain around the complete observation. The copied-state fixture also proves
refusal of writable configuration, explicit preload state, an unreviewed search fragment, and
mid-observation cache metadata drift; a leading-dot `.conf` remains outside glibc's fixed glob.

The disposable workflow removes the hosted image's preinstalled `fakeroot` loader fragment and,
when installed, the x86 `libc6-i386` biarch fragment before testing. Neither search directory is
part of the native-64-bit personal-worker closure, and the production observer continues to refuse
them. This is an opaque current prerequisite only. It does not open a configured search directory,
trust or resolve a cached path, open the ELF interpreter or any transitive library, inspect package
identity, construct classes 8–18, execute a command, or seal runtime readiness.

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
- Noble's packaged `systemctl` and `systemd-run` select the architecture-specific private systemd
  library directory with `DT_RUNPATH`; rejecting every runpath makes the reviewed command graph
  unusable. The parser now represents only that exact runpath as typed unresolved policy, leaving
  later descriptor-bound directory and library proof mandatory.
- Noble's root loader configuration uses exactly one semantic space in
  `include /etc/ld.so.conf.d/*.conf` and ends with two line feeds. Alternate `include` directives
  preceded by leading SP, HT, VT, or FF are unsafe search authority even though their whitespace
  is noncanonical; CR remains a whole-file format refusal. Semantic unsafe-search classification
  must therefore precede the generic active-line format check.
- Noble's x86_64 loader cache includes generic ELF/libc6 compatibility entries in addition to
  exact x86-64 entries. The admitted 64-bit loader ignores the generic cache ID; the parser must
  still validate those entries and the complete cache ordering while withholding them from future
  compatible-library resolution. Unknown cache IDs remain fail-closed.
- Noble catatonit is a static ELF with no interpreter, dynamic search policy, or `DT_NEEDED`
  dependency. Treating all eleven fixed top-level executables as dynamic makes the prerequisite
  unusable; the exception is exact to catatonit, while the other ten remain dynamic.
- The GitHub-hosted image adds `fakeroot` and x86 biarch loader fragments that are absent from the
  native-64-bit target closure. Parser-only checks failed to reveal that ambient authority; the
  descriptor-bound observer refused it until the disposable fixture removed the unrelated
  packages.

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
