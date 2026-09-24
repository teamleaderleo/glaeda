# Fleet compilation cache: prefetch, fail-fast, and where a fresh build's time goes

Follow-up to [the mini-to-mini measurement](fleet-compilation-cache-minis-2026-09-24.md), slice 5
of cmuxterm-hq#573 (#1134 M2). Same minis (M4 Pro, Xcode 26.3), same cmux commit
(`2ae26d1c`), full `cmux` scheme, fixed DerivedData and CAS paths. The fleet store ran on
cmux8s and was unloaded; the reader also ran on cmux8s, because cmux7s was still running a
Chromium fleet job, so these numbers isolate the protocol, not the network between machines.

## Results

| Run | Wall | Upstream calls | Summed upstream time |
| --- | ---: | ---: | ---: |
| fresh reader, one fetch per object (`--no-prefetch`) | 90.8 s | 25,506 | 226 s |
| fresh reader, closure prefetch | 96.7, 96.3 s | 15,409 | 42, 52 s |
| warm node (nothing fetched) | 85.8 s | 0 | 0 |
| fully cache-hit build with timing summary | 91.3 s | | |
| one-line app-target edit, warm DerivedData, caching on | **451 s** | | |

All reader runs: 4,191 of 4,191 cacheable tasks hit.

1. **Prefetch works, but the fetch path is not the bottleneck on a quiet LAN.** Fetching each
   index entry's object closure in one streamed call (`GetClosure`, a glaeda extension served
   beside the LLVM services) removed every per-object fetch (17,801 to 0) and cut summed
   upstream time about 5x, without changing wall time: a fresh read is about 90 to 96 s either
   way, 5 to 10 s over a warm node. The earlier 132 s was the store host being loaded by a
   Chromium build. Prefetch stays because it bounds round trips for off-LAN members, where
   RTT multiplies with 25,000 calls; it needs a remote-member measurement to prove its worth.
2. **A fully cache-hit fresh build is Xcode's own work.** Build timing summary (summed task
   time, not wall): `SwiftDriver` 163 s over 90 tasks (dependency scanning and planning,
   not cached, redone in every fresh DerivedData), `SwiftCompile` 97 s over 3,626 replays,
   script phases 18 s (Rust sidecar, nucleo FFI, wireguard-go), `ExtractAppIntentsMetadata`
   16 s over 89 tasks, link 6 s. A faster cache cannot take this below about 80 s; the levers
   are persistent DerivedData (planning and scans are then incremental), caching the script
   phases' outputs, and skipping App Intents extraction for modules without intents.
3. **With compilation caching on, the app target has no incremental builds.** A one-line edit
   to one app-target file, from a warm DerivedData and a warm node, took 451 s: 5,312 cache
   misses, 1 hit, the whole `cmux` module recompiled. Each compile job's key covers the
   module's sources, so any edit misses every job in the module. (The Air Blue campaign saw
   the same on Xcode 27: 604 s against 48 s with caching off.) Consequence for "<1 min
   day-to-day": the fleet cache is for fresh machines and for catching up after main moves;
   an edit loop on the app target needs a warm DerivedData with caching off, or a smaller app
   module (manaflow-ai/cmux#13108).
4. **The publication-order counter was measuring nothing.** Xcode's index values are its own
   nested record; each output appears as a pair of the plugin's 65-byte ID and the store's
   32-byte ID, several levels down. The prototype only looked for top-level 32-byte values, so
   `kv_put_dangling` and the first prefetch attempt saw no IDs at all. Both now walk the
   nested record. Checked offline on the full-app fleet store: 7,705 entries naming 25,031
   objects, none absent, so the property held; the earlier doc's evidence for it was wrong
   and is corrected there.

## Also in this change

- `scripts/fleet-cas-settings.sh <socket>` prints the plugin build settings only if the node
  daemon accepts a connection on its socket (exit 1 otherwise). `xcode-cache-build.sh` applies
  the same check and drops the plugin settings when the node is down, instead of the 400 s
  crawl. Build wrappers on the fleet should do the same.
- Fleet path convention (agreed for cmuxterm-hq#573; `glaeda-mini-fleet check` reports it):
  DerivedData `/Users/Shared/cmux-build-fleet/xcode/DerivedData`, CAS
  `/Users/Shared/cmux-build-fleet/xcode/cas`, node socket
  `/Users/Shared/cmux-build-fleet/xcode/fleet-cas.sock`, node store
  `/Users/Shared/cmux-build-fleet/xcode/node-store`. Moving jobs from per-job `job-volumes`
  DerivedData to that path is a worker and recipe change in cmuxterm-hq build-fleet.
- Still owed: the cross-machine full-app read (cmux7s reading from cmux8s), and a
  remote-member run with real RTT, which is where prefetch should show.
