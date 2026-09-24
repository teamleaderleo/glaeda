#!/bin/bash
# Read-only preflight probe for one cmux build mini, run after scripts/cmux_mini_probe.sh in the same
# SSH session. glaeda-mini-fleet prepends the inputs as shell assignments:
#   CMUX_ROOT       the cmux checkout (a leading ~ means the login user's home)
#   XCODE_PIN       the pinned Xcode app, or empty
#   WORKLOAD_PATH   cmux_fleet_bootstrap.CMUX_WORKLOAD_TOOL_PATH
#   WORKLOAD_TOOLS  cmux_fleet_bootstrap.MACOS_WORKLOAD_TOOLS, space separated
#   PYTHONS         glaeda-mini-enroll's interpreter candidates, space separated, ~ for home
#   CANDIDATE_FALLBACK  the candidate source commit the operator's cmux checkout names, or empty
# Emits "key<TAB>value" lines like the probe. Installs, downloads and writes nothing: rustup runs with
# RUSTUP_AUTO_INSTALL=0 and git with GIT_OPTIONAL_LOCKS=0. Never prints tokens or runner URLs.
# The group is parsed whole before it runs, so its </dev/null cannot eat the rest of this script.
{
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
e() { printf '%s\t%s\n' "$1" "$2"; }
first() { head -1 | cut -c1-160; }
last() { grep -v '^[[:space:]]*$' | tail -1 | cut -c1-200; }
CMUX_ROOT="${CMUX_ROOT/#\~/$HOME}"
export RUSTUP_AUTO_INSTALL=0 GIT_OPTIONAL_LOCKS=0

# Xcode: the pinned app's own tools, as CI selects it through DEVELOPER_DIR.
dev=""
[ -n "$XCODE_PIN" ] && [ -d "$XCODE_PIN/Contents/Developer" ] && dev="$XCODE_PIN/Contents/Developer"
if [ -n "$dev" ]; then
  out=$(DEVELOPER_DIR="$dev" xcodebuild -showsdks 2>&1)
  if printf '%s' "$out" | grep -qE 'DVTPlugIn|runFirstLaunch|DVTDownloads|PlugInLoading'; then
    e pf_sdks "plugin_error|$(printf '%s\n' "$out" | grep -E 'DVTPlugIn|runFirstLaunch|DVTDownloads|PlugInLoading' | first)"
  elif printf '%s' "$out" | grep -q 'macosx'; then
    e pf_sdks ok
  else
    e pf_sdks "fail|$(printf '%s\n' "$out" | last)"
  fi
  if out=$(DEVELOPER_DIR="$dev" xcrun metal --version 2>&1); then
    e pf_metal "ok|$(printf '%s\n' "$out" | first)"
  else
    e pf_metal "fail|$(printf '%s\n' "$out" | last)"
  fi
fi

# Build tools exactly where the workload looks: the fixed PATH, from the checkout (rustup reads
# rust-toolchain.toml from the working directory).
cd "$CMUX_ROOT" 2>/dev/null || cd "$HOME"
# The probe above ran the Xcode shims already; a hashed /usr/bin path would beat WORKLOAD_PATH.
hash -r
# Without a developer directory the /usr/bin git, xcodebuild and xcrun shims open an install dialog.
xcode-select -p >/dev/null 2>&1 && shims=run || shims=skip
for t in $WORKLOAD_TOOLS; do
  p=$(PATH="$WORKLOAD_PATH" command -v "$t" 2>/dev/null)
  v=""
  case "$p" in /usr/bin/*) [ "$shims" = run ] || v="not run: no developer directory selected" ;; esac
  if [ -n "$p" ] && [ -z "$v" ]; then
    case "$t" in
      zig) v=$("$p" version 2>&1 | first) ;;
      xcodebuild) v=$("$p" -version 2>&1 | first) ;;
      *) v=$(PATH="$WORKLOAD_PATH" "$p" --version 2>&1 | grep -v '^info:' | first) ;;
    esac
  fi
  e pf_tool "$t|$p|$v"
done
rustup=$(PATH="$WORKLOAD_PATH" command -v rustup 2>/dev/null)
channel=$(sed -nE 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$CMUX_ROOT/Native/DiffSidecar/rust-toolchain.toml" 2>/dev/null | head -1)
if [ -n "$channel" ] && [ -n "$rustup" ]; then
  out=$(PATH="$WORKLOAD_PATH" "$rustup" run "$channel" rustc --version 2>&1)
  cargo=$(PATH="$WORKLOAD_PATH" "$rustup" run "$channel" cargo --version 2>&1)
  e pf_rust_channel "$channel|$(printf '%s\n' "$out" | grep -v '^help:' | last)|$(printf '%s\n' "$cargo" | grep -v '^help:' | last)"
elif [ -n "$channel" ]; then
  e pf_rust_channel "$channel|"
fi
# The toolchain plain cargo and rustc resolve to outside the checkout (the manifest's rustup_default).
[ -n "$rustup" ] && e pf_rust_default "$(cd "$HOME" && PATH="$WORKLOAD_PATH" "$rustup" default 2>&1 | grep -v '^info:' | first)"

# glaeda-mini-enroll's interpreter search, in its order.
py=""
for c in $PYTHONS; do
  c="${c/#\~/$HOME}"
  [ -x "$c" ] || continue
  v=$("$c" -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null) || continue
  case "$v" in 3.1[3-9]|3.[2-9][0-9]|[4-9].*) py="$c|$v"; break ;; esac
  [ -n "$py" ] || py="|$v"
done
e pf_python "$py"

# The cmux checkout: submodules, setup artifacts, local changes, and the candidate it names.
if [ -e "$CMUX_ROOT/.git" ]; then
  e pf_cmux present
  e pf_cmux_pin "$(cat "$CMUX_ROOT/.xcode-version" 2>/dev/null)"
  git -C "$CMUX_ROOT" submodule status --recursive 2>&1 | while IFS= read -r line; do e pf_submodule "$line"; done
  e pf_zig_min "$(sed -nE 's/^[[:space:]]*\.minimum_zig_version[[:space:]]*=[[:space:]]*"([0-9.]+)".*/\1/p' "$CMUX_ROOT/ghostty/build.zig.zon" 2>/dev/null | head -1)"
  artifacts=no
  if [ -f "$CMUX_ROOT/ghostty/include/ghostty.h" ]; then
    for k in "$CMUX_ROOT/GhosttyKit.xcframework" "$CMUX_ROOT/ghostty/macos/GhosttyKit.xcframework"; do
      [ -d "$k" ] && artifacts=yes
    done
  fi
  e pf_setup_artifacts "$artifacts"
  e pf_cmux_dirty "$(git -C "$CMUX_ROOT" status --porcelain=v1 --untracked-files=all 2>/dev/null | wc -l | tr -d ' ')"
  source=$(sed -nE 's/^CANDIDATE_SOURCE = "([0-9a-f]{40})".*/\1/p' "$CMUX_ROOT/scripts/ci/persistent_compile_fleet.py" 2>/dev/null | head -1)
else
  e pf_cmux missing
fi
source="${source:-$CANDIDATE_FALLBACK}"
if [ -n "$source" ]; then
  short=${source:0:12}
  staged=no; [ -f "$HOME/Projects/glaeda-generations/$short/stage-receipt.json" ] && staged=yes
  archive=no; [ -f "$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$short/glaeda-$source-aarch64-apple-darwin.tar.gz" ] && archive=yes
  e pf_candidate "$short|$staged|$archive"
fi
[ -f "$HOME/glaeda/scripts/cmux_fleet.py" ] && e pf_glaeda present || e pf_glaeda missing
[ -d "$HOME/.cache/glaeda/cmux-native-cache" ] && e pf_cache_root present || e pf_cache_root missing

# Enrollment progress (glaeda-mini-enroll); the node id itself comes from the probe.
fleet="${XDG_CONFIG_HOME:-$HOME/.config}/glaeda/cmux-fleet"
[ -f "$fleet/enrollment.json" ] && e pf_enroll_state "$(plutil -extract state raw "$fleet/enrollment.json" 2>/dev/null)"
[ -f "$fleet/acceptance/cmux_macos_native_build.json" ] && \
  e pf_acceptance "$(plutil -extract result raw "$fleet/acceptance/cmux_macos_native_build.json" 2>/dev/null)"

# Homebrew's owner decides whether an install needs sudo -u <owner>. Only a runnable brew counts: an
# installer that stops early leaves an empty prefix (bin, Cellar) behind, and that is still no Homebrew.
brew_prefix="${GLAEDA_PREFLIGHT_BREW_PREFIX:-/opt/homebrew}"  # override only for tests
[ -x "$brew_prefix/bin/brew" ] && e pf_brew_owner "$(stat -f %Su "$brew_prefix/bin/brew")"

# A running or prepared macOS update means a restart is coming.
# Only an install or download; softwareupdate -l or --history from a monitoring job is not an update.
applying='(^|[ /])softwareupdate( .*)? (-i|-ia|-ir|-d|-a|--install|--download|--all|--restart|-R)( |$)'
pgrep -f "$applying" >/dev/null && \
  e pf_update_running "$(pgrep -fl "$applying" | grep -v '^[0-9]* sudo ' | head -1 | cut -d' ' -f2- | cut -c1-120)"
# An update prepared for this build since the last boot is waiting for a restart. One prepared before
# the last boot survived a restart without applying (suspended), which does not block.
booted=$(sysctl -n kern.boottime | sed -E 's/^[{] sec = ([0-9]+).*/\1/')
for f in /System/Volumes/Update/Preflight.plist /System/Volumes/Update/Update.plist; do
  [ -f "$f" ] || continue
  from=$(plutil -extract update-asset-attributes.PrerequisiteBuild raw "$f" 2>/dev/null || plutil -extract BootedOSVersion raw "$f" 2>/dev/null)
  [ -n "$from" ] && [ "$from" = "$(sw_vers -buildVersion)" ] || continue
  to=$(plutil -extract update-asset-attributes.OSVersion raw "$f" 2>/dev/null)
  if [ "$(stat -f %m "$f")" -ge "${booted:-0}" ]; then state=pending; else state=suspended; fi
  e pf_update_prepared "$state|${to:-unknown}"
  break
done
pmset -g custom 2>/dev/null | while IFS= read -r line; do e pf_pmset "$line"; done

# Runners registered for this user: the directory and agent name only.
for r in "$HOME"/actions-runner*/.runner; do
  [ -f "$r" ] || continue
  e pf_runner "$(basename "$(dirname "$r")")|$(grep -o '"agentName": *"[^"]*"' "$r" | sed -E 's/.*"([^"]*)"$/\1/')"
done
} </dev/null
