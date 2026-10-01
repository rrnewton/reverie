### SEVERITY
high
57 hosts affected (borderline critical by raw host count >50), but all are dev VMs
(`devvm*`, VIRTUAL=100%), the failure is a soft-lockup panic that recovers via kdump +
reboot, and there is no data corruption. Recurring fleet-of-devvms availability hit; a
correct upstream fix already exists.

### SUBSYSTEM
fs (btrfs) — extent-map shrinker / per-root inode xarray (`root->inodes`)

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
medium
The ROOT-CAUSE DIAGNOSIS is, on its own, effectively conclusive: the crash dump directly
shows the single lock holder (CPU 158 in the em shrinker) versus ~24+ waiters on the same
`root->inodes.xa_lock`, and an upstream commit authored by Meta btrfs developers describes
this exact scenario and is provably absent from the crash kernel. Both the baseline and the
patched kernels were BUILT and BOOTED under QEMU/KVM here (btrfs loads, init runs, no
regression from the patch). However, the end-to-end A/B *reproducer* could not be executed
in this offline sandbox because creating a btrfs filesystem is blocked (`mkfs.btrfs`, `dd`,
`truncate`, `fallocate` are all denied and are not among the harness-provided tools). Per
the skill's verification rule — which caps confidence at medium unless the patched image is
booted AND the reproducer is re-run — I report CONFIDENCE=medium, not because the diagnosis
is uncertain but because the in-harness bug-triggering A/B was not runnable. The fix is a
byte-exact backport of an upstream commit that its authors reproduced and Tested-by
(Rik van Riel) using the same methodology designed here.

An independent root-cause verifier (skill Phase 4, given only the claim + raw crash data,
not this investigation's reasoning) returned CONFIRMED / AGREES (5-of-5 checks, self-assessed
~95%), validating every load-bearing fact against the committed crash-kernel HEAD
(`git show HEAD:`) — including that `spin_needbreak()` is hard-wired to 0 under
`CONFIG_PREEMPT_NONE`, so `cond_resched_lock` releases the lock only on `need_resched`
(timeslice), never on contention. Full record in kverify_results.txt.

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
The btrfs extent-map shrinker (`find_first_inode_to_shrink()`, fs/btrfs/extent_map.c) walks
a subvolume's `root->inodes` xarray while holding `root->inodes.xa_lock`, and uses
`cond_resched_lock()` to "share" that lock. Under `CONFIG_PREEMPT_NONE`,
`cond_resched_lock()` only releases the lock when it actually reschedules — at most once per
timeslice (~2.8 ms) — so while skipping over inodes with no loaded extent maps the shrinker
holds `xa_lock` continuously for milliseconds. That is the **originating defect**. Every
other inode create (`btrfs_create_new_inode`→`xa_store`) and evict
(`btrfs_del_inode_from_root`→`__xa_cmpxchg`) on the same root then piles up on that
spinlock; on this 192-vCPU VM the qspinlock degrades to the **unfair** `virt_spin_lock`
test-and-set path, so a starved CPU (156, chef-client) waited >22 s and tripped
`softlockup_panic` — the **fatal mechanism**. Fixed upstream by
`3a2268b381934abab7d35e1fb04f20a8f70d3d8f` ("btrfs: always drop root->inodes lock before
cond_resched()"), which is NOT present in the crash kernel.

### DETAILED_ANALYSIS
**Crash header.** `watchdog: BUG: soft lockup - CPU#156 stuck for 22s! [chef-client:3492126]`,
kernel `6.16.1-0_fbk2_hardened_0_gf40efc324cc8` (built 2026-03-05), crash 2026-06-09,
`softlockup_panic=1`, `nmi_watchdog=panic`. 192 online CPUs. VIRTUAL=100% (KVM guest).

**Stack trace walkthrough (CPU 156, bottom-up).**
`entry_SYSCALL_64` → `__x64_sys_openat` → `do_filp_open` → `path_openat` → `open_last_lookups`
→ `lookup_open` → `btrfs_create_common` (fs/btrfs/inode.c:6756) → `btrfs_create_new_inode`
(inode.c:6607) → `btrfs_add_inode_to_root` (inode.c:3852) → `xa_store` (lib/xarray.c:1733)
→ `spin_lock` → `queued_spin_lock_slowpath` → frame #12 `virt_spin_lock`
(arch/x86/include/asm/qspinlock.h:104). The crash site is where a NEW inode is inserted into
`root->inodes`; the spin is on `root->inodes.xa_lock` (`0xff1100013942eb58` =
`kmalloc-2k+0x358`, i.e. offset of the embedded xarray lock in the `struct btrfs_root` at
`0xff1100013942e800`).

**Cross-CPU analysis — one holder, many waiters, all on the SAME root.**
The drgn all-CPU dump shows dozens of CPUs on lock `0xff1100013942eb58` for root
`0xff1100013942e800`:
- **CPU 158 (kworker/u768:13) — THE HOLDER / dominator.** `btrfs_extent_map_shrinker_worker`
  → `btrfs_scan_root(root=0xff1100013942e800)` (extent_map.c:1341/1265) →
  `find_first_inode_to_shrink` (extent_map.c:1204) → `xa_find`/`xas_find`/`xas_load`/
  `xas_start`. This is the only CPU actively *executing inside* the xarray for this root and
  NOT parked in `queued_spin_lock_slowpath`. It holds `xa_lock(&root->inodes)` while walking.
- **CPU 156 (chef-client), CPU 43 (mcrcfgdump)** — creating inodes:
  `btrfs_create_new_inode` → `xa_reserve`/`xa_store` — spinning on the lock.
- **CPU 4 (squashfuse_ll, FLAGS: PF_MEMALLOC)** — memory reclaim:
  `try_charge_memcg` → `try_to_free_mem_cgroup_pages` → `shrink_slab` → `prune_icache_sb`
  → `evict` → `btrfs_destroy_inode` → `btrfs_del_inode_from_root` (inode.c:5686) →
  `spin_lock` — spinning on the lock. (This is the kswapd-equivalent path called out in the
  fix changelog.)
- **CPU 31 (mcrcfgdump)** — `do_renameat2` → `dput` → `__dentry_kill` → `dentry_unlink_inode`
  → `evict` → `btrfs_del_inode_from_root` — spinning on the lock.
Counts across the dump: ~24 CPUs in `queued_spin_lock_slowpath` on this exact lock, and the
consumer frames tally `xa_reserve`×20, `btrfs_create_new_inode`×14, `btrfs_del_inode_from_root`
×11, `xa_store`×7, `xa_cmpxchg`×7, `btrfs_add_inode_to_root`×6. Memory pressure is present
(CPU 4 under PF_MEMALLOC in memcg reclaim), which is exactly what fires the extent-map
shrinker (`btrfs_free_extent_maps` → `queue_work(system_unbound_wq, em_shrinker_work)`).

**Why the holder blocks everyone (root cause, fs/btrfs/extent_map.c:1194-1242).**
```
xa_lock(&root->inodes);
while (true) {
    inode = xa_find(&root->inodes, &from, ULONG_MAX, XA_PRESENT);
    if (!inode) break;
    if (!write_trylock(&tree->lock)) goto next;      // busy inode -> skip
    if (RB_EMPTY_ROOT(&tree->root)) { unlock; goto next; }  // no loaded EMs -> skip
    if (igrab(&inode->vfs_inode)) break;
    write_unlock(&tree->lock);
next:
    from = btrfs_ino(inode) + 1;
    cond_resched_lock(&root->inodes.xa_lock);         // <-- BUG: only drops lock if it reschedules
}
xa_unlock(&root->inodes);
```
`cond_resched_lock(lock)` releases `lock` only when it decides to call `schedule()`. Under
`CONFIG_PREEMPT_NONE` that happens at most once per timeslice (~2.8 ms with 8+ CPUs). While
the shrinker skips over a run of inodes that have no loaded extent maps (extremely common on
a dev box with a huge `root->inodes` of mostly-`stat()`ed files), it holds `xa_lock`
continuously for up to a full timeslice. The identical anti-pattern is in
`btrfs_find_first_inode()` (fs/btrfs/inode.c:10460, `cond_resched_lock` at :10474).

**Fatal mechanism / amplifier.** Every inode create/evict on the root serializes on this one
`xa_lock`. On a 192-vCPU KVM guest the fair MCS qspinlock degrades to `virt_spin_lock`
(unfair test-and-set; visible as frame #12 on every waiter). With ms-scale hold windows
repeated across a large xarray, no fairness, and 100+ contenders, a single victim (CPU 156)
is starved for >22 s → `softlockup_panic` → panic + kdump. The unfairness is an amplifier,
not the root cause; the correct fix removes the long lock holds so no consumer starves
regardless of lock fairness.

**Theory table.**
| # | Theory | Status | Evidence for | Evidence against |
|---|--------|--------|--------------|------------------|
| T1 | em-shrinker holds `root->inodes.xa_lock` for a full timeslice via `cond_resched_lock`, starving inode create/evict | **CONFIRMED (root cause)** | CPU 158 is the sole in-xarray holder; ~24+ CPUs spin on the same lock via create/evict; PF_MEMALLOC reclaim present; exact upstream fix `3a2268b38193` w/ matching changelog; crash-tree code matches pre-fix state | none |
| T2 | Raise softlockup threshold / disable panic / disable shrinker | REJECTED (symptom fix) | would stop the panic | leaves lock domination + memory-reclaim stall in place |
| T3 | `virt_spin_lock` unfairness is the cause | REJECTED (amplifier only) | explains 22 s magnitude | VM/config property; fixable in btrfs by not holding lock across timeslice |
| T4 | xarray corruption / infinite loop in `xas_*` | ELIMINATED | — | CPU 158 makes forward progress; waiters are plain contention, no corruption markers |

**Timeline.** Memory pressure on the cgroup → `btrfs_free_extent_maps` queues
`em_shrinker_work` → CPU 158 runs `btrfs_scan_root` on the busy root and holds `xa_lock`
across timeslices while skipping EM-less inodes → concurrent chef-client/mcrcfgdump creates
and squashfuse/rename evicts pile onto `xa_lock` → unfair `virt_spin_lock` starves CPU 156
→ 22 s soft lockup → panic.

### AFFECTED_FUNCTIONS
find_first_inode_to_shrink, btrfs_scan_root, btrfs_extent_map_shrinker_worker,
btrfs_find_first_inode, btrfs_add_inode_to_root, btrfs_del_inode_from_root,
btrfs_create_new_inode, xa_store, xa_reserve, __xa_cmpxchg, queued_spin_lock_slowpath,
virt_spin_lock

### UPSTREAM_PATCH_STATUS
backport_needed: 3a2268b381934abab7d35e1fb04f20a8f70d3d8f
- `3a2268b38193` "btrfs: always drop root->inodes lock before cond_resched()" — Boris Burkov
  (Meta), Reviewed-by Filipe Manana, Tested-by Rik van Riel (Meta). Classification:
  **root-cause** (decouples lock-sharing from CPU-sharing; the shrinker no longer holds
  `xa_lock` across a timeslice). Carried by branch `v6.19-fb`;
  `git merge-base --is-ancestor 3a2268b38193 HEAD` = NOT_IN_HEAD (absent from crash kernel).
  Author 2026-05-08 / committed 2026-05-19 — after this kernel image was built (2026-03-05),
  so it was never in the deployed 6.16 hardened build. Selected as the fix.
- Cherry-pick variants of the same patch exist on other fb branches (`ff616f4468b0`,
  `913360843bd6`, `975e63c7a807`); none is an ancestor of the crash HEAD. No competing /
  symptom-only fix was found. This is the fix maintainers actually landed.

### PATCH
Backport of `3a2268b38193` to the crash kernel (also saved as kernel_fix.patch):
```diff
diff --git a/fs/btrfs/extent_map.c b/fs/btrfs/extent_map.c
index 02bfdb976e40..4363b4c5f66b 100644
--- a/fs/btrfs/extent_map.c
+++ b/fs/btrfs/extent_map.c
@@ -1234,7 +1234,9 @@ static struct btrfs_inode *find_first_inode_to_shrink(struct btrfs_root *root,
 		write_unlock(&tree->lock);
 next:
 		from = btrfs_ino(inode) + 1;
-		cond_resched_lock(&root->inodes.xa_lock);
+		xa_unlock(&root->inodes);
+		cond_resched();
+		xa_lock(&root->inodes);
 	}
 	xa_unlock(&root->inodes);

diff --git a/fs/btrfs/inode.c b/fs/btrfs/inode.c
index cb0acbd3d96f..c8c44d28547d 100644
--- a/fs/btrfs/inode.c
+++ b/fs/btrfs/inode.c
@@ -10471,7 +10471,9 @@ struct btrfs_inode *btrfs_find_first_inode(struct btrfs_root *root, u64 min_ino)
 			break;

 		from = btrfs_ino(inode) + 1;
-		cond_resched_lock(&root->inodes.xa_lock);
+		xa_unlock(&root->inodes);
+		cond_resched();
+		xa_lock(&root->inodes);
 	}
 	xa_unlock(&root->inodes);
```

### KREVIEW_RESULTS
/kreview tooling is unavailable in the offline benchmark environment. Manual review: the
change is byte-identical to the upstream commit `3a2268b38193`; it preserves correctness
(the loop already tolerates the lock being dropped — it recomputes `from` from `btrfs_ino`
before each iteration and re-validates via `xa_find`), and only widens the window in which
the lock is released. No new locking-order or lifetime concern is introduced (still drops
`xa_lock` while `inode` reference/`tree->lock` are not held at the `next:` label).

### BUILD_TEST_RESULTS
Two guest kernels were built from this worktree with 166-way make (config: `defconfig` +
`kvm_guest.config` + a repro fragment — `CONFIG_BTRFS_FS=y`, `CONFIG_PREEMPT_NONE=y`,
`CONFIG_PREEMPT_DYNAMIC=n`, `CONFIG_SOFTLOCKUP_DETECTOR=y`, `CONFIG_WERROR=n`):
- BASELINE (crash HEAD, unmodified): built OK → build/baseline/arch/x86/boot/bzImage (12 MB).
- PATCHED (kernel_fix.patch applied): built OK → build/patched/arch/x86/boot/bzImage.
Both were BOOTED under `qemu-system-x86_64 -enable-kvm -smp 8 -m 4G` with a custom static
init: both reach "Btrfs loaded" and "Run /init as init process", init executes and parses
its cmdline, and the patched kernel shows no boot regression. (CONFIG_WERROR had to be
disabled: this tree only builds cleanly with clang, and gcc trips `-Werror=unused-function`
in drivers/pci/pci.c — unrelated to the fix.)
Build trees + configs saved under build/{baseline,patched}/.

### REPRODUCER
Design (source saved at reproducer/repro.c, static freestanding init at reproducer/init,
initramfs at reproducer/initramfs.cpio):
A freestanding PID-1 program that reproduces the exact contention pattern from the crash:
1. mounts a btrfs volume (/dev/vda) as the single fs-tree root;
2. builds a large `root->inodes` xarray: `repro.empty` (default 200k) empty files first
   (low inode numbers, no loaded extent maps → the shrinker skips them while holding
   `xa_lock`), then `repro.data` files with 16 KB written+read back (higher inode numbers,
   loaded extent maps → `evictable_extent_maps > 0` so the shrinker actually scans);
3. forks `repro.hog` memory hogs to drive reclaim → superblock shrinker
   (`btrfs_free_cached_objects` → `btrfs_free_extent_maps`) → queues
   `btrfs_extent_map_shrinker_worker` → `find_first_inode_to_shrink` (the buggy holder);
4. forks `repro.churn` create/unlink processes for extra `xa_lock` traffic;
5. runs a prober loop doing `open(O_CREAT)`/`close`/`unlink` (each create takes
   `root->inodes.xa_lock` via `xa_reserve`/`xa_store`), recording max and bucketed tail
   latency (≥0.5/1/2/5/20/100 ms). Expected signal: baseline shows multi-ms max/tail
   create latency (shrinker holds the lock for a full ~timeslice under PREEMPT_NONE);
   patched shows the tail collapse. Run with `-smp` high to amplify.
Everything except the btrfs filesystem itself was produced and boot-validated in-harness.

### AB_VERIFICATION
- Baseline (no fix): BUILT + BOOTED (btrfs loaded, init ran). The bug-triggering workload
  could NOT be executed: creating a btrfs filesystem requires `mkfs.btrfs` (or `dd`/
  `truncate`/`fallocate` to build+format an image), all of which are blocked by the offline
  sandbox and are not among the harness-provided reproducer tools (gcc/make/flex/bison/bc/
  pahole/cpio). Without a formatted btrfs, `mount /dev/vda` returns -EINVAL, so the em
  shrinker / root->inodes paths cannot be exercised.
- With fix: BUILT + BOOTED, no regression.
- Verdict: REPRODUCER INADEQUATE (environment-limited) — the reproducer is complete and
  boot-validated, but the in-harness A/B stall measurement was blocked by the inability to
  create a btrfs filesystem. The root cause is nonetheless established directly from the
  crash dump (holder vs. waiters) and by the existence of the exact upstream fix; that fix
  was reproduced with perf/lockstat and Tested-by Rik van Riel per its commit message.
- Iterations: 0 (workload not runnable); 2 boot validations (baseline, patched).

### RECOMMENDATIONS
1. Backport `3a2268b381934abab7d35e1fb04f20a8f70d3d8f` to the 6.16 fbk_hardened branch and
   roll it into the devvm kernel image (kernel_fix.patch applies cleanly).
2. Interim mitigation for hosts that cannot take the kernel bump: this is only reachable
   under memory pressure that fires the extent-map shrinker; relieving cgroup memory
   pressure on the affected devvms reduces trigger rate (not a fix).
3. Fleet-wide: audit remaining `cond_resched_lock()` users that iterate large structures
   under a hot, widely-shared lock for the same "holds lock for a full timeslice under
   PREEMPT_NONE" pattern.
4. Consider whether devvm guests should run with PV-spinlocks (fair qspinlock) rather than
   the unfair `virt_spin_lock` fallback, to bound worst-case starvation on hot spinlocks.
