[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Summary

- consume only the Linux low 32-bit descriptor word for both `sendfile` descriptors and the `lseek` descriptor
- preserve existing fdinfo, captured-output, ordinary-file, error-ordering, and offset behavior
- add focused unit coverage plus required real-KVM native/direct/Tool parity coverage

This is an isolated follow-up from the fd-decoder census. It does not change `lseek` whence decoding or `CLOSE_RANGE_UNSHARE` semantics.

## Test plan

- `cargo fmt --all -- --check`
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`
- `cargo test -p reverie-kvm --lib executor::tests::sendfile_and_lseek_consume_low_descriptor_words -- --exact --nocapture`
- `REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm --test static_elf sendfile_and_lseek_consume_low_descriptor_words_on_kvm -- --exact --nocapture`
  - native once, direct KVM twice, Tool KVM twice
  - exact stdout, stderr, and exit-status comparison; no L2/log/replay-parity claim
- `cargo test -p reverie-kvm --lib -- --test-threads=1`: 817/817
- `cargo test -p reverie-kvm --lib`: clean exact-head run 817/817

Restoring the old sendfile output, sendfile input, or lseek decoder independently is killed by both the focused unit and required KVM cells.

Two preceding parallel runs remain disclosed at 816/817: one failed the unrelated existing SIGPIPE test and one failed unrelated accept cleanup on host `EAGAIN`. A subsequent exact-head parallel run passed 817/817; serial passed 817/817. The failures are preserved, not relabelled or deleted.

## Known limits

- `sendfile`'s pre-existing fd-versus-offset-pointer precedence remains unchanged.
- `lseek`'s pre-existing high-word `whence` decoding and captured-pipe invalid-whence ordering remain unchanged.
- High-word fdinfo `lseek` routing lacks a dedicated regression case; centralized decoding precedes that unchanged route.

## Review hold

Do not merge until the coordinator explicitly relays an independent Claude-family verdict for exact head `730123acb922cfa30fa1bb9281391776088fb35d` and confirms every stated red gate is resolved.
