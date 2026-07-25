#!/usr/bin/env bash
set -euo pipefail

instance="${SMOLRUNNER_VM:-smolrunner}"
guest_repo="${SMOLRUNNER_GUEST_REPO:-/home/lima/smolrunner}"
repo_url="${SMOLRUNNER_REPO_URL:-https://github.com/teamleaderleo/smolrunner.git}"
repo_ref="${SMOLRUNNER_REPO_REF:-main}"
script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
repo_root="$(CDPATH= cd -- "${script_dir}/.." && pwd)"
config="${SMOLRUNNER_LIMA_CONFIG:-${repo_root}/examples/lima/smolrunner-interactive.yaml}"
lima_home="${LIMA_HOME:-${HOME}/.lima}"

usage() {
  cat <<'USAGE'
Usage: bash scripts/macbook-runner-bootstrap.sh COMMAND

Commands:
  create      Install Lima when needed and create the configured instance if absent.
  bootstrap   Create/start the instance, install development prerequisites, build, and run doctor.
  check       Run read-only host and guest checks. The instance must already be running.

Environment:
  SMOLRUNNER_VM           Lima instance name (default: smolrunner)
  SMOLRUNNER_GUEST_REPO   Guest checkout path (default: /home/lima/smolrunner)
  SMOLRUNNER_REPO_URL     Git remote cloned in the guest
  SMOLRUNNER_REPO_REF     Branch updated in the guest (default: main)
  SMOLRUNNER_LIMA_CONFIG  Lima YAML path
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_macos() {
  [ "$(uname -s)" = "Darwin" ] || die 'the Lima development helper supports macOS only'
}

ensure_brew() {
  if command -v brew >/dev/null 2>&1; then
    return
  fi
  for candidate in /opt/homebrew/bin/brew /usr/local/bin/brew; do
    if [ -x "${candidate}" ]; then
      PATH="$(dirname "${candidate}"):${PATH}"
      export PATH
      return
    fi
  done
  die 'Homebrew is required to install Lima'
}

require_lima() {
  command -v limactl >/dev/null 2>&1 || die 'limactl is unavailable'
}

ensure_lima() {
  if command -v limactl >/dev/null 2>&1; then
    return
  fi
  ensure_brew
  printf 'Installing Lima with Homebrew.\n'
  brew install lima
}

instance_exists() {
  [ -d "${lima_home}/${instance}" ]
}

instance_running() {
  [ "$(limactl list --quiet --filter '.status == "Running"' "${instance}" 2>/dev/null)" = "${instance}" ]
}

create_instance() {
  require_macos
  ensure_lima
  [ -f "${config}" ] || die "Lima configuration does not exist: ${config}"

  if instance_exists; then
    printf 'Lima instance %s already exists; preserving it.\n' "${instance}"
    limactl list "${instance}"
    return
  fi

  printf 'Creating Lima instance %s from %s\n' "${instance}" "${config}"
  limactl create --name="${instance}" --tty=false "${config}"
}

start_instance() {
  create_instance
  if ! instance_running; then
    limactl start "${instance}"
  fi
}

bootstrap_guest() {
  start_instance

  printf 'Bootstrapping development prerequisites in %s.\n' "${instance}"
  limactl shell "${instance}" -- \
    /usr/bin/env \
      SMOLRUNNER_GUEST_REPO="${guest_repo}" \
      SMOLRUNNER_REPO_URL="${repo_url}" \
      SMOLRUNNER_REPO_REF="${repo_ref}" \
    /usr/bin/bash -s <<'GUEST'
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential \
  ca-certificates \
  curl \
  fuse-overlayfs \
  git \
  jq \
  libssl-dev \
  pkg-config \
  podman \
  slirp4netns \
  tmux \
  uidmap

# SmolRunner invokes rootless Podman directly. Do not keep an unused
# privileged API socket or tag-based auto-update path enabled.
sudo systemctl disable --now \
  podman.socket \
  podman.service \
  podman-auto-update.timer \
  podman-auto-update.service \
  podman-restart.service >/dev/null 2>&1 || true
sudo systemctl reset-failed podman-restart.service >/dev/null 2>&1 || true

if ! command -v rustup >/dev/null 2>&1; then
  printf 'Installing the Rust development toolchain with rustup.\n'
  curl --proto '=https' --tlsv1.2 --fail --silent --show-error \
    https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi

# shellcheck disable=SC1091
. "${HOME}/.cargo/env"
rustup set profile minimal

repo="${SMOLRUNNER_GUEST_REPO}"
repo_url="${SMOLRUNNER_REPO_URL}"
repo_ref="${SMOLRUNNER_REPO_REF}"

if [ ! -d "${repo}/.git" ]; then
  mkdir -p "$(dirname "${repo}")"
  git clone --branch "${repo_ref}" --single-branch "${repo_url}" "${repo}"
else
  actual_url="$(git -C "${repo}" remote get-url origin)"
  [ "${actual_url}" = "${repo_url}" ] || {
    printf 'error: existing guest checkout uses unexpected origin: %s\n' "${actual_url}" >&2
    exit 1
  }
  if [ -n "$(git -C "${repo}" status --porcelain)" ]; then
    printf 'error: guest checkout has local changes; refusing to update it\n' >&2
    git -C "${repo}" status --short --branch >&2
    exit 1
  fi
  git -C "${repo}" switch "${repo_ref}"
  git -C "${repo}" fetch --prune origin "${repo_ref}"
  git -C "${repo}" merge --ff-only "origin/${repo_ref}"
fi

cd "${repo}"
cargo build --locked
cargo run --locked --quiet -- --output json doctor

podman info --format json | jq -e '
  .host.security.rootless == true and
  .host.cgroupVersion == "v2" and
  .host.cgroupManager == "systemd"
' >/dev/null

printf '\nDevelopment bootstrap complete. No GitHub runner was registered.\n'
GUEST
}

check_environment() {
  require_macos
  require_lima
  instance_exists || die "Lima instance '${instance}' does not exist"
  instance_running || die "Lima instance '${instance}' is stopped; run make vm-up first"

  printf '\n== Lima instance ==\n'
  limactl list --all-fields "${instance}"

  printf '\n== observation ==\n'
  bash "${repo_root}/scripts/macbook-runner-observe.sh" "${instance}"

  printf '\n== guest safety and toolchain ==\n'
  limactl shell "${instance}" -- \
    /usr/bin/env SMOLRUNNER_GUEST_REPO="${guest_repo}" \
    /usr/bin/bash -lc '
      set -euo pipefail

      printf "rootful-podman-socket="
      if sudo test -S /run/podman/podman.sock; then
        printf "present\n"
        exit 1
      else
        printf "absent\n"
      fi

      printf "host-mounts="
      if findmnt -rn -t virtiofs,9p,fuse.sshfs | grep -q .; then
        printf "present\n"
        findmnt -rn -t virtiofs,9p,fuse.sshfs
        exit 1
      else
        printf "absent\n"
      fi

      podman info --format json | jq -r \
        "\"rootless=\(.host.security.rootless) cgroups=\(.host.cgroupVersion) runtime=\(.host.ociRuntime.name) manager=\(.host.cgroupManager)\""

      repo="$SMOLRUNNER_GUEST_REPO"
      [ -d "$repo/.git" ] || {
        printf "error: missing guest checkout: %s\n" "$repo" >&2
        exit 1
      }
      git -C "$repo" status --short --branch
      cd "$repo"
      cargo run --locked --quiet -- --output json doctor
    '
}

case "${1:-}" in
  create)
    create_instance
    ;;
  bootstrap)
    bootstrap_guest
    ;;
  check)
    check_environment
    ;;
  help|-h|--help|'')
    usage
    ;;
  *)
    usage >&2
    die "unknown command '${1}'"
    ;;
esac
