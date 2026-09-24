# Hermit-on-Narf functional-equivalence POC design

Status: owner-review draft, evidence date 2026-09-24. This document designs a
proof of concept; it does not claim that a Narf backend exists or that
Hermit has run on Narf.

## Decision

**Proposed:** build the smallest POC as an x86_64-only, single-task,
trace-producing `reverie-narf` prototype fed by a first-class typed hook at
Narf's live syscall dispatcher. The Linux reference cell uses current
Hermit/Detcore over Reverie ptrace. Both cells emit the same versioned,
fail-closed canonical observable trace, and a separate comparator accepts a
match only when inputs, coverage, semantic records, output, exit disposition,
and cleanup all agree.

P0 is deliberately a software helper, not a conforming Reverie `Backend` and
not a new Hermit backend. Promote it only in stages: mutation and checked
memory; process lifecycle and signals; deterministic scheduling, time, and
instruction events; then optional BPF/framekernel isolation or a larger
ptrace-compatibility program. This boundary is the shortest route to a useful
cross-kernel result because current Narf has a live syscall dispatch seam, but
stock Reverie ptrace has several independent fatal gaps, Narf's seccomp filter
and user-notification path is absent/inert, and Narf's BPF runtime has no guest
syscall attach point. [F-3] [F-8] [F-9] [F-10] [F-11]

The proof is intentionally narrow. A successful P0 would establish that one
controlled Linux ABI program produces an equal, complete syscall-level trace
on Linux and Narf. It would not establish arbitrary-program compatibility,
multithreaded schedule equivalence, full `Tool` portability, production
isolation, aarch64 support, or performance parity.

> **Evidence boundary.** No Hermit-on-Narf implementation exists or was run.
> No external Hermit, strace, or gdb tracer ran inside Narf, and no Reverie
> provider runtime was executed during this research. Exact-commit Narf
> x86_64 QEMU fixtures did execute a limited ptrace subset, but that job later
> failed when QEMU exited with SIGSEGV after 457 seconds; the pinned commit was
> not globally green. Current-state conclusions below are source-only unless
> explicitly marked executed. Proposed interfaces, traces, tests, and phases
> are specifications, not implemented support. Public links were not
> interactively browser-tested; clean local checkouts at the exact revisions
> are the byte authority. No Narf Hermit-style deterministic scheduler,
> virtual-time, or record/replay input-capture implementation was found. [F-31]
> Ptrace SMP stopped-task synchronization is also unproved.

### Claim labels

- **Verified current**: established in pinned source or explicitly named
  execution evidence; `(source-only)` and `(executed)` distinguish them.
- **Proposed**: the recommended POC contract or implementation work.
- **Optional capability**: a narrower opt-in facility that does not satisfy
  the general contract.
- **Software helper**: reusable implementation that is not a conforming
  backend.
- **Above the API**: harness or Tool policy outside the backend contract.
- **Deferred**: intentionally outside the current phase.
- **Unresolved**: evidence or a design choice still needed.

## What current code forces us to design

### The conformance boundary is larger than a syscall hook

**Verified current (source-only):** Reverie's public `Backend` contract hosts
an arbitrary `Tool`, creates one global state, computes subscriptions,
supervises the whole fork/clone/vfork/exec/exit tree, creates process and thread
state, routes subscribed syscall, signal, instruction, timer, and lifecycle
events, permits register and memory mutation through `Guest`, runs exit hooks,
captures output, and returns real statistics. The relevant public boundary is
`Backend` + `Tool` + `Guest` + `GlobalTool` + `Subscription` +
`MemoryAccess`, not one callback. [F-1] [F-19]

**Verified current (source-only):** at the standalone Reverie revision there
are exactly three production public `Backend` implementations: ptrace,
LiteInst, and e9patch. [F-2] This count is not a runtime pass count. KVM,
DynamoRIO, preload, SaBRe/host, and seccomp user notification are real narrower
providers or helpers.

**Verified current (source-only):** LiteInst's public trait path is
direct/in-guest and calls
`run_with_preload*`; Hermit explicitly selects a separate ptrace-host hybrid
through `run_host_with_preload*`. [F-20] [F-21] E9patch implements Reverie's
public trait, but Hermit's narrower product definition treats its CLI path as
e9patch preprocessing followed by the ptrace backend. [F-22] This document
therefore says “public Reverie `Backend` implementation” where trait status is
meant and “e9patch preprocessing with ptrace” for the Hermit path.

### Current Narf has real ptrace, but not stock Reverie ptrace

**Verified current (executed, bounded):** Narf's pinned x86_64 QEMU log records
fourteen named ptrace ABI fixtures plus
`smoke_process_ptrace_e2e` and
`smoke_process_ptrace_syscall_stop` as successful before the later QEMU
failure. This disproves “Narf has no ptrace.” It does not demonstrate an
external tracer or full Linux ptrace compatibility. [F-25]

**Verified current (source-only):** stock Reverie starts with `TRACEME`, stops
before exec, and requests EXEC, EXITKILL, CLONE, FORK, VFORK, VFORKDONE, EXIT,
SECCOMP, and TRACESYSGOOD together. [F-3] Selective seccomp `RET_TRACE` events
are its ordinary subscribed-syscall entry mechanism. [F-33] Exact pending syscalls
later use `PTRACE_SYSCALL`, while suppression/replacement uses register edits,
`SINGLESTEP`, and a private injection page.

Narf implements useful basics—TRACEME, ATTACH/DETACH, word memory,
GET/SETREGS, NT_PRSTATUS regsets, CONT, SINGLESTEP, SYSCALL, SETOPTIONS,
PEEK/POKEUSER, and KILL—but current source has no lifecycle or seccomp
`PTRACE_EVENT_*`, no `GETEVENTMSG`, no `GETSIGINFO`, only acting
TRACESYSGOOD among the stored option bits, incomplete stop/signal semantics,
and an x86_64-only syscall-stop path. [F-8] [F-26] The missing SEIZE, INTERRUPT,
LISTEN, and GET_SYSCALL_INFO requests are not the reason to reject the normal
TRACEME path; current Reverie does not require them for root launch.

Three further gaps independently reject an unchanged stock path:

1. **Verified current (source-only):** Reverie's bulk `MemoryAccess` calls
   `process_vm_readv/writev` from the external tracer to its distinct tracee.
   [F-4] Narf permits self or same-address-space transfers and returns `EPERM`
   for a distinct address space. [F-23]
2. **Verified current (source-only):** default fail-closed Detcore returns
   `Subscription::all()`, which includes CPUID and RDTSC. [F-5] Reverie asks
   for `PR_SET_TSC(PR_TSC_SIGSEGV)` and `ARCH_SET_CPUID(0)`. Narf accepts the
   former without disabling RDTSC and has no latter case, so the subscribed
   Tool events cannot be delivered. [F-6] [F-7]
3. **Verified current (source-only):** `PTRACE_O_EXITKILL` is accepted without
   its Linux containment behavior. A tracer crash can therefore violate the
   no-escaped-guest cleanup invariant even if a happy-path syscall trace were
   otherwise correct.

### Current Narf ingredients

**Verified current (source-only):** Narf's live path decodes six syscall
arguments, calls a concrete handler through `SyscallTable`, and has explicit
ptrace stops before and after dispatch. [F-10] This is the nearest insertion
seam. The exhaustive exact-tree interface search found no generic typed
syscall-interposition trait at the pin. [F-27]

**Verified current (source-only):** Narf's process loader maps an ELF and
follows `PT_INTERP` to a staged interpreter. [F-14] **Proposed:** use a
byte-identical static x86_64 fixture for the least ambiguous P0; admit a
dynamically linked fixture only after both sides stage the same interpreter
and dependency bytes.

**Verified current (source-only):** Narf accepts `BPF_PROG_TYPE_SYSCALL` as a
sleepable program type and can drive it through test-run, while the current
attach resolver exposes fentry and XDP only. [F-11] [F-24] The exhaustive
attach inventory found no guest-syscall target or corresponding syscall
context/copy ABI. [F-28]

**Verified current (source-only):** Narf's available domain backend is selected
at runtime and may be unenforced; FRAME/domain 0 remains writable during BPF
confinement. [F-12] [F-29] Domains are therefore conditional defense in depth,
not a complete sandbox; verifier/helper/dispatcher/uaccess correctness remains
trusted.

## Interposition decision

The criterion is not “can it observe one syscall?” It is the least work that
can produce a falsifiable POC now without making the later Reverie boundary
impossible.

| Mechanism | Current Narf evidence | Reverie/Hermit fit | Required implementation | Smallest-POC judgment | First falsifier |
|---|---|---|---|---|---|
| Stock ptrace | **Verified current (executed, bounded):** named x86_64 basic fixtures ran. **Verified current (source-only):** lifecycle/seccomp events/messages, `GETSIGINFO`, distinct-AS bulk memory, CPUID/RDTSC delivery, and reliable EXITKILL are absent or incompatible. | Naturally fits current `PtraceBackend` only if Linux semantics are supplied end to end. | Kernel events/statuses, regsets, SMP stop ownership, signal/restart fidelity, cross-AS memory, instruction traps, cleanup. | **Not viable unchanged.** Basic syscall stops cannot drive the state machine. | A4 lifecycle, A7 signal provenance, A17 bulk memory, or A18 instruction interception fails. |
| Linux seccomp user notification | **Verified current (source-only):** Narf stores a mode but does not execute a filter, rejects `NEW_LISTENER`, and has no notifier lifecycle. [F-9] | Even on Linux it supplies syscall metadata and a response path, not generic registers, process lifecycle, signals, timers, or instruction events. [F-18] | Filter evaluator, listener object/ioctls, cancellation/restart, memory protocol, lifecycle side channel, containment. | **Not present, and not a complete backend by itself.** | A8 restart/cancellation or A4 lifecycle cannot produce an admissible receipt. |
| Ptrace-compatible emulation | **Proposed:** extend Narf until stock Linux-facing contracts work. | Highest reuse: current Reverie ptrace and Detcore remain mostly unchanged. | All stock gaps above, including event messages, FP/XSTATE, memory, signals, PMU/instruction faults, stop ordering, and tracer death. | **Larger compatibility project, not smallest POC.** Keep as a follow-on if reuse outweighs scope. | A0–A18 against unchanged `PtraceBackend`; any shim-specific exception rejects compatibility. |
| First-class in-kernel Rust interposition | **Verified current seam; Proposed API:** place a typed event source around the live dispatcher and later lifecycle/scheduler seams. | Can grow into a `reverie-narf` adapter, but P0 syscall traces alone are nonconforming. | Stable task/lifetime handles, ordering, disposition/mutation, checked memory, transport, backpressure, cancellation, lifecycle, output/stats, non-syscall events. | **Recommended proposed seam.** It avoids emulating unrelated ptrace ABI before measuring value. | A0/A1/A2 plus A15/A16 must produce complete paired traces; any silent missing field rejects it. |
| Framekernel/BPF-like isolated extension | **Verified current (source-only):** a sleepable syscall program type can load/test-run; attach supports fentry/XDP; the domain backend can be unenforced and FRAME remains writable. [F-11] [F-12] [F-24] [F-29] **Proposed attach:** syscall context/export do not exist. | Could host a restricted observation/filter program; arbitrary async `Tool` portability and lifecycle remain outside it. | Syscall entry/exit attach, typed context, bounded helpers, link lifetime, authorization, export, controller, hostile-program validation. | **Proposed / Deferred P4 isolation follow-on.** Do not put it on the P0 critical path. | Hostile extension tests, A2 bounded copy, A14 teardown, and actual domain-backend enforcement must pass. |

Linux `SECCOMP_RET_TRACE` and `USER_NOTIF` are distinct mechanisms. Narf's
recognition of action constants is not execution of either. Likewise,
`BPF_PROG_TYPE_SYSCALL` is a load/test-run classification, not an attachment
surface.

## Proposed `reverie-narf` architecture

### Boundaries and components

| Component | Placement | Proposed responsibility | Explicit non-inference |
|---|---|---|---|
| Dispatcher event source | Narf syscall entry/exit seam | Emit typed task, ABI, phase, syscall, arguments, sequence, and disposition events. | No generic trait exists today. |
| Task/lifecycle source | Narf process, signal, scheduler, and exit seams | In P0, authoritatively bracket the root task with start, initial-image, final-exit, output-ownership, and teardown/no-leak records ordered around dispatcher events. In later phases, add child/exec, block/wake, signal, and timer records. | Syscall callbacks alone do not establish lifecycle or cleanup. Comparator-synthesized start/exit records are forbidden. |
| Memory/data API | Kernel-mediated and task-scoped | Checked bounded copy-in/copy-out with direction, requested and actual length, exact errno/fault boundary, and revocable lifetime. | A raw guest pointer is not a capability; current cross-AS `process_vm_*` is not usable. |
| Controller transport | Kernel/userspace boundary, still **Unresolved** | Preserve per-task sequence, correlate response, bound queues, expose loss/backpressure, cancel on task exit, and deliver a coverage receipt. | Ring buffer, fd, RPC, and blocking callback are not selected current APIs. |
| `reverie-narf` adapter | Controller side | Map events and dispositions into a growing `Guest`/`Backend` implementation; advertise exact capabilities. | P0 trace production is not an arbitrary-Tool backend. |
| Cross-kernel trace Tool | Above backend API | Canonicalize semantic records and collect coverage/output/cleanup evidence. | Generic Reverie does not define a total order or cross-provider schema. |
| Paired harness/comparator | Above the API | Stage identical inputs, run both cells, verify receipts, apply only declared normalization, and classify the result. | Backends do not manufacture equivalent filesystems, clocks, networks, or images. |

The semantic interface must be stable before choosing transport. A **Proposed
sketch**, intentionally omitting Rust lifetimes and wire layout, is:

```rust
enum Phase { Enter, Exit, Restart, Cancel }

struct SyscallEvent {
    schema: u32,
    task: TaskLifetimeId,
    sequence: u64,
    phase: Phase,
    abi: Abi,
    number: u64,
    args: [u64; 6],
    operation: LogicalOperationId,
    coverage: CoverageEpoch,
}

enum Disposition {
    Execute,
    Complete { raw_result: i64, writes: Vec<CheckedWrite> },
    Inject { call: OwnedSyscall },
    FailController,
}

trait TaskAccess {
    fn read(&mut self, range: GuestRange, out: &mut [u8]) -> CopyResult;
    fn write(&mut self, range: GuestRange, bytes: &[u8]) -> CopyResult;
    fn registers(&mut self) -> Result<OwnedRegisters, AccessError>;
    fn set_registers(&mut self, regs: &OwnedRegisters) -> Result<(), AccessError>;
}
```

`TaskAccess` must be usable only while the kernel proves the named task and
address space are alive and in the required stopped/serialized state. No
borrowed guest pointer may survive an await or task transition. Every copy
returns actual bytes moved and fault information; every mutation is recorded.
P0 can implement only `Execute` and observation, but its schema must reserve
explicit unsupported dispositions instead of pretending they succeeded.

### Control, data, and trust flow

Only the Linux reference path and Narf dispatcher are current. Every box
prefixed `[P]` is **Proposed**.

```text
  Linux cell
  [P] manifest -> [guest user: X] -> [Linux kernel: ptrace/seccomp]
                                      <> [external controller: Reverie ptrace + Detcore]
                                                        |
                                                        +--> [P] canonical trace + coverage --+
                                                                                              |
  Narf cell                                                                                   v
  [P] manifest -> [guest user: same X] -> [Narf kernel: dispatcher/uaccess]        [P] comparator
                                           <> [P] external transport/reverie-narf             ^
                                                        |                                     |
                                                        +--> [P] canonical trace + coverage --+
                                                                                              |
                                matched | semantic divergence | no-result | infrastructure error
```

Control/disposition messages and event/data messages are separate logical
channels even if one transport carries both. A bounded copy request is explicit;
there is no raw arrow from controller to arbitrary kernel memory. BPF and
hardware domains are absent from P0 and may enclose the kernel extension only
in P4. Guest user, kernel, external controller, and harness/comparator regions
are distinct address-space and trust boundaries. Bidirectional kernel/controller
arrows carry event/checked-data outward and disposition/bounded-copy control
inward; they never grant a raw memory pointer.

### Event and lifecycle sequence

All six steps are **Proposed**; each phase's coverage receipt says which are
implemented.

1. The harness verifies executable, dependency, filesystem, environment, and
   configuration digests before starting either cell.
2. The provider assigns a stable task/lifetime identity and emits start and
   initial-image provenance before the first subscribed semantic event. P0
   obtains these records from the process-start seam that owns the root task,
   not from the syscall comparator.
3. Syscall entry exposes scalar metadata and checked task-scoped input reads.
   The controller returns execute, complete/emulate, inject, or fail.
4. Completion records exact raw return/errno, actual bytes moved, mutable
   outputs, register effects, restart identity, and sequence position.
5. Blocking, wake, signal, child, exec, instruction, timer, and exit records
   join the same total order as later phases claim those families.
6. The exit/teardown owner records the root's final disposition, closes its
   stdout/stderr producers, waits for provider-owned task retirement, revokes
   task and memory authority, drains records or marks loss, emits the final
   coverage receipt, and proves no provider-owned task or zombie remains.

P0 implements a strict subset: the minimal root start/initial-image/exit/teardown
envelope plus dispatcher entry/completion for one task. It excludes
fork/clone/vfork/exec after start, signal handling, blocking external I/O,
CPUID/RDTSC in the fixture, and mutation. Those exclusions must be checked from
the binary and coverage receipt where possible, not merely documented. An
excluded event yields `unsupported` or `no_result: coverage`, never `matched`.

### Proposed P2/P3 deterministic transition contract

P2 lifecycle/signals and P3 scheduling are not promoted merely because their
final logs sort the same events. The scheduler, not the callback transport,
assigns each event a causal epoch. Every kernel event producer has a monotonic
sequence and must acknowledge a scheduler-issued `snapshot(epoch, producers,
outstanding_operation_ids)` with a watermark proving that every event causally
owned by that epoch has been enqueued. The scheduler waits for every named
watermark before closing membership, then orders the fixed cohort by guest
identity and operation ID. Controller arrival time is never an epoch input.

Lifecycle events bind to the committed clone/exec/exit operation; guest-generated
signals bind to the committed `kill`/`tgkill` or synchronous fault; timers bind
to a logical deadline. Controlled loopback packets carry the committed send
operation's causal token through the kernel/provider. A physical signal, packet,
or completion with no declared guest-causal token is an uncontrolled input and
is excluded or yields `no_result: input_mismatch`; it cannot be assigned from a
wall-clock cutoff. A host watchdog may terminate a missing watermark or callback
as `infrastructure_error`, but it cannot select cohort membership, advance
virtual time, or convert not-ready into ready. Sorting an already host-selected
set is insufficient.

The implementation must preserve one-resource scheduler requests unless a
separate scheduler audit approves and tests a replacement. A tentative run-queue
selection is not a committed turn. A skipped, vanished, cancelled, or transport-
only turn undoes tentative state, does not re-enqueue work, and advances no
virtual time. A committed semantic turn atomically consumes its request, records
its queue/pool changes, delivers one response, and applies the versioned virtual-
time charge.

| Event class | Eligibility and allowed target state | Required transition | Commit/time and record rule |
|---|---|---|---|
| Syscall completion | Eligible only after the task publishes the matching one-resource blocked request and the producer watermark covers the operation | If blocked on that ID, remove exactly that blocker, install one response, and admit once. If running, tentatively selected, queued, polling on another ID, exiting, or already completed, do not mutate queues: buffer until the matching request or report a duplicate/late protocol error. | Commit in the operation's assigned causal epoch; record requested/actual bytes, request shape, and block/wake state. Duplicate/late completion is infrastructure error. |
| Clone/fork child | Child identity is registered before the parent's continuation becomes eligible | Create child state and Tool state, then admit parent/child according to the fixed policy | One causal epoch records operation kind, identities, and first runnable membership. Reverse callback arrival must not change it. |
| `vfork` child | Parent holds a registration barrier until child registration, then remains blocked until child exec/exit | Admit child once; release parent only on the authoritative completion event | Failed child creation cancels the barrier explicitly; no timer fast-forward crosses an unresolved barrier. |
| Exec/reconnect | New image publishes its generation and replacement identity before former state retires | Migrate state once; retire old generation; resolve caller continuation | Exec never resets logical time. Callback timing around the drain cannot change first post-exec membership. |
| Logical/physical exit | Logical death removes run eligibility; physical completion passes a PID-specific exit barrier | Wake eligible waiters only after authoritative status; revoke memory after final record | No timer fast-forward or waiter completion crosses a pending physical-exit barrier. |
| Signal | Guest-causal/fault signals use their source-operation epoch; timer signals use the deadline epoch. Undeclared external signals are refused. Eligibility uses the watermarked cohort and committed mask/disposition. | Running/tentative/queued targets only gain pending state; a matching interruptible blocker is removed and admitted once; nonmatching or uninterruptible blockers remain; exiting/exited targets follow the declared Linux discard/error rule and are never re-admitted. | Record source token, `siginfo`, pending sets, masks before/after, selection, wake, EINTR/restart, and disposition. Ignored/blocked signals do not fabricate a turn. |
| Timer | Timer arm is already committed and has a finite logical deadline | Fire only when no earlier runnable deterministic work exists and the next deadline is eligible | Fast-forward exactly to that deadline. An indefinite deadline is not fired; it participates in deadlock/liveness classification. |
| External I/O/readiness | The outstanding operation set is frozen when requests publish. A scheduler snapshot names that set; each provider returns a watermark and causal-token state for every named ID. Uncaused host I/O is excluded. | Ordinary work runs first. For only-poller/completion-only progress, inspect the next operation by a stable round-robin cursor and wait for its causal completion or logical deadline; multiple completions are admitted by stable ID after all snapshot watermarks. | Probe/wait wall time does not advance logical time. Host watchdog expiry is infrastructure error, never a semantic timeout or membership cutoff. |
| Controller/Tool failure | May occur from any live state | Close tentative selection, cancel outstanding access, contain every provider-owned task, and emit a terminal stage-tagged receipt | Never becomes a guest semantic result or a matched trace. |

State handling is exhaustive: running, tentatively selected, or queued targets
accumulate only pending state until they publish the matching request; a matching
blocked-pool entry may be removed exactly once; a nonmatching or uninterruptible
blocker remains; pollers change only under their declared interrupt/ready rule;
exiting targets cannot be newly admitted; and events for an exited or completed
operation are discarded only where Linux specifies it, otherwise they are a
protocol error. Every transition records the one-resource request, queue/pool
membership before/after, tentative-pop action, response, re-enqueue, and time
effect.

External-I/O liveness has four explicit cases: ordinary runnable work is not
overtaken by a completion; only pollers trigger the scheduler-controlled
snapshot; multiple completions in one watermarked cohort use stable operation-ID
order regardless of callback order; and a causal completion that is the only
possible progress wakes the scheduler without a busy loop. A nearby not-ready
operation remains blocked. The producer fence, not repetition, proves membership.

Signal conformance is a matrix, not one delivery test. It covers process- versus
thread-directed selection; ignored, blocked, caught, and default actions;
standard coalescing and realtime queue order; atomic temporary masks for
`pselect6`, `ppoll`, and `rt_sigsuspend`; arrival before and after request
publication, tentative selection, blocking, completion, and target exit; and a
mask/disposition change between physical arrival and deterministic commitment.
The trace records pending sets before and after so one physical signal cannot be
delivered twice or disappear while still pending. The ASPLOS 2020 prototype did
not support guest-to-guest signals or sockets, so it is background for the
deterministic model, not validation of these proposed paths.

P3 promotion uses one fully specified cooperative profile,
`poc-coop-v1`. Asynchronous PMU preemption is disabled and the receipt says
`preemption=none`; a guest with competing CPU-bound threads that fail to reach a
syscall, explicit yield, signal, lifecycle, or blocking boundary is
`unsupported`. A semantic scheduler turn is exactly one selected task consuming
one published one-resource request and receiving its response. The logical clock
starts at the manifest's fixed epoch and adds exactly 1 ns for each committed
semantic turn. Skipped/aborted selections, admission drains, polling/snapshot
probes, transport work, and teardown add 0 ns. Exec preserves the clock. When no
runnable work or unresolved lifecycle/exit barrier exists, a finite timer
fast-forwards exactly to the smallest deadline; an indefinite deadline never
fires. Per-task CPU time adds the same 1 ns only to the selected task. Every
clock API and Tool-selected RDTSC value is derived from this exact state.

A9/A10 compare the complete turn and virtual-time trajectory and reject
rounding, freezing, reset, skipped-turn ticks, or filtering. This POC profile is
not the product's future continuous fine-grained time or precise-preemption
claim. A later `poc-rcb-v1` profile is **Deferred** until a separate reviewed
specification fixes guest-user-only conditional-branch counting, exclusion of
kernel/controller/injection code, per-task origin/reset across exec, programmed
threshold, overflow attribution, calibrated skid correction, exact delivery
boundary, unsupported-host behavior, trace identity, and replay equality. No
“RCB or replacement” alias may promote P3.

**Deferred:** Narf record/replay is not a P0–P3 promotion claim; no Narf
record/replay input-capture implementation was found. A later gate must record a
realized execution on one exact provider/revision, replay the same event stream
on that provider, require exact schedule, virtual-time, output, and state
trajectory, and refuse missing, exhausted, or desynchronized records. Only after
that same-provider replay result is classified does the independent Linux/Narf
canonical comparison run. Linux ptrace replay cannot stand in for a Narf replay.

**Deferred:** parallel execution of independent processes is outside this POC.
Before enabling it, a separate design must define read/write resource conflicts,
the independence proof, deterministic commit order, visibility of external
effects, rollback/failure behavior, and record/replay representation, with both
conflicting and nonconflicting process-pair tests.

## Paired experiment and canonical observable trace

### Boot and fixture prerequisites

The first comparison uses x86_64 because current Narf syscall-stop evidence is
x86_64-only and the instruction contracts under discussion are x86-specific.
The recommended guest is a static, position-fixed ELF containing only the
expected syscall instructions and no vDSO dependency. It receives identical
argv, env, cwd, umask, rlimits, fixed virtual personality, and an immutable
read-only fixture whose content manifest is hashed into the run identity.

The Narf image must contain the exact guest bytes. The Linux cell must expose
the same guest-visible paths and bytes, not merely a similar host directory.
Before moving to dynamic binaries, both images must stage the same interpreter
and dependency digests and compare auxv/TLS/loader outcomes. No external
network is allowed in P0; controlled loopback is deferred until A12. Each cell
records architecture, page size, kernel/Hermit/Reverie revisions, image digest,
argv/env/cwd, config, seeds, and fixture digest.

**Above the API:** the harness owns this equivalence of external worlds.
Hermit/Reverie and the proposed adapter only observe or control what their
interfaces expose.

### Trace families and admissibility

| Family | Required fields | Equality rule | Refusal condition | Tests |
|---|---|---|---|---|
| Provenance/inputs | Schema, provider, exact revisions, arch, image/dependency/fixture/config digests | Inputs/config exact; provider and revision retained only as provenance | Schema or digest mismatch | A0, A13, A16 |
| Coverage | Subscriptions, enablement proofs, event counts, lifecycle hooks, memory method, byte/hash coverage, loss/truncation | Both sides cover every claimed family | Missing family, unexplained zero work, native subscribed instruction, loss, truncation | A16–A18 |
| Task/lifecycle/order | Logical IDs, parent/thread group, exec generation, event ordinal, phase, scheduler turn | Exact provider-neutral tree and total semantic order | Missing/ambiguous identity or lifecycle event | A0, A4–A6, A9 |
| Syscall/memory | ABI, number, six args, typed scalars, input bytes, result/errno, partial length, mutable output bytes, register effects | Semantic values and actual bytes exact | Pointer substituted for bytes or moved length unavailable | A1–A3, A17 |
| Persistent state | fd/open-file/path/map/socket/process/signal/timer transition plus follow-up observation | Transition and later observation exact | Success inferred without observation | A3, A6, A11, A12 |
| Signals/time/hardware | Queue/provenance/mask/target/restart; virtual clocks/timers; CPUID/RDTSC ordinal and selected values | Exact declared Linux personality | Missing provenance or subscribed instruction executes natively | A7–A10, A18 |
| Filesystem/network/platform | Guest paths/bytes/metadata/order; controlled packets/readiness; uname/auxv/proc/vDSO | Exact within the declared fixture/personality | Uncontrolled input or undeclared capability difference | A11–A13 |
| Final observation | stdout/stderr exact bytes, lengths, digests; exit/signal/core; cleanup | Exact | Output missing, task escaped, zombie, or receipt incomplete | A0, A14–A16 |

Current strict Hermit comparison removes the wall-clock prefix, canonicalizes
addresses, and compares every admitted INFO message exactly. [F-15] Its
comparison specification separately enables I/O-buffer comparison. [F-30]
That is evidence for the discipline, not an already cross-provider schema.
Current record/replay intentionally proves replay of one recording with real
time capture; it does not prove equality of independent Linux and Narf runs.
[F-17]

### Normalization rules

1. **Proposed:** strip only transport wall-clock prefixes. Preserve virtual
   time, event counts, flags, sizes, returns, errnos, and data.
2. **Proposed:** ordinalize only explicitly tagged controller/host addresses,
   preserving identity, aliasing, and order. An untagged hexadecimal scalar
   stays exact.
3. **Unresolved:** P0 should prefer exact guest virtual addresses. A future
   mapping-identity-plus-offset bijection is admissible only if it preserves
   every alias, comparison, fault boundary, and guest-visible printed value.
4. **Proposed:** assign logical task IDs from authoritative create events and
   preserve parentage, thread groups, and exec generations. Never align by raw
   host PID magnitude.
5. **Unresolved:** PID, fd, inode, device, mount, and port values remain exact
   in P0. Later renaming requires a field-specific bijection covering every
   appearance and allocation/alias order.
6. **Proposed:** compare guest byte paths after guest root/dirfd resolution.
   Host staging prefixes never enter the trace.
7. **Proposed:** buffers compare direction, actual moved length, and bytes.
   Capacity not moved is excluded; page-boundary partial results remain
   observable.
8. **Proposed:** preserve event, directory, readiness, packet, and signal order
   unless the Linux ABI makes a field an unobservable set and the schema names
   that exception.
9. **Proposed:** unknown schema fields, malformed records, missing coverage,
   truncation, comparator errors, and zero unexplained work refuse comparison.
   They cannot be normalized away.

### Outcome taxonomy

Hermit's current verdict types distinguish a match from guest exit status and
retain output digests, counts, and first-divergence information. [F-16] The
cross-kernel comparator extends that discipline without collapsing classes:

| Outcome | Falsifiable discriminator |
|---|---|
| `matched` | Input and coverage receipts are admissible; every canonical record and final output/disposition is exact. This is the only POC success. |
| `diverged_clock` | First semantic difference is clock/timer state; other prefix fields match. |
| `diverged_control` | Event/syscall/scheduler prefix, identity, phase, or count first differs. |
| `diverged_observation` | Control/result/length match but observed bytes differ. |
| `diverged_state` | Immediate completion matches but a required follow-up state observation differs. |
| `unsupported` | A provider explicitly rejects a required capability before comparison. |
| `no_result: coverage` | A subscribed family, buffer, lifecycle hook, or instruction proof is missing. |
| `no_result: comparison_refused` | Trace is malformed, truncated, unknown-version, zero-work, or lacks an artifact. |
| `no_result: input_mismatch` | Declared fixture/provenance differs before execution; if the guest observed it first, classify the resulting semantic divergence instead. |
| `infrastructure_error` | Provider/stage-tagged panic, kernel fault, deadlock, lost task, escaped task, zombie, cleanup failure, or harness failure. |

Two zero exits can diverge. Two identical nonzero exits can match if all
admissibility and trace requirements hold. A guest exit by signal/core is a
canonical disposition and can match or diverge. A provider, kernel,
controller, or harness crash is `infrastructure_error`, never a guest semantic
mismatch. Unsupported behavior is not a pass.

## Security and trust boundary

P0 is restricted to trusted fixtures, but that assumption is explicit and
cannot support a hostile-guest claim.

| Actor/component | Authority and protected assets | Current enforcement | Proposed addition | Failure effect and mandatory test |
|---|---|---|---|---|
| Guest | Controls its user memory, code, syscalls, timing attempts | Narf user/kernel address-space boundary; not penetration-tested here | P0 allowlisted image digest; later adversarial fixtures | Runtime/tool corruption is `infrastructure_error`; A14 containment. |
| Narf dispatcher/uaccess | Can stop/continue tasks and copy user data | Range validation plus guarded user copies [F-34] | Revocable `TaskAccess`, bounds, byte counts, no pointer across await | Wrong-task or stale access must fail; A2/A17 and task-exit races. |
| Controller transport | Carries semantic events and dispositions | None selected | Versioning, sequence, bounded queues, backpressure, authentication/ownership, loss bit, cancellation | Drop/reorder/truncate forces refusal; A16 and forced controller death. |
| `reverie-narf` and Tool | May inspect/mutate registers, memory, results, and global state | Not implemented | Capability advertisement, panic containment, per-task lifetime, least privilege | Tool error cannot release guest; A3/A14. |
| Harness/comparator | Chooses fixtures and decides match | Existing file/process permissions | Artifact hashes, immutable manifest, schema validation, independent negative controls | Any undetected A15 mutation invalidates comparator. |
| BPF verifier/JIT/helpers | Would execute untrusted extension code in P4 | Current load authority is the per-task credential gate, not the bootstrap capability [F-35] | Syscall context/helper verifier rules and hostile-program suite | Escape is security failure, never functional match. |
| Domain backend/manager | Restricts cross-domain memory where enabled | Conditional PKS/PCID/MTE; FRAME remains accessible | Prove effective backend and mapping/tag coverage in receipt | Unenforced/unknown backend blocks isolation claim. |
| Trace artifacts | May contain arguments, file data, secrets | Ordinary storage permissions | Explicit capture policy, redaction only outside compared semantic bytes, hashes, retention | Missing/redacted compared data refuses match. |

Ptrace attach performs target and credential access checks but still grants a
privileged control surface; it is not a sandbox. [F-37] Likewise, an
in-kernel interposer with tracee-memory authority expands the kernel TCB to
include the new interface and uaccess path. For an optional BPF implementation,
the verifier/interpreter/JIT/helper/dispatcher chain is the primary safety
argument; hardware domains remain defense in depth. Narf's current credential
check, not a bootstrap capability object, is the actual BPF load-authority
gate. [F-35]

## Implementation phases and promotion gates

| Phase | Claimed scope and interfaces | Explicit exclusions | Executable gates | Promotion/stop rule |
|---|---|---|---|---|
| P0: trace plumbing | **Proposed, nonconforming software helper.** x86_64, one process/thread, fixed static fixture; authoritative root start/initial-image/exit/teardown; dispatcher enter/exit; checked read-only buffers; exact output/exit; coverage receipt; paired comparator. | Fork/thread/vfork/exec-after-start, signals, blocking external I/O, mutation/injection, CPUID/RDTSC, deterministic scheduling, network, RNG/PID equivalence. | A0; P0 deterministic-buffer and invalid-syscall subcases of A1; only pre-characterized A2 read subcases; the full-schema golden A15 corpus plus live P0-family mutations; both exact A16 refusal subclasses. | Promote only after paired artifacts match and every applicable live and schema-level negative control is detected. An unresolved A2 case cannot promote. Stop if lifecycle ordering, teardown, or memory cannot be observed without an unsafe ABI. |
| P1: mutation/backend skeleton | **Proposed.** Suppress/emulate/inject, registers, task-scoped read/write, real output/stats, initial `Guest` operations. | Whole process tree and signal semantics still unsupported. | Characterized A2 cases, A3, A17, Tool-error cleanup; repeat A15/A16. | Any unmatched/uncharacterized A2 case blocks the claimed subprofile. Stop if mutation cannot be transactional with task lifetime or if distinct-task bytes are ambiguous. |
| P2: lifecycle/signals | **Proposed.** Fork/clone/vfork/exec/exit barriers and identity, waitability, the full signal-state matrix, robust-list/clear-tid, containment. | Deterministic PMU/time/filesystem/network still incomplete. | A4–A8 and A14 plus reversed callback/arrival-boundary permutations. | Only after these may the adapter claim generic process-tree obligations; otherwise remain a bounded helper. |
| P3: cooperative deterministic world | **Proposed.** Watermarked eligibility/drain/commit state machine; exact `poc-coop-v1` 1-ns-per-committed-turn clock; no async preemption; CPUID/RDTSC; seeded randomness and fixed PID/personality rule; filesystem; causal controlled loopback. | External network/signals, PMU/RCB preemption, CPU-bound competing threads without boundaries, Narf record/replay, independent-process parallelism, and production performance. | Expanded cooperative A9–A13 and A18; producer-watermark/snapshot tests; exhaustive synthetic boundary permutations; at least 100 strict repeated runs per timing fixture; full A15/A16. | Require identical watermarks, committed schedules, and full time trajectories. Stop if host arrival changes membership or coverage cannot prove instruction/input interception. Precise RCB is a separately reviewed deferred profile. |
| P4: proposed isolation/compatibility | **Proposed / Deferred.** Add BPF syscall attach/context/copy/export with verified isolation, or separately implement Linux-compatible ptrace for stock reuse. | No automatic claim that either path is lower overhead or production ready; parallelism and Narf replay remain separate designs. | Full A0–A18, hostile-extension tests, actual domain-backend receipt, failure injection, regression comparison with prior phases. | Choose from measured engineering and runtime evidence; do not merge paths only to claim backend count. |

P0 and P1 must be called software helpers if the owner requires “Hermit
backend” to mean arbitrary-Tool conformance from the first deliverable. The
first conforming `reverie-narf` milestone is the earliest phase that implements
every advertised subscription, process-tree lifecycle, mutation/memory,
output/statistics, and cleanup obligation without fabricated zeros.

## Falsifiable acceptance matrix

Every row is **Proposed** and currently has evidence status `specified`, not
`executed Linux`, `executed Narf`, or `paired`. The implementation must provide
a `poc-driver` (name is a proposed command contract, not an existing binary)
whose manifest is validated before it can change that status:

```yaml
schema: narf-hermit-acceptance/v1
case: A0                         # exactly one A0..A18
profile: p0                      # exact capability profile
status: specified               # specified|executed_linux|executed_narf|paired
repetitions: 1                  # P2/P3 timing fixtures: >=100
timeout_seconds: 30
linux:
  command: ["${POC_DRIVER}", "run", "--provider=linux", "--case=A0", "--out=${LINUX_CELL}"]
  revision: "<exact>"
  exit: "<code|signal|not-run>"
narf:
  command: ["${POC_DRIVER}", "run", "--provider=narf", "--case=A0", "--out=${NARF_CELL}"]
  revision: "<exact>"
  exit: "<code|signal|not-run>"
expectation:
  event_prefix: ["run", "task_start", "initial_image"]
  count_constraints: {task_start: 1, task_exit: 1, loss: 0}
  comparator: ["${POC_DRIVER}", "compare", "--expect=matched", "${LINUX_CELL}", "${NARF_CELL}"]
  negative_controls: []
artifacts:                         # every value is required and content-addressed
  image_sha256: "<sha256>"
  fixture_sha256: "<sha256>"
  trace_sha256: {linux: "<sha256>", narf: "<sha256>"}
  coverage_sha256: {linux: "<sha256>", narf: "<sha256>"}
  stdout_sha256: {linux: "<sha256>", narf: "<sha256>"}
  stderr_sha256: {linux: "<sha256>", narf: "<sha256>"}
  duration_ms: {linux: 0, narf: 0}
```

Unknown command paths remain literal `${POC_DRIVER}` placeholders until the
owning phase implements them; a placeholder manifest cannot advance beyond
`specified`. The validator rejects unknown fields, absent artifacts, zero
unexplained event counts, a repetition below the phase minimum, a timeout, or a
command/exit mismatch. Per-case templates and minimum count constraints are:

| ID | Required profile | Command suffix on both `run --provider=<linux|narf>` cells | Required event prefix/count | Comparator/control invocation | Timeout |
|---|---|---|---|---|---:|
| A0 | P0 | `--case=A0 --fixture=exit0` | `task_start=1`, `initial_image=1`, `task_exit=1`, `loss=0` | `compare --expect=matched`; omit exit -> `no_result:coverage`; leave zombie -> `infrastructure_error` | 30 s |
| A1 | P0 subset; full P3 | `--case=A1 --fixture=syscall-values` | exact syscall enter/exit count from fixture manifest; buffer hashes nonzero | `compare --expect=matched`; mutate return/errno/byte | 30 s |
| A2 | P0 read subset/P1 full | `--case=A2 --fixture=<characterized-subcase>` | exactly one attempted transfer; frozen expected actual byte count required | `compare --expect=matched`; delete byte extent -> `no_result:coverage`. Uncharacterized subcases remain `specified` research and cannot promote. | 30 s |
| A3 | P1 | `--case=A3 --fixture=replace-inject` | one suppressed original, exact injected calls, one follow-up observation | `compare --expect=matched`; duplicate original -> control divergence | 30 s |
| A4 | P2 | `--case=A4 --fixture=process-tree --arrival-order=<permutation>` | exact create/exec/exit counts and causal epochs | all callback permutations match one committed trace | 60 s |
| A5 | P2 | `--case=A5 --fixture=nonleader-exec --arrival-order=<permutation>` | one identity migration and exec generation | duplicate migration -> divergence | 60 s |
| A6 | P2 | `--case=A6 --fixture=robust-exit --arrival-order=<permutation>` | one clear-tid and one eligible wake | remove wake -> state/control divergence | 60 s |
| A7 | P2 | `--case=A7 --signal-case=<matrix-row> --arrival-edge=<edge>` | pending-set before/after and one disposition per physical signal | full matrix match; nearby-must-not-wake negative | 60 s |
| A8 | P2 | `--case=A8 --restart-case=<matrix-row> --arrival-edge=<edge>` | logical operation ID, exact transfer/remainder, restart count | wrong restart or duplicate operation -> divergence | 60 s |
| A9 | P3 | `--case=A9 --profile=poc-coop-v1 --sched-edge=<edge> --repeat=100` | identical producer watermarks, committed queue/pool transitions and 1-ns turn trajectory; `preemption=none` | `compare-set --all-identical`; move callback timing or remove a watermark/barrier -> detected | 120 s |
| A10 | P3 | `--case=A10 --profile=poc-coop-v1 --timer-edge=<edge> --repeat=100` | exact full clock trajectory, timer deadline, instruction events, no native escape | skipped-turn tick, wrong 1-ns charge, reset, frozen/rounded time -> detected | 120 s |
| A11 | P3 | `--case=A11 --fixture=immutable-fs --repeat=100` | exact fd/open-file/path transition and later observation counts | mutate later observation -> `diverged_state` | 120 s |
| A12 | P3 | `--case=A12 --io-edge=<edge> --callback-order=<permutation> --repeat=100` | fixed drain membership, readiness/packet counts, bounded progress | nearby-not-ready, reversed callbacks, only-completion progress | 120 s |
| A13 | P3 | `--case=A13 --personality=poc-v1 --repeat=100` | all declared uname/auxv/proc/vDSO fields present | mutate one field -> observation divergence | 60 s |
| A14 | P2+ | `--case=A14 --failure-stage=<stage>` | terminal failure receipt, `live_tasks=0`, `zombies=0` | `compare --expect=infrastructure_error` | 30 s |
| A15 | All | `--case=A15 --mutation=<return|errno|buffer|schedule|signal|output>` | golden full-schema corpus plus live records for claimed families | each `compare --expect=<class>` names exact first record | 30 s |
| A16a | All | `--case=A16 --fault=remove-family` | missing-family name and first expected record location | `compare --expect=no_result:coverage` | 30 s |
| A16b | All | `--case=A16 --fault=truncate` | malformed/truncated record location; never a complete receipt | `compare --expect=no_result:comparison_refused` | 30 s |
| A17 | P1 | `--case=A17 --fixture=remote-memory32` | one 32-byte read, one 32-byte write, exact later observation | same-AS or word-only evidence -> coverage refusal | 30 s |
| A18 | P3 | `--case=A18 --fixture=cpuid-rdtsc --repeat=100` | exactly one Tool event each; effective setup and selected values | native value or zero event -> coverage refusal | 60 s |

Every completed manifest records the literal expanded commands, exact heads,
strictness/log level, all relaxations (normally none), comparator policy,
duration, exit disposition, event-family counts, and artifact hashes. P2/P3
timing fixtures also require exhaustive synthetic state/arrival permutations
and at least 100 strict end-to-end repetitions with one trace hash.

| ID | Phase | Guest/action | Required canonical evidence | Positive outcome and negative control |
|---|---|---|---|---|
| A0 | P0 | Static x86_64 `_exit(0)` | One task start, declared initial exec boundary, one exit, exact empty output, no leaked task | Pair is `matched`; remove exit or leave zombie -> refusal/error. |
| A1 | P0 subset/P3 full | P0: invalid syscall plus `write` of a fixed known buffer. P3: add `getpid` and seeded `getrandom` after freezing guest-visible PID and RNG policy. | One enter/exit each; P0 exact ENOSYS and known bytes; P3 exact PID, random count and bytes | Pair matches only for implemented subcases; mutate return, errno, or byte -> exact first divergence. |
| A2 | P0/P1 | Characterized `read`/`write` subcases cross into `PROT_NONE`; additional page/segment cases remain research-only until characterized | Requested range, frozen expected partial length, exact moved bytes, EFAULT/return boundary | A promotion subcase must be `matched`; unavailable bytes -> `no_result: coverage`. An unresolved research subcase remains `specified` and cannot satisfy a gate. |
| A3 | P1 | Replace syscall; inject `openat`/`close` | Original executes zero times; injected args/results and register restoration exact; fd side effect observed | Any duplicate original or unobserved fd state diverges. |
| A4 | P2 | pthread clone, fork, vfork+exec under reversed callback timing | Logical child identity/parentage, registration barrier, Tool state, vfork suspension, exec generation, wait status | Every callback permutation commits the same membership; missing child/event or early parent continuation refuses/diverges. |
| A5 | P2 | Nonleader thread execs | Former identity maps once to leader; siblings exit; state migrates exactly once | Duplicate/lost state or wrong generation diverges. |
| A6 | P2 | Robust mutex owner exits with clear-tid waiter | Owner-word transition, clear-tid, futex wake, waiter result and order | Matching exits without wake/state evidence are insufficient. |
| A7 | P2 | Full signal-state matrix: directedness, pending/mask/disposition, temporary masks, arrival edges, realtime value, standard coalescing | Exact pending sets before/after, siginfo, selection, wake/no-wake, handler/default/ignored action, and delivery multiplicity | Wrong payload/order/recipient or nearby forbidden wake diverges. |
| A8 | P2 | Interrupt read/poll/nanosleep/`pselect6`/`ppoll`/`rt_sigsuspend`, with transfer/no-transfer and `SA_RESTART` | Atomic temporary mask, exact EINTR/restart, partial bytes/remainder, one logical-operation ID | Duplicate operation, lost pending signal, or wrong per-syscall restart diverges. |
| A9 | P3 | Two cooperatively bounded runnable threads, futex, and every admission edge around publication/selection/drain/completion/exit | Producer watermarks, complete request, runqueue/blocked-pool, skip/commit/reenqueue, and exact 1-ns-per-commit trajectory across permutations and 100 repeats | Coincident one-run equality is insufficient; move callback timing across a barrier or remove a watermark and require detection. |
| A10 | P3 | Repeated clock reads, timerfd, finite/indefinite timers, skipped/committed/polling turns, exec, RDTSC, CPUID under `poc-coop-v1` | Exact full virtual-time trajectory and charge, expiration order, instruction ordinal and Tool-selected values; receipt says no async preemption | Native instruction, skipped-turn tick, wrong delta, rounding, freezing, reset, or filtering -> coverage/divergence failure. |
| A11 | P3 | open/dup/fork/read/write/stat/getdents/procfs fixture | Bytes, offsets, open-file aliasing, metadata, directory order, persistent mutation | Equal returns with differing follow-up state -> `diverged_state`. |
| A12 | P3 | Controlled socketpair/loopback with arrival-before/after-drain, only pollers, ordinary work, reversed simultaneous completions, only-completion progress | Frozen drain membership, packet/readiness order, operation IDs, socket state, timeout, peer close, and bounded liveness | Nearby not-ready operation must stay blocked; host callback order or external packet cannot select guest order. |
| A13 | P3 | uname, auxv, `/proc/self/status`, vDSO clock | Exact declared virtual-personality bytes | Undeclared platform difference is semantic or unsupported, never normalized ad hoc. |
| A14 | P2+ | Kill supervisor; force Tool error | Guest containment, bounded teardown, no escaped process/zombie, exact failure stage | Escape/leak is `infrastructure_error`. |
| A15 | Every promotion | Six mutations—return, errno, buffer byte, schedule turn, signal payload, output byte—against a phase-independent full-schema golden corpus; also mutate every family present in the live phase | Correct first record and divergence class for every golden mutation and applicable live mutation | Any mutation reported `matched` blocks promotion; P0 does not fabricate live schedule/signal events; no tolerance widening. |
| A16 | Every promotion | Remove one event family; separately truncate one trace | First trial names the family/expected location and returns `no_result: coverage`; second names the malformed record and returns `no_result: comparison_refused` | The exact subclasses are mandatory; neither trial may match, use generic `no_result`, or semantic-diverge. |
| A17 | P1 | External controller reads and rewrites known 32-byte task buffer | Exact bytes, method, logical task, byte count, digest, and later guest observation | Current stock Narf external `process_vm_*` fails `EPERM`; new API must prove its own safe path. |
| A18 | P3 | Default fail-closed Detcore executes one differing `rdtsc` and `cpuid` | Both subscription bits, effective setup proof, exactly one Tool event each, only Tool-selected results observed | Native value or zero event is coverage failure. |

A15 is six tests, not one. A16 has two independent failure injections. A17
cannot be weakened to ptrace word access or same-address-space memory. A18
cannot be weakened to accepting setup calls; the guest must observe the
Tool-selected values.

## Risks, unresolved decisions, and stop conditions

| Unresolved item | Why it matters | Blocks | Resolution/stop condition |
|---|---|---|---|
| P0 transport: blocking callback, fd/queue, shared ring, or RPC | Determines reentrancy, backpressure, cancellation, ordering, and kernel attack surface | P0 implementation | Choose only after a prototype proves bounded loss-free enter/exit ordering. Stop if task teardown cannot revoke outstanding access. |
| Semantic total order across provider events | Generic Reverie does not define one | P0 match, all later phases | Version an order rule and make A15/A16 sensitive to it. |
| Task/lifetime and address-space ownership under SMP | Stale or concurrently running tasks make memory/register access unsafe | P1 | Kernel-held lifetime plus stopped/serialized proof; otherwise no controller mutation. |
| Exact guest address vs mapping bijection | Layout can differ even when operations are analogous | P0 fixture choice, later mmap tests | P0 uses fixed exact layout or explicitly avoids address-bearing output. Do not invent a bijection after seeing results. |
| PID/fd/inode/port renaming | These values are guest-visible and allocation order is semantic | P2/P3 | Keep exact until a predeclared class-specific bijection is proved end to end. |
| Narf SINGLESTEP, restart/wait order, realtime signal reinjection, FP/XSTATE, tracer death | Required for stock ptrace or full mutation/signals | P1/P2 or ptrace follow-on | Execute independent external-tracer conformance; source names/options are insufficient. |
| Exact Linux/Narf page/segment fault behavior | Narf behavior is known, exact paired Linux result is not | A2 | Pin Linux implementation and run page-boundary pairs before labeling a divergence. |
| Scheduler/preemption contract | Narf uses per-CPU queues and optional work stealing, not Hermit's deterministic scheduler. [F-13] | P3 | Implement explicit control/observation and compare realized schedules; otherwise constrain claim to single task. |
| Ptrace SMP stopped-task synchronization | Ownership checks exist, but safe stopped-state synchronization for external mutation is unproved | P1/P2 or ptrace follow-on | Race register/memory operations against resume/exit on SMP; require kernel-held stop ownership or reject mutation. |
| Time/randomness/filesystem/network inputs | Current Narf uses hardware-derived time and CSPRNG output [F-32]; filesystem and network remain uncontrolled semantic-input classes for this POC | P3 | Virtualize/capture or exclude each class and prove the receipt; no external network in P0. |
| CPUID/RDTSC and PMU mechanism | Current stock setup is absent/inert and perf equivalence is unproved | A9/A10/A18 | Demonstrate effective interception and selected values/counts, not successful setup returns. |
| Boot/image identity | Same binary, interpreter, dependencies, env, filesystem, and network fixture have not run on both kernels | Any paired claim | Hash and stage all inputs; mismatch refuses before execution. |
| BPF/domain enforcement | Intended domain backend and mapping/tag coverage are unexecuted; FRAME stays trusted | P4 isolation | Receipt proves effective backend plus hostile extension tests; otherwise call it trusted in-kernel code. |
| Narf record/replay | No Hermit-style Narf capture/replay implementation was found | Beyond P3 | Keep **Deferred** until same-provider exact replay, exhaustion/desynchronization refusal, and then independent cross-kernel comparison execute. |
| Independent-process parallelism | Conflict/independence, commit order, and externally visible effects are unspecified | Beyond P4 | Separate reviewed design plus conflicting/nonconflicting process-pair and replay tests. |
| Narf pinned-job failures | QEMU SIGSEGV and aarch64 failures may affect long runs | P0 duration/baseline | Reproduce or bound the x86_64 crash before using long-run absence as POC evidence. |

**Deferred:** uncontrolled external networking, performance equivalence,
arbitrary identity/address renaming, production hardening, and broad aarch64
parity. Performance may be measured only to decide whether the POC is usable;
it is not part of functional match.

## Evidence appendix

### Exact revisions

| Source | Exact revision | Role |
|---|---|---|
| `dev-hermit` | `4b4250928d52a5e1f9f7a0b258fb734f06196d8e` | Registered worktree start and report container. |
| [`rrnewton/hermit`](https://github.com/rrnewton/hermit/tree/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2) | `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2` | Active Hermit/Detcore source. |
| Hermit's nested [`rrnewton/reverie`](https://github.com/rrnewton/reverie/tree/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed) | `ae1d1da78d2d89a1fa0454ee2282cf135e2286ed` | Actual Hermit build pin. |
| Standalone [`rrnewton/reverie`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937) | `123df7c4c0169006fbfe1f11a1553fe333eac937` | Complete provider inventory; seven commits behind nested pin. |
| [`hodgesds/narf`](https://github.com/hodgesds/narf/tree/afe919924a5fd2c8e820ab3a0564b6d45438e6bc) | `afe919924a5fd2c8e820ab3a0564b6d45438e6bc` | Public Narf implementation and captured CI head. |
| [Linux man-pages](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/commit/?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7) | `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7` | Ptrace, process-vm, wait, and seccomp-user-notification comparison contract. |

Vendored provider revisions at the standalone Reverie pin are DynamoRIO
`929840ad9190e5086775e8debc0f0b79b4208d59`, e9patch
`6c2c03c1da74b14daf1788a9f8dccfa354ce04a6`, SaBRe
`41113f849f8799932ed8c7883f5a4de616b9e9fa`, and LiteInst Cargo git revision
`4dab7f01d4d6d394e968fbfad842019f72743c81`.

### Platform matrix

| Cell | Boot/loader evidence | Syscall/interposition evidence | Instruction prerequisites | Executed evidence | POC disposition |
|---|---|---|---|---|---|
| Linux x86_64 + Hermit ptrace | **Verified current (source-only in this research):** mature Linux ELF/process environment assumed by current launch path | Full stock Reverie protocol is implemented in source | Reverie requests TSC and CPUID faulting; default Detcore subscribes | No provider runtime was run in this research | Reference architecture for the paired cell; actual POC run still required. |
| Narf x86_64 | **Verified current (source-only):** ELF/PT_INTERP loader; exact-commit x86_64 boot job failed later | **Verified current (executed, bounded):** named basic ptrace fixtures; **source-only:** syscall stops exist but stock event/memory/signal gaps remain | TSC request is inert; ARCH_SET_CPUID absent | Bounded in-QEMU fixtures passed before QEMU SIGSEGV at 457 seconds; no external tracer | Only defensible first POC target, through the proposed dispatcher hook rather than stock ptrace. |
| Narf aarch64 | **Verified current (source-only):** loader path exists; **executed CI:** exact-commit aarch64 boot-smoke passed while full kernel-test failed [F-36] | **Verified current (source-only):** ptrace syscall-stop logic explicitly excludes aarch64 | x86 CPUID/RDTSC contract does not transfer; replacement personality is unspecified | No external tracer or paired program execution | **Unresolved / Deferred:** no arbitrary-guest parity claim and no P0 target. |

This matrix distinguishes an executed named fixture from a declared code path;
neither is promoted to whole-platform support.

### Complete provider/mechanism inventory

**Verified current (source-only):** this exhaustive provider/helper inventory
is anchored by F-1/F-2 and the concrete helper types in [F-38].

| Provider/mechanism | Public Reverie `Backend`? | Placement and locked status |
|---|---:|---|
| ptrace | Yes | External Linux ptrace/seccomp reference; source-inspected, not run here. |
| KVM | No | Host VMM with bounded x86_64 bare Linux-like personality; **Optional capability / Software helper**. |
| DynamoRIO DBT | No | In-process translated provider with syscall/signal/thread callbacks; **Optional capability / Software helper**. |
| preload | No | Trusted dynamic x86_64 seccomp-TRAP/SIGSYS primitive; **Software helper** with loader/static/exec gaps. |
| LiteInst trait/direct | Yes | Direct in-guest bounded public trait path. |
| LiteInst ptrace-host hybrid | No separate impl | Separate optional API selected by Hermit; ptrace owns lifecycle/control. |
| e9patch hybrid | Yes | AOT root rewrite plus ptrace lifecycle/residual coverage; public Reverie trait implementation. |
| e9patch direct | No separate impl | Optional in-process path with incomplete lifecycle. |
| SaBRe + reverie-host | No | Experimental in-process adapter with host RPC; **Software helper**. |
| Seccomp user notification | No | Linux listener/ioctl helper, no current public execution-backend consumer. |
| `riptrace` | No | Front end over other mechanisms; excluded as a provider/backend. |

This is eleven rows only because direct/hybrid paths and one excluded front end
are separately visible. It is not eleven conforming backends and not a support
rate.

### Canonical finding source index

These commit-pinned public links are the reader-facing source for the `[F-N]`
anchors. Independent literal verification results are in
`evidence/verification.jsonl`.

| Finding | Primary source |
|---|---|
| F-1 | [Reverie `Backend` contract](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/backend.rs#L60-L106) |
| F-2 | [`PtraceBackend`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/backend.rs#L52-L54), [`LiteinstBackend`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L590-L592), and [`E9patchBackend`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-e9patch/src/backend.rs#L1045-L1047) |
| F-3 | [Stock ptrace option mask](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1826-L1836) |
| F-4 | [Ptrace bulk `process_vm_readv`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/safeptrace/src/memory.rs#L62-L84) |
| F-5 | [Default Detcore subscriptions](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L1077-L1087) |
| F-6 | [Narf `PR_SET_TSC`](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_prctl.rs#L417-L430) |
| F-7 | [Narf x86_64 `arch_prctl`](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_arch_prctl.rs#L4-L87) |
| F-8 | [Narf ptrace option behavior](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L1127-L1133) |
| F-9 | [Narf seccomp handler](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_seccomp.rs#L22-L30) |
| F-10 | [Narf live syscall dispatcher](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/syscall.rs#L3789-L3804) |
| F-11 | [Narf BPF syscall program type](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf.rs#L518-L526) and [test-run path](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf.rs#L635-L643) |
| F-12 | [Narf BPF domain-confinement specification](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/bpf/specification/domain-confinement.md#L115-L127) |
| F-13 | [Narf per-CPU/work-stealing scheduler](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/scheduler/src/lib.rs#L25-L31) |
| F-14 | [Narf ELF/PT_INTERP loader](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/process.rs#L194-L214) |
| F-15 | [Hermit canonical comparison policy](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/bin/hermit/verify.rs#L324-L346) |
| F-16 | [Hermit canonical verdict](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/canonical_verdict.rs#L6-L24) |
| F-17 | [Hermit record/replay guarantee boundary](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/metadata.rs#L297-L309) |
| F-18 | [Linux `seccomp_unotify(2)`](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/tree/man/man2/seccomp_unotify.2?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7) |
| F-19 | [Reverie `Guest` task/memory contract](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/guest.rs#L56-L74) |
| F-20 | [LiteInst public direct path](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L590-L603) |
| F-21 | [Hermit LiteInst ptrace-host selection](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/lib.rs#L2756-L2770) |
| F-22 | [E9patch public tracer-backed path](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-e9patch/src/backend.rs#L1045-L1063) |
| F-23 | [Narf distinct-address-space `EPERM`](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6940-L6967) |
| F-24 | [Narf BPF attach resolver](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf_attach.rs#L254-L276) |
| F-25 | [Exact-commit Narf x86_64 CI job](https://github.com/hodgesds/narf/actions/runs/36061908203/job/107842987657) |
| F-26 | [Narf `sys_ptrace` request dispatch](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L757-L1415) |
| F-27 | [Concrete Narf syscall/ptrace seam](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/syscall.rs#L3789-L3804) |
| F-28 | [Complete current Narf BPF attach-surface description](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf_attach.rs#L9-L23) |
| F-29 | [Narf runtime domain backend and `Unenforced`](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/arch/src/lib.rs#L28-L50) |
| F-30 | [Hermit comparison I/O-buffer field](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/bin/hermit/verify.rs#L286-L355) |
| F-31 | [Narf scheduler baseline used for inverse deterministic/replay search](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/scheduler/src/lib.rs#L25-L31) |
| F-32 | [Narf hardware-derived time](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/time/src/lib.rs#L137-L160) and [`getrandom` CSPRNG](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_getrandom.rs#L62-L86) |
| F-33 | [Reverie subscribed-syscall seccomp TRACE filter](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1866-L1892) |
| F-34 | [Narf guarded user-copy boundary](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/compat.inc.rs#L2515-L2580) |
| F-35 | [Narf BPF load-authority boundary](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf.rs#L143-L186) |
| F-36 | [Narf aarch64 boot smoke](https://github.com/hodgesds/narf/actions/runs/36061908203/job/107842987478) and [kernel test](https://github.com/hodgesds/narf/actions/runs/36061908203/job/107842987601) |
| F-37 | [Narf ptrace attach access checks](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L788-L834) |
| F-38 | [Reverie KVM helper](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-kvm/src/vm.rs#L1187-L1192), [DBT guest](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-dbt/src/lib.rs#L183-L190), [preload dispatcher](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-preload/src/dispatch.rs#L178-L190), [SaBRe guest](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/experimental/reverie-sabre/src/reverie_adapter.rs#L1051-L1060), and [notifier ioctls](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-process/src/seccomp/notif.rs#L153-L156) |

### Coverage and audit ledger

| Evidence class | Denominator | What it establishes | Durable detail |
|---|---:|---|---|
| Narf repository inventory | 2,956 tracked paths | Search denominator, not files read or capabilities | [`evidence/narf-source-coverage.md`](evidence/narf-source-coverage.md) |
| Narf research questions | 13 precise/naive pairs | Architecture, boot, syscall, ptrace, seccomp, BPF/domain, module, input, security, CI coverage | Same ledger, including document classes, headings, exclusions, inverse checks, and exact-commit CI receipts |
| Reverie research questions | 14 precise/naive pairs | Public trait, every provider, ptrace protocol, memory, lifecycle, trust | [`evidence/reverie-source-coverage.md`](evidence/reverie-source-coverage.md); 115-entry raw receipt manifest verified |
| Hermit/Linux research questions | 11 precise/naive pairs | Active backend selection, Detcore subscriptions/inputs, trace/verdict/record semantics, Linux contracts | [`evidence/hermit-source-coverage.md`](evidence/hermit-source-coverage.md); 22 receipt entries verified |
| Preliminary findings | 61 records, 99 `file_ref`s | Research-lane provenance only | 23 Narf + 18 Reverie + 20 Hermit; [`evidence/finding-schema-notes.md`](evidence/finding-schema-notes.md) documents 69 literal, 24 ellipsis, 2 compacted, 4 omission/markup forms |
| Canonical actionable findings | 38 `F-N` records | Every material current-state claim in the main design is mapped by the claim inventory and independently verified | [`evidence/actionable-claim-inventory.md`](evidence/actionable-claim-inventory.md), [`evidence/findings.jsonl`](evidence/findings.jsonl), and [`evidence/verification.jsonl`](evidence/verification.jsonl) |
| Reverie implementations | 3 public `Backend`; 6 execution-specific `Guest` impls | Exact source inventory, not execution | [`evidence/reverie-backend-matrix.md`](evidence/reverie-backend-matrix.md) |
| Acceptance primitives | A0–A18 = 19 | Proposed test denominator | Zero paired cross-kernel cells executed |

Opened document classes included code, Cargo/workspace metadata, public API
docs, architecture/security specifications, build/boot/testing docs, provider
READMEs, Linux man-pages, tests, CI workflow/log/API records, and exact Git
metadata. Seen-but-excluded material includes unrelated Narf drivers and
firmware, historical Reverie README limitations superseded by pinned source,
generated/vendor bulk outside the named provider seams, and test names used as
runtime proof. The coverage ledgers preserve every opened heading and the
reason for each exclusion.

Inverse checks searched for all production `impl Backend for` and concrete
`Guest` implementations; raw and wrapped ptrace requests; missing ptrace
events/requests; both seccomp `RET_TRACE` and USER_NOTIF; dispatcher filter
consumers; BPF program and attach types; `process_vm_*` separately from ptrace
word memory; CPUID/RDTSC setup; and provider launch paths. No declaration,
comment, accepted option, callback registration, test name, or zero exit was
treated as a runtime capability.

### Contradictions and independent audits

The canonical fact set preserves stale Narf `linux-compat` feature prose,
conflicting ptrace-stub/SEIZE documentation, MTE/domain/module documentation
drift, older Reverie provider READMEs, accepted-but-inert option bits, and the
failed exact-commit jobs. Pinned implementation plus named executed evidence
decides current behavior; unresolved runtime behavior stays unresolved.

Audit A initially rejected inverted LiteInst direct/hybrid classification,
the demotion of known process-vm and CPUID/RDTSC gaps, incomplete reproducible
query receipts, incomplete preliminary schema documentation, and stale Narf
seccomp classifications. The final independent Audit A reproduced the exact
sources and evidence denominators and returned **LOCKED** in
[`evidence/audit-a-lock3.md`](evidence/audit-a-lock3.md).
Audit B then returned **READY TO DRAFT** with the structure and terminology
controls in [`evidence/audit-b.md`](evidence/audit-b.md). Historical failed
post-draft writing, deterministic-scheduling, and adversarial audits remain in
the evidence directory as correction provenance. Their superseded target
hashes and every finding's current disposition are mapped in
[`evidence/audit-resolution.md`](evidence/audit-resolution.md); final exact-head
verdicts are separate files rather than edits to historical reports.

### Runtime/source reconciliation

The only product execution evidence used here is captured remote CI for exact
Narf commit `afe9199…`. The x86_64 kernel-test job visibly ran the bounded
ptrace fixtures noted above and later recorded QEMU SIGSEGV after 457 seconds;
the job failed. Other cited Narf group results are exact-head captured check
records. No local Narf guest, QEMU guest, Reverie provider, or Hermit command
was run, so there is no unreported local runtime support and no ad-hoc Hermit
invocation to wrap with `safehermit`.

## Owner decision and immediate next action

Authorize P0 only if a syscall-level, single-task equality result is useful on
its own and the team accepts that it is a nonconforming software helper. The
implementation ticket should freeze, before coding, the P0 event schema,
transport choice, task-lifetime rule, exact guest-address policy, static guest
manifest, coverage receipt, and A0/A1/A2/A15/A16 commands.

Do not authorize “make stock Hermit work on Narf” under the P0 estimate: that
means the much larger ptrace-compatible path. Do not require BPF/domain
isolation for the first trusted-fixture result: first prove the semantic
boundary, then decide whether P4 isolation or ptrace reuse is worth the
additional work. A result may be called “Linux/Narf functionally equivalent
for guest X under profile Y” only when its paired artifacts satisfy the
`matched` predicate above; every narrower or failed result must retain its
explicit capability, refusal, divergence, or infrastructure classification.
