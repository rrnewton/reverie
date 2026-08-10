# Stale approval labels: measured scope, and why the existing guard fails open

**Transient report.** 2026-08-10, `claude-reviewer-2`, task
`stale-approval-labels-survive-head-changes`. Measurement only — **no label was stripped,
edited, or added.**

---

## Headline

**The scope is small: 1 stale approval across 198 open PRs.** It is 90 minutes old and I caused
it myself. That is the good news and it should be said first.

**The mechanism defect is larger than the count, and it is the finding.** An invalidation guard
already exists and is written correctly — and it is fail-open in two independent ways, while the
consumer that depends on it verifies nothing and says in a comment that it assumes the guard ran.
So the current low count is luck, not control.

---

## Per-PR table — both terms

Population: **198 open PRs** (`rrnewton/hermit` 181, `rrnewton/reverie` 17). Carrying any
`passed-review-*` label: **3**, all in hermit, **zero** in reverie.

| PR | approval label(s) | reviewed-at-SHA | current-head-SHA | verdict |
|---|---|---|---|---|
| **1864** | `passed-review-codex` | `87f62930d3e2` | `b2e44120e984` | **STALE** |
| 1727 | `passed-review-codex` | `49321e67e635` | `49321e67e635` | current |
| 1666 | `passed-review-codex`, `passed-review-claude` | `26d47f007b54` | `26d47f007b54` | current |

Derived **two independent ways, which agree**: (a) the SHA named in the role-tagged review comment
immediately preceding the label event — the policy authority, since "labels are caches"; and
(b) the label event timestamp against the PR's commit timeline.

- **#1864** — comment naming `87f62930` at `22:34:03Z`, label applied `22:34:12Z`, nine seconds
  later, so the binding is unambiguous. Head moved to `b2e44120` at `2026-08-10T02:26:31Z` when I
  pushed fixes for my own review findings. I flagged it in the PR at the time rather than removing
  the label.
- **#1727** — comment naming `49321e67` at `16:30:09Z`, label at `16:30:16Z`; the last commit
  `49321e67` at `16:15:40Z` *precedes* both. Binds to the head that is still current.
- **#1666** — sole commit `26d47f00` at `13:13:49Z`; both labels applied after it. Current.

### The second approval channel, checked because one channel is not scope

**Native GitHub review approvals: 0 of 198.** `reviewDecision` is empty on every open PR in both
repositories — native reviews are not used at all.

That matters for the fix. GitHub's built-in review **already binds to a `commit_id`** and branch
protection can dismiss stale approvals as a *repository setting* rather than a job. The fleet built
a parallel label system that lacks the binding the built-in one has, and then had to reimplement
invalidation as a workflow — which is the thing that fails open.

---

## The guard already exists, and is correct in intent

`.github/workflows/merge-gate.yml`, job `invalidate-local-validation`, lines 151–171, on
`pull_request` / `synchronize`, deletes `locally-validated`, `passed-review-codex`,
`passed-review-claude`, with the reasoning in a comment:

> "the reviewers approved an older diff, so their approval must be re-earned before the
> core-review-protocol job (below) will let a post-facto-human-review PR land again."

This is not a missing feature. It has also demonstrably worked: `github-actions[bot]` unlabeled
`passed-review-claude` on #1864 at `2026-08-08T18:02:03Z`.

## …and it is fail-open in two independent ways

**1. It only runs if a workflow triggers at all — and for my push, none did.**

```
runs at head b2e44120 (my push)   : 0     (queried twice, 20s apart)
runs at head 87f62930 (previous)  : Merge Gate + P0 Demo Gate
PR #1864 status checks at head    : 0
my standalone branch push         : 0 runs
```

Ruled out: Actions enabled (`allowed_actions: all`) and healthy — other SHAs got runs continuously
through 02:25–02:30; identical triggering actor (`rrnewton`, same as the previous head's runs); no
skip-ci token in the commit message.

**Cause not established.** #1864 is `CONFLICTING`/`DIRTY`, which was my leading hypothesis, but
**#1727 is also `DIRTY` and did get runs** — so this is reported as a fact, not a diagnosed cause.
#1727's runs may simply predate its becoming dirty; I did not spend the time to establish that.

**2. Even when the event fires, the job is `runs-on: [self-hosted, gate]`.** An offline or
saturated self-hosted runner means no strip, silently.

In both cases nothing reports that the invalidation was skipped. **A guard that silently does
nothing is this session's pattern — here applied to the thing protecting an authorization.**

## The consumer verifies nothing and says so

`scripts/core-review-protocol-lint.sh` — the merge gate's review check, 109 lines — is:

```sh
has_label passed-review-codex || fail "missing passed-review-codex (codex has not approved the current revision)."
has_label passed-review-claude || fail "missing passed-review-claude (claude has not approved the current revision)."
```

**Zero SHA binding.** And its header comment (line 16) states the labels "are invalidated on every
new push" — the consumer's correctness is *predicated on an assumption about a producer it never
checks*. The failure message even asserts "has not approved the current revision", a claim the
script has no means to evaluate.

**Composed:** producer fails open silently → consumer trusts it unconditionally → the merge gate is
satisfiable by an approval of different code. Together with #2026 making review mandatory, a
mandatory-review rule becomes satisfiable by an approval that means nothing.

## And the existing guard does the thing we were told not to do

It **deletes** the label. The task warns that erasing approval history is how #1874's review
history was permanently lost. So the guard, *when it works*, destroys history. "Invalidate, don't
delete" is not only advice for a human sweeping labels — it is a correction to the shipped
mechanism.

---

## Proposed binding: make validity DERIVABLE, not EVENT-DEPENDENT

The root fault is architectural: **validity currently requires an action to remove an artifact
(fail-open). It should be computed at the moment of use (fail-closed).** Same principle as any
other gate — a mechanism, not a request, and one that cannot be skipped by an event failing to
arrive.

**R1 (primary) — the consumer derives approval validity at read time.**
Teach `core-review-protocol-lint.sh` to accept an approval only when a role-tagged review comment
naming the **exact current head SHA** exists for that lane. The label stays as the human-readable
cache it is already documented to be; the comment is the authority, which is what policy already
says. Nothing is deleted, so no history is lost — a superseded approval simply stops satisfying the
gate, and remains visible forever as the record of what was reviewed and when.

*Why it forces:* it cannot fail open. There is no event to miss and no runner to be offline; if the
head moved, the comparison fails at the moment of the check. It also makes #1864's current state
correct automatically, with no label surgery.

*Bracket, both ways, required before belief:* plant an approval comment naming a **non-current**
SHA → gate must REFUSE; plant one naming the **current** head → gate must PASS. State both counts.
The second half is not optional: a check that refuses everything is as useless as one that accepts
everything, and #2026's review found six evasion mutations accepted 6/6 — that gate was inert and
nobody noticed.

**R2 — make the skip visible.** Whatever happens to R1, the invalidation job should record that it
ran. Today "no run fired" and "ran and found nothing to strip" are indistinguishable from outside,
which is exactly how this survived. An explicit "invalidation evaluated at `<sha>`" marker turns a
silent absence into a checkable one.

**R3 — stop deleting; supersede.** If a label continues to be stripped, the strip should leave a
durable record (a comment naming the label, the SHA it was granted against, and the SHA that
superseded it) so the review history survives the invalidation. Deletion of an approval and loss of
the evidence that it ever existed are two different operations and only the first is wanted.

**R4 (consider) — adopt the native channel.** GitHub's own reviews bind to `commit_id` and branch
protection dismisses stale approvals as a setting, not a job. It is used on 0 of 198 PRs. Adopting
it would replace a fail-open workflow with a platform guarantee. Not proposed as primary because it
is a workflow change for the whole fleet and the label vocabulary encodes lane identity
(claude/codex) that native reviews do not — but the duplication is worth a decision rather than
drift.

---

## What I did not do

- **Stripped nothing.** Report first, as instructed. #1864's stale `passed-review-codex` is left in
  place and flagged in the PR.
- Did not establish *why* no workflow fired for my push. The fact is verified; the cause is open and
  is worth its own task — it is arguably more serious than the label, because it means a head can
  move with no gate evaluation at all.
- Did not audit closed or merged PRs; the question asked was about open PRs.
