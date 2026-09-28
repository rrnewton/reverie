[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=reviewer]

APPROVED-AT: claude c4df9b6a646beaa506187058965876e56ebccc7a

Claude Opus 5 adversarial round 2: **APPROVE** this exact head.

The four round-1 blockers are resolved rather than worked around: the deny-by-default futex gates now have positive coverage for all 13 admitted commands and all three admitted `FUTEX_CLOCK_REALTIME` combinations; the retained timeout and unsafe-comment lifetime claims are correct; the PR narrative no longer credits the pre-existing entry fence as a new guarantee; and the PI test no longer claims an ordering its inputs cannot distinguish. The review found no implementation correctness defect and no weakened assertion, tolerance, comparator, label, gate, or deleted check. It independently checked the static guest instruction sequence, x86-64 syscall ABI, exact `-ETIMEDOUT` oracle, fail-closed KVM availability, Linux futex command/clock/validation ordering, and the SHA-256 binding between the reviewed source and v11/v12 qualification artifacts.

Two nonblocking follow-ups were identified:

1. The v10 null-pointer mutation receipt came from a superseded version of the unit-test body and lacks the v11/v12 source manifest. Re-run it with exact-head provenance or amend the PR description so it is not represented as an exact-head measurement.
2. The new unit test's rescue loop is unbounded if a regression makes the waiter sleep at an unexpected translated host address. Bound that failing path with a deadline.

Other residuals are pre-existing and outside this narrow marshalling correction: two errno-precedence mismatches, PI lookup/TID and shared-key fidelity, read-vs-write probing for second futex words, and host-real-time behavior for host-owned threads. This approval does not establish KVM determinism, backend parity, record/replay, or Hermit pinning.
