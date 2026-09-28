# Initial clock and injection-stop correction

This is an unexecuted authored successor. It starts from exact Reverie `3d4a401ed8959befad0b2f10db59093725f27432`, tree `73024912837cbbeaa70b6d2173f5b596eebedcb3`, then applies the preserved original clock-origin patch. That commit's whole tree is also the landed `b5e2ab49cd99e5d456fa0238b8cebd75958c529f` tree. All materialization uses read-only Git objects. Live L HEAD, index and tracked source, prior C source, and every prior experiment remain unchanged.

`SOURCE.patch` is the complete six-path change against that base. `INJECTION-CORRECTION.patch` is the complete four-path successor delta after applying C's unchanged clock patch. The latter changes only `task.rs` in production; the other three paths are test code/registration. `SOURCE-MANIFEST.json` binds the full source copy including the exact previously qualified ignored Cargo.lock. All 28 Cargo manifests/toolchain files match C, and timestamp, CPUID and capture production remain byte-identical to the chosen base. Gitlinks remain recorded and unexpanded, as in C's ptrace source carrier.

The measured defect was not a clock constant. With no original syscall pending, `private_inject` still executed the seccomp-skip step and restored the saved RIP. The retained V2 observation authenticates entry `JNE +0` bytes, successful step/trap/restore pairs and task-bound raw RCB reads 0, 1, 3, 67. Mprotect setup and Tool Getpid each replayed entry before normal guest execution. The original test stays raw101 with actual `[3,67]` versus `[1,65]`; V2's wrapper remains raw125 because mapped perf-page reads failed. Neither outcome is relabelled here.

The successor takes `pending_syscall` once and distinguishes same, other and absent original calls in both ordinary and tail injection. Only a present, different real original call goes through the seccomp-skip helper. With no original entry to consume it executes directly in the existing private syscall page. Same-call reinjection, fast-tail transfer, already-converted stops, injected frames and the separately observed mapping-tail path keep their existing machinery.

The ordinary callback epilogue now takes its remaining pending record before suppressing an emulated original. This prevents that consumed record surviving into a later signal, timer or instruction callback. The fixed legacy-vsyscall return still belongs to the kernel and does not use the generic step. Successful exec clears pending, converted and frame state unconditionally, including without LiteInst configured. No new stop enum, public API, clock offset, post-exec counter reset, rounding or comparator change was added.

## Exact oracle correction

The old authored `post_exec=1` assertion described its value as legitimate first-entry work. The measured RIP was still the original entry after backend execute/rewind. The successor requires **post_exec exactly zero**, the actual entry RIP, and zero again after the returned Getpid injection. It continues to require actual guest boundary counts **1 and 65**. The pre-loop fixture still requires **65 and 129**.

The post-exec `Rcbs(1)` timer now requires **exactly one**, replacing the old authored two. `set_timer_precise` maps this to `Precise(1)`, whose target is `read_clock() + 1`. With zero-before-entry, its target is the actual first guest branch. This is a changed causal oracle, not a tolerance or subtraction from results. Original tests and both old values remain unchanged in C and in the local `before` snapshot. `ORACLE-CHANGES.patch` exposes every changed assertion. The new oracles have not run.

## Controls and limits

The new module has **seven declarations and eight modes**. All use real freestanding x86-64 guests and the existing five-second wait deadline. No mode returns early for missing capability. Each mode emits a completion marker only after exact exit, full stdout/stderr and the entire observation sequence pass.

| Control | Required actual outcome |
| --- | --- |
| Post-exec repeated and failed injection | Entry RIP and memory word remain unchanged through two Getpid injections and an EBADF Close; ordinary guest entry increments the word exactly once. |
| Genuine seccomp replacement | Guest's original stdout `O` is suppressed; replacement `R` appears once; the next guest increment does not run during injection. |
| Same reinjection then private operations | Original `O` then replacement `R`, each once; a subsequent EBADF call does not advance guest RIP or memory. |
| Same-call fast tail | Original `O` executes once after callback cancellation/resume, followed by one next instruction. |
| Different-call tail | Only replacement `R`, correct resumed guest memory and exact successful completion. |
| No-original post-exec tail Exit | Exact exit27 and stdout `R`; a private shared mapping preserves the entry word after process exit, where it must still be zero. This closes the blind spot of checking only private memory before a nonreturning call. |
| Emulated success/error then RDTSC callback | Both modes preserve exact emulated return 123/−EPERM in guest memory. Getppid injection at the real later instruction trap leaves RIP/memory untouched; the eventual guest increment happens once. This exercises stale pending state without host signal-arrival timing. |

The Write fixtures explicitly check the three meaningful Write arguments, not all six raw syscall argument registers. The shared-mapping tail control uses the typed six-argument Mmap path and authenticates its returned address, shared file effect and data; it is not a complete register-by-register six-argument oracle. Existing selected mapping/frame/provenance neighbors retain their distinct unit or runtime scope. In particular, the finite selection does not establish every positive activated LiteInst frame, converted mapping tail, interrupted private syscall/restart, or cross-architecture path. Those branches remain unchanged in source and require honest independent assessment; a nearby rejection test is not their positive runtime proof.

The concrete qualification has **45 planned observed phases: metadata, fresh compile, format, two inventories, core/ptrace check, strict Clippy, and 38 exact declarations (37 library / one integration)**. It retains all nine prior selections, adds the seven new declarations and relevant existing injection/exec/cancellation/mapping/frame/legacy neighbors. The legacy test's existing early return is not credited: the caller requires the real vsyscall mapping before launch and refuses the skip marker afterward. Both fresh ELFs and emitted local libraries must be retained immediately after compile; no failed predecessor ELF can be reused. These are plans, not measured passes.

`AUTHOR-FORMAT*.json` records three bounded author rustfmt operations separately. They are not compiler, test or qualification results. The source snapshot, helper bodies, finite argv/limits, old failures and retained failed ELF are bound for review. No metadata, compiler, test, guest or lease admission has been launched for this successor.

## Evidence and review

No original real branch-count assertion was widened. No case, comparison or check was deleted. Old failures remain failures. The two authored temporal oracles changed explicitly with stronger entry-RIP/memory evidence, while actual guest trajectories remain exact. The source needs independent review and actual qualification; this author report grants neither.

For post-facto review: there is no new syscall support, public API shape or Detcore scheduling code. The combined candidate includes C's initial-command clock eligibility/held-request policy, so the new clock strategy is the concrete trigger-3 concern for review; the additional injection repair restores the existing private-injection precondition rather than introducing a new interception abstraction. Root and independent reviewers retain exact-source classification and landing authority. No PR, publication, source landing or whole Hermit/KVM parity claim is made.
