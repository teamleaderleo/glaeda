# CMUX-owned fleet enrollment

Tracking issue: #1056.

This path turns a CMUX-controlled Mac or Linux host into a reviewed fleet node while the existing owners keep their responsibilities. GitHub Actions, CMUX's controller, direct SSH, MDM/configuration management, hosted runners, and future schedulers continue to manage the parts they already own. Glaeda owns the bounded enrollment record, role acceptance binding, lifecycle state, and a machine-status projection that reports role/candidate eligibility.

## Supported v1 bootstrap classes

The repository-owned bootstrap commands are read-only. They inspect an exact CMUX checkout and exact Glaeda executable, produce a bounded receipt, and leave package installation, MDM, SSH, networking, and machine account policy with their existing owners.

- macOS: Apple Silicon for the v1 `cmux_macos_native_build` role; macOS major 15 or 26; CMUX `.xcode-version` must be `26.0`; the selected Xcode and macOS SDK must be generation 26; Git present; at least 25 GiB plus 25 GiB per concurrent build slot free by default (`--build-slots`, default 1, so 50 GiB; see `MACOS_SLOT_FREE_GIB` in `scripts/cmux_fleet_bootstrap.py` for what a slot holds); AC system sleep disabled for unattended work. `cmux-mac-build-large` requires at least 8 logical CPUs and 16 GiB total memory.
- Linux: x86_64 or arm64; Ubuntu 24.04 or Debian 12; kernel major 6 or newer; Git, Python, systemd, bubblewrap, cgroup v2, pressure signals; at least 40 GiB free and 8 GiB available memory by default. `cmux-linux-ci-medium` and `cmux-linux-agent-medium` require at least 4 logical CPUs and 8 GiB total memory. `cmux_linux_ci` also checks `curl`, `tar`, `gzip`, and `ldd`.

A different machine class starts as a bootstrap refusal until the reviewed policy changes.

## Roles

The role vocabulary is stable enough to name future work:

| Role | v1 enrollment | Exact acceptance owner |
| --- | --- | --- |
| `cmux_macos_native_build` | supported | `cmux.macos.dev-check@1` via `scripts/ci/cmux_workload_profile.py` |
| `cmux_linux_ci` | supported | `cmux.ci.guard@1` via `scripts/ci/cmux_workload_profile.py` |
| `cmux_macos_test` | reserved | acceptance workload must land first |
| `cmux_linux_agent` | reserved | acceptance workload must land first |
| `artifact_cache` | reserved | acceptance workload must land first |
| `background_replay` | reserved | acceptance workload must land first |
| `benchmark` | reserved | acceptance workload must land first |
| `diagnostic` | reserved | acceptance workload must land first |

The v1 enrollment validator and bootstrap refuse reserved roles. A role becomes enrolable only when its repository-owned CMUX profile is present at the reviewed generation in the exact CMUX checkout. For supported roles, eligibility then requires node state `eligible` plus a current accepted receipt matching node ID, enrollment generation, Glaeda generation, enrolled toolchain generation, and the role's exact profile ID/generation.

The versioned schema carries the full role vocabulary in `$defs.knownRole` while `allowedExecutionRoles` accepts only the v1 supported subset.

## Enrollment identity and privacy

`examples/cmux-fleet/enrollment-v1.schema.json` is the public v1 schema. The record contains only:

- opaque `cmux-*` node ID;
- architecture;
- OS family and version class;
- hardware capability class;
- accepted toolchain generations;
- current repository-owned CMUX profile ID/generation for every allowed role;
- allowed roles;
- operator/fleet ownership scope;
- enrollment generation;
- installed Glaeda generation;
- lifecycle state and reviewed quarantine reason.

The schema has no hostname, serial number, username, private address, SSH identity, MDM identifier, or raw hardware inventory fields. Unknown top-level fields are rejected.

Role eligibility binds repository-owned CMUX semantic profiles rather than a duplicate acceptance recipe. The low-level `finalize-acceptance` command can validate externally supplied semantic evidence for diagnostics, but v2 marks that evidence as externally supplied and it cannot mint an accepted receipt. Candidate-eligible receipts come from `accept-local`, which owns the local CMUX process invocation. Finalization emits `glaeda-cmux-fleet-acceptance/v2`, which binds the exact CMUX result digest, CMUX environment class/toolchain identity, a fresh post-run bootstrap digest, and the exact Glaeda fleet-contract generation in addition to enrollment/Glaeda/toolchain/process-settlement identity. A change to `cmux_fleet.py` or `cmux_fleet_bootstrap.py` therefore makes prior acceptance stale until the role is accepted again. The CMUX semantic result also carries a bounded repository-owned environment class; Glaeda does not interpret that class, but it requires the class to participate in CMUX's recomputed semantic comparison identity before accepting the result. Bootstrap reads `scripts/ci/cmux-workload-profiles.json`, requires `cmux_linux_ci -> cmux.ci.guard@1` or `cmux_macos_native_build -> cmux.macos.dev-check@1`, and checks that the selected profile admits the observed OS/architecture. `accept-local` consumes the canonical `cmux-workload-result/v1` emitted by that profile and records its exact digest/state together with CMUX environment/toolchain identity, a Glaeda-local attempt digest, the fresh post-run bootstrap digest, and Glaeda-owned enrollment/toolchain/process-settlement identity. Glaeda never reconstructs CMUX argv, artifacts, or pass/fail semantics.

Acceptance v2 intentionally supersedes the earlier v1 receipt format. A v1 receipt cannot supply candidate eligibility after this change; rerun bootstrap/enrollment as required for the current Glaeda generation, then run `accept-local` to mint a v2 receipt. No physical v1 acceptance receipt was claimed by the original rollout.

Canonical enrollment and acceptance documents used for lifecycle or routing-candidate decisions are read through a no-follow private-file gate: current-user owned, regular, single-link, mode `0600`, bounded size, and unchanged inode/metadata across the read. A loose, symlinked, or swapped file cannot supply eligibility. `transition` remains a side-effect-free planner. `transition-apply` holds one private mutation lock, re-reads the current enrollment under that lock, writes a mode-0600 same-directory stage, fsyncs the staged bytes, atomically replaces `enrollment.json`, fsyncs the fleet directory, and revalidates the published bytes before returning.

## Lifecycle

States are:

`discovered -> enrolling -> eligible -> draining/quarantined -> retired`

Supported recovery paths also include `draining -> eligible` and `quarantined -> enrolling`. Every transition into `eligible` requires at least one current accepted role receipt. Leaving quarantine advances the enrollment generation, so every pre-quarantine acceptance receipt becomes stale immediately.

`scripts/cmux-fleet status` reports role/candidate eligibility only. Its `routingCandidateEligible` field means the enrollment and current acceptance receipt satisfy this contract; `automaticDispatchAuthorized` stays `false`. #546 or another separately approved routing-promotion gate owns queue selection and dispatch, including queue/start prediction, hot locality, pressure, allowance scarcity, workload-class evidence, and operator policy. The status projection includes only bounded architecture, OS class, hardware class, accepted toolchain generations, role profiles, Glaeda generation, fleet scope, lifecycle state, and per-role eligibility. A node in `draining`, `quarantined`, or `retired` produces zero eligible roles even when an older acceptance receipt exists. An `eligible` node still produces zero eligible roles when its acceptance evidence is absent, rejected, or stale. Live pressure/heat remains a fresh local admission veto: #970 may publish advisory bounded snapshots, while #546/local execution admission re-observes the machine before dispatch.

For the quiet operator/agent view, use the separate read-only projection:

```bash
python3 scripts/cmux_fleet_operator_summary.py "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

This emits `glaeda-cmux-fleet-operator-summary/v1`. A healthy accepted node reports
`attentionRequired: false`, `action: "none"`, and only the eligible role names.
Missing or stale acceptance reports one concrete `run_role_acceptance` action;
draining reports `observe_drain`; quarantine exposes only its reviewed bounded
reason and `inspect_quarantine`; retired nodes are quiet/inactive. The projection
carries no mutation or dispatch authority and intentionally lives outside the
Glaeda fleet-contract generation, so changing operator presentation does not
invalidate otherwise-current physical acceptance receipts.

Reviewed quarantine reasons are:

- `toolchain_mismatch`
- `disk_pressure`
- `failed_acceptance`
- `dirty_canonical_checkout`
- `service_mismatch`
- `hardware_failure`
- `stale_glaeda_generation`
- `unexplained_process_settlement`

## Prepare a fresh Mac mini

Before enrollment, one command takes a fresh build host to ready. It plans by default and changes nothing until `--apply`:

```bash
scripts/glaeda-mini-setup                              # preflight + plan
scripts/glaeda-mini-setup --apply                      # install, idempotent
scripts/glaeda-mini-setup --cmux-root ~/Projects/cmux  # also run the read-only bootstrap below
scripts/glaeda-mini-setup --uninstall [--apply]        # remove what --apply installed
scripts/glaeda-mini-setup --runner [--apply]           # GitHub Actions runner, see CMUX_MINI_RUNNER.md
```

Preflight reports macOS, hardware against the M4 Pro 14-core 48 GB target, Xcode apps and the cmux pin, Command Line Tools (not needed when a licensed Xcode is selected), free disk, Tailscale, Homebrew, the workload PATH and AC power. `--apply` installs `glaeda-disk`, `glaeda-worktree-reclaim` and `glaeda-worktree-reclaim-all` into `~/.local/bin`, their three LaunchAgents templated for the current user, the build-host directories (including a `--cache-root` under `~/.cache/glaeda/cmux-native-cache`) and three git defaults that are set only when unset. It also finds the Python 3.13+ that `glaeda-mini-enroll` will use (same search: Homebrew, then `~/.local/bin`, then `PATH`). If there is none, `--apply` runs `brew install python@3.13` when this user owns Homebrew; otherwise the host is not ready and the fix is printed, because on shared minis Homebrew often belongs to another account. Everything else is under `$HOME`; nothing uses sudo. Steps that need sudo or a human (pmset, Xcode licence and selection, Tailscale login, runner registration) are printed as operator steps. Runner registration and the compilation-cache node daemon (RFC #1134 M2) are reserved slots that this command never fills. The receipt is `~/.local/state/glaeda/mini-setup/receipt.json` (mode `0600`). With a HOME other than the account's own, launchctl is never called, which is how the tests and rehearsals run.

This command does not replace the bootstrap below: it imports its read-only helpers and runs it unchanged, so fleet-contract generations and acceptance receipts are unaffected.

## Keep a fleet in line with its manifest

`scripts/glaeda-mini-fleet` holds a whole fleet of build minis to one declarative manifest: each host's hardware class, macOS floor, the Xcode apps and selection it must carry, the SSH keys its login user must, may, and must not accept, the launchd jobs that must be running, and a free-disk floor.

```bash
scripts/glaeda-mini-fleet check                       # observe every host over SSH, diff, exit 1 on drift
scripts/glaeda-mini-fleet check --host <name> --json  # one host, machine-readable
scripts/glaeda-mini-fleet observe --out observed.json # raw observation; check/plan accept --observed
scripts/glaeda-mini-fleet plan                        # the fixes, as operator steps
scripts/glaeda-mini-fleet apply --host <name> [--yes] # the additive fixes; dry run without --yes
```

Observation runs `scripts/cmux_mini_probe.sh` over SSH as the login user, with no sudo and no Python, because a mini without an accepted Xcode licence has only a stub `/usr/bin/python3`. It reports key fingerprints and comments, never key material, tokens, or log contents. `apply` only adds: it appends a required key whose `public_key` is in the manifest and whose fingerprint matches, after copying `authorized_keys` aside, and it APFS-clones an Xcode with `cp -c` when the source's version and build match. A clone is a real directory that shares blocks with its source, so it costs no disk and avoids the symlinked-path compilation-cache replay failure. `apply` never removes keys, never uses sudo, skips hosts whose overrides say `"apply": false`, refuses hosts in `never_touch`, and ends with a fresh observation. Licence acceptance, `xcode-select`, and macOS updates need sudo and stay operator steps in `plan`.

A real manifest names hosts, people, and keys, so it does not belong in this repository. The default path is `~/.config/glaeda/mini-fleet.json` (or `--manifest`, or `$GLAEDA_MINI_FLEET_MANIFEST`); `examples/mini-fleet/manifest.example.json` shows the shape. Keys that nobody has ruled on carry `"status": "review"` and sit in `allow`, so `check` stays quiet about them but flags any key the manifest does not name. Xcode apps listed under `pending` are reported without counting as drift. A host's `node_id` assigns its opaque fleet node id (`cmux-mac-NNN`, unique across the manifest), so enrollment reruns always ask for the same one. `check` reads the enrolled id from `~/.config/glaeda/cmux-fleet/enrollment.json`: a host not enrolled yet is pending, with the `--node-id` to use, and a host enrolled under a different id is drift.

Each host may also carry `roles` (`dev-builds`, `ci-runner`, `nightly`, `ios-simulators`, `cache-host`) and the `sudo` mode it is expected to have (`nopasswd` or `password`, observed with `sudo -n -l`, which runs nothing). `controller_token: "present"` requires the fleet worker's controller token file to exist; the probe tests existence only and never reads it.

## Enroll a Mac in one command

After `glaeda-mini-setup --apply` and cmux `./scripts/setup.sh`, one command runs every step of [Onboard a Mac](#onboard-a-mac) below. Give it the downloaded [candidate bundle](FLEET_DISTRIBUTION.md) and the checksum and source from the trusted run's receipt:

```bash
scripts/glaeda-mini-enroll --cmux-root ~/Projects/cmux --node-id cmux-mac-001 \
  --candidate ./glaeda-candidate/ARCHIVE.tar.gz --sha256 SHA256 --source COMMIT          # plan
# the same command with --apply does it
```

It stages the archive with this checkout's `fleet_bundle.py` into `~/Projects/glaeda-generations/<first 12 of source>`, then runs bootstrap, enroll, `accept-local`, the transition to `eligible`, and `status`. Every fleet step uses that generation's own binary and tools, so the node runs exactly the reviewed candidate and builds no Rust. Without `--candidate` it builds glaeda from this checkout instead, which is for development hosts only.

It uses the same paths as the manual steps: the enrollment and acceptance receipt under `~/.config/glaeda/cmux-fleet`, and `glaeda-mini-setup`'s cache root. It finds a Python 3.13+ interpreter itself, because `accept-local` needs `os.waitid`. Each step is skipped when its result already holds, so a re-run re-inspects the staged generation instead of restaging it, resumes at `accept-local` after a rejected acceptance, returns a draining node to `eligible`, and only prints status on an eligible node. An interrupted stage is left in place and reported rather than reused. `--reaccept` forces a fresh acceptance. It refuses a quarantined or retired node and a `--node-id` that differs from the existing enrollment. It never uses sudo and never registers a GitHub runner; in a cmux checkout, `scripts/persistent-compile up` downloads the pinned candidate, runs this command and then registers the runner.

## Onboard a Mac

Use an exact reviewed Glaeda checkout and the CMUX checkout that will run acceptance. A [verified native candidate bundle](FLEET_DISTRIBUTION.md) can supply `GLAEDA_BIN` and the matching fleet scripts without building Rust on the node; skip the build/copy step below when using that path.

```bash
set -euo pipefail
./scripts/bootstrap
cargo build --locked --release --bin glaeda

GLAEDA_INSTALL_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/glaeda/cmux-fleet"
GLAEDA_BIN="$GLAEDA_INSTALL_ROOT/glaeda"
install -d -m 700 "$GLAEDA_INSTALL_ROOT"
if test -f "$GLAEDA_BIN" && ! test -e "$GLAEDA_INSTALL_ROOT/glaeda.rollback"; then
  cp -p "$GLAEDA_BIN" "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
fi
install -m 755 target/release/glaeda "$GLAEDA_INSTALL_ROOT/.glaeda.next"
mv "$GLAEDA_INSTALL_ROOT/.glaeda.next" "$GLAEDA_BIN"

CMUX_ROOT=/absolute/path/to/cmux
CMUX_CACHE_ROOT=/absolute/path/to/cmux-native-cache
(
  cd "$CMUX_ROOT"
  ./scripts/setup.sh
)
# Lets bootstrap describe the toolchain. It does not make it visible to the
# CMUX build, which searches a fixed list of system directories — see the
# workloadToolPath note below before installing rustup only under $HOME.
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

FLEET_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet"
umask 077
install -d -m 700 "$FLEET_ROOT" "$FLEET_ROOT/acceptance"
BOOTSTRAP="$(mktemp "$FLEET_ROOT/.bootstrap.XXXXXX")"
chmod 600 "$BOOTSTRAP"
ENROLLMENT="$FLEET_ROOT/enrollment.json"
ACCEPTANCE="$FLEET_ROOT/acceptance/cmux_macos_native_build.json"
NODE_ID=cmux-mac-001

bash scripts/cmux-fleet-bootstrap-macos \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --cache-root "$CMUX_CACHE_ROOT" \
  --hardware-class cmux-mac-build-large \
  --role cmux_macos_native_build \
  > "$BOOTSTRAP"

ENROLLMENT_NEXT="$(mktemp "$FLEET_ROOT/.enrollment.XXXXXX")"
python3 scripts/cmux_fleet.py enroll "$BOOTSTRAP" \
  --node-id "$NODE_ID" \
  --scope cmux-founders \
  --generation 1 \
  > "$ENROLLMENT_NEXT"
chmod 600 "$ENROLLMENT_NEXT"
mv "$ENROLLMENT_NEXT" "$ENROLLMENT"

ACCEPTANCE_NEXT="$(mktemp "$FLEET_ROOT/acceptance/.cmux_macos_native_build.XXXXXX")"
python3 scripts/cmux_fleet.py accept-local "$ENROLLMENT" \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --cache-root "$CMUX_CACHE_ROOT" \
  --role cmux_macos_native_build \
  > "$ACCEPTANCE_NEXT"
chmod 600 "$ACCEPTANCE_NEXT"
mv "$ACCEPTANCE_NEXT" "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The macOS preparation reuses CMUX's reviewed `scripts/setup.sh` for prerequisites. Bootstrap re-observes those prerequisites read-only and verifies the exact checkout exposes `cmux.macos.dev-check@1` for the observed Apple-Silicon node. `accept-local` launches CMUX's checked-in profile runner on this node inside a private attempt directory, captures the canonical semantic result, reruns the read-only bootstrap on this same node, and emits `glaeda-cmux-fleet-acceptance/v2`. Both Python front doors execute in isolated interpreter mode (`-I`) with a closed allowlist containing only reviewed toolchain/home path inputs plus a private attempt-local `TMPDIR`; Python import controls, Git redirection variables, SSH agents, credentials, and unrelated operator environment never flow into either child. CMUX still owns the developer-build commands, validator, artifacts, timeout, and pass/fail semantics. Glaeda owns the local-attempt binding, fresh capability check, and durable receipt.

Bootstrap still probes the toolchain through the operator's shell, so a node with a misplaced tool produces a complete receipt rather than a bare error. It separately reports whether the CMUX workload will be able to find those tools: the runner rebuilds PATH as `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin` plus a per-attempt Cargo home that starts empty, so a build tool reachable only from `$HOME` is one the build cannot spend. `workloadToolPath` fails and `observed.toolsMissingFromWorkloadPath` names them, which blocks enrollment in seconds instead of passing bootstrap and failing the build minutes later. **`rustup`, `cargo` and `rustc` therefore have to be reachable from those six directories, not only from `$CARGO_HOME/bin`** — the `export PATH` above lets bootstrap describe the toolchain, it does not make it visible to the build.

An attempt that does not accept keeps its runner log and semantic result beside the enrollment, renamed to `rejected-attempt.*`, and drops the child's scratch tree. Both steps are best-effort and say so: if the name is already taken the attempt stays under its hidden `.acceptance-run.*` name, and if the scratch tree survives a second line names it. Either way the retained path is printed on stderr, and the receipt is never sacrificed to report it.

Retention is bounded to the three most recent directories Glaeda itself created. Glaeda recognises those by name: exactly eight characters from `a-z`, `0-9` and `_` after the prefix. Anything else you leave under that prefix — an archive, a symlink onto another volume, a directory renamed `rejected-attempt.x9k2mq4p.keep` — is neither counted against the budget nor removed. Avoid a bare eight-character suffix such as a date (`rejected-attempt.20260923`) for evidence you mean to keep: it is indistinguishable from one Glaeda made. Removing a retained attempt by hand is safe at any time. The retained `cmux-runner.log` is the build's raw merged output at mode `0600` inside the `0700` fleet root: operator-private evidence with an indefinite lifetime, not something to attach to an issue unread.

## Onboard Linux

```bash
set -euo pipefail
./scripts/bootstrap
cargo build --locked --release --bin glaeda

GLAEDA_INSTALL_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/glaeda/cmux-fleet"
GLAEDA_BIN="$GLAEDA_INSTALL_ROOT/glaeda"
install -d -m 700 "$GLAEDA_INSTALL_ROOT"
if test -f "$GLAEDA_BIN" && ! test -e "$GLAEDA_INSTALL_ROOT/glaeda.rollback"; then
  cp -p "$GLAEDA_BIN" "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
fi
install -m 755 target/release/glaeda "$GLAEDA_INSTALL_ROOT/.glaeda.next"
mv "$GLAEDA_INSTALL_ROOT/.glaeda.next" "$GLAEDA_BIN"

CMUX_ROOT=/absolute/path/to/cmux
FLEET_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet"
umask 077
install -d -m 700 "$FLEET_ROOT" "$FLEET_ROOT/acceptance"
BOOTSTRAP="$(mktemp "$FLEET_ROOT/.bootstrap.XXXXXX")"
chmod 600 "$BOOTSTRAP"
ENROLLMENT="$FLEET_ROOT/enrollment.json"
ACCEPTANCE="$FLEET_ROOT/acceptance/cmux_linux_ci.json"
NODE_ID=cmux-linux-001

bash scripts/cmux-fleet-bootstrap-linux \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --hardware-class cmux-linux-ci-medium \
  --role cmux_linux_ci \
  > "$BOOTSTRAP"

ENROLLMENT_NEXT="$(mktemp "$FLEET_ROOT/.enrollment.XXXXXX")"
python3 scripts/cmux_fleet.py enroll "$BOOTSTRAP" \
  --node-id "$NODE_ID" \
  --scope cmux-founders \
  --generation 1 \
  > "$ENROLLMENT_NEXT"
chmod 600 "$ENROLLMENT_NEXT"
mv "$ENROLLMENT_NEXT" "$ENROLLMENT"

ACCEPTANCE_NEXT="$(mktemp "$FLEET_ROOT/acceptance/.cmux_linux_ci.XXXXXX")"
python3 scripts/cmux_fleet.py accept-local "$ENROLLMENT" \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --role cmux_linux_ci \
  > "$ACCEPTANCE_NEXT"
chmod 600 "$ACCEPTANCE_NEXT"
mv "$ACCEPTANCE_NEXT" "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The Linux `accept-local` path runs CMUX's canonical `cmux.ci.guard@1` profile against the exact local commit/tree in cold state, then re-observes this same host. Workload tool visibility and rejected-attempt retention work exactly as described for macOS above; on Linux the tools checked are `git` and `python3`. A v2 receipt becomes accepted only when CMUX reports `passed`, process settlement is complete, and the post-run capability, Glaeda generation, role profile, and selected toolchain generation still match enrollment.

After either onboarding path, remove only the transient evidence files:

```bash
rm -f "$BOOTSTRAP"
```

The canonical enrollment and finalized role receipts stay under `$FLEET_ROOT` across reboot. They contain no credentials or project secrets.

## Drain, quarantine, recover, and retire

Drain before planned operator work:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to draining
bash scripts/cmux-fleet status "$ENROLLMENT" --acceptance "$ACCEPTANCE"
```

Return a drained node to candidate eligibility only with its still-current acceptance receipt:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"
bash scripts/cmux-fleet status "$ENROLLMENT" --acceptance "$ACCEPTANCE"
```

Quarantine on a concrete reviewed reason:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" \
  --to quarantined --reason toolchain_mismatch
```

After a toolchain, OS, hardware class, or Glaeda update, quarantine the enrollment
and rerun the platform bootstrap above into a private mode-0600 `$BOOTSTRAP`
report. Renew from that report instead of hand-editing the enrollment or choosing
a generation number:

```bash
python3 scripts/cmux_fleet.py renew-enrollment "$ENROLLMENT" "$BOOTSTRAP"
# Inspect replacement capabilities and copy planSha256 from the preview.
python3 scripts/cmux_fleet.py renew-enrollment-apply "$ENROLLMENT" "$BOOTSTRAP" \
  --expected-plan-sha256 sha256:REPLACE_WITH_PREVIEW_DIGEST
```

The preview has no side effects. Apply re-reads both private documents under the
existing enrollment mutation lock and refuses if either differs from the preview.
It preserves node ID and fleet scope, refreshes capabilities from bootstrap,
advances the enrollment generation once, and atomically publishes `enrolling`.
Old receipts cannot promote this generation; rerun `accept-local` and then
`transition-apply --to eligible` using the new receipt. A repeated apply refuses
without advancing the generation again. Active, draining, and retired enrollments
cannot use renewal.

Bootstrap is observation-only: renewal does not certify report freshness or grant
execution ownership. Fresh local acceptance still re-observes the machine. The
caller must first stop new work through its actual scheduler and settle existing
work before changing software; quarantine alone does not drain that scheduler.
If the candidate fails, quarantine its enrollment, restore the reviewed previous software,
rerun bootstrap and renew again before acceptance. The old receipt is never a
rollback shortcut.

Rollback an onboarding before candidate promotion, or retire an active node, by preserving the record in the terminal `retired` state:

```bash
python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to retired
bash scripts/cmux-fleet status "$ENROLLMENT"
```

This leaves an auditable local tombstone and zero candidate-eligible roles.

## Operator-owned prerequisite changes

The bootstrap commands deliberately avoid installing packages, changing SSH/Tailscale, enrolling MDM, editing GitHub runner registration, or changing service ownership. Use the machine-management system that already owns those changes, then rerun bootstrap.

For a Mac used unattended without an existing MDM power profile, this reference change is reversible:

```bash
OLD_AC_SLEEP="$(pmset -g custom | awk '/AC Power:/{on=1;next}/Power:/{on=0} on && $1=="sleep"{print $2; exit}')"
test -n "$OLD_AC_SLEEP"
sudo pmset -c sleep 0
# rollback
sudo pmset -c sleep "$OLD_AC_SLEEP"
```

For Glaeda distribution, the bootstrap consumes an exact executable path and records its SHA-256 generation. The examples above install the reviewed repository build into a stable per-user CMUX-fleet location with an atomic rename. The first update attempt preserves the previously installed binary as `glaeda.rollback`; later retries leave that copy untouched until the candidate enrollment generation is accepted. A release package, MDM payload, or configuration manager may supply the same exact file instead.

Rollback a just-installed repository build before re-enrollment with:

```bash
test -f "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
mv "$GLAEDA_INSTALL_ROOT/glaeda.rollback" "$GLAEDA_BIN"
```

After any install, update, or rollback, rerun bootstrap and advance the enrollment generation before candidate eligibility resumes. Once the new generation is accepted, remove the one-step rollback copy:

```bash
rm -f "$GLAEDA_INSTALL_ROOT/glaeda.rollback"
```

## CI and synthetic evidence

`accept-local` compares the returned CMUX source identity with the exact
repository/commit/tree passed to the workload, then re-observes HEAD commit/tree
after post-run bootstrap before issuing acceptance. A self-consistent result for
another source or checkout movement refuses acceptance. These boundary checks
do not freeze the checkout; callers must still coordinate exclusive source use.

Hosted CI needs no physical CMUX machine. Glaeda CI runs the enrollment/bootstrap contract suites and synthetic CMUX semantic-result fixtures. CMUX CI owns the workload-profile contract and executes `cmux.ci.guard@1` through its canonical runner. Physical Mac/Linux acceptance receipts are attached only after the corresponding CMUX-controlled hosts exist and have been explicitly approved for the profile.

Related Glaeda work: #743, #546, #365, #492, #970, #1048, #1008, #1010, #1071.
Related CMUX work: manaflow-ai/cmux#13091, #13095, #13198, #13325, #13411.
