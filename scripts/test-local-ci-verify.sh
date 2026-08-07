#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
helper="${repo_root}/scripts/local-ci-verify.sh"
containerfile="${repo_root}/containers/local-ci/Containerfile"

bash -n "${helper}"

contract="$(bash "${helper}" contract)"
printf '%s\n' "${contract}" | jq -e '
  .schema_version == 1 and
  .contract == "smolrunner.required-local" and
  .concurrency == 1 and
  .resources.cpus == 2 and
  .resources.memory_mib == 2048 and
  .resources.memory_swap_mib == 2560 and
  .resources.pids == 768 and
  .source.committed_only == true and
  .source.mount == "read_only" and
  .network.prepare == "enabled" and
  .network.verify == "disabled" and
  .container.rootless == true and
  .container.capabilities == "dropped" and
  .container.no_new_privileges == true and
  .container.rootfs == "read_only" and
  (.commands | length == 5) and
  (.commands[1] == "cargo fmt --all -- --check") and
  (.commands[4] == "cargo test --locked --all-targets --all-features --offline")
' >/dev/null

if bash "${helper}" contract unexpected >/dev/null 2>&1; then
  printf 'contract unexpectedly accepted an argument\n' >&2
  exit 1
fi

if bash "${helper}" exec -- /bin/sh -c id >/dev/null 2>&1; then
  printf 'wrapper unexpectedly exposed a generic exec command\n' >&2
  exit 1
fi

for required in \
  'podman=/usr/bin/podman' \
  'git=/usr/bin/git' \
  'unset CONTAINER_HOST DOCKER_HOST PODMAN_HOST CONTAINER_CONNECTION' \
  '--network="${network}"' \
  '--read-only' \
  '--cap-drop=all' \
  '--security-opt=no-new-privileges' \
  '--pid=private' \
  '--ipc=private' \
  '--pids-limit="${pids}"' \
  '--cpus="${cpus}"' \
  '--memory="${memory}"' \
  '--memory-swap="${memory_swap}"' \
  'destination=/workspace,ro=true' \
  'run_container "${verify_network}" cargo test --locked --all-targets --all-features --offline'
do
  grep -F -- "${required}" "${helper}" >/dev/null || {
    printf 'missing local verification boundary: %s\n' "${required}" >&2
    exit 1
  }
done

grep -F 'FROM docker.io/library/rust:1.97.1-bookworm' "${containerfile}" >/dev/null
grep -F 'rustup component add clippy rustfmt' "${containerfile}" >/dev/null

for forbidden in \
  'SSH_AUTH_SOCK' \
  'GITHUB_TOKEN' \
  'GH_TOKEN' \
  '/Users/' \
  '/var/run/docker.sock' \
  '--privileged' \
  '--network=host'
do
  if grep -F -- "${forbidden}" "${helper}" "${containerfile}" >/dev/null; then
    printf 'forbidden local verification authority found: %s\n' "${forbidden}" >&2
    exit 1
  fi
done

printf 'local CI verification contract tests passed\n'
