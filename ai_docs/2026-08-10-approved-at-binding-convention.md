# `APPROVED-AT`: the one line that makes a review verdict machine-bindable

**Transient report.** 2026-08-10, `claude-reviewer-2`, task
`emit-approved-at-in-every-reviewer-verdict`. Parser and brackets landed with this note;
**no label was added, removed or edited, and no existing approval was retroactively bound.**

---

## What the detector found, and why it is worse than what we went looking for

We went looking for **stale** approvals — an approval bound to the *wrong* head. What
`ci-hub/health/approval_binding.py` reported is worse: on 2026-08-10 **no approval in the
repository could be bound to any head at all**. #1864, #1851, #1727, #1666 — all `UNDETERMINED`,
because reviewers emitted no machine-readable binding.

A stale approval is at least *retirable*: a push moves the head past it and it can be recognised
as superseded. An **unbound** approval can be neither honoured nor retired, because there is
nothing to compare a head against. It is inert authority.

## The convention

One line in every reviewer verdict, at column 0:

```
APPROVED-AT <40-hex>                 # or: APPROVED-AT: <lane> <40-hex>
```

and for a rejection, the mirror form another reviewer introduced on #2026
(comment 5235451519) without being asked:

```
[adversarial-reviewer agent, CLAUDE] [team, machine]
REQUEST CHANGES AT <40-hex>
```

Both approval spellings are accepted, because both are already in the wild — the bare form the
fix was specified as, and the lane-explicit form used on #1851. **One converged parser beats two
competing conventions**, and converging was the point of reading the reference implementation
before writing anything.

### Prefer the lane-explicit spelling, on evidence

Without an explicit lane the parser must derive it from the role tag, and **the role tag does not
reliably encode the lane**: #1864's codex review is tagged `GPT-5 Codex`, #1727's is
`gpt-5.6-sol` — the latter contains neither "codex" nor "claude". The parser fails *closed* on an
ambiguous tag rather than guessing, which means a reviewer tagged that way gets no binding at all.
`APPROVED-AT: codex <sha>` always works; `APPROVED-AT <sha>` works only if the tag is unambiguous.

## A rejection must never bind as a positive

A rejection that names a SHA is otherwise trivially parsed as a verdict *for* that SHA — the
binding defect inverted, with the detector then confidently reporting a bound approval that is
actually a rejection. So **a comment carrying `REQUEST CHANGES AT <sha>` contributes nothing**,
even if it also contains an `APPROVED-AT`-shaped line.

That is not hypothetical. A comment *documenting* this convention necessarily contains the literal
token, and twice on 2026-08-10 a check of mine matched prose about a defect as the defect: a
`/DELETE/` grep firing on a docstring about deletion, and `NOT_EVALUATED` matching inside the
summary line `NOT_EVALUATED: 0`. Rejection wins within a comment, which fails closed.

**This guard was untested when first written and I caught it by mutation, not by reading.**
Disabling reject-wins turned *nothing* red, because the quoted `APPROVED-AT` in the test fixture
sat mid-line where the line-anchored regex never matched. Moving it to column 0 — exactly how it
would appear in a rejection quoting the convention or quoting a superseded approval — made the
guard load-bearing: disabling it now reports `state=BOUND_CURRENT` **on a rejection**.

## Brackets

`ci-hub/health/tests/test_approval_binding_grammar.py`, 9 assertions, offline:

| case | required |
|---|---|
| `APPROVED-AT <sha>` with an unambiguous tag | binds |
| `APPROVED-AT: claude <sha>` | binds |
| no binding line | `UNDETERMINED` |
| `REQUEST CHANGES AT <sha>` | does **not** bind |
| rejection quoting a line-anchored `APPROVED-AT` | does **not** bind |
| bound approval, head then moved | `SUPERSEDED`, not green |
| codex-tagged approval against the claude lane | does not satisfy it |
| codex-tagged approval against the codex lane | satisfies it |
| ambiguous `gpt-5.6-sol` tag, no explicit lane | fails **closed** |

Non-inert: disabling reject-wins turns the rejection case red. The surrounding suite is 16
assertions and mutating the classifier so nothing is ever stale turns 10 of them red.

## It already works end to end

At the time of writing, **#1851 has both lanes bound to its current head** —
`passed-review-claude` and `passed-review-codex`, both `bound-to ef1392494c11`, matching
`head ef1392494c11`. Two reviewers from two families, independently, on a live PR. The remaining
four labels (#1864, #1727, #1666 ×2) are still `UNDETERMINED`.

So the measurement is now **2 of 6 approval labels bind; 4 do not** — both terms, and the 4 are
pre-convention.

## Recommended disposition for the four pre-convention approvals — NOT chosen here

Nobody can establish which head `passed-review-codex` on #1727 referred to. Guessing would
manufacture the exact false authorisation this tool exists to catch, so this is a recommendation
for the owner, not a decision:

1. **Leave them in place and leave them `UNDETERMINED`.** They are not deleted, so the review
   history survives; they simply do not satisfy a binding check. This is the default and costs
   nothing.
2. **Re-verdict on demand.** When one of those PRs next needs to land, its reviewers post a fresh
   verdict with `APPROVED-AT` at the then-current head. The backlog drains as PRs become active
   rather than in a sweep.
3. **Do not backfill.** Reconstructing a binding from label timestamps and comment order is
   exactly the heuristic that produced confidently wrong verdicts when I tried it earlier tonight —
   it read a comment *about* codex's approval as a comment *by* codex.

**Do not strip.** PR #1874's review history was permanently destroyed on 2026-08-09 by careless
label removal. Invalidation and erasure are different operations and only the first is wanted.

## Recommended policy wording, for the owner of `AGENTS.md`

`AGENTS.md` was modified by another agent while this was written, so it is not edited here. The
addition it wants, under *Publishing And Review*:

> Every reviewer verdict carries its binding as its own line at column 0: `APPROVED-AT: <lane>
> <40-hex>` for a pass, `REQUEST CHANGES AT <40-hex>` for a rejection. The label remains a cache;
> this line is what a consumer can check. An approval whose line does not name the current head
> does not satisfy the review gate — it is superseded, not deleted, and stays visible as the record
> of what was reviewed and when.
