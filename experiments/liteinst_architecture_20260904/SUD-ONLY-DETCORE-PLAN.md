# No-patch SUD: remaining shared Detcore requirements

Read-only checkpoint, 2026-09-04. No product edits, builds, runs, external probes,
delegation or new approval claim. The SUD-only component remains frozen for Carson.

## Decision

**A constructor switch is not enough for a valid default Detcore run.** First
resolve no-patch instruction/vDSO interception and the shared clock/timer entry
contract. Use the existing actual Detcore preload and coordinator harness as the
next integration target, not another substitute Tool. Do not disable subscriptions,
virtual time or the default maximum timeslice to make installation succeed.
Loader-start coverage is a separate unresolved requirement, not supplied by SUD.

## Exact current refusal and initialization order

Hermit `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`, `detcore/src/lib.rs:1063`:
normal run configuration has `passthru_opt=false`, so subscriptions are
`Subscription::all()`. Reverie `reverie/src/subscription.rs:38` defines that as
**all syscall numbers plus CPUID and RDTSC** (the RDTSC event also covers RDTSCP).
This is not just a time-syscall allow-list. Enabling passthrough is not a remedy:
it drops generic syscall-time observations and still requests these events when
the normal virtualization flags are enabled.

Frozen `reverie-liteinst/src/tool_host.rs:190` proceeds as follows:

1. Reject requested/active selected shared clock in SUD-only mode, before RPC.
2. Prepare existing fallback save state, connect shared coordinator RPC, reserve
   its protected descriptor, initialize optional old RCB reading, obtain config.
3. Reject the following exact **seven subscription categories** as one combined
   Unsupported result. This is before `T::new`, signal/controller installation
   and `ToolHost` publication; no shared Detcore callback has executed here.

| Subscription rejected at `tool_host.rs:221` | Observation required without patching |
|---|---|
| CPUID | Existing `Tool::handle_cpuid_event(eax, ecx)` |
| RDTSC (including RDTSCP) | Existing `Tool::handle_rdtsc_event(request)` |
| `time` | Direct syscall and `__vdso_time` return/output buffer |
| `gettimeofday` | Direct syscall and `__vdso_gettimeofday` outputs |
| `clock_gettime` | Direct syscall and `__vdso_clock_gettime` outputs |
| `clock_getres` | Direct syscall and `__vdso_clock_getres` outputs |
| `getcpu` | Direct syscall and `__vdso_getcpu` outputs |

The five syscall instructions are already representable by SUD; their rejection
is conservative because their vDSO function bodies can complete without a
syscall. The authoritative existing rewrite table is
`reverie-ptrace/src/vdso.rs:154`; `patch_current_vdso` at :278 overwrites these
entries and is deliberately bypassed. This table is not proof of exhaustive
coverage of newer kernel vDSO exports, notably possible user-space getrandom.
Their presence/implementation was not probed in this checkpoint.

4. Once subscription capability is genuinely implemented, installation must
   still pass exact CPUID/TSC capability setup and signal disposition/mask checks.
   SUD-only refuses existing custom handlers or blocked SIGSYS; those are caller
   admission rules, not Detcore subscription operations. Tool Rust allocation
   isolation already exists; it does not qualify libc/TLS/lock reentry.
5. `T::new` validates config and creates the same shared Detcore. On the first
   event, `tool_host.rs:346` initializes ThreadState and calls thread-start, then
   the root post-exec hook, then the event. Detcore thread-start registers with
   the global scheduler by RPC, selects the next timeslice, reads/updates time
   and calls its post-hook (`detcore/src/lib.rs:1619`).
6. Default maximum timeslice is 200000000 virtual ns
   (`detcore-model/src/config.rs:488`); `tool_local.rs:2526` and :2721 establish
   its deadline. `lib.rs:623` and :723 request an RCB timer, precise by default,
   and expect success. **Both SUD-only setters explicitly return EOPNOTSUPP**
   (`tool_host.rs:1105`). Thus merely removing the subscription guard would
   expose another real failure, including in a nonsequentialized default config.
   PMU-reading availability is separate: `runtime.rs:348` tolerates old-reader
   setup failure unless the selected clock is requested; :494 then refuses an
   unavailable read. A readable counter is not timer delivery or exact accounting.

## Smallest changes that could satisfy the observations

### CPUID and TSC: reuse kernel faults and shared Tool, not rewriting

Existing `runtime.rs:712`, :822 and :2571 already use `ARCH_SET_CPUID(0)` and
`PR_SET_TSC(PR_TSC_SIGSEGV)` plus SIGSEGV. They affect a thread's instruction
execution, not ELF symbol lookup; libc/JIT/text can fault alike after arming.
Retained host controls received these faults. They do not establish startup or
general asynchronous safety. Expose control failures honestly and retain guest
control virtualization (`detcore/src/syscalls/misc.rs:274`), rather than letting
guest control calls silently re-enable native observations.

The current instruction handler **cannot simply be enabled in no-patch mode**:
`runtime.rs:2351` decodes through patch arenas, and :2420 claims a site, installs
a hook and redirects to its trampoline. No-patch has no such arenas.

Smallest write scope: `runtime.rs` instruction validation/dispatch and
`syscall_fallback.rs` ordinary saved-context continuation, plus focused tests.
Introduce a bounded instruction-kind/length/resume description independent of
patch planning; safely validate fault provenance, readable bytes and actual
CPUID/RDTSC/RDTSCP at the saved PC. Unrelated SIGSEGV must not be misclassified.
Reuse `tool_host::dispatch_instruction` (:300/:496) and the existing shared
Detcore handlers (`detcore/src/lib.rs:1276/:1328`) unchanged. Their timing and
global-clock RPC updates are the semantics; a native answer is not equivalent.

Do not reuse syscall register clobbers: the frozen fallback assumes a two-byte
syscall and uses RCX as its return address. CPUID produces RCX, RDTSC preserves
it, and RDTSCP is three bytes and produces it. The continuation must independently
retain the resume PC and restore every non-output register/flags/stack/redzone
and supported FP state. Borrow the kernel frame only until rt_sigreturn, then
use the existing ordinary-context saving path; no fabricated signal frame or
HookContext IP/SP restoration assumption. Runtime-native instruction scopes
must end before returning to guest and cannot admit guest-handler callbacks.
The existing instruction scope/native helpers may be reused only under that
ownership contract, not as a guest bypass.

### vDSO: choose an explicit function-entry mechanism

TSC faulting inside a vDSO function is **not** sufficient: some paths read only
vvar data, constants or CPU identifiers; clock conversion using host vvar data
is not Detcore clock_gettime semantics. SUD cannot see those paths.

Two possible no-instruction-rewrite approaches need an owner decision and a
separate design review; neither is implemented or authorized by this note:

- **Before-loader vDSO exclusion:** arrange for the real loader/libc to select
  syscall fallbacks before it caches `AT_SYSINFO_EHDR`. Late auxv editing or
  LD_PRELOAD symbol interposition is insufficient for cached/direct vDSO calls.
  This changes guest-visible auxv/vDSO availability, needs protection against
  rediscovery/direct entry, and depends on currently missing startup ownership.
  It is not a constructor-only fix and does not preserve arbitrary direct calls.
- **Exact vDSO-entry faults:** investigate non-executable vDSO pages, validated
  instruction-fetch faults at known symbol entries, then ordinary-context
  function-ABI adaptation to the same Tool syscall driver. Retain original bytes;
  return with real function stack/return-address and output-buffer semantics,
  not the syscall RCX continuation. Unknown entry/interior PCs and unsupported
  exports must fail explicitly. Mapping permissions become guest-visible;
  runtime/libc vDSO use, symbol aliases, memory faults and shared signal/clock
  ownership need resolution. Never temporarily expose a native guest interval
  or implement a second clock/random engine. This is a feasibility design, not
  a claim that mprotect or this mapping policy works here.

No existing source in this bounded audit supplies either complete mechanism.
For post-install Detcore diagnostics, exact entry interception is the more direct
candidate; it is not approved for implementation, arbitrary guests or startup.

### RDRAND/RDSEED: a distinct remaining limitation

Detcore's CPUID table masks their advertised bits (`detcore/src/cpuid.rs:39`);
it does not disable raw instructions on supporting hardware. Neither the shared
Subscription API nor the audited SUD/CPUID/TSC controls supplies their interception.
A library consulting the virtual CPUID may avoid them, but direct or previously
selected execution can still obtain host randomness. No source-supported
process-local no-rewrite remedy was found. A constrained fixture must explicitly
exclude and report these instructions; arbitrary-program qualification requires
a separately proven execution/hardware boundary or interception mechanism. Do
not set a backend CPUID-policy flag as a substitute for implementing it.

### Clock, signals and startup: dependencies, not configuration relaxations

SUD's `user_dispatch.rs:16` runtime mask and forwarding remask block signals;
`clock_boundary.rs:9` provides enter/leave ownership but no authenticated signal
mask policy. Exact owned SIGTRAP windows cannot be deferred or globally unblocked.
The frozen API therefore refuses combined clock selection. Align runtime-owned
trap masks, scalar continuation ownership, ordinary Tool/allocator transitions,
and the timer owner's real arm/delivery contract before admission. Preserve
guest-owned SIGTRAP/TF semantics and precise position requirements; equal RCB
alone is not precise position. Do not import timer0033 as if it alone proves this
composition. No disabled timers, fake successful setters, clock reset, rounding
or first-read normalization are acceptable ways to pass Detcore.

Current preload attaches after some loader activity. Existing root post-exec
delivery is late: Detcore `lib.rs:1694` writes AT_RANDOM assuming libc has not
copied it, whereas the actual constructor and first callback occur later. That
cannot retroactively change stack canaries, cached CPU choices or earlier random
reads. The old retained SUD experiment observed native replacement preinit and
constructor work before rearm and CPUID reset; its three TSC-startup SIGSEGV
cases remain red. TSC persistence was consistent with those failures, not queried
after successful replacement. A new loader/VM/protection design is not supplied
here. No fake post-exec callback or temporarily disabled TSC closes this gap.

## Concrete next actual-Detcore test

Use the retained logging harness, not the RawTool's emulation:
`ignored/liteinst-logging-teardown-20260904/harness/src/main.rs:116` calls
`LiteinstBackend::run_with_output_and_preload_data_and_log::<Detcore>`; the frozen
DSO source calls `install_tool_from_bootstrap::<Detcore>` at :63 after validating
the config fingerprint and setting up shared logging. Its source is in the
checked-source archive, not a request to edit another worker's active files.

After the dependencies above are reviewed, add **only** an explicit bootstrap
mode-selection seam preserving sealed config/logging/RPC, and use that harness
with all subscriptions and the unchanged strict-equivalent config (including
maximum timeslice and precise timers). A controlled dynamic fixture should
exercise CPUID, RDTSC/RDTSCP, all five time/CPU functions through both raw syscalls
and direct vDSO entries, then real getrandom/write/exit. Require actual shared
Detcore local events and global RPC, repeated continuous time trajectories,
live byte equality and enabled zero-patch counters under inherited ptrace denial.
Also require real timer arm/readback/delivery and truthful log completion.

Keep this first result labeled **post-install shared Detcore diagnostic**, with
explicit startup and raw-random-instruction exclusions; never L2/CLI/backend or
arbitrary-program credit. The full default Detcore route stays refused until its
declared subscriptions are supported. If the first dependency still prevents
installation, retain that first exact refusal rather than weaken config. No
need to extend generic fixtures to claim progress while waiting for it.

## Source identity and read-only evidence

- Reverie base `f22573b9335a258316b8d1c9eb0e5f865affd7a1`, frozen patch
  `ac30f54baf17d82d52cdc554f4f02f7a9b0ad93ec9451104ee2f65bf90b4f97a`, tree
  `f507be189dfcf7d85d6cb962987dee61c98851e1`. Reverie paths above are relative to
  `/home/newton/work/dev-hermit/worktrees/slots/liteinst-exec` at that source.
- Hermit paths above refer to immutable `git show 4d8f866102882b6eeabdf99cc7e81433cc3c95c5:<path>`
  in `/home/newton/work/dev-hermit/worktrees/slots/liteinst-owner`, not foreign
  staged config/model edits observed there. Nothing in that slot was modified.
- Archived Detcore DSO source:
  `ignored/liteinst-logging-teardown-20260904/checked-source.tar`, member
  `liteinst-runtime-build/detcore-runtime/src/lib.rs`. Harness SHA256
  `813a6e37e74a9a757731e1445f02b7b049f5c11fe392b7cff4dac57afa0612df`
  matches the retained checked-source manifest. No DSO executed.
- Existing host reset/fault evidence: `SUD-FEASIBILITY.md:64` and
  `ignored/liteinst-sud-20260904/`; full kernel implementation remains absent.
  These are retained observations, not newly executed tests or an inferred kernel
  source audit. No new universal instruction/ELF coverage assertion.
- Frozen source hashes checked unchanged:
  `ignored/liteinst-sud-only-20260904/source-check-detcore-plan.log`.
  All 340 labels/static deferrals and >=306 target remain unchanged.
