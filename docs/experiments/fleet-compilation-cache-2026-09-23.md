# Fleet-shared Xcode compilation cache: first measurements

Status: **feasible, with three blockers**. A fresh machine building a real cmux package chain
went from 118 s cold to 30–33 s when its only warm source was a shared cache
service, with byte-identical compiler outputs. The blockers are an Xcode 27 crash when replaying
some cached warnings, the protocol assuming a same-host service, and index entries that can
point at missing objects. Design and plan: #1134.

## Question

Can every machine in a fleet read one shared compiler cache, so a fresh or disposable machine
builds at close to warm speed, while only trusted builders can write what others will trust?

Today cmux CI shares compilation caches as whole tarballs: `nightly.yml` is the single writer,
saves at most 5 GiB, and pull requests restore all of it. #1068 shares finished products. Neither
shares individual compiler outputs live.

## Mechanism

Xcode 27 (`27A266a`) can send compilation-cache traffic to a remote service:
`COMPILATION_CACHE_ENABLE_PLUGIN=YES` plus `COMPILATION_CACHE_REMOTE_SERVICE_PATH=<unix socket>`,
through the bundled `usr/lib/libToolchainCASPlugin.dylib`. The socket speaks LLVM's open
compilation-caching gRPC protocol (swiftlang/llvm-project
`llvm/lib/RemoteCachingService/RemoteCacheProto`), with two services:

| Service | Calls | Holds | Trust |
| --- | --- | --- | --- |
| `cas.v1.CASDBService` | `Put`, `Get`, `Save`, `Load` | content-addressed objects | server computes each ID from content, so writes cannot impersonate |
| `keyvalue.v1.KeyValueDB` | `GetValue`, `PutValue` | cache key → result | the only poisoning surface |

So the trust split is narrow: anyone may upload objects, only trusted builders may write
index entries.

The prototype is [`tools/fleet-cas-prototype`](../../tools/fleet-cas-prototype): about 300
lines of Rust (tonic), one file per object, SHA-256 IDs, damaged objects read as misses,
first-writer-wins index entries, and an optional read-only index.

## Setup

- Air Blue: Apple M5, 10 cores, 24 GiB, macOS 26.6.2, Xcode 27.0 (27A266a).
- Source: cmux `2ae26d1c7dff6a91104d23258b395f3b1b6940e9` (`upstream/main`),
  `Packages/` extracted with `git archive` into scratch, with no working-tree changes to the shared checkout.
- Workload: `CmuxSettingsUI` scheme (CmuxSettingsUI, CmuxSettings, CmuxFoundation,
  CMUXMobileCore; 600 Swift source files), `xcodebuild build`, Debug, macOS.
- Each run uses its own new DerivedData path and its own empty local CAS. Prefix mapping
  (`SWIFT/CLANG_ENABLE_PREFIX_MAPPING`, `…_PROJECT_PREFIX_MAPPING`,
  `…_OTHER_PREFIX_MAPPINGS=<DerivedData>=/^derived`) keeps keys path-independent.
- Harness: [`xcode-cache-build.sh`](../../tools/fleet-cas-prototype/scripts/xcode-cache-build.sh).
  Hits and misses are counted from Xcode's own `cache hit` / `cache miss` remarks.

## Results

| Run | Warm source | Wall | Hits / misses | Notes |
| --- | --- | ---: | ---: | --- |
| cold | none (caching off) | 118.4 s | – | baseline |
| fill | empty service | 131.2 s | 10 / 338 | +11% to compute keys and upload 201 MB |
| fresh 1 | service only | **33.3 s** | 189 / 1 | 3.6x faster than cold |
| fresh 2 | service only | **30.1 s** | 190 / 0 | 3.9x |

`SWIFT_SUPPRESS_WARNINGS=YES` was set for these four runs (see blocker 1).
Store after the fill: 177 MB, 1,674 objects, 716 index entries. The remaining ~30 s is
work the cache does not cover: package resolution, dependency scanning, build planning,
linking, and bundle steps.

### Correctness and failure checks

- **Identical outputs.** On a synthetic three-module package, every product file from the
  service-fed build (including each `.swiftmodule`) and a sampled per-file object were
  byte-identical to the build that filled it. Only the three linked per-product `.o` files
  differed; linking is not cached and embeds paths.
- **Read-only client.** A build with one changed module against `--read-only-kv` succeeded:
  unchanged modules hit (24), the changed module compiled locally, and all 13 index writes
  were refused. Xcode shows each refusal as a warning, not an error.
- **Corruption.** With every stored object's bytes flipped, the next build had 0 hits, compiled
  everything locally, and succeeded. Xcode also checks for itself: it reported
  `failed to update cache: cache poisoned` for mismatched results and did not use them.
  Before the fix, a re-upload did not replace a damaged object. Now it does.

## Blockers found

1. **Xcode 27 build-service crash replaying some cached warnings.** A fresh DerivedData
   replaying `CmuxFoundation` from a warm cache crashes `SWBBuildService` (`SIGABRT`,
   `std::bad_optional_access` in
   `swift::FileSpecificDiagnosticConsumer::subconsumerForLocation`, reached from
   `swift::CachingDiagnosticsProcessor::replayCachedDiagnostics`). It is not caused by the
   service: the same crash happens with a shared *local* CAS and no remote, and with an
   identical DerivedData path. It goes away when the package emits no warnings (fixing its
   warnings: 90 hits, 0 misses, 7.1 s vs 18–37 s for the filling builds), and with
   `SWIFT_SUPPRESS_WARNINGS=YES`. Synthetic packages with the same warning kinds did not
   reproduce it, so the exact trigger is not yet isolated. Repro: fill and replay
   `Packages/macOS/CmuxFoundation` at the revision above into two DerivedData paths sharing one
   `COMPILATION_CACHE_CAS_PATH`. To do: report it upstream, and check whether cmux CI's
   existing tarball cache hits it.
2. **The protocol assumes a same-host service.** For large blobs the client sends
   `CASBytes.file_path`, a path on its own disk, not the bytes. With the service on Big Red over
   an SSH-forwarded socket, those uploads failed (`invalid argument … localcas-…/plugin/temp-…`),
   the fill took 223 s and uploaded 47 MB of 201 MB, and the next build hit 55 of 288. The
   round-trip time was about 100 ms, so this is also a wide-area worst case. Consequence: every
   machine runs a local cache daemon that Xcode talks to, and the daemon talks to the fleet.
   That is the tier design, now required.
3. **Index entries can reference missing objects.** In that wide-area run, entries landed for
   objects that never arrived (`makeBlob: missing object for ID`). Xcode rejected them and the
   build still passed, but the store must refuse an entry until every object it references is
   present: the staged-publication rule #1068 already uses.

## Cleanup

All prototype servers were stopped: local ones with SIGINT, and the Big Red one by exact PID
after its SSH forward exited. Scratch DerivedData, local CAS directories, stores, and the
extracted cmux sources live only in the session scratch directory. Big Red keeps
`~/scratch/fleet-cas` (source and build) and a 41 MB `~/scratch/fcas-store`; both can be
deleted and rebuilt from this branch. Nothing was installed and no service was registered.
