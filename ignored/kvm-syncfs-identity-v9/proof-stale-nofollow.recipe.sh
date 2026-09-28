#!/usr/bin/env bash
set -euo pipefail

cd /home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920
sha256sum --check ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.pre-source.sha256
patch --forward -p1 < ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.mutation.patch
trap 'patch --reverse -p1 < ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.mutation.patch' EXIT
sha256sum --check ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.mutated-source.sha256
available=$(df -B1 --output=avail . | tail -1)
test "$available" -ge 429496729600
set +e
CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib executor::tests::synthetic_proc_open_clears_stale_nofollow_metadata_on_fd_reuse -- --exact --nocapture
status=$?
set -e
test "$status" -eq 101
patch --reverse -p1 < ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.mutation.patch
trap - EXIT
sha256sum --check ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.post-source.sha256
git diff --binary b13ad926a34f27bb39a349429a1d08e812d741b4 -- reverie-kvm/src/elf.rs reverie-kvm/src/executor.rs reverie-kvm/tests/static_elf.rs > ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.replayed-base.patch
cmp ignored/kvm-syncfs-identity-v9/final.patch ignored/kvm-syncfs-identity-v9/proof-stale-nofollow.replayed-base.patch
