# Exact-tree qualification for documentation follow-up

Repository: rrnewton/reverie
Base: f7bd85e11dd258112148ed2cba6531501a1a00d9
Prior reviewed head: 6152ee999a41324be36456bb0a9b94a703711068
Head: c4376212990ae5072a7bcfd0223ec52003cbaac0
Tree: c59d9ce7d1e77e7622fc3060ab0314da522b44f1
Complete base-to-head binary diff: 258,096 bytes, SHA-256 505d6305a9542d492be8aab4f667fe0a690bd12ec09e9f53553b1f8e148dd79d
Follow-up from prior head: 2,756 bytes, SHA-256 91758615052dfeb221eab4b793fe7c5a7f85d6c5c5a7d0c7a8c14b681f02745b, changing only `reverie/src/tool.rs` and `reverie/src/process_signal_control.rs` documentation.

Every current qualification log records its exact command, working directory, expected and observed pre/post head and tree, raw exit code, and pre/post free space.

- Forced publication fence passed in debug and optimized release profiles. The release rebuild took 1m30s; each exact test passed 1/1 with 773 filtered.
- The first exact-head default-parallel library run exposed three existing timing-sensitive fixture failures after 771 passes: two pipe/SIGPIPE expectations and `fatal_worker_ro_delayed_waiter_qualifies`. Their complete raw run is retained as `context/c437-lib-parallel-three-fixture-failures.log`. All three exact retries passed, and the complete default-parallel retry passed 774/774 in 12.11s.
- The first exact-head serialized library run exposed `fatal_worker_rw_delayed_waiter_qualifies` after 773 passes. Its complete raw run is retained as `context/c437-lib-serialized-fatal-waiter-failure.log`. The exact retry passed, and the complete serialized retry passed 774/774 in 31.06s.
- Those four failures are not counted as passes or hidden. Head c4376212 changes documentation only from the fully qualified 6152ee99 source tree; the raw failures and retries are supplied for the reviewer to classify. The separate fixture repairs are outside this pull request.
- Default-parallel terminal-fork integration passed 23/23 in 0.36s.
- Exact real-KVM grandchild matrix passed 1/1 in 2.20s with post-probe diagnostics and no skip. Modes 6-8 retain waitable, `SIG_IGN`, and `SA_NOCLDWAIT` coverage for a terminal transitive root and live direct parent.
- `cargo test -p reverie-core`: 14 library + 4 bridge + 1 validation + 3 doctests passed; one pre-existing backend doctest remained ignored.
- `cargo check -p reverie-kvm --tests`: passed.
- `cargo check --workspace --all-targets`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed with empty command output.
- Exact base-to-head `git diff --check`: passed with empty command output.
- Tracked tree is clean; protected `HANDOFF.md` and `ignored/` are the only untracked entries.

The follow-up states the contract requested by the actual Claude v5 review: the callback's synchronous prefix spans the fence and must neither await nor block on parent or guest progress; work after publication may await. It also documents KVM's standalone duplicate preflight and the committing Tool-scheduler -> exact-parent-transaction -> run-wide-registry/signal-state lock order, without claiming that every fast path acquires those locks.

Free disk at the final recorded status check was 443,178,659,840 bytes, above the required 429,496,729,600-byte (400 GiB) floor. These are Reverie component checks. No Hermit exact-main `safehermit` run, record/replay, or full backend-parity result is claimed.
