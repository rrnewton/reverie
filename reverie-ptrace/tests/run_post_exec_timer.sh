#!/usr/bin/env bash
set -euo pipefail

repository=$(cd "$(dirname "$0")/../.." && pwd -P)
target_dir=${CARGO_TARGET_DIR:-"$repository/target/liteinst-conformance"}
test_log=$(mktemp)
trap 'rm -f "$test_log"' EXIT

set +e
timeout --kill-after=5s 300s cargo test \
  --manifest-path "$repository/Cargo.toml" \
  --target-dir "$target_dir" \
  --locked --offline --release --package reverie-ptrace \
  --test post_exec_timer -- \
  plain_ptrace_post_exec_timer_fires_at_exact_rcb_deadline \
  --exact --ignored --test-threads=1 --nocapture >"$test_log" 2>&1
test_status=$?
set -e
cat "$test_log"
if [[ $test_status -ne 0 ]]; then
  exit "$test_status"
fi

result_count=$(grep -c \
  '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out;' \
  "$test_log") || result_count=0
if [[ $result_count -ne 1 ]]; then
  echo "ptrace post-exec timer conformance did not execute exactly one passing hardware test" >&2
  exit 1
fi
