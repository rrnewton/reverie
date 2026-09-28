[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Summary

- consume only the Linux low 32-bit descriptor word for both `sendfile` descriptors and the `lseek` descriptor
- validate the decoded `sendfile` output before any input/output `ENOSYS` classification, restoring native/base `EBADF` ordering for negative, closed, read-only, and pipe-read-end output aliases
- keep `ENOSYS` only for valid but unsupported writable routes
- add focused unit coverage plus required real-KVM native/direct/Tool parity coverage, including proof that rejected calls do not consume pipe/socket input bytes

This is an isolated follow-up from the fd-decoder census. It does not change `lseek` whence decoding, non-null-offset precedence, private-fdinfo predispatch, writable-pipe support, or `CLOSE_RANGE_UNSHARE` semantics.

## External-review response

Review of former head `730123acb922cfa30fa1bb9281391776088fb35d` found that negative `out_fd` validation happened after input routing. Head `5d65b9f50f124f615c5f0c862af7b94919037c3f` fixed the negative case, but fresh review then showed that positive high-word aliases of unusable outputs could still reach input classification first and return `ENOSYS`.

Head `79f139bd7e1b8b789e1b770597c03b9f0bbd6050` validates output existence and writability immediately after low-word decoding. A modeled fd must pass `ensure_writable`, or the fd must satisfy the existing open-standard contract; otherwise the call returns `EBADF` before fallback classification.

The unit matrix covers two upper-word encodings × closed/read-only/pipe-read outputs × pipe/socket/directory/stdout/procfs inputs = 30 rows. The real-KVM matrix covers the same encodings and outputs × pipe/socket/directory/stdout inputs = 24 rows. Every row asserts `EBADF`; pipe/socket cases also prove their input byte was not consumed.

## Test plan

- `cargo fmt --all -- --check`: green
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: green
- new positive-output focused unit: 1/1
- negative-output focused unit: 1/1
- original focused low-word unit: 1/1
- required real-KVM fixture: 1/1
  - native once, direct KVM twice, Tool KVM twice
  - exact stdout, stderr, and exit-status comparison; no L2/log/replay-parity claim
- `cargo test -p reverie-kvm --lib`: 819/819 on the exact head
- `cargo test -p reverie-kvm --lib -- --test-threads=1`: 819/819 on the exact head

The late-validation mutant fails the unit matrix with `ENOSYS` instead of `EBADF` and makes the KVM guest exit 46. The existence-only mutant fails on the first read-only output and makes the KVM guest exit 58. The earlier negative-output and strict-decoder mutants remain killed.

Three earlier content-identical unfiltered parallel runs remain disclosed: 817/819 for unrelated reserved-kick `Kvm(Error(4))` plus positioned-I/O SIGPIPE; 818/819 for unrelated descriptor-retirement host `EAGAIN`; and 818/819 for the positioned-I/O SIGPIPE case. A later pre-commit run and the final exact-head parallel run passed 819/819; exact-head serial also passed 819/819. No failing case was filtered, skipped, weakened, or relabelled.

## Frozen artifact

- base: `7bc49f4c4d63018adba61d49246e847569518e33`
- head: `79f139bd7e1b8b789e1b770597c03b9f0bbd6050`
- full diff SHA-256: `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`
- correction-only diff SHA-256: `30777bf3ee93ac725b48e10f55e68f0110713e5ace560feb6efa45beca5a4b41`

Three independent Codex reviews approved these exact artifacts with no correctness finding and no goalpost moving. A fresh independent Claude-family review of this exact committed head is still required.

## Known limits

- Non-null-offset precedence remains pre-existing: ten reviewed rows were native `ESPIPE`, base `EBADF`, and former head `5d65b9f5` `ENOSYS`. This correction restores `EBADF` like base; it does not claim native `ESPIPE` parity.
- A private synthetic `/proc/*/fdinfo` carrier is intercepted before `sendfile` and can still return pre-dispatch `ENOSYS`.
- A valid writable pipe output retains the existing `ENOSYS` mediated fallback; this slice does not add pipe zero-copy support.
- `lseek`'s pre-existing high-word `whence` decoding and captured-pipe invalid-whence ordering remain unchanged.
- High-word fdinfo `lseek` routing lacks a dedicated regression case; centralized decoding precedes that unchanged route.

## Review hold

Do not merge until the coordinator explicitly relays an independent Claude-family verdict for exact head `79f139bd7e1b8b789e1b770597c03b9f0bbd6050` and full diff SHA-256 `af4e2e7f20c4262b3e0cc4eca4ada9382c55a2660da555bcca985564ca538d9c`.
