# Systrap/Sentry legacy-vsyscall handling: source-only addendum

Date: **2026-09-04, America/Los_Angeles**. This is a separate addendum to the approved `GVISOR-SYSTRAP-COMPARISON.md`, not a replacement or retroactive change to its measurements.

## Identity and conclusion

Primary source is the clean local `/home/newton/work/dev-hermit/experiments/gvisor` at **`012cf0b0b14d7b8aa2a4424867ecec8e6121e69d`**, local branch/tracking ref `perf/benchmark-v3-counter2` / `rrnewton/perf/benchmark-v3-counter2`. This is the same locally tracked fork snapshot as the original comparison, **not a claim about current upstream**. Paths and line numbers below are relative to that pinned root.

**The source explicitly handles the three application legacy-vsyscall entries. It does not rely on SUD alone.** Systrap installs host seccomp rules for these services, and its default path converts the resulting event into an execution fault that Sentry recognizes and emulates using its syscall table. This is primary-source evidence, not a successful gVisor runtime test on our host.

The original comparison remains SHA256 `33bd701b7e66796b9ea76d3006817316e9ed88736b2b0979191d990b912b4239`; its existing revision manifest verified. A preserved copy and the old manifest are in `/home/newton/work/dev-hermit/scratch/gvisor-vsyscall-source-review-9bJ2w2/`. All 16 cited source files were checked against their pinned Git blobs; that directory retains `primary-git-blobs.txt` and `primary-source.sha256`.

## Control flow in the pinned source

1. **Specific host trapping rules remain alongside SUD.** `pkg/sentry/platform/systrap/subprocess.go:288` enables SUD for application syscalls, allowing the reserved stub interval and specifying a null selector; EINVAL uses the existing seccomp fallback. Separately, `pkg/sentry/platform/systrap/sysmsg_thread_amd64.go:23` adds `seccomp.Trap` rules for `SYS_GETTIMEOFDAY`, `SYS_TIME`, and `SYS_GETCPU`, each marked `Vsyscall: true`. These are included by `sysmsg_thread.go:123`, copied into the stub by `stub_unsafe.go:56`, and installed on the execution thread by `subprocess.go:1179`. The initial stub also has architecture rules at `subprocess_amd64.go:185`, included by `subprocess_linux.go:140`; `createStub` selects default TRAP at `subprocess_linux.go:44`.

   `Vsyscall` is not a SUD selector: `pkg/seccomp/seccomp.go:546` renders a host seccomp instruction-pointer check for the high address bit before applying these rules. It is not an exact three-address comparison; the later Sentry emulation table supplies exact entry addresses. Consequently, “seccomp is merely unused when SUD works” would be an incorrect description of this source.

2. **Host signal becomes a Sentry event.** `pkg/sentry/platform/systrap/sysmsg/sighandler_amd64.c:236` copies host signal information/register state into the shared application context; SIGSYS selects a syscall context at line 251, and `switch_context_amd64` transfers that context at line 339. On the Sentry side, `subprocess.go:863` retrieves registers and signal information. For syscall contexts, it invokes `maybePatchSignalInfo` at line 877.

   In the default patching-enabled configuration, `subprocess_amd64.go:151` recognizes a signal address in the legacy-vsyscall range, changes the event to SIGSEGV, sets RIP to that address, and subtracts eight from RSP. Its comments explicitly describe undoing the host's vsyscall-emulation stack adjustment. **This function edits signal/register information, not application instructions.** `subprocess.go:878` then returns execute access and no syscall-patching request; `systrap.go:210` exposes `platform.ErrContextSignal` to Sentry.

3. **Sentry recognizes and services the ABI entry.** In `pkg/sentry/kernel/task_run.go:274`, Sentry first attempts ordinary memory-fault handling. If that fails and the access is execution, lines 294–296 consult `LookupEmulate` and call `doVsyscall`. A natural execution fault can also reach this common path; no host-emulation runtime result is claimed for such configurations here. `kernel/syscalls.go:460` looks up the exact address in the syscall table. The amd64 table at `pkg/sentry/syscalls/linux/linux64.go:402` maps:

   | Application entry | Sentry syscall number and handler |
   |---|---|
   | `0xffffffffff600000` | 96, `Gettimeofday` (`linux64.go:141`) |
   | `0xffffffffff600400` | 201, `Time` (`linux64.go:246`) |
   | `0xffffffffff600800` | 309, `Getcpu` (`linux64.go:354`) |

4. **Application return and failures are modeled by Sentry.** `pkg/sentry/kernel/task_syscall.go:376` reads the application return address from its stack and obtains arguments from application registers. It applies the application's Sentry-modeled seccomp/ptrace conditions before invoking the implementation; these are distinct from the host seccomp trapping rules above. `doVsyscallInvoke` at line 440 calls the shared `executeSyscall`, whose table dispatch is at line 136. Success sets the return value, resumes at the saved caller and advances the application stack by one word (lines 449–461). A bad caller-stack read or an implementation EFAULT produces an application SIGSEGV rather than a fabricated successful return.

   The actual services are Sentry implementations: `sys_time.go:164` reads `Kernel().RealtimeClock()` for `Time`; `sys_time.go:295` copies its realtime value for `Gettimeofday` (and handles the optional timezone); `sys_thread.go:851` returns `t.CPU()` and node zero for `Getcpu`. Merely owning these service implementations does not establish deterministic Hermit-style time or guest-RCB accounting.

## Optional patching caveat

The flag `systrap-disable-syscall-patching` defaults to false (`runsc/config/flags.go:123`). It must not be conflated with the separate seccomp rules: the execution-thread vsyscall rules above are not conditional on that flag. However, **`maybePatchSignalInfo` explicitly returns false when patching is disabled** (`subprocess_amd64.go:152`). The remaining syscall-context branch updates registers and reports an ordinary syscall event (`subprocess.go:880`, `subprocess_amd64.go:82`, `systrap.go:198`), reaching Sentry's ordinary `doSyscall` entry (`task_run.go:255`).

That is a concrete source-level path difference. This bounded inspection does not prove equivalent legacy return, error, tracing, or signal semantics with the flag enabled versus disabled; no such runtime comparison was run. It also does not prove that rewriting a legacy entry or caller is required: the explicit default legacy conversion returns without requesting a syscall-site patch.

## Relationship to the observed Hermit gap

Our independent frozen native-preload experiment, recorded in `/home/newton/work/dev-hermit/scratch/vsyscall-sud-review-fheioR/REVIEW.md`, observed three successful legacy services with **0/3 SUD interceptions**, while **6/6 bracketing ordinary raw controls** reached the dispatcher and returned nonnative sentinels on Linux `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, personality zero, execute-only legacy mapping. That controller was Reverie `8e8d36adf2ea220c35a5d2bc0c6a41aa90f92675`, not a gVisor execution or shared Detcore run.

The useful comparison is therefore specific: **SUD interception of ordinary syscall instructions is not by itself coverage of every application request for a Linux service. This gVisor snapshot contains an additional host trap plus Sentry service-emulation path for legacy-vsyscall.** It is relevant prior work for accounting for or refusing that route, not a ready-made in-process Detcore implementation. Our measurement neither proves that an ordinary SYSCALL instruction escaped SUD nor proves that patching is the unique solution. No complete host-kernel call chain, all-host behavior, gVisor runtime success, arbitrary in-process callback safety, precise timers, deterministic scheduling, CLI/L2, or coverage-target reduction follows.

## Verification limits and preservation

Only local primary-source reads, searches, Git-object/content hash checks and external documentation writes were performed. No gVisor build/run, kernel probe, network retrieval, product edit, recursive agent or model CLI occurred. No tests/assertions, tolerances, comparators, case selection, failure labels or coverage targets were changed. The original comparison and its manifest were preserved and rechecked; this addendum adds a scoped source finding instead of revising old evidence into a stronger result.
