# Why `CLOCK_MONOTONIC` and `rdtsc` disagree under hermit

**Question.** A prior experiment found that `CLOCK_MONOTONIC` and `rdtsc` report
different elapsed times for the same execution under `hermit run`, the gap
growing faster than the `--clock-multiplier`. Root-cause it; propose a draft fix
only if the cause is established.

**Answer.** Root cause established, confirmed from both ends, with the code path
named.

> `rdtsc` returns the calling thread's **local** virtual time; `clock_gettime`
> returns **global** virtual time. Only global time is charged
> `NANOS_PER_SCHED × multiplier` for each scheduler turn, and the *number* of
> turns itself grows with the multiplier, because `--max-timeslice` is
> denominated in virtual nanoseconds and converted to a retired-branch budget by
> dividing by the multiplier.

Linear cost per turn × turn count that grows with the multiplier = the observed
square. The code already anticipated this: the line above the `rdtsc` return
reads `// TODO: use global time for rdtsc:`.

Two candidate fixes are implemented and measured below. **They differ in
semantics, not just in mechanism, and choosing between them is an owner call** —
this report deliberately does not pick one.

---

## 1. Methodology

### 1.1 The instrument, and why it is scale-free

Every arm runs **one binary**, so nothing is confounded by differences in the
measuring program. It is freestanding — no libc, no vDSO — because this host has
no static glibc and the QEMU guest has no dynamic loader (the neighbouring
`icount-smp` experiment hit the same wall). `CLOCK_MONOTONIC` is read by **raw
`clock_gettime` syscall**, not the vDSO, so every arm goes through the path a
tracer can see.

The reported quantity is:

```
implied frequency = tsc_delta / monotonic_delta
```

On real hardware that is the TSC frequency, a constant of the machine. Under
hermit, `rdtsc` returns virtual **nanoseconds** — its handler comments "we treat
virtual cycles as equivalent to virtual nanoseconds" — so a faithful pair of
clocks gives exactly **1.000000 GHz**. Either way the test is the same: *the
number must not depend on the run, the workload, or the clock multiplier.* No
knowledge of this box's real TSC frequency is required.

### 1.2 Read order is a variable, not a detail

Two probes, identical except for the order of two lines: one reads `rdtsc`
**before** `clock_gettime`, the other **after**. This matters because a fix that
syncs the thread's clock during a scheduler round-trip will look perfect in one
order and not the other. Reporting only one order would have hidden exactly the
difference between the two candidates.

### 1.3 Guard against short captures

The first version of the driver silently captured 2 of 4 samples on one row and
reported a third of the interval as though it were the whole run — the same
shape of defect as the pre-pause-sample bug caught in the neighbouring pause
experiment. The driver now **requires the expected sample count and the `END`
marker**, and fails the row otherwise. Every number below comes from a run that
passed that check.

### 1.4 Environment

`devbig014`, AMD EPYC 9D85. hermit 0.2.0 (2026-08-21, `g3b6cd503ccd8`) for the
baseline; both candidate builds from `origin/main` `343ebd3633`. QEMU 10.1.2,
guest kernel 6.17.13, the same assets as the neighbouring QEMU experiments.

---

## 2. Checking the framing before adopting it

The task framing was that the discrepancy appears under hermit "but not in
native or qemu execution". The prior work had **not** established that — its own
limits section said the finding measured hermit's clocks against each other, not
against truth. So it was measured:

| environment | implied frequency |
|---|---|
| native, 5 runs | 1.547677, 1.547780, 1.547780, 1.547865, 1.547769 GHz |
| QEMU `-accel kvm`, 2 runs | 1.5477, 1.5477 GHz |
| QEMU `-accel tcg`, 2 runs | 1.5484, 1.5483 GHz |
| **hermit m=1** | **0.990103 GHz** |
| **hermit m=2** | **0.986848 GHz** |
| **hermit m=10** | **0.967755 GHz** |
| **hermit m=100** | **0.793719 GHz** |

Native and QEMU hold the invariant to about **one part in 10⁴**, across both the
hardware-virtualized and software-emulated backends. Under hermit the number
moves with the multiplier, where 1.000000 is correct.

**The framing is confirmed.** Native and QEMU do not diverge; hermit does.

### 2.1 The 1% row is not measurement overhead

At m=1 the gap is 1.0%. The same binary and the same arithmetic give 1 part in
10⁴ natively, so 1% is **three orders of magnitude above the method's noise**.
Section 3.2 shows it is a small, always-present instance of the same defect —
one fix covers both.

---

## 3. The cause

### 3.1 Three facts from the source

1. **The two clocks are different clocks.** `clock_gettime` returns global
   virtual time (`detcore/src/syscalls/time.rs:96` `guest_clock_time` →
   `thread_observe_time` → `global_time_lower_bound`). `rdtsc` returns
   thread-local virtual time (`detcore/src/lib.rs:1229`).
2. **Only global time is charged for scheduler turns.**
   `GlobalTime::add_scheduler_time` (`detcore-model/src/time.rs:824`) adds
   `NANOS_PER_SCHED × multiplier` = 500,000 ns × m per turn into `extra_time`,
   which belongs to global time and to no thread's local time. Called from
   `detcore/src/scheduler.rs:3298`.
3. **The turn count grows with the multiplier.** `--max-timeslice` (default
   200,000,000) is in virtual nanoseconds and converted to a retired-branch
   budget by *dividing* by the multiplier (`into_rcbs_with_multiplier`,
   `detcore-model/src/time.rs:233`). Work permitted per timeslice shrinks as
   1/m, so preemptions for a fixed workload grow as m.

Together, for a workload retiring R conditional branches with maximum timeslice T:

```
excess(m, T) = 3 · NANOS_PER_SCHED · m   +   R · NANOS_PER_RCB · NANOS_PER_SCHED · m² / T
               \_______ fixed turns _______/   \_________ preemptions that grow with m _________/
```

### 3.2 The prediction, and the test that could have refuted it

A quadratic shape alone would not distinguish this from other stories. The
falsifiable half is that the excess is **inversely proportional to the maximum
timeslice** — nothing else in this account touches that knob. Varying only
`--max-timeslice`, at m=100:

| max_timeslice | excess (s) | excess × T |
|---|---|---|
| 50,000,000 | 15.199847 | 7.600 × 10⁸ |
| 100,000,000 | 7.699847 | 7.700 × 10⁸ |
| 200,000,000 (default) | 3.899847 | 7.800 × 10⁸ |
| 400,000,000 | 1.999847 | 8.000 × 10⁸ |
| 800,000,000 | 1.049847 | 8.400 × 10⁸ |

A **sixteen-fold** change in the excess from a flag that has nothing to do with
clocks. Fitting `excess = A/T + B` gives A = 7.5467 × 10⁸ and B = 0.1065 s, and A
pins the only free parameter: `A = R · NANOS_PER_RCB · NANOS_PER_SCHED · m²/10⁹`
gives **R = 1.509 × 10⁷** retired branches. The workload is three loops of
5,000,000 iterations — **1.5 × 10⁷ conditional branches**. The measurement
recovers the loop count to 0.6%, with no fitting freedom.

### 3.3 Removing the cause removes the effect

Scaling the timeslice *with* the multiplier holds the preemption count constant,
and the super-linear term does not merely shrink — it disappears:

| m | max_timeslice | excess (s) | ratio |
|---|---|---|---|
| 1 | 2.0 × 10⁸ | 0.001498 | 1.0100 |
| 10 | 2.0 × 10⁹ | 0.014985 | 1.0100 |
| 100 | 2.0 × 10¹⁰ | 0.149847 | 1.0100 |

The excess is now exactly linear (0.0014985 × m to five digits) and the ratio is
**identical at 1.0100 across two decades**, where the default timeslice gave
1.0100 / 1.0333 / 1.2595.

And the floor is countable. At m=1, once the timeslice is large enough to prevent
preemption, the excess saturates at 0.001498 s. Divided by `NANOS_PER_SCHED` =
500,000 ns that is **2.996 — three scheduler turns**. At m=10 the saturated value
is 0.014985 s = the same three turns, each costing ten times more. **The 1% at
m=1 and the 3.5× at m=1000 are one defect**, differing only in how many times
`add_scheduler_time` fires.

### 3.4 A hypothesis that was tested and refuted

If the thread's local clock were pulled up to global time at each scheduler
round-trip, `rdtsc` would lag by less than one turn and read order would matter.
It does not: baseline, both orders, identical to six digits (0.990103 / 0.967755
/ 0.793719 at m=1/10/100). **The local clock never absorbs scheduler time at
all** — which strengthens the account and makes candidate B a real alternative
rather than a no-op.

---

## 4. Two candidate fixes

Both are implemented, built, and measured. **They are not equivalent.**

### Candidate A — `rdtsc` reads the same clock `clock_gettime` does

Branch: `cloudlint/fix-a-rdtsc-global` @ `7872ab507d`. The TODO the code already
carried. `handle_rdtsc_event` calls `guest_clock_time`, the same helper
`clock_gettime` uses, so the two agree *by construction* rather than by
arithmetic that could drift again.

### Candidate B — the thread's own clock absorbs the scheduler charge

Branch: `cloudlint/fix-b-thread-absorbs-sched` @ `64a5794dab`. `rdtsc` stays a
cheap thread-local read; `thread_observe_time` pulls the thread's local clock up
to the global lower bound, reusing the round trip the caller has already made.

### 4.1 Measured, both read orders

| build | read order | m=1 | m=10 | m=100 |
|---|---|---|---|---|
| baseline | rdtsc first | 0.990103 | 0.967755 | 0.793719 |
| baseline | rdtsc after syscall | 0.990103 | 0.967755 | 0.793719 |
| **A** | rdtsc first | **1.000000** | **1.000000** | **1.000000** |
| **A** | rdtsc after syscall | **1.000000** | **1.000000** | **1.000000** |
| B | rdtsc first | 0.990295 | 0.969052 | 0.826887 |
| B | rdtsc after syscall | **1.000000** | **1.000000** | **1.000000** |

**A is exact in both orders. B is exact only when `rdtsc` is read immediately
after a scheduler round-trip**, and still diverges — 0.83 at m=100 — when read
before one. That is the semantic difference, and it is the reason the two cannot
be treated as interchangeable.

### 4.2 Cost and determinism

| build | 20,000 `rdtsc` reads | determinism (3 runs) | `tests_time` |
|---|---|---|---|
| baseline | 0.35 s | identical sha256 | 16 passed |
| A | 0.36 s | identical sha256 | 16 passed |
| B | 0.35 s | identical sha256 | 16 passed |

The obvious objection to A is that it turns every `rdtsc` into a scheduler round
trip. **Measured, that costs nothing detectable**: 0.36 s against 0.35 s for
20,000 reads. The likely reason is that `rdtsc` already traps to the tracer, so
the round trip is small beside a cost already being paid — but that is a reading
of the numbers, not a separate measurement.

All three remain bit-reproducible. The digests differ *between* builds, as they
must: the values the guest observes have changed.

### 4.3 A side effect of B that argues against it

B inflates total virtual time. At m=100 the run reports 23.106 s where baseline
reports 18.906 s — **+4.2 s, close to the 3.9 s scheduler bill**. The mechanism,
read from the code: global time is the sum of the threads' clocks *plus*
`extra_time`, so once a thread's local clock absorbs the scheduler charge, that
charge is counted twice — once in `extra_time`, once in the thread's component.
The arithmetic is consistent with the measured inflation, but this is my reading
of the code rather than an independently measured claim, and B would need that
double count resolved before it could land.

### 4.4 The decision this leaves open

- **A** makes the clocks agree everywhere, and costs a scheduler round trip per
  `rdtsc` (unmeasurable here, but this workload is single-threaded and does not
  contend for the scheduler). Its semantic consequence: `rdtsc` becomes a
  *global* clock, so it advances when **other threads** run. For a multithreaded
  guest that is a visible change — a thread's own TSC would jump forward while it
  was descheduled, which is what real hardware does, but not what the current
  thread-local reading does.
- **B** keeps `rdtsc` thread-local and cheap, and only closes the gap at
  scheduler round-trips. It also currently double-counts scheduler time into
  global time (§4.3).

**Which is right depends on what `rdtsc` under hermit is supposed to mean** — a
per-thread work counter, or a machine-wide clock. That is a question about
intended semantics, not one the measurements answer, and it is the owner's.

---

## 5. Threats to validity

- **One transient failure, retried rather than hidden.** In the batch, A at m=100
  rdtsc-first returned 4 rows without the `END` marker (rc=1). It did not
  reproduce: three consecutive standalone runs returned rc=0 with all four rows
  and `END`. The cell was refilled by an explicit retry, flagged in the JSON with
  `retry_of_transient_short_capture`, and the exact value (1.000000) agrees with
  the other five A cells.
- **Single-threaded workload.** Every measurement here uses one guest thread.
  The multithreaded semantics discussed in §4.4 are therefore *reasoned*, not
  measured, and that is exactly where the two candidates differ most.
- **R is recovered by a fit**, albeit a one-parameter fit whose result matches the
  loop count to 0.6%.
- §4.3's double-count mechanism is a code reading consistent with a measured
  inflation, not a separately instrumented result.
- `tests_time` is 16 tests. Neither candidate has been run against the full
  validate suite.

---

## 6. Evaluation

The defect is a single missing term, and the code knew about it: `rdtsc` and
`clock_gettime` were wired to two clocks that differ by exactly the scheduler's
own time bill. What made it interesting is the amplification. A fixed 500 µs per
turn would have been a rounding error; it becomes a 3.5× discrepancy because the
timeslice is denominated in the *same* virtual nanoseconds the multiplier
inflates, so raising the multiplier both makes each turn cost more and causes
more turns. Two effects that look independent in the source multiply in the
measurement, which is why the gap grows as the square and why nobody would
predict its size by reading either line alone.

The practical consequence is narrower than "hermit's clocks are wrong" but
sharper: **any guest that cross-checks two clocks against each other sees an
inconsistency that grows with the multiplier**. A TSC calibration loop, a
benchmark harness normalising cycles against wall time, a watchdog comparing two
time sources — each reads a ratio that should be a constant of the machine and
is not. At the default multiplier the error is 1%, which is small enough to be
dismissed as noise and large enough to change a measurement.

The fix is not the hard part; both candidates are a few lines and both pass the
time tests. The hard part is that they answer different questions about what
`rdtsc` means under a deterministic scheduler, and that question has not been
decided. Candidate A is the one the existing TODO describes and is exact in every
configuration tested; candidate B preserves a thread-local reading but is
currently incomplete. **Recommending A over B would be a semantic ruling
disguised as a measurement**, so the measurements are presented and the ruling is
left where it belongs.

---

## 7. Reproducing

```sh
gcc -O2 -static -nostdlib -nostartfiles -ffreestanding -o clockprobe-free clockprobe_free.c
python3 three_environments.py --native-repeats 5 --multipliers 1,2,10,100 --out results/three-env.json
python3 qemu_baseline.py --arms kvm,tcg --repeats 2 --out results/qemu-baseline.json
python3 sweep_multiplier.py --multipliers 100 \
        --max-timeslices 50000000,100000000,200000000,400000000,800000000 \
        --out results/timeslice-m100.json
python3 sweep_multiplier.py --multipliers 1,10,100 --max-timeslices 200000000 \
        --scale-timeslice-with-multiplier --out results/scaled-timeslice.json
python3 compare_fixes.py --multipliers 1,10,100 --out results/compare-fixes.json
```

Branches: `cloudlint/fix-a-rdtsc-global` @ `7872ab507d`,
`cloudlint/fix-b-thread-absorbs-sched` @ `64a5794dab`, both off
`origin/main` `343ebd3633`, pushed and verified with `git ls-remote`.

Committed here: the harness, the probes, and the per-run JSON. Not committed: the
build trees and the hermit binaries.
