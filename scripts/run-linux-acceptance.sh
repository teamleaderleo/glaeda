#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

for command in cargo docker rustup musl-gcc; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'required command is missing: %s\n' "$command" >&2
    exit 1
  fi
done

case "$(uname -m)" in
  x86_64)
    rust_target=x86_64-unknown-linux-musl
    linker_variable=CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER
    ;;
  aarch64 | arm64)
    rust_target=aarch64-unknown-linux-musl
    linker_variable=CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER
    ;;
  *)
    printf 'unsupported Linux architecture for the acceptance binary: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac

if (($# > 0)); then
  images=("$@")
else
  images=(debian:12-slim ubuntu:24.04)
fi

target_dir=$(mktemp -d)
cleanup() {
  rm -rf "$target_dir"
}
trap cleanup EXIT

rustup target add "$rust_target"
env \
  "$linker_variable=musl-gcc" \
  CARGO_TARGET_DIR="$target_dir" \
  cargo test --locked \
    --test linux_acceptance \
    --test rootless_podman_preflight_acceptance \
    --target "$rust_target" \
    --no-run

find_test_binary() {
  local test_name=$1
  find "$target_dir/$rust_target/debug/deps" \
    -maxdepth 1 \
    -type f \
    -name "$test_name-*" \
    -perm -111 \
    -print \
    -quit
}

acceptance_binary=$(find_test_binary linux_acceptance)
preflight_binary=$(find_test_binary rootless_podman_preflight_acceptance)

if [[ -z "$acceptance_binary" || -z "$preflight_binary" ]]; then
  printf 'could not locate both compiled Linux acceptance test binaries\n' >&2
  exit 1
fi

for image in "${images[@]}"; do
  case "$image" in
    debian:*) expected_os=debian ;;
    ubuntu:*) expected_os=ubuntu ;;
    *)
      printf 'image must be an explicit Debian or Ubuntu tag: %s\n' "$image" >&2
      exit 1
      ;;
  esac

  docker pull "$image"
  image_receipt=$(docker image inspect --format '{{index .RepoDigests 0}}' "$image")
  printf '\nRunning %s acceptance against %s\n' "$expected_os" "$image_receipt"

  docker run --rm \
    --tmpfs /run:rw,nosuid,nodev,mode=0755 \
    --tmpfs /tmp:rw,nosuid,nodev,mode=1777 \
    --mount "type=bind,src=$acceptance_binary,dst=/usr/local/bin/smolrunner-linux-acceptance,readonly" \
    --mount "type=bind,src=$preflight_binary,dst=/usr/local/bin/smolrunner-podman-preflight-acceptance,readonly" \
    --env SMOLRUNNER_LINUX_ACCEPTANCE=1 \
    --env "SMOLRUNNER_EXPECTED_OS=$expected_os" \
    "$image" \
    /bin/sh -euxc '
      printf "#!/bin/sh\nexit 101\n" > /usr/sbin/policy-rc.d
      chmod 0755 /usr/sbin/policy-rc.d
      export DEBIAN_FRONTEND=noninteractive
      apt-get update
      apt-get install --yes --no-install-recommends \
        ca-certificates \
        dbus-user-session \
        fuse-overlayfs \
        git \
        passwd \
        podman \
        slirp4netns \
        systemd \
        uidmap \
        util-linux
      rm -rf /var/lib/apt/lists/*
      /usr/local/bin/smolrunner-linux-acceptance --test-threads=1 --nocapture
      /usr/local/bin/smolrunner-podman-preflight-acceptance --test-threads=1 --nocapture
    '
done
