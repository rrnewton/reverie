# Synthetic regular `/proc` snapshot prerequisite — v7 review successor

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked paths and SHA-256:
  - `3ef73519f6666c3108b746cbb6d21691cee1953b585a77b310cf85ee2567a3a6  reverie-kvm/src/elf.rs`
  - `63b43bbdbce3f30de6d419e9459382a69d13791ee470cb88ad51081373da4744  reverie-kvm/src/executor.rs`
  - `f63f7ca8b2dec582ab674eda855c00a9d855faef312a5b357bcf91b871aca1a2  reverie-kvm/tests/static_elf.rs`
- Binary diff: 3,217 insertions, 114 deletions (134/0 in `elf.rs`, 2,708/114 in `executor.rs`, 375/0 in `static_elf.rs`).
- Binary patch SHA-256: `2f4770600874eb386107dca7d3cf5c1581e51dbae48a01783185ee37eea6070e`.
- No commit, push, remote-ref change, TaskGraph write, GitHub action, syncfs implementation, identity marker, or unrelated scope expansion was performed.
- Refused v6 artifacts remain untouched; their exact hashes are recorded in `refused-v6-artifacts.sha256`.

## V7 repairs

### Complete legacy `__O_TMPFILE` admission

- `open_file` masks the low 32-bit legacy flags immediately on entry. Before pathname copy, every non-`O_PATH` request carrying the private `__O_TMPFILE` bit is rejected with `EINVAL` unless it also carries `O_DIRECTORY`, omits `O_CREAT`, and uses `O_WRONLY`, `O_RDWR`, or access mode 3.
- `O_PATH` remains exempt because legacy `build_open_how` normalization discards irrelevant flags before admission.
- Exact unit coverage crosses bad pointers, empty paths, invalid dirfds, missing ordinary paths, nonexistent fdinfo and proc-fd paths, and valid fixed-proc, fdinfo, and proc-fd paths. It covers the bare private bit, a write-only missing-directory form, `O_TMPFILE | O_CREAT` in all three write-capable access modes, and access-mode-3 valid controls.
- Required-KVM coverage exercises the same global ordering and valid synthetic path classes. `O_WRONLY | O_TMPFILE`, access-mode-3 `O_TMPFILE`, `O_PATH | O_TMPFILE`, and plain `O_DIRECTORY` still reach their path-dependent native errors.

### Global create-directory policy

- After the `O_PATH` decision and before pathname copy, non-`O_PATH` `O_CREAT | O_DIRECTORY` returns `EINVAL` globally when the cached host policy is `EarlyEinval`. `LegacyLookup` continues through pathname and dirfd resolution.
- A successfully resolved fdinfo path under injected `LegacyLookup` returns `EEXIST` with `O_EXCL` and `ENOTDIR` otherwise; nonexistent and malformed fdinfo targets remain `ENOENT`.
- Fixed synthetic paths retain the exact helper mapping, including `/proc/mounts` plus `O_NOFOLLOW`. Proc-fd paths retain actual-host preflight rather than emulating a different host policy.
- The v6 fail-closed `OnceLock<Result<RegularCreateDirectoryPolicy, String>>` loader probe remains intact and is initialized before guest execution. Its value is stored in `LoadedStaticElf` and preserved through fork/exec.

### Ordinary-filesystem `O_PATH` normalization

- Only after guest path and dirfd resolution, ordinary fallback requests with `O_PATH` are reduced to the supplied subset of `O_PATH | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` before strict `openat2` validation.
- Native-versus-guest tests cover bad pointers, missing paths, an existing ordinary regular file, and a directory for each ignored extra: `O_WRONLY`, `O_RDWR`, access mode 3, `O_CREAT`, `O_EXCL`, `O_TRUNC`, `O_DIRECT`, `O_APPEND`, `O_SYNC`, `O_NONBLOCK`, `O_NOATIME`, and the private `__O_TMPFILE` bit. Additional comparisons verify retained `O_DIRECTORY`, `O_NOFOLLOW`, and `O_CLOEXEC` behavior.
- No second kernel-version policy was added. `O_NOATIME` and alias normalization outside this legacy `O_PATH` transport remain unchanged.

## Retained prerequisite behavior

- Synthetic regular-proc carriers use `MFD_CLOEXEC | MFD_ALLOW_SEALING`, are populated before sealing, are mode `0444`, and require `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`. Seal readback accepts exactly that mask or that mask plus implicit `F_SEAL_EXEC`; every other shape fails closed. Exposed descriptions are fresh `O_RDONLY` or real `O_PATH` OFDs.
- Nonnegative `pwrite64` is path-specific: `/proc/self/cmdline` and `/proc/sys/kernel/osrelease` return `EBADF`; the other 15 fixed paths return `ESPIPE`. Negative offsets return `EINVAL` first, and nonnegative `O_PATH` returns `EBADF`.
- Direct and proc-fd open precedence remains exact for `O_DIRECTORY`, `O_NOFOLLOW`, `O_CREAT | O_EXCL`, `O_TRUNC`, `O_DIRECT`, and writable access. Supported status flags and synthetic `O_PATH | O_NOFOLLOW` metadata retain dup/fork/exec/close/dup2/dup3/shared-table behavior.

## Native evidence

`timeout 180s python3 ignored/kvm-syncfs-identity-v7/native-open-matrix.py` completed with exit 0 in 0.551 s on x86-64 Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, EUID 212630. It first reruns the complete v3–v6 pwrite, status, loginuid, direct-open, proc-fd, and `O_TMPFILE` matrices. Full output is in `final-native-open-matrix.log`; this host reports `vm.memfd_noexec=0` and `create-directory-policy=EarlyEinval errno=22`.

- Retained combined direct-open matrix: 4,896 cases — 160 `EACCES`, 408 `EEXIST`, 832 `EINVAL`, 12 `ELOOP`, 1,836 `ENOTDIR`, and 1,648 successes.
- Retained proc-fd combined matrix: 60 cases — 24 `EEXIST`, 12 `ELOOP`, and 24 `ENOTDIR`.
- Retained direct-plus-fdinfo `O_TMPFILE` matrix: 180 cases — 108 `EINVAL` and 72 `ENOTDIR`.
- Retained v6 global ordering matrix: 36 cases — 3 `EBADF`, 3 `EFAULT`, 9 `EINVAL`, 12 `ENOENT`, and 9 `ENOTDIR`.
- V7 complete global `__O_TMPFILE` matrix: 99 cases — 5 `EBADF`, 5 `EFAULT`, 54 `EINVAL`, 20 `ENOENT`, and 15 `ENOTDIR`.
- V7 global create-directory matrix: 18 cases — 18 `EINVAL` on this EarlyEinval host.
- V7 ordinary `O_PATH` matrix: 48 cases — 12 `EFAULT`, 12 `ENOENT`, and 24 successes with ignored bits absent from `F_GETFL`.

No genuine Linux 5.8–6.3 runtime is available locally. The running kernel is 7.1.3; installed module trees are 6.13.2, 6.19.2, and 7.1.3, with no booted legacy kernel. LegacyLookup behavior is supported by https://github.com/torvalds/linux/commit/43b450632676fb60e9faeddff285d9fac94a4f58, pure injected mapping tests, and injected guest fdinfo tests—not claimed as locally executed.

## Final qualification

Every Cargo command used `timeout 180s` and `CARGO_INCREMENTAL=0` for build/test work. Each heavy command was preceded by an exact free-space check against 429,496,729,600 bytes (400 GiB). The final recorded audit had 446,762,209,280 bytes available.

- `timeout 180s cargo fmt --all -- --check`: exit 0, 2.142 s (`final-fmt-check.*`).
- `CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: exit 0, 28 passed, 0 failed, 24.408 s (`final-focused-synthetic.*`).
- `CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib executor::tests::file_mutation_guards_preserve_access_mode_and_memfd_seal_results -- --exact --nocapture`: exit 0, 1 passed, 0 failed, 0.121 s (`final-seal-control.*`).
- `REVERIE_REQUIRE_KVM=1 CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: exit 0, 1 passed, 0 failed, 30.701 s (`final-real-kvm.*`). The environment variable makes unavailable KVM a failure rather than a skip.
- `CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib -- --nocapture`: exit 0, 809 passed, 0 failed, 11.406 s (`full-lib.*`).
- `CARGO_INCREMENTAL=0 timeout 180s cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 10.029 s (`clippy-strict.*`).
- Expanded native matrix, `git diff --check`, all three source hash checks, and final binary-patch hashing: exit 0 (`final-native-open-matrix.*`, `final-git-diff-check.*`, `final-source-restore.status`, `final-source.sha256`, `final-patch.sha256`).

## Exact-source negative sensitivity

Each mutation ran alone under the 180-second bound, failed for the intended exact assertion, was immediately reversed, and ended with byte verification of all three frozen source hashes.

- Remove the complete `__O_TMPFILE` admission predicate: exit 101 in 23.930 s; the first invalid bad-pointer spelling returned `EFAULT` instead of `EINVAL` (`mutation-remove-tmpfile-admission.*`).
- Disable the global EarlyEinval create-directory gate: exit 101 in 23.269 s; the bad-pointer case returned `EFAULT` instead of `EINVAL` (`mutation-remove-global-early-create-directory.*`).
- Hardcode the fdinfo create-directory result to `EINVAL`: exit 101 in 29.400 s; injected `LegacyLookup` returned `EINVAL` instead of `ENOTDIR` (`mutation-hardcode-fdinfo-create-directory.*`).
- Remove ordinary `O_PATH` flag normalization: exit 101 in 23.548 s; a missing path with ignored `O_WRONLY` returned `EINVAL` instead of `ENOENT` (`mutation-remove-ordinary-opath-normalization.*`).

Earlier v3–v6 sensitivity evidence remains preserved for sealing, pwrite classification, direct/proc-fd mutation flags, `O_NOFOLLOW` propagation, combined precedence, `/proc/mounts`, create/exclusive behavior, and valid-path `O_TMPFILE` admission.

## Scope and residuals

- The immutable sealed carrier applies to the 17 paths in `synthetic_proc_content` plus dynamic fdinfo carriers. `/proc/self/loginuid`, random devices, and synthetic sysfs files remain on the separate pre-existing generic virtual-file path.
- Direct native `O_PATH | O_NOFOLLOW` on `/proc/mounts` identifies its procfs symlink, while the synthetic surface reports the synthetic regular-file identity. Proc-fd native `O_PATH | O_NOFOLLOW` identifies a procfs magic link; the backend deliberately returns `ELOOP` rather than exposing supervisor identity.
- SCM_RIGHTS transfer of synthetic identity remains outside this prerequisite.
- Existing mmap type/`MAP_SHARED_VALIDATE` differences remain unchanged. File-origin/max-protection provenance remains absent, so later `mprotect(PROT_WRITE)` can still differ from native after a read-only shared mapping.
- No old-kernel runtime evidence was fabricated; LegacyLookup remains source-backed and injection-tested only.

## Goalpost-moving audit

- Assertions weakened, deleted, or relabelled: no. V7 adds exact error comparisons for every requested early-ordering context and exact native-versus-guest flag comparisons.
- Tolerance widened, dual errno accepted, case skipped, or comparator relaxed: no. Each run is bound to the one probed host policy; injected LegacyLookup tests require one exact result.
- Required hardware bypassed: no. `REVERIE_REQUIRE_KVM=1` was set and the real-KVM test executed.
- A changed assertion made easier to pass: no. Four isolated source mutations demonstrate that the new gates and normalization are causally observed.
- Prior evidence removed: no. Refused v6 artifacts and all earlier mutation logs remain present.
