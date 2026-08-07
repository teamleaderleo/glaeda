#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
container_context="${repo_root}/containers/local-ci"
containerfile="${container_context}/Containerfile"

podman=/usr/bin/podman
git=/usr/bin/git
image="localhost/smolrunner-local-ci:rust-1.97.1-v1"
profile="smolrunner.required-local"
cargo_cache="smolrunner-local-cargo-home-v1"
target_cache="smolrunner-local-target-v1"

cpus="2"
memory="2g"
memory_mib=2048
memory_swap="2560m"
memory_swap_mib=2560
pids=768
prepare_network="slirp4netns"
verify_network="none"

usage() {
  cat <<'USAGE'
Usage:
  bash scripts/local-ci-verify.sh contract
  bash scripts/local-ci-verify.sh image
  bash scripts/local-ci-verify.sh prepare --commit COMMIT --tree TREE
  bash scripts/local-ci-verify.sh verify  --commit COMMIT --tree TREE
  bash scripts/local-ci-verify.sh all     --commit COMMIT --tree TREE

The profile is fixed to smolrunner.required-local. The wrapper accepts no arbitrary command,
resource, image, mount, network, cache, or environment override.

prepare:
  validates exact clean source identity and runs only `cargo fetch --locked` with outbound network.

verify:
  validates the same source identity and runs formatting, check, Clippy, and tests in disposable
  rootless Podman containers with network disabled. Cargo and target caches persist in named volumes.

all:
  builds/refreshes the reviewed toolchain image, prepares dependency cache, then verifies.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

validate_hex40() {
  local name="$1"
  local value="$2"
  [[ "${value}" =~ ^[0-9a-f]{40}$ ]] || die "${name} must be exactly 40 lowercase hexadecimal characters"
}

parse_source_args() {
  expected_commit=""
  expected_tree=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --commit)
        [ "$#" -ge 2 ] || die '--commit requires a value'
        expected_commit="$2"
        shift 2
        ;;
      --tree)
        [ "$#" -ge 2 ] || die '--tree requires a value'
        expected_tree="$2"
        shift 2
        ;;
      *)
        die "unsupported argument: $1"
        ;;
    esac
  done
  [ -n "${expected_commit}" ] || die '--commit is required'
  [ -n "${expected_tree}" ] || die '--tree is required'
  validate_hex40 commit "${expected_commit}"
  validate_hex40 tree "${expected_tree}"
}

require_runtime() {
  [ -x "${podman}" ] || die 'rootless Podman is unavailable at /usr/bin/podman'
  [ -x "${git}" ] || die 'Git is unavailable at /usr/bin/git'

  # Never inherit a remote Podman/Docker endpoint from the listener or repository workflow.
  unset CONTAINER_HOST DOCKER_HOST PODMAN_HOST CONTAINER_CONNECTION

  if [ -e /run/podman/podman.sock ] || [ -L /run/podman/podman.sock ]; then
    die 'privileged Podman socket path is present; repair the guest boundary before local verification'
  fi

  local rootless
  rootless="$(${podman} info --format '{{.Host.Security.Rootless}}' 2>/dev/null || true)"
  [ "${rootless}" = "true" ] || die 'Podman did not prove a rootless execution boundary'
}

validate_source() {
  [ -d "${repo_root}/.git" ] || die 'SmolRunner checkout is not a Git worktree'
  case "${repo_root}" in
    *','*|*$'\n'*|*$'\r'*)
      die 'repository path contains a character unsupported by the fixed Podman bind-mount contract'
      ;;
  esac

  local top actual_commit actual_tree status
  top="$(${git} -C "${repo_root}" rev-parse --show-toplevel)"
  [ "${top}" = "${repo_root}" ] || die 'wrapper must run from its exact repository root'

  actual_commit="$(${git} -C "${repo_root}" rev-parse --verify HEAD)"
  actual_tree="$(${git} -C "${repo_root}" rev-parse --verify 'HEAD^{tree}')"
  [ "${actual_commit}" = "${expected_commit}" ] || die 'checkout commit differs from the requested immutable commit'
  [ "${actual_tree}" = "${expected_tree}" ] || die 'checkout tree differs from the requested immutable tree'

  status="$(${git} -C "${repo_root}" status --porcelain=v1 --untracked-files=all)"
  [ -z "${status}" ] || die 'checkout contains local changes or untracked files; local verification requires exact committed source'
}

ensure_volumes() {
  for volume in "${cargo_cache}" "${target_cache}"; do
    if ! ${podman} volume inspect "${volume}" >/dev/null 2>&1; then
      ${podman} volume create "${volume}" >/dev/null
    fi
  done
}

image_id() {
  local value
  value="$(${podman} image inspect --format '{{.Id}}' "${image}" 2>/dev/null || true)"
  [[ "${value}" =~ ^(sha256:)?[0-9a-f]{64}$ ]] || die 'local verification image has no exact immutable image ID'
  printf '%s\n' "${value}"
}

require_image() {
  ${podman} image exists "${image}" || die 'local verification image is missing; run the image command first'
  image_id >/dev/null
}

build_image() {
  require_runtime
  [ -f "${containerfile}" ] || die 'reviewed local verification Containerfile is missing'
  ${podman} build \
    --pull=always \
    --tag "${image}" \
    --file "${containerfile}" \
    "${container_context}"
  image_id >/dev/null
}

run_container() {
  local network="$1"
  shift
  case "${network}" in
    "${prepare_network}"|"${verify_network}") ;;
    *) die 'internal network policy escaped the reviewed prepare/verify classes' ;;
  esac

  ${podman} run \
    --rm \
    --pull=never \
    --network="${network}" \
    --read-only \
    --cap-drop=all \
    --security-opt=no-new-privileges \
    --pid=private \
    --ipc=private \
    --pids-limit="${pids}" \
    --cpus="${cpus}" \
    --memory="${memory}" \
    --memory-swap="${memory_swap}" \
    --tmpfs=/tmp:rw,nosuid,nodev,size=536870912 \
    --mount "type=bind,source=${repo_root},destination=/workspace,ro=true" \
    --mount "type=volume,source=${cargo_cache},destination=/cargo" \
    --mount "type=volume,source=${target_cache},destination=/target" \
    --workdir=/workspace \
    --env=CARGO_HOME=/cargo \
    --env=CARGO_TARGET_DIR=/target \
    --env=HOME=/cargo \
    "${image}" \
    "$@"
}

emit_receipt() {
  local phase="$1"
  local fetch_status="$2"
  local fmt_status="$3"
  local check_status="$4"
  local clippy_status="$5"
  local test_status="$6"
  local result="$7"
  local exact_image
  exact_image="$(image_id)"

  printf '{'
  printf '"schema_version":1,'
  printf '"receipt_type":"smolrunner-local-verification-receipt",'
  printf '"profile":"%s",' "${profile}"
  printf '"phase":"%s",' "${phase}"
  printf '"source":{"commit":"%s","tree":"%s"},' "${expected_commit}" "${expected_tree}"
  printf '"image":{"tag":"%s","id":"%s"},' "${image}" "${exact_image}"
  printf '"resources":{"cpus":2,"memory_mib":%s,"memory_swap_mib":%s,"pids":%s,"concurrency":1},' \
    "${memory_mib}" "${memory_swap_mib}" "${pids}"
  printf '"network":{"prepare":"enabled","verify":"disabled"},'
  printf '"cache":{"cargo":"persistent_private","target":"persistent_private"},'
  printf '"source_mount":"read_only",'
  printf '"container":{"rootless":true,"capabilities":"dropped","no_new_privileges":true,"rootfs":"read_only"},'
  printf '"statuses":{"fetch":%s,"fmt":%s,"check":%s,"clippy":%s,"test":%s},' \
    "${fetch_status}" "${fmt_status}" "${check_status}" "${clippy_status}" "${test_status}"
  printf '"result":"%s"' "${result}"
  printf '}\n'
}

prepare_source() {
  require_runtime
  require_image
  validate_source
  ensure_volumes

  local status
  set +e
  run_container "${prepare_network}" cargo fetch --locked
  status=$?
  set -e

  if [ "${status}" -eq 0 ]; then
    emit_receipt prepare "${status}" null null null null passed
  else
    emit_receipt prepare "${status}" null null null null failed
  fi
  return "${status}"
}

verify_source() {
  require_runtime
  require_image
  validate_source
  ensure_volumes

  local fmt_status check_status clippy_status test_status

  set +e
  run_container "${verify_network}" cargo fmt --all -- --check
  fmt_status=$?
  set -e
  if [ "${fmt_status}" -ne 0 ]; then
    emit_receipt verify null "${fmt_status}" null null null failed
    return "${fmt_status}"
  fi

  set +e
  run_container "${verify_network}" cargo check --locked --all-targets --all-features --offline
  check_status=$?
  set -e
  if [ "${check_status}" -ne 0 ]; then
    emit_receipt verify null "${fmt_status}" "${check_status}" null null failed
    return "${check_status}"
  fi

  set +e
  run_container "${verify_network}" cargo clippy --locked --all-targets --all-features --offline -- -D warnings
  clippy_status=$?
  set -e
  if [ "${clippy_status}" -ne 0 ]; then
    emit_receipt verify null "${fmt_status}" "${check_status}" "${clippy_status}" null failed
    return "${clippy_status}"
  fi

  set +e
  run_container "${verify_network}" cargo test --locked --all-targets --all-features --offline
  test_status=$?
  set -e
  if [ "${test_status}" -ne 0 ]; then
    emit_receipt verify null "${fmt_status}" "${check_status}" "${clippy_status}" "${test_status}" failed
    return "${test_status}"
  fi

  emit_receipt verify null "${fmt_status}" "${check_status}" "${clippy_status}" "${test_status}" passed
}

print_contract() {
  cat <<'JSON'
{"schema_version":1,"contract":"smolrunner.required-local","concurrency":1,"resources":{"cpus":2,"memory_mib":2048,"memory_swap_mib":2560,"pids":768},"source":{"committed_only":true,"mount":"read_only"},"network":{"prepare":"enabled","verify":"disabled"},"cache":{"cargo":"persistent_private","target":"persistent_private"},"container":{"rootless":true,"capabilities":"dropped","no_new_privileges":true,"rootfs":"read_only"},"commands":["cargo fetch --locked","cargo fmt --all -- --check","cargo check --locked --all-targets --all-features --offline","cargo clippy --locked --all-targets --all-features --offline -- -D warnings","cargo test --locked --all-targets --all-features --offline"]}
JSON
}

command="${1:-}"
[ -n "${command}" ] || {
  usage >&2
  exit 2
}
shift

case "${command}" in
  contract)
    [ "$#" -eq 0 ] || die 'contract accepts no arguments'
    print_contract
    ;;
  image)
    [ "$#" -eq 0 ] || die 'image accepts no arguments'
    build_image
    ;;
  prepare)
    parse_source_args "$@"
    prepare_source
    ;;
  verify)
    parse_source_args "$@"
    verify_source
    ;;
  all)
    parse_source_args "$@"
    build_image
    prepare_source
    verify_source
    ;;
  help|-h|--help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
