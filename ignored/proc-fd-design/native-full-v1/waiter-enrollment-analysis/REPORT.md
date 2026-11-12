# Waiter enrollment in the retained native failure

**The fixture admits reverse `FUTEX_CMP_REQUEUE == 0` without a defect in the fatal-worker behavior being tested.** Its successful forward requeue proves enrollment at one instant; it does not preserve that enrollment until the reverse call or worker release. This is a source-derived admissible ordering, not a diagnosis of the historical scheduling delay.

Source: Reverie `696f0476aa46cf29e31b947a89379d80b4542ce3`. `reverie-kvm/src/vm.rs` is byte-identical at base `24cd5bb518b027eddb62a226805210d74d31c3d8`: blob `13aeec30c28f15cb96525f342261efde290d792f`, SHA256 `449c3287f9aa6de363292d5aaced5427a834a2dcda58779f0e3fa87b522d322d`. This equality supplies no base runtime result. Exact source/excerpt and retained-evidence identities are in `binding.json` and `source-excerpts.txt`.

## What the synchronization proves

- At `vm.rs:5335–5346`, the waiter sends `ready`, sleeps 150 ms for this case (`5417–5423`), then enters the real host `FUTEX_WAIT` with a one-second relative timeout. The ready message precedes enrollment; the 150 ms sleep does not consume the timeout, which is passed to the subsequent wait syscall.
- At `5173–5209`, the parent repeatedly requeues at most one waiter from the original word to a stack parking word, waking zero. Reaching the reverse assertion proves the forward call returned one. That confirms a waiter was queued and moved at that operation. The polling deadline is checked only after a zero return; the successful branch has no deadline check.
- At `5210–5227`, the reverse requeue is a separate syscall. There is no acknowledgement, retained lock or other synchronization preventing the already-running wait timeout from removing the waiter before that call. Requeueing does not begin a new `FUTEX_WAIT` or supply a new timeout. The diagnostic `recv_timeout` runs only on assertion failure; it does not itself insert a wait between successful forward and reverse calls.

A permitted ordering is: waiter enrolls; forward requeue moves it; the parent is delayed; the wait's existing timeout expires and removes the waiter; reverse requeue sees no waiter and returns zero. Even reverse return one would establish only instantaneous enrollment back at the original word, not immunity from timeout before the later wake.

The fatal worker is still gated during this phase. The Thread action places its host worker behind `start_receiver.recv()` (`vm.rs:2199–2255`); successful boundary finalization returns the outcome without starting it (`1853–1867`). The fixture calls `runtime::start_pending_children` only at `vm.rs:5377`, after `qualify_waiter_enrollment` returns. That function sends the gate's start command (`runtime.rs:1127–1140`), allowing guest execution and subsequent slot release/clear-TID wake (`vm.rs:2215–2231`). KVM setup already ran earlier; it is specifically this fatal-worker phase that had not been released.

## What was measured

The retained failure at `vm.rs:5221` reports reverse result **0**, expected **1**, and waiter result `Ok((-1, Some(110)))` (**ETIMEDOUT**). Raw stderr SHA256 is `1b538ab47f8b0f5897f8bb2bdb7fc61968161da6050acc337810a0866d406e1d`; observer result is `8f465b62f9ca251f8d9aee87de1927d15c7faebb1e3701b56262c6232da675dc`. The exact executable is `b899a2eec14c966aa64dcadd2064177cd412e3a2e57abe68efbf2e327075a783`, with `--test-threads=1`. The full run remains **426 passed / 1 failed**, payload exit 101.

There are no timestamps for wait entry, either requeue, timer removal or return. The evidence therefore does not locate when the timer fired, identify a host scheduling gap, or establish a base failure, candidate regression or environmental cause. It does establish failure before the intended fatal-worker store/wake/slot assertions. No connection to proc-fd identity, other main failures or the earlier census is inferred.

## Smallest useful next measurement

If further evidence is required, prepare one separately reviewed execution of this named fixture with fixed-size diagnostic records for wait entry/return and errno, both requeue entry/returns, and gate-start/clear-TID-wake events. Keep the one-second futex timeout, the 150 ms deliberate delay, reverse `== 1`, every downstream assertion and the existing external 30-CPU/60-wall/16-GiB envelope unchanged. A recorded ETIMEDOUT return before reverse entry proves the waiter had already timed out; the target product wake must follow its gate release. If the user-space return stamp arrives late, it does not precisely order kernel removal, and the result must remain unresolved rather than infer that missing order. No such instrumentation, build or execution occurred here.

Goalpost check: no assertion was weakened; no tolerance, timeout, exemption, skip or comparator changed; no failure was relabelled; no check was deleted. Accepting zero or extending the wait is not proposed. Any eventual synchronization correction must still prove enrollment before releasing the fatal worker and retain the substantive failed-store/wake/slot-release checks.
