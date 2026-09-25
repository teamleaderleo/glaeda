---
name: github-ci-wait
description: "Wait for GitHub CI, watch a workflow run, poll PR checks, or answer 'is CI green' without burning the shared GitHub quota. Use glaeda-gh (a local daemon that is the only poller) instead of gh run watch, gh run view loops, or per-PR REST scans. Use whenever you would wait for CI, watch the run, poll checks, check whether CI is green or a PR merged, or sweep many PRs."
---

# Wait on GitHub CI with glaeda-gh

Every session on this machine shares one GitHub account, so one REST quota of 5000 requests an
hour. Sessions polling on their own used it all up. `glaeda-gh` is a local daemon that is the
only process polling: one batched GraphQL query for every watched PR, and ETag requests for runs
(an unchanged run costs nothing). Its commands read the daemon's cache and make no API calls.

## Wait and check

```bash
glaeda-gh wait pr OWNER/REPO#N                   # until green; exits 1 at the first failed check
glaeda-gh wait pr OWNER/REPO#N --until done      # until every check finishes (0 green, 1 red)
glaeda-gh wait pr OWNER/REPO#N --until merged
glaeda-gh wait run OWNER/REPO/RUN_ID [--jobs]    # 0 success, 1 failure
glaeda-gh status pr OWNER/REPO#N                 # one look: state, mergeable, checks, review
glaeda-gh status run OWNER/REPO/RUN_ID --jobs
glaeda-gh budget                                 # REST and GraphQL left, and reset times
```

- `wait` exits 0 on success, 1 on failure, 2 on timeout (`--timeout S`, default 3600), 3 when the
  daemon is down. It ends with a summary that names failing checks and links them.
- URLs work too: `glaeda-gh wait pr https://github.com/o/r/pull/12`.
- Run a long `wait` in the background (Bash `run_in_background`) and carry on; you are told when it exits.
- Add `--json` for machine-readable output.

## Do not

- `gh run watch`, `gh pr checks --watch`, or `gh run view` / `gh pr view` in a loop.
- Per-PR REST scans: listing PRs, then `gh api repos/.../pulls/<n>/files` (or `/reviews`, `/commits`) for each.
- Fall back to polling when `glaeda-gh` says the daemon is down. Start it instead:
  `launchctl kickstart gui/$(id -u)/com.teamleaderleo.glaeda.gh-watch` (macOS) or
  `systemctl --user start glaeda-gh` (Linux).

## Fine to do directly

- Single reads, merges, comments, reviews, labels, and dispatches with `gh`.
- Logs: `gh run view RUN_ID --log-failed` once a run has failed.
- One-off reads across many PRs: a single batched GraphQL query with aliases
  (`p0: repository(owner:, name:) { pullRequest(number:) { ... } } p1: ...`) through `gh api graphql`.
  GraphQL has its own 5000-point quota, and one aliased query costs about 1 point.
- Before a sweep that needs many REST calls, run `glaeda-gh budget` first and keep well above 500 left.
