# Glaeda pool state for cmux CI

Glaeda publishes how much of the owned Mac fleet is free right now, and cmux's PR pool picker reads
it (glaeda#1174). `scripts/glaeda-route` and `scripts/glaeda_route_probe.sh` are the executable truth.

**The fleet owns placement.** A mini that is full, busy with a fleet build, reserved, or not an
eligible node refuses a job at start (`glaeda-cmux-runner-hook`: `refused: capacity: ...`,
`refused: node not eligible ...`), and cmux's rescue re-runs a refused job (cmux#14299, #14312). So
the published state is a hint, never a reservation: a race between two runs costs one retry, not a
hang. There is no ledger, no route endpoint and no App.

**Order, for every job type:** `std` minis (48 GB), then `light` minis (16 GB), then Blacksmith, then
GitHub-hosted. **Trust:** fork runs never read the state and never reach an owned machine.

## Pool state (`glaeda-pool-state/v2`)

`glaeda-route publish --yes` writes one compact JSON document to the repository variable
`GLAEDA_POOL_STATE` on `manaflow-ai/cmux`. A workflow reads it as `vars.GLAEDA_POOL_STATE`, with no
token; a fork pull request's run gets no repository variables.

```json
{
  "schema": "glaeda-pool-state/v2",
  "repo": "manaflow-ai/cmux",
  "generatedAt": "2026-09-25T00:10:20Z",
  "observedAt": "2026-09-25T00:10:16Z",
  "maxAgeSeconds": 90,
  "order": ["glaeda-std-xcode-26.6", "glaeda-light-xcode-26.6"],
  "pools": {
    "glaeda-std-xcode-26.6": {
      "class": "std", "xcode": "26.6",
      "declared": 11, "eligible": 7, "runners": 35, "online": 32, "busy": 3,
      "free": {"units": 16, "compile": 3, "gui": 5}
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `generatedAt` | When the document was written. Readers require it within 90 s of their own start. |
| `observedAt` | When this tick's observations began. |
| `order` | Owned pools in routing order: `std`, then `light`, newest Xcode first within a class. |
| `declared` | Runner members the fleet manifest puts in the pool. |
| `eligible` | Members whose runner hook `check` (the read-only eligibility gate) passed within the last 10 minutes. |
| `runners`, `online`, `busy` | Self-hosted runners carrying the pool label, as GitHub lists them (every instance, `<member>-glaeda-K`). |
| `free.units` | Free capacity units, summed over members that offer any (below). A compile costs 2, anything else 1. |
| `free.compile` | Members that can start a compile now: `persistent-dd` token free and at least 2 units free. |
| `free.gui` | Members that can start a GUI job now: `gui` token free and at least 1 unit free. |

A member offers capacity only when every one of these holds: the probe finished, its last
eligibility check passed, it has no active or invalid reservation, no fleet build holds its host
lock exclusively, and at least one of its runners is online. Units and tokens come straight from
the runner hook's capacity ledger (`/Users/Shared/cmux-build-fleet/capacity`). A member with two
pool labels counts in the first pool of `order` only. The document carries counts only, never host
names, because cmux workflow logs are public.

### Reader rules

A reader uses the document only when all of these hold; otherwise it keeps its own behaviour.
`glaeda-route check --state FILE` applies the same rules.

1. `schema` is exactly `glaeda-pool-state/v2`, and `repo` is the reader's repository.
2. `generatedAt` and `observedAt` are dated UTC times, not more than 30 s in the future, and at most
   90 s old.
3. `order` and `pools` name the same `glaeda-<std|light>-xcode-<version>` labels, and every count is
   a non-negative integer, with `busy <= online <= runners`.
4. The run is a trusted same-repository run. Fork runs never read the document.

Compatible changes (new fields, new pools) keep `v2`. Removing or redefining a field bumps the schema.

## Publisher

```bash
glaeda-route state                 # build and print the document (probes over SSH)
glaeda-route publish               # print what would be written
glaeda-route publish --yes         # write GLAEDA_POOL_STATE
glaeda-route check --state FILE    # apply the reader rules
```

Each run is one tick: list the repository's runners with `gh api`, probe every runner member in
parallel over SSH (read-only, 15 s bound, `scripts/glaeda_route_probe.sh` on stdin), run the hook's
eligibility `check` for members never checked plus the four oldest past 5 minutes (60 s bound),
then `gh variable set GLAEDA_POOL_STATE`. A tick takes about 5 s. If the publisher stops, the
document ages past 90 s and cmux falls back to its own picker; nothing else changes.

The probe writes nothing on the member. It opens existing ledger files read-only (never creating
them), takes a non-blocking shared flock and drops it at once; the ledger holds units and tokens
exclusively, so a shared test fails exactly when a job holds one. An admission that lands in those
microseconds takes the next unit, or is refused for a token and re-run by cmux's rescue.

### Access

- **Host:** `cmux7s-mac-mini` (cache host, no runner). Never a runner host, never cmux-lawrence.
- **GitHub:** the host's own `gh` login. Listing self-hosted runners and setting a repository
  variable both need admin on `manaflow-ai/cmux`. Nothing else is stored.
- **SSH:** from `cmux7s-mac-mini` as `cmux` to each runner member as the manifest's `ssh_user`
  (`cmux`), over the LAN: `hosts.<m>.lan_ip` when the manifest sets it, else `<hostname>.local`
  (Manaflow's tailnet blocks mini-to-mini TCP, `--address name` uses the tailnet names instead).
  The probe needs no sudo and runs only `/bin/bash -s` with the script on stdin. SSH runs with
  `BatchMode=yes`, so each member's host key must already be in the host's `known_hosts` for the
  address used.
- **Manifest:** `~/.config/glaeda/mini-fleet.json` on the host.

## Runbook: start the publisher on cmux7s-mac-mini

As `cmux` on `cmux7s-mac-mini`:

```bash
git clone https://github.com/teamleaderleo/glaeda.git ~/glaeda-route      # a dedicated checkout
gh auth login --hostname github.com --git-protocol https --insecure-storage # an account with admin on manaflow-ai/cmux
# copy the fleet manifest from the operator machine:  scp ~/.config/glaeda/mini-fleet.json cmux@cmux7s-mac-mini:.config/glaeda/
python3 ~/glaeda-route/scripts/glaeda-route state | head -40                # SSH and eligibility work?
python3 ~/glaeda-route/scripts/glaeda-route publish                         # dry run
sed "s#@HOME@#$HOME#g" ~/glaeda-route/examples/launchd/com.teamleaderleo.glaeda.route-publish.plist \
  > ~/Library/LaunchAgents/com.teamleaderleo.glaeda.route-publish.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.teamleaderleo.glaeda.route-publish.plist
tail -f ~/Library/Logs/glaeda-route-publish.log                             # "wrote GLAEDA_POOL_STATE: ..."
```

`--insecure-storage` keeps gh's token in `~/.config/gh/hosts.yml` (mode 600) so a LaunchAgent can
read it without an unlocked login keychain. Update with `git -C ~/glaeda-route pull --ff-only`; the
next tick uses it.

**Stop:** `launchctl bootout gui/$(id -u)/com.teamleaderleo.glaeda.route-publish`. The variable
ages out within 90 s and cmux falls back on its own.

**Check from anywhere:**
`gh variable get GLAEDA_POOL_STATE -R manaflow-ai/cmux > /tmp/s.json && glaeda-route check --state /tmp/s.json`
