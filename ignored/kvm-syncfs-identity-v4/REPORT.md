# Synthetic regular `/proc` snapshot prerequisite — v4 review successor

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked paths and SHA-256:
  - `6bf867677f8baad4b9b24db3452dab5b60abee23c2387f8a9b1940412e9be223  reverie-kvm/src/elf.rs`
  - `7c76f11195f4505f9b19cdb8d6b0e05359d6081f0142dd6529579b33436d005f  reverie-kvm/src/executor.rs`
  - `992e00f03adb64c24b57c33fe2fac25a8134908eb4453f065ff524ed0e8047dd  reverie-kvm/tests/static_elf.rs`
- Diff: 2,026 insertions, 130 deletions (12/0 in `elf.rs`, 1,840/130 in `executor.rs`, 174/0 in `static_elf.rs`).
- Final patch SHA-256: `58643ec73eb632642ca78c6b079bf91819371d547582c5ec285f8f83acdb410a`.
- No commit, push, remote-ref change, TaskGraph write, GitHub action, syncfs implementation, identity marker, or new TODO/PR marker was performed.
- Refused v3 artifacts remain untouched; their hashes are recorded in `refused-v3-artifacts.sha256`.

## Corrected behavior

- Synthetic regular-proc carriers use `MFD_CLOEXEC | MFD_ALLOW_SEALING`, are populated before sealing, are mode `0444`, and require `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`. Seal readback accepts only that exact mask or that mask plus an implicit `F_SEAL_EXEC`; every other shape fails closed. Exposed descriptions are fresh `O_RDONLY` or real `O_PATH` OFDs.
- Nonnegative `pwrite64` remains path-specific: `/proc/self/cmdline` and `/proc/sys/kernel/osrelease` return `EBADF`; the other 15 current `synthetic_proc_content` paths return `ESPIPE`. Negative offsets return `EINVAL` before fd/access admission; nonnegative `O_PATH` returns `EBADF`.
- Direct non-`O_PATH` opens now apply native precedence: `O_CREAT | O_DIRECTORY => EINVAL`, then `O_DIRECTORY => ENOTDIR`, then `O_CREAT | O_EXCL => EEXIST`. `/proc/mounts` is treated as the one native final symlink, so non-`O_PATH` `O_NOFOLLOW` returns `ELOOP` before writable/truncate/direct policy. The other 16 fixed paths retain regular-file `O_NOFOLLOW` behavior. Writable access or `O_TRUNC` returns `EACCES`; `O_DIRECT` returns `EINVAL`.
- Direct `O_PATH` ignores access/create/exclusive/truncate/direct/status flags as Linux does, while `O_DIRECTORY` is still checked first. The bounded metadata overlay retains guest-visible `O_NOFOLLOW` for the synthetic description without exposing a host procfs magic-link inode.
- Classified regular-proc proc-fd reopens perform host/path precedence before target policy whenever `O_DIRECTORY`, `O_NOFOLLOW`, or non-`O_PATH` `O_CREAT | O_EXCL` is present. Thus directory-plus-nofollow returns `ENOTDIR`, non-directory non-`O_PATH` nofollow combinations return `ELOOP`, and create-exclusive combinations return `EEXIST`. Only afterward do the DAC-independent `EACCES` and `EINVAL` target gates run. Bare `O_PATH | O_NOFOLLOW` remains deliberately fail-closed with `ELOOP`.
- Allowed non-`O_PATH` status flags remain observable on the fresh read-only OFD. The `O_NOFOLLOW` overlay is exercised across dup, fork, exec survival/filtering, dup2/dup3 replacement in both marked/unmarked directions, close, and a shared `FileTableState` snapshot/install round trip.
- Required-KVM coverage exercises both pwrite result classes, all direct immutable/O_PATH behavior, direct `/proc/mounts` nofollow behavior, direct create/exclusive/directory precedence, and the combined proc-fd precedence cases.

## Native evidence

`native-open-matrix.py` completed successfully in 0.09 s on x86-64 kernel `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, EUID 212630. The full output is retained in `native-open-matrix.log`; this host reports `vm.memfd_noexec=0`.

- The inherited all-17 pwrite/status matrix retained the two `EBADF` paths, 15 `ESPIPE` paths, and global negative-offset `EINVAL` ordering.
- The exhaustive direct combined-open matrix checked 4,896 cases: 160 `EACCES`, 408 `EEXIST`, 832 `EINVAL`, 12 `ELOOP`, 1,836 `ENOTDIR`, and 1,648 successes.
- The focused proc-fd combined matrix checked 60 cases: 24 `EEXIST`, 12 `ELOOP`, and 24 `ENOTDIR`.
- Native `O_PATH | O_NOFOLLOW` succeeds on procfs links and exposes link identity. The backend deliberately does not expose that supervisor procfs identity; the two documented divergences remain direct `/proc/mounts` synthetic identity and proc-fd fail-closed `ELOOP`.

## Qualification

All build commands were bounded to 180 seconds, used `CARGO_INCREMENTAL=0` where compilation was involved, and checked the 400 GiB free-space floor before each heavy build. Final available space was 464,401,092,608 bytes.

- `cargo fmt --all -- --check`: exit 0, 2.07 s (`fmt-check.*`).
- `cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: 20 passed, 0 failed, 23.35 s (`focused-synthetic.*`).
- Exact ordinary/sealed/`F_SEAL_FUTURE_WRITE` mutation-control test: 1 passed, 0 failed, 0.11 s (`focused-mutation-controls.*`).
- `REVERIE_REQUIRE_KVM=1 cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: 1 passed, 0 failed, 29.85 s (`real-kvm-synthetic-proc.*`). An unavailable KVM device cannot pass by skipping.
- Full `reverie-kvm` library suite: 801 passed, 0 failed, 7.74 s (`full-lib.*`).
- `cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 9.55 s (`clippy-strict.*`).
- Final native matrix, `git diff --check`, and all three frozen source hashes: exit 0 (`native-open-matrix.*`, `final-git-diff-check.*`, `final-source-restore.status`).

## Exact-source negative sensitivity

Every v4 mutation ran alone under a 180-second bound, failed for its intended assertion, was immediately reversed, and was followed by verification of all three source hashes.

- Exclude `O_PATH` from proc-fd precedence preflight: exit 101; path-directory-nofollow returned `-ELOOP` instead of `-ENOTDIR` (`mutation-procfd-path-directory-precedence.*`).
- Exclude `O_NOFOLLOW` from proc-fd precedence preflight: exit 101; write-nofollow returned `-EACCES` instead of `-ELOOP` (`mutation-procfd-nofollow-precedence.*`).
- Exclude `O_CREAT | O_EXCL` from proc-fd precedence preflight: exit 101; create-exclusive-write returned `-EACCES` instead of `-EEXIST` (`mutation-procfd-create-exclusive-precedence.*`).
- Remove direct `/proc/mounts` `O_NOFOLLOW` handling: exit 101; read-nofollow returned fd 3 instead of `-ELOOP` (`mutation-direct-mounts-nofollow.*`).
- Remove direct `O_CREAT | O_EXCL` handling: exit 101; create-exclusive-read returned fd 3 instead of `-EEXIST` (`mutation-direct-create-exclusive.*`).
- Remove direct `O_CREAT | O_DIRECTORY` handling: exit 101; create-directory returned `-ENOTDIR` instead of `-EINVAL` (`mutation-direct-create-directory.*`).

The earlier v3 sensitivity evidence remains available unchanged for the path-specific pwrite result, direct/proc-fd truncation and direct-I/O gates, directory handling, and `O_NOFOLLOW` metadata overlay.

## Scope and residuals

- The sealed immutable carrier covers the 17 paths in `synthetic_proc_content` plus dynamic fdinfo carriers. `/proc/self/loginuid`, random devices, and synthetic sysfs files remain on the pre-existing generic virtual-file path and are not claimed by this prerequisite.
- Direct native `O_PATH | O_NOFOLLOW` on `/proc/mounts` identifies its procfs symlink, while the synthetic surface reports its synthetic regular-file identity. No host procfs inode is exposed.
- Proc-fd native `O_PATH | O_NOFOLLOW` returns an `O_PATH` description for the procfs magic link, while the backend deliberately returns `ELOOP` rather than exposing supervisor identity.
- SCM_RIGHTS transfer of synthetic identity remains outside this prerequisite.
- Existing mmap type/`MAP_SHARED_VALIDATE` differences remain unchanged. The mapping model also lacks file-origin/max-protection provenance, so a later `mprotect(PROT_WRITE)` can differ from native after a read-only shared mapping.

## Goalpost-moving check

- Assertions weakened: no. The v4 tests add complete native-bound combined-flag matrices and exact errno comparisons.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. All 17 fixed paths remain in the direct matrix, required KVM cannot skip, and `/proc/mounts` is explicitly distinguished rather than removed.
- Failure renamed or relabelled as a pass: no.
- Check deleted instead of satisfied: no. Six isolated v4 mutations demonstrate that each new precedence/gate assertion is causal, while v3 mutation evidence remains preserved.
