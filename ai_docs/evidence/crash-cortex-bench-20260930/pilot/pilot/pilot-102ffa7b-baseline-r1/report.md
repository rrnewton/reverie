### SEVERITY
critical

Criteria: NULL-pointer-dereference oops (machine panics; Kdump loaded) in the network
softirq RX path, reproduced on **282 hosts** across multiple SKUs
(CRATERLAKE_SPR / DELTALAKE_CPL), hitting a **production kTLS workload**
(WhatsApp `userdb`). Fleet-wide, no config workaround, fix not yet in the running kernel.

### SUBSYSTEM
net (net/tls — kTLS software receive strparser)

### ROOT_CAUSE_CATEGORY
kernel_bug

### CONFIDENCE
high (for root cause and fix selection) — with the empirical A/B caveat below

Root-cause confidence is HIGH on independent, converging evidence: (a) instruction-
level decode of the oops (`cmp %edx,0x28(%rax)`, RAX=0 → `TCP_SKB_CB(NULL)->seq`,
CR2=0x28 = offsetof(sk_buff,cb)); (b) the exact upstream fix `4ab26bce3969` carries
a byte-for-byte matching backtrace and a `Fixes:` tag pointing at `0d87bbd39d7f`,
the commit that introduced the crashing function; (c) git proof the buggy code is
present in fbk10 and the fix is absent (it landed in fbk13). The patch is the
maintainer's own upstream commit and was **built AND booted** (both baseline and
patched kernels compile and boot under QEMU/KVM).

An **independent adversarial verifier** (Phase 4 / kverify) returned **AGREES
(~90%)**: it re-derived `offsetof(sk_buff,cb)=0x28` and `seq@0`, confirmed RAX=0 →
`skb->next==NULL` → crash is *inside* the loop (line 445, not the line-435
`first`-load), confirmed reachability via the `tls_strp_read_copy` early return
(`:396`) + the drgn frame at `:529` (reached with `full_len` pre-set, no reload),
excluded the frag_list-NULL / check_queue_ok-logic / hardware alternatives, and
confirmed the patch fixes the root cause (not a symptom). Its only caveat — the
exact skb-freeing agent (tcp_collapse vs reader vs coalesce) isn't pinned by the
dump — is already captured by T2/T3 and does not affect the fix.

CAVEAT (do not read as "fix empirically proven"): the QEMU **A/B did not reproduce
the crash** — the reproducer is INADEQUATE on fbk10 because loopback TCP coalescing
turns a single ≤16 KB record into one skb, so the multi-skb-with-freed-head
precondition never forms (see AB_VERIFICATION). The fix-works claim therefore rests
on it being the maintainer's root-cause commit, not on an observed A/B flip.

### PATCH_REQUIRED
yes

### NVIDIA_RELATED
no

### DUPLICATE_OF
unchecked

### SUMMARY
`tls_strp_read_sock()` (net/tls/tls_strp.c) only refreshes the strparser anchor's
cached copy of the TCP receive queue (`tls_strp_load_anchor_with_queue()`) **inside**
the `if (!strp->stm.full_len)` branch. When a TLS record header was parsed on an earlier
`->data_ready` call but the full record had not yet arrived (so `stm.full_len` is already
set), a later call with `inq >= full_len` skips the refresh and passes a **stale
`anchor->frag_list`** (and stale `stm.offset`) to `tls_strp_check_queue_ok()`. If TCP has
since coalesced/collapsed and freed the head skb of that cached snapshot, the loop
`while (skb->len < len) { skb = skb->next; TCP_SKB_CB(skb)->seq ... }` walks off a
freed/short chain and dereferences a NULL `skb` at `TCP_SKB_CB(NULL)->seq` (offset
`0x28`) — the panic. **Originating defect:** the missing unconditional queue reload in
`tls_strp_read_sock()`. **Fatal mechanism:** NULL deref in `tls_strp_check_queue_ok()`
(the frame the backtrace ends in). Upstream fix: `4ab26bce3969` / fbk `c76f6f437c46`
("tls: always refresh the queue when reading sock").

### DETAILED_ANALYSIS

**Crash site & instruction decode.**
Panic: `BUG: kernel NULL pointer dereference, address: 0000000000000028`,
`RIP: tls_strp_check_rcv+0x109/0x230 [tls]`, CPU 21, `Comm: swapper/21`, in softirq.
`tls_strp_check_queue_ok()` and `tls_strp_read_sock()` are inlined into
`tls_strp_check_rcv`. The faulting instruction (from the oops `Code:`) is
`39 50 28` = `cmp %edx,0x28(%rax)` with `RAX=0`. That is
`TCP_SKB_CB(skb)->seq` where `TCP_SKB_CB(skb) == &skb->cb[0]`; `offsetof(struct
sk_buff, cb)=40=0x28` and `seq` is the first member of `struct tcp_skb_cb`, so the read
targets virtual address `0x28` — precisely CR2. The immediately preceding
`48 8b 00` (`mov (%rax),%rax`) is `skb = skb->next` (`next` at offset 0), which returned
NULL. Registers corroborate the drgn frame: `RBX=strp=0xffff88873b5a9b58`,
`RDX=seq=0x09bead0a`, `RDI=sk=0xffff8882d01c6e00`, `R14=inq=0x4eca`, `R13=ctx`.

**Stack walk (bottom-up).**
bnxt NAPI poll → `napi_complete_done` → `ipv6_list_rcv` → `tcp_v6_rcv` →
`tcp_rcv_established` → `tcp_data_queue` → `tcp_data_ready` → `tls_data_ready`
(net/tls/tls_sw.c:2437) → `tls_strp_check_rcv` (tls_strp.c:543) →
`tls_strp_read_sock` (:529) → `tls_strp_check_queue_ok` (:445, crash). This is the
normal kTLS software-strparser path invoked from `sk->sk_data_ready` while the socket is
processed under `bh_lock_sock` in softirq.

**The defect (net/tls/tls_strp.c:500-536, HEAD = fbk10).**
```
static int tls_strp_read_sock(struct tls_strparser *strp) {
    inq = tcp_inq(strp->sk);
    if (inq < 1) return 0;
    if (unlikely(strp->copy_mode)) return tls_strp_read_copyin(strp);
    if (inq < strp->stm.full_len) return tls_strp_read_copy(strp, true);   // (A)
    if (!strp->stm.full_len) {
        tls_strp_load_anchor_with_queue(strp, inq);                        // (B) refresh
        sz = tls_rx_msg_size(strp, strp->anchor); ...
        strp->stm.full_len = sz;
        if (!strp->stm.full_len || inq < strp->stm.full_len)
            return tls_strp_read_copy(strp, true);                         // (C)
    }
    if (!tls_strp_check_queue_ok(strp))                                    // (D) uses frag_list
        return tls_strp_read_copy(strp, false);
    WRITE_ONCE(strp->msg_ready, 1); tls_rx_msg_ready(strp); return 0;
}
```
The refresh at (B) — which sets `skb_shinfo(anchor)->frag_list` from
`tcp_recv_skb(sk, copied_seq, &offset)` and sets `stm.offset` — runs ONLY when
`stm.full_len == 0`. `tls_strp_check_queue_ok()` at (D) consumes that `frag_list`/`offset`.

**How the state becomes stale (timeline).**
1. Call N: `full_len==0`. (B) caches `frag_list = first_A`, `stm.offset`, computes and
   sets `full_len = sz`. Record not fully present → `inq < full_len` at (C) →
   `tls_strp_read_copy(strp, true)` → `if (qshort && !tcp_epollin_ready(sk, INT_MAX)) return 0;`.
   With target `INT_MAX`, `tcp_epollin_ready` is false, so it returns 0 **without**
   entering copy_mode and **without** resetting `frag_list`. State persists:
   `frag_list=first_A`, `full_len=sz`, `copy_mode=0`.
2. Between calls, TCP **frees/replaces** the cached head `first_A`. The head is
   freed (not merely grown) by `tcp_collapse()` via `tcp_prune_queue()` under
   receive-memory pressure — it rebuilds the queue into fresh skbs and frees the
   originals — and, upstream, by the mid-2025 net-next change making TCP "compact
   skbs much more aggressively" (the change that unearthed this bug; NOT in fbk10).
   `tcp_try_coalesce()` alone does NOT free the head (it merges later data *into*
   the head), which is why a single coalesced record does not trip the bug — see
   AB_VERIFICATION. Subsequent partial-record `->data_ready` calls hit branch (A)
   (`inq < full_len`) and early-return at tls_strp.c:396 **before** `frag_list` is
   reset at :399, so they never refresh `frag_list` — `first_A` stays cached while
   it is freed underneath.
3. Call N+k: enough data has arrived, `inq >= full_len`. `full_len != 0`, so the
   `if (!full_len)` block (and its refresh at B) is skipped. (D)
   `tls_strp_check_queue_ok()` walks the **stale** `first_A` chain; the freed/short chain
   yields `skb->next == NULL` before `len` bytes are accumulated →
   `TCP_SKB_CB(NULL)->seq` → oops.

Upstream saw the identical stack as a KASAN **slab-use-after-free** (reading the freed
old skb); on this production kernel (no KASAN) the freed memory is reused and the stale
`->next` reads as NULL, producing the NULL-deref instead. Same bug, same code, different
downstream manifestation.

**Theory table.**
| # | Theory | Status | Evidence |
|---|--------|--------|----------|
| T1 | Stale `anchor->frag_list` because `tls_strp_load_anchor_with_queue()` is only called inside `if(!stm.full_len)` | **CONFIRMED (root cause)** | Instruction/register decode; upstream `4ab26bce3969` Fixes 0d87bbd39d7f with identical backtrace; buggy code in HEAD, fix absent |
| T2 | Trigger = TCP skb collapse/coalesce + memory pressure while a record is partially received | SUPPORTING | meminfo pressure (2.3 GB free/93.8 GB, 5.7 GB Dirty); heavy kTLS (WhatsApp userdb); concurrent splice reader + tcp_write_timer on same sk |
| T3 | `6db015fc4b5d` "data disappearing from under the TLS ULP" | RELATED, not this path | Fixes a sibling freed-anchor case via the `WARN_ON_ONCE(!first)` early return; our backtrace is the T1 path. Recommended companion backport |
| — | Hardware fault | REJECTED | No MCE/ECC/AER; deterministic deref at fixed struct offset; 282 hosts/multi-SKU |
| — | Bug inside `tls_strp_check_queue_ok` itself | REJECTED | Loop is correct given a fresh queue; bounding it would hide corruption (symptom fix) |

**Cross-CPU corroboration.** The same socket `sk=0xffff8882d01c6e00` is contended:
CPU 0 `tcp_write_timer` and CPU 11 `tls_sw_splice_read → lock_sock` both spin on its lock
`0xffff8882d01c6f40`, while CPU 21 runs the strparser in softirq (`bh_lock_sock`). Several
CPUs (30/32/35/51/53) are stuck in `csd_lock_wait` targeting CPU 21 — the cascade after
CPU 21 oopsed in softirq with IRQs effectively stalled. This confirms the crash is a
single-socket software fault, not hardware.

### AFFECTED_FUNCTIONS
tls_strp_check_queue_ok, tls_strp_read_sock, tls_strp_check_rcv, tls_strp_load_anchor_with_queue, tls_data_ready, tcp_data_queue, tcp_rcv_established

### UPSTREAM_PATCH_STATUS
backport_needed: 4ab26bce3969 (fbk cherry-pick c76f6f437c46)

Candidate fixes enumerated:
1. **`4ab26bce3969` "tls: always refresh the queue when reading sock"** (Jakub Kicinski),
   `Fixes: 0d87bbd39d7f` (the commit that introduced `tls_strp_check_queue_ok`).
   Classification: **root-cause** — makes `tls_strp_read_sock` always reload the queue
   before checking it. fbk backport `c76f6f437c46` is present on `release-v6.11-fbk13`
   but is **NOT an ancestor of the crash HEAD** (v6.11-fbk10). **SELECTED.**
2. `6db015fc4b5d` "tls: handle data disappearing from under the TLS ULP"
   (`Fixes: 84c61fe1a75b`). Classification: related **root-cause for a sibling path**
   (external reader consuming the queue / `WARN_ON_ONCE(!first)`), also absent from
   fbk10. Recommended companion backport but does not itself cover the T1 backtrace.

The introducing commit `0d87bbd39d7f` was confirmed to be an ancestor of HEAD
(`git merge-base --is-ancestor` = true); fix (1) and (2) confirmed NOT ancestors.

### PATCH
Backport of `4ab26bce3969`; saved at
`/tmp/kinv-logs/102ffa7baec3aca1d13d02224ea9403c/kernel_fix.patch`. Applies cleanly to
the crash worktree (`git apply --check` OK).

```diff
--- a/net/tls/tls_strp.c
+++ b/net/tls/tls_strp.c
@@ -511,9 +511,8 @@ static int tls_strp_read_sock(struct tls_strparser *strp)
 	if (inq < strp->stm.full_len)
 		return tls_strp_read_copy(strp, true);

+	tls_strp_load_anchor_with_queue(strp, inq);
 	if (!strp->stm.full_len) {
-		tls_strp_load_anchor_with_queue(strp, inq);
-
 		sz = tls_rx_msg_size(strp, strp->anchor);
 		if (sz < 0) {
 			tls_strp_abort_strp(strp, sz);
```

### KREVIEW_RESULTS
See `/tmp/kinv-logs/102ffa7baec3aca1d13d02224ea9403c/kreview_results.txt`.
Summary: the patch is a faithful, minimal backport of the maintainer-authored upstream
fix; it only relocates an existing call (no new allocations, no locking change — still
runs under the lower-socket lock as the comment on `tls_strp_read_sock` requires), so it
introduces no new failure modes. It addresses the originating defect (unconditional queue
reload), not the fatal mechanism.

### BUILD_TEST_RESULTS
Built from the crash worktree (v6.11-fbk10) with a local `make O=... defconfig +
kvm_guest.config` config (CONFIG_TLS=y, AES-GCM/GHASH, BPF_SYSCALL for
TASKS_TRACE_RCU, WERROR off, SWAP off — all to build the fbk source with gcc).
- **Baseline (unpatched):** builds clean, boots under `qemu-system-x86_64
  -enable-kvm`, reaches userspace, runs the reproducer. bzImage saved as
  `build/bzImage.baseline`.
- **Fixed (kernel_fix.patch applied):** `git apply` clean; incremental rebuild
  succeeds (tls_strp.o recompiled, vmlinux relinked, exit 0); boots under QEMU and
  runs the reproducer cleanly (clean `reboot: Power down`). bzImage saved as
  `build/bzImage.fixed`. Worktree reverted to clean afterwards.
Both kernels build and boot; the fix compiles and boots with no regression on the
normal kTLS receive path.

### REPRODUCER
Approach: the crash is in the TLS **strparser**, which parses only the 5-byte TLS
record header *before* decryption, so a plain-TCP client sending a crafted
(unencrypted) TLS application_data record to a **kTLS-RX** server that never reads
is enough to drive `tls_strp_read_sock` → `tls_strp_check_queue_ok`. The server
installs `TCP_ULP=tls` + `TLS_RX`; the client sends a record header advertising a
16000-byte body, delivers ~6 KB (partial → `full_len` set, `inq < full_len` early
return caches `frag_list`), pauses to let `tcp_collapse` free the cached head skb,
then completes the record (`inq >= full_len` → stale-queue check). Levers: tiny
`SO_RCVBUF`, tiny `TCP_MAXSEG`, low `net.ipv4.tcp_mem`, and boot `init_on_free=1`
so a freed skb zeroes (`skb->next == NULL` → the exact `0x28` deref). Built as a
static **nolibc** PID-1 init (no libc/initramfs-lib dependencies).
Source + artifacts in `/tmp/kinv-logs/102ffa7baec3aca1d13d02224ea9403c/reproducer/`
(`repro_nolibc.c`, `init`, `initramfs.cpio`, `build_kernel.sh`).

### AB_VERIFICATION
- **Baseline (no fix):** bug **NOT reproduced** — kernel ran the full kTLS RX
  strparser path (`kTLS RX installed` → partial record → completed record) and
  exited cleanly (`REPRO_DONE_NO_CRASH`).
- **With fix:** kernel boots and runs the same path cleanly (as expected).
- **Verdict:** **REPRODUCER INADEQUATE** (could not trigger the race in QEMU).
- **Iterations:** 1 A/B cycle (baseline + fixed), plus reproducer-design analysis.

Why the race did not fire on this kernel: the bug needs the strparser to walk a
**multi-skb** anchor chain whose **head skb was freed** between two data_ready
calls. Over loopback on fbk10, a single ≤16 KB TLS record is **coalesced by
`tcp_try_coalesce` into one growing head skb**, so (a) the `while (skb->len < len)`
loop body — where the crash lives — is never entered, and (b) `tcp_collapse` has no
multi-skb chain whose head it must free. The upstream report notes this bug was
only *unearthed* by a mid-2025 net-next change that makes "TCP compact skbs much
more aggressively" — that change is **not in fbk10**; in production the trigger was
genuine fleet **memory pressure** (host had 2.3 GB free / 93.8 GB, 5.7 GB Dirty,
heavy WhatsApp kTLS) driving `tcp_prune_queue`/`tcp_collapse` across many
records/sockets. Neither condition is reproducible deterministically from a single
loopback record in the QEMU budget. The root cause therefore rests on the
instruction-level decode + the exact upstream `Fixes:`-tagged commit + git
presence/absence proof, not on the (inconclusive) QEMU A/B.

### RECOMMENDATIONS
1. **Backport `4ab26bce3969`** ("tls: always refresh the queue when reading sock") to
   6.11-fbk (the fbk cherry-pick is `c76f6f437c46`, already on fbk13) as the fix for this
   MID. Patch provided.
2. **Also backport `6db015fc4b5d`** ("tls: handle data disappearing from under the TLS
   ULP") as a companion hardening for the sibling freed-anchor path.
3. Roll the hotfix/livepatch to the 282 affected hosts; the running kernel already
   carries a livepatch (`klp_6_11_1_0_fbk10_hotfix1`), so a livepatch of this 3-line
   change is feasible for immediate mitigation.
4. Prefer the root-cause fix over any loop-hardening of `tls_strp_check_queue_ok`;
   bounding the walk would hide (not prevent) the stale-queue corruption.
