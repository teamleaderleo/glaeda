#!/usr/bin/env bash
set -euo pipefail
umask 077

expected_config="/home/smolrunner-runner/actions-runner/config.sh"

[ "$#" -ge 1 ] || {
  printf 'error: missing fixed runner config path\n' >&2
  exit 2
}
config="$1"
shift
[ "${config}" = "${expected_config}" ] || {
  printf 'error: token bridge accepts only the reviewed runner config path\n' >&2
  exit 2
}
[ -x "${config}" ] || {
  printf 'error: reviewed runner config path is unavailable\n' >&2
  exit 2
}

secret_token=""
IFS= read -r secret_token || {
  printf 'error: expected one short-lived GitHub runner token on stdin\n' >&2
  exit 2
}
[ -n "${secret_token}" ] || {
  printf 'error: GitHub runner token is empty\n' >&2
  exit 2
}
[ "${#secret_token}" -le 4096 ] || {
  printf 'error: GitHub runner token exceeds the bounded input length\n' >&2
  exit 2
}
case "${secret_token}" in
  *$'\r'*|*$'\n'*|*$'\t'*)
    printf 'error: GitHub runner token contains unsupported control whitespace\n' >&2
    exit 2
    ;;
esac

export ACTIONS_RUNNER_INPUT_TOKEN="${secret_token}"
secret_token=""
unset secret_token
exec "${config}" "$@"
