#!/usr/bin/env bash
set -uo pipefail

checkout=/home/newton/work/dev-hermit/worktrees/codex-tickgate3/hermit
result_root=/home/newton/work/dev-hermit/experiments/strict_verify_current_matrix_cutover_20260812/focused-systemd-reruns
binary="$checkout/target/release/hermit"
failures=0

cd "$checkout"
for attempt in 1 2 3 4 5; do
    if E2E_RUN_ID="record-getpid-sabre-$attempt" \
        E2E_RESULT_ROOT="$result_root" \
        HERMIT_BIN="$binary" \
        ./ci/test_harness.sh run \
            --test system-utils/record-getpid \
            --mode verify \
            --backend sabre \
            --ci-only; then
        printf 'RERUN %d PASS\n' "$attempt"
    else
        status=$?
        failures=$((failures + 1))
        printf 'RERUN %d FAIL rc=%d\n' "$attempt" "$status"
    fi
done

printf 'RATE failures=%d/5\n' "$failures"
