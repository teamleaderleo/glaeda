#!/usr/bin/env bash
set -euo pipefail

suite="${1:-}"
renderprove_checkout="${RENDERPROVE_CHECKOUT:-}"
evidence_directory="${SMOLRUNNER_EVIDENCE_DIR:-.smolrunner/renderprove}"

die() {
  printf 'error: %s\n' "$*" >&2
  exit 2
}

[ "${suite}" = "render" ] || die 'the example wrapper accepts only the render suite'
case "${renderprove_checkout}" in
  /*) ;;
  *) die 'RENDERPROVE_CHECKOUT must be an absolute path to a trusted Renderprove checkout' ;;
esac
case "${renderprove_checkout}" in
  *$'\n'*|*$'\r'*) die 'RENDERPROVE_CHECKOUT must not contain line breaks' ;;
esac
[ -d "${renderprove_checkout}" ] || die 'RENDERPROVE_CHECKOUT does not name a directory'

case "${evidence_directory}" in
  ''|/*|*'//'*|*$'\n'*|*$'\r'*)
    die 'SMOLRUNNER_EVIDENCE_DIR must be a non-empty project-relative path without line breaks'
    ;;
esac
IFS='/' read -r -a evidence_components <<<"${evidence_directory}"
for component in "${evidence_components[@]}"; do
  case "${component}" in
    ''|.|..|*[!A-Za-z0-9._-]*)
      die 'SMOLRUNNER_EVIDENCE_DIR must contain only safe normal path components'
      ;;
  esac
done

project_directory="$(pwd -P)"
project_parent="$(dirname -- "${project_directory}")"
project_name="$(basename -- "${project_directory}")"

exec /usr/bin/env \
  RENDERPROVE_ENROLLED_ROOT="${project_parent}" \
  RENDERPROVE_PROBE_OUTPUT="${evidence_directory}" \
  npm --prefix "${renderprove_checkout}" run probe:podman -- "${project_name}"
