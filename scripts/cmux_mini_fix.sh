#!/bin/bash
# Non-root repairs for one cmux build mini, one function per repair. glaeda-mini-fleet fix sends
# this file over SSH as the login user, after a WORKLOAD_PATH assignment and before one call such
# as `cmux_clone manaflow-ai/cmux main '~/cmux' 1`. Each function looks before it acts, so a rerun
# after a failure or an interruption resumes and a finished step says "unchanged". Nothing here
# uses sudo, and nothing removes or overwrites a file this file did not create.
set -euo pipefail
# share_recv reads a tar stream on stdin; everything else must not read from the SSH channel.
[ "${KEEP_STDIN:-}" = 1 ] || exec </dev/null
export PATH="$WORKLOAD_PATH:$HOME/.local/bin"
export GIT_TERMINAL_PROMPT=0 HOMEBREW_NO_ENV_HINTS=1
cd "$HOME"

say() { printf 'glaeda-mini-fleet: %s\n' "$*"; }
refuse() { say "refused: $*" >&2; exit 3; }
home() { printf '%s' "${1/#\~/$HOME}"; }

# One changing step at a time per host: an SSH timeout on the operator side does not stop the remote
# step, so a rerun must not start the same work beside it. A lock whose process is gone is taken over.
lock() {
  local dir="$HOME/.local/state/glaeda/mini-fleet/step.lock" pid
  mkdir -p "$(dirname "$dir")"
  if ! mkdir "$dir" 2>/dev/null; then
    pid=$(cat "$dir/pid" 2>/dev/null || true)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then refuse "another glaeda-mini-fleet step (pid $pid) is still running here"; fi
  fi
  echo $$ > "$dir/pid"
  trap 'rm -f "$HOME/.local/state/glaeda/mini-fleet/step.lock/pid"; rmdir "$HOME/.local/state/glaeda/mini-fleet/step.lock" 2>/dev/null || true' EXIT
}

# The cmux checkout: a shallow clone of REF with submodules, as the fleet bootstrap reads them.
cmux_clone() {  # REPO REF ROOT DEPTH
  local root; root=$(home "$3")
  if [ -e "$root/.git" ]; then
    # An interrupted clone leaves a .git with no HEAD; building on it would fail later and obscurely.
    git -C "$root" rev-parse --verify -q HEAD >/dev/null || refuse "$root is an unfinished clone (no HEAD); move it aside"
    say "unchanged: $root is a checkout"; submodules "$3" "$4"; return
  fi
  [ ! -e "$root" ] || refuse "$root exists and is not a git checkout"
  git clone --depth 1 --branch "$2" --progress "https://github.com/$1.git" "$root"
  submodules "$3" "$4"
}

submodules() {  # ROOT DEPTH (0 means full history)
  local root; root=$(home "$1")
  [ -e "$root/.git" ] || refuse "$root is not a git checkout"
  if [ "$2" = 0 ]; then
    git -C "$root" submodule update --init --recursive --progress
  else
    git -C "$root" submodule update --init --recursive --depth "$2" --progress
  fi
  say "submodules: $(git -C "$root" submodule status --recursive | grep -c '^ ' || true) initialized"
}

# A relocatable CPython that the operator copied to DIR becomes ~/.local/bin/python3.
python_prepare() {  # DIR
  mkdir -p "$HOME/.local/bin" "$(home "$1")"
}

python_link() {  # DIR MIN
  local dir link="$HOME/.local/bin/python3"; dir=$(home "$1")
  "$dir/bin/python3" -c 'import sys; want = tuple(int(p) for p in sys.argv[1].split(".")); sys.exit(sys.version_info[:len(want)] < want)' "$2" \
    || refuse "$dir/bin/python3 does not run or is older than $2"
  if [ -e "$link" ] || [ -L "$link" ]; then
    case "$(readlink "$link" || true)" in
      "$HOME"/.local/python-*) ;;
      *) refuse "$link exists and was not made by glaeda-mini-fleet; move it aside first" ;;
    esac
  fi
  ln -sfn "$dir/bin/python3" "$link"
  say "python3: $("$link" --version)"
}

rust_default() {  # TOOLCHAIN
  rustup default "$1"
}

rust_channel() {  # ROOT CHANNEL (the toolchain rust-toolchain.toml in the checkout names)
  cd "$(home "$1")" && rustup toolchain install "$2" --profile minimal
}

# Glaeda itself: the checkout glaeda-mini-enroll runs from, then its user-level setup.
glaeda() {  # PYTHON
  local dir="$HOME/glaeda"
  if [ ! -f "$dir/scripts/cmux_fleet.py" ]; then
    [ ! -e "$dir" ] || refuse "$dir exists and is not a Glaeda checkout"
    git clone --depth 1 --progress https://github.com/teamleaderleo/glaeda.git "$dir"
  fi
  "$(home "$1")" "$dir/scripts/glaeda-mini-setup" --apply
}

# Xcode 26 ships Metal separately; the fleet bootstrap runs xcrun metal.
metal() {  # DEVELOPER_DIR
  if DEVELOPER_DIR="$1" xcrun metal --version >/dev/null 2>&1; then say "unchanged: Metal toolchain present"; return; fi
  DEVELOPER_DIR="$1" xcodebuild -downloadComponent MetalToolchain
  DEVELOPER_DIR="$1" xcrun metal --version | head -1
}

# cmux scripts/setup.sh builds GhosttyKit, the artifacts the bootstrap checks for.
cmux_setup() {  # ROOT DEVELOPER_DIR
  cd "$(home "$1")" && DEVELOPER_DIR="$2" ./scripts/setup.sh
}

# The reviewed Glaeda candidate, where cmux scripts/persistent-compile up looks for it.
candidate_dir() {  # SOURCE12
  mkdir -p "$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$1"
}

candidate_check() {  # SOURCE12 NAME SHA256: exit 0 when the archive is in place, 10 when it is absent
  local file="$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$1/$2"
  [ -f "$file" ] || exit 10
  local got; got=$(shasum -a 256 "$file" | cut -d' ' -f1)
  [ "$got" = "$3" ] || refuse "$file has sha256 $got, want $3; move it aside"
  say "unchanged: $file matches"
}

candidate_place() {  # SOURCE12 NAME SHA256: verify the copy, then move it into place
  local dir="$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$1"
  local got; got=$(shasum -a 256 "$dir/$2.partial" | cut -d' ' -f1)
  [ "$got" = "$3" ] || refuse "$dir/$2.partial has sha256 $got, want $3"
  mv -n "$dir/$2.partial" "$dir/$2"
  [ ! -e "$dir/$2.partial" ] || refuse "$dir/$2 appeared during the copy; $2.partial was left for inspection"
  say "staged: $dir/$2"
}

# ---- LAN seeding (glaeda-mini-fleet share). On the source, share_prepare readies THING once (only
# metal needs it: an exported bundle), share_source checks it and share_send writes it as a tar
# stream to stdout without the step lock, so one source can feed several peers at once. On the peer,
# share_have says whether it is already there, share_recv unpacks the stream into a staging
# directory, and share_finish moves it into place and verifies it. THING is xcode, metal, brew,
# rustup, candidate or python; the arguments follow it.

share_stage() {  # THING ARGS...: the staging directory on the peer, on the same volume as the target
  case "$1" in
    xcode) printf '%s/.glaeda-share.partial' "$(dirname "$2")" ;;
    candidate) printf '%s/Library/Caches/cmux-fleet/glaeda-candidate-%s/.glaeda-share.partial' "$HOME" "$2" ;;
    rustup) printf '%s/.rustup/.glaeda-share.partial' "$HOME" ;;
    python) printf '%s/.local/.glaeda-share.partial' "$HOME" ;;
    metal) printf '%s/Library/Caches/glaeda/share/metal' "$HOME" ;;
    brew) printf '%s/Library/Caches/glaeda/share/homebrew/downloads' "$HOME" ;;
    *) refuse "unknown thing $1" ;;
  esac
}

xcode_matches() {  # APP VERSION BUILD
  [ -d "$1" ] && [ ! -L "$1" ] || return 1
  local v b
  v=$(plutil -extract CFBundleShortVersionString raw "$1/Contents/Info.plist" 2>/dev/null) || return 1
  b=$(plutil -extract ProductBuildVersion raw "$1/Contents/version.plist" 2>/dev/null) || return 1
  [ "$v|$b" = "$2|$3" ]
}

python_ours() {  # print the ~/.local/python-* directory ~/.local/bin/python3 points into, or fail
  local target; target=$(readlink "$HOME/.local/bin/python3" 2>/dev/null) || return 1
  case "$target" in "$HOME"/.local/python-*/bin/python3) dirname "$(dirname "$target")" ;; *) return 1 ;; esac
}

python_at_least() {  # PYTHON MIN
  "$1" -c 'import sys; want = tuple(int(p) for p in sys.argv[1].split(".")); sys.exit(sys.version_info[:len(want)] < want)' "$2"
}

brew_downloads() {  # the Homebrew owner's download cache, or fail
  [ -d /opt/homebrew/Cellar ] || return 1
  local owner; owner=$(stat -f %Su /opt/homebrew/Cellar)
  if [ "$owner" = "$(id -un)" ]; then printf '%s/downloads' "$(brew --cache)"; else printf '/Users/%s/Library/Caches/Homebrew/downloads' "$owner"; fi
}

share_source() {  # THING ARGS...: exit 0 when this host can seed THING
  case "$1" in
    xcode) xcode_matches "$2" "$3" "$4" || refuse "$2 is not Xcode $3 ($4) here" ;;
    candidate) candidate_check "$2" "$3" "$4" >/dev/null ;;
    python) local dir; dir=$(python_ours) || refuse "~/.local/bin/python3 here is not a relocatable CPython under ~/.local"
            python_at_least "$dir/bin/python3" "$2" || refuse "$dir is older than $2" ;;
    rustup) [ -n "$(ls "$HOME/.rustup/toolchains" 2>/dev/null)" ] || refuse "no rustup toolchains here" ;;
    brew) local d; d=$(brew_downloads) || refuse "no Homebrew here"
          [ -r "$d" ] && [ -x "$d" ] || refuse "$d is not readable by $(id -un)" ;;
    metal) local b; b=$(ls -d "$(share_stage metal)"/*.exportedBundle 2>/dev/null | tail -1 || true)
           [ -n "$b" ] || refuse "no exported Metal toolchain bundle here yet (share prepares one: about 700 MB)"
           say "metal: $b" ;;
    *) refuse "unknown thing $1" ;;
  esac
}

share_prepare() {  # THING ARGS...: ready THING on the source; for metal, export a bundle once
  if [ "$1" != metal ]; then share_source "$@"; return; fi
  local stage tmp; stage=$(share_stage metal)
  mkdir -p "$stage"
  if ! ls -d "$stage"/*.exportedBundle >/dev/null 2>&1; then
    # Export beside the cache and move it in whole, so an interrupted export is never shipped.
    tmp=$(mktemp -d "$stage.export.XXXXXX")
    DEVELOPER_DIR="$2" xcodebuild -downloadComponent MetalToolchain -exportPath "$tmp"
    mv "$tmp"/*.exportedBundle "$stage"/
    rmdir "$tmp"
  fi
  share_source "$@"
}

share_have() {  # THING ARGS...: exit 0 when this host already has THING, 1 (10 for candidate) when it needs it
  case "$1" in
    xcode) if [ -e "$2" ] || [ -L "$2" ]; then xcode_matches "$2" "$3" "$4" || refuse "$2 exists and is not Xcode $3 ($4)"; say "unchanged: $2"; exit 0; fi; exit 1 ;;
    candidate) candidate_check "$2" "$3" "$4" ;;  # exits 10 when absent
    python) local p="$HOME/.local/bin/python3"; [ -x "$p" ] && python_at_least "$p" "$2" && { say "unchanged: $p"; exit 0; }; exit 1 ;;
    metal) DEVELOPER_DIR="$2" xcrun metal --version >/dev/null 2>&1 && { say "unchanged: Metal toolchain present"; exit 0; }; exit 1 ;;
    rustup|brew) exit 1 ;;
    *) refuse "unknown thing $1" ;;
  esac
}

share_send() {  # THING ARGS...: a tar stream of THING on stdout; progress on stderr
  share_source "$@" >&2
  case "$1" in
    xcode) tar -C "$(dirname "$2")" -cf - "$(basename "$2")" ;;
    candidate) tar -C "$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$2" -cf - "$3" ;;
    python) local dir; dir=$(python_ours); tar -C "$HOME/.local" -cf - "$(basename "$dir")" ;;
    rustup) tar -C "$HOME/.rustup" -cf - toolchains $(cd "$HOME/.rustup" && ls -d update-hashes 2>/dev/null) ;;
    brew) local d; d=$(brew_downloads); (cd "$d" && { ls | grep -E 'bottle' | grep -v '\.incomplete$' || true; } | tar -cf - -T -) ;;
    metal) local stage; stage=$(share_stage metal)
           tar -C "$stage" -cf - "$(basename "$(ls -d "$stage"/*.exportedBundle | tail -1)")" ;;
  esac
}

share_recv() {  # THING ARGS...: unpack the stream on stdin into the staging directory
  local stage; stage=$(share_stage "$@")
  # A staging directory named *.glaeda-share.partial is only ever this file's; start it fresh.
  case "$stage" in */.glaeda-share.partial) rm -rf "$stage" ;; esac
  mkdir -p "$stage"
  tar -C "$stage" -xf -
}

share_finish() {  # THING ARGS...: move what share_recv staged into place, never over something already there
  local stage entry; stage=$(share_stage "$@")
  case "$1" in
    xcode)
      # mv into an existing directory would nest the copy inside it; refuse instead.
      if [ -e "$2" ] || [ -L "$2" ]; then refuse "$2 appeared during the copy; the staged copy was left in $stage"; fi
      mv -n "$stage/$(basename "$2")" "$(dirname "$2")/"
      [ ! -e "$stage/$(basename "$2")" ] || refuse "$2 appeared during the copy; the staged copy was left in $stage"
      rmdir "$stage" 2>/dev/null || true
      xcode_matches "$2" "$3" "$4" || refuse "$2 is not Xcode $3 ($4) after the copy"
      say "copied: $2 ($3 $4); licence and first launch are sudo-plan steps" ;;
    candidate)
      mv "$stage/$3" "$stage/../$3.partial"
      rmdir "$stage" 2>/dev/null || true
      candidate_place "$2" "$3" "$4" ;;
    python)
      for entry in "$stage"/python-*; do mv -n "$entry" "$HOME/.local/"; done
      rmdir "$stage" 2>/dev/null || true
      python_link "~/.local/$(basename "$entry")" "$2" ;;
    rustup)
      mkdir -p "$HOME/.rustup/toolchains" "$HOME/.rustup/update-hashes"
      for entry in "$stage"/toolchains/* "$stage"/update-hashes/*; do
        [ -e "$entry" ] || continue
        local into; into="$HOME/.rustup/$(basename "$(dirname "$entry")")"
        if [ -e "$into/$(basename "$entry")" ]; then say "unchanged: $into/$(basename "$entry")"; else mv "$entry" "$into/"; say "copied: $(basename "$entry")"; fi
      done
      rm -rf "$stage"  # what is left are copies of toolchains this host already had
      ;;
    metal)
      local bundle; bundle=$(ls -d "$stage"/*.exportedBundle | tail -1)
      DEVELOPER_DIR="$2" xcodebuild -importComponent MetalToolchain -importPath "$bundle"
      DEVELOPER_DIR="$2" xcrun metal --version | head -1 ;;
    brew) say "staged $(ls "$stage" | wc -l | tr -d ' ') Homebrew downloads in $stage; the next brew install (fix or sudo-plan) uses them" ;;
  esac
}
