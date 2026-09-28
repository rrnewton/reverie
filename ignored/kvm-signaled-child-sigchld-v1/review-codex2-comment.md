[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=reviewer]

APPROVED-AT: codex 865023d1a9d2b82320a38e17e5a83bca1c4af3ee

Faithful relay of the completed independent read-only exact-head deterministic-scheduling and Linux-semantics review. The relaying process authored the change coordination but did not author this review.

~~~~text
Review target: Reverie 78203cd45751cba5f86f1e7ad5c545aceb29c017..865023d1a9d2b82320a38e17e5a83bca1c4af3ee
Exact tree: 86314b59b1923a8e52ef7007ece798d369fee2f4

Findings:
- No blocking findings.

Determinism: the diff adds only pure validation and signalfd encoding over an already-frozen SignalEvent; it adds no host-timed input, scheduler request, queue-order branch, wakeup, virtual-time, RCB, or record/replay change. The run-scoped publisher validates exact child generation/status against the frozen family/lifecycle ledger, commits under the process signal transaction, and standard-signal coalescing still preserves the first complete event. Delivery invokes Tool::handle_structured_signal_event and revalidates its returned event in prepare_filtered_signal_delivery; signalfd consumes the same pending event. Ptrace/default backends are unchanged.

Linux semantics: CLD_EXITED is 0..255; CLD_KILLED is raw 1..64 only when the Linux default action terminates, including core-default signals when no core bit is present; CLD_DUMPED is restricted to the x86/Linux core-default set QUIT/ILL/TRAP/ABRT/BUS/FPE/SEGV/XCPU/XFSZ/SYS. Stop, continue, ignore, noncore, zero, and greater-than-64 cases are rejected. signalfd now copies pid/uid/status/utime/stime for all three terminal CLD classes.

Goalpost-moving assessment: no lowered assertion, tolerance, comparator, allowlist, skip, gate, or pass-by-relabel. The base tests that rejected all KILLED/DUMPED events were replaced because those classes are the feature being added, with class-correct positive boundaries and stricter negative coherence cases; all prior CLD_EXITED helpers and assertions remain.

Verification: static exact-object review only; the reviewer did not run tests. Lowest-layer tests cover receiver plus Tool-return validation and exact 128-byte signalfd encoding for all three classes. Existing guest E2E exercises the same path for CLD_EXITED; killed/dumped have compositional rather than full guest-E2E coverage.

Verdict: APPROVE exact head 865023d1a9d2b82320a38e17e5a83bca1c4af3ee, tree 86314b59b1923a8e52ef7007ece798d369fee2f4.
~~~~
