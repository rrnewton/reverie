# Reverie provider/backend matrix

Exact source revision: [`rrnewton/reverie@123df7c4c0169006fbfe1f11a1553fe333eac937`](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937). “Implemented” means an execution path exists in source; it does not mean this investigation ran it. Test inventory is source-only.

## Inventory and contract status

| Provider/mechanism | Public `Backend` impl? | `Guest` impl? | Placement and controller | Current classification |
|---|---:|---:|---|---|
| ptrace | Yes, `PtraceBackend` | `TracedTask` | External same-host tracer; Linux ptrace + seccomp trace | **Verified current** reference backend |
| KVM | No | `KvmGuest` | Host VMM, one in-repo x86-64 bare personality/vCPU model | **Optional capability / Software helper** |
| DynamoRIO DBT | No | `DbtGuest` | DynamoRIO client injected into translated guest; launcher and optional global RPC | **Optional capability / Software helper** |
| preload | No | No standalone generic `Guest` | In-process DSO; seccomp `TRAP`/`SIGSYS` and trusted syscall gate | **Software helper**, shared by other mechanisms |
| LiteInst trait/direct | Yes, `LiteinstBackend` | `LiteinstGuest` | In-process runtime plus UDS coordinator; `Backend::run*` calls `run_with_preload*` | **Verified current** bounded backend |
| LiteInst ptrace-host hybrid | No separate trait impl; explicit `run_host_with_preload*` APIs | `LiteinstGuest` plus ptrace `TracedTask` control | In-process hot sites plus external ptrace lifecycle/controller; [explicitly selected by Hermit CLI](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/lib.rs#L2756-L2771) | **Optional current path** |
| e9patch hybrid | Yes, `E9patchBackend` | `E9patchGuest` | AOT root rewrite plus external ptrace lifecycle/residual coverage | **Verified current** bounded backend |
| e9patch direct | No separate impl; opt-in APIs | `E9patchGuest` | AOT rewrite and in-process DSO; UDS global RPC | **Optional capability**, incomplete lifecycle |
| SaBRe + reverie-host | No | `SabreGuest` | In-process loader/plugin plus external host RPC service | **Software helper / experimental adapter** |
| seccomp user notification | No | No | External listener FD and asynchronous notification responder | **Software helper**, no current Reverie execution backend consumer |
| `riptrace` | No | No | CLI/front end over other runtime pieces | Seen and excluded as a provider/backend |

The exhaustive inverse check found only three `impl Backend for` blocks and six execution-specific `Guest` implementations: `TracedTask`, `KvmGuest`, `DbtGuest`, `LiteinstGuest`, `E9patchGuest`, and `SabreGuest`.

## Execution and lifecycle matrix

| Provider | Boot/executable prerequisites | Root spawn and exec | fork/vfork/clone/thread lifecycle | Syscall entry/exit and mutation | Signals, concurrency, and order |
|---|---|---|---|---|---|
| ptrace | Linux-like host with `ptrace`, seccomp `TRACE`, `PR_SET_NO_NEW_PRIVS`, `personality`; on [default strict Hermit](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L1077-L1090) x86-64, functional [`PR_SET_TSC(PR_TSC_SIGSEGV)`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1638-L1650) and [`ARCH_SET_CPUID(0)`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L1615-L1633) | `TRACEME` + `SIGSTOP`, set options, exec; successful exec is a `PTRACE_EVENT_EXEC` and post-exec callback | Kernel events for fork/vfork/clone; `GETEVENTMSG` supplies TID; vfork-done and exit are explicit states; all ptrace calls remain on controller thread | Selective seccomp-entry prehook; GPR mutation; exact original syscall via `PTRACE_SYSCALL`; replacement/private injection via `SINGLESTEP`; return value mutable | `GETSIGINFO` distinguishes provenance; CPUID/RDTSC faults become subscribed Tool events; current Narf has a verified [RDTSC gap](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_prctl.rs#L407-L430) and [CPUID gap](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_arch_prctl.rs#L5-L87); controller serializes each stopped task transition |
| KVM | `/dev/kvm`, x86-64 KVM, installed ELF accepted by loader; bounded Linux-like personality, not a guest Linux kernel | Static-ELF runner/loader and VMM entry; not the generic `Command`/`Backend` contract | In-repo process/thread/wait state and ownership model exists; this is emulation inside the personality, not kernel event passthrough | Guest trap/hypercall enters `SyscallExecutor`/internal `GuestSyscallExecutor`; tool callback may execute/emulate and access guest memory/registers | Bounded signal model; reserved host RT signal; one vCPU execution core with runtime-owned scheduling/queues; not evidence of arbitrary Linux ordering |
| DBT | x86-64 DynamoRIO client/native build; rejects x32 and `int 0x80` | Launcher starts under DynamoRIO; native module/thread callbacks; not generic `Backend` | Current native client has thread init/exit and process lifecycle code; copied-child `execveat` has an `ENOSYS` path, so complete exec coverage is not established | DynamoRIO filter, pre- and post-syscall callbacks; translated registers and local memory; `Guest::set_regs` does not honor all fields (IP is ignored); injected syscalls use runtime helpers | DynamoRIO signal callback; threads execute in the translated process; global state via local or UDS path; cross-thread canonical order is not specified by generic API |
| preload | Trusted dynamic x86-64, non-`AT_SECURE`; loader startup/static/vDSO/exec uncovered unless another mechanism covers them | DSO constructor installs handler/filter; fork inherits; no complete standalone exec launcher | Dispatcher explicitly fail-closes unsupported vfork, exec and unsafe clone forms; lifecycle controller has in-process/hybrid modes but no generic backend | `SECCOMP_RET_TRAP` -> `SIGSYS` -> ucontext -> `SyscallDispatcher`; dispatcher changes result/resume and trusted gate executes allowed/injected syscall | Runs in guest signal context then normal context; signal masks/altstack guarded; not an isolation boundary; no full cross-process coordinator alone |
| LiteInst trait/direct | Dynamic non-secure x86-64; LiteInst DSO/site patching; direct coordinator/UDS | Public `Backend::run*` loads the tool/runtime through `run_with_preload*`; no ptrace attach; exec rebootstrap unsupported | Process-like fork/clone3 descendants supported; vfork translated to COW child; thread clone excluded | Patched sites call in-process tool host; unpatchable-site fallback remains unsupported on this path; local memory and synchronous tool driving | One supported guest thread per process; no general signal-handler or timer completeness; coordinator serializes global RPC, not all guest execution |
| LiteInst ptrace-host hybrid | Same binary requirements plus stock ptrace prerequisites | Separate `run_host_with_preload*` launches under ptrace and performs activation handshake; Hermit CLI calls this API | Ptrace owns lifecycle/tool and follows supported children; vfork fail-closes before either side resumes | LiteInst supplies hot-site traps while ptrace supplies lifecycle, residual transitions, and arbitrary `Guest` operations | Ptrace controller supplies signal/lifecycle order; in-process runtime remains in guest compromise domain; not a security sandbox |
| e9patch hybrid | Root must be rewriteable ELF for AOT path; otherwise ptrace fallback; e9patch/e9tool artifacts | Rewrites root into a verified temporary artifact; ptrace launches and owns exec/lifecycle | Ptrace supplies process-tree events; rewrite applies to root artifact, so later-image coverage depends on ptrace/residual paths | AOT site callback enters tool; residual subscribed syscall path uses ptrace; tool can mutate event result/register state via hybrid guest | Ptrace supplies signal and lifecycle semantics; rewritten code is in guest address space; not isolated from guest compromise |
| e9patch direct | ELF with at least one recovered site; DSO/memfd bootstrap | Direct opt-in root launch; source says it does not replace `Backend` because lifecycle is incomplete | No generic complete process-tree/exec contract; residual subscribed `SIGSYS` fails `EOPNOTSUPP` after activation | AOT callback and in-process tool; local guest memory/register frame; injection/timer/clock coverage narrower than ptrace | Synchronous in-process callbacks and optional global UDS; no independent signal/lifecycle completeness |
| SaBRe/host | x86-64 dynamic executable compatible with SaBRe loader/plugin; vendored SaBRe | Host launches `sabre plugin -- program`; adapter receives callbacks; current utility source includes `execve` and `execveat` re-entry | Adapter contains thread/process state and clone/fork callbacks; no public `Backend` completeness contract | Loader/plugin reroutes syscalls into synchronous adapter; local guest memory/registers; injection supported through adapter utilities | Remote mode uses one blocking connection per thread; timer methods do not establish actual delivery and clock is zero; no security isolation |
| user notification helper | Linux seccomp `NEW_LISTENER`, notifier ioctls, FD passing | `reverie-process::Command` can request listener on spawn; initial exec must be allowed in its test to avoid spawn deadlock | Kernel notifications include triggering TID but helper supplies no Reverie process/thread lifecycle model | Receives syscall data; response can continue, spoof value/error, or add FD; cannot by itself express generic register mutation/injected arbitrary syscall | Async listener handles concurrent notification IDs; ID validation protects PID reuse race; signal and total-order semantics remain outside helper |

## Data movement and trust matrix

| Provider | Register access | Syscall-data / memory movement | Global state and isolation/security |
|---|---|---|---|
| ptrace | `GETREGSET`/`SETREGSET`: `NT_PRSTATUS`, `NT_PRFPREG`, x86 `NT_X86_XSTATE` | PEEK/POKE for words; external-tracer `process_vm_readv/writev` for bulk; private executable trampoline for injection. [Current Narf returns `EPERM` for the required distinct-AS bulk transfer](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6940-L6967). | Tool is outside tracee, but executes host-kernel syscalls on its behalf. `NO_NEW_PRIVS`, seccomp, and `EXITKILL` are defenses, not a sandbox. |
| KVM | VCPU/guest-frame state translated into `KvmGuest` | Explicit guest physical/virtual memory copy and syscall-argument decoding; host-backed executor moves buffers | Stronger address-space boundary at KVM/VMM layer, but VMM and syscall executor are trusted computing base; no guest Linux kernel in current path. |
| DBT | DynamoRIO mcontext translated to `user_regs_struct`; some setter fields limited | Same-process direct copies, guarded reads including self `process_vm_readv`; RPC for shared state | Client and tool code live in guest process; no hostile-guest isolation. UDS/global server authenticates protocol boundaries only to implemented degree. |
| preload | Ucontext/trap frame | Same-address-space pointers plus trusted syscall gate; event contains number/args/IP/result/source | Tool/runtime share guest compromise domain; expressly intended for trusted execution. |
| LiteInst trait/direct and ptrace-host hybrid | Hook context/guest frame; only the separate hybrid can use ptrace full regsets | Direct path uses same-process access and UDS; separate hybrid can use external ptrace memory paths | In-process runtime is not isolated; hybrid ptracer is external but depends on guest DSO integrity and activation proof. |
| e9patch | AOT hook frame; hybrid ptrace state | Same-process direct access at sites; rewritten artifact/manifest; UDS global RPC; ptrace residual path | Rewriter, DSO, and tool are trusted. Rewritten guest can corrupt in-process state; hybrid controller reduces but does not remove that risk. |
| SaBRe | Adapter-created guest frame with setter restrictions | Same-process direct access and `process_vm_readv` checks; per-thread host RPC | Loader/plugin shares guest process. Blocking host RPC is a coordination channel, not an isolation boundary. |
| user notification helper | No generic register API; kernel supplies syscall metadata | Supervisor must separately read target memory (the code documents `/proc/[tid]/mem` + ID validation); `ADDFD` moves FDs | Supervisor is external, but `CONTINUE` has time-of-check/time-of-use risk for pointer arguments; ID validation only addresses PID reuse/liveness race. |

## Required ptrace request/option/event matrix

This table describes stock `PtraceBackend`, not an idealized minimal ptrace protocol.

| Mechanism | Stock use | Required semantics | Current Narf intersection supplied by Narf researcher |
|---|---|---|---|
| `PTRACE_TRACEME` | Root pre-exec | Parent becomes ptracer; child stops at explicit `SIGSTOP` | Implemented |
| `PTRACE_SETOPTIONS` | Once on root, inherited to children | Every requested option below must actually change event delivery | Request implemented; only `TRACESYSGOOD` acts |
| `O_TRACESYSGOOD` | Decode syscall stops | `PTRACE_SYSCALL` stop signal is `SIGTRAP|0x80` | Implemented/acting; exact alternation unverified |
| `O_TRACESECCOMP` / EVENT_SECCOMP | Primary subscribed syscall entry | Event before execution for `SECCOMP_RET_TRACE`; high-status encoding | Not implemented; **fatal for stock path** |
| `O_TRACEEXEC` / EVENT_EXEC | Root and later successful exec | Stop with former TID obtainable via `GETEVENTMSG` | Not implemented; **fatal** |
| `O_TRACEFORK` / EVENT_FORK | Process child | Parent event with authoritative child PID; child enters ptrace stop | Not implemented; **fatal for process-tree contract** |
| `O_TRACECLONE` / EVENT_CLONE | Thread/process clone | Same, preserving operation kind | Not implemented; **fatal for thread/process tree** |
| `O_TRACEVFORK` / EVENT_VFORK | Vfork child | Parent-child ownership and vfork blocking semantics | Not implemented; fatal when used |
| `O_TRACEVFORKDONE` / EVENT_VFORK_DONE | Release vfork parent | Event after child exec/exit | Not implemented; fatal when used |
| `O_TRACEEXIT` / EVENT_EXIT | Pre-final-exit ownership | Tracee alive/stopped; registers readable; continue permits final exit | Not implemented; **fatal to current state machine** |
| `O_EXITKILL` | Tracer-death cleanup | Kernel kills every tracee if tracer exits | Accepted/ignored; cleanup/security gap |
| `PTRACE_GETEVENTMSG` | Fork/vfork/clone/exec metadata | Child PID or former TID corresponds to current stop | Missing; **fatal with those events** |
| `PTRACE_GETSIGINFO` | Signal/perf/timer provenance | Correct `siginfo_t`, including trap origin | Missing; **fatal to stock signal/timer paths** |
| `PTRACE_CONT` with signal | Resume and signal tool outcome | Zero suppresses; chosen nonzero signal is delivered with Linux rules | Request exists; exact behavior unresolved |
| `PTRACE_SYSCALL` | Exact original injection result | Stop at next syscall exit with `TRACESYSGOOD`; restart rules Linux-compatible | Exists; exact behavior unresolved |
| `PTRACE_SINGLESTEP` | Skip original/private injection | Executes one instruction and reports appropriate trap/signal without losing intervening signals | Exists; exact behavior unresolved |
| `GET/SETREGSET NT_PRSTATUS` | Read/mutate GPR, syscall nr/args/result/IP | Complete x86-64 `user_regs_struct` with orig syscall semantics | Implemented according to supplied inventory |
| `NT_PRFPREG`, `NT_X86_XSTATE` | FP/core and xstate preservation | Round-trip complete register blobs | Not reported; unresolved |
| PEEK/POKE | Word memory | Linux error/value conventions and unaligned/page semantics used by wrapper | Implemented according to supplied inventory; semantics unverified |
| [`process_vm_readv/writev`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/safeptrace/src/memory.rs#L62-L125) | Bulk memory | Cross-process partial-transfer and permission behavior compatible with Linux | **Verified current source gap, fatal for stock external bulk path:** [Narf returns `EPERM` for a distinct address space](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6940-L6967); later-iovec partial-count fidelity remains unresolved |
| [`PR_SET_TSC(PR_TSC_SIGSEGV)`](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/tracer.rs#L1638-L1650) | Default strict Hermit RDTSC subscription | Disable userspace RDTSC so the backend receives the fault and invokes the Tool | **Verified current source gap:** [Narf accepts mode 2 but leaves RDTSC enabled](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_prctl.rs#L407-L430); default RDTSC events are silently absent |
| [`ARCH_SET_CPUID(0)` / CPUID faulting](https://github.com/rrnewton/reverie/blob/123df7c4c0169006fbfe1f11a1553fe333eac937/reverie-ptrace/src/task.rs#L1615-L1633) | Default strict Hermit CPUID subscription | Disable direct CPUID and deliver the resulting fault to the Tool path | **Verified current source gap:** [Narf supports FS/GS `arch_prctl` only and returns `EINVAL`](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_arch_prctl.rs#L5-L87); default CPUID events are absent |
| `PTRACE_SEIZE/INTERRUPT/LISTEN` | No normal root dependency found | Needed only by attach/group-stop variants | Missing but avoidable for normal `TRACEME` root |
| `PTRACE_GET_SYSCALL_INFO` | No consumer found | Not required by current code | Missing but avoidable |
| `PTRACE_SETSIGINFO` | Wrapper exists; normal path did not call it | Not required for ordinary current delivery | Avoidable for base path |

## Test and runtime status

The source inventory found the following matching test files/ignored annotations. This is only a map of test material, not evidence that tests passed at this revision or on Narf.

| Provider | Matching test files | `#[ignore]` annotations found | Investigation execution |
|---|---:|---:|---|
| ptrace | 7 | 1 | Not run |
| KVM | 59 | 0 | Not run |
| DBT | 14 | 36 | Not run |
| preload | 3 | 0 | Not run |
| LiteInst | 6 | 1 | Not run |
| e9patch | 4 | 15 | Not run |
| SaBRe | 9 | 0 | Not run |

`reverie-process` also contains a seccomp-user-notification source test; `validate.sh` skips `tests::seccomp_notify`, so its presence cannot be promoted to current validation evidence.
