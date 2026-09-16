# SUD-only shared Tool: source answer and pre-edit checkpoint

Task: `sud-only-in-guest-mode-with-patching-optional-and-backend-consolidation`.
Read with `tg <id> -v`; existing owner remains `liteinst-owner`. No claim,
ownership change, build, guest run, product edit or new commit in this task.
Own slot is clean at `f22573b9335a258316b8d1c9eb0e5f865affd7a1`.

## Site answer: qualified, not universal Linux coverage

**Patching is not intrinsically required to intercept an ordinary native
x86-64 syscall instruction.** The running kernel's installed
`include/linux/entry-common.h:72` checks the per-task
`SYSCALL_WORK_SYSCALL_USER_DISPATCH` flag and invokes
`syscall_user_dispatch(regs)` before ptrace/seccomp/syscall dispatch. This
entry hook has no ELF, libc, JIT or patch-site registry input. It supports the
site-independent design: a native syscall in libc, executable text, an
anonymous executable mapping, or an otherwise unpatchable two-byte site
does not need a patch merely to reach SUD. Existing host controls prove two
repeated outside-range getpid traps and null-selector interception, not every
site/ABI. No patch-only class of ordinary native syscall is established here.

**The absolute statement “every supported instruction outside the range” is
not fully source-certified in this checkout.** Installed kernel headers do
not include the implementation of `syscall_user_dispatch`, x86 entry bodies,
legacy-vsyscall emulation, reset logic or SUD documentation/selftests. Those
exceptions cannot be filled in from the function name. The decisive coverage
claim remains conditional on reaching the native syscall entry with SUD
armed, no allowed-selector exception, valid signal delivery and the supported
ABI. This limitation does not justify making patching mandatory again.

| Case | Evidence / implementation boundary |
|---|---|
| Native x86-64 `syscall` in ELF/libc/JIT/page-end text | Generic entry hook plus repeated-trap controls support site independence. Add exact per-site tests; no planning/rewriting is required by the interception mechanism. |
| Trusted IP interval / selector | S sets an exclusive allowed gate interval and null selector (`user_dispatch.rs:134`); actual trusted-gate calls do not trap. Retained explicit ALLOW/OFF controls bypass by design, not a permitted backend policy. Kernel-specific signal-restorer exceptions remain unaudited without implementation source. |
| IA32 `int 0x80` / compat `sysenter` | Different argument layout and AUDIT_ARCH_I386 are explicit in installed `asm/syscall.h:119`/`:167`. Current SUD handler accepts only AUDIT_ARCH_X86_64. Do not decode compat registers as native or advertise support. Complete entry-path SUD behavior needs missing kernel bodies. |
| x32 ABI | Same audit architecture as x86-64 (`asm/syscall.h:167`), distinguished by `__X32_SYSCALL_BIT` (`uapi/asm/unistd.h:13`). Architecture validation alone is insufficient. Plan an explicit unsupported-ABI check before native shared Tool decoding, not silent reinterpretation. |
| Legacy fixed-address vsyscall | Installed `asm/vsyscall.h:14` declares instruction-fetch-fault emulation, not ordinary syscall entry; host config enables it. Whether that emulation consults SUD is not verified. Do not count it as covered by SUD or as a proven patch-only syscall site. |
| vDSO fast path | A function completing in user space is not a syscall entry. Only an actual syscall fallback can reach SUD. The current typed installer rewrites subscribed vDSO functions; a genuinely zero-patch mode must not do that. Deterministic time through those functions remains a separate obligation. |
| CPUID/RDTSC/RDTSCP/RDRAND/RDSEED, memory-mapped data | Not syscall events. SUD does not itself intercept these instructions or user-memory reads. Existing instruction faulting/patching cannot silently be presented as SUD-only coverage. |
| New threads/fork/exec and startup | Prior controls observe SUD reset in children/new image, failed exec retaining it, and native startup before rearm. Initial and replacement loader windows are unqualified; exec/static refusals remain. No generic ThreadState serialization, native post-exec interception, or clock reset is assumed. |

Kernel provenance: installed package for the running
`7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, rooted at
`/usr/src/kernels/7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`.
This is not a complete Git checkout. Header hashes/config and observation time
are in `/home/newton/work/dev-hermit/ignored/liteinst-sud-only-plan-20260904/source-evidence.txt`.
The old experiment locator `experiments/linux` contains reports, not kernel
source; Git there resolves to the parent workspace and is not a kernel pin.
Historical `ignored/linux` is absent. Bounded local discovery found no full
implementation; no network probe or alternate fetch was attempted.

## Smallest no-patch path through the real shared Tool

All product paths below refer to the immutable f225 commit in
`/home/newton/work/dev-hermit/worktrees/slots/liteinst-exec`.

1. Extend typed installation with an explicit interception choice and optional
   patching policy; existing APIs retain their current defaults. No CLI or
   hosted-path change. Prepare Tool/RPC/protected descriptors, runtime domain,
   per-thread fallback state and clock ownership before arming SUD.
2. Bypass **all** rewriting/setup in disabled mode: current
   `tool_host.rs:177` calls `patch_current_vdso`; `runtime.rs:1009` calls
   `prepare_instrumentation` (live-patching setup, site table, arena planning);
   `runtime.rs:1010` installs vDSO hooks. Split common initialization from
   these optional operations. `syscall_fallback::initialize` already occurs
   separately at `tool_host.rs:156`; its private runtime return stub is not
   a guest-site patch or site planner.
3. A real validated SUD SIGSYS must retain a mechanism discriminator, not only
   the current conflated `SyscallEventSource::SignalTrap`. After guarded
   runtime-internal handling, disabled mode branches **before**
   `locate_syscall_site`/`claim_site`/`install_site_hook` at
   `runtime.rs:2881`. Call `syscall_fallback::prepare(saved_resume_ip)` and
   `event.defer_to_clocked(entry, entry)` directly. Count SUD deferred dispatch
   as that path, not as failed patch installation.
4. Preserve the kernel's real signal frame until trusted rt_sigreturn. The
   existing assembly `syscall_fallback.rs:153` saves guest registers/redzone
   and its scoped FP state in ordinary context, consumes the scalar clock
   witness, and calls `dispatch_fallback_context` (`runtime.rs:2640`) ->
   `dispatch_syscall_context` -> `tool_trampoline` -> ToolHost -> shared
   `reverie_preload::tool_host::drive_tool_syscall` (`tool_host.rs:375`). The
   same injected/tail/restart/RPC result path returns through actual RSP/flags
   restoration, not HookContext IP/SP metadata. Do not create another emulator.
5. Keep patch-enabled tests/configuration independently available. Pure
   syscall fixture support comes first, not a new trap-to-patch optimization.

## Contracts that must be resolved before claiming the fixture

- SUD handler `trap.rs:274` currently discards the scalar Continuation and is
  unclocked. It must participate in existing shared clock entry/leave and use
  the approved trusted restorer. SUD's all-signals dispatcher mask conflicts
  with retained owned SIGTRAP clock windows. An explicit runtime-owned signal
  policy is still required; no blanket unblocking or removal of windows.
- `syscall_fallback.rs:108` has one mutable Pending record per thread, not a
  nested-continuation stack. Its lifetime extends through the final return
  stub; the borrowed kernel frame ends at rt_sigreturn. Require exclusive
  pending guest continuation or fail closed, without confusing internal
  syscalls with a newly delivered guest handler. Initial quiescence alone
  proves none of these later-entry obligations.
- PatchAllocator/TOOL_HEAP already isolates runtime Rust allocations. That
  does not qualify asynchronous Detcore/libc/TLS/locks or joint Tool/clock
  transitions. Preserve unsupported lifecycle/signal guards; never route a
  guest handler silently through the Tool-internal native path.
- Current CPUID/TSC SIGSEGV handler also installs hooks (`runtime.rs:2434`).
  A no-patch syscall fixture must use a dedicated raw-syscall Tool without
  those instruction subscriptions, while retaining hardware `read_clock`
  checks. Existing ClockTool includes RDTSC and is not that fixture unchanged.
  Preserve its full original suite separately. Unsupported non-syscall
  observation requirements must be explicit, not silently disabled in Detcore.

## Focused validation to implement after this checkpoint

No new configuration has been run yet. Proposed tests are acceptance criteria:

- Under the existing inherited ptrace-denial launcher, run an installing-thread
  typed Tool with blocking global RPC; demand chosen non-native results and
  coordinator receipts, not just exit/stdout or PassthroughDispatcher.
- Repeated identical native syscall sites in ordinary text, a two-byte page-end
  location and a post-install anonymous executable mapping; one existing libc
  syscall path. Assert exact positive validated-SUD counts for each guest
  probe and no direct-hook path. Separate runtime-internal events and shared
  driver restart iterations from guest trap counts.
- Retain/check mapped executable bytes before and after; require actual
  enabled instrumentation counters for patch attempts and installed patches
  equal zero, no site table/planning and no vDSO rewrite. Disabled counters
  or a disk binary hash alone do not prove zero live patching.
- Reuse FallbackTool's errno, six-argument, tail/restart, register/redzone,
  repeated-result and RPC assertions (`src/bin/rpc_tool_guest/syscall_fallback.rs`).
  Existing `tests/rpc_tool.rs:39` evidence is a seccomp/fallback fixture,
  not a previously tested SUD-only mode.
- Add mechanism/ABI negative cases and preserve all standalone ordinary-signal
  regressions. Real clock trajectory, RPC-work invariance and original exact
  boundary-window tests remain required for any combined clock claim. Do not
  label a smaller syscall fixture as equivalent to the original clock suite.

Expected write scope: shared preload dispatch/trap and explicit signal-policy
seam; LiteInst typed installation/runtime selection, fallback ownership and
stats; focused typed test guest/runner. No new timer engine, syscall emulator,
Sentry port, loader, default change or backend promotion. Consolidation should
reuse existing shared preload controller/driver/RPC/ownership primitives.
`reverie-e9patch/src/tool_host.rs:343` also calls the shared driver; source
presence is not measured e9patch in-guest execution. SaBRe/e9patch backend
qualification is not established by this task.

Decision: prioritize the bounded SUD-only real Tool path, with patching optional
and separate. Keep f225 unchanged until the pre-edit checkpoint is accepted.
All startup reds, 340 denominator rows/static deferrals and >=306 target remain.

Question recorded for the coordinator: provide an existing authorized local
kernel source/docs path with provenance covering `kernel/entry/syscall_user_dispatch.c`,
x86 native/compat entry, legacy-vsyscall emulation, and fork/exec reset. This
is needed to close the universal-site/exception claim; it is not a request
to bypass network policy or to reintroduce mandatory patching.
