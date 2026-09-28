#!/usr/bin/env bash
set -euo pipefail

cd /home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920
sha256sum --check ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.pre-source.sha256
patch --forward -p1 < ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.mutation.patch
trap 'patch --reverse -p1 < ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.mutation.patch' EXIT
sha256sum --check ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.mutated-source.sha256
available=$(df -B1 --output=avail . | tail -1)
test "$available" -ge 429496729600
set +e
CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib executor::tests::synthetic_proc_fdinfo_missing_closed_and_malformed_targets_remain_enoent -- --exact --nocapture
status=$?
set -e
test "$status" -eq 101
patch --reverse -p1 < ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.mutation.patch
trap - EXIT
sha256sum --check ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.post-source.sha256
git diff --binary b13ad926a34f27bb39a349429a1d08e812d741b4 -- reverie-kvm/src/elf.rs reverie-kvm/src/executor.rs reverie-kvm/tests/static_elf.rs > ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.replayed-base.patch
cmp ignored/kvm-syncfs-identity-v9/final.patch ignored/kvm-syncfs-identity-v9/proof-fdinfo-trailing.replayed-base.patch
