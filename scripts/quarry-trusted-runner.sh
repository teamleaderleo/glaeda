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
autostart_plist="${HOME}/Library/LaunchAgents/io.lima-vm.autostart.${instance}.plist"

usage() {
  cat <<'USAGE'
Usage: bash scripts/quarry-trusted-runner.sh COMMAND

Commands:
  install   Install/start the persistent Quarry runner and route Quarry CI to it.
  status    Show the VM, guest service, GitHub runner, and routing state.
  route     Route Quarry's common Linux jobs to the trusted runner.
  unroute   Route Quarry's common Linux jobs back to GitHub-hosted Linux.
  pause     Route to hosted, stop the idle VM, and preserve its disk/caches.
  resume    Start the warm VM and route Quarry back to the trusted runner.
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

configured_runner_id() {
  local configured_identity configured_id configured_name
  configured_identity="$(
    limactl shell "${instance}" -- \
      /usr/bin/jq -r '[.agentId, .agentName] | @tsv' "${runner_dir}/.runner"
  )" || die 'unable to inspect the configured guest runner identity'
  IFS=$'\t' read -r configured_id configured_name <<EOF_IDENTITY
${configured_identity}
EOF_IDENTITY
  case "${configured_id}" in
    ''|0|*[!0-9]*) die 'the configured guest runner ID is invalid' ;;
  esac
  [ "${configured_name}" = "${runner_name}" ] \
    || die 'the persistent VM is configured for a different Actions runner'
  printf '%s\n' "${configured_id}"
}

observe_github_runner() {
  local configured_id="$1"
  gh api "repos/${repository}/actions/runners" \
    --jq ".runners[] | select( \
      .id == ${configured_id} and \
      .name == \"${runner_name}\" and \
      .os == \"Linux\" and \
      ([.labels[].name] | index(\"ARM64\")) != null and \
      ([.labels[].name] | index(\"${pilot_label}\")) != null \
    ) | [ \
      .id, \
      .status, \
      .busy, \
      (([.labels[].name] | index(\"${runner_label}\")) != null) \
    ] | @tsv"
}

require_github_identity() {
  local configured_id observation observed_id observed_state observed_busy observed_schedulable
  configured_id="$(configured_runner_id)"
  observation="$(observe_github_runner "${configured_id}")" \
    || die 'unable to observe the GitHub runner identity'
  [ -n "${observation}" ] \
    || die 'the configured guest runner is not the exact Quarry runner'
  IFS=$'\t' read -r \
    observed_id observed_state observed_busy observed_schedulable <<EOF_RUNNER
${observation}
EOF_RUNNER
  [ "${observed_id}" = "${configured_id}" ] \
    || die 'the GitHub runner binding changed during observation'
  printf '%s\n' "${configured_id}"
}

require_github_binding() {
  local configured_id observation observed_id state busy schedulable
  configured_id="$(require_github_identity)"
  observation="$(observe_github_runner "${configured_id}")" \
    || die 'unable to observe the GitHub runner binding'
  IFS=$'\t' read -r observed_id state busy schedulable <<EOF_RUNNER
${observation}
EOF_RUNNER
  [ "${schedulable}" = true ] \
    || die 'the exact Quarry runner is missing its scheduling label'
  printf '%s\n' "${configured_id}"
}

ensure_scheduling_label() {
  local configured_id="$1"
  local observation observed_id state busy schedulable
  observation="$(observe_github_runner "${configured_id}")" \
    || die 'unable to observe the GitHub runner before routing'
  IFS=$'\t' read -r observed_id state busy schedulable <<EOF_RUNNER
${observation}
EOF_RUNNER
  if [ "${schedulable}" != true ]; then
    printf '{"labels":["%s"]}\n' "${runner_label}" | \
      gh api --method POST \
        "repos/${repository}/actions/runners/${configured_id}/labels" \
        --input - >/dev/null \
      || die 'unable to restore the exact runner scheduling label'
  fi
  require_github_binding >/dev/null
}

drain_scheduling_label() {
  local configured_id="$1"
  local observation observed_id state busy schedulable
  observation="$(observe_github_runner "${configured_id}")" \
    || die 'unable to observe the GitHub runner before draining'
  IFS=$'\t' read -r observed_id state busy schedulable <<EOF_RUNNER
${observation}
EOF_RUNNER
  if [ "${schedulable}" = true ]; then
    gh api --method DELETE \
      "repos/${repository}/actions/runners/${configured_id}/labels/${runner_label}" \
      >/dev/null \
      || die 'unable to drain the exact runner scheduling label'
  fi
  observation="$(observe_github_runner "${configured_id}")" \
    || die 'unable to confirm the drained GitHub runner'
  IFS=$'\t' read -r observed_id state busy schedulable <<EOF_RUNNER
${observation}
EOF_RUNNER
  [ "${schedulable}" = false ] \
    || die 'the exact runner remained schedulable after the drain request'
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

stop_runner_service() {
  limactl shell "${instance}" -- \
    /usr/bin/env RUNNER_DIR="${runner_dir}" /usr/bin/bash -c '
set -euo pipefail
cd "${RUNNER_DIR}"
unit="/etc/systemd/system/actions.runner.Quarry-Labs-quarry.quarry-trusted-mac-arm64.service"
if /usr/bin/sudo /usr/bin/test -f "${unit}"; then
  /usr/bin/sudo ./svc.sh stop
fi
state="$(/usr/bin/sudo /usr/bin/systemctl show --property ActiveState --value \
  actions.runner.Quarry-Labs-quarry.quarry-trusted-mac-arm64.service)"
[ "${state}" = inactive ]
'
}

wait_until_online() {
  local configured_id="$1"
  local attempt state
  for attempt in $(seq 1 15); do
    state="$(observe_github_runner "${configured_id}" | /usr/bin/cut -f 2)" \
      || die 'unable to observe the GitHub runner'
    if [ "${state}" = online ]; then
      return 0
    fi
    sleep 2
  done
  die 'the trusted runner did not become online within 30 seconds'
}

require_runner_drained_idle() {
  local configured_id="$1"
  local observation observed_id state busy schedulable
  observation="$(observe_github_runner "${configured_id}")" \
    || die 'unable to observe the GitHub runner before pausing'
  [ -n "${observation}" ] \
    || die 'the exact GitHub runner disappeared before pausing'
  IFS=$'\t' read -r observed_id state busy schedulable <<EOF_RUNNER
${observation}
EOF_RUNNER
  [ "${schedulable}" = false ] \
    || die 'the exact GitHub runner is not drained before pausing'
  [ "${state}" = online ] \
    || die 'the exact GitHub runner is not online for a graceful pause'
  [ "${busy}" = false ] \
    || die 'the trusted runner is busy; Quarry is routed to hosted and pause can be retried'
}

require_runner_drained_not_busy() {
  local configured_id="$1"
  local observation observed_id state busy schedulable
  observation="$(observe_github_runner "${configured_id}")" \
    || die 'unable to observe the GitHub runner before removal'
  [ -n "${observation}" ] \
    || die 'the exact GitHub runner disappeared before removal'
  IFS=$'\t' read -r observed_id state busy schedulable <<EOF_RUNNER
${observation}
EOF_RUNNER
  [ "${schedulable}" = false ] \
    || die 'the exact GitHub runner is not drained before removal'
  case "${state}" in
    online|offline) ;;
    *) die 'the exact GitHub runner has an invalid state before removal' ;;
  esac
  [ "${busy}" = false ] \
    || die 'the trusted runner is busy; Quarry is routed to hosted and removal can be retried'
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
  local configured_id
  start_instance
  if runner_is_configured; then
    configured_id="$(require_github_identity)"
    ensure_scheduling_label "${configured_id}"
    configured_id="$(require_github_binding)"
  else
    install_runner_package
    register_runner
    configured_id="$(require_github_binding)"
  fi
  start_runner_service
  limactl autostart enable --condition=login --tty=false "${instance}"
  wait_until_online "${configured_id}"
  route_jobs
  printf 'runner=%s state=online vm=%s persistence=enabled caches=preserved\n' \
    "${runner_name}" "${instance}"
}

pause() {
  local configured_id
  unroute_jobs
  instance_exists || die "Lima instance '${instance}' does not exist"

  if ! instance_running; then
    die 'the VM is already stopped; exact runner offline state is unobserved'
  fi

  runner_is_configured \
    || die 'the guest runner configuration is absent; a graceful pause cannot be proven'
  configured_id="$(require_github_identity)"
  drain_scheduling_label "${configured_id}"
  require_runner_drained_idle "${configured_id}"

  if [ -e "${autostart_plist}" ]; then
    limactl autostart disable --tty=false "${instance}"
  fi
  [ ! -e "${autostart_plist}" ] \
    || die 'Lima autostart remained installed while pausing'
  stop_runner_service
  limactl stop --tty=false "${instance}"
  instance_running && die 'the Lima instance remained running after pause'
  printf 'runner=%s state=paused vm=%s memory=released caches=preserved\n' \
    "${runner_name}" "${instance}"
}

status() {
  local configured_id observed_id runner_observation vm_state routing
  local runner_state runner_busy runner_schedulable
  vm_state="$(limactl list --format '{{.Status}}' "${instance}" 2>/dev/null || true)"
  routing="$(gh variable get CI_LINUX_RUNNER --repo "${repository}" 2>/dev/null || true)"
  runner_state=absent
  runner_busy=unknown
  runner_schedulable=unknown
  if instance_running && runner_is_configured; then
    configured_id="$(configured_runner_id)"
    runner_observation="$(observe_github_runner "${configured_id}" 2>/dev/null || true)"
    if [ -n "${runner_observation}" ]; then
      IFS=$'\t' read -r \
        observed_id runner_state runner_busy runner_schedulable <<EOF_RUNNER
${runner_observation}
EOF_RUNNER
    fi
  fi

  printf 'vm=%s state=%s\n' "${instance}" "${vm_state:-unknown}"
  printf 'runner=%s state=%s busy=%s\n' \
    "${runner_name}" "${runner_state:-absent}" "${runner_busy:-unknown}"
  printf 'runner_schedulable=%s\n' "${runner_schedulable}"
  printf 'routing=%s\n' "${routing:-unset}"

  if [ "${runner_state}" != absent ]; then
    limactl shell "${instance}" -- \
      /usr/bin/systemctl is-active \
        actions.runner.Quarry-Labs-quarry.quarry-trusted-mac-arm64.service
  fi
}

remove_runner() {
  local configured_id removal_token
  unroute_jobs
  instance_exists \
    || die 'the Lima instance is absent; exact runner removal cannot be proven'
  start_instance
  runner_is_configured \
    || die 'the guest runner configuration is absent; exact removal cannot be proven'
  configured_id="$(require_github_identity)"
  [ -n "${configured_id}" ] || die 'the exact GitHub runner binding is unavailable'
  drain_scheduling_label "${configured_id}"
  require_runner_drained_not_busy "${configured_id}"

  # Disable automatic VM restart while the exact local and GitHub identities
  # are still available for a safe retry if the host operation fails.
  if [ -e "${autostart_plist}" ]; then
    limactl autostart disable --tty=false "${instance}"
  fi
  [ ! -e "${autostart_plist}" ] \
    || die 'Lima autostart remained installed after removal'

  limactl shell "${instance}" -- \
    /usr/bin/env RUNNER_DIR="${runner_dir}" /usr/bin/bash -c '
set -euo pipefail
cd "${RUNNER_DIR}"
unit="/etc/systemd/system/actions.runner.Quarry-Labs-quarry.quarry-trusted-mac-arm64.service"
if /usr/bin/sudo /usr/bin/test -f "${unit}"; then
  /usr/bin/sudo ./svc.sh stop
  /usr/bin/sudo ./svc.sh uninstall
fi
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
  printf 'runner=%s state=removed vm_disk=preserved caches=preserved\n' "${runner_name}"
}

validate_instance
require_macos
require_command limactl
require_command gh

case "${1:-}" in
  install) install ;;
  status) status ;;
  route)
    start_instance
    configured_id="$(require_github_identity)"
    ensure_scheduling_label "${configured_id}"
    configured_id="$(require_github_binding)"
    start_runner_service
    wait_until_online "${configured_id}"
    route_jobs
    ;;
  unroute) unroute_jobs ;;
  pause) pause ;;
  resume) install ;;
  remove) remove_runner ;;
  help|-h|--help|'') usage ;;
  *) die "unknown command: ${1}" ;;
esac
