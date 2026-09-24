#!/bin/bash
# One measured Xcode build with compilation caching on.
#
# usage: PKG=<package or project dir> SCHEME=<scheme> WORK=<scratch dir> \
#          [PROJECT=<x.xcodeproj>] [SPM=<cloned packages dir>] [DD=<path>] \
#          xcode-cache-build.sh <label> [extra build settings...]
#
# SPM builds against packages resolved once into that directory, so every
# run (and machine) sees the same package checkouts at the same path.
#
# Each label gets its own DerivedData and local CAS under $WORK, so a second
# label is a "fresh machine" whose only warm source is whatever the extra
# settings point at (a shared local CAS path or a remote service socket).
# Prefix mapping keeps cache keys independent of the DerivedData path.
#
# With DD=<path>, every label builds in that one DerivedData path (emptied
# first), which must then be the same absolute path on every machine. Keys
# match without mapping DerivedData, and the build avoids a Swift 6.2
# (Xcode 26.3) compiler crash that the DerivedData mapping triggers.
set -u
: "${PKG:?}" "${SCHEME:?}" "${WORK:?}"
label=$1; shift
if [ -n "${DD:-}" ]; then
  dd=$DD
  rm -rf "$dd"
  map_derived=()
else
  dd="$WORK/dd-$label"
  map_derived=(SWIFT_OTHER_PREFIX_MAPPINGS="$dd=/^derived" CLANG_OTHER_PREFIX_MAPPINGS="$dd=/^derived")
fi
log="$WORK/log-$label.txt"
cd "$PKG" || exit 2
where=()
[ -n "${PROJECT:-}" ] && where+=(-project "$PROJECT")
[ -n "${SPM:-}" ] && where+=(-clonedSourcePackagesDirPath "$SPM" -disableAutomaticPackageResolution)
start=$(date +%s.%N)
xcodebuild build ${where[@]+"${where[@]}"} -scheme "$SCHEME" -configuration Debug \
  -destination 'platform=macOS' \
  -derivedDataPath "$dd" \
  COMPILATION_CACHE_ENABLE_CACHING=YES \
  COMPILATION_CACHE_CAS_PATH="$WORK/localcas-$label" \
  COMPILATION_CACHE_ENABLE_DIAGNOSTIC_REMARKS=YES \
  SWIFT_ENABLE_PREFIX_MAPPING=YES CLANG_ENABLE_PREFIX_MAPPING=YES \
  SWIFT_ENABLE_PROJECT_PREFIX_MAPPING=YES CLANG_ENABLE_PROJECT_PREFIX_MAPPING=YES \
  ${map_derived[@]+"${map_derived[@]}"} "$@" >"$log" 2>&1
rc=$?
end=$(date +%s.%N)
printf '%s rc=%s %.1fs hits=%s misses=%s\n' "$label" "$rc" "$(echo "$end - $start" | bc)" \
  "$(grep -c 'cache hit' "$log")" "$(grep -c 'cache miss' "$log")"
grep -q bad_optional_access "$log" && echo "$label: compiler crashed (bad_optional_access)"
