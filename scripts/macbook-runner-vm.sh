#!/usr/bin/env bash
set -euo pipefail

instance="${SMOLRUNNER_VM:-smolrunner}"
guest_repo="${SMOLRUNNER_GUEST_REPO:-/home/lima/smolrunner}"
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "${script_dir}/.." && pwd)"
lima_home="${LIMA_HOME:-${HOME}/.lima}"

usage() {
  cat <<'EOF'
Usage: bash scripts/macbook-runner-vm.sh COMMAND [ARGS...]

Commands:
  up             Start the configured Lima instance if it is stopped.
  shell          Start the VM and open an interactive guest shell.
  tmux           Start the VM and attach/create the guest tmux session.
  status         Show Lima instance state and host/guest Git branch status.
  sync           Fast-forward the clean guest checkout to origin/main.
  doctor         Run SmolRunner doctor inside the guest checkout.
  observe        Run the read-only Mac/guest observation report.
  exec -- CMD    Run an explicit command inside the guest.
  stop           Gracefully stop the Lima instance.

Environment:
  SMOLRUNNER_VM          Lima instance name (default: smolrunner)
  SMOLRUNNER_GUEST_REPO  Guest checkout path (default: /home/lima/smolrunner)
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_lima() {
  command -v limactl >/dev/null 2>&1 \
    || die 'limactl is unavailable; install Lima first'
}

require_instance() {
  [ -d "${lima_home}/${instance}" ] || die "Lima instance '${instance}' does not exist. Create it from examples/lima/smolrunner-interactive.yaml before using this helper."
}

vm_is_running() {
  limactl shell "${instance}" -- /usr/bin/true >/dev/null 2>&1
}

start_vm() {
  require_lima
  require_instance
  if ! vm_is_running; then
    limactl start "${instance}"
  fi
}

guest_bash() {
  limactl shell "${instance}" -- \
    /usr/bin/env SMOLRUNNER_GUEST_REPO="${guest_repo}" \
    /usr/bin/bash -lc "$1"
}

show_git_status() {
  printf '\n== host checkout ==\n'
  git -C "${repo_root}" status --short --branch || true

  printf '\n== guest checkout ==\n'
  if vm_is_running; then
    guest_bash 'if [ -d "$SMOLRUNNER_GUEST_REPO/.git" ]; then git -C "$SMOLRUNNER_GUEST_REPO" status --short --branch; else printf "missing checkout: %s\n" "$SMOLRUNNER_GUEST_REPO"; fi' || true
  else
    printf 'instance %s is stopped\n' "${instance}"
  fi
}

command_name="${1:-}"
case "${command_name}" in
  up)
    start_vm
    ;;
  shell)
    start_vm
    exec limactl shell "${instance}"
    ;;
  tmux)
    start_vm
    exec limactl shell "${instance}" -- /usr/bin/bash -lc '
      if ! command -v tmux >/dev/null 2>&1; then
        printf "tmux is not installed in the guest. Run: sudo apt-get update && sudo apt-get install -y tmux\n" >&2
        exit 1
      fi
      exec tmux new-session -A -s smolrunner
    '
    ;;
  status)
    require_lima
    limactl list
    show_git_status
    ;;
  sync)
    start_vm
    guest_bash '
      set -euo pipefail
      cd "$SMOLRUNNER_GUEST_REPO"
      if [ -n "$(git status --porcelain)" ]; then
        printf "guest checkout has local changes; refusing to switch or update\n" >&2
        git status --short --branch >&2
        exit 1
      fi
      git switch main
      git fetch --prune origin main
      git merge --ff-only origin/main
      git status --short --branch
    '
    ;;
  doctor)
    start_vm
    guest_bash '
      set -euo pipefail
      cd "$SMOLRUNNER_GUEST_REPO"
      cargo run --locked --quiet -- --output json doctor
    '
    ;;
  observe)
    require_lima
    bash "${repo_root}/scripts/macbook-runner-observe.sh" "${instance}"
    ;;
  exec)
    shift
    [ "${1:-}" = "--" ] && shift
    [ "$#" -gt 0 ] || die 'exec requires a command after --'
    start_vm
    exec limactl shell "${instance}" -- "$@"
    ;;
  stop)
    require_lima
    require_instance
    limactl stop "${instance}"
    ;;
  help|-h|--help|'')
    usage
    ;;
  *)
    usage >&2
    die "unknown command '${command_name}'"
    ;;
esac
