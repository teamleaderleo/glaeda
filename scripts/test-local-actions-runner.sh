#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
helper="${repo_root}/scripts/local-actions-runner.sh"
manifest="${repo_root}/examples/local-ci-runner.yml"

bash -n "${helper}"

contract="$(bash "${helper}" contract)"
printf '%s\n' "${contract}" | jq -e '
  .schema_version == 1 and
  .contract == "smolrunner-local-actions-listener" and
  .user == "smolrunner-runner" and
  .repository == "teamleaderleo/smolrunner" and
  .runner_name == "smolrunner-local-arm64" and
  .custom_label == "smolrunner-local-arm64" and
  .default_labels == ["self-hosted", "linux", "ARM64"] and
  .installation.source == "actions/runner" and
  .installation.platform == "linux-arm64" and
  .installation.exact_version_required == true and
  .installation.sha256_required == true and
  .installation.auto_update == false and
  .registration.token_source == "stdin_to_secret_environment" and
  .registration.persistent_token == false and
  .registration.service_install == false and
  .execution.environment == "allowlist" and
  .execution.rootless_podman_required == true and
  .execution.privileged_groups == false and
  .trust.forks == "deny" and
  .trust.trigger == "operator"
' >/dev/null

if bash "${helper}" contract unexpected >/dev/null 2>&1; then
  printf 'runner contract unexpectedly accepted an argument\n' >&2
  exit 1
fi

if bash "${helper}" install --version latest --sha256 "$(printf '0%.0s' {1..64})" >/dev/null 2>&1; then
  printf 'runner installer unexpectedly accepted a mutable version\n' >&2
  exit 1
fi

if bash "${helper}" install --version 2.334.0 --sha256 deadbeef >/dev/null 2>&1; then
  printf 'runner installer unexpectedly accepted a short checksum\n' >&2
  exit 1
fi

if bash "${helper}" register --token secret >/dev/null 2>&1; then
  printf 'runner registration unexpectedly accepted a token argument\n' >&2
  exit 1
fi

for required in \
  'expected_user="smolrunner-runner"' \
  'repository_url="https://github.com/teamleaderleo/smolrunner"' \
  'custom_label="smolrunner-local-arm64"' \
  'actions/runner/releases/download/v${requested_version}/actions-runner-linux-arm64-${requested_version}.tar.gz' \
  '"${sha256sum}" --check --status -' \
  'ACTIONS_RUNNER_INPUT_TOKEN=${secret_token}' \
  '--labels "${custom_label}"' \
  '--disableupdate' \
  'exec "${clean_env[@]}" ./run.sh' \
  '"${env_bin}" -i' \
  'assert_subordinate_ids' \
  'assert_no_privileged_groups' \
  "if [ -e /run/podman/podman.sock ] || [ -L /run/podman/podman.sock ]; then"
do
  grep -F -- "${required}" "${helper}" >/dev/null || {
    printf 'missing listener boundary: %s\n' "${required}" >&2
    exit 1
  }
done

for forbidden in \
  ' --token ' \
  'sudo ' \
  'svc.sh' \
  '--replace' \
  '--no-default-labels' \
  '--privileged' \
  'SSH_AUTH_SOCK' \
  'GITHUB_TOKEN' \
  'GH_TOKEN'
do
  if grep -F -- "${forbidden}" "${helper}" >/dev/null; then
    printf 'forbidden listener authority found: %s\n' "${forbidden}" >&2
    exit 1
  fi
done

grep -F 'repository: teamleaderleo/smolrunner' "${manifest}" >/dev/null
grep -F 'user: smolrunner-runner' "${manifest}" >/dev/null
grep -F 'smolrunner-local-arm64' "${manifest}" >/dev/null
grep -F 'forks: deny' "${manifest}" >/dev/null
grep -F 'trigger: operator' "${manifest}" >/dev/null
grep -F 'memory: 2GiB' "${manifest}" >/dev/null
grep -F 'cpus: 2' "${manifest}" >/dev/null
grep -F 'pids: 768' "${manifest}" >/dev/null

printf 'local Actions runner contract tests passed\n'
