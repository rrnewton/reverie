# Validate scaling measurements

This document records full Hermit validation runs at explicit concurrency levels. Add later measurements to the same table rather than replacing earlier rows.

## Measurement rules

- Launch every group from one script invocation and record the actual service start timestamps and start skew.
- Record the exact commit for every run. Do not combine results from different commits as though they measured one tree.
- Report wall time and CPU time separately for each run. For a group, report elapsed wall time from the earliest service start to the latest finish, the sum of individual wall times, and the sum of CPU times.
- Record the validation result and trailing summary, not only the process exit status.
- Record concurrent host work and the pre-launch `ci-hub load-probe` output. A result under unrecorded neighbours is not comparable with a later concurrency level.
- Frozen validation produces an explicitly non-qualifying result outside the canonical receipt ledger. It measures the named commit but does not make that commit a qualifying green.

Expected result before the first two-way measurement: wall time per run should rise relative to the solo full validate while CPU time per run stays near the solo value. If CPU time per run also rises, that is interference rather than scheduling.

## Results

| Date (UTC) | Concurrent validates | Commits | Start skew | Result per run | Wall per run | CPU per run | Earliest start to latest finish | Sum of run wall | Sum of CPU | Host before launch | Other work | Evidence |
|---|---:|---|---:|---|---|---|---:|---:|---:|---|---|---|
| 2026-08-28 | 1 | `3cc8d13d335bda3877e766c4933347e138e36d27` | n/a | PASS, qualifying full receipt; 76/76 nodes, 2,129 executed tests, zero failures and skips | 1,126 s | 3,533.217 s | 1,126 s | 1,126 s | 3,533.217 s | Not captured by this experiment | Other work not captured | Ledger run `validate-mega-lander-3cc8d13d335b-1787901620409429032-2375852-f7279055` |
| 2026-08-28 | 2 | `4ba6a04f1fbafc501caab88ced237b4ff83dfaed`; `1f221a785c871fdfc8b2182d0cb2f71dfd80d017` | 5.618 ms | PASS after retry; PASS after retry | 1,815 s; 1,791 s | 3,776.450 s; 3,752.048 s | 1,819 s | 3,606 s | 7,528.499 s | 316 CPUs; 12.639 executing cores (4.000%); 95.998% idle; 88.858% memory available; load 11.28/11.12/11.32 | ORC, hermit-149 Sabre process, node/watchman, dnf install, and type-discipline landing preparation | Frozen result and log paths below |
| future | 4 | pending | pending | pending | pending | pending | pending | pending | pending | pending | pending | pending |
| future | 8 | pending | pending | pending | pending | pending | pending | pending | pending | pending | pending | pending |

## Two-way run identities

The first invocation is excluded from the table because both runs refused before the DAG: an explicitly supplied outer systemd log path collided with the inner validate driver's independently reserved durable-log path. Both exited 75 with zero nodes executed.

The corrected invocation launched both commands together at `2026-08-28T12:34:17.937232660Z`. Systemd started both services at `2026-08-28 05:35:31 PDT`; monotonic start values give the measured 5.618 ms skew, with `1f221a` first.

| Commit | Unit | Outer log | Inner validate log | Frozen result |
|---|---|---|---|---|
| `4ba6a04f1fbafc501caab88ced237b4ff83dfaed` | `validate-hermit-147-scale2-4ba6a04f1fba-r2.service` | `/home/newton/work/dev-hermit/ignored/validate/validate-hermit-147-scale2-4ba6a04f1fba-r2.log` | `/home/newton/work/dev-hermit/ignored/validate/validate-full-4ba6a04f1fba-validate-hermit-147-scale2-4ba6a04f1fba-r2.log` | `/home/newton/work/dev-hermit/ignored/validate/frozen/validate-hermit-147-scale2-4ba6a04f1fba-r2.jsonl` |
| `1f221a785c871fdfc8b2182d0cb2f71dfd80d017` | `validate-hermit-147-scale2-1f221a785c87-r2.service` | `/home/newton/work/dev-hermit/ignored/validate/validate-hermit-147-scale2-1f221a785c87-r2.log` | `/home/newton/work/dev-hermit/ignored/validate/validate-full-1f221a785c87-validate-hermit-147-scale2-1f221a785c87-r2.log` | `/home/newton/work/dev-hermit/ignored/validate/frozen/validate-hermit-147-scale2-1f221a785c87-r2.jsonl` |

## Two-way result

Both runs executed all 76 planned outer nodes, 2,129 tests, and 307 of 679 enabled E2E cells. Both finished with zero failed or skipped nodes. Neither was a clean first-attempt pass:

- Both runs had `build.manifest_guests_in_pinned_root` fail after about 49 seconds and pass on its second attempt in 24 seconds.
- `1f221a` also recovered `backend-parity-c/renameat2-flags [ptrace/verify]` on a cell retry. Its summary records two retry occurrences; `4ba6` records one.

Relative to the solo `3cc8d13d` full validate, mean wall time per run rose from 1,126 seconds to 1,803 seconds, an increase of 60.124%. Mean CPU time per run rose from 3,533.217 seconds to 3,764.249 seconds, an increase of 6.539%. Thus the expected wall-time increase occurred, but CPU time was not conserved. Both runs performed extra retry work, so the CPU increase includes work the solo run did not perform.

The retained solo log contains zero `resource_caps` waits. The concurrent `4ba6` log contains 31 waits totaling 1,037.361 seconds; at least one node was waiting during 776.516 seconds of the run. The concurrent `1f221a` log contains 27 waits totaling 847.422 seconds; at least one node was waiting during 656.267 seconds. Those non-overlapping wait durations are the same scale as the 689-second and 665-second wall increases over the solo run. The sum of recorded node execution time did not rise: it was 3,103.367 seconds solo and 3,029.419/3,029.655 seconds in the concurrent runs.

These waits are cross-process for this launch path. `ci-hub/validate/start_unit.py::_shared_resource_caps_path` returns one user-runtime file, `/run/user/<uid>/ci-hub/resource-caps.json`, and `build_systemd_command` exports that same path as `DAGRUN_RESOURCE_CAPS_PATH` to every top-level validate. `agent-utils/rs/dagrun/src/resource_caps.rs` states that setting this path applies the counts across every scheduler process. The log text also says `shared with other scheduler processes`. This is stronger than inferring contention from the labels alone.

Recorded per-step cgroup throttled time rose from 2,653.947 seconds solo to 3,158.488 seconds for `4ba6` and 3,097.399 seconds for `1f221a`. CPU time rose 6.2–6.9%, but the two extra build attempts and the recovered cell mean this run cannot separate retry cost from other CPU interference.

The two-run validation window was 1,819 seconds from the earliest inner start to the latest inner finish. Running the two solo baseline durations consecutively would take 2,252 seconds, so the overlapping window was 433 seconds, or 19.227%, shorter. The sum of individual run wall times was 3,606 seconds; combined CPU time was 7,528.499 seconds.

Both results are deliberately frozen and non-qualifying. After completion, `ci-hub validate-status --sha` still reports `NOT-VALIDATED` with `qualifying_count=0` for both commits, and the canonical ledger contains zero rows for them. The result files close the measurement gap but do not close the canonical ledger gap; current authority rules do not allow a superseded commit to receive a qualifying receipt.

## Exact launch shape

One shell invocation started these two commands as background children and waited for both:

```bash
ci-hub validate-run \
  --checkout /home/newton/work/dev-hermit/worktrees/142/hermit-scale-4ba6a04f1fba \
  --agent hermit-147-scale2-a \
  --target 4ba6a04f1fbafc501caab88ced237b4ff83dfaed \
  --frozen-validate \
  --unit validate-hermit-147-scale2-4ba6a04f1fba-r2 \
  --json -- full &

ci-hub validate-run \
  --checkout /home/newton/work/dev-hermit/worktrees/142/hermit-scale-1f221a785c87 \
  --agent hermit-147-scale2-b \
  --target 1f221a785c871fdfc8b2182d0cb2f71dfd80d017 \
  --frozen-validate \
  --unit validate-hermit-147-scale2-1f221a785c87-r2 \
  --json -- full &

wait
```
