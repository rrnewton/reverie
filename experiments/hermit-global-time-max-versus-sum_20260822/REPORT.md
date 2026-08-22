# Should hermit's global virtual time be a maximum over threads rather than a sum?

**Analysis only — no implementation.** The owner asked for this to be thought
through carefully before anything is built.

## Summary of findings

1. **The aggregate is a scheduling input, not a reporting value.** The
   scheduler's `committed_time` *is* the aggregate, and it is compared directly
   against sleep deadlines and drives timed-event dispatch. Changing SUM to MAX
   is a determinism change wearing a clock change's clothes — the hazard the
   owner pointed at is real and confirmed.
2. **MAX does not fix the defect that was measured.** That defect was
   single-threaded, and over one thread a sum *is* a maximum. The measured gap
   is entirely scheduler time, which MAX leaves untouched.
3. **MAX fixes a different defect — one that candidate A fixes in the wrong
   direction.** Newly measured: with 8 threads, `CLOCK_MONOTONIC` runs **8.4×**
   faster than any single thread's `rdtsc`. Native hardware is flat in thread
   count. Under candidate A the two agree, but by making `rdtsc` scale with
   thread count too — further from native, not closer.
4. **So MAX and candidate A are complementary, not alternatives.**
5. **SUM has a measured guest-visible cost that MAX would bound:** a 50 ms
   `nanosleep` takes **1.404 s** of guest-visible time with 8 threads. The
   mechanism is confirmed to three decimal places.
6. **Existing recordings would not survive**, for programs that use timed waits.
   Not because timestamps change — recorded times are thread-local and MAX does
   not touch them — but because wake ordering changes.

---

## 1. Where global time is read

`GlobalTime::as_nanos()` returns `self.total`, maintained as
`starting_nanos + Σ(per-thread times) + extra_time`
(`detcore-model/src/time.rs:812-819`). Every non-test read of that aggregate:

### Scheduling consumers — MAX changes behaviour

| # | Site | What it does |
|---|---|---|
| S1 | `scheduler.rs:3312` → `:3334` | `snapshot = gtime.as_nanos()` becomes **`committed_time`**, the scheduler's "now" |
| S2 | `scheduler.rs:2959` | `SleepUntil(target) <= committed_time` — releases a sleeping thread |
| S3 | `scheduler.rs:2072` | `timed_waiters.pop_if_before(committed_time)` — dispatches timed events |
| S4 | `scheduler.rs:3047` | child-exit event deadline = `committed_time + 1ns` |
| S5 | `scheduler.rs:2746-2755` | deadlock avoidance: read aggregate, `add_extra_time(next_deadline − now)` |
| S6 | `tool_global.rs:942` | `RegisterAlarm` base instant |
| S7 | `syscalls/time.rs:119-122` | `sleep_request`: deadline = `thread_observe_time() + delta`, **denominated in aggregate units** |

S7 with S2 is the coupling that matters: deadlines are *expressed* in aggregate
units and *compared* against the aggregate. Change what the aggregate counts and
you change how long a sleep lasts in program terms.

### Reporting consumers — MAX changes only the value reported

| # | Site | What it reports |
|---|---|---|
| R1 | `tool_global.rs:929` (`GlobalTimeLowerBound`) | `clock_gettime`, `time`, `gettimeofday` |
| R2 | `tool_global.rs:964` | `AlarmRemaining` |
| R3 | `tool_global.rs:1624` | file mtime on touch |
| R4 | `tool_global.rs:578-589` | final summary `virttime_final` / `virttime_elapsed` |
| R5 | `syscalls/sysinfo.rs:331` | `sysinfo` uptime |

### Not the aggregate — unaffected by MAX

`threads_time` / `threads_duration` (per-thread), `SchedEvent.end_time`
(thread-local, via `with_dettime` → `without_starting`), the preemption timeslice
(`thread_logical_time + timeslice`, `tool_global.rs:2586`), and `rdtsc` itself.

**Verdict on the first deliverable: MAX touches scheduling.** Four of the seven
scheduling consumers are load-bearing for interleaving. This is not the small
decision it might have been.

---

## 2. MAX does not fix the measured defect

The root-cause work measured a single-threaded program. With one thread the
`time_vector` has one entry, so

```
SUM = starting + t₀ + extra_time
MAX = starting + max(t₀) + extra_time = starting + t₀ + extra_time
```

**identical.** The entire measured gap is `extra_time` — scheduler turns at
`NANOS_PER_SCHED × multiplier` each, charged to the aggregate and to no thread.
It was localised precisely: at multiplier 1 the excess saturates at 0.001498 s =
2.996 × 500 µs, exactly three scheduler turns.

MAX leaves `extra_time` alone. **It would not have changed a single number in the
previous report.**

---

## 3. What MAX would fix, measured

Every prior measurement was single-threaded, which is exactly where the three
options cannot differ. Varying thread count, identical per-thread work
(`threadclock.c`, 3 M iterations per thread):

| threads | `CLOCK_MONOTONIC` | one thread's `rdtsc` | ratio |
|---|---|---|---|
| 1 | 0.030043 | 0.030033 | 1.00 |
| 2 | 0.063765 | 0.030313 | 2.10 |
| 4 | 0.131008 | 0.030858 | 4.25 |
| 8 | 0.268792 | 0.031979 | **8.41** |

Global time grows with thread count; a thread's own clock stays flat, as it
should — its work did not change. **Native hardware for comparison:** monotonic
0.001190 / 0.001247 / 0.001554 / 0.001412 for 1/2/4/8 threads — flat. Real
`CLOCK_MONOTONIC` does not speed up when you add threads. A MAX would be flat
too; the SUM is the outlier.

### The same table under each candidate fix

| threads | baseline mono : rdtsc | **A** mono : rdtsc | **B** mono : rdtsc |
|---|---|---|---|
| 1 | 0.030043 : 0.030033 | 0.030043 : 0.030033 | 0.031543 : 0.030033 |
| 8 | 0.268792 : 0.031979 | 0.268792 : **0.268782** | 0.280792 : 0.031979 |

- **A** makes them agree exactly — by making `rdtsc` scale with thread count.
  A thread's own TSC now advances 8.4× faster than its own work when seven
  siblings run. That is agreement achieved by moving `rdtsc` *away* from native
  semantics.
- **B** does not fix the multithreaded divergence at all (8.78× at 8 threads,
  marginally worse than baseline, consistent with the double-count flagged
  earlier). B only ever addressed the scheduler-time term.
- **MAX** would flatten the monotonic column to ≈ 0.030 and match both `rdtsc`
  and native.

**This is the finding that matters: the defect has two independent components.**
Scheduler time charged only to the aggregate (visible single-threaded, fixed by
A or B), and summing over threads (visible only multithreaded, fixed only by
MAX). A fixes the second by inflating `rdtsc` rather than deflating global time.
**A and MAX are complementary.**

---

## 4. The measured cost of SUM in the scheduler

Since the aggregate is `committed_time`, and sleep deadlines are in aggregate
units, a sleep's real duration depends on how many siblings are running.
`sleepscale.c`: one thread sleeps 50 ms while N−1 spin.

| threads | guest-visible time across a 50 ms sleep |
|---|---|
| 1 | 0.050015 s |
| 2 | 0.201015 s |
| 4 | 0.602015 s |
| 8 | **1.404015 s** |

A 50 ms sleep takes 1.4 seconds of guest-visible time. Reproducible to nine
decimal places across independent runs (1.404015303 twice).

**Mechanism, confirmed by the falsifiable prediction.** Each spinning sibling
contributes one full timeslice of virtual nanoseconds to the sum before the
scheduler re-checks the timed waiter, so the overshoot should be
`(N−1) × max_timeslice` and should scale with a flag that has nothing to do with
sleeping. At 4 threads:

| `--max-timeslice` | (N−1) × T | measured | residual |
|---|---|---|---|
| 50,000,000 | 0.150 | 0.152015 | 0.002 |
| 100,000,000 | 0.300 | 0.302015 | 0.002 |
| 200,000,000 | 0.600 | 0.602015 | 0.002 |
| 400,000,000 | 1.200 | 1.202015 | 0.002 |

Exactly linear, slope N−1 = 3, constant 2 ms residual across an eight-fold range.

Under MAX the aggregate advances at one thread's rate, so the overshoot would be
bounded by roughly one timeslice regardless of N. **This is the strongest
argument for MAX, and it is a scheduling argument rather than a clock-reporting
one.**

---

## 5. Do existing recordings survive?

**No, for any program that uses timed waits. Yes, plausibly, for programs that
do not** — but that is unverified and should not be relied on.

The reasoning, in two parts:

**Recorded timestamps are not affected.** `SchedEvent.end_time` is set by
`with_dettime` from the thread's own `DetTime::without_starting()`
(`detcore-model/src/schedule.rs:100-103`) — thread-local, not the aggregate.
Data flows one way: threads report their clocks up via
`update_global_time(dtid, time_from_guest)`, and the aggregate is derived. The
aggregate never flows back into a thread's local clock. So MAX changes no
recorded timestamp directly.

**But the schedule changes.** Section 4 shows sleep release times move by
`(N−1) × max_timeslice`. Different release times mean different interleavings,
different event order, and different RCB counts between events. Replay is strict
about this: `is_desync` (`scheduler/replayer.rs:143-146`) rejects a mismatch on
`(dettid, op, count)` and, whenever the recorded event carries one, on `end_time`
exactly.

So recordings break through the *schedule*, not the *clock*. That distinction
matters for any migration plan: re-recording is required, and a compatibility
shim on the clock read would not help.

**Race exposure changes too**, for the same reason. Any interleaving reached via
a timeout, sleep, or alarm boundary moves. Chaos mode's random choices are seeded
and would replay, but the timed-wait boundaries they interact with would land
elsewhere. Whether coverage improves or worsens is not predictable from here and
would need measuring.

---

## 6. Is a maximum still non-decreasing?

The owner's stated property — a maximum of non-decreasing values is
non-decreasing — is **true, but it does not survive contact with two details in
this code.**

**Thread exit is safe.** Threads are never removed from `time_vector` on exit
(the only `remove` calls are in `reassign_thread`), so a dead thread's time keeps
holding the maximum up.

**`exec` is not, if `extra_time` is dropped.** `reassign_thread`
(`detcore-model/src/time.rs:862-876`) removes the retired leader's entry and adds
its time into `extra_time`. If MAX were defined as `starting + max(vector)`, the
maximum could **drop** at an exec, violating monotonicity. If it is defined as
`starting + max(vector) + extra_time`, monotonicity holds.

That second definition is the one that keeps the other invariants working, and it
has a consequence worth stating plainly: **`extra_time` must stay in the
aggregate**, because S5 (deadlock avoidance) advances the clock *only* through
`add_extra_time`, and because scheduler time is what guarantees the clock moves
when threads are parked. But `extra_time` is exactly the term that causes the
single-threaded gap in §2. **So the useful form of MAX cannot fix the originally
reported defect even in principle** — the term responsible must be retained for
the scheduler to work.

Two mechanical consequences for any implementation:

- `bump_total(delta)` maintains `total` incrementally by addition. A maximum
  cannot be maintained that way; it needs recomputation or argmax tracking.
- `debug_assert_eq!(self.sum_up(), self.total)` (`time.rs:803-805`) is the
  invariant that would have to be rewritten, and it is the guard that would
  catch a mistake. The `bump_global_time` backwards-motion panic
  (`scheduler.rs:3315`) is a second such guard and is worth keeping.

---

## 7. Evaluation

The proposal is sound in its motivation and mis-aimed at its stated target. A
sum over threads genuinely is the wrong shape for a clock that claims to
approximate wall time: measured, it makes `CLOCK_MONOTONIC` run 8.4× fast at
8 threads where real hardware is flat, and it makes a 50 ms sleep take 1.4
seconds. Both are guest-visible, both scale with thread count, and neither is
addressed by either candidate fix from the root-cause work. On that evidence MAX
is aimed at something real.

But it is not aimed at the defect that prompted it. The reported discrepancy was
measured on one thread, where a sum and a maximum are the same number. That gap
is scheduler time, and scheduler time must remain in the aggregate for the
scheduler to function — so no formulation of MAX that keeps hermit working can
close it. The honest framing is that hermit has **two** clock defects that
happened to be found together, and each option addresses one:

| | single-thread gap (scheduler time) | N-thread gap (summing) |
|---|---|---|
| A | fixed | "fixed" by inflating `rdtsc` |
| B | fixed | not addressed |
| MAX | cannot fix | fixed |
| MAX + A | fixed | fixed, both flat in N |

The combination is the only one that is right on both axes, and it is also the
largest change — MAX alone rewrites what four scheduling consumers mean.

The cost is concentrated in one place and is not reducible by care: sleep
deadlines are denominated in the aggregate, so changing the aggregate changes
when sleepers wake, which changes interleavings, which invalidates recordings for
any program with a timed wait. That is not a migration detail to be smoothed
over; it is the change. A decision to adopt MAX is a decision to re-record.

**Recommendation on process, not on outcome.** Before any implementation, the
question worth settling is which of the two defects is actually hurting users.
If it is the single-threaded clock disagreement, MAX is irrelevant and A is
sufficient. If it is sleep durations distorted by thread count — the 1.4-second
result in §4 — then MAX is the only option that touches it, and the
recording-invalidation cost has to be accepted deliberately. That is a product
judgement about which symptom matters, and it is the owner's.

---

## 8. Threats to validity

- **No MAX implementation exists**, per instruction. Every claim about MAX's
  behaviour is derived from the code and from measurements of SUM. The N=1 rows
  in §3 are what MAX would produce for all N *if* the model is right, but that
  is an inference, not an observation.
- §5's claim that recordings *without* timed waits might survive is untested.
- Race-exposure impact is argued, not measured.
- All measurements are on one host (`devbig014`), one workload shape (spin
  loops), and the ptrace backend. The SaBRe backend sets
  `use_thread_local_clock_reads`, which routes `clock_gettime` to thread-local
  time and so already behaves differently — that path was not measured here and
  is worth checking before any change lands.
- Thread counts 1–8 only.

## 9. Reproducing

```sh
gcc -O2 -pthread -o threadclock threadclock.c
gcc -O2 -pthread -o sleepscale  sleepscale.c
for n in 1 2 4 8; do hermit run -- ./threadclock $n 3000000; done
for n in 1 2 4 8; do hermit run -- ./sleepscale  $n 50;      done
for ts in 50000000 100000000 200000000 400000000; do
  hermit run --max-timeslice $ts -- ./sleepscale 4 50
done
```
