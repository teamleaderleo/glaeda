# Native Apple builds

`scripts/apple-build` is the prototype front door for ultra-trusted native Apple
development. It runs SwiftPM, Xcode, or a project's existing executable build
helper. It uses the canonical checkout directly, with no OverlayFS, APFS clone,
extra worktree, VM, or background daemon. The Python front door uses standard
macOS filesystem/process primitives; it is separate from Linux host admission.

From a configured project:

```sh
/path/to/glaeda/scripts/apple-build plan
/path/to/glaeda/scripts/apple-build warm
/path/to/glaeda/scripts/apple-build run --profile app
```

`warm` and `run` execute the same real build. Every invocation executes its native
command; a previous receipt never substitutes for validation. `plan` probes the
checkout and selected Apple toolchain and creates no project state. JSON receipts
contain opaque cache identities and no project paths or raw build output. Private
logs are `.glaeda/apple-build/run-<run-id>.log`; the most recent receipt is
`.glaeda/apple-build/last-run.json`.

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
