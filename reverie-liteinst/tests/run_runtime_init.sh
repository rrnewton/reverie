#!/usr/bin/env bash
set -euo pipefail

repository=$(cd "$(dirname "$0")/../.." && pwd -P)
target_dir=${CARGO_TARGET_DIR:-"$repository/target/liteinst-conformance"}
mkdir -p "$target_dir"
target_dir=$(cd "$target_dir" && pwd -P)

if [[ ! -f "$repository/Cargo.lock" ]]; then
  timeout --kill-after=5s 60s cargo generate-lockfile \
    --manifest-path "$repository/Cargo.toml" --offline
fi

timeout --kill-after=5s 180s cargo build \
  --manifest-path "$repository/Cargo.toml" \
  --target-dir "$target_dir" \
  --locked --offline --release --no-default-features \
  --package reverie-liteinst --lib

runtime=$(realpath "$target_dir/release/libreverie_liteinst.so")
digest_line=$(sha256sum -- "$runtime")
digest=${digest_line%% *}
test_log=$(mktemp)
trap 'rm -f "$test_log"' EXIT

set +e
REVERIE_LITEINST_RUNTIME_INIT_DSO="$runtime" \
REVERIE_LITEINST_RUNTIME_INIT_SHA256="$digest" \
timeout --kill-after=5s 300s cargo test \
  --manifest-path "$repository/Cargo.toml" \
  --target-dir "$target_dir" \
  --locked --offline --release --no-default-features \
  --package reverie-liteinst --test runtime_init -- \
  controller_loads_runtime_before_entry_and_dispatches_one_real_hook \
  --exact --ignored --test-threads=1 --nocapture >"$test_log" 2>&1
test_status=$?
set -e
cat "$test_log"
if [[ $test_status -ne 0 ]]; then
  exit "$test_status"
fi

result_count=$(grep -c \
  '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;' \
  "$test_log") || result_count=0
evidence_count=$(grep -c \
  'runtime-init evidence: pairs=13 ptrace-runs=13 initialized-runs=13 refusals=7 cancellations=1 native-controls=4 getpid-callbacks=52 preinit-markers=4 direct-hooks=13 timer-pairs=3$' \
  "$test_log") || evidence_count=0
precision=$(grep '^runtime-init precision: ' "$test_log") || precision=
precision_pattern='^runtime-init precision: entry-callbacks-per-run=([1-9][0-9]*) entry-callbacks=([1-9][0-9]*) mapping-callbacks=2 preinit-markers=4 timer-callbacks=([1-9][0-9]*)$'
precision_valid=0
if [[ $precision =~ $precision_pattern ]]; then
  entry_callbacks=${BASH_REMATCH[1]}
  entry_total=${BASH_REMATCH[2]}
  timer_total=${BASH_REMATCH[3]}
  if [[ $entry_callbacks -le 4096 && $entry_total -eq $((2 * entry_callbacks)) &&
        $timer_total -eq $((entry_total + 4)) ]]; then
    precision_valid=1
  fi
fi
if [[ $result_count -ne 1 || $evidence_count -ne 1 || $precision_valid -ne 1 ]]; then
  echo "runtime-init conformance did not execute its exact test and case matrix" >&2
  exit 1
fi
