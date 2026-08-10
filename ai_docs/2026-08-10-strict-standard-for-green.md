# Strict standard for a green compatibility-scorecard cell

**Task:** `write-the-strict-standard-for-green`

**Decision date:** 2026-08-10

**Scope:** the current compatibility scorecard, after selecting one row per
`(program, backend)` cell. Here `program` is the CSV `test_id` and `backend` is
the CSV `backend`.

This is an application standard, not a new qualification policy. It records
the owner's already-decided base-green, optional-evidence, counting, and
reporting rules so two readers reach the same verdict from the same rows.

## The decision procedure

Apply these steps in order. Do not count or classify the stored rows before
step 1.

1. **Select the current row.** For each `(test_id, backend)`, retain the last
   physical row in append order. Do not sort `run_id` or timestamps and do not
   count an earlier row. Report stored rows `S`, current cells `T`, and
   superseded rows `S - T` separately. A change from `S` to `T` is a counting
   correction, neither progress nor regression.
2. **Apply base green.** The selected row is base green exactly when:
   - `outcome` is `pass`; and
   - `comparison_tier` is `full-stdout-info-stack-heap` or
     `stdout-info-stack-heap-spot-check`.

   A raw pass with a blank, malformed, unknown, `legacy-unqualified`, or other
   `unqualified-*` comparison tier is not green. A qualifying tier beside an
   outcome other than `pass` is not green. Missing or unrecognized values are
   undetermined, never green. This is the predicate already used by the
   renderer; this document does not add another channel, gate, or oracle.
3. **Grade optional memory evidence separately.** `stack_parity` and
   `heap_parity` never determine base green. They only determine whether a base
   green cell belongs to the higher grade. A cell is **highest-grade green**
   exactly when it is base green and both optional fields are `TRUE`.

The words `stack` and `heap` in today's recognized comparison-tier names do not
override step 3. Under the owner's current decision the two explicit parity
columns are optional grade evidence, not base-qualification gates.

## Three-valued optional fields

Read `stack_parity` and `heap_parity` independently:

| Stored value | Meaning | Counts as optional pass? |
| --- | --- | --- |
| `1` / `true` | the comparison was measured and matched | yes |
| `0` / `false` | the comparison was measured and diverged, **or** its flag was not passed on a new run | no |
| blank | historical evidence does not say whether the flag was passed | no; unknown is not a pass |

Never rewrite a historical blank to false or true. Never collapse false and
blank in a table, JSON result, total, or explanation. New rows know whether a
flag was passed and therefore must not use blank merely because it is
convenient.

One state remains **unrepresentable pending an owner decision**: a new run is
known to have passed the stack or heap flag, but the requested comparison
produces no verdict. It is not measured-and-matched, not
measured-and-diverged, not flag-omitted, and not unknown whether the flag was
passed. A producer must refuse to publish that field rather than invent
`TRUE`, `FALSE`, or blank. This gap is recorded on the owning task; this
standard deliberately does not decide it.

## Highest-grade-green query

This reference query implements only the decided rules above. It binds
"newest" to physical append order and prints every denominator:

```python
import csv

QUALIFYING = {
    "full-stdout-info-stack-heap",
    "stdout-info-stack-heap-spot-check",
}

with open("compat-envelope/scorecard.csv", newline="") as source:
    stored = list(csv.DictReader(source))

current = {}
for row in stored:
    current[(row["test_id"].strip(), row["backend"].strip())] = row

base_green = [
    row for row in current.values()
    if row["outcome"].strip() == "pass"
    and row["comparison_tier"].strip() in QUALIFYING
]
highest_grade = [
    row for row in base_green
    if row.get("stack_parity", "").strip().lower() in {"1", "true"}
    and row.get("heap_parity", "").strip().lower() in {"1", "true"}
]

print(f"stored rows: {len(stored)}/{len(stored)}")
print(f"current cells: {len(current)}/{len(stored)} stored rows")
print(f"superseded rows: {len(stored) - len(current)}/{len(stored)}")
print(f"base green: {len(base_green)}/{len(current)} current cells")
print(f"highest-grade green: {len(highest_grade)}/{len(base_green)} base-green cells")
```

The query is intentionally not a producer and does not modify the scorecard.
If the last denominator is zero, report `0/0 (no base-green population)`; do
not render it as a 0% failure rate.

## Mandatory quoting contract

A green headline is quotable only when it states all of the following:

- generation UTC, artifact path or URL, artifact content SHA, and selection
  scope;
- stored/current/superseded counts: `S/S`, `T/S`, and `(S-T)/S`;
- measured base-verdict coverage `K/T`;
- base green `G/K`, not a bare `G` or percentage;
- undetermined `U/T` and the reasons, where `K + U = T`;
- highest-grade green `H/G`, including both terms even when either is zero;
- hosted-authority evidence coverage over `T`, or an explicit statement that
  the scorecard carries no such binding;
- product result and gate/producer health as separate states; and
- `NORMAL` or `DEGRADED`, with the outage/cache/provenance cause when degraded.

When `U > 0`, use the words **"known green plus unknown cells"**. Never present
`G` as the total product truth. A low green or red count during an outage means
coverage was lost until the denominators prove otherwise. A gate green is not a
product green, and a product green does not assert that the gate or producer is
healthy.

Template:

> As of `<UTC>`, `<artifact>@<content-sha>` in `<scope>` contains `S/S` stored
> rows, projected to `T/S` current cells with `(S-T)/S` superseded. Base
> verdicts are determined for `K/T`; `G/K` are green and `U/T` are
> undetermined (`<reasons>`). Highest-grade green is `H/G`. Hosted authority
> evidence covers `<A/T or explicit unavailable>`. Product=`<state>`;
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
| historical unknown | line 2, `applications/timed-progress-bar` / `ptrace`: `outcome=pass`, `comparison_tier=legacy-unqualified`; the historical schema has neither optional column | not base green; absent historical stack and heap columns both mean unknown; not highest-grade |
| superseded duplicate | lines 26 (`test_mode=chaos`) and 27 (`test_mode=verify`), `determinism-stress/order-violation` / `ptrace` | line 27 is current solely because it is physically last; line 26 is superseded and is not a current cell; count one cell, not two |
| base-only positive control | `outcome=pass`, qualifying tier, both optional fields blank | base green; two optional unknowns; not highest-grade |
| highest-grade positive control | `outcome=pass`, qualifying tier, `stack_parity=true`, `heap_parity=true` | base green and highest-grade green |
| explicit lower grade | `outcome=pass`, qualifying tier, `stack_parity=false`, `heap_parity=true` | base green; not highest-grade; stack false is not stack unknown |
| raw failure control | `outcome=fail`, qualifying tier, both optional fields true | not base green and therefore not highest-grade |

The committed population used to choose the two historical cases contains
`624/624` stored data rows, `541/624` current `(test_id, backend)` cells, and
`83/624` superseded rows. The naive stored-row count overstates the current
cell population by `83/541` (about 15%). The bound historical schema has no
stack or heap columns, so all `624/624` rows carry unknown historical memory
evidence. An absent historical column has the same verdict as a blank field:
unknown, not `624/624` measured failures and not `624/624` passes.

This document is complete only after a reader who did not write it applies the
table above and reports the same six results.
