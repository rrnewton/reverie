# Backend: ptrace (`reverie-ptrace`)

The reference backend, and the correctness oracle for every other
one. Cell **V1-ptrace** in `reverie::selection::implemented_cells`:
ptrace exec-stop activation, ptrace stops as the mechanism, tool
out of process. It implements the generic `Backend` trait
(`PtraceBackend`), with `TracerBuilder<T>`/`Tracer` as the reusable
surface underneath. Start every tool here; move it only when
measurement says to.

## Mechanism

* **Hooking:** no guest code is patched. A subscription-derived
  seccomp filter returns `SECCOMP_RET_TRACE` for the syscalls the
  tool subscribed to, and each such syscall becomes a ptrace stop
  in the tracer, where the tool's handlers run.
* **Trapping:** ptrace owns the subscribed boundary. Unsubscribed
  syscalls execute unobserved *because the subscription declared
  them so*; syscalls the framework injects on the guest's behalf
  pass through a private allowed gate in the same filter.
* **Placement:** process/thread tool state and the global singleton
  both live in the tracer process. `GlobalRPC` is a local call
  tagged with the tracee's TID — no wire, no guest-side code.

The cost is the honest one: a stop per trapped event makes this
the slowest backend per event, and the one with nothing to prove
about coverage.

## Lifecycle coverage

The admission envelope is the widest in the tree: any host process
shape, multi-threaded, exec successors re-armed in the same cell;
threads, fork, vfork, exec, and signals are all at *reference*
level (these semantics define what the other backends are checked
against). Two qualifications belong in the same breath:

* Signal fidelity is **Partial with named holes** — held-signal
  windows, documented in [The safety contract](safety.md). Until
  they are fixed here, they *are* part of the reference semantics.
* CPUID mediation requires environment prerequisite P5 and RDTSC
  requires P6 (faulting support); RDRAND/RDSEED and XGETBV
  mediation are absent in this cell.

The hook-by-hook delivery table is the lifecycle matrix in
[safety](safety.md#the-lifecycle-matrix).

## Refusals and boundaries

* `R-PTRACE` — ptrace is denied by the host policy, or the guest
  is already traced. Exclusive debugger support only: this
  backend and another tracer cannot share the guest.
* `R-ARCH` — not Linux x86-64, or the kernel floor is unmet.
* Instruction classes the cell marks absent (above) refuse at
  admission rather than passing through unmediated.

## Statistics

`run_with_stats` yields a `PtraceBackendStatsSnapshot`
(`BACKEND_NAME = "ptrace"`). Every field is supported, so a zero
means "measured, did not happen" — never "unavailable":

| Accessor | Counts |
|---|---|
| `tracees_started()` / `exited_tracees()` | tracees admitted to / observed exiting the lifecycle |
| `stop_events()` | stopped transitions entering a top-level run loop |
| `seccomp_stops()` | seccomp syscall-entry stops |
| `internal_seccomp_stops()` | of those, the ones the tracer completed itself with no tool callback |
| `signal_stops()` | signal-delivery stops |
| `exec_stops()`, `fork_stops()`, `vfork_stops()`, `clone_stops()`, `vfork_done_stops()` | the named lifecycle stops |
| `injected_trap_stops()` | validated `SIGTRAP` stops from rewritten sites (the e9patch generic mode's marker) |
| `process_seccomp_stops()` | seccomp stops per process in first-observed order (index 0 is the root; raw host PIDs are deliberately not reported) |

Its `dispatch_stats()` projection reports the seccomp and
injected-trap stop counts on the shared record; site counts are
zero (ptrace rewrites nothing), and the `PTRACE_SYSCALL` stops
taken inside injection helpers are reported as *unmeasured*, not
zero.

## Run it

The counter from [your first tool](first-tool.md) defaults to this
backend (`--backend` defaults to `ptrace` in
`reverie-examples/counter1.rs`):

```console
$ cargo run --bin counter1 -- /bin/echo hello
```

Through the trait, the entry points are `PtraceBackend::run`,
`run_with_output`, and `run_with_stats`; the judged cross-backend
corpus on this backend alone is:

```console
$ reverie-examples/e2e/run.sh --backends ptrace
```

On the scorecard in [Testing](testing.md) this backend matches
native on **19/23** corpus programs; the four ledgered mismatches
are named there — the reference has holes too, and they are
written down rather than smoothed over.
