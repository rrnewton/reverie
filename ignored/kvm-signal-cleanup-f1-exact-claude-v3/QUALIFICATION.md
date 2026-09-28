# Exact-head qualification

Review target: Reverie base `60f2d369b49e6ffbc2b2d9d0f0e55fead0ba6b09`, head `ce442fce6b1ee366e0c3a9cff7fa981ff8893ae4`, tree `a83ef7f9e0645b28d3dbe83ed02a32e461674cc7`.

The clean-build source-bound receipt is `../kvm-signal-cleanup-f1-qualification-v5/receipt.json`, SHA-256 `ef91cf0ef114a07db0343b102c3b272246b7d9789881c8acfca325fcf62757b8`. Its runner is hashed in the receipt. Pre/post head, tree, tracked status, changed-path set, and all five file hashes are identical. Every phase has raw stdout/stderr and exit 0:

- focused KVM-required completion module: 10 passed, 0 failed/ignored, 736 filtered;
- full KVM-required library at explicitly bounded 64-way parallelism: 746 passed, 0 failed/ignored/filtered, 8.12 seconds in the test binary;
- full KVM-required library at one test thread: 746 passed, 0 failed/ignored/filtered, 26.87 seconds;
- formatting;
- clean-built default library check and strict Clippy;
- clean-built non-test `native-test-support` library check and strict Clippy;
- all-target/all-feature strict Clippy.

The 64-thread bound is disclosed evidence, not Cargo's default on this 316-logical-CPU host. The prior clean unbounded receipt `../kvm-signal-cleanup-f1-qualification-v4/receipt.json`, SHA-256 `28d3ddc004816851902c9603b80fc0ee43efa94d0b665c0d41ee789d5ab6446b`, is preserved and failed after focused 10/10 when unchanged `descriptor_retirement_accept_cleanup_releases_both_guards` immediately read `EAGAIN` from its deliberately nonblocking Unix socket instead of EOF.

A predeclared no-early-stop base/head matrix is preserved at `../kvm-signal-cleanup-parallel-matrix-v1/receipt.json`, SHA-256 `1ee92cd1df0c4100a06d6cf2d675bc3e4e39fd00045a1c3d00f2bd5733cb996c`. Under identical unbounded default concurrency (316 test threads):

- base: 3/5 passed; 2/5 failed in unchanged `positioned_vectored_io_handles_pipes_partial_writes_and_sigpipe` (wrote 4 instead of observing `EPIPE`);
- head: 1/5 passed; one failed in that same unchanged pipe/SIGPIPE test and three failed in the unchanged nonblocking accept/EOF test.

The changed `executor.rs` hunk is only a `cfg(test | native-test-support)` attribute on a test/native-only wrapper; both failing test bodies are unchanged. The new signal-cleanup module passed in every recorded run. This establishes a latent extreme-concurrency suite problem at base but does not turn any failed head run into a pass. Assess the limitation explicitly.

These are author-run Reverie L0 checks. No exact-head Hermit consumer, guest parity, ptrace/KVM comparison, or full KVM parity is claimed.
