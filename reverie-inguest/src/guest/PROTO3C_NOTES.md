# 3c prototype notes (throwaway; branch signals/proto-3c on Hermit main f34129f6)

Host: devbig014, kernel 7.1.3-0_fbk0_rc18. Kernel sources cited: v6.12 in ../kernel-v6.12/ (plus fpu-signal-master.c, upstream master).

## M4: switching the alternate stack (m4/altstack.c)

SIGUSR2 stands for the runtime's SIGSYS (SA_ONSTACK, runtime altstack); SIGUSR1 is a pending guest handler unblocked by the trap's return.
- mode 1 (uc_stack = SS_DISABLE in the trap frame only): no effect. The guest frame saves the runtime altstack (flags 0, 64 KiB), and an SA_ONSTACK guest handler runs ON the runtime stack. Cause: rt_sigreturn's restore_altstack -> do_sigaltstack refuses changes while sp is on the altstack (signal.c 4272-4273), and the error is ignored.
- mode 2 (trusted sigaltstack(SS_DISABLE) from the trap handler): EPERM, same reason.
- mode 3 (trusted sigaltstack(SS_DISABLE) issued from a different stack, plus SS_DISABLE in the frame): works. The guest frame saves SS_DISABLE, sp NULL, size 0; an SA_ONSTACK guest handler runs on the current stack; the altstack stays disabled afterwards.
=> Mechanism: the continuation (already on its own stack) disables the runtime altstack before the completion marker when a delivery is predicted, and re-enables it at a later non-delivering completion.

## M3: a failing rt_sigreturn (m3/badreturn.c, m3/probe.c)

badreturn.c: the frame's mask (block SIGUSR2, unblock pending SIGTERM) plus a corruption.
- An unmapped fpstate, or a corrupt xstate header: the kernel installs the frame's mask, delivers the pending SIGTERM at that return, AND forces SIGSEGV. The SIGSEGV frame is built first (synchronous signals dequeue first), so SIGTERM's handler runs on top of it. The SIGSEGV handler's saved mask is the frame's mask.
- A bad cs (REG_CSGSFS = 0) is not a restore failure: the return succeeds, SIGTERM is delivered, and SIGSEGV comes when that context is resumed (a fault at resume, like any instruction fault).
=> The prediction from the frame's mask holds for non-synchronous signals; a failed restore adds a forced SIGSEGV delivered first. Detcore needs a predicate "the restore fails".

probe.c: predicate by execution. Replay check_xstate_in_sigframe's checks (magic1, xstate_size vs user_size = CPUID(0xD,0).EBX, magic2), then run the same XRSTOR (RFBM = sw.xfeatures & XCR0) or FXRSTOR on the frame's buffer in place, under a temporary fault handler, saving and restoring the runtime's own FPU state around it.
- 13 corruption kinds x 200 seeds = 2600 cases: legacy-area bits, header bits, MXCSR bits, sw-byte bits, magic2, misaligned pointer, unmapped pointer, extended-state bits, xstate_bv bits, PROT_NONE pages, read-only pages (with and without magic1 broken), partially protected buffers.
- Result on kernel 7.1.3: 2600/2600 agree (738 kernel failures). fuzz3.out.
- With v6.12's save_xstate_epilog write (setfx path writes the sw bytes back to the buffer): 98 mismatches on read-only pages, because current kernels (upstream master, host 7.1) dropped that write. fuzz2.out. The predicate must follow the running kernel version.
- In the runtime this probe would run in runtime context (under P no guest code runs); the temporary fault handler swap is not guest-observable there. Shadow stacks (CET) are not covered: refuse a shadow-stack-enabled guest on in-guest by name.

## M2: the trapped rt_sigreturn, completed at the runtime restorer (m2/sigreturn_gate.c)

A standalone model of the runtime's mechanism: seccomp traps every rt_sigreturn except at the runtime restorer's `syscall` (IP checked over 64 bits). The SIGSYS handler (SA_ONSTACK, sa_mask = all but SIGSYS) counts the report and returns to the restorer's `syscall` with rax = 15, the trapped rsp, and a full uc_sigmask; the kernel then runs the guest's rt_sigreturn there, once.
- 5 tests: a pending signal the frame's mask unblocks (delivered at the return); a handler editing the frame's mask; a failing restore (unmapped fpstate; mask installed, pending SIGTERM delivered, SIGSEGV forced); nested handlers; siglongjmp out of a handler.
- Output byte-identical native (NOFILTER=1) vs model; 7 returns reported and completed (native.out, model.out).
- Gap: test 2's register edit is not observable through raise() (raise restores rbx); the mask edit is.
=> The kernel side of 16.7 works. On in-guest the report goes to Tool::handle_sigreturn (record-only, no turn: reverie/src/tool.rs:880-929, capability reports_sigreturns), not to a syscall event.

## M1: the two-phase hand-off (design from code, prototype pending the build recipe)

`handle_inbound_signal` (detcore/src/lib.rs:1066-1200) is shaped by ptrace's delivery stop: it assumes the kernel has already dequeued the signal.
- It sends `DiscardedSignalControl::Dequeue` (the answer can be Suppress or Rewrite of the siginfo for a POSIX timer entry).
- It ends `own_signal_turn_hold`, normalizes ITIMER_REAL for SIGALRM, handles SIGCHLD claims, and only then takes the turn (`pre_handler_hook(..., CheckIn::Event)`).
- `handle_signal_event` then runs `check_and_record_delivery`.

The split the in-guest final-read loop needs:
- **Phase A, the announce (a turn, before the final read):** `pre_handler_hook` and the continuation bookkeeping (`returned_from_sigreturn`, `scheduler_grants`). Nothing that asserts a dequeue.
- **Phase B, the commit (after the final read, no turn):**
  - the `Dequeue` control and its records (`note_process_signal_taken`, `thread_timer_copies`);
  - ITIMER_REAL normalization and re-arm;
  - `own_signal_turn_hold`;
  - `check_and_record_delivery`.

  None of these waits for a turn; they are RPCs to global state.
- **The token:** (signal, queue: private or shared, the delivery point's identity: generation and tid, and the grant count at phase A). It is created at phase A and consumed by phase B only if the final read confirms the same signal and queue at this delivery point; otherwise it is dropped at the return.
- **The consequence:** a signal displaced by a later arrival gets a second phase A at its next delivery point, one extra turn compared with ptrace, deterministic. It is named.
- **In the first cut** the `Dequeue` answers Suppress and Rewrite cannot arise (POSIX timer signals and SIGCHLD catching are refused), so phase B needs no answer before the kernel delivers; assert it.
- **API:** a Tool entry point pair (or one entry point with a phase argument) on the Reverie side, used by any backend whose delivery points come before the kernel's dequeue. That is a trigger-2 change.

## M5: the exec mask hand-off

Not testable today: in-guest-trap refuses execve (docs/ARCHITECTURE.md backend table), and `guest_resume_mask` belongs to the feature-gated private exec path. To be done after the prototype is rebased onto the loader's commit 3 (dh-fei4).

## P1: first in-guest-trap runs (prototype build, 2026-10-20 ~07:00 ET)

Reverie edits (patch: reverie-proto-p1.patch; module guest/proto3c.rs):
- the SIGSYS action's sa_mask = RUNTIME_MASK (all blockable but SIGSYS, SIGSEGV, SIGTRAP);
- prepare_signal_for keeps the trap frame's guest mask (Continuation::guest_mask) and enters the continuation under RUNTIME_MASK;
- complete() installs the saved guest mask;
- guest handlers admitted for every signal except SIGSYS, SIGSEGV-while-subscribed and SIGALRM (phase 1 keeps SIGALRM);
- a trapped guest rt_sigreturn is completed at the restorer's `syscall` (rax 15, the trapped rsp, RUNTIME_MASK) instead of refused (trap.rs);
- rt_sigprocmask and rt_sigpending work on the saved guest mask (`virtual_signal_call` hook). Without this, both read and changed the runtime mask: "mask after" showed everything blocked, and a handler's SIG_UNBLOCK was lost.
Build: .cargo/config.toml patch (RECIPE.md); hermit 15 s, runtime 9 s.

Results (in-guest-trap, --strict, default log level, relaxation --max-timeslice=disabled):
- m2/sigreturn_gate without its own filter, tests 1, 2, 4, 5 (test 3 needs a guest SIGSEGV handler, refused while instructions are subscribed, so the forced SIGSEGV kills): stdout identical to native in 3/3 runs, 5 returns completed per run; --verify --verify-strict: matched, bitwise_parity true, 104/104 INFO messages.
- Corpus guests from tests/c (built with host gcc, not the pinned-root compiler):

| guest | verify-strict | INFO compared | stdout vs native | stdout vs ptrace | returns completed |
|---|---|---|---|---|---|
| hello_signals | matched, bitwise | 38/38 | same | same | 1 |
| signal_disposition | matched, bitwise | 50/50 | same | same | 0 |
| sigaction_state | matched, bitwise | 48/48 | same | same | 0 |
| dbt_self_sigqueue | matched, bitwise | 40/40 | same | same | 2 |

On main these get EPERM for their handlers (the 11 red cells of 16.1).
Not yet in the prototype: the Tool report (handle_sigreturn), the prediction and deliver_signal check, the restart table, the alternate-stack switch (guest frames still save the runtime altstack in uc_stack), ppoll/pselect/rt_sigsuspend masks, /proc SigBlk.
New design point for revision 5: every call that reads or changes current->blocked (rt_sigprocmask, rt_sigpending, rt_sigsuspend, masked ppoll/pselect6/epoll_pwait, io_pgetevents, /proc SigBlk) must work on the guest mask the completion installs, with the rules in Detcore.

## P2: the alternate-stack switch in-guest (M4)

Added `proto3c::switch_alternate_stack`, called at the end of the continuation's `dispatch` (on the continuation's own stack, before the completion marker): if a signal is pending and unblocked under the guest mask, a trusted `sigaltstack(SS_DISABLE)` parks the runtime's alternate stack; otherwise a parked one is re-enabled. No frame edits: the marker's trap frame records the current state. (A real version uses the prediction, not rt_sigpending & ~mask.)
- m4/guest_ucstack.c (no guest alternate stack; a plain and an SA_ONSTACK handler):
  - **before the switch:** the SA_ONSTACK handler ran on the runtime's alternate stack, and the process died at its first syscall (rc 122, the run refused afterwards for missing statistics);
  - **after the switch:** both handlers run on the guest's main stack, every sigaltstack query matches native, rc 0.
- **Remaining difference, unreachable by any mechanism:** `uc_stack.ss_flags` is SS_DISABLE (2) where native shows 0. A thread that never configured an alternate stack saves its stored flags, 0; an explicit sigaltstack(SS_DISABLE) stores 2; and sigaltstack cannot restore "never configured" (flags 0 with size 0 is ENOMEM). Only a handler reading uc_stack sees it. Named difference for revision 5, or use no runtime alternate stack at all.
- Regression check after the switch: the gate guest (bitwise, 104/104, same as native) and the four cells (bitwise, same stdout as ptrace), unchanged.

## P3: prediction and the final-read loop at syscall completions

Added `proto3c::predict` (Linux's order over /proc/thread-self/status SigPnd/ShdPnd, the saved guest mask and rt_sigaction queries; handler masks, SA_NODEFER, SA_RESETHAND, ignored and default-ignored discards, stop at default-fatal) and `proto_announce_predicted` in host.rs, right after the Tool's syscall handler returns. It drives `Tool::handle_signal_event` for each new predicted caught signal (the InboundSignal turn), dequeues a suppressed one, and reads again until nothing new is predicted; it prints that final prediction.
- The predictions match where the guests' handlers ran: SIGUSR1 at raise's completion; in gate test 4, SIGUSR2 at the handler's own sigprocmask(SIG_UNBLOCK) completion (the nested delivery); SIGILL for hello_signals (it uses pthread_kill(SIGILL)).
- With the loop's turns: all four cells still verify bitwise (hello_signals 39/39 messages, was 38; dbt_self_sigqueue 42/42, was 40); stdout still equals ptrace's.
- Detcore's log now records the delivery on in-guest: `COMMIT ... InboundSignal(SigWrapper(4))` for hello_signals, as on ptrace (ptrace turn 4, in-guest turn 2: the backends' turn numbering and virtual time differ as they did before).
- Not yet: deliveries at a trapped rt_sigreturn are neither predicted nor announced (that path completes in trap.rs, outside the tool host; gate test 1's SIGUSR2 at its handler's return); the split into turn and commit (check_and_record_delivery still runs inside each turn); the in-guest deliver_signal check; the restart table; the M3 predicate in-guest; a cross-process test that sends during the loop's turns.

## P4: handler returns through the tool host (M2 complete on in-guest)

A trapped guest rt_sigreturn now goes through the fallback continuation to the tool host:
- The refusal is lifted for the fallback dispatch in installed_tool.rs:409 (a second refusal site, besides trap.rs).
- host.rs intercepts it before the subscription check (an unsubscribed call would be forwarded raw and trap again). The host then:
  - reads the frame's uc_sigmask at sp + 296;
  - reports `SigreturnReport` to `Tool::handle_sigreturn` (record-only, no turn);
  - runs the final-read loop with the frame's mask;
  - sets the continuation's `reissue`, so the completion resumes at the restorer's `syscall` with rax 15 and RUNTIME_MASK.
- Gate guest: SIGUSR2 (12) is now predicted at test 1's handler return, where the kernel delivers it; stdout identical to native; verify bitwise, 110/110 INFO (was 104: the delivery at the return is now recorded).
- hello_signals scheduler commits: 5 with the return completed in trap.rs (P3), 5 through the tool host (P4), 7 on ptrace. So the report adds no turn, unlike revision 4's "syscall event" plan.
- All four cells: bitwise, same stdout as ptrace.

## P5: the final-read loop under a cross-process race (Claude round-3 H1)

The loop prints "final read after N passes of turns" when N > 1. Guests in m1/: the parent wakes the child through a pipe, then sends itself SIGUSR2; the child reads and sends the parent SIGUSR1 at once. SIGUSR1's handler blocks SIGUSR2. Handlers write H<sig> to fd 2, interleaved with the runtime's predictions. check.py: after each prediction, the first handler to run must be the LAST predicted signal, because at one return the kernel builds the frames in delivery order and the top one runs first.
- **race2 (parent uses raise, a private-queue SIGUSR2):**
  - one return per run needed 2 passes: pass 0 turned SIGUSR2, SIGUSR1 arrived during that turn, pass 1 turned SIGUSR1; the final prediction "12 10" is what the kernel delivered (H10 then H12);
  - 0 mismatches in runs of 40/41/43/47 rounds;
  - delivery counts equal to native and ptrace (40 and 40);
  - verify bitwise, 1614 INFO messages.
- **race3 (parent uses kill(getpid()), a shared-queue SIGUSR2): the displacement case.**
  - Pass 0 turned SIGUSR2, SIGUSR1 arrived, pass 1 turned SIGUSR1, and the final prediction was "10" alone: SIGUSR1 goes first (lower number) and its mask holds SIGUSR2.
  - The kernel delivered SIGUSR1; at its handler's return SIGUSR2 was predicted and delivered, taking a second turn there (the named extra turn).
  - 0 mismatches; counts equal to native and ptrace; verify bitwise, 1534 INFO messages.
- **Single-phase defect, visible in race3:** handle_signal_event's check_and_record_delivery ran inside SIGUSR2's first turn, at a return where SIGUSR2 was not delivered. Harmless here only because the copy is off on in-guest. This is the evidence for M1's split: phase B, the record, belongs after the final read.
