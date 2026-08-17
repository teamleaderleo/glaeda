#!/usr/bin/env bash
set -euo pipefail

# Quarry's explicitly trusted, persistent fast lane. This intentionally does
# not share the hostile-workload disposable-worker lifecycle.
instance="${SMOLRUNNER_TRUSTED_VM:-smolrunner}"
repository="Quarry-Labs/quarry"
runner_name="quarry-trusted-mac-arm64"
runner_label="quarry-trusted-local"
pilot_label="smolrunner-quarry-pilot"
runner_dir="/home/lima/actions-runner"
runner_version="2.336.0"
runner_size="138824064"
runner_sha256="58b758e420b87093fbd4bfddd368074960053e2f1388f01848c82624b90f27d1"
hosted_fallback="ubuntu-24.04"

usage() {
  cat <<'USAGE'
Usage: bash scripts/quarry-trusted-runner.sh COMMAND

Commands:
  install   Install/start the persistent Quarry runner and route Quarry CI to it.
  status    Show the VM, guest service, GitHub runner, and routing state.
  route     Route Quarry's common Linux jobs to the trusted runner.
  unroute   Route Quarry's common Linux jobs back to GitHub-hosted Linux.
  remove    Unroute, unregister, and disable autostart; preserve VM disk/caches.

This lane is for operator-trusted Quarry jobs. It preserves the VM, workspace,
toolchains, package caches, and Actions caches between jobs for minimum latency.
Use the disposable worker service for hostile or third-party repositories.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_macos() {
  [ "$(uname -s)" = Darwin ] || die 'the trusted Quarry runner is supported on macOS only'
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is unavailable: $1"
}

validate_instance() {
  case "${instance}" in
    ''|-*|.*|*[!A-Za-z0-9._-]*)
      die 'SMOLRUNNER_TRUSTED_VM is not a safe Lima instance name'
      ;;
  esac
}

instance_running() {
  [ "$(limactl list --quiet --filter '.status == "Running"' "${instance}" 2>/dev/null)" = "${instance}" ]
}

instance_exists() {
  [ "$(limactl list --quiet "${instance}" 2>/dev/null)" = "${instance}" ]
}

start_instance() {
  instance_exists || die "Lima instance '${instance}' does not exist"
  if ! instance_running; then
    limactl start --tty=false "${instance}"
  fi
}

runner_is_configured() {
  limactl shell "${instance}" -- /usr/bin/test -f "${runner_dir}/.runner" >/dev/null 2>&1
}

verify_configured_name() {
  local configured_name
  configured_name="$(
    limactl shell "${instance}" -- /usr/bin/jq -r .agentName "${runner_dir}/.runner"
  )" || die 'unable to inspect the configured guest runner identity'
  [ "${configured_name}" = "${runner_name}" ] \
    || die 'the persistent VM is configured for a different Actions runner'
}

install_runner_package() {
  limactl shell "${instance}" -- \
    /usr/bin/env \
      RUNNER_VERSION="${runner_version}" \
      RUNNER_SIZE="${runner_size}" \
      RUNNER_SHA256="${runner_sha256}" \
    /usr/bin/bash -c '
set -euo pipefail
runner_dir=/home/lima/actions-runner
archive="/home/lima/actions-runner-linux-arm64-${RUNNER_VERSION}.tar.gz"
url="https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-arm64-${RUNNER_VERSION}.tar.gz"
hash_file() { /usr/bin/sha256sum -- "$1" | /usr/bin/cut -d " " -f 1; }

/usr/bin/mkdir -p -m 0700 -- "${runner_dir}"
if [ ! -f "${archive}" ] \
  || [ "$(/usr/bin/stat -c %s -- "${archive}")" != "${RUNNER_SIZE}" ] \
  || [ "$(hash_file "${archive}")" != "${RUNNER_SHA256}" ]; then
  next="${archive}.next"
  /usr/bin/rm -f -- "${next}"
  /usr/bin/curl \
    --fail --location --silent --show-error \
    --connect-timeout 15 --max-time 300 \
    --retry 3 --retry-all-errors --retry-max-time 300 \
    --output "${next}" "${url}"
  [ "$(/usr/bin/stat -c %s -- "${next}")" = "${RUNNER_SIZE}" ]
  [ "$(hash_file "${next}")" = "${RUNNER_SHA256}" ]
  /usr/bin/mv -- "${next}" "${archive}"
fi

if [ ! -x "${runner_dir}/config.sh" ]; then
  /usr/bin/tar -xzf "${archive}" -C "${runner_dir}"
fi
cd "${runner_dir}"
/usr/bin/sudo ./bin/installdependencies.sh >/dev/null
'
}

register_runner() {
  local registration_token
  registration_token="$(
    gh api --method POST \
      "repos/${repository}/actions/runners/registration-token" \
      --jq .token
  )" || die 'unable to obtain a one-time runner registration token'
  [ -n "${registration_token}" ] || die 'GitHub returned an empty runner registration token'

  # The one-time token crosses into the guest only over stdin. It never appears
  # in the host-side limactl argv, environment, filesystem, or output.
  printf '%s\n' "${registration_token}" | \
    limactl shell "${instance}" -- \
      /usr/bin/env \
        RUNNER_REPOSITORY="${repository}" \
        RUNNER_NAME="${runner_name}" \
        RUNNER_LABELS="${runner_label},${pilot_label}" \
        RUNNER_DIR="${runner_dir}" \
      /usr/bin/bash -c '
set -euo pipefail
IFS= read -r registration_token
[ -n "${registration_token}" ]
cd "${RUNNER_DIR}"
./config.sh --unattended \
  --url "https://github.com/${RUNNER_REPOSITORY}" \
  --token "${registration_token}" \
  --name "${RUNNER_NAME}" \
  --labels "${RUNNER_LABELS}" \
  --work _work
unset registration_token
'
  unset registration_token
}

start_runner_service() {
  limactl shell "${instance}" -- \
    /usr/bin/env RUNNER_DIR="${runner_dir}" /usr/bin/bash -c '
set -euo pipefail
cd "${RUNNER_DIR}"
unit="/etc/systemd/system/actions.runner.Quarry-Labs-quarry.quarry-trusted-mac-arm64.service"
if ! /usr/bin/sudo /usr/bin/test -f "${unit}"; then
  /usr/bin/sudo ./svc.sh install lima
fi
/usr/bin/sudo ./svc.sh start
'
}

wait_until_online() {
  local attempt state
  for attempt in $(seq 1 15); do
    state="$(
      gh api "repos/${repository}/actions/runners" \
        --jq ".runners[] | select(.name == \"${runner_name}\") | .status"
    )" || die 'unable to observe the GitHub runner'
    if [ "${state}" = online ]; then
      return 0
    fi
    sleep 2
  done
  die 'the trusted runner did not become online within 30 seconds'
}

route_jobs() {
  gh variable set CI_LINUX_RUNNER --repo "${repository}" --body "${runner_label}"
  printf 'routing=trusted label=%s\n' "${runner_label}"
}

unroute_jobs() {
  gh variable set CI_LINUX_RUNNER --repo "${repository}" --body "${hosted_fallback}"
  printf 'routing=github-hosted label=%s\n' "${hosted_fallback}"
}

install() {
  start_instance
  if runner_is_configured; then
    verify_configured_name
  else
    install_runner_package
    register_runner
    verify_configured_name
  fi
  start_runner_service
  limactl autostart enable --condition=login --tty=false "${instance}"
  wait_until_online
  route_jobs
  printf 'runner=%s state=online vm=%s persistence=enabled caches=preserved\n' \
    "${runner_name}" "${instance}"
}

status() {
  local vm_state routing runner_state runner_busy
  vm_state="$(limactl list --format '{{.Status}}' "${instance}" 2>/dev/null || true)"
  routing="$(gh variable get CI_LINUX_RUNNER --repo "${repository}" 2>/dev/null || true)"
  runner_state="$(
    gh api "repos/${repository}/actions/runners" \
      --jq ".runners[] | select(.name == \"${runner_name}\") | .status" 2>/dev/null || true
  )"
  runner_busy="$(
    gh api "repos/${repository}/actions/runners" \
      --jq ".runners[] | select(.name == \"${runner_name}\") | .busy" 2>/dev/null || true
  )"

  printf 'vm=%s state=%s\n' "${instance}" "${vm_state:-unknown}"
  printf 'runner=%s state=%s busy=%s\n' \
    "${runner_name}" "${runner_state:-absent}" "${runner_busy:-unknown}"
  printf 'routing=%s\n' "${routing:-unset}"

  if instance_running && runner_is_configured; then
    limactl shell "${instance}" -- \
      /usr/bin/systemctl is-active \
        actions.runner.Quarry-Labs-quarry.quarry-trusted-mac-arm64.service
  fi
}

remove_runner() {
  local removal_token
  unroute_jobs
  if instance_exists; then
    start_instance
    if runner_is_configured; then
      verify_configured_name
      limactl shell "${instance}" -- \
        /usr/bin/env RUNNER_DIR="${runner_dir}" /usr/bin/bash -c '
set -euo pipefail
cd "${RUNNER_DIR}"
/usr/bin/sudo ./svc.sh stop || true
/usr/bin/sudo ./svc.sh uninstall || true
'
      removal_token="$(
        gh api --method POST \
          "repos/${repository}/actions/runners/remove-token" \
          --jq .token
      )" || die 'unable to obtain a one-time runner removal token'
      printf '%s\n' "${removal_token}" | \
        limactl shell "${instance}" -- \
          /usr/bin/env RUNNER_DIR="${runner_dir}" /usr/bin/bash -c '
set -euo pipefail
IFS= read -r removal_token
cd "${RUNNER_DIR}"
./config.sh remove --token "${removal_token}"
unset removal_token
'
      unset removal_token
    fi
    limactl autostart disable --tty=false "${instance}" || true
  fi
  printf 'runner=%s state=removed vm_disk=preserved caches=preserved\n' "${runner_name}"
}

validate_instance
require_macos
require_command limactl
require_command gh

case "${1:-}" in
  install) install ;;
  status) status ;;
  route) start_instance; start_runner_service; wait_until_online; route_jobs ;;
  unroute) unroute_jobs ;;
  remove) remove_runner ;;
  help|-h|--help|'') usage ;;
  *) die "unknown command: ${1}" ;;
esac
