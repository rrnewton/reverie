# Malformed approval lines: remediation verdict, and a retraction

2026-08-12. Task `remediate-malformed-approval-lines-causing-binding-failures`. No comment was
posted, no line edited, no label changed, no PR touched. Everything below is read-only
evidence plus a decision not to act.

## Verdict

**No line may be repaired, and none of them needs to be.** Zero of the four `UNDETERMINED`
approval lanes is caused by a malformed line. All four are caused by a **conforming
`CHANGES-REQUESTED-AT` verdict**, three of them at the exact current head, each naming
substantive technical blockers. Every affected lane routes to *fix the blocker, then
re-review* — not to a text repair.

## Retraction

My own audit
([drain-evidence-verification](drain-evidence-verification_20260812.md)) concluded that "the
malformed-line defect is causing the binding failures", and for #2172 specifically that "the
content is correct at the exact current head; only the decoration voids it. Owner action is to
re-emit the line undecorated. A re-review would be wasted work."

**That was wrong, and acting on it would have been the worst available outcome.** #2172's
codex lane already had two *conforming* approvals at the current head; they were revoked
later by a conforming rejection at that same head. Re-emitting an approval there would have
overridden a live rejection — reaching the manufacturing-authority failure by way of the
repair that was supposed to prevent it.

The error was methodological and worth naming: I inferred causation from co-location.
Malformed lines and unbound labels appeared on the same three PRs, so I read the first as
causing the second, without replaying the binder over the full comment chronology. Adjacency
is not causation, and the chronology is where the answer was.

## Method

`ci-hub/health/approval_binding.py` was replayed line-for-line against the complete issue
comment history of all three pull requests: the same `undecorate`, the same `reject_legacy` /
`reject_current` / `approve` / `suspect` patterns, the same "a rejection clears the accumulated
bindings" rule. The replication reproduces the tool's four `UNDETERMINED` verdicts and its two
`BOUND_CURRENT` verdicts exactly, which is what makes the chronology below trustworthy rather
than a re-reading of prose.

## What actually happened, per lane

### #2172 codex — LIVE REJECTION at the current head

| when (UTC) | event |
| --- | --- |
| 09:51 | `CHANGES-REQUESTED-AT: codex fcf201e9…` (clears) |
| 10:09 | **`APPROVED-AT: codex 9d930bd57752…`** — conforming, current head |
| 10:11 | **`APPROVED-AT: codex 9d930bd57752…`** — conforming, current head |
| 10:33 | `**APPROVED-AT \`9d930bd5…\`**, with one unchanged landing` — malformed |
| 22:24 | **`CHANGES-REQUESTED-AT: codex 9d930bd57752…`** — conforming, current head |

The malformed line at 10:33 was **never load-bearing**: conforming approvals already existed
before it. The lane is `UNDETERMINED` because it was rejected at 22:24, substantively — the
comment names the Reverie pin bump breaking the DBT budget guard, with
`ci/run-with-reverie-dbt-budget.sh:28` exiting 2 on mismatch.

### #2176 claude — LIVE REJECTION at the current head

Conforming `APPROVED-AT: claude b3ca4e155098…` at 18:15, then two rejections at the same head
at 01:27 and 01:28 from `hermit-903, role=reviewer`. The 01:27 one was written as a Markdown
heading and was invisible to the old parser; the 01:28 one is the same reviewer re-emitting
their **own** verdict in conforming grammar and saying exactly why. Live rejection.

### #2176 codex — never approved at the current head

Three conforming approvals, all at superseded head `ca696688d8bb`, then
`CHANGES-REQUESTED-AT: codex e211b9d01e63…` at 18:37. The current head `b3ca4e155098` has
never carried a codex approval.

### #2059 codex — LIVE REJECTION at the current head

Five conforming approvals at successively older heads, a rejection at `868d1d9b`, then three
malformed `APPROVED — exact head 86cc94b0…` lines (superseded head), then
`CHANGES-REQUESTED-AT: codex 965d81be4f39…` at 22:25 — conforming, current head, substantive:
the capacity test passes with the pin deleted, so the assertion does not discriminate.

### #2059 claude — no authority of any kind

Its only verdict-shaped lines are three malformed ones at superseded head `86cc94b0`, and the
PR carries no `passed-review-claude` label. #2059 is critical class, so it needs a genuine
claude review at the current head.

## Why I emitted nothing, even where provenance is provable

`lane_shas` takes the lane **from the line text, never from the comment author**, and its
docstring explains why at length: role tags do not encode the lane (`[adversarial-reviewer
agent, GPT-5 Codex]` versus `[adversarial-reviewer agent, gpt-5.6-sol]`), and a prose-matching
version produced confidently wrong verdicts on all three approval-bearing PRs on 2026-08-10.
Refusing to infer is right.

The consequence is structural and worth stating plainly: **any commenter can bind any lane.**
If I post `APPROVED-AT: codex <sha>`, the verifier binds codex. Nothing checks that I am the
codex reviewer, or a reviewer at all. Lane authority rests on the convention that only the
reviewer emits it, and nothing enforces that convention.

So a third party "restoring" someone else's verdict is, to every consumer of that record,
indistinguishable from fabricating it. Provable provenance in the comment history does not
change what the *emitted artifact* asserts. The correct pattern is already on the record in
this very data set: on #2176, `hermit-903` repaired its **own** malformed heading line by
re-emitting it in conforming grammar and explaining the fix. Original author only.

That is the whole reason this task produced a report instead of six comments.

## Observations for the owner

1. **Lane authority is unauthenticated.** See above. This is a deliberate trade — the
   alternative (inferring lane from author) was measured to be worse — but it means the
   `APPROVED-AT` grammar is a convention protected by good behaviour, not a control. Worth an
   explicit decision rather than leaving it implicit.
2. **The two rejections at 22:24 and 22:25 were posted with plain `gh`, not the registered
   boundary**, and their own disclosure notes say so and ask to be read as disclosure rather
   than authentication. Under (1) that is the same trust level as any other verdict line. The
   asymmetry is tolerable in this direction — the binder is deliberately generous about
   *rejections* because recognising one can only withdraw authority, never grant it — but the
   owner should know the rejections that are currently blocking three lanes carry that caveat.
3. **The pending #1847 ruling is moot for these four lanes.** Whether a malformed line at or
   after the binding blocks changes nothing here: under either reading, all four lanes are
   unapproved because a conforming rejection stands. Do not expect the ruling to unblock
   anything in this set.
4. **The malformed lines remain worth cleaning for hygiene**, and only their authors can do
   it. Cleaning them changes no verdict in this set. They are preserved exactly as found.

## Disposition of the six malformed lines

| PR | line (abridged) | author role tag | disposition |
| --- | --- | --- | --- |
| 2172 | `**APPROVED-AT \`9d930bd5…\`**, with one unchanged landing` | `codex-timeouts, role=relay` | **Do not repair.** Not load-bearing (conforming approvals existed), and the lane is now rejected at that head. Author may clean it. |
| 2176 | `APPROVED AT EXACT HEAD \`ca696688…\` — independent Codex adversarial code review.` | `codex-timeouts, role=reviewer` | **Do not repair.** Superseded head; a conforming codex approval at the same head already exists and was later revoked. |
| 2176 | `APPROVED AT EXACT HEAD \`ca696688…\` — Claude-family review.` | `claude-review2157, role=reviewer` | **Do not repair.** Same: superseded head, conforming twin already exists. |
| 2059 | `APPROVED — exact head 86cc94b0…` | `codex-reviewA, adversarial-reviewer` | **Do not repair.** Superseded head; lane subsequently rejected at the current head. |
| 2059 | `APPROVED — F1 metadata follow-up at exact head 86cc94b0…` | `codex-tickgate, adversarial-reviewer` | **Do not repair.** Same. |
| 2059 | `## APPROVED at exact head \`86cc94b0…\`, with two non-blocking notes.` | `adversarial-reviewer agent, claude-opus-5` | **Do not repair.** Superseded head, and this is the claude lane's only verdict-shaped line on a PR with no claude label. Needs a real review, not a transcription. |

## What each PR's owner must actually do

- **#2172** — resolve the codex blockers named at 22:24 (pin bump versus the DBT budget guard),
  then obtain a fresh codex verdict at the new head. The claude lane holds at the current head
  and will be invalidated by the next push, as designed.
- **#2176** — resolve the claude blockers named at 01:28, then obtain fresh verdicts in **both**
  lanes at the new head. Codex has never approved this head.
- **#2059** — resolve the codex blocker named at 22:25 (the capacity assertion does not
  discriminate), then obtain fresh codex **and** claude verdicts; the claude lane has no
  authority at any head.

None of these three is landable today on review grounds, independently of the receipt gap
already recorded for all seventeen non-draft PRs.

## Commands

```sh
with-proxy env FLEET_REAL_GH=$(command -v gh) ./ci-hub/bin/gh api \
  "repos/rrnewton/hermit/issues/<pr>/comments?per_page=100" \
  --jq '.[] | {id:.id, user:.user.login, at:.created_at, body:.body}'
# then replay the pinned grammar from ci-hub/health/approval_binding.py lane_shas():
#   undecorate wrapping markdown; reject_legacy / reject_current (prefix class allowed);
#   approve = ^APPROVED-AT:\s*(claude|codex)\s+([0-9a-f]{40})$  (no prefix class, by design);
#   a rejection for the lane CLEARS all accumulated bindings.
with-proxy env FLEET_REAL_GH=$(command -v gh) python3 ci-hub/health/approval_binding.py --json
```
