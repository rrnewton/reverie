# Exact-tree qualification for exact head

Repository: rrnewton/reverie
Base: f7bd85e11dd258112148ed2cba6531501a1a00d9
Head: 6152ee999a41324be36456bb0a9b94a703711068
Tree: 1aa237f33bbf42528ae3082e09acc7cfe1d18b75
Complete base-to-head binary diff: 257,157 bytes, SHA-256 8eed79a86b435be20433ecf98e2ca4d855cdb2d07b81c98a55dc0e7620b94a57

Every current qualification log records its exact command, working directory, expected and observed pre/post head and tree, raw exit code, and pre/post free space. All final commands ran on the exact committed tree.

- Focused family/publication module: 38 passed, 0 failed in 0.17s, including both direct-parent orderings and explicit fail-closed missing-family behavior.
- Forced publication fence: 1 passed. This drives both `wait4(WNOHANG)` and `waitid(WNOHANG)` into an already-armed publication fence and requires the exact event after release.
- Fenced failure cleanup: 1 passed. This proves failure wakes the waiter and that publication cannot resurrect afterward.
- A first fresh `cargo test -p reverie-kvm --lib` run found one failure after 773 passes: the pre-existing `descriptor_retirement_accept_cleanup_releases_both_guards` test unwrapped `EAGAIN` from an immediate nonblocking peer read. The test is unchanged from base f7bd85e, and the pull-request diff does not touch it. The complete raw failure is retained as `context/fresh-6152-lib-parallel-eagain-failure.log`; it is not relabelled as a pass. Its exact retry passed 1/1, the complete default-parallel retry passed 774/774 in 12.52s, and the complete serialized run passed 774/774 in 38.41s. Decide whether this pre-existing fixture timing failure affects the prerequisite; its separate repair is outside this change.
- `cargo test -p reverie-kvm --test static_elf terminal_fork:: -- --nocapture`: 23 passed, 0 failed in 1.03s with the default parallel harness.
- Serialized terminal-fork rerun: 23 passed, 0 failed in 6.09s.
- Exact cancellation/callback-panic case: 1 passed in 0.23s with the deliberate callback panic and required wait-collected event.
- Exact real-KVM child-wait publication: 1 passed, 335 filtered, 0.41s. The retained command used `--nocapture`; its known `/dev/kvm` skip line is absent.
- Exact real-KVM grandchild matrix: 1 passed, 335 filtered, 3.04s, with post-probe diagnostics and no skip. Modes 6-8 cover a logically terminal root with a live direct parent for waitable, `SIG_IGN`, and `SA_NOCLDWAIT`, at virtual root PIDs 1 and 3. Retained diagnostics are the expected fail-closed unsupported cases elsewhere in the matrix.
- Optimized release publication-fence regression: 1 passed, 773 filtered.
- Injected backend failure routing: 1 passed, proving a typed wait-ledger failure becomes `HandlerOutcome::RuntimeError`, not a guest errno.
- `cargo test -p reverie-core`: 14 library + 4 bridge + 1 validation + 3 doctests passed; one pre-existing backend doctest remained ignored.
- `cargo check -p reverie-kvm --tests`: passed.
- `cargo check --workspace --all-targets`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed with empty command output.
- Exact base-to-head `git diff --check`: passed with empty command output.
- Tracked tree is clean; protected `HANDOFF.md` and ignored evidence remain untracked.

The old a2e414dc default-parallel failure was a real waitability race, not a load-only timeout. Its raw log is retained as `context/a2e-terminal-fork-parallel-race.log`: the parent reached the callback-prefix wait event but not wait collection, later assertions saw status 255, and the controller timed out. The isolated and serialized passes at that head merely selected a winning interleaving and did not qualify it. Head 6152ee99 adds a deterministic publication-fence regression and the current exact-head default-parallel 23/23 integration run; those are the evidence offered for closing that race.

Free disk at the final recorded status check was 448,325,578,752 bytes, above the required 429,496,729,600-byte (400 GiB) floor. These are Reverie component checks. No Hermit exact-main `safehermit` run, record/replay, or full backend-parity result is claimed.
