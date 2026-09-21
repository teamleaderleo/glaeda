# CMUX-owned fleet enrollment

Tracking issue: #1056.

This path turns a CMUX-controlled Mac or Linux host into a reviewed fleet node without making Glaeda the host's sole manager. GitHub Actions, CMUX's controller, direct SSH, MDM/configuration management, hosted runners, and future schedulers can continue to manage the parts they already own. Glaeda owns the bounded enrollment record, role acceptance binding, lifecycle state, and the machine-status projection used for automatic routing.

## Supported v1 bootstrap classes

The repository-owned bootstrap commands are read-only. They inspect an exact CMUX checkout and exact Glaeda executable, produce a bounded receipt, and leave package installation, MDM, SSH, networking, and machine account policy with their existing owners.

- macOS: Apple Silicon or x86_64; macOS major 15 or 26; CMUX `.xcode-version` must be `26.0`; the selected Xcode and macOS SDK must be generation 26; Git present; at least 120 GiB free by default; AC system sleep disabled for unattended work.
- Linux: x86_64 or arm64; Ubuntu 24.04 or Debian 12; kernel major 6 or newer; Git, Python, systemd, bubblewrap, cgroup v2, pressure signals; at least 40 GiB free and 8 GiB available memory by default. `cmux_linux_ci` also checks `curl`, `tar`, `gzip`, and `ldd`.

A different machine class starts as a bootstrap refusal until the reviewed policy changes.

## Roles

The v1 enrollment schema recognizes:

- `cmux_macos_native_build`
- `cmux_macos_test`
- `cmux_linux_ci`
- `cmux_linux_agent`
- `artifact_cache`
- `background_replay`
- `benchmark`

Enrollment is an allowlist. A role becomes eligible only when the node state is `eligible` and a current accepted role receipt matches the node ID, enrollment generation, Glaeda generation, and one enrolled toolchain generation.

The first executable acceptance recipes live in `manaflow-ai/cmux` for `cmux_macos_native_build` and `cmux_linux_ci`. Every other role stays ineligible until its own reviewed acceptance workload exists.

## Enrollment identity and privacy

`examples/cmux-fleet/enrollment-v1.schema.json` is the public v1 schema. The record contains only:

- opaque `cmux-*` node ID;
- architecture;
- OS family and version class;
- hardware capability class;
- accepted toolchain generations;
- allowed roles;
- operator/fleet ownership scope;
- enrollment generation;
- installed Glaeda generation;
- lifecycle state and reviewed quarantine reason.

The schema has no hostname, serial number, username, private address, SSH identity, MDM identifier, or raw hardware inventory fields. Unknown top-level fields are rejected.

## Lifecycle

States are:

`discovered -> enrolling -> eligible -> draining/quarantined -> retired`

Supported recovery paths also include `draining -> eligible` and `quarantined -> enrolling`.

Automatic routing reads `scripts/cmux-fleet status`. A node in `draining`, `quarantined`, or `retired` produces zero eligible roles even when an older acceptance receipt exists. An `eligible` node still produces zero eligible roles when its acceptance evidence is absent, rejected, or stale.

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
cargo build --locked --release
GLAEDA_BIN="$PWD/target/release/glaeda"
CMUX_ROOT=/absolute/path/to/cmux
BOOTSTRAP=/tmp/cmux-fleet-bootstrap.json
ENROLLMENT=/tmp/cmux-fleet-enrollment.json
ACCEPTANCE_EVIDENCE=/tmp/cmux-fleet-acceptance-evidence.json
ACCEPTANCE=/tmp/cmux-fleet-acceptance.json
NODE_ID=cmux-mac-001

bash scripts/cmux-fleet-bootstrap-macos \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --hardware-class cmux-mac-build-large \
  --role cmux_macos_native_build \
  > "$BOOTSTRAP"

python3 scripts/cmux_fleet.py enroll "$BOOTSTRAP" \
  --node-id "$NODE_ID" \
  --scope cmux-founders \
  --generation 1 \
  > "$ENROLLMENT"

CMUX_COMMIT="$(git -C "$CMUX_ROOT" rev-parse HEAD)"
bash "$CMUX_ROOT/scripts/fleet-accept-macos-native-build" \
  --commit "$CMUX_COMMIT" \
  --node-id "$NODE_ID" \
  --enrollment-generation 1 \
  --glaeda-generation "$(jq -r .glaedaGeneration "$ENROLLMENT")" \
  --toolchain-generation "$(jq -r '.supportedToolchainGenerations[0]' "$ENROLLMENT")" \
  --output "$ACCEPTANCE_EVIDENCE"

python3 scripts/cmux_fleet.py finalize-acceptance \
  "$ENROLLMENT" "$ACCEPTANCE_EVIDENCE" > "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition "$ENROLLMENT" --to eligible \
  > "$ENROLLMENT.next"
mv "$ENROLLMENT.next" "$ENROLLMENT"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The macOS acceptance creates a fresh private DerivedData root, runs an exact clean Debug build with code signing disabled, verifies the produced `cmux DEV` executable is Mach-O, rechecks the canonical checkout, and refuses a surviving acceptance process group.

## Onboard Linux

```bash
cargo build --locked --release
GLAEDA_BIN="$PWD/target/release/glaeda"
CMUX_ROOT=/absolute/path/to/cmux
BOOTSTRAP=/tmp/cmux-fleet-bootstrap.json
ENROLLMENT=/tmp/cmux-fleet-enrollment.json
ACCEPTANCE_EVIDENCE=/tmp/cmux-fleet-acceptance-evidence.json
ACCEPTANCE=/tmp/cmux-fleet-acceptance.json
NODE_ID=cmux-linux-001

bash scripts/cmux-fleet-bootstrap-linux \
  --cmux-root "$CMUX_ROOT" \
  --glaeda "$GLAEDA_BIN" \
  --hardware-class cmux-linux-ci-medium \
  --role cmux_linux_ci \
  > "$BOOTSTRAP"

python3 scripts/cmux_fleet.py enroll "$BOOTSTRAP" \
  --node-id "$NODE_ID" \
  --scope cmux-founders \
  --generation 1 \
  > "$ENROLLMENT"

CMUX_COMMIT="$(git -C "$CMUX_ROOT" rev-parse HEAD)"
bash "$CMUX_ROOT/scripts/fleet-accept-linux-ci" \
  --commit "$CMUX_COMMIT" \
  --node-id "$NODE_ID" \
  --enrollment-generation 1 \
  --glaeda-generation "$(jq -r .glaedaGeneration "$ENROLLMENT")" \
  --toolchain-generation "$(jq -r '.supportedToolchainGenerations[0]' "$ENROLLMENT")" \
  --output "$ACCEPTANCE_EVIDENCE"

python3 scripts/cmux_fleet.py finalize-acceptance \
  "$ENROLLMENT" "$ACCEPTANCE_EVIDENCE" > "$ACCEPTANCE"

python3 scripts/cmux_fleet.py transition "$ENROLLMENT" --to eligible \
  > "$ENROLLMENT.next"
mv "$ENROLLMENT.next" "$ENROLLMENT"

bash scripts/cmux-fleet status "$ENROLLMENT" \
  --acceptance "$ACCEPTANCE"
```

The Linux acceptance archives the exact commit into a private temporary tree, runs the CMUX self-hosted-runner guard and Linux routing unittest there, verifies the archive artifact, rechecks the canonical checkout, and refuses a surviving acceptance process group.

## Drain, quarantine, recover, and retire

Drain before planned operator work:

```bash
python3 scripts/cmux_fleet.py transition "$ENROLLMENT" --to draining \
  > "$ENROLLMENT.next"
mv "$ENROLLMENT.next" "$ENROLLMENT"
bash scripts/cmux-fleet status "$ENROLLMENT" --acceptance "$ACCEPTANCE"
```

Quarantine on a concrete reviewed reason:

```bash
python3 scripts/cmux_fleet.py transition "$ENROLLMENT" \
  --to quarantined --reason toolchain_mismatch \
  > "$ENROLLMENT.next"
mv "$ENROLLMENT.next" "$ENROLLMENT"
```

After a toolchain, OS, hardware class, or Glaeda update, rerun bootstrap and create a fresh enrollment with generation N+1. Old acceptance receipts then become stale by construction. Retire with `--to retired`.

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

For Glaeda distribution, the bootstrap consumes an exact executable path and records its SHA-256 generation. A release package, MDM payload, configuration manager, or a reviewed repository build can supply that file. Updating the executable always requires a fresh bootstrap and enrollment generation before automatic routing resumes.

## CI and synthetic evidence

Hosted CI needs no physical CMUX machine. Glaeda CI runs the enrollment and bootstrap fixture suites. CMUX CI runs the acceptance-harness fixture suite. Physical Mac/Linux acceptance receipts are attached only after the corresponding CMUX-controlled hosts exist and have been explicitly approved for the workload.

Related Glaeda work: #743, #546, #365, #492, #970, #1048, #1008, #1010.
Related CMUX work: manaflow-ai/cmux#13091, #13095, #13198, #13325.
