# Synthetic regular `/proc` snapshot prerequisite

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked source: `reverie-kvm/src/executor.rs`
- Final source SHA-256: `7aa3723aa765834f72d811b0383166b0b8c7f843c3a9a09d07cda9a01f268259`
- Diff size: 541 insertions, 7 deletions
- No commit, push, remote-ref change, TaskGraph write, or GitHub action was performed.

## Implemented behavior

- Create synthetic regular proc snapshots with `MFD_CLOEXEC | MFD_ALLOW_SEALING`.
- Populate content, set backing mode to exact `0444`, add exact `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`, and only then publish a freshly reopened `O_RDONLY` open-file description.
- Preserve guest CLOEXEC, shared dup offsets, fresh proc-fd reopen offsets, proc metadata, and exact seals across aliases.
- Refuse direct `fchmod` on classified proc carriers with native results: `EBADF` for `O_PATH`, then `EPERM` for an ordinary proc descriptor. Ordinary writable files remain chmod-capable.
- Match regular-proc scalar `pwrite64` results: `EINVAL` for a negative offset, otherwise `ESPIPE`. Ordinary `O_RDONLY` files retain `EBADF`.
- Match shared-writable mmap gates: `EBADF` for `O_PATH`, `EACCES` without read/write access, `EPERM` for `F_SEAL_WRITE`, and success for an unsealed `O_RDWR` ordinary file.
- Match `ftruncate` access results used by this path: `EBADF` for `O_PATH`, `EINVAL` for `O_RDONLY`, and success for an ordinary writable file.

## Qualification

- `cargo fmt --all -- --check`: exit 0, 2.08 s wall (`fmt-check-corrected-final.*`).
- `cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: 12 passed, 0 failed, 8.32 s wall (`focused-synthetic-proc-fchmod.*`).
- `cargo test --locked -p reverie-kvm --lib file_mutation_guards_preserve_access_mode_and_memfd_seal_results -- --nocapture`: 1 passed, 0 failed, 0.10 s wall (`focused-access-seal-controls-fchmod.*`).
- `cargo test --locked -p reverie-kvm --lib -- --nocapture`: 793 passed, 0 failed, 0 ignored, 15.20 s wall (`full-lib-corrected-final.*`).
- `cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 4.84 s wall (`clippy-strict-corrected-final.*`).
- Native syscall probe: exit 0, 0.06 s wall (`native-semantics-probe-final.*`). It measured proc `write=EBADF`, `pwrite=ESPIPE`, `ftruncate=EINVAL`, `fallocate=EBADF`, `fchmod=EPERM`, writable proc-fd reopen `EACCES`, proc `O_PATH` fchmod `EBADF`, and shared-writable sealed-memfd mmap `EPERM`.

## Exact-source negative sensitivity

Each mutation was applied alone to the final source, its targeted test was run with a 180-second hard bound, the expected failure was recorded, and the edit was restored before another command. `source-pre-final-mutations.sha256` and `source-after-all-mutations.sha256` compare byte-identically (comparison exit 0).

- Remove the classified-proc `fchmod` guard: exit 101; observed success `0` instead of `-EPERM` (`negative-final-remove-proc-fchmod-guard.*`).
- Remove `F_SEAL_WRITE`: exit 101; observed seals `7` instead of exact `15` (`negative-exact-remove-write-seal.*`).
- Remove construction-time mode `0444`: exit 101; observed backing permissions `0777` instead of `0444` (`negative-exact-remove-readonly-mode.*`).
- Remove the regular-proc `pwrite64` branch: exit 101; observed `-EBADF` instead of `-ESPIPE` (`negative-exact-remove-proc-pwrite.*`).
- Remove the mmap write-seal gate: exit 101; observed a mapping address instead of `-EPERM` (`negative-exact-remove-mmap-seal-gate.*`).

## Known remaining Linux-semantic mismatch

KVM's existing generic mapping model does not retain file-origin/max-protection provenance. A guest can first create `MAP_SHARED|PROT_READ` and later request `mprotect(PROT_WRITE)`; the current `mprotect` path checks only current mapped coverage and therefore can accept the upgrade. Native Linux returns `EACCES` for the corresponding read-only/write-sealed mapping, as captured by `sealed-reader-shared-mprotect-write` in `native-semantics-probe-final.log`. Closing that requires a separate mapping-provenance design and was not folded into this prerequisite.
