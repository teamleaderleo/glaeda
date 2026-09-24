# cmux Mac mini runner runsheet

Turns one cmux Mac mini into a persistent GitHub Actions self-hosted runner for
`manaflow-ai/cmux`, labelled `self-hosted,macOS,ARM64,glaeda-mini`. The tool is
`scripts/glaeda-cmux-runner`, also reachable as `scripts/glaeda-mini-setup --runner`.
Plan is the default and changes nothing; `--apply` acts; `--uninstall --apply` undoes it.

Registering a runner does not move any job. Jobs move only when a repository
variable points at the `glaeda-mini` label (step 4), and deleting that variable moves
them back to Blacksmith.

## 1. Prerequisites (operator, on the mini)

- Apple silicon Mac mini, logged in as the build user, with the Command Line Tools
  (`xcode-select --install`) and Homebrew.
- `scripts/glaeda-mini-setup --apply` already run, so `glaeda-disk` is on
  `~/.local/bin` (optional: without it the hooks skip disk pressure).
- `gh` installed and logged in as a `manaflow-ai/cmux` admin (runner registration
  needs admin): `brew install gh && gh auth login`.
- A glaeda checkout: `git clone https://github.com/teamleaderleo/glaeda ~/Projects/glaeda`.
- The cmux Xcode pin and `pmset` settings from `glaeda-mini-setup` operator steps, so
  jobs can build once routed.

## 2. One command

Look at the plan first, then apply:

```bash
cd ~/Projects/glaeda
scripts/glaeda-mini-setup --runner            # plan, no side effects
scripts/glaeda-mini-setup --runner --apply    # install, register, start
```

Useful flags: `--name NAME` (default `<short hostname>-glaeda`), `--labels a,b`
(extra labels), `--org ORG [--group GROUP]` instead of the default
`--repo manaflow-ai/cmux`, `--runner-dir DIR` (default `~/actions-runner-glaeda`),
`--runner-version V --runner-sha256 HEX` to pin, `--replace` to take over an existing
registration with the same name, `--output json` for the receipt.

What `--apply` does:

1. Downloads the latest official `actions/runner` `osx-arm64` tarball, verifies its
   SHA-256 against the release notes and the GitHub asset digest (both must agree),
   and unpacks it into `~/actions-runner-glaeda`.
2. Gets a one-time registration token with `gh api -X POST .../registration-token`
   and runs `config.sh --unattended` (persistent, not ephemeral, so `_work` keeps hot
   state). The token is read from gh's stdout into memory and passed to `config.sh`
   only as `ACTIONS_RUNNER_INPUT_TOKEN` in that child's environment, which the runner
   reads and clears. It is never in an argv, a file, a log, or the output.
3. Writes the job hooks into `~/actions-runner-glaeda/glaeda-hooks/`:
   - job-started refuses the job (exits 1 before any step runs) for
     `pull_request_target`, for any pull request whose head repository is a fork, is
     missing, or differs from the base repository, for a `workflow_run` from another
     repository, for any repository other than `manaflow-ai/cmux`, and whenever the
     event payload is missing or unreadable. Admitted jobs then run
     `glaeda-disk --pressure --apply --top 0` with a 120 s timeout that never fails
     the job.
   - job-completed runs the same disk pressure pass and always exits 0.
4. Writes and loads `~/Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist`
   (runs `run.sh`, restarts on crash, logs to `~/Library/Logs/glaeda-cmux-runner.log`).
5. Confirms through the GitHub API that the runner is listed with every label and
   waits up to 90 s for it to report online.

The receipt is `~/.local/state/glaeda/cmux-runner/receipt.json`. A second `--apply`
reports every step as unchanged. A plan with any blocked step applies nothing.

## 3. Verify

```bash
scripts/glaeda-mini-setup --runner                       # every step "unchanged", verify ok
launchctl print gui/$(id -u)/com.teamleaderleo.glaeda.cmux-runner | head -20
gh api repos/manaflow-ai/cmux/actions/runners --jq '.runners[] | select(.name | endswith("-glaeda")) | {name, status, busy, labels: [.labels[].name]}'
tail -f ~/Library/Logs/glaeda-cmux-runner.log
```

Expect `status: "online"` and labels `self-hosted, macOS, ARM64, glaeda-mini`.

## 4. Route

The plan prints these; it never runs them. Start with one variable, watch a few
jobs, then add the rest:

```bash
gh variable set MACOS_RUNNER_15 --body glaeda-mini --repo manaflow-ai/cmux
gh variable set MACOS_RUNNER_26 --body glaeda-mini --repo manaflow-ai/cmux
gh variable set MACOS_RUNNER_PR --body glaeda-mini --repo manaflow-ai/cmux
gh variable set MACOS_RUNNER_DUAL_XCODE --body glaeda-mini --repo manaflow-ai/cmux
```

cmux workflows read these as `runs-on: ${{ vars.MACOS_RUNNER_15 || 'blacksmith-6vcpu-macos-15' }}`.
Fork pull requests are meant to stay on Blacksmith (a separate cmux change makes
these expressions fall back for forks); the job-started hook refuses them anyway if
one ever lands here. One mini runs one job at a time, so routing every variable to
it queues work behind it.

## 5. Rollback

Stop routing (jobs fall back to Blacksmith on their next run):

```bash
gh variable delete MACOS_RUNNER_15 --repo manaflow-ai/cmux
gh variable delete MACOS_RUNNER_26 --repo manaflow-ai/cmux
gh variable delete MACOS_RUNNER_PR --repo manaflow-ai/cmux
gh variable delete MACOS_RUNNER_DUAL_XCODE --repo manaflow-ai/cmux
```

Remove the runner:

```bash
scripts/glaeda-mini-setup --runner --uninstall            # plan
scripts/glaeda-mini-setup --runner --uninstall --apply    # deregister and remove
```

Uninstall acts on the directory, name and scope in the receipt; flags cannot redirect it. It unloads and removes the LaunchAgent
only if it still has the bytes this tool wrote, deregisters with a one-time removal
token (falling back to `DELETE .../actions/runners/<id>`), and removes
`~/actions-runner-glaeda` (including `_work`) only if the receipt created it and
the directory's marker still matches. If deregistration fails, the directory and
receipt stay so the command can be re-run. Nothing outside those paths is touched.
