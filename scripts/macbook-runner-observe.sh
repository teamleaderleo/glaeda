#!/usr/bin/env bash
set -eu

instance="${1:-smolrunner}"
lima_home="${LIMA_HOME:-${HOME}/.lima}"

section() {
  printf '\n== %s ==\n' "$1"
}

section "timestamp"
date -u '+%Y-%m-%dT%H:%M:%SZ'

section "macOS"
sw_vers
uname -a
sysctl hw.memsize
sysctl vm.swapusage
memory_pressure -Q || true
vm_stat

section "Lima"
if ! command -v limactl >/dev/null 2>&1; then
  printf 'limactl is unavailable\n'
  exit 0
fi

limactl --version
limactl list
if [ -d "${lima_home}/${instance}" ]; then
  du -sh "${lima_home}/${instance}"
fi
if [ -d "${HOME}/.cache/lima" ]; then
  du -sh "${HOME}/.cache/lima"
fi

section "VM processes"
ps -axo pid,ppid,%cpu,%mem,rss,vsz,etime,comm \
  | /usr/bin/awk 'NR == 1 || $8 ~ /(limactl|lima|qemu|Virtualization)/'

section "guest"
if ! limactl shell "${instance}" -- /usr/bin/true >/dev/null 2>&1; then
  printf 'instance %s is unavailable or stopped\n' "${instance}"
  exit 0
fi

limactl shell "${instance}" -- /usr/bin/uname -a
limactl shell "${instance}" -- /usr/bin/free -h
limactl shell "${instance}" -- /usr/bin/cat /proc/loadavg
limactl shell "${instance}" -- /usr/bin/uptime
limactl shell "${instance}" -- /usr/bin/df -h /
limactl shell "${instance}" -- /usr/bin/systemctl is-system-running || true
limactl shell "${instance}" -- /usr/bin/bash -lc '
  cgroup_path="$(awk -F: '\''$1 == "0" { print $3 }'\'' /proc/self/cgroup)"
  for metric in memory.current memory.peak; do
    file="/sys/fs/cgroup${cgroup_path}/${metric}"
    printf "%s: " "${metric}"
    if [ -r "${file}" ]; then
      cat "${file}"
    else
      printf "unavailable (%s)\n" "${file}"
    fi
  done
' || true

if limactl shell "${instance}" -- /usr/bin/test -x /usr/bin/podman; then
  limactl shell "${instance}" -- /usr/bin/podman system df
fi
