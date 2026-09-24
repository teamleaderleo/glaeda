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

(Caching-off edit and no-op numbers are from the Air Blue campaign; the rest are from the
minis on Xcode 26.3.) With caching on, every compile job's key covers the whole module, so any
edit misses every job in the app target. Turning caching off for the app target alone while
packages stay cached is not a way out: on a fresh build the package hits collapsed (76 instead
of about 1,500) and the build took 802 s, as long as cold.

So a slot keeps two DerivedData directories and picks per job:

- **Catch-up mode (caching on):** a fresh machine, a slot far behind main, or a one-shot build
  of a commit (CI, a PR product). Fed by the fleet store: about 100 s from nothing.
- **Iteration mode (caching off):** edit loops on a slot that is already warm at, or near, the
  commit being edited. About 40 s per app edit, 11 s no-op; under 20 s needs a smaller app
  module (manaflow-ai/cmux#13108).

## Paths

| What | Path | Shared across machines? |
| --- | --- | --- |
| catch-up DerivedData | `/Users/Shared/cmux-build-fleet/xcode/DerivedData` | yes: part of every cache key |
| local CAS | `/Users/Shared/cmux-build-fleet/xcode/cas` | yes: part of the app target's keys |
| node socket | `/Users/Shared/cmux-build-fleet/xcode/fleet-cas.sock` | per host |
| node store | `/Users/Shared/cmux-build-fleet/xcode/node-store` | per host |
| iteration DerivedData | `/Users/Shared/cmux-build-fleet/xcode/slot-<n>/iter/DerivedData` | no: never touches the cache |

The catch-up DerivedData is one path per host, not per slot, because its path is in every key
(mapping it crashes swift-frontend on Xcode 26.3 and 26.6).

Both path rules hold on the fleet pin, Xcode 26.6 (17F113), measured on cmux8s:

- the `CmuxSettingsUI` chain with the DerivedData mapping still aborts with
  `bad_optional_access`;
- a fresh full-app build against a filled local CAS at the same path hit 4,192 of 4,192 in
  102 s, while the same CAS cloned to a different path hit 1,535 of 4,192 and took 600 s (the
  whole app target missed). The host lock already serializes
builds, so one catch-up build per host at a time is not a new limit. Iteration directories can
be per slot because nothing is shared from them.

Contract, checked by `glaeda-mini-fleet check`:

- the paths above are real directories (not symlinks, not under `/tmp`), owned by the build
  user;
- the selected Xcode build (`xcodebuild -version`, today 26.6 17F113) is the fleet pin, since
  entries are only shared between identical toolchains; the store is segmented by it;
- catch-up builds set `COMPILATION_CACHE_ENABLE_CACHING=YES`, the CAS path above,
  `SWIFT/CLANG_ENABLE_PREFIX_MAPPING=YES` and `SWIFT/CLANG_ENABLE_PROJECT_PREFIX_MAPPING=YES`,
  and no DerivedData mapping;
- the two plugin settings come only from `fleet-cas-settings.sh <socket>`, which prints them
  only when the node answers (a dead socket makes a build crawl instead of failing).

## When to switch

| Situation | Mode |
| --- | --- |
| no warm iteration DerivedData for this slot | catch-up for the first product; warm iteration behind it |
| iteration DerivedData at main, change is app-target only | iteration |
| main moved and touched a low-level package (the app would recompile anyway) | catch-up for the product, iteration rewarms behind it |
| CI or a PR product build, nobody iterating | catch-up |

"Behind it" means a background build in the slot's iteration DerivedData at the same commit,
started once the catch-up product is delivered and yielding to any foreground build (it takes
the host lock like any job).

## Warming the iteration DerivedData

It cannot be derived from the catch-up DerivedData: caching changes every compile job's command
line, so flipping a DerivedData from caching on to caching off rebuilds everything (776 s on
Air Blue, Xcode 27; to be confirmed on 26.6 before the worker relies on it). The iteration
DerivedData is therefore warmed by its own caching-off builds:

- an idle warmer rebuilds each slot's iteration DerivedData at main's tip (main moves about
  1.5 commits an hour, so most rebuilds are incremental; the warm-slots campaign measured 36 s
  for a new tag into a warm pair, 620 s when a low-level package changed);
- the source checkout per slot stays at a fixed path too, so incremental state stays valid.

## Next

1. Confirm the flip-is-a-full-rebuild result on Xcode 26.6.
2. Cache the non-compiler work a catch-up build still does: script phases (about 18 s summed:
   Rust diff sidecar, nucleo FFI, wireguard-go) keyed by their inputs, and App Intents metadata
   extraction (16 s over 89 tasks, most modules have no intents). Target: catch-up under 60 s.
3. Worker and recipe change in cmuxterm-hq build-fleet: move catch-up builds from per-job
   `job-volumes` DerivedData to the fixed path, and add the iteration directories.
