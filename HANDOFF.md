# Reverie KVM signal phase-one handoff

## Identity

- Slot: `/home/newton/work/dev-hermit/worktrees/slots/kvm-signal-phase1`
- Branch: `kvm-signal-phase1`
- Base: exact https://github.com/rrnewton/reverie/pull/539 head `8c173b6b3f0fbd9e99872722f774371310b828b3` (original merge base `8c8c0a57649c9ffbf8a7a14291a64320f64b935f`).
- Local commit: `2b3123c9460ff1b4f73b4a19621e40bb29b300c4`
- Tree: `0e525b0ab3ce73ed479d5991c44e00b496eeeadb`
- Diff digest versus the composed base: `912c53d61ef4584e1004d1ec74a75350ebe5924a`
- Publication: local only; no push or merge. Publication remains on the coordinator's https://github.com/rrnewton/reverie/pull/529 merge hold.
- Review: two independent read-only reviews bound to this exact head/tree/diff. The cleanup/ownership review approved it; the signal-test review found no code blocker.
- Task: keep `reverie_kvm_has_no` open. This commit is the Reverie foundation, not the later Hermit timer/scheduler integration.

## Implemented boundary

This one commit provides:

- Additive `SignalEvent`/`SignalTarget`, `Guest::defer_signal_delivery`, and `Tool::handle_structured_signal_event`. Existing backends keep their legacy behavior. The compatibility bridge preserves unchanged/suppressed events and rejects incoherent legacy number replacement with `ENOSYS`.
- Process-shared dispositions/process-pending standard signals plus per-thread masks, altstack, and thread-pending standard signals. Standard instances coalesce while retaining the first siginfo.
- A process-shared per-signal generation invalidates pending entries that predate a discard-causing ignored disposition across every sibling queue. A blocked signal generated after SIG_IGN remains pending across handler reinstall, matching the native ptrace reference; stale entries are filtered from delivery, `rt_sigpending`, `rt_sigtimedwait`, and signalfd readiness/dequeue.
- `rt_sigaction`, `rt_sigprocmask`, `rt_sigpending`, zero-timeout `rt_sigtimedwait`, `sigaltstack`, virtual signalfd, and supported self-directed `kill`/`tkill`/`tgkill`.
- Deferred delivery at ordinary syscall return and successful backend-owned `rt_sigreturn`, using the transported userspace syscall frame rather than live VMCALL registers.
- x86-64 kernel `rt_sigframe`/`ucontext`/`sigcontext` and x87/SSE/YMM state codecs. Restore accepts NULL FP state, 512-byte legacy FXSAVE, and bounded standard XSAVE feature subsets.
- `SA_SIGINFO`, `SA_RESTORER`, `SA_ONSTACK`, `SA_RESTART`, `SA_NODEFER`, and `SA_RESETHAND`; Linux FIX_EFLAGS behavior; immediate pending selection after `rt_sigreturn`; and `ERESTARTSYS` handling after the structured Tool hook.
- The structured hook runs once per selected event in production. Replacement re-resolves the new signal's mask/action/disposition; suppression, ignore, and blocked replacement continue selection at the same boundary.
- Ordinary nonreturning Tool injection from a signal hook is refused before execute for execve/execveat, exit/exit_group, and self SIGKILL through kill/tkill/tgkill. A distinct `SignalReturn` context keeps the refusal after returning fork/clone actions. Real KVM injects Fork and then proves ExitGroup is refused.
- Aggregate 128-byte signalfd records across `read`, `readv`, `preadv`, and `preadv2(-1)`, including partial-vector capacity, trailing bytes, EINVAL/EAGAIN/ESPIPE, copyout-fault consumption, standard coalescing, aliases, and shared mask updates.
- Virtual signalfds are nonblocking-only. Creation without `SFD_NONBLOCK`, clearing `O_NONBLOCK` through any alias, SCM_RIGHTS donation, and epoll ADD/MOD are pre-mutation `ENOSYS`. A potentially blocking ppoll directly naming any signalfd alias first performs a zero-time readiness probe: ready/invalid results return normally; an empty wait returns `ENOSYS` without changing guest pollfds or sleeping.
- Every matching signalfd alias is refreshed after pending selection, Tool suppression/replacement, ordinary delivery, `rt_sigreturn`, `rt_sigtimedwait`, reads, and ignored-disposition purge.
- Process groups are tracked separately from TGIDs and inherited on fork/exec. `kill(-1)` excludes caller and PID 1. Single-process `kill(0)` and `kill(-getpgrp())` work; multi-process fanout is refused before mutation. Positive lookup sees a leaderless process through its live worker.
- Alternate-stack membership matches Linux's downward `(base, top]` rule and treats armed `SS_AUTODISARM` as off-stack. Descriptor setup accepts and preserves overflowing `ss_sp + ss_size`; membership uses subtraction and fresh-stack top uses wrapping addition before frame/access checks. Frame placement retains fresh/nested red-zone, lower-bound, guard-page, exact-fit, and writable-memory checks.
- `rt_sigreturn` validates altstack updates using the live restorer RSP, rejects `SS_ONSTACK` as input, ignores validation/EPERM errors while restoring mask/register/FP state, and retains an active altstack when a handler requests replacement.
- The new fatal-`rt_sigreturn` exit path clears/wakes CHILD_CLEARTID immediately before the Tool exit callback.

## Return-to-user and wait-path audit

Signal delivery occurs at exactly two production boundaries in both plain and Tool loops:

1. ordinary syscall completion after any returning process action, before guest execution;
2. successful `rt_sigreturn`, from the restored register context, before a user instruction.

Initial exec, thread start, post-exec, successful image replacement, and exit have no resumable syscall frame. Signal-producing injection/defer is rejected there. Post-exec mask changes that would expose preserved process- or thread-pending signals are refused before mutation, including recursive exec.

The same-boundary loop after Tool suppression or disposition-ignore intentionally supersedes the architecture note's earlier “at most one eligible event” wording. A native ptrace two-pending probe showed the second delivery stop before tracee user code. A blocked replacement is requeued while selection continues. The loop is bounded to 64 selections.

Wait-path enumeration:

- `read`/`readv`/`preadv`/`preadv2`: virtual signalfd never enters a host wait.
- `poll` and `select`: existing KVM implementations always use a zero-time probe, even for a guest blocking timeout.
- `ppoll`: masked waits already zero-probe/fail closed; unmasked waits directly naming signalfd aliases now zero-probe and return `ENOSYS` only when empty.
- `epoll_wait` and `epoll_pwait`: existing KVM implementations always call host epoll with timeout zero.
- `pselect6` and `epoll_pwait2`: undispatched and return `ENOSYS`.
- Signalfd registration in epoll is refused before host epoll mutation, preventing an indirect unbounded ppoll from hiding the virtual readiness source.

## ABI and native evidence

- Independent C/kernel-header sizes and offsets match the Rust codec: kernel sigset 8, stack 24, sigcontext 256, kernel ucontext 304, fixed rt frame 440, xstate 832. libc `ucontext_t` is 968 with sigmask offset 296.
- Native guard-contained FP-state matrix on Linux 7.1.3 used the full kernel-advertised 2444-byte extended image:
  - legacy FXSAVE: alignments 1/2/4/8 faulted; 16/32/64 succeeded;
  - standard XSAVE: alignments 1/2/4/8/16/32 faulted; 64 succeeded.
- The installed-kernel restore path validates standard metadata and reaches `xrstor64`; the legacy path reaches `fxrstor64`. The implementation therefore detects format from the 512-byte prefix before applying 16-byte legacy versus 64-byte extended alignment.
- Real KVM distinguishes a guard-adjacent 16-not-64 legacy image (success) from a valid 16-not-64 extended image (SIGSEGV), and verifies modified RAX/XMM restoration.
- Native `sigaltstack` accepts and reports an overflowing descriptor. Linux's `__on_sig_stack` uses `sp > base && sp - base <= size`, and `sigsp` uses unsigned addition; unit tests pin setup, query, and stack-top selection.
- Native producer codes: alarm/setitimer use process-directed `SI_KERNEL`; POSIX timers use process-directed `SI_TIMER`. Validation admits those exact future producer classes while rejecting queued and synchronous-fault metadata.
- Native signalfd copyout: a 128-byte record split across 64 accessible bytes and a fault returns EFAULT after consuming; total capacity 64 returns EINVAL and preserves pending state.
- Native ptrace: explicit/default ignore each creates one delivery stop, and suppressing/ignoring the first of two pending events selects the second before user execution. A pending blocked signal queued before SIG_IGN is flushed and cannot reappear after handler reinstall; one generated while SIG_IGN is active and blocked remains pending and is delivered after reinstall/unblock.

## Determinism and explicit limits

No Hermit scheduler or wall-time signal producer is added. Direct process-pending delivery with live siblings, new CLONE_THREAD while process-pending state exists, multi-process group fanout, cross-thread/process delivery, and per-reader multithread signalfd readiness are refused before mutation rather than depending on host timing.

Still unclaimed:

- alarm, ITIMER_REAL, and POSIX timer production; later Hermit virtual-time integration owns these;
- raw realtime signals 32-64 and queued siginfo;
- SIGCHLD delivery, SIGPIPE generation/delivery, synchronous fault-to-handler delivery, and stopped-state scheduling;
- lifecycle delivery without a transported syscall frame or eligible-pending delivery across image replacement;
- multithreaded signalfd setup/update, fork with an open virtual signalfd, signalfd epoll registration, and empty blocking signalfd/ppoll waits;
- cross-process/group fanout and cross-thread recipient selection;
- XSAVE features beyond the backend's fixed x87/SSE/YMM subset.

## Exact-head test and mutation evidence

- Independent exact-head `REVERIE_REQUIRE_KVM=1 cargo test -p reverie-kvm -- --test-threads=1`: 363 passed, 0 failed/ignored, 29.67 s. Per binary: lib 282, counter 3, erestartsys 2, static_elf 67, strace 3, vmcall 6.
- Restored local focused controls: KVM library 282/282 (0.53 s); ptrace ignored-signal reference 3/3 (0.01 s); required-KVM sibling generation 1/1 (0.14 s); unchanged PR539 shared-state regression 1/1 (0.15 s); signal bridge 3/3 independently.
- Exact-head formatting and clippy checks passed in independent review; local `cargo fmt --all -- --check`, `git diff --check`, `cargo check -p reverie-kvm --tests`, and `cargo check -p reverie-ptrace --tests` also passed.
- The required bounded serial non-KVM workspace command was started only after review, then intentionally interrupted with status 130 when the owner issued an emergency-drain stop. Every completed group shown before interruption had zero failures, but this is not a completed workspace gate and must not be reported as one.

Mutation/fail-before checks caught and were fully reverted before the exact head:

1. bypassed ordinary-injection and tail-injection preflights;
2. skipped completed-boundary restoration at each production caller and mishandled returning versus image-replacing actions;
3. removed action-local child cleanup, resent a one-shot child gate, broadly started an unrelated shared-map child, or started instead of cancelling a failed action;
4. staged an injected action result into the interrupted guest frame, or failed to stage a returning outer Exec result;
5. weakened the exact `rt_sigreturn` boundary classification or nonleader Tool-exit guard;
6. accepted virtual-signalfd writes/POLLOUT or blocking wait paths and exposed backing-descriptor behavior;
7. omitted the ignored-disposition generation advance: the unit exposed stale `rt_sigpending`, and the real-KVM guest exited 22;
8. coalesced a new current-generation event into a stale row: the queue unit returned `Ok(false)` instead of `Ok(true)`, and the real-KVM guest exited 27;
9. omitted generation filtering from the synchronous-priority path: stale SIGSEGV (11) won over current SIGUSR1 (10);
10. reset generations across exec: the retained shared-pending visibility assertion failed;
11. reset PGID during nonleader exec promotion: the exact caller test observed `(tgid=3, pgid=3)` instead of `(tgid=3, pgid=55)`;
12. earlier signal-frame, XSAVE, altstack, process-group, wait-path, and return-boundary mutations recorded during this branch all failed their named focused tests.

No assertions, tolerances, comparators, labels, required-KVM checks, or skip conditions were weakened. The signalfd unbounded-wait test deliberately retains a `<1 s` wall-time assertion against a 60 s/null guest timeout; host descheduling is a documented test limitation. Full nested Tool-injected refused-Exec behavior is covered by the production wrapper plus end-to-end Fork/Thread continuation tests, not a separate end-to-end Exec callback fixture.

## Thread-exit composition

This head already composes https://github.com/rrnewton/reverie/pull/539 at exact commit `8c173b6b3f0fbd9e99872722f774371310b828b3`. Thread-exit commit `ff73ae1e7a8c16398c7b934c0dd06204f204519c` remains a later composition step. Do not stack it mechanically.

The composed result must:

- keep one `clear_tid_before_tool_exit` helper;
- retain ff73's clear/wake immediately before all seven Tool-exit callbacks that existed on the base;
- retain this signal commit's clear/wake immediately before the eighth, new fatal-`rt_sigreturn` callback;
- therefore have eight `notify_tool_exit` sites and eight immediately preceding clears;
- retain both branches' KVM tests and this branch's signal filtering/exit-first behavior.

This signal commit intentionally does not duplicate ff73's seven existing-path changes.

## Complete changed-file list

- `reverie/src/signal.rs`
- `reverie/src/guest.rs`
- `reverie/src/tool.rs`
- `reverie/src/lib.rs`
- `reverie/tests/signal_bridge.rs`
- `reverie-kvm/src/signal.rs`
- `reverie-kvm/src/bootstrap.rs`
- `reverie-kvm/src/elf.rs`
- `reverie-kvm/src/executor.rs`
- `reverie-kvm/src/runtime.rs`
- `reverie-kvm/src/vm.rs`
- `reverie-kvm/src/memory.rs`
- `reverie-kvm/src/lib.rs`
- `reverie-kvm/tests/erestartsys.rs`
- `reverie-kvm/tests/static_elf.rs`
- `reverie-kvm/tests/fixtures/signal_abi_kernel.c`
- `reverie-kvm/tests/fixtures/signal_abi_libc.c`
- `reverie-kvm/tests/fixtures/rt_sigreturn_fpregs.c`
- `reverie-ptrace/tests/ignored_signal_hook.rs`

`HANDOFF.md` is intentionally untracked and is not part of the commit.

