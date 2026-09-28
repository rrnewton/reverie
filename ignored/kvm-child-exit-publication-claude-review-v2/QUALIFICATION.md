# Exact-head qualification

Repository: rrnewton/reverie
Base: f7bd85e11dd258112148ed2cba6531501a1a00d9
Head: d794a5e32c096d942e965bad399e7928683b8c43
Tree: f2cf92f97e4539e127639a4dc4a3e1c140589e19
Complete base-to-head binary diff SHA-256: 109d749cb60a603418be8d9f1ac31b7577e11e7497c8d8734f75946b3f0d79f0

All listed commands returned raw status 0, captured before reading their output.

- `cargo test -p reverie-kvm --lib -- --test-threads=1`: 762 passed, 0 failed, 0 ignored.
- `cargo test -p reverie-kvm --test static_elf terminal_fork:: -- --nocapture`: 23 passed, 0 failed, 0 ignored.
- exact real-KVM `grandchild_family_transitions_are_causal_on_real_kvm`: 1 test passed; its fixture covers root PID 1 and 3 across forced-live, WNOWAIT-zombie, wait4-consumed, waitid-consumed, explicit SIG_IGN, and SA_NOCLDWAIT modes. The four child-failure diagnostics are expected fail-closed controls.
- `cargo check --workspace --all-targets`: passed. Existing vendored-C fallthrough warnings are retained in the raw log; Rust compilation succeeded.
- `cargo clippy -p reverie-kvm --all-targets -- -D warnings`: passed.
- `cargo test -p reverie-core`: 14 library + 4 signal-bridge + 1 validation-source + 3 doctests passed; one pre-existing backend doctest remained ignored.
- `cargo fmt --all -- --check`: passed with empty output.
- `git diff --check`: passed with empty output.

The final disk reading retained by the driver was 458,551,812,096 bytes free, above the required 400 GiB floor. These are Reverie component checks. No Hermit consumer, exact-main safehermit run, record/replay, or full parity result is claimed.
