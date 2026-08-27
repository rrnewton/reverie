#!/usr/bin/env bash
# Brackets for the rule "a comparison VERDICT must carry the REFERENCE it compared against".
#
# The positive control is what makes the negatives honest: if a correctly
# referenced cell ever stopped passing, every refusal below would fire for the
# wrong reason and the guard would look effective while being merely broken.
#
# Each fixture is a whole synthetic scorecard tree, so the guard's own population
# discipline (explicit --root, stated pattern) is exercised rather than bypassed.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
GUARD="$ROOT/compat-envelope/check_cell_comparison.py"
[ -x "$GUARD" ] || { echo "no guard at $GUARD" >&2; exit 2; }

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
pass=0; fail=0
ok()  { printf '  \033[32mok\033[0m    %s\n' "$1"; pass=$((pass+1)); }
bad() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; fail=$((fail+1)); }
check() {
  if [ "$2" = "$3" ]; then
    ok "$1 (exit $2)"
  else
    bad "$1: expected exit $3, got $2"
  fi
}

# $1=name  $2=header  $3..=rows   -> creates a root and echoes it
mkroot() {
  local name="$1" header="$2"; shift 2
  local r="$TMP/$name"; mkdir -p "$r/compat-envelope"; ( cd "$r" && git init -q . 2>/dev/null )
  { echo "$header"; for row in "$@"; do echo "$row"; done; } > "$r/compat-envelope/scorecard.csv"
  echo "$r"
}

H_FULL="backend,test_id,stdout_parity,output_hash,ref_output_hash,exit_code_parity,detlog_parity,oracle_verdict"
H_NOREF="backend,test_id,stdout_parity,output_hash,exit_code_parity,detlog_parity,oracle_verdict"
H_NO_CAPABILITIES="backend,test_id,stdout_parity,output_hash,ref_output_hash"

echo "cell comparison-evidence guard"

# POSITIVE CONTROL -- a properly compared cell must pass UNCHANGED.
r=$(mkroot good "$H_FULL" "dbi,c/t1,1,aaa,aaa,,," "dbi,c/t2,0,bbb,ccc,,,")
"$GUARD" --root "$r" >"$TMP/good.out" 2>&1; check "a properly compared cell passes" "$?" 0
if grep -q "2 row(s).*2 verdict(s)" "$TMP/good.out"; then
  ok "positive reports rows and verdict denominator"
else
  bad "positive denominator missing"
fi

# THE VACUOUS CLASS: rows with blank verdicts are an honest no-result, but a
# whole non-empty population of them is a refusal rather than a passing check.
r=$(mkroot blank "$H_FULL" "dbi,c/t1,,aaa,,,," "dbi,c/t2,,,,,,,")
"$GUARD" --root "$r" >"$TMP/blank.out" 2>&1; check "rows but no verdicts are refused" "$?" 1
if grep -q "zero-verdict predicate REFUSED.*2 row(s)" "$TMP/blank.out"; then
  ok "zero-verdict refusal carries denominator"
else
  bad "zero-verdict denominator missing"
fi

# THE CLASS: a green with no reference must be REFUSED.
r=$(mkroot green_noref "$H_FULL" "dbi,c/t1,1,aaa,,,,")
"$GUARD" --root "$r" >"$TMP/g.out" 2>&1; check "a GREEN with no reference is refused" "$?" 1
if grep -q "carry NO reference" "$TMP/g.out"; then
  ok "names the failing rule"
else
  bad "no reason given"
fi

# A recorded MISMATCH also asserts a comparison happened.
r=$(mkroot fail_noref "$H_FULL" "dbi,c/t1,0,aaa,,,,")
"$GUARD" --root "$r" >/dev/null 2>&1; check "a FAIL verdict with no reference is refused" "$?" 1

# A schema with a verdict column but NO reference column cannot express a
# qualified cell at all -- a violation at the schema level, not just in data.
r=$(mkroot schema_noref "$H_NOREF" "dbi,c/t1,1,aaa,,,")
"$GUARD" --root "$r" >"$TMP/s.out" 2>&1; check "a schema that cannot express a reference is refused" "$?" 1
if grep -q "SCHEMA CANNOT EXPRESS" "$TMP/s.out"; then
  ok "distinguishes schema-level from row-level"
else
  bad "no schema-level reason"
fi

# A reference-capable stdout schema is still unable to express the other
# certification dimensions.  Report all of them instead of a false boolean.
r=$(mkroot schema_missing_dimensions "$H_NO_CAPABILITIES" "dbi,c/t1,1,aaa,aaa")
"$GUARD" --root "$r" >"$TMP/cap.out" 2>&1; check "missing exit/DETLOG/oracle capacity is refused" "$?" 1
for capability in exit_code detlog oracle; do
  if grep -q "$capability" "$TMP/cap.out"; then
    ok "reports missing $capability capacity"
  else
    bad "missing $capability was hidden"
  fi
done
"$GUARD" --root "$r" --json >"$TMP/cap.json" 2>"$TMP/cap.err"
python3 - "$TMP/cap.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
f = d["files"][0]
assert f["schema_cannot_express"] is True
assert f["missing_schema_capabilities"] == ["exit_code", "detlog", "oracle"]
PY
check "JSON reports schema_cannot_express=true with exact dimensions" "$?" 0

# One bad row among good ones must still fail, and the count must be k of N.
r=$(mkroot mixed "$H_FULL" "dbi,c/t1,1,aaa,aaa,,," "dbi,c/t2,1,bbb,,,," "dbi,c/t3,1,ccc,ccc,,,")
"$GUARD" --root "$r" >"$TMP/m.out" 2>&1; check "one unreferenced row among good ones fails" "$?" 1
if grep -q "1 of 3 verdict(s) lack a reference" "$TMP/m.out"; then
  ok "reports k of N"
else
  bad "count missing its denominator"
fi

# The population must be REFUSED, never reported clean, when it cannot be established.
r="$TMP/emptyroot"; mkdir -p "$r/compat-envelope"; ( cd "$r" && git init -q . 2>/dev/null )
"$GUARD" --root "$r" >/dev/null 2>&1; check "an empty population is REFUSED, not clean" "$?" 2

echo
echo "cell comparison guard: $pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
echo "PASS"
