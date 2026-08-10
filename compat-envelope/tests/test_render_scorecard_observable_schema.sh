#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
RENDER=${1:-$ROOT/compat-envelope/render-scorecard.rs}
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

COMMON_PREFIX=run_id,run_utc,hermit_sha,reverie_sha,dirty,run_mode,lane,bucket,test_id,test_mode,backend,cell_state,outcome,deterministic
HERM_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
REV_SHA=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
OUT_SHA=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
population_id() {
  {
    printf 'scorecard-population-v2\n'
    printf '%s\n' "$@" | sort
  } | sha256sum | cut -d' ' -f1
}
STDOUT_POP=sha256:$(population_id \
  $'regression\tportable\tb\tt\tverify\tdbi\tenabled' \
  $'regression\tportable\tb\tt\tverify\tptrace\tenabled')
TOOL_POP=sha256:$(population_id \
  $'reverie\tportable\tb\tt\tcounter\tkvm\tenabled' \
  $'reverie\tportable\tb\tt\tcounter\tptrace\tenabled')
POP=$STDOUT_POP # malformed-schema fixtures below are refused before population use
STRICT_TIER=full-stdout-info-stack-heap
COMMON_SUFFIX=output_hash,duration_ms,max_rss_kb,reason,compared_log_messages,ref_output_hash,parity_comparator,parity_tier,profile_flags,population_id,selected_count,executed_count,evidence_count,comparison_tier,stack_parity,heap_parity

write_stdout_csv() {
    local path=$1 column=$2
    cat >"$path" <<EOF
$COMMON_PREFIX,$column,$COMMON_SUFFIX
r,@0,$HERM_SHA,$REV_SHA,false,regression,portable,b,t,verify,ptrace,enabled,pass,1,1,$OUT_SHA,1,,reference,2|2,$OUT_SHA,stdout-sha256-exact-v1,stdout-exact,"{""comparison"" : [""run"",""--strict""]}",$STDOUT_POP,2,2,2,$STRICT_TIER,1,1
r,@0,$HERM_SHA,$REV_SHA,false,regression,portable,b,t,verify,dbi,enabled,pass,1,1,$OUT_SHA,1,,match,2|2,$OUT_SHA,stdout-sha256-exact-v1,stdout-exact,"{""comparison"" : [""run"",""--strict""]}",$STDOUT_POP,2,2,2,$STRICT_TIER,1,1
EOF
}

write_tool_csv() {
    local path=$1 column=$2
    cat >"$path" <<EOF
$COMMON_PREFIX,$column,$COMMON_SUFFIX
r,@0,$HERM_SHA,$REV_SHA,false,reverie,portable,b,t,counter,ptrace,enabled,pass,1,1,$OUT_SHA,1,,reference,2|2,$OUT_SHA,tool-count-sha256-exact-v1,tool-count-exact,"{""comparison"" : [""counter""],""collector"" : [""--reps"",""2""]}",$TOOL_POP,2,2,2,$STRICT_TIER,1,1
r,@0,$HERM_SHA,$REV_SHA,false,reverie,portable,b,t,counter,kvm,enabled,pass,1,1,$OUT_SHA,1,,match,2|2,$OUT_SHA,tool-count-sha256-exact-v1,tool-count-exact,"{""comparison"" : [""counter""],""collector"" : [""--reps"",""2""]}",$TOOL_POP,2,2,2,$STRICT_TIER,1,1
EOF
}

write_stdout_csv "$TMP/stdout-current.csv" stdout_parity
write_stdout_csv "$TMP/stdout-legacy.csv" parity
write_tool_csv "$TMP/tool-current.csv" tool_count_parity
write_tool_csv "$TMP/tool-legacy.csv" parity

"$RENDER" --csv "$TMP/stdout-current.csv" --all --backends dbi --json >"$TMP/stdout-current.json"
"$RENDER" --csv "$TMP/stdout-legacy.csv" --all --backends dbi --json >"$TMP/stdout-legacy.json"
jq -S 'del(.source_csv)' "$TMP/stdout-current.json" >"$TMP/stdout-current.normalized.json"
jq -S 'del(.source_csv)' "$TMP/stdout-legacy.json" >"$TMP/stdout-legacy.normalized.json"
cmp "$TMP/stdout-current.normalized.json" "$TMP/stdout-legacy.normalized.json"
jq -e '.rows[-1].backends.dbi.stdout_parity_count == 1' "$TMP/stdout-current.json" >/dev/null
jq -e '.comparison_tier_distribution["full-stdout-info-stack-heap"] == 2
       and .qualified_green_count == 2 and .highest_grade_green_count == 2
       and .raw_pass_count == 2' \
    "$TMP/stdout-current.json" >/dev/null

# A qualifying tier is a claim, not evidence. Keep the tier and stdout match
# intact while removing the INFO operand; both blank and zero-record forms must
# produce zero greens rather than self-certify from the label.
for info_case in blank zero; do
    if [ "$info_case" = blank ]; then
        sed 's/,2|2,/,,/' "$TMP/stdout-current.csv" >"$TMP/info-$info_case.csv"
    else
        sed 's/,2|2,/,0|0,/' "$TMP/stdout-current.csv" >"$TMP/info-$info_case.csv"
    fi
    set +e
    "$RENDER" --csv "$TMP/info-$info_case.csv" --all --backends dbi --json \
        >"$TMP/info-$info_case.json" 2>"$TMP/info-$info_case.err"
    info_rc=$?
    set -e
    [ "$info_rc" -eq 3 ] || {
        echo "info-$info_case: expected no-data exit 3, got $info_rc" >&2
        cat "$TMP/info-$info_case.err" >&2
        exit 1
    }
    grep -F 'qualified green=0/2 raw passes' "$TMP/info-$info_case.err" >/dev/null
    jq -e '.qualified_green_count == 0 and .highest_grade_green_count == 0
           and .raw_pass_count == 2' "$TMP/info-$info_case.json" >/dev/null
done

# Blank is historical unknown, false is a measured non-highest-grade result;
# neither may be counted as a high-grade pass.
sed '2s/,1,1$/,,/' "$TMP/stdout-current.csv" >"$TMP/stdout-blank-memory.csv"
"$RENDER" --csv "$TMP/stdout-blank-memory.csv" --all --backends dbi --json \
    >"$TMP/stdout-blank-memory.json"
jq -e '.highest_grade_green_count == 1 and .qualified_green_count == 2' \
    "$TMP/stdout-blank-memory.json" >/dev/null
sed '2s/,1,1$/,0,0/' "$TMP/stdout-current.csv" >"$TMP/stdout-false-memory.csv"
"$RENDER" --csv "$TMP/stdout-false-memory.csv" --all --backends dbi --json \
    >"$TMP/stdout-false-memory.json"
jq -e '.highest_grade_green_count == 1 and .qualified_green_count == 2' \
    "$TMP/stdout-false-memory.json" >/dev/null

"$RENDER" --csv "$TMP/tool-current.csv" --all --denominator counter \
    --backends kvm --observable tool-count --json >"$TMP/tool-current.json"
"$RENDER" --csv "$TMP/tool-legacy.csv" --all --denominator counter \
    --backends kvm --observable tool-count --json >"$TMP/tool-legacy.json"
jq -S 'del(.source_csv)' "$TMP/tool-current.json" >"$TMP/tool-current.normalized.json"
jq -S 'del(.source_csv)' "$TMP/tool-legacy.json" >"$TMP/tool-legacy.normalized.json"
cmp "$TMP/tool-current.normalized.json" "$TMP/tool-legacy.normalized.json"
jq -e '.rows[-1].backends.kvm.tool_count_parity_count == 1' "$TMP/tool-current.json" >/dev/null

expect_refused() {
    local name=$1 needle=$2
    shift 2
    set +e
    "$RENDER" "$@" >"$TMP/$name.out" 2>&1
    local rc=$?
    set -e
    if [ "$rc" -ne 2 ]; then
        echo "$name: expected refusal exit 2, got $rc" >&2
        cat "$TMP/$name.out" >&2
        exit 1
    fi
    grep -F "$needle" "$TMP/$name.out" >/dev/null || {
        echo "$name: refusal did not name '$needle'" >&2
        cat "$TMP/$name.out" >&2
        exit 1
    }
}

# A current schema is bound to its declared observable, not merely to a boolean
# in the same ordinal position.
# Backticks are literal renderer diagnostics.
# shellcheck disable=SC2016
expect_refused stdout-as-tool 'found `stdout_parity`, expected `tool_count_parity`' \
    --csv "$TMP/stdout-current.csv" --observable tool-count
# Backticks are literal renderer diagnostics.
# shellcheck disable=SC2016
expect_refused tool-as-stdout 'found `tool_count_parity`, expected `stdout_parity`' \
    --csv "$TMP/tool-current.csv"

# A transition file carrying both spellings is ambiguous and must not silently
# prefer whichever column happens to appear first.
cat >"$TMP/ambiguous.csv" <<EOF
$COMMON_PREFIX,stdout_parity,parity,$COMMON_SUFFIX
r,@0,$HERM_SHA,$REV_SHA,false,regression,portable,b,t,verify,ptrace,enabled,pass,1,1,0,$OUT_SHA,1,,conflict,2|2,$OUT_SHA,stdout-sha256-exact-v1,stdout-exact,"{""comparison"" : [""run"",""--strict""]}",$POP,1,1,1,$STRICT_TIER
EOF
# Backticks are literal renderer diagnostics.
# shellcheck disable=SC2016
expect_refused ambiguous 'expected exactly one parity observable column' \
    --csv "$TMP/ambiguous.csv"

cat >"$TMP/missing.csv" <<EOF
$COMMON_PREFIX,comparison,$COMMON_SUFFIX
r,@0,$HERM_SHA,$REV_SHA,false,regression,portable,b,t,verify,ptrace,enabled,pass,1,1,$OUT_SHA,1,,missing,2|2,$OUT_SHA,stdout-sha256-exact-v1,stdout-exact,"{""comparison"" : [""run"",""--strict""]}",$POP,1,1,1,$STRICT_TIER
EOF
# Backticks are literal renderer diagnostics.
# shellcheck disable=SC2016
expect_refused missing 'expected exactly one parity observable column' --csv "$TMP/missing.csv"

# Plant the exact integrity violation: the same passing rows with an empty tier
# must be refused, never defaulted to the strict fixture's prior value.
sed "s/,$STRICT_TIER,1,1$/,,1,1/" "$TMP/stdout-current.csv" >"$TMP/tierless.csv"
expect_refused tierless 'has no comparison_tier' \
    --csv "$TMP/tierless.csv" --all --backends dbi

# An explicit weaker tier is retained as history but cannot emit a green.  Both
# raw passes remain visible in the refusal, with a zero qualified denominator.
sed "s/,$STRICT_TIER,1,1$/,legacy-unqualified,1,1/" \
    "$TMP/stdout-current.csv" >"$TMP/legacy-unqualified.csv"
set +e
"$RENDER" --csv "$TMP/legacy-unqualified.csv" --all --backends dbi \
    >"$TMP/legacy-unqualified.out" 2>&1
legacy_rc=$?
set -e
[ "$legacy_rc" -eq 3 ] || {
    echo "legacy-unqualified: expected no-data exit 3, got $legacy_rc" >&2
    cat "$TMP/legacy-unqualified.out" >&2
    exit 1
}
grep -F 'qualified green=0/2 raw passes' "$TMP/legacy-unqualified.out" >/dev/null
grep -F '0 ptrace/verify qualifying passing cells' "$TMP/legacy-unqualified.out" >/dev/null

# A self-verify tier is a known provenance class, but within-backend
# repeatability must never be counted as cross-backend stdout parity.
sed "s/,$STRICT_TIER,1,1$/,unqualified-self-verify-only,1,1/" \
    "$TMP/stdout-current.csv" >"$TMP/self-verify-only.csv"
set +e
"$RENDER" --csv "$TMP/self-verify-only.csv" --all --backends dbi \
    --json >"$TMP/self-verify-only.json" 2>"$TMP/self-verify-only.err"
self_verify_rc=$?
set -e
[ "$self_verify_rc" -eq 3 ] || {
    echo "self-verify-only: expected no-data exit 3, got $self_verify_rc" >&2
    cat "$TMP/self-verify-only.err" >&2
    exit 1
}
jq -e '.comparison_tier_distribution["unqualified-self-verify-only"] == 2
       and .qualified_green_count == 0 and .raw_pass_count == 2' \
    "$TMP/self-verify-only.json" >/dev/null

# Even a malformed producer that pairs stdout_parity=1 with "no comparison"
# cannot launder that unsupported boolean into a qualified green.
sed "s/,$STRICT_TIER,1,1$/,unqualified-no-comparison,1,1/" \
    "$TMP/stdout-current.csv" >"$TMP/no-comparison.csv"
set +e
"$RENDER" --csv "$TMP/no-comparison.csv" --all --backends dbi \
    --json >"$TMP/no-comparison.json" 2>"$TMP/no-comparison.err"
no_comparison_rc=$?
set -e
[ "$no_comparison_rc" -eq 3 ] || {
    echo "no-comparison: expected no-data exit 3, got $no_comparison_rc" >&2
    cat "$TMP/no-comparison.err" >&2
    exit 1
}
jq -e '.comparison_tier_distribution["unqualified-no-comparison"] == 2
       and .qualified_green_count == 0 and .raw_pass_count == 2' \
    "$TMP/no-comparison.json" >/dev/null

echo "observable schema: 4 positive current/legacy reads; 7 evidence/schema/tier violations refused; blank and 0|0 INFO plus legacy, self-verify, and no-comparison tiers each credited 0/2 raw passes"
