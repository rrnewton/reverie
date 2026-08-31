#!/usr/bin/env bash
# A ratchet floor is meaningless against a population it was not measured over.
#
# The floors in collect-fullcorpus.sh were measured over a 235-cell corpus. 30
# performance sources were once absent from Hermit and nine rows named files
# that had been renamed, leaving 196 buildable cells. The gate went on comparing
# a 196-cell result against 235-cell floors and reported "NOT-COMPARABLE" rather
# than a result. That was the correct refusal, but the missing population meant
# the ratchet could never answer.
#
# The fix carries the denominator WITH the floor and refuses instead of lying.
# This test brackets all four outcomes, because the easy mistake is to silence
# the false red by making the gate incapable of ever firing.
#
# Usage: test_ratchet_floor_denominator.sh [collect-fullcorpus.sh]
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
COLLECT=${1:-$ROOT/compat-envelope/collect-fullcorpus.sh}
[ -x "$COLLECT" ] || { echo "no collector at $COLLECT" >&2; exit 2; }

TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT
pass=0; fail=0
ok()   { printf '  \033[32mok\033[0m    %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; fail=$((fail+1)); }
check(){ [ "$2" = "$3" ] && ok "$1 (exit $2)" || bad "$1: expected exit $3, got $2"; }

# The 19/20-column schema; only backend(11) and deterministic(14) are read.
hdr='run_id,run_utc,hermit_sha,reverie_sha,dirty,run_mode,lane,bucket,test_id,test_mode,backend,cell_state,outcome,deterministic,stdout_parity,output_hash,duration_ms,max_rss_kb,reason'
# backend:floor pairs as the collector declares them
FLOORS="ptrace:214 dbi:190 sabre:199 e9patch:214 liteinst:118"

synth() { # $1=out $2=cells-per-backend $3=extra det subtracted from each floor
  local out="$1" cells="$2" delta="$3" b f i det
  echo "$hdr" > "$out"
  for pair in $FLOORS; do
    b=${pair%%:*}; f=${pair##*:}
    for ((i=0; i<cells; i++)); do
      det=0; [ "$i" -lt "$((f - delta))" ] && det=1
      echo "r,@0,h,v,false,expansion,portable,bucket,$b/cell-$i,verify,$b,expansion,pass,$det,,,0,," >> "$out"
    done
  done
}

echo "ratchet floor vs denominator"

# A. the live shape: fewer cells than the floors were measured over.
synth "$TMP/short.csv" 205 0
"$COLLECT" --assert-only "$TMP/short.csv" >"$TMP/a.out" 2>&1; rc=$?
check "205-cell corpus refuses instead of reporting a regression" "$rc" 4
grep -q "NOT-COMPARABLE" "$TMP/a.out" && ok "says NOT-COMPARABLE" || bad "no NOT-COMPARABLE line"
grep -q "REGRESSION" "$TMP/a.out" && bad "still calls it a REGRESSION" || ok "does not call it a regression"
grep -q "unreachable by construction" "$TMP/a.out" \
  && ok "names the floors that 205 cells cannot reach" || bad "does not name the unreachable floors"

# B. full corpus, everything exactly at its floor: green.
synth "$TMP/green.csv" 235 0
"$COLLECT" --assert-only "$TMP/green.csv" >"$TMP/b.out" 2>&1; rc=$?
check "full corpus at floor is green" "$rc" 0
grep -q "NOT-COMPARABLE" "$TMP/b.out" && bad "spurious NOT-COMPARABLE on a full corpus" || ok "no spurious refusal"

# C. THE ONE THAT MATTERS: full corpus, one cell under. The gate must still fire,
#    or the fix above has merely disabled it.
synth "$TMP/red.csv" 235 1
"$COLLECT" --assert-only "$TMP/red.csv" >"$TMP/c.out" 2>&1; rc=$?
check "a real regression on a full corpus still FAILS" "$rc" 1
grep -q "REGRESSION" "$TMP/c.out" && ok "reports it as a REGRESSION" || bad "no REGRESSION line"

# D. no rows is a no-result, never a pass.
echo "$hdr" > "$TMP/empty.csv"
"$COLLECT" --assert-only "$TMP/empty.csv" >"$TMP/d.out" 2>&1; rc=$?
check "an empty scorecard is refused, not passed" "$rc" 3

echo
echo "ratchet floor denominator: $pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
echo "PASS"
