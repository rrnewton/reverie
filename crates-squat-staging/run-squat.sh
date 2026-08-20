#!/usr/bin/env bash
# Runner for the remaining squat publishes (liteinst2 already done).
# Logs per-crate PASS/FAIL to LOG; breaks on rate-limit to avoid hammering.
cd "$(dirname "$0")/crates" || exit 1
LOG=../squat-run.log
: > "$LOG"

REMAINING=(
  safeptrace reverie-syscalls reverie-utils reverie-memory reverie-rpc-transport
  reverie-process reverie-preload reverie-core reverie-ptrace reverie-kvm
  reverie-liteinst reverie-dbi reverie-dbt reverie-e9patch reverie-dynamorio
  reverie-sabre test-allocator detcore-model detcore-dbi hermit-resources
  hermit-verify hermetic-infra hermit-run
)

for c in "${REMAINING[@]}"; do
  echo "=== PUBLISH $c ===" | tee -a "$LOG"
  out=$(with-proxy cargo publish --manifest-path "$c/Cargo.toml" --allow-dirty 2>&1)
  rc=$?
  echo "$out" >> "$LOG"
  if [[ $rc -eq 0 ]]; then
    echo "RESULT $c: PASS" | tee -a "$LOG"
  else
    echo "RESULT $c: FAIL(rc=$rc)" | tee -a "$LOG"
    if echo "$out" | grep -qiE "429|rate limit|too many|try again"; then
      echo "RATE-LIMIT DETECTED at $c — stopping to avoid hammering." | tee -a "$LOG"
      break
    fi
  fi
done
echo "=== RUNNER DONE ===" | tee -a "$LOG"
