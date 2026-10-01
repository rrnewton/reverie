### SEVERITY
high
57 hosts affected; kernel panic (softlockup_panic) taking down whole VMs; no data
corruption. Widespread across a VM fleet but confined to developer VMs (devvm),
so high rather than critical.

### SUBSYSTEM
fs (btrfs) — extent-map shrinker / per-root inode XArray. Fatal mechanism in
kernel/locking (qspinlock virt path) and the guest's lack of PV spinlocks.

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
high
Root cause is directly evidenced by the all-CPUs dump: the lock HOLDER (CPU 158,
extent-map shrinker) and every waiter are identified on the same lock and same
root, and drgn args show a single uninterrupted ~772M-ino-space hold. The fix was
built, booted, and A/B tested in QEMU: with identical workload/pressure the
unpatched kernel stalled the crashing operation (openat create) to 225 ms with a
~10x throughput collapse, while the patched kernel held it to 53 ms with no
collapse (see AB_VERIFICATION). Caveat: the QEMU A/B reproduced the CONTENTION
MECHANISM (stall + throughput collapse) at reduced scale, not the full 22 s
soft-lockup panic, which needs prod scale (192 vCPUs and a far larger cached-inode
population).

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
Root cause (originating defect): the btrfs extent-map shrinker's
`find_first_inode_to_shrink()` (fs/btrfs/extent_map.c) holds the per-subvolume
`root->inodes` XArray spinlock across an unbounded linear scan of the inode
xarray, releasing it only via `cond_resched_lock()` — which drops the lock only
when a reschedule is already pending, so on a busy CPU it is held continuously
while hundreds of thousands of cached inodes are scanned. That lock is the hottest
per-subvolume lock in btrfs (taken by every inode create and every inode evict).
Fatal mechanism (consequence): on this 192-vCPU KVM guest, which falls back to the
unfair `virt_spin_lock` test-and-set path (no PV spinlocks), ~20 vCPUs pile up on
that one lock while the shrinker holds it; a file-creating task (chef-client,
`btrfs_create_new_inode -> xa_store`) starves for >22s and `softlockup_panic=1`
turns the stall into a panic. The fix bounds the shrinker's lock hold.

### DETAILED_ANALYSIS

**Stack trace (crashing CPU 156, chef-client) — bottom-up**
`__x64_sys_openat -> do_filp_open -> path_openat -> btrfs_create_common ->
btrfs_create_new_inode -> btrfs_add_inode_to_root (inode.c:3852) -> xa_store ->
spin_lock -> virt_spin_lock -> queued_spin_lock_slowpath` (stuck 22s).
The crash site is the `xa_store(&root->inodes, ino, inode, GFP_ATOMIC)` that
inserts the new inode into its subvolume's inode xarray. It is not the defect —
it is the victim waiting for the lock.

**The contended lock and its holder (drgn all-CPUs snapshot)**
- Contended lock = `0xff1100013942eb58` = `root->inodes.xa_lock`
  (btrfs_root `0xff1100013942e800` + 0x358). The address appears 80 times across
  the dump: ~20 of the 192 vCPUs are spinning on this single lock, every one via
  `virt_spin_lock()` (the unfair TAS fallback), qspinlock word `locked:1`.
- Waiters split into two btrfs workloads, both legitimate and both O(1) under the
  lock:
  * CREATE (openat O_CREAT): CPUs 43/156/175/189… in `btrfs_create_new_inode` ->
    `xa_reserve` (inode.c:6431) and `xa_store` (inode.c:3852 via
    btrfs_add_inode_to_root).
  * EVICT: CPUs 4/31/168/169/182/188/190… in `btrfs_del_inode_from_root`
    (inode.c:5686). Most are driven by **memcg direct reclaim**
    (`try_charge_memcg -> try_to_free_mem_cgroup_pages -> shrink_slab ->
    super_cache_scan -> prune_icache_sb -> evict`), the rest by rename/unlink.
- **The holder**: CPU 158 (kworker/u768:13) is in
  `btrfs_extent_map_shrinker_worker -> btrfs_scan_root -> find_first_inode_to_shrink
  -> xa_find` on the SAME root `0xff1100013942e800`. It is executing xarray
  traversal (not in the spin slowpath), i.e. it HOLDS `root->inodes.xa_lock`.
  drgn args prove a long single hold: `btrfs_scan_root(min_ino=0x1bff524b)` but the
  local `find_first_inode_to_shrink(from=0x4a0740e8)` — the scan has advanced
  ~772M in ino-space within one uninterrupted lock hold (`cond_resched_lock` never
  fired).

**Why the hold is unbounded (fs/btrfs/extent_map.c:1194)**
```
xa_lock(&root->inodes);
while (true) {
    inode = xa_find(&root->inodes, &from, ULONG_MAX, XA_PRESENT);   // 1204
    ...
    if (!write_trylock(&tree->lock)) goto next;
    if (RB_EMPTY_ROOT(&tree->root)) { write_unlock(&tree->lock); goto next; } // skip
    if (igrab(&inode->vfs_inode)) break;
    write_unlock(&tree->lock);
next:
    from = btrfs_ino(inode) + 1;
    cond_resched_lock(&root->inodes.xa_lock);   // 1237 - only drops if need_resched
}
xa_unlock(&root->inodes);
```
On an active subvolume, most cached inodes have no loaded extent maps
(`RB_EMPTY_ROOT` — directories, stat-only files, files whose maps were already
shrunk), so the loop skips them one by one while holding `xa_lock`, searching for
the next inode that has extent maps to drop. The only in-loop release is
`cond_resched_lock()`, which is a no-op unless a reschedule is pending — on a busy
vCPU running the kworker this may never happen, so the lock is held for the entire
scan. Notably the sibling loop `btrfs_scan_inode()` DOES bail on lock contention
(`rwlock_needbreak(&tree->lock)`, line 1184); `find_first_inode_to_shrink()` has no
equivalent bound on the far hotter `xa_lock`.

**Amplifier: memcg reclaim.** MemTotal 471 GB / MemFree 177 GB — NO global memory
shortage. The pressure is cgroup-local: a memcg (`0xff11000a6d1fe180`) at its
`memory.max`. Every allocation by its tasks enters direct reclaim ->
`super_cache_scan`, which both (a) evicts btrfs inodes (`prune_icache_sb`, adding
~8 CPUs to the lock) and (b) queues the extent-map shrinker worker
(`btrfs_free_cached_objects -> btrfs_free_extent_maps`, super.c:2391). So the same
condition that fuels the shrinker also floods the lock with evictors.

**Fatal mechanism: unfair VM spinlock.** The guest executes `virt_spin_lock()`
(arch/x86/include/asm/qspinlock.h:104) — the TAS fallback used when PV spinlocks
are not active (KVM guest without KVM_FEATURE_PV_UNHALT or booted nopvspin).
Unlike the fair MCS qspinlock, it provides no queueing/fairness and no
holder-kick, so under the shrinker's long hold + the create/evict storm on 192
vCPUs, CPU 156 can starve arbitrarily long. `softlockup_panic=1` (Meta hardened
kernel) then converts the 22s stall into a panic.

**Theory table**
| # | Theory | Role | Status |
|---|--------|------|--------|
| T1 | find_first_inode_to_shrink() holds root->inodes.xa_lock across an unbounded inode scan | ORIGINATING DEFECT | CONFIRMED (holder caught mid-scan on the contended root; 772M ino-space in one hold) |
| T2 | Unfair virt_spin_lock (no PV spinlocks) + softlockup_panic=1 make the stall fatal | FATAL MECHANISM | CONFIRMED (all waiters in virt_spin_lock) |
| T3 | memcg-limited direct reclaim multiplies lock contenders & fuels the shrinker | AMPLIFIER | CONFIRMED (8 CPUs evicting from try_to_free_mem_cgroup_pages; MemFree 177 GB) |
| T4 | e690747316c8 "lost inode" subvolume-deletion lockup | — | REJECTED (fix already present; inode.c:5696 uses __xa_cmpxchg; different symptom) |
| T5 | create path takes xa_lock twice (xa_reserve + xa_store) | — | REJECTED as primary (deliberate preallocation 061ea8581b2e; O(1) holds) |

**Timeline**: active chef/mcrcfg file churn on the root subvolume builds a large
cached-inode population -> memcg hits memory.max -> direct reclaim storm evicts
inodes and queues the em shrinker -> shrinker grabs root->inodes.xa_lock and scans
the huge empty-extent-tree inode population -> ~20 vCPUs (creates+evicts) pile up
on the unfair lock -> CPU 156 starves 22s -> softlockup_panic -> panic.

### AFFECTED_FUNCTIONS
find_first_inode_to_shrink, btrfs_scan_root, btrfs_extent_map_shrinker_worker,
btrfs_add_inode_to_root, btrfs_create_new_inode, btrfs_del_inode_from_root,
xa_store/xa_find, queued_spin_lock_slowpath/virt_spin_lock

### UPSTREAM_PATCH_STATUS
no_upstream_fix (as of crash-kernel cutoff v6.16-fbk2, commit f40efc324cc8).
Related history examined in fs/btrfs:
- 102044384056 "make the extent map shrinker run asynchronously as a work queue
  job" (Filipe Manana) — introduced the current worker; `find_first_inode_to_shrink`
  holds xa_lock with only cond_resched_lock. This is the code being fixed.
- e690747316c8 "fix subvolume deletion lockup caused by inodes xarray race" (Omar
  Sandoval, osandov@fb.com) — root-cause fix for a DIFFERENT soft-lockup (infinite
  loop in btrfs_kill_all_delayed_nodes); already an ancestor of HEAD; not our bug.
- e2844cce75c9 / e98f259c1cc1 / 764e290f096a — the lock-split for delayed_nodes vs
  inodes xarrays and Meta's revert/re-apply dance; final state has separate
  xa_locks; already present.
No landed commit bounds the xa_lock hold in find_first_inode_to_shrink, so a new
patch is proposed (root-cause: it prevents the long hold rather than suppressing
the soft-lockup diagnostic).

### PATCH
Bounds the `root->inodes` lock hold in `find_first_inode_to_shrink()`: force a drop
of the lock (with cond_resched) every BTRFS_EM_SHRINK_INODES scanned inodes,
resuming the scan from `from`. This guarantees other lock users make progress
regardless of the preemption model or spinlock fairness. Full diff:

```diff
diff --git a/fs/btrfs/extent_map.c b/fs/btrfs/extent_map.c
index 02bfdb976e40..ee0fe4e1f65f 100644
--- a/fs/btrfs/extent_map.c
+++ b/fs/btrfs/extent_map.c
@@ -1191,11 +1191,18 @@ static long btrfs_scan_inode(struct btrfs_inode *inode, struct btrfs_em_shrink_c
 	return nr_dropped;
 }
 
+/*
+ * Maximum number of inodes find_first_inode_to_shrink() scans while holding the
+ * root->inodes lock before dropping it to let other users make progress.
+ */
+#define BTRFS_EM_SHRINK_INODES 100
+
 static struct btrfs_inode *find_first_inode_to_shrink(struct btrfs_root *root,
 						      u64 min_ino)
 {
 	struct btrfs_inode *inode;
 	unsigned long from = min_ino;
+	unsigned int scanned = 0;
 
 	xa_lock(&root->inodes);
 	while (true) {
@@ -1234,7 +1241,26 @@ static struct btrfs_inode *find_first_inode_to_shrink(struct btrfs_root *root,
 		write_unlock(&tree->lock);
 next:
 		from = btrfs_ino(inode) + 1;
-		cond_resched_lock(&root->inodes.xa_lock);
+		/*
+		 * The root->inodes lock is heavily contended: every inode
+		 * creation (xa_reserve()/xa_store() from btrfs_create_new_inode())
+		 * and every inode eviction (btrfs_del_inode_from_root()) for the
+		 * subvolume takes it. Holding it while scanning a large number of
+		 * inodes here can stall those tasks for a long time; on
+		 * virtualized hosts that fall back to unfair spinlocks this has
+		 * been observed to trigger multi-second soft lockups. cond_resched_lock()
+		 * only drops the lock when a reschedule is pending, which may never
+		 * happen on a busy CPU, so also force a drop every
+		 * BTRFS_EM_SHRINK_INODES inodes. The scan resumes from 'from'.
+		 */
+		if (++scanned == BTRFS_EM_SHRINK_INODES) {
+			scanned = 0;
+			xa_unlock(&root->inodes);
+			cond_resched();
+			xa_lock(&root->inodes);
+		} else {
+			cond_resched_lock(&root->inodes.xa_lock);
+		}
 	}
 	xa_unlock(&root->inodes);
```

### KREVIEW_RESULTS
/kreview tool is not available in this offline benchmark environment. Manual
self-review: the change preserves all existing invariants — the drop happens only
on the skip (`next`) path where no `tree->lock` is held; `from` already tracks
scan progress so the resumed `xa_find()` neither skips nor revisits incorrectly;
`inode` is not dereferenced after the lock is dropped; behavior is unchanged
functionally (best-effort shrinker), only the max lock-hold is bounded. Patch
compiles cleanly (see BUILD_TEST_RESULTS).

### BUILD_TEST_RESULTS
Guest kernel built from the crash-kernel source tree (v6.16-fbk2) with
defconfig + kvm_guest.config + btrfs/memcg/softlockup/paravirt-spinlocks:
- Baseline (unpatched): bzImage built OK (14,914,560 bytes).
- Patched: incremental rebuild OK (exit 0) — the fix compiles with no warnings.
Both boot in QEMU 8.1.2 (-enable-kvm). See AB_VERIFICATION for the run.

### REPRODUCER
Approach: recreate the crash's ingredients in a QEMU/KVM guest — many cached btrfs
inodes with EMPTY extent trees (so the shrinker scans past them under xa_lock), a
few with extent maps (so the shrinker keeps running), concurrent inode create+evict
(the contenders), memory pressure (drives reclaim -> em shrinker + prune_icache
eviction), the unfair spinlock (`nopvspin`), and `softlockup_panic=1`. The monitor
times the EXACT crashing operation: `openat(O_CREAT)` on the subvolume
(= btrfs_create_new_inode -> xa_store).

Artifacts (in reproducer/):
- `repro_init.c`  — static-ish /init: mounts btrfs (compress=zstd), forks 24
  populate workers (1M files, 7/8 empty), 48 create+unlink stress workers, 8
  memory-hog workers, and a monitor probing openat latency.
- `build_repro.py` — creates a 12 GiB btrfs.img (mkfs.btrfs), compiles /init,
  bundles loader+libc into an initramfs cpio.
- `run_qemu.py`   — boots a given bzImage with the cpio + btrfs.img, captures
  serial, enforces a wall timeout.
- `qemu_baseline.log`, `qemu_fixed.log` — the two A/B runs.

Invocation (both arms identical except the kernel image):
`qemu-system-x86_64 -enable-kvm -cpu host -smp 32 -m 8192 -kernel <bzImage>
 -initrd initramfs.cpio -drive file=btrfs.img,if=virtio -nographic -no-reboot
 -append "console=ttyS0 rdinit=/init nopvspin softlockup_panic=1 watchdog_thresh=10
 panic=1 repro.files=1000000 repro.secs=100 repro.memhogs=8 repro.stress=48"`

Both kernels built from the crash source tree (v6.16-fbk2) with
defconfig+kvm_guest.config+repro fragment (btrfs=y, MEMCG, SOFTLOCKUP_DETECTOR,
PARAVIRT_SPINLOCKS).

### AB_VERIFICATION
Same guest (32 vCPU, 8 GB, nopvspin, softlockup_panic=1), same workload (1M files
7/8 empty; 48 create/evict workers; 8 memhogs); only the kernel differs. Metric =
max latency of openat(O_CREAT) on the contended subvolume (the crashing op) and
create throughput during the shrinker/reclaim window (t=32-46 s).

- Baseline (no fix): bug REPRODUCED. openat max latency 0.225 s; during the window
  create throughput collapsed ~10x (~18k creates in the 14 s window; +2-3k/2s vs
  ~19k/2s before); total creates 768,832. (qemu_baseline.log)
- With fix: bug GONE. openat max latency 0.053 s and FLAT across the whole run —
  no spike in the same window; throughput stayed steady (~138k creates in the same
  14 s window; ~20k/2s); total creates 941,184 (+22% work done). (qemu_fixed.log)
- Verdict: FIX VERIFIED (contention mechanism reproduced and removed). Latency of
  the crashing operation dropped 4.2x and the throughput collapse was eliminated
  under identical memory pressure. The full 22 s soft-lockup panic was not
  reproduced at this scale (32 vCPUs vs 192 in prod; inodes partly evicted by the
  memhogs shorten scans), so the fatal endpoint is inferred, not reproduced.
- Iterations: 1 A/B cycle (baseline + fixed) at the reported scale, plus an
  earlier small-scale smoke boot confirming the pipeline.

### RECOMMENDATIONS
1. Apply the patch to bound the extent-map shrinker's `root->inodes.xa_lock` hold
   (kernel_fix.patch). This attacks the originating defect.
2. Enable paravirt (PV) qspinlocks for these KVM guests (ensure the host exposes
   KVM_FEATURE_PV_UNHALT and the guest is not booted with `nopvspin`). Fair
   PV locks with holder-kick eliminate the unfair-TAS starvation that makes this
   (and any other hot-lock contention) fatal — complementary mitigation for T2.
3. Review the memcg `memory.max` sizing / reclaim behavior for the affected
   workload (chef/mcrcfg); the cgroup-local reclaim storm (T3) is what exposes the
   defect. Consider raising limits or smoothing allocation bursts.
4. Consider upstreaming the fix (Cc btrfs maintainers / Filipe Manana, Josef
   Bacik); the async em-shrinker worker is upstream and has the same unbounded hold.
5. Longer term, evaluate sharding or lockless iteration for `root->inodes` to
   reduce baseline contention on very large, active subvolumes.
