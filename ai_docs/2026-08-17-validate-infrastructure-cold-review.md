# Cold Review of the Validate Infrastructure

## Review target

- Repository: `hermit`
- Base: `d601651a3a0f4865b72af50a9c5967eaf60c351a`
- Exact reviewed head: `79cc84087904bf7069cef9c585a2eac041341d40`
- Branch at review time: `main`
- Review date: 2026-08-17
- Scope: `ci/test_harness.sh`, `ci/manifest-plan/`, `ci/compat-envelope/`, `scripts/validate.rs`, and `ci/dag/`
- Method: cold, read-only review of the complete relevant execution paths and their tests. This verdict is bound to the exact head above; a changed head requires re-review.

The owner supplied the framing measurement that the recorded figure, 176 green cells, understated the measured figure, 531, by a factor of three. This review independently traced where cells are admitted, executed, classified, and counted. It found four defects that can disconnect the published result from the evidence or prevent intended measurements from running.

## Findings

### 1. Critical: the scorecard combines same-SHA result rows by file modification time without admitting their evidence

**Location:** `ci/compat-envelope/scorecard.rs:201`, `ci/compat-envelope/scorecard.rs:1024`, `ci/compat-envelope/scorecard.rs:1041`, `ci/compat-envelope/scorecard.rs:1060`, `ci/compat-envelope/scorecard.rs:1113`, `ci/compat-envelope/scorecard.rs:1127`, and `scripts/validate.rs:1889`.

**What the code does:** `ResultRow` contains schema, clean source identity, cell identity, classification, and outcome. It does not deserialize the harness run ID, effective argv, binary or test hashes, relaxations, or the canonical verify/replay report. Unknown JSON fields are ignored. `read_result_candidates` recursively reads every `results.jsonl` below the supplied result root, admits clean rows for the exact Hermit SHA, and records each file's modification time. `verify_candidate_set` sorts all rows for the same cell by modification time (then path), chooses the newest, and accepts it when `outcome == "PASS"`. `configure_e2e_result_root` accepts an existing caller-supplied `E2E_RESULT_ROOT`; it does not require an empty directory or establish one outer validation invocation ID for all rows counted by the scorecard.

**Why this is wrong:** the scorecard cannot prove that the rows it combines came from one validation invocation or from the command, binaries, relaxations, and canonical reports that the published claim describes. On a reused result root, cells omitted from the current invocation can be supplied by earlier same-SHA rows. When more than one same-SHA row exists, the deciding provenance is a filesystem timestamp. A green can therefore outlive the evidence that produced it and be combined with rows from a different invocation.

The self-test at `ci/compat-envelope/scorecard.rs:1178` confirms the weak admission boundary: its positive candidate contains only the fields in `ResultRow`, including a bare `outcome: "PASS"`; it carries no run ID or comparison proof.

**What was run:**

```text
./ci/compat-envelope/scorecard.rs self-test-and-check
nl -ba ci/compat-envelope/scorecard.rs | sed -n '195,218p;1018,1135p;1172,1205p'
nl -ba scripts/validate.rs | sed -n '1882,1912p'
```

Result: passed, including `compatibility scorecard: tracked table and 5376 cells are current`. I did not run a synthetic reused-result-root attack; the finding is established by the production admission and selection control flow plus the executable positive self-test accepting a bare PASS row.

**Required change:** bind every counted row to one validation invocation and independently admit the effective argv, binary/test hashes, relaxations, and canonical mode-specific report evidence. Reject pre-existing or unbound rows. Selection among evidence for a published scorecard must not be decided by file modification time.

### 2. High: 410 of 453 enabled comparable `ci = false` cells have no `ci_disabled_reason`, and the validator and refusal tests were deleted

**Location:** `ci/manifest-plan/src/main.rs:653` and `ci/manifest-plan/src/main.rs:667`. The still-documented requirement is at `tests/e2e/manifests/README.md:171`.

**What the code does:** `validate_mode` permits the `ci_disabled_reason` key but reads only `ci`; it never validates that a disabled mode has a non-empty reason or that an enabled mode has no stale reason. Commit `be4c090500` deleted `CI_REASON_REQUIRED_BUCKETS`, `validate_ci_disabled_reason`, its call from `validate_mode`, and the tests that refused a missing reason, a blank reason, and a stale reason on an enabled cell. The manifest documentation still says the `backend-parity-c` guard exists.

The current generated plan has 453 enabled comparable cells whose mode declares `ci = false`; 410 lack `ci_disabled_reason`. The mode-level count is 219 `ci = false` declarations, 201 without a reason. The larger cell count reflects each mode expanding over its enabled backends.

**Why this is wrong:** a cell can be switched out of the required plan without recording why, and the validator passes it. The missing reason is not merely missing prose: the guard and all three negative tests that proved its refusal behavior were removed. That makes silent exclusions indistinguishable from reviewed exclusions and leaves no executable check that exclusions exclude only what they claim.

**What was run:**

```text
cargo run --quiet -p hermit-manifest-plan -- --format matrix-json > /tmp/ov-cold-matrix.json
cargo run --quiet -p hermit-manifest-plan -- --format harness-json > /tmp/ov-cold-harness.json
jq '[.[] | .test[] | .modes | to_entries[] | select(.key != "custom" and .value.ci == false) | (.value.backends_enabled | length)] | add' /tmp/ov-cold-harness.json
jq '[.[] | .test[] | .modes | to_entries[] | select(.key != "custom" and .value.ci == false and ((.value.ci_disabled_reason // "") | length) == 0) | (.value.backends_enabled | length)] | add' /tmp/ov-cold-harness.json
jq '[.[] | .test[] | .modes | to_entries[] | select(.key != "custom" and .value.ci == false and (.value.backends_enabled | length) > 0)] | length' /tmp/ov-cold-harness.json
jq '[.[] | .test[] | .modes | to_entries[] | select(.key != "custom" and .value.ci == false and (.value.backends_enabled | length) > 0 and ((.value.ci_disabled_reason // "") | length) == 0)] | length' /tmp/ov-cold-harness.json
git diff be4c090500^ be4c090500 -- ci/manifest-plan/src/main.rs
cargo test -p hermit-manifest-plan
```

Results: 453 enabled comparable `ci = false` cells, of which 410 have no `ci_disabled_reason`; 201 of 219 enabled `ci = false` mode declarations have no reason. All 34 manifest-plan tests passed: 3 footprint tests, 24 main tests, and 7 strict-authority tests. That green result does not bracket this requirement because the validator and its missing/blank/stale refusal tests are absent.

**Required change:** restore and broaden the validator so every applicable `ci = false` mode must carry a non-empty reason, and every corresponding `ci = true` mode must reject a stale reason. Restore positive and missing/blank/stale negative tests, and make the documentation match the enforced scope.

### 3. High: an all-red pressure run selects 303 disabled naked cells and emits `--backend native`, which the harness rejects

**Location:** `ci/manifest-plan/src/main.rs:933`, `ci/compat-envelope/pressure-test.rs:1841`, `ci/compat-envelope/pressure-test.rs:2582`, `ci/test_harness.sh:69`, `ci/test_harness.sh:2269`, and `ci/test_harness.sh:2352`.

**What the code does:** the manifest planner gives naked mode three attempts even when its native cell is disabled. Pressure selection includes every selected red cell with an attempt budget. For disabled cells, the DAG command always appends `--probe-disabled --backend <cell backend>`, so a disabled naked cell becomes `--backend native`. The harness represents naked disabled gaps as backend `native`, but `native` is not in its accepted `BACKENDS` list. Argument validation therefore refuses the command before the cell runs.

The current matrix contains 303 disabled naked cells. Consequently, an all-red pressure run includes 303 commands that are rejected by the harness rather than measuring the cells.

**Why this is wrong:** the pressure plan claims these disabled cells are runnable red measurements, budgets attempts for them, and publishes commands for them, but the harness refuses every one at option parsing. This can turn intended guest measurements into harness failures and makes the all-red plan materially different from what it says it will execute.

**What was run:**

```text
jq '{disabled_naked: ([.[] | select(.mode == "naked" and .backend == "native" and .enabled == false)] | length), enabled_naked: ([.[] | select(.mode == "naked" and .backend == "native" and .enabled == true)] | length)}' /tmp/ov-cold-matrix.json
./ci/test_harness.sh plan --test applications/c-toolchain-workflow --mode naked --backend native --format json
./ci/test_harness.sh audit-gaps --test applications/c-toolchain-workflow --mode naked --format json
nl -ba ci/compat-envelope/pressure-test.rs | sed -n '1834,1860p;2572,2600p'
nl -ba ci/test_harness.sh | sed -n '55,80p;2258,2280p;2344,2368p'
```

Results: the matrix count was 303 disabled naked cells and 33 enabled naked cells. The direct plan command failed with `test_harness.sh: invalid backend: native`. The gap audit emitted the same disabled naked cell as backend `native`, confirming that the pressure planner and harness share the cell identity but disagree on the accepted command line.

**Required change:** generate the harness's accepted naked-mode selector for disabled native cells without passing `--backend native`, or make the harness accept `native` consistently for naked-mode filtering. Add an executable all-red negative/positive bracket that proves every generated command passes harness argument validation before a pressure run is accepted.

### 4. High: inner verify/replay timeouts are classified as `infrastructure-error`

**Location:** `ci/compat-envelope/pressure-test.rs:3219`, `ci/compat-envelope/pressure-test.rs:3300`, `ci/compat-envelope/pressure-test.rs:3325`, `ci/compat-envelope/pressure-test.rs:3327`, and `ci/compat-envelope/pressure-test.rs:3772`.

**What the code does:** `reason_reports_timeout` recognizes the harness's exact inner timeout reasons. During summarization, however, a verify/replay attempt that has an inner timeout normally has no canonical verification report, so the summarizer records missing verification evidence. `classify_result` tests `!verification_evidence_valid` at line 3325 and returns `infrastructure-error` before it tests `reason_reports_timeout` at line 3327. The timeout reason is therefore unreachable for this production shape.

The self-test has a reason-parser bracket at line 5815 and a classification call at line 5981, but that classification call supplies `verification_evidence_valid = true`. It does not exercise the production combination of an exact inner timeout reason and the missing report caused by that timeout.

**Why this is wrong:** an observed guest attempt that reached the harness's own inner verify/replay deadline is a timeout result, not missing infrastructure evidence. The ordering converts a measured timeout into `infrastructure-error`, distorting pressure outcome counts and hiding where execution time was actually spent.

**What was run:**

```text
./ci/compat-envelope/pressure-test.rs self-test
nl -ba ci/compat-envelope/pressure-test.rs | sed -n '3215,3350p;3764,3795p;5808,5830p;5974,6000p'
```

Result: the self-test failed earlier at `ci/compat-envelope/pressure-test.rs:5552`, before reaching the timeout classification brackets. I did not run a live timed-out guest. This finding is established by tracing the production summarization and classification control flow; the current self-test neither confirms nor refutes it because its classification fixture forces verification evidence valid.

**Required change:** classify the exact recognized inner timeout reason before treating the expected absence of a verify/replay report as an infrastructure error, while continuing to require the existing exact timeout strings. Add a production-shaped test with the recognized reason, nonzero harness status, no verification report, and otherwise valid row identity.

## Goalpost-moving assessment

- Assertions weakened: **yes**. The missing/blank/stale `ci_disabled_reason` refusal tests were deleted with their validator.
- Tolerance widened, exemption added, case skipped, or comparator relaxed: **yes for exclusions**. Current `ci = false` exclusions can pass without the reason the documented guard required. This review found no weakening in the harness's direct canonical verify/replay comparator.
- Failure renamed or relabelled as a pass: **no direct relabelling found**. The scorecard defect instead admits and combines PASS rows without the evidence needed to re-establish the published claim.
- Check deleted instead of satisfied: **yes**. Commit `be4c090500` removed the `ci_disabled_reason` check and all three refusal tests while the documentation continued to claim the guard.

## Verification results

The following results are bound to Hermit head `79cc84087904bf7069cef9c585a2eac041341d40`:

- **34 manifest-plan tests PASSED.** This includes 3 footprint tests, 24 main tests, and 7 strict-authority tests.
- **Scorecard self-test PASSED; the 5,376-cell table was current.** The pass also demonstrates that its positive bracket accepts a result row without run or comparison evidence.
- **Pressure-test self-test FAILED at this head.** It failed at `ci/compat-envelope/pressure-test.rs:5552` because the fixture assumes its replay-mode green selection contains exactly one cell, while the current scorecard contains seven replay green cells.
- **The full harness correctly REFUSED three tracked C tests missing from inventory:** `tests/c/record_replay_execveat_paths.c`, `tests/c/record_replay_mkdir_eexist.c`, and `tests/c/record_replay_poll_partial_copyout.c`. `./ci/test_harness.sh validate` exited 2 at the inventory audit, before invoking the pressure self-test.

### Scope of the pressure self-test failure

I established that this failure is isolated self-test drift with respect to published pressure verdicts. The stale one-cell assertion is in the separate `self-test` command path at `ci/compat-envelope/pressure-test.rs:1328`; production `run` and `summarize` use separate command paths. The failed assertion does not participate in result classification or publication.

The exact count was checked with:

```text
jq '[.cells[] | select(.status == "green" and .mode == "replay")] | length' ci/compat-envelope/cells.json
```

Result: `7`.

It is not harmless to validation: `ci/test_harness.sh:4115` runs the pressure self-test as part of `validate`. Once the earlier inventory refusal is cleared, the stale cardinality assertion will make that metadata gate fail. I did not find a path by which this particular self-test failure changes an already published cell verdict.

## Correction and bounded conclusion

The harness's direct verify/replay admission itself is **sound**: malformed, stale, noncanonical, output-only, and zero-message reports fail closed. `ci/test_harness.sh:3324` requires readable JSON with `verified == true`, `verdict == "matched"`, `bitwise_parity == true`, canonical strictness, log comparison enabled, and positive message counts on both sides. `ci/test_harness.sh:3470` removes the exact old report before launching the next attempt, and `ci/test_harness.sh:3923` refuses to treat a zero process exit as sufficient.

That bounds the damage. The direct cell admission is not manufacturing a PASS from a malformed comparison report. The defects are downstream or adjacent: the scorecard discards that evidence when counting PASS rows, disabled naked pressure commands are rejected before measurement, inner timeouts are classified under the wrong result, and manifest exclusions lack the validator and refusal tests that the documentation claims.

## Limitations and verdict

The scorecard result-root reuse and timeout classification findings were established by executable self-tests plus production control-flow tracing, not by synthetic end-to-end attacks. The disabled naked rejection and missing-inventory refusal were reproduced directly. No product files were edited during the review.

**Verdict: changes requested.** Re-review is required on any head other than `79cc84087904bf7069cef9c585a2eac041341d40`.
