# Syscall user dispatch: bounded feasibility evidence

## Verdict

SUD works on this host as an in-process syscall trap after explicit per-thread
installation and does not impose the inherited-seccomp exec failure. Genuine
dynamic exec and new-image re-arming succeeded. It is **not** interception of
the replacement image's first loader syscall: the observed preinit and preload
constructor calls ran natively before re-arming. No Detcore, Tool migration,
continuous-clock, CLI, L2, or coverage qualification follows.

The current seccomp refusal and static refusal remain unchanged; all 340 frozen
reference labels remain in the denominator (target 306). No product source was
edited. Own fallback remains clean at
`7b51e521fcacbbe876c93d1cd0893c442c1a52f6`.

## Anchor and commands

- Raw directory: `/home/newton/work/dev-hermit/ignored/liteinst-sud-20260904`.
- Final source/runtime observation window: 2026-09-04T23:51:16Z to 23:51:19Z.
- Kernel: `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`, x86-64.
- Compiler: installed GCC 11.5.0, no Rust/Cargo invocation or toolchain override.
- Probe ELF SHA256: `788ce4d4d6d045f7aa0700c69222fd5b6a4d42aa0990fb3fce5651a99ef392e9`.
- Observer DSO SHA256: `6c90b8297ae137eee463648856a85487c5d9733d036b907127304daa3e2b5806`.
- Probe C SHA256: `13355573ea51b50fbecbc924d2798ba31dad67626a02f448b33debde9646996a`.
- Pinned product audit: Reverie `42d55f7db2c5d34e00b47800c561ec6da96f271e`,
  Hermit `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`. Neither product was executed.
- `admission.json` contains exact compile/execution commands. The runner was:

```text
DAGRUN_ENGINE=python /home/newton/work/dev-hermit/agent-utils/bin/dagrun run --dag /home/newton/work/dev-hermit/ignored/liteinst-sud-20260904/admission.json --max-steps 1 --max-cpus 2 --max-mem 1G --admission 0 --no-profile-feedback --run-timeout 150 --perf-dir /home/newton/work/dev-hermit/ignored/liteinst-sud-20260904/admission-profile-round2
```

Admission and containment were observed: 1 GiB, swap zero, CPU bandwidth two
cores, one active step; step deadlines 60s, execution wrapper 50s with 2s kill
grace, each case alarm 5s, core limit zero, file-size limit 1 MiB. The existing
`deny-ptrace` launcher installs KILL_PROCESS for ptrace before the controller;
its SHA256 is
`bb1ba6b3d7dfd20f4d99c364024a73c4f1f71349758b3c5403fac9e8e940843a`.
The explicit ptrace negative control dies with SIGSYS. No Hermit invocation
occurred; dagrun is the admitted/bounded route, not a direct guest bypass of
safehermit. The observer is a tiny external constructor, **not a Detcore DSO**.

`build.sha256`, `platform.sha256`, `elf.txt`, `disassembly.txt`, `compiler.txt`,
`observations.tsv`, `stderr.log`, `probe.exit`, and `admission.log` retain exact
inputs and outputs. The dynamic probe has PT_INTERP
`/lib64/ld-linux-x86-64.so.2`; its loader and libc hashes are recorded.

## Observations, not backend test results

Final round: 17/20 case expectations matched: 12 exit-zero checks and five
expected fatal negative controls. Three exec-startup cases remained red; the
probe and admission return 1. No expectation was relaxed to make them green.

| Case / observation | Exact outcome |
| --- | --- |
| First untrusted getpid, repeated | Two SIGSYS events, syscall 39, si_code 2 (SYS_USER_DISPATCH), arch 0xc000003e; returned chosen value 424242, not host PID |
| Trusted gate / signal return | Native getpid between the two traps; restorer in the 68-byte trusted range works with selector BLOCK |
| Null selector | Both outside-range calls still intercepted; no writable selector required for this control |
| Explicit ALLOW control | Exactly one intentionally allowed getpid returns native PID, no trap; not a backend policy |
| Explicit OFF control | Native getpid, no trap after successful OFF |
| Invalid mode / OFF arguments / overflowing range | Each -EINVAL (-22) |
| Non-user selector pointer | Setup -EFAULT (-14) |
| Accessible-address but PROT_NONE selector | Setup succeeds; next outside-range syscall kills with SIGSEGV, wait status 139 |
| Selector byte 2 / no handler / outside-range custom restorer | SIGSYS, wait status 159, each |
| fork reset | Child's getpid native, parent's next getpid still intercepted; child CPUID mode 0 and TSC mode 2 inherited |
| raw CLONE_THREAD reset | Child getpid native; parent still intercepted. Minimal assembly child, not pthread/TLS lifecycle qualification |
| Failed exec | Native execve -ENOENT (-2); SUD still intercepts; CPUID remains disabled (0), TSC mode remains SIGSEGV (2) |
| CPUID/RDTSC fault control | Own handler receives one SIGSEGV for each exact test instruction, si_code 128 |
| Dynamic exec, SUD only | Preinit -> observer DSO constructor -> main constructor -> main; getpid native, exit 0 |
| Dynamic exec, SUD + CPUID disabled | Same sequence; CPUID query changes 0 before exec to 1 in preinit; actual CPUID does not fault, exit 0 |
| Dynamic exec, new-image SUD re-arm | Same native startup sequence, then new gate address; main getpid intercepted with 424242, exit 0 |
| Dynamic exec, TSC trapping (alone or with CPUID trapping) | Three cases SIGSEGV, wait status 139, before any preinit/constructor marker; retained red |

The two combined-setting failures were found in the first round, retained under
`round1/` with its exact sources/binaries/logs, and repeated unchanged in the
final round. Seven additional controls isolate the settings rather than
relabeling those failures. An initial malformed admission JSON was rejected
before compilation; its log is retained separately.

## Source availability and limits

Read and copied the live kernel's installed `syscall_user_dispatch.h` and
`syscall_user_dispatch_types.h`, plus installed prctl UAPI and x86 arch_prctl
headers. The kernel UAPI `asm-generic/siginfo.h:301` defines SYS_SECCOMP=1 and
SYS_USER_DISPATCH=2. The prctl header defines OFF, exclusive ON, inclusive ON,
ALLOW=0 and BLOCK=1. This experiment uses exclusive ON only. These headers
describe per-task selector/range storage, **not the complete reset code**.

The installed kernel package contains Makefiles but not the implementation
`kernel/entry/syscall_user_dispatch.c`, x86 process implementation, or the SUD
selftest source. Local man bodies are absent. Retrieval of the official
kernel.org SUD documentation and man7 manual was denied by the content tool;
the explicitly suggested Meta CLI discovery route did not provide a reader.
No network bypass, download, source provisioning, or external model was used.
Consequently reset/restorer claims here are measured host behavior, not a
claimed line-by-line audit of the running kernel implementation. In particular
vDSO-specific restorer exceptions and inclusive mode remain unverified.

Earliest observable new-image point is executable `.preinit_array`, **not**
kernel entry or the dynamic loader's first syscall. TSC-only isolation makes
that setting causal for the early crash in this probe, but no fatal fault PC
or successful new-image PR_GET_TSC was captured in those crashing cases. Thus
TSC persistence through exec is consistent with the evidence, not directly
queried after successful replacement here. It must not be conflated with the
directly measured CPUID reset. No core dump, ptrace inspection, or kernel trace
was taken. No alternative ELF loader or static guest was used.

## Smallest proposed product scope (not implemented)

1. Add an opt-in `InProcessUserDispatch` implementation at the existing
   `reverie-preload/src/lifecycle.rs:52` controller seam, with a separate
   `reverie-preload/src/user_dispatch.rs` implementation. Install on the
   current thread only after handler/altstack and runtime state are ready;
   prefer tested null-selector behavior when no native-selector bypass is
   needed. Do not stack it as a supposed cure for an already installed
   irreversible trapping seccomp filter. Leave existing seccomp default intact.
2. Extend `reverie-preload/src/trap.rs:185` with mechanism-specific SIGSYS
   provenance (2 versus 1), preserving rejection of unrelated SIGSYS and
   validating syscall ABI. Reuse its existing event dispatcher and LiteInst's
   ordinary-context Tool/fallback handoff, not a second emulator. The probe
   tests only getpid's syscall ABI; six-argument and all-register continuation
   validation is still required before integrating that path.
3. Add a runtime-owned rt_sigreturn trampoline contiguous with the narrow
   syscall gate, and use it in `reverie-preload/src/signal.rs:42`. Current libc
   sigaction installation does not guarantee the restorer lies in the gate.
   Do not allow all libc text or silently toggle selector ALLOW during Tool
   dispatch. Preserve signal-frame ownership and existing nested-syscall rules.
4. Explicitly re-arm at fork/new-thread entry and new-image startup, before
   guest work. Protect SUD prctl configuration and runtime signal/FD state
   against guest changes. This is not thread support; existing restrictions
   remain until entry ordering is proved.
5. Separately implement genuine exec state handoff in LiteInst's tool_host,
   backend/bootstrap, RPC and guest_log modules: preserve process Tool plus
   serialized ThreadState, config, PRNG, record/replay, identities and clocks;
   await RPC/log delivery; distinguish exec from process exit; preserve the
   failed-exec continuation and rollback; restore then issue the real post_exec
   hook exactly once. ThreadState serde is required by Tool, but process Tool
   migration still needs an explicit opt-in contract.
6. Before lifting either exec refusal, solve startup and instruction-clock
   coverage. At the pinned source, `runtime.rs:808` enables CPUID/TSC faulting;
   CPUID must be re-armed, and TSC cannot simply remain enabled through this
   loader path. Turning off TSC until an ordinary constructor is **not** an
   accepted continuous-clock solution. The native preinit/constructor window
   already disproves full-startup Tool coverage for that approach. Early loader
   ownership, auxv/post_exec ordering and exact accounting need their own
   reviewed design; SUD alone does not supply them.

Works: PARTIAL (measured first-trap and exec/re-arm feasibility). Evidence UX:
explicit per-case statuses and preserved failures; no whole-backend claim.
