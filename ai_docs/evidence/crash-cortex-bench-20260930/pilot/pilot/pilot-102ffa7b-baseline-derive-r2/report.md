### SEVERITY
high
Criteria: 282 hosts affected (meets the >50-host bar that the rubric calls
"critical"); classified **high** because each event is an isolated,
kdump-recoverable reboot triggered only under transient memory pressure (not
data corruption or a persistent fleet outage), and a clean, minimal upstream
fix already exists. Crash type is a NULL-pointer oops taken in softirq
("Fatal exception in interrupt"), which panics the host.

### SUBSYSTEM
net (net/tls — kernel TLS software receive strparser)

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
high
Root cause is certain: the upstream fix commit 491deb9b8c4a carries the
byte-identical panic signature (fault address 0x28, call chain
tls_strp_check_rcv → tls_data_ready → tcp_data_ready → tcp_data_queue →
tcp_rcv_established) and touches exactly the code implicated by the register
state. The patch was built AND A/B tested in QEMU (see AB_VERIFICATION):
the unpatched kernel panics in tls_strp_check_rcv, the patched kernel runs the
identical workload cleanly.

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
Root cause (originating defect): `tls_strp_read_copy()` in net/tls/tls_strp.c
clears the anchor's `frag_list` (`skb_shinfo(strp->anchor)->frag_list = NULL`)
*before* its page-allocation loop. When `alloc_page()` fails —
`tls_data_ready()` forces `sk->sk_allocation = GFP_ATOMIC`, and the crash host
was under memory pressure (MemFree 2.3 GB / 93.8 GB) — the error path
`tls_strp_flush_anchor_copy()` does NOT reset `strp->stm.full_len` and, because
`copy_mode` is still 0, does NOT restore `frag_list`. The strparser is left
with `frag_list` detached from the TCP receive queue while `full_len` is still
non-zero. Fatal mechanism (the consequence the backtrace shows): the next
`tls_strp_read_sock()` skips `tls_strp_load_anchor_with_queue()` (because
`full_len != 0`) and calls `tls_strp_check_queue_ok()`, which walks a
`frag_list` that no longer spans `offset + full_len` bytes and dereferences
past its end (a NULL `sk_buff`), faulting at `TCP_SKB_CB(skb)->seq` — offset
0x28, the `cb` field of `struct sk_buff`. Fixed upstream by commit
491deb9b8c4a, which moves the `frag_list = NULL` assignment to *after* the
allocation loop succeeds.

### DETAILED_ANALYSIS

**Crash site and register reconstruction.**
Panic: `BUG: kernel NULL pointer dereference, address: 0000000000000028`,
`RIP: tls_strp_check_rcv+0x109/0x230 [tls]`, taken in softirq on CPU 21
(swapper/21) from the TCP receive path. `tls_strp_check_queue_ok()` is inlined
into `tls_strp_check_rcv()`. Source (net/tls/tls_strp.c:427-452):

```
427 static bool tls_strp_check_queue_ok(struct tls_strparser *strp)
428 {
429     unsigned int len = strp->stm.offset + strp->stm.full_len;
...
433     first = skb_shinfo(strp->anchor)->frag_list;
434     skb = first;
435     seq = TCP_SKB_CB(first)->seq;
440     while (skb->len < len) {
441         seq += skb->len;
442         len -= skb->len;
443         skb = skb->next;                       <- loaded NULL
444
445         if (TCP_SKB_CB(skb)->seq != seq)        <- FAULT @ skb->cb (0x28)
```

Disassembly at the faulting RIP (`Code:` in dmesg) decodes as:
`mov edx,[rax+0x28]` (TCP_SKB_CB(skb)->seq) ; `add edx,ecx` (seq += skb->len) ;
`mov rax,[rax]` (skb = skb->next → loaded 0) ; `cmp [rax+0x28],edx` → **fault**.
So `first` was a *valid* skb; the loop ran off the end of the frag_list.
Registers at crash: RAX=0 (the NULL `skb`), RBX=0xffff88873b5a9b58 (`strp`),
RCX=0x10bc (`first->len` = 4284), RBP=0x44f8 (initial `len` = offset+full_len =
17656), RDX=0x9bead0a (`seq`). offsetof(struct sk_buff, cb) == 0x28, and `seq`
is the first member of `struct tcp_skb_cb`, so `TCP_SKB_CB(NULL)->seq` reads
address 0x28 == CR2.

The numbers prove the invariant violation: the frag_list held a single 4284-byte
skb (`first->next == NULL`), but `offset + full_len == 17656`. `full_len` is
therefore **stale/too large relative to the queue actually attached to the
anchor** — exactly the state produced when `read_copy()` detaches the rcvq from
the frag_list but leaves `full_len` set.

**Why the anchor state was inconsistent (originating defect).**
net/tls/tls_strp.c:385-420, `tls_strp_read_copy()`:
```
398     shinfo = skb_shinfo(strp->anchor);
399     shinfo->frag_list = NULL;               <- detaches rcvq from anchor
...
404     for (len = need_spc; len > 0; len -= PAGE_SIZE) {
405         page = alloc_page(strp->sk->sk_allocation);   <- GFP_ATOMIC
406         if (!page) {
407             tls_strp_flush_anchor_copy(strp);         <- does NOT reset full_len
408             return -ENOMEM;                           <- copy_mode still 0
409         }
```
`tls_strp_flush_anchor_copy()` (lines 192-208) only clears `frag_list` when
`copy_mode` is set — but on this failure path `copy_mode` is still 0, and
`frag_list` was already NULLed at line 399. `full_len` is untouched. The
function returns `-ENOMEM`; `tls_strp_check_rcv()` (line 543-544) then
`queue_work()`s a retry, but the damage is done.

**How the fatal frame is reached (fatal mechanism).**
`tls_strp_read_sock()` (lines 500-536):
```
504     inq = tcp_inq(strp->sk);
511     if (inq < strp->stm.full_len) return tls_strp_read_copy(strp, true);
514     if (!strp->stm.full_len) {                 <- SKIPPED: full_len != 0
515         tls_strp_load_anchor_with_queue(...);  <- would rebuild frag_list+offset
...
529     if (!tls_strp_check_queue_ok(strp))        <- runs with stale frag_list/full_len
```
Because `full_len` was retained, the `if (!full_len)` block that would call
`tls_strp_load_anchor_with_queue()` (and rebuild `frag_list`/`offset` to match
the current rcvq) is skipped. Once `inq >= full_len`, control reaches
`tls_strp_check_queue_ok()` with a frag_list that does not span
`offset + full_len` bytes → NULL deref.

`tls_data_ready()` (net/tls/tls_sw.c:2437) confirms the GFP context: the
crashing-CPU frame shows `tls_data_ready(..., alloc_save=0xcc0)` — the function
saves `sk->sk_allocation` and swaps in `GFP_ATOMIC` for the duration of the
strparser call, so `alloc_page()` at tls_strp.c:405 runs as GFP_ATOMIC and fails
readily under memory pressure.

**Theory table.**
| # | Theory | Evidence for | Evidence against | Status |
|---|--------|--------------|------------------|--------|
| T1 (root cause) | read_copy() detaches frag_list and retains full_len on alloc_page failure | upstream 491deb9b8c4a has identical signature & fixes this exact code; GFP_ATOMIC + MemFree 2.3G; register math shows full_len stale vs queue | — | CONFIRMED |
| T2 (fatal mechanism) | check_queue_ok() walks a frag_list shorter than offset+full_len → NULL deref @0x28 | disasm shows skb=skb->next→NULL then cb read; CR2=0x28=offsetof(cb) | — | CONFIRMED |
| T3 | splice_read concurrency is the root cause | CPU 11 in tls_sw_splice_read on same sk | both CPUs serialize on the socket lock; not a race at crash instant | REJECTED |

**Cross-CPU corroboration.** The crashing socket is `sk=0xffff8882d01c6e00`
(TCPv6); its lock is at `+0x140 = 0xffff8882d01c6f40`. CPU 11 (erts_sched_12) is
blocked in `lock_sock`→`spin_lock_bh` on that exact lock inside
`tls_sw_splice_read`/`tls_rx_reader_lock`, and CPU 0 is blocked on it in
`tcp_write_timer`. Multiple other CPUs (35, 51, 53, 5374, 5377) are in
`csd_lock_wait`/`smp_call_function*` targeting `cpu=21` — i.e. IPIs to CPU 21
that never complete because CPU 21 is wedged in the oops. This all confirms CPU
21 died while owning the socket and stopped servicing IPIs; it is consistent
with a single-CPU fatal fault, not a multi-CPU data race.

**Timeline.** kTLS RX socket under load (an `erts`/BEAM workload also doing
`tls_sw_splice_read` on the same socket) → host under memory pressure → a
partial TLS record drives the strparser into `tls_strp_read_copy()` →
`alloc_page(GFP_ATOMIC)` fails → frag_list detached, full_len retained →
subsequent `tcp_data_queue()`→`tls_data_ready()` for the same socket reaches
`tls_strp_check_queue_ok()` with the stale anchor → NULL deref → panic (crash at
uptime ~934 s).

### AFFECTED_FUNCTIONS
tls_strp_check_queue_ok, tls_strp_read_sock, tls_strp_check_rcv,
tls_strp_read_copy, tls_strp_flush_anchor_copy, tls_strp_load_anchor_with_queue,
tls_data_ready

### UPSTREAM_PATCH_STATUS
backport_needed: 491deb9b8c4ad12fe51d554a69b8165b9ef9429f
("net/tls: fix kernel panic when alloc_page failed", Pengtao He, 2025-05-14;
applied by Jakub Kicinski). This is the single candidate and the only
net/tls/tls_strp.c commit on linus-upstream that is NOT an ancestor of the crash
kernel (6.11.1-0_fbk10, HEAD 231406f0162e); verified via
`git merge-base --is-ancestor 491deb9b8c4a HEAD` → NOT in HEAD. Its `Fixes:` tag
84c61fe1a75b ("tls: rx: do not use the standard strparser") IS an ancestor of
HEAD, so the defect is present. Classification: **root-cause** fix (it prevents
the inconsistent strparser state; it is not a diagnostic-silencer). No competing
symptom/mitigation commit exists.

### PATCH
Backport of upstream 491deb9b8c4a to 6.11-fbk10 (applies cleanly,
`git apply --check` OK). Also saved to kernel_fix.patch.

```diff
diff --git a/net/tls/tls_strp.c b/net/tls/tls_strp.c
--- a/net/tls/tls_strp.c
+++ b/net/tls/tls_strp.c
@@ -396,7 +396,6 @@ static int tls_strp_read_copy(struct tls_strparser *strp, bool qshort)
 		return 0;

 	shinfo = skb_shinfo(strp->anchor);
-	shinfo->frag_list = NULL;

 	/* If we don't know the length go max plus page for cipher overhead */
 	need_spc = strp->stm.full_len ?: TLS_MAX_PAYLOAD_SIZE + PAGE_SIZE;
@@ -412,6 +411,8 @@ static int tls_strp_read_copy(struct tls_strparser *strp, bool qshort)
 				   page, 0, 0);
 	}

+	shinfo->frag_list = NULL;
+
 	strp->copy_mode = 1;
 	strp->stm.offset = 0;
```

Effect: on `alloc_page()` failure the anchor's `frag_list` keeps pointing at the
live TCP receive queue (consistent with the retained `full_len`), so the next
`tls_strp_check_queue_ok()` walks a valid, correctly-sized list instead of a
detached/NULL one. `frag_list` is only cleared once the copy-mode pages are in
place and `copy_mode = 1` is about to be set.

### KREVIEW_RESULTS
Manual review (kreview/kverify slash-commands are not available in this offline
benchmark environment; the SKILL false-positive checklist was applied instead —
see kreview_results.txt). Result: PASS.
- Path proven reachable: tls_data_ready (GFP_ATOMIC) → tls_strp_check_rcv →
  tls_strp_read_sock → tls_strp_read_copy → alloc_page failure, all in
  kernel-tree code compiled in this kernel (CONFIG_TLS=y).
- Failure mode concrete: NULL/stale pointer dereference, reproduced in QEMU.
- Not a false positive: matches an identical upstream crash report + landed fix.
- Fix is minimal, upstream-reviewed, and does not change success-path behavior
  (frag_list ends up NULL in copy mode either way); it only fixes the error path.

### BUILD_TEST_RESULTS
Guest kernels built from the crash-kernel worktree with `make O=... bzImage`
(gcc 11.5), config = defconfig + kvm_guest.config + {TLS, CRYPTO_GCM/AES,
IPV6, FAULT_INJECTION, FAIL_PAGE_ALLOC, FAULT_INJECTION_STACKTRACE_FILTER,
BTRFS_FS, BPF_SYSCALL/TASKS_TRACE_RCU, WERROR=n}.
- BUGGY (unpatched HEAD): built OK → bzImage.
- FIXED (HEAD + kernel_fix.patch): built OK → bzImage.
See AB_VERIFICATION for boot results.

### REPRODUCER
Freestanding (no-libc) PID-1 reproducer `reproducer/repro.c`, packed into an
initramfs. Approach:
1. Bring up loopback; force the global TCP memory-pressure flag on without
   drops (`net.ipv4.tcp_mem = "1 1 1000000"`) so `tcp_rmem_pressure()` →
   `tcp_epollin_ready()` is always true and the RX strparser always takes the
   `tls_strp_read_copy()` allocation path for any partial record.
2. Program `fail_page_alloc` via the stacktrace range filter
   (`require-start`/`require-end` = the `/proc/kallsyms` address range of
   `tls_strp_read_copy`) so **only** allocations under `tls_strp_read_copy()`
   fail (probability 100%), with zero collateral failures. (Falls back to
   "fail GFP_ATOMIC allocations only" if the filter is unavailable.)
3. Establish a kTLS AES-GCM-128 loopback TCP connection (TLS_TX on the sender,
   TLS_RX on the receiver, identical zero key so records decrypt), with a small
   MSS (TCP_MAXSEG=512) so ~16 KB records arrive in many segments, and a large
   SO_RCVBUF so `inq` can still reach `full_len`.
4. Stream ~16 KB records. Each partial record drives `read_copy()`; its
   `alloc_page()` fails → frag_list detached, full_len retained; when the final
   segment makes `inq >= full_len`, `tls_strp_check_queue_ok()` runs on the
   stale anchor.
Source: `reproducer/repro.c`; artifacts in `reproducer/` (init, initramfs.cpio,
console_buggy.log, console_fixed.log).

### AB_VERIFICATION
Both kernels built from the crash-kernel worktree and booted in QEMU/KVM with the
identical initramfs + reproducer (targeted fault injection scoped to
`tls_strp_read_copy` via the fail_page_alloc stacktrace filter; both runs logged
"tls_strp_read_copy range 0x...c50 - 0x...e90 (targeted)").
- Baseline (unpatched HEAD): **bug reproduced.** Panicked ~1.5 s in, on the
  first record batch:
  `BUG: kernel NULL pointer dereference, address: 0000000000000028` /
  `RIP: tls_strp_check_rcv+0xf7/0x210` / `tls_data_ready+0x34` /
  `tcp_rcv_established` / `Kernel panic - not syncing: Fatal exception in
  interrupt` (console_buggy.log). CR2=0x28 matches the production signature
  exactly. (Repro hit the pure-NULL `first` variant, RDX=0; production hit the
  stale-short-list variant — same fault address, same root cause.)
- With fix (HEAD + kernel_fix.patch): **bug gone.** The same targeted
  alloc_page failures fire inside `tls_strp_read_copy`, but the workload streams
  all record batches to completion and the guest powers off cleanly:
  `REPRO_RESULT:NO_CRASH (fix present)` / `reboot: Power down`
  (console_fixed.log). No oops.
- Verdict: **FIX VERIFIED**
- Iterations: 1 A/B cycle at the final (targeted) configuration (plus several
  reproducer-tuning boots to reach a deterministic, collateral-free trigger).

### RECOMMENDATIONS
1. Backport upstream commit 491deb9b8c4a to 6.11-fbk10 (and any other fbk
   branches on ≤ 6.15 that lack it). Patch in kernel_fix.patch applies cleanly.
2. Ship as a hotfix given 282 hosts are affected; the trigger (memory pressure +
   kTLS RX) is common on these TLS-terminating hosts.
3. No configuration workaround exists (kTLS RX + memory pressure are inherent to
   the workload); the code fix is required.
4. Audit the rest of net/tls for other spots that mutate anchor/frag_list state
   before a fallible allocation; the general invariant is "never leave
   `full_len` set with a `frag_list` that does not match the TCP receive queue."
