# Native Apple builds

`scripts/apple-build` is the prototype front door for ultra-trusted native Apple
development. It runs SwiftPM, Xcode, or a project's existing executable build
helper. It uses the canonical checkout directly, with no OverlayFS, APFS clone,
extra worktree, VM, or background daemon. The Python front door uses standard
macOS filesystem/process primitives; it is separate from Linux host admission.

From a configured project:

```sh
/path/to/glaeda/scripts/apple-build plan
/path/to/glaeda/scripts/apple-build explain
/path/to/glaeda/scripts/apple-build warm
/path/to/glaeda/scripts/apple-build run --profile app
```

`warm` and `run` execute the same real build. Every invocation executes its native
command; a previous receipt never substitutes for validation. `plan` probes the
checkout and selected Apple toolchain and creates no project state. JSON receipts
contain opaque cache identities and no project paths or raw build output. Private
logs are `.glaeda/apple-build/run-<run-id>.log`; the most recent receipt is
`.glaeda/apple-build/last-run.json`.

`explain` is also read-only. It compares the current checkout with the last build
in the same cache generation, distinguishes a changed commit from an uncommitted
working tree, and reports the last build's measured phases when available. It
does not treat two dirty working trees as identical. A matching generation means
the owned cache directory exists, not that all dependencies or outputs are ready.
Source edits and dependency lockfile changes retain cache paths; the compiler and
package manager validate the changed inputs. Toolchain, SDK, profile, direct build
helper, checkout identity, or generation-label changes select another generation.
No explanation grants result reuse or bypasses active/quarantined state checks.

New build receipts include `timings_seconds`: initial observation, store/lock
acquisition, admission/preparation, native command, and completion observation.
The native interval includes spawning and recording the child. These are elapsed
wall times, not CPU times; store/lock includes filesystem work and contention.
CLI output additionally reports `toolchain_and_profile_probe_seconds` for the
initial probe (outside the durable build receipt). Final receipt publication and
printing are not included. Legacy receipts remain readable without timing fields.
Package resolution, compilation, bundling, and signing inside a project helper
are not yet separately instrumented. Use these measurements to establish whether
the next improvement belongs in Glaeda admission or the native workflow.

## Profile format

Add `glaeda.apple.json` to the project and ignore `.glaeda/apple-build/`:

```json
{
  "schema_version": 1,
  "profiles": {
    "app": {
      "engine": "swiftpm",
      "configuration": "debug",
      "product": "Example"
    },
    "xcode": {
      "engine": "xcode",
      "project": "Example.xcodeproj",
      "scheme": "Example",
      "configuration": "Debug",
      "destination": "platform=macOS",
      "settings": {"CODE_SIGNING_ALLOWED": "NO"}
    }
  }
}
```

SwiftPM accepts `action` (`build` or `test`), an optional relative `package`
directory, `configuration`, `product` (build only), `triple`, and bounded `jobs`.
Xcode accepts exactly one relative `project` or `workspace`, explicit `scheme`,
`configuration`, `destination`, optional build `settings`, and `action` (`build`
or `test`). Neither adapter accepts arbitrary extra flags that redirect managed
cache paths. SDK selection defaults to `macosx`; `sdk` also accepts Apple's iOS,
tvOS, watchOS, and visionOS device/simulator SDK names when installed. That routing
is supported by the planner; physical evidence so far covers macOS only.

Projects with bundling, tagging, signing or native extensions should retain their
build helper:

```json
{
  "schema_version": 1,
  "profiles": {
    "app": {
      "engine": "script",
      "executable": "build.sh",
      "arguments": ["app"],
      "environment": {"BUILD_DIR": "{products}"}
    }
  }
}
```

Helper arguments and declared environment values can use `{derived_data}`,
`{source_packages}`, `{scratch}`, `{module_cache}`, `{products}`, and `{extensions}`.
The helper must explicitly consume the appropriate paths: this layer cannot
redirect a custom compiler invocation that ignores them. cmux's profile keeps its
tagged reload helper; Idlesse's profile keeps app/extension bundling and routes
SwiftPM, module and extension caches through explicit build-script inputs.

The child environment starts empty, retaining only HOME, USER, LOGNAME, PATH,
TMPDIR and locale variables. The observed DEVELOPER_DIR and managed module-cache
path are set explicitly. Build settings must be declared in the profile rather
than inherited silently. Ambient tokens and unrelated environment variables are
not forwarded. Profile files are trusted executable configuration, not a sandbox
boundary; do not put credentials in them. Helpers must return only after their
build subprocesses finish and must not detach build work into another session.

## Cache validity and concurrency

### Editing, pulling, and warm latency

Finish an active build before editing or pulling, then run `glaeda-apple warm`
again. A normal source edit or fast-forward pull keeps the cache location; the
native build system checks what needs rebuilding. Dependency changes still go
through SwiftPM/Xcode resolution. Warm never means returning an old success
without running the build.

A changed profile, direct build helper, Xcode/Swift/SDK, or generation label
selects a new cache. Moving or replacing the physical checkout requires explicit
ownership reconciliation; copying `.glaeda` into another checkout is not cache
adoption. An interrupted build requires the recovery procedure below.

The build lock coordinates Glaeda builds, not editors or Git. Before/after source
observations do not prove that a build saw one consistent revision, especially
for dirty files. If a pull or edit overlaps a build, run another build after it
settles before relying on the result.

Glaeda owns cache placement, toolchain identity, admission, recovery and timing.
Projects own their dependency graph, incremental compiler settings, bundle
assembly, signing and launch behavior. Keep those in their existing helpers;
sharing an Xcode build directory between unrelated projects is not an
optimization.

On one arm64 Mac (24 GiB, macOS 26.6.2), sequential September 12, 2026 trials
measured an Idlesse fresh build at 50.0 seconds and unchanged warm builds at a
2.4-second median. A shared Swift source edit took 19.9 seconds. The paired
controller overhead was approximately 0.42 seconds. The older cmux fork's three
warm runs had a 31.9-second median; an extra resource-measured run took 46.6
seconds. These are local observations, not guarantees or current-upstream cmux
results. OS caches and background activity were not controlled.

Optimize in measured order: inspect native build timing and always-run script
phases; avoid repeated setup/network work on the build path; improve incremental
extension compilation and bundle copying; then reduce controller probe overhead.
Benchmark unchanged, implementation edit, shared-interface edit, dependency
update and toolchain reset separately. Keep signing and dependency validation
correct. A resident process may reduce launch latency, but it needs a separate
lifecycle contract and is not implemented by this build helper.

Each generation binds the physical checkout's path/device/inode, full profile,
selected developer directory, Xcode build, Swift version, requested SDK path/build,
host architecture, generation label, and direct helper content digest. Source
commits and ordinary edits do not change its pathname. SwiftPM/Xcode still own
source, dependency, lockfile, and compiler-level incremental validity. Runtime
source snapshots report commit and cleanliness before/after; they are not an
atomic source transaction or a complete identity for a dirty checkout.

DerivedData, source packages, SwiftPM scratch, module cache, products, and extension
state live under `.glaeda/apple-build/cache/<key>/`. New settings/toolchains get
separate cold generations. Existing unmarked directories and symlinks are not
adopted. Previously unmanaged `.build` and DerivedData are left intact.

A nonblocking project lock serializes Glaeda Apple commands across every profile
and generation, including project helpers that write shared app bundles. Other
build entrypoints do not participate in this lock: coordinate them explicitly.
This is cooperative single-user development, not hostile-workload isolation.

## Failure, recovery, and cold rebuilding

The private ownership marker is bound to the checkout. An atomic, fsynced in-flight
record precedes command launch. The command has its own process group, and SIGINT
or SIGTERM is forwarded. A normal return is settled only after the group is absent.
An interrupted controller leaves the record in place, even after its OS lock is
released. Subsequent builds refuse instead of assuming the cache is safe.

```sh
scripts/apple-build plan --project /path/to/project
scripts/apple-build recover --project /path/to/project --run-id EXACT_ID
scripts/apple-build run --project /path/to/project --generation recovery-1
```

Recovery checks the exact run id and absence of its recorded process group. It
never signals a stale PID, claims the interrupted build succeeded, or deletes
build data. The interrupted generation is quarantined; use a new generation
label for a cold rebuild, then keep that label for subsequent warm iterations.
A crash before the child's process group was durably recorded is intentionally
ambiguous and requires operator inspection; this slice cannot safely infer
whether a child was launched. Detached descendants are outside this contract.

To diagnose stale native state without deleting it, choose a fresh generation:
`run --generation diagnostic-1`. Old generations are retained for inspection;
this prototype performs no automatic eviction or broad cache cleanup. Disk use
can therefore grow after toolchain/configuration changes. All generated state is
rebuildable from the checkout and installed toolchain. Installation of SDKs,
signing credentials, releases, app launch, and remote Apple fleet leasing remain
owned by the project/operator's existing flows.
