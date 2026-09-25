# Over-the-air glaeda updates

Status: live for the glaeda host tools (glaeda-disk, glaeda-worktree-reclaim, glaeda-fleet-cas-prune,
glaeda-update itself, and the LaunchAgents and systemd units that run them). Owner issues: #525
(releases) and #149 (updates on hosts). The fleet runtime bundle is published in the same release
but still installs through `glaeda-mini-fleet upgrade` (below).

## How a merge reaches every host

1. **Release on merge.** `.github/workflows/release.yml` builds once per push to `main`, for
   `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu`, and publishes the GitHub release
   `r-<UTC date>-<commit[:12]>`:
   - `glaeda-hygiene-<target>.tar.gz`: the committed tree under `glaeda/` plus a prebuilt
     `bin/glaeda-worktree-reclaim`;
   - `glaeda-<commit>-<target>.tar.gz`: the fleet candidate bundle (`scripts/fleet_bundle.py`);
   - `release.json`: the source commit and the SHA-256 and size of every asset.

   Every asset gets a build-provenance attestation. Releases are prereleases, so none is marked
   "latest".
2. **Canary.** The same workflow points `canary` in `channels.json` at the new release. The
   file is the one asset of the `ota-channels` release, and it is the only mutable file.
3. **Hosts pull.** `glaeda-update` runs hourly on every host: a LaunchAgent
   `com.teamleaderleo.glaeda.update` on macOS, the systemd user timer `glaeda-update.timer` on
   Linux, with up to 15 minutes of random delay. It reads its ring from
   `~/.config/glaeda/update.json`, then:
   - downloads by hash;
   - checks the attestation when `gh` can;
   - runs the release's own `scripts/glaeda-mini-setup --apply` with the host's setup flags and
     the prebuilt reclaim binary;
   - checks that the installed tools run.

   If the new tools do not run, it re-runs the previous release's setup and quarantines the failed
   release on that host.
4. **Canary health.** A canary host with an authenticated `gh` posts the commit status
   `glaeda-ota/<host>` (success or failure) on the release commit.
5. **Stable.** `.github/workflows/promote.yml` runs hourly. It moves `stable` to the canary
   release once all of these hold:
   - the release is at least 6 hours old;
   - at least one canary host reported success;
   - no canary host's newest status is a failure.

   Stable hosts pick it up within the hour.

End to end, a merged change reaches canary hosts in about an hour and the whole fleet in about
eight.

## Rings

| Host | Ring | Why |
| --- | --- | --- |
| Air Blue | canary | Leo's Mac; `gh` is authenticated, so it reports health |
| Big Red | canary | always on; reports health when the Mac is asleep |
| manaflow minis | stable | build hosts: take only what a canary has run for 6 hours |

Set a host's ring once with `glaeda-mini-setup --apply --ota-ring canary` (or `stable`). The
first setup writes the config with `stable`, and later runs leave it alone unless `--ota-ring`
names another ring.

## Operator controls

```bash
gh workflow run promote.yml --repo teamleaderleo/glaeda -f action=pause    # every host stops updating
gh workflow run promote.yml --repo teamleaderleo/glaeda -f action=resume
gh workflow run promote.yml --repo teamleaderleo/glaeda -f action=promote  # check promotion now
gh release download ota-channels --repo teamleaderleo/glaeda -p channels.json -O -   # what each ring runs
glaeda-update --status                     # on a host: ring, current, previous, quarantined
tail ~/Library/Logs/glaeda-update.jsonl    # macOS; ~/.local/state/glaeda-update.jsonl on Linux
```

A broken release on stable is fixed forward: revert on `main`, and the revert becomes the next
canary. Dispatching `promote` only runs the same check early; it never skips the soak.

## Trust

- Hosts trust GitHub over TLS and nothing by name. `channels.json` pins release.json by
  SHA-256, and release.json pins every asset.
- With `gh` present the attestation must verify. Without it, hash pinning is the check.
- The updater runs only what a release's `glaeda-mini-setup` installs. That command is
  idempotent, never uses sudo, and owns every file it writes (its receipt records them).
- A release comes only from a push to `main` of this repository by someone with write access.
  `main` has no branch protection rule today, so repository write access is the root of trust.

## Not automatic yet

The fleet runtime on the minis (`glaeda` binary, enrollment, class acceptance) still moves with
`glaeda-mini-fleet upgrade --candidate-run RUN --yes`. Its bundle says
`automaticUpdateAuthorized: false`, and renewing it needs a class acceptance (a 13-minute cold
cmux build on a seed per hardware class) before other hosts can adopt it. Automating that means a
canary mini that runs `accept-local` on each stable candidate and publishes the class receipt as
a release asset for the others to adopt. That work is tracked in #149.
