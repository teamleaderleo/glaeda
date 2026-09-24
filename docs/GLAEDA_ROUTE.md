# Glaeda CI routing

Glaeda decides where cmux CI jobs run (glaeda#1174). This page is the contract for what
Glaeda publishes and how readers must treat it. `scripts/glaeda-route` is the executable truth.

## Pool state (`glaeda-pool-state/v1`)

`glaeda-route publish --yes` writes one compact JSON document to the repository variable
`GLAEDA_POOL_STATE` on `manaflow-ai/cmux`. A workflow reads it as `vars.GLAEDA_POOL_STATE`,
with no token and no tailnet. A fork pull request's run gets no repository variables, so it
never sees the document and must never route to an owned pool anyway.

```json
{
  "schema": "glaeda-pool-state/v1",
  "repo": "manaflow-ai/cmux",
  "generated_at": "2026-09-24T15:40:20Z",
  "observed_at": "2026-09-24T15:40:12Z",
  "max_age_seconds": 60,
  "order": ["glaeda-std-xcode-26.6", "glaeda-light-xcode-26.6"],
  "pools": {
    "glaeda-std-xcode-26.6": {
      "class": "std", "xcode": "26.6", "rank": 0,
      "declared": 11, "conforming": 10, "reserved": 1, "locked": 0,
      "runners": 11, "online": 11, "busy": 2, "idle": 8
    }
  }
}
```

| Field | Meaning |
| --- | --- |
| `generated_at` | When the publisher assembled the document (RFC 3339, UTC). |
| `observed_at` | The oldest observation the document rests on: the fleet probe or the GitHub runner listing. Freshness is measured from here. |
| `max_age_seconds` | The publisher's promise. Readers use their own limit, 60 s by default. |
| `order` | Owned pools in routing order: class `std` (48 GB minis), then `light` (16 GB), newest Xcode first within a class. Blacksmith and GitHub-hosted come after, and belong to the caller. |
| `declared` | Members the fleet manifest puts in the pool. |
| `conforming` | Members the fleet probe saw reachable with the pool's exact Xcode, not reserved, not holding the fleet host lock. |
| `reserved` | Members with an active `glaeda-mini-fleet reserve` marker (0 until the probe reports it). |
| `locked` | Members whose fleet host lock was held at observation (0 until the probe reports it). |
| `runners`, `online`, `busy` | Self-hosted runners on the repository that carry the pool label, as GitHub lists them. |
| `idle` | Runners that are online, not busy, and belong to a conforming member (`<member>-glaeda`). The routable capacity at `observed_at`. |

The document carries counts only, never host names or node ids, because cmux workflow
logs are public.

### Reader rules

A reader may route a job to an owned pool only when every one of these holds; otherwise the
job keeps its Blacksmith route. `glaeda-route check --state FILE` applies the same rules.

1. `schema` is exactly `glaeda-pool-state/v1`. A future `v2` is a different document.
2. `repo` is the reader's repository.
3. `generated_at` and `observed_at` parse as dated UTC times, neither in the future by more
   than 30 s, and `observed_at` is at most 60 s old.
4. `order` and `pools` name the same labels, every label is `glaeda-<std|light>-xcode-<version>`,
   and every count is a non-negative integer, with `idle` and `busy` at most `online`.
5. The run is trusted: a same-repository pull request, or a push, merge group, schedule or
   dispatch on the repository itself, and attempt 1. Fork runs never read the document.

`idle` is a snapshot. A reader that places more than one job must count what it has already
placed since `generated_at`, and must still expect a job to queue: GitHub never re-routes a
queued job, so a rescue that re-runs a stuck run on Blacksmith is required (glaeda#1174).

Compatible changes (new fields, new pools) keep `v1`. Removing or redefining a field bumps the
schema, and the publisher then writes both documents until readers move.

## Publisher

```bash
glaeda-route state                     # build and print the document
glaeda-route publish                   # print what would be written
glaeda-route publish --yes             # write GLAEDA_POOL_STATE
glaeda-route check --state FILE        # apply the reader rules
```

`publish` is one-shot. Run it every 20 s from launchd (`StartInterval` 20) or a systemd timer
on an always-on host that holds the fleet manifest and can SSH to the members. Never run it on
cmux-lawrence. It observes the fleet with `glaeda-mini-fleet pools`, lists the repository's
self-hosted runners, and writes the variable. If it stops, the document ages past 60 s and every
reader falls back to Blacksmith; nothing else changes.

### Credentials

One GitHub App, installed on `manaflow-ai/cmux` only, with repository permissions:

| Permission | Access | Why |
| --- | --- | --- |
| Administration | Read | list the repository's self-hosted runners |
| Variables | Read and write | write `GLAEDA_POOL_STATE` |
| Metadata | Read | required by GitHub |

Its private key lives only on the publisher host, in a file only its owner can read
(`chmod 600`). `publish --app-id ID --app-key-file PATH` signs the App JWT with
`/usr/bin/openssl` and mints a one-hour installation token scoped to the one repository, per
run. `--token-file PATH` (same permission check) or `GLAEDA_ROUTE_TOKEN` in the environment
also work, for a fine-grained token with the same permissions. No token is ever taken from
argv, printed, or written to disk.
