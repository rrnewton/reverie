# Reverie LiteInst

`reverie-liteinst` is an experimental Linux x86-64 Reverie backend built on the
standalone `liteinst2` patching library, the shared `reverie-preload` runtime,
and `reverie-rpc-transport`.

## Event path

1. A tool-specific DSO registers its preload constructor with
   `tool_root_constructor!(initialize)` and calls `install_tool::<T>` only from
   that constructor body. An ordinary executable fixture makes an explicit
   unsafe call to `with_tool_root!` around the complete setup body instead.
2. `reverie-preload` installs the SIGSYS handler, alternate stack, trusted
   syscall gate, and seccomp filter.
3. The launcher deterministically selects the lowest CPU in its allowed set and
   installs that singleton affinity in the child before exec. `CoordinatorRpc`
   then captures the root's native PMU type/config exactly once while CPUID is
   still native and requires the authenticated supervisor EVENT (or unsupported
   response) to match. The matched CPU/profile is inherited across fork and
   checked against every child EVENT without executing CPUID while the child
   Tool is rebuilt. Each separately launched exec image has a new RPC/event
   lifetime and performs its own one capture after the previous process and
   supervisor have torn down the prior event. No successful exec crosses a live
   event: the event is `CLOEXEC`, and the current in-process runtime still
   refuses a later guest `execve` before replacing the loaded runtime.
4. The first syscall at an instruction reaches SIGSYS. The LiteInst dispatcher
   installs a replace-first hook and changes the saved signal-context RIP to the
   generated trampoline entry. An unpatchable syscall uses the deferred entry
   described below, leaving its instruction bytes intact.
5. After `sigreturn`, the trampoline invokes `T::handle_syscall_event` in normal
   guest context. The first invocation and later patched invocations therefore
   use the same tool path; the first site trap is not also a tool execution.
6. `LiteinstGuest<T>` supplies in-process memory/register access and syscall
   injection through the trusted gate. `CoordinatorRpc<G>` serializes
   `GlobalRPC` messages over the same UDS/bincode framing as
   `reverie-rpc-transport::RpcServer<G>`. The launcher accepts concurrent local
   connections against the one coordinator-owned global state and cancels any
   outstanding connection tasks when the guest run ends.

The cross-crate counter bridge is private Rust API and its hidden native entry
requires the address of a LiteInst-private process allocation that root entry
binds before calling external setup. A wrong address is refused with `EPERM`.
This boundary protects safe Rust consumers and separately loaded DSOs. Code
already linked into the same DSO is trusted native code: it can read process
memory and issue the same kernel operations directly, so the bridge does not
claim to isolate a hostile object within that address space.

Root assembly captures the exact incoming signal mask and blocks asynchronous
signals before its first Rust call, global access, allocation, or frame
publication. A constructor that does not select LiteInst restores that captured
mask byte for byte. A selected runtime may clear only SIGSYS, plus SIGSEGV when
instruction emulation installs its handler. The setup thunk uses the non-unwind
`extern "C"` ABI, so a panic or panicking setup destructor aborts before the
assembly frame can return to guest execution.

Before the event can first become `RUNNING`, Tool mode resets inherited callable
signal dispositions and the active filter refuses later callable actions and
mask mutations. The runtime-owned SIGSYS action, and SIGSEGV action when
instruction emulation is selected, install a full action mask. At every
installed callback, SIGSYS entry, and runtime SIGSEGV entry, a straight-line
initial-exec TLS lookup and direct `PERF_EVENT_IOC_DISABLE` occur before the
first conditional branch. A failed disable while the record says `RUNNING`
selects `exit_group` without a conditional branch and cannot produce a result.
The entry then captures and blocks the incoming mask, keeps it blocked through
Tool work, enables the event before publishing `RUNNING`, and restores the exact
mask only after publication. The installed callback relies on the pre-activation
callable-action refusal; kernel signal entry additionally has the full action
mask before its first instruction. Synchronous fault delivery remains available.

The signal qualification has two distinct causal edges. The supervisor sends
a preblocked SIGWINCH after target assembly has published `RUNNING`; it must
remain pending with zero handler entries until the PAUSED Tool callback releases
it. The supervisor also asynchronously sends runtime-owned SIGSYS while
`RUNNING`, before the wrapper's explicit mask syscall; the assembly entry must
report PAUSED and the unchanged absolute first clock proves that no conditional
handler branch retired before physical DISABLE. The older post-enable SIGUSR1
edge remains and must observe `RUNNING`.

The supervisor creates the event with both the authenticated target TID and the
explicit selected CPU; perf's `pinned` attribute is not treated as task
affinity. It validates the target's exact singleton mask immediately before and
after `perf_event_open`, while the target checks both its mask and current CPU
before profile capture, acquisition, root enable, and every later re-enable.
The supervisor never discovers a PMU profile on its own CPU. Guest syscalls,
nested Tool syscalls, and Tool-injected syscalls cannot change affinity. Fork
inherits the CPU/profile and obtains a distinct child event on that CPU.
Affinity interfaces wider than the authenticated 1024-bit representation are
refused, and loss of the selected CPU before exec is a typed launch failure.

Same-UID or privileged processes that can externally change target affinity,
and host administration that changes cpusets, CPU online state, or PMU topology
during a run, are outside the in-process trust boundary. The next checked
boundary fails terminally, but branches could already have run uncounted after
an external migration, so such a run earns no result or determinism credit.
This design does not claim to prevent hostile host mutation.

Linux stable v7.1.3 marks perf VMAs `VM_DONTCOPY`. Fork qualification binds the
parent metadata address and requires the custom running kernel's child to
report `ENOMEM` from `mincore`, `EFAULT` from self `process_vm_readv`, and zero
overlap in `/proc/self/maps` before acquiring its new event. The parent must
retain the same authenticated event ID and owner with an advancing clock. The
copied Rust owner is intentionally invalidated without Drop after the child's
single inherited-FD close, since there is no child VMA to unmap and the numeric
slot may subsequently be reused.

The CPUID-subscribed fork control deliberately virtualizes every guest CPUID to
an unrecognized synthetic model. It requires zero Tool CPUID callbacks between
the child acquisition begin/complete markers, then exactly one callback for the
deliberate post-acquisition guest instruction. Separate corrupt-supervisor
controls require root refusal for a false unsupported response and for a real
event whose advertised type and config are each changed by one bit.

The regression proof reports `calls=32 traps=1 hooks=32` and sends a real
Reverie tool RPC for every callback.

### Unpatchable syscalls

In typed Tool mode, a syscall without a supported patch window returns from the
real kernel SIGSYS frame into an ordinary assembly entry. That entry calls the
same Tool driver as installed syscall hooks, including six arguments, error
results, tail injection, retries, and coordinator RPC. It retains no pointer
into the expired kernel signal frame and does not rewrite guest text.

The entry saves registers below the guest's 128-byte red-zone, aligns its
extended-state area to 64 bytes, and sizes that area from CPUID leaf 0xD. XSAVE
and XRSTOR use every user-state component enabled in XCR0; there is no fixed
component mask. Machines without OS-enabled XSAVE use FXSAVE64/FXRSTOR64.
The XSAVE header is zeroed before saving. On OS-enabled PKU systems the entry
saves the original PKRU in a register, opens runtime memory access before any
global or TLS read, and puts the original PKRU back in the saved image with its
correct initial-state bit. Nested Tool syscall traps reopen runtime access;
their real signal return restores the interrupted permissions. The callback
receives an empty x87 stack, default FP controls and a cleared direction flag;
the guest's FP state, permissions, flags and libc errno are restored afterward.
After the final XRSTOR, only registers and the guest-accessible frame are read.
Future runtime or clock boundaries must finish before that restore. The syscall outputs remain RAX
(result), RCX (continuation PC), and R11 (saved flags).

The installing thread initializes and touches its continuation TLS before
seccomp installation. A supported COW fork inherits that initialized state.
Each such thread owns one pending continuation; a second preparation while it
is occupied refuses without replacing it. Future thread support must initialize
these TLS keys in ordinary child-thread startup before its first trap, rather
than first touching dynamic TLS in a signal handler. After dispatch, the saved
RCX owns the return address and return reads no mutable TLS continuation.
Nested Tool-internal syscalls use the existing trusted gate and do not acquire
another continuation. Callable guest signal handlers remain unsupported;
this guard does not add asynchronous callback support. Callbacks must obey the
ordinary no-unwind ABI and preserve TLS bases. HookContext IP/SP fields retain
the existing register API's metadata semantics.

The guest stack must accommodate the callback stack plus at most
`335 + round_up(CPUID.0D.0:EBX, 64)` bytes below the original RSP. This includes
the 128-byte red-zone, 144-byte register frame, at most 63 alignment bytes and
the full state area. The FXSAVE case needs at most 847 bytes plus the callback
stack. Initialization checks size arithmetic and rejects an area smaller than
the XSAVE header or larger than the signed stack-adjustment bound. The actual
CPUID size describes the complete standard layout for the actual XCR0 mask;
there is no smaller fixed allocation that could omit a high component.

This may require more stack than the installed-hook path on an AMX host:
a 11008-byte enabled state area needs at most 11343 bytes before the callback
stack. The full mask preserves state that an AMX-permitted guest can use.
XFD gates use of permission-controlled state; XSAVE records the initial-state
status of disabled components, and XRSTOR initializes components whose saved
XSTATE_BV bits are clear. The entry preserves those CPU-provided bits for all
components other than the explicitly restored PKRU. AMX/XFD execution has not
been tested on the current AMD host; preserving the full mask is not a claim
of measured AMX coverage.

`tests/rpc_tool.rs` covers an RX page ending in `syscall; ret`, with six traps,
zero installed hooks, exact original bytes, Tool results distinct from native,
seven RPCs, all arguments, errno, retry, tail injection, GPRs, flags, red-zone
and XMM0. A separate fixture seeds x87, MXCSR, XMM and the host-enabled YMM,
opmask, ZMM and PKRU components, then requires byte-exact saved-state equality
across the Tool callback. It first verifies equality across a native syscall
and detects the callback's real state clobber when restoration is omitted.
It reports the actual XCR0 mask and image size. Components absent from that
host are not execution-tested; this is not a Hermit determinism or parity
qualification. No XSAVE header exception is used in these comparisons.

A separate protection-key fixture uses an actual nondefault-key stack and
checks PKRU zero and key0 access denied, full state, exact reported RSP, Tool
results and RPC with the alternate signal stack both enabled and disabled.
Before both native and Tool runs it explicitly unregisters
its own glibc rseq area through SYS_rseq, then re-registers it once the original
permissions have been restored. Registered rseq metadata must remain readable
and writable for kernel signal/preemption fixups: merely observing one native
raw syscall succeed while denying that metadata does not prove a valid rseq
lifetime. The original registered-rseq native signal failure is separate
kernel/guest lifecycle evidence; no production rseq behavior changes here.
Fork regressions retain installed-hook accounting and require fallback children
to report their own trap, fallback and syscall counts, with two process reports
through the existing Backend statistics API.

## Backend launcher

`LiteinstBackend` implements Reverie's `Backend` trait. It owns the single
`GlobalTool`, starts a UDS coordinator, sets `LD_PRELOAD`, runs the guest,
and returns its status and final global state. Existing preload APIs retain the
`REVERIE_LITEINST_COORDINATOR` environment contract. The example launcher
uses `run_with_output_and_preload_data` instead, passing the coordinator
path and selector in a sealed, dynamically allocated memfd that the preload
discovers, validates, consumes, and closes before guest `main`.
`REVERIE_LITEINST_TOOL_PRELOAD` must name a DSO that embeds the same concrete
`T`. Its author must register exactly one constructor with
`reverie_liteinst::tool_root_constructor!(initialize)` and call
`install_tool::<T>` or `install_tool_from_bootstrap::<T>` only inside that
constructor's complete setup body. Selector decoding, bootstrap/result
handling, and setup-owned destructors must all finish before `initialize`
returns. A raw `.init_array` function or an unwrapped installer is rejected
before the first process-global effect. Code after `with_tool_root!` is guest
execution and must not perform another installation.

Built-in `strace` and compatibility modes remain available through
`configure_command`. They use the same shared preload and LiteInst hook path
without a coordinator.

### Shared `reverie-preload` built-in tools

The single `REVERIE_LITEINST_TOOL` selector is a superset of the
LiteInst-native `strace`/`compat` modes: it also accepts the shared
`reverie-preload` built-ins `passthrough` and `spoof-getpid`, selected through
`configure_command_builtin(&mut Command, BuiltinTool)`. When one of these values
is set, the runtime installs the built-in verbatim through
`reverie_preload::install_builtin` — it does **not** run the LiteInst patching
dispatcher or prepare instrumentation. This is the LiteInst analog of the
e9patch built-in selector, so the same `BuiltinTool` value installs the same
shared dispatcher in both backends.

`spoof-getpid` proves the fallback/trap path can service **and mutate** a
syscall result: a raw `getpid` returns `reverie_preload::SPOOF_PID` instead of
the real PID, while `passthrough` leaves the result unchanged. The
`reverie-liteinst-spoof-guest` fixture and the
`spoof_getpid_builtin_mutates_getpid_result` /
`passthrough_builtin_preserves_getpid_result` tests in `tests/strace.rs` cover
both.

### Shared `reverie-preload` runtime configuration

The in-guest runtime's `SIGSYS` handler is installed through the shared
`reverie-preload` `RuntimeConfig`, whose `use_alt_stack` knob decides whether the
handler runs on an alternate signal stack. The `RuntimeConfig` and the
controller that honors it live in `reverie-preload` and are reviewed once; both
ld-preload backends install through that same seam. The launcher selects the
knob per guest with `set_guest_alt_stack(&mut Command, bool)`, which sets the
`REVERIE_LITEINST_ALT_STACK` environment variable (`1`/`0`, `true`/`false`,
`on`/`off`, `yes`/`no`; unset means the shared default, alt stack **on**). Only
the env-var spelling is LiteInst's — this is the LiteInst analog of e9patch's
`REVERIE_E9PATCH_ALT_STACK`, so the same `RuntimeConfig` drives both backends.
It applies to the LiteInst-dispatcher install path (the `strace`/`compat`/Detcore
modes); a shared `BuiltinTool` installs through `reverie_preload::install_builtin`
with the shared default. The `alt_stack_from_env_value` parser and the
`set_guest_alt_stack` round-trip are unit-tested in `src/runtime.rs` and
`src/lib.rs`.

On systems reporting OSPKE, the shared SIGSYS entry uses a register-only
WRPKRU prefix to open runtime access before touching its stack, siginfo,
ucontext, globals or TLS. Linux's default handler permissions may otherwise
deny a nondefault-key signal stack when `use_alt_stack` is false. Feature
selection runs during installation, before CPUID faulting; the prefix preserves
all three signal arguments and then runs the existing provenance and reentry
checks. It leaves the kernel's saved guest registers and PKRU unchanged, so
signal return restores the interrupted permissions. The non-OSPKE entry is
unchanged. This prefix does not make arbitrary Tool callbacks signal-safe or
add guest signal-handler support.

The shared signal dispatcher reads the interrupted PKRU from the kernel's
standard XSAVE signal frame without changing that frame. Installation validates
the component layout before CPUID faulting. Guest forwarding uses a second exact
trusted syscall site: it applies the saved permissions for the real kernel
operation, then restores runtime permissions using only registers before reading
its return stack. Runtime-private syscalls keep their original gate. Nested Tool
signals carry the nested interrupted rights, so RPC buffers are not treated as
buffers belonging to an earlier outer guest call.

The shared preload protection-key test compares native execution with both
signal-stack modes: 12 read/write cases, four actual partial transfers, and 24
clock/time/signal-action buffer cases. It checks raw errno, unchanged refused
buffers, pipe and signal-disposition effects, and returned PKRU. Successful clock
outputs are checked for valid values rather than identical wall-clock timestamps.
Direct installed hooks, deferred Tool injection and guest-memory policy emulation
still need their own permission provenance and are outside this measurement.
These cases do not establish complete backend PKRU parity.
Deferred typed callbacks already run with runtime PKRU zero: their `LocalMemory`
C-string reads and writes can access a key-denied guest buffer that a native
syscall rejects. The protection-key behavior of Tool injection, indirect policy
reads and the separate CPUID/RDTSC SIGSEGV entry also remains unqualified.

## Patch publication modes

The stopped ptrace install helper uses LiteInst2's quiescent entrypoint. The
backend must have every other tracee thread stopped for the complete helper
call; the current single-process, single-thread hybrid satisfies that contract.
Planning and relocation remain unchanged, so this route can patch a cache-line
straddler without registering WordPatch++ traps.

The in-process SIGSYS dispatcher always uses concurrent publication because
other application threads may fetch the site. Single-line patches publish
atomically. Split patches retain the full guarded WordPatch++ protocol and
require `REVERIE_LITEINST_STRADDLER_STALENESS_TICKS` to be set above the
machine's measured `Tmax`; without that calibration they fail closed to the
trap path. Quiescent publication is never selected from this route.

## Current boundaries

- Dynamically linked, non-`AT_SECURE` Linux x86-64 guests only.
- One thread per process is supported by `LiteinstBackend`. Plain `fork` creates
  a fresh child-local `Tool` and reconnects to the shared coordinator. The same
  path accepts process-like `clone3`; `vfork` is translated to a COW child and
  preserves parent suspension through child exit. The coordinator drains
  inherited RPC connections to follow outliving and signaled descendants
  without attaching ptrace. Thread-style clone remains fail closed.
- Patchable and unpatchable syscalls dispatch the Tool in guest, and intercepted normal exits
  route thread and process callbacks on the supported single-threaded path.
  CPUID and RDTSC/RDTSCP route through the Tool; determinized CPUID responses
  hide RDRAND/RDSEED from conforming guests.
- Subscribed vDSO symbols share ptrace's authoritative symbol table, are
  rewritten into syscall entry sites before activation, and use ordinary
  LiteInst Tool hooks.
- Tool mode resets callable signal dispositions before activation, rejects
  later callable handlers, and validates that SIGSYS came from seccomp.
  `SIG_DFL` and `SIG_IGN` remain supported; guest signal handlers remain
  unsupported.
- The launch CPU is part of the counter contract. Guest and Tool
  `sched_setaffinity` changes return `EPERM`; this intentionally differs from
  Linux for a caller that would otherwise be permitted to change affinity.
- Denying access to registered glibc rseq metadata can make native signal
  delivery fail in the kernel before any handler runs. A short native syscall
  can survive that denial, while LiteInst's mandatory SIGSYS interception
  triggers the kernel's rseq check and fails. The protected-stack fixture
  unregisters its own rseq area for both native and Tool controls; the runtime
  does not unregister production guests. Direct installed-hook behavior for
  this registered-rseq case remains unmeasured.
- Timer arming currently returns success without delivery. Clock reads use the
  supervisor-created, target-CPU RCB counter; physical entry boundaries pause
  it around runtime work rather than estimating a subtraction. Only a locally
  unsupported CPU profile is an unavailable clock. Permission, affinity,
  open, transfer, control, or read failures are terminal. This is not PMU timer
  delivery or complete scheduling support.
- Hardware qualification gives result credit to a selected method only when
  that method retains every `Report.events` record from its own supervised
  processes and joins each event ID, owner and target CPU to a live in-guest
  counter snapshot. The record also requires the exact raw retired conditional
  branch profile and an event acquired physically disabled. Zero events,
  missing or duplicate results, a result from another guest mode, unsupported
  PMU success, `rcb=unmeasured`, and unavailable markers all fail the method.
  The separate counter matrix cannot supply evidence for an `rpc_tool` method.
- Rust tool futures must make progress synchronously. Coordinator RPC and guest
  syscall injection do so; a tool future that depends on an unrelated executor
  can stall.
- The five-byte patch window and executable mapping must be supported by
  `liteinst2` to install a hook. Unpatchable syscalls use deferred Tool dispatch;
  other intercepted instructions still require a prepared reachable arena.
- `execve` cannot safely cross the inherited filter because the handler and DSO
  mappings disappear. It remains fail closed; completing exec requires a
  non-seccomp in-guest coverage mechanism or another bootstrap that does not
  reintroduce a ptracer.
- This is in-process instrumentation, not a security sandbox.

Hermit CLI linkage and a published `liteinst2` revision are separate integration
steps. The direct Backend harness has run Detcore with `/bin/echo`, `/bin/true`,
and `/bin/cat /dev/null`; this does not make `hermit --backend liteinst` real
until that CLI path constructs `LiteinstBackend` and the corresponding Detcore
preload DSO on the same landed revisions.

## Fallback-surface observability

The runtime exports C-ABI counters that make the size and shape of the residual
fallback surface — trapped syscalls without an installed hook
— observable from the guest:

- `reverie_liteinst_site_trap_count(address)` / `reverie_liteinst_site_hook_count(address)`
  — the per-**site** breakdown keyed by the un-patched instruction's address.
- `reverie_liteinst_fallback_dispatch_count()` — the process-wide total of
  syscalls that reached fallback dispatch, including successful typed Tool calls.
- `reverie_liteinst_fallback_syscall_count(number)` — the per-syscall-number
  breakdown, keyed the same way as `reverie_e9patch_fallback_syscall_count` so
  the two ld-preload backends expose a symmetric metric.
- `reverie_liteinst_fallback_refusal_count()` and
  `reverie_liteinst_fallback_syscall_refusal_count(number)` report the subset
  refused by the runtime before Tool dispatch, including an instruction-state
  restoration failure. A Tool's own error result does not count as a refusal.

The legacy dispatch counter counts attempts, including this refusal subset.
Enabled statistics classify a refused attempt as `fallback_refusal`, exclusive
with successful `cacheline_straddler` or `unpatchable_or_other` fallback paths.
The common `in_guest_sigsys` delivery count includes both outcomes.

These counters are **per-process**: they are process-global statics, so a
`fork`/`clone` child copy-on-write inherits the parent's accumulated values.
Left alone, a child would report the parent's residual surface and hook activity
as its own. In compatibility/strace mode LiteInst forwards a fork-like syscall
itself, so `process_syscall` invokes the shared
[`reverie_preload::fork::ForkHook`] seam in the child (guarded by the shared
`is_fork_like` classifier and a zero return value): immediately after the fork
returns `0` in the child, `reset_fallback_observability` clears inherited
counters so the child's attribution starts clean. Typed Tool mode then records
the child's actual fork dispatch method: an installed hook remains a hook,
while deferred fallback records a trap and successful fallback, not a hook. Only the observability fields
are reset — the site registry's functional patch state (address, hook, mapping
generation) is left intact because the child COW-inherits the installed hooks and
the same executable mappings, so its instrumentation keeps working. The reset is
relaxed-atomic and allocation/lock-free, so it is safe to run in the child from
inside the `SIGSYS` handler. This is the *same* fork-following seam and
reviewed-once mechanism reverie-e9patch uses for its per-process fallback
counters (round 7); LiteInst hosts its own dispatcher rather than the shared
`PassthroughDispatcher`, so it calls the hook directly, but reuses the shared
`ForkHook`/`is_fork_like` API rather than a private fork-detection path.

## Corpus sweep scorecard

A 20-program C corpus was run through `hermit --backend liteinst run --strict
--verify` and compared against native and the ptrace backend (full harness,
CSV, and per-program logs live in the `dev-hermit` parent workspace under
`experiments/liteinst_corpus_sweep_20260728/`, not in this repo). The result
is historical evidence from July 2026, not a qualification of the current
in-process runtime or a current L2 measurement:

- **Single-process / single-thread C: 16/16 reported repeat comparisons.** Every non-boundary program
  (arithmetic, heap, file I/O, env, libm, clocks, libc `rand`, `argv`,
  recursion, buffered stdio, `getrandom`, anonymous `mmap`, `gmtime`)
  determinized to a bitwise-identical repeat run, matching the ptrace baseline.
  Where a source is non-reproducible, LiteInst determinizes it *correctly*:
  `getpid` (spoofed PID) and `getrandom` (deterministic bytes) both diverge from
  native by design. Canonical L2 assurance is not established by these results.
- **Four boundary modes were reported in that historical sweep**,
  shared with e9patch because both ld-preload backends routed
  clone/fork through the same `reverie-preload` dispatcher and share this
  crate's signal/timer policy: thread `clone` and `fork` are rejected, a
  callable guest signal handler is rejected (fail-closed, nonzero exit), and an
  armed timer never fires (the guest spins to timeout).

### Caution: `--verify` cannot detect an ignored clone/fork rejection

`--verify` proves run₁ == run₂, **not** run == native. A guest that ignores the
errno from a rejected `clone`/`fork` and keeps running reaches a wrong but
perfectly reproducible result, which `--verify` then reports as "Determinism
verified" with `rc = 0`. In the sweep, the threaded and `fork` programs produced
degraded single-process output despite a successful repeat comparison. This is a
property of the shared clone/fork policy plus `--verify` semantics, not a
LiteInst-only defect; such a repeat comparison does not establish correct
thread or process behavior. Current support is described above. The
rejection itself is covered by `unsafe_clone_is_rejected_in_compatibility_and_strace_modes`
and the compatibility-fork tests in `tests/strace.rs`.
