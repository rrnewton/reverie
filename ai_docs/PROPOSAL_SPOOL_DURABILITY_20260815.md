# Proposal: the validation spool stages results in a gitignored directory

Status: PROPOSED, NOT IMPLEMENTED. ci-hub infrastructure; any change follows the
out-of-place change protocol and needs the owner.
Author: hermit-coord (devbig030), 2026-08-15. All figures measured this session
from the tracked ledger's own history unless marked otherwise.

## The defect

`ci-hub/ledger/validate_rows.py::live_spool_dir` stages every validation result
under `ignored/ci-hub/validate-ledger-spool`, which `.gitignore` excludes. A
result exists in exactly one place, on one host, in a directory that cannot
enter the repository, from the moment it is produced until a publisher moves it.

This is the fourth instance of the same shape found on 2026-08-15 — after the
night report, the exit-gating screen, the mechanical-cells list and the ENOTCONN
gate. **It is the only one nobody chose.** The other three were an author
picking a path and can be fixed by that author picking a better one. This is the
system's default output location, so it recurs without anyone making a mistake,
and it will recur the moment devbig014 returns and the spool starts filling.

The publisher fix landed today (`88ec6e62`) made the drain POSSIBLE from any
host. It did not make the staging durable.

## Exposure, measured — and the unit decides the answer

Latency from `emitted_at` to the commit that published each row, over all 1948
published hermit events:

    min 0.1m   p50 47.9h   p90 110.7h   max 167.3h
    <5m 18.7% | <1h 27.8% | <6h 30.1%

**Per-EVENT, the median result waits two days and the worst waited seven.**

But per-COMMIT the picture inverts: of 150 publish commits, **133 carry only 1-2
events**, i.e. most publishes are prompt. Both are true. They disagree because
**86% of all events (1675/1948) arrive in just 8 bulk commits**, each catching up
a multi-day stall:

    654 events, oldest 97.6h at publish
    555 events, oldest 167.3h
    253 events, oldest 162.2h

So the system alternates between prompt publication and multi-day stalls that
are later bulk-drained. **Quoting "most publishes are prompt" is true and
misleading; the population at risk is events, not commits.** This is the same
wrong-noun error that cost nine hours elsewhere tonight, and it would have
produced the opposite recommendation.

**Largest observed single exposure: 654 events in one spool at once.**

## Why the window is currently unbounded, not merely long

**Nothing runs the publisher.** `systemctl --user` shows no ledger service and
no timer; the publisher's own docstring says the tick launches it detached and
nothing does. Today the window closes only when a coordinator happens to ask —
it closed twice today because one did. That is the same "depends on someone
remembering" shape as the trigger-based re-promotion proposal, and **the two
want the same trigger.**

## Options, smallest first

**A. Do nothing; rely on the publisher fix. REJECTED, with the reason.** The fix
removed the host binding but not the trigger gap. With no scheduler the exposure
window has no upper bound at all, and the measured history is what an unbounded
window produces: repeated multi-day stalls ending in 500-event catch-ups.

**B. Schedule the existing publisher. SMALLEST REAL FIX, RECOMMENDED FIRST.** A
timer invoking `ci-hub/ledger/publish_backlog.py`. It adds no authority: the
publisher already writes through a dedicated clone and `parent-main-write`,
appends only, and drops a batch only after confirming ancestry. This does not
move the spool — it shrinks the window the spool is exposed for, from unbounded
to the timer interval. **It also fixes the re-promotion proposal's trigger gap
if the two share one mechanism.**

**C. Move the spool out of `ignored/`. NOT RECOMMENDED ALONE.** A tracked
staging path makes every unpublished result durable immediately, but it commits
un-adjudicated rows to the repository and inverts the publisher's append-only,
ancestry-confirmed contract into "already committed, tidy up later". The current
design deliberately keeps the ledger a *published* artifact. **B plus the
existing design preserves that; C weakens it.**

**D. Retain a durable copy without changing the ledger contract.** If B's
residual window is judged too large, stage a second copy under a tracked path
that is explicitly a staging area, not the ledger. Costs repository churn.
Worth considering only after B is measured.

## The weak case, stated as such

**If B is adopted and runs on a short interval, the residual exposure is one
interval and this proposal is a ratchet rather than a repair.** A ten-minute
timer reduces the measured p50 from 47.9h to at most 10m, and at that point
whether the staging path is gitignored stops mattering for anything except a
host dying inside the window — which is a real risk but a small one against a
10-minute exposure.

**So the honest recommendation is B alone, and C/D only if B proves
insufficient.** The durable fix is the trigger, not the path. I am not proposing
to move the spool.

What makes it worth the owner's attention despite that: today's window is not
ten minutes, it is unbounded, and the measured consequence is 654 events sitting
undurable at once. **The counter-argument to my own recommendation is that B has
been the obvious fix all along and still does not exist**, which is weak evidence
that scheduling alone is harder here than it looks — the tick-launch path was
designed and is not running, and nobody has established why. That should be
answered before B is built, or B will be the second scheduler that does not run.

## Bracketing, if B is built

- Accept: a batch spooled with the timer active is published within one interval.
- Refuse: with identity underivable the sweep still returns 2 and publishes
  nothing (already proven for `88ec6e62`; must not regress).
- Refuse: a batch whose commit is not an ancestor of freshly-fetched remote is
  still not dropped from the spool.
- Unchanged: `ledger_spool_backlog` still reports `stalled` when a real backlog
  exceeds the bound — the timer must not be allowed to mask the detector.
- Never plant a fake receipt to test any arm; use an inert fixture spool.

## Related and separate

`ledger_spool_backlog` **emits intermittently** — measured absent/present/absent
across three consecutive ticks with the condition confirmed present in the gaps
(see `known-limitations.md`). Any scheduling work must not be validated by
watching that gate, because its silence is uninterpretable.
