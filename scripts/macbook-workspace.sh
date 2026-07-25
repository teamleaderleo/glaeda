#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "${script_dir}/.." && pwd)"
session="${SMOLRUNNER_WORK_SESSION:-smolrunner}"
vm_helper="${repo_root}/scripts/macbook-runner-vm.sh"

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

case "${session}" in
  ''|*[!A-Za-z0-9_.-]*)
    die 'SMOLRUNNER_WORK_SESSION must contain only letters, digits, dot, underscore, or dash'
    ;;
esac

[ "$(uname -s)" = "Darwin" ] || die 'the MacBook workspace helper supports macOS only'

if ! command -v brew >/dev/null 2>&1; then
  for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [ -x "${candidate}" ]; then
      PATH="$(dirname "${candidate}"):${PATH}"
      export PATH
      break
    fi
  done
fi
command -v brew >/dev/null 2>&1 || die 'Homebrew is required before running make work'

packages=()
command -v tmux >/dev/null 2>&1 || packages+=(tmux)
command -v limactl >/dev/null 2>&1 || packages+=(lima)
if [ "${#packages[@]}" -gt 0 ]; then
  printf 'Installing missing Mac tools with Homebrew: %s\n' "${packages[*]}"
  brew install "${packages[@]}"
fi

bash "${vm_helper}" up

if ! tmux has-session -t "${session}" 2>/dev/null; then
  tmux new-session -d -s "${session}" -n host -c "${repo_root}"
fi

if ! tmux list-windows -t "${session}" -F '#{window_name}' | grep -Fqx host; then
  tmux new-window -d -t "${session}:" -n host -c "${repo_root}"
fi

if ! tmux list-windows -t "${session}" -F '#{window_name}' | grep -Fqx vm; then
  tmux new-window -d -t "${session}:" -n vm -c "${repo_root}" \
    'exec bash scripts/macbook-runner-vm.sh shell'
fi

tmux select-window -t "${session}:host"

if [ -n "${TMUX:-}" ]; then
  exec tmux switch-client -t "${session}"
else
  exec tmux attach-session -t "${session}"
fi
