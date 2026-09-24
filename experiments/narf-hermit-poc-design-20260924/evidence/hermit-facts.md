# Hermit and Linux-equivalence fact set

Status: evidence collection for Audit A. This is not the reader-facing design
and makes no backend recommendation.

## Revision boundary

- **Verified current:** parent `dev-hermit` pins `rrnewton/hermit` at
  `ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2` and its separate
  `rrnewton/reverie` checkout at
  `123df7c4c0169006fbfe1f11a1553fe333eac937`.
- **Verified current:** the Hermit commit itself pins the Reverie submodule used
  by its Cargo build at `ae1d1da78d2d89a1fa0454ee2282cf135e2286ed`.
  This is seven descendants beyond `123df7c…`. The material `Tool`, `Guest`,
  `Backend`, `task.rs`, `safeptrace`, and memory-access files are byte-identical
  between those two Reverie commits; `reverie-ptrace/src/tracer.rs` differs in a
  startup-exit error fix. Ptrace options and interception flow cited below are
  present at the build pin `ae1d1da…`. This revision distinction is load-bearing
  and must not be collapsed in Audit A.
- **Verified current:** Linux comparison text is Linux man-pages commit
  `227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7`.
- Public pinned roots: [Hermit](https://github.com/rrnewton/hermit/tree/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2),
  [Hermit's Reverie build pin](https://github.com/rrnewton/reverie/tree/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed),
  [separately pinned Reverie](https://github.com/rrnewton/reverie/tree/123df7c4c0169006fbfe1f11a1553fe333eac937),
  and [Linux man-pages](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/commit/?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7).

Runtime evidence: none was executed in this research lane. All claims below are
source/specification evidence unless explicitly labeled otherwise. No source
declaration or test name is treated as proof of runtime support.

Claim labels follow the frozen contract: **Verified current** is pinned source
or primary-spec behavior; **Proposed** is a candidate contract;
**Optional capability** is valid only if advertised; **Software helper** is
composed support rather than the transport itself; **Above the API** is fixture
or orchestration policy; **Deferred** is intentionally outside the first POC;
and **Unresolved** lacks enough primary evidence.

## Actual Hermit execution and event path

1. **Verified current:** the CLI enum exposes six spellings: `ptrace` (default),
   `dbt`, `liteinst`, `sabre`, `kvm`, and `e9patch`. The enum itself says
   e9patch preprocesses the main ELF and then uses ptrace; a name in this enum
   is not runtime evidence. [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/lib.rs#L1029-L1046)
2. **Verified current:** dispatch has dedicated KVM, DBT, SaBRe, and LiteInst
   paths. The residual/default path constructs
   `reverie_ptrace::TracerBuilder::<Detcore>`, spawns it, waits, and cleans the
   shared global state. [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/lib.rs#L2716-L2794)
3. **Verified current:** the Reverie `Backend` contract already supplies a
   backend-level Rust extension boundary. A conforming implementation must host
   arbitrary `T: Tool`, initialize one global state, compute subscriptions once,
   supervise the whole fork/clone/vfork/exec tree, allocate process/thread
   state, route subscribed syscall/signal/timer/CPUID/RDTSC events, run exit
   hooks, return final state, capture output, and report real activity stats.
   [source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie/src/backend.rs#L60-L111)
4. **Verified current:** the generic trait surface is `Backend::run`,
   `run_with_stats`, and `run_with_output`; its future is explicitly `?Send`
   because the ptrace implementation must remain on a current-thread executor.
   [source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie/src/backend.rs#L208-L289)
5. **Verified current:** `Tool::subscriptions` defaults to all syscalls, and
   Detcore's default fail-closed run configuration returns
   `Subscription::all()`. Reverie's `all()` includes every syscall plus CPUID
   and RDTSC instruction events. A syscall handler fires before execution and
   can inspect/mutate memory and registers and either inject a syscall or return
   an emulated result. The same trait has thread-start, post-exec, signal,
   structured-signal, timer, CPUID, RDTSC, and exit hooks.
   [Detcore source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L1077-L1090),
   [subscription source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie/src/subscription.rs#L35-L44),
   [Tool source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie/src/tool.rs#L401-L616)
6. **Verified current:** `Guest<T>` is the tool-facing data/motion boundary. It
   provides task identity, config, process/thread/global state, guest memory,
   registers, syscall injection/tail injection, lifecycle operations, timer
   control, and signal/process control. This surface is broader than “a syscall
   callback” and is the contract any new backend must actually satisfy.
   [source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie/src/guest.rs#L58-L185)
7. **Verified current:** Detcore logs a syscall prehook using deterministic
   thread identity, displayed arguments, and an optional scheduling event;
   every intercepted syscall increments a per-thread ordinal and logical time.
   [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L1847-L1904)
8. **Verified current:** after handling, Detcore logs syscall number/arguments,
   result or errno, optional registers, memory-map evidence, return-bounded I/O
   buffer hashes, a scheduler posthook, and canonicalizes syscall-clobbered
   registers unless the backend owns that guarantee.
   [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L2763-L2832)

## Exact ptrace and adjacent Linux surface

The “normal path” column means the `TracerBuilder::<Detcore>` launch above, not
every helper method present in `safeptrace`.

| Linux facility | Normal Hermit ptrace use | Why it matters | Narf-compatible acceptance primitive |
| --- | --- | --- | --- |
| `PTRACE_TRACEME` + initial `SIGSTOP` | Required | Child enters a controlled stop before exec; parent then installs options. | Launch `/bin/true`; prove the initial stop is distinguishable and resumable. |
| `PTRACE_SETOPTIONS` | Required with `TRACEEXEC`, `EXITKILL`, `TRACECLONE`, `TRACEFORK`, `TRACEVFORK`, `TRACEVFORKDONE`, `TRACEEXIT`, `TRACESECCOMP`, `TRACESYSGOOD` | These are not advisory: Reverie dispatches lifecycle and syscall-entry behavior from their events. | Assert each requested option either has Linux semantics or fails explicitly; “accepted but ignored” is a failure. |
| `PTRACE_EVENT_SECCOMP` from seccomp `RET_TRACE` | Required | This is the ordinary subscribed-syscall prehook. The filter allows unlisted syscalls and traces listed ones; `rt_sigreturn` and a private injection IP range are allowed. | Subscribe only to `getpid` and `getrandom`; observe exactly one prehook per call, then exact result/buffer. |
| `PTRACE_GETEVENTMSG` | Required for fork/vfork/clone child identity and nonleader-exec former TID | `safeptrace::Event` immediately reads the child/former identity when decoding the event. | Run fork, vfork, clone-thread, and nonleader-exec cases; bind every event to the correct new/former TID. |
| `PTRACE_EVENT_{FORK,VFORK,CLONE,VFORK_DONE,EXEC,EXIT}` | Required for nontrivial programs | Reverie creates per-process/thread Tool state, preserves vfork blocking, handles exec identity change, and runs exit hooks from these stops. | A process-tree guest must show parent/child start order, exec transition, final status, and exactly-once teardown. |
| `PTRACE_GETREGSET` / `PTRACE_SETREGSET` (`NT_PRSTATUS`, `NT_PRFPREG`, and x86 `NT_X86_XSTATE`) | Required | Syscall decode/result, emulation, injected calls, signal frames, CPUID/RDTSC interception, and context preservation read or replace registers. | Replace one syscall, check all six args and return; exercise FP/XSTATE across a signal and injection. |
| `PTRACE_CONT` with optional signal | Required | Resumes ordinary, lifecycle, syscall, and signal-delivery stops; delivers or suppresses a selected signal. | Deliver, replace, and suppress a queued signal and compare `siginfo`, handler execution, and restart result. |
| `PTRACE_SINGLESTEP` | Required | Reverie suppresses an intercepted original syscall by setting its number to -1 and single-stepping; private-page injection also steps. | Replace `getpid` with emulation and an injected syscall; original must execute zero times. |
| `PTRACE_SYSCALL` + `TRACESYSGOOD` | Required on injection/original-syscall paths | Runs an exact pending syscall to its exit and distinguishes syscall stops from ordinary `SIGTRAP`. | Inject a syscall that returns an error and one with a partial buffer result; observe exact exit-stop/result. |
| `PTRACE_GETSIGINFO` | Required for controller trap/timer provenance | SIGTRAP classification and the PMU timer path read stop provenance; the ordinary ptrace guest-signal hook currently passes a `Signal` enum, not full `siginfo_t`, to the Tool. | Exercise breakpoint/single-step classification and a PMU timer; separately test guest-observable queued-signal payload/order as a semantic acceptance case. |
| `PTRACE_PEEKDATA` / `PTRACE_POKEDATA` and `process_vm_readv/writev` | Required by current out-of-process memory implementation | Small transfers use ptrace words; larger transfers use process-vm calls from the tracer to the distinct tracee address space. Linux permits partial remote-page transfers and does not make them atomic. | First transfer a known buffer larger than eight bytes across distinct tracer/tracee address spaces; then cross an inaccessible page and compare the exact partial length/bytes/`EFAULT` behavior. |
| `prctl(PR_SET_TSC, PR_TSC_SIGSEGV)` | Required when RDTSC is subscribed; required by default Detcore | Reverie requests faulting before exec and rejects launch if the request fails. The resulting fault is needed to deliver `handle_rdtsc_event`. | Execute `rdtsc` under the default subscription; require one Tool event and the Tool-selected value rather than native execution. |
| `arch_prctl(ARCH_SET_CPUID, 0)` | Required when CPUID is subscribed; required by default Detcore | Reverie injects the request after process setup and verifies the disabled state before treating CPUID interception as active. | Execute `cpuid` under the default subscription; require one Tool event and the Tool-selected register tuple rather than native execution. |
| `PTRACE_DETACH` | Error/cleanup path | Reverie attempts to detach a stopped task after handler failure. | Force a Tool error; prove no stopped task or zombie remains. |
| `PTRACE_O_EXITKILL` | Safety/lifecycle requirement | Linux kills tracees if the tracer exits. | Kill the tracer mid-run; no guest may escape and continue. |
| `PTRACE_SEIZE`, `PTRACE_INTERRUPT`, `PTRACE_LISTEN` | **Not used by the normal Hermit launch** | `safeptrace` exposes seize/interrupt, but `TracerBuilder` uses TRACEME. No production reference to LISTEN was found. | Do not gate the first TRACEME-compatible experiment on these requests; keep the inverse search in the ledger. |
| `PTRACE_GET_SYSCALL_INFO` | **Not used** | Reverie decodes registers instead. | Not a first-path requirement; absence cannot compensate for missing event/register semantics. |

**Verified current:** the exhaustive production-path request set found by the
paired source searches is `TRACEME`, `SETOPTIONS`, `GETREGSET`, `SETREGSET`,
`PEEKDATA`, `POKEDATA`, `CONT`, `SINGLESTEP`, `SYSCALL`, `GETSIGINFO`,
`GETEVENTMSG`, and cleanup `DETACH`, plus wait-status collection and the
non-ptrace `process_vm_readv/writev` memory fast path. `SETSIGINFO` and
`PEEKSIGINFO` are exposed by `safeptrace` but have no normal-task call site;
`ATTACH`, `SEIZE`, and `INTERRUPT` occur in auxiliary/test paths rather than the
normal TRACEME launch; `LISTEN` and `GET_SYSCALL_INFO` are absent.

Primary implementation evidence: [tracer initialization and option mask](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/tracer.rs#L1658-L1867),
[seccomp filter](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/tracer.rs#L1896-L1918),
[event decoding and `GETEVENTMSG`](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/safeptrace/src/lib.rs#L144-L243),
[register/resume operations](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/safeptrace/src/lib.rs#L617-L835),
[syscall callback](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/task.rs#L4065-L4199),
[suppression/injection](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/task.rs#L5190-L5353),
and [remote memory](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/safeptrace/src/memory.rs#L62-L125).
The adjacent default instruction setup is
[`PR_SET_TSC`](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/tracer.rs#L1668-L1678)
and
[`ARCH_SET_CPUID`](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/task.rs#L1624-L1633).

Linux comparison contract: [ptrace(2)](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/tree/man/man2/ptrace.2?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7),
[process_vm_readv(2)](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/tree/man/man2/process_vm_readv.2?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7),
and [wait(2)](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/tree/man/man2/wait.2?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7).

### Ptrace gaps cross-checked against the Narf lane

- **Verified current (cross-agent canonical fact):** Narf commit `afe9199…`
  implements only a traditional ptrace
  subset; `SEIZE`, `INTERRUPT`, `LISTEN`, `GETSIGINFO`, `GETEVENTMSG`, and
  `GET_SYSCALL_INFO` are absent; fork/clone/exec/exit option bits are accepted
  but ignored; no `PTRACE_EVENT_*` stops are produced; only `TRACESYSGOOD` has
  effect; syscall stops are x86_64-only.
- **Verified current implication:** on those facts, the current Hermit ptrace
  path cannot work merely because Narf has a syscall named `ptrace`. Missing
  SEIZE/INTERRUPT/LISTEN/GET_SYSCALL_INFO are avoidable for the first normal
  launch, but missing `PTRACE_EVENT_SECCOMP`, lifecycle events,
  `GETEVENTMSG`, and controller `GETSIGINFO` contradict current required behavior.
- **Verified current (source-only), fatal for the stock out-of-process bulk
  memory path:** pinned Reverie's `MemoryAccess` calls
  `process_vm_readv/writev` from the tracer to its distinct tracee. Pinned Narf's
  `process_vm_transfer` permits only self or a `CLONE_VM`/same-address-space
  target and returns `EPERM` for a distinct target address space. Therefore the
  stock bulk path cannot move tracee syscall data on current Narf even if the
  ptrace event gaps are repaired.
  [Reverie source](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/safeptrace/src/memory.rs#L62-L125),
  [Narf source](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6880-L7038)
- **Unresolved:** Narf's same-address-space implementation gathers before it
  scatters and returns `EFAULT` when a later segment faults; whether every
  page-boundary case matches Linux's permitted partial byte count remains a
  separate runtime question. This uncertainty does not weaken the known
  distinct-address-space `EPERM` incompatibility.
- **Verified current (source-only), stock default instruction-interception
  gaps:** Detcore's default `Subscription::all()` requires CPUID and RDTSC
  events. Reverie asks for `PR_TSC_SIGSEGV` and injects
  `ARCH_SET_CPUID(0)`. Pinned Narf accepts `PR_SET_TSC` values 1 and 2 but
  explicitly does not disable RDTSC, and its `arch_prctl` handles FS/GS cases
  only, returning `EINVAL` for `ARCH_SET_CPUID`. A default strict run therefore
  cannot receive either required Tool instruction event on current Narf.
  [Narf TSC source](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_prctl.rs#L407-L431),
  [Narf arch_prctl source](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_arch_prctl.rs#L5-L88)
- **Proposed:** a deliberately reduced experiment could disable CPUID/RDTSC
  subscriptions only if its declared claim and guest exclude those
  instructions. That is a narrower experiment, not evidence of stock default
  compatibility.
- **Unresolved:** exact Narf behavior for `GET/SETREGSET`, FP/XSTATE, ptrace
  word-memory operations, wait status encoding, seccomp `RET_TRACE`, perf
  events, and `EXITKILL` remains owned by the Narf evidence lane. No support is
  inferred here.

## Linux seccomp user-notification comparison

- **Verified current:** Linux user notification stops the target before the
  syscall, reports TID, number, six arguments and instruction metadata through
  a listener FD, lets a supervisor send a return/errno, continue the real
  syscall, or atomically add a file descriptor. It is not ptrace and does not
  itself provide the complete Reverie lifecycle/register/timer contract.
- **Verified current:** its primary specification warns that continued syscalls
  have a TOCTOU race; target memory may become invalid whenever a signal abandons
  the blocked call; supervisor writes to target memory can never be considered
  safe under that protocol; blocking emulation must detect target cancellation;
  and `SA_RESTART` can create multiple notifications for one logical syscall.
  [seccomp_unotify(2)](https://git.kernel.org/pub/scm/docs/man-pages/man-pages.git/tree/man/man2/seccomp_unotify.2?id=227f4c2fd6c421ad8e4f81ccf1acad1a139fe3a7)
- **Software helper:** Reverie contains reusable user-notification structures
  and ioctls in `reverie-process`, but the actual Hermit ptrace path uses
  seccomp `RET_TRACE` plus `PTRACE_EVENT_SECCOMP`, not `USER_NOTIF`. Source
  availability is not runtime adoption.
- **Optional capability:** USER_NOTIF can be considered only if a provider
  advertises and passes listener, ioctl, cancellation, memory, lifecycle and
  security tests; a type definition is not an advertised provider capability.
- **Verified current (source-only):** pinned Narf does not evaluate seccomp
  filters, rejects `SECCOMP_FILTER_FLAG_NEW_LISTENER`, and has no notification
  object/ioctl lifecycle in the repository-wide source inventory. This is
  current absence/inertness, not an executed-runtime result.
- **Unresolved:** restart/cancellation, safe memory/data movement, listener
  lifecycle, and ioctl semantics for any *proposed future* Narf notifier.

## Inputs that bound a Linux-versus-Narf equivalence claim

| Input/state class | Verified Hermit/Linux fact | Consequence for comparison |
| --- | --- | --- |
| Executable, loader, argv, env, cwd | record metadata persists executable, argv, cwd, hostname/domainname, env, version, and mount provenance. | Both sides need byte-identical executable/dependencies/argv/env and the same guest-visible working tree. |
| Syscall inputs | handler receives syscall number and six ABI arguments before execution. | Compare number, typed scalar/flag values, and pointed-to input bytes before any result. |
| Syscall outputs | integer return/errno and mutable output buffers are separately observable; record schema contains bytes, stat/statx, mmap, poll, socket, ioctl, time, exec, and I/O events. | Return equality without buffer equality is insufficient. Persistent kernel side effects must be observed by follow-up calls. |
| Process/thread lifecycle | whole fork/clone/vfork/exec/exit tree is part of Backend/Tool. | Stable logical identities, parentage, start/exec/exit ordering, status, waitability, robust-list and clear-tid behavior belong in the trace. |
| Scheduling | Detcore increments logical time at every intercepted syscall and records pre/post scheduler control points when sequentializing. PMU RCB timers add preemption inputs. | Fix configuration/seeds/PMU availability; compare turns, runnable choice, block/unblock reason, RCB position and syscall phase. |
| Signals | ptrace exposes signal-delivery stops; Tool may filter/replace/suppress; Linux restart rules differ per syscall and `SA_RESTART`. | Compare signal number, queue domain, `siginfo`, mask/disposition, chosen recipient, handler boundary, EINTR/restart, and ordering. |
| Time | strict run virtualizes time; record/replay deliberately captures real time and only proves replay fidelity. | Cross-kernel functional equivalence must use strict virtual time (or explicitly compare one recording only); never infer independent-run equality from record/replay verification. |
| Randomness and auxv | Detcore owns a seeded PRNG and initial `AT_RANDOM` handoff; `getrandom` writes deterministic bytes while preserving flags, fault, partial-write semantics. | Compare exact bytes and partial/error behavior under the same RNG seed; include `AT_RANDOM`/loader state. |
| Filesystem | host contents and metadata can enter through path syscalls, directory order, stat/inodes, procfs, mountinfo/fdinfo, mappings and executable dependencies. | Use an immutable identical root or capture every read; compare guest paths, bytes, offsets, flags, identities, ordering and mutations. |
| Network | local loopback is the default; host networking exposes changing external state. Detcore config's `network_trace` is explicitly inert absent integration. | First equivalence tests should be network-free or deterministic loopback; packet bytes/order/peer identity/readiness must be controlled for later tests. |
| Hardware/kernel personality | CPUID, RDTSC, PMU counts, uname, proc/auxv, page size, ABI, syscall table, ASLR, vDSO and signal frame differ by platform. | Pin x86_64 ABI and expose/canonicalize a declared virtual personality; otherwise classify mismatch as environment/capability, not guest semantic divergence. |
| stdout/stderr/exit | current strict reports carry exact hashes/lengths and exit code/signal separately from the verdict. | Canonical trace acceptance must require exact stdout, stderr, and final disposition in addition to internal events. |

## Current trace and record/replay evidence

- **Verified current:** strict `BitwiseInfoV1` removes only the real wall-clock
  log prefix, rewrites explicitly marked host addresses to first-appearance
  ordinals, keeps scheduler turns, virtual time, syscall values, counts, sizes,
  flags and the remainder exact, requires complete nontruncated structured INFO
  records, and requires I/O-buffer content hashes. [policy](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/bin/hermit/verify.rs#L324-L455),
  [renderer](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/logdiff.rs#L540-L585)
- **Verified current:** a `Matched` verdict is distinct from guest exit status;
  current typed outcomes also distinguish `Diverged`, `NoResult`, and
  `InfrastructureError`, and retain exact stdout/stderr digests, counts, status,
  compared-message counts and first-divergence positions.
  [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/canonical_verdict.rs#L6-L132)
- **Verified current:** the record/replay event schema captures selected syscall
  mutable outputs and return values, but source coverage is enumerated rather
  than assumed complete. [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/event.rs#L37-L107)
- **Verified current:** `record_or_replay_config` deliberately leaves virtual
  time off and says its guarantee is replay of one recording, not agreement
  between independent recordings. [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/hermit-cli/src/metadata.rs#L297-L344)
- **Proposed:** use the current strict observation discipline as a starting
  schema for a cross-kernel trace, not the raw log text and not the record file
  alone. The field and normalization contract is in `trace-input-matrix.md`.
- **Above the API:** producing byte-identical executable/filesystem/network
  fixtures and preserving their digests is harness responsibility; the
  `Backend` trait does not manufacture equivalent external worlds.
- **Deferred:** uncontrolled external networking, performance equivalence, and
  equivalence-up-to-arbitrary guest-address renaming are outside the smallest
  trace POC.
- **Unresolved:** current `BitwiseInfoV1` compares two executions produced by
  one Hermit backend/configuration. It does not by itself prove that a Linux
  log and a future Narf log share event coverage, semantic field definitions,
  or equivalent failure boundaries. A cross-provider coverage receipt is needed.

## Divergence and failure facts

- **Verified current:** Hermit's source taxonomy distinguishes pure-clock
  divergence, true guest/control-flow divergence, and pure-observation payload
  divergence; identical event counts do not rule out payload divergence.
  [source](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/docs/DIVERGENCE_CLASSES.md#L1-L59)
- **Proposed:** cross-kernel comparison must additionally distinguish semantic
  mismatch, trace-coverage mismatch, unsupported capability, backend/kernel
  fault, harness/infrastructure failure, comparison refusal/truncation, and
  uncontrolled-input mismatch. These are separate outcomes; none may be
  relabeled “equivalent.”

## Facts the writing audit must not reinterpret

1. A Narf `ptrace` symbol is not evidence that current Hermit can run.
2. Current Hermit depends on seccomp ptrace events and lifecycle events, not only
   entry/exit syscall stops.
3. `SEIZE`, `INTERRUPT`, `LISTEN`, and `GET_SYSCALL_INFO` are not prerequisites
   of the normal TRACEME path; their absence must not obscure the fatal gaps.
4. Return/errno equality without output-buffer and persistent-effect equality is
   not functional equivalence.
5. Record/replay verification at this revision is one-recording replay fidelity,
   not independent Linux-versus-Narf equality.
6. Source inspection was performed; no Hermit-on-Narf runtime was executed.
7. Proposed trace normalization is not current behavior and cannot be reported
   as implemented support.
8. Distinct-address-space `process_vm_*` bulk access and default CPUID/RDTSC
   delivery are current stock prerequisites with verified Narf source gaps, not
   unresolved optional capabilities.
