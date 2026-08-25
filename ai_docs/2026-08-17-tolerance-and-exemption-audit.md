# Tolerance and exemption audit, 2026-08-17

## Provenance and denominator

The read-only audit was performed against parent commit
`ec5280a70276ea1875abb4e4e7d761a97090da93` and Hermit commit
`79cc84087904bf7069cef9c585a2eac041341d40`.

The search covered verdict- or coverage-affecting uses of skip, ignore, allow,
tolerate, exempt, relax, normalize, non-fatal, best effort, `|| true`,
`continue-on-error`, nonblocking classifications, and defaults that mask an
invalid value. Cleanup-only error suppression, formatting-only normalization,
test fixtures, third-party code, and unreferenced helpers were excluded.

The resulting denominator was 28 mechanisms:

- 18 had scope broader than their stated reason, or had no recorded reason.
- 10 had a recorded reason whose scope matched the implementation.

The retained-evidence follow-up classified the 18 broader mechanisms as:

- **ACTIVELY HIDING FAILURES: 3**
- **FIRES BUT ONLY ON ITS STATED CAUSE: 2**
- **NEVER OBSERVED HIDING A FAILURE: 13**

“Never observed” is a statement about the retained evidence inspected that
night, not proof that the mechanism cannot fire.

## The 18 broader mechanisms

| # | Location at the audited commit | Stated reason | Actual scope | Retained-evidence classification |
|---|---|---|---|---|
| 1 | `ci/manifest-plan/src/main.rs:655` and manifest `ci_disabled_reason` fields | A disabled CI mode should explain why it is disabled. | The field was parsed but not required or validated; 168 of 186 inspected cells had no reason. | **ACTIVELY HIDING FAILURES** |
| 2 | `ci/compat-envelope/scorecard.rs` disabled-cell prose check near the expected-plan validation | Prevent impossible cells from entering the required plan. | The check could block on a prose typo while not rejecting the nine cells that could not run. | Never observed hiding a failure |
| 3 | `ci/compat-envelope/pressure-test.rs:75,307-309,816-818` | None recorded. | `PRESSURE_JOBS = 4` fixed pressure concurrency without a measured or historical rationale. | Never observed hiding a failure |
| 4 | `ci/compat-envelope/pressure-test.rs:3535-3599` | Re-read retained verification evidence before admitting a pressure result. | `verified=true` plus `verdict=matched` could be accepted without requiring bitwise parity, canonical strictness, log comparison, or positive INFO counts. Current fresh runs were protected only transitively by the shell harness predicate. | Never observed hiding a failure |
| 5 | parent `demos/lib/demo_common.py:423-456` | Python-launcher environment variation should not invalidate byte-identical VM artifacts. | Every residual INFO divergence, and missing INFO evidence, was non-fatal. In the measured Demo 5 repeat, 471,412 of 473,309 mismatches occurred after QEMU `execve`. | **ACTIVELY HIDING FAILURES** |
| 6 | `ci/compat-envelope/scorecard.rs:201-225,1024-1083,1102-1150` | Require a fresh clean-HEAD PASS row for every required cell. | The reader did not bind the row to run id, harness/runner status, verification JSON, canonical predicate, retained logs, or literal argv. | Never observed hiding a failure |
| 7 | `.github/workflows/merge-gate.yml:423-433` | Determine whether changed files require KVM review text. | `gh api ... || true` converted an API failure into an empty changed-file set and `PR_IS_KVM=false`. | Never observed hiding a failure |
| 8 | `scripts/core-review-protocol-lint.sh:22-23,67-69` | Enforce the extra review text when `post-facto-human-review` is present. | The lint passed every PR without the label and did not check whether the label was required. | Never observed hiding a failure |
| 9 | `scripts/test-fail-closed.sh:71-86,118-138,160-162` | Skip tests whose named host or tool capability is unavailable. | 263 allowed `#[ignore]` rows were unconditional across hosts; only non-empty prose and row presence were checked. | Never observed hiding a failure |
| 10 | `scripts/test-fail-closed.sh:54-69,144-145,156-158` | Keep five known failures from breaking the gate for their recorded causes. | The five tests were never executed, so recovery or a different failure cause could not be observed. | Never observed hiding a failure |
| 11 | `scripts/lib/validate_corpus.rs:125-145` | Permit the named `lsof` and `make` compatibility failures. | Admission keyed only on the test label, so any failure for those tests was nonblocking. | Fires, but retained evidence showed only the stated cause |
| 12 | `scripts/validate.rs:3325-3334` | Permit the named `top` and `zstd` portable diagnostic failures. | Admission keyed only on the test label, so any failure for those tests was nonblocking. | Fires, but retained evidence showed only the stated cause |
| 13 | `scripts/lib/validate_corpus.rs:159-168` and `scripts/lib/validate_plan.rs:419-430` | Exclude five RR tests for their recorded divergences. | Whole tests were omitted rather than checking for the stated divergence. | Never observed hiding a failure |
| 14 | `scripts/lib/validate_super.rs:266-273,454-497` | Keep initial KVM and DBT stress measurements nonblocking. | Every KVM/DBT stress failure remained nonblocking indefinitely, with no exit transition or failure-cause check. | Never observed hiding a failure |
| 15 | `.github/workflows/ci-privileged.yml:108-133` | GitHub Actions deliberately skips ordinary systemd scope creation. | `--allow-cgroup-failure` accepted any boxing failure as an unboxed run, including failures unrelated to the Actions skip. | **ACTIVELY HIDING FAILURES** |
| 16 | `.github/workflows/ci-privileged.yml:134-150` | Keep the named occasional KVM probe non-gating. | Job-level `continue-on-error` also exempted checkout, install, build, preparation, and evidence-publication failures. | Never observed hiding a failure |
| 17 | `.github/workflows/ci-portable.yml:802-810` | Keep the named SaBRe probe non-gating. | Job-level `continue-on-error` also exempted checkout, install, build, preparation, and evidence-publication failures. | Never observed hiding a failure |
| 18 | `scripts/lib/validate_runtime.rs:55-64`, `validate_envelope.rs:79-85,215-219`, and `validate_super.rs:706-712` | Use defaults when the environment variable is absent. | Malformed explicit `VALIDATE_ENV_BLOCK_RETRIES`, `L4_REPS`, `HERMIT_RR_TIMEOUT`, and `SUPER_REPETITIONS` values silently became defaults instead of configuration errors. | Never observed hiding a failure |

## Scope-matched counterexamples

Ten mechanisms were retained as correctly scoped counterexamples. The most
important were the canonical M2 normalization and predicate, explicit
CI-bucket-only `--allow-empty`, fail-closed Demo 8 asset handling in workflows,
the exact detached-verify banner normalization, ledger parse failures producing
no cache hit, full selection on unknown affected-test mappings, and Demo qcow2
normalization of one validated snapshot timestamp field. These were not added
to the 18-item work queue.

## Unresolved validate-port review item: replay command publication

At frozen Hermit commit `a75a0c2df6219e8940d0ffe62dd85deb6a268895`,
all three command producers correctly probed the selected binary help and added
`--verify-strict` when advertised. Literal replay command identity still failed.

Measurement used `system-utils/record-getpid`, replay mode, ptrace backend, and
a harmless selected binary whose `run --help` and `record start --help` output
advertised `--verify-strict`. The binary recorded the argv it received.

`scripts/manifest-to-commands.rs` executed:

```text
--log=info --backend ptrace record start --strict --verify-strict --verify --data-dir ignored/e2e-commands/work/system-utils-record-getpid/recording --record-timeout 60 -- ignored/e2e-commands/work/system-utils-record-getpid/guest
```

`tests/manifest-cli.rs` executed:

```text
--log=info --backend ptrace record start --strict --verify-strict --verify --verify-json ignored/e2e-commands/work/system-utils-record-getpid/captures/verify.json --data-dir ignored/e2e-commands/work/system-utils-record-getpid/recording --record-timeout 60 -- ignored/e2e-commands/work/system-utils-record-getpid/guest
```

`ci/manifest-plan/src/runner.rs` executed:

```text
--log info --backend ptrace record start --strict --verify-strict --verify --verify-json /tmp/codex-runner-command-TiJ8lR/results/runs/measure-replay/system-utils-record-getpid-replay-ptrace/verify-1.json --data-dir /tmp/codex-runner-command-TiJ8lR/results/runs/measure-replay/system-utils-record-getpid-replay-ptrace/recording --record-timeout 60 -- /tmp/codex-runner-command-TiJ8lR/results/runs/measure-replay/system-utils-record-getpid-replay-ptrace/fixtures/program
```

The unresolved difference was therefore concrete: the flattened command
generator omitted `--verify-json`, while the CLI generator and the runner
included it. This record describes the measured defect at `a75a0c2d`; a later
fix must be re-measured rather than assumed from tests alone.

## Fixes prepared separately

- Hermit commit `59120a4bd1da38bb8bda35114949f9f8daeb3162` narrows the privileged workflow to `GITHUB_ACTIONS=true` plus `--unsafe-no-cgroups` and fails outside Actions.
- Parent commit `f83820c05399e5c2bd7eff1c712a0368d77400c7` makes residual or missing Demo INFO evidence fail and adds four INFO-admission regression tests. It explicitly records that Demo 5 will report RED while the VM artifacts remain bit-for-bit equal.
