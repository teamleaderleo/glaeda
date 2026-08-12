#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root/home/.lima/smolrunner" "$test_root/bin" "$test_root/store"

export TEST_CMUX_LOG="$test_root/cmux.log"
export TEST_CMUX_STATE="$test_root/cmux.state"
export TEST_OPEN_LOG="$test_root/open.log"
: >"$TEST_CMUX_LOG"
: >"$TEST_OPEN_LOG"

cat >"$test_root/bin/uname" <<'FAKE_UNAME'
#!/usr/bin/env bash
printf 'Darwin\n'
FAKE_UNAME

cat >"$test_root/bin/open" <<'FAKE_OPEN'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$TEST_OPEN_LOG"
FAKE_OPEN

cat >"$test_root/bin/limactl" <<'FAKE_LIMACTL'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  list)
    state="${FAKE_LIMA_STATE:-Running}"
    case "$state" in
      Running) printf 'Running|8|10GiB\n' ;;
      Stopped) printf 'Stopped|8|10GiB\n' ;;
      *) printf '%s|8|10GiB\n' "$state" ;;
    esac
    ;;
  shell)
    if [[ "$*" == *'/usr/bin/test -x /usr/bin/pgrep'* ]]; then
      exit 0
    fi
    if [[ "$*" == *'/usr/bin/pgrep -f Runner.Worker'* ]]; then
      exit "${FAKE_WORKER_STATUS:-1}"
    fi
    exit 0
    ;;
  start|stop|edit)
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
FAKE_LIMACTL

cat >"$test_root/bin/cargo-smolrunner" <<'FAKE_SMOLRUNNER'
#!/usr/bin/env bash
printf '{"active_count":1,"queued_entry_count":2,"draining_count":0}\n'
FAKE_SMOLRUNNER

cat >"$test_root/bin/cmux" <<'FAKE_CMUX'
#!/usr/bin/env bash
set -euo pipefail
workspace=0
split=0
host_named=0
vm_named=0
if [[ -f "$TEST_CMUX_STATE" ]]; then
  # shellcheck disable=SC1090
  source "$TEST_CMUX_STATE"
fi
save_state() {
  cat >"$TEST_CMUX_STATE" <<EOF_STATE
workspace=$workspace
split=$split
host_named=$host_named
vm_named=$vm_named
EOF_STATE
}
printf '%s\n' "$*" >>"$TEST_CMUX_LOG"
case "${1:-}" in
  ping)
    exit 0
    ;;
  list-workspaces)
    if [[ "$workspace" -eq 1 ]]; then
      printf '{"workspaces":[{"id":"ws-1","ref":"workspace:1","title":"smolrunner"}]}\n'
    else
      printf '{"workspaces":[]}\n'
    fi
    ;;
  new-workspace)
    workspace=1
    split=0
    host_named=0
    vm_named=0
    save_state
    ;;
  current-workspace)
    printf '{"workspace":{"id":"ws-1","ref":"workspace:1","title":"smolrunner"}}\n'
    ;;
  tree)
    host_title=''
    vm_title=''
    [[ "$host_named" -eq 1 ]] && host_title='Mac host'
    [[ "$vm_named" -eq 1 ]] && vm_title='Lima VM'
    if [[ "$split" -eq 1 ]]; then
      printf '{"workspace":{"id":"ws-1","ref":"workspace:1","panes":[{"surfaces":[{"id":"surface-host","ref":"surface:1","title":"%s"},{"id":"surface-vm","ref":"surface:2","title":"%s"}]}]}}\n' "$host_title" "$vm_title"
    else
      printf '{"workspace":{"id":"ws-1","ref":"workspace:1","panes":[{"surfaces":[{"id":"surface-host","ref":"surface:1","title":"%s"}]}]}}\n' "$host_title"
    fi
    ;;
  rename-tab)
    title="${*: -1}"
    case "$title" in
      'Mac host') host_named=1 ;;
      'Lima VM') vm_named=1 ;;
    esac
    save_state
    ;;
  new-split)
    split=1
    save_state
    ;;
  send|set-status|clear-status|select-workspace|log|notify|trigger-flash)
    ;;
  *)
    ;;
esac
FAKE_CMUX

chmod +x \
  "$test_root/bin/uname" \
  "$test_root/bin/open" \
  "$test_root/bin/limactl" \
  "$test_root/bin/cargo-smolrunner" \
  "$test_root/bin/cmux"

export HOME="$test_root/home"
export PATH="$test_root/bin:/usr/bin:/bin"
export SMOLRUNNER_BIN="$test_root/bin/cargo-smolrunner"
export SMOLRUNNER_PERSONAL_WORKER_STORE_ROOT="$test_root/store"

bash -n "$ROOT/scripts/macbook-workspace.sh" "$ROOT/scripts/macbook-ui-state.sh"

bash "$ROOT/scripts/macbook-workspace.sh" cmux

grep -Fq 'new-workspace --name smolrunner --cwd' "$TEST_CMUX_LOG"
grep -Fq 'new-split right --workspace ws-1 --surface surface-host' "$TEST_CMUX_LOG"
grep -Fq 'rename-tab --surface surface-host -- Mac host' "$TEST_CMUX_LOG"
grep -Fq 'rename-tab --surface surface-vm -- Lima VM' "$TEST_CMUX_LOG"
grep -Fq 'send --surface surface-vm exec /bin/bash' "$TEST_CMUX_LOG"
grep -Fq 'set-status lima work --workspace ws-1' "$TEST_CMUX_LOG"
grep -Fq 'set-status worker 1 active --workspace ws-1' "$TEST_CMUX_LOG"
grep -Fq 'set-status queue 2 queued --workspace ws-1' "$TEST_CMUX_LOG"
grep -Fq 'select-workspace --workspace ws-1' "$TEST_CMUX_LOG"

if grep -Fq 'osascript' "$ROOT/scripts/macbook-workspace.sh"; then
  printf 'error: cmux workspace still depends on AppleScript\n' >&2
  exit 1
fi

FAKE_LIMA_STATE=Stopped bash "$ROOT/scripts/macbook-workspace.sh" sync-cmux
grep -Fq 'clear-status lima --workspace ws-1' "$TEST_CMUX_LOG"
grep -Fq 'set-status lima stopped --workspace ws-1' "$TEST_CMUX_LOG"

ui_state="$(FAKE_WORKER_STATUS=0 bash "$ROOT/scripts/macbook-ui-state.sh")"
printf '%s\n' "$ui_state" | python3 -c '
import json, sys
value = json.load(sys.stdin)
assert value == {
    "schema_version": 1,
    "state": "running",
    "profile": "work",
    "actions_worker": "active",
    "operator_run_active": False,
}
'

before_auto="$(wc -l <"$TEST_CMUX_LOG")"
bash "$ROOT/scripts/macbook-workspace.sh" auto
after_auto="$(wc -l <"$TEST_CMUX_LOG")"
[[ "$after_auto" -gt "$before_auto" ]]
grep -Fq 'select-workspace --workspace ws-1' "$TEST_CMUX_LOG"

CMUX_WORKSPACE_ID=ws-1 bash "$ROOT/scripts/macbook-workspace.sh" notify-doctor 0
grep -Fq 'notify --title SmolRunner doctor --body Doctor completed successfully.' "$TEST_CMUX_LOG"

printf 'smolrunner: cmux workspace integration checks passed\n'
