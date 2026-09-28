[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Summary

- consume only the Linux-declared 32-bit descriptor word for `fallocate`, `fsync`, `fdatasync`, `readahead`, and `sync_file_range`
- preserve low-word bit-31 `EBADF` behavior and the existing range/flag validation order
- add focused unit coverage plus a required real-KVM native/direct/Tool parity test

This is the first isolated follow-up from the read-only fd-decoder census. It intentionally does not change the remaining `read`, `pread64`, `sendfile`, `lseek`, `fstat`, `fchdir`, or `getdents64` decoders.

## Test plan

- `cargo fmt --all -- --check`
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`
- `cargo test -p reverie-kvm --lib executor::tests::positioned_write_seek_truncate_and_sync_round_trip -- --exact --nocapture`
- `REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm --test static_elf storage_fd_syscalls_consume_low_words_on_kvm -- --exact --nocapture`
  - native once, direct KVM twice, Tool KVM twice
  - exact stdout, stderr, and exit-status comparison; no L2/log/replay-parity claim
- `cargo test -p reverie-kvm --lib`: clean 815/815 exact-head run
- `cargo test -p reverie-kvm --lib -- --test-threads=1`: clean 815/815 exact-head run

Five exact-head reds remain disclosed. Author runs reproduced the existing SIGPIPE-sensitive failure (814/815) and separately failed only the reserved-kick test with `Kvm(Error(4))` (814/815). One review run failed only the unchanged descriptor-retirement accept-cleanup test with `EAGAIN` (814/815; then 10/10 isolated). Another reviewer saw two different unchanged cells fail in separate serial runs (each 814/815; each then 5/5 isolated). Fresh post-review serial and default runs both passed 815/815, in addition to earlier clean author/reviewer runs. This satisfies the clean-run gate without erasing the earlier observations or claiming their causality was disproved.

Each of the four old `i32::try_from` decoder implementations was also restored individually as a causal mutant. Both the focused unit and required KVM cell rejected every mutant.

Three independent adversarial reviews approved exact head `fb5698fb210f6549c9256ae80ef91cf6e682cfd2` and frozen diff SHA-256 `8a641d5594bff37bae9da8ff79379eefb7713c3e529fd09198f38cc348b3adad` for opening. All explicitly found no goalpost moving. These are not Claude-family release approval.

## Review hold

Do not merge until the coordinator explicitly relays an independent Claude-family verdict for this exact head and confirms every stated red gate is resolved.
