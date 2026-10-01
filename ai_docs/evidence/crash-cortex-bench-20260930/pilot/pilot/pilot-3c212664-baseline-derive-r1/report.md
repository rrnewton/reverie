### SEVERITY
critical
130 hosts hard-locked (fleet-wide), production T1_TRN (AMD Turin) SKUs, whole-machine hang requiring kdump/reboot. Panic type: NMI hard LOCKUP.

### SUBSYSTEM
sched (kernel/sched/fair.c, kernel/sched/core.c) — amplified by printk/netconsole (drivers/net/netconsole.c, net/core/netpoll.c)

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
high

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
The fatal mechanism (the hard lockup) is a **recursive rq->lock self-deadlock**: CPU 38 held its own runqueue lock inside `__schedule()`, and deep in `pick_next_task_fair()` an experimental EEVDF avg_vruntime `WARN_ON_ONCE` fired at `kernel/sched/fair.c:691`. Because the WARN executes with `rq->__lock` held, its `printk` was emitted **synchronously** through the wake-capable netconsole path (netpoll → NIC TX cleanup → `dev_kfree_skb_irq_reason` → `raise_softirq_irqoff` → `wakeup_softirqd` → `wake_up_process` → `try_to_wake_up` → `ttwu_queue` → `rq_lock`), which re-acquired the very lock CPU 38 already held. The lock was never released; 52 CPUs then piled onto CPU 38's rq in nohz idle balance with IRQs disabled, stalling TLB-shootdown IPIs (csd_lock timeouts) until the NMI watchdog declared a hard lockup on CPU 3 (a victim). The **originating condition** that triggered the WARN is a still-incomplete `key*weight` s64 overflow in Meta's experimental avg_vruntime rework (partially mitigated by commits already in-tree). The actionable fix for this fleet-wide lockup is to make that rq-lock-held WARN defer its console output (`printk_deferred_enter/exit`), matching how the rest of kernel/sched/ already handles printk under rq->lock.

### DETAILED_ANALYSIS

**Crash kernel:** 6.13.2-0_fbk16_rc1_0_g242a742712f1, AMD Turin (T1_TRN), 252 CPUs, Wiwynn YosemiteV4. Panic: "Kernel panic - not syncing: Hard LOCKUP".

**Backtrace (crashing CPU 3) — the FATAL MECHANISM, not the bug.**
CPU 3 is `swapper/3` (idle). It took a `call_function_single` IPI while idle, ran softirqs on the way out of the IRQ (`irq_exit_rcu` → `handle_softirqs`, SCHED_SOFTIRQ vec 7), entered `_nohz_idle_balance` → `sched_balance_domains` → `sched_balance_rq` (fair.c:12016) → `rq_lock_irqsave(busiest)` → `raw_spin_rq_lock_nested` → `queued_spin_lock_slowpath`, where it spun until the NMI watchdog (`watchdog_hardlockup_check`) fired via the AMD PMU NMI and panicked. The qspinlock word (`locked:1 pending:1 tail:324`) shows a long MCS queue — heavy contention, not corruption. **CPU 3 is a victim.**

**Cross-CPU analysis (drgn "All CPUs") — locating the real culprit.**
- The contended lock is `0xffff88cefbf45a80`. Computing the per-CPU layout (qnode stride 0x80000; runqueues offset 0x45a80, verified against CPU 38's qnode `0xffff88cefbf46b80`) shows this is **CPU 38's `rq->__lock`**.
- **52 CPUs** are spinning on that one lock in `_nohz_idle_balance → sched_balance_domains → sched_balance_rq` — all selected CPU 38's rq as the "busiest" and are trying to pull tasks from it. They hold rq_lock_irqsave, i.e. **IRQs disabled** (CPU 22's NMI backtrace in dmesg confirms EFLAGS=0x46, IF=0, RBX=0xffff88cefbf45a80).
- **~60 CPUs** are in `csd_lock_wait_toolong` (kernel/smp.c) — stuck in `smp_call_function_many_cond` doing **TLB shootdowns** (`flush_tlb_multi` from `madvise`/`munmap`), waiting for the IRQ-disabled spinners to ACK. dmesg is full of: `csd: Detected non-responsive CSD lock (#NN) ... waiting 5000000020 ns for CPU#22 flush_tlb_func`. The non-responsive targets named are exactly the rq-lock spinners (CPU 22, 23, 26, 33, 35, 45, ...).
- **CPU 38** (`(umount)` PID 1059308, in `execve → sched_exec → stop_one_cpu → _cond_resched → __schedule`) is the **holder**. It is ALSO spinning in `queued_spin_lock_slowpath` on `0xffff88cefbf45a80` — its OWN rq lock — because:

```
__schedule()                      # holds CPU38 rq->__lock (core.c:6716)
 pick_next_task() > put_prev_task_fair() > put_prev_entity()
  __enqueue_entity() > sum_w_vruntime_add() > __sum_w_vruntime_add()
   WARN_ON_ONCE(...)              # fair.c:691 FIRES (w_vruntime=-6865589408623720101, weight=0x1ca3fd)
    exc_invalid_op > report_bug > __warn > printk
     vprintk_emit > console_unlock > console_emit_next_record   # SYNCHRONOUS (legacy_direct)
      netconsole write_ext_msg > netpoll_send_udp > netpoll_poll_dev
       bnxt_poll_p5 > __bnxt_tx_int > dev_consume_skb_any > dev_kfree_skb_irq_reason
        raise_softirq_irqoff(NET_TX_SOFTIRQ) > wakeup_softirqd > wake_up_process(ksoftirqd/38)
         try_to_wake_up() > ttwu_queue(cpu=38) > rq_lock(CPU38 rq)   # re-acquire held lock
          queued_spin_lock_slowpath()                                # SPINS FOREVER
```

Because CPU 38 never returns from that nested `rq_lock`, it never releases `rq->__lock`. Every other CPU that picks CPU 38 as "busiest" queues behind it forever (IRQs off), which prevents them from ACKing TLB-flush IPIs (the csd_lock timeouts), and the NMI watchdog eventually kills the box. Confirmed by source: `dev_kfree_skb_irq_reason()` (net/core/dev.c) unconditionally `raise_softirq_irqoff(NET_TX_SOFTIRQ)`, which wakes ksoftirqd when `!in_interrupt()`; and `try_to_wake_up → ttwu_queue → rq_lock` takes the rq lock directly for a **local** wakeup (target CPU == current CPU), so it self-deadlocks.

**Why the WARN fired (originating condition).** `__sum_w_vruntime_add()` computes `w_vruntime = entity_key(cfs_rq,se) * weight` and WARNs when the product's bits 62/63 differ (i.e. |product| ≥ 2^62, the safe-summation headroom is exhausted). Observed weight=0x1ca3fd (1,876,477, a cgroup-scaled weight) and key ≈ -3.66e12, so |key*weight| ≈ 6.87e18 (< 2^63 but > 2^62). This is Meta's experimental EEVDF `avg_vruntime` rework (`sum_w_vruntime`/`zero_vruntime`/`sum_shift`/`PARANOID_AVG`). The overflow-avoidance commits are already in the crash kernel (see UPSTREAM_PATCH_STATUS) yet the WARN still fired — the overflow is a still-open issue in that in-flux code. Note the non-paranoid path WARNs but then still adds the near-overflow value to `cfs_rq->sum_w_vruntime`, so avg_vruntime accounting is also being corrupted; the `PARANOID_AVG` path (sum_shift + rebuild) handles it, the default path does not.

**Theory table.**
| # | Theory | Evidence for | Evidence against | Status |
|---|--------|--------------|------------------|--------|
| T1 | FATAL MECHANISM: WARN under rq->lock → synchronous netconsole → try_to_wake_up → recursive rq->lock self-deadlock on CPU 38 | Full CPU 38 stack shows __schedule (holds rq lock) → …WARN… → netpoll → wake_up_process → ttwu_queue → rq_lock spinning on the same lock; 52 CPUs queued on CPU 38's rq; TLB-flush csd timeouts to the spinners | none | CONFIRMED |
| T2 | ORIGINATING CONDITION: avg_vruntime key*weight s64 near-overflow at fair.c:691 | drgn args: w_vruntime=-6865589408623720101, weight=0x1ca3fd; matches upstream overflow report; commit c06e05dae2c6 example is the same scenario | Mitigations already in-tree, so it is a residual/edge case, not a fresh regression | CONFIRMED as trigger |
| T3 | Pure nohz-idle-balance thundering herd (no stuck holder) | 52 CPUs on one rq lock | qspinlock is FIFO-fair and would drain in µs unless the holder is stuck; holder (CPU 38) IS stuck (T1) | ELIMINATED |
| T4 | Hardware fault (MCE/GHES) | ghes_notify_nmi appears in the NMI chain | It is only an NMI handler on the panic path; no MCE/CPER/ECC in dmesg; 130 hosts across many DCs | ELIMINATED |

**Timeline.** t≈60370.75s: first `csd: Detected non-responsive CSD lock` for CPU#22 (TLB flush) → repeated NMI-probes/re-sends for CPUs 22/23/26/33/35/45 for ~4s → t≈60374.33s: `watchdog: Watchdog detected hard LOCKUP on cpu 3` → `Kernel panic - not syncing: Hard LOCKUP` → kdump.

**Corroborating evidence (absence of the WARN in the logs).** The avg_vruntime WARN text (`cut here`, `WARNING ... fair.c`, `sum_w_vruntime`) appears in NONE of the host's logs — not dmesg.txt, kdump_dmesg.txt, or the netcons host_timeline — only in the vmcore stack (drgn). This is exactly what the theory predicts: the WARN's `printk` deadlocked in the middle of its synchronous netconsole emission, so the message never finished printing. dmesg therefore shows only the downstream cascade (csd_lock TLB-flush timeouts) and the eventual hard lockup, never the originating WARN. The reproducer reproduces this precisely: the baseline VM freezes at `------------[ cut here ]------------` with no WARN body and no further output.

### AFFECTED_FUNCTIONS
__sum_w_vruntime_add, sum_w_vruntime_add, __enqueue_entity, put_prev_entity, put_prev_task_fair, pick_next_task_fair, __schedule (kernel/sched/{fair,core}.c); try_to_wake_up, ttwu_queue, raw_spin_rq_lock_nested; sched_balance_rq, sched_balance_domains, _nohz_idle_balance; dev_kfree_skb_irq_reason, netpoll_poll_dev, netpoll_send_udp (console/netpoll amplifier)

### UPSTREAM_PATCH_STATUS
no_upstream_fix (for the fatal mechanism). Related fixes for the *originating overflow condition* are ALREADY in the crash kernel and did not prevent it:
- `c06e05dae2c6` "sched/fair: Avoid overflow in enqueue_entity()" (K Prateek Nayak; cherry-pick of upstream 556146ce5e94) — ancestor of HEAD (in crash kernel). Classification: root-cause mitigation for the overflow, but INCOMPLETE (WARN still fired).
- `3f374515315d` "sched/fair: Increase weight bits for avg_vruntime" — ancestor of HEAD. Same class; also insufficient.
- `fd7b718ecda1` "sched/fair: Fix zero_vruntime tracking", `b743dfdd6ee9` "…Rename …avg_vruntime to …sum_w_vruntime" — the experimental rework itself.
No in-tree commit addresses the printk-under-rq->lock / netconsole self-deadlock. Selected fix: defer the rq-lock-held WARN's console output (below), because (a) it prevents the fleet-wide lockup for this AND any other WARN that fires under rq->lock via a wake-capable console, and (b) a correct math fix for the residual avg_vruntime overflow is the ongoing upstream EEVDF work and cannot be validated offline.

### PATCH
Fatal-mechanism fix (this is the fix for the lockup category; the residual avg_vruntime overflow is tracked separately upstream and is not a fresh regression). Saved to `/tmp/kinv-logs/3c212664570a91b16403d990fa79281b/kernel_fix.patch`.

```diff
diff --git a/kernel/sched/fair.c b/kernel/sched/fair.c
index 5dc612effbec..14a5f8196b11 100644
--- a/kernel/sched/fair.c
+++ b/kernel/sched/fair.c
@@ -688,7 +688,17 @@ __sum_w_vruntime_add(struct cfs_rq *cfs_rq, struct sched_entity *se)
 	s64 w_vruntime, key = entity_key(cfs_rq, se);
 
 	w_vruntime = key * weight;
+	/*
+	 * This runs under rq->__lock with IRQs disabled (pick/enqueue paths).
+	 * A plain WARN here emits printk synchronously; with a wake-capable
+	 * console (netconsole -> netpoll TX cleanup -> wake_up_process()) that
+	 * re-enters try_to_wake_up() and self-deadlocks on the rq->__lock we
+	 * already hold. Defer console output like the other scheduler
+	 * diagnostics (see printk_deferred* users in kernel/sched/).
+	 */
+	printk_deferred_enter();
 	WARN_ON_ONCE((w_vruntime >> 63) != (w_vruntime >> 62));
+	printk_deferred_exit();
 
 	cfs_rq->sum_w_vruntime += w_vruntime;
 	cfs_rq->sum_weight += weight;
```

Rationale: `printk_deferred_enter/exit` (documented "scheduler/timekeeping" facility; requires IRQs disabled, which holds here) makes `vprintk_emit` take the deferred/offload path (`defer_console_output()`/irq_work) instead of the synchronous `console_unlock` → netconsole → netpoll → `wake_up_process` chain. The WARN is still logged; only the dangerous synchronous console re-entry under rq->lock is removed. kernel/sched/{psi,core,deadline}.c and even fair.c:5870 already use printk_deferred for exactly this reason; the experimental avg_vruntime WARN was the outlier.

### KREVIEW_RESULTS
/kreview tool is not available in this offline benchmark environment. Manual review notes:
- Correctness: printk_deferred_enter/exit requires IRQs disabled; `__sum_w_vruntime_add` is called only from `__enqueue_entity`/`__dequeue_entity`/`update_zero_vruntime` paths that run under `rq->__lock` with IRQs off — precondition satisfied. Enter/exit are strictly balanced (no early return between them). No functional change to the accounting; only console-emission timing changes.
- Blast radius: minimal, one function; no ABI/struct change.
- Does not "silence" the diagnostic (WARN still fires and is logged via deferred path) — not a symptom-suppression fix.

### BUILD_TEST_RESULTS
Built from the crash-kernel worktree with gcc 11.5 (x86_64 defconfig + kvm_guest.config; CONFIG_WERROR disabled to work around a pre-existing clang-vs-gcc `-Werror` issue in drivers/pci/pci.h unrelated to this change). Result: `arch/x86/boot/bzImage is ready` (13.5 MB), exit 0. Both the fair.c fix and the reproducer scaffold compile cleanly. See AB_VERIFICATION.

### REPRODUCER
Network is disabled in this environment, so the real netconsole path cannot be used. Instead the reproducer recreates the exact fatal mechanism with a self-contained, in-kernel scaffold (kernel/sched/core.c, guarded by boot params; NOT part of kernel_fix.patch) + a freestanding init:
- A **wake-capable console** whose `->write` calls `wake_up_process()` — mimicking netconsole/netpoll's `dev_kfree_skb_irq → wakeup_softirqd → wake_up_process`.
- A **victim** kthread bound to the test CPU (the wake target) and a **runner** kthread bound to the same CPU that loops `schedule_timeout(1)` to drive `__schedule()` there.
- A one-shot hook in `__schedule()` (right after `rq_lock`, so rq->__lock is held with IRQs off) that fires a `WARN_ONCE` — the stand-in for the `__sum_w_vruntime_add` WARN.
- Runtime toggle so ONE build serves both arms: `rqlock_warn_test=1` = plain WARN (unfixed), `rqlock_warn_test=2` = same WARN wrapped in `printk_deferred_enter/exit` (the exact technique kernel_fix.patch applies).

Files under `/tmp/kinv-logs/3c212664570a91b16403d990fa79281b/reproducer/` (init.c, initramfs.cpio) and the scaffold diff in `reproducer/scaffold.core.c.patch`. Boot: `qemu-system-x86_64 -enable-kvm -smp 4 -m 2048 -kernel bzImage -initrd initramfs.cpio -nographic -append "console=ttyS0 rdinit=/init rqlock_warn_test=<1|2> rqlock_warn_cpu=1 ..."`.

### AB_VERIFICATION
Single build, runtime-toggled (rqlock_warn_test=1 vs 2), booted in QEMU (KVM, -smp 4), 40 s window each. Logs: `reproducer/qemu_baseline_mode1.log` (excerpted in `reproducer/ab_results.txt`), `reproducer/qemu_fixed_mode2.log`.
- **Baseline (mode 1, WARN not deferred):** bug **reproduced**. Console prints `ccrepro: firing WARN under rq->lock` then `------------[ cut here ]------------` and **wedges mid-WARN** — no WARN body, no "SURVIVED", /init never runs, no heartbeat; QEMU killed by timeout (rc=124). This mirrors the production host hanging mid-backtrace inside synchronous netconsole emission.
- **With fix (mode 2, WARN wrapped in printk_deferred_enter/exit):** bug **gone**. The full WARN backtrace is logged safely (`WARNING: CPU: 1 ... __schedule`), then `ccrepro: SURVIVED WARN under rq->lock (no deadlock)`, followed by `CCREPRO-INIT-UP` and 30+ continuous 1 Hz heartbeats — scheduler and userspace keep running for the whole window.
- **Verdict: FIX VERIFIED** (the deadlock is present without the printk_deferred wrap and absent with it — the exact technique kernel_fix.patch applies to `__sum_w_vruntime_add`).
- **Iterations:** 1 A/B cycle.

Caveat: the A/B exercises the fatal mechanism via a synthetic rq-lock-held WARN (stand-in for the `__sum_w_vruntime_add` WARN, which needs the rare avg_vruntime overflow to fire naturally) with a synthetic wake-capable console (stand-in for netconsole, which needs networking — disabled here). The mechanism (WARN under rq->__lock → console write → wake_up_process → try_to_wake_up → recursive rq->__lock) and the fix are identical to production.

### RECOMMENDATIONS
1. Land the fatal-mechanism fix: defer the rq-lock-held avg_vruntime WARN (kernel_fix.patch). Also audit the sibling diagnostics that run under rq->__lock in the same experimental code — `WARN_ON_ONCE(!load)` in `place_entity()` and `BUG_ON(cfs_rq->sum_shift >= 10)` in `sum_w_vruntime_add_paranoid()` — and give them the same printk_deferred treatment (a BUG_ON under rq->lock will hang the same way).
2. Systemic hardening (separate change): make the netpoll/netconsole console-emit path incapable of waking tasks — netpoll runs from arbitrary atomic contexts, so `dev_kfree_skb_irq → wakeup_softirqd → wake_up_process` from within console output under rq->lock is a general hazard for ANY WARN/printk in the scheduler.
3. Continue/​backport the upstream EEVDF avg_vruntime overflow work so the WARN stops firing (prevents the trigger). c06e05dae2c6 / 3f374515315d are present but insufficient; track the follow-on series.
4. Consider `sched_feat(PARANOID_AVG)` on affected fleets as an interim measure: it routes overflow through the sum_shift rebuild path instead of WARN-and-corrupt.
5. Fleet: the 130 affected hosts hard-locked and were kdump'd; no code change needed on the hardware side (not a hardware fault).
