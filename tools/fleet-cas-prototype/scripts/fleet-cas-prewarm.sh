#!/bin/bash
# Keep this reader's node warm between builds: copy the writer's newest fill into the node
# store, so a catch-up build's own warm (fleet-cas-warm.sh) finds nearly everything local.
# Runs from a LaunchAgent every 5 minutes (glaeda-fleet-cas node --prewarm REPO).
#
# usage: fleet-cas-prewarm.sh REPO
# The writer names its newest fill in the signed marker REPO/latest/<Xcode build>. Nearby
# commits share almost all keys, so a build a few commits away fetches only the difference.
# Skips while a build holds the host lock (never takes it), and once a commit is warmed. At
# most daily it prunes the node store to what builds and warms used in the last 3 days.
set -u
repo=${1:?usage: fleet-cas-prewarm.sh REPO}
ROOT=${FLEET_CAS_ROOT:-/Users/Shared/cmux-build-fleet/xcode}
LOCK=${FLEET_HOST_LOCK:-/Users/Shared/cmux-build-fleet/host.lock}
env_get() { sed -n "s/^$1=//p" "$ROOT/fleet-cas.env" 2>/dev/null | tail -1; }
store=$(env_get FLEET_CAS_STORE) keys=$(env_get FLEET_CAS_TRUSTED_KEYS)
[ -n "$store" ] && [ -n "$keys" ] || { echo "$ROOT/fleet-cas.env has no store or trusted keys" >&2; exit 2; }
say() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*"; }
# A build holds the lock exclusively; a shared, non-blocking probe dropped at once says whether
# one is running without ever blocking it.
if [ -e "$LOCK" ] && ! perl -MFcntl=:flock -e 'open(my $f, "<", shift) or exit 0; exit(flock($f, LOCK_SH | LOCK_NB) ? 0 : 1)' "$LOCK"; then
  exit 0
fi
xcode=$(xcodebuild -version 2>/dev/null | awk '/Build version/ {print $3}')
[ -n "$xcode" ] || { say "no Xcode build version"; exit 2; }
latest=$("$ROOT/bin/fleet-cas" marker get "http://$store" "$repo/latest/$xcode" --trusted-keys "$keys" 2>/dev/null)
rc=$?
[ $rc -eq 1 ] && exit 0  # no fill for this Xcode yet
[ $rc -eq 0 ] || { say "latest marker for $repo/$xcode did not verify or the store is unreachable ($rc)"; exit 2; }
commit=$(printf '%s\n' "$latest" | sed -n 's/^commit=//p')
case $commit in *[!0-9a-f]* | "") say "latest marker names no commit"; exit 2 ;; esac
mkdir -p "$ROOT/run"
state=$ROOT/run/prewarm-$(printf '%s' "$repo" | tr -c 'A-Za-z0-9.@-' _)-$xcode
if [ "$(cat "$state" 2>/dev/null)" != "$commit" ]; then
  out=$(nice -n 10 "$ROOT/bin/fleet-cas" warm "http://$store" "$repo/$commit/$xcode" \
    --trusted-keys "$keys" --store "$ROOT/node-store" --timeout 900 2>&1)
  rc=$?
  say "$out"
  [ $rc -eq 0 ] && echo "$commit" >"$state"
fi
# Prune the node store at most daily (a few seconds). gc keeps entries used within 3 days
# (local hits and warms record use) and every object they reach.
gcstamp=$ROOT/run/prewarm.gc
if [ -z "$(find "$gcstamp" -mtime -1 2>/dev/null)" ]; then
  touch "$gcstamp"
  say "$(nice -n 10 "$ROOT/bin/fleet-cas" gc "$ROOT/node-store" --keep-days 3 2>&1)"
fi
