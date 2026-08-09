# Podman execution-closure audit

Status: pre-implementation security contract for issues #291 and #319  
Baseline: Ubuntu 24.04 ARM64, Podman 4.9.3 series  
Scope: the personal-worker verification lane only

## Outcome

Binding `/usr/bin/podman` is necessary but not sufficient to execute repository code. Podman
loads configuration, storage metadata, an OCI runtime, `conmon`, user-namespace helpers, an
optional init, security policy, and dynamic libraries after the descriptor-bound launcher has
selected the top-level ELF. Several of those inputs may select another executable or add a host
mount.

The existing Renderprove native probe is therefore not a reusable hostile-code boundary. It
passes the persistent runner `HOME`, `PATH`, and `XDG_RUNTIME_DIR`, and it binds only the top-level
Podman ELF and working directory. Its container flags remain useful defense in depth, but they do
not prove a closed host-side runtime.

The initial personal-worker runtime must instead use:

- one exact root-owned runtime closure and immutable verification image generation;
- a root-owned empty configuration home and explicit configuration files;
- a fresh execution-private graph root, run root, temporary directory, and libpod database;
- a root-owned read-only additional image store;
- one separately owned, envelope/runtime/source-scoped writable build cache;
- a dedicated pre-created cgroup-v2 execution identity; and
- no persistent runner home, user configuration, auth file, remote connection, hook, plugin,
  device, or network-helper discovery.

No readiness object may be issued until every required edge below has exact evidence and every
prohibited edge is proven unreachable. This document authorizes no Podman invocation, storage
creation, image installation, container execution, or cleanup mutation.

## Reviewed command shapes

The audit covers only these future operations:

1. inspect one already-installed, digest-pinned verification image;
2. run one checked-in verification command in the same image with `--pull=never` and
   `--network=none`; and
3. inspect or remove only the exact container created by that attempt during durable recovery.

Pull, build, login, remote mode, system service, arbitrary `exec`, caller-provided arguments,
caller-provided mounts, and generic Podman administration remain outside the boundary.

## Supported package baseline

Ubuntu 24.04 currently ships the Podman 4.9.3 series. The package declares `conmon` and one OCI
runtime as hard dependencies and recommends a container init plus rootless namespace helpers.
The ARM64 baseline also provides the following relevant versions:

| Component | Ubuntu 24.04 baseline | Expected executable |
| --- | --- | --- |
| Podman | `4.9.3+ds1` series | `/usr/bin/podman` |
| containers/common | `0.57.4+ds1` series | configuration data, no executable |
| conmon | `2.1.10+ds1` series | `/usr/bin/conmon` |
| crun | `1.14.1` series | `/usr/bin/crun` |
| catatonit | `0.1.7` series | `/usr/libexec/podman/catatonit` |
| netavark | `1.4.0` series | distribution-owned helper path |
| fuse-overlayfs | `1.13` series | `/usr/bin/fuse-overlayfs` |
| newuidmap/newgidmap | installed from `uidmap` | `/usr/bin/newuidmap`, `/usr/bin/newgidmap` |

Versions are compatibility evidence, not identity. R01 must bind installed file content and
metadata. Any package/security update changes the runtime generation and requires a fresh
readiness receipt before another workload. It must not silently accept a version range.

Primary package references:

- [Ubuntu 24.04 Podman package](https://packages.ubuntu.com/noble/podman)
- [Ubuntu 24.04 conmon package](https://packages.ubuntu.com/noble/conmon)
- [Ubuntu 24.04 crun package](https://packages.ubuntu.com/noble/crun)
- [Ubuntu 24.04 catatonit package](https://packages.ubuntu.com/noble/catatonit)
- [Ubuntu 24.04 containers/common source](https://packages.ubuntu.com/source/noble-updates/misc/golang-github-containers-common)
- [Ubuntu 24.04 netavark package](https://packages.ubuntu.com/noble/netavark)
- [Ubuntu 24.04 fuse-overlayfs package](https://packages.ubuntu.com/noble/fuse-overlayfs)

## Execution graph

The graph is split by whether an input can execute code, change the generated OCI specification,
select persistent state, or merely provide bounded host facts.

### Loader and executable closure

| Edge | Risk | Required disposition |
| --- | --- | --- |
| `/usr/bin/podman` | selected host program | Exact root-owned single-link regular ELF, content digest, device/inode, mode, package identity, and rebind check. |
| ELF interpreter and transitive `DT_NEEDED` libraries | replacement changes Podman before its own checks | Resolve with a non-executing ELF parser or an exact descriptor-bound `/usr/bin/readelf`, never `ldd`; bind the interpreter, every resolved library and hardware-capability variant, `/etc/ld.so.cache`, `/etc/ld.so.preload`, and applicable loader configuration/search directories. Reject preload entries, `RPATH`/`RUNPATH`, unresolved or ambiguous entries, or a user-writable directory. |
| `/usr/bin/conmon` | monitor spawned for every container | Select with explicit `--conmon`; exact ELF and loader closure; no `$PATH` fallback. |
| `/usr/bin/crun` | creates namespaces, mounts, capabilities, seccomp, and cgroups | Select with explicit `--runtime`; exact ELF and loader closure; no named-runtime or `$PATH` fallback. |
| `/usr/libexec/podman/catatonit` | `--init` executes it inside the container | Select with explicit `--init-path`; exact regular-file content and metadata. If the image supplies the reviewed init instead, omit `--init` and prohibit this edge. |
| `/usr/bin/newuidmap`, `/usr/bin/newgidmap` | setuid helpers establish the rootless user namespace | Exact root-owned executables, setuid/mode/package evidence, content digest, loader closure, exact runner subordinate ranges, and no `$PATH` ambiguity. |
| netavark and aardvark-dns | the configured backend may resolve helpers while libpod initializes, even for a no-network run | Fix `network_backend="netavark"` and one root-owned helper directory; bind exact content and loader closure until the package fixture proves an edge unreachable. `--network=none` must keep both unexecuted. |
| fuse-overlayfs | storage config can execute it as `mount_program` | Initial R01 selects native rootless overlay and requires no mount program. Presence of any configured mount program is conflicting. A later fuse backend requires a separate exact executable closure. |
| helper search directories | may select netavark, catatonit, pasta, or slirp4netns | One exact root-owned directory set; reject `/usr/local`, runner-owned, missing, extra, symlinked, group-writable, or world-writable candidates. |

The Podman 4.9.3 source explicitly falls back to `$PATH` for OCI runtimes and `conmon` when
configured candidates fail. Merely passing a short `PATH` is not proof. R01 must pass absolute
runtime/conmon/init paths and prove the effective configuration contains no alternate candidate.

### Configuration and specification closure

| Edge | Effect | Required disposition |
| --- | --- | --- |
| `containers.conf` and modules | selects runtime, conmon, helpers, hooks, CDI, init, env, privileges, mounts, devices, cgroups, logging, and remote mode | Set `CONTAINERS_CONF` to one exact root-owned file; prohibit drop-ins, modules, `CONTAINERS_CONF_OVERRIDE`, and runner config. The file fixes empty CDI/plugin/device/volume lists as well as the admitted runtime settings. Parse and canonicalize the admitted subset, then content-bind it. |
| `storage.conf` | selects graph/run roots, driver, mount program, additional stores, and mutable options | Set `CONTAINERS_STORAGE_CONF` to one exact root-owned file. It names only run-private graph/run roots plus one root-owned read-only image store and native overlay with no helper. |
| `registries.conf` and drop-ins | changes name resolution, mirrors, and transports | Set `CONTAINERS_REGISTRIES_CONF` to one exact root-owned file and bind its associated drop-in directory as empty. Admit no search registries, mirrors, short-name aliases, or insecure transport. The command still uses a full digest reference and `--pull=never`. |
| signature policy | can admit alternate transport/signature behavior | The 4.9.3 `run` surface has no global signature-policy selector. Prove the policy path is not opened when the exact local image exists and `--pull=never`; until then bind the applicable root-owned policy as reject-by-default and prohibit runner overrides. |
| `mounts.conf` | silently copies or mounts host paths into every container | Use a root-owned empty `HOME` and `XDG_CONFIG_HOME`; prove the runner override absent and bind system/vendor files as absent or empty. Any entry is conflicting. |
| OCI hook directories | a `precreate` hook can alter the OCI spec and add mounts | Pass one explicit root-owned empty `--hooks-dir`; prove it remains empty and same-object through spawn. Any hook is conflicting. |
| CDI spec directories | selected devices can add host devices, mounts, env, and hooks | The 4.9.3 global CLI has no CDI-directory override. Fix the versioned `containers.conf` field to one exact empty root-owned directory, reject any CDI entry or caller device, and verify package behavior. |
| seccomp profile | controls syscall filtering | Bind an exact root-owned profile digest selected explicitly by the admitted config; reject unconfined or image-selected policy. |
| AppArmor policy | controls mandatory access confinement on Ubuntu | Bind the exact admitted profile name plus root-owned loaded-policy/config generation. `unconfined` is prohibited. |
| default container environment | may inject proxies, credentials, or host values | Configuration has `env=[]`, `env_host=false`, and `http_proxy=false`; command repeats the negative flags where Podman supports them. Only immutable image-config env plus the fixed checked-in command env is admitted. |
| image-declared volumes, health checks, entrypoint, user, and working directory | may create hidden writable volumes or select a different in-image process | Ignore image volumes, disable health checks/restarts, and pass the exact checked-in absolute entrypoint, user, and working directory. The immutable image config is still bound and reviewed. |
| auth and remote connection files | can expose credentials or redirect Podman to a socket/SSH service | Empty environment excludes all `CONTAINER_*`, `REGISTRY_AUTH_FILE`, and connection variables. Set `REGISTRY_AUTH_FILE` to an exact empty run-private JSON object and prove Docker auth/config absent. Pass `--remote=false`. |
| network configuration | can select plugins/helpers or host namespace behavior | Pass `--network=none`; bind an empty exact network-config directory; prohibit host, slirp, pasta, bridge, CNI, plugins, DNS helpers, ports, and network namespace joins. |
| host `/etc/hosts`, resolver, hostname, localtime, and timezone inputs | can disclose host identity or make the generated spec depend on ambient host files | Use a private UTS namespace with a fixed non-secret hostname, `--no-hosts`, no network, an image-owned resolver file, and fixed UTC container configuration. The Ubuntu fixture must prove the generated spec contains no copied host path or content. |
| signature registries and short-name state | can redirect image resolution or signature lookup | Prohibit short names, bind or prove unreachable the root-owned `registries.d`/short-name state, and address the local image only by the sealed fully qualified digest with `--pull=never`. |

Upstream 4.9.3 documents the user-overridable config hierarchy, rootless storage/auth defaults,
helper/runtime selection, automatic mounts, and powerful precreate hooks:

- [Podman 4.9.3 global options and configuration files](https://docs.podman.io/en/v4.9.3/markdown/podman.1.html)
- [Podman 4.9.3 run options](https://docs.podman.io/en/v4.9.3/markdown/podman-run.1.html)
- [Podman 4.9.3 rootless configuration](https://github.com/containers/podman/blob/v4.9.3/docs/tutorials/rootless_tutorial.md)
- [containers/common 0.57.4 containers.conf contract](https://github.com/containers/common/blob/v0.57.4/docs/containers.conf.5.md)
- [containers/common automatic mounts implementation](https://github.com/containers/common/blob/v0.57.4/pkg/subscriptions/subscriptions.go)

### Kernel and host identity inputs

| Edge | Effect | Required disposition |
| --- | --- | --- |
| `/etc/subuid`, `/etc/subgid` | authorizes the rootless UID/GID map | Exact single non-overlapping runner ranges, content/object revalidation, and the accepted account policy. Unknown or duplicate authority blocks. |
| account/NSS inputs | can change UID/GID/name and supplementary-group resolution | Bind the runner's exact numeric account and primary group plus the applicable root-owned `/etc/passwd`, `/etc/group`, `/etc/nsswitch.conf`, and NSS library closure. The launcher still clears supplementary groups before Podman. |
| cgroup-v2 mount and delegated parent | controls resource isolation and group ownership | Exact cgroup2 filesystem, controller set, delegated parent object, owner/mode, and no unexpected processes. Reread applied files before workload start and group membership through cleanup. |
| user/mount namespaces and kernel capability | defines whether rootless isolation is actually available | Bind boot ID, kernel release, architecture, namespace support, and the exact successful journaled smoke generation. A reboot or kernel change invalidates readiness. |
| `/proc`, `/sys`, `/dev` facts | consulted while constructing namespaces, devices, mounts, and limits | Admit only the fixed kernel interfaces and minimal device set required by the reviewed spec. `/dev/fuse`, KVM, GPUs, host sockets, and caller devices are prohibited in the native-overlay first slice. |
| user systemd and D-Bus | may create scopes outside the sealed cgroup | Explicit `cgroupfs` plus the pre-created delegated parent makes this edge unnecessary. Absence of a usable user bus is acceptable; any systemd fallback or sibling scope is a refusal. |

These are live host facts rather than package content. They are re-observed immediately before the
first runtime mutation and after any reboot. A successful earlier smoke receipt does not make stale
kernel, account, or cgroup evidence current.

### Mutable runtime state

| State | Lifecycle class | Rule |
| --- | --- | --- |
| root-owned verification image store | installation-owned persistent, read-only to runner | Created only by an explicit journaled image-install operation. Bind installation ID, runtime generation, complete image manifest/config/layer identity, owner/mode, and store metadata. Unknown content blocks. |
| execution graph root | run-private | Fresh empty directory under one durable attempt. It holds writable container layers only. Remove after cgroup emptiness, Podman/conmon exit, unmount proof, and exact ownership revalidation. |
| execution run root | run-private | Fresh empty directory under the same attempt. It holds locks and transient libpod state. Never reused. |
| libpod database/static/tmp/volume state | run-private | Use transient store and explicit run-private roots. A crash leaves recovery debt; it never becomes the next attempt's input. |
| protected Cargo target cache | installation-owned persistent, writable and executable only inside the admitted container | Namespace includes installation, source, tree, command, Rust envelope, immutable image, toolchain, and runtime generation. Cargo must execute build scripts, proc macros, and test binaries from this mount, so `noexec` is invalid. The cache is never in a host `PATH`, config search path, working directory, or host command argument; its bytes are never verification evidence. |
| source checkout | protected read-only mount | Reopen and match canonical path/device/inode plus commit/tree/cleanliness immediately before first mutation. Mount read-only at one fixed container path. |
| cgroup v2 subtree | run-private authoritative execution identity | Pre-create under an exact delegated parent; apply and reread CPU, memory, swap, and PID controls before repository code. Retain the authoritative handle until group-empty proof. |
| auth, config, hooks, CDI, network, empty home | run-private control state | Created by the privileged steward as exact empty or exact-content root-owned objects, readable but not replaceable by the runner. Destroy only after full cleanup. |
| `XDG_RUNTIME_DIR` and temporary state | run-private mutable state | Fresh leased directories writable by the runner beneath an exact steward-owned parent. Their contents are never reused or trusted as configuration; non-empty pre-state blocks and cleanup waits for group emptiness. |

Persistent runner-owned Podman storage is not adoptable. A matching path, UID, image name, image
digest string, or libpod database field cannot prove installation ownership. Existing state must be
quarantined or left untouched; no automatic `podman system reset`, recursive deletion, migration,
or config rewrite is permitted.

The persistent cache is disk state, not a live service: it holds no process, cgroup, Podman database,
or VM-liveness lease. Keeping it must not keep the Lima guest running or reserve guest RAM. The
personal-worker store admits at most one active verification attempt, so no second container or
second Lima instance may be created to improve throughput.

## Required invocation envelope

R02 must derive the final argument vector from sealed identities, but it must include the following
closed global intent. Exact option support is verified against the admitted Podman build before R01
can issue readiness.

The image-readiness operation uses the same executable, environment, config, storage, remote,
helper, and transient-state closure as the run below. Its only subcommand is one fixed `image
inspect` form against the sealed fully qualified digest; output is bounded and parsed into private
typed evidence. Because the packaged command may initialize storage or a libpod database, it is
never part of static planning: the first call and every recovery call are journaled mutations until
the Ubuntu fixture proves a narrower effect set. No caller supplies a Go template or image string.

```text
/usr/bin/podman
  --remote=false
  --runtime=/usr/bin/crun
  --conmon=/usr/bin/conmon
  --events-backend=none
  --hooks-dir=<exact-empty-hooks-dir>
  --network-config-dir=<exact-empty-network-dir>
  --cgroup-manager=cgroupfs
  --tmpdir=<attempt-tmpdir>
  --transient-store
  run
  --pull=never
  --rm
  --attach=stdout
  --attach=stderr
  --init
  --init-path=/usr/libexec/podman/catatonit
  --network=none
  --no-hosts
  --ipc=private
  --pid=private
  --uts=private
  --hostname=smolrunner-verification
  --read-only
  --read-only-tmpfs=false
  --image-volume=ignore
  --cap-drop=all
  --security-opt=no-new-privileges
  --security-opt=apparmor=<exact-apparmor-profile>
  --security-opt=seccomp=<exact-seccomp-profile>
  --cgroup-parent=<sealed-execution-group>
  --cgroupns=private
  --pids-limit=<applied-limit>
  --memory=<applied-limit>
  --memory-swap=<explicit-applied-policy>
  --cpus=<applied-limit>
  --env-host=false
  --http-proxy=false
  --log-driver=none
  --privileged=false
  --systemd=false
  --restart=no
  --no-healthcheck
  --name=<attempt-scoped-nonauthority-name>
  --cidfile=<attempt-private-cidfile>
  --userns=keep-id
  --user=<fixed-numeric-uid>:<fixed-numeric-gid>
  --passwd=false
  --workdir=<fixed-source-path>
  --entrypoint=<checked-in-absolute-image-program>
  --mount=type=bind,src=<exact-source>,dst=<fixed-source-path>,ro,nosuid,nodev,noexec
  --mount=type=bind,src=<exact-cache>,dst=<fixed-cache-path>,rw,exec,nosuid,nodev
  --tmpfs=/tmp:rw,noexec,nosuid,nodev,size=<bounded-scratch-bytes>,mode=1777
  <exact-image>@sha256:<manifest>
  <checked-in-arguments...>
```

The launcher supplies null stdin, pipes only stdout/stderr, and allocates no TTY. The displayed paths
above are private plan values, never public receipt fields. The attempt name and CID file are
recovery lookups only; neither proves ownership, and both must be matched to the durable attempt,
exact run-private store, and authoritative cgroup before inspect or removal. `--dns=none` is
deliberately absent because Podman 4.9.3 rejects it in combination with `--network=none`. The bounded
scratch tmpfs is charged to the same memory cgroup and is created only on demand; it does not create
an idle RAM floor. R02 must verify that 4.9.3 option precedence makes each command-line value
authoritative over the sealed config. Any unsupported negative flag is removed only after the same
property is fixed and validated in the sealed config. No caller may append an option.

The child environment starts empty and contains only exact private locations and identity strings:

- root-owned empty `HOME` and `XDG_CONFIG_HOME`;
- run-private `XDG_RUNTIME_DIR` and `TMPDIR`; the exact per-attempt `storage.conf` carries the
  graph root and run root so the explicit additional image store is not discarded by a global
  `--root` override;
- exact `CONTAINERS_CONF`, `CONTAINERS_STORAGE_CONF`,
  `CONTAINERS_REGISTRIES_CONF`, and empty `REGISTRY_AUTH_FILE`;
- fixed `USER` and `LOGNAME`; and
- `PATH=/usr/bin` only if a proven Podman 4.9.3 edge cannot accept an absolute helper path.

The in-container environment is separately closed: it contains only the reviewed immutable image
environment plus fixed values required by the checked-in Rust envelope, including UTC and the exact
Cargo target directory. No host variable is copied by name or wildcard.

`XDG_DATA_HOME`, `CONTAINERS_CONF_OVERRIDE`, `CONTAINER_HOST`, `CONTAINER_CONNECTION`,
`CONTAINER_SSHKEY`, `PODMAN_CONNECTIONS_CONF`, `STORAGE_DRIVER`, `STORAGE_OPTS`, proxy variables,
Docker variables, credential variables, agent sockets, and host-home variables are absent. If an
internal Podman edge still performs `$PATH` lookup, R01 must both bind the single candidate and prove
no alternate candidate exists in the sole admitted directory.

## Evidence model

R01 readiness is sealed, equality-only, non-serializable authority. It contains no public path,
inode, UID, package version, or digest accessor. Internally it binds:

1. Ubuntu release and architecture;
2. installation ID and monotonically advancing runtime generation;
3. exact executable, interpreter, library, config, policy, and helper objects by canonical path,
   device, inode, owner, group, mode, link count, size, and SHA-256;
4. exact directory entries before and after content hashing, with no-follow traversal and ancestor
   ownership/mode checks;
5. exact empty-directory identities for hooks, CDI, network, auth/config home, and run-private roots;
6. exact runner account, primary group, subordinate ranges, runtime directory, and cgroup
   delegation;
7. exact image-store generation and immutable image identity;
8. exact supported option/capability set for the admitted Podman build; and
9. a canonical domain-separated digest over the complete closure.

R02/B06 must re-confirm held or reopened objects immediately before the first cache/container
mutation. A content match at a replaced pathname is source drift. A package version match is not a
substitute for object identity. Any inability to reopen, hash, parse, or prove absence blocks before
Podman executes.

## Adversarial verification matrix

Every row must run on disposable Ubuntu 24.04. The marker executable writes only inside a bounded
temporary fixture, and each test proves the marker remains absent.

| Case | Injection | Required result |
| --- | --- | --- |
| runner containers config | hostile runtime, conmon, env, privilege, mount, or remote setting in persistent home | Unreachable because persistent home is absent; zero Podman/helper calls. |
| config drop-in/module | hostile rootless drop-in or `CONTAINERS_CONF_OVERRIDE` | Refused during closure observation; zero Podman calls. |
| mounts.conf | automatic host mount from runner/system/vendor file | Any entry blocks; marker path never enters OCI spec. |
| OCI hook | precreate hook adds a mount or executes marker | Explicit empty hook directory wins; marker absent. |
| runtime/conmon | replace configured binary or add earlier `$PATH` candidate | Rebind/digest refusal before Podman. |
| helper directory | substitute netavark, init, pasta, slirp4netns, or fuse-overlayfs | Refusal or proven unreachable; marker absent. |
| loader/library | replace interpreter, cache, or one `DT_NEEDED` object | Runtime generation mismatch; zero Podman calls. |
| auth/remote | populate auth, Docker config, Podman connection, or socket variables | Refusal; no network or remote socket attempt. |
| storage config | alternate graphroot, runroot, driver, mount program, additional store, or ignore-chown | Refusal before storage initialization. |
| old libpod state | prior database/container metadata in a new attempt root | Non-empty run-private root blocks; no adoption or deletion. |
| image store | manifest/config/layer or store-generation substitution | Image identity/runtime readiness refusal. |
| network helper | hostile netavark/CNI configuration while command says none | Empty network config and `--network=none`; marker absent. |
| host-file injection | hostile host resolver/hosts/hostname/timezone content | Generated spec and container fixture contain only fixed image/runtime values; no host marker appears. |
| source rebind | replace checkout after planning or after descriptors open | Fresh path/object/source check refuses before cache/container mutation. |
| cache poisoning | prior cache contains executable/object with expected filename | Verification output remains process/receipt-derived; cache bytes grant no authority. |
| cgroup drift | controller/limit/parent changes between plan and spawn | Refusal before repository code; no fallback cgroup. |
| descendant escape | fork, double-fork, ignore TERM, retain pipe or deleted file | Complete owned group is killed and proven empty, or receipt is cleanup-incomplete. Never success. |
| response loss | crash after create/start/exit/remove checkpoint | Durable recovery classifies exact run-private state; no second workload starts. |

## Implementation sequence

1. **Static closure observation:** add a Linux-only observer for exact files, configuration, empty
   directories, package capability, runner identity, image-store generation, and cgroup delegation.
   It invokes no Podman command.
2. **Journaled installation/smoke:** explicitly create the root-owned config/image generation and
   run-private smoke state under ADR 0020. Unknown existing state blocks; rollback never deletes
   unclassified storage.
3. **Closed container plan:** map one B05 plan to the exact invocation envelope. Invalid or stale
   evidence fails before the first executor call.
4. **Execution group:** create and retain the cgroup-v2 identity before Podman can start repository
   code; integrate issue #205 group-empty and cleanup evidence plus bounded stdout/stderr capture and
   a fixed wall-clock deadline for Podman, conmon, and the payload.
5. **B06 composition:** recheck durable/source/runtime/deadline/cache evidence, run the closed plan,
   and emit the bounded attempt/cleanup/resource receipt consumed by B07.

## Remaining questions that block R01 code

- Confirm the exact Ubuntu ARM64 Podman package option set and compiled paths in a disposable
  24.04 fixture; do not infer them only from upstream 4.9.3 defaults.
- Confirm whether `image inspect` initializes conmon, OCI runtime, networking, libpod database, and
  storage on the packaged build; treat all as reachable until the fixture proves otherwise.
- Confirm native rootless overlay works on the admitted Lima kernel with no `mount_program`; a
  failure blocks rather than selecting fuse-overlayfs automatically.
- Confirm the exact source/cache bind option syntax and bounded `/tmp` behavior; the cache must
  remain executable for Cargo while `nosuid`/`nodev`, and scratch allocation must remain inside the
  applied memory limit.
- Confirm that `--network=none` plus the fixed hosts/resolver/hostname/timezone policy copies no host
  content; Podman 4.9.3 rejects the otherwise tempting `--dns=none` combination.
- Select the exact root-owned AppArmor and seccomp profiles and prove they are effective for a
  rootless cgroup-v2 run.
- Prove a root-owned read-only additional image store works with a fresh run-private graph root and
  `--pull=never` without writes to the image store.
- Decide whether the host catatonit edge is retained or replaced by an init inside the immutable
  image; either choice is exact and non-optional.
- Prove `cgroupfs` can use the pre-created delegated cgroup without Podman/systemd creating an
  unowned sibling scope.

Until those fixture results are recorded and the exact closure is independently reviewed, #319/R01
remains blocked and B06 must not execute repository code.
