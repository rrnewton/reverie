### SEVERITY
high
57 hosts affected, crash type is a soft lockup that panics the box
(`softlockup_panic=1`), no data-loss risk (pure lock contention, no
corruption), and all affected hosts are developer VMs (VIRTUAL=100%,
`devvm*`) rather than production-serving fleet. The >50-host count alone would
argue "critical"; it is scored "high" because the impact is a self-recovering
panic/reboot of dev VMs with no data loss and a clear fix exists.

### SUBSYSTEM
fs (btrfs — extent map shrinker / per-root inode xarray)

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
medium
Root cause is proven from the all-CPU dump + source (see DETAILED_ANALYSIS) and
is unambiguous. Confidence is held at medium (not high) because, per the report
rubric, the patch has not yet been booted + A/B tested to completion in this run
— see BUILD_TEST_RESULTS / AB_VERIFICATION.

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
The btrfs extent map shrinker's `find_first_inode_to_shrink()`
(fs/btrfs/extent_map.c) holds the per-root inode xarray spinlock
(`root->inodes.xa_lock`) across an effectively unbounded scan that skips every
cached inode with no loaded extent maps. The only yield in the loop,
`cond_resched_lock()`, does not bound the hold time on a PREEMPT_NONE kernel
(spin_needbreak()==0) running on a KVM guest that uses an unfair test-and-set
spinlock. Under memcg-driven reclaim on a root with a very large, sparse inode
set, the shrinker holds that lock for many seconds while ~25 other CPUs spin on
it doing inode creation (`btrfs_add_inode_to_root()`→`xa_store()`) and eviction
(`btrfs_del_inode_from_root()`), producing the soft lockup seen at
`queued_spin_lock_slowpath`←`xa_store`←`btrfs_create_new_inode`. The originating
defect is the unbounded lock hold in the shrinker; the `xa_store` soft lockup in
the create path is only the consequence.

### DETAILED_ANALYSIS

**Crash.** `watchdog: BUG: soft lockup - CPU#156 stuck for 22s! [chef-client:3492126]`,
panic via `softlockup_panic=1`. Kernel 6.16.1-0_fbk2_hardened_0_gf40efc324cc8,
192-vCPU KVM guest (AMD EPYC-Genoa), preemption model "NONE" (dmesg CPU line).

**Stack walkthrough (crashing CPU 156, bottom-up).**
`entry_SYSCALL_64` → `__x64_sys_openat` → `do_filp_open` → `path_openat` →
`lookup_open` → `btrfs_create_common` (inode.c:6756) → `btrfs_create_new_inode`
(inode.c:6607) → `btrfs_add_inode_to_root` (inode.c:3852) → `xa_store`
(lib/xarray.c:1733) → `spin_lock` → `queued_spin_lock_slowpath` →
`virt_spin_lock`. The RIP is `queued_spin_lock_slowpath+0x22`, whose bytes
`f3 90 <83> 3f 00 75 f9` are the `virt_spin_lock` TAS retry loop
(`pause; cmpl $0,(%rdi); jne`). Registers: RDI/R12 = 0xff1100013942eb58 (the
lock), R13 = 0xff1100013942e800 (the `struct btrfs_root`), R15 = 0x605c9543
(the inode number being stored). The lock lives at offset 0x358 in the root
object = `root->inodes.xa_lock`.

**This is contention, and there is a single long-holding culprit.**
The all-CPU dump (drgn_output.txt) shows **24 CPUs** spinning in
`virt_spin_lock(lock=0xff1100013942eb58)` on this one lock, plus crashing CPU
156, i.e. ~25 CPUs contending on a single root's inode xarray lock. They fall
into two groups, both of which must take `root->inodes.xa_lock`:
- inode creation: `btrfs_create_new_inode` → `btrfs_add_inode_to_root` →
  `xa_reserve`/`xa_store` (inode.c:3847/3852);
- inode eviction: `evict` → `btrfs_destroy_inode` → `btrfs_del_inode_from_root`
  (inode.c:5686), including via icache pruning under memcg reclaim (CPU 4) and
  via `btrfs_iget`/`btrfs_read_locked_inode` failure paths (CPU 168).

Exactly one busy CPU is **not** in the slowpath: **CPU 158 (kworker/u768:13)** is
executing `xa_find`→`xas_find`→`xas_load`→`xa_is_node` inside
`find_first_inode_to_shrink` → `btrfs_scan_root(root=0xff1100013942e800)` →
`btrfs_extent_map_shrinker_worker` (extent_map.c:1204/1265/1341). Because
`find_first_inode_to_shrink` takes `xa_lock(&root->inodes)` at extent_map.c:1200
*before* the scan loop, CPU 158 is the lock **holder** while every other CPU
waits. Its `from=0x4a0740e8` (~1.2e9) vs target inode numbers ~0x605c95xx
(~1.6e9) show the root has a huge, sparse inode set and the scan is deep.

**Why the hold is unbounded (the defect).** `find_first_inode_to_shrink`
(extent_map.c:1194-1242) is:
```
xa_lock(&root->inodes);
while (true) {
    inode = xa_find(&root->inodes, &from, ULONG_MAX, XA_PRESENT);
    if (!inode) break;
    if (!write_trylock(&inode->extent_tree.lock)) goto next;
    if (RB_EMPTY_ROOT(&inode->extent_tree.root)) { write_unlock(...); goto next; }
    if (igrab(&inode->vfs_inode)) break;
    write_unlock(...);
next:
    from = btrfs_ino(inode) + 1;
    cond_resched_lock(&root->inodes.xa_lock);   /* the ONLY yield */
}
xa_unlock(&root->inodes);
```
Every inode with an empty extent tree (files opened only for stat(2),
directories, inodes whose extent maps were already released) is skipped via
`goto next` while the xarray lock stays held. The only relief is
`cond_resched_lock()`, and it does not help here:
- `__cond_resched_lock()` (kernel/sched/core.c:7373) drops the lock only if
  `spin_needbreak(lock) || should_resched()`.
- `spin_needbreak()` (include/linux/spinlock.h:463) returns 0 when
  `!preempt_model_preemptible()`. The kernel is **PREEMPT_NONE**, so this is
  always 0.
- `should_resched()` is true only when the current task needs rescheduling. The
  shrinker is a cpu-bound workqueue worker; with the other CPUs busy-spinning it
  can be the only runnable task on its CPU, so `need_resched()` may stay clear
  for many seconds.
- Even a preemptible kernel would not detect the contention here: the guest uses
  `virt_spin_lock` (arch/x86/include/asm/qspinlock.h:88, "fall back to a
  Test-and-Set spinlock, because fair locks have horrible lock 'holder'
  preemption issues"), an **unfair** lock whose waiters do not set qspinlock
  queue bits, so `spin_is_contended()` cannot see them.

So the shrinker holds `root->inodes.xa_lock` continuously while walking a very
large run of skip-able inodes, and the ~25 create/evict CPUs starve → CPU 156
exceeds the 22s soft-lockup threshold. The unfair TAS lock compounds it: even at
the brief moments the lock is free, the cache-hot shrinker tends to re-win.

**Trigger (environmental precondition).** There is **no global memory pressure**
(MemFree 177.4 GB, MemAvailable 370.7 GB, Slab 17 MB). The reclaim is per-cgroup:
CPU 4 shows `try_charge_memcg` → `try_to_free_mem_cgroup_pages` →
`shrink_slab_memcg` → `super_cache_scan` → `prune_icache_sb` → `evict` →
`btrfs_del_inode_from_root`. The btrfs sb shrinker
(`btrfs_free_cached_objects`, super.c:2386 → `btrfs_free_extent_maps`,
extent_map.c:1355) queues `em_shrinker_work` (a single async worker per fs_info),
which is CPU 158. The workload (chef-client plus many builder/test processes:
mcrcfgdump, CPUThreadPool, FuncSched, BatchedAtomicFi, SingletonFuncti,
squashfuse_ll) does heavy concurrent create/rename/open/evict on one large
subvolume, supplying the ~25-way contention.

**Theory table.**
| # | Theory | Status | Evidence |
|---|--------|--------|----------|
| T1 | `find_first_inode_to_shrink` holds `root->inodes.xa_lock` across an unbounded skip-scan; `cond_resched_lock` cannot bound it under PREEMPT_NONE + unfair virt spinlock (ROOT CAUSE) | CONFIRMED | CPU 158 is sole non-waiter, holding lock mid-`xa_find`; spinlock.h:465, core.c:7380, qspinlock.h:96; identical in upstream 6.18 |
| T2 | Memcg reclaim (not global) triggers the sb→em shrinker (PRECONDITION) | CONFIRMED | MemFree 177GB but CPU 4 in try_charge_memcg reclaim → super_cache_scan; super.c:2386 |
| Fatal | 25-way contention on `root->inodes.xa_lock`; CPU 156 stuck in `xa_store` create path (CONSEQUENCE) | CONFIRMED | 24 CPUs in virt_spin_lock on 0x…eb58; RIP=TAS loop |
| T3 | hardware_fault | ELIMINATED | no MCE/ECC/APEI; KVM guest |
| T4 | global OOM/resource exhaustion as root cause | ELIMINATED | memory abundant; defect is locking/scalability |

**Cross-CPU summary:** 1 holder (CPU 158, em shrinker) + 24 waiters + crashing
CPU 156, all on `root->inodes.xa_lock` of root 0xff1100013942e800; remaining CPUs
idle (swapper). Single-lock, single-root contention.

### AFFECTED_FUNCTIONS
find_first_inode_to_shrink, btrfs_scan_root, btrfs_extent_map_shrinker_worker,
btrfs_add_inode_to_root, btrfs_create_new_inode, btrfs_del_inode_from_root,
xa_store, xa_find, queued_spin_lock_slowpath, virt_spin_lock, cond_resched_lock

### UPSTREAM_PATCH_STATUS
no_upstream_fix
The current `find_first_inode_to_shrink()` (holding `root->inodes.xa_lock`
across the scan with only `cond_resched_lock()`) is **identical in
linus-upstream at v6.18** (verified: `git show linus-upstream:fs/btrfs/extent_map.c`).
Commits after HEAD touching extent_map.c on linus-upstream
(d7447f2dce46, 71e545d4e33f, cc53bd2085c8, 67e78f983e6a, 7a4f92d39f66,
fd00922abc07) are unrelated (tracepoints, alignment checks, bool conversions,
workqueue rename) and none change the lock-hold behavior. No local fbk branch
carries a fix either. Therefore a new fix is required (developed below).

### PATCH
```diff
diff --git a/fs/btrfs/extent_map.c b/fs/btrfs/extent_map.c
index 02bfdb976e40..d3d7ba237e70 100644
--- a/fs/btrfs/extent_map.c
+++ b/fs/btrfs/extent_map.c
@@ -1191,11 +1191,19 @@ static long btrfs_scan_inode(struct btrfs_inode *inode, struct btrfs_em_shrink_c
 	return nr_dropped;
 }

+/*
+ * Number of inodes to skip while holding the root's inode xarray lock in
+ * find_first_inode_to_shrink() before unconditionally releasing it so that
+ * other tasks contending on the lock can make progress.
+ */
+#define BTRFS_EM_SHRINK_LOCK_BATCH 100
+
 static struct btrfs_inode *find_first_inode_to_shrink(struct btrfs_root *root,
 						      u64 min_ino)
 {
 	struct btrfs_inode *inode;
 	unsigned long from = min_ino;
+	unsigned int skipped = 0;

 	xa_lock(&root->inodes);
 	while (true) {
@@ -1234,7 +1242,39 @@ static struct btrfs_inode *find_first_inode_to_shrink(struct btrfs_root *root,
 		write_unlock(&tree->lock);
 next:
 		from = btrfs_ino(inode) + 1;
-		cond_resched_lock(&root->inodes.xa_lock);
+		/*
+		 * A root can have a huge number of inodes cached in memory that
+		 * have no loaded extent maps (files opened only to stat(2) them,
+		 * directories, inodes whose extent maps were already released,
+		 * etc.), and we skip all of them here while holding the inode
+		 * xarray lock. That same lock is taken by every inode creation
+		 * (btrfs_add_inode_to_root()) and eviction
+		 * (btrfs_del_inode_from_root()) for this root, so holding it
+		 * while scanning a long run of such inodes starves those tasks.
+		 *
+		 * cond_resched_lock() is not enough to bound the hold time: on
+		 * non-preemptible kernels spin_needbreak() is always 0, so the
+		 * lock is only dropped when the current task needs rescheduling,
+		 * which may not happen for a long time for a cpu-bound worker
+		 * that is the only runnable task on its CPU. On virtualized
+		 * guests, which fall back to an unfair test-and-set spinlock,
+		 * the contention is not even detectable via spin_is_contended().
+		 * The result is that the extent map shrinker can hold this lock
+		 * for many seconds, causing soft lockups in tasks creating or
+		 * evicting inodes on the same root.
+		 *
+		 * So, in addition to cond_resched_lock(), unconditionally drop
+		 * and reacquire the lock every BTRFS_EM_SHRINK_LOCK_BATCH skipped
+		 * inodes to give waiters a chance to grab it and make progress.
+		 */
+		if (++skipped >= BTRFS_EM_SHRINK_LOCK_BATCH) {
+			skipped = 0;
+			xa_unlock(&root->inodes);
+			cond_resched();
+			xa_lock(&root->inodes);
+		} else {
+			cond_resched_lock(&root->inodes.xa_lock);
+		}
 	}
 	xa_unlock(&root->inodes);
```
Rationale: `from` is an index (not a dereferenced pointer), and after
reacquiring the lock `xa_find()` re-finds the next present entry `>= from`, so
correctness is unchanged; only the lock-hold granularity changes. The periodic
unconditional drop is not gated on preemption model or on the (undetectable on
virt TAS locks) contention state, so it bounds the hold to at most
BTRFS_EM_SHRINK_LOCK_BATCH iterations. Dropping the unfair lock thousands of
times during a long scan gives the ~25 waiting create/evict CPUs frequent
windows to win it, eliminating the multi-second starvation. The drop only
happens on the skip path (`next:`), never after a successful `igrab()`+`break`,
so the returned inode's reference/extent-tree lock are unaffected.

### KREVIEW_RESULTS
/kverify verdict (checklist applied manually — the `kverify` skill is not
registered in this offline env): ROOT CAUSE CONFIRMED, not a false positive.
(1) Path proven to execute — the all-CPU dump *observes* CPU 158 holding
root->inodes.xa_lock inside find_first_inode_to_shrink()'s xa_find while 24+
CPUs spin on that exact lock address; this is measured state, not hypothesis.
(2) Failure structurally proven — spin_needbreak()==0 under PREEMPT_NONE
(spinlock.h:465) and unfair virt_spin_lock (qspinlock.h:96) with waiters
invisible to spin_is_contended() together mean cond_resched_lock() cannot bound
the hold. (3) Full context checked — trigger (super_cache_scan gate),
contenders (btrfs_add_inode_to_root/btrfs_del_inode_from_root), preempt model
(taint "NONE"), lock identity (addr = root+0x358 = root->inodes, confirmed by
drgn slab annotations + RIP TAS bytes). (4) Not intentional — it is a real
scalability defect (soft lockup on 57 hosts). (5) No hallucination — every
file:line re-read and matches. No conclusion was reversed (retraction rule OK).
The fix targets the originating defect (unbounded hold), not the watchdog
(no symptom-fix).

kreview MCP tool is unavailable offline; a manual review checklist + /kverify was
applied to the patch. No correctness issues found:
- No UAF: `from` (an unsigned long index, not a pointer) is computed from
  `btrfs_ino(inode)` while the lock is still held, BEFORE the added
  `xa_unlock`/`xa_lock`; `inode` is not dereferenced after the drop.
- Reference/lock safety: the drop only runs on the skip path (`next:`), never
  after a successful `igrab()`+`break`, so the returned inode's reference and
  write-locked extent_tree are unaffected.
- Termination preserved: `from` still increases monotonically toward ULONG_MAX;
  `xa_find()` re-finds the next present entry `>= from` after reacquiring, so the
  scan result is unchanged (only lock-hold granularity changes).
- Patch compiles cleanly (fs/btrfs/extent_map.o, no warnings) and the patched
  kernel boots (see BUILD_TEST_RESULTS).

### BUILD_TEST_RESULTS
Built with local gcc 11.5 (mkosi-kernel unavailable offline), config =
x86_64 defconfig + kvm_guest.config + a repro fragment (BTRFS_FS=y, MEMCG,
SHRINKER_DEBUG, LOCKUP/SOFTLOCKUP/HUNG_TASK detectors, VIRTIO_BLK,
PREEMPT_NONE, WERROR off).
- Baseline (unpatched HEAD) kernel: BUILT OK; boots in QEMU 8.1.2 -enable-kvm.
  Crash preconditions reproduced in-guest: `Dynamic Preempt: none` and
  `kvm-guest: PV spinlocks disabled, forced by "nopvspin"` (i.e. the unfair
  virt_spin_lock TAS path is active), AMD EPYC-Genoa vCPUs, btrfs mounted.
- Fixed kernel: BUILT OK incrementally (only fs/btrfs/extent_map.o recompiled +
  relink, "bzImage is ready (#2)"); boots and runs the reproducer workload
  (which drives find_first_inode_to_shrink via the shrinker triggers) with NO
  crash, hang, or regression.
Patch is therefore BUILD- and BOOT-verified, but NOT A/B-verified (the
reproducer did not trigger the stall — see AB_VERIFICATION). Per the rubric,
confidence is capped at medium.

### REPRODUCER
A self-contained mkosi-substitute harness was built (all artifacts under
`reproducer/` and `build/`):
- `reproducer/init.c` — static-ish PID1 that: mounts devtmpfs/proc/sys/debugfs
  and a virtio btrfs disk; creates EM data-bearing files at LOW inode numbers
  (so `evictable_extent_maps > 0`, keeping the sb shrinker's gate open); PINS P
  empty-extent-tree inodes at HIGH inode numbers by holding them open (so they
  cannot be evicted and every shrinker cycle must scan the whole empty run);
  runs "keeper" workers that re-read the data files with POSIX_FADV_DONTNEED to
  force extent-map reload; runs a "victim" that times single `open(O_CREAT)`
  calls on the same subvolume (max latency = time blocked behind
  root->inodes.xa_lock); and repeatedly triggers the btrfs sb shrinker via
  `/sys/kernel/debug/shrinker/sb-btrfs:vda-*/scan`.
- `reproducer/Makefile.repro` builds the init, the initramfs cpio (via in-tree
  gen_init_cpio), and a 40G btrfs disk image (mkfs.btrfs).
- Boot: `qemu-system-x86_64 -enable-kvm -cpu host -smp 32 -m 20480 -kernel
  build/repro/arch/x86/boot/bzImage -initrd reproducer/out/initramfs.cpio
  -drive .../btrfs.img,if=virtio -append "... preempt=none nopvspin
  repro.P=3000000 repro.EM=40000 repro.H=6 repro.T=20 repro.thresh=1"`.
The harness boots the exact crash-kernel source with the crash preconditions
(PREEMPT_NONE + unfair virt TAS spinlock) and exercises the shrinker path, but
did not produce a measurable victim stall (see AB_VERIFICATION).

### AB_VERIFICATION
- Baseline (no fix): bug NOT reproduced. Max victim create() latency stayed
  0–2 ms across configurations (P=1e5..6e6 pinned inodes, H=4..48 workers,
  data-low/high orderings, with/without fadvise cache-drop). The multi-second
  lock-hold / soft lockup was not triggered.
- With fix: patched kernel booted and ran the same workload; max latency 0 ms;
  no crash/hang/regression (the patched find_first_inode_to_shrink path was
  exercised via ~33k shrinker triggers).
- Verdict: REPRODUCER INADEQUATE. On a 32-vCPU VM the debugfs-driven shrinker
  scans did not hold root->inodes.xa_lock long enough to stall the victim —
  consistent with the production event needing (a) far more contending CPUs
  (~25 waiters on 192 vCPUs) for unfair-TAS starvation and/or (b) a much larger
  live inode set so a single scan exceeds the 22s threshold, plus organic memcg
  reclaim rather than a debugfs poke that also prunes the icache. The root cause
  and fix therefore rest on the crash-dump + source evidence (all-CPU dump shows
  the shrinker holding the lock while ~25 CPUs spin), not on the VM A/B.
- Iterations: ~6 baseline configurations + 1 fixed-kernel run.

### RECOMMENDATIONS
1. Apply the patch (bound the `root->inodes.xa_lock` hold in
   `find_first_inode_to_shrink()`); it is the root-cause fix and applies cleanly
   to 6.16 and current upstream.
2. Submit upstream (linux-btrfs) — the defect exists unchanged through v6.18.
3. Consider a follow-up making the btrfs sb shrinker scan honor the memcg/nid
   from the shrink_control instead of always scanning all roots globally, and/or
   bounding total skip-scan work per shrinker invocation (secondary efficiency,
   not required for correctness).
4. Interim mitigation for affected dev VMs if needed: boot with `preempt=voluntary`
   does NOT fix it (virt TAS lock still undetectable); the reliable mitigation is
   the patch. `nopvspin`/pvspin choices do not remove the unbounded hold.
