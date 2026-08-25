# External-program compatibility repeat sweep (214 x 6 x 5)

This directory is the durable source for population
`compat-repeat-sweep/external-programs-v1/214x6x5`. It preserves five raw
compatibility sweeps, the exact classifier, the exact corpus and matrix-driver
sources, and the derived classification that was previously available only in
an ignored worktree directory.

The headline partition is **495 green + 348 stable fail + 118 unstable + 323
no verdict = 1,284 cells**. These four categories are disjoint and exhaustive;
run `python3 verify_artifact.py` to check hashes, key uniqueness, the partition,
and byte-for-byte regeneration of the derived files.

## Correction: defects are attributed by root executable, not by label

The first publication decided attribution by comparing `Path(argv[0]).name` to
the corpus **label**. Labels are scenario names chosen by the corpus author --
`mpstat-softirqs`, `sar-resource-tables` -- so any scenario whose name differed
from its binary was filed as `shell-or-launcher-wrapped`, i.e. as a defect that
could not be blamed on a named program, even though `argv[0]` named a real
executable outright.

Attribution is now taken from `argv[0]`. A root in `WRAPPER_ROOTS`
(`bash`, `sh`, `env`) or the `real_compat_workload.sh` driver remains
**unattributable**, because what a wrapper went on to `exec` is not recoverable
from `argv` -- that is a genuine unknown and is reported as one.

The measured cell population is unchanged: **1,284 unique cells x 5
observations = 6,420**, and the headline partition above is unchanged. What
moves is the stable-GAP attribution split:

| stable GAP attribution | before | after |
| --- | ---: | ---: |
| named-program-direct | 17 | **23** |
| shell-or-launcher-wrapped | 221 | **215** |
| workload-script | 110 | 110 |
| total | 348 | 348 |

Exactly six cells are re-attributed, all of them stable failures that always
named a real binary:

| cell | root executable |
| --- | --- |
| `mpstat-softirqs` / `kvm` | `mpstat` |
| `pidstat-disk` / `kvm` | `pidstat` |
| `sar-resource-tables` / `liteinst` | `sar` |
| `sar-resource-tables` / `sabre` | `sar` |
| `sysctl-random-uuid` / `kvm` | `sysctl` |
| `vmstat-disk` / `kvm` | `vmstat` |

**The remaining 325 wrapper-attributed cells are never reported as healthy.**
215 shell-or-launcher-wrapped plus 110 workload-script are stable failures with
no nameable program; `wrapper_unattributable_unknown: 325` in
`results/stability-5.json` records them as unknown, which is the honest verdict.
Counting them as clean is the failure this correction exists to prevent.

One deliberate consequence: `bash` invoked as the subject of a test is now
unattributable, because `argv[0]` cannot distinguish it from `bash` acting as a
launcher. The old rule special-cased it as direct; attribution by root
executable cannot make that distinction and does not pretend to.

### This tooling has a caller

The analyzer and `verify_artifact.py` previously had **none** -- nothing in the
parent invoked either, so a drifted classifier or a stale republished CSV could
not be detected. `tools/test_analyze_repeat_stability.py` brackets the rule from
both sides and invokes `verify_artifact.py` end to end, and the suite is
registered in `ci-hub/tests/run_python_suites.py` with a counted floor, so the
CI runner executes it.

## Provenance

- Population ID: `compat-repeat-sweep/external-programs-v1/214x6x5`
- Run host: `devbig014` (short hostname)
- Run interval: 2026-08-10 through 2026-08-11 UTC; the fifth sweep completed
  at `2026-08-11T06:11:42Z` and its producer finished at
  `2026-08-11T06:14:44Z`.
- Hermit: `5165ca03d67dc035b618a599e6c8350f46200fe0`
- Reverie: `99437f05e82377a80ad1edb9e501d89a38c91ecb`
- Matrix driver at the final attempt:
  `e68b24bdf6523e57d51ac5a6487c2a304107c686`
- The complete file and runtime-input hashes are in `metadata.json`.
- **`provenance/producer.log` does not exist and never did.** `metadata.json`
  listed it in `file_sha256`, so `verify_artifact.py` raised `FileNotFoundError`
  before checking anything -- a broken verifier that read as an unrun one. It is
  absent from every ref including the publication commit (`git log --all --
  '*/provenance/*'` returns nothing), so there is no authoritative copy to
  restore and none has been fabricated. The claim is removed; the removal and
  the hash it formerly asserted are recorded in `metadata.json` under
  `provenance_claim_correction`.
- The archived `tools/run-combined-original.sh` retains the producing machine's
  absolute paths intentionally; it is provenance, not a claim of portable
  execution.

This is historical, branch-specific evidence. The Hermit SHA is not an
ancestor of current Hermit main; it is three commits after its historical base
`b5a4748e87a6bc32bc3d87935ab571c1c57c25ad`. The Reverie SHA is historical.
Therefore this artifact does **not** establish current-main compatibility.

## Methods

The corpus contains 214 external-program labels. Each sweep evaluates one cell
for every `(program, backend)` pair across six backends: `ptrace`, `dbt`,
`liteinst`, `sabre`, `kvm`, and `e9patch`. Five completed sweeps yield five
observations per one of the 1,284 unique cells. The raw outputs are
`inputs/sweep-1.csv` through `inputs/sweep-5.csv`.

`tools/analyze_repeat_stability.py` joins the five sweeps by `(program,
backend)`, refuses incomplete or duplicate inputs, records the five-observation
sequence, and emits the files under `results/`. The six backend smoke checks
were green before the recorded attempts. Each matrix cell had a 90-second wall
bound; timeout is an observed outcome, not a pass.

The binaries used to run the campaign are deliberately not committed. Their
SHA-256 digests are retained in `metadata.json`; generated binaries and build
outputs are outside the repository's artifact policy.

## Evaluation

The published four-way interpretation is:

- `GREEN`: `repeat_stability=CONSISTENT` and `dominant_state=PASS`.
- `STABLE_FAIL`: `repeat_stability=CONSISTENT` and `dominant_state=FAIL`.
- `UNSTABLE`: every `repeat_stability=FLAKE` row, regardless of dominant state.
- `NO_VERDICT`: consistent `TIMEOUT`, `ATTEMPTED_UNQUALIFIABLE`, or
  `NOT_ATTEMPTED` rows.

This interpretation produces exactly one class per cell. The verifier also
requires 214 distinct programs, six distinct backends, 1,284 unique keys, five
runs per row, and counter sums of five.

## Results

| Classification | Cells | Denominator | Rate |
|---|---:|---:|---:|
| Green | 495 | 1,284 | 38.55% |
| Stable fail | 348 | 1,284 | 27.10% |
| Unstable | 118 | 1,284 | 9.19% |
| No verdict | 323 | 1,284 | 25.16% |

`NO_VERDICT` consists of 43 consistent timeouts, 276 consistent attempted but
unqualifiable results, and four consistent not-attempted results.

## Population boundary: not the owner's 1,200-cell scorecard

Do not compare or combine this result with the historical 1,200-cell scorecard
as if they shared a denominator. That scorecard is 200 Hermit `test_id`
workloads x six backends at Hermit
`82a8e853357584a3a567fd80812e015572a607c7` and Reverie
`a4f33d69a56ed4233a53b218c39d93807ffc8cd0`. This campaign is 214 merged
external-program labels x six backends at the SHAs above, with five repeated
observations and a different comparator. Exact identifier intersection is
**0**; basename-normalized intersection is only `uname` (1). The populations
are neither additive nor valid numerator/denominator substitutes.
