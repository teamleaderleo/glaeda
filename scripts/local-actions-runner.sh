#!/usr/bin/env bash
set -euo pipefail

expected_user="smolrunner-runner"
expected_home="/home/smolrunner-runner"
repository_url="https://github.com/teamleaderleo/smolrunner"
runner_name="smolrunner-local-arm64"
custom_label="smolrunner-local-arm64"
install_dir="${expected_home}/actions-runner"
marker="${install_dir}/.smolrunner-install"
work_dir="_work"

id=/usr/bin/id
getent=/usr/bin/getent
awk=/usr/bin/awk
curl=/usr/bin/curl
sha256sum=/usr/bin/sha256sum
tar=/usr/bin/tar
mktemp=/usr/bin/mktemp
podman=/usr/bin/podman
stat=/usr/bin/stat
env_bin=/usr/bin/env
rm=/usr/bin/rm
mkdir=/usr/bin/mkdir
chmod=/usr/bin/chmod

usage() {
  cat <<'USAGE'
Usage:
  bash scripts/local-actions-runner.sh contract
  bash scripts/local-actions-runner.sh check
  bash scripts/local-actions-runner.sh install --version VERSION --sha256 SHA256
  bash scripts/local-actions-runner.sh register    # reads one short-lived registration token from stdin
  bash scripts/local-actions-runner.sh run
  bash scripts/local-actions-runner.sh remove      # reads one short-lived removal token from stdin
  bash scripts/local-actions-runner.sh status

This helper must run as the dedicated `smolrunner-runner` guest user. Account creation, subordinate-ID
allocation, and linger belong to SmolRunner host preparation. The helper never uses sudo and never
installs a system service.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

validate_version() {
  local version="$1"
  [[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die 'runner version must be numeric MAJOR.MINOR.PATCH'
}

validate_sha256() {
  local digest="$1"
  [[ "${digest}" =~ ^[0-9a-f]{64}$ ]] || die 'runner SHA-256 must be exactly 64 lowercase hexadecimal characters'
}

listener_uid() {
  "${id}" -u "${expected_user}"
}

runtime_dir() {
  printf '/run/user/%s\n' "$(listener_uid)"
}

assert_no_privileged_groups() {
  local groups
  groups="$(${id} -nG)"
  for forbidden in sudo docker lxd root; do
    case " ${groups} " in
      *" ${forbidden} "*) die "listener account must not belong to privileged group ${forbidden}" ;;
    esac
  done
}

assert_subordinate_ids() {
  "${awk}" -F: -v user="${expected_user}" '
    $1 == user && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ && $3 >= 65536 { found = 1 }
    END { exit(found ? 0 : 1) }
  ' /etc/subuid || die 'listener account has no bounded subordinate UID range'

  "${awk}" -F: -v user="${expected_user}" '
    $1 == user && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ && $3 >= 65536 { found = 1 }
    END { exit(found ? 0 : 1) }
  ' /etc/subgid || die 'listener account has no bounded subordinate GID range'
}

assert_listener_identity() {
  local current_user passwd_home uid run_dir run_owner
  current_user="$(${id} -un)"
  [ "${current_user}" = "${expected_user}" ] || die "run this helper as ${expected_user}, not ${current_user}"
  [ "$(${id} -u)" -ne 0 ] || die 'listener helper refuses uid 0'

  passwd_home="$(${getent} passwd "${expected_user}" | "${awk}" -F: 'NR == 1 { print $6 }')"
  [ "${passwd_home}" = "${expected_home}" ] || die 'listener account home differs from the reviewed path'
  [ "${HOME:-}" = "${expected_home}" ] || die 'HOME differs from the reviewed listener home'
  [ -d "${expected_home}" ] || die 'listener home is missing'

  assert_no_privileged_groups
  assert_subordinate_ids

  uid="$(listener_uid)"
  run_dir="$(runtime_dir)"
  [ -d "${run_dir}" ] || die 'listener user runtime directory is unavailable; enable reviewed linger/session support first'
  run_owner="$(${stat} -c '%u' "${run_dir}")"
  [ "${run_owner}" = "${uid}" ] || die 'listener user runtime directory has the wrong owner'
}

assert_rootless_podman() {
  local run_dir rootless
  [ -x "${podman}" ] || die 'rootless Podman is unavailable at /usr/bin/podman'
  if [ -e /run/podman/podman.sock ] || [ -L /run/podman/podman.sock ]; then
    die 'privileged Podman socket path is present; repair the guest before listener activation'
  fi
  run_dir="$(runtime_dir)"
  rootless="$(
    XDG_RUNTIME_DIR="${run_dir}" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=${run_dir}/bus" \
      "${podman}" info --format '{{.Host.Security.Rootless}}' 2>/dev/null || true
  )"
  [ "${rootless}" = "true" ] || die 'listener account did not prove a rootless Podman boundary'
}

assert_guest_boundary() {
  assert_listener_identity
  assert_rootless_podman
  case "$(/usr/bin/uname -m)" in
    aarch64|arm64) ;;
    *) die 'local Actions listener requires an ARM64 Linux guest' ;;
  esac
}

parse_install_args() {
  requested_version=""
  requested_sha256=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --version)
        [ "$#" -ge 2 ] || die '--version requires a value'
        requested_version="$2"
        shift 2
        ;;
      --sha256)
        [ "$#" -ge 2 ] || die '--sha256 requires a value'
        requested_sha256="$2"
        shift 2
        ;;
      *) die "unsupported install argument: $1" ;;
    esac
  done
  [ -n "${requested_version}" ] || die '--version is required'
  [ -n "${requested_sha256}" ] || die '--sha256 is required'
  validate_version "${requested_version}"
  validate_sha256 "${requested_sha256}"
}

read_marker_value() {
  local key="$1"
  [ -f "${marker}" ] || return 1
  "${awk}" -F= -v key="${key}" '$1 == key { print substr($0, index($0, "=") + 1); found = 1 } END { exit(found ? 0 : 1) }' "${marker}"
}

installed_version() {
  read_marker_value version
}

installed_sha256() {
  read_marker_value sha256
}

verify_installation() {
  [ -d "${install_dir}" ] || die 'Actions runner installation is missing'
  [ -f "${marker}" ] || die 'Actions runner installation marker is missing'
  [ -x "${install_dir}/config.sh" ] || die 'Actions runner config.sh is missing or non-executable'
  [ -x "${install_dir}/run.sh" ] || die 'Actions runner run.sh is missing or non-executable'
  [ -x "${install_dir}/bin/Runner.Listener" ] || die 'Actions runner listener binary is missing or non-executable'

  local version binary_version
  version="$(installed_version)" || die 'installation marker has no version'
  validate_version "${version}"
  validate_sha256 "$(installed_sha256)" || true
  binary_version="$("${install_dir}/bin/Runner.Listener" --version 2>/dev/null | /usr/bin/head -n 1)"
  [ "${binary_version}" = "${version}" ] || die 'installed runner binary version differs from the reviewed marker'
}

archive_is_safe() {
  local list_file="$1"
  "${awk}" '
    function bad_component(path, count, parts, i) {
      count = split(path, parts, "/")
      for (i = 1; i <= count; i++) {
        if (parts[i] == "..") return 1
      }
      return 0
    }
    /^\// { bad = 1 }
    { if (bad_component($0)) bad = 1 }
    END { exit(bad ? 1 : 0) }
  ' "${list_file}"
}

install_runner() {
  parse_install_args "$@"
  assert_guest_boundary

  if [ -e "${install_dir}" ] || [ -L "${install_dir}" ]; then
    verify_installation
    if [ "$(installed_version)" = "${requested_version}" ] \
      && [ "$(installed_sha256)" = "${requested_sha256}" ]; then
      printf '{"schema_version":1,"operation":"install","disposition":"already_installed","version":"%s","package_sha256":"%s"}\n' \
        "${requested_version}" "${requested_sha256}"
      return 0
    fi
    die 'a different Actions runner installation already exists; update requires a separately reviewed replacement'
  fi

  umask 077
  local temp archive list url lock_dir install_created
  lock_dir="${expected_home}/.smolrunner-actions-runner-install.lock"
  "${mkdir}" "${lock_dir}" 2>/dev/null || die 'another listener installation operation is active or left recovery debt'
  temp="$(${mktemp} -d "${expected_home}/.smolrunner-actions-runner.XXXXXX")"
  archive="${temp}/runner.tar.gz"
  list="${temp}/archive.list"
  install_created=0
  cleanup_install() {
    "${rm}" -rf -- "${temp}"
    if [ "${install_created}" -eq 1 ] && [ ! -f "${marker}" ]; then
      "${rm}" -rf -- "${install_dir}"
    fi
    /usr/bin/rmdir -- "${lock_dir}" 2>/dev/null || true
  }
  trap cleanup_install EXIT

  url="https://github.com/actions/runner/releases/download/v${requested_version}/actions-runner-linux-arm64-${requested_version}.tar.gz"
  "${curl}" --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "${archive}" -- "${url}"
  printf '%s  %s\n' "${requested_sha256}" "${archive}" | "${sha256sum}" --check --status - \
    || die 'downloaded Actions runner package SHA-256 differs from the reviewed digest'

  "${tar}" -tzf "${archive}" >"${list}"
  archive_is_safe "${list}" || die 'Actions runner archive contains an unsafe path'

  "${mkdir}" -m 0700 -- "${install_dir}"
  install_created=1
  "${tar}" -xzf "${archive}" -C "${install_dir}" --no-same-owner
  [ ! -e "${install_dir}/.runner" ] || die 'release archive unexpectedly contains runner registration state'
  [ ! -e "${install_dir}/.credentials" ] || die 'release archive unexpectedly contains runner credentials'

  printf 'version=%s\nsha256=%s\n' "${requested_version}" "${requested_sha256}" >"${marker}"
  "${chmod}" 0600 "${marker}"
  verify_installation

  printf '{"schema_version":1,"operation":"install","disposition":"installed","version":"%s","package_sha256":"%s","auto_update":false}\n' \
    "${requested_version}" "${requested_sha256}"
}

read_secret_token() {
  secret_token=""
  IFS= read -r secret_token || die 'expected one short-lived GitHub runner token on stdin'
  [ -n "${secret_token}" ] || die 'GitHub runner token is empty'
  [ "${#secret_token}" -le 4096 ] || die 'GitHub runner token exceeds the bounded input length'
  case "${secret_token}" in
    *$'\r'*|*$'\n'*|*$'\t'*) die 'GitHub runner token contains unsupported control whitespace' ;;
  esac
}

registration_env() {
  local run_dir
  run_dir="$(runtime_dir)"
  printf '%s\n' \
    "HOME=${expected_home}" \
    "USER=${expected_user}" \
    "LOGNAME=${expected_user}" \
    'SHELL=/bin/bash' \
    'PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin' \
    "XDG_RUNTIME_DIR=${run_dir}" \
    "DBUS_SESSION_BUS_ADDRESS=unix:path=${run_dir}/bus"
}

register_runner() {
  [ "$#" -eq 0 ] || die 'register accepts no command-line arguments'
  assert_guest_boundary
  verify_installation
  if [ -f "${install_dir}/.runner" ]; then
    printf '{"schema_version":1,"operation":"register","disposition":"already_registered","name":"%s","label":"%s","auto_update":false}\n' \
      "${runner_name}" "${custom_label}"
    return 0
  fi

  read_secret_token
  local run_dir status
  run_dir="$(runtime_dir)"
  set +e
  ACTIONS_RUNNER_INPUT_TOKEN="${secret_token}" \
  HOME="${expected_home}" \
  USER="${expected_user}" \
  LOGNAME="${expected_user}" \
  SHELL=/bin/bash \
  PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  XDG_RUNTIME_DIR="${run_dir}" \
  DBUS_SESSION_BUS_ADDRESS="unix:path=${run_dir}/bus" \
    "${install_dir}/config.sh" \
      --unattended \
      --url "${repository_url}" \
      --name "${runner_name}" \
      --labels "${custom_label}" \
      --work "${work_dir}" \
      --disableupdate
  status=$?
  set -e
  secret_token=""
  [ "${status}" -eq 0 ] || return "${status}"
  [ -f "${install_dir}/.runner" ] || die 'runner configuration returned success without registration state'

  printf '{"schema_version":1,"operation":"register","disposition":"registered","name":"%s","label":"%s","auto_update":false}\n' \
    "${runner_name}" "${custom_label}"
}

run_runner() {
  [ "$#" -eq 0 ] || die 'run accepts no arguments'
  assert_guest_boundary
  verify_installation
  [ -f "${install_dir}/.runner" ] || die 'runner is not registered'
  local run_dir
  run_dir="$(runtime_dir)"

  cd "${install_dir}"
  exec "${env_bin}" -i \
    HOME="${expected_home}" \
    USER="${expected_user}" \
    LOGNAME="${expected_user}" \
    SHELL=/bin/bash \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    XDG_RUNTIME_DIR="${run_dir}" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=${run_dir}/bus" \
    ./run.sh
}

remove_runner() {
  [ "$#" -eq 0 ] || die 'remove accepts no command-line arguments'
  assert_listener_identity
  verify_installation
  if [ ! -f "${install_dir}/.runner" ]; then
    printf '{"schema_version":1,"operation":"remove","disposition":"already_removed"}\n'
    return 0
  fi

  read_secret_token
  local run_dir status
  run_dir="$(runtime_dir)"
  set +e
  ACTIONS_RUNNER_INPUT_TOKEN="${secret_token}" \
  HOME="${expected_home}" \
  USER="${expected_user}" \
  LOGNAME="${expected_user}" \
  SHELL=/bin/bash \
  PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  XDG_RUNTIME_DIR="${run_dir}" \
  DBUS_SESSION_BUS_ADDRESS="unix:path=${run_dir}/bus" \
    "${install_dir}/config.sh" remove --unattended
  status=$?
  set -e
  secret_token=""
  [ "${status}" -eq 0 ] || return "${status}"
  [ ! -f "${install_dir}/.runner" ] || die 'runner removal returned success but registration state remains'

  printf '{"schema_version":1,"operation":"remove","disposition":"removed"}\n'
}

status_runner() {
  [ "$#" -eq 0 ] || die 'status accepts no arguments'
  assert_listener_identity
  if [ ! -d "${install_dir}" ]; then
    printf '{"schema_version":1,"installed":false,"registered":false}\n'
    return 0
  fi
  verify_installation
  local registered=false
  [ -f "${install_dir}/.runner" ] && registered=true
  printf '{"schema_version":1,"installed":true,"registered":%s,"version":"%s","package_sha256":"%s","name":"%s","label":"%s","auto_update":false}\n' \
    "${registered}" "$(installed_version)" "$(installed_sha256)" "${runner_name}" "${custom_label}"
}

check_runner() {
  [ "$#" -eq 0 ] || die 'check accepts no arguments'
  assert_guest_boundary
  if [ -d "${install_dir}" ]; then
    verify_installation
  fi
  printf '{"schema_version":1,"user":"%s","architecture":"arm64","rootless_podman":true,"privileged_groups":false,"subordinate_ids":true}\n' \
    "${expected_user}"
}

print_contract() {
  cat <<'JSON'
{"schema_version":1,"contract":"smolrunner-local-actions-listener","user":"smolrunner-runner","repository":"teamleaderleo/smolrunner","runner_name":"smolrunner-local-arm64","custom_label":"smolrunner-local-arm64","default_labels":["self-hosted","linux","ARM64"],"installation":{"source":"actions/runner","platform":"linux-arm64","exact_version_required":true,"sha256_required":true,"auto_update":false},"registration":{"token_source":"stdin_to_secret_environment","persistent_token":false,"service_install":false},"execution":{"environment":"allowlist","rootless_podman_required":true,"privileged_groups":false},"trust":{"forks":"deny","trigger":"operator"}}
JSON
}

command="${1:-}"
[ -n "${command}" ] || {
  usage >&2
  exit 2
}
shift

case "${command}" in
  contract)
    [ "$#" -eq 0 ] || die 'contract accepts no arguments'
    print_contract
    ;;
  check) check_runner "$@" ;;
  install) install_runner "$@" ;;
  register) register_runner "$@" ;;
  run) run_runner "$@" ;;
  remove) remove_runner "$@" ;;
  status) status_runner "$@" ;;
  help|-h|--help) usage ;;
  *)
    usage >&2
    exit 2
    ;;
esac
