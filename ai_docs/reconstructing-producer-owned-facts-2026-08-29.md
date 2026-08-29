# Remaining reconstruction of producer-owned facts

Date: 2026-08-29

## Result

The published baseline is
[`type-discipline-census-2026-08-27.md`](type-discipline-census-2026-08-27.md).
This follow-up audits its 14 text-classification families against the later,
published type-sharing work and includes functional test harnesses, which the
original production-file census deliberately excluded.

One instance below is one place that makes a semantic decision from rendered
text, not one regex literal or call.

| Current state | Sites |
|---|---:|
| A serialized typed fact already exists; the consumer does not use it | 13 |
| The producer holds a typed value, but does not serialize it for this consumer | 5 |
| A producer-owned typed result or strict external adapter must be added | 10 |
| **Current semantic decision sites** | **28** |

Separate from those 28 are 9 non-authoritative consistency or diagnostic
readers, 6 historical-only fallbacks, and 2 dormant parsers. They are recorded
below but do not silently select a current verdict.

The audit was performed by content at these heads:

- dev-hermit `type-audit/text-classification-contracts`:
  `1bbb527cfa2dd0d63b1b8b527cb52f44dfbe2c48`
- Hermit `type-audit/text-classification-contracts`:
  `7f050cc4c9bc778022bbc661c6c53b1603d16710`
- Hermit typed dagrun termination change: `3d5b8148e`
- Hermit `origin/main` at audit time:
  `02d978736328c67a7d54ccd4ae252a6a837202c2`

## A serialized typed fact already exists

1. `hermit/ci/run-nextest-counted.sh:22-86` derives aggregate counts from the
   human `Summary` although the same invocation already captures
   `libtest-json-plus` events.
2. Eleven verification consumers decide from the `Determinism verified`
   banner although `VerificationOutcome` and `--verify-json` are the
   producer-owned contract:
   - `hermit/tests/backend-parity/e9patch_corpus.py:182-183,241-244`
   - `hermit/tests/backend-parity/run_matrix.py:1181-1227`
   - `hermit/tests/e2e/lib/data-handling/common.bash:127-132`
   - `hermit/tests/e2e/lib/determinism-stress/common.sh:211-215`
   - `hermit/tests/e2e/lib/language-runtimes/run.sh:78-80`
   - `hermit/tests/e2e/lib/system-utils/_common.sh:169-173`
   - `hermit/tests/qemu-boot/strict_l2_network_test.sh:245-250`
   - `hermit/tests/qemu-boot/strict_l2_test.sh:214-215`
   - `hermit/tests/qemu-boot/strict_l2_userspace_test.sh:240-241`
   - `hermit/tests/standalone/strict_setitimer.sh:86-92`
   - `hermit/tests/standalone/strict_timer_create.sh:59-65`

   The `run_matrix.py` fallback is conservative: it records
   stripped/unmeasured rather than bitwise evidence. It still selects a
   functional tier from the banner. The current DBT implementation at the
   audited head writes `--verify-json`, so the nearby comment saying DBT writes
   no file is stale.
3. `hermit/tests/backend-parity/run_matrix.py:932-958` classifies CPUID
   prerequisite absence from stderr phrases although manifest-plan owns
   `HostCapability::CpuidFaulting` and cell results serialize
   `host_capabilities`.

These 13 sites are the first priority by measurement relevance, not by effort:
they are consumers that can state a pass, an L2, or host-inapplicable from
prose while a typed record already exists. That is a verdict derived from text,
the defect class at its most consequential.

## The producer holds a typed value but does not serialize it here

1. `hermit/ci/manifest-plan/src/runner.rs:3373-3395` parses the binary build SHA
   from `hermit --version`. `build.rs` holds the binary's source identity, but
   there is no structured version endpoint.
2. `hermit/tests/backend-parity/e9patch_corpus.py:193-200` detects compile-time
   e9patch feature absence by matching the runtime error. The feature state
   exists at compile time but is not exposed structurally to Python.
3. `hermit/hermit-cli/src/bin/hermit/backends.rs:1252-1304` rebuilds
   `DbtSummary` from the native client's stderr even though
   `DbtBackendStatsSnapshot` carries branches, syscalls, rewritten syscalls,
   stdin reads, and memory hash through the typed wire stream.
4. `compat-envelope/collect-envelope.rs:1407-1422,1659-1666` derives backend
   engagement from four rendered counter spellings. The producing
   implementations hold the counters (`PreparedBinary`, SaBRe path evidence,
   `DbtBackendStatsSnapshot`, and `RunSummary`), but there is no one serialized
   engagement field for this direct-run consumer.
5. `hermit/detcore/src/logdiff.rs:616-703,920-980` recovers record kind,
   scheduler turn, virtual time, and syscall number from rendered DETLOG
   records. Detcore has those values before formatting; a structured event
   stream is not persisted.

## A typed result or strict external adapter must be added

1. `hermit/ci/manifest-plan/src/runner.rs:1936-1953` assigns
   `guest-launch-refused` from Hermit stderr.
2. `hermit/ci/manifest-plan/src/runner.rs:1963-1989` assigns
   `backend-unavailable` from Hermit stderr. Hermit emits only an error string
   and exit status for these two cases; it needs a closed process result before
   the harness can stop matching prose.
3. `hermit/scripts/validate.rs:11772-11783,11962-11968` calls
   `hermit/scripts/lib/validate_runtime.rs:574-635` to classify failed-node
   detail. This is now the innermost repository-owned adapter and writes the
   closed `FailureClass`, but underlying build and shell tools expose only
   text. Test-harness cells could instead reuse `CellResult.failure_class`.
4. `ci-hub/validate/reverie_node.py:200-217,311-332` derives per-gate
   zero/absent coverage from libtest banners. The source row has aggregate
   `executed_tests`/`filtered_tests` and gate names, but not per-gate counts;
   the Reverie producer must add them.
5. `hermit/tests/backend-parity/e9patch_corpus.py:177-179,246-258` parses
   `candidate_sites`, `mapped_sites`, and `b0_sites` from e9patch diagnostics.
   e9tool is external; this needs a strict adapter or machine-readable e9tool
   mode.
6. `hermit/hermit-cli/src/e9patch.rs:502-523,1095-1106` parses
   `num_patched` and `num_patched_B0` from e9tool diagnostics. This is the same
   external boundary, but a separate consumer.
7. `compat-envelope/collect-envelope.rs:1280-1329` decides backend availability
   from Hermit error phrases. The CLI has no structured availability response
   for this probe.
8. `ci-hub/health/operational_health.py:1634-1689` decides merge-authority
   state from the exit status plus `PASS:`, `FAIL:`, and `unset` prose. The shell
   checker owns the closed three-way state but emits no JSON result.
9. `ci-hub/health/approval_binding.py:708-719,1167-1179` derives
   approval/refusal authority from canonical comment lines. This is a strict
   shared grammar, not arbitrary output, but no separate durable review record
   exists.
10. `ci-hub/landing/cherry_pick_context.py:53-75,175-242` derives source,
    disclosure, and reviewed-context facts from commit messages, PR Summary
    prose, and review comments. Those values need a durable structured record
    to remove the text dependency.

## Re-parsing that is not current authority

Nine sites are retained consistency checks or diagnostics. They are not part
of the 28 because the typed value remains authoritative:

- `hermit/scripts/validate.rs:4951-5005` checks `Step.manifest` against the
  rendered command.
- `hermit/ci/audit-test-binary-registration.py:138-167` checks
  `integration_test_binaries` against the rendered command.
- `hermit/ci/compat-envelope/pressure-test.rs:4221-4253` compares
  `CellResult.result`/`failure_class` with the retained classifier and refuses
  disagreement.
- `ci-hub/validate/failure_evidence.py:1073-1112` keeps prose signatures beside
  the producer's `failure_class` as diagnostics.
- `scripts/owner-checkout-validate-watch.py:562-623` extracts lock-holder
  detail after typed admission has already established refusal.
- `scripts/owner-checkout-validate-watch.py:626-660,1792-1795` retains
  presentation-only admission-refusal evidence.
- `scripts/owner-checkout-validate-watch.py:931-987` extracts privileged-node
  detail for the report.
- `scripts/owner-checkout-validate-watch.py:1023-1172` reconstructs relink
  recurrence evidence.
- `scripts/owner-checkout-validate-watch.py:1175-1290` reconstructs relink
  retirement evidence. The latter two explicitly carry
  `verdict_effect=none-evidence-only`; typed compile/node events would be needed
  before either could safely become policy.

## Historical-only fallbacks

Six readers tolerate retained rows without granting them current authority:

- `ci-hub/validate/service_result.py:219-256,344-361`
- `ci-hub/validate/failure_evidence.py:1026-1102`, reached through
  `ci-hub/validate/attribute_reds.py:248-294`
- `ci-hub/validate/flake_class.py:303-321,452-457`
- `ci-hub/validate/finalize_receipt.py:261-275,552-574`
- `ci-hub/landing/task_pr_join.py:335-350`
- `ci-hub/closure/drain_publication.py:186-198,413-466`

Two additional parsers are dormant or unwired and are not in the count:

- `ci-hub/validate/sabre_reach.py:86-149`; the active harness consumes typed
  `sabre_path_evidence`.
- `ci-hub/validate/aggregate.py:224-285`; only its tests call `parse_raw_log`.

## Census status and ordering

Eight of the original 14 families have no current authoritative prose path
after the published work: retry class, per-test nextest execution,
validation-service result, active SaBRe path evidence, validation-step identity
authority, scorecard schema, union-rebase result, and dependency policy.

Six remain partly or wholly live: failure attribution at process/tool
boundaries, aggregate test totals/per-gate coverage, owner-watch diagnostics,
review/cherry-pick records, one operational-health gate, and Detcore log
semantics.

After the first-priority typed-record consumers, the remaining order by
measurement relevance is: nextest aggregate and Reverie per-gate counts;
runner launch/backend failure causes and collect-envelope
availability/engagement; DBT's duplicate summary parse; merge-authority output;
then the human-authored review/cherry-pick protocols and Detcore's larger event
serialization change.

Two controls make the numbers above a remainder rather than a total: the
TIMEOUT and OOM prose removal in Hermit `3d5b8148e`, and the build-message test
reading compile-time feature state in Hermit `9215d68de`.
