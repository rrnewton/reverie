# Exact-tree qualification for exact head

Repository: rrnewton/reverie
Base: f7bd85e11dd258112148ed2cba6531501a1a00d9
Head: e1ee50dad596f33903caa80313ff8eb32be34754
Tree: d21731c768d50d9a4980e3657a4f088424eae6c0
Complete base-to-head binary diff SHA-256: b4216063340b5574c83035cffc31232c7f1e73dd1b5b449f51f51fda4daa8e35

The commands below ran on the clean tracked tree later committed as tree d21731c768d50d9a4980e3657a4f088424eae6c0. All listed final commands returned raw status 0, captured before output inspection.

- `cargo test -p reverie-kvm --lib`: final rerun 766 passed, 0 failed, 0 ignored in 7.73s.
- Transparency note: the immediately preceding full-library run had 765 pass and one unrelated `descriptor_retirement_accept_cleanup_releases_both_guards` failure at `accept().unwrap()` with host `EAGAIN`. The exact failed test then passed alone, and the complete suite passed on rerun. Both raw logs are retained; decide whether this is acceptable evidence rather than hiding it.
- Focused family/publication module: 34 passed, 0 failed, 732 filtered.
- `cargo test -p reverie-kvm --test static_elf terminal_fork::`: 23 passed, 0 failed, 313 filtered.
- exact real-KVM `child_waitability_callback_and_auto_reap_are_observed_on_real_kvm`: 1 passed, 335 filtered, 0.37s. The raw log has no `/dev/kvm` skip line.
- exact real-KVM `grandchild_family_transitions_are_causal_on_real_kvm`: 1 passed, 335 filtered, 1.50s. Four child-failure diagnostics are expected fail-closed fixture modes; the test passed and did not skip.
- `cargo test -p reverie-core`: 14 library + 4 signal-bridge + 1 validation-source + 3 doctests passed; one pre-existing backend doctest remained ignored.
- `cargo check --workspace --all-targets`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed with empty output.
- exact base-to-head `git diff --check`: passed with empty output.
- tracked status at exact head: clean; protected untracked HANDOFF/evidence are deliberately outside the commit.

Free disk after the final gates was 454,276,620,288 bytes, above the required 400 GiB floor. These are Reverie component checks. No Hermit exact-main `safehermit` run, record/replay, or full backend-parity result is claimed.
