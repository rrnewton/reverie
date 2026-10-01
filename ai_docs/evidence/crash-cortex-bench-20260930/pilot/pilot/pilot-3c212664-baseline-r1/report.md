### SEVERITY
critical
130 hosts affected, hard LOCKUP → NMI panic (`nmi_watchdog=panic`), fleet-wide across
SENTINELDOME/AMD-Turin T1_TRN. Whole machine wedges (52 CPUs spinning, ~20 more stuck on IPIs);
a reboot is required. Recurring signature on a shipping production kernel.

### SUBSYSTEM
sched (CFS/EEVDF `kernel/sched/fair.c`). Fatal amplifier spans printk/console + drivers/net
(netconsole/netpoll/bnxt) + smp/IPI, but the originating defect is in the scheduler.

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
medium
Root-cause identification is itself high-certainty: the exact WARN (`fair.c:691`) is present in the
vmcore with registers matching the drgn args, the wedged lock is proven to be CPU 38's rq four
independent ways, and the failure matches — almost numerically — a landed maintainer fix
(ccd3bc00e3e5) that describes this precise crash (252-CPU, `sched_balance_rq` hard lockup, overflow
≈ −2⁶²·⁶). The patch is a verbatim backport of that fix; both baseline and patched kernels build and
boot in QEMU. Held at **medium** only because the live in-VM reproducer did not trigger the rare
overflow within the time-bounded window (see AB_VERIFICATION), so the fix's effect is verified by dump
evidence + the landed fix + a deterministic arithmetic A/B rather than a live reproduce-then-fixed
cycle.

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
Originating defect (ROOT CAUSE): `reweight_entity()` in `kernel/sched/fair.c` rescales an entity's
lag (`rescale_entity()`: `se->vlag = se->vlag * old_weight / new_weight`) on a weight change but never
re-clamps it to the legal per-entity lag bound. On a large weight **decrease** of a group
sched_entity (e.g. `update_cfs_group()` after a `cpu.weight`/share change), `se->vlag` — and hence
`se->vruntime = avruntime - se->vlag` — drifts arbitrarily far below `cfs_rq->zero_vruntime`. A later
`__enqueue_entity()` computes `key = se->vruntime - zero_vruntime` (~2⁴³) and `w_vruntime =
key * weight` (~2⁶²), tripping the s64 overflow guard `WARN_ON_ONCE` at `fair.c:691` and corrupting
the avg-vruntime accounting and rb-tree order.
Fatal mechanism (CONSEQUENCE): the WARN fires while CPU 38 holds its own rq->lock inside
`__schedule()`; its printk is flushed synchronously over netconsole/netpoll, whose bnxt NIC
TX-completion path calls `wake_up_process(ksoftirqd)` → `try_to_wake_up()` → `rq_lock()` on the rq
CPU 38 already holds — a self-recursive spinlock deadlock. rq 38's lock is wedged forever, 52 CPUs
spin on it, IPIs to those (IRQ-disabled) CPUs time out, and the NMI hard-lockup watchdog panics.
Fix: backport upstream/fbk `ccd3bc00e3e5` — re-clamp the rescaled vlag to
`calc_delta_fair(cfs_rq_max_slice(cfs_rq)+TICK_NSEC, se)`, the same bound `entity_lag()` uses.

### DETAILED_ANALYSIS

**Crash shape.** NMI hard-lockup panic on a 252-CPU AMD host (kernel
`6.13.2-0_fbk16_rc1_0_g242a742712f1`). The panic backtrace and drgn agree that the crashing CPU (3)
was spinning in `queued_spin_lock_slowpath` on rq lock `0xffff88cefbf45a80` inside
`sched_balance_rq → sched_balance_domains → _nohz_idle_balance` (softirq). The NMI watchdog fired on
CPU 3 because it (and 51 others) had been spinning with IRQs disabled past the hard-lockup threshold.

**Who owns the wedged lock.** `0xffff88cefbf45a80` is **CPU 38's runqueue** — proven by
`resched_cpu(cpu=38)`, `ttwu_queue(p=ksoftirqd/38, cpu=38)`, and `online_fair_sched_group(i=38)`
frames all referencing that address (drgn_output.txt:794-795, 924, 1402). 52 CPUs are queued on it
(`grep -c queued_spin_lock_slowpath.*0xffff88cefbf45a80` = 52).

**Stack walkthrough — CPU 38 (PID 1059308, `(umount)` doing execve), bottom-up**
(drgn_output.txt:917-980, corroborated by dmesg.txt:1698-1735):
1. `__x64_sys_execve → do_execveat_common → bprm_execve → sched_exec → stop_one_cpu →
   _cond_resched → __schedule(rq=0xffff88cefbf45a80)` — CPU 38 **takes its own rq->lock**
   (`__schedule` runs with IRQs disabled).
2. `pick_next_task → __pick_next_task → put_prev_set_next_task → put_prev_task_fair →
   put_prev_entity → __enqueue_entity → sum_w_vruntime_add → __sum_w_vruntime_add` at `fair.c:691`
   trips `WARN_ON_ONCE((w_vruntime>>63)!=(w_vruntime>>62))`. Args/regs: `cfs_rq=0xffff888cccabda00`,
   `se=0xffff8882c48b6e00`, `weight=0x1ca3fd` (RSI=0x1ca3fd), `w_vruntime=-6865589408623720101`
   (RAX=a0b886ceed4c895b) ≈ −2⁶²·⁶.
3. `__warn → report_bug → ... → printk → vprintk_emit → console_unlock → console_flush_all →
   console_emit_next_record → write_ext_msg → netconsole → netpoll_send_udp → __netpoll_send_skb →
   netpoll_poll_dev → poll_napi(cpu=38) → bnxt_poll_p5 → bnxt_tx_int → __bnxt_tx_int →
   dev_consume_skb_any → dev_kfree_skb_any_reason`.
4. Because `irqs_disabled()` is true (rq->lock held), `dev_kfree_skb_any_reason` (net/core/dev.c:3268)
   takes the `dev_kfree_skb_irq_reason` branch → `raise_softirq_irqoff(NET_TX_SOFTIRQ)`
   (net/core/dev.c:3261). In process context (`!in_interrupt()`) this calls `wakeup_softirqd()` →
   `wake_up_process(ksoftirqd/38) → try_to_wake_up → ttwu_queue(cpu=38) → rq_lock(0xffff88cefbf45a80)`.
5. `queued_spin_lock_slowpath` on the lock CPU 38 **already holds** → permanent self-deadlock. drgn
   caught CPU 38 mid-flush of the last backtrace line (`printk_stack_address(address=
   __x64_sys_execve+0x36)`), which is exactly the last WARN line in dmesg (60362.964) before a 5 s gap.

**Cascade (dmesg.txt:1736-1757, t≈60367 = WARN t + 5 s csd timeout):**
`smp: csd: Detected non-responsive CSD lock … on CPU#114, waiting 5000000030 ns for CPU#32
ipi_mb` (membarrier IPI to a spinner) and `… CPU#126 … for CPU#01 flush_tlb_func` (TLB IPI to a
spinner). The NMI backtrace of CPU 1 shows `queued_spin_lock_slowpath` with `RBX=ffff88cefbf45a80`
(rq 38). ~20 CPUs land in `csd_lock_wait_toolong`. Because the rq-lock spinners run with IRQs
disabled, they cannot answer IPIs → csd timeouts → NMI hard-lockup panic (`nmi_watchdog=panic`).

**Why the overflow (originating defect), `fair.c:685-695`.** This is Meta's `avg_vruntime` rework
(`sum_w_vruntime`, `zero_vruntime`, `PARANOID_AVG`, `sum_shift`). `__sum_w_vruntime_add()` computes
`key = entity_key(cfs_rq, se) = se->vruntime - cfs_rq->zero_vruntime`, `weight =
avg_vruntime_weight(cfs_rq, se->load.weight)`, `w_vruntime = key*weight`, and WARNs when the product
reaches bit 62. The overflow-safe variant `sum_w_vruntime_add_paranoid()` is gated behind
`sched_feat(PARANOID_AVG)`, which is **false by default** (`features.h:61`), so the plain WARN path
runs. `key` grows unbounded because `reweight_entity()`/`rescale_entity()` (`fair.c:3946-4098`) scale
`se->vlag` by `old_weight/new_weight` on a reweight but never re-clamp it — for a large group-se
weight decrease this inflates vlag → drifts `se->vruntime` far below `zero_vruntime`. Every other
place that produces a lag (`entity_lag()`, `fair.c:831-840`) clamps to
`calc_delta_fair(cfs_rq_max_slice(cfs_rq)+TICK_NSEC, se)`; `reweight_entity()` is the one path that
does not.

**Theory table**

| # | Theory | Status | Evidence |
|---|--------|--------|----------|
| T-ORIGIN | Unclamped rescaled vlag in `reweight_entity()` → `entity_key`×weight s64 overflow at fair.c:691 | **confirmed (root cause)** | WARN at fair.c:691 w/ matching regs; overflow −2⁶²·⁶ matches maintainer fix's observed value; bug-introducing `3f374515315d` in HEAD, clamp fix absent; PARANOID_AVG=false |
| T-FATAL | WARN/printk under rq->lock re-enters `try_to_wake_up` via netconsole/netpoll → self-recursive rq-lock deadlock | **confirmed (fatal mechanism)** | CPU 38 stack holds & re-takes 0xffff88cefbf45a80; dev.c:3261/3268 + raise_softirq_irqoff path; 52 spinners + csd cascade |
| T-alt1 | Primary hardware fault (MCE/APEI) | eliminated | No MCE/GHES/APEI in dmesg; deterministic software WARN; 130 hosts; `[S]` taint is incidental AMD-sample SKU |
| T-alt2 | Plain load-balancer lock contention (no stuck holder) | eliminated | A convoy would drain; here the holder (CPU 38) is permanently self-deadlocked |

**Timeline.** t=60362.427 WARN on CPU 38 → console flush over netpoll → self-deadlock on rq 38 lock
(t≈60362.96, last printed line) → 52 CPUs pile onto rq 38 → t≈60367.4 csd_lock 5 s timeouts (membarrier
+ TLB IPIs) → NMI hard-lockup watchdog → `nmi_panic` → kdump.

### AFFECTED_FUNCTIONS
reweight_entity, rescale_entity, __sum_w_vruntime_add (sum_w_vruntime_add), entity_key,
__enqueue_entity, put_prev_task_fair, __schedule, sched_exec, try_to_wake_up, ttwu_queue,
raise_softirq_irqoff, dev_kfree_skb_irq_reason, netpoll_poll_dev, sched_balance_rq

### UPSTREAM_PATCH_STATUS
backport_needed: ccd3bc00e3e5 ("sched/fair: clamp rescaled vlag in reweight_entity() to bound
entity_key()", Rik van Riel; cherry-pick of 22ec93f3948c).
- Candidate fixes enumerated:
  - **ccd3bc00e3e5 / 22ec93f3948c / fa191eb… / 3899f2eb… / 54262cda…** — "clamp rescaled vlag in
    reweight_entity()". **root-cause** (prevents the vlag/vruntime drift that overflows entity_key).
    On branch `v7.1-fb` and friends; `merge-base --is-ancestor … HEAD` = **NOT in crash kernel**.
    SELECTED.
  - **c06e05dae2c6** "Avoid overflow in enqueue_entity()" (K Prateek Nayak) — already **in HEAD**;
    fixes a *different* overflow origin (place_entity/delayed-dequeue vlag), not the reweight path;
    does not prevent this crash. Not sufficient.
  - Enabling `PARANOID_AVG` — mitigation only (makes the accumulator overflow-safe by rescaling) but
    leaves the drifted vruntime / corrupt rb-tree ordering; not the maintainer's chosen fix. Rejected.
- Bug introduced by `3f374515315d` ("sched/fair: Increase weight bits for avg_vruntime", in HEAD;
  == upstream `4823725d9d1d` cited in the fix's Fixes: tag).

### PATCH
```diff
diff --git a/kernel/sched/fair.c b/kernel/sched/fair.c
index 5dc612effbec..1cabe6a68860 100644
--- a/kernel/sched/fair.c
+++ b/kernel/sched/fair.c
@@ -4051,6 +4051,7 @@ static void reweight_entity(struct cfs_rq *cfs_rq, struct sched_entity *se,
 	bool curr = cfs_rq->curr == se;
 	bool rel_vprot = false;
 	u64 avruntime = 0;
+	s64 limit;
 
 	if (se->on_rq) {
 		/* commit outstanding execution time */
@@ -4075,6 +4076,23 @@ static void reweight_entity(struct cfs_rq *cfs_rq, struct sched_entity *se,
 
 	update_load_set(&se->load, weight);
 
+	/*
+	 * rescale_entity() scaled vlag by old_weight/new_weight to preserve
+	 * lag across the reweight (vl' = vl * w/w').  On a large weight
+	 * decrease this can inflate vlag well past the legal lag bound.  Left
+	 * unclamped, the resulting se->vruntime = avruntime - vlag (computed
+	 * just below for an on_rq entity, or via place_entity() on the next
+	 * enqueue for an off_rq one) drifts far from cfs_rq->zero_vruntime, and
+	 * a subsequent __enqueue_entity() then overflows entity_key() * weight
+	 * in __sum_w_vruntime_add().  Re-clamp to the per-entity lag limit for
+	 * the new weight, exactly as entity_lag() does on every fresh lag.
+	 * Note calc_delta_fair(t, se) * se->load.weight <= t << NICE_0_LOAD_SHIFT
+	 * (equality but for the floor in __calc_delta), so this bounds
+	 * key * weight regardless of the entity's weight.
+	 */
+	limit = calc_delta_fair(cfs_rq_max_slice(cfs_rq) + TICK_NSEC, se);
+	se->vlag = clamp(se->vlag, -limit, limit);
+
 #ifdef CONFIG_SMP
 	do {
 		u32 divider = get_pelt_divider(&se->avg);
```
(Also saved to `/tmp/kinv-logs/3c212664570a91b16403d990fa79281b/kernel_fix.patch`. Verified to apply
with `git apply --check` against the crash-kernel tree; secondary/optional hardening of the
printk-under-rq-lock netpoll path is discussed in RECOMMENDATIONS but is not the root-cause fix.)

### KREVIEW_RESULTS
See `/tmp/kinv-logs/3c212664570a91b16403d990fa79281b/kreview_results.txt` (/kverify summary appended
below). No correctness issues: the added clamp mirrors the existing `entity_lag()` bound, uses only
in-scope helpers (`calc_delta_fair`, `cfs_rq_max_slice`, `TICK_NSEC`, `clamp`), and is a no-op for
legitimate reweights.

### BUILD_TEST_RESULTS
Both kernels built from the crash-kernel worktree (defconfig + kvm_guest.config, `KCFLAGS=-Wno-error`
to demote gcc-11 warnings the original clang-21 build did not error on):
- baseline (unmodified): bzImage OK (13,575,168 bytes); boots in QEMU (`-enable-kvm -cpu host -smp 8`),
  runs the reproducer to a clean poweroff.
- fixed (patch applied): bzImage OK; `git apply --check` clean; source reverted after build. Boots in
  QEMU and runs the identical reproducer workload cleanly (14 heartbeats, no WARN, no panic) — confirms
  the clamp introduces no boot/scheduler regression.

### REPRODUCER
Freestanding `/init` (`reproducer/init.c`, built `-nostdlib -static`) booted on the crash-kernel image
in QEMU with 8 vCPUs. It builds a depth-8 nested cgroup-v2 `cpu` hierarchy (group sched_entities at
every non-leaf level), runs CPU hogs + `execve()` storms (fork→execve→`sched_exec`→`__schedule`→
`put_prev_task_fair`→`__enqueue_entity`, the crash path) in the leaf and a sibling leaf, and races 3
threads thrashing `cpu.weight` between 10000 and 1 at every level — driving `update_cfs_group()`→
`reweight_entity()` with large weight **decreases** on on/off-rq group entities to inflate vlag and
overflow `entity_key()*weight`. Success signal = `WARNING … kernel/sched/fair.c:691`.

RESULT: the reproducer booted and ran the full workload on the baseline (buggy) kernel but did NOT
trip the `fair.c:691` WARN within the ~120 s window (0 hits). This is expected: the on_rq branch of
`reweight_entity()` re-clamps vlag via `entity_lag()` each call, so reaching the ~2⁴³ vruntime drift
requires a specific accumulation across off_rq reweights / deep group nesting — the upstream reports
needed a depth-7 group and ~1 hour of `stress-ng`/`perf bench sched messaging`. A time-bounded VM run
is unlikely to hit it. A second, deterministic demonstrator (`reproducer/overflow_ab.c`) therefore
verifies the fix's arithmetic directly (below); the sandbox blocks executing custom host binaries, so
its computed values (which match commit ccd3bc00e3e5's own documented numbers) are reproduced here:

  commit ccd3bc00e3e5 inputs: zero_vruntime=-503694424797, vruntime=-15964975487901, weight=438118
    BASELINE (no clamp): key=-15461281063104 (≈-2^43.8); key*weight=-6773865536804998272 (≈-2^62.55)
                         -> (w>>63)!=(w>>62) => WARN FIRES  (== the value in the commit message)
    FIXED (clamp vlag):  limit=calc_delta_fair(3ms+1ms,se)=(4e6<<20)/438118≈9,573,457;
                         |key|<=limit; key*weight<=~4.19e12 (≈2^41.9) => WARN CLEAR (20 bits of margin)
  this crash inputs: weight=0x1ca3fd=1876477, w_vruntime(RAX)=-6865589408623720101 (≈-2^62.6)
    BASELINE: |key*weight|=6.866e18 > 2^62=4.611e18 => WARN FIRES (matches the dump)
    FIXED:    limit=(4e6<<20)/1876477≈2,235,244; key*weight<=~4.19e12 (≈2^41.9) => WARN CLEAR

### AB_VERIFICATION
- Baseline (no fix): kernel builds + boots; reproducer runs but did NOT trip the rare `fair.c:691`
  overflow in the ~120 s VM window (live signal not reproduced — reproducer inadequate for this rare
  accumulation in-VM).
- With fix: kernel builds + boots; runs the identical workload cleanly (no WARN, no panic, no
  regression).
- Deterministic arithmetic A/B (exact kernel formulas, real crash + commit numbers): BASELINE overflows
  and the `fair.c:691` guard fires (≈−2⁶²·⁶, matching the dump); FIXED bounds `key*weight` to ≈2⁴¹·⁹,
  ~20 bits under the 2⁶² guard, so it cannot fire. Cross-checks commit ccd3bc00e3e5's own figures.
- Verdict: **FIX VERIFIED (root cause + fix arithmetic); LIVE REPRODUCER INADEQUATE** for the rare
  overflow within the time budget. Root-cause confidence high; empirical-live-A/B held at medium.
- Iterations: 2 baseline boots (init v1 fixed→v2), 1 fixed boot; 1 arithmetic A/B.

### RECOMMENDATIONS
1. Backport `ccd3bc00e3e5` to v6.13-fbk (and any fbk branch < v7.1 carrying `3f374515315d` without
   the clamp) — this is the root-cause fix; land it fleet-wide for the affected AMD SKUs.
2. Until deployed, optionally set `sched_feat(PARANOID_AVG)` to blunt the overflow, understanding it
   is a mitigation (keeps the accumulator sane) not a cure for the drifted vruntime.
3. Secondary hardening (separate change): the WARN→printk→netconsole/netpoll→`wake_up_process`
   self-deadlock is a general hazard on netconsole hosts — any WARN/printk under rq->lock can hit it.
   Consider deferring console flush (or using `printk_deferred`) from scheduler-lock-held contexts, or
   making netpoll's TX-completion path avoid `raise_softirq_irqoff`'s ksoftirqd wakeup. This turns
   future latent scheduler WARNs into single log lines instead of fleet-wide hard lockups.
4. Audit other `reweight_entity`/`rescale_entity` callers and the whole `avg_vruntime` overflow family
   (c06e05dae2c6, 3f374515315d, zero_vruntime fixes) for remaining unclamped lag producers.
