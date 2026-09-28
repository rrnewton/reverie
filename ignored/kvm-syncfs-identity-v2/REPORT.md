# Synthetic regular `/proc` snapshot prerequisite

- Repository/checkout: Reverie, `/home/newton/work/dev-hermit/worktrees/slots/kvm-syncfs-identity-20260920`
- Branch/base: `codex/kvm-syncfs-identity-20260920` at `b13ad926a34f27bb39a349429a1d08e812d741b4`
- Final tracked paths: `reverie-kvm/src/executor.rs`, `reverie-kvm/tests/static_elf.rs`
- Final SHA-256:
  - `bbeec6ada747b90aa5c8a13819ef6586b13ac869f10256f576ed6b8ca91f0f0f  reverie-kvm/src/executor.rs`
  - `e22e9709eb184adf345e4e13dd27881b61d02ca051570f3ac976a14b06db0f8e  reverie-kvm/tests/static_elf.rs`
- Diff: 1,004 insertions, 28 deletions (899/28 in `executor.rs`, 105/0 in `static_elf.rs`).
- Final patch SHA-256: `56796da083307456bc5cf82358b0ded072ea3fe6f914411716081b67956e8761`.
- No commit, push, remote-ref change, TaskGraph write, GitHub action, syncfs implementation, or identity marker was performed.

## Implemented behavior

- Synthetic regular proc snapshots use `MFD_CLOEXEC | MFD_ALLOW_SEALING`, are populated, changed to mode `0444`, and then receive required `F_SEAL_WRITE | F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL` seals before exposure.
- Production reads the resulting seal mask and accepts only the required mask or the required mask plus implicit Linux `F_SEAL_EXEC`; all other shapes fail closed. This host reports `vm.memfd_noexec=0` (`host-vm-memfd-noexec.txt`).
- The writable construction descriptor never enters the guest table. Ordinary synthetic opens expose a fresh `O_RDONLY` description; direct `O_PATH` opens expose a real `O_PATH` description. Guest CLOEXEC, dup shared offsets, fresh proc-fd reopen offsets, proc classification, and seal state are preserved.
- Direct and proc-fd `O_WRONLY`/`O_RDWR` access to classified regular proc carriers is rejected with `EACCES`, including under credentials capable of bypassing carrier mode bits. `O_PATH`, `O_NOFOLLOW`, wrong-type, and synthetic proc-directory behavior retain their separate precedence.
- Classified regular proc `pwrite64` returns `EINVAL` first for a negative offset, then `EBADF` for `O_PATH`, otherwise `ESPIPE`. The test enumerates all 17 current regular synthetic-proc match arms.
- `ftruncate` checks negative length before descriptor lookup, then returns `EBADF` for `O_PATH` and `EINVAL` for an `O_RDONLY` regular file.
- `fchmod` checks `O_PATH` first (`EBADF`), then rejects classified proc objects with `EPERM`; an ordinary writable file remains chmod-capable and the proc carrier remains mode `0444`.
- Shared writable mmap admission requires `O_RDWR` and rejects either `F_SEAL_WRITE` or `F_SEAL_FUTURE_WRITE` with `EPERM`. Ordinary `O_RDONLY`, `O_PATH`, unsealed writable, fully sealed, and FUTURE_WRITE-only controls are covered.
- Touched pwrite64, ftruncate, fchmod, and mmap descriptor arguments consume their low 32 bits, with high-upper-bit controls.
- The real-KVM regression checks deterministic `/proc/uptime` content and metadata, `O_RDONLY`/CLOEXEC exposure, exact write/pwrite/ftruncate/fallocate/fchmod/mmap errors, retained mode, direct `O_PATH` errors, negative-offset/length ordering, and DAC-independent proc-fd write-open refusal.

## Qualification

- `cargo fmt --all -- --check`: exit 0, 2.08 s (`fmt-check.log`).
- `cargo test --locked -p reverie-kvm --lib synthetic_proc_ -- --nocapture`: 16 passed, 0 failed, 0.10 s (`focused-synthetic-proc.log`).
- Exact ordinary/sealed/FUTURE_WRITE mutation-control unit test: 1 passed, 0 failed, 0.10 s (`focused-mutation-controls.log`).
- `REVERIE_REQUIRE_KVM=1 cargo test --locked -p reverie-kvm --test static_elf real_kvm_synthetic_proc_snapshot_is_immutable_and_opath_correct -- --exact --nocapture`: 1 passed, 0 failed, 8.51 s (`real-kvm-synthetic-proc.log`). The required environment variable means an unavailable KVM device cannot become a skip.
- First full library run: 796 passed, 1 unrelated KVM timing test failed after host `Kvm(Error(4))`/`EINTR`, 16.91 s (`full-lib.log`). Its exact rerun passed 1/1 in 0.70 s (`full-lib-single-flake-rerun.log`).
- Full library retry: 797 passed, 0 failed, 12.57 s (`full-lib-rerun.log`).
- `cargo clippy --locked -p reverie-kvm --all-targets -- -D warnings`: exit 0, 5.15 s (`clippy-strict.log`).
- `git diff --check`: exit 0 (`git-diff-check.*`).
- Native Linux probe: exit 0 (`native-semantics-probe.log`). `/proc/uptime` produced write `EBADF`, pwrite `ESPIPE`, ftruncate `EINVAL`, fallocate `EBADF`, fchmod `EPERM`, and `O_RDWR` proc-fd reopen `EACCES`; `O_PATH` fchmod produced `EBADF`; a shared writable mapping of the sealed memfd produced `EPERM`.

## Exact-source negative sensitivity

Each mutation was applied alone, exercised under a 180-second bound, and immediately reversed. Every restore verified both tracked source hashes against `source-before-mutations.sha256` before the next mutation.

- Remove `F_SEAL_WRITE` from the required mask: exit 101; observed `0x7` rather than the required immutable mask (`mutation-remove-write-seal.*`).
- Remove the classified-proc pwrite branch: exit 101; observed `-EBADF` (`-9`) rather than `-ESPIPE` (`-29`) on `/proc/uptime` (`mutation-remove-proc-pwrite.*`).
- Stop checking `F_SEAL_FUTURE_WRITE` during shared-writable mmap admission: exit 101; a mapping address was returned rather than `-EPERM` (`mutation-remove-future-write-mmap.*`).
- Remove the explicit regular-proc proc-fd access gate: exit 101; a deliberately mode-`0666` classified carrier returned guest fd 4 for `O_WRONLY` rather than `-EACCES` (`-13`) (`mutation-remove-procfd-access-gate.*`).

## Known remaining Linux-semantic gaps

- The existing KVM mapping model does not retain file-origin/max-protection provenance. After a successful `MAP_SHARED | PROT_READ`, `mprotect(PROT_WRITE)` can be accepted even when native Linux returns `EACCES` for a read-only or write-sealed source. The native evidence is in `native-semantics-probe.log`; fixing this requires a separate mapping-provenance design.
- Existing mmap type/`MAP_SHARED_VALIDATE` validation differences are outside this prerequisite and remain unchanged.
