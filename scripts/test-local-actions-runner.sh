#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
helper="${repo_root}/scripts/local-actions-runner.sh"
token_bridge="${repo_root}/scripts/local-actions-token-bridge.sh"
manifest="${repo_root}/examples/local-ci-runner.yml"

bash -n "${helper}"
bash -n "${token_bridge}"

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
  .installation.token_bridge_blob_pinned == true and
  .installation.token_bridge_pinned == true and
  .installation.auto_update == false and
  .registration.identity_fields_verified == true and
  .registration.token_source == "stdin_to_installed_bridge_to_secret_environment" and
  .registration.persistent_token == false and
  .registration.token_in_argv == false and
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
  'expected_token_bridge_blob="08c6efa27c3faf40729056c4d797317054058565"' \
  'hash-object --no-filters' \
  'actions/runner/releases/download/v${requested_version}/actions-runner-linux-arm64-${requested_version}.tar.gz' \
  '"${sha256sum}" --check --status -' \
  'token_bridge_sha256=' \
  'verify_registration_identity' \
  '(.AgentName // .agentName // "") == $expected_name' \
  '(.GitHubUrl // .gitHubUrl // "") == $expected_url' \
  '(.WorkFolder // .workFolder // "") == $expected_work' \
  '(.DisableUpdate // .disableUpdate // false) == true' \
  '/bin/bash "${installed_token_bridge}"' \
  '--labels "${custom_label}"' \
  '--disableupdate' \
  'clean_env=(' \
  '"${env_bin}" -i' \
  'exec "${clean_env[@]}" ./run.sh' \
  'assert_subordinate_ids' \
  'assert_no_privileged_groups' \
  "if [ -e /run/podman/podman.sock ] || [ -L /run/podman/podman.sock ]; then"
do
  grep -F -- "${required}" "${helper}" >/dev/null || {
    printf 'missing listener boundary: %s\n' "${required}" >&2
    exit 1
  }
done

for required in \
  'umask 077' \
  'expected_config="/home/smolrunner-runner/actions-runner/config.sh"' \
  'IFS= read -r secret_token' \
  'export ACTIONS_RUNNER_INPUT_TOKEN="${secret_token}"' \
  'unset secret_token' \
  'exec "${config}" "$@"'
do
  grep -F -- "${required}" "${token_bridge}" >/dev/null || {
    printf 'missing token-bridge boundary: %s\n' "${required}" >&2
    exit 1
  }
done

for forbidden in \
  ' --token ' \
  'svc.sh' \
  '--replace' \
  '--no-default-labels' \
  '--privileged' \
  'SSH_AUTH_SOCK' \
  'GITHUB_TOKEN' \
  'GH_TOKEN' \
  'CONTAINER_HOST=' \
  'DOCKER_HOST=' \
  'PODMAN_HOST='
do
  if grep -F -- "${forbidden}" "${helper}" "${token_bridge}" >/dev/null; then
    printf 'forbidden listener authority found: %s\n' "${forbidden}" >&2
    exit 1
  fi
done

if grep -Eq '^[[:space:]]*(sudo|doas)([[:space:]]|$)' "${helper}" "${token_bridge}"; then
  printf 'listener control unexpectedly contains a privilege-elevation command\n' >&2
  exit 1
fi

grep -F 'repository: teamleaderleo/smolrunner' "${manifest}" >/dev/null
grep -F 'user: smolrunner-runner' "${manifest}" >/dev/null
grep -F 'smolrunner-local-arm64' "${manifest}" >/dev/null
grep -F 'forks: deny' "${manifest}" >/dev/null
grep -F 'trigger: operator' "${manifest}" >/dev/null
grep -F 'memory: 2GiB' "${manifest}" >/dev/null
grep -F 'cpus: 2' "${manifest}" >/dev/null
grep -F 'pids: 768' "${manifest}" >/dev/null

printf 'local Actions runner contract tests passed\n'
