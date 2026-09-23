#!/bin/bash
# One measured Xcode build with compilation caching on.
#
# usage: PKG=<package dir> SCHEME=<scheme> WORK=<scratch dir> \
#          xcode-cache-build.sh <label> [extra build settings...]
#
# Each label gets its own DerivedData and local CAS under $WORK, so a second
# label is a "fresh machine" whose only warm source is whatever the extra
# settings point at (a shared local CAS path or a remote service socket).
# Prefix mapping keeps cache keys independent of the DerivedData path.
set -u
: "${PKG:?}" "${SCHEME:?}" "${WORK:?}"
label=$1; shift
dd="$WORK/dd-$label"
log="$WORK/log-$label.txt"
cd "$PKG" || exit 2
start=$(date +%s.%N)
xcodebuild build -scheme "$SCHEME" -destination 'platform=macOS' \
  -derivedDataPath "$dd" \
  COMPILATION_CACHE_ENABLE_CACHING=YES \
  COMPILATION_CACHE_CAS_PATH="$WORK/localcas-$label" \
  COMPILATION_CACHE_ENABLE_DIAGNOSTIC_REMARKS=YES \
  SWIFT_ENABLE_PREFIX_MAPPING=YES CLANG_ENABLE_PREFIX_MAPPING=YES \
  SWIFT_ENABLE_PROJECT_PREFIX_MAPPING=YES CLANG_ENABLE_PROJECT_PREFIX_MAPPING=YES \
  SWIFT_OTHER_PREFIX_MAPPINGS="$dd=/^derived" CLANG_OTHER_PREFIX_MAPPINGS="$dd=/^derived" \
  "$@" >"$log" 2>&1
rc=$?
end=$(date +%s.%N)
printf '%s rc=%s %.1fs hits=%s misses=%s\n' "$label" "$rc" "$(echo "$end - $start" | bc)" \
  "$(grep -c 'cache hit' "$log")" "$(grep -c 'cache miss' "$log")"
