# Synthetic regular `/proc` snapshot prerequisite — review successor

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked paths and SHA-256:
  - `6bf867677f8baad4b9b24db3452dab5b60abee23c2387f8a9b1940412e9be223  reverie-kvm/src/elf.rs`
  - `42a1898c16df728c9e3e825d6a5b571985751aa8aa5fb1f4d5d49a8cbdea1c8e  reverie-kvm/src/executor.rs`
  - `5e79c927554ba19979c410318271bab09cb5b2e357d865d808b509a3e102fa1b  reverie-kvm/tests/static_elf.rs`
- Diff: 1,464 insertions, 63 deletions (12/0 in `elf.rs`, 1,310/63 in `executor.rs`, 142/0 in `static_elf.rs`).
- Final patch SHA-256: `a436194465a259683a17aed157a80be143540b10cfbadde832fbfbf970499006`.
- No commit, push, remote-ref change, TaskGraph write, GitHub action, syncfs implementation, identity marker, or new TODO/PR marker was performed.
- The refused v2 report and patch were left byte-identical; their hashes are recorded in `refused-v2-artifacts.sha256`.

## Corrected behavior

- The immutable-carrier behavior remains: `MFD_CLOEXEC | MFD_ALLOW_SEALING`, population before sealing, mode `0444`, required `WRITE | GROW | SHRINK | SEAL` seals, exact readback acceptance of required or required-plus-implicit-`F_SEAL_EXEC`, and a fresh exposed `O_RDONLY` or real `O_PATH` OFD. This host reports `vm.memfd_noexec=0`.
- Nonnegative `pwrite64` is now path-specific. `/proc/self/cmdline` and `/proc/sys/kernel/osrelease` return `EBADF`; the other 15 current `synthetic_proc_content` regular paths return `ESPIPE`. Negative offsets return global `EINVAL` first, and an `O_PATH` description returns `EBADF` next. The unit test invokes native Linux and the guest implementation for one-byte, zero-count, and negative-offset calls across all 17 paths.
- Direct synthetic regular-proc opens return `ENOTDIR` for `O_DIRECTORY` before access checks, `EACCES` for any non-`O_PATH` writable access or `O_TRUNC`, and `EINVAL` for non-`O_PATH` `O_DIRECT`.
- Direct non-`O_PATH` descriptions preserve `O_NONBLOCK`, `O_APPEND`, `O_SYNC`, and `O_DSYNC` in `F_GETFL` (plus the supported `O_ASYNC`/`O_NOATIME` bits when requested). `O_PATH` ignores irrelevant status and mutation flags.
- Direct `O_NOFOLLOW`, including `O_PATH | O_NOFOLLOW`, is retained in a bounded per-fd metadata set because the supervisor must follow its private proc-fd link to expose the memfd rather than a host procfs magic-link inode. The bit follows `dup`, `F_DUPFD`, fork, shared file-table synchronization, and surviving exec descriptors; close and CLOEXEC-on-exec remove it.
- Classified regular-proc proc-fd reopen preserves the existing fail-closed `O_PATH | O_NOFOLLOW => ELOOP` policy and host `O_DIRECTORY => ENOTDIR` precedence, then returns `EACCES` for non-`O_PATH` writable access or `O_TRUNC`, and `EINVAL` for non-`O_PATH` `O_DIRECT`, independently of carrier DAC. Allowed status flags remain on the fresh host OFD.
- Required-KVM coverage now exercises both pwrite result classes and the direct/proc-fd directory, truncation, direct-I/O, status-flag, O_PATH, and O_NOFOLLOW behavior in addition to the original immutability checks.

## Native evidence

`native-proc-matrix.py` ran successfully in 0.07 s on x86-64 kernel `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, EUID 212630. Its 285-line output is retained in `native-proc-matrix.log`.

- All 17 paths return `EINVAL` for offset `-1` before descriptor/write admission.
- `/proc/self/cmdline` and `/proc/sys/kernel/osrelease` return `EBADF` for both one-byte and zero-count nonnegative pwrite; the other 15 return `ESPIPE`.
- Direct `/proc/uptime`: normal and `O_PATH` `O_DIRECTORY` return `ENOTDIR`; `O_RDONLY | O_TRUNC` returns `EACCES`; `O_RDONLY | O_DIRECT` returns `EINVAL`; `F_GETFL` retains `O_NONBLOCK`, `O_APPEND`, `O_SYNC`, `O_DSYNC`, and `O_NOFOLLOW`; `O_PATH` ignores mutation/status flags while `O_PATH | O_NOFOLLOW` reports both bits.
- Native proc-fd truncation/direct/directory results are `EACCES`/`EINVAL`/`ENOTDIR`; its `O_PATH | O_NOFOLLOW` result is a procfs symlink descriptor, which this backend deliberately refuses with `ELOOP` instead of exposing supervisor procfs identity.
- Native `/proc/self/loginuid` is distinct: O_RDONLY pwrite returns `EBADF`, while the probed invalid O_RDWR payload returns `EINVAL`.

## Qualification

All build commands were bounded to 180 seconds, used `CARGO_INCREMENTAL=0` for final compilation, and checked the 400 GiB free-space floor first. Final available space was 436,009,046,016 bytes.

- `cargo fmt --all -- --check`: exit 0, 2.07 s (`fmt-check.*`).
- `cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: 17 passed, 0 failed, 23.03 s (`focused-synthetic-proc.*`).
- Exact ordinary/sealed/FUTURE_WRITE mutation-control test: 1 passed, 0 failed, 0.10 s (`focused-mutation-controls.*`).
- `REVERIE_REQUIRE_KVM=1 cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: 1 passed, 0 failed, 30.41 s (`real-kvm-synthetic-proc.*`). An unavailable KVM device cannot pass by skipping.
- Full library suite: 798 passed, 0 failed, 7.59 s (`full-lib.*`).
- `cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 13.09 s (`clippy-strict.*`).
- Final `git diff --check` and all three frozen source hashes: exit 0 (`final-git-diff-check.*`, `final-source-restore.status`).

## Exact-source negative sensitivity

Every mutation ran alone under a 180-second bound and failed for its intended assertion. It was immediately reversed, and all three source hashes were verified before the next mutation.

- Force uniform `ESPIPE`: exit 101; `/proc/self/cmdline` produced `-29` instead of native `-9` (`mutation-uniform-pwrite.*`).
- Remove direct-open `O_DIRECTORY`: exit 101; returned fd 3 instead of `-ENOTDIR` (`mutation-remove-direct-directory-gate.*`).
- Remove direct-open `O_TRUNC`: exit 101; returned fd 3 instead of `-EACCES` (`mutation-remove-direct-trunc-gate.*`).
- Remove direct-open `O_DIRECT`: exit 101; returned fd 3 instead of `-EINVAL` (`mutation-remove-direct-direct-gate.*`).
- Remove proc-fd `O_TRUNC`: exit 101; returned fd 4 instead of `-EACCES` (`mutation-remove-procfd-trunc-gate.*`).
- Remove proc-fd `O_DIRECT`: exit 101; returned fd 4 instead of `-EINVAL` (`mutation-remove-procfd-direct-gate.*`).
- Remove the synthetic-proc `O_NOFOLLOW` `F_GETFL` overlay: exit 101; observed bit zero (`mutation-remove-nofollow-overlay.*`).

## Scope and residuals

- The sealed immutable carrier covers the 17 paths in `synthetic_proc_content` plus dynamic fdinfo carriers. `/proc/self/loginuid`, random devices, and synthetic sysfs files remain on the pre-existing generic virtual-file path; they are follow-on input for PR A and are not claimed by this prerequisite.
- Direct `O_PATH | O_NOFOLLOW` on native `/proc/mounts` identifies that path's procfs symlink, while this existing synthetic alias surface reports its synthetic regular-file identity. No host procfs inode is exposed.
- The backend intentionally returns `ELOOP` for proc-fd `O_PATH | O_NOFOLLOW` instead of exposing the supervisor's procfs magic-link inode, although native Linux returns an O_PATH descriptor for that link.
- Existing mmap type/`MAP_SHARED_VALIDATE` differences remain unchanged. The mapping model also lacks file-origin/max-protection provenance, so a later `mprotect(PROT_WRITE)` can differ from native after a read-only shared mapping.

## Goalpost-moving check

- Assertions weakened: no. The prior uniform-ESPIPE assertion was replaced by a complete live native oracle plus exact per-path guest comparison; it is stricter and detects either class being collapsed.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. The path matrix still contains every current regular `synthetic_proc_content` arm, and required KVM cannot skip.
- Failure renamed or relabelled as a pass: no.
- Check deleted instead of satisfied: no. Direct/proc-fd flag and metadata assertions were added, with isolated mutations demonstrating causality.
