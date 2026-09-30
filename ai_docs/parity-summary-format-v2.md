# Parity summary format: S12 (hermit scorecard.rs) and D5 (dev-hermit parity panel), version 2

Version 2, written by pg-s12c on 2026-09-30 for parity-green, so that slice
S12 and slice D5 of https://github.com/rrnewton/hermit/issues/3301 keep
working from one text. It revises version 1 (/tmp/pg-s12-parity-format.md,
sha256 2eae2eb1945bf396f6e75e56ff9e12d3bf6520eed14e0abdbd0c7578a0498969,
approved by parity-green at about 04:12Z) as parity-green's review of S12
phase 1 required: the floor prints its own denominator, and the line says
`counted as 0:` and `excluded:`.

## Changes from 2eae2eb1

1. The floor clause prints `floor_cells` as its denominator:
   `floor F over <floor_cells> of <selected> selected`. Version 1 printed
   `floor F over <measured> measured of <selected> selected`, which is the
   mean's denominator. The floor's is larger whenever a cell is counted as 0
   (the RUN 1953 fixture's total: mean over 175 measured, floor over 178).
2. The parentheses after the floor hold two groups: `counted as 0: ...`, the
   unmeasured, record-missing and refused cells that the floor counts as 0
   (omitted when there are none), then `excluded: ...`, the cells the floor
   leaves out. Version 1 printed one list.
3. Version 1's last entry, `floor excludes N record-missing or refused
   cell(s) whose inputs cannot be equalized`, is gone. Those cells are listed
   under `excluded:` instead: by kind when every record-missing and refused
   cell is excluded, and as a sum on each side when some are counted as 0
   and some are excluded (section 4).
4. Section 1: the `floor_cells` row and invariant say which cells the floor
   counts, and a new invariant gives the count of excluded record-missing and
   refused cells that the line derives.
5. Section 4: `render_line` builds the two groups. `coverage_text` and
   `inputs_marker` are unchanged.
6. Section 5: every line with a floor clause is rendered again; C and E3
   (the all-not-compared line, which has no floor clause) are unchanged. C3's
   heading says "excluded". Two cases are new: G (DBT's record-missing and
   refused cells, all excluded) and H (a run total with such cells on both
   sides of the floor). Both have an unknown committed selection, like the E
   cases, so that their lines are exactly the ones S12's test asserts. There
   are now 14 cases, so D5's `FORMAT_CASES`, its case count, and its copy of
   section 5 and that copy's hash change with this text.
7. Sections 2, 3 and 6 are unchanged.

- D5 reference: `ci-hub/compatibility-website/parity_summary.py` in slot
  parity-d5 at 6ba6a2c378dd14e2859a499bee0d6cbd932db9b8 (sha256 of the file
  2758ff57b38004f332eb745bf00bf7999a01ef4b8cd4422682087b5f2143c952). Its
  `render_line` gives version 1's line for every version 1 case.
- Every line in section 5 was produced by the reference code in section 4,
  run over the tallies shown (script /tmp/pg-s12c-gen-v2.py, output
  /tmp/pg-s12c-gen-v2-out.json). For every version 1 case, the script also
  asserts that version 1's own section 4 code and D5's current `render_line`
  still give version 1's line. It then reads this file back and checks every
  section 5 line against the section 4 code printed here.
- S12 reference: `ParityTally::render_line` in hermit's
  `ci/compat-envelope/scorecard.rs`, in the working tree of slot parity-s12 on
  0f371c9f93aadd5d0b36e9ba435927d1e6f9e643 (not yet committed).
  It produces these exact strings. Its tests assert the lines of case E
  (`a_run_whose_rows_name_two_commits_is_refused_whole_wherever_the_stray_row_sits`)
  and of cases G and H
  (`unequalizable_record_missing_and_refused_cells_are_excluded_from_the_floor`)
  verbatim, and the five lines of the RUN 1953 fixture
  (`the_real_run_1953_rows_summarize_to_the_checked_in_expectation`). The
  script compares all of them with the section 4 code's output.

## 1. Tally fields (every `per_backend.<backend>` and every run `total`)

In the order S12 serializes them. D5's `_exact` compares sets, so order is
informational. `[S12]` marks the fields D5 does not accept yet and must add.

| # | Field | Type | Meaning |
|---|---|---|---|
| 1 | `selected` | count | cells in this tally |
| 2 | `measured` | count | `matched + diverged` |
| 3 | `matched` | count | |
| 4 | `diverged` | count | |
| 5 | `no_golden` | count | sum of `no_golden_by_class` |
| 6 | `no_golden_by_class` | object | exactly the 9 no-golden classes, below |
| 7 | `not_compared` | count | cells of class `inputs-not-equalized` |
| 8 | `unmeasured` | count | sum of `unmeasured_by_class` |
| 9 | `unmeasured_by_class` | object | exactly the 7 unmeasured classes, below |
| 10 | `record_missing` | count | |
| 11 | `refused` | count | |
| 12 | `credited` | count | cells with a non-null `credit` |
| 13 | `clean_credited` | count | credited with `credit_basis` `clean` |
| 14 | `unequalized_credited` | count | credited with `credit_basis` `unequalized` |
| 15 | `credit_sum` | number | sum of every non-null cell credit |
| 16 | `clean_credit_sum` [S12] | number | sum over the clean-credited cells |
| 17 | `unequalized_credit_sum` [S12] | number | sum over the unequalized-credited cells |
| 18 | `floor_cells` | count | the floor's denominator: the measured cells and the cells counted as 0 |
| 19 | `mean_credit` | number or null | `credit_sum / measured`; null when `measured` is 0 |
| 20 | `clean_mean_credit` [S12] | number or null | `clean_credit_sum / clean_credited`; null when 0 |
| 21 | `unequalized_mean_credit` [S12] | number or null | `unequalized_credit_sum / unequalized_credited`; null when 0 |
| 22 | `floor_credit` | number or null | `credit_sum / floor_cells`; null when `floor_cells` is 0 |
| 23 | `committed` [S12] | count or null | cells of this tally's scope that the run's own commit selects in `ci/parity-cells.json`; null when that selection is unknown |
| 24 | `committed_selected` [S12] | count or null | how many of those the run reported |
| 25 | `outside_committed` [S12] | count or null | cells the run reported that the selection does not name |
| 26 | `first_divergence` | object or null | unchanged (D5's `GROUP_FIELDS`) |
| 27 | `line` | string | `render_line` of this tally (section 4) |

S12 no longer emits `unavailable` or `unavailable_by_class`.

`no_golden_by_class` keys, in print order: `determinism-mismatch`, `timeout`,
`crash`, `oom`, `infrastructure-error`, `sandbox-denied`, `failed-untyped`,
`ended`, `host-inapplicable`.

`unmeasured_by_class` keys, in print order: `log-not-retained`,
`log-unreadable`, `no-result-row`, `log-diff-failed`, `invalid-test-id`,
`golden-not-written`, `epoch-not-shared`.

Both objects always carry every key of their group, zeros included.

### Invariants S12 guarantees, and D5 can check

- `selected = measured + no_golden + not_compared + unmeasured + record_missing + refused`.
- `measured = matched + diverged`; `credited = clean_credited + unequalized_credited <= measured`.
- `floor_cells`, the floor's denominator, counts the measured cells and the
  cells counted as 0. A cell is in a floor unless its group is no-golden or
  not-compared, or its backend is `dbt`, whose inputs cannot be equalized: no
  dbt cell enters any mean or floor, record-missing and refused included, and
  dbt has no measured and no unmeasured cell (hermit's
  `ci/manifest-plan/src/parity.rs` `check_class`). So
  `per_backend.dbt.floor_cells = 0`, every other per-backend
  `floor_cells = selected - no_golden - not_compared`, and the total's is
  that less `per_backend.dbt.record_missing + per_backend.dbt.refused`.
- It follows that, in every tally, `selected - no_golden - not_compared -
  floor_cells` is the number of record-missing and refused cells excluded
  from the floor, and `record_missing + refused` less that number is how
  many of them are counted as 0. The line (section 4) derives both from
  these fields alone.
- `clean_credit_sum + unequalized_credit_sum = credit_sum` within
  `CREDIT_SUM_TOLERANCE`. Every sum is a plain left-to-right `+=` over the
  cells in the run's `cells` order. Python's built-in `sum()` of floats is
  compensated since 3.12 and can differ from Rust in the last bits.
- `committed`, `committed_selected` and `outside_committed` are all null, or
  all set. They are null exactly when the run's `committed_unknown` is
  non-null.
- When set: `committed_selected + outside_committed = selected` and
  `committed_selected <= committed`. For the total,
  `outside_committed = len(run.cells_outside_committed)` and
  `committed = committed_selected + len(run.committed_cells_without_row)`.
  Per backend, the same holds over the list entries ending in `@<backend>`.

## 2. Run fields (`producers[]`)

`producer`, `run_id`, `hermit_sha`, `conflicting_hermit_shas` [S12], `depth`,
`emitted_at`, `line`, `committed_unknown` [S12], `committed_cells_without_row`
[S12], `cells_outside_committed` [S12], `per_backend` (exactly `dbt`, `kvm`,
`liteinst`, `sabre`, a backend with no cells included), `total`, `cells`.

- `conflicting_hermit_shas`: a list of commit strings, empty unless the run's
  rows named more than one Hermit commit. In that case `hermit_sha` is null
  and every row of the run is refused.
- `committed_unknown`: string or null. It gives the reason the run's committed
  selection is unknown.
- `committed_cells_without_row`: a sorted list of `<test>@<backend>` cells
  that the selection names and the run did not report. It is empty when the
  selection is unknown.
- `cells_outside_committed`: a sorted list of `<test>@<backend>` cells that
  the run reported and the selection does not name. It is empty when the
  selection is unknown.
- `line`: equals `total.line`.

## 3. Other objects

- Summary: D5's `SUMMARY_FIELDS` plus `legacy_dropped_cells` [S12]. That
  field is an object mapping each reason to a count of distinct
  `<canonical test id>@<backend>` cells, parallel to `legacy_dropped`, which
  counts rows.
- Cell: exactly D5's `CELL_FIELDS`. S12 adds `operand` after
  `unavailable_class`; both come straight from the ledger row and are never
  derived from the reason text. A refused cell has both null.
- Brief (`runs[]`), legacy, legacy entry, depth and first-divergence group:
  unchanged from D5's field lists.

## 4. The line: reference code

`credit_text` is D5's: `n/a` for null; otherwise three decimals, correctly
rounded with ties to even; `0.999` for a value below 1 that would print
`1.000`. Rust's `{:.3}` and Python's `:.3f` agree on this (checked
2026-09-30 with 0.0625 giving 0.062, 0.34375 giving 0.344, 0.1875 giving
0.188, and 0.9995 giving 1.000, in both languages). The class counts in the
parentheses list the nonzero classes in the print order of section 1, as
`name n`, separated by `, `. The parentheses after the floor hold
`counted as 0: <entries>`, omitted when it has no entry, then
`excluded: <entries>`; the groups, and the entries within a group, are
separated by `; `.

```python
def coverage_text(tally):
    committed = tally["committed"]
    reported = tally["committed_selected"]
    outside = tally["outside_committed"]
    if committed is None:
        assert reported is None and outside is None
        return "committed selection unknown"
    text = f"selected {reported} of {committed} committed"
    if reported < committed:
        text += " (partial)"
    if outside > 0:
        text += f"; {outside} outside the committed selection"
    return text


def inputs_marker(tally):
    if tally["credited"] > 0 and tally["clean_credited"] == 0:
        return " [inputs not equalized]"
    if tally["unequalized_credited"] > 0:
        return (
            f" [inputs equalized for {tally['clean_credited']} of {tally['credited']} credited: "
            f"mean {credit_text(tally['clean_mean_credit'])} over {tally['clean_credited']} with equal inputs; "
            f"mean {credit_text(tally['unequalized_mean_credit'])} over {tally['unequalized_credited']} with unequal inputs]"
        )
    return ""


def render_line(tally):
    selected = tally["selected"]
    if selected > 0 and tally["not_compared"] == selected:
        return (
            f"parity: not compared: {tally['measured']} measured of {selected} selected "
            f"(inputs cannot be equalized); {coverage_text(tally)}"
        )
    # Counted as 0 in the floor.
    counted_as_zero = []
    if tally["unmeasured"] > 0:
        counted_as_zero.append(
            f"{tally['unmeasured']} unmeasured: "
            + ", ".join(
                f"{n} {tally['unmeasured_by_class'][n]}" for n in UNMEASURED_CLASSES if tally["unmeasured_by_class"][n] > 0
            )
        )
    # Excluded from every floor.
    no_golden = f"{tally['no_golden']} no golden"
    if tally["no_golden"] > 0:
        no_golden += ": " + ", ".join(
            f"{n} {tally['no_golden_by_class'][n]}" for n in NO_GOLDEN_CLASSES if tally["no_golden_by_class"][n] > 0
        )
    excluded = [no_golden, f"{tally['not_compared']} not compared"]
    kinds = []
    if tally["record_missing"] > 0:
        kinds.append(f"{tally['record_missing']} record-missing")
    if tally["refused"] > 0:
        kinds.append(f"{tally['refused']} refused")
    # The record-missing and refused cells of a backend whose inputs cannot be
    # equalized are excluded; the rest are counted as 0. Section 1: nothing
    # else is outside the no-golden, not-compared and floor cells.
    unequalizable = max(0, selected - tally["no_golden"] - tally["not_compared"] - tally["floor_cells"])
    missing_or_refused = tally["record_missing"] + tally["refused"]
    if unequalizable == 0:
        counted_as_zero.extend(kinds)
    elif unequalizable == missing_or_refused:
        excluded.append(" and ".join(kinds) + " whose inputs cannot be equalized")
    else:
        # The tally's fields do not say which kind each side's cells are.
        in_floor = max(0, missing_or_refused - unequalizable)
        counted_as_zero.append(f"{in_floor} record-missing or refused")
        excluded.append(f"{unequalizable} record-missing or refused whose inputs cannot be equalized")
    groups = [f"counted as 0: {'; '.join(counted_as_zero)}"] if counted_as_zero else []
    groups.append(f"excluded: {'; '.join(excluded)}")
    credited = f" ({tally['credited']} credited)" if tally["credited"] != tally["measured"] else ""
    return (
        f"parity: {tally['matched']}/{selected} matched; {coverage_text(tally)}; "
        f"mean {credit_text(tally['mean_credit'])} over {tally['measured']} measured{credited}; "
        f"floor {credit_text(tally['floor_credit'])} over {tally['floor_cells']} of "
        f"{selected} selected ({'; '.join(groups)}){inputs_marker(tally)}"
    )
```

Differences from D5's current `render_line` (version 1's), and nothing else:

1. The floor's denominator is `floor_cells`: the clause reads
   `floor F over <floor_cells> of <selected> selected`, not
   `floor F over <measured> measured of <selected> selected`.
2. The entries are grouped. `counted as 0:` holds the unmeasured entry, then
   the record-missing and refused entries of cells in the floor. `excluded:`
   holds the no-golden and not-compared entries, then the record-missing and
   refused entries of cells whose backend's inputs cannot be equalized.
3. Version 1's `floor excludes N record-missing or refused cell(s) whose
   inputs cannot be equalized` entry is gone. When every record-missing and
   refused cell is excluded, `excluded:` ends with the nonzero kinds joined
   by ` and `, then ` whose inputs cannot be equalized` (cases C3 and G).
   When some are counted as 0 and some are excluded, which only a total over
   several backends can be, `counted as 0:` ends with
   `<n> record-missing or refused` and `excluded:` ends with
   `<u> record-missing or refused whose inputs cannot be equalized` (case H),
   because the tally's fields do not say which kind each side's cells are.

## 5. Cases

### A. A normal backend (kvm; one no-golden cell; every committed cell reported)

Tally: selected=77, measured=76, matched=0, diverged=76, no_golden=1, not_compared=0, unmeasured=0, record_missing=0, refused=0, credited=76, clean_credited=0, unequalized_credited=76, floor_cells=76, committed=77, committed_selected=77, outside_committed=0; no_golden_by_class: determinism-mismatch 1.
  credit_sum=7.599999999999989, clean_credit_sum=0.0, unequalized_credit_sum=7.599999999999989, mean_credit=0.09999999999999985, clean_mean_credit=null, unequalized_mean_credit=0.09999999999999985, floor_credit=0.09999999999999985.

    parity: 0/77 matched; selected 77 of 77 committed; mean 0.100 over 76 measured; floor 0.100 over 76 of 77 selected (excluded: 1 no golden: determinism-mismatch 1; 0 not compared) [inputs not equalized]

### B. A normal backend with every clause

This case has partial coverage, one cell outside the selection, mixed inputs,
one divergence without a prefix (so it is measured but not credited),
no-golden cells, an unmeasured cell, a record-missing cell and a refused cell.

Tally: selected=10, measured=5, matched=2, diverged=3, no_golden=2, not_compared=0, unmeasured=1, record_missing=1, refused=1, credited=4, clean_credited=3, unequalized_credited=1, floor_cells=8, committed=12, committed_selected=9, outside_committed=1; no_golden_by_class: timeout 1, crash 1; unmeasured_by_class: log-diff-failed 1.
  credit_sum=2.75, clean_credit_sum=1.75, unequalized_credit_sum=1.0, mean_credit=0.55, clean_mean_credit=0.5833333333333334, unequalized_mean_credit=1.0, floor_credit=0.34375.

    parity: 2/10 matched; selected 9 of 12 committed (partial); 1 outside the committed selection; mean 0.550 over 5 measured (4 credited); floor 0.344 over 8 of 10 selected (counted as 0: 1 unmeasured: log-diff-failed 1; 1 record-missing; 1 refused; excluded: 2 no golden: timeout 1, crash 1; 0 not compared) [inputs equalized for 3 of 4 credited: mean 0.583 over 3 with equal inputs; mean 1.000 over 1 with unequal inputs]

### C. DBT, every selected cell not compared

Tally: selected=14, measured=0, matched=0, diverged=0, no_golden=0, not_compared=14, unmeasured=0, record_missing=0, refused=0, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=0, committed=14, committed_selected=14, outside_committed=0.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=null.

    parity: not compared: 0 measured of 14 selected (inputs cannot be equalized); selected 14 of 14 committed

### C2. DBT with some no-golden cells: the ordinary line, floor n/a

Tally: selected=14, measured=0, matched=0, diverged=0, no_golden=2, not_compared=12, unmeasured=0, record_missing=0, refused=0, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=0, committed=14, committed_selected=14, outside_committed=0; no_golden_by_class: determinism-mismatch 2.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=null.

    parity: 0/14 matched; selected 14 of 14 committed; mean n/a over 0 measured; floor n/a over 0 of 14 selected (excluded: 2 no golden: determinism-mismatch 2; 12 not compared)

### C3. DBT with a record-missing cell, which is excluded from every floor

Tally: selected=14, measured=0, matched=0, diverged=0, no_golden=0, not_compared=13, unmeasured=0, record_missing=1, refused=0, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=0, committed=14, committed_selected=14, outside_committed=0.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=null.

    parity: 0/14 matched; selected 14 of 14 committed; mean n/a over 0 measured; floor n/a over 0 of 14 selected (excluded: 0 no golden; 13 not compared; 1 record-missing whose inputs cannot be equalized)

### D. A backend with nothing measured and a cell in the floor: floor 0.000, mean n/a

Tally: selected=2, measured=0, matched=0, diverged=0, no_golden=1, not_compared=0, unmeasured=1, record_missing=0, refused=0, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=1, committed=2, committed_selected=2, outside_committed=0; no_golden_by_class: timeout 1; unmeasured_by_class: no-result-row 1.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=0.0.

    parity: 0/2 matched; selected 2 of 2 committed; mean n/a over 0 measured; floor 0.000 over 1 of 2 selected (counted as 0: 1 unmeasured: no-result-row 1; excluded: 1 no golden: timeout 1; 0 not compared)

### D2. A backend with nothing measured and nothing in the floor: both n/a

Tally: selected=2, measured=0, matched=0, diverged=0, no_golden=2, not_compared=0, unmeasured=0, record_missing=0, refused=0, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=0, committed=2, committed_selected=2, outside_committed=0; no_golden_by_class: timeout 1, crash 1.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=null.

    parity: 0/2 matched; selected 2 of 2 committed; mean n/a over 0 measured; floor n/a over 0 of 2 selected (excluded: 2 no golden: timeout 1, crash 1; 0 not compared)

### D3. A backend the run reported no cell for, which its commit selects

Tally: selected=0, measured=0, matched=0, diverged=0, no_golden=0, not_compared=0, unmeasured=0, record_missing=0, refused=0, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=0, committed=2, committed_selected=0, outside_committed=0.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=null.

    parity: 0/0 matched; selected 0 of 2 committed (partial); mean n/a over 0 measured; floor n/a over 0 of 0 selected (excluded: 0 no golden; 0 not compared)

### E. Committed selection unknown: 13 refused rows (a run whose rows name two Hermit commits)

Tally: selected=13, measured=0, matched=0, diverged=0, no_golden=0, not_compared=0, unmeasured=0, record_missing=0, refused=13, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=13, committed=null, committed_selected=null, outside_committed=null.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=0.0.

    parity: 0/13 matched; committed selection unknown; mean n/a over 0 measured; floor 0.000 over 13 of 13 selected (counted as 0: 13 refused; excluded: 0 no golden; 0 not compared)

### E2. Committed selection unknown: an ordinary measured run

Tally: selected=12, measured=12, matched=0, diverged=12, no_golden=0, not_compared=0, unmeasured=0, record_missing=0, refused=0, credited=12, clean_credited=0, unequalized_credited=12, floor_cells=12, committed=null, committed_selected=null, outside_committed=null.
  credit_sum=1.2, clean_credit_sum=0.0, unequalized_credit_sum=1.2, mean_credit=0.09999999999999999, clean_mean_credit=null, unequalized_mean_credit=0.09999999999999999, floor_credit=0.09999999999999999.

    parity: 0/12 matched; committed selection unknown; mean 0.100 over 12 measured; floor 0.100 over 12 of 12 selected (excluded: 0 no golden; 0 not compared) [inputs not equalized]

### E3. Committed selection unknown: DBT not compared

Tally: selected=14, measured=0, matched=0, diverged=0, no_golden=0, not_compared=14, unmeasured=0, record_missing=0, refused=0, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=0, committed=null, committed_selected=null, outside_committed=null.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=null.

    parity: not compared: 0 measured of 14 selected (inputs cannot be equalized); committed selection unknown

### F. A run total that includes DBT's record-missing cell: the total's floor excludes it

Tally: selected=7, measured=4, matched=0, diverged=4, no_golden=0, not_compared=2, unmeasured=0, record_missing=1, refused=0, credited=4, clean_credited=0, unequalized_credited=4, floor_cells=4, committed=7, committed_selected=7, outside_committed=0.
  credit_sum=0.4, clean_credit_sum=0.0, unequalized_credit_sum=0.4, mean_credit=0.1, clean_mean_credit=null, unequalized_mean_credit=0.1, floor_credit=0.1.

    parity: 0/7 matched; selected 7 of 7 committed; mean 0.100 over 4 measured; floor 0.100 over 4 of 7 selected (excluded: 0 no golden; 2 not compared; 1 record-missing whose inputs cannot be equalized) [inputs not equalized]

### G. DBT with a record-missing and a refused cell, both excluded (committed selection unknown)

The DBT backend of S12's test
`unequalizable_record_missing_and_refused_cells_are_excluded_from_the_floor`:
one record-missing cell and one refused cell (a measured dbt row, which
`check_class` refuses). Both kinds are excluded, so the line names each.

Tally: selected=2, measured=0, matched=0, diverged=0, no_golden=0, not_compared=0, unmeasured=0, record_missing=1, refused=1, credited=0, clean_credited=0, unequalized_credited=0, floor_cells=0, committed=null, committed_selected=null, outside_committed=null.
  credit_sum=0.0, clean_credit_sum=0.0, unequalized_credit_sum=0.0, mean_credit=null, clean_mean_credit=null, unequalized_mean_credit=null, floor_credit=null.

    parity: 0/2 matched; committed selection unknown; mean n/a over 0 measured; floor n/a over 0 of 2 selected (excluded: 0 no golden; 0 not compared; 1 record-missing and 1 refused whose inputs cannot be equalized)

### H. A run total with record-missing and refused cells on both sides of the floor (committed selection unknown)

The run total of the same test: kvm has one diverged cell credited 0.1 on
unequal inputs and one record-missing cell; dbt has G's two cells. kvm's
record-missing cell is counted as 0 and dbt's two cells are excluded. The
tally does not say which kind each side's cells are, so the line gives
each side's sum.

Tally: selected=4, measured=1, matched=0, diverged=1, no_golden=0, not_compared=0, unmeasured=0, record_missing=2, refused=1, credited=1, clean_credited=0, unequalized_credited=1, floor_cells=2, committed=null, committed_selected=null, outside_committed=null.
  credit_sum=0.1, clean_credit_sum=0.0, unequalized_credit_sum=0.1, mean_credit=0.1, clean_mean_credit=null, unequalized_mean_credit=0.1, floor_credit=0.05.

    parity: 0/4 matched; committed selection unknown; mean 0.100 over 1 measured; floor 0.050 over 2 of 4 selected (counted as 0: 1 record-missing or refused; excluded: 0 no golden; 0 not compared; 2 record-missing or refused whose inputs cannot be equalized) [inputs not equalized]

## 6. Wording that does not change

- A null mean or floor prints `n/a` and is never `0.000`.
- A zero mean or floor over a nonzero denominator prints `0.000`, as in cases
  D and E.
- The headline of a run is its total's `line`.
- S12's Markdown per-backend table has these columns: `Candidate backend`,
  `Selected`, `Measured`, `Matched`, `Diverged`, `No golden`, `Not compared`,
  `Unmeasured`, `Record-missing`, `Refused`, `Mean credit (measured)`,
  `Floor credit`, `Credit inputs`. This is for information only; D5 renders
  its own panel from the JSON.
