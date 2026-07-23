# reverie-kvm

`reverie-kvm` is an x86-64 research backend for driving small KVM guests. Its
current scope is intentionally narrow: it creates a VM and vCPU, provides
bounded guest-physical memory access, and turns a guest `vmcall`/`vmmcall` into
a host-side Linux syscall request.

The guest places the syscall number and six arguments in a fixed-size frame in
guest memory. The hypercall passes the frame address to the host, which lets
the backend inspect pointer arguments without limiting Linux syscalls to the
five registers left after a transport opcode.

## Syscall bridge (Phase 0 spike)

`KvmBackend::run_with` drives the guest and forwards each transported syscall to
a `SyscallHandler` — the reverie-kvm analog of gVisor's per-syscall interception
hook (`SyscallTable.External`) and of Reverie's `Tool::handle_syscall_event`.
`HostSyscallDispatch` is a minimal handler that *services* (not merely observes)
a small allowlist by running the real host syscall: `getpid`/`getuid`/`getgid`/
`gettid`, `write` to stdout/stderr (reads the guest buffer — gVisor `CopyIn`),
and `clock_gettime` (writes the result into guest memory — gVisor `CopyOut`).
Everything else returns `-ENOSYS`. The guest observes real results.

Two caveats this spike surfaced, both documented in
`ai_docs/kvm-syscall-bridge-spike.md`:

- The hypercall *return register* is delivered to the guest 32-bit-wide, so the
  full 64-bit result is also marshalled back through the frame's return slot
  (`SyscallRequest::RETURN_SLOT_OFFSET`); a real backend should treat that slot
  as the source of truth.
- The handler is invoked in the run loop rather than through a `Switch` /
  `set_return` split, because the result must be written while the hypercall
  exit is still borrowed.

This crate is not yet a Linux execution backend for arbitrary programs. KVM
does not provide Linux syscall semantics, process lifecycle, virtual memory,
signals, or filesystem behavior. Those require a guest kernel or user-space
Linux personality before this prototype can implement the full Reverie
`Guest` contract.
