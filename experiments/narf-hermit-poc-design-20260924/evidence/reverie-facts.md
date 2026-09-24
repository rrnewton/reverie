# Reverie fact set (Audit A input)

Status: fact collection complete for Reverie revision `123df7c4c0169006fbfe1f11a1553fe333eac937`; source-only unless a runtime result is explicitly stated. This is evidence, not reader-facing design prose or a backend recommendation.

Repository: [`rrnewton/reverie`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937), committed 2026-09-20. Every GitHub link below is pinned to that commit (or to an exact vendored revision).

Claim labels used here:

- **Verified current**: directly exercised by a current implementation path at the pinned source revision. It does not mean this investigation executed that path.
- **Software helper**: implemented support code that is not itself a conforming Reverie `Backend`.
- **Optional capability**: a current opt-in path with narrower coverage than the generic `Backend` contract.
- **Above the API**: tool policy rather than a backend mechanism.
- **Deferred**: expressly incomplete in current source.
- **Unresolved**: the source set did not establish the behavior.

## Canonical backend and tool boundary

**Verified current.** The public `Backend` trait is a clean backend boundary. A conforming implementation must accept an arbitrary `T: Tool`, initialize one global state, compute subscriptions once, supervise the complete process/thread tree through `fork`/`clone`/`vfork`/`execve`, create process/thread states, route subscribed syscall/signal/instruction/timer/lifecycle callbacks, run exit hooks, and return final state. It also must implement real statistics and output capture, without substituting zero or empty output for unsupported behavior ([`backend.rs` lines 60-106](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/backend.rs#L60-L106), [lines 218-285](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/backend.rs#L218-L285)).

**Verified current.** An exhaustive `impl Backend for` search finds exactly three implementations at the pinned revision:

1. `PtraceBackend` ([`reverie-ptrace/src/backend.rs` lines 25-115](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/backend.rs#L25-L115));
2. `LiteinstBackend` ([`reverie-liteinst/src/backend.rs` lines 590-645](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L590-L645));
3. `E9patchBackend` ([`reverie-e9patch/src/backend.rs` lines 1045-1105](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-e9patch/src/backend.rs#L1045-L1105)).

**Verified current.** The lower extension boundary consists of `Tool`, `Guest`, `GlobalTool`, `Subscription`, `MemoryAccess`, and typed syscalls. `Tool::handle_syscall_event` is a prehook: returning without injection suppresses the original syscall; `Guest::inject` executes a syscall and returns its result; `Guest::tail_inject` deliberately never returns. The same guest handle exposes memory, GPRs, mutable GPRs, stack, per-thread state, global RPC, timers, and a backend-defined clock ([`tool.rs` lines 317-425](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/tool.rs#L317-L425), [lines 480-559](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/tool.rs#L480-L559), [`guest.rs` lines 56-216](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/guest.rs#L56-L216), [lines 432-459](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/guest.rs#L432-L459)).

**Verified current.** `Subscription` presently selects syscalls plus x86-64 CPUID and RDTSC. It has no generic enum for fork/exec/signal events; those are lifecycle obligations of a conforming backend ([`subscription.rs` lines 21-131](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/subscription.rs#L21-L131)).

**Verified current (source-only).** Hermit/Detcore's default fail-closed run configuration returns `Subscription::all()`, so stock Hermit asks for both CPUID and RDTSC events rather than only syscalls ([`detcore/src/lib.rs` lines 1077-1090](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L1077-L1090)). On x86-64 the ptrace backend requests RDTSC faulting with `PR_SET_TSC(PR_TSC_SIGSEGV)` before exec and later injects `arch_prctl(ARCH_SET_CPUID, 0)` when CPUID is subscribed ([`tracer.rs` lines 1638-1650](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1638-L1650), [`task.rs` lines 1615-1633 and 1958-2015](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L1615-L1633)).

**Above the API.** Scheduling decisions, virtual time, filesystem/network policy, trace construction, and normalization belong to a `Tool` such as Hermit/Detcore. Backends must expose the events and mutation operations needed by that policy but the `Backend` trait does not define a canonical trace format.

## Stock ptrace execution path

### Placement and launch

**Verified current.** `PtraceBackend` is a thin generic adapter over `TracerBuilder`. The tracee pre-exec hook requests `PR_SET_TSC` when RDTSC is subscribed, always requests `PR_SET_NO_NEW_PRIVS`, disables ASLR with `personality`, then calls `PTRACE_TRACEME` and raises `SIGSTOP` before exec ([`tracer.rs` lines 1628-1685](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1628-L1685)). All ptrace operations for a tracee must remain on the ptracer thread; the generic backend explicitly permits a non-`Send` future for that reason ([`backend.rs` lines 210-217](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/backend.rs#L210-L217)).

### Exact options and event encoding

**Verified current.** Immediately after the root's `SIGSTOP`, stock Reverie makes one `PTRACE_SETOPTIONS` call with all of:

- `PTRACE_O_TRACEEXEC`
- `PTRACE_O_EXITKILL`
- `PTRACE_O_TRACECLONE`
- `PTRACE_O_TRACEFORK`
- `PTRACE_O_TRACEVFORK`
- `PTRACE_O_TRACEVFORKDONE`
- `PTRACE_O_TRACEEXIT`
- `PTRACE_O_TRACESECCOMP`
- `PTRACE_O_TRACESYSGOOD`

([`tracer.rs` lines 1815-1836](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1815-L1836)). Accepting these bits without implementing their stops is not equivalent to implementing them.

**Verified current.** Stop decoding requires Linux wait-status encoding: `SIGTRAP|0x80` identifies `PTRACE_SYSCALL` entry/exit stops; the high 16 bits identify `PTRACE_EVENT_*`; an ordinary stop is delivered as `Event::Signal`. `PTRACE_EVENT_FORK`, `VFORK`, and `CLONE` immediately call `PTRACE_GETEVENTMSG` for the child TID. `PTRACE_EVENT_EXEC` calls it for the former TID. Other decoded events are `VFORK_DONE`, `EXIT`, `SECCOMP`, and `STOP` ([`safeptrace/src/lib.rs` lines 144-243](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/safeptrace/src/lib.rs#L144-L243), [lines 393-430](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/safeptrace/src/lib.rs#L393-L430)).

### Syscall interception and restart semantics

**Verified current.** The ptrace backend installs a seccomp filter whose default is `ALLOW`; each subscribed syscall returns `SECCOMP_RET_TRACE(0)`. `rt_sigreturn` is always allowed from the private path, and syscalls from the private trampoline IP range are allowed. `restart_syscall` has no unconditional exception: it is intercepted only when the tool subscribed to it ([`tracer.rs` lines 1866-1892](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1866-L1892)). Therefore stock syscall entry delivery depends on `PTRACE_O_TRACESECCOMP` plus `PTRACE_EVENT_SECCOMP`, not just `PTRACE_SYSCALL`.

**Verified current.** At a seccomp stop the task reads GPRs and the syscall arguments, records the pending syscall, calls the tool, and returns the tool result. If the tool returns a value without injecting, the original syscall is suppressed. Suppression or replacement sets the original syscall number to `-1`, `PTRACE_SINGLESTEP`s past the instruction, restores registers, and uses a private executable page for an injected syscall ([`task.rs` lines 4056-4186](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L4056-L4186), [lines 5190-5353](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L5190-L5353)).

**Verified current.** If `Guest::inject` requests the exact pending syscall, Reverie resumes with `PTRACE_SYSCALL` and consumes the syscall-exit stop. A replacement or standalone injection uses the private page and single-step path ([`task.rs` lines 5491-5522](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L5491-L5522)). If a private injected syscall is interrupted by a non-`SIGTRAP` signal, Reverie writes `-ERESTARTSYS` to the return register and queues the signal ([`task.rs` lines 5418-5460](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L5418-L5460)). `Guest::inject_with_retry` separately retries `EINTR` and `ERESTARTSYS` ([`guest.rs` lines 383-423](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie/src/guest.rs#L383-L423)).

### Registers and guest memory

**Verified current.** The base GPR path is `PTRACE_GETREGSET`/`SETREGSET` with `NT_PRSTATUS`; floating-point paths use `NT_PRFPREG`, and x86-64 full state paths use `NT_X86_XSTATE` ([`safeptrace/src/lib.rs` lines 622-749](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/safeptrace/src/lib.rs#L622-L749)). `TracedTask` implements generic `Guest`, including memory and GPR access ([`task.rs` lines 6016-6061](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L6016-L6061)). Debug/core-register paths explicitly read and write both GPR and FP register sets ([`task.rs` lines 6000-6012](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L6000-L6012)).

**Verified current.** Small scalar guest-memory accesses use `PTRACE_PEEKDATA`/`POKEDATA`; bulk vectored accesses use the ordinary host `process_vm_readv`/`process_vm_writev` syscalls ([`safeptrace/src/memory.rs` lines 23-125](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/safeptrace/src/memory.rs#L23-L125)). A kernel with ptrace word access but without compatible `process_vm_*` behavior therefore does not establish the full `MemoryAccess` contract.

**Verified current (source-only), fatal for stock bulk `MemoryAccess` on Narf.** Narf's `process_vm_transfer` permits [self or same-address-space/`CLONE_VM` transfers](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6880-L6887) but [returns `EPERM` when the target resolves to a distinct address space](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6940-L6967). Stock Reverie invokes `process_vm_readv/writev` from the external tracer against its distinct tracee, so word PEEK/POKE support does not rescue the bulk path. Narf's later-iovec fault/partial-byte-count compatibility remains **Unresolved** and is separate from this already-fatal address-space check.

### Lifecycle, signals, ordering, and isolation

**Verified current.** New-child events supply the new TID and distinguish fork/vfork/clone. Exec supplies the former TID, which Reverie uses to reconcile a nonleader exec. Vfork-done unblocks the parent. Exit is handled out of band through an exit future and an owned `PTRACE_EVENT_EXIT` stop before the final wait status; these are active state-machine dependencies, not only diagnostics ([`task.rs` lines 2109-2174](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L2109-L2174), [lines 2868-2895](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L2868-L2895), [lines 5004-5071](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L5004-L5071)).

**Verified current.** `PTRACE_GETSIGINFO` is used to distinguish timer/perf/internal traps and signal provenance before tool delivery. For an ordinary externally delivered signal, `Tool::handle_signal_event` may preserve, replace, or suppress the signal; Reverie passes the chosen signal number in the `PTRACE_CONT` data argument. The normal path did not call `PTRACE_SETSIGINFO` at this revision ([`task.rs` lines 2779-2853](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L2779-L2853), [`safeptrace/src/lib.rs` lines 752-830](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/safeptrace/src/lib.rs#L752-L830)).

**Verified current.** `PTRACE_O_EXITKILL` binds tracee lifetime to tracer lifetime. Ptrace itself is not a security sandbox; the tracee remains a Linux process and the backend deliberately invokes its syscalls. `PR_SET_NO_NEW_PRIVS`, selective seccomp trace, and tracer ownership reduce specific hazards but do not establish isolation from the host.

## Ptrace requirement intersection supplied for Narf Audit A

The Narf researcher independently reported for exact Narf revision `afe919924a5fd2c8e820ab3a0564b6d45438e6bc`: `TRACEME`, `ATTACH`, `DETACH`, peek/poke, `GETREGS`/`SETREGS`, `CONT`, `SINGLESTEP`, `SYSCALL`, `SETOPTIONS`, `GETREGSET`/`SETREGSET(NT_PRSTATUS)`, `PEEKUSER`/`POKEUSER`, and `KILL` exist; `SEIZE`, `INTERRUPT`, `LISTEN`, `GETSIGINFO`, `GETEVENTMSG`, and `GET_SYSCALL_INFO` do not; only `TRACESYSGOOD` has behavior, other options are accepted but ignored; no `PTRACE_EVENT_*` stops exist; syscall stops are x86-64 only. These Narf facts must be reproduced and pinned by the Narf evidence owner before canonical lock. The classifications below are deductions from that supplied inventory and the Reverie source dependencies above.

| Stock Reverie dependency | Intersection classification | Evidence-based reason |
|---|---|---|
| `TRACESECCOMP` + `PTRACE_EVENT_SECCOMP` | **Fatal for stock ptrace** | This is how subscribed syscall-entry prehooks are delivered. `PTRACE_SYSCALL` is only used after the callback for an exact pending syscall. |
| `TRACEEXEC` + EXEC event + `GETEVENTMSG` former TID | **Fatal for stock ptrace** | Root/post-exec setup and successful exec lifecycle are event driven; nonleader exec reconciliation requires former TID. |
| `TRACEFORK`/`TRACECLONE`/`TRACEVFORK` + child events + `GETEVENTMSG` child TID | **Fatal for the generic Backend contract** | Complete process/thread-tree supervision and tool state allocation require authoritative child identity. Avoidable only by changing the claimed guest scope and backend implementation, not by configuring stock Reverie. |
| `TRACEVFORKDONE` | **Fatal when a guest uses vfork** | Reverie has a distinct vfork-parent release transition. Avoidable only by excluding/emulating vfork in a modified backend. |
| `TRACEEXIT` and exit event | **Fatal for stock ptrace state machine** | Exit-stop ownership and cancellation are wired into the run/cleanup path. A new backend could use a different lifecycle design. |
| `GETSIGINFO` | **Fatal for stock signal/timer paths** | Current code reads provenance for internal/timer/trap discrimination. An extremely constrained no-signal/no-timer fork would still be a code change, not stock support. |
| `EXITKILL` | **Cleanup/security invariant; not an ordinary happy-path data dependency** | Ignoring it can leak/continue guests if the tracer dies. It can only be replaced by an independently proven lifetime mechanism. |
| `TRACESYSGOOD` and `PTRACE_SYSCALL` | **Present in the supplied Narf inventory** | Required syscall-exit stops are encoded as `SIGTRAP|0x80`. Entry-versus-exit alternation still requires validation. |
| `SINGLESTEP`, GPR get/set | **Present in the supplied Narf inventory** | Required for skip/replacement/private-page injection. Exact post-step stop and register semantics remain runtime-unverified. |
| `NT_PRFPREG`, `NT_X86_XSTATE` | **Unresolved/absent from supplied inventory** | Narf reports only `NT_PRSTATUS`. Core GPR mutation could work; FP/XSTATE-dependent debug and preservation paths are not established. |
| `process_vm_readv`/`writev` | **Verified current source-only gap; fatal for stock bulk `MemoryAccess`** | Narf accepts only self/same-AS transfers and returns `EPERM` for the external tracer's distinct tracee address space. Later-iovec partial-transfer fidelity remains unresolved. |
| `PR_SET_TSC(PR_TSC_SIGSEGV)` | **Verified current source-only gap for default strict Hermit** | Narf accepts mode 2 but explicitly never disables RDTSC, so no fault reaches the subscribed Tool ([source](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_prctl.rs#L407-L430)). |
| `ARCH_SET_CPUID(0)` / CPUID faulting | **Verified current source-only gap for default strict Hermit** | Narf `arch_prctl` implements FS/GS subcodes only and returns `EINVAL` for other codes, including `ARCH_SET_CPUID` ([source](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_arch_prctl.rs#L5-L87)). |
| `CONT` signal reinjection | **Request present; semantics Unresolved** | Reverie relies on data-argument signal delivery/suppression/replacement, not merely accepting `PTRACE_CONT`. |
| `SEIZE`, `INTERRUPT`, `LISTEN`, `GET_SYSCALL_INFO` | **Avoidable for the normal TRACEME root path** | No stock command-root dependency was found. They can matter to attach/debug/cleanup variants but are not the listed launch path. |
| `SETSIGINFO` | **Avoidable for ordinary stock delivery** | Wrapper exists, but current normal signal path did not call it. |
| `ATTACH`/`DETACH` | **Auxiliary** | Used by attach/debug/cleanup paths, not the normal TRACEME root launch. |

The intersection does not support a claim that stock `PtraceBackend` runs on current Narf. It does identify individual mechanisms a modified backend could reuse; that is distinct from current runtime support.

## Other current providers and mechanisms

Detailed per-provider facts are in `reverie-backend-matrix.md`; canonical summaries follow.

### KVM

**Software helper / Optional capability.** `reverie-kvm` has `KvmBackend`, `KvmGuest`, tool callbacks, guest memory/register abstractions, a VMM, a guest syscall executor boundary, and a static-ELF tool runner, but it does not implement the public `Backend` trait. Its source explicitly scopes the personality to x86-64 and a bounded Linux-like ABI rather than a general Linux kernel ([`reverie-kvm/src/lib.rs` lines 9-24](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-kvm/src/lib.rs#L9-L24), [`vm.rs` lines 1187-1230](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-kvm/src/vm.rs#L1187-L1230), [`runtime.rs` lines 1092-1106 and 1282-1300](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-kvm/src/runtime.rs#L1092-L1106), [lines 4176-4210](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-kvm/src/runtime.rs#L4176-L4210)). `SyscallExecutor` is an explicit syscall-policy seam; current default behavior is the in-repository bare personality, not a delegation to an arbitrary guest kernel ([`runtime.rs` lines 259-276](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-kvm/src/runtime.rs#L259-L276)).

### DynamoRIO DBT

**Software helper / Optional capability.** `reverie-dbt` launches the target under a vendored DynamoRIO client and implements `DbtGuest`, native pre/post-syscall callbacks, thread/module lifecycle, signal hooks, and CPUID/RDTSC rewriting. It does not implement `Backend` ([`launcher.rs` lines 57-118](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-dbt/src/launcher.rs#L57-L118), [`lib.rs` lines 321-447](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-dbt/src/lib.rs#L321-L447), [`native/client.c` lines 4780-4809](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-dbt/native/client.c#L4780-L4809)). The native path rejects x32 and `int 0x80` syscall ABIs ([`native/client.c` lines 3924-3952](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-dbt/native/client.c#L3924-L3952)).

### Preload runtime

**Software helper.** `reverie-preload` is a shared in-process interception runtime, not a `Backend`. Its `SyscallEvent`/`SyscallDispatcher` boundary carries syscall number, six arguments, instruction pointer, trap source, result, resume action, and PKRU; the trap path converts `SECCOMP_RET_TRAP`/`SIGSYS`/ucontext into an event and writes the result back to `RAX` ([`dispatch.rs` lines 22-201](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-preload/src/dispatch.rs#L22-L201), [`trap.rs` lines 9-20](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-preload/src/trap.rs#L9-L20)). It fail-closes uncovered exec, signal-mask/altstack, vfork, and non-null clone forms ([`dispatch.rs` lines 203-290](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-preload/src/dispatch.rs#L203-L290)). Source scope is trusted, dynamic, non-`AT_SECURE`, x86-64 execution; static binaries, loader startup, vDSO, and exec transitions require another mechanism ([`lib.rs` lines 34-40](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-preload/src/lib.rs#L34-L40)).

### LiteInst

**Verified current backend with bounded direct path.** The public `impl Backend for LiteinstBackend` delegates to `run_with_preload*`, the direct in-guest patching path with a UDS coordinator; it does not attach ptrace. That path follows process-like fork/clone3 descendants, translates vfork to a COW child, and leaves thread clone, exec rebootstrap, and unpatchable-site fallback unsupported ([`backend.rs` lines 361-405](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L361-L405), [lines 590-645](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L590-L645)). `run_host_with_preload*` is a separate optional ptrace-owned hybrid API ([`backend.rs` lines 204-359](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-liteinst/src/backend.rs#L204-L359)); current Hermit CLI explicitly selects that separate hybrid when `Backend::Liteinst` is requested ([`hermit-cli/src/lib.rs` lines 2756-2771](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/lib.rs#L2756-L2771)).

### e9patch

**Verified current backend with bounded direct path.** `E9patchBackend` rewrites the root executable and uses ptrace for lifecycle and residual coverage; non-ELF inputs fall back to ptrace. The rewrite verifies the produced manifest/artifact. A direct opt-in in-process path exists but source explicitly says it does not replace `Backend` because lifecycle coverage is incomplete ([`backend.rs` lines 398-552](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-e9patch/src/backend.rs#L398-L552), [lines 554-650](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-e9patch/src/backend.rs#L554-L650), [`rewrite.rs` lines 222-333](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-e9patch/src/rewrite.rs#L222-L333)).

### SaBRe and reverie-host

**Software helper / experimental adapter.** `experimental/reverie-host` launches `sabre plugin -- program` and hosts RPC; `experimental/reverie-sabre` adapts `Tool`/`Guest` inside the guest process. It has process/thread callbacks and direct memory/register access but no `Backend` implementation, and remote mode uses a blocking per-thread connection ([`experimental/reverie-host/src/tracer.rs` lines 23-117](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/experimental/reverie-host/src/tracer.rs#L23-L117), [`reverie_adapter.rs` lines 69-161](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/experimental/reverie-sabre/src/reverie_adapter.rs#L69-L161), [lines 320-390](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/experimental/reverie-sabre/src/reverie_adapter.rs#L320-L390)).

### Seccomp user notification

**Software helper, not a current Reverie execution backend.** `reverie-process` can install a seccomp user-notification listener, asynchronously receive notifications, validate IDs, send continue/spoof responses, and inject file descriptors. A source test launches `cat` and continues notifications ([`seccomp/notif.rs` lines 49-288](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-process/src/seccomp/notif.rs#L49-L288), [`lib.rs` lines 798-859](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-process/src/lib.rs#L798-L859)). No current `Backend` or `Guest` implementation consumes this notifier as its syscall-interposition mechanism. Its existence proves a reusable Linux helper, not backend completeness, signal/lifecycle equivalence, or Narf support.

## Revisions and runtime evidence

- Reverie: `123df7c4c0169006fbfe1f11a1553fe333eac937`.
- Vendored DynamoRIO: `929840ad9190e5086775e8debc0f0b79b4208d59`.
- Vendored e9patch: `6c2c03c1da74b14daf1788a9f8dccfa354ce04a6`.
- Vendored SaBRe: `41113f849f8799932ed8c7883f5a4de616b9e9fa`.
- LiteInst Cargo git revision: `4dab7f01d4d6d394e968fbfad842019f72743c81`.
- Narf intersection revision supplied by the Narf researcher: `afe919924a5fd2c8e820ab3a0564b6d45438e6bc`.
- No runtime backend test was executed in this fact collection. Test files and ignored-test counts are inventory evidence only, not runtime support. `/dev/kvm` existed and Rust 1.96.0 was installed, but a bounded build/run would have required fetching/building large native dependencies and would not prove Narf compatibility.

## Contradictions and unresolved facts

1. `BACKENDS.md` in the pinned tree contains links pinned to older commit `2f812...`; it was used for discovery only. Current implementation source decides current behavior.
2. Several provider READMEs describe earlier limitations that current code has since implemented (notably DBT lifecycle work, shared preload reuse by LiteInst/e9patch, LiteInst deferred handling, and SaBRe `execveat`). Those statements are historical contradictions, not current negative evidence.
3. Presence of tests, declarations, package names, or comments was never treated as runtime proof.
4. **Verified current (source-only):** pinned Narf does not evaluate seccomp
   filters (including `RET_TRACE`), rejects listener creation, and has no
   user-notification lifecycle. **Unresolved:** current Narf semantics for
   `CONT` signal reinjection, `PTRACE_SYSCALL` entry/exit alternation and
   restart, FP/XSTATE regsets, later-iovec partial `process_vm_*` transfers,
   and ptracer death remain outside the established fact set. Restart,
   cancellation, memory movement, listener lifetime, and ioctl semantics for a
   *proposed future* notifier are also unresolved. Distinct-address-space
   `process_vm_*`, RDTSC faulting, and CPUID faulting are verified current gaps,
   not unresolved.
5. **Unresolved:** a single canonical ordering across different current providers is not defined by Reverie's generic traits. A comparison tool must define it above the API.
6. **Deferred:** complete arbitrary-Linux execution is not claimed by KVM, DBT, preload, direct LiteInst, direct e9patch, or SaBRe paths merely because they implement a `Guest` or invoke `Tool` callbacks.
