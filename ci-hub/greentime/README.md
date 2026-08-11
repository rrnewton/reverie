# `ci-hub green-time` — sparse-signal timeline

Signal names in this metric are inputs, not authority definitions. Follow
skill `hermit-validation-authority` for hard/soft-green meaning, landability, and the
local-versus-GitHub boundary.

## The model

On a branch's linear history every commit either carries a **signal**
(`red` / `soft-green` / `hard-green`) or carries none. State is **carried
forward** from a signal point until the next signal point, where it flips. Any
non-zero amount of signal yields a usable estimate; the estimate converges as
signal densifies, and validating every commit is the precise limit of the same
model rather than a different one.

```
c000        c004        c008                    c020        tip
 |           |           |                       |           |
 hard-green  .           red . . . . . . . . . . hard-green  .
 <--- green ------------><----- red ------------><--- green --->
```

## Why it replaces the reign model

The previous green-time reported ~81% of wall-clock as **"no data"** and
presented that as a measurement. It is not one — it is a statement that nobody
went and got the signal, and it *looks* like a result, which is worse than
saying nothing. Under carry-forward there is no no-data bucket to hide in. What
replaces it is an explicit **quality** report: how sparse the signal is, where
the holes are, and exactly which commits to validate to close them.

## What it outputs

- `green_pct` / `hard_green_pct` / `soft_green_pct` / `red_pct` — over the
  **attributable** denominator.
- `unknown_lead_seconds` — time before the first signal point, which has nothing
  to carry forward from. Excluded from the denominator and never counted green.
- `quality` — `max_gap_seconds`, `target_max_gap_seconds` (default 1 h of
  **commit-timestamp** realtime), `gaps_over_target`, `meets_target`.
- `densification_plan` — the SHAs to validate, **best value first**. Each probe
  is the time midpoint of the widest remaining gap, so it halves the worst
  attribution error. Every prefix is the best plan of its length: run it
  head-first and stop when the box gets busy.
- `red_tightening` — per red observation, a `fix_probe` (walk **later**, so a red
  segment is measured to its real end rather than assumed to run to the next
  arbitrary signal) and a `first_bad_probe` (walk **earlier** — blame). Walking
  earlier is a natural part of debugging a breakage, so this is signal we were
  going to produce anyway.

## Worked example

24 hourly commits, green at `c000`, red at `c008`, green again at `c020`:

```
before: green=47.83%  max_gap=39600s  meets=False  plan=11 probes
after : green=91.30%  max_gap=3600s   meets=True
```

The estimate moved by 43 points because the sparse version was attributing one
large hole to red. That is the model working as intended: usable immediately,
convergent under densification, and honest about which it is.

## Usage

Pure: no network, no repo access, no clock. Commits and signals come in on
stdin, which is what makes it reusable by any project rather than a dev-hermit
one-off.

```bash
ci-hub green-time [--max-gap-seconds N] [--plan-limit K] [--format json|text] < input.json
```

```json
{
  "commits": [{"sha": "…", "timestamp": 1767225600}],   // OLDEST FIRST, history order
  "signals": {"…": "hard-green", "…": "red"},
  "now": 1767312000
}
```

Commits must be in **history order**, not sorted by timestamp: a merge can place
an older timestamp later, and carry-forward follows history.

## Required plumbing — not yet wired

**All debug bisection must run through `ci-hub` so its validates land in the
ledger.** Bisection already produces exactly the signal this metric needs, and
today it is discarded. Two integration steps remain:

1. **Signal source.** A adapter that reads the validate ledger + authoritative
   GitHub conclusions and emits the `signals` map. Until then the model is fed
   by its caller.
2. **Bisection through ci-hub.** `first-bad` / multisect paths must launch their
   probes via `ci-hub validate-run` so each lands a receipt. This is the step
   that makes the metric densify itself as a side effect of ordinary debugging.

## Known limitation, stated rather than hidden

A red→green flip entirely inside a no-signal gap is invisible; the gap is
attributed to the earlier state. `max_gap_seconds` bounds that error, which is
precisely why densification is part of the metric rather than an optional extra.
In practice the bias is mild: fixing a breakage involves thrashing that leaves
its own signal.
