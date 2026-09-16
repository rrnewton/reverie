# Frozen LiteInst reference and build admission — 2026-09-04

## Population and assurance

The denominator is **340**, not 253. Select every row in the pinned matrix with
`backend=ptrace`, `mode=verify`, `status=green`; join LiteInst by the exact
`(lane, category, test, mode)` tuple. Do not filter on linkage, enabled state,
measurement, or evidence availability. All 340 have a paired LiteInst row:
28 green labels, 23 red labels, 289 not-applicable labels. These labels describe
selection, not independently verified successes or failures.

Hermit: `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`.
Reverie pin: `320412c5967790939ebe405c73e394ffd9c41459`.
Matrix: `ci/compat-envelope/cells.json`, SHA-256
`a35b1ded0c09500e7a8b4e3ad0661dbf64ae95e17e7b8d364970d4827b85e5e2`.
Projection refreshed `2026-08-30T23:21:21Z`, source commit
`cd6583f954c3490aafa82718f8bbb180e2a11670` (repository unresolved).

**253/340** contain cached passing canonical-comparison summaries with nonzero
left/right INFO counts at `07c10eb80ac4fafbbe65134b504d77ef7e388d25`;
**87/340** lack that cached evidence. This is a diagnostic subset, never a new
denominator. The original strict verification JSONs were not independently
opened and authenticated here. No current-head L2 claim is made.

The owner architecture audit identifies the old CLI as mandatory ptrace
host-hybrid, with the in-process Detcore tool not wired into the staged DSO.
Thus **28/340 is only an old-route label count**. Requested in-process strict
qualification is **unknown/340**. No compiler or Hermit guest was executed in
this investigation. The only executed workload was a host-only resource probe.
Guest runs remain deferred: the typed path lacks preemption.

## Linkage, including static

Pinned manifest build metadata explicitly declares `-static` for four reference
guests: `backend-parity-c/static-nolibc-syscall-sites`,
`c-programs/hello-nostdlib`, `c-programs/pread64-nostdlib`, and
`c-programs/racewrite-nostdlib`. They remain in the full 340.

Classification: **4 static, 0 proven dynamic, 336 unknown**. This describes the
declared build metadata, not inspection of a compiled ELF. No explicit dynamic
linkage evidence was found. Absence of `-static`, a `.c` suffix, host-binary use,
or a shell interpreter is not proof of dynamic linkage. No dynamic-only success
rate is reported. Per-row flags and evidence are retained in `baseline.json`.

90% requires **306/340**, allowing at most 34 unsuccessful cells. Deferring the
four known static cells leaves a theoretical ceiling of **336/340 (98.8%)**:
not a demonstrated contradiction, and not proof of feasibility because linkage
of 336 cells remains unknown. No static rows have been removed.

## Seeded selection, not a fresh failure claim

Seed: `liteinst-baseline-20260904-v1`.
Identity: UTF-8 `lane/category/test/mode/backend`; `test` already includes its
category, so repeated category components are intentional. Rank by ascending
SHA-256 of `seed + NUL + identity`, breaking ties by identity.

Start from the 25 historical LiteInst verify red labels. Sampling eligibility
(not denominator membership) requires a paired member of the full reference,
membership in the 253 cached-evidence subset, a C guest with no explicit static
flag, and historical ptrace maximum `wall_time_max_ms <= 3000`. The C/no-static
criterion is only a selection heuristic, not proof of dynamic linkage.
Four candidates satisfy it; select the first three without replacement:

1. `portable/c-programs/c-programs/ioctl-fioclex/verify/liteinst`
2. `portable/backend-parity-c/backend-parity-c/o-tmpfile-anon/verify/liteinst`
3. `portable/backend-parity-c/backend-parity-c/record-lock/verify/liteinst`

`sample.json` retains all 25 ranks and eligibility decisions. The original seed
and list are unchanged. Timing rows span historical SHAs and are used only for
small-workload selection, not performance or current assurance claims. Their
counts/maxima/medians were independently recomputed from both monthly series
shards plus the outstanding series spool; all 25 matched the captured input.

The three selected LiteInst cells have pass-only retained rows at
`e12c472d22d2725aa1e58b7169c4a44bf82fc890` (`2026-08-30T16:25:59Z`) and
`5b811757980b9cab599206c047560a7962176a6b` (`2026-08-30T16:44:13Z`). They lack
cell-specific failing logs in those rows. Population-level historical demotion
reasons are retained per row; do not infer ioctl, tmpfile, or locking defects.

## Files and reproducibility

- `baseline.json`: summary and all 340 paired rows, zero-based raw matrix row
  indexes, build flags, manifest Git blob/content hashes, cached evidence run IDs
  and hashes. All source reads use `git show <frozen-SHA>:<path>`, not dirty files.
- `baseline.tsv`: the same 340 identities and status/linkage/evidence columns.
- `population-identities.txt`: sorted identities, LF-terminated; SHA-256
  `2cf9960cbe319f9214023d748482a6986e47f5822f728254c96ddfb7248bc530`.
- `sampling-input.json`: frozen selection inputs, original scratch-artifact
  hash, three raw source paths/hashes/timestamps, and retained duration-row path.
- `sample-observations.jsonl`: 331 compact paired sample observations, joined by
  source path, line, raw-line hash, event/run ID, source SHA and timestamp.
- `freeze.py`: metadata-only generator; uses installed Python/PyYAML, avoiding
  a Rust-script compiler bootstrap for this no-build audit. Run
  `python3 experiments/liteinst_architecture_20260904/freeze.py` from the parent.
  It asserts 340 rows, the 253 subset, exact source hash, 4/0/336 linkage counts,
  all sample ranks/eligibility, and the unchanged three selections.

This directory supersedes the preliminary `/tmp/liteinst-baseline-20260904`
reports, including their incorrect 253 denominator and inferred dynamic counts.
Raw logs and retained duration-source rows live outside the product repository,
under `/home/newton/work/dev-hermit/ignored/liteinst_architecture_20260904/`.

## Actual modest-build admission

The tracked Python `dagrun run` provides host-wide memory admission. Bare
`dagrun box` does not opt into the shared reservation. Its real command is:

```bash
parent=/home/newton/work/dev-hermit
experiment="$parent/experiments/liteinst_architecture_20260904"
export LITEINST_BUILD_CHECKOUT=/absolute/writer-owned/checkout
export LITEINST_BUILD_TARGET=/absolute/private/external/target
export LITEINST_BUILD_PACKAGE=hermit  # choose the actual package for a Reverie slot
DAGRUN_ENGINE=python "$parent/agent-utils/bin/dagrun" run \
  --dag "$experiment/compile-check.json" \
  --max-steps 1 --max-cpus 2 --max-mem 8G --admission 0 \
  --no-profile-feedback --run-timeout 960 \
  --perf-dir "$parent/ignored/liteinst_architecture_20260904/build-UNIQUE"
```

The single step prints source SHA/dirty status and runs
`cargo check --locked --offline -j2 -p "$LITEINST_BUILD_PACKAGE" --lib`, with a
900-second wall timeout, 1800 CPU-seconds, and an 8-GiB hard memory cap. These
are proposed modest iteration limits, not a measured cold-build requirement.
No compile has been launched. If an offline dependency/toolchain is missing or
the cap is insufficient, retain the refusal/failure and scope the next action;
do not bypass containment or silently increase resources. Do not use the same
target directory as another writer. A dirty iterative check is not an exact-head
validation receipt; freeze a clean source/runtime/binary pair before guest work.

Admission reserves 8 GiB through a shared flock-protected ledger. The documented
host budget is 85% of MemTotal minus min(8 GiB, MemTotal/8), also gated on live
headroom. `--admission 0` never waits indefinitely: queue/refusal exits 4;
usage exits 2; unavailable cgroup boxing exits 3. Do not set private ledger,
budget/headroom overrides, `--allow-cgroup-failure`, or `--unsafe-no-cgroups`.
`--max-cpus 2` is bandwidth, not exclusive CPU pinning.

A host-only `admission-probe.json` run using these same 8-GiB/2-CPU limits
received **GRANTED**, observed two-level cgroup-v2 containment, and exited 0.
Observed outer `memory.max=8589934592`, swap limit 0, `cpu.max=200000 100000`.
The probe reservation is released; every future build requests its own grant.
`admission.json` records exact command, timestamps, code hashes and log/profile
paths. No claim of compilation success or Hermit assurance follows from it.

Qualifying validation is a different workflow: parent `ci-hub validate-run`
enters `validate-lock` (six validation slots; benchmarks reserve all), with
source/service provenance requirements. Do not relabel a standalone compile
as that receipt. Several parent skill links are broken; commands above were
checked against actual `agent-utils/py/dagrun/cli.py`, the user guide and live
help, rather than treating missing links as a resource blocker.
