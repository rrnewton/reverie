# Synthetic regular `/proc` snapshot prerequisite — v6 review successor

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked paths and SHA-256:
  - `3ef73519f6666c3108b746cbb6d21691cee1953b585a77b310cf85ee2567a3a6  reverie-kvm/src/elf.rs`
  - `573e15c1a56768b8ee526c586be3d494a0383743e75cf51114cefa6bc59ee69b  reverie-kvm/src/executor.rs`
  - `e1a4d563a966236fda099d14b58cef8b31fc8507c8ab98ce782e65a7b74f7296  reverie-kvm/tests/static_elf.rs`
- Binary diff: 2,740 insertions, 122 deletions (134/0 in `elf.rs`, 2,298/122 in `executor.rs`, 308/0 in `static_elf.rs`).
- Binary patch SHA-256: `4684df79ea3e32af85ce5119a922e2fcf3b6bd7e191dcd89900540c6377e85e5`.
- No commit, push, remote-ref change, TaskGraph write, GitHub action, syncfs implementation, identity marker, or unrelated scope expansion was performed.
- Refused v5 artifacts remain untouched; their hashes are recorded in `refused-v5-artifacts.sha256`.

## V6 repairs

### Global read-only `O_TMPFILE` ordering

- `open_file` now masks the low 32-bit legacy flags immediately on entry and returns `EINVAL` for a complete `O_TMPFILE` request with `O_RDONLY` and without `O_PATH` before pathname copy, empty-path handling, dirfd lookup, fdinfo parsing, proc-fd parsing, or ordinary path resolution.
- The later fixed-proc and fdinfo copies of this validation were removed.
- Exact unit and required-KVM coverage includes a bad pointer, empty path, invalid dirfd, missing ordinary path, nonexistent fdinfo, nonexistent proc-fd, and valid fixed-proc/fdinfo/proc-fd paths.
- At every stage, `O_WRONLY | O_TMPFILE`, `O_PATH | O_TMPFILE`, and plain `O_DIRECTORY` controls continue to reach the native `EFAULT`, `ENOENT`, `EBADF`, or `ENOTDIR` path-dependent result.
- Linux `openat` ignores `O_TMPFILE`'s private bit under `O_PATH` while retaining its embedded `O_DIRECTORY`. The ordinary-file fallback now strips only that private transport bit before `openat2`, after guest path resolution, so missing ordinary paths return `ENOENT` and existing regular files return `ENOTDIR`. No `O_NOATIME` or alias behavior changed.

### Linux-version-dependent `O_CREAT | O_DIRECTORY` policy

- A `Copy` `RegularCreateDirectoryPolicy::{EarlyEinval, LegacyLookup}` is selected once through a thread-safe `OnceLock<Result<_, String>>`, so both success and failure are cached.
- Before any guest image is mapped, both initial loader entry points initialize the policy. The probe first opens `/proc/self/status` with `O_PATH | O_CLOEXEC`, verifies `S_IFREG` and procfs `f_type`, then invokes raw `openat` with `O_RDONLY | O_CREAT | O_DIRECTORY | O_CLOEXEC`. Each raw open has a strict 16-attempt `EINTR` bound.
- Only `EINVAL => EarlyEinval` and `ENOTDIR => LegacyLookup` are accepted. Any other errno or unexpected success is cached as failure; an unexpected descriptor is closed, and executable installation returns a typed host-I/O error rather than guessing during a guest syscall.
- The resolved non-optional policy is stored in `LoadedStaticElf`, copied on fork, and explicitly retained from the previous image during exec. Required-KVM execution exercises the production loader, proving a guest cannot execute without initialization.
- The pure direct-open mapping is exact: `EarlyEinval => EINVAL`; `LegacyLookup` plus `O_EXCL => EEXIST`; otherwise raw `/proc/mounts` plus `O_NOFOLLOW => ELOOP`; otherwise `ENOTDIR`. `O_PATH` never enters this rule.
- The all-17 direct matrix derives one exact expected result from the single cached host policy and compares both native and guest results to it. Separate injected tests exercise every mapping branch and the production direct-open path under both policy values; no assertion accepts either errno.
- The required-KVM test derives exact native errno values before backend construction and passes those values as guest argv. The C guest asserts each one exactly for plain, exclusive, and `/proc/mounts`-nofollow create-directory requests.
- Upstream basis: https://github.com/torvalds/linux/commit/43b450632676fb60e9faeddff285d9fac94a4f58. No genuine Linux 5.8–6.3 runtime is available locally: the running kernel is 7.1.3 and installed module trees are 6.13.2, 6.19.2, and 7.1.3 (`legacy-kernel-availability.log`). Legacy behavior is therefore supported by the upstream transition plus injected exact mapping/production tests, not claimed as locally executed.

## Retained prerequisite behavior

- Synthetic regular-proc carriers use `MFD_CLOEXEC | MFD_ALLOW_SEALING`, are populated before sealing, are mode `0444`, and require `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`. Seal readback accepts exactly that mask or that mask plus implicit `F_SEAL_EXEC`; all other shapes fail closed. Exposed descriptions are fresh `O_RDONLY` or real `O_PATH` OFDs.
- Nonnegative `pwrite64` remains path-specific: `/proc/self/cmdline` and `/proc/sys/kernel/osrelease` return `EBADF`; the other 15 fixed paths return `ESPIPE`. Negative offsets return `EINVAL` first, and nonnegative `O_PATH` returns `EBADF`.
- Direct and proc-fd open precedence from v5 remains intact, including `O_DIRECTORY`, `O_CREAT | O_EXCL`, `/proc/mounts` final-component `O_NOFOLLOW`, explicit access/truncate/direct gates, and the deliberate proc-fd `O_PATH | O_NOFOLLOW => ELOOP` refusal.
- Supported status flags and the bounded `O_NOFOLLOW` overlay retain their dup/fork/exec/close/dup2/dup3/shared-file-table behavior.

## Native evidence

`python3 ignored/kvm-syncfs-identity-v6/native-open-matrix.py` completed successfully in 0.10 s on x86-64 Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, EUID 212630. It reruns the complete pwrite/status/loginuid matrix before the open matrices. Full output is in `native-open-matrix.log`; this host reports `vm.memfd_noexec=0` and `create-directory-policy=EarlyEinval errno=22`.

- Combined direct-open matrix: 4,896 cases — 160 `EACCES`, 408 `EEXIST`, 832 `EINVAL`, 12 `ELOOP`, 1,836 `ENOTDIR`, and 1,648 successes.
- Proc-fd combined matrix: 60 cases — 24 `EEXIST`, 12 `ELOOP`, and 24 `ENOTDIR`.
- `O_TMPFILE` direct-plus-fdinfo matrix: 180 cases — 108 `EINVAL` and 72 `ENOTDIR`.
- Global `O_TMPFILE` ordering matrix: 36 cases — 3 `EBADF`, 3 `EFAULT`, 9 `EINVAL`, 12 `ENOENT`, and 9 `ENOTDIR`.

## Qualification

Every build command was bounded to 180 seconds, used `CARGO_INCREMENTAL=0`, and followed a free-space check above the 400 GiB floor. Final available space was 453,988,540,416 bytes.

- `cargo fmt --all -- --check`: exit 0, 2.10 s (`fmt-check.*`).
- `cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: 25 passed, 0 failed, 24.76 s (`focused-synthetic.*`).
- Exact ordinary/sealed/`F_SEAL_FUTURE_WRITE` control: 1 passed, 0 failed, 0.23 s (`focused-mutation-controls.*`).
- `REVERIE_REQUIRE_KVM=1 cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: 1 passed, 0 failed, 31.93 s (`real-kvm-synthetic-proc.*`). An unavailable KVM device cannot pass by skipping.
- Full `reverie-kvm` library suite: 806 passed, 0 failed, 11.11 s (`full-lib.*`).
- `cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 10.32 s (`clippy-strict.*`).
- Expanded native matrix, `git diff --check`, and all three frozen source hashes: exit 0 (`native-open-matrix.*`, `final-git-diff-check.*`, `final-source-restore.status`).

Development evidence retains two useful failures: the first global-ordering unit run exposed the `openat2` `O_PATH | O_TMPFILE` transport mismatch (`dev-global-tmpfile.log`), and the first required-KVM compile exposed misplaced embedded-C includes (`dev-real-kvm.log`). Both defects were repaired before the frozen mutation source.

## Exact-source negative sensitivity

Each v6 mutation ran alone under a 180-second bound, failed for its intended assertion, was immediately reversed, and was followed by byte verification of all three frozen source hashes.

- Move read-only `O_TMPFILE` admission below pathname copy: exit 101 in 22.83 s; the bad-pointer case returned `-EFAULT` instead of global `-EINVAL` (`mutation-late-readonly-tmpfile.*`).
- Hardcode `EarlyEinval` in the production direct-open path: exit 101 in 24.21 s; the injected `LegacyLookup` `/proc/uptime` case returned `-EINVAL` instead of `-ENOTDIR` (`mutation-hardcode-early-policy.*`).

Earlier v3–v5 sensitivity evidence remains preserved for sealing, pwrite classification, direct/proc-fd mutation flags, `O_NOFOLLOW` propagation, combined precedence, `/proc/mounts`, create/exclusive behavior, and valid-path `O_TMPFILE` admission.

## Scope and residuals

- The sealed immutable carrier covers the 17 paths in `synthetic_proc_content` plus dynamic fdinfo carriers. `/proc/self/loginuid`, random devices, and synthetic sysfs files remain on the separate pre-existing generic virtual-file path.
- Direct native `O_PATH | O_NOFOLLOW` on `/proc/mounts` identifies its procfs symlink, while the synthetic surface reports its synthetic regular-file identity. No host procfs inode is exposed.
- Proc-fd native `O_PATH | O_NOFOLLOW` returns an `O_PATH` description for the procfs magic link, while the backend deliberately returns `ELOOP` rather than exposing supervisor identity.
- SCM_RIGHTS transfer of synthetic identity remains outside this prerequisite.
- Existing mmap type/`MAP_SHARED_VALIDATE` differences remain unchanged. File-origin/max-protection provenance remains absent, so a later `mprotect(PROT_WRITE)` can differ from native after a read-only shared mapping.

## Goalpost-moving audit

- Assertions weakened: no. V6 adds exact global-order cases, exact per-policy native binding, and injected production-path checks for both policy variants.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. No guest assertion accepts `EINVAL` or `ENOTDIR`; each run selects one cached policy and one exact result. All previous matrices remain, and required KVM cannot skip.
- Failure renamed or relabelled as a pass: no.
- Check deleted instead of satisfied: no. Two isolated mutations demonstrate causal coverage of the global admission position and runtime policy consumption.
