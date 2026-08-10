#!/usr/bin/env bash
# validate-envelope.sh — the compat-envelope REGRESSION gate.
#
# Runs the known-green cells for both the hermit Detcore envelope and the
# reverie B1.5 Guest/Tool boundary, asserts they stayed green, and writes an
# ignored per-run observation. Publishing the current scorecard is a separate
# reviewed operation through publish-scorecard.py.
#
# This is what the outer-repo `validate` entry point and CI call. It is
# deliberately fast + safe (regression mode only, no expansion sweep). The
# expansion sweep (full superset, bounded by safe-ci-dag-runner) is separate;
# see expansion-dag.rs.
#
# Exit non-zero if ANY enabled cell stopped passing (green-stays-green).
#
# Usage:
#   validate-envelope.sh [--lane LANE] [--repo HERMIT_CHECKOUT] [--no-reverie]
#                        [--backends b1,b2,...]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
lane="portable"
repo="${here}/../hermit"
backends="ptrace,dbi,sabre"
do_reverie=1

while [ $# -gt 0 ]; do
  case "$1" in
    --lane) lane="$2"; shift 2 ;;
    --repo) repo="$2"; shift 2 ;;
    --backends) backends="$2"; shift 2 ;;
    --no-reverie) do_reverie=0; shift ;;
    -h|--help)
      grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "validate-envelope: unknown arg $1" >&2; exit 2 ;;
  esac
done

fail=0

echo "== compat-envelope regression: hermit (lane=${lane}, backends=${backends}) =="
if ! "${here}/collect-envelope.rs" --mode regression --lane "${lane}" \
      --repo "${repo}" --backends "${backends}" --with-parity \
      --assert-green; then
  echo "validate-envelope: HERMIT envelope regression detected" >&2
  fail=1
fi

echo "== compat-envelope: current view must equal its observation-history projection =="
if ! python3 "${here}/publish-scorecard.py" --check \
      --current "${here}/scorecard.csv" \
      --history "${here}/history/scorecard-observations.csv"; then
  echo "validate-envelope: scorecard.csv is not the bound current projection" >&2
  fail=1
fi

if [ "${do_reverie}" -eq 1 ]; then
  echo "== compat-envelope regression: reverie B1.5 (ptrace vs kvm) =="
  # KVM parity is measurable only where /dev/kvm exists; on a portable lane it
  # records not-runnable cells honestly and still asserts ptrace determinism.
  if ! "${here}/collect-reverie-compat.rs" --repo "${repo}" \
        --csv "${here}/reverie-scorecard.csv" --assert-green; then
    echo "validate-envelope: REVERIE envelope regression detected" >&2
    fail=1
  fi
fi

# EVERY determinism claim must be EARNED, and must say what earned it.
#
# This gate had no production call site: it existed, it worked, and nothing ran
# it, so a row could claim `deterministic=1` (or `tier=bitwise` on a stripped
# comparison, or with 0|0 counts) and no pipeline would object. An unwired
# verifier is a comment. It runs on the CSVs the collectors just wrote, so it
# gates the freshly produced evidence rather than a stale snapshot.
echo "== compat-envelope: determinism claims must be earned =="
if ! "${here}/check-determinism-earned.sh" "${here}/scorecard.csv"; then
  echo "validate-envelope: UNEARNED or OVER-TIERED determinism claims in scorecard.csv" >&2
  fail=1
fi

# A raw execution pass is not a scorecard green unless the row names the exact
# comparison standard it met. The checker derives the published set and refuses
# missing/blank/unknown values; the renderer separately withholds explicit
# legacy/weak tiers from the green denominator.
if ! python3 "${here}/check-scorecard-tier.py" --root "${here}"; then
  echo "validate-envelope: UNTIERED comparison rows in the published scorecards" >&2
  exit 1
fi

# The other half of the tier gate, and the reason it needed one. The check above
# validates the tier VOCABULARY -- is the label in the allowed set -- and never
# reads an evidence column, so `comparison_tier` was a self-declared string with
# nothing binding it to the comparison actually performed. `tier_evidence.py` was
# written to close that, passed 18 tests, and had ZERO CALL SITES; the same defect
# the determinism gate above carries a comment about. An unwired verifier is a
# comment.
#
# EXPECT `fully evidenced : 0 of 6` HERE. That is the correct starting number, not
# a regression: it is the first time the claims have been checked at all. The six
# pre-existing unevidenced claims are named in tier-evidence-baseline.json, which
# is a RATCHET, not a mute button -- they are counted and printed on every run, a
# SEVENTH unevidenced claim fails this gate, and an entry whose debt is gone also
# fails it. `fail=1` rather than `exit 1` so this cannot mask the provenance checks
# below.
echo "== compat-envelope: a tier claim must carry evidence for every component it names =="
if ! python3 "${here}/tier_evidence.py" --root "${here}" \
      --baseline "${here}/tier-evidence-baseline.json"; then
  echo "validate-envelope: UNREGISTERED unevidenced tier claim, or a stale debt-register entry" >&2
  fail=1
fi

# The tier gate above can only check evidence that EXISTS, so this is the half
# that keeps the evidence honest. `stdout_parity` is a boolean with two SHA-256
# operands beside it, and at 2026-08-08 the reference operand was populated in 0
# of 2290 published rows while the candidate was populated in 2068 -- so no reader
# could tell parity-HELD from parity-DIFFERED from NEVER-ATTEMPTED. This keeps the
# three states distinct and, above all, refuses the tempting repair: writing a
# boolean into an empty column would convert a visible gap into an invisible false
# record, which is strictly worse than the gap.
#
# EXPECT `UNMEASURED : 2290` AND rc 0 HERE. Unmeasured is counted and printed, never
# coerced to a zero or a pass, and never a failure by itself -- an honest blank is
# not an error, or this gate could not be wired at all. What DOES fail is a row
# asserting a parity its own operands cannot support, of which there are currently
# none. The gate exists to keep that count at zero.
echo "== compat-envelope: a stdout parity verdict must be re-derivable from its row =="
if ! python3 "${here}/stdout_operands.py" --root "${here}"; then
  echo "validate-envelope: parity assertion unsupported by its own operands" >&2
  fail=1
fi

# The tier gate REPORTS an unevidenced claim; this refuses to let one keep a
# qualifying tier at all. The six `full-stdout-info-stack-heap` rows that were the
# entire measured full-tier envelope had NO PRODUCER IN THE TREE -- their commit
# ddfd448 changed only scorecard.csv, zero code -- so they could not be re-derived
# and no producer fix could re-emit them. They were demoted to legacy-unqualified
# by the tool below rather than by hand, because a hand-edited row is exactly what
# created the problem and hand-repairing it would reproduce the defect while
# appearing to cure it.
#
# EXPECT 0 HERE. This is --check: it reports and refuses, it never rewrites during
# validation. Its criterion is computed, not a list -- a row is caught iff its tier
# is qualifying and tier_evidence cannot evidence it -- so it catches the NEXT one
# too. Remedy is `retire_unevidenced_tier_claims.py --apply`, which is idempotent.
echo "== compat-envelope: no qualifying tier may outlive the evidence for it =="
if ! python3 "${here}/retire_unevidenced_tier_claims.py" --root "${here}"; then
  echo "validate-envelope: a qualifying tier claim its own row cannot evidence" >&2
  fail=1
fi

# A parity boolean is accepted only when the row carries both hashed operands,
# exact code state, comparison/profile identity, and counted run coverage.  This
# is the same semantic verifier the renderer calls; labels and cached booleans
# are not a second authority.
echo "== compat-envelope: parity claims must carry full provenance =="
if ! "${here}/check-scorecard-provenance.py" \
      "${here}/history/scorecard-observations.csv"; then
  echo "validate-envelope: UNBOUND parity claims in scorecard observation history" >&2
  fail=1
fi
if [ "${do_reverie}" -eq 1 ] && [ -f "${here}/reverie-scorecard.csv" ]; then
  if ! "${here}/check-scorecard-provenance.py" "${here}/reverie-scorecard.csv" \
        --observable tool-count; then
    echo "validate-envelope: UNBOUND parity claims in reverie-scorecard.csv" >&2
    fail=1
  fi
fi
if [ "${do_reverie}" -eq 1 ] && [ -f "${here}/reverie-scorecard.csv" ]; then
  if ! "${here}/check-determinism-earned.sh" "${here}/reverie-scorecard.csv"; then
    echo "validate-envelope: UNEARNED or OVER-TIERED determinism claims in reverie-scorecard.csv" >&2
    fail=1
  fi
fi

if [ "${fail}" -ne 0 ]; then
  echo "validate-envelope: FAILED — see regressions above" >&2
  exit 1
fi

echo "== compat-envelope: ALL enabled cells green; per-run observations recorded =="
echo "   ${here}/scorecard.csv"
[ "${do_reverie}" -eq 1 ] && echo "   ${here}/reverie-scorecard.csv"
"${here}/render-scorecard.rs" --csv "${here}/scorecard.csv" --latest || true
