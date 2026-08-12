# Compat-envelope baseline, locked BEFORE the `--verify-strict` switch

Measured 2026-08-12 for task
`measure-and-lock-the-scorecard-under-the-current-known-bad-standard`. This is the
**before** number for the strict-compat comparator switch. It is deliberately not
flattering; every count that could be inflated is shown alongside the inflated version.

## Provenance, and the first thing a reader must know

| | |
| --- | --- |
| source | `compat-envelope/scorecard.csv`, read from `git show HEAD:` (the working copy goes dirty) |
| parent commit | `6afe356f8aee` |
| hermit `origin/main` at measurement | `0a8a44aac39f` |
| raw rows | 541 |
| distinct logical cells | 541 — keyed `(bucket,test_id,test_mode,backend)`; no cell repeats |

**THE SCORECARD CONTAINS ZERO ROWS AT CURRENT MAIN.** It spans **six** different
`hermit_sha` values across **ten** run identities, none of which is `0a8a44aa`. The most
recent is `590fcc9e`. So this baseline is *locked to the artifact*, not to current main —
it is the state of the envelope as last measured, not the state of the envelope at the
commit the switch will land on. An after-number taken at a different SHA is comparable to
this only if the corpus and backends are unchanged.

## The envelope: 95 green

Only `cell_state == enabled` cells are in the regression envelope. `disabled` means
*expansion candidate* and `expansion` means *not yet admitted*; neither counts.

| classification | cells |
| --- | ---: |
| **GREEN** (enabled + pass) | **95** |
| RED (enabled + fail) | 4 |
| UNEVALUATED (enabled + unavailable) | 7 |
| **envelope denominator** | **106** |
| not in the envelope (disabled / expansion) | 435 |

### The number that would flatter it

**378 rows have `outcome == pass`.** Only **95** of those are envelope green. The other 283
are `disabled`+`pass` (275), `expansion`+`pass` (8) — expansion candidates that pass, which
is not the same claim. Any report of "378 passing" is wrong by a factor of four.

## Slices

**By backend**

| backend | green | red | uneval | total |
| --- | ---: | ---: | ---: | ---: |
| ptrace | 58 | 0 | 0 | 58 |
| dbi | 34 | 0 | 0 | 34 |
| kvm | 3 | 4 | 0 | 7 |
| sabre | 0 | 0 | **7** | 7 |

**sabre is 0 green and 7 unevaluated** — reason `backend binary not present`. It is
UNMEASURED, not failing, and must never be reported as either passing or regressed.

**By run mode** — regression 86 green / 0 red / 7 uneval; expansion 9 green / 4 red / 0 uneval.

**By lane** — `portable` 95 / 4 / 7. **There is only one lane.** The envelope covers no
other lane at all, so "the compat envelope" today means the portable lane.

**By bucket** — backend-parity 50, c-programs 19 (+4 red, +3 uneval), system-utils 6 (+2
uneval), language-runtimes 6, ptrace-short-full-tier 6, determinism-stress 4,
data-handling 2 (+1 uneval), applications 1 (+1 uneval), determinism-stress-c 1.

**By test mode — the slice that matters most for this switch**

| test_mode | green | red | uneval |
| --- | ---: | ---: | ---: |
| `verify` | 69 | 4 | 7 |
| `strict` | 26 | 0 | 0 |

**69 of the 95 green cells were measured under `verify`, not `strict`.** Whatever the
switch tightens, it tightens the standard those 69 cells were certified against. The 26
`strict` cells are all backend `dbi`. Broken out: strict/dbi 26, verify/ptrace 58,
verify/dbi 8, verify/kvm 7, verify/sabre 7.

> **Open question for whoever owns the switch:** confirm whether the scorecard's
> `test_mode` values (`verify` / `strict`) map onto `--verify` versus `--verify-strict`, or
> onto `hermit run --strict`. The corpus command for `CompatMode::Strict` is
> `run --strict --verify`, i.e. it already carries both words, so the mapping is not
> self-evident from the column name and should be confirmed rather than assumed. The
> 69-vs-26 split is the *shape* of the exposure either way.

## The non-green cells, named

**RED (4)** — all `kvm`, all `c-programs`:
`epoll-determinism` (kvm-run-fail-exit2), `madvise-determinism` (exit17),
`mmap-determinism` (exit2), `thread-sync-determinism` (exit2).

**UNEVALUATED (7)** — all `sabre`, reason `backend binary not present`:
`applications/timed-progress-bar`, `c-programs/get-robust-list-self`,
`c-programs/ioctl-fioclex`, `c-programs/kcmp-eperm`, `data-handling/archive-roundtrip`,
`system-utils/date-nanoseconds`, `system-utils/random-device`.

## Re-derivation

```sh
git show HEAD:compat-envelope/scorecard.csv > /tmp/sc.csv
# envelope = cell_state == enabled; green = outcome == pass
# logical cell key = (bucket,test_id,test_mode,backend)
```

## Carried forward from the 2026-08-08 baseline

`ai_docs/2026-08-08-compat-envelope-baseline.md` measured the *tier* dimension: 6 rows
claimed a qualifying `comparison_tier` and **0** were evidenced, and cross-backend was
`0/463` **UNMEASURED, not failed**. Nothing here supersedes that; this document counts
green cells, which is a weaker and more permissive standard than the tier claim. A cell
counted green above may still be unevidenced at tier level.
