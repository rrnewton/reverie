#!/usr/bin/env bash
#
# collect-reverie-compat.rs — backend coverage, typed absence, and idempotence.
#
# Hermetic: builds a throwaway hermit/reverie pair in $TMP with NO launchers
# built, so every cell resolves through the absence taxonomy rather than
# executing anything. That is exactly the property under test — the collector
# must emit a TYPED row for every known backend even when nothing can run.
#
# Bracketed on both sides per the Proxy Binding axis: each assertion below has a
# positive case (the expected token IS emitted, with a nonzero count) and, where
# it is meaningful, a negative case (the wrong token is NOT emitted).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
COLLECT="$ROOT/collect-reverie-compat.rs"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ok()   { echo "  ok: $*"; }

# --- fixture: a hermit/reverie pair with no built launchers -------------------
mkdir -p "$TMP/hermit" "$TMP/reverie/target/debug"
git -C "$TMP/hermit"  init -q  && git -C "$TMP/hermit"  commit -q --allow-empty -m init
git -C "$TMP/reverie" init -q  && git -C "$TMP/reverie" commit -q --allow-empty -m init
GUEST="$TMP/guest-elf"; : >"$GUEST"        # never executed; only its path is used

CSV="$TMP/out.csv"
RUN=(env RUST_SCRIPT_BASE_PATH="$ROOT" "$COLLECT"
     --repo "$TMP/hermit" --guest "$GUEST" --csv "$CSV"
     --run-id fixed-run --run-utc '@1700000000')

KNOWN=(ptrace kvm dbt sabre liteinst)
PROGRAMS=(counter1-true counter1-echo-hi counter1-pwd
          counter2-true counter2-echo-hi counter2-pwd)

col() { awk -F, -v n="$1" 'NR>1{print $n}' "$CSV"; }   # 1-indexed CSV column
named_col() { python3 - "$CSV" "$1" <<'PY'
import csv, sys
for row in csv.DictReader(open(sys.argv[1], newline="")):
    print(row[sys.argv[2]])
PY
}
count_backend_absence() { python3 - "$CSV" "$1" "$2" "$3" <<'PY'
import csv, sys
path, backend, absence, equal = sys.argv[1:]
rows = csv.DictReader(open(path, newline=""))
print(sum(row["backend"] == backend and
          ((row["absence_reason"] == absence) == (equal == "eq")) for row in rows))
PY
}

echo "== 1. every known backend x every program gets exactly one row =="
"${RUN[@]}" >/dev/null 2>&1
rows=$(($(wc -l <"$CSV") - 1))
[ "$rows" -eq 30 ] || fail "expected 30 rows (6 programs x 5 backends), got $rows"
ok "30 rows emitted"

for b in "${KNOWN[@]}"; do
  n=$(col 11 | grep -cx "$b" || true)
  [ "$n" -eq 6 ] || fail "backend $b has $n rows, expected 6"
done
ok "each of ${KNOWN[*]} has 6 rows"

for p in "${PROGRAMS[@]}"; do
  n=$(col 9 | grep -cx "$p" || true)
  [ "$n" -eq 5 ] || fail "program $p has $n rows, expected 5"
done
ok "each of the 6 programs has 5 rows (one per backend)"

echo "== 2. header carries exactly one named absence_reason column =="
# Derive the width and absence field FROM THE HEADER rather than hardcoding
# either its position or that it must be last. Parity fields may be appended to
# the shared schema after absence_reason; requiring a trailing field made this
# regression stale even though every reader below already resolves by name.
absence_columns=$(head -1 "$CSV" | awk -F, '{ n=0; for (i=1; i<=NF; i++) n += ($i == "absence_reason"); print n }')
[ "$absence_columns" -eq 1 ] \
  || fail "header must contain exactly one absence_reason column, got $absence_columns"
ncol=$(head -1 "$CSV" | awk -F, '{print NF}')
expected=$("$ROOT/check-scorecard-schema.rs" --header reverie-scorecard.csv | awk -F, '{print NF}')
[ -n "$expected" ] || fail "cannot read the shared reverie scorecard row"
[ "$ncol" -eq "$expected" ] || fail "header width $ncol != collector HEADER width $expected"
ok "header width $ncol matches the full provenance collector contract"
HEADER=$(head -1 "$CSV")

echo "== 3. no blank ambiguous cells: unmeasured => typed token =="
# Every row here is unmeasured (nothing is built), so every absence_reason must
# be one of the four tokens. Positive: count > 0. Negative: zero empties.
blank=$(named_col absence_reason | grep -c '^$' || true)
[ "$blank" -eq 0 ] || fail "$blank row(s) have a BLANK absence_reason but were not measured"
ok "0 blank absence_reason cells"
bad=$(named_col absence_reason | grep -vcE '^(not_collected|unsupported|unavailable|no_result)$' || true)
[ "$bad" -eq 0 ] || fail "$bad row(s) carry an absence_reason outside the taxonomy"
ok "all 30 absence_reason values are in the taxonomy"
tiered=$(named_col comparison_tier | grep -cx 'unqualified-tool-count-only' || true)
[ "$tiered" -eq 30 ] || fail "expected 30 explicit tool-count-only tiers, got $tiered"
ok "30/30 rows state comparison_tier=unqualified-tool-count-only (never strict green)"

echo "== 4. absence taxonomy is assigned to the right cause =="
# unsupported: dbt/sabre/liteinst have no launcher entry for either tool.
for b in dbt sabre liteinst; do
  n=$(count_backend_absence "$b" unsupported eq)
  [ "$n" -eq 6 ] || fail "$b: expected 6 'unsupported' rows, got $n"
done
ok "dbt/sabre/liteinst => unsupported (no launcher), 18 rows"
# NEGATIVE: they must NOT be mislabelled as a failure-ish or not-asked token.
n=0
for b in dbt sabre liteinst; do
  wrong=$(count_backend_absence "$b" unsupported ne)
  n=$((n + wrong))
done
[ "$n" -eq 0 ] || fail "$n dbt/sabre/liteinst row(s) carry the wrong token"
ok "negative: 0 dbt/sabre/liteinst rows carry any other token"

# unavailable: ptrace IS supported+requested, but the launcher is not built.
n=$(count_backend_absence ptrace unavailable eq)
[ "$n" -eq 6 ] || fail "ptrace: expected 6 'unavailable' rows, got $n"
ok "ptrace => unavailable (launcher not built), 6 rows"
grep -q 'launcher not built' "$CSV" || fail "unavailable rows must say WHY in the reason column"
ok "unavailable rows carry a human reason"

echo "== 5. not_collected appears iff a supported backend is left out =="
# Ask for ptrace only: kvm is supported by both tools but unrequested.
"${RUN[@]}" --backends ptrace >/dev/null 2>&1
n=$(count_backend_absence kvm not_collected eq)
[ "$n" -eq 6 ] || fail "expected 6 kvm 'not_collected' rows when only ptrace requested, got $n"
ok "kvm => not_collected when unrequested (6 rows)"
# NEGATIVE: with kvm requested it must NOT be not_collected.
"${RUN[@]}" --backends ptrace,kvm >/dev/null 2>&1
n=$(count_backend_absence kvm not_collected eq)
[ "$n" -eq 0 ] || fail "kvm still 'not_collected' after being requested ($n rows)"
ok "negative: 0 kvm not_collected rows once requested"
# Row count is invariant to the requested set — absence is visible, not silent.
rows=$(($(wc -l <"$CSV") - 1))
[ "$rows" -eq 30 ] || fail "row count changed with --backends; absence must stay visible ($rows)"
ok "row count stays 30 regardless of --backends"

echo "== 6. DBT terminology with legacy DBI read compatibility =="
out=$("${RUN[@]}" --backends dbi 2>&1 >/dev/null || true)
grep -q 'dbi->dbt' <<<"$out" || fail "legacy 'dbi' must be reported as aliased to dbt"
ok "--backends dbi is announced as dbi->dbt"
n=$(col 11 | grep -cx dbi || true)
[ "$n" -eq 0 ] || fail "legacy name 'dbi' must never be EMITTED ($n rows)"
ok "negative: 'dbi' never appears in the backend column"
n=$(col 11 | grep -cx dbt || true)
[ "$n" -eq 6 ] || fail "canonical 'dbt' should have 6 rows, got $n"
ok "canonical 'dbt' emitted (6 rows)"

echo "== 7. unknown backend is refused in-band, not silently dropped =="
out=$("${RUN[@]}" --backends ptrace,nosuchbackend 2>&1 >/dev/null || true)
grep -q 'not in the known vocabulary' <<<"$out" || fail "unknown backend must warn"
ok "unknown backend warns and names the vocabulary"

echo "== 8. regeneration is idempotent (byte-identical, no duplicates) =="
"${RUN[@]}" >/dev/null 2>&1; a=$(sha256sum <"$CSV" | cut -d' ' -f1); ra=$(($(wc -l <"$CSV") - 1))
"${RUN[@]}" >/dev/null 2>&1; b=$(sha256sum <"$CSV" | cut -d' ' -f1); rb=$(($(wc -l <"$CSV") - 1))
"${RUN[@]}" >/dev/null 2>&1; c=$(sha256sum <"$CSV" | cut -d' ' -f1); rc=$(($(wc -l <"$CSV") - 1))
[ "$a" = "$b" ] && [ "$b" = "$c" ] || fail "3 runs produced differing bytes: $a / $b / $c"
[ "$ra" -eq 30 ] && [ "$rb" -eq 30 ] && [ "$rc" -eq 30 ] \
  || fail "row count drifted across runs: $ra / $rb / $rc"
ok "3 consecutive runs are byte-identical at 30 rows"

echo "== 9. --append restores accumulating behaviour (control for #8) =="
"${RUN[@]}" --append >/dev/null 2>&1
rows=$(($(wc -l <"$CSV") - 1))
[ "$rows" -eq 60 ] || fail "--append should have grown 30 -> 60 rows, got $rows"
ok "--append grows the file, proving #8 measured replacement not a no-op"

echo "== 10. foreign buckets are preserved, never clobbered =="
: >"$CSV"
head -1 <<<"$(env RUST_SCRIPT_BASE_PATH="$ROOT" "$COLLECT" --help 2>&1)" >/dev/null || true
python3 - "$CSV" "$HEADER" <<'PY'
import csv, sys
path, header = sys.argv[1], sys.argv[2].split(",")
row = {column: "" for column in header}
row.update(
    run_id="r", run_utc="@1", hermit_sha="h", reverie_sha="rv",
    dirty="false", run_mode="regression", lane="portable", bucket="c-programs",
    test_id="someone-elses-test", test_mode="strict", backend="ptrace",
    cell_state="enabled", outcome="pass", deterministic="1", output_hash="abc",
    duration_ms="10", reason="foreign",
)
with open(path, "w", newline="") as handle:
    writer = csv.DictWriter(handle, fieldnames=header)
    writer.writeheader()
    writer.writerow(row)
PY
"${RUN[@]}" >/dev/null 2>&1
grep -q 'someone-elses-test' "$CSV" || fail "a foreign bucket's row was destroyed by the rewrite"
ok "foreign c-programs row survived the reverie-examples rewrite"
n=$(awk -F, 'NR>1 && $8=="reverie-examples"' "$CSV" | wc -l)
[ "$n" -eq 30 ] || fail "expected 30 reverie-examples rows beside the foreign row, got $n"
ok "30 reverie rows written alongside it"

echo
echo "PASS: all 10 checks green."
