#!/bin/bash
# Run the trusted writer's build, then publish the per-commit marker only if
# every write reached the fleet store (#1134 M3).
#
# usage: fleet-cas-writer-build.sh REPO COMMIT -- BUILD COMMAND...
#
# Runs on the writer host, whose node daemon signs every index entry. The
# marker `REPO/COMMIT/<Xcode build version>` tells workers the fleet store
# holds that commit (fleet-cas-marker.sh). A build that succeeds while some
# writes failed (store down, refused, backing off) leaves the store partly
# filled: it gets no marker, and the next main build fills the rest.
# Settings come from /Users/Shared/cmux-build-fleet/xcode/fleet-cas.env, which
# `glaeda-fleet-cas writer` installs. The build command must add the plugin
# settings from `fleet-cas-settings.sh`. Exit: the build's code; 3 if the node
# is down (nothing is built); 4 if the build passed but no marker was written.
# The writer node keeps its own copy of what it wrote, so a fleet store that is
# emptied needs the writer's node store emptied too, or later markers can
# claim entries the store lost.
set -u
repo=${1:?usage: fleet-cas-writer-build.sh REPO COMMIT -- CMD...}
commit=${2:?usage: fleet-cas-writer-build.sh REPO COMMIT -- CMD...}
[ "${3:-}" = -- ] || { echo "usage: fleet-cas-writer-build.sh REPO COMMIT -- CMD..." >&2; exit 2; }
shift 3
case $commit in *[!0-9a-f]* | "") echo "COMMIT must be a full hex SHA" >&2; exit 2 ;; esac
[ ${#commit} -eq 40 ] || { echo "COMMIT must be a full 40-character SHA" >&2; exit 2; }
ROOT=${FLEET_CAS_ROOT:-/Users/Shared/cmux-build-fleet/xcode}
env_get() { sed -n "s/^$1=//p" "$ROOT/fleet-cas.env" | tail -1; }
store=$(env_get FLEET_CAS_STORE) key=$(env_get FLEET_CAS_SIGN_KEY)
[ -n "$store" ] && [ -n "$key" ] || { echo "$ROOT/fleet-cas.env has no writer settings" >&2; exit 2; }
stats=$ROOT/node-store/stats.json
# Sum of the named node counters; empty if the stats file is unreadable.
count() {
  perl -e 'open my $f, "<", shift or exit 1; my $s = <$f>; my $n = 0;
    for my $k (@ARGV) { $n += $1 if $s =~ /"$k":(\d+)/ } print $n' "$stats" "$@"
}
# A write may not have reached the fleet store.
failures() { count write_refused up_errors up_skipped; }
# Xcode talked to the node (a build without the plugin settings does not).
activity() { count kv_get_hit kv_get_miss kv_put; }
xcode=$(xcodebuild -version | awk '/Build version/ {print $3}')
[ -n "$xcode" ] || { echo "no Xcode build version" >&2; exit 2; }
"$ROOT/bin/fleet-cas-settings.sh" "$ROOT/fleet-cas.sock" >/dev/null || exit 3
f0=$(failures) a0=$(activity)
[ -n "$f0" ] && [ -n "$a0" ] || { echo "cannot read $stats" >&2; exit 3; }
"$@"
rc=$?
[ "$rc" -eq 0 ] || exit "$rc"
sleep 1  # the node rewrites stats.json every 500 ms
f1=$(failures) a1=$(activity)
if [ -z "$f1" ] || [ "$f1" != "$f0" ]; then
  echo "fleet-cas: failed or skipped fleet-store calls during the build; no marker for $repo/$commit" >&2
  exit 4
fi
if [ "$a1" = "$a0" ]; then
  echo "fleet-cas: the build never used the node (plugin settings missing?); no marker for $repo/$commit" >&2
  exit 4
fi
"$ROOT/bin/fleet-cas" marker put "http://$store" "$repo/$commit/$xcode" --sign-key "$key" \
  --entry "repo=$repo" --entry "commit=$commit" --entry "xcode=$xcode" \
  --entry "time=$(date -u +%Y-%m-%dT%H:%M:%SZ)" || exit 4
echo "fleet-cas: marker $repo/$commit/$xcode"
