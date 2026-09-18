# scorecard — handoff, 2026-09-16

Interpretation only. Numbers, commands and verdicts are already in task notes and
on pull requests; this is the reading of them, which is what dies with the lane.

Slot: `worktrees/slots/scorecard-publish-sept14`. Status file: `/home/newton/temp/scorecard-status.md`.

---

## 1. The two owner questions — answer these from here, they are planning on them

### Where the cell-level ledger lives

`dev-hermit/series/<team>/<host>/<YYYY>-<MM>.jsonl`. **He is right that it is at the
dev-hermit level.** 84 MB, three shards, tracked in git. Run-level rows are the
neighbouring `dev-hermit/ledger/...` (92 MB). **Neither exists in hermit.**

The distinction that matters and is easy to lose: producers write only to
`dev-hermit/ignored/ci-hub/series-spool/`, never to the shards; readers union the
spool with the shards. So "in the ledger" has two meanings and a publisher job
promotes spool → shards. It ran automatically within the hour on both of my
appends today, so promotion is not a manual step in practice.

**On his plan (separate repo, submoduled, nightly builds from accumulated data):
nothing prevents dev-hermit for the PUBLISHED rows — they are already there and
they are cheap, 1,240 bytes per row.** What does not fit is raw campaign evidence:
one 149-cell campaign is 3.80 GB across 22,882 files, of which `verify-logs/` is
94.93%. His instinct to separate something resettable is right, but the thing to
separate is the raw evidence, not the published rows.

Two facts that genuinely bear on it, as opposed to general caution:
- `dev-hermit/.git` is **52 GB already**. That is the binding constraint, not the JSONL.
- dev-hermit is **main-only with direct pushes and nothing server-side can check
  them** (`CLAUDE.md:133`). So a submodule bump is a direct push to main and the
  size question cannot be deferred to a PR gate. This is the one I would raise
  with him before he commits to submodule mechanics.

### What created the compatibility-website release

**I did, by hand, with `gh release create`.** Nothing in either repository creates
releases; there is no automation. It is not stray, though: `docs.yml` publishes by
downloading `releases/tags/<tag>` then `releases/assets/<id>`, so **the publish
path requires one release per artifact** and cannot work without it. Every
artifact identity I built today has one. If that surprises him, the surprising
part is that a required step is manual, not that the release exists.

### Backup

**No. There is none.** `design-durable-backup-for-cell-observation-ledger` is
CLOSED but as research only — its own closure says no cloud resource was created,
nothing was uploaded, nothing changed. Today the ledger's durability is "it is
committed in dev-hermit and dev-hermit exists." I proved the gap the hard way:
the 149-campaign's 3.7 GB raw tree was reclaimed from `ignored/` and I had to
re-run the campaign to recover ingestible rows.

---

## 2. The ordering fix — why it is two changes and why one would look like both

Committed `b0c4e086a`, also on branch `scorecard/newest-first-ordering`. 107 tests pass.

**The split is the whole point.** Three instances, two mechanisms:

- `recorded-evidence` (cell detail) **has no sort control at all.** Its order is
  literally the order the generator emits, which was `model.series_by_cell`, built
  by appending rows in captured input order. The ledger is append-only, so that
  was chronological ascending. Fixing it means **changing emitted data order**.
- `recorded-cells` and `recorded-not-selected` (run detail) are governed by a
  **declared `default_direction`**. Fixing them means changing a parameter.

A change touching only one mechanism fixes some pages and leaves others ascending
**while appearing to address the directive**, because the word "ordering" covers
both. That is why they had to land together. Also: those two run-page lists come
from **one** helper call, so counting call sites undercounts — six calls emit
seven lists.

**Why `emitted_at` and not `observed_at`.** Both exist on those rows. The page
renders `emitted_at` under the label **"Recorded"** — it is the date a reader can
actually see. A reader asking for newest-first means *that* date. Sorting on
`observed_at` would have produced a list that is genuinely descending and looks
correct while ordering on a field the page never shows, which is the worst
outcome because nothing about it appears wrong. Tiebreak is `event_id`, because
the artifact identity is a digest over emitted bytes and a tie would let identical
input produce two different artifacts.

**The lists I judged NOT to be time orders, and the grounds:**
- `projection-audit` — sorts on schema (a version number) and on counts.
- `tests`, `custom-commands` — "manifest order" is **declaration order in the
  manifest file**, not time. This is the one most likely to be mistaken for a
  time order by a later reader.
- `selected-groups` — mode/backend/category hierarchy.
- `published-cells` — mode and backend within a test.
- `omitted-cells` — the omissions list in its published order.

**Already correct, verified rather than assumed:** `validation-runs`
(`default_sort="started"`, `default_direction="desc"`) and `selection-records`
(sorts `observed_at`, returns the **negated** comparison).

**Open residual, with the owner:** `validation-runs` offers `history` (sort source
`"ordinal"`) and `run` (a run id) as *selectable* sorts. Both are time orders
wearing another name and both ascend when chosen. The **default** is correct, so
the directive as written is satisfied. Whether newest-first should govern a sort
the reader explicitly picks is his call.

---

## 3. The republish — the thing everyone keeps misreading

**The site stamps the MEASUREMENT date, not the publish date** (`render_page.py`
reads `historical["finished_at"]`). Today's publish ran fine and the site still
reads 2026-09-11 because **no full-path validate has finished since then**. The
site is honest and starved, not broken. Do not go looking for a stale cache, a
stale ledger or an old manifest — I ruled out all three.

**The stamp cannot move by republishing.** Only a full validate moves it. Not by
hand, not by workflow. That ceiling is now reachable because the main-red root
cause landed.

**Two traps that will bite the next person:**

1. **`build-historical` is `published_only: true`, pinned to `--parent-commit`.**
   I copied a parent commit from a previous build; it was 54 commits stale. The
   build read data as of that commit, found nothing changed, and returned
   `reused: true` with the same identity — **it reported success and rebuilt
   nothing.** A nightly must resolve the parent commit fresh every run. Hard-coding
   it reproduces exactly the failure this whole effort exists to prevent.
2. **`--record-id` and `--run-id` are explicit arguments.** Copying my command
   verbatim after the next validate passes would rebuild the 09-11 snapshot again
   and look entirely successful. They must be derived.

**Ingested ≠ counted.** I appended 190 rows today and the site's physical-row
counts rose by exactly 190 — and **not one of the 30 red cells changed its
displayed result.** The pressure run executed at hermit `68fabc2f`, which is **not
an ancestor of hermit main**, and the reducer qualifies rows by
`current_main_ancestry`. A row at a non-ancestor commit is admitted and counted
but cannot set a cell's latest result. To refresh what the site *shows*, re-run
those cells at a commit on main's ancestry — 6m24s for 30 cells at 3 repetitions,
so cheap to redo correctly rather than work around.

---

## 4. Judgements about the data that took a day to earn

**The classifier errs in BOTH directions.** Run 1819 labelled 17 missing-result
rows as `product_failure`; my campaign labelled real crashes as
`infrastructure-error`. So `failure_class` cannot separate infrastructure from
product, and **Red must be defined by result class** (`determinism-failure` or
`crash-error`) rather than by `failure_class`. That is the outstanding owner
ruling.

**My "six misclassified cells" list is RETRACTED and must not be revived.** The
rerun measured all six at 20-of-20 `crash-error` — genuine product failures that
belong *inside* Red. I had read absence of evidence as absence of a product
failure. The rule survives; only my example was wrong. One of them,
`c-programs/resource-determinism/verify/ptrace`, already had `crash-error` rows
from 2026-08-30 under a different campaign, so it is a long-standing crash.

**Carrying `verification_report_sha256` is necessary but NOT sufficient for
ingest.** Today's 30-cell run had it on all 90 result files and still only 12 of
30 cells converted; 17 failed on `"framework result has no supporting canonical
typed attempt"` (`series.py:2639`). That refusal appeared in both campaigns and is
a real producer/checker disagreement, not box time.

**62 cells produce no comparison evidence across two independent runs.** That is a
stable inability, not lost data. A third run will not recover them. But 30 cells
*flipped* between the two runs, which is genuine harness instability and a better
reason to look at those cells than the ingest backlog was.

---

## 5. Things I got wrong, so they are not rediscovered as novel

- **A control that silently does nothing reports everything green.** I measured a
  test control with `git stash -- <paths>`, which is a **no-op once changes are
  committed**. My "4 of 5 fail" was a no-op reported as a pass. Use
  `git checkout <base> -- <paths>`.
- **A one-row fixture makes any ordering or differential assertion vacuous.** It
  bit me twice: a heading test and the evidence-order test both passed with the
  fix reverted. Always assert the population can express the failure —
  `len(set(stamps)) > 1` before asserting order.
- **Pushing new work onto an approved branch destroys the approval.** I did that to
  PR 364 and invalidated recon-2's exact-head approval. The conflict there was
  pre-existing; the lost approval was mine.
- **A mutation matrix only tests the axes you choose.** I approved hermit#3027 on
  two axes the change itself named, and a reviewer found a defect on an axis it
  did not — the never-written return precedes the timeout/OOM branch. I withdrew
  that approval. Probe intersections, not just reversions.
- **I ended turns at report boundaries rather than work boundaries**, twice naming
  a next step and stopping. If you inherit this lane, do the step.

---

## 6. State

- PR rrnewton/hermit#3019 — **merged**, site published, verified from origin.
- PR rrnewton/dev-hermit#364 — carries the review fixes **and** the ordering fix.
  Base is **429 commits behind main with 9 conflicting paths I did not write**.
  Coordinator has ruled: **do not rebase it.** Another route is being found.
  Four review follow-up tasks (`make_cell_evidence_occurrence`,
  `explain_the_historical_measurement`, `make_scorecard_cell_and`,
  `stop_scorecard_headings_and`) cannot close until it lands — `close-task`
  correctly refuses without a durable reference.
- hermit#3027 — my approval **withdrawn**, claude-lane refusal posted at
  `ec8917a6`. Two real P1 defects stand.
- Demo-Green-Review for `demos/02-record-replay.sh` given at dev-hermit
  `c2dc6caf3e3b98ade88f123abd2d51e37cd748d2`, run to `SUCCESS`, rc=0. **It attests
  dev-hermit main, not recon-2's change**, whose head I could not identify. If its
  change is not on that sha the attestation does not cover it.
- Latest built artifact: `47967efefc9ba756e5bba52105153ec9a07789e4a0b9367f5ca8560dc06bac6f`
  at parent `aefc245b`, carrying today's 190 rows. **Not released, not pinned.**
- **Do not let `ignored/compat-envelope/red30-x3` be reclaimed** before its rows
  are ingested. Reclamation of exactly this kind of directory is what forced a
  campaign re-run today.
