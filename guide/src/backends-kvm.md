# Backend: KVM (`reverie-kvm`)

The virtual-machine substrate. Cell **V11-kvm** in
`reverie::selection::implemented_cells`: VM exits replace host
interception, and the guest's Linux personality is *emulated* by
the backend rather than observed on the host. KVM is registered
for admission and explainability, but it does **not** implement
the generic `Backend` trait — hosts invoke it through its own
entry points (below). `reverie-linuxemu` is a building block
inside this backend, not a backend or a `Tool`-contract surface
of its own.

## Mechanism

* **Hooking:** no host ELF is patched and no host process is the
  guest. A **static ELF** image is installed into a KVM guest;
  its syscall trampoline reaches the host as VM exits, which
  drive the tool's handlers in the host process.
* **Trapping:** the emulated personality *is* the boundary —
  there is no host syscall stream to miss, and equally no
  general Linux guest kernel behind the syscalls the personality
  does not model. What it cannot express, it refuses.
* **Placement:** the tool runs in the host; `KvmGuest` holds real
  per-thread tool state per vCPU thread, children share one
  global state, and RPC is a direct `receive_rpc` call tagged
  with the issuing TID.

Entry points, as the examples' launcher uses them:
`KvmBackend::new_with_stdin(memory, stdin)`,
`install_static_elf_with_exec_context(...)`, then
`run_static_elf_with_tool::<T>(config, ...)`. All of it requires
`/dev/kvm`; without the device, construction fails by name
before any guest exists.

## Lifecycle coverage

Threads, fork, and vfork are **emulated** inside the VM
personality (multi-thread guests, no host process forked). Two
edges are declared-weaker, in the admission registration's own
words: leader exec is emulated but **non-leader exec refuses by
name at runtime**, and **stop-signal delivery refuses by name**,
so neither strict successor coverage nor reference signal
fidelity is claimed. Guest signals are personality-emulated;
host signal 64 and `SIGSTKFLT` are reserved by the backend (the
preemption timer owns the latter) and embedding code must not
use them. KVM is also the one backend with a real exec *commit*
point — `handle_exec_commit` can refuse before the old image is
gone (see the [lifecycle matrix](safety.md#the-lifecycle-matrix)).

In exchange, this is the only cell where CPUID and RDTSC are
**mediated** outright rather than gated on host faulting
prerequisites (RDRAND/RDSEED and XGETBV remain absent).

## Refusals and boundaries

* Non-static images are outside the personality: dynamically
  linked programs need a different backend, full stop.
* The two by-name runtime refusals above (non-leader exec, stop
  signals) are the contract working, not bugs to route around.
* Syscalls the personality does not model come back as its
  emulation defines (the scorecard below is the standing
  example); a tool that needs host-true behaviour for them
  belongs on ptrace.

## Statistics

The snapshot is `KvmBackendStats` (`BACKEND_NAME = "kvm"`), from
one collector shared by the whole guest process tree:

| Accessor | Reports |
|---|---|
| `exits()` / `count(reason)` / `total_exits()` | vCPU exits per `KvmExitReason`: `Hypercall`, `Hlt`, `Io`, `Mmio`, `Debug`, `IrqWindowOpen`, `Shutdown`, `FailEntry`, `InternalError`, `SystemEvent`, `Intr`, `Other` (unmodelled exits collapse into `Other`, so the mapping stays total) |
| `timer_window_entries()` | vCPU entries set up inside a precise timer request's stepping window |

Its `dispatch_stats()` is `None`, deliberately: every syscall
leaves the guest as a VM exit, which is none of the shared
record's signal, patched-call, or ptrace routes — an absent
measurement, not a zero.

## Run it

```console
$ reverie-examples/e2e/run.sh --backends kvm   # needs /dev/kvm; refused by name without it
```

On the [scorecard](testing.md) KVM matches **0/23** corpus
programs, and the zero is one fact, not twenty-three: the first
difference on every program is the same ledgered divergence
(`rseq=ok -> rseq=ENOSYS`), behind which a first-difference
scorecard never looks. Read that as "the personality's syscall
surface is still growing", and choose this backend today for
what only it offers — mediated CPU instructions and a
commit-point exec — not as a drop-in ptrace substitute.
