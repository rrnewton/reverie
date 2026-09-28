# Exact-tree qualification for exact head

Repository: rrnewton/reverie
Base: f7bd85e11dd258112148ed2cba6531501a1a00d9
Head: a2e414dca1227903a56dcf5bf8a7bc0bf2f6ea06
Tree: 859eb9e457f6a4fc86e17be13d349bdaf7363ade
Complete base-to-head binary diff: 240,803 bytes, SHA-256 ed53ad69e5686d017c124faa238636e56f99028544c1ce45c5c0a6863b0f545e

All final commands below ran on the exact committed tree. Raw status was captured before output inspection.

- `cargo test -p reverie-kvm --lib`: 771 passed, 0 failed in 7.41s.
- Exact-head serialized rerun: 771 passed, 0 failed in 28.37s.
- Focused family/publication module: 37 passed, 0 failed, including typed wait drift, stale effect, ancestry corruption, and the explicit live-root/terminal-root order boundary.
- `cargo test -p reverie-kvm --test static_elf terminal_fork:: -- --test-threads=1 --nocapture`: 23 passed, 0 failed in 4.22s.
- Transparency note: the preceding parallel terminal-fork run had 22 pass and one `cancellation_preserves_status_across_child_wait_callback_panic` controller timeout at 5.00s under 23-way host load. The exact failed case immediately passed alone in 0.08s with its required deliberate callback panic and event assertions, then the complete serialized set passed. All three raw logs are retained; decide whether the timeout is acceptable rather than hiding it.
- Exact real-KVM child-wait publication: 1 passed, 335 filtered, 0.16s, with no `/dev/kvm` skip line.
- Exact real-KVM grandchild matrix: 1 passed, 335 filtered, 1.43s, with four expected fail-closed fixture diagnostics and no skip.
- Release-mode status/identity validator: 1 passed, 770 filtered. This specifically proves the former `debug_assert` gap is closed with optimization enabled.
- Injected backend failure routing: 1 passed, proving a typed wait-ledger failure becomes `HandlerOutcome::RuntimeError`, not a guest errno.
- `cargo test -p reverie-core`: 14 library + 4 bridge + 1 validation + 3 doctests passed; one pre-existing backend doctest remained ignored.
- `cargo check --workspace --all-targets`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed with empty output.
- Exact base-to-head `git diff --check`: passed with empty output.
- Tracked tree is clean; protected `HANDOFF.md` and ignored evidence remain untracked.

Free disk at packet construction remained above the required 400 GiB floor. These are Reverie component checks. No Hermit exact-main `safehermit` run, record/replay, or full backend-parity result is claimed.
