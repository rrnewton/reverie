#!/usr/bin/env bash
# Build one variant of the reverie workspace tests (no run) and record rc and
# wall time. This script does NOT copy the binary: the copy of the
# reverie-ptrace library unit-test binary to /tmp/cml-attr749-1227/bin/<variant>,
# its sha256 and its --list output were done by hand after each build.
# (Header comment corrected after the runs; no code in this script changed.)
# Usage: build.sh <variant>
# All paths are literal; the checkout is whatever is currently in repo/.
set -u
V="$1"
S=/tmp/cml-attr749-1227
LOG="$S/logs/build-$V.log"
JSON="$S/logs/build-$V.artifacts.json"
JLOG="$S/logs/build-$V.artifacts.stderr"
REC="$S/logs/build-$V.record"

{
  echo "variant=$V"
  echo "head=$(git -C $S/repo rev-parse HEAD)"
  echo "tree=$(git -C $S/repo rev-parse HEAD^{tree})"
  echo "status_porcelain_lines=$(git -C $S/repo status --porcelain | wc -l)"
  echo "df_avail_before=$(df -B1 --output=avail /data | tail -1 | tr -d ' ')"
  echo "rustc=$(env -C $S/repo rustc -V)"
  echo "cargo=$(env -C $S/repo cargo -V)"
  echo "start_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$REC"

t0=$(date +%s.%N)
env -C $S/repo \
  CARGO_TARGET_DIR=$S/target \
  RUSTFLAGS="-D warnings -C link-arg=-llzma" \
  RUSTDOCFLAGS="-D warnings" \
  nice -n 19 cargo test --workspace --all-features --no-run --offline -j 8 \
  > "$LOG" 2>&1
rc=$?
t1=$(date +%s.%N)
{
  echo "end_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "build_rc=$rc"
  echo "build_wall_s=$(echo "$t1 - $t0" | bc)"
} >> "$REC"

if [ "$rc" -ne 0 ]; then
  echo "BUILD_FAILED" >> "$REC"
  exit "$rc"
fi

# Cross-check: a no-op re-invocation that reports artifacts as JSON.
t2=$(date +%s.%N)
env -C $S/repo \
  CARGO_TARGET_DIR=$S/target \
  RUSTFLAGS="-D warnings -C link-arg=-llzma" \
  RUSTDOCFLAGS="-D warnings" \
  nice -n 19 cargo test --workspace --all-features --no-run --offline -j 8 \
  --message-format=json > "$JSON" 2> "$JLOG"
jrc=$?
t3=$(date +%s.%N)
{
  echo "json_rc=$jrc"
  echo "json_wall_s=$(echo "$t3 - $t2" | bc)"
} >> "$REC"
echo "BUILD_DONE" >> "$REC"
exit 0
