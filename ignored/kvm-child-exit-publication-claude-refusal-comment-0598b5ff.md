[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=reviewer]

CHANGES-REQUESTED-AT: claude 0598b5ffbeb737866372f89224d915efaeb29943

Faithful relay of the completed independent Claude read-only review at this exact head. The review finished with exit code 0 after 1004.2 seconds; its 197-file input snapshot, prompt, and patch were unchanged. The relaying process authored the change coordination but did not author this review.

Blocking findings:

1. `reverie-kvm/src/process_signal_publication.rs`: a successful child publication has no backend-side at-most-once record. Once the parent dequeues the first standard SIGCHLD, repeating the same committed completion can enqueue another SIGCHLD. The failure-after-commit latch does not cover this successful case. Add a run-scoped committed-completion ledger keyed by the generation-bearing child identity, or otherwise make the caller obligation explicit and prove it with a committed-then-retry test.

2. `Guest::queue_child_exit_signal` remains a second public child-exit publication surface over the same pending state and signalfd carriers, but it does not consult the new run terminal latch. After a post-commit readiness failure, that older API can still mutate the same process. Reconcile the two surfaces through the same latch or explicitly refuse the legacy surface while tool-controlled run-scoped publication is installed, and document which API the Hermit consumer must use.

3. The real-KVM callback test uses identities where every generation equals its numeric tgid and only records the callback. It therefore cannot catch `generation = tgid`, does not exercise PID reuse, and never calls `publish_child_exit` inside the real callback. Strengthen it with a generation that differs from the tgid or a real PID-reuse case, and exercise publication from the callback so the binding-lifetime claim is actually tested.

Nonblocking findings retained by the review: narrow the parent-binding comment; account for the unblocked default-disposition SIGCHLD pending/readiness divergence; remove or document the mutually recursive recipient defaults; cover partial signalfd readiness with multiple carriers; and close the stated verification gaps.

Goalpost-moving assessment: no weakened assertions, widened tolerances, exemptions, skipped cases, relaxed comparators, relabelled failures, or deleted checks were found. The review judged the mechanism and unit-layer validation disciplined, but the three findings above block approval.

Verification inspected: exact base `f7bd85e11dd258112148ed2cba6531501a1a00d9`, exact head and tree, complete 52,124-byte patch (SHA-256 `b973675db1aa7125c3fcb62a06026494d7cc9d69955727baeca8f9fb0aad8f53`), all eight changed files, retained raw output for all eight green qualification phases, the complete DetTrace paper and appendix, complete Hermit scheduler, and vision documents. Limits remain: no workspace-wide build, the terminal-fork cases compiled but were filtered out, cached check/clippy phases, and no Hermit consumer or end-to-end backend-parity/record-replay proof.

Verdict: changes required at exact head `0598b5ffbeb737866372f89224d915efaeb29943`.
