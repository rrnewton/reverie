#!/usr/bin/env bash
# The declaration must bite in BOTH directions, or it is decoration.
#
# A column blank on every published row has no producer. That state is invisible
# to every landed guard -- the schema guard sees the column in the header and is
# satisfied; check_cell_comparison.py iterates verdicts, finds none, and reports
# `0 of 0`, exit 0. It passes BECAUSE the producer is missing. It also cannot be
# mutation-tested: there is nothing to plant a wrong value in.
#
# So the schema declares which columns have no producer, and this pins that the
# declaration cannot rot in either direction:
#   * a value appearing in a declared-unimplemented column is REFUSED, so no cell
#     can quietly appear to satisfy a comparison nobody performs;
#   * a clean tree PASSES, so the guard is not refusing everything;
#   * removing a column from the declaration while it is still blank is allowed
#     ONLY together with a producer -- pinned here as: an undeclared blank column
#     is not refused, which is what makes deleting the declaration a deliberate
#     act rather than a silent one.

set -uo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ENV_DIR=$(dirname "$HERE")
GUARD="$ENV_DIR/check_unimplemented_comparisons.py"
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
checks=0; fails=0

ck() { # label expected_rc actual_rc
    checks=$((checks + 1))
    if [[ $2 -eq $3 ]]; then printf '  ok   %-58s rc=%s\n' "$1" "$3"
    else printf '  FAIL %-58s rc=%s want %s\n' "$1" "$3" "$2"; fails=$((fails + 1)); fi
}

# A minimal fixture root: schema + one scorecard, so the test never depends on
# the live corpus (which changes under it as cells are earned).
mk_root() { # $1=dir $2=stdout_parity_value
    mkdir -p "$1/compat-envelope"
    cat > "$1/compat-envelope/scorecard-schema.json" <<'JSON'
{
  "core": ["test_id", "outcome", "stdout_parity", "parity_tier"],
  "variants": {"scorecard.csv": {"emitter": "x", "extra_columns": [],
    "omits_core": [], "columns": ["test_id", "outcome", "stdout_parity", "parity_tier"],
    "core_prefix_safe": true}},
  "unimplemented_comparisons": {
    "columns_with_no_producer": {
      "stdout_parity": "no producer: blank on every row.",
      "parity_tier": "no producer: blank on every row."
    },
    "comparisons_with_no_column": {"exit_code": "NOT REPRESENTABLE. No column."}
  }
}
JSON
    printf 'test_id,outcome,stdout_parity,parity_tier\nt1,pass,%s,\n' "$2" \
        > "$1/compat-envelope/scorecard.csv"
}

# 1. Clean: both declared columns blank -> PASS. Guards that refuse everything
#    are as useless as guards that refuse nothing.
mk_root "$TMP/clean" ""
python3 "$GUARD" --root "$TMP/clean" >/dev/null 2>&1
ck "blank declared columns pass" 0 $?

# 2. A value in a declared-unimplemented column -> REFUSED.
mk_root "$TMP/planted" "1"
python3 "$GUARD" --root "$TMP/planted" >/dev/null 2>&1
ck "populated declared column is refused" 1 $?

# 3. The refusal names the column and the row, or it is not actionable.
out=$(python3 "$GUARD" --root "$TMP/planted" 2>&1)
checks=$((checks + 1))
if grep -q "stdout_parity" <<<"$out" && grep -q "scorecard.csv:2" <<<"$out"; then
    printf '  ok   %-58s\n' "refusal names the column and row"
else
    printf '  FAIL %-58s\n' "refusal names the column and row"; fails=$((fails + 1))
fi

# 4. The refusal must forbid the wrong fix explicitly. Blanking the value to
#    silence the guard restores the invisible state it exists to end.
checks=$((checks + 1))
if grep -qi "not blank the value\|Do not blank" <<<"$out"; then
    printf '  ok   %-58s\n' "refusal forbids blanking the value to silence it"
else
    printf '  FAIL %-58s\n' "refusal forbids blanking the value to silence it"; fails=$((fails + 1))
fi

# 5. A column NOT declared may carry values freely -- that is what "implemented"
#    looks like, and it is why deleting a declaration is the honest way to ship a
#    producer.
mkdir -p "$TMP/undeclared/compat-envelope"
cp "$TMP/clean/compat-envelope/scorecard-schema.json" "$TMP/undeclared/compat-envelope/"
python3 - "$TMP/undeclared/compat-envelope/scorecard-schema.json" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
del d["unimplemented_comparisons"]["columns_with_no_producer"]["stdout_parity"]
json.dump(d, open(p, "w"), indent=2)
PY
printf 'test_id,outcome,stdout_parity,parity_tier\nt1,pass,1,\n' \
    > "$TMP/undeclared/compat-envelope/scorecard.csv"
python3 "$GUARD" --root "$TMP/undeclared" >/dev/null 2>&1
ck "undeclared column may carry a value" 0 $?

# 6. An empty population must not pass. A guard whose corpus vanished should
#    report that, not report success over nothing.
mkdir -p "$TMP/empty/compat-envelope"
cp "$TMP/clean/compat-envelope/scorecard-schema.json" "$TMP/empty/compat-envelope/"
python3 "$GUARD" --root "$TMP/empty" >/dev/null 2>&1
ck "empty population is refused, not passed" 1 $?

echo
if ((fails == 0)); then
    echo "PASS ($checks checks)"
else
    echo "FAIL ($fails of $checks checks)"
    echo "A declared-unimplemented column must refuse a value and permit a blank."
    exit 1
fi
