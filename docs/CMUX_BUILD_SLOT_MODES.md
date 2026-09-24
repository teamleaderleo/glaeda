# cmux build slots: catch-up mode and iteration mode

Status: design, from measurements on the Manaflow minis (cmuxterm-hq#573 slice 5, #1134).
Evidence: [mini to mini](experiments/fleet-compilation-cache-minis-2026-09-24.md),
[prefetch and floor](experiments/fleet-compilation-cache-prefetch-2026-09-24.md).

## Why two modes

Xcode's compilation cache and incremental builds do not mix for the cmux app target:

| Build (full `cmux` app, M4 Pro) | Caching on | Caching off |
| --- | ---: | ---: |
| fresh DerivedData, fleet store warm | 90 to 130 s | about 750 s (cold) |
| one-line app-target edit, warm DerivedData | 451 s (whole target recompiles) | about 40 to 48 s |
| no-op, warm DerivedData | | about 11 s |

The caching-off edit and no-op numbers are from the Air Blue campaign (Xcode 27) and still
need measuring on a mini at the fleet pin; the rest are from the minis on Xcode 26.3. The
fresh-build row assumes the fleet store already holds that commit. With caching on, every
compile job's key covers the whole module, so any edit misses every job in the app target.

Turning caching off for the app target alone (a per-target macro) while packages stay cached
is not adopted for now: one fresh build that way (Xcode 26.3, against a 26.3 store) lost the
package hits (76 against about 1,535) and took 802 s. The cause was not diagnosed (the macro
may reach package compiles), and the warm edit and no-op runs that would decide the question
were invalidated by an Xcode switch mid-run, so this stays open (Next, item 1).

So a slot keeps two DerivedData directories and picks per job:

- **Catch-up mode (caching on):** a fresh machine, a slot far behind main, or a one-shot build
  of a commit (CI, a PR product). Fed by the fleet store: about 100 s from nothing.
- **Iteration mode (caching off):** edit loops on a slot that is already warm at, or near, the
  commit being edited. About 40 to 48 s per app edit and 11 s no-op (Air Blue, Xcode 27); under
  20 s needs a smaller app module (manaflow-ai/cmux#13108).

A **slot** is one checkout plus its iteration DerivedData, leased to one user or agent at a
time. Slots on a host never build at the same time: every build takes the host lock.

## Paths

| What | Path | Same path on every machine? |
| --- | --- | --- |
| catch-up DerivedData | `/Users/Shared/cmux-build-fleet/xcode/DerivedData` | required: part of every cache key |
| local CAS | `/Users/Shared/cmux-build-fleet/xcode/cas` | required: part of the app target's keys |
| node socket | `/Users/Shared/cmux-build-fleet/xcode/fleet-cas.sock` | per host |
| node store | `/Users/Shared/cmux-build-fleet/xcode/node-store` | per host |
| iteration DerivedData | `/Users/Shared/cmux-build-fleet/xcode/slot-<n>/iter/DerivedData` | no: never touches the cache |

The catch-up DerivedData is one path per host, not per slot, because its path is in every key
(mapping it crashes swift-frontend on Xcode 26.3 and 26.6). The host lock already serializes
builds, so one catch-up build per host at a time is not a new limit. Iteration directories can
be per slot because nothing is shared from them.

Both path rules hold on the fleet pin, Xcode 26.6 (17F113), measured on cmux8s:

- the `CmuxSettingsUI` chain with the DerivedData mapping still aborts with
  `bad_optional_access`;
- a fresh full-app build against a filled local CAS at the same path hit 4,192 of 4,192 in
  102 s, while the same CAS cloned to a different path hit 1,535 of 4,192 and took 600 s (the
  whole app target missed). Xcode 26.6 reports 4,192 cacheable tasks where 26.3 reported
  4,191, so counts are not comparable across versions.

Catch-up DerivedData lifecycle, since one directory serves every slot on the host:

- emptied at the start of each catch-up build (a kept one would turn the next edit into the
  451 s whole-target rebuild, and it is not where anyone iterates);
- the job copies its products out before releasing the host lock, because the next catch-up
  build on the host empties the directory;
- the local CAS is kept between builds (the 102 s result depends on a filled one), with a size
  budget and least-recently-used eviction in the node daemon (not built yet).

Contract. The host check (`glaeda-mini-fleet check`) can verify:

- the selected Xcode build (`xcodebuild -version`, today 26.6 17F113) is the fleet pin, since
  entries are only shared between identical toolchains (exists: the check verifies the pin);
- the paths above are real directories (not symlinks, not under `/tmp`), owned by the build
  user (planned, once the manifest declares them).

The worker recipe must enforce, since a host check cannot see a job's settings:

- catch-up builds set `COMPILATION_CACHE_ENABLE_CACHING=YES`, the CAS path above,
  `SWIFT/CLANG_ENABLE_PREFIX_MAPPING=YES` and `SWIFT/CLANG_ENABLE_PROJECT_PREFIX_MAPPING=YES`,
  and no DerivedData mapping (planned);
- the two plugin settings come only from `fleet-cas-settings.sh <socket>`, which prints them
  only when the node answers (a dead socket makes a build crawl instead of failing; the script
  exists, the recipe change is planned);
- the fleet store is segmented by Xcode build (planned; the prototype has one namespace).

Writer: CI's main build is the trusted writer, filling the store for every main commit it
builds (planned; the prototype has no signed writes yet, see #1134 M3). Nothing else writes.

## When to switch

Catch-up is only fast when the store already holds the target commit; on a store miss it
is a cold build (about 750 to 800 s), slower than an incremental caching-off rebuild. So the
worker asks first: the writer records a marker per commit it has filled (planned), and
catch-up is chosen only when the marker for the target commit exists.

| Situation | Mode |
| --- | --- |
| store lacks the target commit | iteration (incremental from wherever the slot is) |
| no warm iteration DerivedData for this slot, store has the commit | catch-up for the first product; warm iteration behind it |
| iteration DerivedData at main, change is app-target only | iteration |
| main moved and touched a low-level package (the app would recompile anyway) | catch-up for the product, iteration rewarms behind it |
| CI or a PR product build, nobody iterating | catch-up |

"Behind it" means a background build in the slot's iteration DerivedData at the same commit,
started once the catch-up product is delivered. It takes the host lock like any job, and a
`flock` does not preempt, so a foreground job must be able to cancel it: the warmer registers
its xcodebuild process, the foreground job stops it and requeues the warm (planned). An
interrupted incremental build leaves DerivedData usable; the next build redoes the unfinished
work.

## Warming the iteration DerivedData

It cannot be derived from the catch-up DerivedData: caching changes every compile job's command
line, so flipping a DerivedData from caching on to caching off rebuilds everything (776 s on
Air Blue, Xcode 27; to be confirmed on 26.6 before the worker relies on it). The iteration
DerivedData is therefore warmed by its own caching-off builds:

- an idle warmer rebuilds a slot's iteration DerivedData at main's tip, but only a slot with
  no active lease and a clean tree; it never moves a checkout that has local changes or is on
  someone's branch;
- main moves about 1.5 commits an hour; the warm-slots campaign (Air Blue, Xcode 27) measured
  36 s for a new tag into a warm pair and 620 s when a low-level package changed. How often a
  main commit touches a low-level package has not been counted, so the warmer's average cost
  is unknown;
- warming a slot from nothing is a cold caching-off build (about 750 s on a mini), so a host
  warms its slots one at a time, idle only, and a new slot is usable in catch-up mode before
  its iteration DerivedData is ready;
- the source checkout per slot stays at a fixed path, so incremental state stays valid.

## Next

1. On a mini at the fleet pin: caching-off edit and no-op times in a warm iteration
   DerivedData, the flip-is-a-full-rebuild result, and the mixed-mode edit and no-op runs
   (with the fresh-build hit loss diagnosed).
2. Cut the non-compiler work a catch-up build still does. Summed task time, not wall time:
   SwiftDriver planning and scanning 163 s, script phases 18 s (Rust diff sidecar, nucleo FFI,
   wireguard-go), App Intents extraction 16 s over 89 tasks. Script phases can be cached by
   their inputs and App Intents extraction skipped for modules without intents; planning is
   the largest item and has no lever yet. How far below the 77 to 86 s warm-node floor this
   gets has to be measured.
3. Worker and recipe change in cmuxterm-hq build-fleet: move catch-up builds from per-job
   `job-volumes` DerivedData to the fixed path, add the iteration directories, the writer's
   per-commit marker, and warmer cancellation.
