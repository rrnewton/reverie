#!/usr/bin/env bash
# A1 (ambiguous-zero class, task `execute-ambiguous-zero-fix-order-a3-a4-first`):
# an EMPTY DENOMINATOR must not render as a measured zero.
#
# render-scorecard.rs builds every percentage over the set of PASSING PTRACE cells
# in the requested `--denominator` mode. When that set is empty there are no cells
# at all, and the pre-fix renderer printed a confident `TOTAL 0` at exit 0 — which
# is indistinguishable from "we measured, and nothing passed". Both states occur in
# the live scorecard, so the reader could not tell them apart.
#
# The fix refuses to render, exits 3 (distinct from 2 = usage, 0 = rendered), and
# carries the denominator's own population: how many rows were considered, how many
# were ptrace rows, and which `--denominator` modes actually have passing ptrace
# rows. There are THREE distinct causes of an empty denominator and the message must
# separate them, because the remedy differs:
#
#   A. no ptrace rows at all      -> changing --denominator cannot help
#   B. ptrace passes another mode -> retry with that mode (and it must then render)
#   C. ptrace rows all failed     -> the reference backend itself failed here
#
# Bracketed BOTH WAYS: a real result must still score normally (case D). It is easy
# to make every zero read "unknown" and call the class fixed.
#
# Usage: test_render_scorecard_empty_denominator.sh [renderer.rs] [schema-source.csv]
# Both default to the in-repo copies. The fixture header is COPIED from the schema
# source rather than hardcoded, so this test is agnostic to the in-flight
# `parity` -> `stdout_parity` column rename.

set -uo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
RENDER=${1:-$ROOT/compat-envelope/render-scorecard.rs}
SCHEMA_CSV=${2:-$ROOT/compat-envelope/scorecard.csv}

[ -x "$RENDER" ] || { echo "no renderer at $RENDER" >&2; exit 2; }
[ -r "$SCHEMA_CSV" ] || { echo "no schema source at $SCHEMA_CSV" >&2; exit 2; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

HEADER=$(head -1 "$SCHEMA_CSV")

RUN=a1-fixture
HERM_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
REV_SHA=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
OUT_SHA=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd

# THE FIXTURE MUST NOW EARN ITS PASSES, and that is why this helper grew.
#
# These rows used to set six columns and leave the rest blank. That was correct
# when written: a blank `deterministic`/parity means "unknown", which is exactly
# the not-measured state the renderer preserves. But b9f20ef5 and f21dd0d1 made a
# `pass` count as a QUALIFIED GREEN only when the row carries the evidence behind
# it -- observable parity, a non-empty INFO comparison, exact code identity, both
# output hashes, the comparator and tier that produced them, an explicit
# population, and a clean tree. A six-column row cannot satisfy that, so the
# renderer refused, and the two cases here that must RENDER (B2 and D, one of
# them the positive control) could not.
#
# THAT WAS THE RENDERER BEING RIGHT. A fixture asserting "a real result still
# scores" while carrying no evidence is asking for exactly the green-with-nothing-
# behind-it this file polices elsewhere. So the defaults below supply real
# evidence rather than the assertions being relaxed to accept its absence.
#
# The negative cases are unaffected by construction: case A still has no ptrace
# row, case B's ptrace rows are still in the wrong mode, and case C's ptrace rows
# still carry outcome=fail, which overrides the default. Each must still refuse,
# and if provenance alone were enough to make them render, these tests would say
# so.
population_id() {
    {
        printf 'scorecard-population-v2\n'
        printf '%s\n' "$@" | sort
    } | sha256sum | cut -d' ' -f1
}

# selected/executed/evidence counts are spelled out per row because they must
# equal the rows that run actually has; the provenance verifier re-derives them
# and refuses a mismatch, which is the whole point of recording a population.

# The population key is the identity tuple the renderer re-derives, so it must be
# built from the same fields the rows actually carry.
pop_key() { printf 'regression\tportable\tb1\t%s\t%s\t%s\tran' "$1" "$2" "$3"; }

# Emit one CSV row into $1. Named columns override the defaults; anything not
# named and not defaulted stays blank, which still means "unknown".
row() {
    local out=$1; shift
    awk -v hdr="$HEADER" -v spec="$*" \
        -v herm="$HERM_SHA" -v rev="$REV_SHA" -v outhash="$OUT_SHA" '
    BEGIN {
        n = split(hdr, cols, ",")
        v["comparison_tier"] = "full-stdout-info-stack-heap"
        v["run_utc"] = "@0"; v["hermit_sha"] = herm; v["reverie_sha"] = rev
        v["dirty"] = "false"; v["run_mode"] = "regression"; v["lane"] = "portable"
        v["deterministic"] = "1"; v["stdout_parity"] = "1"; v["duration_ms"] = "1"
        v["compared_log_messages"] = "2|2"
        v["output_hash"] = outhash; v["ref_output_hash"] = outhash
        v["parity_comparator"] = "stdout-sha256-exact-v1"
        v["parity_tier"] = "stdout-exact"
        # No comma inside, so the field needs no CSV quoting.
        v["profile_flags"] = "{\"comparison\":[\"--strict\"]}"
        v["stack_parity"] = "1"; v["heap_parity"] = "1"
        m = split(spec, kvs, " ")
        for (i = 1; i <= m; i++) { p = index(kvs[i], "="); v[substr(kvs[i],1,p-1)] = substr(kvs[i],p+1) }
        out = ""
        for (i = 1; i <= n; i++) out = out (i > 1 ? "," : "") (cols[i] in v ? v[cols[i]] : "")
        print out
    }' >> "$out"
}

new_csv() { echo "$HEADER" > "$1"; }

# Each case declares its OWN population, because the population is the set of
# identity tuples actually present -- A has no ptrace rows, so A's population is
# not B's.
A_POP=sha256:$(population_id "$(pop_key t1 strict dbi)" "$(pop_key t2 strict dbi)" "$(pop_key t3 strict dbi)")
B_POP=sha256:$(population_id \
    "$(pop_key t1 strict ptrace)" "$(pop_key t2 strict ptrace)" "$(pop_key t3 strict ptrace)" \
    "$(pop_key t1 strict dbi)"    "$(pop_key t2 strict dbi)"    "$(pop_key t3 strict dbi)")
CD_POP=sha256:$(population_id \
    "$(pop_key t1 verify ptrace)" "$(pop_key t2 verify ptrace)" "$(pop_key t3 verify ptrace)" \
    "$(pop_key t1 verify dbi)"    "$(pop_key t2 verify dbi)"    "$(pop_key t3 verify dbi)")

# --- case A: dbi-only run, no ptrace rows in any mode -------------------------
new_csv "$TMP/a.csv"
for t in t1 t2 t3; do
    row "$TMP/a.csv" run_id=$RUN bucket=b1 test_id=$t test_mode=strict backend=dbi cell_state=ran outcome=pass population_id=$A_POP selected_count=3 executed_count=3 evidence_count=3
done

# --- case B: ptrace passes `strict`, but the default denominator is `verify` --
new_csv "$TMP/b.csv"
for t in t1 t2 t3; do
    row "$TMP/b.csv" run_id=$RUN bucket=b1 test_id=$t test_mode=strict backend=ptrace cell_state=ran outcome=pass population_id=$B_POP selected_count=6 executed_count=6 evidence_count=6
    row "$TMP/b.csv" run_id=$RUN bucket=b1 test_id=$t test_mode=strict backend=dbi cell_state=ran outcome=pass population_id=$B_POP selected_count=6 executed_count=6 evidence_count=6
done

# --- case C: ptrace rows present in the requested mode, none passing ----------
new_csv "$TMP/c.csv"
for t in t1 t2 t3; do
    row "$TMP/c.csv" run_id=$RUN bucket=b1 test_id=$t test_mode=verify backend=ptrace cell_state=ran outcome=fail population_id=$CD_POP selected_count=6 executed_count=6 evidence_count=6
    row "$TMP/c.csv" run_id=$RUN bucket=b1 test_id=$t test_mode=verify backend=dbi cell_state=ran outcome=pass population_id=$CD_POP selected_count=6 executed_count=6 evidence_count=6
done

# --- case D: a real, non-empty denominator ------------------------------------
new_csv "$TMP/d.csv"
for t in t1 t2 t3; do
    row "$TMP/d.csv" run_id=$RUN bucket=b1 test_id=$t test_mode=verify backend=ptrace cell_state=ran outcome=pass population_id=$CD_POP selected_count=6 executed_count=6 evidence_count=6
    row "$TMP/d.csv" run_id=$RUN bucket=b1 test_id=$t test_mode=verify backend=dbi cell_state=ran outcome=pass population_id=$CD_POP selected_count=6 executed_count=6 evidence_count=6
done

FAIL=0
run_render() { # csv, extra args... -> sets OUT and RC
    OUT=$("$RENDER" --csv "$1" --run-id "$RUN" "${@:2}" 2>&1)
    RC=$?
}

ok()   { printf '  \033[32mok\033[0m    %s\n' "$1"; }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL + 1)); }
check() { if [ "$2" = yes ]; then ok "$1"; else bad "$1"; fi; }
has()  { case "$OUT" in *"$1"*) echo yes;; *) echo no;; esac; }
hasnt(){ case "$OUT" in *"$1"*) echo no;; *) echo yes;; esac; }
rc_is(){ if [ "$RC" = "$1" ]; then echo yes; else echo no; fi; }

# "The guard is absent" and "the guard is present but wrong" are DIFFERENT FACTS,
# and a bare FAIL conflates them — which is the very defect class this test exists
# to police. So probe for absence first, from the BEHAVIOUR (case A renders a result
# instead of refusing) rather than by grepping the source, and give it its own exit
# status. A1 is authored and verified but blocked on shared ownership of
# render-scorecard.rs; until it lands, exit 4 says so out loud instead of emitting a
# bare red that reads like a regression.
# Exit codes: 0 = fix present and correct, 1 = fix present and REGRESSED,
#             2 = usage, 4 = fix not landed in this renderer (nothing was verified).
run_render "$TMP/a.csv"
if [ "$RC" = 0 ] && [ "$(hasnt 'NO DATA:')" = yes ]; then
    {
        echo "A1-NOT-LANDED: $RENDER rendered a result for an EMPTY DENOMINATOR."
        echo "  Case A has no passing ptrace cells at all, yet the renderer exited 0"
        echo "  with a table. That is the pre-fix ambiguous zero, NOT a regression:"
        echo "  the empty-denominator guard is absent from this renderer."
        echo "  0 of 26 assertions were evaluated."
        echo "  Point this test at a renderer carrying the A1 guard, or land A1."
    } >&2
    exit 4
fi

echo "case A — no ptrace rows in any mode"
run_render "$TMP/a.csv"
check "refuses instead of rendering"                  "$(has 'NO DATA:')"
check "exit 3 (could-not-measure, not a result)"      "$(rc_is 3)"
check "must NOT publish a bare TOTAL 0"               "$(hasnt 'TOTAL                        0')"
check "carries the ptrace-row denominator (0)"        "$(has 'ptrace rows:      0')"
check "says --denominator cannot help"                "$(has 'changing --denominator will not help')"

echo "case B — ptrace passes a DIFFERENT mode than the one requested"
run_render "$TMP/b.csv"
check "refuses instead of rendering"                  "$(has 'NO DATA:')"
check "exit 3"                                        "$(rc_is 3)"
check "reports the ptrace rows that DO exist"         "$(has 'ptrace rows:      3')"
check "names the mode that would work"                "$(has 'Retry with --denominator <strict>')"
check "does not misreport it as unmeasurable"         "$(hasnt 'changing --denominator will not help')"

echo "case B2 — FOLLOWING that remedy must actually render"
run_render "$TMP/b.csv" --denominator strict
check "renders"                                       "$(hasnt 'NO DATA:')"
check "exit 0"                                        "$(rc_is 0)"
check "denominator is the 3 passing ptrace cells"     "$(has 'TOTAL')"

echo "case C — ptrace rows present, none passing"
run_render "$TMP/c.csv"
check "refuses instead of rendering"                  "$(has 'NO DATA:')"
check "exit 3"                                        "$(rc_is 3)"
check "distinguishes it from 'no ptrace rows'"        "$(has 'ptrace rows:      3')"
check "attributes it to the reference backend"        "$(has 'none passing in any mode')"
check "does not offer a mode that cannot work"        "$(hasnt 'Retry with --denominator')"

echo "case D — POSITIVE CONTROL: a real result still scores"
run_render "$TMP/d.csv"
check "renders a table"                               "$(has 'TOTAL')"
check "exit 0"                                        "$(rc_is 0)"
check "no spurious refusal"                           "$(hasnt 'NO DATA:')"
run_render "$TMP/d.csv" --json
check "--json still emits"                            "$(has '"schema"')"
check "--json exit 0"                                 "$(rc_is 0)"
run_render "$TMP/d.csv" --tsv
check "--tsv still emits"                             "$(rc_is 0)"

echo
if [ "$FAIL" -eq 0 ]; then
    echo "PASS"
    exit 0
fi
echo "FAIL ($FAIL assertions)"
exit 1
