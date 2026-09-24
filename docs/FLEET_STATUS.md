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
- `command` runs from the operator machine, already wrapped in `ssh -- HOST ...`
  (`ssh -t --` when it needs a password), and only for members named in the
  manifest.
- A fix becomes a command only if it fits a closed grammar: a known program or a
  path, then plain words, with no shell metacharacters and no prose words.
  Anything else becomes `note`. Fix text comes from the members' own probes, so
  a hostile member cannot plant shell syntax in a command.
- A command that carries anything secret-looking, a home path, or more than 400
  characters is withheld: the action goes to a person with a note, because
  redacting it would change what runs.
- `safe_to_apply` is true only for read-only commands: refreshing this document
  and listing a runner's launchd job. Everything that changes a machine is false,
  including `glaeda-disk` (it refreshes its own snapshot) and
  `glaeda-mini-fleet apply` (it runs every typed fix for a host, `add_key`
  included).
- Members in `never_touch` get no command and no agent action.
- Severity: `error` means broken now (unreachable, stale or unhealthy worker,
  offline runner, a queued job no online runner can take, cache down). `preflight`
  blockers are `warn`: they block onboarding, not today's builds.

A queued job whose labels include `self-hosted` and match no online runner in
scope is its own `queue.no_eligible_runner:<labels>` finding, naming the members
whose runners carry those labels. Hosted and Blacksmith labels are not judged. The
queue is judged only against a fresh, complete runner list; otherwise it is a
`queue.unjudged` info finding.

Findings from `check` and `preflight` that name the same command for the same
member merge into one, keeping the more severe and listing the other in `also`.

## Bounds and privacy

At most 128 members, 500 findings, 240 characters per text field, 400 per
command, 512 KiB per document. Member fields are typed (numbers stay numbers,
text is cleaned and capped). Over the byte limit, info then warn findings drop
first and `fleet.truncated_findings` counts them; counts always describe the
findings that remain. An error is never dropped to fit: the tool fails instead.
A malformed sources bundle fails with exit 2, never a traceback.
Secrets (`ghp_`, `github_pat_`, `Bearer`, `token=`) are redacted and home paths
become `~`. Child processes get an allowlisted environment.

## How an agent uses it

1. Read `sources`; refresh anything `stale` or `unavailable` first.
2. Apply actions with `who: agent` and `safe_to_apply: true`.
3. Hand every other action to a person as its `command`, or its `note` when there
   is no command, grouped by `who`.

Scheduled regeneration, and typed fixes emitted by `glaeda-mini-fleet` itself
instead of parsed fix text, are follow-ups.
