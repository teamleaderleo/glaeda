#!/bin/bash
# Read-only capacity probe for one cmux fleet member, run over SSH by scripts/glaeda-route.
#
#   glaeda_route_probe.sh UNITS [CHECK FLEET_CLASS TOOLCHAIN_XCODE]
#
# Emits "key<TAB>value" lines and ends with "probe<TAB>done", so a cut-off probe is never read as a
# complete one. Runs as the SSH login user (the runner user), with no privileged command, and writes
# nothing: every lock test opens an existing file read-only (never O_CREAT), takes a non-blocking
# SHARED flock and drops it at once. The capacity ledger (glaeda-cmux-runner-hook take_capacity)
# holds units and tokens EXCLUSIVELY, so a shared test fails exactly when a job holds one. A job
# admission that lands in the microseconds a test holds its shared lock sees that unit as taken and
# takes the next one; a token it refuses, and cmux's rescue re-runs the refused job.
#
# With CHECK=1 it also runs the runner hook's read-only eligibility gate (`check`: no lock, no rustup
# change), with the runner's own PATH from its LaunchAgent, bounded by a 45 s alarm.
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
units=${1:-0}
check=${2:-0}
fleet_class=${3:-}
toolchain_xcode=${4:-}
F=${GLAEDA_FLEET_DIR:-/Users/Shared/cmux-build-fleet}
C="$F/capacity"
e() { printf '%s\t%s\n' "$1" "$2"; }
case "$units" in ''|*[!0-9]*) units=0;; esac
e units_total "$units"

# One perl process tests every lock: "free" (no exclusive holder, or no file yet: the hook creates
# a unit or token file the first time it takes it), "held", or "unknown".
files=()
i=0
while [ "$i" -lt "$units" ]; do files+=("$C/unit-$i"); i=$((i + 1)); done
files+=("$C/persistent-dd.token" "$C/gui.token" "$F/host.lock")
/usr/bin/perl -MFcntl=:DEFAULT,:flock -e '
  for my $p (@ARGV) {
    my $f;
    if (-l $p) { print "$p\tunknown\n"; next }
    if (!-e $p) { print "$p\tfree\n"; next }
    if (!sysopen($f, $p, O_RDONLY | O_NOFOLLOW | O_NONBLOCK) || !-f $f) { print "$p\tunknown\n"; next }
    if (flock($f, LOCK_SH | LOCK_NB)) { flock($f, LOCK_UN); print "$p\tfree\n" }
    else { print "$p\t", ($!{EWOULDBLOCK} ? "held" : "unknown"), "\n" }
    close($f);
  }' "${files[@]}" </dev/null 2>/dev/null | while IFS="$(printf '\t')" read -r path state; do
  case "$path" in
    "$C"/unit-*) e unit "${path##*/unit-}|$state" ;;
    "$C/persistent-dd.token") e token "persistent-dd|$state" ;;
    "$C/gui.token") e token "gui|$state" ;;
    "$F/host.lock") e host_lock_exclusive "$state" ;;
  esac
done

# Reservation marker (glaeda-mini-fleet reserve), shipped raw and bounded; glaeda_reservation.py
# parses it on the publisher, as the runner hook does here.
R="$F/reservation.json"
if [ -L "$R" ] || { [ -e "$R" ] && { [ ! -f "$R" ] || [ ! -r "$R" ]; }; }; then
  e reservation invalid
elif [ -f "$R" ]; then
  e reservation_raw "$(head -c 4097 "$R" | base64 | tr -d '\n')"
fi

if [ "$check" = 1 ]; then
  wrapper="$HOME/actions-runner-glaeda/glaeda-hooks/job-started.sh"
  plist="$HOME/Library/LaunchAgents/com.teamleaderleo.glaeda.cmux-runner.plist"
  line=$(grep -m1 '^exec ' "$wrapper" 2>/dev/null)
  # glaeda-cmux-runner writes "exec PYTHON HOOK job-started ...". Only plain words are taken; a quoted
  # path is not parsed (no eval), and the member then reads as not eligible.
  set -- $line
  py=${2:-}
  hook=${3:-}
  job_path=$(plutil -extract EnvironmentVariables.PATH raw "$plist" 2>/dev/null)
  if [ -z "$py" ] || [ -z "$hook" ] || [ ! -x "$py" ] || [ ! -f "$hook" ] || [ -z "$job_path" ] \
     || [ -z "$fleet_class" ] || case "$py$hook" in *"'"*|*'"'*) true;; *) false;; esac; then
    e eligible "no|cannot find this member's runner hook or its PATH"
  else
    args=(check --fleet-class "$fleet_class")
    [ -n "$toolchain_xcode" ] && args+=(--toolchain-xcode "$toolchain_xcode")
    out=$(cd "$HOME" && PATH="$job_path" /usr/bin/perl -e 'alarm 45; exec @ARGV' "$py" "$hook" "${args[@]}" \
          </dev/null 2>/dev/null | tail -1)
    case "$out" in
      *": eligible"*) e eligible "yes|" ;;
      *"refused: "*) e eligible "no|${out#*refused: }" ;;
      *) e eligible "no|the eligibility check gave no answer" ;;
    esac
  fi
fi
e probe done
