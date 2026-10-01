# 2026-10-01 flake census: green e2e cells 10x and nextest stress, retries off

Owner request: polish the stress tooling onto the shared harness, run the green e2e cells 10 times with retries off, and report how many flakes there really are, with infrastructure failures separated from product failures.

## Result in brief

| Population | Units | Runs | CLEAN | FLAKY | FAILING | INCOMPLETE |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| E2E manifest cells (green in the last clean full validation of main) | 1,084 cells | 10,840 | 863 | **1** | 0 | 220 |
| `test.detcore_misc` (nextest) | 28 tests | 500 iterations (14,000 test runs) | 27 | **1** | 0 | 0 |
| `test.liteinst_strict` (nextest) | 25 tests | 5 iterations (125 test runs) | 25 | 0 | 0 | 0 |

- **E2E:** 8,689 of the 10,840 runs reached a product verdict (8,688 passes and 1 product failure, a 0.012% failure rate). The other 2,151 runs gave no product verdict: 2,150 came from two pressure-test environment gaps (1,890 compat runs and 260 DBT runs) and 1 was a Hermit internal error on KVM. The classifier kept all of them out of the flake count.
- **vfork, the key number:** `tests_misc vfork::vfork_parent_resumes_after_child_exec` passed **484/500 = 96.8%**, so it fails **3.2%** of the time. That matches the 3-7% recorded in https://github.com/rrnewton/hermit/issues/3357. Every other detcore_misc test passed 500/500.
- **Every non-CLEAN e2e result and most of the nextest stress ran while a full validation was also running** (see "Host conditions"). Both e2e events fall inside that window.

## What ran

- **Hermit:** `39762829129ea8c52e65c4d6cf072b6c04574d16` for the e2e census, which is main `342433e2` plus the `--no-retry` change. The nextest stress used the same source plus the new `scripts/stress-test.sh`. None of these changes touch product code. The tooling landed through https://github.com/rrnewton/hermit/pull/3476 as hermit main `e58a9f086` + `b40707ad6` at 2026-10-01T20:53Z. The landed tree `7b69ccbe34c5` is byte-identical to the validated head `2a7c0584f`: full validation PASSED with 87 nodes and 3,920 of 3,920 tests, and the adversarial review approved it after one round of fixes. In that round, `flake_verdict` was tightened so that a pass whose evidence contradicts itself is not CLEAN, which is why the e2e row reads 863 CLEAN rather than the 867 the census run first printed.
- **Population:** every cell that passed in the last clean full validation of main (`validate-liteinst-iced-fix-342433e251d7-…`: 1,089 e2e cells, all first-attempt PASS). pressure-test `--green` selects 1,084 of them. The 5 `custom`-mode cells are outside the scorecard's comparable population that pressure-test draws from, so they were **not run**: `c-programs/environment-and-workdir@ptrace`, `c-programs/io-uring-fallback@ptrace`, `c-programs/io-uring-fallback@dbt`, `system-utils/clock-determinism@liteinst`, `system-utils/clock-determinism@ptrace`.
- **Retries:** off. Every repetition was exactly one harness attempt (`test-harness run --no-retry`), and the summary records `retried_repetitions = 0` across 10,840 runs. Nextest ran with `--retries 0`.
- **E2E command:**
  `ci/compat-envelope/pressure-test.rs run --green --repetitions 10 --no-retry --run-timeout 86400 --jobs 316 --manifest-guest-cap 40 --kvm-guest-cap 8`
  The plan had 11,400 steps: 10,840 cells, 550 serialized fixture preparations, 9 build/setup steps and 1 summary step. The memory check printed by the plan capped manifest guests at 41.
- **Nextest command:** `scripts/stress-test.sh -n N -- <the validate node's selection>`, which runs `cargo nextest run --profile ci --no-fail-fast --retries 0 --stress-count N`. The detcore_misc selection was `-p hermit-detcore --test tests_misc -j 1` with the node's five `--skip` flags; liteinst_strict was `-p hermit --features third-party-backends --test liteinst_advanced -j 1`.
- **Why these counts:** detcore_misc takes 1.4 s per iteration, so 500 iterations cost 12 minutes and resolve a 3% flake to about ±0.8 percentage points. liteinst_strict takes 363 s per iteration (serial, `-j 1`), so 5 iterations already cost 30 minutes. That is enough to catch a flake of roughly 10% or more, not a rare one.

## Cost

| Run | Window (UTC) | Wall | CPU (user + sys) |
| --- | --- | ---: | ---: |
| E2E 10x, including fresh checkout and build | 19:04:27-19:25:56 | 21 min 28 s | 26,070 s (16,827 + 9,242) |
| detcore_misc 500x | 19:17:15-19:29:08 | 11 min 53 s | 696 s (380 + 316) |
| liteinst_strict 5x | 19:29:08-19:59:22 | 30 min 14 s | 1,835 s (1,560 + 274) |

For comparison, a full validation of main takes about 22 minutes (342433e2: 17:37-17:59Z). The e2e census therefore costs about one full validation of wall time.

## Host conditions (some flakes depend on load)

- devbig030 has 316 cores. The 1-minute load average sampled during the e2e census ranged from 12 to 75 (median about 60 from 19:14Z to the end of the run).
- **Overlap with full validations.** The census held a box-exclusive `bench` lease (from `validate-lock acquire`). Validation `c9015c08` (`…2939731-0a265ad0`) queued behind it at 19:09:23Z, took over the global anchor at 19:12:04Z, and ran until about 19:27:30Z. The anchor's recorded owner was the short-lived `acquire` CLI process, so the waiting validation treated the lease as dead. `renew` kept reporting success the whole time. This is a lock defect, filed as https://github.com/rrnewton/dev-hermit/issues/481. **Every e2e cell executed inside the 19:12-19:27Z overlap window**, including both non-CLEAN events (19:15:16Z and 19:17:50Z). For detcore_misc, iterations 1-about 385 overlapped `c9015c08` and the e2e census. liteinst_strict overlapped validations `stress-infra-0fc789a69004` (19:33-19:55Z) and `ee479da8` (19:38-20:01Z).
- vfork failure timing: 7 of the 16 failures came in iterations 56-385, under load average 35-75. The other 9 came in iterations 403-485, after the e2e census ended, under load average about 10-30. This sample does not show the flake rising with load.

## Every non-CLEAN result

### Product flakes

| Unit | Pass rate | Failure class | Signature | Issue |
| --- | ---: | --- | --- | --- |
| `c-programs/proc-locks` verify@sabre | 9/10 | product_failure (determinism-failure) | first divergence at turn 9, record 52: `read(6, …, 262143)` returned `Ok(80)` in one run and `Ok(997)` in the other. The guest's read of `/proc/locks` saw the host's live lock table. The same cell passed 10/10 on ptrace and kvm. | new: https://github.com/rrnewton/hermit/issues/3472 (related: https://github.com/rrnewton/hermit/issues/2152) |
| `tests_misc vfork::vfork_parent_resumes_after_child_exec` (test.detcore_misc) | 484/500 (96.8%) | product (nextest `test failure with exit code 101`) | panics at `detcore/tests/testutils/src/lib.rs:716` (7x) and `:724` (9x), followed by `:786`/`:826`. Some failures happen in Test Run 1, some in Test Run 2. | existing: https://github.com/rrnewton/hermit/issues/3357 (the fix exists only in pending reverie PRs) |

### Hermit internal errors with no verdict (not host infrastructure)

| Unit | Pass rate | Class | Signature | Issue |
| --- | ---: | --- | --- | --- |
| `c-programs/getitimer-determinism-probe` verify@kvm | 9/10 + 1 no-result | `no_result` (incomplete-verification-evidence) | `HERMIT_INTERNAL_FAILURE class=cli-error` / `Error: KVM producer-captured mount identities do not match the backend namespace`, after 42 ms | new: https://github.com/rrnewton/hermit/issues/3473 |

### Passes whose retained evidence contradicts itself (not counted as CLEAN)

| Unit | Harness result | Class | Signature |
| --- | --- | --- | --- |
| `c-programs/nanosleep-threads-simple`, `shared-futex-c/qemu-hello`, `system-utils/sh-exit-status`, `util-c/pmu-skid` (all verify@ptrace) | 10/10 PASS each | sample classification `incomplete` (`unknown_history_repetitions = 10`) | pressure-test's evidence reader reports `pressure_evidence matched report contradicts its inner process disposition` for all 40 runs. The census output first printed these 4 cells as CLEAN. Review of https://github.com/rrnewton/hermit/pull/3476 caught it, and the verdict now requires an intact history, which makes them INCOMPLETE. The harness itself saw no failure; the disagreement is between pressure-test's consistency check and the retained reports. |

### Infrastructure: the runner could not measure these cells

These are not flakes. They are gaps in what pressure-test can run outside validation.

| Group | Cells | Runs | Class | Signature | Issue |
| --- | ---: | ---: | --- | --- | --- |
| all `compat/*` cells, verify@ptrace | 189 | 1,890 | prerequisite: 111 cells recorded as infrastructure failures, 78 as missing | `its argv names {{VALIDATE_RUN_STATE}} but VALIDATE_RUN_STATE is not set`. The pressure graph has no `compatprep.fixtures` step. | new: https://github.com/rrnewton/hermit/issues/3474 |
| all DBT cells, verify@dbt | 26 | 260 | no_result (mixed) | `Error: unshare mount namespace: Operation not permitted`. pressure-test runs cells on the host, outside the pinned-root user namespace that validation uses (`unshare -m true` exits 1 there). The 2026-09-11 campaign shows the same thing (DBT 610/610). | new: https://github.com/rrnewton/hermit/issues/3475 |

No OOM kill, timeout or SIGKILL appeared in any run.

Known flakes that did **not** reproduce: PMU skid overshoot (https://github.com/rrnewton/hermit/issues/1845) was `c-programs/getrusage-self-accounting` verify@ptrace 10/10 and `util-c/pmu-skid` verify@ptrace 10/10. The iced decoder panic (https://github.com/rrnewton/hermit/issues/3462) is fixed on main `342433e2`, and every LiteInst cell (146) was CLEAN.

## The shared classifier

The classifier is the one the harness already has. There is no new one.

- **E2E:** each harness result row carries a `failure_class` produced by the code that runs the cell (`product_failure`, `understood_infrastructure_failure`, `understood_prerequisite_failure` or `no_result`; `CellVerdict::{Timeout, Oom}` map to `no_result`). pressure-test already folded these into per-cell counts. The PR adds only the verdict on top. The verdict rule is ci-hub/stress's flaky-is-red rule: only product failures can make a cell FLAKY or FAILING, and a run that produced no product verdict can never make a cell read CLEAN.
- **Nextest:** the typed `type` attribute that nextest writes on each JUnit `<failure>`, mapped onto the same classes. `test failure …` and an abort by any signal other than SIGKILL are product failures; `test timeout` and SIGKILL are no-result. The old `scripts/stress-test.sh` matched failure-message text against words such as "eperm" and "namespace". That code is deleted.
- Two caveats found while building this. First, nextest 0.9.143 exits 0 from a `--stress-count` run even when iterations failed (it printed "484 passed, 16 failed" and returned 0), so `stress-test.sh` reads the JUnit report, not the exit status. Second, `ci/run-nextest-counted.sh`, which applies the validate node's per-test CPU budgets, refuses `--stress-count`: its per-attempt CPU records are keyed without the stress iteration. The nextest half therefore uses the canonical `.config/nextest.toml` timeouts.

## The ci-hub/stress decision: deleted

`ci-hub/stress/` (the August flaky-is-red nightly) is **deleted** on dev-hermit main in `7f9d59273`, together with its alarm test and suite registration. It only ever stressed one test (vfork), depended on an external `hermit-multisect` `matched.sh` and a calibrator binary that is absent on devbig030, and was never scheduled here. Its one valuable idea, the rule that anything other than 0% or 100% passing is FLAKY, now lives in the shared tools: `flake_verdict` in `ci/compat-envelope/pressure-test.rs`, and the same rule in `scripts/stress-test.sh`. Both use the harness's failure classes.

## How to rerun

```console
# E2E, from a clean hermit checkout (about 22 min on devbig030)
./ci/compat-envelope/pressure-test.rs run --green --repetitions 10 --no-retry \
  --run-timeout 86400 --jobs 316 --manifest-guest-cap 40 --kvm-guest-cap 8
# Rust tests: one validate test.* node's selection
scripts/stress-test.sh -n 500 -- -p hermit-detcore --test tests_misc -j 1 -- --skip has_rdrand_without_detcore
```

Hold the box while these run. `validate-lock acquire --kind bench` does **not** currently exclude a waiting validation (https://github.com/rrnewton/dev-hermit/issues/481).

Retained evidence on devbig030: `worktrees/slots/stress-infra/ignored/stress/{e2e-10x,nx-detcore-500,nx-liteinst-5}/` and `ignored/stress-20261001/`, which holds the load samples, time output and driver scripts.
