#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "${script_dir}/.." && pwd)"
session="${SMOLRUNNER_WORK_SESSION:-smolrunner}"
vm_helper="${repo_root}/scripts/macbook-runner-vm.sh"
command_name="${1:-auto}"

usage() {
  cat <<'USAGE'
Usage: bash scripts/macbook-workspace.sh COMMAND

Commands:
  auto        Open cmux when its CLI is available, otherwise use tmux.
  cmux        Open or select the cmux Mac/Lima workspace.
  setup-cmux  Install cmux through its reviewed Homebrew cask, then open the workspace.
  tmux        Open or select the compatibility tmux workspace.
  sync-cmux   Refresh cmux sidebar metadata from current SmolRunner/Lima observations.
  notify-doctor STATUS
              Emit a fixed cmux notification for a completed doctor run.

Environment:
  SMOLRUNNER_WORK_SESSION               Workspace/session name (default: smolrunner)
  SMOLRUNNER_PERSONAL_WORKER_STORE_ROOT Optional personal-worker store for queue/activity badges
  SMOLRUNNER_BIN                        Optional built SmolRunner CLI used for worker reads
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

validate_session() {
  case "${session}" in
    ''|*[!A-Za-z0-9_.-]*)
      die 'SMOLRUNNER_WORK_SESSION must contain only letters, digits, dot, underscore, or dash'
      ;;
  esac
}

require_macos() {
  [ "$(uname -s)" = "Darwin" ] || die 'the MacBook workspace helper supports macOS only'
}

ensure_brew() {
  if command -v brew >/dev/null 2>&1; then
    return
  fi
  for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [ -x "${candidate}" ]; then
      PATH="$(dirname "${candidate}"):${PATH}"
      export PATH
      return
    fi
  done
  die 'Homebrew is required before running the MacBook workspace helper'
}

ensure_lima() {
  if command -v limactl >/dev/null 2>&1; then
    return
  fi
  ensure_brew
  printf 'Installing Lima with Homebrew.\n'
  brew install lima
}

find_cmux_cli() {
  local candidate
  if candidate="$(command -v cmux 2>/dev/null)" && [ -x "${candidate}" ]; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  for candidate in \
    /Applications/cmux.app/Contents/Resources/bin/cmux \
    "${HOME}/Applications/cmux.app/Contents/Resources/bin/cmux" \
    /opt/homebrew/bin/cmux \
    /usr/local/bin/cmux; do
    if [ -x "${candidate}" ]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

install_cmux() {
  if find_cmux_cli >/dev/null; then
    return
  fi

  ensure_brew
  printf 'Installing cmux through the manaflow-ai/cmux Homebrew cask.\n'
  brew tap manaflow-ai/cmux
  brew install --cask cmux
  find_cmux_cli >/dev/null || die 'cmux was installed but its CLI could not be found'
}

ensure_cmux_ready() {
  local cmux_cli="$1"
  local attempt

  if "${cmux_cli}" ping >/dev/null 2>&1; then
    return 0
  fi
  command -v open >/dev/null 2>&1 || return 1
  open -a cmux >/dev/null 2>&1 || return 1
  for attempt in 1 2 3 4 5 6 7 8 9 10; do
    if "${cmux_cli}" ping >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

require_python3() {
  command -v python3 >/dev/null 2>&1 \
    || die 'python3 is required to read cmux machine output'
}

workspace_id_from_json() {
  local wanted="$1"
  python3 -c '
import json, sys
wanted = sys.argv[1]
data = json.load(sys.stdin)

def walk(value):
    if isinstance(value, dict):
        ref = value.get("ref")
        if isinstance(ref, str) and ref.startswith("workspace:"):
            yield value
        for child in value.values():
            yield from walk(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk(child)

for row in walk(data):
    names = [row.get(key) for key in ("custom_title", "title", "name", "display_title")]
    if wanted in names:
        ident = row.get("id") or row.get("ref")
        if ident:
            print(ident)
            raise SystemExit(0)
raise SystemExit(1)
' "${wanted}"
}

current_workspace_id_from_json() {
  python3 -c '
import json, sys
data = json.load(sys.stdin)

def walk(value):
    if isinstance(value, dict):
        ref = value.get("ref")
        if isinstance(ref, str) and ref.startswith("workspace:"):
            ident = value.get("id") or ref
            if ident:
                print(ident)
                raise SystemExit(0)
        for child in value.values():
            walk(child)
    elif isinstance(value, list):
        for child in value:
            walk(child)
walk(data)
raise SystemExit(1)
'
}

surface_id_from_json() {
  local wanted="${1:-}"
  python3 -c '
import json, sys
wanted = sys.argv[1]
data = json.load(sys.stdin)
rows = []

def walk(value):
    if isinstance(value, dict):
        ref = value.get("ref")
        if isinstance(ref, str) and ref.startswith("surface:"):
            rows.append(value)
        for child in value.values():
            walk(child)
    elif isinstance(value, list):
        for child in value:
            walk(child)
walk(data)
if wanted:
    for row in rows:
        names = [row.get(key) for key in ("custom_title", "title", "name", "display_title")]
        if wanted in names:
            print(row.get("id") or row.get("ref"))
            raise SystemExit(0)
    raise SystemExit(1)
for row in rows:
    ident = row.get("id") or row.get("ref")
    if ident:
        print(ident)
'
 "${wanted}"
}

find_workspace_id() {
  local cmux_cli="$1"
  local json
  require_python3
  json="$("${cmux_cli}" list-workspaces --json --id-format both)" || return 1
  printf '%s\n' "${json}" | workspace_id_from_json "${session}"
}

current_workspace_id() {
  local cmux_cli="$1"
  local json
  json="$("${cmux_cli}" current-workspace --json --id-format both)" || return 1
  printf '%s\n' "${json}" | current_workspace_id_from_json
}

workspace_tree_json() {
  local cmux_cli="$1"
  local workspace_id="$2"
  "${cmux_cli}" tree --workspace "${workspace_id}" --json --id-format both
}

first_surface_id() {
  local cmux_cli="$1"
  local workspace_id="$2"
  workspace_tree_json "${cmux_cli}" "${workspace_id}" \
    | surface_id_from_json \
    | sed -n '1p'
}

named_surface_id() {
  local cmux_cli="$1"
  local workspace_id="$2"
  local title="$3"
  workspace_tree_json "${cmux_cli}" "${workspace_id}" \
    | surface_id_from_json "${title}"
}

second_surface_id() {
  local cmux_cli="$1"
  local workspace_id="$2"
  local host_surface="$3"
  local surface
  while IFS= read -r surface; do
    if [ -n "${surface}" ] && [ "${surface}" != "${host_surface}" ]; then
      printf '%s\n' "${surface}"
      return 0
    fi
  done < <(workspace_tree_json "${cmux_cli}" "${workspace_id}" | surface_id_from_json)
  return 1
}

shell_quote() {
  local value="$1"
  printf "'%s'" "${value//\'/\'\\\'\'}"
}

ensure_standard_surfaces() {
  local cmux_cli="$1"
  local workspace_id="$2"
  local host_surface vm_surface vm_command

  host_surface="$(named_surface_id "${cmux_cli}" "${workspace_id}" 'Mac host' 2>/dev/null || true)"
  if [ -z "${host_surface}" ]; then
    host_surface="$(first_surface_id "${cmux_cli}" "${workspace_id}")"
    [ -n "${host_surface}" ] || die 'cmux workspace has no terminal surface'
    "${cmux_cli}" rename-tab --surface "${host_surface}" -- 'Mac host' >/dev/null
  fi

  vm_surface="$(named_surface_id "${cmux_cli}" "${workspace_id}" 'Lima VM' 2>/dev/null || true)"
  if [ -z "${vm_surface}" ]; then
    vm_surface="$(second_surface_id "${cmux_cli}" "${workspace_id}" "${host_surface}" 2>/dev/null || true)"
  fi
  if [ -z "${vm_surface}" ]; then
    "${cmux_cli}" new-split right --workspace "${workspace_id}" --surface "${host_surface}" >/dev/null
    vm_surface="$(second_surface_id "${cmux_cli}" "${workspace_id}" "${host_surface}")"
    [ -n "${vm_surface}" ] || die 'cmux did not expose the new Lima surface'
    "${cmux_cli}" rename-tab --surface "${vm_surface}" -- 'Lima VM' >/dev/null
    vm_command="exec /bin/bash $(shell_quote "${vm_helper}") shell"
    "${cmux_cli}" send --surface "${vm_surface}" "${vm_command}\n" >/dev/null
  elif ! named_surface_id "${cmux_cli}" "${workspace_id}" 'Lima VM' >/dev/null 2>&1; then
    "${cmux_cli}" rename-tab --surface "${vm_surface}" -- 'Lima VM' >/dev/null
  fi
}

find_worker_read_binary() {
  local candidate
  if [ -n "${SMOLRUNNER_BIN:-}" ] && [ -x "${SMOLRUNNER_BIN}" ]; then
    printf '%s\n' "${SMOLRUNNER_BIN}"
    return 0
  fi
  for candidate in \
    "${repo_root}/target/release/smolrunner" \
    "${repo_root}/target/debug/smolrunner"; do
    if [ -x "${candidate}" ]; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done
  return 1
}

worker_counts() {
  local store_root="${SMOLRUNNER_PERSONAL_WORKER_STORE_ROOT:-}"
  local binary json
  [ -n "${store_root}" ] || return 1
  binary="$(find_worker_read_binary)" || return 1
  json="$("${binary}" --output json worker status --store-root "${store_root}" 2>/dev/null)" || return 1
  printf '%s\n' "${json}" | python3 -c '
import json, sys
value = json.load(sys.stdin)
print(f"{int(value.get(chr(97)+chr(99)+chr(116)+chr(105)+chr(118)+chr(101)+chr(95)+chr(99)+chr(111)+chr(117)+chr(110)+chr(116), 0))}|{int(value.get(chr(113)+chr(117)+chr(101)+chr(117)+chr(101)+chr(100)+chr(95)+chr(101)+chr(110)+chr(116)+chr(114)+chr(121)+chr(95)+chr(99)+chr(111)+chr(117)+chr(110)+chr(116), 0))}|{int(value.get(chr(100)+chr(114)+chr(97)+chr(105)+chr(110)+chr(105)+chr(110)+chr(103)+chr(95)+chr(99)+chr(111)+chr(117)+chr(110)+chr(116), 0))}")
'
}

sync_cmux_status_for_workspace() {
  local cmux_cli="$1"
  local workspace_id="$2"
  local state_json vm_state profile actions_worker counts active queued draining

  state_json="$(bash "${vm_helper}" ui-state)" || return 1
  IFS='|' read -r vm_state profile actions_worker <<EOF_STATE
$(printf '%s\n' "${state_json}" | python3 -c '
import json, sys
value = json.load(sys.stdin)
print("{}|{}|{}".format(value.get("state", "unknown"), value.get("profile", "unknown"), value.get("actions_worker", "unknown")))
')
EOF_STATE

  "${cmux_cli}" clear-status lima --workspace "${workspace_id}" >/dev/null 2>&1 || true
  "${cmux_cli}" clear-status worker --workspace "${workspace_id}" >/dev/null 2>&1 || true
  "${cmux_cli}" clear-status queue --workspace "${workspace_id}" >/dev/null 2>&1 || true

  case "${vm_state}" in
    running)
      case "${profile}" in
        interactive|work) label="${profile}" ;;
        *) label='running' ;;
      esac
      "${cmux_cli}" set-status lima "${label}" --workspace "${workspace_id}" --icon server.rack --color '#30d158' >/dev/null
      ;;
    stopped)
      "${cmux_cli}" set-status lima 'stopped' --workspace "${workspace_id}" --icon server.rack --color '#8e8e93' >/dev/null
      ;;
    *)
      "${cmux_cli}" set-status lima "${vm_state}" --workspace "${workspace_id}" --icon exclamationmark.triangle --color '#ff9f0a' >/dev/null
      ;;
  esac

  if counts="$(worker_counts 2>/dev/null)"; then
    IFS='|' read -r active queued draining <<EOF_COUNTS
${counts}
EOF_COUNTS
    if [ "${active}" -gt 0 ] 2>/dev/null || [ "${draining}" -gt 0 ] 2>/dev/null; then
      "${cmux_cli}" set-status worker "${active} active" --workspace "${workspace_id}" --icon hammer --color '#64d2ff' >/dev/null
    fi
    if [ "${queued}" -gt 0 ] 2>/dev/null; then
      "${cmux_cli}" set-status queue "${queued} queued" --workspace "${workspace_id}" --icon list.bullet --color '#ffd60a' >/dev/null
    fi
  elif [ "${actions_worker}" = active ]; then
    "${cmux_cli}" set-status worker 'runner active' --workspace "${workspace_id}" --icon hammer --color '#64d2ff' >/dev/null
  fi
}

sync_cmux() {
  local cmux_cli workspace_id
  cmux_cli="$(find_cmux_cli)" || return 0
  "${cmux_cli}" ping >/dev/null 2>&1 || return 0
  workspace_id="$(find_workspace_id "${cmux_cli}" 2>/dev/null || true)"
  [ -n "${workspace_id}" ] || return 0
  sync_cmux_status_for_workspace "${cmux_cli}" "${workspace_id}"
}

launch_cmux() {
  local cmux_cli="$1"
  local workspace_id

  ensure_lima
  bash "${vm_helper}" up
  ensure_cmux_ready "${cmux_cli}" || die 'cmux is installed but its control CLI is unavailable'

  workspace_id="$(find_workspace_id "${cmux_cli}" 2>/dev/null || true)"
  if [ -z "${workspace_id}" ]; then
    "${cmux_cli}" new-workspace --name "${session}" --cwd "${repo_root}" >/dev/null
    workspace_id="$(current_workspace_id "${cmux_cli}")"
    [ -n "${workspace_id}" ] || die 'cmux did not report the newly created workspace'
    "${cmux_cli}" log --workspace "${workspace_id}" --level info -- "Mac checkout: ${repo_root}" >/dev/null
    "${cmux_cli}" log --workspace "${workspace_id}" --level info -- 'Pull requests: https://github.com/teamleaderleo/smolrunner/pulls' >/dev/null
  fi

  ensure_standard_surfaces "${cmux_cli}" "${workspace_id}"
  sync_cmux_status_for_workspace "${cmux_cli}" "${workspace_id}"
  "${cmux_cli}" select-workspace --workspace "${workspace_id}" >/dev/null
  command -v open >/dev/null 2>&1 && open -a cmux >/dev/null 2>&1 || true
}

notify_doctor() {
  local status="${1:-}"
  local cmux_cli body

  case "${status}" in
    0) body='Doctor completed successfully.' ;;
    ''|*[!0-9]*) die 'notify-doctor requires a numeric exit status' ;;
    *) body="Doctor failed with exit status ${status}." ;;
  esac

  cmux_cli="$(find_cmux_cli)" || return 0
  "${cmux_cli}" ping >/dev/null 2>&1 || return 0
  if [ -n "${CMUX_WORKSPACE_ID:-}" ]; then
    "${cmux_cli}" notify --title 'SmolRunner doctor' --body "${body}" >/dev/null
  else
    workspace_id="$(find_workspace_id "${cmux_cli}" 2>/dev/null || true)"
    [ -n "${workspace_id}" ] || return 0
    "${cmux_cli}" trigger-flash --workspace "${workspace_id}" >/dev/null 2>&1 || true
  fi
  sync_cmux || true
}

launch_tmux() {
  local packages=()

  ensure_brew
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
}

launch_auto() {
  local cmux_cli
  require_macos
  if cmux_cli="$(find_cmux_cli)" && ensure_cmux_ready "${cmux_cli}"; then
    launch_cmux "${cmux_cli}"
  else
    launch_tmux
  fi
}

validate_session

case "${command_name}" in
  auto)
    launch_auto
    ;;
  cmux)
    require_macos
    cmux_cli="$(find_cmux_cli)" || die 'cmux is unavailable; run make work-cmux-setup once or use make work-tmux'
    launch_cmux "${cmux_cli}"
    ;;
  setup-cmux)
    require_macos
    install_cmux
    cmux_cli="$(find_cmux_cli)"
    launch_cmux "${cmux_cli}"
    ;;
  tmux)
    require_macos
    launch_tmux
    ;;
  sync-cmux)
    sync_cmux
    ;;
  notify-doctor)
    notify_doctor "${2:-}"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    die "unknown workspace command: ${command_name}"
    ;;
esac
