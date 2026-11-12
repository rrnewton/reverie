[adversarial-reviewer agent, OPUS] [claude-coord-fable, devbig030]

**CHANGES-REQUESTED-AT `96306604d7808389e89f2ce567c43d4402d29cf0`**

This is a fresh, independent review derived from source at this exact head (rebased onto `reverie/main` `349460925ee56f2aca686a3392b534e8861ba375`). It does not rely on, defer to, or inherit anything from the voided verdicts at `f21205ff`, `a6aa8bc4`, or `f178de87`. I enumerated the call graph myself before reading any prior comment; the one blocking finding below has not been raised by any earlier round.

For the record on the patch itself: `git diff <old-base>...a6aa8bc` and `git diff 3494609...9630660` are **byte-identical** (empty interdiff), so the change under review is exactly the one previously validated — only the base moved (951 files, none in `reverie-kvm/`; one in `reverie/`: `src/backend.rs`).

---

## BLOCKING — interception is propagated to a vCPU that has no dispatcher for it

`ThreadOwnership::Host` CLONE_THREAD workers inherit `CR4.TSD` but run a loop that cannot service the resulting fault. Their first CPL3 `RDTSC` is fatal.

Exact chain, all at this head:

1. `runtime.rs:1180` — `set_rdtsc_interception(subscriptions.has_rdtsc())` sets `CR4.TSD` on the root vCPU when the Tool subscribes.
2. `vm.rs:1175` — `ProcessAction::Thread { .. } if self.thread_ownership.executes_on_host() => self.run_process_action(...)` delegates the host-owned worker to the **tool-less** action handler.
3. `vm.rs:980` — that handler passes `self.intercept_rdtsc` (== `true`) into `from_thread_state`, which at `vm.rs:769` calls `child.set_rdtsc_interception(true)` → **`CR4.TSD` is set on the child vCPU**.
4. `vm.rs:1011` — the child is spawned running `run_static_elf_process` (the tool-less loop at `vm.rs:1473`).
5. `vm.rs:1516-1521` — that loop's `VcpuExit::Hlt` arm calls **only** `try_resume_vmware_backdoor_probe()`, then `static_elf_halt_error()`. It never calls `timestamp_counter_exception()`. `timestamp_counter_exception` has exactly one caller in the tree (`runtime.rs:1421`), and it is not on this path.

Result: `RDTSC` in that worker takes `#GP(0)`, is not recognised, and becomes `Error::GuestException { vector: 13 }` (`vm.rs:1443-1449`), which `vm.rs:1016-1020` reduces to an `eprintln!` while the worker thread dies. Before this PR the same instruction executed natively.

This is the mirror image of the bug the last round caught. That one was *consumer missing at the dispatch site*; this one is *state set on a vCPU with no dispatch site at all*. Either reading of `ThreadOwnership::Host` gives a correct fix, and the current code gives neither:

* If Host-owned means "uninstrumented, native execution" (which is what `reverie/src/tool.rs:70-86` says — "the Tool never sees the thread's syscalls"), then pass `false` at `vm.rs:980` when the tool path delegates, so the worker keeps native `RDTSC`.
* If Host-owned workers should still route timestamps, add the recognizer to the `vm.rs:1516` arm.

What it is **not**: this is not reachable from Hermit today. `hermit-cli/src/lib.rs:1333-1335` guards `backend.unmonitored_threads()` on `!config.backend_dispatches_thread_tools`, and `prepare_backend_config` hard-wires that to `true` (`hermit-cli/src/lib.rs:1523`) with `#[clap(skip = true)]` (`detcore-model/src/config.rs:107-109`), so the branch is currently dead. It **is** reachable through the public `reverie-kvm` API — `KvmBackend::unmonitored_threads()` (`vm.rs:551`), `set_thread_ownership(Host)` (`vm.rs:529`), or `Tool::thread_ownership -> Host` — and `reverie-kvm`'s own suite exercises that configuration deliberately (`tests/static_elf.rs:1493`, `WorkerDispatch::Tool(ThreadOwnership::Host)`). It escapes CI only because `StraceTool` takes the `Tool::subscriptions` default `Subscription::all_syscalls()` (`reverie/src/tool.rs:232-234`), which carries no `Instructions::RDTSC`, and because that test's hand-written guest has no `RDTSC`. A one-line fix plus a `Host` + `rdtsc()` case in `timestamp_dispatch_survives_thread_fork_and_exec_vcpu_lifecycles` closes it.

---

## 1. Subscription gating, exhaustively — otherwise correct

`timestamp_counter_exception` has one definition (`vm.rs:1355`) and one caller (`runtime.rs:1421`). I enumerated all eight `vcpu.run()` loops and every `VcpuExit::Hlt` arm:

| site | runs CPL3 guest? | can `CR4.TSD` be set? | dispatches timestamps? |
|---|---|---|---|
| `runtime.rs:1420` (static-ELF + Tool) | yes | yes | **yes**, guarded |
| `vm.rs:1516` (static-ELF, tool-less) | yes | **yes, via `vm.rs:1175` delegation** | **no** ← blocking finding |
| `runtime.rs:1116` (`run_with_tool`, vmcall) | yes | no (`set_rdtsc_interception` never called on this entry point) | no — see finding B |
| `vm.rs:1663` (`run`, vmcall demo) | yes | no | no |
| `vm.rs:826 / 949 / 1038 / 1213` | no — park/trampoline `Hlt => Ok(())` | n/a | n/a |

The guard itself is correctly placed: `vm.rs:1357-1359` returns `None` **before** `static_elf_exception()` and before any byte decoding, so an unsubscribed guest's genuine fault is never even inspected. That satisfies the documented contract at `reverie/src/tool.rs:362` ("never called by default unless rdtsc events are subscribed") at the one site that dispatches.

Storage and default: `intercept_rdtsc: false` at construction (`vm.rs:485`); the only mutator is `set_rdtsc_interception` (`vm.rs:565-569`), which read-modify-writes `CR4` (`bootstrap.rs:253-262`) rather than clobbering sregs.

## 2. Unsubscribed guests keep native behavior — yes, and the negative discriminates

`static_elf_unsubscribed_rdtscp_remains_guest_exception` (`tests/static_elf.rs`) is the discriminating negative and I verified it by construction: with `subscribed=false`, `CR4.TSD` is clear, `RDTSCP` still `#UD`s because `DETERMINISTIC_EXTENDED_CPUIDS[1][3] == 0x2010_0800` and `0x2010_0800 & bit(27) == 0` (`cpuid.rs:233`, ratcheted by the new assertion at `cpuid.rs:329`). Delete `vm.rs:1357-1359` and the recognizer matches vector 6, decodes `0f 01 f9`, dispatches to the Tool, the run returns `Ok`, and the test's `unwrap_err()` panics. It fires; it is not inert.

Bracket, as counted:

* **Positive (4):** `static_elf_timestamp_reads_dispatch_exact_tool_results_repeatably` (2 fresh runs, exact `EDX:EAX` + `ECX` sentinels, exactly `[Tsc, Tscp]`); `repeated_timestamp_reads_evolve_once_per_instruction` (2 fresh runs, exactly `[Tsc, Tsc, Tscp]`); `timestamp_dispatch_survives_thread_fork_and_exec_vcpu_lifecycles` (real glibc C program, ≥3 distinct senders, ≥2 from the fork child spanning `execl`); `two_byte_rdtsc_decode_does_not_read_a_third_byte` (`vm.rs:1789`, unit).
* **Negative (4):** unsubscribed `RDTSCP` → `#UD` (**discriminates the guard**); `subscribed_timestamp_dispatch_refuses_unrelated_exceptions` — `0f 0b` UD2 → `#UD` refused and `0xed` → `#GP` refused (**discriminates the decoder**); the `read_rdtscp_suffix() == None` arm of the unit test; the `cpuid.rs:329` ratchet that keeps the `#UD` premise true.
* **Inert, stated not blocking:** `static_elf_unsubscribed_rdtsc_runs_without_tool_dispatch` does **not** discriminate `vm.rs:1357-1359`. With `TSD` clear, `RDTSC` never faults, so the guard is never consulted — delete the guard and this test still passes. It is a real test of the `CR4` gate, just not of the guard; the PR body should not count it toward the guard's bracket.

## 3. Continuous virtual time — not blunted

`vm.rs:1390-1408`: `rax = tsc & 0xffff_ffff`, `rdx = tsc >> 32` — full 64 bits delivered through the architectural split with the upper halves implicitly zeroed, as the ISA requires. `rcx = aux` is written **only** for `Tscp` (`vm.rs:1394-1396`), so plain `RDTSC` correctly leaves `RCX` alone. RIP advances by `RDTSC.len()` = 2 and by a literal `3` for `RDTSCP` — both correct for `0f 31` and `0f 01 f9`. `rsp`/`rflags` are restored from the exception frame, and `RDTSC`/`RDTSCP` do not architecturally touch flags, so restoring the pre-fault value is right.

No value is clamped, masked, re-derived, cached, or coalesced: `git grep -i '_rdtsc\|Instant::now\|SystemTime' -- reverie-kvm/src/` returns zero host time reads. Strict progression is proven by `repeated_timestamp_reads_evolve_once_per_instruction`, which asserts the guest observes `base`, `base+1`, `base+2` with `aux == AUX+2` and exactly three dispatches — that simultaneously proves one dispatch per instruction (no re-fault, no double-count) and correct RIP advance for both encodings.

Frame decoding checks out: `static_elf_exception` (`vm.rs:1331-1349`) reads word 0 = RIP, 2 = RFLAGS, 3 = RSP past an error code when `exception_pushes_error_code` says so, and `bootstrap.rs` lists `13` (yes) but not `6` (no) — correct for both vectors. Because the CPL3→CPL0 transition pushes onto TSS.RSP0, the guest red zone is untouched.

GPR preservation across the handler: I chased the injection path and it is safe. `StaticElfSyscallExecutor::execute` (`runtime.rs:209-222`) services injected syscalls **host-side** through `ElfExecutor`, never by re-entering the vCPU, so live guest GPRs survive. The one path that does re-enter is `complete_injection` → `run_process_action_with_tool`, and under `ProcessExecutionContext::Lifecycle` (`runtime.rs:1435`) that is restricted to `Exec` with fork/clone rejected (`runtime.rs:292-300`); the dispatch site then refuses the whole event via `handler_process_completed` **before** calling `resume_timestamp_counter`, and refuses `HandlerOutcome::TailInjected` as well. `hide_tool_scratch`'s side effect is taken before the early `?` returns and only its error is deferred — deliberate and correct.

## 4. Determinism — no leakage found

No host TSC path remains for a subscribed guest: `CR4.TSD` faults CPL3 `RDTSC`, and `RDTSCP` faults as `#UD` under `CpuidPolicy::deterministic()` (the `KvmBackend::new` default) or as `#GP` under `host_supported()` — both caught by `matches!(exception.vector, 6 | 13)`. `RDPMC` still `#GP`s (`CR4.PCE` unset) and is correctly not emulated. `aux.unwrap_or(0)` is a deterministic constant, not an uninitialised read; nothing reads IA32_TSC_AUX host-side. `CR4.TSD` survives `execve` because `exec_process` (`vm.rs:774`) routes through `configure_long_mode`, whose `sregs.cr4 |= ...` (`bootstrap.rs:183`) is read-modify-write, and it survives resumption because `configure_user_segments` (`bootstrap.rs:278`) is likewise read-modify-write — I checked both specifically for a full-sregs clobber that would silently disable interception after the first event, and neither does it. The value the guest sees depends only on Tool state.

For completeness: `Tool::handle_rdtsc_event`'s default body is `RdtscResult::new(request)` (`reverie/src/tool.rs:371`), i.e. the raw host TSC. A tool that subscribes without overriding the handler gets host time — but that is the pre-existing cross-backend contract, identical under ptrace, and not this PR's doing.

## 5. Lifecycle propagation — correct for every path that has a dispatcher

Fork: snapshotted at `vm.rs:710`, applied at `vm.rs:730`. Tool-owned thread: `vm.rs:1244` → `vm.rs:769`. Exec: same vCPU, `CR4` preserved as above. Ordering in `from_thread_state` is right — `configure_long_mode_with_syscall_area` first, `set_rdtsc_interception` last (`vm.rs:756-769`) — so nothing overwrites `CR4` afterwards, and the later `set_user_segment_base` calls are read-modify-write. New-task default is the safe `false` (`vm.rs:485`). The live glibc lifecycle test independently confirms root, pthread worker, fork child, and post-exec image all dispatch. The single gap is the Host-owned worker in the blocking finding — where propagation is, ironically, *too* faithful.

## 6. Core-abstraction line

**DOES-NOT-CROSS.**

The diff is confined to `reverie-kvm/{bootstrap,cpuid,runtime,vm}.rs` + `reverie-kvm/tests/static_elf.rs` (`git diff --stat 3494609...9630660`). `reverie/src/tool.rs`, `guest.rs`, `backend.rs`, `subscription.rs`, and `rdtsc.rs` are untouched. No public trait requirement, method signature, associated type, or default was added or altered; `Tool::handle_rdtsc_event`, `Subscription::has_rdtsc`, `Rdtsc`, and `RdtscResult` are all **consumed** exactly as defined. The only visibility change is private → `pub(crate)` on `StaticElfException` and its fields (`vm.rs:101-106`); `timestamp_counter_exception`, `resume_timestamp_counter`, `set_rdtsc_interception`, and `set_userspace_rdtsc_interception` are all `pub(crate)`. `reverie-kvm`'s public surface is unchanged. Guest register semantics change only *inside* the KVM backend and only for a subscribed tool, which is the fix, not a contract change. Trigger 3 (new determinization strategy) is the right label and is claimed correctly.

## 7. PR description — sections complete, evidence SHA stale, tags missing

Against `reverie/AGENTS.md:378-382`:

| required | present |
|---|---|
| **Summary** | yes |
| **Determinism** (informal proof, not only tests) | yes — the CR4.TSD/Tool-boundary argument, plus the explicit "no second clock domain" claim |
| **Validation** | yes |
| **Relationship to gVisor** (KVM) | **yes** — present and substantive; not a finding |
| **Human Review Required** naming a numbered trigger | yes — trigger 3, named explicitly |
| **Linux Semantics** | yes |

Non-blocking, but please fix before landing:

* **Validation is bound to `a6aa8bc41683f471aef1ba8cd3a23e9c0016ef3a`, not to this head.** `a6aa8bc` is not an ancestor of `9630660` and has a different tree. I verified the patch is byte-identical, so the risk is low, but the recorded `cargo test -p reverie-kvm` 229/0 was taken on a base 951 files older — including `reverie/src/backend.rs` (+49/-5, a new required `Backend::run_with_stats` + `type Stats`). Evidence binds to commits: re-record at the current head.
* **No commit carries the role + team tag.** `reverie/AGENTS.md:200-211` calls the commit trailer "the load-bearing one" precisely because this repo lands by rebase merge; all three commits (`841107a`, `d65ab38`, `9630660`) end without it, and none carries a `Task:` trailer.
* **The PR body opens `[impl agent, gpt-5.6-sol]`** — missing the `[<team>, <machine>]` bracket the same section requires.
* `841107a`'s body contains a literal `\n\n` escape instead of newlines.
* Very minor: `AUTONOMOUS-BOT-IMPLEMENTED` at `vm.rs:1352` is specified by `reverie/AGENTS.md:308-318` for *new syscall support* and explicitly "not blanket markers for API changes, backend work". This is instruction interception. The file has precedent (PR-172, PR-192, PR-202), so this is drift, not a regression.

## Non-blocking, stated for the record

* **B. `run_with_tool` (`runtime.rs:995`, public) never calls `set_rdtsc_interception`.** A tool subscribing to RDTSC on that entry point silently reads the raw host TSC. This is pre-existing (the hook did not exist before), and that path is the hand-written vmcall demo, so no real guest is affected — but the PR's "route every trapped instruction through the existing `Tool::handle_rdtsc_event` contract" is entry-point-specific. A one-line doc note on `run_with_tool` would prevent a future reader assuming coverage.
* **C. Prefixed encodings are not decoded.** `decode_timestamp_counter_instruction` (`vm.rs:86-97`) reads two bytes at the faulting RIP, so `f3 0f 31` or a segment-overridden form falls through to `GuestException` rather than being emulated — turning a working (if unusual) instruction into a fault *only under subscription*. No compiler emits these; fail-closed is the right default; noting the limitation is enough.

## Summary

The core mechanism is right, and I checked it rather than inheriting it: the guard is correctly placed and provably discriminating, the `EDX:EAX`/`ECX` write-back and both instruction lengths are correct, virtual time is genuinely continuous and Tool-owned with no host leakage, injection cannot corrupt guest GPRs, and nothing in the public Reverie API moved. The one thing blocking is that `CR4.TSD` reaches a vCPU class whose run loop has no recognizer — a one-line fix plus one test case, in the same "state without a consumer" family as the defect the previous round caught, which is why I am not waving it through.
