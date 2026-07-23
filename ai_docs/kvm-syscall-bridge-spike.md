# reverie-kvm syscall-bridge spike (Phase 0)

Status: implemented spike + findings. Backend: `reverie-kvm`. Companion to the
hermit-side study `ai_docs/transient/gvisor-kvm-architecture-analysis.md` (the
gVisor → hermit/reverie mapping), which frames this as the "Phase 0
Sentry-bridge spike".

## What this spike does

The pre-existing `reverie-kvm` prototype could *observe* a guest syscall: the
guest traps out through a transport hypercall (KVM's `KVM_HC_MAP_GPA_RANGE`,
nr 12, repurposed as a userspace exit) carrying a syscall frame address, and the
host reads the number + six arguments. The handler was a closure that returned
an integer; it never *serviced* the syscall.

This spike makes the bridge real, along the gVisor Platform/Context/hook shape:

- **`SyscallHandler` trait** (`bridge.rs`) — the reverie-kvm analog of gVisor's
  per-syscall hook (`SyscallTable.External` / the `Task.doSyscall` site) and of
  Reverie's `Tool::handle_syscall_event`. A blanket impl lets a plain closure be
  a handler. `memory` is passed mutably so a handler can service pointer
  arguments in both directions (gVisor `AddressSpaceIO::CopyIn`/`CopyOut`).
- **`HostSyscallDispatch`** (`bridge.rs`) — a minimal handler that executes a
  small allowlist on the host and returns the real result to the guest:
  - stateless: `getpid`, `getuid`, `getgid`, `gettid`;
  - pointer-in (`CopyIn`): `write` to stdout/stderr — reads the guest buffer;
  - pointer-out (`CopyOut`): `clock_gettime` — writes the timespec into guest
    memory;
  - everything else: `-ENOSYS`.
- **`KvmBackend::run_with`** (`vm.rs`) — the `runApp` loop fused with
  `Context.Switch`: each hypercall exit *is* gVisor's "`Switch` returned nil ⇒ a
  syscall was intercepted"; the handler result is placed in the guest's `rax`.

Coverage lives in `reverie-kvm/tests/syscall_bridge.rs` (6 tests): `getpid`
returns the host pid; `write` forwards a guest buffer and returns the count;
`write` to an unlisted fd is `-EBADF`; `clock_gettime` writes a plausible
timespec into guest memory; an unlisted syscall is `-ENOSYS`; a closure handler
drives the same loop. All pass on a host with `/dev/kvm`; they self-skip when it
is absent.

## Findings (concrete, transport-level)

1. **The hypercall return register is 32-bit-wide through this transport.**
   Empirically, a 64-bit handler result `0x0001_2345_6789_ABCD` arrives in the
   guest's `rax` as `0x0000_0000_6789_ABCD` — the low 32 bits, zero-extended.
   Positive small results and `0` are fine; negative errnos and large values
   (pointers, offsets) are not representable. The spike therefore also marshals
   the full `i64` back through the frame's return slot
   (`SyscallRequest::RETURN_SLOT_OFFSET`, immediately after the request words —
   the same channel the arguments travel on). **A real backend must treat the
   frame slot, not the register, as the source of truth** (or leave real mode).

2. **No `Switch` / `set_return` split is expressible with this transport.**
   KVM copies `run->hypercall.ret` into `rax` on the next VM entry, so the result
   must be written while the hypercall exit is still borrowed. gVisor's
   `Context.Switch` (return nil ⇒ syscall) followed by the caller setting the
   arch-context return before the next `Switch` maps to an in-loop handler here,
   not a two-call API. The `SyscallHandler` hook preserves the *shape* (a
   generic interception point around dispatch) without the split.

3. **The `AddressSpace` CopyIn/CopyOut shape already fits.** `GuestMemory`'s
   `read`/`write` are exactly gVisor `AddressSpaceIO::CopyIn`/`CopyOut` for the
   single identity-mapped region. `write` (CopyIn) and `clock_gettime` (CopyOut)
   exercise both directions today.

## Achievable now vs. needs the Sentry bridge

Achievable on the current Platform sliver (this spike + small extensions):

- Forwarding any syscall out of the guest and servicing it in host Rust, for
  syscalls that are **stateless** or **touch only a caller-provided buffer**
  (the allowlist above; extendable to e.g. `gettimeofday`, `getrandom`,
  `uname`, `sched_getaffinity`). This is where Detcore-style determinism hooks
  (virtual clock, virtual pid) could first attach.
- Full 64-bit result fidelity via the frame return slot.
- A generic interception point (`SyscallHandler`) that a Reverie `Tool` could
  sit behind.

Needs the missing Linux-ABI substrate (gVisor's Sentry) — **not** achievable by
extending this spike:

- **ELF loading + initial stack/auxv/VDSO** (gVisor `loader/`). The KVM path has
  no loader; `hermit run --backend kvm -- /bin/echo` cannot run a real binary
  until this exists. gVisor's KVM platform does *not* special-case ELF loading —
  `loader.Load` runs in the Sentry against the shared MM/AddressSpace and is
  platform-independent, so the loader is reusable substrate, not KVM-specific.
- **Protected-mode ring-3 execution + per-app page tables** (the vCPU here is
  real-mode, identity VA=GPA). Needed before an ELF's own `syscall`
  instructions (not hand-installed `vmcall` frames) can be trapped.
- **Process/thread lifecycle, virtual memory, signals, VFS** — the bulk of the
  syscalls a real program issues (`mmap`, `openat`, `clone`, `rt_sigaction`,
  `futex`, `brk`, …) have no meaning without state behind them.

## Recommended next steps (unchanged from the gVisor study)

- Grow `KvmBackend` toward the Platform interface: `NewAddressSpace`,
  protected-mode ring-3, per-app page tables, and an async run loop.
- Decide the substrate: **reuse** a Linux personality (gVisor Sentry via a
  syscall-boundary bridge, or a guest kernel) rather than reimplementing it.
- Only then does routing the `SyscallHandler` hook through a tokio-hosted
  Detcore (like ptrace's `TracerBuilder::<Detcore>`) make `--strict`/`--verify`
  meaningful on the KVM backend.
