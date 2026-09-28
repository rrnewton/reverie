# Base release-test control

- Completed: `2026-09-20T14:24:36-07:00`
- Reverie base: `be09e5100bca6dad77aede0349de9a5c92990854`
- Reviewed head: `1fdadb7940dc232d07c1e36494f0f102b74f3140`
- Command: `cargo test --release -p reverie-kvm --features native-test-support --test native_entry_signal -- --nocapture`
- Result: exit 0; 1 passed, 0 failed; build/test elapsed 20.56 seconds.

The detached base checkout received only the reviewed test-only exposure in
`entry.rs`, the reviewed wrapper in `runtime/native_test_support.rs`, and the
reviewed `tests/native_entry_signal.rs`. The production implementation under
test, `entry/signal.rs`, was not changed: its Git object is
`191736e429c7db28b5a84d7521488d9d11e42abd`, identical to the base tree.

The base implementation passed this control, so `native_entry_signal` is a
shape/preservation guard, not a negative reproducer of the release-only
Hermit failure. The negative defect evidence remains the captured old Hermit
release failure and signal trace in the Claude review packet.

Evidence outside this detached checkout:

- `../base-negative-control.out` SHA-256
  `3993c4097a793500b988d1c76c15fce38a491ee4cc7aa7dfc385046380d79b64`
- `../base-negative-control.err` SHA-256
  `caf457d585724a931845e0910a5058898be237aa583992394b6016a2323651d1`
- transplanted test SHA-256
  `0a9f1ea53bade4b7bbdb65a8a9ed797c9e10317e3c1413d3f158c853f6ee4735`
