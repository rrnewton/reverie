[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Summary

- consume only the Linux low 32-bit descriptor word for `read` and `pread64`
- preserve special-descriptor routing and existing validation/error ordering
- add focused unit coverage plus required real-KVM native/direct/Tool parity coverage

This is the second isolated follow-up from the fd-decoder census. It intentionally leaves `sendfile`, `lseek`, `fstat`, `fchdir`, and `getdents64` for separate slices.

## Test plan

- `cargo fmt --all -- --check`
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`
- `cargo test -p reverie-kvm --lib executor::tests::read_and_pread64_consume_low_descriptor_words -- --exact --nocapture`
- `REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm --test static_elf read_and_pread64_consume_low_descriptor_words_on_kvm -- --exact --nocapture`
  - native once, direct KVM twice, Tool KVM twice
  - exact stdout, stderr, and exit-status comparison; no L2/log/replay-parity claim
- `cargo test -p reverie-kvm --lib`: 816/816
- `cargo test -p reverie-kvm --lib -- --test-threads=1`: 816/816

Restoring either old `i32::try_from` decoder independently is killed by both the focused unit and required KVM cells.

Preliminary review corrected a bad oracle before freeze: `pread64` with offset `-1` returns `EINVAL` before fd lookup on Linux. The final bad-fd discriminator uses offset `0`, while a separate valid-high-fd case requires `EINVAL` for offset `-1`.

Pre-existing ordinary-file `pread64` ordering gaps for zero-length/noncanonical pointers and negative-offset combined with a bad buffer or bad fd remain out of scope.

## Review hold

Do not merge until the coordinator explicitly relays an independent Claude-family verdict for exact head `03ce6d472df1537b68fdc6ead5e2a89d5294a390` and confirms every stated red gate is resolved.
