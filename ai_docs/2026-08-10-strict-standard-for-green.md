# Strict standard for a green compatibility-scorecard cell

**Task:** `write-the-strict-standard-for-green`

**Decision date:** 2026-08-10

**Scope:** the current compatibility scorecard, after selecting one row per
`(program, backend)` cell. Here `program` is the CSV `test_id` and `backend` is
the CSV `backend`.

This is an application standard, not a new qualification policy. It records
the owner's already-decided base-green, optional-evidence, counting, and
reporting rules so two readers reach the same verdict from the same rows.

The owner's definition is the authority and is reproduced verbatim:

> **"GREEN MEANS WE RAN TWICE (DETERMINISM OR PARITY) AND GOT FULL RUNS WITH IDENTICAL OUTPUT."**

This is binary. If either run is incomplete, absent, or crashes, or if the
comparison errors or produces no result, the cell is **red**. A requested mode
or a partially produced artifact cannot turn any of those cases green.

## The decision procedure

Apply these steps in order. Do not count or classify the stored rows before
step 1.

1. **Select the current row.** For each `(test_id, backend)`, retain the last
   physical row in append order. Do not sort `run_id` or timestamps and do not
   count an earlier row. Report stored rows `S`, current cells `T`, and
   superseded rows `S - T` separately. A change from `S` to `T` is a counting
   correction, neither progress nor regression.
2. **Apply the binary verdict.** The selected row is base green exactly when it
   records the owner's definition: two complete runs on the declared
   determinism-or-parity axis and an identical-output verdict. In the current
   schema this requires all of the following:
   - `outcome` is `pass`;
   - `comparison_tier` is `full-stdout-info-stack-heap` or
     `stdout-info-stack-heap-spot-check`; and
   - the row carries the qualifying tier's real comparison evidence, rather
     than only the tier label. For the current stdout/INFO tiers that means a
     matched stdout comparison and a nonempty INFO-log comparison.

   The producer may write `pass` only after both full runs and the comparison
   finish. One side emitting nothing, either run dying before comparison, or
   the comparator erroring is red. A raw pass with a blank, malformed, unknown,
   `legacy-unqualified`, or other `unqualified-*` tier is also red because it
   does not establish the required two-run comparison. A qualifying tier label
   without its evidence is red; labels do not certify themselves.
3. **Grade run mode separately.** Despite their historical names,
   `stack_parity` and `heap_parity` record whether the corresponding flags were
   passed. They are inputs/modes, not comparison outcomes, and never determine
   base green. A cell is **highest-grade green** exactly when it is base green
   by step 2 and both stronger-mode fields are `TRUE`.

The words `stack`, `heap`, and `parity` in legacy field/tier names do not
override step 3. A `TRUE` mode says only that the flag was passed. It never says
the run completed or the output matched.

## Three-valued run-mode fields

Read `stack_parity` and `heap_parity` independently:

| Stored value | Meaning | Counts as stronger mode? |
| --- | --- | --- |
| `1` / `true` | the corresponding flag was passed | yes |
| `0` / `false` | the corresponding flag was not passed | no |
| blank | an older row does not say whether the flag was passed | no; unknown mode is not stronger mode |

Never rewrite a historical blank to false or true. Never collapse false and
blank in a table, JSON result, total, or explanation. New rows know whether a
flag was passed and therefore must not use blank. These three values classify
only mode. The verdict itself is binary: two full runs with identical output is
green; every incomplete, absent, crashed, divergent, or errored comparison is
red.

## Highest-grade-green query

This reference query implements only the decided rules above. It binds
"newest" to physical append order and prints every denominator:

```python
import csv

QUALIFYING = {
    "full-stdout-info-stack-heap",
    "stdout-info-stack-heap-spot-check",
}

def true(value):
    return (value or "").strip().lower() in {"1", "true"}

def nonempty_comparison(value):
    value = (value or "").strip().lower()
    return value not in {"", "-", "n/a", "none", "null", "0", "0|0", "0/0"}

# The append-only history is the counting source. scorecard.csv is now the
# deduplicated projection and cannot reveal how many rows it superseded.
with open("compat-envelope/history/scorecard-observations.csv", newline="") as source:
    stored = list(csv.DictReader(source))

current = {}
for row in stored:
    current[(row["test_id"].strip(), row["backend"].strip())] = row

base_green = [
    row for row in current.values()
    if row["outcome"].strip() == "pass"
    and row["comparison_tier"].strip() in QUALIFYING
    and true(row.get("stdout_parity"))
    and nonempty_comparison(row.get("compared_log_messages"))
]
highest_grade = [
    row for row in base_green
    if true(row.get("stack_parity"))
    and true(row.get("heap_parity"))
]
red = [row for row in current.values() if row not in base_green]
unknown_stack_mode = [
    row for row in current.values()
    if not (row.get("stack_parity") or "").strip()
]
unknown_heap_mode = [
    row for row in current.values()
    if not (row.get("heap_parity") or "").strip()
]

print(f"stored rows: {len(stored)}/{len(stored)}")
print(f"current cells: {len(current)}/{len(stored)} stored rows")
print(f"superseded rows: {len(stored) - len(current)}/{len(stored)}")
print(f"base green: {len(base_green)}/{len(current)} current cells")
print(f"red: {len(red)}/{len(current)} current cells")
print(f"highest-grade green: {len(highest_grade)}/{len(base_green)} base-green cells")
print(f"unknown stack mode: {len(unknown_stack_mode)}/{len(current)} current cells")
print(f"unknown heap mode: {len(unknown_heap_mode)}/{len(current)} current cells")
```

The query is intentionally not a producer and does not modify the scorecard.
If the highest-grade denominator is zero, report `0/0 (no base-green
population)`; do not render it as a 0% failure rate.

## Mandatory quoting contract

A green headline is quotable only when it states all of the following:

- generation UTC, artifact path or URL, artifact content SHA, and selection
  scope;
- stored/current/superseded counts: `S/S`, `T/S`, and `(S-T)/S`;
- base green `G/T` and red `R/T`, never a bare numerator or percentage, with
  `G + R = T`;
- highest-grade green `H/G`, including both terms even when either is zero;
- unknown stack mode `US/T` and unknown heap mode `UH/T`, kept separate from
  the binary green/red verdict;
- hosted-authority evidence coverage over `T`, or an explicit statement that
  the scorecard carries no such binding;
- product result and gate/producer health as separate states; and
- `NORMAL` or `DEGRADED`, with the outage/cache/provenance cause when degraded.

Never move a blank mode into the green numerator, and never describe it as a
red outcome: it says only that the historical run mode is unknown. A gate green
is not a product green, and a product green does not assert that the gate or
producer is healthy.

Template:

> As of `<UTC>`, `<artifact>@<content-sha>` in `<scope>` contains `S/S` stored
> rows, projected to `T/S` current cells with `(S-T)/S` superseded. `G/T` are
> green and `R/T` are red (`G+R=T`). Highest-grade green is `H/G`; mode evidence
> is unknown for stack on `US/T` and heap on `UH/T`. Hosted authority evidence
> covers `<A/T or explicit unavailable>`. Product=`<state>`;
> gate/producer=`<state>`; reporting=`NORMAL|DEGRADED: reason`.

## Cold-application fixtures

The historical cases below are from committed
`compat-envelope/scorecard.csv` at parent
`43e5bb73276f7d6c3fac56822c152c44a09940f0`, content SHA-256
`d26423d4088aac6235025b7e6cecb5a8d7c8eefd6c54c36a7dbed1707de9a34c`;
line numbers include the header. They exercise absence and deduplication
without changing the artifact.

| Case | Input | Required result |
| --- | --- | --- |
| historical unknown mode and unqualified verdict | line 2, `applications/timed-progress-bar` / `ptrace`: `outcome=pass`, `comparison_tier=legacy-unqualified`; the historical schema has neither mode column | red: the row does not prove two full matching runs; stack and heap modes are both unknown; not highest-grade |
| superseded duplicate | lines 26 (`test_mode=chaos`) and 27 (`test_mode=verify`), `determinism-stress/order-violation` / `ptrace` | line 27 is current solely because it is physically last; line 26 is superseded and is not a current cell; count one cell, not two |
| legacy-shaped base-only positive control | two full runs, `outcome=pass`, qualifying tier with matched stdout and nonempty INFO comparison, both historical mode fields blank | base green; two historical unknown modes; not highest-grade. A new producer must record both modes rather than publish blanks |
| highest-grade positive control | same complete matching pair, `stack_parity=true`, `heap_parity=true` | base green and highest-grade green; both stronger flags were passed |
| explicit lower mode | same complete matching pair, `stack_parity=false`, `heap_parity=true` | base green; not highest-grade; stack mode false is not stack mode unknown |
| stronger-mode divergence | two full runs complete but output differs, `outcome=fail`, both mode fields true | red; not highest-grade; true modes do not override the verdict |
| stronger-mode absence | both flags passed, but one side emits no comparison evidence | red; true modes, failed verdict, not highest-grade |
| interrupted or errored comparison | either run dies before compare, or the comparator errors | red regardless of mode; not highest-grade |

The committed population used to choose the two historical cases contains
`624/624` stored data rows, `541/624` current `(test_id, backend)` cells, and
`83/624` superseded rows. The naive stored-row count overstates the current
cell population by `83/541` (about 15%). The bound historical schema has no
stack or heap columns, so all `624/624` rows carry unknown historical memory-mode
evidence. An absent historical mode column has the same mode classification as
a blank field: unknown, not stronger mode and not weaker mode. It does not
change the row's binary green/red verdict.

This document is complete only after a reader who did not write it applies the
table above and reports the same eight results.
