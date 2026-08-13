# Independent evidence verdict for the drain queue

2026-08-12. Task `independently-verify-drain-evidence-before-landing`. Read-only. No pull
request was modified, no label touched, no landing performed, no ordering proposed — order is
coord's.

## Verdict in one line

**Zero of the 17 non-draft open pull requests is landable on its evidence today.** None has a
qualifying receipt bound to its exact current head, by any of the three routes that could
supply one. Three of them are blocked only by the CPUID-faulting capability gap and need
routing rather than work; the other fourteen have gaps their owners must close.

## Two numbers in the brief did not survive measurement

**"3 of 6 approval labels fail the superseded binding check" → it is 4 of 6, and none of them
fails for that reason.** `approval_binding.py --json` reports 6 approvals, 2 authoritative,
4 `not_holding`. Not one is in a `SUPERSEDED` state; all four are `UNDETERMINED`, meaning no
parseable `APPROVED-AT` line binds them at all. Every one of the four sits on a pull request
that also has malformed approval lines. The malformed-line defect is not a second problem
running alongside the binding failures — **it is causing them.**

That distinction changes the owner action per pull request, so each malformed line was
resolved against its live head:

| PR | label | line names | current head | what it means |
| --- | --- | --- | --- | --- |
| 2172 | `passed-review-codex` | `9d930bd57752` | `9d930bd57752` | **same head.** The verdict is correct at the exact current head; only the `**…**` decoration voids it. Owner action is to re-emit the line undecorated. A re-review would be wasted work. |
| 2176 | both lanes | `ca696688d8bb` | `b3ca4e155098` | genuinely superseded; needs real re-review at the current head, both lanes |
| 2059 | `passed-review-codex` | `86cc94b0c517` | `965d81be4f39` | genuinely superseded |

Holding: `#2213 passed-review-claude` and `#2172 passed-review-claude`, both `BOUND_CURRENT`.

**"Roughly ten changes" → 19, confirmed by my own measurement, but the PR set is 17 and they
are not the same 19.** `tg sql` gives 19 tasks tagged `implemented`, all `IN_PROGRESS`, none
`OPEN` (30 more are closed with the tag). Task notes map only 12 of those 19 to a hermit PR
number, and at least one — `resolve-task-closure-policy-contradiction` — landed in the
**dev-hermit parent**, not hermit. So the task-side count and the PR-side count answer
different questions. This audit anchors on the live GitHub set, because that is what can
actually land and it does not depend on any author's note being accurate.

## Per-PR verdict

Verdict scheme, stated so it can be checked: **VERIFIED** = review bound at the exact head for
the lane count its class requires, plus a qualifying receipt at that exact head.
**NEEDS-ROUTING** = the receipt gap is caused solely by the CPUID capability limit, so the
work is to run it elsewhere, not to change the code. **MISSING-EVIDENCE** = anything else,
with the gap named. Class is `critical` when the PR carries `post-facto-human-review` (two
review lanes required) and `ordinary` otherwise (one).

| PR | verdict | class | review at exact head | receipt at exact head |
| --- | --- | --- | --- | --- |
| 2222 | **NEEDS-ROUTING** | ordinary | none (0 labels, needs 1) | red: `privileged-cpuid.faulting` only |
| 2218 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 2217 | **NEEDS-ROUTING** | ordinary | none (0 labels, needs 1) | red: `privileged-cpuid.faulting` only |
| 2216 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 2215 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 2213 | MISSING-EVIDENCE | ordinary | **1/1 bound** (claude) | NOT-VALIDATED |
| 2212 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | TRUNCATED; real `scorecard.compatibility` exit 2 |
| 2182 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 2176 | **NEEDS-ROUTING** | critical | 0/2 bound, both UNDETERMINED, 2 malformed | red: `privileged-cpuid.faulting` only |
| 2172 | MISSING-EVIDENCE | critical | 1/2 bound; codex UNDETERMINED, 1 malformed | NOT-VALIDATED |
| 2150 | MISSING-EVIDENCE | critical | none (0 labels, needs 2) | red: real `test.hermit_integration` exit 100 |
| 2129 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 2108 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 2070 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 2059 | MISSING-EVIDENCE | critical | 0/2 bound; codex UNDETERMINED, 3 malformed | NOT-VALIDATED |
| 2012 | MISSING-EVIDENCE | ordinary | none (0 labels, needs 1) | NOT-VALIDATED |
| 1851 | MISSING-EVIDENCE | critical | none (0 labels, needs 2) | NOT-VALIDATED |

Note that #2176 is *both* NEEDS-ROUTING on its receipt and short two bound reviews; routing
alone will not make it landable. It is listed by its receipt cause because that is the part
no owner can fix by working harder.

## The receipt finding, and the three routes I checked before asserting it

`ci-hub validate-status --sha <head>` for all 17 heads: **12 NOT-VALIDATED** (no record at
all), **4 NEEDS-RERUN**, **1 TRUNCATED**. Not one VALIDATED.

Before calling that a universal gap I checked the two ways a pull request could be landable
without a fresh receipt at its head:

1. **A class exemption.** There is none that I can find. Neither
   `.claude/skills/hermit-validation-authority/SKILL.md` nor `ci-hub/landing/README.md` nor
   `ci-hub/qualifying_receipt.py` contains a docs-only or any other exemption. If the owner
   intends one, it is unwritten — itself worth knowing.
2. **Soft-green inheritance.** `SKILL.md` is explicit that soft green is landable when a
   hard-green X is rebased to Y with no conflicts and the rebase is recorded. Only 2 of the 17
   heads have any record in `ignored/rebase-records.jsonl` — and both records already say so
   themselves:

   ```
   PR 2176  landable: false  "no-receipt-at-pushed-head: soft-green + base clears every
                              floor, but NO clean full-validation receipt is bound to the
                              PUSHED head b3ca4e155098"
   PR 2213  landable: false  same, for pushed head 6cfe77adc3b2
   ```

   That is an independent mechanism reaching the same verdict I did, which is the strongest
   corroboration available here.

## The red receipts, classified by their actual failing gate

The `validate-status` summary string is not the evidence; the ledger gate list is. For each
red head I read the genuine failing gate rather than the summary:

| PR | head | host | genuine failing gate | collateral aborts | class |
| --- | --- | --- | --- | --- | --- |
| 2176 | `b3ca4e15` | devbig030 | `privileged-cpuid.faulting` exit 101 | 14 | capability |
| 2217 | `c2fbd504` | devbig030 | `privileged-cpuid.faulting` exit 101 | 14 | capability |
| 2222 | `ef016b0d` | devbig030 | `privileged-cpuid.faulting` exit 101 | 14 | capability |
| 2150 | `dd23a243` | **devbig014** | `test.hermit_integration` exit 100 | 3 | **real** |
| 2212 | `cb581044` | devbig030 | `scorecard.compatibility` exit 2 | 1 | **real** |

Every one of these records `failures=1`. The 14 other non-passing gates in each capability
case are `ABORTED (eager-exit after another step failed)` — collateral from a 0.1-second
failure, not additional defects.

**A trap worth naming before the drain, because following the tool costs a validate slot per
pull request:** `validate-status` reports the three capability reds as
`NEEDS-RERUN … rerun solo at -j 4`. **That advice is wrong on this box.** A solo rerun on
devbig030 fails identically in about 0.1 s, because the kernel cannot support CPUID faulting
at all ([host-capability note](host-capability-cpuid-faulting_20260812.md)). Those three must
be validated on devbig014.

By contrast #2150's red is on devbig014, which *does* have the capability, and its failing
gate is an integration test. That one is a real failure and rerunning it elsewhere will not
help.

## What each owner must close

- **#2222, #2217** — one validate on devbig014. Nothing else known missing except review
  (ordinary class, one lane).
- **#2176** — one validate on devbig014 **and** genuine re-review in both lanes at head
  `b3ca4e155098`; its existing approvals name a superseded head.
- **#2172** — re-emit the codex verdict line undecorated (its content is already correct at
  the current head), then one validate. Claude lane already holds.
- **#2059** — re-review in the codex lane at head `965d81be4f39`; existing line names a
  superseded head. Then a validate.
- **#2213** — review holds; needs only a validate at head `6cfe77adc3b2`.
- **#2150** — fix or explain `test.hermit_integration` exit 100, then two-lane review.
- **#2212** — fix or explain `scorecard.compatibility` exit 2.
- **#2218, #2216, #2215, #2182, #2129, #2108, #2070, #2012** — no review label and no receipt;
  both are needed (one lane each).
- **#1851** — critical class with no review labels at all; two lanes plus a receipt.

## Scope and limits

- This is an evidence audit, not a code review, and not an ordering proposal. A gap named here
  is a statement of what is missing, not a veto.
- Ten draft pull requests were not audited; they cannot land while draft.
- I did not verify that the two `BOUND_CURRENT` approvals are *substantively* adequate
  reviews — only that they are bound to the exact current head, which is what the binding
  check establishes.
- The malformed-line ruling is the owner's. The lines are flagged here with which ones name a
  current head and which name a superseded one, so the ruling can be applied per case rather
  than uniformly.
- Both `approval_binding.py` and `validate-status` were run at approximately 2026-08-12 19:55
  to 20:10 PDT. Heads move; every SHA in this report should be re-checked at land time.

## Commands

```sh
tg sql "SELECT local_id,status FROM tasks WHERE tags LIKE '%implemented%' AND status!='CLOSED'"
with-proxy env FLEET_REAL_GH=$(command -v gh) python3 ci-hub/health/approval_binding.py --json
with-proxy env FLEET_REAL_GH=$(command -v gh) ./ci-hub/bin/gh pr list --repo rrnewton/hermit \
  --state open --limit 100 --json number,title,isDraft,mergeable,headRefOid,labels
./ci-hub/ci-hub validate-status --sha <head>          # once per head
# genuine vs collateral gate failures, per head, from ledger/hermit/<machine>/2026-08.jsonl:
#   legacy_row.gates[] where result != pass and reason does not contain "ABORTED"
grep -n "no-receipt-at-pushed-head" ignored/rebase-records.jsonl
```
