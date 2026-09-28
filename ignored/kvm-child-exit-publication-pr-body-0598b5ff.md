[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Plain-language summary

Expose the generation-bound KVM child-exit publication primitive needed by Hermit's deterministic scheduler. The API carries exact parent and child generations, terminal status, waitability, virtual UID, and CPU ticks rather than reconstructing identity from reusable numeric PIDs.

Publication validates the exact live relationship and Linux SIGCHLD policy before mutation. It preserves explicit SIG_IGN suppression, SA_NOCLDWAIT notification with auto-reap, standard-signal coalescing with first-siginfo retention, independent signalfd carriers, sorted mask-aware recipients, and a retained child-specific receipt or post-commit failure. Both process bindings remain alive through callback acknowledgement.

This is the Reverie prerequisite only. The following Hermit consumer will invoke it synchronously inside the backend child-wait callback while holding the scheduler reservation, then remove Hermit's synthetic child-exit timer and numeric host-signal fallback. This pull request does not claim KVM determinism or backend parity complete.

Continuation of https://github.com/rrnewton/reverie/pull/599.

## Validation at exact head 0598b5ffbeb737866372f89224d915efaeb29943

- `cargo test -p reverie-kvm --lib process_signal_publication`: 23 passed
- `cargo check -p reverie-kvm --lib --features native-test-support`: passed
- `cargo clippy -p reverie-kvm --lib --features native-test-support -- -D warnings`: passed
- real-KVM exact child-wait callback test: 1 passed, 333 filtered
- `cargo test -p reverie-core`: passed
- serialized `cargo test -p reverie-kvm --lib -- --test-threads=1`: 753 passed
- `git diff --check`: passed

The rebased diff is byte-identical to the independently reviewed pre-rebase diff, SHA-256 `b973675db1aa7125c3fcb62a06026494d7cc9d69955727baeca8f9fb0aad8f53`. No assertion, tolerance, exemption, skip, comparator, label, or gate was weakened.

Independent adversarial review APPROVED exact head `0598b5ffbeb737866372f89224d915efaeb29943` with no findings after tracing the real callback lifetime, causal-fence contract, generation validation, Linux child-signal policy, coalescing, recipient selection, and retained failure semantics. Residual coverage is explicit: the real Hermit consumer is the next change; killed/core encoding and post-commit carrier failure are unit-covered rather than composed through the real KVM callback.
