# Synthetic regular `/proc` snapshot prerequisite — v8 review successor

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked paths and SHA-256:
  - `3ef73519f6666c3108b746cbb6d21691cee1953b585a77b310cf85ee2567a3a6  reverie-kvm/src/elf.rs`
  - `74b0e3732462aa83ef926dcd7e44a52802a63df1303bd62722e7fd7bb72df6a2  reverie-kvm/src/executor.rs`
  - `b51557a4ce47c007d58a1257b787c2ff8f60bba6bc9d49d98e5860b775d5ea9f  reverie-kvm/tests/static_elf.rs`
- Binary diff: 3,722 insertions, 241 deletions (134/0 in `elf.rs`, 3,200/241 in `executor.rs`, 388/0 in `static_elf.rs`).
- Binary patch SHA-256: `356462ca55635b56f572697db8d5e935a6915debb147350b7f587f4d148be0fb`.
- No commit, push, remote-ref change, TaskGraph write, GitHub action, syncfs implementation, identity marker, or unrelated scope expansion was performed.
- Refused v7 artifacts remain untouched; their exact hashes are recorded in `refused-v7-artifacts.sha256`.

## V8 repair

- `fdinfo_target_generation` now establishes only pathname existence: the selected task generation must be live and the target guest descriptor must resolve to a live host descriptor. It no longer rejects a path merely because the target's content cannot be represented by the existing fdinfo reader.
- `open_fdinfo` performs this existence check first. It then applies the already-defined path/type policy: a valid write-capable `O_TMPFILE` or plain `O_DIRECTORY` returns `ENOTDIR`; under injected `LegacyLookup`, `O_CREAT | O_DIRECTORY` returns `EEXIST` with `O_EXCL` and `ENOTDIR` without it. The global `EarlyEinval` gate remains before pathname access.
- Only an otherwise admissible read reaches the separate content-support check. Synthetic-proc, random, signalfd, captured-output, and anonymous-inode targets therefore still return `ENOSYS` for ordinary readable fdinfo opens.
- `newfstatat` and `statx` now observe that every syntactically admitted live fdinfo pathname exists even when its content is unsupported. Raw native and guest checks require success and matching regular-file mode, link count, and zero size; deliberately synthetic identity and ownership fields are not compared.
- Rejected opens leave the file map, fd-entry map, and fdinfo-description map unchanged, proving that no fdinfo description or sequence state is allocated. The implementation does not lock the shared fdinfo table during open; successful reads retain the established sequence -> file-table -> lifecycle ordering.
- Lifecycle coverage retains a shared table in a worker after leader exit: fresh open/newfstatat/statx requests for `/proc/1/fdinfo/<live-fd>` return `ENOENT`, while the corresponding `/proc/thread-self/fdinfo/<live-fd>` requests succeed.
- Missing, closed, empty-suffix, nonnumeric, leading-zero, and absent numeric targets remain exact `ENOENT` after applicable global validation.

## Required-KVM coverage

The existing required-KVM snapshot program now constructs a live synthetic-proc descriptor and addresses its fdinfo entry. It requires successful regular-file metadata, `ENOSYS` for ordinary readable content, `ENOTDIR` for plain directory and valid write-capable `O_TMPFILE`, and the exact host-derived create-directory errno with and without `O_EXCL`.

## Retained v7 behavior

- Full non-`O_PATH` `__O_TMPFILE` admission remains before pathname copy: the private bit requires embedded `O_DIRECTORY`, forbids `O_CREAT`, and requires `O_WRONLY`, `O_RDWR`, or access mode 3.
- Cached `EarlyEinval` versus `LegacyLookup` create-directory policy, ordinary-fallback `O_PATH` normalization, exact fixed/proc-fd precedence, status propagation, path-specific pwrite behavior, immutable sealed carriers, and nofollow lifecycle metadata remain unchanged.
- The loader still fails closed unless its verified procfs probe returns exactly `EINVAL` or `ENOTDIR`, caches success and failure in `OnceLock`, initializes before guest execution, and preserves the selected policy through fork/exec.

## Native evidence

`timeout 180s python3 ignored/kvm-syncfs-identity-v8/native-open-matrix.py` completed with exit 0 in 0.118 s on x86-64 Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, EUID 212630. It reruns every v3-v7 native oracle before the v8 matrix. This host reports `vm.memfd_noexec=0` and `create-directory-policy=EarlyEinval errno=22`.

- Retained combined direct-open matrix: 4,896 cases — 160 `EACCES`, 408 `EEXIST`, 832 `EINVAL`, 12 `ELOOP`, 1,836 `ENOTDIR`, and 1,648 successes.
- Retained proc-fd combined matrix: 60 cases — 24 `EEXIST`, 12 `ELOOP`, and 24 `ENOTDIR`.
- Retained direct-plus-fdinfo `O_TMPFILE` matrix: 180 cases — 108 `EINVAL` and 72 `ENOTDIR`.
- Retained v6 global ordering matrix: 36 cases — 3 `EBADF`, 3 `EFAULT`, 9 `EINVAL`, 12 `ENOENT`, and 9 `ENOTDIR`.
- Retained v7 complete global `__O_TMPFILE` matrix: 99 cases — 5 `EBADF`, 5 `EFAULT`, 54 `EINVAL`, 20 `ENOENT`, and 15 `ENOTDIR`.
- Retained v7 global create-directory matrix: 18 exact `EINVAL` results on this EarlyEinval host.
- Retained v7 ordinary `O_PATH` matrix: 48 cases — 12 `EFAULT`, 12 `ENOENT`, and 24 successes.
- V8 live-fdinfo native matrix: 25 cases across proc, random, signalfd, stdout, and anonymous-inode targets — 10 `EINVAL`, 10 `ENOTDIR`, and 5 successful readable opens. Each path also had native regular mode `0444`, link count 1, and size 0.

The first v8 native-script invocation is preserved in `dev-native-open-matrix.*`: it failed because the thin wrapper selected the wrong nested `runpy` namespace. `native-open-matrix-rerun.*` and the final artifact prove the corrected oracle.

No genuine Linux 5.8–6.3 runtime is available locally. The running kernel is 7.1.3; installed module trees are 6.13.2, 6.19.2, and 7.1.3. LegacyLookup behavior is supported by https://github.com/torvalds/linux/commit/43b450632676fb60e9faeddff285d9fac94a4f58, pure injected mapping tests, and injected guest fdinfo tests—not claimed as locally executed.

## Final qualification

Every Cargo command used `timeout 180s` and `CARGO_INCREMENTAL=0` for build/test work. Each heavy command was preceded by an exact free-space check against 429,496,729,600 bytes (400 GiB). The final pre-report audit had 443,254,628,352 bytes available.

- `timeout 180s cargo fmt --all -- --check`: exit 0, 2.092 s (`final-fmt-check.*`).
- `CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: exit 0, 31 passed, 0 failed, 23.167 s (`final-focused-synthetic.*`).
- Exact seal/`F_SEAL_FUTURE_WRITE` control: exit 0, 1 passed, 0 failed, 0.107 s (`final-seal-control.*`).
- Exact dead-leader/live-worker pathname-generation control: exit 0, 1 passed, 0 failed, 0.110 s (`final-generation-paths.*`).
- `REVERIE_REQUIRE_KVM=1 CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: exit 0, 1 passed, 0 failed, 29.680 s (`final-real-kvm.*`). KVM unavailability cannot pass by skipping.
- `CARGO_INCREMENTAL=0 timeout 180s cargo test --locked -p reverie-kvm --lib -- --nocapture`: exit 0, 812 passed, 0 failed, 8.650 s (`full-lib.*`).
- `CARGO_INCREMENTAL=0 timeout 180s cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 9.603 s (`clippy-strict.*`).
- Expanded native matrix, `git diff --check`, all three source hash checks, and final binary-patch hashing: exit 0 (`final-native-open-matrix.*`, `final-git-diff-check.*`, `final-source-restore.status`, `final-source.sha256`, `final-patch.sha256`).

## Exact-source negative sensitivity

Each mutation ran alone under the 180-second bound, failed for the intended exact assertion, was immediately reversed, and ended with byte verification of all three frozen source hashes.

- Restore content rejection before fdinfo path/type gates: exit 101 in 22.481 s; the synthetic-proc `O_DIRECTORY` case returned `ENOSYS` instead of `ENOTDIR` (`mutation-old-early-enosys-order.*`).
- Restore synthetic-proc content rejection inside the existence helper: exit 101 in 22.886 s; native-backed fdinfo metadata returned `ENOSYS` instead of success (`mutation-restore-content-gate-in-existence.*`).
- Retained v7 full `__O_TMPFILE` mutation: exit 101 in 22.739 s; bad-pointer input returned `EFAULT` instead of `EINVAL` (`mutation-retained-v7-tmpfile.*`).
- Retained v7 global EarlyEinval mutation: exit 101 in 23.082 s; bad-pointer input returned `EFAULT` instead of `EINVAL` (`mutation-retained-v7-global-early.*`).
- Retained v7 fdinfo-policy mutation: exit 101 in 23.789 s; injected LegacyLookup returned `EINVAL` instead of `ENOTDIR` (`mutation-retained-v7-fdinfo-policy.*`).
- Retained v7 ordinary-`O_PATH` mutation: exit 101 in 22.532 s; ignored `O_WRONLY` changed missing-path `ENOENT` to `EINVAL` (`mutation-retained-v7-opath.*`).

Earlier v3-v6 sensitivity evidence remains preserved for sealing, pwrite classification, direct/proc-fd mutation flags, `O_NOFOLLOW` propagation, combined precedence, `/proc/mounts`, create/exclusive behavior, and valid-path `O_TMPFILE` admission.

## Scope and residuals

- The immutable sealed carrier applies to the 17 paths in `synthetic_proc_content` plus dynamic fdinfo carriers. `/proc/self/loginuid`, random devices, and synthetic sysfs files remain on the separate generic virtual-file path.
- Readable fdinfo content for synthetic-proc, random, signalfd, captured-output, and anonymous-inode targets remains deliberately unsupported with `ENOSYS`; v8 corrects their pathname metadata and requested flag-error ordering only.
- Known pre-existing fdinfo parity residual: Linux permits O_PATH, including O_PATH|O_NOFOLLOW and ignored legacy bits, on every live /proc/.../fdinfo/N entry and exposes regular 0444 metadata while reads fail with EBADF. Reverie continues to return ENOSYS. This change seals carriers for already-supported readable fdinfo opens and fixes pathname metadata plus selected flag-error ordering; it does not claim fdinfo O_PATH lifecycle, link, or status parity.
- Adjacent fdinfo `O_TRUNC`, `O_DIRECT`, and bare `O_CREAT | O_EXCL` precedence still differs from native and remains outside the explicit v8 matrix.
- Direct `/proc/mounts` and proc-fd `O_PATH | O_NOFOLLOW` identity residuals, SCM_RIGHTS synthetic identity, mmap type/`MAP_SHARED_VALIDATE`, and later `mprotect(PROT_WRITE)` provenance remain unchanged.
- No old-kernel runtime evidence was fabricated.

## Goalpost-moving audit

- Assertions weakened, deleted, or relabelled: no. New expectations use literal errno values rather than the production mapping helper.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. Each path/type result is exact; current host policy is probed once and LegacyLookup is separately injected.
- Failure renamed or relabelled as a pass: no. The first native-script wiring failure remains preserved and labelled as a development failure.
- Check deleted instead of satisfied: no. Two v8 mutations prove both open-order and metadata-existence causality; all four v7 mutations were rerun on the v8 source.
- The code-review procedure prompted the native metadata comparison, lifecycle-generation coverage, literal expected errnos, and explicit no-sequence-allocation assertions.
