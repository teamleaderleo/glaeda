#!/bin/bash
# Non-root repairs for one cmux build mini, one function per repair. glaeda-mini-fleet fix sends
# this file over SSH as the login user, after a WORKLOAD_PATH assignment and before one call such
# as `cmux_clone manaflow-ai/cmux main '~/cmux' 1`. Each function looks before it acts, so a rerun
# after a failure or an interruption resumes and a finished step says "unchanged". Nothing here
# uses sudo, and nothing removes or overwrites a file this file did not create.
set -euo pipefail
exec </dev/null
export PATH="$WORKLOAD_PATH:$HOME/.local/bin"
export GIT_TERMINAL_PROMPT=0 HOMEBREW_NO_ENV_HINTS=1
cd "$HOME"

say() { printf 'glaeda-mini-fleet: %s\n' "$*"; }
refuse() { say "refused: $*" >&2; exit 3; }
home() { printf '%s' "${1/#\~/$HOME}"; }

# The cmux checkout: a shallow clone of REF with submodules, as the fleet bootstrap reads them.
cmux_clone() {  # REPO REF ROOT DEPTH
  local root; root=$(home "$3")
  if [ -e "$root/.git" ]; then say "unchanged: $root is a checkout"; submodules "$3" "$4"; return; fi
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

candidate_check() {  # SOURCE12 NAME SHA256: exit 0 when the archive is already in place
  local file="$HOME/Library/Caches/cmux-fleet/glaeda-candidate-$1/$2"
  [ -f "$file" ] || exit 1
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
