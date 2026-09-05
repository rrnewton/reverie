# reverie-preload

Shared `LD_PRELOAD` + seccomp/`SIGSYS` instrumentation runtime for Reverie's
ld-preload backends (**e9patch** and **liteinst**). It exists so the ld-preload
mechanism is written and reviewed once instead of duplicated per backend (DRY).

## What it provides

| Module | Responsibility |
| --- | --- |
| `seccomp` | Trap-everything-but-the-trusted-gate classic-BPF filter (`TSYNC`), with a testable builder. |
| `trap` | Trusted syscall gate (asm), the `SIGSYS` handler, `raw_syscall6`, dispatcher registration. |
| `dispatch` | The `SyscallDispatcher` seam + shared fail-closed `PassthroughDispatcher`. |
| `fork` | Fork-following hook (the filter is inherited atomically; only per-process state resets). |
| `signal` | Signal multiplexing / reserved-signal (`SIGSYS`) policy + alt-stack. |
| `lifecycle` | `LifecycleController` guest-half seam: `InProcessSeccomp` and `HybridPtrace` install the same SIGSYS/seccomp mechanism; launcher selection is separate. |
| `user_dispatch` | Opt-in `InProcessUserDispatch`, with explicit per-thread enable/disable/re-arm and no selector pointer. |
| `rpc` (feature `coordinator-rpc`) | Synchronous coordinator client, **wire-compatible** with the async `reverie-rpc-transport` (`RpcServer<G>`). |

## Coverage boundaries

Established by the `research-ldpreload-derisking` task and enforced here: this
runtime is for **trusted, dynamically linked, non-`AT_SECURE`, no-exec** x86-64
guests. It does **not** cover vDSO fast paths, the ~40 loader/startup syscalls
before the constructor runs, static binaries, or `execve` (all fail closed).
With the default seccomp controller, `fork`/`clone` children *are* fully covered — the kernel inherits the filter
atomically, so there is no post-fork install race.

## Opt-in syscall user dispatch

Library callers may explicitly select `lifecycle::InProcessUserDispatch` in
`install`; the standalone constructor and all existing backends keep their
existing controller. This is a trap mechanism, not a complete backend or exec
implementation. The shared exec/static restrictions remain in force.

SUD is per-thread. `install` reserves the process SIGSYS handler and arms only
the calling thread. `InProcessUserDispatch::enable_current_thread()` also
re-arms a fork/clone child or a previously disabled thread, checking its handler,
trusted restorer and unblocked SIGSYS, and preparing its alternate stack when
needed. The caller must first restore its own Tool/TLS invariants and must not
allow guest work before re-arming. `disable_current_thread()` is an unsafe
runtime-owned operation, not a callback or guest escape hatch. It leaves the
handler, dispatcher and alternate stack alive; all of their code and state
must remain valid while any thread still uses them. No selector pointer is
accepted or retained: the controller uses the kernel's null-selector mode.

Only the assembler-labeled shared SYSCALL site and its return instruction are
in the trusted IP range, not all libc/runtime text. The public unsafe extern-C
`trap::trusted_sigreturn_restorer() -> !` is a **kernel SA_RESTORER entry**: it
preserves RSP, sets RAX to `SYS_rt_sigreturn`, and tail-jumps to the existing
SYSCALL label. A clock wrapper must restore the original kernel-expected RSP
and tail-jump, never CALL this entry. It allocates nothing, accesses no TLS,
and creates no signal frame. SUD uses this restorer; seccomp's handler setup
is unchanged. The same dispatcher, scalar `SyscallEvent` and deferred-resume
contract are used after mechanism-specific SIGSYS validation. Guest SUD prctl
reconfiguration is refused with EPERM; SUD is nevertheless not a security
boundary against trusted code that can deliberately call the syscall gate.

For an ordinary libc-installed signal handler, SUD can also trap the handler's
own `rt_sigreturn`. This operation is not forwarded through a Rust call frame.
After validating the SUD event, the SIGSYS handler changes only the saved RIP
to the trusted restorer (and normalizes RAX), leaving the saved restorer RSP
intact. Returning from SIGSYS first restores that interrupted state; the
trusted restorer then asks the kernel to consume the still-live outer signal
frame. No frame is copied, fabricated, retained after return, or interpreted
as a normal syscall result. This matches seccomp's existing unconditional
`rt_sigreturn` allowance without trusting additional application code. Kernel
restoration owns the signal mask, alternate-stack state and register state.
The paired libc-handler regression covers repeated returns, handlers installed
before and after interception, regular/alternate stacks, errno and restored
signal masks; it is not a general signal, xstate or exact-clock qualification.
Guest `prctl` options are classified at Linux's signed `int` width, independent
of unused upper argument-register bits.

SUD masks ordinary signals only during runtime dispatcher work. Each live
SIGSYS activation owns a stack-local dispatch scope borrowing its kernel
frame's saved guest mask. `SyscallEvent::forward` marks that activation
suspended and reinstates precisely that mask before entering the trusted gate;
this includes blocking syscalls, so guest signals can still interrupt them.
Guest handlers delivered there may make intercepted syscalls: nested scopes
have separate live frames and restore the prior suspended scope on return.
After the kernel call, forwarding remasks runtime work, records the resulting
guest mask back into the original frame, and resumes dispatcher exclusion.
The normal kernel signal return restores that mask. Runtime-internal
`raw_syscall6` calls do not open guest-delivery scopes. This does not disable
SUD, trust libc text, change seccomp, or selectively route signal syscalls
around the shared dispatcher.

Only scope pointers to still-live activations exist in thread-local storage;
none survives its corresponding signal return. Dispatcher code must not hold
locks or exclusive shared-state borrows across `forward`, where it is suspended
and can be invoked by a guest handler. Runtime entry/leave hooks still nest;
this does not qualify PMU guest/runtime boundaries, arbitrary nested signals,
nonlocal jumps out of handlers, or generic signal restart behavior.
Thread preparation also ensures the alternate stack meets the same minimum
size already provisioned by the seccomp controller (at least 64 KiB), rather
than reusing an undersized inherited Rust/libc stack. This is not a bound on
arbitrary nesting depth. The signal tests cover both trusted-gate and intercepted
delivery, nested handler syscalls with non-native dispatcher results and exact
mask checks, and interruption of a forwarded blocking read. The latter uses a
native periodic signal solely as a functional fixture, not a deterministic
timer or clock qualification.

Kernel/policy failures are errors, never success with native execution or a
skipped capability test. A failed install can leave its reserved handler and
alternate stack installed. Do not mix this controller with a trapping seccomp
controller: SUD cannot remove an inherited irreversible filter.

SUD resets across successful exec and new tasks; a constructor re-arm does not
intercept earlier loader/preinit work. This component does not migrate Tool
state, bootstrap FDs, RPC/log lifetimes or clocks, and does not change CPUID/TSC
policy. The retained three TSC-enabled startup crashes remain red; disabling
clock trapping is not an implemented solution. There is no new L1/L2 claim.

`cargo test -p reverie-preload --test trusted_restorer` checks linked machine
instructions and two real SUD signal returns through that exact gate.
`cargo test -p reverie-preload --test user_dispatch` runs bounded subprocess
probes with kernel ptrace denial. These demonstrate non-native **dispatcher**
results, not shared Detcore execution: this crate alone has no typed Guest
implementation. Its unavailable-kernel control injects ENOSYS with seccomp;
real unsupported kernels fail rather than silently skipping the suite.

## Two ways to use it

* **As a library (`rlib`):** a backend embeds the runtime, registers its own
  `SyscallDispatcher`, and calls `reverie_preload::install(...)`.
* **As a standalone `LD_PRELOAD` (`cdylib`):** set `REVERIE_PRELOAD_TOOL`
  (`passthrough` or `spoof-getpid`) and preload `libreverie_preload.so`.

```rust,ignore
use reverie_preload::dispatch::PassthroughDispatcher;
use reverie_preload::lifecycle::{InProcessSeccomp, RuntimeConfig};

// From a backend, before untrusted threads start:
unsafe {
    reverie_preload::install(
        Box::new(PassthroughDispatcher::new()),
        &InProcessSeccomp,
        &RuntimeConfig::default(),
    )?;
}
```

## Hybrid-ptrace boundary

The `LifecycleController` trait separates *policy* (`SyscallDispatcher`) from
the guest-half trap mechanism. `lifecycle::HybridPtrace` is implemented and
installs the same in-process SIGSYS handler and trusted-gate seccomp filter as
`InProcessSeccomp`; it does not create or inspect a ptrace launcher. A caller may
pair it with a unit-tool `TracerBuilder<()>`: that launcher adds no
`PTRACE_EVENT_SECCOMP` syscall action, but it still sees residual SIGSYS as a
signal-delivery stop before reinjection. Neither controller closes the
dynamic-loader window before the preload constructor or covers vDSO fast paths;
`exec` requires caller-owned rebootstrap policy. The dispatcher, seccomp filter,
trap handler, and RPC client remain shared.

## Migration note for existing backends

`reverie-liteinst` predates this crate and currently carries its own copies of
the seccomp/`SIGSYS`/gate primitives. Folding it (and `reverie-e9patch`) onto
this shared runtime is a follow-up owned by those crates' maintainers; this
crate is deliberately standalone and does **not** modify them.
