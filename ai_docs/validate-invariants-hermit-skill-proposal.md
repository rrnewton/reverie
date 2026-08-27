# PROPOSAL — Hermit-level VALIDATE TOOL invariants skill

Status: PROPOSED, not landed. Delivered as a diff for the owner to place under
`hermit/.llms/skills/` (the tracked Hermit skill dir; `hermit/.claude/skills/`
mirrors it). Per the task caveat, the owner places/edits canonical product-repo
skills; this file is the proposed content plus its provenance.

Companion (already landed in the parent): coordinator/orchestrator behavior lives
in `dev-hermit/.claude/skills/validate-orchestrator-discipline/SKILL.md` (core-memory
`validate-orchestrator-discipline`). **No symlink sharing** between the parent
skill dir and Hermit's skill dir — the two cross-reference by explicit link only.

Every claim below was verified 2026-08-03 against `worktrees/226v/hermit`
(`validate.sh`, `ci/…`, `.github/workflows/…`, `detcore/…`) and parent
`ci-hub/…`; file:line citations are inline. Where a popular belief was found
FALSE, it is marked so the skill does not re-assert a myth.

---

## Proposed skill body (`validate-invariants.md`)

> **description:** The invariants the VALIDATE tool actually enforces — and the
> ones it does NOT. Read before asserting what `validate.sh` / the CI DAG runner
> guarantees, before adding a gate, or before explaining a green/red result.
> Coordinator/orchestrator behavior is a SEPARATE skill:
> `dev-hermit/.claude/skills/validate-orchestrator-discipline/SKILL.md` (no symlink
> share; explicit cross-link).

### 1. Commit anchoring — records the SHA; does NOT gate on a clean tree
`validate.sh` records `VALIDATION_COMMIT=$(git rev-parse HEAD)` (`validate.sh:341`)
into the JSONL run ledger (`:836`) and the PR comment (`:3280`, `:3397`). It runs
regardless of working-tree cleanliness: there is **no** dirty-tree refusal and
**no** `--run-on-dirty-tree` flag (arg parser `:142-249`; unknown args `exit 2`).
`git_ahead`/`git_behind` are computed (`:345-348`) but never gate execution.
→ Report results bound to the recorded SHA, but do not claim the tool refuses a
dirty tree.

### 2. Affected-test selection — fail-OPEN to FULL; default is FULL in validate.sh
- Default level is **FULL**: `VALIDATION_LEVEL=${VALIDATE_LEVEL:-full}`
  (`validate.sh:102`). Selection is opt-in via `--selective` / `--since-green`
  (`:171`). Selection is the *default* only in the GitHub portable workflow,
  which calls `ci/select-tests.rs` on every PR (`.github/workflows/ci-portable.yml:131`).
- **Fail-open is sacred:** `run_selective_suite` (`:3585-3644`) runs the FULL lane
  on select-tests error (`:3597`), empty subset (`:3615`), un-buildable subset
  (`:3621`), or any decision that is not exactly `skip`/`selective` (`:3639-3641`).
  CI mirrors it: `emit_full "select-tests.rs error"` (`ci-portable.yml:133`).
- Heavy jobs gate on `needs.select.outputs.X != 'false'` (`ci-portable.yml:244,
  377, 439, 504, 546, 652, 720`) — the `!= 'false'` form is deliberate so a
  missing/unknown decision runs the job, never skips it.
- Files: `ci/select-tests.rs`, `ci/test-footprints.json`.
→ A "skip" is only ever honored on an explicit affirmative decision; anything
uncertain runs FULL. Never "optimize" this into fail-closed.

### 3. Run ledger & scope — `locally-validated`, not `all-tests-passed`
Each run appends profile+result+commit+timings to a JSONL ledger
`ignored/validate-run-ledger.jsonl` (`validate.sh:338-339`, `:831-841`). A green
FULL run applies the **`locally-validated`** PR label (`:3330`) — the substitute
for green CI. There is **no** `all-tests-passed` label and **no** incremental
chain-depth counter (neither string exists). Per-test-run tagging does not carry
a total/incremental scope distinction.

### 4. Timeouts are WALL-CLOCK; there are NO CPU-time budgets
Every DAG node uses a wall `"timeout"` key; **zero** nodes carry `cpu_timeout`
(`ci/dag/portable.json`, `ci/dag/privileged.json`;
`jq '[.steps[]|select((.cpu_timeout//0)>0)]|length'` = 0 on both). `validate.sh`
gate timeouts are wall-clock process-tree kills: `GATE_TIMEOUT_SECONDS`
(default 600, override `VALIDATE_GATE_TIMEOUT_SECONDS`, `:312`) enforced by
`kill_process_tree` TERM/KILL (`:768-778`). No `RLIMIT_CPU`/`prlimit` anywhere.
→ If a directive says "CPU-time timeouts are set," that is NOT in effect today
(see orchestrator-discipline rule 6). Do not describe the timeouts as CPU-based.

### 5. There is NO performance ratchet
`--perf-dir` produces profiling CSVs (`ci/run-dag.sh:19`, privileged/manual
workflows), aggregated by `ci-hub/validate/aggregate.py:248-309` — but purely as
**observability**. There is no timing baseline, threshold, alarm, or commit
attribution. The only "regression" gates are count-based: `--envelope-compare`
count monotonicity (`validate.sh:3311`) and the compat ratchets (§7).
→ Do not claim validate.sh catches perf regressions; it measures, it does not gate.

### 6. Flaky-is-RED is a real trinary classifier
`ci-hub/stress/matched-burst.sh:84-95` maps a raw exit-code file
(`124`=hang, `0`=pass, else=other) to CLEAN / FLAKY / FAILING, mixed ⇒ **FLAKY**.
`stress/README.md:7-8`: anything other than 0% or 100% is FLAKY and FLAKY IS RED
(29/30 must ALARM, not round to green). `stress_store.py:152,276` sets
`alarm = verdict in RED_VERDICTS` and exits 2 (P0) on any non-CLEAN.
→ Treat any pass ratio strictly between 0% and 100% as RED, never a footnote.

### 7. Honest count ratchets — asserted count guarded against the real array
`RR_COMPAT_EXPECTED=139` (`validate.sh:625`) is enforced against the label array
size at `:727-730` (`if ((${#RR_COMPAT_PASSING_LABELS[@]} != RR_COMPAT_EXPECTED)) … exit 2`).
Same pattern: `SABRE_COMPAT_EXPECTED=207` (`:629`), `STRICT_COMPAT_TOTAL=191`
(`:616`). Provenance is documented in-file: the g++/ar/strip/gprof/gcov
known-failures block `RR_COMPAT_KNOWN_FAILURES` (`:689-695`) with per-program
divergence reasons, and the 144→139 derivation (`:617-624`).
→ Preserve the guard and the provenance comments; never bump a count without
adjusting the array and the documented reason.

### 8. Never-blind wall+CPU + honest cost estimate
`print_wall_cpu_summary` (`validate.sh:878-905`) prints
`Elapsed: wall … | CPU … (user …, sys …) | CPU/wall …x across N cores` and is
called from the `cleanup` EXIT trap (`:949`, trap set `:959`), so it fires on
success, failure, timeout, and interrupt. The pre-run estimate is derived from
history: `history_estimate` (`:466-550`) reads the ledger for a median/range over
prior same-profile passing runs bucketed by cache state, or prints an honest
"insufficient history" message — the header comment (`:285-290`) explicitly
rejects a static guess. Per-NODE CPU exists only when a runner path passes
`--perf-dir` (privileged/manual only), NOT in local default or GitHub portable.

### 9. Four-layer architecture (with the real nuances)
- (a) `ci/run-dag.sh:103` `exec "$runner" "$verb" --dag "$dag" "$@"` forwards
  `-j/--perf-dir/--cgroups/--max-mem` to `safe-ci-dag-runner run`, which schedules
  the graph. Nuance: the locally-built **Rust** runner leaves cgroups/perf
  UNIMPLEMENTED; only the Python impl honors them, and only when flags are passed.
- (b) `validate.sh` drives it: `run_ci_manifest_lane` (`:3515-3523`) runs one
  manifest-validate then `./ci/run-dag.sh <lane> -j <jobs> -v`; full = both lanes
  (`:3740-3743`).
- (c) Test definitions are shared TOML manifests `tests/e2e/manifests/*.toml`,
  planned by the typed Rust `hermit-manifest-plan` crate (`ci/test_harness.sh:128`,
  `ci/manifest-plan/`). The DAG *graph* is JSON in `ci/dag/*.json`; the
  *definitions* are the TOML.
- (d) Harness: TOML parse/validate/plan is typed Rust; execution shell
  `test_harness.sh` is still bash (a "fully typed harness" is aspirational).
- (e) Determinism uses the shipped verifier, not bash: quick suite calls
  `hermit_verify_smoke` / `hermit_record_replay_smoke` (`:3736-3737`). (This is the
  invariant PR #1543 protects — see the R/R migration note.)
- Corrected facts: the observed **1.58×** CPU/wall is the `-j 2` DAG width
  (`CI_DAG_JOBS:-2`, `:3518`), NOT an absent scheduler. GitHub **portable does
  NOT use the runner** — `ci/run-node.sh:50-60` jq-extracts and `bash`-runs each
  node; GH Actions is the outer scheduler. Privileged/manual workflows DO use the
  runner (`ci-privileged.yml:82`, `ci-dag.yml:64,90`, `validation-levels.yml:129`).
  **No workflow passes `--cgroups`** (zero hits across all workflows).

### 10. The R/R determinism check is the product's, not bash's
`validate.sh` must not reimplement determinism comparison in host bash. The shipped
verifier compares stdout + stderr + the full DETLOG event stream + exit status
(`hermit-cli/src/bin/hermit/verify.rs::compare_two_runs`, `record_start.rs::record_verify`);
host `cmp -s` on stdout alone is strictly weaker. Migrated in PR #1543 (record
start --verify). Do not reintroduce host-side stdout-only determinism checks.

---

## Facts that overturned prior doctrine premises (do not re-assert the myths)
- Dirty-tree refusal / `--run-on-dirty-tree` flag: **does not exist**.
- "Smart selection is the default in validate.sh": **false** (default = FULL;
  selective is portable-CI's default).
- `all-tests-passed` label / incremental chain-depth counter: **do not exist**
  (label is `locally-validated`).
- CPU-time / `RLIMIT_CPU` timeouts: **do not exist** (all wall-clock).
- Performance ratchet: **does not exist** (profiling is observability-only).
- "validate.sh can NEVER exit 0 on a devserver": **retracted** — owner ran full →
  5 gates passed / 0 failed (2026-08-03); no structural non-zero floor
  (`validate.sh:4027` is just `((failures == 0))`).
