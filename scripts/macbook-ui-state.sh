#!/usr/bin/env bash
set -euo pipefail

instance="${SMOLRUNNER_VM:-smolrunner}"
lima_home="${LIMA_HOME:-${HOME}/.lima}"
instance_dir="${lima_home}/${instance}"
active_run_marker="${instance_dir}/smolrunner-operator-run.active"

case "${instance}" in
  [a-z0-9]*) ;;
  *)
    printf 'error: invalid Lima instance name\n' >&2
    exit 1
    ;;
esac
case "${instance}" in
  *[!a-z0-9._-]*)
    printf 'error: invalid Lima instance name\n' >&2
    exit 1
    ;;
esac
case "${lima_home}" in
  /*) ;;
  *)
    printf 'error: LIMA_HOME must be absolute\n' >&2
    exit 1
    ;;
esac

command -v limactl >/dev/null 2>&1 || {
  printf 'error: limactl is unavailable\n' >&2
  exit 1
}
[ -d "${instance_dir}" ] || {
  printf 'error: Lima instance is unavailable\n' >&2
  exit 1
}

facts="$(limactl list --format '{{.Status}}|{{.CPUs}}|{{.Memory}}' "${instance}")" || {
  printf 'error: unable to observe Lima state\n' >&2
  exit 1
}
IFS='|' read -r observed_status observed_cpus observed_memory <<EOF_FACTS
${facts}
EOF_FACTS

case "${observed_status}" in
  Running) state=running ;;
  Stopped) state=stopped ;;
  *) state=transitioning ;;
esac

case "${observed_cpus}|${observed_memory}" in
  '4|3GiB') profile=interactive ;;
  '8|10GiB') profile=work ;;
  *) profile=custom ;;
esac

if [ -e "${active_run_marker}" ]; then
  operator_run_active=true
else
  operator_run_active=false
fi

actions_worker=idle
if [ "${state}" = running ]; then
  if limactl shell "${instance}" -- /usr/bin/test -x /usr/bin/pgrep >/dev/null 2>&1; then
    set +e
    limactl shell "${instance}" -- /usr/bin/pgrep -f Runner.Worker >/dev/null 2>&1
    worker_status=$?
    set -e
    case "${worker_status}" in
      0) actions_worker=active ;;
      1) actions_worker=idle ;;
      *) actions_worker=unknown ;;
    esac
  else
    actions_worker=unknown
  fi
fi

printf '{"schema_version":1,"state":"%s","profile":"%s","actions_worker":"%s","operator_run_active":%s}\n' \
  "${state}" "${profile}" "${actions_worker}" "${operator_run_active}"
