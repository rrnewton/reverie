# HOLD record: scorecard-publish-sept14

Tracking issue: https://github.com/rrnewton/dev-hermit/issues/393. This file answers the owner's Google Chat
message `3QSxEZA5oY4` ("drive down the worktree slots to exactly match active agents").

It preserves the retention HOLD and every untracked handoff of `/home/newton/work/dev-hermit/worktrees/slots/scorecard-publish-sept14` on devbig014,
written at 2026-09-29T17:52:19Z before the slot was removed. The HOLD file itself stays in `worktrees/`.

## Registry row and liveness

- Registry row: generation 1, agent `scorecard-publish`, owner PID 1, boot `b597126a-0485-436b-b809-4c28c423193d`, start_ticks 131.
- Current boot is `47cceadc-9970-4ca3-87b0-90121ca7e88f` (the host booted at 2026-09-26T17:55:31Z), so the owner is not live.
- HOLD issuer: The HOLD file names no issuer. It was written at 2026-09-19T00:25:02Z by the same cleanup sweep that held the two integration-pr529-pr538 slots, on the previous boot. HANDOFF-cleanup-3-20260918.md below identifies that sweep as the cleanup-3 lane. No cleanup-3 pane is live. The live "scorecard" pane is a different lane started on the current boot; the HOLD text does not name it.
- `herdr agent list` at 2026-09-29T17:52:19Z: coord wJ:p1, kvm wJ:p38, (unnamed) wJ:p3A, liteinst wJ:p3B, gchat-iris wJ:p3X, parity-green wJ:p5B, scorecard wJ:p5C, kvm-readzero-main-claude-final wJ:p5F. None of these lanes is named in the HOLD text.

## Why the HOLD condition is met

The HOLD says to release "after these files are preserved elsewhere". The three untracked HANDOFF documents and the staged ai_docs/handoffs/hermit-136-20260827.md are in the WIP commit below and the three HANDOFF documents are reproduced verbatim at the end of this file.

## Rescue refs (checked with `git ls-remote` before removal)

- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-publish-sept14 at `b0c4e086a79283de395a6ab92c53f7ea06c5b4e4`: the checkout HEAD (4 unpicked dev-hermit patches).
- https://github.com/rrnewton/dev-hermit/tree/rescue/slots-20260928/scorecard-publish-sept14--wip at `f596984fc06d48ffe09eb8d03428de17a4caeb84`: staged file and the 3 untracked HANDOFF documents.
- https://github.com/rrnewton/hermit/tree/rescue/slots-20260928/scorecard-publish-sept14--hermit--branch-scorecard-publish-republish-sept14 at `3c54333d7fd6eb860cdcbf0e42a3d7e885392c83`: hermit branch scorecard-publish/republish-sept14.

## HOLD file, verbatim

`/home/newton/work/dev-hermit/worktrees/HOLD.9.devbig014.scorecard-publish-sept14.json` (411 bytes, sha256 `2fed0a95a89cc025768bd321f47d5ddd0abc686890bed78690ca66aa19b194ff`):

````json
{
  "held_at": "2026-09-19T00:25:02+00:00",
  "machine": "devbig014",
  "reason": "Holds untracked handoff documents under names the remove guard cannot see (_legacy_handoff_artifact hardcodes HANDOFF.md). Protected pending wrkslots_remove_guards_only. Release only after that guard covers prefixed handoffs, or after these files are preserved elsewhere.",
  "schema": 1,
  "slot": "scorecard-publish-sept14"
}
````

## Untracked handoff files, verbatim

### `HANDOFF-cleanup-3-20260918.md`

8497 bytes, sha256 `3a73e46ed3fb2e5f96c3ca56d74f5cf7e2a657f7aeeb08cc4c803c6fe4f225dd`.

````markdown
# Handoff — cleanup-3 — 2026-09-18

Written at context exhaustion. Everything below that is reconstructable from
TaskGraph has been left out deliberately; what is here is either an unlanded
artifact you need to find, or an interpretation you cannot re-derive by
re-running anything.

Do not write to `HANDOFF.md` in this slot. That name is a **tracked fleet
restart manifest** (sha256 `696aa7a417fc…`) and writing there destroys it. This
slot also holds `HANDOFF-scorecard-2.md` and `HANDOFF-scorecard-20260916.md`,
both genuine and both now preserved in dev-hermit main `adf8dd598`.

---

## 1. The unlanded revert

**https://github.com/rrnewton/hermit/pull/3096** — open, not landed.

It reverts **three lines** added by `1d9c9094de` "Preserve the initial thread
identity in synchronous tracing" (PR 2945), which added
`equalize_tracing_thread_number()` to `init_sync_file_tracing()` in
`hermit-cli/src/bin/hermit/tracing.rs`. That helper spawns and joins a host
thread purely to burn a DetTid. Under KVM with an interval timer the sandbox is
then killed by SIGALRM, which is what fails
`c-programs/setitimer-determinism (verify/kvm)`.

Located by bisect (7 probes, zero untestable) and **confirmed by revert**:
fail-before / pass-after at the head being landed on, both backends.

The cost is named in the PR body rather than left to be discovered: reverting
reintroduces the DetTid baseline misalignment 2945 set out to fix, there is **no
test on either side** (the helper has three call sites and zero tests), and the
obligation is tracked as
`restore-the-dettid-alignment-without-spawning-a-host-thread`.

### ⚠️ The cycle, which is the thing to understand before planning

3096 fixes `e2e.manifest_c_programs`. To land it needs a clean full-profile
receipt. A receipt needs **both** e2e nodes green. `e2e.manifest_backend_parity_c`
is not going green, because every non-ptrace backend fails 100% of its cells.

**So 3096 cannot land by the ordinary path, and neither can anything behind it:**
the reverie pin bump (https://github.com/rrnewton/hermit/pull/3090),
https://github.com/rrnewton/hermit/pull/3066, and
https://github.com/rrnewton/hermit/pull/3069. The parity defects are not merely
adjacent to the landing queue — they are load-bearing for it. Someone will have
to decide whether a receipt can be granted with a known, named, separately-owned
parity gap, or the queue stays shut.

Also open and unlanded: **PR 3093** (`check.portability_paths`). That node does
not appear in the current tip's failures, so it may already be resolved by other
means; verify before assuming 3093 is still needed.

---

## 2. The four defects, and who holds each

| task | backend | first divergence | holder |
|---|---|---|---|
| `kvm_backend_emits_no` | kvm | rec 10 | KVM lane |
| `kvm_virtual_time_accounting` | kvm | rec 11 | KVM lane |
| `liteinst_backend_emits_two` | liteinst | rec 16 | parked, lane in model-capacity stall |
| `sabre_diverges_from_ptrace` | sabre | rec 3 | newly filed, unrouted |

Measured cell-lines at tip `39ac9f8a55f1`: ptrace 102 pass / 0 fail; kvm 0/150;
liteinst 0/194; sabre 0/2. **Every non-reference backend fails every cell it
runs.** "Parity" currently means ptrace agreeing with itself.

---

## 3. Are the four one mechanism or four? — the question I was asked

**My view: four filed rows, but three underlying mechanisms, and the split is
not where the row boundaries are.**

The rdtsc counts, candidate against the ptrace reference, measured directly from
retained per-run logs and uniform across every cell sampled:

```
kvm       0  vs 10      liteinst  12 vs 10      sabre  0 vs 10
```

**kvm and sabre are almost certainly ONE defect.** Both emit *exactly zero*.
That is not a counting discrepancy, it is the absence of a code path: rdtsc
interception is recorded by ptrace and not recorded at all by either. A single
shared recording path that only the ptrace backend implements explains both, and
nothing in the evidence distinguishes them.

**liteinst is a different defect, and I would resist folding it in.** It emits
12 against ptrace's 10 — it *has* the recording path and produces two records
ptrace does not. Absence and over-count are opposite signs; a fix for "rdtsc is
never recorded" cannot produce "two extra rdtsc records". Treating all three as
one recording path would make liteinst's surplus inexplicable.

**`kvm_virtual_time_accounting` is definitely separate.** Both sides emit **zero**
rdtsc on that cell, so it cannot be the rdtsc mechanism under another name. It is
a 2,270 ns difference in committed virtual time, and 2,270 is not any of
detcore's per-syscall class constants, so it is not one extra or missing charge
either.

**`sabre_diverges_from_ptrace` is one row containing two things.** Its *first*
divergence at rec 3 is a randomness-initialisation ordering difference — ptrace
seeds the root PRNG where sabre initialises auxv `AT_RANDOM` — and that is
genuinely sabre-specific. Behind it sits the same zero-rdtsc gap as kvm. So that
row should be split, or at minimum its rdtsc half cross-referenced to
`kvm_backend_emits_no`.

### The falsifying test nobody has run

If kvm and sabre share one recording path, **a fix to the kvm rdtsc defect should
change sabre's rdtsc count from 0 to 10 with no sabre-specific work.** That is
cheap to check and it settles the question. Note that sabre's cell will still
fail afterwards, on the rec-3 randomness ordering — so measure the rdtsc count,
not the cell verdict, or the test will look like a failure.

⚠️ Sabre runs **one cell** here against kvm's and liteinst's dozens. Its 100%
failure rate rests on that single cell, so the extent of its gap is *unknown*
rather than small. Do not size sabre work from `FAIL=2`.

---

## 4. Conclusions not already in a task note

- **The owner's parity definition settles the framing and I got it wrong first.**
  Parity is ptrace versus another backend *in the same run*, nothing stored, no
  golden reference. I had proposed that the comparison envelope "should select on
  record identity rather than index" — that would have stopped comparing exactly
  the records where the backends disagree, discarding four real gaps while
  looking like a correctness fix. Do not reach for it.
- **Never clear a parity cell by dropping a backend, scoping the cell, or adding
  a stored golden log.** The owner has declined golden references specifically:
  regolding manufactures spurious failures.
- **Two instruments here report confidently and wrongly about what is safe to
  delete**, and they have been compensating for each other: `wrkslots audit`
  says DELETABLE without modelling the salvage push, and `read-handoff` records a
  read of any file at `HANDOFF.md`. Fixing `agent_slot_salvage_push` *before*
  `wrkslots_read_handoff_records` would remove the net while leaving the hole.
  That ordering warning is on both rows; it is the one thing in the slot work I
  would not want lost.
- **A clean tool root is how you get a validate past the dirty shared parent**:
  `worktrees/toolroot-cleanup-3`, a worktree of the canonical parent with **both**
  `hermit` and nested `hermit/agent-utils` initialised. A fresh worktree has no
  submodules and refuses in 0.007s with "the committed wrkslots package is
  missing" — the sub-10ms timing is the tell that distinguishes it from a lock
  collision, which takes 4–6s.
- **Do not put a checkout under `/tmp`**: it cannot run its own guests
  ("Program … is under host /tmp").

---

## 5. Method notes that cost me real time

- **Ask the process.** I killed the sabre vDSO hypothesis by elimination and the
  reasoning was unsound; a direct probe both corrected the argument and, by
  accident, reproduced the actual defect.
- **A bisect probe must key on the product condition, not the harness wording.**
  The same condition appeared as `FAIL … Signaled{14}` and as
  `ERROR … verdict=no_result`; mapping ERROR to untestable gave 43 untestable
  against 4 bad and converged on nothing. Treat only an explicit PASS as good.
- **Identical values across unrelated slots mean a shared-repository artifact,
  not a finding.** It caught me three times: `unpushed=1420`, `off-origin=3364`,
  and a branch census returning 3426/4961/31 for every worktree.
- **Absence of evidence read as evidence of absence** caught me three more times:
  "first observed" as "introduced", "PASS then FAIL now" as "regressed", and
  "not where I looked" as "not retained".
````

### `HANDOFF-scorecard-2.md`

8442 bytes, sha256 `d56c816163942b5b251ac460af0f06ab5030a6c9f2a41d7e5e7b2141e154ee37`.

````markdown
# scorecard-2 handoff — 2026-09-17

NOT in `HANDOFF.md`: that name is a TRACKED file in this checkout, the fleet
restart manifest. Writing there would have destroyed it. This is the file to
read; `HANDOFF-scorecard-20260916.md` beside it is my predecessor's.

Interpretation only. Every number, sha and path is in the task notes and in the
repository. What follows is the reading of them, which is what dies with the
lane.

---

## 1. The 407 GB question: what it actually turns on

I asked the coordinator twice whether reclaiming 407 GB outranks preventing the
next leak, and got "go, continue" both times. **That is not an answer and it
should not be read as one.** Do not treat the silence as authorisation for
either ordering.

The question is not "disk versus correctness". It turns on one fact I could not
establish cheaply: **how much of the 407 GB is actually reclaimable.** These are
btrfs trees and `du` counts blocks per tree, so if the seven checkouts share
extents — they are checkouts of the same repository at nearby commits, so they
plausibly do — the reclaimable total could be a small fraction of the sum.
`compsize` over the seven would settle it in minutes and I did not run it.

Why that decides the ordering:

- **If most of it is shared**, reclaiming buys little, the teardown is clearly
  first, and the cleanup timeout stays third.
- **If most of it is real**, 407 GB against 1.2T free is a third of the runway,
  the cleanup becomes the thing standing between the fleet and a full disk, and
  the ordering inverts.

The second consideration, which is softer but points the same way: the teardown
prevents the *next* leak and does nothing about the existing seven, while the
cleanup can drain them but does nothing about the next. They are complements,
not alternatives, and whichever is done first the other is still needed. The
only reason to sequence them at all is scarcity of attention.

My own inclination, stated so it can be argued with rather than inherited:
**measure with compsize first, then almost certainly teardown.** The leak rate is
slow enough (seven trees over roughly twenty-five hours, and two of those were
reclaimed during the night) that the disk is unlikely to be the binding
constraint within a day, and the teardown removes a whole class of failure
rather than an instance of it.

---

## 2. Measured versus reasoned — the distinction that must travel

Tonight's findings are not uniform in strength and the notes do not always make
that obvious at a glance. Sort them before relying on any of them.

**Measured, by running something:**

- ExecStopPost fires after the main process exits, can remove its own working
  directory given `cd /`, and **fires even on SIGKILL**. Three real transient
  units, all three properties observed.
- Inode deletion cost: 20,041 entries in 0.23 s on this filesystem.
- The record reaches terminal shape without the waiter — read off 205 completed
  `validate` records.
- The disposition reader's verdicts on the real fleet.
- Every test control: each new assertion was checked by removing the thing it
  guards and confirming it then fails.

**Reasoned, not tested — treat as hypotheses:**

- **ExecStopPost inherits everything the writeback needs from the unit's
  environment.** I argued this from the unit's `--setenv` list. I did not run a
  writeback from an ExecStopPost. If the wiring misbehaves, look here first.
- **ExecStopPost's own process is not caught by the residual census.** I argued
  it from the census closing inside the main process before it exits. Plausible
  and untested, and if wrong it quarantines every run — which is the same
  failure the post-child placement would have caused.
- **Freeing 44 GB of extents fits inside a stop timeout.** Only the inode half
  is measured. This is what `TimeoutStopSec` must be set against, so measure a
  real retire before choosing the number.
- **The 29 writeback absences are waiter deaths.** Inferred from the field being
  absent rather than failed. I did not open one.

The general lesson, which cost me three public corrections tonight: I was wrong
every time I reasoned one step past a measurement I could have taken in seconds.
The stale contract, the "unpushed" commits, and my own blocker on non-worktree
paths were all of that shape. When something is cheap to measure, measure it —
the confident-sounding note is the dangerous artifact.

---

## 3. What I would do next

### Teardown

One step remains and it is wiring, not design: add the `ExecStopPost` property
and an explicit `TimeoutStopSec` to `build_systemd_command`, keeping the sweep as
the backstop.

Do these in order and do not compress them:

1. **Measure a real retire first.** Run `--retire-unit` by hand against one
   stranded checkout and time it. That produces the `TimeoutStopSec` number and
   simultaneously exercises the success path, which no test covers.
2. **Then wire it.** The wiring is a few lines; the review should be about
   whether the timeout is right and whether the sweep is still reachable when
   the bound trips.
3. **Expect the first real run to surface the two reasoned assumptions above.**
   Watch for a quarantined run and for a writeback that cannot find something in
   its environment.

The reason the capability landed inert is that wiring changes the behaviour of
every validate on the box. Keep that separation: if the wiring is bad it should
be revertible without losing the capability.

### Marker

The marker task is closed and the gate is landed. The piece I deliberately left
is the five `name.startswith("validate-fresh-")` predicates. **Do not convert
them just because they look untidy.** They answer classification — "is this the
kind of tree this sweep manages" — which is a different question from
authorisation, and the gate now covers the case that mattered. Converting them
broke 91 tests when I tried it, and the reason was that I was answering the wrong
question in the wrong place. If someone later has a real reason to convert them,
the reason must be something other than consistency.

The open question worth keeping: **what removability means for a managed path
that is not a git worktree.** Zero of twenty-five are in that state today, so it
costs nothing now, but a half-created checkout is exactly the leak class and the
gate would protect it forever. That wants its own rule, not a weakening of this
one.

---

## 4. What I deliberately did not do, and why

- **I did not resolve the two timeout bounds.** The cleanup's 30 s, the audit's
  CPU budget, and a 180 s workflow bound failing twelve times with no variance
  look related. They might share a cause. I declined to guess and I still would:
  a consistently-at-the-bound failure has several explanations and I had no way
  to separate them without work nobody had asked for.
- **I did not delete any checkout, including ones my own reader calls
  disposable.** The cleanup is dark, so nothing else would have; that makes a
  manual deletion tempting and also makes it the only deletion happening, with
  no second pair of eyes. The five commits I found are preserved to
  `refs/rescue/*` instead, which is append-only and reversible.
- **I did not convert 91 tests to admit a change.** That is the goalpost move and
  it is worth naming because the alternative reading — "the fixtures were
  wrong" — was partly true and would have been an easy story to tell.
- **I did not start the ExecStopPost wiring after the capability landed.** I had
  context left. I stopped because the next honest step was a measurement
  (timing a real retire) and starting the wiring without it would have meant
  choosing a timeout by guess, which is the exact habit the rest of this handoff
  warns about.

---

## 5. One thing about this lane's own failure mode

I stopped and reported at the end of most turns, and the coordinator had to say
"go" repeatedly. Some of that was right — twice I stopped because proceeding
would have destroyed data, and both times the stop was the correct call. But
some of it was ending at a report boundary rather than a work boundary, which my
predecessor named as their own failure and which I then repeated.

The distinction worth inheriting: stopping to report a **finding that changes
what someone else should do** is right. Stopping to report **progress** is not.
If you find yourself writing a summary of what you just did, keep working
instead.
````

### `HANDOFF-scorecard-20260916.md`

11895 bytes, sha256 `f264e9dc043f450cadc7dc49171a0825d7539856b8a7799c4336f9fb41d1b908`.

````markdown
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
````
