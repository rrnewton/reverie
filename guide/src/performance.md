# Performance

Interception is never free; the question is what you pay, per what,
and whether you chose to. This chapter separates mechanism costs
(physics) from tool costs (your code) and marks what is measured vs
aspiration.

## Where the time goes

Per intercepted event, roughly, from cheapest to dearest mechanism:
an in-place patched site detouring to an in-guest handler; a kernel
trap handled in-guest (`SIGSYS`, ~µs scale); a ptrace stop with the
tool in the tracer (~tens of µs). Literature anchors: ~41 ns for a
statically rewritten call, ~1.2 µs SUD, ~1.3 µs `int3`, ~31 µs ptrace.
A syscall-heavy guest under ptrace therefore slows in proportion to
its intercepted-syscall rate — the workload, not the tool, sets the
bill until your handler adds its own.

On top of mechanism cost, three tool-side costs dominate in practice:

1. **Subscription breadth.** Trapping syscalls you ignore is pure
   overhead. Implement `subscriptions` narrowly — this is the primary
   performance knob the current implementation gives you.
2. **RPC frequency.** One `send_rpc` per event (counter1 style)
   serializes on global state; fold locally and reduce rarely
   (counter2 style) unless the next event needs the global answer.
3. **Handler work.** Guest-memory reads, formatting, and allocation
   in the hot handler dwarf mechanism costs fast. The typed-syscall
   pretty printer is for humans; hot paths should record compactly
   and format later (the `recorder` example's shape).

## Measured here

The repository carries a non-CI **counter2 shootout**
(`benchmarks/counter2-shootout/`): native-normalized overhead across
the available backends on calibrated multi-second workloads. It
correctness-probes the known-green workload intersection *before*
timing anything — steal that ordering for your own benchmarks. A
same-SHA, 200-cell corpus comparison (in `BACKENDS.md`) found
e9patch ≈2.49× faster than SaBRe on the 140 mutually-clean cells by
wall time; treat single-run, concurrent-cell numbers like that as
orientation, not latency truth.

The shootout's one *published* run is in the tree
(`benchmarks/counter2-shootout/results/20260802T042358Z/`, run
2026-08-02 against commit `36ce950a`, 5 measured repetitions after
1 warmup on 5-second native targets, correctness probes first).
Slowdowns are median backend wall time over the matching native
binary variant (KVM runs a static guest normalized to native
static; the rest dynamic to native dynamic):

| Backend | CPU-heavy (28k syscalls) | Syscall-mix (450k syscalls) | Geomean |
|---|---:|---:|---:|
| LiteInst | 1.008× | 1.057× | 1.032× |
| e9patch | 1.006× | 1.067× | 1.036× |
| SaBRe | 1.015× | 1.098× | 1.056× |
| KVM | 1.101× | 2.318× | 1.597× |
| ptrace | 1.116× | 2.551× | 1.687× |
| DynamoRIO (DBT) | 5.379× | 5.488× | 5.433× |

Read the caveats as part of the table, because they are: these
are end-to-end wall times (launch and teardown included, amortized
over multi-second guests) from **one loaded host** — the run
records load averages of roughly 71/94/103 at launch — not
per-syscall latencies or idle-host estimates. The DynamoRIO row is
its branch-instrumenting client, not a null client, so its ~5.4×
is that client's instruction-instrumentation floor, nearly flat
across syscall densities. Backends also observe slightly
different startup/lifecycle call counts (the results document
says exactly how many); the probes required exact guest behaviour
and a nonzero counter, not identical event surfaces. Reproduce it
with `benchmarks/counter2-shootout/run.py` after `prepare.sh` —
noting the shootout README's own status: the core driver covers
ptrace/KVM/LiteInst today, and the external backends' preparation
now lives in their repositories with requalification still open.

> 🚧 **Under construction — the bake-off table.** The table above
> is the measured part: six backends, two syscall densities, one
> loaded host (2026-08-02). Still TODO before this box retires:
> an idle-host rerun with retained raw samples; an in-guest-trap
> column; a DynamoRIO **null** client (today's row is its
> branch-instrumenting client); lazypoline; branch-dense and JIT
> workloads; and requalification of the external backends'
> preparation in their own repositories. Until that campaign
> lands, this guide makes no general "fastest" claim for any
> backend — only the run above, with its provenance.
> <!-- UC: perf-bakeoff -->
> Tracking: [reverie#1272](https://github.com/rrnewton/reverie/issues/1272) (`perf-bakeoff`).
>
> 🚧 **Under construction — precise preemption.** Dense (every-
> branch) counting is not a selectable mechanism today: admission
> removes every cell whose mechanism is not translation, no
> translation cell is in `implemented_cells()` (V10 exists in
> `design_cells()` as *Unbuilt*), and a dense requirement is
> refused by name as `R-DENSE`. What exists today is precise
> *timer* preemption — timers counting retired conditional
> branches, delivered on ptrace (RCB schedules) and KVM (its
> preemption timer) — documented under
> [Timers](tool-api.md) and the timer recipe in
> [Advanced handlers](advanced.md); that is preemption by
> schedule, not dense instrumentation, and this guide will not
> blur the two. TODO: document the translation path when a
> translation cell is registered as implemented, with measured
> skid and cost; prototype numbers stay in design docs until
> then. <!-- UC: perf-preemption -->
> Tracking: [reverie#1272](https://github.com/rrnewton/reverie/issues/1272) (`perf-preemption`).
