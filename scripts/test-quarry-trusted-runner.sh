#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
helper="${script_dir}/quarry-trusted-runner.sh"
tmp="$(mktemp -d)"
trap 'rm -rf -- "${tmp}"' EXIT HUP INT TERM

bin="${tmp}/bin"
state="${tmp}/state"
home="${tmp}/home"
mkdir -p "${bin}" "${state}" "${home}/Library/LaunchAgents"
log="${state}/commands.log"
configured="${state}/configured"
routing="${state}/routing"
: > "${log}"
printf 'ubuntu-24.04\n' > "${routing}"

cat > "${bin}/uname" <<'STUB'
#!/usr/bin/env bash
printf 'Darwin\n'
STUB

cat > "${bin}/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf 'gh %s\n' "$*" >> "${TRUSTED_RUNNER_TEST_LOG}"
case "$*" in
  'api --method POST repos/Quarry-Labs/quarry/actions/runners/registration-token --jq .token')
    printf 'registration-secret\n'
    ;;
  'api --method POST repos/Quarry-Labs/quarry/actions/runners/remove-token --jq .token')
    printf 'removal-secret\n'
    ;;
  *'actions/runners --jq '*'.id == 22'*'@tsv'*)
    if [ "${TRUSTED_RUNNER_TEST_GITHUB_BINDING:-exact}" = exact ]; then
      printf 'online\tfalse\n'
    fi
    ;;
  *'actions/runners --jq '*'.id == 22'*'.status'*)
    if [ "${TRUSTED_RUNNER_TEST_GITHUB_BINDING:-exact}" = exact ]; then
      printf 'online\n'
    fi
    ;;
  'variable set CI_LINUX_RUNNER --repo Quarry-Labs/quarry --body '*)
    printf '%s\n' "${*: -1}" > "${TRUSTED_RUNNER_TEST_ROUTING}"
    ;;
  'variable get CI_LINUX_RUNNER --repo Quarry-Labs/quarry')
    cat "${TRUSTED_RUNNER_TEST_ROUTING}"
    ;;
  *)
    printf 'unexpected gh invocation: %s\n' "$*" >&2
    exit 91
    ;;
esac
STUB

cat > "${bin}/limactl" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf 'limactl %s\n' "$*" >> "${TRUSTED_RUNNER_TEST_LOG}"
case "${1:-}" in
  list)
    if [ "${2:-}" = --quiet ]; then
      printf 'smolrunner\n'
    elif [ "${2:-}" = --format ]; then
      printf 'Running\n'
    else
      exit 92
    fi
    ;;
  start)
    ;;
  autostart)
    if [ "${2:-}" = enable ]; then
      : > "${HOME}/Library/LaunchAgents/io.lima-vm.autostart.smolrunner.plist"
    elif [ "${2:-}" = disable ]; then
      [ "${TRUSTED_RUNNER_TEST_AUTOSTART_DISABLE_STATUS:-0}" = 0 ] \
        || exit "${TRUSTED_RUNNER_TEST_AUTOSTART_DISABLE_STATUS}"
      rm -f -- "${HOME}/Library/LaunchAgents/io.lima-vm.autostart.smolrunner.plist"
    else
      exit 95
    fi
    ;;
  shell)
    shift
    [ "${1:-}" = smolrunner ]
    shift
    [ "${1:-}" = -- ]
    shift
    command_line="$*"
    case "${command_line}" in
      '/usr/bin/test -f /home/lima/actions-runner/.runner')
        [ -f "${TRUSTED_RUNNER_TEST_CONFIGURED}" ]
        ;;
      '/usr/bin/jq -r [.agentId, .agentName] | @tsv /home/lima/actions-runner/.runner')
        printf '22\tquarry-trusted-mac-arm64\n'
        ;;
      *'RUNNER_VERSION=2.336.0'*)
        ;;
      *'RUNNER_REPOSITORY=Quarry-Labs/quarry'*)
        IFS= read -r token
        [ "${token}" = registration-secret ]
        : > "${TRUSTED_RUNNER_TEST_CONFIGURED}"
        ;;
      *'config.sh remove --token'*)
        IFS= read -r token
        [ "${token}" = removal-secret ]
        rm -f -- "${TRUSTED_RUNNER_TEST_CONFIGURED}"
        ;;
      *'RUNNER_DIR=/home/lima/actions-runner'*)
        ;;
      '/usr/bin/systemctl is-active actions.runner.Quarry-Labs-quarry.quarry-trusted-mac-arm64.service')
        printf 'active\n'
        ;;
      *)
        printf 'unexpected guest invocation: %s\n' "${command_line}" >&2
        exit 93
        ;;
    esac
    ;;
  *)
    printf 'unexpected limactl invocation: %s\n' "$*" >&2
    exit 94
    ;;
esac
STUB

chmod +x "${bin}/uname" "${bin}/gh" "${bin}/limactl"

cat > "${bin}/sleep" <<'STUB'
#!/usr/bin/env bash
exit 0
STUB
chmod +x "${bin}/sleep"

run_helper() {
  PATH="${bin}:/usr/bin:/bin" \
    HOME="${home}" \
    TRUSTED_RUNNER_TEST_LOG="${log}" \
    TRUSTED_RUNNER_TEST_CONFIGURED="${configured}" \
    TRUSTED_RUNNER_TEST_ROUTING="${routing}" \
    bash "${helper}" "$@"
}

install_output="${tmp}/install.out"
run_helper install > "${install_output}"
grep -F 'runner=quarry-trusted-mac-arm64 state=online' "${install_output}" >/dev/null
[ "$(cat "${routing}")" = quarry-trusted-local ]
[ -f "${configured}" ]
grep -F 'autostart enable --condition=login --tty=false smolrunner' "${log}" >/dev/null
grep -F '.id == 22' "${log}" >/dev/null

status_output="${tmp}/status.out"
run_helper status > "${status_output}"
grep -F 'runner=quarry-trusted-mac-arm64 state=online busy=false' "${status_output}" >/dev/null
grep -F 'routing=quarry-trusted-local' "${status_output}" >/dev/null
grep -Fx active "${status_output}" >/dev/null

run_helper unroute > /dev/null
[ "$(cat "${routing}")" = ubuntu-24.04 ]

binding_output="${tmp}/binding.out"
set +e
TRUSTED_RUNNER_TEST_GITHUB_BINDING=foreign \
  run_helper route > "${binding_output}" 2>&1
binding_status=$?
set -e
[ "${binding_status}" -ne 0 ]
[ "$(cat "${routing}")" = ubuntu-24.04 ]
grep -F 'did not become online' "${binding_output}" >/dev/null

run_helper route > /dev/null
[ "$(cat "${routing}")" = quarry-trusted-local ]

failed_remove_output="${tmp}/remove-failed.out"
set +e
TRUSTED_RUNNER_TEST_AUTOSTART_DISABLE_STATUS=7 \
  run_helper remove > "${failed_remove_output}" 2>&1
failed_remove_status=$?
set -e
[ "${failed_remove_status}" = 7 ]
if grep -F 'state=removed' "${failed_remove_output}" >/dev/null; then
  printf 'failed autostart disable was reported as removed\n' >&2
  exit 1
fi

remove_output="${tmp}/remove.out"
run_helper remove > "${remove_output}"
[ "$(cat "${routing}")" = ubuntu-24.04 ]
[ ! -e "${configured}" ]
grep -F 'autostart disable --tty=false smolrunner' "${log}" >/dev/null
grep -F 'vm_disk=preserved caches=preserved' "${remove_output}" >/dev/null

for secret in registration-secret removal-secret; do
  if grep -F "${secret}" \
    "${log}" "${install_output}" "${status_output}" \
    "${binding_output}" "${failed_remove_output}" "${remove_output}" >/dev/null; then
    printf 'one-time token escaped into observable output: %s\n' "${secret}" >&2
    exit 1
  fi
done

printf 'trusted Quarry runner tests passed\n'
