# 149-cell verify pressure campaign, 10 repetitions

Date: 2026-09-11  
Host: `devbig014`, kernel `7.1.3-0_fbk0_rc18_0_gd373cd4b8dbf`  
Hermit: `68fabc2f07093d9390e403a6d5f8a63c428f64a3` (clean source tree)  
Detcore tree: `644cd5a9ca08487316b54c4a5e9dbf2cb2bde901`

## Result

The campaign ran all 149 frozen verify cells ten times: 1,490 / 1,490 scheduled cell steps started and ended. The run used all 15 KVM identities; `/dev/kvm` was opened read/write by the official host probe. No host-inapplicable substitution was used.

The scheduler reached the configured limits of 64 simultaneous manifest guests and 4 simultaneous KVM guests. It recorded zero duplicate starts, zero unpaired steps, zero OOM kills, and 17 cell wall timeouts. Total observed outer-scope wall time was 451.7 seconds; scheduler wall time was 405.8 seconds.

The 1,490 final results were 516 pass, 73 determinism failure, 151 crash/error, and 750 infrastructure error. Infrastructure and no-result rows are not compatibility evidence. At cell level, 39 / 149 passed cleanly in all ten repetitions and are promotion-review candidates; 20 / 149 failed every repetition; 13 / 149 were flaky; 77 / 149 were incomplete. No scorecard status was changed.

## Frozen population and command

Population: `ignored/compat-envelope/pressure-sample-200-never-measured-verify/eligible-population.jsonl`, SHA-256 `136d55373312bcd1dc0c5c7587357c13743cbb498e76fd736c9a2761cc1ab7eb`.

The 149 identities comprise 61 DBT, 15 KVM, 25 LiteInst, 16 ptrace, and 32 SaBRe cells. Every identity is `verify` mode in the `portable` lane.

```bash
cd /home/newton/work/dev-hermit/worktrees/slots/hermit-004/hermit
env \
  DEV_HERMIT_PARENT=/home/newton/work/dev-hermit/worktrees/slots/hermit-004 \
  DAGRUN_OUTER_MEMORY_MAX_BYTES=549755813888 \
  ./ci/compat-envelope/pressure-test.rs run \
  --results /home/newton/work/dev-hermit/worktrees/slots/hermit-004/ignored/compat-envelope/pressure-sample-200-never-measured-verify/campaign-149x10-w064-k04 \
  --cells-file /home/newton/work/dev-hermit/worktrees/slots/hermit-004/ignored/compat-envelope/pressure-sample-200-never-measured-verify/eligible-population.jsonl \
  --repetitions 10 --jobs 316 \
  --manifest-guest-cap 64 --kvm-guest-cap 4 \
  --cell-timeout 60 --run-timeout 86400
```

The generated DAG contains 1,631 steps: 1,490 unique cell steps, 140 serialized fixture-preparation steps, and one summary step. Every cell step has a 60-second wall limit and a 120-second CPU limit. All 1,490 hold `manifest_guest`; exactly 150 KVM repetitions additionally hold `kvm_guest`; the summary depends on all 1,490 cell steps.

## Measured concurrency and memory

The 16-cell cross-backend calibration population has SHA-256 `4974444eeba367b8b574568fc5cf5b9f4406de2837e514602872ddde136af9b5` and contains 3 DBT, 4 KVM, 3 LiteInst, 3 ptrace, and 3 SaBRe identities.

| Manifest/KVM cap | Repetitions | Scheduled cells | Measured peak | Outer memory peak, bytes | Step timeouts | OOM kills | Scheduler wall, s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4 / 1 | 1 | 16 | 4 | 23,883,804,672 | 0 | 0 | 382.222 |
| 32 / 2 | 3 | 48 | 17 | 27,659,960,320 | 0 | 0 | 376.633 |
| 64 / 4 | 5 | 80 | 29 | 29,738,930,176 | 0 | 0 | 380.101 |
| 128 / 8 | 10 | 160 | 58 | 35,064,401,920 | 6 | 0 | 386.305 |

The 128 / 8 calibration is the positive failure control: two ptrace timed-progress-bar and four LiteInst timed-progress-bar steps hit the unchanged 60-second scheduler wall limit. The 64 / 4 calibration completed every scheduled step with no scheduler timeout, no OOM, and no infrastructure-error identity beyond the six present at 4 / 1. Therefore 64 / 4 was the highest safe tested setting and was used for the campaign. The full workload then reached both caps.

The full campaign's declared peak was 272,730,423,296 bytes including 1,073,741,824 bytes of control headroom under the 549,755,813,888-byte outer bound. Observed memory peak was 47,257,657,344 bytes; `memory.events` recorded zero `max`, `oom`, and `oom_kill` events. Peak process count was 1,125.

## Final results

The denominator in every row is the number of scheduled repetitions for that backend.

| Backend | Pass | Determinism failure | Crash/error | Infrastructure error | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| DBT | 0 | 0 | 0 | 610 | 610 |
| KVM | 142 | 8 | 0 | 0 | 150 |
| LiteInst | 220 | 0 | 0 | 30 | 250 |
| ptrace | 29 | 23 | 0 | 108 | 160 |
| SaBRe | 125 | 42 | 151 | 2 | 320 |
| **Total** | **516** | **73** | **151** | **750** | **1,490** |

| Backend | Cells | Passed all 10 | Flaky | Failed all 10 | Incomplete |
| --- | ---: | ---: | ---: | ---: | ---: |
| DBT | 61 | 0 | 0 | 0 | 61 |
| KVM | 15 | 3 | 12 | 0 | 0 |
| LiteInst | 25 | 22 | 0 | 0 | 3 |
| ptrace | 16 | 2 | 0 | 2 | 12 |
| SaBRe | 32 | 12 | 1 | 18 | 1 |
| **Total** | **149** | **39** | **13** | **20** | **77** |

The exact 149 identities, all ten step durations per identity, quartiles, IQR, maximum, retry count, timeout count, and classification are in `ignored/compat-envelope/pressure-sample-200-never-measured-verify/campaign-149x10-w064-k04/cell-timing-and-classification.csv`. “Passed all 10” requires 10 clean passes and zero retries. “Failed all 10” contains only product outcomes; infrastructure/no-result rows remain “incomplete.”

## Timing

Quartiles use linear interpolation at positions `(n - 1) × p`. Every scheduled step, including timeouts and infrastructure results, remains in the timing denominator.

| Backend | n | p25, s | median, s | p75, s | IQR, s | max, s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| DBT | 610 | 3.370 | 3.665 | 4.027 | 0.657 | 5.530 |
| KVM | 150 | 3.817 | 4.633 | 7.195 | 3.378 | 27.042 |
| LiteInst | 250 | 4.794 | 5.084 | 6.399 | 1.605 | 60.475 |
| ptrace | 160 | 3.996 | 4.387 | 6.089 | 2.093 | 60.518 |
| SaBRe | 320 | 4.870 | 6.599 | 11.623 | 6.754 | 60.435 |
| **All cell steps** | **1,490** | **3.631** | **4.421** | **5.928** | **2.297** | **60.518** |

## Guest execution and comparison counts

The all-149 DAG has 1,490 verify steps. Each verify command requests two guest executions and one comparison, so the no-retry request is 2,980 guest executions and 1,490 comparisons. The task text's “2,000 comparison steps” is inconsistent with its fixed 1,490 repetitions; no result was changed to manufacture that count.

The summary records 1,869 attributable harness attempts after 379 repetitions retried. Typed final reports prove 667 matched comparisons, 73 divergent comparisons, and 10 infrastructure-error comparisons; 740 final rows have no trustworthy comparison report. Including the 89 retained earlier divergent attempts, the campaign proves 839 completed comparisons and 1,678 corresponding guest executions. This is a lower bound on processes started, not an inference that the 740 no-report rows executed no guest code.

## Accepted series rows

The automatic full-batch append was atomically refused and wrote nothing because 20 raw timeout rows have `outcome=ERROR`, `result=timeout`, and `failure_class=no_result`; the shared checker requires timeout results to carry `outcome=FAIL`. A second direct full-batch attempt was also atomically refused because comparison-mode crash/error rows did not carry the required `no_verdict_evidence`. Source was not changed to bypass either check.

Rows were then selected only when the pressure summary marked the row valid, the raw classification agreed with the summary classification, and the existing shared SeriesRow checker accepted the projected row. That produced 737 accepted observations, compressed by the existing writer to 296 append-only series events: 516 pass, 162 divergence (73 final plus 89 retained earlier attempts), 10 infrastructure error, and 49 no-result observations. The spool was read back through the shared checker successfully.

Unpublished spool: `/home/newton/work/dev-hermit/ignored/ci-hub/series-spool/campaign-149x10-w064-k04.jsonl`  
Spool SHA-256: `3da6d1261866e35881f9fe7b787a601e4cfbcd10cd6720a7ff72cfa4dc4ca7b4`

The 151 crash/error final rows and all rows whose summary classification disagreed with their raw classification remain preserved in the campaign directory and were not presented to the accepted series as a different outcome.

## Primary artifacts and repeatability

Campaign directory: `/home/newton/work/dev-hermit/worktrees/slots/hermit-004/ignored/compat-envelope/pressure-sample-200-never-measured-verify/campaign-149x10-w064-k04`

| Artifact | SHA-256 |
| --- | --- |
| `dag.json` | `9662e38a480146d860828a49788a7657c39394819056c75fe9670b00cb1cf482` |
| `run.json` | `50a6788a8df5df8e62a92ceca28283bed8d22da99e7e7b1b967ffb8a5fdc6ecd` |
| `runner-outcomes.json` | `a93071a43a808d6b921e9a8f30b74b13fd86fc4c638bb2fe47b4c2b4a6e0087c` |
| `summary.json` | `238e692e14e56cb892aafeb9b710a037d277f0fee6d2a1c541c526e977b6a76f` |
| `outer-cgroup-samples.jsonl` | `e99e441cf4459e8e338eae9e01b694d02b545e2e6196c55d7bed9af14f1a46e0` |
| `campaign-audit.json` | `e956fab9bfa70172e191e0869d47499b55f6f076b6dd98a374fa4a803735053a` |
| `cell-timing-and-classification.csv` | `7b058b12e5d9f6681e903a7786b7a8252e7b5ed4a9830c6a52c6c424cb2fa416` |
| `accepted-series-rows.jsonl` | `ab9e0a1edc088b23b53c27c258351a17f4a42954c048560f41561946f2296a07` |

The directory also retains every step log, journal event, raw cell result, invocation command, explicit environment, verification report, verification log, captured stdout/stderr, and per-run artifact. It occupies 3.7 GiB. The isolated checkout was removed only after the runner retained those artifacts.

To repeat the process, regenerate the DAG with `pressure-test.rs plan` using the same population file, SHA, repetitions, limits, and caps; verify the counts in `dag.json` and `run.json`; run the command above; recompute `campaign-audit.json` and the CSV with `ignored/compat-envelope/pressure-sample-200-never-measured-verify/analyze-campaign.py`; then pass only checker-accepted rows to `ci-hub/series/series.py append-cells`. Publication of the existing spool into tracked shards is a separate reviewed operation and was not performed by this measurement-only task.
