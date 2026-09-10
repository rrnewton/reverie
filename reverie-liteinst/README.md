# Reverie LiteInst

`reverie-liteinst` is an experimental Linux x86-64 in-process instrumentation
package built on the standalone `liteinst2` patching library, the shared
`reverie-preload` runtime, and `reverie-rpc-transport`.

The in-process implementation now lives once in `reverie-liteinst-runtime`;
this package reexports its runtime APIs and owns the launcher-side RPC server,
the inherent `LiteinstBackend` caller-owned preparation methods and cdylib
target. It has no ambient preload-library discovery, pathname-based launcher,
or ptrace-owned LiteInst execution mode.
Custom tool runtimes can depend directly on the leaf with default features
disabled; see the leaf README for initialization, linkage and Cargo export
synchronization obligations.

## Event path

### Retained guest logs

`retained_guest_log::retained_log(Options::bounded(byte_limit))` returns a
single-use sink and a caller-owned handle. Pass the sink to
`LiteinstBackend::prepare_with_owned_command_data_and_log_sink` or
`prepare_with_owned_configuration`. Keep the handle outside
the cancelled future and Tokio runtime. Captured stdin keeps its configured
behavior; captured stdout/stderr remain separate exact byte streams. Inherited
stdio uses the actual inherited descriptors, not host re-emission. Errors retain
a `LoggedRunError` inside the Reverie I/O error.

Before exec, the child transfers its pidfd to the launcher and waits for an
acknowledgement. The launcher releases it only after receiving ownership;
failure to open or receive the descriptor aborts exec. This does not rely on
the best-effort `Command::create_pidfd` option or use numeric-PID signal
fallbacks. An additional host thread performs the blocking spawn handshake.
If the child dies before transferring the pidfd, that thread reaps its only
child with `waitid(P_ALL, __WNOTHREAD)` before releasing launch resources;
children created by other threads are excluded from that wait. Guest syscall
dispatch is unchanged.

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

1. A tool-specific DSO consumes the sealed bootstrap and calls
   `install_tool_from_bootstrap::<T>` from its preload constructor. It connects
   to the coordinator and receives `T::GlobalState::Config` before seccomp is
   active.
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

The regression fixture checks that each callback sends a Reverie tool RPC when
that fixture is executed; this description is not execution evidence.

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

`LiteinstBackend` implements Reverie's `Backend` trait, but `Backend::run`,
`Backend::run_with_stats`, and `Backend::run_with_output` all return an
`Unsupported` error. A bare generic `Command` cannot express ownership of the
resources that must remain valid until the child has been terminated and
reaped.

The runnable path is
`LiteinstBackend::prepare_with_owned_command_data_and_log_sink` (or
`prepare_with_owned_configuration`). It accepts a caller-owned
`PreparedCommand`, starts the UDS RPC server, passes bootstrap data in a sealed
memfd, and returns a `RunObserver` plus the future that drives the run. The
owned path retains the supplied owner through pidfd-based child termination and
reap; `with_spawn_check` and `with_cleanup` let the caller bind resource checks
and cleanup to that lifetime. The tool-specific DSO must embed the same
concrete `T` and consume the sealed bootstrap. The old
`REVERIE_LITEINST_TOOL` constructor selector is refused; it is not a fallback
launch route.

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
It applies to the explicit LiteInst Tool install path. The
`alt_stack_from_env_value` parser and the
`set_guest_alt_stack` round-trip are unit-tested in `src/runtime.rs` and
`src/lib.rs`.

## Patch publication modes

The unsafe `install_tool_quiescent` entry point is available only when its
caller has stopped every other application thread for the complete install.
No ptrace-owned LiteInst launcher establishes that precondition. Planning and
relocation remain unchanged, so a caller that independently establishes the
contract can patch a cache-line straddler without registering WordPatch++
traps.

The in-process SIGSYS dispatcher always uses concurrent publication because
other application threads may fetch the site. Single-line patches publish
atomically. Split patches retain the full guarded WordPatch++ protocol and
require `REVERIE_LITEINST_STRADDLER_STALENESS_TICKS` to be set above the
machine's measured `Tmax`; without that calibration they fail closed to the
trap path. Quiescent publication is never selected from this route.

## Current boundaries

- Dynamically linked, non-`AT_SECURE` Linux x86-64 guests only.
- One thread per process is supported by the caller-owned LiteInst path. Plain `fork` creates
  a fresh child-local `Tool` and reconnects to the shared coordinator. The same
  path accepts process-like `clone3`; `vfork` is translated to a COW child and
  preserves parent suspension through child exit. The coordinator drains
  inherited RPC connections to follow outliving and signaled descendants
  without attaching ptrace. Thread-style clone remains fail closed.
- Patchable syscalls dispatch the Tool in guest, and intercepted normal exits
  route thread and process callbacks on the supported single-threaded path.
  CPUID and RDTSC/RDTSCP route through the Tool; determinized CPUID responses
  hide RDRAND/RDSEED from conforming guests.
- Subscribed vDSO symbols use Reverie's shared symbol definitions, are
  rewritten into syscall entry sites before activation, and use ordinary
  LiteInst Tool hooks.
- Tool mode resets callable signal dispositions before activation, rejects
  later callable handlers, and validates that SIGSYS came from seccomp.
  `SIG_DFL` and `SIG_IGN` remain supported; guest signal handlers remain
  unsupported.
- Production timer requests return `EOPNOTSUPP`; no production timer is armed
  or delivered. Clock reads use a calling-thread RDPMC RCB counter and deduct
  branches retired inside active LiteInst handlers. Hosts that deny perf-event
  access report the clock as unsupported. This is not PMU preemption or
  complete scheduling support.
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

Hermit CLI linkage is not present. The manifests pin public, fetchable
`liteinst2` revision `95ee5e6917fa33191eb41c3f1606ea8b03c1b78c`, and
crates.io publishes `liteinst2` 0.1.0. This document makes no successful
Detcore or `hermit --backend liteinst` execution claim.

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

These counters are **per-process** process-global statics. Their values and
functional patch state are copied on `fork`; callers comparing parent and child
reports must account for the inherited prefix.

## Explicit SUD-only Tool fixture

The unsafe `install_tool_with_mode` API can select
`SyscallMode::UserDispatchWithoutPatching`. Existing installation APIs still use
seccomp and retain optional site patching. There is no Hermit CLI activation.
`install_tool_from_bootstrap_with_mode` accepts the same explicit mode from the
sealed bootstrap; `install_tool_from_bootstrap` still selects
`SeccompWithPatching`.
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

## Verification scope

The native XSAVE signal controls compare the complete 2440-byte standard-format
image, preserving every payload byte and every header field except one explicit
initialized-state transition. XSTATE_BV bit 7 may change from 1 to 0 only when
both complete Hi16_ZMM payloads at offsets 1408..2432 are zero. All other 63 bits
of XSTATE_BV must remain exact; a reverse transition, nonzero payload, changed
XCOMP_BV, reserved-header change or any payload change is rejected. The existing
CPUID/XCR0 geometry, initial-state, signal-mask, CPU-control and callback-count
checks remain in place. This is a comparison of native captures for that fixed
profile, not an arbitrary XSAVE-image validator or a change to runtime restore.

AMD APM Volume 4, publication 26568, revision 3.27 (July 2026), printed page 1774
(PDF page 1828), explicitly permits ordinary XSAVE to clear XSTATE_BV for a
hardware-initialized component as an implementation-dependent optimization:
https://docs.amd.com/api/khub/documents/OCw3Z61a0v0jKvHJl_Qmtg/content#page=1828
The control reports whether that transition occurred rather than claiming full
byte identity. This corrects the previous nonportable raw-header requirement;
it does not infer an internal XINUSE change from a later XSAVE result.

The `xsave_restore` integration target exercises real standalone clobbering,
restoration and deliberately corrupted restore input, with three executions per
variant and retained raw images. Omitted restore, payload corruption, unrelated
header changes and a falsely permitted mask for nonzero Hi16_ZMM all fail.
Mutation tests cover every bit of the 2440-byte image and every byte of the
Hi16_ZMM initialized-state condition. The all-features target retains all cases.

The `owned_compiled_step` integration target qualifies compiler-generated
machine code with exact clocks of 33 and 553 completed steps in the armed case.
It builds its guest with the pinned `nightly-2026-07-29` release profile, offline
and locked, enabling all features of the launcher, runtime and RPC transport.
This build is bounded to 600 seconds and has no existing-binary fallback.
The ordinary Cargo profile still builds the original target and runs its unit
tests; the integration target executes a retained copy of the release guest.
Its output directory records the build command, profile environment, compiler
identity, Cargo artifact metadata, build status and executable SHA-256.

Without `REVERIE_COMPILED_STEP_ARTIFACTS`, the target creates and retains a short
temporary directory. An explicit directory remains authoritative and existing
case directories are never overwritten. Loader variables are removed from
each child command and recorded without changing the parent's environment;
the guest still refuses loader overrides. Evidence files are opened before
installation. After the final guest state capture, verification writes through
the trusted raw gate without widening syscall subscriptions. Output errors and
failed assertions terminate nonzero. All clock trajectories, repetitions,
frame linkage, register/XSAVE checks and once-only pipe assertions remain exact.

Plain `hermit run --strict --verify` uses the lossy comparator and cannot
establish L2. A future LiteInst L2 claim requires an actual Hermit CLI path and
`--verify --verify-strict --verify-json`, with `bitwise_parity: true` and
nonzero compared INFO-message counts under the `BitwiseInfoV1` policy. It must
also name the backend, log level, and relaxations. No such LiteInst result is
claimed here.
