[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Summary

- consume only the Linux low 32-bit descriptor word for both `sendfile` descriptors and the `lseek` descriptor
- validate the decoded `sendfile` output before any input/output `ENOSYS` classification, restoring native/base `EBADF` ordering for negative, closed, read-only, and pipe-read-end output aliases
- keep `ENOSYS` only for valid but unsupported writable routes
- prove rejected pipe/socket inputs are not consumed in focused unit and real-KVM native/direct/Tool coverage

This is an isolated descriptor-decoder follow-up. It does not change `lseek` whence decoding, non-null-offset precedence, private-fdinfo predispatch, writable-pipe support, or `CLOSE_RANGE_UNSHARE` semantics.

## Rebase and review response

PR 623 advanced main to `96598dc07490ec15411845d27ed5bfd5c0a34076`. It changed only `reverie-ptrace/*`, so rebasing this three-commit stack was conflict-free and left both KVM files byte-identical to the previously frozen correction.

Review of former head `730123acb922cfa30fa1bb9281391776088fb35d` found that negative `out_fd` validation happened after input routing. Head `5d65b9f50f124f615c5f0c862af7b94919037c3f` fixed the negative case, but fresh review then showed that positive high-word aliases of unusable outputs could still reach input classification first and return `ENOSYS`.

Rebased head `748321a70995f1e7c1d5385de09f620f4becd685` validates output existence and writability immediately after low-word decoding. A modeled fd must pass `ensure_writable`, or the fd must satisfy the existing open-standard contract; otherwise the call returns `EBADF` before fallback classification.

The unit matrix covers two upper-word encodings × closed/read-only/pipe-read outputs × pipe/socket/directory/stdout/procfs inputs = 30 rows. The real-KVM matrix covers the same encodings and outputs × pipe/socket/directory/stdout inputs = 24 rows. Every row asserts `EBADF`; pipe/socket cases also prove their input byte was not consumed.

## Exact-head test plan

- `cargo fmt --all -- --check`: green
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: green
- new positive-output focused unit: 1/1
- negative-output focused unit: 1/1
- original focused low-word unit: 1/1
- required real-KVM fixture: 1/1
  - native once, direct KVM twice, Tool KVM twice
  - exact stdout, stderr, and exit-status comparison; no L2/log/replay-parity claim
- `cargo test -p reverie-kvm --lib`: 819/819
- `cargo test -p reverie-kvm --lib -- --test-threads=1`: 819/819

Eight non-equivalent mutants are killed by both unit and KVM cells: restoring any of the three strict full-register decoders; clearing low-word bit 31 at any decoder; restoring late output validation; or omitting early writability validation. KVM exits distinguish them as 8, 9, 6, 34, 4, 5, 46, and 58. Removing only the explicit negative branch is now equivalent because the stronger general early output validation subsumes it; it is not counted as a killed mutant.

The pre-rebase content-identical history included three disclosed unfiltered parallel flakes. The rebased exact head passed parallel and serial 819/819 without filtering, skipping, weakening, or relabelling any case.

## Frozen artifact

- base: `96598dc07490ec15411845d27ed5bfd5c0a34076`
- head: `748321a70995f1e7c1d5385de09f620f4becd685`
- full diff SHA-256: `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`
- correction-only diff SHA-256: `30777bf3ee93ac725b48e10f55e68f0110713e5ace560feb6efa45beca5a4b41`

Three independent Codex reviews approved these rebased exact artifacts with no correctness finding or goalpost moving. A fresh independent Claude-family review of this exact committed head is still required.

## Known limits

- Non-null-offset precedence remains pre-existing: ten reviewed rows were native `ESPIPE`, old base `EBADF`, and former head `5d65b9f5` `ENOSYS`. This correction restores `EBADF` like the old base; it does not claim native `ESPIPE` parity.
- A private synthetic `/proc/*/fdinfo` carrier is intercepted before `sendfile` and can still return pre-dispatch `ENOSYS`.
- A valid writable pipe output retains the existing `ENOSYS` mediated fallback; this slice does not add pipe zero-copy support.
- `lseek`'s pre-existing high-word `whence` decoding and captured-pipe invalid-whence ordering remain unchanged.
- High-word fdinfo `lseek` routing lacks a dedicated regression case; centralized decoding precedes that unchanged route.

## Review hold

Do not merge until the primary coordinator explicitly relays a fresh independent Claude-family verdict for exact head `748321a70995f1e7c1d5385de09f620f4becd685` and full diff SHA-256 `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`.
