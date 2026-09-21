# CMUX-owned fleet enrollment

Tracking issue: #1056.

This path turns a CMUX-controlled Mac or Linux host into a reviewed fleet node while the existing owners keep their responsibilities. GitHub Actions, CMUX's controller, direct SSH, MDM/configuration management, hosted runners, and future schedulers continue to manage the parts they already own. Glaeda owns the bounded enrollment record, role acceptance binding, lifecycle state, and a machine-status projection that reports role/candidate eligibility.

## Supported v1 bootstrap classes

The repository-owned bootstrap commands are read-only. They inspect an exact CMUX checkout and exact Glaeda executable, produce a bounded receipt, and leave package installation, MDM, SSH, networking, and machine account policy with their existing owners.

- macOS: Apple Silicon for the v1 `cmux_macos_native_build` role; macOS major 15 or 26; CMUX `.xcode-version` must be `26.0`; the selected Xcode and macOS SDK must be generation 26; Git present; at least 120 GiB free by default; AC system sleep disabled for unattended work. `cmux-mac-build-large` requires at least 8 logical CPUs and 16 GiB total memory.
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

Role eligibility binds repository-owned CMUX semantic profiles rather than a duplicate acceptance recipe. Bootstrap reads `scripts/ci/cmux-workload-profiles.json`, requires `cmux_linux_ci -> cmux.ci.guard@1` or `cmux_macos_native_build -> cmux.macos.dev-check@1`, and checks that the selected profile admits the observed OS/architecture. Finalization consumes the canonical `cmux-workload-result/v1` emitted by that profile, records only its exact digest/state plus Glaeda-owned enrollment/toolchain/process-settlement identity, and never reconstructs CMUX argv, artifacts, or pass/fail semantics.

Canonical enrollment and acceptance documents used for lifecycle or routing-candidate decisions are read through a no-follow private-file gate: current-user owned, regular, single-link, mode `0600`, bounded size, and unchanged inode/metadata across the read. A loose, symlinked, or swapped file cannot supply eligibility. `transition` remains a side-effect-free planner. `transition-apply` holds one private mutation lock, re-reads the current enrollment under that lock, writes a mode-0600 same-directory stage, fsyncs the staged bytes, atomically replaces `enrollment.json`, fsyncs the fleet directory, and revalidates the published bytes before returning.

## Lifecycle

States are:

`discovered -> enrolling -> eligible -> draining/quarantined -> retired`

Supported recovery paths also include `draining -> eligible` and `quarantined -> enrolling`. Every transition into `eligible` requires at least one current accepted role receipt. Leaving quarantine advances the enrollment generation, so every pre-quarantine acceptance receipt becomes stale immediately.

`scripts/cmux-fleet status` reports role/candidate eligibility only. Its `routingCandidateEligible` field means the enrollment and current acceptance receipt satisfy this contract; `automaticDispatchAuthorized` stays `false`. #546 or another separately approved routing-promotion gate owns queue selection and dispatch, including queue/start prediction, hot locality, pressure, allowance scarcity, workload-class evidence, and operator policy. The status projection includes only bounded architecture, OS class, hardware class, accepted toolchain generations, role profiles, Glaeda generation, fleet scope, lifecycle state, and per-role eligibility. A node in `draining`, `quarantined`, or `retired` produces zero eligible roles even when an older acceptance receipt exists. An `eligible` node still produces zero eligible roles when its acceptance evidence is absent, rejected, or stale. Live pressure/heat remains a fresh local admission veto: #970 may publish advisory bounded snapshots, while #546/local execution admission re-observes the machine before dispatch.

Reviewed quarantine reasons are:

- `toolchain_mismatch`
- `disk_pressure`
- `failed_acceptance`
- `dirty_canonical_checkout`
- `service_mismatch`
- `hardware_failure`
- `stale_glaeda_generation`
- `unexplained_process_settlement`

## Onboard a Mac

Use an exact reviewed Glaeda checkout and the CMUX checkout that will run acceptance.

```bash
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
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

FLEET_ROOT="${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet"
umask 077
install -d -m 700 "$FLEET_ROOT" "$FLEET_ROOT/acceptance"
BOOTSTRAP="$(mktemp)"
CMUX_RESULT="$(mktemp)"
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

CMUX_COMMIT="$(git -C "$CMUX_ROOT" rev-parse HEAD)"
CMUX_TREE="$(git -C "$CMUX_ROOT" rev-parse 'HEAD^{tree}')"
CMUX_STATE="$(mktemp -d)"
python3 "$CMUX_ROOT/scripts/ci/cmux_workload_profile.py" run cmux.macos.dev-check \
  --generation 1 \
  --commit "$CMUX_COMMIT" \
  --tree "$CMUX_TREE" \
  --state-class cold \
  --state-root "$CMUX_STATE" \
  --result "$CMUX_RESULT"

TOOLCHAIN_GENERATION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["supportedToolchainGenerations"][0])' "$ENROLLMENT")"
ACCEPTANCE_NEXT="$(mktemp "$FLEET_ROOT/acceptance/.cmux_macos_native_build.XXXXXX")"
python3 scripts/cmux_fleet.py finalize-acceptance \
  "$ENROLLMENT" "$CMUX_RESULT" \
  --role cmux_macos_native_build \
  --toolchain-generation "$TOOLCHAIN_GENERATION" \
  > "$ACCEPTANCE_NEXT"
chmod 600 "$ACCEPTANCE_NEXT"
mv "$ACCEPTANCE_NEXT" "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The macOS preparation reuses CMUX's reviewed `scripts/setup.sh` for prerequisites. Bootstrap re-observes those prerequisites read-only and verifies the exact checkout exposes `cmux.macos.dev-check@1` for the observed Apple-Silicon node. CMUX's own profile runner owns the developer-build commands, semantic validator, artifact checks, timeout, and cleanup result. Glaeda accepts only the matching profile/result digest and requires complete process settlement; it does not maintain a second definition of the CMUX build.

## Onboard Linux

```bash
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
BOOTSTRAP="$(mktemp)"
ACCEPTANCE_EVIDENCE="$(mktemp)"
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

CMUX_COMMIT="$(git -C "$CMUX_ROOT" rev-parse HEAD)"
CMUX_TREE="$(git -C "$CMUX_ROOT" rev-parse 'HEAD^{tree}')"
CMUX_STATE="$(mktemp -d)"
python3 "$CMUX_ROOT/scripts/ci/cmux_workload_profile.py" run cmux.ci.guard \
  --generation 1 \
  --commit "$CMUX_COMMIT" \
  --tree "$CMUX_TREE" \
  --state-class cold \
  --state-root "$CMUX_STATE" \
  --result "$CMUX_RESULT"

TOOLCHAIN_GENERATION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["supportedToolchainGenerations"][0])' "$ENROLLMENT")"
ACCEPTANCE_NEXT="$(mktemp "$FLEET_ROOT/acceptance/.cmux_linux_ci.XXXXXX")"
python3 scripts/cmux_fleet.py finalize-acceptance \
  "$ENROLLMENT" "$CMUX_RESULT" \
  --role cmux_linux_ci \
  --toolchain-generation "$TOOLCHAIN_GENERATION" \
  > "$ACCEPTANCE_NEXT"
chmod 600 "$ACCEPTANCE_NEXT"
mv "$ACCEPTANCE_NEXT" "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition-apply "$ENROLLMENT" --to eligible \
  --acceptance "$ACCEPTANCE"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The Linux acceptance runs CMUX's canonical `cmux.ci.guard@1` profile against the exact commit/tree in cold state. CMUX owns the guard list and pass/fail semantics. Glaeda binds the canonical semantic-result digest and requires complete process settlement before the role can become candidate-eligible.

After either onboarding path, remove only the transient evidence files:

```bash
rm -f "$BOOTSTRAP" "$CMUX_RESULT"
rm -rf "$CMUX_STATE"
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

After a toolchain, OS, hardware class, or Glaeda update, rerun bootstrap and create a fresh enrollment with generation N+1. Old acceptance receipts then become stale by construction.

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

Hosted CI needs no physical CMUX machine. Glaeda CI runs the enrollment/bootstrap contract suites and synthetic CMUX semantic-result fixtures. CMUX CI owns the workload-profile contract and executes `cmux.ci.guard@1` through its canonical runner. Physical Mac/Linux acceptance receipts are attached only after the corresponding CMUX-controlled hosts exist and have been explicitly approved for the profile.

Related Glaeda work: #743, #546, #365, #492, #970, #1048, #1008, #1010, #1071.
Related CMUX work: manaflow-ai/cmux#13091, #13095, #13198, #13325, #13411.
