2026-09-20 KVM lane exact-head review evidence for https://github.com/rrnewton/reverie/pull/603

Actual Claude review completed at 2026-09-20T10:44:08.203375Z with exit code 0 after 1004.237 seconds. It reviewed exact head 0598b5ffbeb737866372f89224d915efaeb29943, tree dab8c59a89f490c6143c197afc70ab6e6a3c8ff7, against base f7bd85e11dd258112148ed2cba6531501a1a00d9. The 197-file input snapshot and prompt were unchanged.

Evidence packet: /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918/ignored/kvm-child-exit-publication-claude-review-v1
INPUTS.json SHA-256: c3551b5a7cc6bc1ae0d5072f8141a587d36ed4e7e61e6fde09f4d8a1bf9fa1c4
prompt.txt SHA-256: 9b197e9a5c5c9663295bdb8a1bd893f08e3b9b23d3989fedcb5c2a506af1bac2
BASE-TO-HEAD.patch SHA-256: b973675db1aa7125c3fcb62a06026494d7cc9d69955727baeca8f9fb0aad8f53
exit.json SHA-256: c89f307352e81cac53fc897aa712ebeacb79c694d1d375c0cef6187516cba293

Final verdict: CHANGES REQUESTED. Blocking findings:
1. A successfully committed child publication has no backend at-most-once record, so an identical retry after the first standard SIGCHLD is dequeued can enqueue a duplicate.
2. Guest::queue_child_exit_signal remains a second public surface over the same pending state and signalfd carriers but bypasses the new run terminal latch.
3. The real-KVM callback test makes every generation numerically equal its tgid and only records the callback; it neither discriminates generation from PID nor calls publish_child_exit in the callback.

The review found no weakened assertions, widened tolerances, exemptions, skips, relaxed comparators, relabelled failures, or deleted checks.

Canonical refusal comment: https://github.com/rrnewton/reverie/pull/603#issuecomment-5749312998
Canonical refusal attestation: https://github.com/rrnewton/reverie/pull/603#issuecomment-5749313743
GitHub readback confirmed the PR remains open, non-draft, mergeable, and at exact head 0598b5ffbeb737866372f89224d915efaeb29943.

Independent consumer-path audit also established that the current callback fires only after child Tool exit hooks and recursive descendant joins. A Hermit scheduler barrier waiting for that late callback can deadlock parent-child-grandchild workloads. Therefore this head will not land or be pinned. The Reverie continuation must combine an early own-child completion witness with the three review corrections and new ordering/publication tests; Hermit pinning and synthetic timer removal remain blocked until that revised exact head qualifies and receives fresh reviews.
