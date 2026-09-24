# Narf source-coverage ledger

Coverage target: public Narf facts required by the frozen Hermit-on-Narf POC
research contract. Revision:
[`afe919924a5fd2c8e820ab3a0564b6d45438e6bc`](https://github.com/hodgesds/narf/commit/afe919924a5fd2c8e820ab3a0564b6d45438e6bc).

This ledger answers what was searched, opened, excluded, and left unresolved.
It is separate from the canonical fact set in `narf-facts.md`.

## Repository acquisition and inventory

- Repository discovery: `gh search repos narf --limit 100` returned one
  framekernel match, `hodgesds/narf` (“Not Another Rust Framekernel”). A naive
  GitHub repository API search for `narf kernel` returned no items, which is why
  the broader name search was necessary.
- Acquisition command:
  `with-proxy git clone --no-tags https://github.com/hodgesds/narf.git ignored/narf-hermit-poc-design-20260924/narf/repo`.
- Inspected HEAD: `afe919924a5fd2c8e820ab3a0564b6d45438e6bc`, clean
  `main...origin/main` at capture.
- Complete tracked-file inventory: 2,956 paths, recorded in the ignored raw
  artifact `ignored/narf-hermit-poc-design-20260924/narf/all-files.txt`.
- All temporary clones, GitHub results, query output, and CI logs are beneath
  `ignored/narf-hermit-poc-design-20260924/narf/`; no raw evidence is tracked.

## Query-pair ledger

All initial source queries ran against the pinned clone with `git grep -n -i
-E` and were recorded verbatim in the ignored 116,516-line raw artifact
`query-results.txt`. The Audit-A correction query Q13 is recorded separately in
ignored `audit-a-correction-queries.txt`. Counts below are matching output
lines, not unique files or unique capabilities. Large counts are expected
because the userspace syscall implementation includes generated
`core.inc.rs`/`compat.inc.rs` snapshots and large test fixtures. Results were
used for discovery only; material claims were verified by opening the
implementation.

| ID / research question | Precise query | Naive control query | Hits (precise / naive) | Follow-through |
|---|---|---|---:|---|
| Q1 architecture/domain lifecycle | `framekernel\|protection domain\|domain lifecycle\|enter_domain\|exit_domain` over Rust/Markdown | `frame kernel\|domain` | 250 / 5,403 | Opened `arch/src/lib.rs`, x86 PKS/PCID, aarch64 MTE, module/BPF domain guards, security spec, backend doc. |
| Q2 Linux ABI execution | `linux-compat\|Linux ABI\|syscall numbers\|musl\|ELF` within userspace/compat/init/docs/README | `linux compat\|linux program` repository-wide | 5,696 / 20 | Opened Cargo features, syscall tables, ELF/process/interpreter/exec handlers, Linux-compat docs, exact-commit musl CI signals. |
| Q3 boot/executable prerequisites | `x86_64-unknown-none\|aarch64-unknown-none\|Limine\|Multiboot2\|PVH\|ELF loader\|PT_INTERP` | `boot\|executable` within boot/build/userspace/docs | 418 / 1,842 | Opened boot spec/objects/parsers, TESTING prerequisites, process/ELF loader. |
| Q4 syscall dispatch/data flow | `syscall_entry\|dispatch_syscall\|syscall_dispatch\|syscall number\|SyscallCtx\|SyscallArgs\|ERESTART\|copy_from_user\|copy_to_user` | `syscall\|argument` within userspace/arch/memory/docs | 3,333 / 14,583 | Opened live entry/exit dispatcher, table installation, uaccess range and copy helpers. |
| Q5 scheduling/time | `run queue\|runqueue\|wake_up\|wakeup\|schedule\|scheduler\|CLOCK_MONOTONIC\|clock_gettime\|timerfd\|virtual time` | `task\|time` | 2,970 / 21,055 | Opened scheduler top-level implementation/spec, time counter/wall/timer-wheel surfaces, guest clock handler. |
| Q6 filesystem/network/random/signals | `getrandom\|urandom\|random\|signal\|SIG[A-Z]\|socket\|filesystem\|vfs\|network namespace` | `file\|network` | 14,036 / 11,192 | Opened `sys_getrandom`, VFS mount registry/namespace portions, network/socket symbols, signal return path, CI net smoke. |
| Q7 address-space/memory access | `copy_from_user\|copy_to_user\|UserPtr\|user memory\|address space\|page table\|read_process\|write_process` | `memory access\|user pointer` | 1,609 / 163 | Opened guarded uaccess, ptrace target-AS translation, scheduler address-space lookup, and the full `process_vm_transfer` implementation. The latter is same-AS only and returns `EPERM` for a distinct address space. |
| Q8 isolation/trust | `trusted computing base\|TCB\|PKS\|MTE\|PCID\|capability\|isolation\|threat model` | `security\|trust` | 1,055 / 3,872 | Opened security-model spec, domain backend doc/source, BPF/domain threat docs, module signature policy. |
| Q9 framekernel/BPF/extensions | `framekernel\|eBPF\|BPF_PROG\|program attach\|attach point\|verifier\|struct_ops\|extension\|module` | `bpf\|extension` | 2,508 / 6,303 | Opened BPF spec/runtime/domain/prog/attach/syscall layer/tests and module lifecycle/loader/docs. |
| Q10 ptrace exact surface | `PTRACE_[A-Z0-9_]+\|ptrace\(\|sys_ptrace\|TracerPid\|TRACEME\|PEEKDATA\|GETREGS\|SEIZE\|SYSCALL` | `ptrace\|trace process` | 16,252 / 591 | Inventoried every constant and match arm; opened dispatcher links and x86 CI test log. |
| Q11 seccomp/user notification | `SECCOMP\|seccomp\|USER_NOTIF\|NEW_LISTENER\|seccomp_notif\|no_new_privs` | `secure computing\|syscall filter` | 169 / 2 | Opened full seccomp handler, state consumers, prctl references, and negative tests. |
| Q12 generic interposition | `interpos\|syscall hook\|syscall observer\|syscall filter\|before_syscall\|after_syscall\|audit syscall\|personality` | `hook\|emulation` | 39 / 3,083 | Opened live syscall entry/exit. No generic typed interposition trait found; concrete ptrace calls and cfg text logging are the current seams. |
| Q13 external-tracer bulk memory and instruction traps | `process_vm_readv\|process_vm_writev\|process_vm_transfer\|PR_SET_TSC\|PR_GET_TSC\|ARCH_SET_CPUID\|ARCH_GET_CPUID` | `bulk memory\|cross-address-space\|instruction fault\|rdtsc\|rdtscp\|cpuid` | 52 / 1,508 | Opened the complete process-vm transfer, TSC prctl arms, and arch-prctl handler. Verified same-AS-only bulk transfer, accepted-but-inert TSC faulting, and absent CPUID faulting. |

### Supplemental inverse/precision queries

- All occurrences of `PTRACE_SEIZE|PTRACE_INTERRUPT|PTRACE_LISTEN|
  PTRACE_GETSIGINFO|PTRACE_GETEVENTMSG|PTRACE_GET_SYSCALL_INFO` were inspected.
  Only comments/tests discussing Linux ordering or missing optional requests
  appeared; no constant or request arm exists.
- All `r.stopped`, `stopped.get`, and `is_task_ptrace_stopped` uses in
  `ptrace.rs` were inspected. No request arm performs stopped-state validation.
- All `seccomp_mode`, `USER_NOTIF`, `NEW_LISTENER`, and `seccomp_notif` uses
  were inspected. Mode is stored/reported; user notification is rejected; no
  dispatch evaluator exists.
- All `run_sleepable` and `Context::Sleepable` call sites were inspected. A
  userspace-loaded syscall-type BPF program runs from test-run; no syscall attach
  path invokes it.
- All `feature = "linux-compat"`, `linux-compat =`, and dependency feature
  references were inspected. They occur only in stale docs; no current Cargo
  feature/cfg exists.
- All `process_vm_transfer`, `process_vm_readv`, and `process_vm_writev` source
  hits were inspected. The sole transfer core accepts only self/same
  `AddressSpace`; a distinct target returns `EPERM`, and gather/scatter faults
  return `EFAULT` rather than a partial byte count.
- All `PR_SET_TSC`, `PR_GET_TSC`, `ARCH_SET_CPUID`, and `ARCH_GET_CPUID` Rust
  hits were inspected. The TSC setter accepts values 1/2 without storing or
  enforcing them; no CPUID arch-prctl constant or arm exists.

## Document classes searched

| Document class | Coverage | Gaps / disposition |
|---|---|---|
| Current implementation | Searched repository-wide, then opened all load-bearing files for ptrace, seccomp, syscall dispatch/uaccess, process/ELF, scheduler/time, domains, BPF, modules, boot, and security. | Files outside the frozen questions were not read end-to-end. Generated `.inc.rs` files were opened only at authoritative table/uaccess locations. |
| Versioned specifications/public interfaces | Opened root design/comparison/roadmap/testing, subsystem specs for boot, scheduler, BPF, security model, and domain backends. | Specifications were treated as claims and checked against current implementation. Several are stale; contradictions are preserved. |
| Tests | Searched all Rust test fixtures for target surfaces and opened ptrace/BPF/seccomp test regions. | Test existence is source-only. Runtime claims use exact-commit CI logs separately. |
| Executable/runtime evidence | Queried exact-commit GitHub check runs and fetched the completed x86_64 kernel-test job log directly from the Actions job API. | No local QEMU run and no external Hermit/strace/gdb run. The exact-commit workflow was not globally green. |
| Landed diffs/commit history | Inspected pinned merge commit identity and current tree. | Historical diffs/PR discussion were not needed to answer current behavior and were not used as evidence. This is a declared gap for evolution/rationale, not current capability. |
| Design documents | Opened BPF confinement/spec, module design, security model, domain backend matrix, root design/comparison. | Design-only statements were not promoted without implementation. |
| Launch/admission threads | GitHub Actions is the only launch-like evidence inspected. | No public launch/admission thread was used. |
| Planning/leadership posts | Root ROADMAP/STATUS were discovered and selectively read around relevant headings. | No external leadership posts searched; not authoritative for source behavior. |
| Retrospectives/deprecation notices | Repository docs and comments containing current gaps/corrections were searched. | No separate formal deprecation archive found. |
| External specs | Narf source comments cite Linux implementation/man-pages, but this agent's assigned scope was Narf primary sources. | Linux normative semantics are covered by another research thread; Narf comments alone are not treated as the Linux contract. |

## Opened-source heading/symbol ledger

For Markdown, the rows below contain the complete heading inventory for every
document whose body was opened, including documents later excluded as
load-bearing evidence. Heading-only inventory is explicitly identified. Files
found only by repository inventory or query output are classified as
discovered-not-opened. For Rust/TOML, “headings” means the top-level
symbols/sections actually opened.

| Source | Opened headings / symbols |
|---|---|
| `Cargo.toml` | `[workspace]`, members/excludes, `[workspace.package]`, release/dev profiles. |
| `README.md` | `NARF`; `What is a framekernel?`; `How NARF uses the framekernel model`; `Linux compatibility is a first-class feature`; `Install on a Linux distribution`; `Debian or Ubuntu`; `Fedora or another RPM-based distribution`; `Arch Linux`; `Quick start`; `Boot the async demo under QEMU`; `Boot the interactive shell — type echo hello world at the narf> prompt`; `Run the full kernel-test suite (prints a pass/fail/skip summary)`; `Run an OCI-style container end-to-end: the oci_smoke runtime reads a`; `kernel-seeded bundle at /oci, unshares namespaces, chroots into the`; `bundle rootfs, and execs the contained entrypoint, which proves it is`; `isolated (sees the container's own /etc/os-release). The nightly`; `nightly-oci CI job runs this same demo on a schedule.`; `Serve off-box: boot a guest TCP echo server reachable from the host via`; `a QEMU port-forward, then connect a real host socket and round-trip a`; `line over virtio-net (kernel TCP server path + blocking accept).`; `Boot via Limine ISO + OVMF UEFI and require a clean boot marker`; `Boot the AA64 fallback loader + ESP under AAVMF`; `Build the ISO without booting`; `Auto-detect the first USB-attached disk`; `Or specify the device + fast-wipe (zeroes MBR/GPT/ESP regions only)`; `How it differs from Linux / BSD`; `Repository layout`; `Target architectures`; `Documentation index`; `License`. Complete: 30 heading lines. |
| `DESIGN.md` | `NARF: Not Another Rust Frankenkernel`; `1. Architectural Blueprint: The Framekernel`; `2. The Continuity Scheduler (Async-First)`; `3. Security Model: Capability-Based Access`; `4. Performance Innovations`; `5. Development Roadmap`. |
| `COMPARISON.md` | `NARF compared to Linux, the BSDs, and classical microkernels`; `Design-space chart`; `Where NARF goes further than KSPP / lockdown`; `Performance posture vs Linux`; `Where NARF is behind`; `Where NARF and Linux are roughly equivalent`; `When NOT to use NARF`. |
| `ROADMAP.md` | `NARF Roadmap`; `Stage 1 — The Skeleton`; `Stage 2 — The Barrier`; `Stage 3 — The Flow`; `Stage 4 — The Compatibility`; `Stage 5 — The Silicon`; `Stage × subsystem matrix`. |
| `TESTING.md` | `Testing NARF`; `Quick reference`; `Prerequisites`; `QEMU testing`; `Async-demo boot (the simplest path)`; `Graphical display`; `host UI determined automatically:`; `Remote display via VNC`; `Server-side (the host running QEMU):`; `Client-side, on your laptop:`; `or`; `Server-side:`; `Client-side, via SSH tunnel:`; `Hardware profiles`; `Running the kernel-test suite`; `Just the boot-smoke phase`; `Running a single subsystem's tests`; `Run only USB tests:`; `Run only the wireless 4-way handshake:`; `Re-running on flake`; `Building bootable images`; `Limine ISO + boot under OVMF`; `What's in the ISO`; `Real-hardware boot`; `Burning the ISO to a USB stick`; `Auto-detect the first USB-attached disk:`; `Or pin a specific device:`; `Skip the slow full-device wipe if you know the USB has no leftover`; `bootable signatures past the ISO size:`; `Fast wipe: zero the MBR / GPT / EFI / El Torito regions only`; `(first 100 MiB + last 4 MiB), skip the middle-of-disk zero-fill.`; `Same boot-correctness as a full wipe when the USB is larger than`; `the ISO.`; `Burn a custom ISO path:`; `Partitioned disk install (ESP + ext4 root)`; `Stage kernel + initramfs + Limine first (builds only, no QEMU):`; `Then lay down the partitioned disk:`; `Tunables:`; `Tested target laptops`; `Real-hardware boot tips`; `Debugging`; `Reading dmesg`; `GDB stub`; `In one terminal, boot the kernel with a TCP serial passthrough:`; `In another:`; `Single-step boot diagnostics`; `Wedged QEMU NVMe image`; `Then retry.`; `Performance measurement`; `CI integration`. Complete: 50 heading lines. |
| `docs/LINUX_COMPAT.md` | `NARF — Linux Compatibility Reference`; `1. Overview`; `2. Syscall coverage`; `2.0 The cfg-gating sharp edge (read this first)`; `2.1 Process / scheduling`; `2.2 Memory`; `2.3 Filesystem / VFS`; `2.4 Network / sockets`; `2.5 Signals`; `2.6 Time`; `2.7 IPC / synchronization`; `2.8 cgroup v2 / namespaces / misc`; `3. ABI details & sharp edges`; `3.1 struct stat / struct statx`; `3.2 Signal delivery model, masks, RT signals`; `3.3 struct termios — the 60-byte userspace shape (canary sharp edge)`; `3.4 Syscall return ABI (SyscallReturn field order)`; `3.5 timespec / timeval`; `3.6 auxv and vDSO`; `4. Proven userland`; `5. Filesystems & pseudo-filesystems`; `/proc`; `/sys`; `/dev`; `cgroup v2`; `tmpfs / in-memory`; `On-disk filesystems`; `6. Known gaps & non-goals`; `7. How to test compatibility yourself`. |
| `docs/PERSONAS.md` | `Kernel Personalities`; `posix-core (default, no feature flag)`; `linux-compat`; `container`; `cgroup`; `Composition rules`; `Implementation status`. |
| `docs/DOMAIN_BACKENDS.md` | `Domain Backends — Hardware Enforcement Matrix`; `What this means for security claims`; `What the two backends do and do not cover`; `The access graph this rests on`; `What is verified and what is not`; `How the kernel picks at boot`; `Why three backends instead of one`. |
| `security-model/specification/spec.md` | `security-model — Specification`; `1. Purpose & scope`; `2. Assumptions`; `3. Public interface`; `4. Invariants & safety properties`; `4.2 Documented isolation exceptions (enumerated)`; `4.1 DomainId assignment table (authoritative)`; `5. Architecture notes`; `x86_64`; `aarch64`; `6. Dependencies`; `7. Stage assignment`; `8. Threat model (formal)`; `8.1 Attacker capabilities`; `8.2 Attacker goals (defended against)`; `8.3 Attacker goals (out of scope)`; `8.4 The fence`; `9. Trust chain (root to driver)`; `10. Resolved decisions`; `10.1 Speculative side channels at domain boundaries`; `10.2 Bootloader TCB membership`; `10.3 Firmware re-measurement after boot`; `11. Open questions`. |
| `boot/specification/spec.md` | `boot — Specification`; `1. Purpose & scope`; `2. Assumptions`; `3. Public interface`; `4. Invariants & safety properties`; `5. Architecture notes`; `x86_64`; `aarch64`; `6. Dependencies`; `7. Stage assignment`; `8. Resolved decisions`; `8.1 Bootloader portfolio (resolved)`; `8.2 Measured boot / TPM (resolved)`; `8.3 Framebuffer handoff (resolved)`; `9. RawBootInfo wire format`; `10. Open questions`. |
| `bpf/specification/spec.md` | `bpf/ — specification`; `1. Purpose & scope`; `2. Assumptions`; `3. Public interface`; `3.1 Crates`; `3.2 The kfunc contract`; `LED command kfunc`; `3.3 Memory-subsystem interface (Stream B)`; `3.4 Maps`; `3.5 Object pinning`; `3.6 Runtime statistics`; `3.7 Program instruction introspection`; `3.8 Program tags`; `3.9 Program license metadata`; `3.10 Program provenance metadata`; `3.11 Program verifier log`; `3.12 Named raw tracepoints`; `3.13 Program recursion misses`; `3.14 Rust-native typed tracing reads`; `3.15 Dynamically bounded XDP packet reads`; `3.16 Native arena atomics`; `4. Invariants`; `5. Architecture notes`; `x86_64`; `aarch64`; `6. Dependencies`; `7. Stage assignment`; `8. Open questions`; `9. Post-review corrections`. |
| `bpf/specification/domain-confinement.md` | `bpf/ — hardware-domain confinement (design note)`; `1. Purpose`; `2. Background: two facts that make this cheap`; `2.1 BPF barely uses address-space visibility`; `2.2 The domain fence is already live (on Intel)`; `3. Threat model`; `4. Design`; `4.1 DOMAIN_BPF and the FRAME-stays-writable model`; `4.2 Atomic and sleepable enter/exit seams`; `4.3 Cost`; `5. The maps tension (the one real wrinkle)`; `6. Answering the visibility question directly`; `7. Program tiers, tracing reads, and BTF`; `7.1 Rejected: two program-trust tiers`; `7.2 The tier is a kfunc capability, not a domain`; `7.3 Reads are mediated, and only off trusted pointers`; `7.4 On BTF: not needed; typed access is a Rust-native extension`; `8. Scope / non-goals`; `9. First milestone`; `10. Open questions`; `11. As built`. |
| `modules/README.md` | `narf-modules` (the document's only heading). |
| `modules/DESIGN.md` | `NARF loadable kernel modules — design`; `What works, and what does not`; `Where NARF deviates from Linux`; `1. Cap-typed exports`; `2. Domain placement`; `3. Versioned ABI`; `Where module memory lives`; `aarch64 needs veneers`; `Pipeline`; `Trust model`; `W^X enforcement`; `Per-export CRC versioning`; `§6 Per-module symbol ownership and unregister-on-unload`; `Design choice: Design A — owner-id field on each KSYMTAB entry`; `API`; `Init-attribution context`; `Unload sweep`; `Verified by e2e smoke 4`; `Deferred items`; `Open questions / future phases`; `File layout`. |
| `modules/MODULE_AUTHORING.md` | `Writing a NARF kernel module`; `Project layout`; `None. A module links against the kernel — the loader resolves its`; `undefined symbols through KSYMTAB — so it declares what it calls in an`; `extern C block and depends on nothing. In particular do NOT depend`; `on narf-modules: that is the loader, and pulling it in would compile`; `a second copy of the memory, filesystem, console and time crates into`; `your module, all already resident in the running kernel.`; `The .modinfo section`; `modinfo.txt`; `What you can call`; `Lifecycle symbols`; `Declaring capability requirements`; `Module parameters`; `kparams.txt`; `Loading the module`; `Once narf-libc ships the init_module wrapper:`; `A complete hello world module`; `modinfo.txt`; `Reference counting`; `Reading the manifest at runtime`; `Lifecycle errors`; `Differences vs Linux .ko`; `Diagnostics`. Complete: 24 heading lines. |
| `arch/src/lib.rs` | `DomainBackend`; runtime effective backend; `DomainPrimitive`; current-domain state/scope. |
| `arch/src/aarch64/mte.rs` | `SavedMteState`; `DomainRights`; `Mte: DomainPrimitive`; support/tagging/TCF helpers. |
| `arch/src/x86_64/pks.rs` | PKS rights/state; active probe; enter/exit domain. |
| `memory/src/per_domain_root.rs`, `asid_alloc.rs`, `x86_64/domain.rs` | domain root register/lookup/unregister; tag allocation; private/confined slots. Read through focused symbol searches. |
| `boot/src/info.rs` | `MemRegion`; `RawBootInfo`; `BootInfo`; validation. |
| `boot/src/x86_64/mod.rs`, `multiboot2.rs`, `pvh.rs` | protocol detection; memory/initramfs/RSDP/framebuffer/cmdline parsing. |
| `boot/src/aarch64/mod.rs` | FDT handoff; initramfs/cmdline; reserved-memory subtraction. |
| `userspace/Cargo.toml` | dependencies; actual feature list (`syscall-trace`, `container`, `cgroup`, tests). |
| `userspace/src/elf.rs` | ELF header/program-header parsing; PT_LOAD/PT_INTERP/PT_DYNAMIC/PT_TLS. |
| `userspace/src/process.rs` | `UserProcess`; load errors; load process/resolve interpreter/relocate/stack/auxv/TLS. |
| `userspace/src/interp.rs` | register/lookup/reset interpreter. |
| `userspace/src/handlers/sys_execve.rs`, `sys_execveat.rs` | Linux argument decoding; resolved exec; fd/memfd execution. |
| `userspace/src/syscall.rs` | table/handler types; shared entry; live plain entry/exit; trace logging; ptrace stops; signal delivery. |
| `userspace/src/handlers/compat.inc.rs` | uaccess validation/copies; syscall installation rows for ptrace/seccomp/process_vm. |
| `userspace/src/handlers/sys_ptrace.rs` | forwarding shim and stale-comment correction. |
| `userspace/src/ptrace.rs` | full file: constants, registry/lifecycle, stop/signal hooks, register translation, every request arm. |
| `userspace/src/abi_proc2_tests.rs`, `process_e2e_tests.rs` | ptrace negative/permission/namespace/options/signals/memory and live syscall-stop fixtures. |
| `userspace/src/handlers/sys_seccomp.rs` | full handler. |
| `userspace/src/handlers/sys_prctl.rs`, `handlers/core.inc.rs` | seccomp/no-new-privs state set/get and consumers, through focused regions/searches. |
| `userspace/src/handlers/sys_bpf.rs` | authorization, program-type mapping/load, test-run, map/program lifetime portions. |
| `userspace/src/handlers/sys_bpf_attach.rs` | full attach-target model and target resolution. |
| `userspace/src/abi_bpf_tests.rs` | syscall program type and attach-context mismatch cases. |
| `bpf/src/prog.rs` | `BpfProg` object/lifetime; load request; run atomic/sleepable portions. |
| `bpf/src/domain.rs` | full atomic/sleepable domain guard and drop. |
| `bpf/src/attach.rs`, `link.rs` | native attach/link lifecycle through focused symbol/implementation regions. |
| `modules/src/lifecycle.rs` | full state model. |
| `modules/src/domain.rs` | register/resolve domain; entry/exit guard and limitations. |
| `modules/src/loader.rs` | load pipeline; W^X/relocation; init/exit under domain; release. |
| `modules/src/syscalls.rs` | init/finit/delete lifecycle and reference ordering. |
| `modules/src/sign.rs` | complete verifier hook/default policy. |
| `scheduler/src/lib.rs` | queue/wake/work-steal/preemption/address-space/current-task surfaces. |
| `scheduler/specification/spec.md` | `scheduler — Specification`; `1. Purpose & scope`; `2. Assumptions`; `3. Public interface`; `3.1 Core executor API`; `3.1.1 Pluggable scheduling policy`; `3.2 CPU topology`; `3.3 Affinity and placement`; `3.4 Resource accounting (CPU share)`; `3.5 CPU hot-plug`; `4. Invariants & safety properties`; `5. Architecture notes`; `x86_64`; `aarch64`; `6. Dependencies`; `7. Stage assignment`; `8. Resolved decisions`; `8.1 Wake-overhead cap (resolved)`; `8.2 Priority taxonomy (resolved)`; `8.3 Driver-domain vs user-task fairness (resolved)`; `8.4 E-core / P-core (resolved)`; `8.5 Gang scheduling (resolved)`; `8.6 Realtime class shape (resolved)`; `8.7 CPU hot-plug PKS coherence (resolved)`; `9. ABI versioning`; `10. Open implementation gates`. |
| `memory/research/sfi.md` | `Research note — Software Fault Isolation as a domain-isolation backend`; `What it is`; `Why it is a candidate for NARF`; `Why we are not building it now`; `What would change to revisit`. Opened only to classify SFI as research/deferred. |
| `memory/research/snp_vmpl.md` | `Research note — AMD SEV-SNP VMPL as a domain-isolation backend`; `What it is`; `Why it is a candidate for NARF`; `Why we are not building it now`; `What would change to revisit`. Opened only to classify VMPL as research/deferred. |
| `time/src/lib.rs`, `wall.rs`, `timer_wheel.rs` | counters/calibration; monotonic/wall; sleep/timer registration/fire. |
| `userspace/src/handlers/sys_clock_gettime.rs`, `sys_gettimeofday.rs`, `sys_getrandom.rs` | full handlers. |
| `filesystem/src/lib.rs` | VFS/mount/namespace registry and readiness-related regions through focused symbol searches. |
| `net/src/iface.rs`, `tcp_stack.rs`; `userspace/src/socket.rs` | interface/RX/socket lifecycle through focused symbol searches. |
| Exact-commit GitHub Actions run/job log | check/job status matrix; x86 kernel-test ptrace pass lines; later QEMU SIGSEGV. |

### Heading-only inventory for opened-but-excluded narratives

The following bodies were not used as load-bearing evidence. Their headings
were nevertheless opened during exclusion triage, so the complete inventories
are recorded rather than implying they were merely discovered.

| Source | Complete heading inventory |
|---|---|
| `STATUS.md` | `NARF — Current Status`; `Stage progression`; `Working today (both arches on QEMU)`; `Crates that exist`; `Stage 1 exit-gate status (closed)`; `Stage 2 Barrier — what exists`; `Stage 2 Barrier — status`; `Stage 3 Flow — what exists`; `Stage 3 exit-gate integration`; `Stage 2 design debt (not blocking, worth tracking)`; `Deviations from the v0.2 design (in-code comments flag each)`; `How to run`; `Default flavour: async executor demo, 5 ticks, clean exit.`; `Kernel-test harness: 14 tests, isa-debug-exit signals pass/fail.`; `IDT self-test: deliberately trigger #UD; print trap frame; exit 42.`; `Cross-compile for aarch64 (qemu-system-aarch64 not installed locally).`; `Host-side unit tests on narf-lib.`; `Commit log (high-level)`; `Pickup hint for the next session`; `Stage 4 structural landing`; `Stage 4 structural surfaces landed (both arches green)`; `Post-Stage-4 session — primitives + driver hardening`; `New core primitives (in narf-time / narf-scheduler / narf-interrupts)`; `Driver-wide sweep`; `USB stack — spec-compliant baseline`; `Input-pipeline observability`; `Boot path`; `Stage 4 blockers — updated status`; `Memory-management surface — what's wired`; `Networking surface (post-Stage-4)`; `Observability (post-Stage-4)`; `Bug-fix landings worth noting`; `Desktop-Linux graphics track (June 2026)`; `Booting a real Linux distro (Alpine) — June 2026`. Complete: 34 heading lines. |
| `epoll_hang.md` | `Intermittent epoll/signalfd wake diagnosis`; `Current conclusion`; `Fedora KDE branch delta considered`; `QDBus trace: wake delivery works; partial munmap is the startup blocker`; `Post-KWinWrapper D-Bus trace: all environment replies arrive`; `Max-reasoning DRM/DAC review and the implemented narrow fix`; `Max-review result: the #GP is glibc abort fallback, not corruption`; `Max-reasoning review results (semcode across NARF + systemd)`; `Wake fixes implemented after the review`; `Observed boot frontier: graphical target reached; Plasma processes not yet proved`; `August 2 live GDB localization: epoll fixed, later cgroup lock recursion`; `Live startup profile: synchronous ext2/virtio reads dominate`; `Timer-wheel cache: directly relevant to the epoll backstop`; `Ownership/metadata changes: indirect load and boot-progress effects`; `Fedora image service change: narrows the observed handshake`; `What the signal-return hypothesis does and does not explain`; `Leading permanent-hang candidates`; `1. Publisher/signalfd task mismatch`; `2. Exit publication never occurred`; `3. Wake occurred but no readiness re-scan ran`; `Confirmed defects adjacent to, but not sufficient for, PID 1's watch`; `Child exit bypasses the signalfd edge-generation invariant`; `Stop/continue SIGCHLD publication has no signalfd wake`; `Minimal non-perturbing trace to resolve the occurrence`; `Regression coverage needed before a fix is considered closed`; `2026-08-02 timer-wheel cache repair and Fedora replay`; `Correction from scoped syscall trace and semcode callchain review`; `Confirmed SysV startup-stack layout defect`; `8-GiB KVM acceptance replay after the stack fix`; `Final syscall/epoll callchain audit and epoll_pwait wiring repair`; `Max-review conclusion: current Plasma gate is the kcminit ready pipe`; `kcminit ready-pipe callchain review`; `Refined kcminit/X11 timeline and XKB corpus check`; `Scoped kcminit trace: parent wake is not the reproduced failure`; `KWin trace: the apparent Wayland wait ends in DRM-node permission failure`; `Max-reasoning finding: maxevents consumed events that were never returned`; `Live DRM policy result and persistent-node coverage`; `Current startplasma trace: leader waits after its worker renames`; `Dual D-Bus trace: socket-activation listener lost O_NONBLOCK`; `Post-SCM_RIGHTS replay: KWin starts, Plasma shell still gated by kcminit`; `kcminit/session-bus dual trace: AUTH and wake succeed`; `Max metadata review: ext2 and chmod/chown fixes validated`; `2026-08-03 Wayland-only kcminit replay: kded starts, ksmserver does not`; `2026-08-03 Plasma-session debug replay: kded broadcast is on the wire`; `2026-08-03 QDBUS_DEBUG replay: no usable Qt records and an earlier plateau`; `2026-08-03 independent GLib name-wait test: kded delivery succeeds`; `2026-08-03 QDBus match-lifecycle replay: persistent kded rule is installed`; `2026-08-03 isolated Qt watcher test: QDBusServiceWatcher succeeds`; `2026-08-03 classic-session supervisor replay: ksmserver exits early`; `2026-08-03 ksmserver status replay: early exit is SIGABRT`; `2026-08-03 first direct-shell replay: supervisor blocks reaping waiter`; `2026-08-03 final Wayland replay: PLASMA-READY`. Complete: 52 heading lines. |

## Seen and excluded ledger

| Source seen | Why not used as load-bearing evidence |
|---|---|
| `STATUS.md` (100 KiB) | Heading inventory was opened and is complete above; body passages around relevant status claims were triaged. The chronological landing narrative is secondary to implementation and exact CI, so no broad “working today” statement was promoted. |
| `epoll_hang.md` (170 KiB) | Heading inventory was opened and is complete above. The incident/debug body was not read as primary evidence because it is unrelated to the requested interposition architecture except as general scheduler risk. |
| `*/research/summaries/*` | Discovered by tracked-file inventory/query output, not opened. These are distilled third-party sources and are not authoritative for current Narf behavior. |
| `memory/research/sfi.md`, `memory/research/snp_vmpl.md` | Both were opened; their complete headings are above. They are research-only future backends, used only to classify SFI/VMPL as not current implementations. |
| Generated `userspace/src/handlers/core.inc.rs` / `compat.inc.rs` wholesale | Authoritative for table/uaccess implementation but enormous and duplicative. Only exact live rows/functions were opened and cited. |
| Test names without execution | Source coverage only; never runtime proof. Exact CI log was required for executed claims. |
| Root README feature/support tables | Useful inventory, but contradicted by Cargo/source in several places. Canonical facts use current implementation. |
| GitHub repository descriptions/search results | Used solely to identify the repository, never to infer features. |
| Current commit's merged PR description/history | Not needed to establish current code. No rationale/evolution claim relies on it. |
| Unrelated driver/firmware/audio/graphics specifications | Present in the 2,956-file inventory but outside the POC decision surface. |

## Inverse checks

| Candidate conclusion | What should exist if the opposite were true | Search/result |
|---|---|---|
| Ptrace is live | If absent: syscall table would omit it or shim would return ENOSYS; no request state machine/runtime cases. | Opposite refuted: live install/forwarder, 14 request groups, and exact-commit x86 QEMU passes. |
| `PTRACE_SEIZE` is absent | If present: numeric constant, match arm, relationship semantics, interrupt/listen tests. | None exists. A doc sentence claiming SEIZE is contradicted. |
| Ptrace event stops are absent | If present: `PTRACE_EVENT_*`, option-triggered clone/exec/exit hooks, `GETEVENTMSG`. | Only comments say options are stored/ignored; no event constants/hooks/request arm. |
| Stopped-state enforcement is absent | If present: request preamble would consult `stopped`/task state before memory/register operations. | Exhaustive `stopped` use search found no such check. |
| Seccomp filter enforcement is absent | If present: filter copy/validation/storage, dispatcher evaluation, action result path. | Mode integer only; no filter evaluator. |
| Seccomp user notification is absent | If present: NEW_LISTENER fd, notif request/response structs, ioctl/lifecycle, blocked-task wake. | NEW_LISTENER rejected; names/objects absent. |
| Generic syscall-interposition trait is absent | If present: public trait/callback install, before/after context, lifecycle/registration, dispatch invocation. | Precise/naive searches and dispatcher read found only concrete ptrace calls and cfg logging. |
| BPF syscall attach is absent | If present: attach type/target, context data contract, dispatcher invocation, link detach lifecycle. | Syscall type loads and test-runs only; attach resolver has fentry/XDP only. |
| Cross-address-space process-vm access is absent | If present: a target address-space mapping/copy primitive and permission path would operate on a distinct `AddressSpace`, with partial-transfer behavior on faults. | The sole transfer core permits only pointer-identical current/same AS and explicitly returns `EPERM` for a distinct AS; faults return `EFAULT`. |
| RDTSC faulting is absent | If present: `PR_SET_TSC(PR_TSC_SIGSEGV)` would store task state and arrange CR4.TSD or an equivalent user-instruction trap. | Handler accepts the value but explicitly says RDTSC is never disabled; `PR_GET_TSC` always reports enabled. |
| CPUID faulting is absent | If present: `ARCH_SET_CPUID`/`ARCH_GET_CPUID` constants and arch-prctl arms would set/query per-task faulting state. | Complete handler defines FS/GS subcodes only; exhaustive Rust search found no CPUID subcode. |
| Linux ABI is not currently feature-gated | If feature-gated: Cargo feature plus `cfg(feature="linux-compat")` call sites. | Neither exists; docs are stale. |
| PCID/MTE broad enforcement is demonstrated | If demonstrated: a runner executes enforcement cases on appropriate hardware and reports pass. | Backend doc says PCID enforcement skips on all available runners; no hardware proof gathered. |
| External tracer compatibility is demonstrated | If true: exact commands/logs for Hermit/strace/gdb against Narf userspace. | None found or executed. In-kernel fixtures are preserved as narrower evidence. |

## Runtime/source reconciliation

- GitHub Actions check-run data came from the exact commit via the GitHub API.
- Completed x86 kernel-test job log was fetched directly from
  `repos/hodgesds/narf/actions/jobs/107842987657/logs`.
- Ptrace cases and two e2e fixtures visibly emitted `[ OK ]`; these are executed
  evidence.
- The same job later ended when `qemu-system-x86_64` exited with SIGSEGV after
  457 seconds. There was no final all-suite summary. The job conclusion is
  failure and remains so in the fact set.
- No local Narf runtime test was needed after exact-commit execution evidence was
  available. No Hermit command was run.

## Coverage gaps carried forward

1. Independent external-tracer execution (Hermit/strace/gdb) is missing.
2. aarch64 syscall-entry/exit ptrace stopping is source-absent and unexecuted.
3. Real PCID hardware enforcement is unexecuted according to the repository's
   own backend document.
4. MTE allocation-tag coverage is internally inconsistent across source
   comments/docs and was not run on MTE hardware.
5. Historical PR/diff rationale and external governance were not researched;
   neither is needed for current implementation facts.
6. Linux normative ptrace/seccomp semantics are not established by this file;
   they require the parallel Linux/Reverie evidence thread.
7. Exact cause and relevance of the pinned commit's failed QEMU/aarch64 jobs are
   unresolved.
8. Cross-address-space `process_vm_*`, RDTSC faulting, and CPUID faulting are
   **not** unresolved: source proves those stock external-Reverie requirements
   are absent or inert at this pin. No runtime test was used to upgrade or
   soften those source-only gaps.
