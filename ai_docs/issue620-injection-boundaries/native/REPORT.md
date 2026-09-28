# Native ptrace injection-boundary observations

For https://github.com/rrnewton/reverie/issues/620, the one released experiment completed all 15 cells. Compilation, probe, HBR, and attached caller each returned literal 0. This is diagnostic completion, not product approval or a claim that the proposed KVM model passed. I authored the probe and controllers; this is not independent review.

The measured suppressed-resume paths **do not preserve queued SIGUSR1**. Every A completed with raw -514 (ERESTARTNOHAND), current `/proc` mask T=0, PTRACE-reported mask O=512, and SIGUSR1 still pending. In all 15 cells there was then a real self-SI_TKILL SIGUSR1 delivery stop before B. Signal 0 resume suppressed that selected signal; at actual B entry the mask was O and both pending domains were empty. No application handler ran. All five B mask queries returned O=512; all five second temporary waits returned literal 0.

The first 12 cells use PTRACE_SYSCALL completion; six additionally advance to the signal-delivery stop before installing B. The last three use the private PTRACE_SINGLESTEP route: actual completion was SIGTRAP, si_code=1 at the exact after-syscall address, with SIGUSR1 still queued. A later resume produced the genuine SIGUSR1 stop before B. That observed ordering is retained; no universal single-step stop order is assumed.

| Case | A route / B start | B | Literal B result | Raw lines |
|---:|---|---|---:|---|
| 1 | exact-syscall / syscall-exit-queued | getpid | 329920 | 1–38 |
| 2 | exact-syscall / syscall-exit-queued | mask-query | 0 | 39–76 |
| 3 | exact-syscall / syscall-exit-queued | second-wait | 0 | 77–114 |
| 4 | exact-syscall / signal-held | getpid | 329945 | 115–152 |
| 5 | exact-syscall / signal-held | mask-query | 0 | 153–190 |
| 6 | exact-syscall / signal-held | second-wait | 0 | 191–228 |
| 7 | private-syscall / syscall-exit-queued | getpid | 329956 | 229–279 |
| 8 | private-syscall / syscall-exit-queued | mask-query | 0 | 280–330 |
| 9 | private-syscall / syscall-exit-queued | second-wait | 0 | 331–381 |
| 10 | private-syscall / signal-held | getpid | 329970 | 382–432 |
| 11 | private-syscall / signal-held | mask-query | 0 | 433–483 |
| 12 | private-syscall / signal-held | second-wait | 0 | 484–534 |
| 13 | private-step / actual-step-completion | getpid | 329984 | 535–580 |
| 14 | private-step / actual-step-completion | mask-query | 0 | 581–626 |
| 15 | private-step / actual-step-completion | second-wait | 0 | 627–672 |

Each getpid result equals its exact owned child PID. Mask-query results are syscall return 0, with output word 512. For second-wait cells 3/6/9/12/15, the predeclared diagnostic alternatives were 0 or -514; the measured value was 0 in every cell. No output expectation was changed after execution. Case 2 raw lines 54–75, case 5 lines 168–189, and case 14 lines 603–625 give compact complete mask/queue/stop comparisons. CASE-SUMMARY.json indexes every cell; the full unmodified 673-line stdout is retained.

Limits stayed 30 CPU seconds / 60 wall seconds / 1 GiB / two cores / 4 MiB aggregate per output stream. Actual HBR CPU was 0.779396 seconds; outer caller wall was 15.513931 seconds. Compilation took 0.208974 seconds and the one experiment 0.061791 seconds. Compiler and probe stderr are empty; outer stderr contains only maintained operational-tool provenance. Actual producer was 38b287609e256c1d28db6462f603ba3ec81cf178. No retry, cap adjustment, source change, S access, Cargo, Hermit, or KVM run occurred.

The 15 children were intentionally killed and reaped only after the final B observation; their literal terminal signal is SIGKILL, not claimed normal exit 0. Fresh exact PID/start checks confirm all original generations gone, and the exact HBR unit and cgroup are absent. Compiler, probe source, Q product source/status/index, and Cargo.lock remained identical before/after. Source/execution authority is relinquished.

Limits of inference: this ran on Linux 7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf, x86_64. It measures genuine Linux syscall/ptrace stops and queue ownership, not the full Reverie Tool callback or final guest return. Installing B rewrites the syscall request and instruction pointer; no syscall result was forged. The tracer deliberately suppresses SIGUSR1 rather than delivering an application handler. It does not cover replacement/reblocking, competing signals, callback cancellation, tail injection, or final A settlement. Those semantics are not approved by these observations. In particular, a KVM implementation that merely restores O and leaves the same SIGUSR1 queued before B would diverge from the measured suppression paths; selecting or dropping a logical signal requires a separate source-grounded design decision.

All prior issue620 baseline failures and routing results remain unchanged. This packet does not claim full temporary-mask or pselect support. The unchanged parser retains hypothesis_qualified=false.
