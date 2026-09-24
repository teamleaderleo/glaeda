# Fleet status

`scripts/glaeda-fleet-status` answers "what is wrong with the fleet, and who fixes
it" in one typed, bounded JSON document. Humans read the page rendered from it;
agents read the JSON. It is slice 9 of the fleet roadmap (cmuxterm-hq#573,
"Seeing and fixing the fleet").

It is read-only. Collection runs SSH probes, `gh api` reads and HTTP GETs; nothing
on any machine changes.

## Run it

```bash
scripts/glaeda-fleet-status \
  --github-runners orgs/OWNER --queue-repo OWNER/REPO \
  --controller-status ~/Projects/cmuxterm-hq/build-fleet/status.py \
  --cache t5=http://CACHE_HOST:8787/healthz \
  --out fleet-status.json --html fleet-status.html
```

Every source except the manifest is optional. Without a flag the source shows as
`not_collected`, and a source that fails shows as `unavailable` with its error.

| Source | From | Flag |
| --- | --- | --- |
| manifest | `glaeda-mini-fleet` manifest (members, class, roles, `never_touch`) | `--manifest` |
| check, preflight | one `glaeda-mini-fleet` probe per member | on; `--no-fleet-probe` |
| controller | cmuxterm-hq `build-fleet/status.py --json` (needs `CMUX_CI_TOKEN_FILE`) | `--controller-status`, `--controller-json` |
| runners | `gh api orgs/X/actions/runners` or `repos/X/Y/...` | `--github-runners` |
| queue | queued jobs of queued and in-progress runs, 25 runs at most | `--queue-repo` |
| cache | HTTP status of each endpoint | `--cache NAME=URL` |
| lima | `limactl list --json` per member, never on `never_touch` hosts | on; `--no-lima` |

Split collection from building to reuse or replay a view:

```bash
scripts/glaeda-fleet-status collect --sources-out sources.json
scripts/glaeda-fleet-status build sources.json --out status.json
scripts/glaeda-fleet-status render status.json --html status.html
```

Exit status: 0 no error findings, 1 at least one, 2 the tool failed.

## The document

`schema: glaeda-fleet-status/v1`, with `generated_at`, `sources` (state, observed
time, age, error per source), `fleet` (counts), `members` and `findings`.

Each finding:

```json
{
  "id": "controller.stale@cmux-austin-mini-0",
  "source": "controller",
  "code": "stale",
  "severity": "error",
  "member": "cmux-austin-mini-0",
  "summary": "controller: stale; ...",
  "observed_at": 1790260594,
  "action": {
    "id": "restore_worker_heartbeat",
    "who": "person",
    "safe_to_apply": false,
    "command": null,
    "note": "worker heartbeat is older than 120 s; ..."
  }
}
```

- `who` is `agent` (the SSH login user can run it), `password` (needs sudo) or
  `person` (a decision, an account, or a physical step).
- `command` runs from the operator machine, already wrapped in `ssh HOST ...`
  (`ssh -t` when it needs a password). A fix that reads as prose becomes `note`,
  never `command`.
- `safe_to_apply` is true only for read-only commands: refreshing this document,
  `glaeda-disk`, listing a runner's launchd job. Everything that changes a machine
  is false, including `glaeda-mini-fleet apply`, which runs every typed fix for a
  host.
- Members in `never_touch` get no command and no agent action.
- Severity: `error` means broken now (unreachable, stale or unhealthy worker,
  offline runner, a queued job no online runner can take, cache down). `preflight`
  blockers are `warn`: they block onboarding, not today's builds.

A queued job whose labels include `self-hosted` and match no online runner is its
own `queue.no_eligible_runner:<labels>` finding, naming the members whose runners
carry those labels. Hosted and Blacksmith labels are not judged.

## Bounds and privacy

At most 128 members, 500 findings, 240 characters per text field, 400 per
command, 512 KiB per document; `fleet.truncated_findings` counts what was dropped.
Secrets (`ghp_`, `github_pat_`, `Bearer`, `token=`) are redacted and home paths
become `~`. Child processes get an allowlisted environment.

## How an agent uses it

1. Read `sources`; refresh anything `stale` or `unavailable` first.
2. Apply actions with `who: agent` and `safe_to_apply: true`.
3. Hand every other action to a person as its `command`, or its `note` when there
   is no command, grouped by `who`.

Scheduled regeneration, and typed fixes emitted by `glaeda-mini-fleet` itself
instead of parsed fix text, are follow-ups.
