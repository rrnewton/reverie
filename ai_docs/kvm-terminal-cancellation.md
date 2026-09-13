# Terminal cancellation from KVM Tool callbacks

This component adds `Guest::cancel_current_thread(&mut self) -> Never` so a Tool can complete an already terminal thread without injecting a syscall into a context that cannot resume one. It is separate from the pipe FIONREAD commit `c8852338edfe71e1282fa9a50d47875d8a256684`, which remains its parent. Both sit above the preserved local vector/signal composition `89a9f0217b2f3ab6106aac12ea2a650eee7972d6`. Public sources for https://github.com/rrnewton/reverie/pull/538 and https://github.com/rrnewton/reverie/pull/552 remain unchanged. No Hermit configuration field or scheduling code changes in this component.

The motivation is the actual Hermit `InboundSignal` failure retained in `/tmp/astra-hermit-ready-signal-exit-vectored/accounting.json`. Hermit `7b9748c638deacfd9f3e13ba4640bf6ec38c92d2` with the earlier Reverie composition and local cancellation setting returned status 37 while the KVM worker reported ENOSYS and omitted its consuming exit hook. Native and ptrace returned 37; ptrace recorded both hooks. Matching the process status did not establish cleanup. The unchanged strict full/partial vector tests and their earlier deadline failures remain separate evidence; this backend component does not turn those failures into passes.

## Runtime behavior

The new Guest operation has no syscall or status argument. Its ordinary result is current-thread status zero, matching the previous Hermit tail-injected `Exit::default()`. A backend exit already established before the operation retains its status. The default implementation uses that existing tail path for other backends; `IntoGuest` explicitly forwards so stacked Tools reach the backend override.

KVM publishes `HandlerSignal::ThreadCancelled`, and `drive_handler` returns a distinct `HandlerOutcome::ThreadCancelled` after the suspended callback yields. It does not execute a syscall, call injection completion, write a syscall result, or poll the callback again. The callback borrow is released and scratch hidden before its owning caller consumes ThreadState. Signal helpers distinguish terminal cancellation from both a selected signal and no signal, so cancellation cannot look like suppression and continue the 64-event filtering loop.

The ELF finalizer preserves a previously established exit or creates status zero, retires the exact task generation through the existing `take_exit`, releases the transport slot, consumes and clears registered CHILD_CLEARTID, and invokes the consuming Tool exit callback. It returns immediately afterward. Successful pending child actions are started before ordinary parent-thread termination; failed boundary actions keep their existing cancellation path. The same typed outcome is handled in thread-start, initial-exec, post-exec, ordinary syscall, ordinary signal-return, rt_sigreturn, first-instruction signal, and page-zero fault callbacks. The direct hypercall runtime handles both its start and syscall callbacks as well.

Both existing injection guards remain unchanged. `SignalBoundary`, `FaultBoundary`, and `ThreadEntrySignal` still refuse arbitrary tail injection; ordinary nonreturning injection retains its restrictions. The operation adds no general exception to those guards and does not convert ENOSYS or another runtime error to a successful exit.

`GuestThreadGroup::join_workers` now joins every batch, including workers registered by a worker in an earlier batch. It records every inner worker error and panic, indexed by guest TID, and retains them across intermediate joins. Final owner cleanup runs the owner's consuming hooks and then reports all retained worker failures in guest-TID order. Plain root termination and the existing exec teardown also check this retained result. A forced worker hook failure remains an error even when all other threads and the process hook complete. Existing first-error behavior in the separate failed/unstarted-action cleanup helper is not redesigned by this component.

## Determinism and Linux scope

The operation makes no scheduling decision. In the motivating Hermit caller, the existing scheduler has already committed logical removal and returned the permanent `ThreadExited` result. This component adds no scheduler request, run-queue ordering, retry exemption, virtual-time adjustment, host-readiness selection, or signal recipient selection. The terminal callback makes no further guest progress. Error aggregation orders diagnostics by guest TID rather than selecting a host completion order.

Ordinary nonleader cancellation leaves live peers and their shared process Tool state intact. Tests require a live peer to finish and a later thread to execute after the cancelled worker. Process Tool consumption still follows all joined worker callbacks. The existing KVM leader/process boundary is preserved explicitly: KVM currently treats leader exit as process completion, so Linux leader-SYS_exit while siblings survive remains unsupported. This component does not claim to fix that limitation or the separately retained fork-from-worker wait ownership limitation. Existing exit_group behavior remains covered by the unchanged suite.

Linux v6.17 `kernel/exit.c` distinguishes current-task exit from group exit; the reviewed source is retained at `/tmp/astra-hermit-ready-linux-v6.17-exit.c`. This supports separate current-thread completion, but is not itself evidence that Tool callbacks or KVM lifecycle cleanup ran. The actual callback controls below provide that evidence. There is no change to signal masks, disposition, SA_RESTART, partial I/O results, or general asynchronous signal capability.

## Controls and preserved failures

Five new Rust tests retain three unit controls and two required-KVM controls. The ELF test runs 13 modes under the existing 30-second outer process bound with a two-second kill grace. The guest retains a 15-second alarm; a five-second diagnostic wait checks that a named consuming hook completed before libc joins that thread. The latter keeps `pthread_join` from overwriting the cleared TID word with -1 before the hook observes it; it does not replace the exact zero assertion. The direct hypercall test has the same outer bound and two modes. Compilation uses the existing `/usr/bin/gcc -O2 -pthread <source> -o <executable>` helper. These are actual backend Tool executions, not Hermit strict replay measurements or a native implementation of the new Tool API.

The ELF modes require:

| Mode | Actual callback or behavior | Required result |
| --- | --- | --- |
| 0 | Root thread start | One start, one consuming thread hook, one process hook; no guest instruction |
| 1 | Initial exec callback | Same lifecycle; no guest instruction |
| 2 | Initial post-exec callback | Same lifecycle after the real post-exec callback |
| 3 | Worker thread start | Cancelled child enters no guest code; live peer and replacement complete |
| 4 | Worker syscall callback | No instruction after the selected syscall |
| 5 | Ordinary signal boundary | Exact getpid origin; no delivered guest handler or continuation |
| 6 | Pending signal after rt_sigreturn | First handler runs once; exact rt_sigreturn origin for the second callback; no second handler or resumed continuation |
| 7 | Raw-clone first-instruction signal | Tool admission precedes signal hook; orig_rax is the entry sentinel; child entry marker remains zero |
| 8 | Page-zero fault | Exact SIGSEGV/address-zero context; no fault handler or fault resumption |
| 9 | Successful clone followed by parent-worker cancellation | Completed child start survives; all five threads get unique consuming hooks |
| 10 | Two forced worker exit-hook EIO errors | All four hooks and consuming process hook run; final process result is Err containing both failures in TID order |
| 12 | Post-exec after an actual successful exec | Both post-exec callbacks observed; replacement guest executes no instruction |
| 13 | Nearby ordinary signal delivery | Normal handler runs once, target returns its exact pointer, peers and replacement finish |

Every worker mode checks zero CHILD_CLEARTID at the consuming hook. All normal modes require exact stdout and empty guest stderr. Per-process Tool counters require one consuming hook per unique started TID and one final consuming process hook. The unchanged transport-slot test also requires reuse before the exit callback returns. The unit controls require no executor or injection-completion call, explicit IntoGuest forwarding, successful pending-gate release only by the owning caller, exact identity retirement with safe TID reuse, preserved existing thread/group status, all worker batches joined, all errors retained, and stable TID ordering. Existing failed-boundary child-start refusal and restricted-injection controls are retained unchanged.

Development failures remain under `/tmp/astra-reverie-terminal-controls/` and are not qualifying passes. `live-second` exposed a fixture that tracked clone but omitted clone3; the final fixture accounts for both actual creation paths. `live-third` stopped at its unchanged 30-second bound after its entry assertion observed orig_rax 14: glibc had blocked signals during pthread creation, so the event reached rt_sigprocmask. The final entry case uses a raw clone and retains the sentinel and first-write assertions. `live-fifth` observed a libc TID word of -1 after pthread_join; the final observation synchronizes before joining and retains exact zero. Earlier compilation failures are retained in the same directory or the named `/tmp/astra-reverie-terminal-unit-*.log` and `-check*.log` files. An initial mutation-driver source-selection assertion failed before any mutation ran; the corrected driver then ran all six controls.

Six defect-restoring controls change production source only and leave the tests unchanged. Every run returns test status 101 with an actual assertion failure; none is counted as a qualifying timeout or compilation failure:

| Restored defect | Failure | Wall time including build |
| --- | --- | --- |
| New operation falls back to tail injection | Actual signal callback reports ENOSYS; named consuming hook is missing | 10.741 s |
| Skip worker notify during terminal cleanup | Named consuming hook is missing | 10.465 s |
| Omit IntoGuest forwarding | Adapted terminal outcome assertion fails | 2.439 s |
| Discard inner worker errors on join | Both forced hook errors incorrectly produce success, rejected by final Err assertion | 6.347 s |
| Drop successful pending clone start | Created child's consuming hook never completes | 10.978 s |
| Omit identity retirement | Live-target probe returns 0 instead of ESRCH | 2.488 s |

`/tmp/astra-reverie-terminal-mutations.py` restores every touched production file in a finally block. Exact patches, mutated sources, source hashes, binary hashes, command records and stderr/stdout are retained in `/tmp/astra-reverie-terminal-controls/mutation-results.json` and its companion files. The missing-worker-notify patch is suitable for ready's separately copied Hermit composition diagnostic. No mutation remains in the candidate.

## Review and remaining qualification

The applicable policy is `/home/newton/work/dev-hermit/hermit/.claude/skills/post-facto-review/SKILL.md`, numbered trigger 2: a Reverie Guest/core API abstraction change. Independent Codex and Claude exact-head review is required before public landing. This routes post-facto human review and is not a human approval gate. The pipe FIONREAD component was independently approved separately; that does not approve this component or qualify a Hermit composition.

Relationship to gVisor: this is a Reverie Tool control operation and KVM lifecycle repair. It neither imports gVisor code nor claims a gVisor-style general signal, task, or syscall implementation. The API closes the measured gap between Hermit's terminal scheduler response and the existing consuming Tool cleanup boundary.

Full final checks, exact copied binary identities, test inventory preservation, source hashes and the immutable review diff are recorded with this component's verified TaskGraph note and `/tmp/astra-reverie-terminal-controls/final-artifacts.json`. Final composed Hermit full/partial strict tests and the status-37 lifecycle diagnostic still require their separate authorized paired run. No public PR head, capability flag, assertion, comparator, or original deadline is changed to claim that work completed.

Final measured checks: `cargo test -p reverie-core -p reverie-kvm --all-features --all-targets` passed 652 tests, zero failures and zero ignored, in 14.783 seconds including build: 18 core tests and 634 KVM tests (374 library, 246 static-ELF, 14 other integration tests). `REVERIE_REQUIRE_KVM=1` is recorded in the driver environment. Clippy for those packages with all features/targets and warnings denied passed in 3.605 seconds; formatting passed in 1.285 seconds. `cargo check --locked --workspace --all-features` passed in 23.559 seconds, including ptrace, DBT, SaBRe, E9patch and LiteInst consumers; its existing vendor C fallthrough/build-cache warnings remain in the log. No runtime claim for those other backends follows from a compile check.

The copied final static-ELF test binary is `target/astra-terminal-cancellation/final/static_elf`, SHA256 `9f6029fde5d447e046eec4639f5668015c62fefe4a197c8f34456749390c2208`. Its direct 13-mode run passed in 1.276 seconds with all callback events retained, and its direct hypercall modes passed in 0.072 seconds. The copied KVM library test binary is `target/astra-terminal-cancellation/final/reverie_kvm`, SHA256 `aee428158cf9a09af9838d77b307dda7364677ebe1db0381202c4e01d0206aed`.

Exact binary enumeration retains every previous test: KVM library 371 to 374; static-ELF 244 to 246. The complete previous 476,410-byte static-ELF source remains an exact prefix, and all 134,456 bytes of the old vm test module remain unchanged. The old runtime test module differs only by the necessary new exhaustive-match arm, which panics if the existing tail-refusal control unexpectedly cancels a thread; none of its old arms or assertions changed. The complete 3,382-byte `ProcessExecutionContext` implementation, including both injection guards, is byte-identical to the base (SHA256 `ce6689deccd6acfa71e276bc4ee990731a5e1611cd0a2aa4322de74f00af19ea`). `/tmp/astra-reverie-terminal-controls/inventory-preservation.json` binds those comparisons to the base and copied final binaries. An initial inventory-driver syntax error ran no comparisons; only the corrected recorded driver supplies this evidence.
