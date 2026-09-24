# Narf primary-source fact set

Scope: pinned public Narf implementation, documentation, tests, and observed CI
execution relevant to a Hermit-on-Narf functional-equivalence POC. This is an
evidence artifact for Audit A, not reader-facing design prose and not a backend
recommendation.

## Source identity and evidence boundary

- **Verified current:** the inspected repository is
  [`hodgesds/narf`](https://github.com/hodgesds/narf), whose Cargo workspace
  declares that same repository URL
  ([`Cargo.toml` lines 143–149](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/Cargo.toml#L143-L149)).
- **Verified current:** all source claims below are pinned to commit
  [`afe919924a5fd2c8e820ab3a0564b6d45438e6bc`](https://github.com/hodgesds/narf/commit/afe919924a5fd2c8e820ab3a0564b6d45438e6bc),
  committed 2026-09-24T21:30:23Z. The clone was clean and matched
  `origin/main` when inspected.
- **Verified current (source-only)** means an implementation path was read at
  that commit. It does not mean it booted.
- **Verified current (executed)** means an exact-commit CI log showed the named
  case run and pass. Test declarations and comments are never promoted to this
  class.
- **Unresolved:** repository ownership outside GitHub and independent project
  governance were not established. Repository identity is strong enough to pin
  the public code under discussion, not to infer external endorsement.

## Canonical current-state facts

### 1. Kernel architecture, objects, and lifecycle

| Surface | Canonical fact | Evidence class |
|---|---|---|
| Kernel shape | Narf is a Rust `no_std` kernel workspace for x86_64 and aarch64. The implementation is split into a trusted frame, memory/domain management, scheduler, capabilities, userspace, VFS/network, drivers, BPF, and loadable-module crates. | **Verified current (source-only)**; workspace members are enumerated in [`Cargo.toml` lines 4–125](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/Cargo.toml#L4-L125). |
| Trust boundary | The specification names `{frame, memory domain manager, capabilities table code, scheduler executor core}` as the kernel TCB; code outside it is untrusted relative to framekernel guarantees. Capabilities say *what* may be done and domains constrain *where memory access may reach*. | **Verified current (versioned specification)**; [`security-model/specification/spec.md` lines 31–44](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/security-model/specification/spec.md#L31-L44). Actual enforcement is qualified below. |
| Domain identity | Sixteen `DomainId` values are reserved. `FRAME=0`, `CAPS=1`, `MEMORY_MGR=2`, `SCHED=3`, `IPC=4`, `TRACER=5`, `KEYS=6`, `OBSERVE=7`, `USERSPACE_K=8`, driver domains 9–13, `BPF=14`, `SCRATCH=15`. | **Verified current (source-only)**; constants are defined in [`lib/src/id.rs` lines 62–92](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/lib/src/id.rs#L62-L92) and pinned by tests at [`lib/src/tests.rs` lines 636–654](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/lib/src/tests.rs#L636-L654). |
| Runtime-selected domain backend | `DomainBackend` has `Pks`, `Mte`, `Pcid`, `Sfi`, and `Unenforced`; `effective_backend()` is the runtime answer. `Sfi` is reserved, not implemented. | **Verified current (source-only)**; [`arch/src/lib.rs` lines 28–50](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/arch/src/lib.rs#L28-L50) and [lines 115–126](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/arch/src/lib.rs#L115-L126). |
| Domain crossing | `DomainPrimitive` exposes save/restore, per-domain rights, and balanced `enter_domain(FRAME, target)`/`exit_domain(saved)`. The currently running domain is recorded per CPU, supports nesting, and scope comments state that a scope must not span a context switch. | **Verified current (source-only)**; [`arch/src/lib.rs` lines 128–192](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/arch/src/lib.rs#L128-L192) and [lines 287–321](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/arch/src/lib.rs#L287-L321). |
| x86_64 enforcement | PKS changes `IA32_PKRS`; the fallback uses per-domain PCID-tagged page-table roots and CR3 switching. If neither is active, scopes are inert and the effective backend can be `Unenforced`. | **Verified current (source-only)**; backend dispatch and caveats in [`arch/src/lib.rs` lines 28–66](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/arch/src/lib.rs#L28-L66), module entry in [`modules/src/domain.rs` lines 122–171](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/modules/src/domain.rs#L122-L171). |
| aarch64 enforcement | The MTE implementation saves/restores `SCTLR_EL1` and `GCR_EL1` and changes TCF to synchronous checking on entry. Its `get_rights` reports allow-all and `set_rights` is a no-op because mapping-time tags/AP bits carry policy. | **Verified current (source-only)**; [`arch/src/aarch64/mte.rs` lines 65–159](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/arch/src/aarch64/mte.rs#L65-L159). The breadth of tagged allocations remains **Unresolved**, because nearby docs/comments disagree (contradiction C4). |
| Isolation ceiling | FRAME/domain-0 remains accessible during domain execution. The current domain system primarily provides cross-domain isolation; ordinary untagged kernel memory remains reachable, and PCID confines only explicitly separated slots. | **Verified current (spec/source analysis)**; [`docs/DOMAIN_BACKENDS.md` lines 43–83](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/docs/DOMAIN_BACKENDS.md#L43-L83) and BPF threat statement in [`bpf/specification/domain-confinement.md` lines 110–136](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/bpf/specification/domain-confinement.md#L110-L136). |
| Boot object | Boot handoff is normalized to `BootInfo`: memory regions, kernel image, framebuffer, RSDP/DTB, initramfs, and command line. x86_64 parses Multiboot2 or Xen PVH; aarch64 parses an FDT/U-Boot-style handoff. | **Verified current (source-only)**; [`boot/src/info.rs` lines 36–78](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/boot/src/info.rs#L36-L78), [`boot/src/x86_64/mod.rs` lines 72–160](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/boot/src/x86_64/mod.rs#L72-L160), and [`boot/src/aarch64/mod.rs` lines 60–232](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/boot/src/aarch64/mod.rs#L60-L232). |

### 2. Boot and Linux-executable prerequisites

- **Verified current (source-only):** the documented build targets are
  `x86_64-unknown-none` and `aarch64-unknown-none`; QEMU, Rust nightly, and
  firmware/image tools are build/test prerequisites
  ([`TESTING.md` lines 23–42](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/TESTING.md#L23-L42)).
- **Verified current (executed):** exact-commit CI booted both architecture
  paths far enough for `boot-smoke / aarch64` to pass. At the same commit,
  `boot-smoke / x86_64` failed and both full kernel-test jobs failed; see the
  runtime section. Therefore “both arches boot” is not a green-at-this-revision
  blanket claim.
- **Verified current (source-only):** the ELF/process loader creates a distinct
  address space, maps `PT_LOAD` segments, supports `PT_INTERP`, applies
  relocations, creates a SysV startup stack/auxv, stages TLS, and uses the
  interpreter entry when one is resolved. A missing interpreter fails the load
  rather than entering the application directly
  ([`userspace/src/process.rs` lines 194–337](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/process.rs#L194-L337)).
- **Verified current (source-only):** an interpreter may be registered as an
  in-memory blob or loaded from VFS by its `PT_INTERP` path
  ([`userspace/src/interp.rs` lines 1–45](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/interp.rs#L1-L45),
  [`userspace/src/process.rs` lines 294–385](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/process.rs#L294-L385)).
- **Verified current (source-only):** `execve` takes Linux-shaped pathname,
  argv, and envp arguments and routes to the shared resolved-exec path
  ([`userspace/src/handlers/sys_execve.rs` lines 4–30](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_execve.rs#L4-L30)).
- **Unresolved:** the source still labels parts of the aarch64 SysV stack setup
  as x86_64-only (for example [`userspace/src/process.rs` lines 736–752](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/process.rs#L736-L752)).
  Exact parity for an arbitrary Linux guest binary on aarch64 was not executed.

### 3. Linux syscall path and syscall-data movement

- **Verified current (source-only):** a live user trap is decoded to a `Syscall`
  variant, looked up in the installed `SyscallTable`, wrapped in an
  `ArgsOnlyCtx`, dispatched, then followed by ptrace exit-stop and signal
  delivery before returning to user mode
  ([`userspace/src/syscall.rs` lines 3720–3805](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/syscall.rs#L3720-L3805),
  [lines 3847–3887](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/syscall.rs#L3847-L3887)).
- **Verified current (source-only):** syscall arguments are six scalar words in
  `SyscallArgs`; pointer-shaped data is copied explicitly. `validate_user_range`
  rejects over-large, null, overflowing, or non-user-half ranges. x86_64 copies
  are SMAP-bracketed and fault-fixed-up; aarch64 uses its guarded uaccess path
  ([`userspace/src/handlers/compat.inc.rs` lines 2515–2604](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/compat.inc.rs#L2515-L2604),
  [lines 2690–2728](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/compat.inc.rs#L2690-L2728)).
- **Verified current (source-only):** there is no generic syscall-interposition
  trait in the pinned tree. The live dispatcher contains concrete calls to
  `ptrace_syscall_stop` on entry and exit and a compile-time textual tracing
  block. Paired repository-wide queries for `interpos|syscall hook|syscall
  observer|before_syscall|after_syscall` and naive `hook|emulation` found no
  separate typed interposition API. This is an absence result, not proof that no
  ad-hoc call site exists outside the queried languages.
- **Software helper:** feature `syscall-trace` prints textual `SYSC` and `SYSR`
  lines to the synchronous serial console. Source comments explicitly warn that
  full tracing perturbs latency
  ([`userspace/Cargo.toml` lines 50–62](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/Cargo.toml#L50-L62),
  [`userspace/src/syscall.rs` lines 3757–3788](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/syscall.rs#L3757-L3788)). It is not a stable structured trace interface.

### 4. Ptrace: implemented contract and exact gaps

`Syscall::Ptrace` is installed by the live compatibility table and the shim
forwards to `crate::ptrace::sys_ptrace`
([`userspace/src/handlers/compat.inc.rs` lines 10949–10955](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/compat.inc.rs#L10949-L10955),
[`userspace/src/handlers/sys_ptrace.rs` lines 4–24](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_ptrace.rs#L4-L24)).

| Request or behavior | Current source behavior | Evidence status |
|---|---|---|
| `PTRACE_TRACEME` | Installs parent as sole tracer; second call returns `EPERM`. | **Verified current (executed x86_64 fixture)** and source [`ptrace.rs` lines 757–787](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L757-L787). |
| `PTRACE_ATTACH` | Resolves caller PID namespace (container build), requires a real task, rejects self/already-traced/credential or dumpability failure, records tracer, and raises pending `SIGSTOP`. | **Verified current (executed x86_64 fixtures)** and source [`ptrace.rs` lines 735–755](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L735-L755), [lines 788–846](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L788-L846). |
| `PTRACE_DETACH` | Validates low-32-bit signal number, removes all relationship/stop/option state, wakes tracee. **Gap:** it does not enqueue/deliver the requested detach signal. | First part **Verified current (executed fixture)**; signal gap **Verified current (source-only)** from [`ptrace.rs` lines 847–889](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L847-L889). |
| `PEEKTEXT`/`PEEKDATA`, `POKETEXT`/`POKEDATA` | Translates in the target task's address-space root, reads/writes one `u64` through the kernel mapping, and distinguishes tracee address `EIO` from tracer-buffer `EFAULT`. | **Verified current (source-only)**; [`ptrace.rs` lines 891–987](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L891-L987). |
| `process_vm_readv` / `process_vm_writev` | Supports only self or another task sharing the caller's `AddressSpace` (for example, a `CLONE_VM` thread). A target with a distinct address space returns `EPERM`. The implementation gathers the entire source into a kernel buffer before scattering; a source-segment fault occurs before destination writes, while a later destination-segment fault can follow earlier writes, but either fault returns `EFAULT` rather than a byte count. | **Verified current (source-only), fatal for stock external Reverie's bulk-memory path:** [`core.inc.rs` lines 6880–6887](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6880-L6887), [lines 6940–6979](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L6940-L6979), and [lines 7001–7040](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/core.inc.rs#L7001-L7040). Stock Reverie uses these syscalls from the distinct tracer process for bulk access ([build-time Reverie pin `ae1d1da…`, `safeptrace/src/memory.rs` lines 62–125](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/safeptrace/src/memory.rs#L62-L125)). **Unresolved (paired Linux comparison):** no pinned Linux implementation analysis or executed Linux/Narf pair establishes the exact native Linux result for every proposed page/segment fault boundary, so this artifact does not classify Narf's boundary result as a proved Linux divergence. |
| `GETREGS`/`SETREGS` | Copies architecture-specific `user_regs_struct` to/from tracer memory. x86_64 includes `orig_rax`, RIP/RSP, flags, and FS base; aarch64 includes x0–x30, SP, PC, PSTATE. | **Verified current (executed x86_64 end-to-end fixture)**; structs [`ptrace.rs` lines 125–166](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L125-L166), requests [lines 988–1035](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L988-L1035). |
| `GETREGSET`/`SETREGSET` | Only `NT_PRSTATUS` is supported. GET clamps output length. SET requires a full struct even though Linux permits partial multiples of the register width; source labels this a Linux gap. | **Verified current (source-only; negative ordering fixture executed)**; [`ptrace.rs` lines 1143–1259](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L1143-L1259). |
| `PEEKUSER`/`POKEUSER` | Operates only on the modeled register struct. Misaligned/out-of-range offsets are `EIO`; offsets in Linux's larger `struct user` beyond registers are absent. | **Verified current (executed x86_64 fixture)**; [`ptrace.rs` lines 1260–1355](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L1260-L1355). |
| `CONT`, `SINGLESTEP`, `SYSCALL` | Clears stop, wakes the tracee, and for `SYSCALL` arms entry/exit phase; `SINGLESTEP` sets x86 TF or aarch64 PSTATE SS. Data >64 is `EIO`. **Gap:** signal values 32–64 are accepted but not enqueued because the delivery arm checks `<32`. | **Verified current (source-only)**; [`ptrace.rs` lines 1036–1100](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L1036-L1100). x86 syscall-stop fixture executed. |
| syscall entry/exit stops | The live x86_64 syscall path stops before dispatch and after a completed call, preserves return `rax`, pins `orig_rax`, and uses `SIGTRAP|0x80` under `TRACESYSGOOD`. Re-executed blocking syscalls skip a false exit stop. | **Verified current (executed x86_64 fixture)**; [`ptrace.rs` lines 414–493](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L414-L493), dispatcher links above. |
| syscall-stop architecture | Source explicitly says the syscall-stop path is x86_64-only; aarch64 never reaches the x86 `orig_rax` logic. | **Verified current (source-only)**; [`ptrace.rs` lines 470–487](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L470-L487). |
| `PTRACE_SETOPTIONS` | Accepts the Linux `PTRACE_O_MASK`, rejects bits outside it, stores accepted bits. Only `PTRACE_O_TRACESYSGOOD` changes behavior. `TRACEFORK`, `TRACECLONE`, `TRACEEXEC`, `TRACEEXIT`, `EXITKILL`, `SUSPEND_SECCOMP`, etc. are accepted and ignored; no requested `PTRACE_EVENT_*` stop is generated. | Mask validation **Verified current (executed fixture)**; ignored-event gap **Verified current (source-only)** from [`ptrace.rs` lines 69–81](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L69-L81), [lines 1102–1141](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L1102-L1141). |
| `PTRACE_KILL` | Requires tracer ownership, clears stop state, queues SIGKILL, wakes tracee. | **Verified current (executed fixture)**; [`ptrace.rs` lines 1356–1395](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L1356-L1395). |
| stop-state validation | **Gap:** requests validate tracer ownership but do not check that the tracee is currently ptrace-stopped. The `stopped` map is written/read for wait/reporting, but no request arm performs Linux `ptrace_check_attach`'s stopped-state check. | **Verified current (source-only absence/inverse check)**; all `r.stopped` uses are at [`ptrace.rs` lines 227–293](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L227-L293), [lines 534–552](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L534-L552), and state-clearing arms. |
| missing requests | `PTRACE_SEIZE`, `INTERRUPT`, `LISTEN`, `GETSIGINFO`, `SETSIGINFO`, `GETEVENTMSG`, `PEEKSIGINFO`, `GET_SYSCALL_INFO`, signal-mask requests, seccomp-filter requests, and rseq configuration are not request arms. Unknown requests return `EIO`. | **Verified current (source-only exhaustive request-arm inventory)**; constants/arms [`ptrace.rs` lines 40–58](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L40-L58), [lines 725–1422](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/ptrace.rs#L725-L1422). |
| RDTSC/RDTSCP faulting | `prctl(PR_SET_TSC, PR_TSC_SIGSEGV)` accepts the Linux value and returns success, but does not store the selected mode or disable RDTSC; `PR_GET_TSC` always reports enabled. | **Verified current (source-only gap)**; [`sys_prctl.rs` lines 385–431](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_prctl.rs#L385-L431). This prevents the RDTSC Tool events requested by the default fail-closed Detcore subscription ([Detcore default](https://github.com/rrnewton/hermit/blob/ba9c4f4475e4ce187c6b0683dd60898e74b2b0e2/detcore/src/lib.rs#L1077-L1090), [`Subscription::all`](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie/src/subscription.rs#L35-L44), [ptrace setup](https://github.com/rrnewton/reverie/blob/ae1d1da78d2d89a1fa0454ee2282cf135e2286ed/reverie-ptrace/src/tracer.rs#L1638-L1650)). |
| CPUID faulting | `arch_prctl` defines only FS/GS operations; unknown subcodes return `EINVAL`. `ARCH_SET_CPUID`/`ARCH_GET_CPUID` are absent, so CPUID cannot be converted to a fault for the tracer. | **Verified current (source-only gap)**; full handler [`sys_arch_prctl.rs` lines 4–88](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_arch_prctl.rs#L4-L88). This prevents the CPUID Tool events in the same default Detcore subscription. |

**Intersection fact for a Reverie/Hermit audit:** Narf now has the essential
mechanism for x86_64 syscall entry/exit stops plus register and tracee-memory
access. It does **not** yet provide the complete modern ptrace state machine:
seize/interrupt, event options and event messages, stopped-state validation,
and several signal semantics remain absent or observably divergent. In
addition, its bulk `process_vm_*` path rejects the external tracer's distinct
address space, and the instruction-fault controls required for default Detcore
CPUID/RDTSC subscriptions are absent or accepted-but-inert. Those three facts
are current stock-Reverie incompatibilities, not unresolved runtime support.
This artifact does not infer compatibility from the presence of
`PTRACE_SYSCALL`.

### 5. Seccomp and user notification

- **Verified current (source-only):** `seccomp(SECCOMP_SET_MODE_STRICT)` and
  `SECCOMP_SET_MODE_FILTER` both merely set a per-task integer mode to `2` and
  return success. They do not read a BPF filter pointer and no syscall-dispatch
  path evaluates such a filter
  ([`userspace/src/handlers/sys_seccomp.rs` lines 4–31](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_seccomp.rs#L4-L31)).
- **Verified current (source-only):** `SECCOMP_GET_ACTION_AVAIL` recognizes five
  action constants, but that recognition does not implement the actions
  ([`sys_seccomp.rs` lines 33–55](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_seccomp.rs#L33-L55)).
- **Verified current (source-only):** `SECCOMP_FILTER_FLAG_NEW_LISTENER` returns
  `EINVAL`. There are no `seccomp_notif`/`USER_NOTIF` structures or listener-fd
  lifecycle in the pinned source. Current seccomp user notification is absent.
- **Verified current (source-only):** the stored seccomp mode is exposed by
  `prctl`, but its only consumers found are set/get/status reporting. Paired
  precise and inverse searches found no dispatcher enforcement.
- **Conclusion limited to current state:** seccomp/user notification cannot
  currently supply syscall interception on Narf. Treating the mode setter as a
  filter implementation would be a false positive.

### 6. BPF/framekernel extension surface

| Topic | Canonical fact | Evidence class |
|---|---|---|
| Program lifecycle | `BPF_PROG_LOAD` copies instructions and referenced objects from user memory, verifies the program, stores it in an `Arc<BpfProg>`, and returns a close-on-exec fd. Maps and load references are strong references held for program lifetime. | **Verified current (source-only)**; [`userspace/src/handlers/sys_bpf.rs` lines 501–633](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf.rs#L501-L633), [`bpf/src/prog.rs` lines 147–251](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/bpf/src/prog.rs#L147-L251). |
| Context typing | Program type XDP/raw-tracepoint/tracing maps to `Context::Atomic`; `BPF_PROG_TYPE_SYSCALL` maps to `Context::Sleepable`. Hook/program context mismatch is rejected. | **Verified current (source-only)**; [`sys_bpf.rs` lines 514–537](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf.rs#L514-L537). |
| Current attach surfaces | The Linux-shaped attach layer documents and implements only atomic fentry-shaped tracing probes and XDP. All other Linux attach types are `ENOTSUP` (out-of-enum values are `EINVAL`). `struct_ops` uses a kernel-side trait slot, not an fd link. | **Verified current (source-only)**; [`userspace/src/handlers/sys_bpf_attach.rs` lines 9–48](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf_attach.rs#L9-L48), [lines 249–276](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf_attach.rs#L249-L276). |
| Syscall-type execution | `BPF_PROG_TYPE_SYSCALL` is accepted as a sleepable program type, but the only current user-triggered execution path found is `BPF_PROG_TEST_RUN`, which copies a synthetic context and drives the program to completion. It is not attached around each guest syscall. | **Verified current (source-only)**; [`sys_bpf.rs` lines 635–710](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf.rs#L635-L710) plus the attach inventory above. |
| Execution placement | Atomic programs use a per-CPU stack and a non-send domain guard; sleepable programs allocate a future-owned heap stack and re-enter the BPF domain for each poll so no CPU-local rights snapshot spans an await. | **Verified current (source-only)**; [`bpf/src/domain.rs` lines 70–178](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/bpf/src/domain.rs#L70-L178), [`bpf/src/prog.rs` lines 1509–1540](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/bpf/src/prog.rs#L1509-L1540). |
| Memory-data movement | Program context is copied by value; map/arena accesses are verifier-mediated; the current design denies arbitrary raw kernel-address dereference and uses typed kfunc shims for mediated access. FRAME remains writable, so hardware-domain confinement does not by itself protect general domain-0 heap/state. | **Verified current (source/spec)**; [`bpf/specification/domain-confinement.md` lines 26–58](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/bpf/specification/domain-confinement.md#L26-L58), [lines 90–136](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/bpf/specification/domain-confinement.md#L90-L136). |
| Load authority | There is no unprivileged BPF mode. Source comments say the per-task credential check is the current security boundary because the capability object minted by the syscall is plumbing until per-task capability tables are real. | **Verified current (source-only)**; [`sys_bpf.rs` lines 143–186](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_bpf.rs#L143-L186). |

**Current-state boundary:** Narf already has most components of an isolated
software-extension runtime—verifier, interpreter/JIT, typed kfuncs, maps,
lifetime-bearing fds, atomic/sleepable contexts, and a BPF domain. It does not
currently expose a syscall-entry/exit BPF attach target or a trace-context ABI
containing syscall number, arguments, return, process/thread identity, and
copy-in/copy-out operations. Those are not current capabilities.

### 7. Loadable kernel modules as an adjacent extension surface

- **Verified current (source-only):** modules have an explicit lifecycle
  `Loading -> Live -> Going -> Dead`, with a C ABI `init` returning an error and
  optional `exit`
  ([`modules/src/lifecycle.rs` lines 1–60](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/modules/src/lifecycle.rs#L1-L60)).
- **Verified current (source-only):** the loader parses an ELF/manifest, checks
  architecture and W^X, resolves a named domain, relocates and seals the image,
  then calls init/exit under the module domain
  ([`modules/src/loader.rs` lines 155–239](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/modules/src/loader.rs#L155-L239),
  [lines 680–769](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/modules/src/loader.rs#L680-L769)).
- **Verified current (source-only):** the default signature verifier is
  `AcceptAll`; Ed25519 is deferred
  ([`modules/src/sign.rs` lines 1–53](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/modules/src/sign.rs#L1-L53)).
- **Security boundary:** module domains prevent cross-driver-domain access only
  where a domain backend and tagged mappings enforce it. FRAME remains reachable,
  so a kernel module is not an equivalently sandboxed alternative to a verified
  BPF program.
- **Unresolved/runtime:** module docs state no test loads a real compiled `.ko`
  in one place while other design sections describe e2e loader fixtures. No
  real-module runtime was executed in this research.

### 8. Scheduling, time, and external nondeterministic inputs

| Input/surface | Current source behavior relevant to trace comparison |
|---|---|
| Task scheduling | **Verified current (source-only):** per-CPU ready queues, wake placement, optional work stealing, preemption requests, priorities/EEVDF policy, affinity, and direct handoff exist. Work stealing is explicitly enabled/disabled at runtime. This is not a deterministic single global schedule ([`scheduler/src/lib.rs` lines 15–59](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/scheduler/src/lib.rs#L15-L59), [lines 352–371](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/scheduler/src/lib.rs#L352-L371)). |
| Monotonic time | **Verified current (source-only):** x86_64 reads TSC; aarch64 reads `CNTPCT_EL0`; boot calibration converts cycles to nanoseconds. Sleep/wakeup uses a timer wheel ([`time/src/lib.rs` lines 137–192](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/time/src/lib.rs#L137-L192), [lines 206–285](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/time/src/lib.rs#L206-L285)). |
| Realtime | **Verified current (source-only):** realtime is calibrated monotonic time plus a mutable atomic wall offset, with optional leap smear ([`time/src/wall.rs` lines 1–23](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/time/src/wall.rs#L1-L23), [lines 69–97](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/time/src/wall.rs#L69-L97)). Guest `clock_gettime` exposes realtime, monotonic/raw/coarse/boottime, and CPU-time clocks ([`sys_clock_gettime.rs` lines 4–65](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_clock_gettime.rs#L4-L65)). |
| Randomness | **Verified current (source-only):** `getrandom` uses the filesystem ChaCha20 CSPRNG and considers it always ready. Its entropy/output is an observable external input and is not tied to a deterministic seed in this interface ([`sys_getrandom.rs` lines 27–86](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/handlers/sys_getrandom.rs#L27-L86)). |
| Filesystem | **Verified current (source-only):** VFS has global and per-task mount namespaces, mutable filesystem state, caches, file offsets, and filesystem/device-backed reads. Mount-table mutation changes readiness. These are live inputs/state, not automatically snapshotted by ptrace. |
| Network | **Verified current (source-only + executed smoke):** sockets and a host network stack are present; exact-commit `net-smoke` passed. Packet arrival/order/readiness are external asynchronous inputs. |
| Signals | **Verified current (source-only):** pending signals are delivered on every completed syscall return and on blocking/yield paths; timer, process, ptrace, and external events can change delivery order ([`syscall.rs` lines 3860–3887](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/src/syscall.rs#L3860-L3887)). |
| PIDs/fds/addresses | **Verified current (source-only):** PID allocation is lowest-free/reuse within 1..32768, fd numbers come from mutable per-task tables, and ELF/heap/mmap layout produces environment-dependent numeric values. Any canonical trace must decide which values are compared literally versus normalized. |

No source was found for Hermit-style virtualized time, deterministic scheduling,
or record/replay input capture inside Narf. This is a current-absence result, not
a claim that such support cannot be added above the API.

### 9. Security and trust facts for an interposer

- **Verified current:** ptrace is a privileged cross-process read/write/control
  surface. Attach checks caller/target credentials and dumpability, and container
  builds translate the target PID in the caller's namespace. Its later requests
  rely on the relationship table, not a general capability object.
- **Verified current:** tracee data reads/writes cross address spaces by resolving
  the tracee's page-table root and using the kernel mapping. Tracer buffers use
  guarded uaccess. Any new in-kernel interposer that accepts arbitrary tracee
  pointers would therefore join memory management/uaccess in the trust boundary.
- **Verified current:** BPF's software verifier and interpreter are the first
  isolation boundary; hardware domains are defense in depth. Because FRAME
  remains accessible, verifier/JIT/kfunc/syscall-handler correctness is still
  trusted for domain-0 memory.
- **Verified current:** the current `bpf(2)` credential check is the actual
  authority gate; the type-level capability passed into the loader is explicitly
  described as plumbing. A design must not cite that bootstrap capability as a
  user/task authorization proof.
- **Unresolved:** exact production policy for who may load tracing code, attach
  it to a particular task, copy syscall pointer data, and export traces is not a
  current Narf interface. The current code supplies ingredients, not that policy.

## Executed evidence at the pinned commit

GitHub Actions run
[`36061908203`](https://github.com/hodgesds/narf/actions/runs/36061908203)
is the exact-commit run inspected. It was still completing when captured, but
individual finished-job logs and conclusions were available.

### Positive runtime observations

- **Verified current (executed, x86_64 QEMU fixture):** the `kernel-test
  (x86_64)` job log ran and passed the following ptrace cases before a later
  unrelated QEMU crash:
  `ptrace_attach_self_is_eperm`, `unknown_request_is_eio`,
  `traceme_second_call_is_eperm`, `not_tracer_is_esrch`,
  `attach_requires_same_user`, `attach_resolves_in_caller_pid_ns`,
  `attach_honours_dumpable`, `peekuser_offset_is_eio`,
  `tracer_exit_retires_active_count`, `detach_signal_width_and_eio`,
  `getregset_fault_beats_bad_regset`,
  `setoptions_rejects_unknown_bits`, `cont_bad_signal_is_eio`, and
  `kill_requires_tracer`.
- **Verified current (executed, x86_64 QEMU fixture):** `smoke_process_ptrace_e2e`
  and `smoke_process_ptrace_syscall_stop` both ran and passed. The latter covers
  attach, `TRACESYSGOOD`, syscall entry/exit phase, GETREGS/PEEKUSER orig_rax,
  GETREGSET, and disarming with CONT.
- **Verified current (executed):** exact-commit jobs passed `host unit tests`,
  `musl-demo / linux-compat`, process, signals, fs, ipc and net groups,
  `net-smoke`, `boot-smoke / aarch64`, feature checks, and both architecture
  clippy jobs.

### Negative/limiting runtime observations

- **Verified current (executed):** `kernel-test (x86_64)` ultimately failed
  because QEMU itself exited with SIGSEGV after 457 seconds, after the ptrace
  cases above passed but before a final suite summary.
- **Verified current (executed):** `kernel-test (aarch64)` and
  `boot-smoke / x86_64` were also failed jobs at capture time; `cargo fmt
  --check` failed. Consequently the commit was not globally green.
- **Unresolved:** no real external tracer (Hermit, strace, gdb) was executed
  against Narf. The successful cases are in-kernel/QEMU fixtures, stronger than
  source declarations but narrower than an independent userspace tracer.
- **Unresolved:** the x86_64 ptrace job result does not demonstrate aarch64
  syscall-stop support; source explicitly excludes that path.

## Contradiction ledger

| ID | Severity | Contradiction | Canonical Audit-A disposition |
|---|---:|---|---|
| C1 | 0 | `README.md`/`docs/PERSONAS.md` describe a `linux-compat` Cargo feature, while no current `Cargo.toml` declares it and no source `cfg` uses it. `userspace` has `default=[]` and says the Linux ABI is always present. | **Verified current:** treat the Linux syscall surface as always built in the pinned source; treat feature-composition prose as stale. Evidence: [`userspace/Cargo.toml` lines 42–88](https://github.com/hodgesds/narf/blob/afe919924a5fd2c8e820ab3a0564b6d45438e6bc/userspace/Cargo.toml#L42-L88). |
| C2 | 0 | `docs/LINUX_COMPAT.md` first calls ptrace a stub, later corrects that and claims `TRACEME/ATTACH/SEIZE`; source has no `PTRACE_SEIZE` constant or arm. | **Verified current:** ptrace is live, but SEIZE is absent. Never cite the doc's request list without the source inventory. |
| C3 | 1 | `userspace/Cargo.toml` still says “ptrace is a stub” in the `syscall-trace` feature comment. | Stale comment; live table/shim/source and executed tests prevail. |
| C4 | 1 | `arch/src/lib.rs` says aarch64 MTE methods are `unimplemented!`; `arch/src/aarch64/mte.rs` contains a concrete implementation. Other BPF comments still call MTE structural until tags land. | Concrete code prevails for save/TCF behavior. The coverage breadth and hardware execution remain **Unresolved**. |
| C5 | 1 | `modules/README.md` says domain placement is named but not enforced; `modules/src/domain.rs` and loader enter/exit now dispatch to live backends. | Concrete source prevails for conditional enforcement. Actual hardware coverage remains source-only. |
| C6 | 1 | `docs/DOMAIN_BACKENDS.md` says PCID is wired end-to-end but also states available runners skip its enforcement and it has never executed on available hardware. | **Verified source, unexecuted runtime.** Do not call PCID enforcement demonstrated. |
| C7 | 1 | Root docs imply broad Linux compatibility and “both arches” while the exact pinned commit has failing kernel-test/boot jobs and documented syscall gaps. | Use per-interface source and per-job runtime evidence; reject blanket support language. |
| C8 | 2 | Module docs alternate between “no real `.ko` load test” and e2e loader/unload fixture claims. | Treat module loader as source-implemented; real compiled-module runtime is **Unresolved**. |
| C9 | 0 | Earlier coverage left cross-process `process_vm_readv/writev` unresolved even though the opened implementation explicitly rejects a distinct address space with `EPERM`. | **Verified current (source-only):** same-AS only, fatal for stock external Reverie bulk memory. Narf's gather-before-scatter and `EFAULT` behavior is also source-verified. **Unresolved:** the exact paired Linux result at each proposed page/segment fault boundary; do not call it a proved cross-kernel divergence without pinned Linux implementation analysis or execution. |
| C10 | 0 | Earlier coverage left instruction interception unresolved even though source explicitly accepts-but-ignores `PR_TSC_SIGSEGV` and the complete `arch_prctl` handler lacks `ARCH_SET_CPUID`. | **Verified current (source-only):** default Detcore CPUID/RDTSC Tool events cannot be delivered on current Narf. |

## Immutable facts for downstream writing audit

1. Narf at the pinned commit **does implement ptrace**; “no ptrace” is false.
2. Ptrace implementation is incomplete in ways directly relevant to a tracer:
   no SEIZE/INTERRUPT, no event stops/messages, no stopped-state validation,
   and incomplete signal semantics.
3. `process_vm_readv/writev` rejects a distinct tracer address space with
   `EPERM`; this is fatal for stock external Reverie's bulk-memory path.
4. `PR_SET_TSC(PR_TSC_SIGSEGV)` succeeds without disabling RDTSC and
   `ARCH_SET_CPUID` is absent; default Detcore instruction subscriptions are
   not serviced.
5. The current syscall entry/exit stop path is x86_64-only.
6. Seccomp filter enforcement and seccomp user notification are absent despite
   mode/action-shaped compatibility calls.
7. Narf BPF accepts a sleepable syscall program type but has no syscall attach
   point; current attach links are atomic fentry and XDP.
8. No generic Rust syscall-interposition trait exists in the live dispatcher;
   ptrace's concrete before/after calls are the closest current boundary.
9. Hardware domain isolation is conditional, and FRAME/domain-0 remains
   reachable; it is defense in depth, not a complete sandbox for an interposer.
10. Exact-commit x86_64 QEMU ptrace fixtures passed, but the overall kernel-test
   job failed later and no external Hermit/strace/gdb tracer ran.
11. Scheduling, clocks, randomness, VFS/network state, signals, PIDs/fds, and
   addresses are current observable inputs requiring explicit capture or
   normalization in any equivalence experiment.

## Unresolved questions preserved for Audit A

- Which exact subset of the remaining ptrace requests/options/events can be
  replaced without changing Hermit's semantics? Cross-AS bulk memory and
  default CPUID/RDTSC interception are already known-incompatible and are not
  part of this unresolved question.
- Does Narf's `PTRACE_SINGLESTEP` produce all required traps and wait statuses on
  both architectures? Source sets the architectural bit; no independent tracer
  executed it.
- Can tracee register/memory mutation safely occur only while stopped after a
  stopped-state check is added, without racing Narf's SMP scheduler?
- For each proposed page/segment fault boundary, does pinned native Linux
  return `EFAULT` or a partial byte count, and what destination side effects
  occur? Narf's own gather/scatter behavior is source-verified; the paired
  Linux outcome is **Unresolved** pending pinned implementation analysis or an
  executed pair.
- What task/process identity and lifetime object should a first-class
  interposition API hold across fork/clone/exec/exit?
- What is the smallest safe syscall-context data ABI for pointer arguments:
  scalar metadata only, bounded copy helpers, or capability-scoped tracee
  memory handles?
- Which domain backend is available in the intended Narf POC environment, and
  has its enforcement actually executed rather than merely compiled?
- Can a Narf guest image stage the exact same guest binary, input filesystem,
  interpreter, environment, and network isolation as the Linux comparison run?
- What caused the pinned commit's x86_64 QEMU SIGSEGV and failed aarch64 kernel
  job, and could it affect longer tracing runs?
