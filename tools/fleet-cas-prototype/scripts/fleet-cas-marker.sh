#!/bin/bash
# Does the fleet store hold COMMIT for this host's Xcode? Checks the signed
# per-commit marker the trusted writer publishes after a complete fill.
#
# usage: fleet-cas-marker.sh REPO COMMIT
# exit 0: held (marker entries printed), 1: not held, 2: unverified or error.
# Pick catch-up mode only on 0 (docs/CMUX_BUILD_SLOT_MODES.md, "When to switch").
# A controller-allowlisted canary may set FLEET_CAS_STORE_OVERRIDE to a validated
# read endpoint. It never changes the trusted-key source or the local node socket.
set -u
repo=${1:?usage: fleet-cas-marker.sh REPO COMMIT}
commit=${2:?usage: fleet-cas-marker.sh REPO COMMIT}
ROOT=${FLEET_CAS_ROOT:-/Users/Shared/cmux-build-fleet/xcode}
env_get() { sed -n "s/^$1=//p" "$ROOT/fleet-cas.env" 2>/dev/null | tail -1; }
store=${FLEET_CAS_STORE_OVERRIDE:-$(env_get FLEET_CAS_STORE)}
if [ -n "${FLEET_CAS_STORE_OVERRIDE:-}" ]; then
  case "$store" in
    *[!A-Za-z0-9._:-]*|*:*:*) echo "invalid FLEET_CAS_STORE_OVERRIDE endpoint" >&2; exit 2 ;;
    *:*) port=${store##*:}; case "$port" in ''|*[!0-9]*) echo "invalid FLEET_CAS_STORE_OVERRIDE port" >&2; exit 2 ;; esac ;;
    *) echo "invalid FLEET_CAS_STORE_OVERRIDE endpoint" >&2; exit 2 ;;
  esac
fi
keys=$(env_get FLEET_CAS_TRUSTED_KEYS)
[ -n "$store" ] && [ -n "$keys" ] || { echo "$ROOT/fleet-cas.env has no store or trusted keys" >&2; exit 2; }
xcode=$(xcodebuild -version 2>/dev/null | awk '/Build version/ {print $3}')
[ -n "$xcode" ] || { echo "no Xcode build version" >&2; exit 2; }
exec "$ROOT/bin/fleet-cas" marker get "http://$store" "$repo/$commit/$xcode" --trusted-keys "$keys"
