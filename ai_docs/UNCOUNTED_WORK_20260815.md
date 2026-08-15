# Work the scorecard does not count

Available today, needs no devbig014, moves `CELLS LANDED` by zero.

Produced 2026-08-15 by hermit-coord (devbig030). Read-only survey; nothing was
enabled, qualified, or edited to produce it.

## WHY THIS EXISTS

The session recorded that "the metric and the available work have come apart".
That names a gap without listing what is in it, and **nobody can act on a gap
whose contents they cannot see.** With no list, the only visible option is to
wait for devbig014. This is the input to deciding whether the metric or the
prioritisation is wrong; it is not an argument for either.

## THE ONE RESULT WORTH LEADING WITH

**The 47 needs-judgement cells now have names.** Published this session as the
49-cell complement in `ai_docs/LITEINST_MECHANICAL_CELLS_20260815.tsv`. Before
this session they existed only as a number in one header line, unauditable by
anyone including its author. **They are now a workable list and nobody has worked
it.** That is the difference between a backlog someone can pick up and a quantity
someone can only quote — and it is the same transformation item 4 below still
needs.

## THE LIST

Every figure is marked MEASURED (derived here, method stated) or RELAYED (another
agent's, not independently reproduced). The last column is the one that matters:
some of this is blocked on judgement and some is merely unrewarded, and they need
different answers.

| # | Item | Size | Why the scorecard does not count it | Blocked on |
|---|---|---|---|---|
| 1 | liteinst cells eligible but not in #2279 | **71** by my predicate (85 − 14) · MEASURED; **72** by sabre's (86 − 14) · RELAYED — one-cell gap, both published | Qualification is not landing, and they cannot fold into #2279 until #2276/#2277 land | Judgement — see caveat |
| 2 | `backend-limitation` disabled slots | **55** · MEASURED | No cell work can fix them; they are capability facts | **Time** |
| 3 | Mixed-oracle DAG nodes | **13** of 60 · RELAYED (devbig014) | Node health is not a cell | Time, then per-node judgement |
| 4 | DAG nodes never examined for self-certification | **~50** of 60 · RELAYED, unsized | Same | **Time** |
| 5 | Fixtures returning 0 unconditionally | **~85** RELAYED (hermit-sabre); own screen **32 of 39** MEASURED | Repair makes a green able to go red; the count does not move | Time to repair; judgement for the assert after |
| 6 | DBT adapter publishes no comparison | 1 defect · MEASURED | Fixing it changes evidence tier, not cell count | **Time** (code fix) |
| 7 | `ledger_spool_backlog` emits intermittently | 1 defect, 3-tick evidence · MEASURED | Monitor health is not a cell | **Time** to diagnose |
| 8 | No scheduler runs the ledger publisher | exposure p50 47.9h, max 167.3h · MEASURED | Durability is not a cell | **Owner** |
| 9 | Trigger-based re-promotion | unsized | Same | **Owner** |
| 10 | `bitwise_parity` validator rule | 2 cells today · MEASURED | A ratchet; refuses nothing now | **Owner** |
| 11 | Malformed approval lines | 2 left · MEASURED | Review hygiene | #2237 time; #2215 nothing gained |
| 12 | Branch protection on unprotected `main` | — | Safety is not a cell | **Owner** |
| 13 | Content-conditioned landing floor | — | Would unblock the stack | **Owner** — recommended against |
| 14 | Not-determinable non-C cells | **4** · MEASURED | Currently classified unknown | **Time** (3 need the cell fixture dir built, 1 needs rust-script) |
| 15 | Reconcile the two eligibility predicates | **1 cell** · MEASURED (was ~7; sabre corrected 78→86) | Not a cell | Judgement — gates item 1 |

**Owner decisions: 8, 9, 10, 12, 13 — five, and 9 and 10 want the same trigger,
so four conversations.**
**Merely unrewarded, needs only time: 2, 3, 4, 6, 7, 14, and the repair half of 5.**
**Genuinely blocked on judgement: 1 and 15.**

## TWO FLAGS BEFORE ANYONE PLANS FROM THIS

**Item 4's `~50` is a relayed round number with no membership list.** It is the
same shape as the `47` before it was named: a quantity that can be quoted and not
worked. **It should be enumerated before it is scheduled** — and the fix is the
one already demonstrated on the 47, so this flag predicts its own remedy.

**Item 5's `~85` is hermit-sabre's; my own screen of a 39-cell slice found 32
not-exit-gated.** Consistent, but **not the same population**, and the two must
not be added or compared without saying which is which.

**Item 1's caveat — PARTIALLY RESOLVED, and the residue is ONE cell.**

hermit-sabre re-derived and corrected itself from 78 to 86 after finding a filter
leak, **so the live gap against my 85 is one cell, not seven.** Both figures are
over the same 147 disabled `verify/liteinst` slots.

**The candidate cause was tested here and is REFUTED.** The hypothesis was that
my predicate counts `backend-parity-c/cpuid-probe` while sabre's excludes it as
host-blocked. It is in my 85 — but removing it gives **84**, which *widens* the
gap to two rather than closing it. sabre has one cell MORE than me, so an
exclusion on my side cannot be the explanation.

**My likeliest over-exclusion was tested and is also refuted.** My threads
pattern includes `#include <pthread.h>`, which would wrongly exclude a fixture
that includes the header without creating a thread. **Zero cells are caught only
by the header:** every one of my 14 exclusions has a substantive call.

**MY PREDICATE, FULLY ACCOUNTED SO THE REMAINING CELL CAN BE FOUND BY DIFF:**
147 slots − 48 non-C-or-missing = 99 C-backed − 14 disqualified = **85**. The 14,
with the call that triggered each: `no-new-privs-refusal` exec · `pipe-ipc` fork ·
`short-io-split-identity` fork · `fork-exec-pipeline` fork+exec ·
`random-sources` pthread_create · `lock-free` pthread_create · `pid-tid`
pthread_create+fork · `pid-tid-identity` pthread_create+fork+execl ·
`pipe-prefill` fork · `producer-consumer` pthread_create · `signal-order`
pthread_create · `order-violation` pthread_create ·
`shm-coherency-identity` fork · `clock-exec-continuity` exec.

**BOTH NUMBERS STAND WITH THEIR PREDICATES ATTACHED. NEITHER IS AVERAGED AND
NEITHER IS CHOSEN** — the same treatment as thirteen-and-fourteen, and for the
same reason: a one-cell disagreement between two defensible predicates is a
definition to publish, not an error to correct. **I cannot say which of us is
right without sabre's cell list**, and I have made mine diffable so that one
comparison settles it.

**So item 1 is 71 by my predicate (85 − 14 in #2279) and 72 by sabre's (86 − 14).
It remains an upper bound under either.** Item 15 is now a one-cell diff rather
than a survey.

## NO EFFORT ESTIMATES, DELIBERATELY

There are no hour figures here and the omission is not an oversight. **I have no
basis for them.** This record spent the session retracting numbers that were
produced confidently and did not survive derivation, and a fabricated estimate
would be exactly that failure in a new place. Sizes are given in counts where a
count exists and marked unsized where one does not.

<!-- PROVENANCE: promoted 2026-08-15 by hermit-coord at a stopping point -- this artifact is
     FINISHED, not live. Source figures and their derivations:
     partition source   ai_docs/DISABLED_SLOT_PARTITION_20260815.tsv  sha256 f17ff18bad356bf5148edd5399990d0266fc9e42c227dde8a7a8f1ae27ae88d5
     mechanical cells   ai_docs/LITEINST_MECHANICAL_CELLS_20260815.tsv sha256 1e4413c4308d3b044c7300af654d6ec9b04a40cd9093e0746c3d4a0eb8193186
     dbt probe          ai_docs/DBT_ADAPTER_PROBE_20260815.md          sha256 96b6b4e4324563589e63a7fa698d17c0aca93f8dc6e82b3e4059a0884c9175d6
     session report     ignored/overnight-report-2026-08-15.md  sha256 035d329ff131db3bd0145ef0fa0a7988589cfa0d8c7693bcf5096d31e1c3e840
     DAG denominator    hermit/ci/dag/portable.json 51 + privileged.json 9 = 60 nodes, MEASURED
     If any hash above no longer matches, a figure here may be stale. A snapshot with no
     embedded source hash is indistinguishable from a current one. -->
