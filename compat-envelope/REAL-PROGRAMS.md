# Real programs in the compatibility envelope

`real-programs-scorecard.csv` is the compatibility-envelope view of Hermit's
backend-agnostic real-program matrix. It closes the two-axis inversion:

- the e2e envelope has a broad backend axis but only 17/235 real-world cells;
- the legacy compatibility corpora have 214/214 real programs but separate,
  incomplete backend-specific views.

The source matrix is produced in the Hermit repository from one 214-program
corpus. Every program is driven through the same user-facing command structure
for every member of `Backend::ALL`; only the backend token changes. The parent
adapter does not run guests. It validates and translates that artifact into the
canonical scorecard schema with `bucket=real-programs`.

## Evidence boundary

The ingest command is:

```sh
compat-envelope/ingest_real_program_matrix.py \
  --matrix hermit/ignored/compat/backend-matrix.csv \
  --metadata hermit/ignored/compat/backend-matrix.metadata.json \
  --corpus hermit/ci/compat/corpus.json \
  --annotations compat-envelope/real-program-annotations.json \
  --output compat-envelope/real-programs-scorecard.csv
```

The metadata must use schema
`hermit-compat-backend-matrix-metadata/v1` or `/v2`, name matrix schema
`hermit-compat-backend-matrix/v1` or `/v2`, carry the matrix SHA-256, exact clean
Hermit and locked Reverie SHAs, run identity/time, the bound corpus SHA-256, and
the exact front-door profile.
Ingest refuses dirty, unbound, incomplete, duplicated, or non-Cartesian input.
The default population is exactly 214 programs × 6 backends = 1284 cells.
The bound profile is the literal shared structure
`hermit --backend <backend> run --strict --verify --verify-strict --verify-json
<verdict-json> -- <program-argv>`; a backend-specific test binary is refused.

The parent artifact aliases the product token `dbt` to the established
scorecard token `dbi`; the mapping is explicit in the adapter. No other backend
name is rewritten.

Matrix schema v1 carries only within-backend strict verification. Matrix schema
v2 may additionally carry a per-row `comparison_tier`, stdout parity verdict,
both SHA-256 operands, and comparator contract. The importer re-derives the
verdict from the operands and refuses a tier label without its evidence. This
keeps the narrower but real ptrace-vs-DBI stdout witness as
`unqualified-stdout-only`; it is never downgraded to self-verification merely
because another producer lacks that witness. The separate
`collect-dbi-corpus.rs` emitter owns those DBI measurements and is not duplicated
here.

## State and denominator contract

The source has five emitted values implementing the four-state rule:

- `PASS`
- `FAIL` or `TIMEOUT`
- `ATTEMPTED_UNQUALIFIABLE`
- `NOT_ATTEMPTED(reason)`

`NOT_ATTEMPTED` must carry a reason and never enters an attempted denominator.
`ATTEMPTED_UNQUALIFIABLE` records a real run but never enters a passing
numerator. A failed or timed-out run can never be rewritten as absence.

Every backend summary carries both terms and names the narrower numerator:

- `SOURCE_SELF_VERIFY_PASS / ATTEMPTED`
- `ATTEMPTED / CORPUS`

A source self-verify pass is not presented as cross-backend scorecard green.
A percentage without both terms is not published. This matters because an
apparently low rate can coexist with the largest absolute leading-edge count.

Metadata v2 also carries a run-level `evaluation` receipt bound to the exact
Hermit head and observation time. `EVALUATED` requires real result rows. An
explicitly observed `NOT_ATTEMPTED` receipt requires a reason and evidence
source (for example, an exact-head workflow query reporting zero runs); its
matrix is header-only and the adapter synthesizes the full 214 × 6 population
as `NOT_ATTEMPTED`. The output repeats `evaluation_state`,
`evaluation_reason`, and `evaluation_evidence` on every row. Thus a head that
was never evaluated is neither blank nor indistinguishable from an evaluated,
clean head. A header-only matrix claiming `EVALUATED` remains refused.

## Coverage and workload-class axes

The output appends classification columns after the canonical scorecard core.
`source_legacy_modes`, its count, and `source_legacy_coverage` make the old
per-corpus holes explicit. In the measured 214-program population, 25/214
labels came from exactly one legacy corpus; that is historical coverage, not a
program capability claim. Across the four old modes the larger quantity is
107/856 unexplained absent mode×program cells: strict 23/214, SaBRe 2/214,
e9patch 42/214, and record/replay 40/214. The ingest summary carries all four
terms rather than turning a missing legacy row into a program property.

`program_classes`, `concurrency_kind`, `program_class_evidence`, and the
SHA-256 `classification_id` preserve behavior that a scan of argv misses. The
initial bound annotations record the measured seven-task `node` guest and the
multi-process `curl-localhost`/`wget-localhost` server paths. The importer emits
both ratio terms for each annotated class, so a scheduling regression appears
under `class=concurrency`, not only as an unexplained runtime or server red.

The bound canonical corpus also supplies `harness_form` as `direct`,
`shell-wrapped`, or `workload-script`, plus a `corpus_id`. A stable failure in a shell or workload
wrapper therefore remains attributable to that invocation form; it is not
silently presented as proof that the named utility itself failed.

## Certification boundary

Hermit matrix-v1 source `PASS` proves strict within-backend self-verification.
It does not prove ptrace-vs-backend parity, so those v1 attempts use
`comparison_tier=unqualified-self-verify-only` and render
`ATTEMPTED_UNQUALIFIABLE` in the cross-backend scorecard. That rule is scoped to
v1 rows, not generalized to every producer. Matrix-v2 rows carry their truthful
per-row tier; a bound ptrace stdout witness remains `unqualified-stdout-only`.
The adapter leaves exit-code, detlog, and oracle parity blank until those
channels have their own reference operands.
