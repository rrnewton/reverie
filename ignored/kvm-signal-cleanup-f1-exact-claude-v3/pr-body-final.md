[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Plain Language Summary and Project Impact

Restore ordinary KVM library builds and preserve signal-cleanup errors through cancellation and callback destruction. The ownership refactor in https://github.com/rrnewton/reverie/pull/598 left production cleanup calling a test-only helper, so normal Hermit consumption failed to compile. This continuation also resolves the required F1 follow-up from the earlier review: an impossible child-start batch can no longer be abandoned or cancelled before its real parent failure exists.

## Summary

Ready consuming cleanup still takes precedence over cancellation. Pending cleanup terminates inside the owning completion driver, which retains the selected result and every callback/failure destructor panic payload.

Signal-dequeue notifications cannot create children: `DequeueNotification` rejects injection and the lifecycle context independently rejects fork/clone actions. The defensive violation path now recovers a poisoned child-start vector, finalizes the callback, attaches the exact signal ledger and raw syscall result, synchronously publishes the real parent failure, and only then sends `CancelAfterFailure` and joins the batch. Real errors/panics stay primary; a bare derived `RunAborted` cannot replace the invariant failure.

## Determinism and Linux semantics

Ordinary callbacks retain their existing `BeforeCallback` ordering. This change adds no guest turn, virtual-time rule, backend selection, comparison exemption, signal acknowledgement, or fabricated guest errno. Cancellation transfers irreversible removals without acknowledging them, and a registered-child control proves terminal publication precedes cancellation and join.

## Validation

Exact head `ce442fce6b1ee366e0c3a9cff7fa981ff8893ae4`, tree `a83ef7f9e0645b28d3dbe83ed02a32e461674cc7`:

- 10/10 focused completion tests passed with KVM required;
- all 746 KVM-required library tests passed at 64 test threads and again serially, with zero failed or ignored;
- formatting, default and non-test `native-test-support` checks, both strict Clippy configurations, and all-target/all-feature strict Clippy passed from a package-clean build;
- two native exact-source reviews approved; a fresh external exact-head review is recorded separately.

This 316-logical-CPU host makes Cargo's unbounded default use 316 test threads. That run is not relabelled green: a clean head run failed an unchanged immediate nonblocking EOF probe. A predeclared five-run matrix reproduced a separate unchanged SIGPIPE race on base in 2/5 runs; head passed 1/5, failed that same SIGPIPE control once, and failed the unchanged EOF probe three times. The failed receipt and every matrix stream are preserved. The scoped source change does not alter either test body; bounded 64-way and serial full suites are the qualifying evidence.

No Hermit guest, ptrace/KVM comparison, or full backend-parity result is claimed here. The Hermit consumer pin and strict 13-pair qualification follow the landed Reverie SHA.

## Human Review Required

Trigger 2: Reverie backend callback-completion, signal-effect ownership, and terminal child-lifecycle ordering.

Task: kvm-lane-to-full-determinism-and-parity
