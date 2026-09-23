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
From an independently reviewed Glaeda checkout, verify without executing anything
in the bundle:

```bash
python3 scripts/fleet_bundle.py verify /private/candidate-download/ARCHIVE.tar.gz \
  --sha256 SHA256_FROM_TRUSTED_RUN \
  --source EXACT_40_CHARACTER_COMMIT --target aarch64-apple-darwin
```

The verifier checks the exact closed file inventory, sizes, hashes, source and
target, and rejects links, foreign paths, duplicates and incomplete bundles. It
does not extract, install, run the binary, touch services or grant update authority.
The download contains the named archive and `receipt.json`; select that exact
archive, never an arbitrary newest file.

After verification, unpack into a new private generation directory. Keep the
previous installed generation intact. Set `GLAEDA_BIN` to that directory's
`bin/glaeda` and run the included fleet tools from its root; continue with
[bootstrap and local acceptance](CMUX_FLEET_ENROLLMENT.md), skipping the source-build
and binary-copy step. The bundle does not include a CMUX checkout, Python, Xcode,
GitHub runner registration or operator credentials.

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
