# Fleet candidate distribution

Owner: #525. Host promotion/rollback: #149. Enrollment: #1056/#1058.

`Fleet candidate bundles` produces native macOS ARM64 and Linux x86_64 candidates
on hosted machines. Each archive contains `bin/glaeda`, its matching fleet Python
tools, the enrollment runbook, license notices, and a manifest with exact source
commit/tree, target, Rust toolchain and per-file hashes. No Rust compiler is needed
on the receiving node. Fleet tools require Python 3.11 or newer.

The [GitHub runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
defines `macos-15` as ARM64; the builder also checks the actual native Rust target.
Linux candidates are built on Ubuntu 24.04, not a promise of compatibility with
older glibc hosts. Run local diagnostics and role acceptance on the receiving host.

## Produce and obtain a candidate

Dispatch the workflow on an exact reviewed commit, or a reviewed branch and then
verify its resolved SHA. Save the run ID; inspect that exact run's source, successful
jobs and candidate receipt before downloading. Pull-request runs are build tests,
not approved installation or update sources; their source may be a synthetic merge.

```bash
gh workflow run fleet-candidate.yml --repo teamleaderleo/glaeda --ref REVIEWED_REF
gh run view RUN_ID --repo teamleaderleo/glaeda
gh run download RUN_ID --repo teamleaderleo/glaeda \
  --name glaeda-candidate-aarch64-apple-darwin --dir /private/candidate-download
```

Use the receipt from the trusted run's summary for the archive SHA-256. A checksum
downloaded beside an unknown archive is not an independent source of trust.
From an independently reviewed Glaeda checkout, stage the exact archive:

```bash
mkdir -m 700 "$HOME/Projects/glaeda-generations" # first install only
python3 scripts/fleet_bundle.py stage /private/candidate-download/ARCHIVE.tar.gz \
  --sha256 SHA256_FROM_TRUSTED_RUN \
  --source EXACT_40_CHARACTER_COMMIT --target aarch64-apple-darwin \
  --directory "$HOME/Projects/glaeda-generations/GENERATION"
```

This previews the operation. Repeat with `--apply` to verify and unpack into that
new private directory. The parent must already exist, be owned by you with mode
0700, and use a canonical absolute path. Each update uses a new generation name.
The verifier checks bounded decompression, the closed file inventory, hashes,
source and target before writing. It rejects links and existing destinations.

A successful command returns `state: staged`, the binary/tool paths, and writes
`stage-receipt.json` after file readback and sync. Set `GLAEDA_BIN` to the returned
binary and run the included fleet tools from that generation; continue with
[bootstrap and local acceptance](CMUX_FLEET_ENROLLMENT.md), skipping the source-build
and binary-copy step. Keep the previous generation for rollback.

Interrupted staging leaves its directory for inspection. Choose a fresh directory
when retrying; an incomplete generation is never adopted. Staging runs no bundle
code and changes no services or active pointers. The receiving host still provides
Python, the CMUX checkout, Xcode and GitHub runner registration. For verification
alone, use `verify ARCHIVE --sha256 HASH --source COMMIT --target TARGET`.

## Local production

Use a clean dedicated checkout at an exact reviewed commit and the native target:

```bash
python3 scripts/fleet_bundle.py build --source EXACT_40_CHARACTER_COMMIT \
  --target aarch64-apple-darwin --output target/fleet-bundles
```

The builder uses locked Cargo resolution, an explicit target directory and a
restricted child environment. It checks source before and after the build and
refuses to overwrite an archive. Those checks do not freeze a shared checkout;
do not edit it concurrently. An isolated build produces the bundle, rather than
accepting an arbitrary prebuilt binary and claiming its source identity.

## Update boundary

These are downloadable candidates with a 30-day Actions retention period, not
permanent signed releases or an unattended update channel. Manifest identity
does not grant installation or service authority. #525 still owns durable release
publication, authenticated provenance and promotion of unchanged candidate bytes;
#149 owns staged installation, scheduler drain, canary verification, promotion and
rollback. No automatic predecessor upgrade is authorized by this format.

For now, an operator selects an exact reviewed candidate, preserves the previous
generation, and runs the enrollment renewal and acceptance sequence after updating.
Repository-side automation should build on this same bundle, without requiring
the CMUX team to design a second distribution system.
