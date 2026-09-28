[hermit2, degraded-dev-hermit-14, gpt-6-astra, devbig014, role=impl]

## Plain Language Summary and Project Impact

Restore ordinary KVM library builds and preserve signal-cleanup errors through cancellation and callback destruction. The ownership refactor in https://github.com/rrnewton/reverie/pull/598 left production cleanup calling a test-only helper, so normal Hermit consumption failed to compile. This repair uses the production completion path and retains every panic payload with its owning backend.

## Summary

Ready consuming cleanup still takes precedence over cancellation. Pending cleanup observes failure inside the owning driver, so callback and failure-future destruction remain caught. Finalization transfers any remaining signal-removal records and the original syscall result into the error. A child-registration forwarding helper is compiled only in the test/native-support configurations that use it; production keeps the panic-owning registration path.

Resolve the required F1 follow-up from the earlier Claude review: signal-dequeue notification starts with an empty child-start vector, and both `SignalGuard::DequeueNotification` and the lifecycle boundary reject child-producing injection before either producer can publish a start gate. The flush now checks that invariant after callback destruction. A future violation becomes a typed diagnostic folded through the completion finalizer; any impossible gate is cancelled and joined without unwinding across the selected outcome or panic payloads. A production-path test requires `ENOSYS` for `fork`, `vfork`, thread `clone`, and `clone3`; negative controls prove typed violation, poison recovery, gate cancellation, and exact error/panic retention.

## Determinism

Ordinary callbacks retain their existing failure checks before and after polling. The explicit cleanup ordering preserves the previous ready-result priority while keeping cancellation and destruction inside one owning driver. No guest turn, virtual time, backend selection, replay filter, or comparison rule changes. Fatal errors and panic payloads remain attached to their concrete owner through cleanup and publication.

## Linux Semantics

Signal removals and acknowledgements retain their actual identities and order. Cancellation does not acknowledge a removal or replace the original syscall errno. A ready successful notification stays successful; a pending notification can terminate on run failure while preserving its irreversible effects. The new child-start assertion encodes an existing notification restriction; it does not change fork or clone behavior in ordinary Tool callbacks.

## Validation

At exact head `33d71aa0b02ca0a3183314e6d8db8696ad117191`, tree `1e9fdd2e169655062f88adcac71ce039b24f20fe`:

- all 745 `reverie-kvm` library tests passed with KVM required, zero failed or ignored, both with normal parallelism (7.80 seconds) and one test thread (27.17 seconds) in the preserved source-bound receipt;
- the nine focused signal-cleanup completion tests passed;
- default-feature library check and strict library Clippy with `-D warnings` passed;
- non-test `native-test-support` library check and strict Clippy passed;
- formatting passed.

The initial full run, before rebasing, exposed one stale readiness fixture already repaired independently by https://github.com/rrnewton/reverie/pull/600 and one transient clock `EINTR`; the clock case passed immediately alone. After rebasing without editing that separate repair, both complete 744-test runs passed. The original default-build E0425 and later dead-code Clippy failure remain recorded as failures; neither was reclassified.

This is Reverie library/source evidence. Exact locked Hermit consumer checks and 13 strict KVM self-repeat pairs remain separate work. No ptrace/KVM comparison, full backend parity, or full-profile receipt is claimed here.

## Relationship to gVisor

The repair preserves ownership while leaving guest execution and completing backend cleanup. It introduces no mapping, translation-invalidation, or Linux-personality mechanism and makes no equivalence claim about those parts of gVisor.

## Human Review Required

Trigger 2: Reverie backend core callback-completion and signal-cleanup ownership. Independent exact-head Codex-family and Claude-family adversarial review records are required before landing. Human review follows landing under the standing owner directive.

Task: kvm-lane-to-full-determinism-and-parity
