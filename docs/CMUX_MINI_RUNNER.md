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
- Either `gh` on the mini, logged in as a `manaflow-ai/cmux` admin (runner
  registration needs admin): `brew install gh && gh auth login`. Or no `gh` on the
  mini at all, with the token minted on the operator's machine (section 2b). The
  second keeps admin credentials off a shared build host.
- A glaeda checkout: `git clone https://github.com/teamleaderleo/glaeda ~/Projects/glaeda`.
- The cmux Xcode pin and `pmset` settings from `glaeda-mini-setup` operator steps, so
  jobs can build once routed. cmux jobs select Xcode by path from repository
  variables (`CMUX_CI_XCODE_APP_PR`, `..._MACOS_15`, `..._MACOS_26`), not through
  `xcode-select`, so that exact path must exist as a real directory (not a symlink).
  Pass it as `--xcode-app /Applications/Xcode_26.6.app` and the plan warns when it
  is missing. `gh variable list --repo manaflow-ai/cmux | grep XCODE` shows the pins.
- Automatic login for the build user (`sysadminctl -autologin status`). The runner is
  a LaunchAgent in that user's GUI session, so it starts again after a reboot only
  when the user logs in automatically.

## 2. One command

Look at the plan first, then apply:

```bash
cd ~/Projects/glaeda
scripts/glaeda-mini-setup --runner            # plan, no side effects
scripts/glaeda-mini-setup --runner --apply    # install, register, start
```

Useful flags: `--name NAME` (default `<short hostname>-glaeda`), `--labels a,b` or `--manifest M --member NAME` (section 2c)
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
   - job-started admits only `push`, `pull_request`, `merge_group`,
     `workflow_dispatch`, `schedule` and `workflow_run` (so `issue_comment`,
     `check_run` and the like, which can act for a fork PR with secrets, are refused).
     It refuses the job (exits 1 before any step runs) for
     `pull_request_target`, for any pull request whose head repository is a fork, is
     missing, or differs from the base repository, for a `workflow_run` from another
     repository, for any repository other than `manaflow-ai/cmux`, and whenever the
     event payload is missing or unreadable. Admitted jobs then run
     `glaeda-disk --pressure --apply --top 0` with a 120 s timeout that never fails
     the job.
   - An admitted job is then held to the fleet host (`/Users/Shared/cmux-build-fleet`),
     so a PR job never lands on a mini that is busy with other work. It is refused fast,
     so the pool picker re-runs it elsewhere, when the host is reserved
     (`reservation.json`, `glaeda-reservation/v1` with integer Unix-second `since` and
     `until`, active while now is before `until`, whoever owns it; an unreadable or
     invalid marker also refuses, an expired one is ignored; parsed by
     `glaeda_reservation.py`, which the installer puts next to the hook), when free disk is below `--min-free-gib` (from the manifest's
     `disk.min_free_gib` with `--manifest`), or when another build holds `host.lock`, the
     `flock` that `with-host-lock` and the build worker take. Otherwise the job takes that
     lock: a small detached holder keeps it until job-completed releases it or the job's
     `Runner.Worker` exits, so fleet builds wait for the PR job and a crash cannot leave
     the lock held. A machine without `host.lock` skips the lock.
   - With `--manifest`, the hook also requires an eligible Glaeda node on its class
     toolchain (`--require-eligible --fleet-class <hardware> --toolchain-xcode <app>`,
     baked in from the member's `hardware` and first verified Xcode). It refuses with
     `refused: node not eligible (...)` unless the node status from the staged
     generation's `cmux_fleet.py status` (the command `glaeda-mini-enroll` uses) says
     `eligible` with `routingCandidateEligible` true and the build role eligible, the
     enrollment references `~/.config/glaeda/cmux-fleet/class-acceptance/<hardware>.json`,
     that receipt validates (its digest recomputed by the generation's
     `validate_class_acceptance`) and records all six toolchain strings, and `rustc`,
     `cargo`, `zig`, `xcodebuild` and `xcrun --show-sdk-version`, run from `$HOME` on the
     runner's own PATH (the job's PATH), print exactly those strings, all within one
     20 s budget. Other fleet jobs on the same mini (the cmux-ci dev-build worker) flip
     the global `rustup default`, so after taking the host lock the hook first sets it
     back to the receipt's toolchain, provided that toolchain is already installed.
     Under the lock no other fleet job runs, so the default holds for the whole job.
     `RUSTUP_TOOLCHAIN` is deliberately not used: it would override cmux's
     `rust-toolchain.toml` pins (DiffSidecar 1.88.0, cmux-tui 1.95.0, iroh-relay-minter
     1.91.0). The DiffSidecar pin itself is checked with `rustup run`. A refusal after
     the lock is taken releases it. The runner's PATH is exactly cmux's workload PATH
     (`/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin`), so a tool under
     the user's home never shadows the accepted one. The default is only changed when
     `rustc --version` differs from the receipt, and `python3` on that PATH must be 3.13
     or newer (cmux's profile runner needs it). To see what a job would get without
     touching anything: `glaeda-cmux-runner-hook check --fleet-class <hardware>
     --toolchain-xcode <app>`, run with the workload PATH, is read-only (no lock, no
     rustup change). The check takes about a second and runs
     on every job, so a mini that drifts or loses eligibility stops taking PR jobs at
     once, and refusal recovery re-runs the job elsewhere.
   - With weighted capacity (`--capacity-units N`, baked in from the manifest class for
     every member, see 2d), the job takes a share of the mini instead of the whole host
     lock. It holds a shared `flock` on `host.lock` (so `with-host-lock`'s `LOCK_EX`,
     the build worker, waits for every PR job, and a held worker lock refuses them),
     plus units and tokens under `/Users/Shared/cmux-build-fleet/capacity`, all as
     `flock`s held by the job's detached holder, so a crash frees them. The cost comes
     from `GITHUB_JOB`: `macos-compile-admission` 2 units plus the `persistent-dd`
     token (one writer of the kept DerivedData at a time), `app-host-unit-tests` and
     `tests-build-and-lag` 1 unit plus the `gui` token (one console session),
     `cli-product-tests`, `swift-package-tests` and the side lanes `cli-pipe-regressions`,
     `remote-daemon-macos-tests` and `claude-wrapper` 1 unit, and any other job counts
     as a compile. When units or a token are taken it refuses at once with
     `refused: capacity: ...`, which the refusal rescue re-runs elsewhere; it never
     waits. Because `flock` gives no preference to the exclusive waiter, it also
     refuses while another process (the build worker in `with-host-lock`) is waiting
     for `host.lock`, so the worker gets the host as soon as the running PR jobs end.
     Admissions on one mini are serialized for a moment (`capacity/admission.lock`),
     so two jobs never split the free units between them.
   - The toolchain check also requires `gh` on the job PATH: cmux's CI scripts call
     `gh api`, and a mini without it fails jobs midway instead of refusing them.
   - job-completed releases the host lock (or the capacity share), runs the same disk
     pressure pass and always exits 0.
4. Writes and loads `~/Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist`
   (runs `run.sh`, restarts on crash, logs to `~/Library/Logs/glaeda-cmux-runner.log`).
5. Confirms through the GitHub API that the runner is listed with every label and
   waits up to 90 s for it to report online.

The receipt is `~/.local/state/glaeda/cmux-runner/receipt.json`. A second `--apply`
reports every step as unchanged. A plan with any blocked step applies nothing, and
a blocked `--apply` exits 1. One install per user: an `--apply` with a different
`--runner-dir`, `--name`, `--repo` or `--org` than the receipt is blocked until the
first one is uninstalled.

## 2b. No gh on the mini: pipe the token over SSH

Mint the one-time token on the operator's machine and pipe it in. It travels only
through the pipe: never an argv, a file, or the output on either side.

```bash
ssh MINI '~/glaeda/scripts/glaeda-cmux-runner --token-stdin'    # plan the gh-free path
gh api -X POST repos/manaflow-ai/cmux/actions/runners/registration-token --jq .token \
  | ssh MINI '~/glaeda/scripts/glaeda-cmux-runner --apply --token-stdin'
```

`glaeda-cmux-runner`, `glaeda-cmux-runner-hook` and `glaeda_reservation.py` must sit
side by side on the mini, plus `glaeda_fleet_labels.py` for `--manifest` (section 2c). Without `gh`, the release metadata comes from the public API through curl,
a name that is already registered is refused by `config.sh` itself, and step 5 is
confirmed from the runner's own log (`Listening for Jobs`) and `.runner` instead of
the API. Check the labels from the operator's machine (section 3).

Uninstall works the same way with a removal token:

```bash
gh api -X POST repos/manaflow-ai/cmux/actions/runners/remove-token --jq .token \
  | ssh MINI '~/glaeda/scripts/glaeda-cmux-runner --uninstall --apply --token-stdin'
```

If that removal fails there is no API fallback on the mini; delete it by id from
the operator's machine with `gh api -X DELETE repos/manaflow-ai/cmux/actions/runners/ID`.

## 2c. Labels come from the fleet manifest

For a fleet member, pass the manifest instead of `--labels` (the two are exclusive):

```bash
glaeda-cmux-runner --apply --token-stdin --manifest ~/glaeda-runner/mini-fleet.json --member cmux10s-mac-mini
```

The label rule lives in `scripts/glaeda_fleet_labels.py`, shared with
`glaeda-mini-fleet`, and must sit next to `glaeda-cmux-runner` on the mini. The
manifest is cmuxterm-hq `build-fleet/mini-fleet.json`; see that repository's
`build-fleet/FLEET-MEMBERSHIP.md`. The member's entry decides everything, and the
runner is named `<member>-glaeda` unless `--name` says otherwise:

| Manifest field | Label |
| --- | --- |
| always | `glaeda-mini` |
| `class`: `xl`, `std` or `light` | `glaeda-class-<class>` |
| `availability`: `dedicated` or `opportunistic` | `glaeda-<availability>` |
| each `defaults.xcode.apps` entry (with host `overrides`) that is really installed | `xcode-<version>` |
| the same, only for `dedicated` members | `glaeda-<class>-xcode-<version>` |

"Really installed" means the path is a real directory, not a symlink, and
`xcodebuild -version` under it prints exactly that `Build version`. The combined
`glaeda-<class>-xcode-<version>` label (for example `glaeda-std-xcode-26.6`) is the
one a pool picker routes a whole run to. Opportunistic members never carry it, so
they never receive a required job.

The installer refuses a member whose roles lack `ci-runner`, and refuses class `dev`
(takes no jobs) and `borrowed` (jobs only inside a VM, not through this tool).

GitHub fixes labels at registration, and changing them through the API needs an
admin token that a mini does not hold. So when the manifest's labels differ from
what the receipt registered, `--apply` with a registration token (`--token-stdin`,
or a logged-in admin `gh`) re-registers the same runner in place. It stops the
LaunchAgent, clears the local registration files (including the runner's
`_migrated` copies), runs
`config.sh --replace` under the same name, and starts the agent again. `_work` and
its hot state stay. It refuses while a job is running. If `config.sh` fails midway,
the runner stays stopped with the old registration still listed. Re-running with
a fresh token re-registers the same name with `--replace`; when `gh` is on the mini,
it refuses first if that name now belongs to a different runner id. A re-run that
passes neither `--labels` nor `--manifest` keeps the labels and name it registered
and never relabels. If `--replace` itself was interrupted after GitHub assigned the
new id, the id check blocks; pass `--replace` to take the name back.

Relabelling keeps the runner's name. Moving an existing `<hostname>-glaeda` runner
to a member whose name `<member>-glaeda` differs is a different install: the
command refuses it as a conflict until `--uninstall --apply` removes the old one,
or pass `--name` with the existing name to relabel it in place.

## 2d. Several runners per mini

A manifest class carries a runner count and the capacity units they share
(defaults: `std` 4 runners and 4 units, `light` 2 and 2, `xl` 8 and 8). Override
them with `defaults.runner.classes.<class>` `{"runners": N, "capacityUnits": U,
"compileSlots": C}`, or per host under `overrides.runner.classes.<class>`.
`compileSlots` (default 1, at most U/2) is how many compiles run at once, one
`persistent-dd` token each (`persistent-dd.token`, `persistent-dd-1.token`, ...).
Raise it only once cmux's compile admission keeps its canonical root and kept
state per runner; with shared paths two compiles would clobber each other. Re-apply
every instance on the mini together: runners that disagree on the count run as many
compiles as the highest one allows. Each runner is one
`--instance K`:

    for k in 0 1 2 3; do
      gh api -X POST repos/manaflow-ai/cmux/actions/runners/registration-token --jq .token |
        ssh MINI "~/glaeda-runner/scripts/glaeda-cmux-runner --apply --token-stdin \
          --manifest ~/glaeda-runner/mini-fleet.json --member MEMBER --instance $k"
    done

Instance 0 keeps the original paths. Instance K gets `~/actions-runner-glaeda-K`,
the LaunchAgent `com.teamleaderleo.glaeda.cmux-runner.K`, its log
`~/Library/Logs/glaeda-cmux-runner-K.log`, its receipt under
`~/.local/state/glaeda/cmux-runner/instance-K/` and the name `<member>-glaeda-K`,
with the same labels, so the pool grows by K runners. An instance past the class's
count is refused. Every instance bakes the same `--capacity-units`, so the mini
never runs more than its units, however many runners pick up jobs. Uninstall one
with `--uninstall --apply --instance K`.

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
Until cmux's `runs-on` expressions fall back to Blacksmith for fork pull
requests, routing `MACOS_RUNNER_PR` sends fork PR jobs here, where the job-started
hook refuses them: they fail instead of running on Blacksmith. Route
`MACOS_RUNNER_PR` only after that fallback lands. One mini runs one job at a time, so routing every variable to
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
token (falling back to `DELETE .../actions/runners/<id>` when `gh` is present, and only for
the id this install registered), and removes
`~/actions-runner-glaeda` (including `_work`) only if the receipt created it and
the directory's marker still matches. If deregistration fails, the directory and
receipt stay so the command can be re-run. Nothing outside those paths is touched.
