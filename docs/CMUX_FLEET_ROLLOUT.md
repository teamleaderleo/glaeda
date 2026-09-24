# cmux fleet rollout: merged means deployed

Status: design. Owner issue: #149 (automated updates with canary checks and rollback).
Builds on [host reconciliation](HOST_RECONCILIATION.md), [fleet distribution](FLEET_DISTRIBUTION.md),
the mini-fleet manifest, and host reservations (`glaeda_reservation.py`).

## The problem, measured on 2026-09-24

Six build-fleet PRs merged in cmuxterm-hq in one day (#575, #576, #579, #581, #584, #585).
By evening none of them ran anywhere:

- Deploying meant an operator copying a binary, recipes and a wrapper to each mini over SSH,
  then restarting the worker. Agents doing this were blocked by their permission classifier
  ("remote shell writes"), so every step became a command handed to a human.
- Restarting a system LaunchDaemon needs `sudo`, and only 2 of 15 minis have passwordless sudo.
- The minis have drifted: `bin/worker` is a wrapper that points at a different binary on
  almost every host (`worker-policy-<hash>`, `worker-telemetry-<hash>`, `worker-snapshot-20260921`,
  `worker-recovery-<hash>`). Nobody can say from one place what each host runs.
- The controller runs on cmux-lawrence, which only its owner may touch, so any controller change
  waits on one person.
- Three workers were silent for days (a rotated controller token never reached them) and nobody
  noticed, because nothing compares what a host runs against what it should run.

## Goal

A merged, reviewed build-fleet change reaches every eligible worker without a human copying
files. It reaches one canary first, promotes itself when the canary stays healthy, rolls
itself back when it does not, and never touches a host that is building, reserved, or pinned.

## Design

### 1. Releases are built once, in CI

On every push to cmuxterm-hq `main` that touches `build-fleet/`, a workflow builds one release:
the worker binary (darwin/arm64), `recipes/`, `with-host-lock`, and the client files. It writes a
`release.json` with the source commit, per-file SHA-256, and the minimum controller
capabilities the release expects. The release is published as a GitHub release asset, with a
build-provenance attestation (`actions/attest-build-provenance`), and mirrored to the LAN cache
(`172.20.21.158:8787`) so minis do not depend on GitHub for bytes.

Operators and agents never build a worker on their own machine. Every host runs the same
bytes, identified by one hash.

### 2. Desired state lives in the manifest

Each member in the mini-fleet manifest gains a `worker` block:

```json
"worker": {"ring": "canary", "build_slots": 2, "pin": null}
```

- `ring`: `canary` or `stable`. One or two `std` minis are canary.
- `build_slots`: `CMUX_CI_BUILD_SLOTS` for that host (2 on `std`, 1 on `light`).
- `pin`: a release hash or `"manual"`. A pinned host is never upgraded automatically. Use it
  for hosts running an experiment (today's `worker-snapshot-*` and `worker-recovery-*` hosts
  would be `manual` until their owners release them).

A small `channels.json` in the same repository maps each ring to a release hash:

```json
{"canary": "<sha256>", "stable": "<sha256>", "paused": false}
```

Changing desired state is a reviewed PR, like everything else. `paused: true` stops all rollout
fleet-wide.

### 3. A pull agent on each mini applies it

`glaeda-fleet-agent` runs on each mini as the build user (`cmux`), from a LaunchDaemon installed
once at enrollment. It needs no sudo afterwards, because the two things it changes are owned by
`cmux`: the versioned binaries under `bin/`, and the `bin/worker` wrapper. The worker daemon is
`KeepAlive` with `UserName cmux`, so stopping an idle worker makes launchd relaunch it on the new
wrapper.

Every few minutes it runs the reconciliation loop:

1. **Observe.** Record the wrapper target, running binary hash, recipe release, slot count,
   reservation marker, and host lock state. Unknown stays unknown and blocks mutation
   (`needs_inspection`), as in host reconciliation.
2. **Plan.** Compare against the manifest and `channels.json` for this host's ring and pin.
3. **Apply**, only when all of these hold:
   - the target release verifies: hash matches, and the attestation checks out;
   - the host lock is free, there is no active reservation, and the host has no leased or running job;
   - the controller advertises every capability the release lists (so order stops mattering);
   - the host is not `pin: manual` and not the coordinator.

   Apply installs `bin/worker-<hash>` and the recipe release (`recipe-release.py`), writes
   `bin/worker.env` from the manifest (slot count and other per-host settings that live in the
   root-owned plist today), swaps the wrapper atomically, keeps the previous wrapper, and stops
   the idle worker.
4. **Verify.** Within 2 minutes the relaunched worker must run the new hash, heartbeat without
   errors, and pass `worker cache-check`. On failure, restore the previous wrapper, restart, and
   record the failure.
5. **Report.** Write a receipt (before, after, verification, rollback) under
   `/Users/Shared/cmux-build-fleet/receipts/rollout/`, and include the running release hash in
   the worker heartbeat so `glaeda-fleet-status` shows drift fleet-wide.

The agent never runs while the host is busy, never interrupts a job, and never deletes the
previous two releases.

### 4. Promotion is automatic and gated

A scheduled workflow reads canary receipts and controller job history. It promotes the canary
hash to `stable` by merging a one-line `channels.json` PR once the canary has:

- run the new release for at least 30 minutes,
- completed at least 2 real jobs without a worker-attributed failure, and
- rolled back zero times.

Any canary rollback, or a jump in worker-attributed failures on stable hosts, sets
`paused: true` and opens an issue with the receipts. Humans review PRs, not deploys.

### 5. Compatibility rules make order irrelevant

Automatic rollout only works if a new worker runs against an old controller and the reverse.
The pattern from #575 and #584 becomes a rule for every build-fleet PR:

- a new capability is announced by the controller (`/v1/capabilities`) and used by clients only
  when present;
- a job that needs new worker behaviour carries a worker label (`source-patch-v1`) that only new
  workers advertise;
- new heartbeat or job fields are optional in both directions.

The release's `release.json` lists the capabilities it requires, and the agent waits for them.

### 6. The coordinator stays opt-in

cmux-lawrence is reserved for its owner. The agent refuses it by name and address (as
`glaeda-fleet-cas-rollout` already does). The controller gets its own ring, `coordinator`, which
installs only after its owner approves the specific hash, for example with a signed
`approve <hash>` file or a one-click workflow he runs. Until then the capability rule above keeps
new workers useful against the old controller.

### 7. Credentials

The token rotation that silenced three workers is the same drift problem. The controller token
moves from each plist into `/Users/Shared/cmux-build-fleet/secrets/controller.token` (already
used by newer hosts). Rotation publishes the new token through the same agent, as an
out-of-band secret the agent fetches with its own per-host credential, never through the public
release. A worker that gets `401` for 10 minutes reports `credential_rejected` in its heartbeat
receipt, so it shows up in fleet status instead of going silent for days.

## What this replaces

| Today | With the agent |
| --- | --- |
| Operator copies files over SSH, restarts with sudo | Merge the PR; the canary picks it up within minutes |
| Per-host wrappers pointing at hand-built binaries | One release hash per ring, drift visible in fleet status |
| Settings edited in root-owned plists | `bin/worker.env` from the manifest, applied by the agent |
| Silent workers found days later | Rollout and credential failures raise an issue |
| Controller changes block worker changes | Capability-gated releases, any order |

## Milestones

1. **Release workflow** in cmuxterm-hq: build, hash, attest, publish, mirror to the LAN cache.
2. **Agent, observe-only**: install at enrollment, report running hash and drift to
   `glaeda-fleet-status`. No mutation. Surfaces today's drift on day one.
3. **Agent apply on the canary ring**, with verification and rollback. Move `CMUX_CI_BUILD_SLOTS`
   and the controller token path into `worker.env`.
4. **Automatic promotion** to stable, the pause switch, and the failure issue.
5. **Coordinator ring** with owner approval, and credential rotation through the agent.

## Open questions

- Where the agent's per-host credential for fetching secrets comes from at enrollment.
- Whether the one-time enrollment step (installing the agent daemon) can reuse the existing
  `cmux-fleet-bootstrap-macos` path, which already needs an administrator once.
- Soak thresholds (30 minutes, 2 jobs) are guesses; set them from the first month of receipts.
