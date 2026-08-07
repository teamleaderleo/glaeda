#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
workflow="${repo_root}/.github/workflows/local-verification-canary.yml"

[ -f "${workflow}" ] || {
  printf 'local verification canary workflow is missing\n' >&2
  exit 1
}

for required in \
  'workflow_dispatch:' \
  'commit:' \
  "github.ref == 'refs/heads/main'" \
  "github.repository == 'teamleaderleo/smolrunner'" \
  "github.event_name == 'workflow_dispatch'" \
  'runs-on: [self-hosted, linux, ARM64, smolrunner-local-arm64]' \
  'contents: read' \
  'persist-credentials: false' \
  'fetch-depth: 0' \
  'actions/checkout@11d5960a326750d5838078e36cf38b85af677262' \
  'SOURCE_COMMIT: ${{ inputs.commit }}' \
  'CONTROL_SHA: ${{ github.sha }}' \
  '[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]' \
  '/usr/bin/git cat-file -e "${CONTROL_SHA}^{commit}"' \
  '/usr/bin/git merge-base --is-ancestor "$commit" "$CONTROL_SHA"' \
  "expected_wrapper_blob='ce493a2a7e7230f90db5ceb8b90d1fa6f9d5305f'" \
  '/usr/bin/git hash-object -- scripts/local-ci-verify.sh' \
  '/bin/bash scripts/local-ci-verify.sh prepare' \
  '/bin/bash scripts/local-ci-verify.sh verify' \
  '/usr/bin/podman image exists localhost/smolrunner-local-ci:rust-1.97.1-v1'
do
  grep -F -- "${required}" "${workflow}" >/dev/null || {
    printf 'missing local canary boundary: %s\n' "${required}" >&2
    exit 1
  }
done

if grep -Eq '^[[:space:]]+(pull_request|pull_request_target|push|schedule):' "${workflow}"; then
  printf 'local canary unexpectedly has an automatic or PR trigger\n' >&2
  exit 1
fi

for forbidden in \
  'secrets.' \
  'persist-credentials: true' \
  'pull_request_target' \
  'runs-on: ubuntu-' \
  'runs-on: macos-'
do
  if grep -F -- "${forbidden}" "${workflow}" >/dev/null; then
    printf 'forbidden local canary authority found: %s\n' "${forbidden}" >&2
    exit 1
  fi
done

if grep -Eq 'run:[[:space:]]*\$\{\{' "${workflow}"; then
  printf 'workflow input/expression was interpolated directly as executable shell text\n' >&2
  exit 1
fi

if [ "$(grep -c '^        uses:' "${workflow}")" -ne 1 ]; then
  printf 'local canary must contain exactly one pinned third-party action use\n' >&2
  exit 1
fi

printf 'local Actions canary authority tests passed\n'
