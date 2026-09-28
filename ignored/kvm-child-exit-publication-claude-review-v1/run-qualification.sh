#!/usr/bin/env bash
set -uo pipefail

ROOT=$(git rev-parse --show-toplevel)
PACKET="$ROOT/ignored/kvm-child-exit-publication-claude-review-v1"
EVIDENCE="$PACKET/qualification"
EXPECTED_HEAD=0598b5ffbeb737866372f89224d915efaeb29943

test "$(git rev-parse HEAD)" = "$EXPECTED_HEAD" || exit 90
mkdir -p "$EVIDENCE"

overall=0
run_case() {
  name=$1
  shift
  case_dir="$EVIDENCE/$name"
  mkdir -p "$case_dir"
  printf '%q ' "$@" > "$case_dir/command.txt"
  printf '\n' >> "$case_dir/command.txt"
  date --utc --iso-8601=seconds > "$case_dir/started-at.txt"
  /usr/bin/time -p -o "$case_dir/time.txt" \
    timeout --signal=TERM --kill-after=10s 180s \
    "$@" > "$case_dir/stdout" 2> "$case_dir/stderr"
  rc=$?
  printf '%s\n' "$rc" > "$case_dir/exit-code.txt"
  date --utc --iso-8601=seconds > "$case_dir/completed-at.txt"
  if [ "$rc" -ne 0 ]; then
    overall=1
  fi
}

cd "$ROOT" || exit 91
run_case diff-check git diff --check f7bd85e11dd258112148ed2cba6531501a1a00d9...0598b5ffbeb737866372f89224d915efaeb29943
run_case format cargo fmt --all -- --check
run_case focused cargo test -p reverie-kvm --lib process_signal_publication
run_case native-support-check cargo check -p reverie-kvm --lib --features native-test-support
run_case clippy cargo clippy -p reverie-kvm --lib --features native-test-support -- -D warnings
run_case real-kvm cargo test -p reverie-kvm --test static_elf child_waitability_callback_and_auto_reap_are_observed_on_real_kvm -- --exact --nocapture
run_case reverie-core cargo test -p reverie-core
run_case full-lib-serial cargo test -p reverie-kvm --lib -- --test-threads=1

exit "$overall"
