# TaskGraph nonterminal census, 2026-08-07

Read-only census and classification of every OPEN / IN_PROGRESS / BACKLOG task in the
`hermit` TaskGraph, for task `taskgraph-hygiene-full-open-census`. **Nothing was closed,
demoted, re-owned or edited by this census.**

**2026-08-27 update:** the task name preserved in the historical census refers
to infrastructure that has since been deleted; it is not current operating
guidance.

## Denominator, and why it needs a transaction

`sqlite3` opened read-only (`file:~/.tg/hermit.db?mode=ro`) with every query inside one
`BEGIN`, so all counting methods see the same snapshot.

Without the transaction the denominator drifts *while you count*: the first attempt read 606
by aggregate and 605 by cursor walk, purely because a task closed between two statements. The
graph then moved 605 → 612 → 616 → 618 over about thirty minutes. **A census is only
meaningful as of an instant**, and a fixed number quoted without one — the dispatch's 573, for
example — is already wrong when it is read.

The classification below is pinned to the **616** snapshot.

## Three independent counts, required to agree

| method | how | result |
| --- | --- | --- |
| A aggregate | `SELECT status,COUNT(*) … GROUP BY status` | BACKLOG 325 + IN_PROGRESS 209 + OPEN 82 = **616** |
| B cursor walk | `WHERE local_id > :cursor ORDER BY local_id LIMIT 100`, 7 pages | **616** rows, 616 distinct, 0 duplicates, strictly increasing |
| C full fetch | unordered `SELECT local_id` | **616** |

`A == B == C`, and `set(B) == set(C)` — set equality is what rules out gaps *and* extras; the
strictly-increasing walk rules out repeats and skips within the pagination. No
`LIMIT K`-from-the-head was used anywhere.

## Category counts

Mutually exclusive, assigned by strict precedence in the order listed. The sum equals the
denominator exactly and every task is classified exactly once (verified set-equal to the walk).

| category | count | share |
| --- | ---: | ---: |
| active | 18 | 2.9% |
| ready | 232 | 37.7% |
| blocked | 102 | 16.6% |
| implemented-awaiting-land | 203 | 33.0% |
| owner-decision | 34 | 5.5% |
| stale-premise | 9 | 1.5% |
| subsumed/duplicate | 1 | 0.2% |
| obsolete | 11 | 1.8% |
| orphaned | 6 | 1.0% |
| malformed | 0 | 0.0% |
| **TOTAL** | **616** | **100%** |

## The finding

**203 of 616 (33%) are `implemented-awaiting-land`.** That is the burial mechanism, and it also
explains the shape of `IN_PROGRESS`: 209 rows carry the status but only 20 have an owner,
because 185 of the ownerless ones are *finished* work correctly parked at `in_progress` by
policy until it lands. Only **4** are IN_PROGRESS, unowned and not implemented.

So the graph is not full of abandoned running work; it is full of completed work waiting on the
landing pipeline. `IN_PROGRESS` is therefore **not a measure of activity**, and any view that
treats it as one overstates the fleet by an order of magnitude. The real active set is 18.

The same shape holds at the top: of 224 P0 tasks, 86 are implemented-awaiting-land and 15 are
active.

## IN_PROGRESS split by ownership and liveness

Snapshot `IN_PROGRESS = 210`. Live-agent roster from orc's isolated tmux socket (26 panes, every
one `dead=0` and process-alive). "Current" = the owning agent's most recently modified
non-implemented IN_PROGRESS task.

| bucket | count |
| --- | ---: |
| active-current | 19 |
| implemented-awaiting-land | 191 |
| stranded-unowned | 0 |
| dead-owner | 0 |
| live-owned-not-current | 0 |
| **TOTAL** | **210** |

Three buckets are zero, and that is a real result rather than a measurement failure: every
non-implemented IN_PROGRESS task had an owner, every owner had a live pane, and every live agent
owned exactly one.

**The stranding was real but hiding inside the implemented bucket.** Of the 191
implemented-awaiting-land, **190 had no owner** and exactly one had a live owner — P0 83, P1 79,
P2 28. "Stranded" reads 0 only because `implemented` is decided first in precedence. The
stranded population was 190 finished *landing obligations* with nobody assigned to land them.

## Two classifier defects found and fixed during the census

Recorded because the loose versions produced confident, wrong closure lists, and a census's
whole value is that its lists can be acted on.

1. **The 6 "orphaned" are project containers, not orphans.** The first pass defined orphaned as
   "no outgoing `blocks` edge" — which is exactly the shape of a *goal root*. It flagged
   `__adhoc__` (776 in-project children), `dev_hermit_parent` (25), `reproducible_builds` (56),
   `reverie_backends` (33), `qemu_linux` (11), `test_power_to_weight` (5). Closing those would
   detach the entire graph. Corrected to "no blocks edge in **either** direction" and reported
   split by container-vs-true-orphan. **None of the six is a closure candidate.**
2. **The `obsolete` regex was mostly false positives.** Loose matching on
   `superseded|obsolete|no longer|moot` over notes returned 33. Sampling six showed five were
   incidental prose — "left on a superseded reverie commit", "a script which no longer exists",
   "reap obsolete process groups", "batch-bump-43 was moot", and a quotation of the closure
   policy itself. Tightened to statements *about this task* → 11. Same tightening on
   `stale-premise`: 12 → 9.

## Closure and reprioritisation candidates

Every one needs a human decision. This census closed nothing.

- **subsumed/duplicate (1):** `e2e_perl_hash_order_2` → survivor `e2e_perl_hash_order`.
- **near-duplicate candidates** (token-set ≥ 0.75, not auto-classified): only two pairs clear the
  bar — the pair above, and `dbi-close-remaining-cells` ~~ `liteinst-close-remaining-cells`
  (probably legitimately parallel per-backend work). **Title duplication is not what buries the
  queue.**
- **obsolete (11)** and **stale-premise (9)**: listed in the raw output below.

**The highest-leverage move is none of these.** 203 implemented-awaiting-land dwarfs the 21
obsolete + stale-premise + duplicate rows combined. Closing those 21 is tidying; landing the 203
removes roughly ten times more.

## Protected

32 tasks tagged `release:0.3`, and all 18 `active` owner-held tasks, were excluded from every
candidate list.

## Addendum: 193 tasks were closed mid-census, and the closures are not backed by landing

Between 03:19Z and 03:22Z, while this artifact was being written:

| | before | after |
| --- | ---: | ---: |
| nonterminal | 616 | 428 |
| IN_PROGRESS | 210 | 18 |
| CLOSED | 4079 | 4277 |
| implemented-and-nonterminal | 191 | 0 |

193 tasks moved to CLOSED in that three-minute window — every one of the 190 unowned
implemented-awaiting-land tasks enumerated above among them.

55 of the 193 cite a PR URL in their notes. Fourteen were sampled alphabetically (arbitrary with
respect to merge state) and queried live: **14 of 14 are unlanded, zero merged**, with
`mergedAt = null` and `mergeCommit = null` on every one — hermit #1736, #1742, #1751, #1758,
#1719, #1778, #1779, #1750 (×2), #1730, #1832, #1147 (closed, never merged), and reverie #394
(×2).

The clearest case, verifiable first-hand:

- `dbi_detlog_stack_hashes` → CLOSED; its PR, reverie #394, is OPEN and still **draft**.
- `fix-reverie-pr-394-clone-rearm-over-scrub` → CLOSED; that task exists *because* #394's head
  erases a guest-owned stack marker after clone in 4/4 DBI runs while ptrace preserves it.
- `rereview-reverie-pr-394-after-clone-rearm-fix` → still IN_PROGRESS.

The graph now asserts that the work is done, the fix for the broken work is done, and someone
should re-review the fix that is done.

This is the phantom closure `AGENTS.md` names as "a recurring, expensive failure mode": closure
was taken from the `implemented` **tag** rather than from landed ancestry, and `implemented`
means finished-**and-awaiting-landing** — precisely not landed. Policy is explicit ("the task
stays IMPLEMENTED until the PR lands on `main`"), and `./ci-hub/bin/close-task` freshly verifies
ancestry and returns REFUSED or UNVERIFIABLE rather than closing. A gateway-verified sweep could
not have produced these fourteen.

**Consequence for reading this artifact:** the sweep did not drain the landing backlog, it
deleted the record of it. The post-sweep 428/18 looks healthier than the 616/210 above, but
nothing landed between the two reads. Suggested remediation, not acted on here: re-run the
gateway ancestry check across the 193 and re-open anything returning REFUSED/UNVERIFIABLE; and
close on `mergeCommit.oid` ancestry against freshly-fetched main rather than on a tag.

## Raw output

Full reconciliation, per-category member lists, the P0 active/ready head, and the IN_PROGRESS
split, as generated:

```
### METHOD RECONCILIATION (all inside ONE read transaction)
A aggregate by status {'BACKLOG': 325, 'IN_PROGRESS': 209, 'OPEN': 82} sum=616
B cursor walk by local_id: 616 rows in 7 pages; distinct 616; duplicates 0; strictly increasing True
C unordered full fetch: 616
A==B==C: True   B set == C set (no gaps, no extras): True

### CATEGORY COUNTS (mutually exclusive; strict precedence in that order)
  active                         18
  ready                         232
  blocked                       102
  implemented-awaiting-land     203
  owner-decision                 34
  stale-premise                   9
  subsumed/duplicate              1
  obsolete                       11
  orphaned                        6
  malformed                       0
  TOTAL                         616  denominator 616  SUM==DENOMINATOR True
  every task classified exactly once: True

### ORPHANED, split (structural containers are NOT work items)
  __adhoc__                in_project_children= 776  project/vision container
  dev_hermit_parent        in_project_children=  25  project/vision container
  qemu_linux               in_project_children=  11  project/vision container
  reproducible_builds      in_project_children=  56  project/vision container
  reverie_backends         in_project_children=  33  project/vision container
  test_power_to_weight     in_project_children=   5  project/vision container

### CROSS-CUTTING (overlaps by construction; NOT part of the sum)
  IN_PROGRESS, no owner, not implemented : 4  <- claims to be running; nobody is running it
  tagged release:0.3 (PROTECTED)         : 32
  IN_PROGRESS total / with owner         : 209 / 20

### P0/P1 BY CATEGORY
  P0 total 224: implemented-awaiting-land=86, blocked=58, ready=42, active=15, owner-decision=10, obsolete=7, stale-premise=5, subsumed/duplicate=1
  P1 total 226: implemented-awaiting-land=86, ready=79, blocked=30, owner-decision=22, obsolete=4, stale-premise=3, active=2

### NEAR-DUPLICATE CANDIDATES (token-set >=0.75; NOT auto-classified, needs confirmation)
  1.0  e2e_perl_hash_order  ~~  e2e_perl_hash_order_2
  0.75  dbi-close-remaining-cells  ~~  liteinst-close-remaining-cells
  (2 candidate pairs total)

### STALE-PREMISE MEMBERS
  detcore-wait4-nondelivery-sigkilled-child
  determinism_stress_order_violation_2
  drain-implemented-to-landed
  matrix-tsv-schema-consolidation
  merge-gate-trusts-label-presence-not-the-ledger
  staging-hermit-33-free-merges
  unwire-executed-tests-from-remaining-consumers
  validate-dag-runner-p0-umbrella
  validate_sh_retry_classifier

### OBSOLETE MEMBERS
  ci-hub-green-time-metric
  data-driven-pr-planning-next-10-with-soft-green-inheritance
  dbi_branch_count_preemption
  demo5-fix-vtime-skew-poller-livelock
  drain-the-49-no-record-prs-once-the-cargo-pin-lands
  full-validate-green-proof-with-1520-1521
  impl-coord-heartbeat
  impl-dbi-dynamorio-prototype
  mass-parallel-rebase-stale-base
  triage-and-review-62-abandoned-draft-prs
  validate_hermit_prs_1470

### SUBSUMED/DUPLICATE MEMBERS
  e2e_perl_hash_order_2 -> survivor e2e_perl_hash_order

### MALFORMED MEMBERS

--- IN_PROGRESS SPLIT ---
IN_PROGRESS denominator: 210   live agents with a pane: 26

=== IN_PROGRESS SPLIT (mutually exclusive; implemented decided first) ===
  active-current                 19
  implemented-awaiting-land     191
  stranded-unowned                0
  dead-owner                      0
  live-owned-not-current          0
  TOTAL                         210   SUM==DENOMINATOR True
  every IN_PROGRESS task appears exactly once: True

=== CROSS-TAB of the 203 implemented, by owner liveness (who can actually land them) ===
  no owner      190
  live owner      1

=== active-current: the real running set ===
  hermit-cc            P1  fix-speculative-land-obligations-workflow-timeout
  hermit-coord         P0  coordinate-release-0.3-overnight-critical-path
  hermit-w10           P0  test-team-machine-ledger-invariants
  hermit-w11           P2  surface-substantial-taskgraph-work-to-github-20260807t03
  hermit-w13           P2  status_log_records_a
  hermit-w14           P0  capture-immediate-pre-tightening-scorecard-baseline
  hermit-w15           P0  fix-parent-cihub-bounded-operations-shard-main-red
  hermit-w17           P0  guard-heavy-fbsource-buck-runs-from-hostwide-io-stall
  hermit-w18           P0  prototype-github-checks-index-for-local-validate
  hermit-w2            P0  audit-patching-backend-arc-current-main-and-prs
  hermit-w20           P0  land-scorecard-tier-correction
  hermit-w21           P0  prototype-github-commit-status-index-for-local-validate
  hermit-w23           P1  liteinst_detlog_heap_and
  hermit-w24           P0  audit-vdso-treatment-consistency-across-backends
  hermit-w25           P0  rereview-reverie-pr-394-after-clone-rearm-fix
  hermit-w5            P0  prototype-github-commit-comment-index-for-local-validate
  hermit-w6            P0  taskgraph-hygiene-full-open-census
  hermit-w7            P0  fix-scorecard-tier-correction-review-blockers
  hermit-w9            P0  maintain-dev-hermit-main-freshness-during-release

=== live-owned-not-current: live agent, but NOT the task it is on (candidates to re-own or park) ===

=== dead-owner: owner has NO live pane (STRANDED; candidates to re-own) ===

=== stranded-unowned: IN_PROGRESS with no owner at all ===
```
