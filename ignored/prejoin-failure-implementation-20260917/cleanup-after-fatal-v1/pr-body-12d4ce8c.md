[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]

## Plain Language Summary and Project Impact

Fatal callback cleanup could return the original error wrapped with an additional `RunAborted` from an unstarted child, even though that child's cleanup succeeded. The complete library run exposed this in two production-wrapper controls. After joining and removing the exact child whose pending gate was cancelled, discard only that bare derived marker on the explicit fatal path. Preserve the parent's original error, ordinary cancellation, all aggregated cleanup errors and real hook/join errors.

This is one correction commit on top of the landed repair, https://github.com/rrnewton/reverie/pull/577. It also corrects the partial-start fixture to require the actual fatal `CancelAfterFailure` command after its lost start gate. The neighboring ordinary `Cancel` requirement and all consumption, registry and started-worker assertions remain intact.

## Determinism

The existing synchronous failure publication, gate ordering and physical joins retain their production paths. The decision uses the explicit fatal cancellation path and exact bare error variant; it does not depend on timing, a sampled primary error or diagnostic text. The new eight-case control covers fork/thread ownership, ordinary/fatal cancellation and bare/aggregated errors through the actual cancellation and discard functions with owned OS-thread joins.

## Linux Semantics

A fatal runtime failure remains separate from a guest errno or ordinary exit status. The two original production-wrapper assertions still require a direct `InvalidGuestAddress`, now with its exact guest-end address and `FRAME_SIZE` length. The new control requires genuine cleanup errors to retain their full aggregate and original non-Clone error identity. No child start, consuming hook, syscall return, status, clock or output behavior is changed.

## Validation

Final source: `c8f4ca9d2e95460e027678ff23f6dec2529d255d..12d4ce8c0bc426f1ae41416f5b4a699e2c300879`. Only `reverie-kvm/src/vm.rs` changes.

- The complete serial library passed **454 tests on its first attempt after the correction**: all previous 453 names plus the one new eight-case method, with no failures, ignored tests or filtering. It used 9.178808 CPU seconds and 19.010510442 observed wall seconds. This includes the three previously failing methods, all earlier 44 selected native methods and four focused VM methods; those were not repeated separately.
- All original **22 static integration methods** are accepted, retaining the ten exec diagnostic modes, all 17 leader methods and all four terminal-fork methods with their original writes, status, wait and cleanup assertions. The result comprises **21 first attempts and one authorized retry** after an observer accounting refusal. The original refusal remains refused. Accepted stages used 8.410475 CPU seconds and 25.211394556 seconds summed across individual observed stage wall times.
- Compilation and actual inventories passed: 454 library names and the unchanged 288 static integration names. No structured Rust compiler messages were emitted. Workspace formatting and `cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings` passed.
- Runtime checks required actual `/dev/kvm` API 12 admission and `REVERIE_REQUIRE_KVM=1`, unchanged 30 CPU / 60 wall / 16 GiB / zero swap / 1 MiB limits, exact executable bindings, complete accounting and fresh inactive/empty service checks. Raw output is bounded and untruncated.

The historical 426-pass/1-failure futex enrollment timeout remains unexplained. The prior 450-pass/3-failure result and observer refusals remain retained; none is relabelled. The new eight-case test injects a controlled child result rather than claiming a real exit-hook fault injection. Existing first-cleanup-error retention is unchanged. This is Reverie component qualification; no Hermit strict INFO, repeat-determinism or canonical parity result is inferred. The separate successful-exec/pending-RPC issue and Hermit integration are outside this correction.

## Relationship to gVisor

This corrects the existing Reverie KVM child cleanup path. It imports no gVisor code and makes no new compatibility claim.

## Human Review Required

Retain the `post-facto-human-review` disclosure for trigger **2**, core failure and cleanup semantics. This follow-up does not introduce another GlobalTool API or alter the Hermit scheduler. Independent native source review accepted the exact correction; actual Claude review of `12d4ce8c` is pending at this preparation cutoff. Approval labels and landing must describe the actual final verdicts, not that pending status.
