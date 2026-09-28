[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Summary

- consume only the Linux low 32-bit descriptor word for both `sendfile` descriptors and the `lseek` descriptor
- reject a negative decoded `sendfile` output fd before ordinary input routing can return the KVM `ENOSYS` fallback
- preserve existing fdinfo, captured-output, ordinary-file, and offset behavior outside that corrected ordering
- add focused unit coverage plus required real-KVM native/direct/Tool parity coverage

This is an isolated follow-up from the fd-decoder census. It does not change `lseek` whence decoding or `CLOSE_RANGE_UNSHARE` semantics.

## External-review response

Review of the former head `730123acb922cfa30fa1bb9281391776088fb35d` found that a negative low-word `out_fd` was checked only after input classification. Pipe, socket, directory, standard-stream, and ordinary procfs inputs could therefore return `ENOSYS` instead of `EBADF`.

Head `5d65b9f50f124f615c5f0c862af7b94919037c3f` adds the immediate `out_fd < 0` check. Unit coverage crosses both required encodings (`0x5a5a5a5a80000001` and sign-extended `-1`) with all five input classes. The real-KVM fixture crosses both encodings with pipe and AF_UNIX socket inputs.

## Test plan

- `cargo fmt --all -- --check`
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`
- new focused negative-output unit: 1/1
- original focused low-word unit: 1/1
- required real-KVM fixture: 1/1
  - native once, direct KVM twice, Tool KVM twice
  - exact stdout, stderr, and exit-status comparison; no L2/log/replay-parity claim
- `cargo test -p reverie-kvm --lib`: 818/818
- `cargo test -p reverie-kvm --lib -- --test-threads=1`: 818/818

Removing only the new guard makes both the unit and KVM regressions fail: the unit observes `ENOSYS` instead of `EBADF`, and the KVM guest exits 22 on the first pipe case. Restoring it returns both to green.

The earlier decoder mutations remain killed independently. Two preceding parallel runs on the former head remain disclosed at 816/817: one failed the unrelated existing SIGPIPE test and one failed unrelated accept cleanup on host `EAGAIN`; later exact-head runs were clean.

## Frozen artifact

- base: `7bc49f4c4d63018adba61d49246e847569518e33`
- head: `5d65b9f50f124f615c5f0c862af7b94919037c3f`
- full diff SHA-256: `2b8dbdfb356d055e3573aecc23cd0dbb02f0a4c0bb0e74b2f75b71febdd6f88a`
- correction-only diff SHA-256: `c1847753c83977877f2d7ff5186b9f4a2c5f2625f3e73bab4bfd8b5749380dfc`

## Known limits

- `sendfile`'s pre-existing fd-versus-offset-pointer precedence remains unchanged.
- A private synthetic `/proc/*/fdinfo` carrier is intercepted before `sendfile`; its negative-output ordering is pre-existing on the base and is not claimed fixed here.
- `lseek`'s pre-existing high-word `whence` decoding and captured-pipe invalid-whence ordering remain unchanged.
- High-word fdinfo `lseek` routing lacks a dedicated regression case; centralized decoding precedes that unchanged route.

## Review hold

Do not merge until the coordinator explicitly relays an independent Claude-family verdict for exact head `5d65b9f50f124f615c5f0c862af7b94919037c3f` and full diff SHA-256 `2b8dbdfb356d055e3573aecc23cd0dbb02f0a4c0bb0e74b2f75b71febdd6f88a`.
