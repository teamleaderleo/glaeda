#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
helper="${script_dir}/macbook-runner-vm.sh"
tmp="$(mktemp -d)"
trap 'rm -rf -- "${tmp}"' EXIT HUP INT TERM

home="${tmp}/home"
bin="${tmp}/bin"
log="${tmp}/limactl.log"
mkdir -p "${home}/.lima/smolrunner" "${bin}"
: > "${log}"

cat > "${bin}/limactl" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${SMOLRUNNER_TEST_LOG}"
exit 99
STUB
chmod +x "${bin}/limactl"

run_invalid() {
  local label="$1" expected="$2" lima_home="$3" instance="$4"
  local stdout_file="${tmp}/${label}.out" stderr_file="${tmp}/${label}.err"
  : > "${log}"
  set +e
  HOME="${home}" \
  LIMA_HOME="${lima_home}" \
  SMOLRUNNER_VM="${instance}" \
  SMOLRUNNER_TEST_LOG="${log}" \
  PATH="${bin}:/usr/bin:/bin" \
    bash "${helper}" profile work > "${stdout_file}" 2> "${stderr_file}"
  local status=$?
  set -e
  [ "${status}" -ne 0 ] || {
    printf 'expected invalid helper environment to fail: %s\n' "${label}" >&2
    exit 1
  }
  grep -F -- "${expected}" "${stderr_file}" >/dev/null || {
    printf 'missing expected refusal for %s\n' "${label}" >&2
    cat "${stderr_file}" >&2
    exit 1
  }
  [ ! -s "${log}" ] || {
    printf 'limactl was called before environment validation: %s\n' "${label}" >&2
    cat "${log}" >&2
    exit 1
  }
}

run_invalid instance-traversal 'must begin with a lowercase ASCII letter or digit' "${home}/.lima" '../escape'
run_invalid instance-option 'must begin with a lowercase ASCII letter or digit' "${home}/.lima" '--debug'
run_invalid instance-uppercase 'must begin with a lowercase ASCII letter or digit' "${home}/.lima" 'SmolRunner'
run_invalid relative-home 'LIMA_HOME must be an absolute path' 'relative/lima' 'smolrunner'
run_invalid aliased-home 'lexically canonical non-root path' "${home}/.lima/../escape" 'smolrunner'
run_invalid root-home 'lexically canonical non-root path' '/' 'smolrunner'

[ ! -e "${home}/escape/smolrunner-vm-helper.lock" ]
[ ! -e "${home}/escape/smolrunner-operator-run.active" ]

printf 'macbook runner VM path validation tests passed\n'
