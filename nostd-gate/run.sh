#!/usr/bin/env bash
# Compile-time gate for the shared no_std Reverie contract (Narf step 6.1).
#
# Positive: the gate crate, which uses the contract crates with
# `default-features = false`, must build for `x86_64-unknown-none` (core and
# alloc only). Execution: the gate's unit tests, built for the host with the
# same `std`-free contract crates, must pass; they run reverie-examples'
# counter1 tool through the gate's `Guest`. Negative: turning any one contract
# crate's `std` feature back on must fail with E0463 (no `std` for this
# target); otherwise the positive build would prove nothing about that crate.
#
# Usage: nostd-gate/run.sh [LOG_DIR]
# Exit status is 0 only if the positive build and the execution step pass and
# every negative control fails for the expected reason.
set -uo pipefail

TOOLCHAIN=${NOSTD_GATE_TOOLCHAIN:-nightly-2025-09-14}
TARGET=x86_64-unknown-none
HERE=$(cd "$(dirname "$0")" && pwd)
LOG_DIR=${1:-$HERE/target/gate-logs}
mkdir -p "$LOG_DIR"
cd "$HERE" || exit 2

NEGATIVES=$(sed -n 's/^\(negative-std-[a-z-]*\) = .*/\1/p' Cargo.toml)

check() {
    cargo "+$TOOLCHAIN" check --target "$TARGET" -Zbuild-std=core,alloc "$@"
}

status=0

check > "$LOG_DIR/positive.log" 2>&1
rc=$?
echo "positive rc=$rc"
[ "$rc" -eq 0 ] || status=1

cargo "+$TOOLCHAIN" test > "$LOG_DIR/execution.log" 2>&1
rc=$?
passed=$(grep -m1 -E '^test result:' "$LOG_DIR/execution.log" || true)
echo "execution rc=$rc ${passed:-<no test result>}"
[ "$rc" -eq 0 ] || status=1

for feature in $NEGATIVES; do
    check --features "$feature" > "$LOG_DIR/$feature.log" 2>&1
    rc=$?
    reason=$(grep -m1 -E '^error\[E0463\]' "$LOG_DIR/$feature.log" || true)
    required=$(grep -m1 -E 'required by' "$LOG_DIR/$feature.log" || true)
    echo "$feature rc=$rc ${reason:-<no E0463>} ${required:+($required)}"
    if [ "$rc" -eq 0 ] || [ -z "$reason" ]; then
        status=1
    fi
done

exit "$status"
