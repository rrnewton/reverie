#!/usr/bin/env bash
set -uo pipefail

repo=/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918
evidence=$repo/ignored/kvm-futex-timeout-copy-v12
floor=429496729600
cd "$repo" || exit 2

check_disk() {
  read -r blocks block_size < <(stat -f --format='%a %S' "$repo") || return 2
  available=$((blocks * block_size))
  printf '%s\n' "$available"
  test "$available" -ge "$floor"
}

run_phase() {
  name=$1
  shift
  before=$(check_disk) || return 125
  printf '%s\n' "$before" > "$evidence/$name.free-before.bytes"
  /usr/bin/time -f 'elapsed=%e user=%U sys=%S maxrss_kb=%M' \
    -o "$evidence/$name.time" "$@" \
    > "$evidence/$name.stdout" 2> "$evidence/$name.stderr"
  rc=$?
  printf '%s\n' "$rc" > "$evidence/$name.status"
  after=$(check_disk) || return 125
  printf '%s\n' "$after" > "$evidence/$name.free-after.bytes"
  if test "$rc" -ne 0; then
    tail -80 "$evidence/$name.stderr" >&2
    return "$rc"
  fi
}

sha256sum -c ignored/kvm-futex-timeout-copy-v11/source-before.sha256 || exit 2
git diff --check || exit 2
run_phase unrelated-exact env CARGO_NET_OFFLINE=true \
  /usr/bin/timeout --signal=TERM --kill-after=5s 120s \
  cargo test --locked -p reverie-kvm --lib \
  executor::tests::descriptor_retirement_accept_cleanup_releases_both_guards \
  -- --exact --nocapture || exit $?
run_phase full-lib-retry env CARGO_NET_OFFLINE=true \
  /usr/bin/timeout --signal=TERM --kill-after=5s 300s \
  cargo test --locked -p reverie-kvm --lib -- --nocapture || exit $?
sha256sum -c ignored/kvm-futex-timeout-copy-v11/source-before.sha256 || exit 2
git diff --check || exit 2
printf '%s\n' qualified
