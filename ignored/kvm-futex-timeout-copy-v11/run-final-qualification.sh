#!/usr/bin/env bash
set -uo pipefail

repo=/home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918
evidence=$repo/ignored/kvm-futex-timeout-copy-v11
floor=429496729600
expected_head=8865eb263cb44530364cb90eda6cdb40826aba7d

cd "$repo" || exit 2

check_source() {
  test "$(git rev-parse HEAD)" = "$expected_head" || return 2
  git diff --check || return 2
  test "$(git diff --name-only | sort)" = "$(printf '%s\n' \
    reverie-kvm/src/executor.rs \
    reverie-kvm/src/executor/entry_host_wait_tests.rs \
    reverie-kvm/tests/static_elf.rs | sort)" || return 2
}

check_disk() {
  read -r blocks block_size < <(stat -f --format='%a %S' "$repo") || return 2
  available=$((blocks * block_size))
  printf '%s\n' "$available"
  test "$available" -ge "$floor"
}

run_phase() {
  name=$1
  shift
  before=$(check_disk) || {
    printf '%s\n' "disk floor refused before $name" >&2
    return 125
  }
  printf '%s\n' "$before" > "$evidence/$name.free-before.bytes"
  /usr/bin/time -f 'elapsed=%e user=%U sys=%S maxrss_kb=%M' \
    -o "$evidence/$name.time" "$@" \
    > "$evidence/$name.stdout" 2> "$evidence/$name.stderr"
  rc=$?
  printf '%s\n' "$rc" > "$evidence/$name.status"
  after=$(check_disk) || {
    printf '%s\n' "disk/source gate refused after $name" >&2
    return 125
  }
  printf '%s\n' "$after" > "$evidence/$name.free-after.bytes"
  if test "$rc" -ne 0; then
    tail -80 "$evidence/$name.stderr" >&2
    return "$rc"
  fi
}

check_source || exit 2
sha256sum \
  reverie-kvm/src/executor.rs \
  reverie-kvm/src/executor/entry_host_wait_tests.rs \
  reverie-kvm/tests/static_elf.rs > "$evidence/source-before.sha256"
git diff --binary > "$evidence/source.patch"
sha256sum "$evidence/source.patch" > "$evidence/source.patch.sha256"

run_phase fmt /usr/bin/timeout --signal=TERM --kill-after=5s 120s \
  cargo fmt --all -- --check || exit $?
run_phase focused env CARGO_NET_OFFLINE=true /usr/bin/timeout --signal=TERM --kill-after=5s 180s \
  cargo test --locked -p reverie-kvm --lib futex -- --nocapture || exit $?
run_phase real-kvm env CARGO_NET_OFFLINE=true REVERIE_REQUIRE_KVM=1 \
  /usr/bin/timeout --signal=TERM --kill-after=5s 180s \
  cargo test --locked -p reverie-kvm --test static_elf \
  static_elf_host_futex_wait_observes_finite_timeout -- --exact --nocapture || exit $?
run_phase clippy env CARGO_NET_OFFLINE=true /usr/bin/timeout --signal=TERM --kill-after=5s 300s \
  cargo clippy --locked -p reverie-kvm --lib --tests -- -D warnings || exit $?
run_phase full-lib env CARGO_NET_OFFLINE=true /usr/bin/timeout --signal=TERM --kill-after=5s 300s \
  cargo test --locked -p reverie-kvm --lib -- --nocapture || exit $?

sha256sum \
  reverie-kvm/src/executor.rs \
  reverie-kvm/src/executor/entry_host_wait_tests.rs \
  reverie-kvm/tests/static_elf.rs > "$evidence/source-after.sha256"
cmp -s "$evidence/source-before.sha256" "$evidence/source-after.sha256" || exit 3
check_source || exit 2
printf '%s\n' qualified
