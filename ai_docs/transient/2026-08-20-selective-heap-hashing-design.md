# Selective and targeted full-heap hashing — design proposal

*2026-08-20. Author: claude (diverge), task `design-doc-selective-targeted-full-heap-hashing`.
Proposal only; nothing here is implemented.*

## 1. Why

`--detlog-heap` is unusable as an always-on check, and the reason is a multiplier,
not a slow hash. Measured today on the demo-5 QEMU/Linux boot (632,229 syscalls,
191,796 scheduler turns, hermit release `52204423…`):

| arm | wall | vs no-logging |
| --- | --- | --- |
| no logging | 43.71 s | 1.00x |
| info log | 55.59 s | 1.27x |
| `+ --detlog-regs` | 58.17 s | 1.33x |
| `+ --detlog-stack` | 273.17 s | 6.25x |
| `+ --detlog-heap` | **8,390.80 s (2.33 h), measured** | **192x** |

It hashes **10.9 TB per boot to observe a heap that never exceeds 16.49 MiB**,
because it re-reads the whole region after *every* syscall whether or not that
syscall touched a byte of it.

The proposal is the owner's framing: stop treating `--detlog-heap` as a check
you leave on, and make it a **targeted measurement an agent points at a specific
divergence**.

## 2. The measured cost model, and what sampling can and cannot touch

Fitted with the existing flags, each term varied with the other held fixed, and
validated four ways (predicted `--detlog-stack`'s boot increment 226 s against a
measured ~222 s; implied QEMU's mapping count 487 against an actual 499–506;
predicted the heap boot's rate 12.85 s/1000 syscalls against a measured
12.8–13.3; predicted the maps-parse term 158 s against a measured 164 s):

```
cost = syscalls × ( mappings × 0.5 µs  +  region_bytes × 0.729 ns )
         ^^^^^^^^   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
         sampling   per-hash cost — sampling does not change this at all
         attacks
         only this
```

For QEMU at a 16.49 MiB heap the per-hash cost is **12.95 ms** — 0.25 ms of
`/proc/<pid>/maps` parsing and 12.7 ms of hashing. **Every axis in this document
multiplies that number by fewer occasions. None of them makes it smaller.**
Making it smaller is the subject of
[hermit#2344](https://github.com/rrnewton/hermit/issues/2344), *"Backlog:
accelerate memory hashing — incrementality, parallelism, and divergence
localisation"* (incremental page-level hashing gated on dirty-page tracking,
parallel hashing, a faster algorithm). The two are complementary and should not
be conflated: **#2344 attacks the price of one hash; this document attacks how
often you pay it.**

Two further distinctions from #2344, so neither is read as covering the other:

- Its "divergence localisation" section is about **spatial** localisation —
  which page or address inside the heap differs, via hierarchical hashes. This
  document is about **temporal** localisation — at which point in the run the
  difference first appears. An investigator generally needs both, and they
  compose: §5 here names the syscall, #2344's hierarchy would name the page.
- #2344 is explicitly filed as BACKLOG and its central mechanism is blocked on
  an unanswered precondition (no dirty-page tracking of any kind exists in
  Reverie today — no `KVM_GET_DIRTY_LOG`, `clear_refs`, soft-dirty or `pagemap`
  use anywhere in the source). **Everything proposed here is implementable with
  what is already in the tree**, which is the main argument for doing it first.

Consequence worth stating plainly: the floor for any sampling scheme on this
workload is 12.95 ms per sample point. A scheme that wants 10,000 sample points
costs 130 s no matter how cleverly the points are chosen.

## 3. Precedents already in the tree

This should not invent mechanism where the project already has some. Three
existing things are directly reusable:

- **`--detlog-regs-cadence <N>`** — "hash every Nth guest-logical-control point".
  This is already exactly a sampling axis, with an established vocabulary (its
  doc calls `1` the *full tier* and `N > 1` a *spot-check tier*, and every emitted
  line records which tier produced it). Two lessons in its implementation must
  carry over verbatim:
  - *The cadence counter is PER-THREAD, not a shared atomic.* A global counter
    would be incremented in whatever order threads happen to reach it, so the
    sampling schedule itself would be nondeterministic — and a determinism
    instrument must not have a nondeterministic sampling schedule.
  - *It indexes from zero so the first control point of every thread is always
    sampled.* The earlier version indexed from the log's syscall ordinal (which
    starts at 2), so a guest whose control points never landed on a multiple of
    the cadence emitted **nothing** and the run still reported PASS. A
    spot-tier green backed by zero samples is worse than no tier at all.
- **`--stop-after-turn <N>` / `--stop-after-iter <N>`** — existing addressing of
  a specific scheduler turn (requires `--sequentialize-threads`; the config
  already warns and disables when it is not set).
- **`--stacktrace-event <index[,path]>`** — an existing repeatable "do something
  at these specified points" flag, including the optional per-point output path.

The scheduler turn number and the virtual-time value are already in every COMMIT
record, so all the coordinates below are already observable in the log.

## 4. The axes, and what each actually buys

Costs are for the demo-5 boot: 191,796 turns, 632,229 syscalls, 3.30 syscalls per
turn, 12.95 ms per hash, 8,187 s to hash at every syscall.

### 4.1 Every K scheduler turns

| K | sample points | hashing cost | vs the 43.71 s baseline |
| --- | --- | --- | --- |
| 1 (turns) | 191,796 | 2,484 s | 57x |
| 10 | 19,180 | 248 s | 6.7x |
| 100 | 1,918 | 24.8 s | 1.6x |
| 1,000 | 192 | 2.5 s | 1.06x |
| 10,000 | 19 | 0.25 s | 1.01x |

**What it buys:** uniform coverage of the whole run at a cost the investigator
chooses. This is the right *first* pass — the one that answers "did the heap
diverge at all, and roughly where". At K=1,000 it is essentially free.

**Determinism:** turn numbers are globally ordered and deterministic, so the
schedule is identical across runs without extra work. This is the safest axis.

### 4.2 At specified turns

**What it buys:** zero waste. This is the *second* pass, after 4.1 has named an
interval. Cost is exactly `points × 12.95 ms`.

It is also the axis that makes exhaustive narrowing practical — see §5.

### 4.3 At specified syscall counts

**What it buys:** the same addressing in the coordinate the DETLOG syscall
records already use (`finish syscall #352`), which is what an investigator is
actually reading when they find something suspicious. Converting a syscall
ordinal to a turn number by hand is an avoidable step.

**Caveat that must be designed for:** the syscall count in the log is
**per-thread** (`dtid 3` has its own `#352`), while turn numbers are global. A
syscall-count sample point therefore has to be qualified by thread, or it means
different things in different threads. `--detlog-regs-cadence` already made this
choice — per-thread — and this axis should match it rather than differ.

### 4.4 At specified syscall types

**What it buys:** targeting a suspected mechanism rather than a location. "Hash
after every `recvmsg`" is the natural way to test a hypothesis about a specific
syscall.

**Its cost is entirely a property of the type's frequency, and the spread is
four orders of magnitude.** From the boot:

| type | calls | hashing cost |
| --- | --- | --- |
| `recvmsg` | 244 | 3.2 s |
| `getdents64` | 34 | 0.4 s |
| `read` | 22,875 | 296 s |
| `poll` | 17,120 | 222 s |
| `write` | 207,108 | 2,682 s |

So this axis is excellent for a rare syscall and no help at all for a common
one. The doc should not present it as a general cost reduction; targeting
`write` saves only 67% while still costing 61x the baseline.

### 4.5 At virtual-nanosecond sample points

**What it buys:** the one coordinate that is meaningful *across configurations*.
Turn numbers and syscall counts are properties of one run's schedule; virtual
time is hermit's deterministic clock and is comparable between two runs that
executed different amounts of scheduler bookkeeping. It is also the coordinate
the owner's other work is already expressed in.

**The honest limit:** if the *schedule itself* has diverged, no coordinate is
comparable — turn N in run A and turn N in run B are not the same point, and
neither are equal virtual timestamps. All five axes assume the divergence being
hunted is a **content** divergence with the schedule still identical, which is
exactly the case measured today (ten boots agreed on every syscall, every COMMIT
turn and every register hash while the payload differed). For a schedule
divergence, `log-diff` on the COMMIT stream already localizes it and no memory
hashing is needed.

## 5. Narrowing from a coarse sample to a precise point

A sampled hash says *the state differed somewhere in `(T_i, T_{i+1}]`* — not
where. This is the axis' central weakness and the design has to answer it.

**Bisection works, but it is the wrong tool here, and the arithmetic says so.**
Bisecting a 1,000-turn interval to a single turn is ~10 rounds, and each round
pays a whole boot (43.71 s baseline plus the run's own hashing), so ~10 boots
≈ 8–10 minutes dominated entirely by re-booting, not by hashing.

**Exhaustive hashing of the suspect interval is better and needs only ONE more
run.** An interval of `S` syscalls costs `S × 12.95 ms`:

| interval | syscalls | hashing cost | affordable in one run? |
| --- | --- | --- | --- |
| 100 turns | 330 | 4.3 s | yes |
| 1,000 turns | 3,296 | 42.7 s | yes |
| 4,600 turns | 15,180 | 197 s | yes, comparable to `--detlog-stack` today |
| 10,000 turns | 33,000 | 427 s | getting expensive |

So the recommended workflow is **two runs, not ten**:

1. Pass 1 — every K=1,000 turns. Hashing cost 2.5 s. Names an interval of 1,000
   turns.
2. Pass 2 — hash at *every* turn in that interval (§4.2). Hashing cost 42.7 s.
   Names the exact syscall.

Total ≈ 45 s of hashing across two boots, against 2.28 hours for the flag as it
stands: a localization that currently costs 2.28 h becomes ~2 minutes wall.

K should be chosen so that one interval is exhaustively affordable. On this
workload anything up to K ≈ 4,600 turns keeps pass 2 under ~200 s, so **K=1,000
is a safe default with a wide margin**.

**⚠️ The assumption both passes rest on, and it must be checked before relying
on them.** Iterative narrowing requires the divergence to *recur at the same
point on a later run*. That is true for an input-determined divergence — the
netlink case reproduced 5 of 5 — and false for an intermittent one. When an
investigator cannot assume recurrence, the schedule of sample points must be
fixed in advance and captured in a **single** run, which argues for supporting a
list of points (§4.2) and not only iterative refinement. A run that has to be
right the first time should spend its whole budget on one dense pass rather than
on a cheap coarse pass it cannot follow up.

## 6. What sampling does NOT fix

**Sampling makes an existing tool affordable. It does not widen what that tool
can see.** Measured 4×3 today, three runs per cell, same netlink exchange in
every arm with only the receive buffer's home changed:

| buffer home | `--detlog-stack` | `--detlog-heap` | both |
| --- | --- | --- | --- |
| `[stack]` | CAUGHT | **MISSED** | CAUGHT |
| `[heap]` (brk) | **MISSED** | CAUGHT | CAUGHT |
| BSS / static | **MISSED** | **MISSED** | **MISSED** |
| anonymous `mmap` | **MISSED** | **MISSED** | **MISSED** |

`--detlog-heap` hashes one named mapping. Sampling it every K turns, at chosen
turns, by syscall type or on a virtual-time grid changes *when* that mapping is
hashed and never changes *which* mapping it is. A divergence in a stack buffer,
a static buffer or an anonymous `mmap` is invisible at every sampling rate,
including rate 1.

Two of those four homes are missed even with both flags enabled, and anonymous
`mmap` is not a corner case — it is where glibc puts any `malloc` above the
128 KiB `M_MMAP_THRESHOLD`.

**Coverage is the job of the syscall-boundary output-buffer check (working name
`io-buffers`, the owner's candidate name and not yet settled), which takes the
address and length from the syscall arguments and is therefore indifferent to
where the buffer lives.** Nobody should read this document as making that work
unnecessary; the two solve different problems and the project needs both:

- sampling → *makes a whole-region check affordable to point at something*
- syscall-boundary check → *sees the bytes a syscall actually moved, anywhere*

There is one place they meet, and it is worth noting because it argues for
building both: a syscall-boundary check tells you *which syscall's buffer*
diverged but says nothing about memory the guest computed without a syscall,
while a sampled whole-heap hash covers everything in that one mapping including
purely computational state. Neither is a superset of the other.

## 7. One defect to fix regardless of which axes are built

`detlog_memory_maps` (`detcore/src/lib.rs:858`) gates on the **config flag**,
with no log-level check. `procmaps::from_pid()` is then a plain call that runs on
every syscall whether or not the record is observed; only the hash itself is
short-circuited by the `detlog!`/`tracing::info!` macro. Measured at boot scale
with `RUST_LOG` unset, so each run produced **123 bytes** of log:

```
no flag           43.71 s   1.00x
--detlog-regs     45.86 s   1.05x   (free — the register path is properly inert)
--detlog-stack   190.37 s   4.36x   146.7 s spent to produce 123 bytes
--detlog-heap    207.90 s   4.76x   164.2 s spent to produce 123 bytes
```

Any sampling scheme inherits this: a run sampling at 192 points would still pay
the maps parse at all 632,229 syscalls. The parse must move inside the
log-level gate, or the sampling gate must be evaluated before it. This is
independent of which axes are chosen and should not wait on them.

## 8. Recommended order

1. The §7 gating fix — smallest, and every other item is worth less without it.
2. §4.1 every K turns — one integer, the biggest single win (187x → 1.06x at
   K=1,000), and it reuses the `--detlog-regs-cadence` tier vocabulary and its
   two hard-won correctness rules.
3. §4.2 specified turns — makes the two-run workflow in §5 possible; without it
   §4.1 finds an interval nobody can narrow cheaply.
4. §4.4 syscall types — cheap to add, genuinely useful for rare syscalls,
   but label its cost honestly per type.
5. §4.3 syscall counts and §4.5 virtual-nanosecond points — convenience
   coordinates over the same mechanism; add when someone needs them.

## 9. Provenance

Every number here is measured on this box today under task
`measure-what-detlog-heap-actually-costs`, on hermit release binary sha256
`52204423138bb2633de007afd826467fc1adb3055a46f5fbe86e28fce85faa81`. Scripts and
raw data: `experiments/first-divergence_20260820/price/` (`hashcost.c`,
`hashcost2.c`, `netlink_where.c`, `sweep.sh`, `sweep.txt`) and
`experiments/first-divergence_20260820/runs/`.

The `~8,200 s` full-boot `--detlog-heap` figure was a projection when this was
written. **The confirming full boot has since COMPLETED: 8,390.80 s wall, rc=0,
633,448 syscalls, 389.5 MB of log** (`experiments/first-divergence_20260820/runs/hf/`).

That is **192x** the 43.71 s no-logging baseline, and it lands **2.1–2.7% above**
the projection — the cost model predicted 8,171 s and the measured flat rate
extrapolated to 8,222 s. The model was fitted on a toy microbenchmark and
validated out of sample four times before this run; surviving contact with the
full 2.3-hour boot is the fifth. Every figure in §4 and §5 derives from that
model, so the ~2% error is the error bar on all of them.
