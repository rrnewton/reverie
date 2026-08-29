# Test-run facts still reconstructed from output

Date: 2026-08-29

## Result

This audit follows
[`reconstructing-producer-owned-facts-2026-08-29.md`](https://github.com/rrnewton/dev-hermit/blob/ecc1509b0199595e208a3769593e19df4661d240/ai_docs/reconstructing-producer-owned-facts-2026-08-29.md)
and asks the same question specifically at the test-framework boundary: where
does outer code still establish a fact about a test run by reading its output?

One code site means one place that makes a decision, not one regular expression.
The two `dagrun` test-event recognizers each contain two different boundaries:
an exact `##TEST-START`/`##TEST-END` contract for repository-owned producers and
adapters for external libtest, pytest, and TAP output. Therefore the count is
23 producer/consumer paths across 21 code sites.

| Classification | Producer/consumer paths |
|---|---:|
| Recordable by the runner or producing component | 17 |
| Genuinely outer because the producer is an external tool or the fact spans runs | 6 |
| **Current paths** | **23** |

Those 23 paths occupy 21 code sites. The two mixed `dagrun` recognizers each
contain one path in each classification.

The audit was performed by content at these published heads:

- dev-hermit `62cc2fbd938fd126b3c46a365ee9f68f6b96275a`
- Hermit producer-boundary stack `f7a3759886204694631b565598f7c2c6666bf73d`
- Hermit framework retry/timeout/typed-termination stack through
  `92d06be049fcca67e3786772467c995ef35c116b`
- Reverie `8d21c8472d25e776cce3d9dde039dc39691741a5`
- agent-utils `2f3f9775bc031c8aa6a3de21241ddf5f05bcbcb4`

## Current paths

| Priority | Fact established from output | Current consumer | Component that knows it | Classification and reason |
|---:|---|---|---|---|
| 1 | DBT parity case identity, attempt, and pass/fail | [`validate.rs`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/validate.rs#L14758-L14813), used in the receipt summary at [L17181-L17216](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/validate.rs#L17181-L17216) | [`run_matrix.py`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/tests/backend-parity/run_matrix.py#L1937-L2011) holds every result and already has a whole-run TSV writer, but the DAG does not request it | **Recordable by the runner.** This is the highest-value remaining gap because text currently supplies individual test identities and retry health. Extend the existing structured count record or request a complete result file; do not add another parser. |
| 1 | Per-gate executed and filtered test counts | [`gate_test_counts`](https://github.com/rrnewton/reverie/blob/8d21c8472d25e776cce3d9dde039dc39691741a5/validate.sh#L159-L177) | Cargo/libtest knows each suite total | **Recordable by the runner.** These fields set each ledger gate's denominator. Missing banners remain absent, which is correct, but the values still come from human output. |
| 1 | Whole-run executed and filtered test counts | [`append_validation_ledger`](https://github.com/rrnewton/reverie/blob/8d21c8472d25e776cce3d9dde039dc39691741a5/validate.sh#L212-L236) | The same Cargo/libtest invocations | **Recordable by the runner.** This is a second independent parse of the same facts and can disagree with the per-gate values; the parent now detects disagreement, but does not prevent it. |
| 1 | Exact commit, full selection, validation level, cache use, terminal status, and executed-test count for a release candidate | [`manual_validate`](https://github.com/rrnewton/dev-hermit/blob/62cc2fbd938fd126b3c46a365ee9f68f6b96275a/scripts/release-0.3-acceptance.py#L318-L368) | Hermit validate already writes the validation-service result and ledger row | **Recordable by the runner; most is already recorded.** The release artifact supplies only raw log and exit, so this consumer reconstructs a second result and can accept or refuse a different population. |
| 1 | Whether `lint-checks` had an unevaluated case | [`lint-checks-node.sh`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/ci/lint-checks-node.sh#L46-L85) | [`test_validate_stop_paths.py`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/test_validate_stop_paths.py#L800-L836) owns the unevaluated list | **Recordable by the runner.** Today this is an exact, anchored `NO-RESULT-CASE:` text contract because `make` collapses recipe exits. It is fail-closed and tested, not a free-form prose guess, but a structured side file would remove the remaining text dependency. |
| 2 | Aggregate test counts for a generic `dagrun` step without a structured count file | Rust [`captured_test_counts`](https://github.com/rrnewton/agent-utils/blob/2f3f9775bc031c8aa6a3de21241ddf5f05bcbcb4/rs/dagrun/src/scheduler.rs#L2435-L2553) | The invoked test harness knows the total; `dagrun` does not | **Genuinely outer for arbitrary commands.** Repository-owned harnesses should write `DAGRUN_TEST_COUNTS_PATH`. Hermit validate sets `DAGRUN_REQUIRE_STRUCTURED_TEST_COUNTS=1`, so this fallback cannot set its receipt denominator. |
| 2 | Live test start/end and the likely hung test | Rust [`recognize`](https://github.com/rrnewton/agent-utils/blob/2f3f9775bc031c8aa6a3de21241ddf5f05bcbcb4/rs/dagrun/src/attribution.rs#L369-L458) and Python [`recognize`](https://github.com/rrnewton/agent-utils/blob/2f3f9775bc031c8aa6a3de21241ddf5f05bcbcb4/py/dagrun/attribution.py#L266-L315) | A repository-owned harness can emit explicit boundaries; external libtest, pytest, and TAP processes own their formats | **Two paths per implementation.** `##TEST-START`/`##TEST-END` is recordable by the runner and is already an exact contract. The libtest/pytest/TAP arms are genuinely outer: attribution must happen while the process is live, before a hung process could write an end-of-run report. Unknown text produces no test name rather than a guessed one. |
| 2 | Environmental/prerequisite/product failure class for generic build and shell nodes | [`validate.rs`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/validate.rs#L11739-L11807) and its terminal pass at [L11948-L11977](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/validate.rs#L11948-L11977) | Cargo, shell commands, and external tools expose exit status and diagnostics, not the project's `FailureClass` | **Genuinely outer.** This is the innermost repository-owned adapter for those nodes. Manifest cells are excluded from this outer retry path and publish their own `CellResult.failure_class`. |
| 2 | Stress-run failure shape such as harness failure, hang, mismatch, or crash | [`classify_shape`](https://github.com/rrnewton/dev-hermit/blob/62cc2fbd938fd126b3c46a365ee9f68f6b96275a/ci-hub/attribution/attribution.py#L644-L661) | The capture wrapper knows timeout and exit directly; arbitrary commands do not share a richer result type | **Genuinely outer.** Timeout and exit are already typed. Message-based mismatch/crash/harness recognition is a strict adapter over arbitrary commands and must remain unknown when no rule matches. |
| 2 | Infrastructure versus flaky versus consistent failure across nextest JUnit rows | [`stress-test.sh`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/stress-test.sh#L120-L177) | Nextest/JUnit knows test outcomes, but not the project's infrastructure taxonomy | **Genuinely outer.** The cross-iteration health category is an aggregate fact, and the infrastructure bucket is an adapter over external failure messages. It must not be presented as a producer verdict. |
| 3 | Fail-closed ratchet count and first failure for the progress report | [`progress-report.sh`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/progress-report.sh#L83-L96) | `test-fail-closed.sh` owns its running totals and first failing test | **Recordable by the runner.** Manual and unscheduled, so it does not affect a landing receipt. |
| 3 | Record/replay test counts for the progress report | [`progress-report.sh`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/progress-report.sh#L48-L53) and [L107-L112](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/progress-report.sh#L107-L112) | Cargo/libtest | **Recordable by the runner.** Manual report only. |
| 3 | Per-application test counts for the progress report | [`progress-report.sh`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/progress-report.sh#L113-L125) | Cargo/libtest | **Recordable by the runner.** This is a second parser in the same script rather than reuse of the helper above. |
| 3 | Optional RR suite pass/fail counts | [`compat-map.sh`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/compat-map.sh#L241-L260) | Cargo/libtest | **Recordable by the runner.** Manual report only. |
| 3 | Exact and ignored test inventory used by the fail-closed ratchet | [`test-fail-closed.sh`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/scripts/test-fail-closed.sh#L107-L169) | Cargo/libtest | **Recordable by the runner.** The parser determines the population before running it. This script is deliberately unscheduled and reached only by `progress-report.sh`. |
| 3 | Whether a stress binary contains the requested exact test | [`stress-burst`](https://github.com/rrnewton/dev-hermit/blob/62cc2fbd938fd126b3c46a365ee9f68f6b96275a/ci-hub/stress/stress-burst#L109-L123) | The libtest binary | **Recordable by the runner.** Output presence currently selects the binary and distinguishes `NOTEST`; a structured inventory would make the distinction exact. |
| 3 | Burst passes, hangs, and other exits | [`stress-burst`](https://github.com/rrnewton/dev-hermit/blob/62cc2fbd938fd126b3c46a365ee9f68f6b96275a/ci-hub/stress/stress-burst#L125-L152) | The per-instance wrapper knows exit and timeout, and capture mode already writes `meta.json` | **Recordable by the runner.** The aggregate is legitimately outer, but its inputs should be typed rows rather than a file containing bare exit-code lines. |
| 3 | Whether Demo 8 used cold calibration or a cached seed | [`demo-hot-path.yml`](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/.github/workflows/demo-hot-path.yml#L281-L318) | `prepare-demo08-assets.sh` knows which branch it took | **Recordable by the producing script.** The workflow currently treats one of two human messages as the observed path. |
| 3 | Final Reverie validation status | [`reverie_safe_ci.py`](https://github.com/rrnewton/dev-hermit/blob/62cc2fbd938fd126b3c46a365ee9f68f6b96275a/ci-hub/validate/reverie_safe_ci.py#L24-L70) | `reverie_node.py` writes one closed status value to a dedicated file | **Recordable by the runner and already recorded through an exact text contract.** The consumer requires one known value and checks the matching exit code; this is a control, not a prose inference. |
| 3 | Hermit launch refusal and backend unavailability | [`manifest-plan` runner](https://github.com/rrnewton/hermit/blob/f7a3759886204694631b565598f7c2c6666bf73d/ci/manifest-plan/src/runner.rs#L1924-L1984) | Hermit emits one exact `HERMIT_INTERNAL_FAILURE class=...` line before human detail | **Recordable by the product and already recorded through an exact text contract.** Unknown or altered classes do not silently match. This is the strict adapter delivered by the producer-boundary work, not arbitrary English matching. |

## What is correctly outer

The following whole-run facts are not candidates for a cell-owned result:

- dependency relationships, skipped dependents, and graph completeness;
- whether the selected profile covered the required population;
- repository cleanliness, exact commit/tree identity, and receipt qualification;
- totals across independent cells or stress repetitions;
- host pressure and low-load comparison across separate executions.

Those facts become true only after combining more than one cell or facts outside
the test process. The outer layer should compute them from typed cell, scheduler,
ledger, and repository records. Moving them into one cell would give that cell
authority over facts it cannot know.

## Existing controls

These paths are no longer current output-derived authority and are excluded from
the 23-path count:

- nextest aggregate and per-test results come from `libtest-json-plus`; the
  wrapper's libtest banner is presentation only;
- test-harness writes one typed `CellResult` with all `AttemptResult` values,
  and per-cell retry publishes every attempt while only the terminal attempt
  sets the verdict;
- timeout and OOM termination are typed scheduler fields in the published
  agent-utils/Hermit change; pressure-test reads prose only for retained schema-1
  rows, which gain no current authority;
- owner-watch decides lock refusal from typed admission and parses the log only
  for operator-facing holder detail;
- current failure-evidence classification reads the ledger's `failure_class`;
  prose contributes diagnostic signatures only;
- scorecard import and pressure-test consume typed cell rows and verification
  reports, not PASS/FAIL banners;
- `aggregate.py`, `nonzero_result.py`, and `sabre_reach.py` have no production
  caller; retained-row parsers in service-result, receipt, flake, task/PR, and
  drain code are historical fallbacks and do not grant current authority.

## Order

The order is measurement relevance rather than effort:

1. DBT per-case results, Reverie per-gate and aggregate counts, and release
   acceptance, because these can change a receipt verdict or its denominator.
2. Generic scheduler count/attribution and failure-class adapters, preserving
   the live attribution needed for a process that hangs before an end record.
3. Manual progress, compatibility, stress, and demo reporting.

The mutation test for each move is the DETLOG pattern: hold printed text fixed,
change the structured value, and require the consumer's result to change. A
consumer that reads a field but still follows the text has not been migrated.
