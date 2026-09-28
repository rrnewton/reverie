# Synthetic regular `/proc` snapshot prerequisite — v5 review successor

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked paths and SHA-256:
  - `6bf867677f8baad4b9b24db3452dab5b60abee23c2387f8a9b1940412e9be223  reverie-kvm/src/elf.rs`
  - `1b304da156c605038ecf2dc3b82ffc8b9c981ea5fcd1ee161cd442dd060307fa  reverie-kvm/src/executor.rs`
  - `e29cb46ac25538acee3f1935061260cfca30570287d367c5b00c276d64b9946b  reverie-kvm/tests/static_elf.rs`
- Binary diff: 2,202 insertions, 124 deletions (12/0 in `elf.rs`, 1,989/124 in `executor.rs`, 201/0 in `static_elf.rs`).
- Binary patch SHA-256: `32dffeb031e51dfdb6e037461df3e35aaf3d1fa3eb9ce5517b36488d0aaad70b`.
- No commit, push, remote-ref change, TaskGraph write, GitHub action, syncfs implementation, identity marker, or scope expansion was performed.
- Refused v4 artifacts remain untouched; their hashes are recorded in `refused-v4-artifacts.sha256`.

## v5 blocker repair

- `readonly_tmpfile_is_invalid` recognizes a complete `O_TMPFILE` mask with `O_RDONLY` and without `O_PATH`. Direct fixed synthetic-proc opens and direct fdinfo opens return `EINVAL` through this check before their generic `O_DIRECTORY => ENOTDIR` gate.
- `O_RDONLY | O_TMPFILE` retains `EINVAL` when combined with `O_NOFOLLOW`, `O_TRUNC`, `O_DIRECT`, `O_EXCL`, or all four together.
- `O_WRONLY | O_TMPFILE`, `O_RDWR | O_TMPFILE`, `O_PATH | O_TMPFILE`, and the tested `O_PATH` all-extras combination retain native `ENOTDIR`.
- The direct native-versus-guest unit oracle applies those ten combinations to every one of the 17 fixed `synthetic_proc_content` paths, including `/proc/mounts`. A separate native-versus-guest fdinfo oracle applies the same ten cases to a valid dynamic fdinfo entry.
- The required-KVM C regression executes the same ten combinations through both `/proc/uptime` and a live `/proc/self/fdinfo/<ordinary-fd>` path.
- No `O_NOATIME` or alias-normalization behavior changed.

## Retained prerequisite behavior

- Synthetic regular-proc carriers use `MFD_CLOEXEC | MFD_ALLOW_SEALING`, are populated before sealing, are mode `0444`, and require `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`. Seal readback accepts only that exact mask or that mask plus implicit `F_SEAL_EXEC`; every other shape fails closed. Exposed descriptions are fresh `O_RDONLY` or real `O_PATH` OFDs.
- Nonnegative `pwrite64` remains path-specific: `/proc/self/cmdline` and `/proc/sys/kernel/osrelease` return `EBADF`; the other 15 fixed paths return `ESPIPE`. Negative offsets return `EINVAL` first, and nonnegative `O_PATH` returns `EBADF`.
- Direct and proc-fd open precedence from v4 remains intact, including `O_CREAT | O_DIRECTORY`, `O_DIRECTORY`, `O_CREAT | O_EXCL`, `/proc/mounts` final-component `O_NOFOLLOW`, explicit access/truncate/direct gates, and the deliberate proc-fd `O_PATH | O_NOFOLLOW => ELOOP` refusal.
- Supported status flags and the bounded `O_NOFOLLOW` overlay retain their dup/fork/exec/close/dup2/dup3/shared-file-table behavior.

## Native evidence

`native-open-matrix.py` completed successfully in 0.10 s on x86-64 kernel `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, EUID 212630. It reruns the complete v4 matrix before the v5 extension. The full output is retained in `native-open-matrix.log`; this host reports `vm.memfd_noexec=0`.

- Existing direct combined-open matrix: 4,896 cases — 160 `EACCES`, 408 `EEXIST`, 832 `EINVAL`, 12 `ELOOP`, 1,836 `ENOTDIR`, and 1,648 successes.
- Existing proc-fd combined matrix: 60 cases — 24 `EEXIST`, 12 `ELOOP`, and 24 `ENOTDIR`.
- New `O_TMPFILE` direct-plus-fdinfo matrix: 180 cases — 108 `EINVAL` and 72 `ENOTDIR`. This is ten combinations across all 17 fixed paths plus one live fdinfo path.

## Qualification

Every build command was bounded to 180 seconds, used `CARGO_INCREMENTAL=0`, and followed a free-space check above the 400 GiB floor. Final available space was 457,157,816,320 bytes.

- `cargo fmt --all -- --check`: exit 0, 2.10 s (`fmt-check.*`).
- `cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: 21 passed, 0 failed, 24.63 s (`focused-synthetic.*`).
- Exact ordinary/sealed/`F_SEAL_FUTURE_WRITE` control: 1 passed, 0 failed, 0.11 s (`focused-mutation-controls.*`).
- `REVERIE_REQUIRE_KVM=1 cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: 1 passed, 0 failed, 30.16 s (`real-kvm-synthetic-proc.*`). An unavailable KVM device cannot pass by skipping.
- Full `reverie-kvm` library suite: 802 passed, 0 failed, 9.62 s (`full-lib.*`).
- `cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 9.54 s (`clippy-strict.*`).
- Expanded native matrix, `git diff --check`, and all three frozen source hashes: exit 0 (`native-open-matrix.*`, `final-git-diff-check.*`, `final-source-restore.status`).

## Exact-source negative sensitivity

The sole v5 mutation replaced the new read-only-`O_TMPFILE` predicate with `false` and ran the focused suite serially under a 180-second bound. It failed with exit 101 in 26.30 s:

- The all-17 direct matrix observed `-ENOTDIR` instead of `-EINVAL` for `/proc/uptime` read-only `O_TMPFILE`.
- The fdinfo matrix independently observed `-ENOTDIR` instead of `-EINVAL` for its read-only `O_TMPFILE` case.
- Result: 19 passed, 2 failed. The predicate was immediately restored, and all three frozen source hashes verified byte-for-byte before final qualification (`mutation-disable-readonly-tmpfile-gate.*`, `restore-readonly-tmpfile-gate.status`).

The earlier v3/v4 sensitivity evidence remains preserved for sealing, pwrite classification, direct/proc-fd mutation flags, `O_NOFOLLOW` propagation, combined precedence, `/proc/mounts`, and create/exclusive behavior.

## Scope and residuals

- The sealed immutable carrier covers the 17 paths in `synthetic_proc_content` plus dynamic fdinfo carriers. `/proc/self/loginuid`, random devices, and synthetic sysfs files remain on the separate pre-existing generic virtual-file path.
- Direct native `O_PATH | O_NOFOLLOW` on `/proc/mounts` identifies its procfs symlink, while the synthetic surface reports its synthetic regular-file identity. No host procfs inode is exposed.
- Proc-fd native `O_PATH | O_NOFOLLOW` returns an `O_PATH` description for the procfs magic link, while the backend deliberately returns `ELOOP` rather than exposing supervisor identity.
- SCM_RIGHTS transfer of synthetic identity remains outside this prerequisite.
- Existing mmap type/`MAP_SHARED_VALIDATE` differences remain unchanged. File-origin/max-protection provenance remains absent, so a later `mprotect(PROT_WRITE)` can differ from native after a read-only shared mapping.

## Goalpost-moving audit

- Assertions weakened: no. Ten exact native-derived `O_TMPFILE` cases were added to every fixed path and fdinfo, plus required-KVM assertions.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. Existing v4 matrices and all earlier cases remain; required KVM still cannot skip.
- Failure renamed or relabelled as a pass: no.
- Check deleted instead of satisfied: no. Disabling the production predicate causes two independent exact guest matrices to fail while their native expectations remain unchanged.
