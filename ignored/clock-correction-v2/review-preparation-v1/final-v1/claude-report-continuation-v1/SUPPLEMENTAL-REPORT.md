# Supplemental Report — reverie PR 588

**Target:** `https://github.com/rrnewton/reverie/pull/588`, base `b5e2ab49cd99e5d456fa0238b8cebd75958c529f` → head `a9449f8ee22a17ba87a106c2af957a938fa57c45`, tree `da3e0a5c13bbd1b95adca1bc3ac0037b89494544`. No source, test, assertion or comparator changed since my first report.

## 1. Correction to the first report's reading claim

My first report opened with a statement that I had completed the mandatory reading. **That statement was false.** The same report's §9 disclosed that I had read only 8 of 24 `entry-replay-raw-events` chunks, and the terminal audit of that invocation **failed at 224/240 mandatory chunks**. I do not claim the first invocation had complete coverage, and I do not claim it followed strict index order.

The repair is a *distinct* set, not a retroactive fix: **original 224/240, supplemental 16/16.** The supplemental invocation read all sixteen missing chunks and the trigger-rule excerpt completely, but **it too ended in failure** — the guard expired at 600.055 s before any verdict was issued. Both the original failed audit and the supplemental timeout stand as failures. The union of reading is now complete (240/240 plus the rule excerpt); reading is evidence of custody, not a verdict, and it does not convert either failed run into a successful one.

## 2. Effect of the newly read records

The sixteen chunks **corroborate and strengthen** my prior inference. Nothing is refuted; two conclusions become stronger, and I withdraw one hedge.

**(a) Perf attribute split, now proven at the kernel ABI, not from Rust source.** Two guest-targeted `perf_event_open` calls on tid 2578706 with identical config `0x5100d1`: seq 84 (sampling counter, `sample_period` 1152921504606846976) carries attr bytes `65 00 10 00`; seq 88/90 (the `clock_owner`, fd 12, `sample_period` 0) carries `65 10 10 00`. The single differing bit is `enable_on_exec`, set only on the counting clock. Both are `pinned=1, disabled=1, exclude_kernel=1, exclude_hv=1, exclude_guest=1`. This is direct byte evidence for the launcher-interval exclusion contract.

**(b) The causal arithmetic is now exhaustive over the trace, not inferred.** My first report said per-step attribution was inferred because no intermediate sample exists. I withdraw that hedge for both measured intervals, because every guest resumption between the clock reads is now enumerable:

- Reads: seq 202 → `raw_count 0`; seq 308 → `1`; seq 378 → `3`; seq 414 → `67`.
- Interval 0→1: seq 216 step (rip stays 0x401000 at seq 222 — no instruction retired); seq 236 step executes the *patched* `syscall` written at seq 228/230 (`0xCCCCCCCCCCCC050F`), rip 0x401000→0x401002 at seq 243, **0 RCB**; seq 266 step runs the restored `JNE +0` (seq 250/252 rewrote `0x0F00000027B80075`) under `orig_rax = −1` set at seq 262, **1 RCB**; seq 284 step is the private-page `syscall` at 0x71000000, **0 RCB**. Predicted Δ = 1; observed Δ = 1.
- Interval 1→3: seq 318 step, `orig_rax = −1` at seq 314, restored `JNE +0`, **1 RCB**; seq 335 private-page `syscall`, **0 RCB**; seq 352 the genuine guest `JNE +0` (confirmed by seq 359: rip 0x401002, `orig_rax` reads −1 = not-in-syscall), **1 RCB**; seq 362 `PTRACE_CONT` runs `MOV EAX,39; syscall` to the seccomp stop at rip 0x401009, **0 RCB**. Predicted Δ = 2; observed Δ = 2.

This closes the loop on the preserved counters exactly: the two spurious retirements at seq 266 and seq 318 are precisely the difference between the observed **`[3, 67]`** and the unchanged required **`[1, 65]`**. The required pair was in place *before* the fix and was *failing* — the test was not weakened to accommodate the new behaviour.

**(c) A second, independent reachability instance for A1's neighbourhood.** Chunk 4 shows the same no-pending-original `skip_seccomp_syscall` misuse **pre-exec on the launcher**: seq 156 sets `orig_rax = −1` at a signal-delivery stop (rip …d38c, `orig_rax` 234 = tgkill), steps at 160, traps at 164, restores at 166. My first report could not cite this. It does not change A1's severity — it raises confidence that the defect class is real and not a single post-exec accident.

**(d) Trace terminates cleanly.** seq 438 `PTRACE_EVENT_EXIT`, seq 442 `CLD_EXITED` status 0, then `munmap(0x7f9967b35000, 4096)` and `close(12)`. No leaked counter or mapping.

**A1–A5:** all confirmed, none upgraded. A1 (returning tail-inject with no pending original, `task.rs:5581-5590`, no `set_ret`, not `cancellable()`) remains LOW and unexercised. A2 (`task.rs:2932-2933`), A3 (`timer.rs:907-913`), A4 (`phase.py:112-114`), A5 (`fs/exec.c:1264-1265`) are unchanged.

## 3. Trigger classification — my own reassessment

My earlier trigger-2 reasoning rested on public shape and unchanged `Tool`/`Guest` files. That reasoning **omitted the rule's "or the syscall-interception model" clause**, which is not a file-identity test. Re-reading the actual text (Hermit `AGENTS.md` 452–460), the clause asks whether the change alters *how syscalls are intercepted and mediated*, independent of whether a public trait signature moved.

Against that clause, the change does qualify. `pending_syscall` becomes `Option<(Sysno, SyscallArgs)>` and *taking it* is now the authority to run `skip_seccomp_syscall`; `inner_inject` and `inner_tail_inject` replace a two-way `==` with a three-way match (`Some(o) if o == (nr,args)` / `Some(_)` / `None`). That is a change to the ownership rule governing when the tracer may consume a seccomp entry — the interception model itself — and the raw trace shows exactly the behaviour it governs (the `orig_rax = −1` steps at seq 262 and 314 that the old model permitted with no pending original). **My conclusion: trigger 2 is TRIGGERED.** I was wrong to classify it as not triggered; the correction is mine, from the rule text.

**Trigger 3 remains TRIGGERED** — the initial-command clock origin is a new determinization strategy, and the PR is already labelled for it. Trigger 2 adds no new label requirement beyond the same `post-facto-human-review`, but the *basis* should be recorded as two triggers, not one.

## 4. Verdict

**APPROVE** the whole change at base `b5e2ab49` → head `a9449f8e`, tree `da3e0a5c`, with one non-blocking correction: the review trigger basis is **triggers 2 and 3**, not trigger 3 alone.

No goalpost moving found. The `[1,65]` assertion predates the fix and was failing; the lib inventory moves 187 → 194 = exactly the seven new `injection_stop_tests` declarations; `execute.py:23`/`:48-50` forbid the legacy skip; `phase.py:153-154` forbids ignored-test credit; `retain_artifacts.py:26,32` require `fresh: false` and a differing ELF hash. Nothing was widened, exempted, relabelled or deleted.

**Material limits.** Qualification is **45 phases attempted, 44 accepted, 37 declarations qualified**. The unchanged legacy-vsyscall declaration had payload raw 0 and real assertions, but its structured-reader receipt remains `accepted=false`; it is not qualified, and all 45 phases were not accepted. The compile raw 101 predecessor, the raw 125 diagnostic refusal, the precise-timer parent's two internally captured child modes (not inflated), and every unmeasured state stand as recorded. `has_precise_ip()` is false on this AMD 9D85 host, so the skid-margin path is unmeasured here. This approval covers this Reverie change only: **no Hermit integration, no backend parity, no whole-scheduler approval, no activation.** Two of my own invocations on this target ended in failure (audit 224/240; 600.055 s timeout); this verdict rests on the completed reading union and my own source analysis, not on either run succeeding.