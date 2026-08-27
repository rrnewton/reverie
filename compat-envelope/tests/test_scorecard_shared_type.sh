#!/usr/bin/env bash
# The scorecard schema, producers, reader, and published CSVs derive from one
# shared ScorecardRow. Each negative changes one link while holding the others.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
CHECK="$ROOT/compat-envelope/check-scorecard-schema.rs"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

"$CHECK" --self-check >/dev/null
"$CHECK" --root "$ROOT" >/dev/null

mkdir -p "$TMP/repo/compat-envelope/history"
for path in \
  scorecard-schema.json scorecard.csv fullcorpus-scorecard.csv \
  e9patch-scorecard.csv reverie-scorecard.csv collect-envelope.rs \
  collect-fullcorpus.sh ingest_real_program_matrix.py \
  collect-e9patch-compat.rs collect-dbi-corpus.rs collect-reverie-compat.rs \
  render-scorecard.rs; do
  cp "$ROOT/compat-envelope/$path" "$TMP/repo/compat-envelope/$path"
done
cp "$ROOT/compat-envelope/history/scorecard-observations.csv" \
  "$TMP/repo/compat-envelope/history/scorecard-observations.csv"

cp "$TMP/repo/compat-envelope/scorecard-schema.json" "$TMP/schema.clean"
sed -i '0,/"run_id",/s//"run_identifier",/' \
  "$TMP/repo/compat-envelope/scorecard-schema.json"
if "$CHECK" --root "$TMP/repo" >"$TMP/schema.out" 2>&1; then
  fail "a JSON projection that differs from ScorecardRow passed"
fi
grep -F 'core differs from ScorecardRow' "$TMP/schema.out" >/dev/null \
  || fail "schema mutation refusal did not name the shared type"
cp "$TMP/schema.clean" "$TMP/repo/compat-envelope/scorecard-schema.json"

sed -i '1s/^run_id,/run_identifier,/' "$TMP/repo/compat-envelope/scorecard.csv"
if "$CHECK" --root "$TMP/repo" >"$TMP/csv.out" 2>&1; then
  fail "a published CSV that differs from ScorecardRow passed"
fi
grep -F 'header differs from shared Scorecard row' "$TMP/csv.out" >/dev/null \
  || fail "CSV mutation refusal did not name the shared row"
cp "$ROOT/compat-envelope/scorecard.csv" "$TMP/repo/compat-envelope/scorecard.csv"

sed -i '/#\[path = "lib\/scorecard.rs"\]/d' \
  "$TMP/repo/compat-envelope/collect-dbi-corpus.rs"
if "$CHECK" --root "$TMP/repo" >"$TMP/import.out" 2>&1; then
  fail "a Rust producer that stopped importing ScorecardRow passed"
fi
grep -F 'does not derive its row shape from the shared ScorecardRow projection' \
  "$TMP/import.out" >/dev/null \
  || fail "producer mutation refusal did not name the missing shared type"

echo "scorecard shared type: positive projection and CSV pass; schema, CSV, and producer-import mutations refused"
