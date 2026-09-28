# Synthetic regular `/proc` snapshot prerequisite — v9 candidate

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/head: `codex/kvm-syncfs-identity-20260920` at committed v8 head `96d0b5848f3a61bc7c4c1806bd8e64d25c5f7bb7`; base `b13ad926a34f27bb39a349429a1d08e812d741b4`.
- Final tracked source SHA-256:
  - `3ef73519f6666c3108b746cbb6d21691cee1953b585a77b310cf85ee2567a3a6  reverie-kvm/src/elf.rs` (unchanged in v9)
  - `df5ca5a039ca5656055e86f54f6051c4798548f4dee5e8fcf7794225d5d36b95  reverie-kvm/src/executor.rs`
  - `62eac72d498dbf1afb6fffaa9a18767d0d44fa7b331f8ac3d8fae6e3520d6627  reverie-kvm/tests/static_elf.rs`
- V9 delta from `96d0b584`: 446 insertions/52 deletions in `executor.rs`, 54 insertions/1 deletion in `static_elf.rs`; no v9 change to `elf.rs`.
- Full base-to-working-tree binary diff: 4,137 insertions/209 deletions across the three authorized paths. Patch SHA-256: `ca2a95b12e6cdadfb3ac983c67aa21dd6fee6e9b7ac66271a7e7228732e5eb1e`.
- No commit, push, PR edit, GitHub/TaskGraph action, Hermit invocation, syncfs implementation, identity marker, or tracked-file change outside the authorized paths was performed.

## V9 corrections

- The cached `LegacyLookup` mapping for an existing regular proc entry is now path-independent: `O_CREAT|O_DIRECTORY` returns `ENOTDIR`, or `EEXIST` when `O_EXCL` is present. Thus `/proc/mounts|O_NOFOLLOW` no longer incorrectly returns `ELOOP` when `O_DIRECTORY` is also present. The ordinary `/proc/mounts|O_NOFOLLOW` control remains exact `ELOOP`.
- This mapping follows pinned Linux v6.3 `fs/namei.c:3533-3544`: `LOOKUP_DIRECTORY` rejects the already-resolved final symlink with `ENOTDIR` before `may_open`; `O_EXCL` still wins with `EEXIST`.
- Fdinfo path parsing now distinguishes a valid target, a negative nonempty final component, an empty trailing-slash component, and an invalid task parent. Under injected `LegacyLookup`:
  - a present live target is `ENOTDIR` without `O_EXCL` and `EEXIST` with it; `O_NOFOLLOW` does not change either result;
  - a missing/closed numeric fd, nonnumeric leaf, leading-zero leaf, or invalid/dead task parent is `ENOENT` for every `O_EXCL`/`O_NOFOLLOW` combination;
  - exact `/proc/.../fdinfo/` with `O_CREAT|O_DIRECTORY` is `EISDIR` for every `O_EXCL`/`O_NOFOLLOW` combination.
- The negative-final result rebuts the refused review's proposed `EACCES`: Linux v6.3 `fs/proc/fd.c:221-240` returns `ERR_PTR(-ENOENT)` directly, and `fs/namei.c:3393-3401` propagates it before the create-error fallback at `3421-3424`. The trailing-slash exception follows `fs/namei.c:3460-3469`.
- Opening any sealed synthetic proc carrier without `O_NOFOLLOW` now removes a stale fd-number entry from `synthetic_proc_nofollow_fds`. A targeted reuse test preseeds the next fd, opens `/proc/uptime`, and requires both the side table and `F_GETFL` overlay to be clear.
- A readable live fdinfo carrier is checked directly through its host descriptor: its exposed OFD is `O_RDONLY`; its seals are exactly `WRITE|GROW|SHRINK|SEAL`, or that mask plus the host's implicit `F_SEAL_EXEC`. A duplicate has the same properties.
- The native and guest fdinfo pwrite oracle is exact: one byte at offset 0 and zero bytes at offset 0 return `ESPIPE`; either positive or zero count at offset -1 returns `EINVAL`; a bad pointer at a nonnegative offset still returns `ESPIPE`; invalid fd/nonnegative returns `EBADF`, while invalid fd/negative returns `EINVAL`. This matches v6.3/v7.1 `single_open`/`seq_file` clearing `FMODE_PWRITE` after the global negative-offset check.
- The fchmod guard remains deliberately DAC-independent even when a classified carrier is host-writable mode 0666. Tests always require guest `EPERM` and unchanged host mode. Native `EACCES`/`EPERM` comparisons are credential-qualified; the required-KVM program separately proves the guest reports uid/euid 0 before requiring `fchmod(...)=EPERM`.

## Native evidence

`timeout 180s python3 ignored/kvm-syncfs-identity-v9/native-open-matrix.py` completed with exit 0 in 0.232 s on x86-64 Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, EUID 212630, with `vm.memfd_noexec=0`. The v9 packet contains a self-contained source closure for the inherited v3, v6, v7, and v8 matrices.

- Retained combined direct-open matrix: 4,896 cases — 160 `EACCES`, 408 `EEXIST`, 832 `EINVAL`, 12 `ELOOP`, 1,836 `ENOTDIR`, and 1,648 successes.
- Retained proc-fd matrix: 60 cases — 24 `EEXIST`, 12 `ELOOP`, and 24 `ENOTDIR`.
- Retained direct/fdinfo `O_TMPFILE`: 180 cases — 108 `EINVAL`, 72 `ENOTDIR`.
- Retained global `O_TMPFILE`: 36 cases; v7 complete global `__O_TMPFILE`: 99 cases; v7 global create-directory: 18 exact `EINVAL`; ordinary `O_PATH`: 48 cases.
- Retained v8 live-fdinfo matrix: 25 cases — 10 `EINVAL`, 10 `ENOTDIR`, 5 successes.
- V9 mounts matrix: four create-directory combinations plus one ordinary nofollow control — four `EINVAL` and one `ELOOP` on this `EarlyEinval` host.
- V9 fdinfo create-directory matrix: 20 exact `EINVAL` results across live, missing numeric, malformed text, leading-zero, and trailing-slash paths on this host.
- V9 fdinfo pwrite matrix: seven cases — one `EBADF`, three `EINVAL`, and three `ESPIPE`.

No genuine Linux 5.8–6.3 runtime is installed locally; available module trees are 6.13.2, 6.19.2, and 7.1.3. Legacy behavior is therefore supported by pinned upstream source, https://github.com/torvalds/linux/commit/43b450632676fb60e9faeddff285d9fac94a4f58, and injected exact unit/guest tests, not claimed as locally executed.

The supervisor was non-root for this run, so all 160 credential-sensitive native `EACCES` rows executed. The source labels and skips only those native comparisons if run as EUID 0; credential-independent native rows and all exact guest protection assertions always run.

## Final qualification

Every Cargo build/test command used `CARGO_INCREMENTAL=0` and `timeout 180s`. Every heavy command checked for at least 429,496,729,600 available bytes first. Including the evidence-only proof reruns, the lowest recorded pre-command value was 442,128,363,520 bytes. Durations below are whole-command wall times, including compilation and linking where they occurred.

- `cargo fmt --all -- --check`: exit 0, 2.143 s (`final-fmt-check.*`).
- `cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: exit 0, 33 passed, 0 failed, 23.205 s (`final-focused-synthetic.*`).
- Exact seal/mmap control: exit 0, 1 passed, 0 failed, 0.114 s (`final-seal-control.*`).
- Exact fdinfo carrier/seal/pwrite control: exit 0, 1 passed, 0 failed, 0.107 s (`final-fdinfo-carrier-control.*`).
- Exact dead-leader/live-worker fdinfo lifecycle control: exit 0, 1 passed, 0 failed, 0.108 s (`final-fdinfo-generation-control.*`).
- Exact stale-`O_NOFOLLOW` fd-reuse control: exit 0, 1 passed, 0 failed, 0.107 s (`final-nofollow-reuse-control.*`).
- `REVERIE_REQUIRE_KVM=1 cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: exit 0, 1 passed, 0 failed, 31.112 s (`final-real-kvm.*`). KVM unavailability cannot pass by skipping.
- `cargo test --locked -p reverie-kvm --lib -- --nocapture`: exit 0, 815 passed, 0 failed, 13.905 s (`final-full-lib.*`).
- `cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 9.842 s (`final-clippy-strict.*`).
- Expanded native matrix: exit 0, 0.232 s (`final-native-open-matrix.*`).
- `git diff --check`, exact source restoration, and pre-/post-mutation binary patch identity: exit 0.

For clarity, the v8 report's 23.167 s focused-test duration and 29.680 s real-KVM duration were also whole-command wall times including compilation; they were not test-body-only timings.

## Preserved first failures

- The first format check exited 1 in 3.949 s solely on rustfmt layout differences (`dev-fmt-check.*`); `cargo fmt --all` then exited 0 in 2.089 s, and the final check passed.
- The first full-library run exited 101 after 9.502 s with 814 passes and one pre-existing concurrent pipe test failure: `positioned_vectored_io_handles_pipes_partial_writes_and_sigpipe` expected `EPIPE` but observed a four-byte write. The exact failed test reran successfully in 0.112 s, the immediate full suite reran 815/815 in 7.504 s, and the final post-mutation full suite passed 815/815 in 13.905 s. No source change was made for this unrelated flake.

## Exact-source negative sensitivity

Each mutation was applied alone, ran under the 180-second bound with a fresh disk check, failed on its intended literal assertion, was immediately reversed, and restored all three frozen source hashes plus the complete binary patch byte-for-byte. The original `mutation-*` logs remain untouched. The independently repeated `proof-*` evidence adds, for every mutation, a shell-safe replay recipe, exact isolated unified patch, pre/mutated/post source hashes, command, disk reading, status, duration, full log, forward/reverse dry-run status, and post-restore full base patch/hash.

- Reintroduce the obsolete Legacy nofollow `ELOOP` branch: proof rerun exit 101 in 22.857 s; `/proc/mounts|O_CREAT|O_DIRECTORY|O_NOFOLLOW` returned `-ELOOP` instead of literal `-ENOTDIR`. Mutated `executor.rs` SHA-256 `03dfca165b0d5f9b5876b5d0b4f4c1a644549951271a65e64706d9525c4db1db`; isolated patch SHA-256 `dfae1298e3aca4655b454c55ccae7fc3ba10c72fdf8c224df523a7b3bc9a2ea7` (`proof-mounts.*`).
- Replace the fdinfo trailing-slash `EISDIR` gate with `ENOENT`: proof rerun exit 101 in 23.259 s; the exact path returned `-ENOENT` instead of literal `-EISDIR`. Mutated `executor.rs` SHA-256 `62d92438be1cd3ff9255a729197604eaa49ee42cd92f5c0d48650049d210547d`; isolated patch SHA-256 `2863a594349624bba9666e193cd754ffdd4a0fdcbc5d929238790acd1199d641` (`proof-fdinfo-trailing.*`).
- Remove non-nofollow stale-key cleanup: proof rerun exit 101 in 22.732 s; the preseeded reused fd retained the forbidden marker. Mutated `executor.rs` SHA-256 `821e0ef3be50ce874dec88832045014f48077156cdc58578b51c05207b7b1f4f`; isolated patch SHA-256 `8928397bfb026e5b18ed66f7aa980b0a4d0a3b8030b05a4b8dc5e5eadc12b411` (`proof-stale-nofollow.*`).
- Remove `F_SEAL_WRITE` from the production required mask: proof rerun exit 101 in 23.051 s; the independent fdinfo-carrier assertion observed `0x7` instead of either accepted immutable mask. Mutated `executor.rs` SHA-256 `10248961d2c12e6b50d7375a66dff1fe09f334bafda7fd8306041cc35c5696af`; isolated patch SHA-256 `050708ad42418540786457b584bbeb684432c86edcc8c0b7aa9ccc5d4201b9d7` (`proof-seal-write.*`).

For all four proof reruns, `*.mutation-patch.status` is the expected `diff(1)` result meaning the isolated files differ; both patch dry-run directions succeed. Every replay recipe installs an EXIT trap immediately after mutation so any later refusal restores the source. Every `*.restore.status` records `source=0`, `base_patch=0`, and `forward_patch=0`. Every post-restore base patch hashes to `ca2a95b12e6cdadfb3ac983c67aa21dd6fee6e9b7ac66271a7e7228732e5eb1e`. `proof-artifacts.sha256` inventories 78 retained proof files and has SHA-256 `65bb95c36e8d6af9389e51fa8af3fe62008121c604d7c5862fb75a365a1f3edc`; its full verification and all four recipe syntax checks exit 0. Recipe SHA-256 values are: mounts `fbc83dc4700ad96a7a04a01589901b8cfbfed5c131d94165c61cd42606999ed0`, fdinfo trailing slash `f0b561e26381f339f125ef32e89765887546819e47c5f2a41e7e291652f91387`, stale nofollow `ee63927046628c0d86212969f8cc19c327c2026734d12048d167eae7b28a7777`, and seal removal `c004d1372a59e034d7c8bdd2d1c4ecb88ee1f28085314ae23711a5b0495134f9`.

## Scope and residuals

- Immutable carriers cover the 17 fixed paths in `synthetic_proc_content` plus readable supported fdinfo descriptions. This is not a claim about every synthetic `/proc` object.
- `/proc/self/loginuid`, random devices, and synthetic sysfs CPU-frequency files use the older generic virtual-file carrier. The unit control samples exactly `/proc/self/loginuid`, `/dev/urandom`, and `/sys/devices/system/cpu/cpufreq/boost`; it does not directly test `/dev/random` or every cpufreq file. For those three sampled paths it proves the carrier has only the default `F_SEAL_SEAL`, is outside `proc_files`, and can still be reopened through guest proc-fd as writable. That redesign remains out of scope. Native loginuid also has its separate path-specific pwrite behavior (`O_RDONLY` => `EBADF`, `O_RDWR` => `EINVAL`).
- The exact seal policy deliberately fails closed with `EOPNOTSUPP` if a future kernel adds an implicit seal other than the currently accepted `F_SEAL_EXEC`. The named hypothetical-bit test makes that compatibility risk explicit.
- Fdinfo metadata now exposes the pre-existing truncated synthetic statx mask: the guest returns `0x71f` for a requested `STATX_BASIC_STATS` (`0x7ff`), omitting ATIME/MTIME/CTIME mask bits even though the native fdinfo path reports all requested basic bits. Mode, link count, and size match. No content/sequence state is observed or allocated by metadata lookup.
- Known pre-existing fdinfo parity residual: Linux permits `O_PATH`, including `O_PATH|O_NOFOLLOW` and ignored legacy bits, on every live `/proc/.../fdinfo/N` entry and exposes regular 0444 metadata while reads fail with `EBADF`. Reverie continues to return `ENOSYS`. This change seals carriers for already-supported readable fdinfo opens and fixes pathname metadata plus selected flag-error ordering; it does not claim fdinfo `O_PATH` lifecycle, link, or status parity.
- Direct `/proc/mounts` `O_PATH|O_NOFOLLOW` synthetic identity, proc-fd `O_PATH|O_NOFOLLOW` fail-closed identity, SCM_RIGHTS identity transport, mmap type/`MAP_SHARED_VALIDATE`, and later `mprotect(PROT_WRITE)` provenance remain unchanged residuals.
- The guest presents uid 0 but intentionally refuses fchmod and writable/truncating proc-fd access independently of host DAC. Native non-root `EACCES`/`EPERM` evidence is credential-dependent, and native uid-0 behavior can diverge; this candidate does not claim credential-exact mutation parity.
- The retained pre-existing `/proc/self/fdinfo/` ordinary-open/directory/TMPFILE residual remains `ENOENT`; v9 changes only Legacy `O_CREAT|O_DIRECTORY` on that exact trailing-slash pathname to the source-backed `EISDIR` result.
- No old-kernel runtime evidence was fabricated.

## Goalpost-moving audit

- Assertions weakened, deleted, or relabelled: no. The initially dropped trailing-slash `ENOENT` controls for ordinary read, `O_DIRECTORY`, and write-capable `O_TMPFILE` were restored after read-only review; the new Legacy create-directory expectations are separate exact `EISDIR` assertions.
- Tolerance widened: no. Every flag/error comparison is exact. The two accepted seal shapes remain exactly REQUIRED and REQUIRED|`F_SEAL_EXEC`; arbitrary supersets are rejected.
- Exemption added: only the explicit EUID-0 guard around credential-sensitive native `EACCES`/`EPERM` comparisons. This prevents a root supervisor from being mislabeled as the native oracle; guest protection assertions are unconditional. This run used EUID 212630, so no such row was skipped.
- Failure renamed as pass: no. The first format mismatch and unrelated full-suite flake are retained with their nonzero statuses and exact follow-up results.
- Check deleted instead of satisfied: no. Corrected mounts and fdinfo expectations are literals, stale-key cleanup is dynamically observed, the fdinfo seal expectation is independent of the production constant, and each changed production gate has an isolated failing mutation.
