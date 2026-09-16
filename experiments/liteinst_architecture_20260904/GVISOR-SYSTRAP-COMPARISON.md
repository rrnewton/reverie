# gVisor systrap and Hermit's intended LiteInst path

Read-only related-work draft, 2026-09-04 America/Los_Angeles. Source bindings,
line references and provenance are in `GVISOR-SYSTRAP-SOURCES.md` beside this
file. **G** denotes the local gVisor snapshot, **R** the immutable Reverie
dependency, **S** the committed SUD controller, **H** the pinned Hermit CLI.
This is source analysis, not a port, performance result, or compatibility test.

## Position

gVisor systrap is a close precedent for **trap first, patch eligible syscall
sites afterward**. Patching is enabled by default and can be disabled with
`--systrap-disable-syscall-patching`. At the inspected revision it also enables
Linux syscall user dispatch (SUD), allowing the reserved stub interval and
trapping outside it, with seccomp trapping as fallback. Describing it as only
seccomp signals, or as trapping every hot syscall forever, would miss actual
source. However, systrap's stub is not the Linux implementation: application
syscalls go to the separate Sentry user-space kernel. Its process creation,
ELF loading, application signals, file descriptors and exec are Sentry-owned.
Hermit's intended path instead keeps shared Detcore's local Reverie Tool in
the application's process, with an external GlobalTool reached by RPC.

That difference explains the startup advantage. Sentry constructs an image
in an address space whose stub infrastructure is prepared before application
execution. It does not wait for an LD_PRELOAD constructor after native Linux
has entered the dynamic loader. This is a different ownership model, not a
solution for preserving a native exec'd preload or safely reentering an
in-process asynchronous Tool. [G1–G5, R1–R2]

## Compact comparison

| Concern | gVisor systrap, inspected source | Intended LiteInst / current pinned source |
|---|---|---|
| Linux behavior and isolation | Separate Sentry implements the application Linux ABI; restricted host stub executes application instructions and reports events. | Same shared Detcore local Tool in guest, external global RPC; not a second Linux kernel. H's CLI still selects mandatory ptrace-hosted LiteInst, distinct from R's typed path. |
| Initial loading | Sentry parses ELF/scripts, maps PT_INTERP, stack and vDSO, then selects initial registers. Stub code/handlers are prepared beforehand. | Typed launch uses native process spawn + LD_PRELOAD/bootstrap. Constructor installation does not cover earlier loader execution. |
| Exec | Loads a new TaskImage, then switches task image and resets modeled exec state. No host exec of that application image is required by this path. | Exec refusals remain. SUD resets on successful native exec; rearming later does not intercept earlier replacement-loader calls or migrate Tool state. |
| Syscall path | SUD allows the reserved stub interval and traps outside it, alongside seccomp; optional, default-enabled patching sends eligible sites to assembly/C stub code, then shared-memory Sentry dispatch. Others remain trapped. | First trap installs LiteInst hooks leading to the same Tool; reviewed restricted unpatchable fallback is present in R. S is standalone; composition has not selected SUD for typed execution. |
| Signals and restoration | Host stub signal frames/altstacks are separate from Sentry-modeled application frames/masks. Both signal and patched entries have explicit state handling. | Native handlers execute in the same process and thread as the local Tool. PatchAllocator/TOOL_HEAP already isolates runtime Rust allocations; libc/TLS/lock reentrancy and joint guest-handler/Tool/clock ownership remain unqualified. Initial quiescence is insufficient. [R3] |
| Communication | Shared context records, ring queue, atomic state, optional spinning and futex sleep/wakeup; a stub may resume a different application context. | Shared Tool driver + blocking RPC client; this is not systrap's task/context scheduler or memory transport. |
| Time and scheduling | Host TSC-based transport timing, Go task execution, calibrated clocks and coarse CPU accounting; no demonstrated deterministic RCB policy here. | Continuous deterministic virtual time and precise guest-only branch accounting are requirements, not optional performance choices; composed clock component remains restricted. |
| Ptrace | Application sysmsg threads are detached for execution, but setup and some management explicitly attach/inject/detach through ptrace. | Intended runtime path must not depend on ptrace. Systrap is not evidence that its complete startup works under inherited ptrace denial. |

## What the source actually does

**Loading and exec.** `stubInit` copies the linked process stub and embedded
signal-handler blob into reserved mappings and makes code executable.
`forkStub` clones a child, installs handlers/filter, and attaches to prepare
its state. A sysmsg thread receives shared and private stack mappings, an
alternate signal stack, its filter, and initial registers. An artificial
SIGSEGV enters the handler; ptrace detach releases it into the context
protocol, not directly into an uncontrolled application loader. [G1–G2]

Sentry's loader recognizes ELF and interpreter scripts, loads a dynamic
interpreter when present, and starts at that interpreter's entry. The loader
also accepts the no-interpreter case; this is not evidence for Hermit static
support. Exec calls `LoadTaskImage` with a new memory manager; load errors
return through the existing task. Success proceeds through sibling exit and
image replacement, resets caught signal dispositions/alternate stack, keeps
the modeled signal mask, and closes modeled close-on-exec descriptors.
Task/kernel state outside the image survives under Sentry's ownership.
This avoids native exec resetting the executing stub's mappings and SUD:
application exec is an emulated kernel operation, not a host exec of the
stub. [G3–G4]

In our retained SUD experiment, genuine dynamic exec and later rearm
**succeeded**, but preinit/constructor observations before rearm were native.
The failure is the promised startup interception, not that all exec failed.
Three retained TSC-startup cases remain red; CPUID mode reset while TSC fault
mode persisted. Systrap's owned loader is not evidence that SUD alone fixes
those observations. R still refuses exec injection; S explicitly disclaims
loader coverage and Tool migration. [S1, E1]

**Signals and syscall handling.** gVisor's host stub installs SA_ONSTACK,
SA_SIGINFO and SA_RESTORER handlers with SIGCHLD/SIGSYS masked. Its restorer
issues real rt_sigreturn from the mapped stub; the handler saves registers,
FS base and supported FP state into shared context records and restores a
selected context. It postpones its own interrupt during specified stub
states and handles explicitly recognized internal faults. Separately,
Sentry's `SignalSetup`/`SignalReturn` constructs and restores application
signal state and masks. A syscall by an application handler is still an
application syscall to Sentry, not a callback recursively entering Sentry's
Go allocator in that application process. This separation is the important
precondition—not general permission to copy the stub's signal mask into our
Tool runtime. [G5–G6]

**Patching and state.** The SIGSYS path recognizes an eligible immediate
syscall-number load plus syscall, asks Sentry to patch, and retains trap
dispatch otherwise. Patched entry changes to the private handler stack,
saves registers/FP state, calls `__syshandler`, and still exchanges a context
with Sentry. Return restores actual RSP and flags and jumps back; it is not
merely updating saved IP/SP metadata. Source documents GS-use and TF/debugger
limitations, skips new patching for a guest being ptraced, and has a TODO for
already-patched code when tracing attaches. FP sizing excludes AMX. These are
explicit scope limits, not guarantees of arbitrary-PC state restoration.
[G7–G8]

## Useful techniques, with their conditions and costs

- **Prepare interception before the initial instruction.** Sentry can do this
  because it owns ELF/interpreter mappings and initial registers. Adopting the
  same approach would change native loading, exec and process-identity
  responsibilities; it is not a small preload fix or a proposal to add a loader.
- **Retain signal dispatch for sites that cannot be patched.** gVisor separates
  this correctness path from eligible-site optimization. Hermit can preserve
  that principle through the existing shared Tool, but must establish faithful
  syscall ABI, restart and clock accounting on both paths. gVisor's pattern
  assumptions/GS/TF limitations cannot silently become Hermit exemptions.
- **Use explicit saved state and ownership transitions.** Real kernel frames
  and architecture-specific assembly, rather than HookContext metadata, make
  restoration concrete. Reuse of the idea requires our own validated xstate,
  TLS, nesting and scalar continuation contracts. LiteInst already isolates
  runtime Rust allocations through PatchAllocator/TOOL_HEAP, but this does not
  qualify arbitrary asynchronous Detcore/libc/TLS/lock reentry. Sentry's
  separate runtime avoids that local reentry problem rather than solving it.
- **Batch communication through shared memory.** Atomic queues and futexes can
  reduce transport overhead, but assume controlled shared mappings and a
  context scheduler. Host-timed spinning, changed worker affinity/priority and
  choosing whichever context is ready cannot determine Hermit's guest schedule
  or time. Any transport must stay outside guest branch accounting and preserve
  the shared GlobalTool's ordering policy. No speedup is measured here.
- **Treat SUD as interception, not the entire security boundary.** gVisor makes
  the whole reserved stub interval allowed by SUD, retains seccomp restrictions, and uses
  ptrace for setup. Our narrow shared gate and no-ptrace requirement differ;
  copying its allowed range or management route would give up those properties.
- **Do not borrow its clock semantics.** TSC-based spinning/timeouts and coarse
  CPU ticks serve responsiveness/accounting, not deterministic execution.
  Replacing continuous guest time or precise preemption with these mechanisms
  would weaken Hermit's contract. No PMU/RCB determinism result follows from
  the systrap sources inspected. [G9–G10]

## Current integration limit and uncertainty

The frozen 49be + e792/2df composition still installs typed seccomp; standalone
SUD remains unclocked. Its all-signals dispatcher mask conflicts with exact
SIGTRAP clock-window tests. gVisor's separate task-signal implementation does
not supply the missing runtime-owned signal policy or resolve local Tool
reentry. Do not remove the tests, accept silent internal forwarding, relax
comparators or relabel failures to make this comparison look favorable. [E2]

This local gVisor branch includes benchmark-related history; it is not asserted
to be current upstream. No builds, executions, probes, performance comparisons,
or fresh validation were performed. Source presence does not prove either
backend's coverage. All 340 frozen reference cells, static deferrals and the
>=306 target remain unchanged; this work earns no numerator credit.
