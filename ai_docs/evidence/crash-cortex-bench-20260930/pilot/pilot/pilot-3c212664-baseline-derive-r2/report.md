### SEVERITY
critical

130 hosts panicked with a hard LOCKUP, across multiple SKUs (SENTINELDOME_TUR_T1/T2 +
retimer) and multiple regions (pnb1, nha5, nha3, vll6, snb2, cco3, ...). Whole-machine
wedge (every online CPU stalled) ending in a kernel panic; recurring fleet-wide on
6.13-fbk16. Meets the Critical bar (panic on >50 hosts, fleet-wide).

### SUBSYSTEM
sched (kernel/sched/fair.c) — with the fatal reentrancy running through printk/console
(kernel/printk) and netconsole/netpoll (drivers/net/netconsole.c, net/core/netpoll.c,
net/core/dev.c). The scheduler is where the fix belongs.

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
high (fatal mechanism + verified lockup fix); see the two-defect note below re: completeness.

Root cause of the LOCKUP is proven directly from the vmcore: the single "busiest" rq lock
everyone is stuck on is CPU 38's own runqueue, and CPU 38's full backtrace shows it
re-acquiring that same rq->lock recursively from a WARN printk that re-entered the scheduler
via netconsole/netpoll. The fix was built and A/B-tested in QEMU (see AB_VERIFICATION):
baseline deadlocks, patched boots through.

**Independent verification verdict: PARTIALLY_AGREES.** The independent verifier confirmed
the mechanism, the GS-base→rq arithmetic, the 52-CPU convoy and the CSD/TLB cascade
end-to-end, and agreed the deadlock is independent of the trigger (a plain printk under
rq->lock reproduces it). Its one correction — which I accept — is that there are TWO distinct
defects and the patch fixes only one:
  - **D2 (fatal mechanism / cause of the lockup):** synchronous console flush under rq->lock
    re-enters the scheduler (`try_to_wake_up`). This is what kills the 130 hosts. The patch
    fixes D2 at its origin and is A/B-verified.
  - **D1 (trigger + accounting bug):** the `avg_vruntime` overflow that trips the WARN. In the
    non-PARANOID_AVG path, `__sum_w_vruntime_add()` still executes
    `cfs_rq->sum_w_vruntime += w_vruntime` with the overflowed product (fair.c:693), so
    avg_vruntime accounting drifts regardless of the WARN. The patch does NOT prevent D1.
Why the patch is still the correct PRIMARY fix for THIS crash: the investigated signature is
a hard LOCKUP, whose direct cause is D2; D2 is an independent latent deadlock (any
under-rq->lock printk on a netconsole host triggers it, not only D1's WARN). D1 is a
`WARN_ON_ONCE`-survivable condition the fast path tolerates by design (that is why the
graceful `sum_w_vruntime_add_paranoid()` and the overflow-reducing commit c06e05dae2c6
exist), so it is a separate, lower-severity follow-up (Recommendation #3), not the cause of
the machine death. Confidence in the lockup fix: high; confidence that D1 is fully resolved:
this patch does not attempt it.

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
Fatal mechanism: a **recursive rq->lock self-deadlock**. CPU 38 held its own rq->lock
inside `__schedule()` (reached via `execve()->sched_exec()->stop_one_cpu()`), and while
holding it a `WARN_ON_ONCE()` at `kernel/sched/fair.c:691` (`__sum_w_vruntime_add()`,
inlined into `__enqueue_entity()`) `printk()`'d **synchronously** to the configured
polling console (netconsole). Console flushing under the lock ran
`netpoll_poll_dev()->bnxt_tx_int()->dev_kfree_skb_irq_reason()->raise_softirq_irqoff()->
wakeup_softirqd()->try_to_wake_up()->rq_lock()`, which tried to take **the same rq->lock
CPU 38 already held** — a permanent self-deadlock. Originating defect (root cause): the
scheduler emits this warning while holding rq->lock **without deferring console output**
(**D2**); on any host with a polling console this re-enters the scheduler and deadlocks, and
it is independent of the trigger (any under-rq->lock printk hits it). The WARN itself firing —
an avg_vruntime `key*weight` overflow (**D1**) — is a separate, lower-severity defect: it is
the trigger, and in the non-PARANOID_AVG path it also drifts accounting because
`sum_w_vruntime += w_vruntime` commits the overflowed product (fair.c:693); but `WARN_ON_ONCE`
is meant to warn and continue and the overflow-reducing commit c06e05dae2c6 is already present,
so D1 does not by itself kill the machine. **This patch fixes D2 (the cause of the lockup);
D1 is a tracked follow-up (Recommendation #3).** The deadlocked rq is then the
permanent "busiest" target for NOHZ idle balancing, so dozens of idle CPUs convoy on its
lock with IRQs disabled and cascade into a machine-wide CSD/TLB-shootdown IPI stall; CPU 3's
NMI watchdog fires "Hard LOCKUP".

### DETAILED_ANALYSIS

**Hardware / config.** Wiwynn YosemiteV4, AMD EPYC "Turin" (Galena), 1 socket / 252 logical
CPUs, single NUMA node, ~256 GB. Crash kernel `6.13.2-0_fbk16_rc1_0_g242a742712f1`.
netconsole is configured on the kernel command line (`netconsole=+6969@...,1514@...`) — this
is essential: the fatal reentrancy is through netconsole's netpoll Tx path. The perf-based
NMI hard-lockup watchdog is enabled (`nmi_watchdog`), which is what ultimately panics.

**Fatal backtrace (CPU 3, the panicking CPU).** Bottom-up:
`start_secondary -> cpu_startup_entry -> do_idle -> cpuidle_enter_state` (idle) ->
`asm_sysvec_call_function_single` (took an IPI) -> `irq_exit_rcu -> __do_softirq ->
_nohz_idle_balance -> sched_balance_domains -> sched_balance_rq` -> `rq_lock_irqsave(busiest)`
-> `raw_spin_rq_lock -> queued_spin_lock_slowpath` spinning on lock `0xffff88cefbf45a80`
(dmesg NMI backtrace confirms RBX=`ffff88cefbf45a80`). This is the *fatal mechanism's victim*,
not the cause.

**Who owns the lock — proven.** The WARNING on CPU 38 (dmesg t=60362) prints
`GS:ffff88cefbf00000` = CPU 38's per-CPU base. `struct rq` sits at offset `0x45a80` in the
per-CPU area (verified against CPU 32's local rq `0xffff88cefbc45a80`), so CPU 38's runqueue
is `ffff88cefbf00000 + 0x45a80 = 0xffff88cefbf45a80` — **exactly the contended lock.** So
CPU 38 owns the lock everyone else is stuck on.

**CPU 38's stack (drgn_output.txt lines 917-957) — the smoking gun**, bottom-up:
`__x64_sys_execve -> do_execveat_common -> bprm_execve -> sched_exec -> stop_one_cpu ->
__cond_resched -> __schedule` [**acquires CPU 38 rq->lock, IRQs disabled**] `->
put_prev_task_fair -> put_prev_entity -> update_curr -> __enqueue_entity ->
__sum_w_vruntime_add -> WARN_ON_ONCE (fair.c:691)` -> `report_bug -> __warn ->
printk/_printk -> vprintk_emit -> console_unlock -> console_flush_all ->
console_emit_next_record(netconsole_ext) -> write_ext_msg -> send_ext_msg_udp ->
send_msg_no_fragmentation -> send_udp -> netpoll_send_udp -> netpoll_send_skb ->
__netpoll_send_skb -> netpoll_poll_dev -> poll_napi -> poll_one_napi -> bnxt_poll_p5 ->
__bnxt_poll_work_done -> bnxt_tx_int -> __bnxt_tx_int -> dev_consume_skb_any ->
dev_kfree_skb_any_reason -> dev_kfree_skb_irq_reason -> raise_softirq_irqoff ->
wakeup_softirqd -> wake_up_process(ksoftirqd/38) -> try_to_wake_up -> ttwu_queue ->
rq_lock(0xffff88cefbf45a80) -> raw_spin_rq_lock -> queued_spin_lock_slowpath` [**SPIN
FOREVER on the lock CPU 38 already holds**].

Why the irq path is taken: `dev_kfree_skb_any_reason()` (net/core/dev.c:3266) chooses
`dev_kfree_skb_irq_reason()` when `in_hardirq() || irqs_disabled()`. IRQs are disabled here
(rq->lock is held), so the deferred path runs; `dev_kfree_skb_irq_reason()`
(net/core/dev.c:3261) calls `raise_softirq_irqoff(NET_TX_SOFTIRQ)`, and because we are in
process context (`!in_interrupt()`) that calls `wakeup_softirqd() -> wake_up_process() ->
try_to_wake_up()`, which locks the target task's rq. `ksoftirqd/38` is pinned to CPU 38, so
its rq is CPU 38's rq — the very lock held → recursive acquisition of a non-recursive
`raw_spinlock` → the CPU spins on itself forever.

**Amplification into a whole-machine lockup.** With CPU 38's rq->lock permanently held and
CPU 38's rq being "busiest", every idle CPU that runs newidle/NOHZ idle balancing
(`sched_balance_rq()` at fair.c:12016, `rq_lock_irqsave(busiest)`) queues on that one lock
with IRQs disabled. drgn shows **52 CPUs** spinning on `0xffff88cefbf45a80`
(`qspinlock: locked=1, pending=1`). Because those 52 CPUs sit in the qspinlock MCS queue
with IRQs disabled, they cannot service inbound IPIs; **60 more CPUs** are stuck in
`csd_lock_wait_toolong()` — almost all TLB shootdowns from `madvise(MADV_DONTNEED) ->
flush_tlb_multi()` targeting the spinning CPUs, plus one `membarrier` broadcast. dmesg
confirms: `csd: Detected non-responsive CSD lock ... waiting 5000000030 ns for CPU#32
ipi_mb` and `... for CPU#01 flush_tlb_func`. 112 of 114 running CPUs are wedged. ~12 s
after the WARN, CPU 3's perf-NMI watchdog declares "Hard LOCKUP" and panics (t=60374).

**Timeline.**
- t=60362.42  CPU 38 WARN at fair.c:691 during `execve` (Comm `(umount)`), under rq->lock.
- t≈60362.4   Console flush re-enters scheduler; CPU 38 self-deadlocks on its rq->lock.
- t=60367.4   First 5 s CSD timeouts fire (waiters for CPU 32 / CPU 1, both rq-lock spinners);
              NMI backtraces sprayed to the wedged CPUs.
- t=60374.33  CPU 3 NMI hard-lockup panic; kdump.

**Theory table.**
| # | Theory | Status | Evidence |
|---|--------|--------|----------|
| T1 | Recursive rq->lock self-deadlock: WARN printk under rq->lock -> netconsole/netpoll -> wakeup_softirqd -> try_to_wake_up -> same rq->lock | CONFIRMED (root cause / fatal mechanism) | CPU 38 full stack (drgn 917-957); GS->rq address math; dev.c:3261/3266; qspinlock locked=1,pending=1 with CPU 38 owning + spinning |
| T2 | NOHZ/newidle idle-balance convoy on the one stuck rq amplifies to machine-wide lockup + CSD/TLB cascade | CONFIRMED (amplifier) | 52 spinners on 0xffff88cefbf45a80; 60 csd_lock_wait (TLB/membarrier); dmesg CSD timeout lines |
| T3 | avg_vruntime `key*weight` overflow makes the WARN fire (the trigger) | CONFIRMED (trigger, survivable) | fair.c:691 WARN text; commit c06e05dae2c6 describes the identical 256-CPU overflow; fix present yet WARN still fires |
| T4 | Pure lock contention (no stuck owner) | ELIMINATED | An owner exists (locked=1) and no CPU is past the acquisition in a critical section; MCS head waits on locked|pending that never clears -> a permanently-held lock, i.e. the CPU 38 self-deadlock |
| T5 | Hardware fault (MCE/APEI) | ELIMINATED | No MCE/ECC/APEI in dmesg; GHES only appears as an NMI handler on the panic path; clean software deadlock |

**Root cause vs fatal mechanism.** Fatal mechanism = recursive `rq->lock` acquisition (T1) →
machine-wide convoy/CSD cascade (T2). Originating defect (root cause) = the scheduler emits
a warning **while holding rq->lock without deferring console output**, allowing a polling
console (netconsole/netpoll) to re-enter `try_to_wake_up()` under the lock. The avg_vruntime
overflow (T3) is only the trigger; it is a `WARN_ON_ONCE` the code is designed to survive,
and the same deadlock would occur for *any* printk/WARN emitted under rq->lock on a
netconsole host.

### AFFECTED_FUNCTIONS
__sum_w_vruntime_add, __enqueue_entity, put_prev_entity, __schedule, sched_exec,
sched_balance_rq, sched_balance_domains, _nohz_idle_balance, queued_spin_lock_slowpath,
try_to_wake_up, ttwu_queue, wakeup_softirqd, raise_softirq_irqoff, dev_kfree_skb_irq_reason,
netpoll_poll_dev, bnxt_tx_int, console_flush_all/vprintk_emit

### UPSTREAM_PATCH_STATUS
no_upstream_fix (for the deadlock). Candidates considered:

1. c06e05dae2c6 "sched/fair: Avoid overflow in enqueue_entity()" (K Prateek Nayak/AMD,
   cherry-pick of upstream 556146ce5e94). Class: root-cause fix for the *overflow trigger*
   (T3), NOT for the deadlock. **Already an ancestor of HEAD** (verified with
   `git merge-base --is-ancestor` = present) — yet the WARN still fired here, so it is
   necessary-but-insufficient; it does not address the reentrancy.
2. printk_deferred idiom already used by the scheduler for warnings emitted under rq->lock
   — `kernel/sched/fair.c:5870` (`printk_deferred("pick_eevdf would have returned NULL")`)
   and `kernel/sched/core.c:3254,3558`. The avg_vruntime WARN simply *missed* this idiom.
   No existing commit applies it to `__sum_w_vruntime_add()` in this tree.

Selected fix: apply the established scheduler printk_deferred idiom to the fair.c:691 WARN
(prevents the console flush — and thus the `try_to_wake_up()` recursion — from happening
under rq->lock). This addresses the originating defect (console reentrancy under the lock),
not merely the overflow trigger.

### PATCH
```diff
diff --git a/kernel/sched/fair.c b/kernel/sched/fair.c
index 5dc612effbec..e8535d6b5a47 100644
--- a/kernel/sched/fair.c
+++ b/kernel/sched/fair.c
@@ -688,7 +688,20 @@ __sum_w_vruntime_add(struct cfs_rq *cfs_rq, struct sched_entity *se)
 	s64 w_vruntime, key = entity_key(cfs_rq, se);

 	w_vruntime = key * weight;
+	/*
+	 * This runs under rq->lock (IRQs disabled).  Emitting this warning must
+	 * not synchronously flush the consoles from here: with a polling console
+	 * such as netconsole, console_unlock() -> netpoll_poll_dev() cleans the
+	 * NIC Tx ring and frees skbs via dev_kfree_skb_irq() ->
+	 * raise_softirq_irqoff() -> wakeup_softirqd() -> try_to_wake_up(), which
+	 * takes this very same rq->lock and self-deadlocks the CPU (and, via the
+	 * idle-balance convoy on the now-stuck rq, the whole machine).  Defer the
+	 * console output like the other scheduler warnings emitted under rq->lock
+	 * (see pick_next_entity()).
+	 */
+	printk_deferred_enter();
 	WARN_ON_ONCE((w_vruntime >> 63) != (w_vruntime >> 62));
+	printk_deferred_exit();

 	cfs_rq->sum_w_vruntime += w_vruntime;
 	cfs_rq->sum_weight += weight;
```
`printk_deferred_enter()/exit()` requires IRQs disabled for its duration; that is always
true here because rq->lock is held. The warning still fires and is logged — only its inline
console flush is deferred to a safe context (irq_work, after rq->lock is dropped), breaking
the reentrancy.

Scope/honesty note (per independent verifier PARTIALLY_AGREES): this patch prevents the
fatal invariant violation — `try_to_wake_up()` under rq->lock via a synchronous console
flush (D2) — which is the direct cause of the hard LOCKUP, and it is A/B-verified. It is NOT
a suppression of the diagnostic: the WARN still fires. It does NOT, however, prevent the
avg_vruntime overflow (D1) or the `sum_w_vruntime += w_vruntime` accounting drift that D1
causes in the non-PARANOID_AVG path; D1 is a separate, survivable-by-design condition tracked
as Recommendation #3. If the accounting overflow is considered in scope, D1 needs its own
follow-up on top of this lockup fix.

### KREVIEW_RESULTS
See kreview_results.txt (/kverify run). Summary: the change is minimal, matches the existing
scheduler printk_deferred idiom (fair.c:5870), preserves the diagnostic, and the
IRQs-disabled precondition for printk_deferred is satisfied by the held rq->lock. No
false-positive concerns identified; not a symptom-suppression (the WARN still fires; only
the fatal console reentrancy is removed).

### BUILD_TEST_RESULTS
Guest kernel built from the crash-kernel source with the patch applied:
`make O=build defconfig kvm_guest.config` then `make -j166 KCFLAGS=-Wno-error bzImage`
(-Wno-error works around a pre-existing gcc-vs-clang `-Werror` issue in drivers/pci, unrelated
to the patch). Build: SUCCESS (exit 0), bzImage produced (13.5 MB). Version string of the
booted image: `6.13.2-02441-g242a742712f1-dirty` — confirms it is the crash-kernel source
(g242a742712f1) plus the patch. Regression check: a clean boot with NO reproducer params
proceeds normally through init to the expected no-rootfs `VFS: Unable to mount root fs` panic
(exit 0) — the patch does not disturb normal boot. See AB_VERIFICATION for the A/B boots.

### REPRODUCER
Type: kernel (boot-param-gated in-tree reproducer) + QEMU. Full-fidelity reproduction (252-CPU
AMD idle-balance convoy + real avg_vruntime overflow + netconsole over a real NIC) is not
feasible in a single-node QEMU guest, so the reproducer exercises the **exact fatal invariant**
instead: a `printk()` emitted while holding rq->lock reaches a *reentrant polling console* whose
`->write()` performs the identical `raise_softirq_irqoff(NET_TX_SOFTIRQ) -> wakeup_softirqd() ->
try_to_wake_up()` sequence that netpoll's Tx cleanup performs — re-taking the current CPU's
rq->lock.

Mechanism (added to kernel/sched/fair.c, gated off by default; see reproducer/repro_harness.diff):
- A minimal `struct console` ("repro") whose `->write()` calls `raise_softirq_irqoff(NET_TX_SOFTIRQ)`
  while "armed" — mimicking netconsole/netpoll re-entering the scheduler.
- `repro_maybe_fire()` called from `__enqueue_entity()` (which runs under rq->lock): on CPU 0,
  once, it arms the console and `printk()`s a line, wrapping it in
  `printk_deferred_enter()/exit()` iff `repro_fix` is set (this models the patch).
- Boot params: `repro_fire` (arm), `repro_fix` (apply the deferral).

Expected:
- Without fix (`repro_fire`): the inline console flush re-enters `try_to_wake_up()` on the held
  rq->lock -> self-deadlock; boot hangs (no "SURVIVED", never reaches end of boot).
- With fix (`repro_fire repro_fix`): console flush is deferred; boot prints
  "REPRO: SURVIVED under-rq->lock printk (fix=1)" and proceeds to the end of boot.
Grep: `REPRO:` markers in the serial log.

### AB_VERIFICATION
Single build (crash-kernel source + kernel_fix.patch + the boot-param-gated reproducer
harness), booted twice in QEMU (`-enable-kvm -smp 2 -m 2048 -no-reboot`, no rootfs):

- Baseline (`console=ttyS0 repro_fire panic=5`): serial log ends at
  `REPRO: printk under rq->lock on cpu0 (fix=0)` and makes **no further progress**; QEMU was
  killed by `timeout` (exit 124) = **bug reproduced** (self-deadlock on rq->lock inside the
  inline console flush; the CPU never returns).
- With fix (`console=ttyS0 repro_fire repro_fix panic=5`): serial log shows
  `REPRO: printk under rq->lock on cpu0 (fix=1)` immediately followed by
  `REPRO: SURVIVED under-rq->lock printk (fix=1)`, then boot proceeds normally to
  `Kernel panic - not syncing: VFS: Unable to mount root fs` (expected: no rootfs) and QEMU
  exits cleanly (exit 0) = **bug gone**.
- Verdict: **FIX VERIFIED**. The `repro_fix` toggle (which applies exactly the patch's
  `printk_deferred_enter()/exit()` deferral) is the only difference between the two boots.
- Iterations: 1 (deterministic).

### RECOMMENDATIONS
1. Land the fair.c printk_deferred fix (this patch) — it removes the fatal rq->lock reentrancy
   for this WARN and matches the scheduler's own idiom.
2. Audit other WARN/printk sites reachable under rq->lock (and other scheduler raw locks) for
   the same missing deferral; consider a systemic guard so scheduler-context console output is
   always deferred.
3. Investigate the residual avg_vruntime `key*weight` overflow that still trips fair.c:691
   despite c06e05dae2c6 (the survivable trigger): it also corrupts `sum_w_vruntime` accounting
   in the non-PARANOID_AVG path (adds the overflowed product). Track the entity that produces
   the extreme `entity_key`, and consider routing the non-paranoid path through saturating/
   graceful handling like `sum_w_vruntime_add_paranoid()`.
4. Defense-in-depth (net): netpoll's Tx-cleanup poll should avoid `wakeup_softirqd()` (e.g.
   free via the completion queue without waking ksoftirqd), so a console flush can never
   re-enter the scheduler regardless of caller context.
5. Fleet: this reproduces wherever netconsole is configured (all affected hosts) and the WARN
   fires; prioritize the backport across 6.13-fbk branches.
