[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=reviewer]

APPROVED-AT: codex afd9ddc64d21967c6b1a9e203ffcd06964a4128a

Faithful relay of the completed independent read-only exact-head deterministic-scheduling and Linux-semantics review. The relaying process authored the change coordination but did not author this review.

Review target: Reverie `78203cd45751cba5f86f1e7ad5c545aceb29c017..afd9ddc64d21967c6b1a9e203ffcd06964a4128a`, tree `613abcec9ac50416dc36873340ed8c3547f1210a`.

Findings: none.

Determinism is preserved. The production changes are pure class/status validation and signalfd field selection over an already selected event. They add no scheduler request, event source, host-timed membership decision, wakeup, queue ordering, RCB or virtual-time input, or record/replay change. Existing signal transactions, first-complete standard-signal coalescing, Tool callbacks, and Tool-return revalidation remain the ordering path.

Linux semantics are coherent: `CLD_EXITED` accepts only 0..255; `CLD_KILLED` accepts raw signals 1..64 only when the Linux default action terminates, including a core-default signal without a core bit; `CLD_DUMPED` accepts exactly the x86/Linux core-default set. Ignore, continue, stop, non-core dump, zero, and out-of-range cases are rejected. The fallback fails closed. Producer and receiver use the same core-default classification, and signalfd copies all terminal child fields into the 128-byte record.

No goalpost lowering was found. Exact-head evidence and hashes were inspected: 11 focused unit tests plus one static-ELF guest contract, 776/776 serial library tests, all-target Clippy with warnings denied, Cargo formatting, direct included-file rustfmt, and diff check all passed. The reviewer did not rerun commands and does not credit superseded-head remote CI or infer Hermit consumer success.

Verdict: APPROVE exact head `afd9ddc64d21967c6b1a9e203ffcd06964a4128a`, tree `613abcec9ac50416dc36873340ed8c3547f1210a`.
