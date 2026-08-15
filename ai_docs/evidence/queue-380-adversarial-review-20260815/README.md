# Adversarial review of the qualifiable-fixture queue — the control and the amendment audit

**Produced by `hermit-bytes`.** Promoted by `hermit-coord`; not authored and not re-measured.
Source `ignored/evidence/queue-380-adversarial-review-20260815/`, checksums recorded below at copy
time. Newest source mtime 2026-08-15 13:53:23 — **at a stopping point, not still being written.**

## The queue itself is NOT copied here, because it is already tracked as a superset

`ai_docs/QUALIFIABLE_FIXTURE_QUEUE_20260815.tsv` already carries this content.
**MEASURED BY hermit-coord by containment, not byte-comparison:**

| | tracked | this source |
|---|---:|---:|
| total lines | 538 | 499 |
| data rows | **381** | **381** |
| comment lines | 157 | 118 |

Data rows absent from tracked: **0**. Tracked-only data rows: **0**. The entire 39-line difference is
header and amendment commentary, which the tracked copy has *more* of. **Copying the source over it
would have replaced a superset with a subset** — the byte-comparison would have shown a difference
and read as staleness in the wrong direction.

## What was genuinely unpreserved, and is promoted here

- `positive-control.txt` — the control for the "baseline already exits N natively" inclusion route,
  asking whether exit status **discriminates** or the fixture is stuck non-zero. **5 of 5
  discriminate.** It carries its author's own caveat, which travels with it: the 231 baseline-route
  rows are sound in substance, but *the header's stated definition of "qualifiable" does not describe
  them*.
- `amendment-history.txt` and `amendment-add-delete-counts.txt` — the audit across three amendments
  (`be05e446` → `730aa749` → `cb6a3914` → `578e9b08`): **+25/-0, +27/-0, +27/-0, zero deletions in
  all three**, with the 380 data rows byte-identical between the first and last.

## Anchors

The control was run against hermit main `61b9ba7aa`, which **has since been superseded** — hermit
`origin/main` was `302a1a9c0` at promotion time. The queue is anchored at `578e9b08`. Both are fixed
SHAs and the measurements stay valid against them; neither describes the current head.

## Source checksums at copy time

```
0e5b5adf189a9ae35e20c844e0f93c53  positive-control.txt              15 lines
3854f1f37f7303184afc1c26f7399a2d  amendment-history.txt              4 lines
0db17d7bb9d568f0c2f30da4e8731703  amendment-add-delete-counts.txt    5 lines
```
