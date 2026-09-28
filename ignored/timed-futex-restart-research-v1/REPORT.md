# Timed futex restart policy

Source-only author research for the process-signal proposal. No product edit, build, native probe or guest execution. Linux reference: upstream v7.1.3, commit `199c9959d3a9b53f346c221757fc7ac507fbac50`; documentation: retained man-pages 6.19. The running host identifies as `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`; its vendor patches have not been authenticated against that upstream revision. Reverie context is landed `000c15a1161ea2d58749431b5ddaaa97f7aa37d5`; Hermit context is frozen v27, not a newly executed composition.

## Required correction to the draft

**A caught handler interrupts a timed FUTEX_WAIT or timed FUTEX_WAIT_BITSET with EINTR, even when that handler has SA_RESTART.** Do not preserve the relative deadline by restarting that caught wait on rt_sigreturn. The deadline-preserving restart block belongs to the **no-delivered-handler** path. Untimed WAIT and WAIT_BITSET use ERESTARTSYS and the ordinary SA_RESTART rule.

For Hermit's precise modeled waits, the smallest correction needs no new runtime restart-block API: retain the original wait and deadline during ignored/suppressed observation; for a caught timed wait return EINTR and deliver the already prepared frame; for a caught untimed wait use the existing ERESTARTSYS/prepared-frame behavior. The scheduler's actual ready/timeout winner still takes precedence as described below. This is a proposed mapping, not an implemented or qualified result.

## Exact source chain

1. `kernel/futex/syscalls.c:173–207` copies/validates the supplied timeout. WAIT converts a relative duration to a monotonic absolute expiry once using `ktime_add_safe(ktime_get(), duration)`. WAIT_BITSET preserves an absolute value, converting the monotonic time-namespace domain where appropriate. A NULL pointer leaves `tp == NULL`.
2. `do_futex` at `syscalls.c:111–131` maps WAIT to MATCH_ANY and sends both families to the same `futex_wait` function.
3. `kernel/futex/core.c:463–478` returns no timer for NULL; otherwise it creates an absolute timer using CLOCK_REALTIME when FLAGS_CLOCKRT is set, CLOCK_MONOTONIC otherwise. It retains the original expiry and kernel timer slack. This is not permission to widen Hermit tolerances or change its clock.
4. `kernel/futex/waitwake.c:666–707` checks the futex value/enrolls atomically, waits, and resolves an actual wake, timeout or pending signal. A signal interruption initially returns `-ERESTARTSYS`.
5. In `futex_wait` at `waitwake.c:710–739`, **no timer** returns that result unchanged. With a timer, the code cancels/destroys the timer and, for ERESTARTSYS, stores uaddr, expected value, the already absolute expiry, bitset and flags|HAS_TIMEOUT in the task's restart block. `set_restart_fn` (`include/linux/thread_info.h:83–90`) returns `-ERESTART_RESTARTBLOCK` (516), not 512.
6. Existing bound `arch/x86/kernel/signal.c:259–282` distinguishes those classes when building a caught handler frame: RESTARTBLOCK/NOHAND become EINTR unconditionally; RESTARTSYS becomes EINTR without SA_RESTART and rewinds the original syscall with it. Thus SA_RESTART cannot turn the timed 516 path into the untimed 512 path.
7. When no handler is delivered, `arch_do_signal_or_restart` (`arch/x86/kernel/signal.c:333–363`) rewinds 512 to the original syscall, but rewinds 516 to `restart_syscall`. `kernel/signal.c:3177–3185` calls the saved restart function; `futex_wait_restart` (`waitwake.c:742–753`) reuses the stored absolute expiry, flags, value and bitset. It does not reread the original relative timeout pointer or add a fresh duration. It resets the restart function before re-entering futex_wait; a further interruption can install another block.

The no-handler branch includes the ordinary stop/continue restart described by restart_syscall(2); ignored or tracer-suppressed delivery can also reach the source's no-handler branch. This is different from an actual caught handler returning through rt_sigreturn. Signal-default termination is not an EINTR/restart result. Stop/continue implementation is not claimed supported in this finite KVM proposal merely because the kernel source describes it.

## Per-family mapping

The table assumes a valid syscall has actually enrolled and the signal, rather than an already completed wake/timeout, determines its result. PRIVATE affects futex identity/sharing, not these restart rules.

| Operation | Timeout and clock | Kernel interruption class | Caught handler, SA_RESTART clear | Caught handler, SA_RESTART set | No delivered handler |
| --- | --- | --- | --- | --- | --- |
| WAIT, NULL timeout | Untimed | 512 | EINTR | Handler then original syscall re-entry | Continue/re-enter original wait |
| WAIT, non-NULL timeout | Relative duration converted once to absolute monotonic expiry | 516 | EINTR | EINTR | Restart with retained absolute expiry, not a new duration |
| WAIT_BITSET, NULL timeout | Untimed, nonzero bitset | 512 | EINTR | Handler then original syscall re-entry | Continue/re-enter original wait |
| WAIT_BITSET, non-NULL, no CLOCK_REALTIME | Absolute monotonic expiry, nonzero bitset | 516 | EINTR | EINTR | Retain same absolute monotonic expiry |
| WAIT_BITSET, non-NULL, CLOCK_REALTIME | Absolute realtime expiry, nonzero bitset | 516 | EINTR | EINTR | Retain same absolute realtime expiry and clock selection |
| WAIT with CLOCK_REALTIME | This reference kernel rejects the operation | No enrolled wait | ENOSYS after earlier applicable argument validation | Same | No restart contract to invent |

`WAIT_BITSET | CLOCK_REALTIME` with NULL timeout is accepted but has no timer; it therefore stays in the untimed row. A zero bitset is EINVAL. A zero/nonfuture **non-NULL** timeout is a timed request, not the same as NULL: usual value/setup and timeout ordering still applies.

The flags qualification matters: `syscalls.c:118–123` allows CLOCK_REALTIME only for WAIT_BITSET, WAIT_REQUEUE_PI and LOCK_PI2. The man-pages 6.19 futex options paragraph lists WAIT as supported, but the same page's ENOSYS paragraph excludes WAIT and agrees with this kernel. This is a concrete documentation discrepancy, not evidence that the fetched source accepts the flag. The finite proposal should not silently broaden flags or claim a vendor measurement. PI, requeue-PI, waitv/futex2 and their restart policies are outside this investigation.

## Completion and interruption ordering

`__futex_wait` at `waitwake.c:685–707` first returns 0 if another actor already unqueued/woke the waiter; then checks expired timer; then retries an internal spurious wake with no pending signal; only then reports the signal interruption. Preserve a committed 0 or ETIMEDOUT when observation/hook activity races with completion. Do not overwrite either with EINTR merely because a caught frame exists. Deliver the real selected frame with that result.

WAIT/WAIT_BITSET have **no partial byte count**: success is 0. FUTEX_WAKE's positive count is the number of woken waiters and is a different operation. Do not import partial-I/O restart rules or manufacture a positive count for WAIT. Initial expected-value mismatch is EAGAIN/EWOULDBLOCK; invalid pointers/timespecs/alignment/bitset retain their real validation order. In this reference, timeout copying/validation occurs before `do_futex`, while the futex value check precedes the actual wait. This report does not authorize moving existing error checks just to reach the interruption branch.

The standard-signal selection/Tool observation must already have committed before returning caught EINTR; publication alone is insufficient. Ignored or suppressed observation must not synthesize EINTR. It must preserve the same owned wait, absolute deadline, expected value, futex identity, bitset and continuation identity while real hook operations run. A completed wake/timeout remains attached to that original wait. If an implementation actually tears down and restarts kernel-like enrollment, it must also repeat the correct atomic futex value check and cannot lose a wake; retaining the scheduler-owned wait avoids claiming that a generic callback retry implements this protocol.

## Smallest Hermit/Reverie continuation

Hermit v27 `syscalls/threads.rs:1254–1375` explicitly models precise futex and does not execute it in the kernel. It computes the deadline in `futex_timeout_deadline` (`:783–844`), sends WaitRequest, retains the wait until an answer and then sends WaitFinished. Its current response does not express the proposed interruption/observation protocol; extending that typed continuation is still work. `tool_global.rs:3405–3439` is the separate FutexAction RPC path, not the ordinary resource-request loop.

For that precise path:

1. Save whether the original timeout pointer was NULL, the validated absolute deadline/clock interpretation when timed, and the original full operation/wait identity at enrollment. Never decide timed versus untimed by whether the deadline is in the future.
2. On a real ignored/suppressed observation, retain/resume this same wait. Do not return 512 merely to have the runtime call the whole relative-timeout handler again. This requires no hidden rt_sigreturn cookie and no accepted raw 516.
3. On a caught signal that wins before completion: timed returns EINTR regardless SA_RESTART; untimed returns the existing ERESTARTSYS request. Preserve the already prepared signal until the actual return boundary. A handler's same-shaped futex call is a new operation; it cannot redeem or cancel the interrupted wait by argument equality.
4. If wake/timeout already won, return 0/ETIMEDOUT and still deliver the prepared caught frame. Each path closes the original wait exactly once with actual scheduler accounting.

At Reverie000c `runtime.rs:3684–3723`, the classifier accepts 512 and explicitly rejects 513/514/516. Its prepared-frame return path (`:3588–3657`) rewinds only when restart was requested and the caught action has SA_RESTART. Explicit EINTR does not request that rewind, so the timed case can use the current frame mechanism. **Keep the 516 rejection unchanged for this finite correction.** Merely adding 516 to the existing 512 arm would both restart timed caught waits incorrectly and recompute relative deadlines.

If a later backend path needs actual raw restart-block support, the minimal new continuation is typed, operation-owned state containing full task/image/operation identity, futex key/address, expected value, bitset/flags, clock and retained absolute expiry. It is selected only by the no-delivered-handler restart-block path, cleared/consumed on completion, invalidated on exec/exit, and never redeemed by a matching handler syscall. Dispatch must distinguish 512 from 516 and use the retained state rather than the original timeout pointer. That is additional design and testing, not necessary for the precise modeled path above and not implemented here.

## Finite controls still owed

Use one unchanged native/reference oracle per family and the actual modeled path. Required cases are timed WAIT and timed WAIT_BITSET under both SA_RESTART settings (one handler, EINTR, no callback replay); untimed variants with/without SA_RESTART (handler-before-reentry only when enabled); ignored/suppressed observations preserving the original deadline; wake and timeout winners during hook activity preserving 0/ETIMEDOUT; zero timeout versus NULL; invalid/mismatched futex and zero bitset; monotonic/realtime WAIT_BITSET and rejected WAIT|CLOCK_REALTIME. Preserve complete raw arguments, original comparisons and timing bounds. A native confirmation on the vendor kernel remains useful but was not run under this source-only task.

Do not reuse an existing timing tolerance as proof of deadline retention: a focused scheduler control should bind the original exact deadline and actual continuation identity. No new runtime coverage, test declarations, independent approval or whole timer repair is claimed. Assertions, comparisons, error admissibility, time/CPU bounds and existing failed evidence are unchanged.

## Inputs and retrieval limits

`INPUTS.json` binds full fetched bytes, URLs, upstream revision/Git blob identities, reused primary inputs and exact product context. `READBACK.json` authenticates the final report and all retained artifacts. Seven kernel files were fetched at the exact upstream revision and their decoded size/Git blob hashes verified; four man pages were fetched and their full HTML retained. Six initial HTTP404 responses are retained separately: the first URLs omitted `man-pages/`, and the two constant-page follow-ups used the wrong section directory. Correct constant URLs were then taken from the actual futex page links. These were source-retrieval errors, not guest/test outcomes. No network policy bypass, repository fetch, product change or executable measurement occurred.
