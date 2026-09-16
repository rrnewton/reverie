# The scorecard writeback producer and consumer as one contract

Review for `review-the-writeback-producer-and-consumer-as-one-contract`.
The three defects below are filed as
`writeback-contract-three-defects-wedge-136-gib-across-nine-runs`; that row is
a summary, and this is the evidence behind it.
Measured 2026-09-16 against dev-hermit `7a0518f071df716d33fa19746bf6daa3993e31cc`.
Nothing here was cleaned, reattached or modified. Read-only.

## The field, and everyone who touches it

`scorecard_writeback` is written by `ci-hub/validate/service_result.py` and
carried in the durable run handle under `ignored/validate/runs/<unit>.json`.
Non-test consumers:

| consumer | what it decides |
|---|---|
| `ci-hub/validate/start_unit.py:5278` | whether a terminal checkout may be disposed |
| `ci-hub/landing/pr_head_ci.py:320` | whether a failed writeback still counts as VALIDATED |
| `ci-hub/validate/signal_crosscheck.py:1159` | whether a run handle is a recognised historical shape |
| `ci-hub/validate/pane_watch.py` | reporting only |

## F1 — disposal keys on the bookkeeping, not on the verdict

`start_unit.py:5278` requires `scorecard_writeback == {"status": "completed"}`.
A run that reached a terminal verdict but whose writeback failed is refused
disposal on every pass, permanently, because nothing will ever revisit it.

Measured over all 49 materialized run handles:

| class | n | disposable |
|---|---|---|
| historical path (`state=unknown` + schema + published handoff) | 37 | yes |
| current path (`state=completed` + writeback completed) | 3 | yes |
| **`state=completed` + writeback `failed`** | **2** | **no** |
| **terminal + writeback absent** | **6** | **no** |
| **`state=unknown`, no published handoff** | **1** | **no** |

9 of 49 are wedged. The two failed-writeback runs retain **136.8 GiB**
(63.1 G + 73.7 G). `validate-checkout-cleanup` reports
`removed_checkouts=0 cleanup_failures=4` on every operational tick; the
escalation has fired hourly since at least 06:28Z today.

## F2 — the third state is the largest, and it is defended by a comment

Six of the nine are terminal with **no** writeback value at all. The docstring
at `start_unit.py:5178` states the rule deliberately: *"an absent writeback is
pending, not `False`."* That is right on the first tick and wrong forever
after, because no bound ever converts pending into decided. Ages of the
absent-writeback runs, in hours:

    246.9  201.1  45.6  41.5  41.4  41.1  8.3

The oldest has been "pending" for 10.3 days. None of these six retain a
checkout directory, so the cost is permanently-retained run handles rather
than disk — a bookkeeping leak, not a storage one.

## F3 — the durable record keeps the exit code and discards the reason

`start_unit.py:4813` runs the tool, prints `result.stderr` to the **process's**
stderr (4820–4825), and stores only the return code (4827). But
`scorecard.rs` maps *every* `Err` to exit 2 through a single arm in `main`
(`hermit/ci/compat-envelope/scorecard.rs:2241`):

    Err(message) => { eprintln!("compatibility scorecard: {message}"); ExitCode::from(2) }

So the one line that names the refusal is the one line that is not kept, and
the value that is kept discriminates nothing. Every wedged run reads
identically: `observe-results refused with exit status: 2`. A missing results
directory, a write-lock timeout, a writer-boundary refusal and a tracked-dirt
refusal are indistinguishable in the durable evidence.

That defeats the purpose of retaining the checkout: the artifact was kept so
the failure could be examined without re-running, and the reason it failed was
dropped on the floor.

## F4 — a correlation I found, and the mechanism I disproved

2 of the 19 retained validate checkouts hold a stray untracked
`.hermit-verify-summary-*`, and they are **exactly** the 2 whose writeback
failed. 17 negatives. I took that as the chain: stray summary corrupts source
identity, `observe-results` refuses, writeback fails, checkout wedges.

**That chain is wrong and the code refutes it.** `check_observation_worktree`
(`scorecard.rs:3464`) runs only `git diff --cached --quiet` and `git diff
--quiet -- . :(exclude)SCORECARD.md :(exclude)ci/compat-envelope/cells.json`.
Both are tracked-only; `git diff` does not see untracked files. Confirmed in
the wreckage: `git diff --quiet -- .hermit-verify-summary-yNAe17` returns 0,
and both dirt checks pass there *now*.

The likeliest reading is common cause — a `hermit run` executed in that root,
which both leaves the stray and triggers scorecard observation — not cause and
effect. **PR 3026 must not be credited with fixing these wedged checkouts.**
It fixes the stray; the stray is not why the writeback failed.

## F5 — the two deciding consumers disagree in principle

`pr_head_ci.py:320`, `is_passed_validation_with_failed_scorecard_writeback`,
exists precisely to say that a failed writeback is *bookkeeping only* and the
run is still VALIDATED. The cleanup consumer has no equivalent and treats the
same failure as disqualifying.

They do not conflict on this population: landing's carve-out requires
`exit_code == 75` and passing validation, while all the
wedged runs are `exit_code=1, final_validate_status='FAILED'`. So landing
would not admit them either. The disagreement is in principle, and landing is
the one that already articulates the right rule: a decided verdict is not
invalidated by failed bookkeeping.

## The fix, and the controls it needs

Disposal should depend on the verdict being **terminal and decided**, with the
writeback outcome *recorded* rather than gating:

1. terminal + writeback completed → publish the handoff, dispose (unchanged);
2. terminal + writeback failed → record the failure durably **with the
   producer's stderr**, then dispose;
3. terminal + writeback absent beyond a stated bound → decide it as
   undecided-and-closed, then dispose;
4. genuinely non-terminal → still not disposable.

Both-direction control, because each half can pass for the wrong reason:

- a completed run with a failed writeback **becomes** disposable; and
- a run that is genuinely not terminal **still is not**.

F3 should be fixed first and separately. Until the stderr is retained, every
disposal decision about a failed writeback is made without knowing what
failed, and fixing disposal would dispose the evidence faster.

## Not done

No checkout was cleaned, no run reattached, no cleanup decision taken. The
136.8 GiB is still on disk and the two stray summaries are still in place.
