# Screening the disabled `verify/kvm` slots for whether a fixture can refuse

Captured 2026-08-15 by `hermit-kvm2` on `devbig030.atn3.facebook.com`.
Hermit `e18acde586f72829e4a96d92b57b78f3abe21b2c`; `origin/main` at capture was
`61b9ba7aa4dd763c1bda091316852d5290f9466c`; Reverie pin `c261050cfd41bec67e31bfd0cf6f56be008d0ebb`.

## Hypothesis

Enabling a cell is only worth doing if that cell can go red when the backend is wrong. These
operators measure, per fixture, whether a defect would reach the channel the verdict reads.

## What the verdict actually reads

Established from `ci/test_harness.sh`, not assumed. The gate is an `elif` chain (~3785-3810):

1. guest-launch-refused
2. timeout → `FAIL`
3. **`elif [[ $status != 0 ]] → FAIL`** — exit status, unconditional, every mode
4. `elif [[ $mode == verify && verify_asserts_bitwise_parity ]]` → require full parity

Step 4 is a per-cell manifest opt-in (`verify_asserts_bitwise_parity`, line 3185, defaults false)
used by **2** cells in the whole tree on `origin/main`. So for ~every cell here, **exit status is the
operative channel**. All 313 slots carry an identical `observation = {status=true, stdout=true,
stderr=false}` — **stderr is not observed**, so a fixture whose only failure report is
`fprintf(stderr, ...)` while returning 0 cannot express failure at all.

Note also: a `verify` cell runs the same backend twice and compares run-to-run, so stdout catches
*variation*, never *wrongness* — a deterministic wrong answer compares equal to itself.

## Two operators, and why both are needed

| | `operators/screen.py` | `operators/value_mutate.py` |
|---|---|---|
| mutation | flip one `==`/`!=` (check sense) | `+1` on an integer reaching stdout |
| question | does *some condition* reach exit status? | would a *wrong value* be caught? |

**They give opposite verdicts on the same fixture.** Many fixtures check that a syscall *succeeded*
and never compare the value — the expected value lives only in a header comment. `vectored_io.c`
(`checksum=32640 → 32641`) and `eventfd_probe.c` (`counter=36 → 37`) both survive at rc 0.

Check mutation is strictly *more* sensitive, so its REFUSABLE count is an **upper bound** and its
CANNOT-FAIL verdicts are safe.

## Results

Population: the 313 manifest entries with a `kvm` key under
`[test.modes.verify.backends_disabled]` (313 disabled + 23 enabled = 336).

Check mutation over 313 — REFUSABLE (exit-status gated) **86** · CANNOT-FAIL (not exit-gated) **34** ·
recorded KVM capability limit **79** · NOT-DETERMINABLE **114**.

Value mutation over the 86 — VALUE-GATED **10** · NOT-VALUE-GATED **24** · NOT-DETERMINABLE **52**.
So the bound tightens from `≤86` to **`[10, 62]`**; 24 slots move out of REFUSABLE on measurement.

`--probe-disabled` sample (15 cells) — 12 PASS, 3 FAIL. Of the 11 whose justification *asserts* an
observed KVM failure, **9 now PASS**: those justifications are stale. `--probe-disabled` is a
selector (it only sets `classification=disabled` and swaps `emit_required_plan` for `emit_gap_plan`);
it does not touch the verdict path.

## Two caveats a later user needs

**1. Load manufactures failures, not passes.** Under host contention a PASS is robust — it completed
and satisfied every gate, and an idle host would only make that easier. A FAIL is *unattributable*:
it may be a timeout or launch refusal caused by load. This is why the 12 PASSes above stand while the
3 FAILs must be re-run before anyone records them as KVM gaps. The probe sample was taken at load
average 20/51/57.

**2. One row was screened against older source — now resolved, and the resolution is the more
interesting half.** At capture the checkout differed from `origin/main` by exactly two lines, both in
`tests/backend-parity/fixtures/set_tid_address.c` and `backend-parity-c.toml`. Hermit `main` has
since advanced to `302a1a9c` and those two lines are what changed:

```
-    return 0;
+    return ok == 4 ? 0 : 1;
+assert = { bitwise_parity = true }
```

The older source had **no non-zero exit path at all**. This screen bucketed it
`REFUSABLE / channel=stdout` — "mutation left exit status 0 but changed observed stdout" — and the
final partition moves every stdout-channel row *out* of the exit-status-gated 86. So the verdict was
correct for the source it was measured against, and **no number in this directory changes.**

What happened in the tree is better than a caveat resolving: someone **converted that fixture from
not-exit-gated to exit-gated and paired it with the parity assert**, which is exactly the defect
class this screen identifies — acted on in the tree while the screen was running, rather than merely
recorded. `ci/test_harness.sh` was byte-identical to `origin/main` at capture, and at `302a1a9c` its
verdict chain, probe selector, relaxation profile and parity gate are all still untouched.

## Operator gotchas, each one measured rather than anticipated

- **`return ok == 4 ? 0 : 1` is the canonical case for why a static pattern undercounts.** It has
  no `return [1-9]`, so a grep for that scores the fixture as unable to fail while it refuses
  perfectly well. Check any pattern against this form before trusting it. This one shape has now
  caught this fleet out three separate times in a single day: it defeated an earlier lane's static
  screen, it defeated the first version of the operator here, and it is the form the tree chose when
  fixing `set_tid_address.c` (see caveat 2). Treat it as the standing control case, not an anecdote.
- **Mutate one site at a time.** A naive mutation misses it too, so this is not solved merely by
  preferring mutation over grep. Flipping every `==` at once lets mutations cancel: on
  `return ok == 4 ? 0 : 1`, two flips restored a 0 return and the fixture scored CANNOT-FAIL. Wrong,
  and in the dangerous direction.
- **Strip string literals before finding write sites.** `printf("counter=%llu", ...)` matches as a
  write to `counter`.
- **Try every write site**, not first and last — the genuine one is often in the middle.
- **Exclude pointers and array indices.** `state += 1` on `struct s *state` is pointer arithmetic
  (hang); `id += 1` used as `arr[id]` segfaults. Both fake a VALUE-GATED verdict.
- **Inject after the write, never at the printf** — the printf is downstream of any check, so
  injecting there steps over the very check you are testing for.
- Keep "mutation never applied" strictly apart from "applied, nothing changed". Only the second is
  evidence; the first is NOT-DETERMINABLE.

## Controls

`controls/` holds the ground-truth pair for each operator. Each pair differs in **exactly one**
respect — the presence of the check — so it pins the operator; fixtures differing in more than one
respect would pin nothing.

- check operator: `pos_ternary.c` (must be caught) · `neg_cannotfail.c`, `neg_stderr_only.c`
- value operator: `val_must_be_caught.c` (has `if (total != 36) return 1;`) ·
  `val_must_not_be_caught.c` (identical, 36 only in a comment)
- `pos_known_refusable.c` is the corpus fixture that the check operator **fails** to detect (it exits
  1 standalone, needing the harness environment). Kept as a recorded negative result.

Run the controls first. They caught four defects in these operators before any result was believed.

## Reproducing

Operators run fixtures **natively** — they never invoke hermit or the harness, because the question
is about the fixture's source, not the backend.

These files are byte-identical to the ones that produced the results in `results/`, hardcoded paths
included, so the record matches what was measured. Two edits are therefore needed before reuse: set
`REPO` at the top of `screen.py` and `value_mutate.py`, and fix the absolute `sys.path.insert` and
output filename in `run_value.py`. Then:

```bash
python3 operators/screen.py        # reads results/population.json, writes results.json
python3 operators/run_value.py     # reads results/refusable86.json, writes value_results.json
```

`operators/probe_batch.sh` drives `ci/test_harness.sh run --probe-disabled` one cell at a time
(that selector requires exact `--test`, `--mode` and `--backend`).

Stability: the value operator was run twice over all 116 rows with **0 verdict disagreements**, so
one pass suffices. Per-row stability is *not* guaranteed for the concurrency fixtures —
`lock_granularity.c` was observed once with a flaky standalone baseline.


## Third operator: `operators/direct.py` — is a recorded justification still true?

Added after the two mutation operators. A disabled entry's justification records why a cell was
turned off **at authoring time** and cannot know about a fix landed later, so it goes stale by
default. This classifier re-checks one, three ways:

| verdict | meaning |
|---|---|
| `DOES-NOT-HOLD` | the cell now passes `--strict --verify` on kvm; the reason no longer describes reality |
| `STILL-HOLDS` | it fails **and** the guest's own message names the boundary the reason cites |
| `NOT-DETERMINABLE` | timeout, no message, or a message that does not correspond to the stated boundary |

Result over the falsifiable subset — the 79 of 313 whose reason *asserts* a failure, all 79 run:
**22 DOES-NOT-HOLD · 3 STILL-HOLDS · 54 NOT-DETERMINABLE.** Of the 25 decidable, 22 are stale.
That is a floor, not a total, and 22/25 does **not** extrapolate onto the 54 — 44 of those are
non-C fixtures, correlating with the privileged-runner reasons, so they are a systematically
different class rather than a random remainder. The remaining 234 entries say "not yet qualified"
and cannot go stale; a truncated sample of 8 of them is in
`results/survey-notyetqualified-truncated.log` (N=8 carries no rate, only the existence claim).

The 3 `STILL-HOLDS` are all prctl-family — `prctl-pdeathsig`, `thp-disable`, `prctl-identity` —
each naming its cited boundary. Some justifications in one syscall family are current while others
beside them are obsolete, so the axis must be measured per cell.

### Two things this operator taught, both worth knowing before reusing it

**The probe harness discards the discriminating evidence.** A failed `--probe-disabled` cell reports
only `verify exited with status 1` in its results row and its junit; no guest stderr is retained.
The boundary-naming message is obtainable *only* by re-running the cell directly. A staleness survey
built on harness rows alone cannot apply the message-bearing test at all.

**Validate the cheap path against the expensive one before scaling.** Direct runs cost seconds where
the harness costs ~90s of setup, so the survey is only affordable directly. Checked against 15
already-measured harness verdicts: the first pass disagreed **12 of 15**, every disagreement one
cause — `tempfile` stages the guest under `/tmp`, which hermit isolates and refuses to launch from.
After staging outside `/tmp`: **15/15 agree**. Without that check the survey would have reported
near-total `NOT-DETERMINABLE` for a reason having nothing to do with the cells.

### Known defect in this operator

`subprocess.run(timeout=)` kills hermit but not its grandchildren, and they hold the stderr pipe
open, so the read blocks past the timeout and the classifier wedges — it stalled one sample and left
three orphaned hermit processes. Before running this at scale, give it `start_new_session=True` and
kill the process group.


## Fourth operator: `operators/qualify.py` — a candidate is not a qualified cell

A justification that no longer holds makes a cell a **candidate**. Enabling needs both directions
measured, so this operator runs two arms per cell:

- **ACCEPT** — the real fixture passes `--strict --verify` on kvm.
- **REFUSE** — an **inert violating copy** of the same fixture *fails* the same command. The copy is
  a mutant staged in scratch; no manifest and no real cell is ever touched.

A cell that passes but cannot be made to fail would book a green that can never go red, which is
worse than leaving it disabled.

Over the 22 candidates (all of them C fixtures), at load 17-25:

| | |
|---|---|
| qualified — both arms measured | **9** |
| refuse arm measured, **could not be made to fail** | **11** |
| refuse arm **not measured** (no mutation sites) | **2** |
| accept arm passed | **22 of 22** |

All 22 passing the accept arm re-confirms the staleness result by a second route. The 2 with no
mutation sites are *not measured*, kept apart from the 11 that were measured and did not refuse —
"mutation never applied" is never evidence.

**No cross-validation against the check-mutation screen is available**, and the reason is worth
recording: that screen gave the recorded-capability-limit reasons their own bucket and did not
mutate them, while all 22 candidates come from exactly that bucket. The two populations are
disjoint. A first cross-check appeared to yield "11 consistent, 9 divergent"; it was vacuous.

This operator also carries the fix for the defect in `direct.py`: children run via
`start_new_session=True` and a timeout kills the whole process group, so hermit's grandchildren
holding the stderr pipe can no longer wedge the run.


### Second refuse arm: `operators/refuse2.py`

The refuse arm above uses one operator (check mutation). A single negative is weak evidence for
"cannot be made to fail", so the 13 non-qualified cells were re-attempted with the *other* mechanism
— value mutation, run under kvm rather than natively.

**It promoted zero.** All 11 that the check arm could not fail stay negative under value mutation
too, and the 2 with no usable site under one operator have none under the other either. So:

| | |
|---|---|
| qualified — accept and refuse both measured | **9** |
| refuse arm negative under **both** operators | **11** |
| refuse arm **not measured** by either | **2** |

Two independent operators agreeing that a cell cannot be made to fail is much better evidence than
one, and it is the evidence that matters: those 11 pass today, so a staleness check alone would mark
them enablable, and enabling them would book a green that can never go red. Measured at host load
16-22, hermit `e18acde5` (verdict chain unchanged at `302a1a9c`).


## Compile-flag divergence between this screen and the harness — measured, and it inverts

The operators compile `cc -std=gnu11 -O1 -w -D_GNU_SOURCE -pthread`. The harness compiles
`cc -std=c11 -O2 -g -Wall -Wextra -Werror` **plus each cell's `build.cflags`** (`test_harness.sh:341`
maps `.build.cflags` into `compile_args`, applied at `:3143`). A screen that builds differently from
the harness is predicting a binary nobody runs, so the divergence was measured over all 267 C
fixtures screened here.

**The expected direction does not occur, and the measurement carries a positive control.** The
concern was that `-w` hides warnings `-Werror` would turn into errors, so a fixture that cannot build
for real would read as a clean result here. Measured: **0 fixtures build under the operator flags and
fail under the harness flags.**

That zero is only worth reading because the same measurement *can* return non-zero: omitting each
cell's declared `build.cflags` from the harness command produces **54 failures over these 267
fixtures**. The mechanism is real and the test would have detected it. hermit-teardown reached the
same conclusion independently on the narrower population of the 25 passing cells — 25 of 25 compile
both ways with the declarations, **5 failures without them**. Two populations, two positive controls,
same verdict: **the mechanism is real but does not bite here.** Keep the control attached to this
result, because a population that declares `cflags` differently would make it bite. The reason
is that the harness is *not* uniformly stricter — 69 of 283 C fixtures carry `build.cflags`, and 35
of those carry six `-Wno-error=` relaxations precisely so `-Werror` does not reject them. Reproducing
the harness command without `build.cflags` produces 54 spurious failures and is simply the wrong
command; that mistake was made here first and caught by a contradiction, since one of the "failing"
fixtures had already PASSED through the real harness probe.

**The real divergence runs the other way: 9 fixtures the operator flags cannot build, the harness
builds fine** — they need `-fopenmp`, `-nostdlib -static`, or a `-D` the operator does not pass. All
9 were scored `NOT-DETERMINABLE` ("does not compile standalone"), so the cost was avoidable unknowns,
never a false clean result. Re-screened under the harness flags, 2 of the 9 yield a real verdict
(`proc_fdinfo`, `pread64_nostdlib`, both REFUSABLE via exit status) and 7 stay undecidable for
unrelated reasons.

**For fixtures that build under both flag sets the divergence is verdict-neutral, and that is
measured rather than argued:** the same operator was re-run under harness flags on a 45-fixture
sample and returned **45 of 45 identical verdicts, 0 changed**. `-O1` versus `-O2` did not move a
verdict on any of them. That is evidence about this sample, not a proof for every fixture; a
fixture with undefined behaviour could in principle differ under a different optimisation level.

### Corrected counts

| | |
|---|---|
| REFUSABLE (exit-status gated) | 86 → **88** |
| NOT-DETERMINABLE | 114 → **112** |
| CANNOT-FAIL (not exit-gated) | 34, unchanged |
| recorded KVM capability limit | 79, unchanged |

The value-mutation bound was computed over the old 86; the 2 additions are unmeasured on that axis,
so it becomes **`[10, 64]`**. No staleness or qualification verdict is affected — none of the 9
carried one, so **25 / 22 / 9 all stand.**

### Two nouns that are easy to swap, stated explicitly

- **25** cells returned `DOES-NOT-HOLD` — that is the count of cells that *pass today*.
- **22** of those 25 are `cap=True`, i.e. cells whose recorded reason actually asserts a failure.
  **22 is the count of stale justifications**; the other 3 are "not yet qualified" reasons, which
  cannot go stale because they never claimed the cell fails. 22 is also, coincidentally, the number
  carried into qualification — because qualification only took the falsifiable ones.
- **9** of those 22 qualified on both arms. **Reading 22 as "enablable" overreads by more than
  double**: 11 pass but cannot be made to fail under either operator, and 2 are unmeasurable.

## Files

`metadata.json` records a `sha256` and line count for every file here.
