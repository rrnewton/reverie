#!/usr/bin/env bash
# Paced runner for remaining squat publishes. Honors crates.io 429 retry-after,
# skips names already live, works through the full list. Safe to re-run.
cd "$(dirname "$0")/crates" || exit 1
LOG=../squat-paced.log
UA="hermit-squat-verify (hermit@rrnewton.github.io)"

REMAINING=(
  reverie-rpc-transport reverie-process reverie-preload reverie-core reverie-ptrace
  reverie-kvm reverie-liteinst reverie-dbi reverie-dbt reverie-e9patch
  reverie-dynamorio reverie-sabre test-allocator detcore-model detcore-dbi
  hermit-resources hermit-verify hermetic-infra hermit-run
)

log() { echo "[$(date -u '+%H:%M:%S')] $*" | tee -a "$LOG"; }

for c in "${REMAINING[@]}"; do
  # Skip if already live (idempotent re-runs).
  code=$(with-proxy curl -s -o /dev/null -w "%{http_code}" -A "$UA" "https://crates.io/api/v1/crates/$c" 2>/dev/null)
  if [[ "$code" == "200" ]]; then log "SKIP $c (already live)"; continue; fi

  attempt=0
  while :; do
    attempt=$((attempt+1))
    log "PUBLISH $c (attempt $attempt)"
    out=$(with-proxy cargo publish --manifest-path "$c/Cargo.toml" --allow-dirty 2>&1)
    rc=$?
    if [[ $rc -eq 0 ]]; then
      log "PASS $c"
      break
    fi
    if echo "$out" | grep -qiE "already (exists|uploaded)"; then
      log "PASS $c (already existed)"
      break
    fi
    when=$(echo "$out" | grep -oE "try again after [^)]*" | sed 's/try again after //; s/ and see.*//')
    if [[ -n "$when" ]]; then
      target=$(date -d "$when" +%s 2>/dev/null)
      now=$(date +%s)
      if [[ -n "$target" && "$target" -gt "$now" ]]; then
        wait=$(( target - now + 15 ))
        log "RATE-LIMIT $c -> sleeping ${wait}s until $when +15s"
        sleep "$wait"
        continue
      fi
    fi
    # Non-rate-limit failure: record and move on (do not spin).
    log "FAIL $c rc=$rc :: $(echo "$out" | tail -2 | tr '\n' ' ')"
    break
  done
done
log "RUNNER DONE"
