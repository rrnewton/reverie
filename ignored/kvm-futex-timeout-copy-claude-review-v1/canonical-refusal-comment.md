[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=reviewer]

CHANGES-REQUESTED-AT: claude 8865eb263cb44530364cb90eda6cdb40826aba7d

Faithful relay of the completed independent Claude Opus read-only adversarial review of https://github.com/rrnewton/reverie/pull/608. The review finished normally after 776.3 seconds; all 130 bound inputs and the tracked source remained unchanged. The relaying process did not author the review.

Blocking finding:

1. `reverie-kvm/src/executor.rs:1056-1081` adds two deny-by-default gates—the three-command `FUTEX_CLOCK_REALTIME` allow-set and the thirteen-command futex allow-set—but the changed tests exercise only their rejecting sides. Ten allowed commands and all three allowed clock combinations have no positive non-`ENOSYS` coverage. Add a table-driven test over every allowed command and every allowed clock combination, using `libc::FUTEX_*` constants where available and an aligned inaccessible word so each case deterministically reaches a non-`ENOSYS` result without performing a live futex operation.

Required corrections from the same review:

2. Update the `unsafe` block's comment at `reverie-kvm/src/executor.rs:1134-1136`: argument four is now host-owned stack storage rather than a translated guest pointer. State why that storage stays live through the syscall; explicitly retaining `timeout` through the call would make the lifetime parallel to the word operands.
3. Correct the pull-request narrative. The entry gate already closed with retained operands at the base revision; the measured change is two retained operands to one, not newly enabling gate closure. The genuine benefits are faithful Linux import/validation order and eliminating a guest-memory TOCTOU between validation and kernel consumption.
4. Reword `entry_host_wait_tests.rs:311-327`. Its `FUTEX_WAIT_REQUEUE_PI` inputs return `EFAULT` under both adapter and Linux lookup orders, so the test does not prove the ordering claimed by its comment. Keep PI ordering explicitly out of scope.

Goalpost-moving assessment: no assertion was weakened; no tolerance was widened; no exemption, skip, or relaxed comparator was added; no failure was relabelled as a pass; and no check was deleted instead of satisfied. The `EFAULT` to `EINVAL` expectation matches Linux's non-PI alignment precedence, and the owner-count reduction is paired with a stronger exact retained-operand assertion.

Verification inspected: complete exact-head diff and source path, base versions, raw focused 5/5 result, final full-library 788/788 result with zero ignored, strict Clippy result, retained superseded pipe failure and isolated retry, the complete Hermit scheduler, product vision/roadmap, and the DetTrace paper. The reviewer found no live implementation defect in the copied-timeout logic and judged the reviewed Linux ordering correct. Limits: the packet did not contain the cited formatting/diff receipts or a real KVM guest result; existing PI ordering, private/shared futex identity, guest/host PI TIDs, wakeup scheduling, and full futex parity remain outside this patch.

Verdict: CHANGES REQUESTED at exact `8865eb263cb44530364cb90eda6cdb40826aba7d`.
