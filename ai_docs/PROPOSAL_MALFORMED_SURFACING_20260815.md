# Proposal: make a malformed-line repair visible in `approval_binding --gate`

Status: PROPOSED, NOT IMPLEMENTED. CI infrastructure; any change follows the
ci-hub out-of-place change protocol and needs the owner.
Task: `malformed-state-word-does-not-move-while-the-count-does`
Author: hermit-coord (devbig030), 2026-08-15. All figures measured this session.

## The defect, in one line

`malformed_state` reports `present` whether three lines are malformed or one, so
a reader who repairs one and re-runs the gate sees no change in the field they
reached for and may conclude the repair failed.

Found while verifying my own repair of PR #2182: the count went 3 -> 2 and the
state word did not move. The concrete harm is a wrong *second* action — either
reverting a working fix, or editing a second historical comment chasing the
first. Both damage records and the second damages one that was fine.

## Size, stated plainly

**One field, one gate, zero production consumers.** This is not a class of
defect. The information already exists, is already computed, and is already
emitted in the `--json` mode. Nothing new is measured.

## What consumes `malformed_state` — measured, not assumed

Fleet-wide grep with no truncating filter, because this asserts an absence.

Production consumers: **zero**. The only parsers are the field's own tests,
`ci-hub/health/tests/test_approval_binding.sh:165` and `:187`. Two other
fleet-wide hits are unrelated concepts, checked rather than assumed:
`agent-utils/py/tests/test_tickhub_cli.py:188` (tick-hub YAML validity) and
`agent-utils/py/tests/test_rebase_delta_guard.py:386` (rebase field shapes).

The four real gate consumers all bypass the text fields:

| Consumer | How it invokes | What it reads |
| --- | --- | --- |
| `ci-hub/landing/pr_head_ci.py:105` | `--gate` | `returncode` only |
| `ci-hub/landing/land-pr.sh:503` | `--json` | JSON, never these lines |
| `ci-hub/health/pr_status.py:84` | module import | not stdout |
| `ci-hub/health/tick-hub.yaml:629,656` | `--gate --gate-on ...` | rc, plus `{summary}` in the title |

So redefining `malformed_state` would break two test assertions and no callers.
That is not a licence to redefine it. The boolean is correct for a boolean
consumer, and those assertions encode the #1847 advisory contract. **Add
discrimination; do not change existing semantics.**

## Do the siblings share it? No — and that is the good news

The task flagged #2255/#2250 as the more consequential instance. Measured with
exactly the tick's command (`--gate --gate-on superseded`, rc=1):

```
summary=2/16 approval labels fail the superseded binding check; affected=#2255,#2250; ...
```

The tick title interpolates `{summary}`, so both the count and the affected PR
list reach the reader. Repairing #2255 changes the title to `1/16 ...
affected=#2250`. **A binding repair is already visible.** Only the unread
`state=unbound` word stays constant.

Inert caveat, recorded not proposed: `affected` is built from `gating[:8]`
(line 547), a truncating filter. Safe while read as presence; unsafe if ever
read as a complete set. With 2 failures it cannot bite.

## The larger finding, and it is a different defect

Lines 612-617 make malformed **advisory**: print `GATE ADVISORY`, return 0.
Measured on PR #2215, which carries a malformed line and nothing else:
`state=ok`, `malformed_state=present`, **rc=0**.

The tick fires `when: failure`. **A malformed line alone therefore raises
nothing.** Every malformed line seen tonight reached a human only because #2255
and #2250 independently force rc=1 and `capture: true` dumps stdout alongside.
The malformed report is riding an unrelated alarm.

**The trap this sets on the repair path:** repair #2255 and #2250 — the
explicitly correct next action — and the tick goes green, at which point the
remaining malformed lines stop being reported at all. Not undiscriminated:
unreported. Observability gets worse as a direct consequence of doing the right
thing.

## Proposed change

**P1 — surface the count and membership in the text mode.** Two additional
lines beside the existing three, inside the existing `if malformed:` block. Both
values already exist: `len(malformed)` and `sorted(malformed_prs)` (set built at
line 475, already emitted as `malformed_affected_prs` in JSON at line 496).

```
malformed_state=present            # unchanged semantics, unchanged consumers
malformed_count=2                  # NEW
malformed_prs=#2237,#2215          # NEW
malformed_summary=...              # unchanged
malformed_remedy=...               # unchanged
```

A reader repairing one line now sees `malformed_count` move and the PR leave
`malformed_prs`. No new computation, no field redefined, no gate weakened.

**P2 — give the standing condition its own daily tick entry.** This addresses
the rc-0 invisibility, and the file already has the exact precedent: the daily
`approvals_unverifiable_against_head` entry exists so a standing condition stays
visible instead of the check being silently inert. A sibling entry gated on
malformed presence, at daily cadence, titled with `{malformed_summary}`, keeps
the lines visible after the binding failures are repaired. **P2 is the more
valuable half and should not be dropped as the cosmetic-sounding one.**

## Bracketing, both directions

Additive fields still need a refusal arm, or the assertion does not
discriminate — a `malformed_count` that is never checked against a *changed*
value is a printed number, not a gate.

- Accept: fixture with 2 malformed lines emits `malformed_count=2` and both PRs.
- Accept: fixture with 1 emits `malformed_count=1` and one PR.
- **Refuse: a fixture whose malformed set shrinks 2 -> 1 must show a changed
  `malformed_count`.** This is the assertion the current field cannot make and
  the only one that proves the defect is fixed.
- Unchanged: `malformed_state` still `present` in all three, and rc still 0 —
  proving the advisory contract was not altered.
- Never plant a real approval or label to test a refusal; use an inert fixture.

## What this does not do

It does not make malformed lines block anything, and it should not. Malformed
syntax is not proof that authority is absent — the #1847 contract — and
tightening it here would be bar-raising through the back door of an
observability fix.
