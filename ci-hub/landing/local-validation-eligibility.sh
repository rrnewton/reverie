#!/usr/bin/env bash
# Exact-head local-validation authority for the lander. The GitHub label is a
# cache of this result, never an independent authorization signal.
set -uo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: local-validation-eligibility.sh <40-hex-sha> [observed-labels]" >&2
  exit 2
fi

sha=$1
observed_labels=${2:-}
case "$sha" in
  *[!0-9a-f]*|'')
    echo "eligibility: SHA must be lowercase hexadecimal" >&2
    exit 2
    ;;
esac
if [ "${#sha}" -ne 40 ]; then
  echo "eligibility: SHA must be exactly 40 hexadecimal characters" >&2
  exit 2
fi

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=$(cd -- "$script_dir/../.." && pwd)
status_bin=${CI_HUB_VALIDATE_STATUS_BIN:-$root/ci-hub/ci-hub}
status_args=(validate-status --sha "$sha")

status_output=$("$status_bin" "${status_args[@]}" 2>&1)
status_rc=$?

printf 'LABEL_CACHE=%s (observed only; non-authoritative)\n' \
  "${observed_labels:-<absent>}"
printf '%s\n' "$status_output"
case "$status_rc" in
  0)
    echo "ELIGIBILITY=VALIDATED"
    exit 0
    ;;
  3)
    echo "ELIGIBILITY=KNOWN_FAILED"
    exit 3
    ;;
  4)
    echo "ELIGIBILITY=NOT_VALIDATED"
    exit 4
    ;;
  *)
    echo "ELIGIBILITY=ERROR validate-status-rc=$status_rc" >&2
    exit 2
    ;;
esac
