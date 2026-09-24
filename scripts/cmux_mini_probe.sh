#!/bin/bash
# Read-only probe for one cmux build mini. Emits "key<TAB>value" lines; repeated keys form lists.
# Runs as the login user with no privileged command and no Python (a mini without an accepted Xcode licence
# has only stub /usr/bin/python3). Never prints key material, tokens, or log contents.
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
e() { printf '%s\t%s\n' "$1" "$2"; }
e user "$(id -un)"
e uid "$(id -u)"
id -Gn | tr ' ' '\n' | grep -qx admin && e admin_group yes || e admin_group no
# -n -l lists privileges without running anything; it succeeds only when no password is needed.
if sudo -n -l >/dev/null 2>&1; then e sudo nopasswd; else e sudo password; fi
e hostname "$(scutil --get LocalHostName 2>/dev/null)"
e computer_name "$(scutil --get ComputerName 2>/dev/null)"
e model "$(sysctl -n hw.model)"
e chip "$(sysctl -n machdep.cpu.brand_string)"
e cpus "$(sysctl -n hw.ncpu)"
e memory_bytes "$(sysctl -n hw.memsize)"
e macos "$(sw_vers -productVersion)"
e macos_build "$(sw_vers -buildVersion)"
e uptime_boot "$(sysctl -n kern.boottime | sed -E 's/.*sec = ([0-9]+).*/\1/')"
for app in /Applications/Xcode*.app; do
  [ -e "$app" ] || [ -L "$app" ] || continue
  kind=dir; [ -L "$app" ] && kind="symlink:$(readlink "$app")"
  v=$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist" 2>/dev/null)
  b=$(plutil -extract ProductBuildVersion raw "$app/Contents/version.plist" 2>/dev/null)
  e xcode_app "$app|$kind|$v|$b"
  # </dev/null: this probe arrives on stdin (bash -s), so a child that reads stdin would eat it.
  # Ask the app itself: the licence agreement is shared across releases, so the plist's version
  # string is not what xcodebuild checks. Both queries run nothing privileged.
  if [ -x "$app/Contents/Developer/usr/bin/xcodebuild" ]; then
    DEVELOPER_DIR="$app/Contents/Developer" xcodebuild -license check </dev/null >/dev/null 2>&1 && l=accepted || l=needed
    DEVELOPER_DIR="$app/Contents/Developer" xcodebuild -checkFirstLaunchStatus </dev/null >/dev/null 2>&1 && f=done || f=needed
    e xcode_ready "$app|$l|$f"
  fi
done
e xcode_select "$(xcode-select -p 2>/dev/null)"
if [ -e /Library/Preferences/com.apple.dt.Xcode.plist ]; then
  e xcode_license_accepted "$(defaults read /Library/Preferences/com.apple.dt.Xcode IDEXcodeVersionForAgreedToGMLicense 2>/dev/null)"
fi
df -k / /System/Volumes/Data 2>/dev/null | awk 'NR>1{printf "disk\t%s|%d|%d\n",$NF,$2/1048576,$4/1048576}'
for d in "$HOME/Library/LaunchAgents" /Library/LaunchAgents /Library/LaunchDaemons; do
  for p in "$d"/*.plist; do
    [ -e "$p" ] || continue
    # Some fleet plists are root-only readable; fall back to the file name, which matches the label.
    label=$(plutil -extract Label raw "$p" 2>/dev/null) || label=$(basename "$p" .plist)
    case "$label" in com.apple.*) continue;; esac
    state=unknown
    if [ "$d" = /Library/LaunchDaemons ]; then
      st=$(launchctl print "system/$label" 2>/dev/null)
    else
      st=$(launchctl print "gui/$(id -u)/$label" 2>/dev/null)
    fi
    if [ -n "$st" ]; then
      state=$(printf '%s\n' "$st" | awk -F' = ' '/^\tstate = /{print $2; exit}')
      pid=$(printf '%s\n' "$st" | awk -F' = ' '/^\tpid = /{print $2; exit}')
      ex=$(printf '%s\n' "$st" | awk -F' = ' '/^\tlast exit code = /{print $2; exit}')
      state="${state:-loaded}${pid:+ pid=$pid}${ex:+ last_exit=$ex}"
    else
      state=not-loaded
    fi
    e launchd "$d|$label|$state"
  done
done
F=/Users/Shared/cmux-build-fleet
if [ -d "$F" ]; then
  e fleet_root present
  # Existence only; the token is never read.
  [ -f "$F/secrets/controller.token" ] && e controller_token present || e controller_token missing
  e fleet_labels "$(plutil -extract EnvironmentVariables.CMUX_CI_LABELS raw /Library/LaunchDaemons/ai.manaflow.cmux-build-worker.plist 2>/dev/null)"
  for f in logs/worker.log worker.log job-history.jsonl; do
    [ -f "$F/$f" ] && e fleet_mtime "$f|$(stat -f %m "$F/$f")"
  done
  last=$(tail -n 200 "$F/job-history.jsonl" 2>/dev/null | grep -o '"kind":"[a-z_]*","state":"[a-z_]*"' | tail -1)
  e fleet_last_job "$last"
  e fleet_recipe "$(ls "$F/recipe-releases" 2>/dev/null | tail -1 | cut -c1-12)"
  e fleet_worker_sha "$(shasum -a 256 "$F/bin/worker" 2>/dev/null | cut -c1-12)"
  pgrep -f "$F/bin/worker" >/dev/null && e fleet_worker_proc running || e fleet_worker_proc absent
fi
for ak in "$HOME"/.ssh/authorized_keys*; do
  [ -f "$ak" ] || continue
  e ak_file "$(basename "$ak")|$(stat -f '%Lp' "$ak")"
  # Options (from=, command=, restrict) are reported as a flag, never their values.
  # "|| [ -n ]" keeps a final line that has no trailing newline; sshd still honours it.
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue;; esac
    opts=no
    case "$line" in ssh-*|ecdsa-*|sk-*) ;; *) opts=yes;; esac
    fp=$(printf '%s\n' "$line" | ssh-keygen -lf /dev/stdin 2>/dev/null | head -1)
    [ -n "$fp" ] || fp="unparseable"
    e ak_key "$(basename "$ak")|$opts|$fp"
  done < "$ak"
done
