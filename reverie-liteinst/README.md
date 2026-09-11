# Reverie LiteInst

`reverie-liteinst` is an experimental Linux x86-64 Reverie backend built on the
standalone `liteinst2` patching library, the shared `reverie-preload` runtime,
and `reverie-rpc-transport`.

The in-process implementation now lives once in `reverie-liteinst-runtime`;
this package reexports its existing runtime APIs and retains the coordinator,
all inherent `LiteinstBackend` methods, command configuration and cdylib target.
Host tracer launches and backtrace support remain here. Custom tool runtimes
can depend directly on the leaf with default features disabled; see the leaf
README for initialization, linkage and Cargo export synchronization obligations.

## Event path

### Retained guest logs

`retained_guest_log::retained_log(Options::bounded(byte_limit))` returns a
single-use sink and a caller-owned handle. Pass the sink to
`LiteinstBackend::run_with_output_and_preload_data_and_log_sink` or
`run_with_inherited_stdio_and_preload_data_and_log_sink`. Keep the handle outside
the cancelled future and Tokio runtime. Captured stdin keeps its configured
behavior; captured stdout/stderr remain separate exact byte streams. Inherited
stdio uses the actual inherited descriptors, not host re-emission. No-log APIs
retain their existing behavior. The older captured-log convenience delegates to
this path; errors retain a `LoggedRunError` inside the Reverie I/O error.

The V3 sealed bootstrap and `RLG3` descriptor handshake attach one fixed shared
mapping with bounded per-process rings. Both host and preload must be rebuilt
together; V2 packet logs do not fall back to or decode as V3 buffered records.
The shared implementation is in `reverie-rpc-transport::guest_log`, separate from
synchronous GlobalRPC; no per-record ACK or host-determined Tool ordering is
introduced. Free slots accept BEGIN/DATA/END publication; Full retries the same
frame under the process writer lock. Waiting uses the trusted gate only with the
existing guest counter paused (or no active/requested counter). It does not reset,
subtract from, or drive guest virtual time. Exhaustion/failure is not successful
logging. Reentry fails explicitly. Actual retained RPC failures have 64 reserved
evidence slots shared with outstanding connections; capacity exhaustion refuses
another dispatch rather than discarding an original cause. Default RPC handling
is unchanged, including its historical partial-header EOF limitation.

Snapshots keep complete bytes, unfinished fragments, producer incarnation/order,
unread-frame counts, original RPC causes, run state and collection state. The
snapshots separately retain the first rejected committed frame per producer:
its exact 40-byte header fields and up to 256 declared payload bytes, with an
explicit omitted-byte count. This diagnostic budget is separate from canonical
bytes and accepted fragments; it does not make an invalid stream complete. Clean
collection requires all registrations/resolved forks and FINISH records **plus
real lifetime peer closure** and no failure. FINISH, root reaping, ring emptiness
and whole-tree exit are not interchangeable. The endpoint stays protected through
process teardown and admitted COW forks; failed forks consume tombstones, while a
successful fork followed by wait failure remains a real child. Exec/image handoff
is not added. SUD-only private admissions remain single-thread/no-fork.

Use a handle-bound per-producer `cursor().write_to(...)` to advance only for bytes
actually written; destination failure retains the remaining bytes and a failure
status. The destination's file budget is still the caller's shared budget, not a
new allowance per publication. Multiple producer streams are not concatenated
into a deterministic canonical order. The old convenience API labels its
multi-producer concatenation diagnostic-only. `Report::qualifies()` requires
clean collection and run completion; it does not establish backend parity.

Both logged adapters await cancellation-aware collector readiness before spawning
the producer. A saturated blocking pool therefore cannot strand a published
prefix behind a revocable queued collector. Common transport users must likewise
wait for readiness before independently scheduling producers (exclusive,
non-cancellable synchronous prepopulation is a separate startup contract).
Cancellation revokes a queued collector before it can append, or requests a
single non-resetting 30-second drain deadline from the active collector. Root
kill/reap and stdio/RPC drains have separate bounded waits. Runtime destruction
can leave reaping unknown. Arbitrary blocking `pre_exec` callbacks remain
synchronous and cannot be made cancellable by this API. Mappings and additional
descriptors (above fd2), formatter allocations, TLS/cache activity, backpressure
and startup allocation order remain guest perturbations; they are not invisible.
The byte limit bounds retained logical record data, not exact host RSS (snapshots
and publication copies also consume bounded memory). Native buffered logging
under instrumentation/clock pressure still needs independent qualification;
this change does not port DBT or admit additional Detcore capabilities.

### Instruction and syscall dispatch

1. A tool-specific DSO calls `install_tool::<T>` from its preload constructor.
   It connects to the coordinator and receives `T::GlobalState::Config` before
   seccomp is active.
2. `reverie-preload` installs the SIGSYS handler, alternate stack, trusted
   syscall gate, and seccomp filter.
3. The first syscall at an instruction reaches SIGSYS. The LiteInst dispatcher
   installs a replace-first hook and changes the saved signal-context RIP to the
   generated trampoline entry.
4. After `sigreturn`, the trampoline invokes `T::handle_syscall_event` in normal
   guest context. The first invocation and later patched invocations therefore
   use the same tool path; the first site trap is not also a tool execution.
5. `LiteinstGuest<T>` supplies in-process memory/register access and syscall
   injection through the trusted gate. `CoordinatorRpc<G>` serializes
   `GlobalRPC` messages over the same UDS/bincode framing as
   `reverie-rpc-transport::RpcServer<G>`. The launcher accepts concurrent local
   connections against the one coordinator-owned global state and cancels any
   outstanding connection tasks when the guest run ends.

The regression proof reports `calls=32 traps=1 hooks=32` and sends a real
Reverie tool RPC for every callback.

### In-process unpatchable syscall fallback

The typed Tool path also defers unpatchable syscall sites until after SIGSYS
returns. It leaves the syscall instruction and following guest bytes untouched.
The ordinary-context callback uses the same Tool, RPC, injection and restart
driver as installed hooks; it does not substitute native execution or ptrace.
If instruction-faulting policy cannot be restored after a patch attempt, the
runtime still refuses rather than executing with weakened instrumentation.

`syscall_fallback.rs` initializes two private pages per supported guest thread
before interception: an RX return stub and a separate RW continuation slot.
They remain allocated for the process lifetime and are inherited privately by
supported single-threaded fork children. The entry saves the guest GPRs, flags,
red-zone and the same x87/SSE/AVX/AVX-512/PKRU state selected by the pinned
LiteInst2 trampoline. Callbacks must preserve TLS bases and excluded extended
state, including AMX. The fallback additionally preserves libc `errno` around
Tool dispatch. It requires adequate writable guest-stack space.

Actual RSP restoration follows the saved assembly frame, not
`HookContext.stack_pointer`; IP/SP in that structure remain metadata. The
return stub restores R11/flags/RSP and jumps through its thread's continuation
slot without consuming a guest return address or constructing a signal frame.
This is a syscall return: RAX receives the result, RCX the post-syscall address,
and R11 the saved flags. It is not an arbitrary-PC timer restoration API.

The focused `unpatchable_syscall_dispatches_tool_after_signal_return` test uses
a two-byte syscall followed by `ret` at an executable page end. It requires
non-native results and RPC receipts, unchanged bytes, repeated traps without
hooks, errno/six-argument/tail/restart behavior, register/red-zone/XMM preservation
and nested Tool-internal syscalls. These are Reverie-only tests, not Hermit L2.
Integration with timer delivery remains unqualified: callback clock brackets do
not cover assembly entry and return. No calibrated offsets or clock relaxation
are used. The exact assembly range is `fallback_entry..fallback_entry_end`;
each private return stub copies `fallback_return_template..fallback_return_template_end`.
Timer handling must distinguish its internal correction trap from guest SIGTRAP
and coordinate the shared syscall/fault handoff before redirecting a kernel frame.

## Backend launcher

`LiteinstBackend` implements Reverie's `Backend` trait. It owns the single
`GlobalTool`, starts a UDS coordinator, sets `LD_PRELOAD`, runs the guest,
and returns its status and final global state. Existing preload APIs retain the
`REVERIE_LITEINST_COORDINATOR` environment contract. The example launcher
uses `run_with_output_and_preload_data` instead, passing the coordinator
path and selector in a sealed, dynamically allocated memfd that the preload
discovers, validates, consumes, and closes before guest `main`.
`REVERIE_LITEINST_TOOL_PRELOAD` must name a DSO that embeds the same concrete
`T` and calls `install_tool::<T>`.

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
- Patchable syscalls dispatch the Tool in guest, and intercepted normal exits
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
- Timer arming currently returns success without delivery. Clock reads use a
  calling-thread RDPMC RCB counter and deduct branches retired inside active
  LiteInst handlers. Hosts that deny perf-event access report the clock as
  unsupported. This is not PMU preemption or complete scheduling support.
- Rust tool futures must make progress synchronously. Coordinator RPC and guest
  syscall injection do so; a tool future that depends on an unrelated executor
  can stall.
- Installing a hook requires a five-byte patch window and an executable
  mapping supported by `liteinst2`. The typed syscall path falls back without
  patching when those conditions are absent; other instruction kinds retain
  their existing refusal policy.
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
fallback surface — the trapped syscalls that did not receive an installed hook
— observable from the guest:

- `reverie_liteinst_site_trap_count(address)` / `reverie_liteinst_site_hook_count(address)`
  — the per-**site** breakdown keyed by the un-patched instruction's address.
- `reverie_liteinst_fallback_dispatch_count()` — the process-wide total of
  syscalls that reached fallback dispatch (including successful typed fallback).
- `reverie_liteinst_fallback_syscall_count(number)` — the per-syscall-number
  breakdown, keyed the same way as `reverie_e9patch_fallback_syscall_count` so
  the two ld-preload backends expose a symmetric metric.

These counters are **per-process**: they are process-global statics, so a
`fork`/`clone` child copy-on-write inherits the parent's accumulated values.
Left alone, a child would report the parent's residual surface and hook activity
as its own. In compatibility/strace mode LiteInst forwards a fork-like syscall
itself, so `process_syscall` invokes the shared
[`reverie_preload::fork::ForkHook`] seam in the child (guarded by the shared
`is_fork_like` classifier and a zero return value): immediately after the fork
returns `0` in the child, `reset_fallback_observability` zeroes all three counter
families so the child's attribution starts clean. Only the observability fields
are reset — the site registry's functional patch state (address, hook, mapping
generation) is left intact because the child COW-inherits the installed hooks and
the same executable mappings, so its instrumentation keeps working. The reset is
relaxed-atomic and allocation/lock-free, so it is safe to run in the child from
inside the `SIGSYS` handler. This is the *same* fork-following seam and
reviewed-once mechanism reverie-e9patch uses for its per-process fallback
counters (round 7); LiteInst hosts its own dispatcher rather than the shared
`PassthroughDispatcher`, so it calls the hook directly, but reuses the shared
`ForkHook`/`is_fork_like` API rather than a private fork-detection path.

## Explicit SUD-only Tool fixture

The unsafe `install_tool_with_mode` API can select
`SyscallMode::UserDispatchWithoutPatching`. Existing installation APIs still use
seccomp and retain optional site patching. There is no Hermit CLI activation.
The environment-preserving `install_tool_from_bootstrap_with_mode` API accepts
the same explicit mode without removing a caller-provided coordinator variable;
`install_tool_from_bootstrap` still selects `SeccompWithPatching`.
This bounded mode handles native x86-64 syscall instructions after installation:
validated SUD enters the existing deferred fallback, returns through the trusted
kernel signal restorer, then runs the shared Tool driver and coordinator RPC in
ordinary context. It does not prepare sites, install hooks or rewrite the vDSO.
Always-enabled attempt and installation counters make these exclusions observable.

The original kernel frame is borrowed only during signal handling, not retained
after `rt_sigreturn`. One per-thread pending instruction address is exclusive
until ordinary Tool dispatch completes; a second pending continuation refuses
rather than overwriting it. The saved RCX then supplies the real return address,
not HookContext IP/SP metadata. The existing assembly saves and restores actual
guest registers, stack/redzone, flags and the supported FP state. RCX and R11
follow syscall clobber semantics, not asynchronous preserve-all semantics.
XSAVE uses the existing restricted enabled-state mask `0x2e7` (FXSAVE fallback);
this is not general xstate, arbitrary-PC or TLS-base switching support. Errno is
saved across Tool dispatch. Existing PatchAllocator/TOOL_HEAP isolation remains.

The unsafe caller must maintain one application thread and prevent application
handlers/asynchronous callbacks or nonlocal exits from entering the runtime for
the remaining process lifetime. Initial quiescence alone is insufficient. The
API rejects preinstalled custom handlers and blocked SIGSYS, preserves the prior
guest mask, and does not grant general signal or lifecycle support. Fork/clone
and exec remain refused, including Tool injection. The loader before installation
is not trapped; exec/thread rearming is not implemented.

Instruction subscriptions and vDSO-dependent syscall subscriptions are rejected,
not silently made native. This excludes a full shared Detcore configuration.
Selected shared-clock execution remains rejected without an explicitly configured
runtime-owned signal policy. The bounded clock fixture configures immutable,
source-validated runtime actions through `signal::configure_runtime_signals`;
ordinary custom guest handlers, blocked required signals and signal-mask mutations
remain refused. The original frame transfers only scalar clock/mask ownership
to ordinary fallback. Typed injections run with the recorded guest mask; after
Rust cleanup the trusted assembly tail restores that exact mask before enabling
the cumulative clock. No frame reference survives `rt_sigreturn`.

The additional `clocked_tool/guest_raw.s` fixture retains the original six-sample
trajectory and tests runtime-work variation, actual boundary interruption,
nonzero guest-mask queries and pending private-signal delivery inside the final
mask-restoration syscall. The original instruction fixture is unchanged.
This is a bounded clock prerequisite, not arbitrary asynchronous Tool reentry,
precise timer execution, general signal/lifecycle support or Detcore qualification.
Both timer setters return EOPNOTSUPP for every schedule in SUD-only
mode. The composed shared timer-control component retains disabled preparation
and boundary reconciliation; default-mode setters cancel any owned request
and explicitly refuse delivery as well. No production timer is armed or
dispatched in either mode.

`rpc_tool::sud_only_shared_tool_without_guest_patching` checks repeated ordinary
text, libc, page-end and post-install anonymous sites, live code/vDSO byte
identity, zero planning/patch/vDSO attempts, non-native Tool results, actual shared
RPC receipts, six arguments, errno, tail injection, restart, registers/redzone
and nested internal syscalls. Companion cases check unsupported ABI (x32 and
compat), subscription/signal/clock refusal and inherited ptrace denial. These are
typed raw-syscall Tool tests, not Hermit Detcore, L2, deterministic time or corpus
coverage evidence. vDSO fast paths, nondeterministic instructions and legacy
vsyscall fault emulation are not established by the syscall fixture.

### Private owned syscall qualification

The default-off `test-owned-cpuid` feature also exposes the unsafe finite
`__install_owned_syscall_timer_fixture` contract. It admits only native x86-64
`getpid` and ready bounded pipe `read`, optionally alongside the existing private
instruction subscriptions. Authentic SUD frames transfer to the same owned
runtime, Tool/thread state, CoordinatorRPC and direct return as precise timers.
The syscall number comes from siginfo; saved RIP is already the continuation.
Only RAX is edited for a result. An authenticated pending SYSCALL cancels rather
than retires a step; owned TF removal from flags/R11 checks the exact profile.

Unarmed SIGSYS must be qualified before armed ordering on each native profile.
Unexpected sources or clobbers fail closed and retain bounded raw frame evidence.
The new tests use real native injection, a distinct second pipe block as the
once-only read oracle, full seeded XSAVE comparisons, and complete clock vectors
across first ThreadStart, syscall/timer callbacks, and final guest return. Evidence
collection after that return holds a terminal-only runtime guard until exit.
No public/default capability, blocking/restart/lifecycle/vDSO support, arbitrary
TF stepping, Detcore first-event or CLI qualification follows. Existing original
zero/unseeded FP comparison obligations are unchanged and remain unresolved.

## Corpus sweep scorecard

A 20-program C corpus was run through `hermit --backend liteinst run --strict
--verify` and compared against native and the ptrace backend (full harness,
CSV, and per-program logs live in the `dev-hermit` parent workspace under
`experiments/liteinst_corpus_sweep_20260728/`, not in this repo). The result
characterizes the supported frontier and each boundary mode:

- **Single-process / single-thread C: 16/16 L2.** Every non-boundary program
  (arithmetic, heap, file I/O, env, libm, clocks, libc `rand`, `argv`,
  recursion, buffered stdio, `getrandom`, anonymous `mmap`, `gmtime`)
  determinized to a bitwise-identical repeat run, matching the ptrace baseline.
  Where a source is non-reproducible, LiteInst determinizes it *correctly*:
  `getpid` (spoofed PID) and `getrandom` (deterministic bytes) both diverge from
  native by design and still verify L2.
- **Boundary programs fail in the four documented modes** listed under *Current
  boundaries*, all shared with e9patch because both ld-preload backends route
  clone/fork through the same `reverie-preload` dispatcher and share this
  crate's signal/timer policy: thread `clone` and `fork` are rejected, a
  callable guest signal handler is rejected (fail-closed, nonzero exit), and an
  armed timer never fires (the guest spins to timeout).

### Caution: `--verify` cannot detect an ignored clone/fork rejection

`--verify` proves run₁ == run₂, **not** run == native. A guest that ignores the
errno from a rejected `clone`/`fork` and keeps running reaches a wrong but
perfectly reproducible result, which `--verify` then reports as "Determinism
verified" with `rc = 0`. In the sweep, the threaded and `fork` programs produced
degraded single-process output that was nonetheless blessed L2. This is a
property of the shared clone/fork policy plus `--verify` semantics, not a
LiteInst-only defect; treat an L2 pass on a program that legitimately uses
threads or child processes as suspect until multi-process support lands. The
rejection itself is covered by `unsafe_clone_is_rejected_in_compatibility_and_strace_modes`
and the compatibility-fork tests in `tests/strace.rs`.
