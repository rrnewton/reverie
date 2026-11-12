[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]

Add a narrowly scoped, currently unused `Guest::queue_process_alarm_signal` API for canonical SIGALRM/SI_KERNEL publication at a resumable KVM boundary. It validates the current task and sole receiver before mutation, retains first siginfo on coalescing, preserves the actual pending domain through Tool replacement, and distinguishes refusal from failure after publication. Other backends explicitly refuse the operation; there is no Hermit caller.

Qualification v5 passed 24 focused library tests and two initialized-VM tests, with zero failures or ignored tests, plus focused rustfmt and Clippy (`-D warnings`). Actual ELF inventories and source/ELF/loader identities were checked; `REVERIE_REQUIRE_KVM=1` and the existing 30-second VM self-exec bound were retained. The alarm VM control includes the real returning-fork refusal probe.

The predecessor VM fixture failed because its follow-up `getpid()` could retain the marker in unused syscall arguments. The corrected fixture saves the expected PID before the marked syscall, retaining every assertion; the original failure remains recorded.

This supplies pending publication only. It does not integrate scheduler delivery, dequeue-driven periodic rearm, parked-wait continuation, RPC-origin capability or getitimer interval state. No setitimer or determinism/parity result is claimed.
