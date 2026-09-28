Exact head 7d1f29f973978e3ea4bbda8768b855a91fe5c6ed, tree 4f66c7d04e92640b3ddb883d703f3da59a79de42, was committed and tracked-clean before these author-run commands.

Passed at that tree:

- `REVERIE_REQUIRE_KVM=1 cargo test --offline --locked -j 2 -p reverie-kvm --lib runtime::signal_cleanup_completion_tests -- --nocapture`: 8 passed, 0 failed, 0 ignored, 736 filtered.
- `REVERIE_REQUIRE_KVM=1 cargo test --offline --locked -j 2 -p reverie-kvm --lib -- --nocapture`: 744 passed, 0 failed, 0 ignored, 0 filtered, 7.61 seconds.
- `cargo fmt --all -- --check`.
- `cargo check --offline --locked -j 2 -p reverie-kvm --lib`.
- `cargo clippy --offline --locked -j 2 -p reverie-kvm --lib -- -D warnings`.
- `cargo check --offline --locked -j 2 -p reverie-kvm --lib --features native-test-support`.

The full-suite run initially found one stale readiness fixture and one clock EINTR at the pre-rebase tree. The clock case passed immediately alone. The stale fixture had already been repaired independently by https://github.com/rrnewton/reverie/pull/600; after rebasing without editing that repair, the full 744-test run passed. These are author-run outputs, not independent reviewer execution. No Hermit consumer/runtime execution of this exact head is claimed yet.
