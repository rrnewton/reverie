# reverie-kvm

`reverie-kvm` is an x86-64 research backend for driving small KVM guests. It
creates a VM and vCPU, provides bounded guest-physical memory access, turns a
guest `vmcall`/`vmmcall` into a typed Reverie syscall event, and can run
minimal static ELF executables in a bare long-mode process personality.

The guest places the syscall number and six arguments in a fixed-size frame in
guest memory. The hypercall passes the frame address to the host. `run` exposes
the original raw callback, while `run_with_tool` converts the frame to
`reverie::syscalls::Syscall` and dispatches a normal `reverie::Tool`. Its guest
adapter implements the shared `Guest` contracts for memory, registers, stack,
thread state, global RPC, syscall injection, and tail injection. Until a guest
kernel supplies Linux syscall semantics, callers provide a `SyscallExecutor`
for injected and unsubscribed syscalls.

## Static ELF execution

`install_static_elf` accepts little-endian x86-64 `ET_EXEC` images with no
`PT_INTERP`. It copies `PT_LOAD` segments, zeros BSS, creates a Linux-style
`argc`/`argv`/auxv stack, and installs an identity-mapped long-mode address
space. The vCPU starts at CPL3. `EFER.SCE`, `STAR`, `LSTAR`, and
`SFMASK` direct real `SYSCALL` instructions to a ring-0 trampoline that
serializes the Linux ABI register frame, exits KVM, then returns with
`SYSRETQ`.

`run_static_elf` supplies a deliberately small single-process Linux
personality. It handles process exit, stdout/stderr writes, deterministic
identity, time, and random queries, FS/GS bases, `brk`, anonymous `mmap`,
and common startup no-ops. Unsupported syscalls return `ENOSYS`.

## CPUID policy

Every vCPU receives an explicit CPUID table through `KVM_SET_CPUID2` before
its first `KVM_RUN`. The default `CpuidPolicy::deterministic` policy removes
`RDRAND`, `RDSEED`, TSX, AVX-512 feature bits, and the AVX-512 extended
register state. Callers that need KVM's full host-supported table can opt into
`CpuidPolicy::host_supported`.

The KVM integration test executes CPUID inside the VM and copies the resulting
registers to guest memory. This checks the vCPU-visible table rather than only
unit-testing the host-side mask.

This is a static vCPU feature policy, not a per-instruction
`Tool::handle_cpuid_event` callback. The latter still requires the planned
Linux execution bridge to preserve task-local callback context.

## gVisor model

gVisor's KVM platform keeps syscall policy above the architecture transport:
`pkg/ring0/entry_amd64.s` saves the user register frame and enters its syscall
trampoline, while `pkg/sentry/platform/kvm/bluepill_unsafe.go` classifies KVM
exits before returning control to the sentry. This prototype follows the same
separation on a smaller scale: the VM-exit layer validates and decodes the
transport once, and the runtime layer presents backend-neutral Reverie types to
the tool. The static ELF path adds only the ring-0 syscall entry needed by its
small process personality, not gVisor's complete sentry.

## Current limits

This crate is not a Linux execution backend for arbitrary ELF programs. The
static path has one vCPU, fixed-address identity mappings, and no threads,
signals, filesystem, dynamic linker, or page-permission enforcement. Its
syscall set is sufficient for minimal static programs, not general libc
workloads. The current hypercall transport also reuses standardized KVM
hypercall 12 because it is the only hypercall KVM exposes to userspace; that
prototype ABI must be replaced before running a stock guest kernel.

The host `/bin/true` on typical distributions is a dynamically linked PIE
with `PT_INTERP`, so it is rejected instead of being partially loaded. The
static ELF integration test generates an equivalent fixed-address program,
executes a real `getpid` syscall, checks the returned value in guest code, and
exits through `exit_group`. Supporting the literal host binary requires
loading its interpreter and DSOs or booting a guest kernel.
