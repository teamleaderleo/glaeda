#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
helper="${script_dir}/macbook-runner-vm.sh"
tmp="$(mktemp -d)"
trap 'rm -rf -- "${tmp}"' EXIT HUP INT TERM

home="${tmp}/private-home"
bin="${tmp}/bin"
state_dir="${tmp}/state"
mkdir -p "${home}/.lima/smolrunner" "${bin}" "${state_dir}"
log="${state_dir}/limactl.log"
audit_log="${state_dir}/limactl-audit.log"
: > "${audit_log}"
status_file="${state_dir}/status"
cpus_file="${state_dir}/cpus"
memory_file="${state_dir}/memory"
worker_file="${state_dir}/worker"
command_status_file="${state_dir}/command-status"

reset_state() {
  : > "${log}"
  printf 'Running\n' > "${status_file}"
  printf '4\n' > "${cpus_file}"
  printf '3GiB\n' > "${memory_file}"
  printf 'idle\n' > "${worker_file}"
  printf '0\n' > "${command_status_file}"
  rm -rf -- "${home}/.lima/smolrunner/smolrunner-vm-helper.lock"
  rm -f -- "${home}/.lima/smolrunner/smolrunner-operator-run.active"
}

cat > "${bin}/limactl" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${SMOLRUNNER_TEST_LOG}"
printf '%s\n' "$*" >> "${SMOLRUNNER_TEST_AUDIT_LOG}"
command_name="${1:-}"
shift || true
case "${command_name}" in
  list)
    if [ "${1:-}" = --format ]; then
      shift 2
      printf '%s|%s|%s\n' \
        "$(cat "${SMOLRUNNER_TEST_STATUS}")" \
        "$(cat "${SMOLRUNNER_TEST_CPUS}")" \
        "$(cat "${SMOLRUNNER_TEST_MEMORY}")"
    else
      printf 'NAME STATUS CPUS MEMORY\nsmolrunner %s %s %s\n' \
        "$(cat "${SMOLRUNNER_TEST_STATUS}")" \
        "$(cat "${SMOLRUNNER_TEST_CPUS}")" \
        "$(cat "${SMOLRUNNER_TEST_MEMORY}")"
    fi
    ;;
  stop)
    printf 'Stopped\n' > "${SMOLRUNNER_TEST_STATUS}"
    ;;
  start)
    printf 'Running\n' > "${SMOLRUNNER_TEST_STATUS}"
    ;;
  edit)
    [ "${1:-}" = --tty=false ]
    [ "${2:-}" = --cpus ]
    printf '%s\n' "${3}" > "${SMOLRUNNER_TEST_CPUS}"
    [ "${4:-}" = --memory ]
    printf '%sGiB\n' "${5}" > "${SMOLRUNNER_TEST_MEMORY}"
    [ "${6:-}" = smolrunner ]
    ;;
  shell)
    [ "${1:-}" = smolrunner ]
    [ "${2:-}" = -- ]
    shift 2
    case "${1:-}" in
      /usr/bin/test)
        [ "${2:-}" = -x ]
        [ "${3:-}" = /usr/bin/pgrep ]
        ;;
      /usr/bin/pgrep)
        if [ "$(cat "${SMOLRUNNER_TEST_WORKER}")" = active ]; then
          exit 0
        fi
        exit 1
        ;;
      /usr/bin/nproc)
        cat "${SMOLRUNNER_TEST_CPUS}"
        ;;
      /usr/bin/awk)
        case "$(cat "${SMOLRUNNER_TEST_MEMORY}")" in
          3GiB) printf '3000000\n' ;;
          10GiB) printf '10000000\n' ;;
          *) printf '1\n' ;;
        esac
        ;;
      /usr/bin/fail-seven)
        exit 7
        ;;
      *)
        exit "$(cat "${SMOLRUNNER_TEST_COMMAND_STATUS}")"
        ;;
    esac
    ;;
  --version)
    printf 'limactl version test\n'
    ;;
  *)
    printf 'unexpected limactl command: %s\n' "${command_name}" >&2
    exit 99
    ;;
esac
STUB
chmod +x "${bin}/limactl"

run_helper() {
  HOME="${home}" \
  LIMA_HOME="${home}/.lima" \
  PATH="${bin}:/usr/bin:/bin" \
  GITHUB_TOKEN='top-secret-github-token' \
  AWS_SECRET_ACCESS_KEY='top-secret-aws-key' \
  SMOLRUNNER_TEST_LOG="${log}" \
  SMOLRUNNER_TEST_AUDIT_LOG="${audit_log}" \
  SMOLRUNNER_TEST_STATUS="${status_file}" \
  SMOLRUNNER_TEST_CPUS="${cpus_file}" \
  SMOLRUNNER_TEST_MEMORY="${memory_file}" \
  SMOLRUNNER_TEST_WORKER="${worker_file}" \
  SMOLRUNNER_TEST_COMMAND_STATUS="${command_status_file}" \
    bash "${helper}" "$@"
}

assert_contains() {
  local needle="$1" file="$2"
  grep -F -- "${needle}" "${file}" >/dev/null \
    || { printf 'missing expected text: %s\n' "${needle}" >&2; cat "${file}" >&2; exit 1; }
}

assert_not_contains() {
  local needle="$1" file="$2"
  if grep -F -- "${needle}" "${file}" >/dev/null; then
    printf 'unexpected text: %s\n' "${needle}" >&2
    cat "${file}" >&2
    exit 1
  fi
}

assert_count() {
  local expected="$1" needle="$2" file="$3" actual
  actual="$(grep -F -c -- "${needle}" "${file}" || true)"
  [ "${actual}" = "${expected}" ] \
    || { printf 'expected %s occurrences of %s, got %s\n' "${expected}" "${needle}" "${actual}" >&2; cat "${file}" >&2; exit 1; }
}

reset_state
cache_sentinel="${home}/.lima/smolrunner/cache-sentinel"
printf 'persistent-cache-identity\n' > "${cache_sentinel}"
work_output="${tmp}/work.out"
run_helper profile work > "${work_output}"
assert_contains 'profile=work state=running cpus=8 memory=10GiB verified=true' "${work_output}"
assert_contains "list --format {{.Status}}|{{.CPUs}}|{{.Memory}} smolrunner" "${log}"
assert_contains 'shell smolrunner -- /usr/bin/test -x /usr/bin/pgrep' "${log}"
assert_contains 'shell smolrunner -- /usr/bin/pgrep -f Runner.Worker' "${log}"
assert_contains 'stop smolrunner' "${log}"
assert_contains 'edit --tty=false --cpus 8 --memory 10 smolrunner' "${log}"
assert_contains 'start smolrunner' "${log}"
assert_contains 'shell smolrunner -- /usr/bin/nproc' "${log}"
assert_contains 'shell smolrunner -- /usr/bin/awk /^MemTotal:/ { print $2; exit } /proc/meminfo' "${log}"
[ "$(cat "${cache_sentinel}")" = persistent-cache-identity ]
assert_not_contains 'delete' "${log}"
assert_not_contains 'prune' "${log}"

: > "${log}"
run_helper profile work > "${tmp}/work-repeat.out"
assert_count 0 'stop smolrunner' "${log}"
assert_count 0 'edit --tty=false' "${log}"
assert_count 0 'start smolrunner' "${log}"
assert_contains 'shell smolrunner -- /usr/bin/nproc' "${log}"

reset_state
printf 'active\n' > "${worker_file}"
set +e
run_helper profile work > "${tmp}/active.out" 2> "${tmp}/active.err"
active_status=$?
set -e
[ "${active_status}" -ne 0 ]
assert_contains 'an Actions worker process is active' "${tmp}/active.err"
assert_count 0 'stop smolrunner' "${log}"
assert_count 0 'edit --tty=false' "${log}"
assert_not_contains "${home}" "${tmp}/active.err"

reset_state
printf 'active\n' > "${worker_file}"
set +e
run_helper profile interactive > "${tmp}/active-exact.out" 2> "${tmp}/active-exact.err"
active_exact_status=$?
set -e
[ "${active_exact_status}" -ne 0 ]
assert_contains 'an Actions worker process is active' "${tmp}/active-exact.err"
assert_not_contains 'shell smolrunner -- /usr/bin/nproc' "${log}"

reset_state
printf 'Stopped\n' > "${status_file}"
run_helper profile interactive > "${tmp}/interactive.out"
assert_count 0 'edit --tty=false' "${log}"
assert_contains 'start smolrunner' "${log}"
assert_contains 'profile=interactive state=running cpus=4 memory=3GiB verified=true' "${tmp}/interactive.out"

reset_state
run_helper run work -- /usr/bin/printf 'hello world' > "${tmp}/run.out"
assert_contains 'run profile=work state=starting' "${tmp}/run.out"
assert_contains 'run profile=work state=completed status=0 vm_shutdown=explicit' "${tmp}/run.out"
assert_contains 'shell smolrunner -- /usr/bin/printf hello world' "${log}"
assert_count 1 'stop smolrunner' "${log}"
[ ! -e "${home}/.lima/smolrunner/smolrunner-operator-run.active" ]


reset_state
printf 'active\n' > "${worker_file}"
set +e
run_helper run interactive -- /usr/bin/true > "${tmp}/run-active.out" 2> "${tmp}/run-active.err"
run_active_status=$?
set -e
[ "${run_active_status}" -ne 0 ]
assert_contains 'an Actions worker process is active' "${tmp}/run-active.err"
assert_not_contains 'shell smolrunner -- /usr/bin/true' "${log}"

reset_state
set +e
run_helper run work -- /usr/bin/fail-seven > "${tmp}/run-fail.out" 2> "${tmp}/run-fail.err"
run_status=$?
set -e
[ "${run_status}" -eq 7 ]
assert_contains 'run profile=work state=completed status=7 vm_shutdown=explicit' "${tmp}/run-fail.out"
[ ! -e "${home}/.lima/smolrunner/smolrunner-operator-run.active" ]

reset_state
printf 'profile=work\n' > "${home}/.lima/smolrunner/smolrunner-operator-run.active"
set +e
run_helper stop > "${tmp}/stop-active.out" 2> "${tmp}/stop-active.err"
stop_status=$?
set -e
[ "${stop_status}" -ne 0 ]
assert_contains 'an operator run is active' "${tmp}/stop-active.err"
assert_count 0 'stop smolrunner' "${log}"
assert_not_contains "${home}" "${tmp}/stop-active.err"
rm -f -- "${home}/.lima/smolrunner/smolrunner-operator-run.active"

reset_state
run_helper stop > "${tmp}/stop.out"
assert_contains 'state=stopped memory_released=true persistent_disk_retained=true' "${tmp}/stop.out"
assert_contains 'stop smolrunner' "${log}"
[ "$(cat "${cache_sentinel}")" = persistent-cache-identity ]

reset_state
set +e
run_helper profile away > /dev/null 2> "${tmp}/invalid-profile.err"
invalid_profile_status=$?
run_helper run work /usr/bin/true > /dev/null 2> "${tmp}/missing-separator.err"
missing_separator_status=$?
set -e
[ "${invalid_profile_status}" -ne 0 ]
[ "${missing_separator_status}" -ne 0 ]
assert_contains 'expected interactive or work' "${tmp}/invalid-profile.err"
assert_contains 'run requires PROFILE -- CMD...' "${tmp}/missing-separator.err"

assert_not_contains 'top-secret-github-token' "${audit_log}"
assert_not_contains 'top-secret-aws-key' "${audit_log}"
assert_not_contains 'GITHUB_TOKEN' "${audit_log}"
assert_not_contains 'AWS_SECRET_ACCESS_KEY' "${audit_log}"
if grep -Eq 'limactl (delete|prune|factory-reset)|--mount|SMOLRUNNER_GUEST_REPO=.*token|GITHUB_TOKEN|AWS_' "${helper}"; then
  printf 'helper contains a forbidden deletion, mount, or credential propagation path\n' >&2
  exit 1
fi

printf 'macbook runner VM helper tests passed\n'
