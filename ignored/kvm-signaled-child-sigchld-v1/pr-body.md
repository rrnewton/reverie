[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=impl]

## Plain Language Summary and Project Impact

Hermit's exact-main KVM child-exit qualification reaped a SIGTERM-terminated child correctly, then failed when the parent returned the resulting `SIGCHLD` through Reverie's Tool-controlled filter. The producer already emitted coherent `CLD_KILLED` and `CLD_DUMPED` events, but the KVM receiver accepted only `CLD_EXITED`.

Accept all three terminal child-notification classes with their class-specific Linux status domains, and preserve their child fields when read through `signalfd`. This is a narrow receiver correction following https://github.com/rrnewton/reverie/pull/599 and the generation-bound publication API landed in https://github.com/rrnewton/reverie/pull/603.

## Determinism and Linux Semantics

This changes only validation and encoding of an already-frozen `SignalEvent`. It adds no host-timed input, scheduler request, run-queue branch, wakeup, virtual-time rule, RCB rule, or record/replay event. Publication still commits under the process signal transaction, standard-signal coalescing still retains the first complete event, and Tool-return delivery revalidates the exact event before guest delivery.

The accepted status domains are:

- `CLD_EXITED`: unsigned exit byte 0 through 255;
- `CLD_KILLED`: signal 1 through 64 whose Linux default action terminates, including core-default signals when no core bit is present;
- `CLD_DUMPED`: Linux/x86 core-default signals only.

Stopped, continued, trapped, default-ignore, default-stop, zero, out-of-range, and non-core dumped combinations remain rejected before publication. `signalfd_siginfo` now copies pid, uid, status, user time, and system time for every accepted terminal class.

## Validation

Exact target: base `78203cd45751cba5f86f1e7ad5c545aceb29c017`, head `afd9ddc64d21967c6b1a9e203ffcd06964a4128a`, tree `613abcec9ac50416dc36873340ed8c3547f1210a`.

- Focused exact-head suite: 11 unit tests and one real static-ELF child-exit contract passed; 0 failed.
- Exact-head serial `reverie-kvm` library suite: 776 passed, 0 failed or ignored, in 26.61 seconds.
- Exact-head all-target Clippy with `-D warnings`: passed in 14.85 seconds.
- Cargo formatting, direct rustfmt of the separately included test file, and `git diff --check`: passed.
- Independent adversarial code review: approved this exact head/tree with no findings and no weakened assertion, tolerance, comparator, skip, exemption, or gate.
- Independent deterministic-scheduling/Linux review: approved this exact head/tree with no blocking findings.
- Independent read-only Claude review: approved this exact head/tree with no blocking findings after its earlier documentation and coverage findings were resolved.
- All guarded runs stayed above the 429,496,729,600-byte disk floor; the focused build's low-water mark was 441,628,839,936 bytes.

The earlier remote workflow at superseded head `865023d1` and an exact-base `78203cd4` control both stopped in the same three unchanged `reverie-dbt` diagnostic-FD tests in both hosted and self-hosted jobs, each at 100 passed, 3 failed, 13 ignored. That remains a real red repository signal and is not relabelled green or credited as exact-head KVM evidence.

Full guest E2E currently exercises the same callback, handler, and `signalfd` path for `CLD_EXITED`. Killed and core-dumped classes have direct receiver/Tool-return and exact 128-byte encoder coverage rather than separate full guest-E2E cases; that is retained as a nonblocking follow-up.

The pre-fix Hermit exact-main `bin/safehermit` run is retained as defect evidence. A post-fix Hermit exact-main run requires landing this Reverie change and updating the Hermit consumer pin, so no post-fix Hermit parity or record/replay result is claimed here.

## Human Review Required

Trigger 2 applies because this changes KVM signal-event validation at the Tool/backend boundary. Route it for post-facto human review; no pre-land owner hold is inferred.

Task: kvm-lane-to-full-determinism-and-parity
