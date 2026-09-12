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

### Check code without assembling a launchable app

`check` runs an explicitly declared project helper; `plan-check` is read-only.
Use a top-level `checks` map, keyed by build profile, with a script recipe:

```json
"checks": {
  "app": {
    "engine": "script",
    "executable": "scripts/check-native.sh",
    "arguments": ["{derived_data}", "{source_packages}"]
  }
}
```

The check inherits the build profile's toolchain, environment and cache paths.
It always runs under the same lock and interrupted-state rules. Its helper bytes
and declaration are revalidated after admission. It records `last-check.json`,
including recovery, and never replaces app-build evidence. Adding a check does
not reset the build generation. Native compilers remain responsible for input
validity. A check helper must use native incremental behavior compatible with the
shared build caches; it is trusted project code, not a sandboxed validation service.
Its scope is project-defined: compilation success is not evidence that packaging,
signing, installation, runtime checks, or the test suite passed. Use `warm`/`run`
for the original complete app-build flow.

### Separate dependency preparation

`ensure-dependencies` optionally reuses preparation after a successful resolver
run. A preparation recipe may declare:

```json
"reuse": {
  "inputs": ["Package.swift", "Package.resolved", "Packages/**/Package.swift"],
  "required_files": [{"cache": "source_packages", "path": "artifacts/example/Info.plist"}]
}
```

Input globs are project-relative; their matched names and file contents are
hashed. Required files are relative to a named existing cache category, must
resolve inside it, and are also content-hashed. Observations are bounded to 32
patterns, 512 input files, 32 required files and 16 MiB of total file content.
No matched inputs is an error. Globs observe additions and removals; select all
manifests and configuration files that affect dependency preparation, including
local package manifests. Source code generally does not belong in this set.

Under the normal project lock, an unchanged generation, recipe, input fingerprint
and required-file fingerprint may return `preparation_reused`. Missing files,
changed bytes, absent/failed/legacy receipts or an undeclared reuse policy run the
resolver. Input changes during resolution prevent publication of reusable evidence.
`dependencies` always forces the resolver. Neither action skips app builds,
disables automatic package validation, or certifies the entire artifact tree:
required files are a declared preparation check, not a substitute for compiler
validation. The reusable observation never overwrites the prior dependency run
receipt or the app-build receipt. Active/quarantined/foreign state is never reused.

`dependencies` runs a declared native resolver without building or launching the
app. `plan-dependencies` checks the declaration without creating state. Add an
optional top-level `preparations` map alongside `profiles`, keyed by build profile:

```json
"preparations": {
  "app": {"engine": "xcode", "project": "cmux.xcodeproj", "scheme": "cmux"}
}
```

For SwiftPM use `{"engine":"swiftpm"}` (or add a relative `package` path).
Xcode requires exactly one `project` or `workspace` and a scheme. Preparation
uses the build profile's existing cache generation, toolchain and environment.
Changing this resolver declaration does not invalidate build caches, because it
does not change the build command; its identity is revalidated under the project
lock before execution. The native resolver always runs and validates dependency
inputs. There is no readiness shortcut based on a prior receipt.

Preparation shares the normal lock, process tracking and interrupted-generation
quarantine. Failure never triggers automatic deletion. Its receipt is private
`last-dependencies.json`, separate from `last-run.json`, so resolving packages
cannot replace evidence of an app build. Resolver commands may update lockfiles;
before/after source observations report this, and callers should review changes.
Dependency preparation does not disable resolution in subsequent builds, verify
every package's binary artifacts, or prepare non-package dependencies such as
Ghostty/Zig/Rust. Those are separate measured follow-up stages, not implied by a
successful resolver exit.

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
state live under `.glaeda/apple-build/cache/<key>/`. By default, new settings/toolchains get
separate cold generations. Existing unmarked directories and symlinks are not
adopted. Previously unmanaged `.build` and DerivedData are left intact.

A nonblocking project lock serializes Glaeda Apple commands across every profile
and generation, including project helpers that write shared app bundles. Other
build entrypoints do not participate in this lock: coordinate them explicitly.
This is cooperative single-user development, not hostile-workload isolation.

## Native cache lifetime across pulls and recipe changes

Projects whose builder validates all source, dependency, and compiler inputs can
explicitly opt into native cache lifetime:

```json
"cache_policies": {"app": "native"}
```

The first executed operation binds the existing recipe cache to the physical
checkout, profile name, engine, toolchain/SDK/architecture, and generation label.
This binding is private, atomic, and written under the project lock. Planning
does not create it. Bind the policy before editing a recipe to retain its current
cache; otherwise the first operation binds the new recipe's cache.

Later settings, helper, and profile changes keep these paths. Their exact current
invocation identity is checked again under the lock; the native build still runs
every time. Dependency readiness also binds that invocation identity. This does
not restore an old successful result or certify arbitrary script outputs. Script
profiles should opt in only when their helpers invoke a native builder that owns
input invalidation. Keep the default `recipe` policy for helpers needing clean
outputs after recipe changes.

Toolchain, engine, physical checkout, or generation-label changes start a new
lineage. Quarantine still refuses reuse. `--generation diagnostic-1` remains the
cold fallback; no cache is copied, shared across projects, or deleted. Changing
back to `recipe` uses the current recipe's original cache calculation.

## Compiler experiments and repeatable measurements

Completed receipts include bounded `native_work` advice when the native log
reports Xcode task timings or compilation-cache totals. Only known task names
and numeric values are retained; source paths and raw log text are excluded.
The last reported timing summary and last reported cache total are labelled
separately, rather than added across nested builds. Task timings may overlap.
Unsupported, oversized, or unavailable logs do not fail an otherwise completed
build. None of these reported values grants reuse or execution authority.

SwiftPM profiles accept optional Boolean `incremental_file_hashing` and
`incremental_diagnostics` fields. The former explicitly enables or disables
Swift's file-content hashing; omission preserves the compiler default. Diagnostics
report the native driver's scheduling decisions. Unsupported toolchains fail
through the compiler rather than silently ignoring the option.

Xcode profiles accept `COMPILATION_CACHE_ENABLE_CACHING`,
`COMPILATION_CACHE_ENABLE_DIAGNOSTIC_REMARKS`, and
`COMPILATION_CACHE_LIMIT_SIZE` in their settings. These are opt-in native settings;
Glaeda does not implement a second compiler cache or share writable compiler
state across projects. First enabling an option may require substantial warming.

Run the physical, local-only benchmark on a Mac:

```sh
python3 scripts/benchmark-apple-incremental.py --output /path/to/new-private-report
```

It creates its own fixture under `target`, compares hashing off/on, and exercises
unchanged builds, timestamp-only changes, implementation edits, reverts, branch
returns, public API changes, a real fast-forward Git pull, and a real local Git
dependency-version update. Every resulting executable must return the expected
value. The fixture and builds are removed; private logs, timing summaries, and
toolchain metadata remain in the specified new report directory. These small
fixture timings are not application latency claims. Application benchmarks must
also account for linking, build scripts, signing, and packaging.

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
