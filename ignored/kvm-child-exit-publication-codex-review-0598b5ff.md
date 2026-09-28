[hermit2, degraded-unresolved, gpt-5.6-sol, devbig014, role=reviewer]

APPROVED-AT: codex 0598b5ffbeb737866372f89224d915efaeb29943

Faithful relay of the completed independent native read-only exact-head review. The relaying process authored the change coordination but did not author this review. The complete report follows. The approval is scoped to the Reverie prerequisite and explicitly does not claim that the Hermit consumer exists or that KVM determinism or parity is complete.

~~~~text
Review target: Reverie f7bd85e11dd258112148ed2cba6531501a1a00d9..0598b5ffbeb737866372f89224d915efaeb29943

Findings:
- None.

Determinism guarantee:
- APPROVE for this scoped prerequisite. ProcessSignalControl explicitly requires the caller's causal scheduler fence; the backend primitive itself adds no host-timed admission. Exact parent/child generations are captured while both executors are live (vm.rs:2815-2839), both bindings survive through the awaited callback (vm.rs:2863-2887), and publication validates exact parentage plus the lifecycle's committed terminal status before mutation (process_signal_publication.rs:341-369). Recipient snapshots are transaction-bound, mask-aware, generation-bound, and TID-sorted (process_signal_publication.rs:638-687). This does not claim the Hermit consumer exists or KVM parity is complete.

Linux/POSIX semantics:
- APPROVE. Full ExitStatus maps to CLD_EXITED, CLD_KILLED, or CLD_DUMPED and si_status; uid and signed clock-tick fields occupy the Linux siginfo layout (process_signal_publication.rs:253-292). Waitability is cross-checked against explicit SIG_IGN and SA_NOCLDWAIT before mutation; explicit SIG_IGN suppresses generation while Linux SA_NOCLDWAIT still queues SIGCHLD (391-407). Standard-signal coalescing retains the first full siginfo, independent signalfd carriers mirror pending readiness, and eligible process-directed recipients exclude masking tasks and are sorted. These match signal(7), sigaction(2), wait(2), and signalfd(2).

Goalpost-moving assessment:
- Assertions weakened: no. The sole changed real-KVM callback assertion is strengthened from numeric PIDs to exact PID and generation values; the replaced private child-publication test's original live-child, parent, generation, status, SIG_IGN, siginfo, and no-waitability assertions remain and are expanded.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: no. No tolerances, allowlists, skips, ignores, comparator settings, or gate files changed.
- Failure renamed or relabelled as a pass: no. Precommit rejection, committed receipt, and failed-after-commit remain distinct typed outcomes; the child path adds exact receipt-bound failure forwarding.
- Check deleted instead of satisfied: no. Changed tests strengthen identity checks and add killed/core, waitability/disposition, coalescing, recipient mask/order, independent carrier, postcommit failure, and binding-lifetime cases.

Verification and limits:
- Independently confirmed exact HEAD and binary diff SHA-256 b973675db1aa7125c3fcb62a06026494d7cc9d69955727baeca8f9fb0aad8f53; inspected all eight changed files, the complete diff, relevant executor/lifecycle/signal/scheduler paths, the complete DetTrace paper and appendix, the complete Hermit scheduler, and vision documents. git diff --check is clean. Per the qualification runner, exact-head evidence is green: 23 focused tests, native-support check, clippy with warnings denied, real-KVM callback test, Reverie core, and 753 serialized reverie-kvm library tests. The reviewer did not rerun tests because this assignment was strictly read-only.
- Residual coverage: no real KVM GlobalTool publishes from on_backend_child_wait_event yet; that is intentionally the Hermit consumer follow-up. Killed/core publication and postcommit carrier failure are unit-covered rather than real-KVM end-to-end. The BackendChildWaitEvent field type change is intentionally source-incompatible.

Verdict: APPROVE exact head 0598b5ffbeb737866372f89224d915efaeb29943.
~~~~
