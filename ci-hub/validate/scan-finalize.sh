#!/usr/bin/env bash
# scan-finalize.sh -- consume-path wiring for finalize_receipt.py --scan.
#
# WHY: Hermit's Rust validation driver can write a count-less receipt whenever it
# cannot reach the parent count helper (DEV_HERMIT_PARENT unset in the slot /
# systemd-run producer env -> executed_tests/filtered_tests stay null). With the
# uncounted-receipt grandfather REMOVED (ci-hub/lib/validate_status.rs), such a
# receipt is NotValidated. This helper re-mints a count-backed schema-5 row for
# one caller-selected count-less clean/full/pass green from that green's OWN
# durable log, BEFORE a landing consumer reads the ledger -- so a genuine green
# is never stranded merely because the producer failed to inline its counts.
#
# SAFETY: finalize_receipt.py --scan is APPEND-ONLY and holds the same ledger
# flock as scripts/validate.rs. Exact SHA plus the Rust-canonical source-row digest bind
# it to one row; a matching duplicate is refused. It NEVER fabricates: a row
# whose log is gone ("no-log") or whose manifest is not derivable at that sha
# ("no-manifest") is reported and skipped, not minted. It is best-effort at its
# landing call sites (`|| true`) -- this wrapper itself preserves a nonzero
# refusal so tests and direct callers can observe it. The authoritative gate
# remains validate-status.
#
# Usage:
#   scan-finalize.sh --hermit-checkout <path> --sha <40-hex> \
#     --select-candidate-sha256 [--ledger <path>]
#   scan-finalize.sh --hermit-checkout <path> --sha <40-hex> \
#     --selected-row-sha256 <64-hex> [--ledger <path>]
set -uo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)

hermit_checkout=""
sha=""
selected_row_sha256=""
select_candidate=0
ledger="${CI_HUB_VALIDATE_LEDGER:-$ROOT/ignored/validate-run-ledger.jsonl}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --hermit-checkout) hermit_checkout=$2; shift 2 ;;
    --ledger) ledger=$2; shift 2 ;;
    --sha) sha=$2; shift 2 ;;
    --selected-row-sha256) selected_row_sha256=$2; shift 2 ;;
    --select-candidate-sha256) select_candidate=1; shift ;;
    *) echo "scan-finalize: unknown arg '$1'" >&2; exit 2 ;;
  esac
done

finalizer="$ROOT/ci-hub/validate/finalize_receipt.py"

if [ -z "$hermit_checkout" ]; then
  echo "scan-finalize: REFUSE (no --hermit-checkout; cannot derive per-node manifests)" >&2
  exit 2
fi
if [[ ! $sha =~ ^[0-9a-f]{40}$ ]]; then
  echo "scan-finalize: REFUSE (--sha must be exactly 40 lowercase hex)" >&2
  exit 2
fi
if [ "$select_candidate" -eq 1 ] && [ -n "$selected_row_sha256" ]; then
  echo "scan-finalize: REFUSE (--select-candidate-sha256 conflicts with --selected-row-sha256)" >&2
  exit 2
fi
if [ "$select_candidate" -eq 0 ] && [[ ! $selected_row_sha256 =~ ^[0-9a-f]{64}$ ]]; then
  echo "scan-finalize: REFUSE (--selected-row-sha256 must be exactly 64 lowercase hex)" >&2
  exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "scan-finalize: REFUSE (python3 unavailable)" >&2; exit 2
fi
if [ ! -r "$finalizer" ]; then
  echo "scan-finalize: REFUSE (finalizer not found at $finalizer)" >&2; exit 2
fi
if [ ! -f "$ledger" ]; then
  echo "scan-finalize: REFUSE (ledger not found at $ledger)" >&2; exit 2
fi
if [ ! -d "$hermit_checkout/.git" ] && [ ! -f "$hermit_checkout/.git" ]; then
  echo "scan-finalize: REFUSE (hermit checkout $hermit_checkout is not a git repo)" >&2; exit 2
fi

if [ "$select_candidate" -eq 1 ]; then
  exec python3 "$finalizer" --select-candidate-sha256 --ledger "$ledger" \
    --hermit-checkout "$hermit_checkout" --sha "$sha"
fi

out=$(python3 "$finalizer" --scan --ledger "$ledger" \
  --hermit-checkout "$hermit_checkout" --sha "$sha" \
  --selected-row-sha256 "$selected_row_sha256" 2>&1)
rc=$?
printf '%s\n' "$out" | sed 's/^/scan-finalize: /'
if [ "$rc" -ne 0 ]; then
  echo "scan-finalize: finalize_receipt --scan rc=$rc (best-effort; validate-status remains authoritative)"
fi
exit "$rc"
