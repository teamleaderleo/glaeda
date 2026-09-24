#!/bin/bash
# Fresh-reader measurements: each run starts an empty, read-only node daemon
# whose only warm source is the fleet store, then builds in a fresh
# DerivedData. A last run keeps the node's store to measure a warm node.
#
# usage: PKG=... SCHEME=... WORK=... DD=... reader-runs.sh <upstream-url> <runs> [label-prefix]
set -u
: "${PKG:?}" "${SCHEME:?}" "${WORK:?}" "${DD:?}"
up=$1; runs=$2; prefix=${3:-fresh}
here=$(cd "$(dirname "$0")" && pwd)
bin="$here/../target/release/fleet-cas"
sock="$HOME/.cache/fleet-cas/nr.sock"
store="$WORK/node-r"
np=

stop_node() { [ -n "$np" ] && kill -INT "$np" && wait "$np" 2>/dev/null; np=; }
trap stop_node EXIT

start_node() {
  stop_node
  [ "${1:-}" = keep ] || rm -rf "$store"
  rm -f "$sock"
  "$bin" "$sock" "$store" --read-only-kv --upstream "$up" >"$WORK/node-r.log" 2>&1 &
  np=$!
  for _ in 1 2 3 4 5 6 7 8 9 10; do [ -S "$sock" ] && return; sleep 0.5; done
  cat "$WORK/node-r.log"; exit 1
}

run() {
  "$here/with-fleet-lock.pl" "$here/xcode-cache-build.sh" "$1" \
    COMPILATION_CACHE_ENABLE_PLUGIN=YES COMPILATION_CACHE_REMOTE_SERVICE_PATH="$sock"
  sleep 1
  echo "$1 node: $(cat "$store/stats.json")"
  echo "$1 xcode: $(grep -oE '[0-9]+ hits / [0-9]+ cacheable' "$WORK/log-$1.txt")"
}

for i in $(seq 1 "$runs"); do start_node; run "$prefix$i"; done
start_node keep; run "${prefix}-warmnode"
