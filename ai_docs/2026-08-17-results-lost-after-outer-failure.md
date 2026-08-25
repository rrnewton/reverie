# Results lost after an outer failure

Date: 2026-08-17

## Conclusion

Three independent measurements found the same structural failure across the
validation runner, ptrace verification, and DBT execution:

> A run established a result, a later outer event prevented publication, and
> the system reported that nothing had been established. In every measured
> case, the result was already present in retained evidence on disk.

The required correction is not to treat every partial run as a result. The
guard is equally important:

> A run that crashes before establishing anything must still report
> `no_result`. The distinction is between “diverged, then crashed” and “crashed
> before we knew.” A matching prefix, an unmatched tail caused by truncation,
> zero selected messages, or unreadable evidence cannot support a fabricated
> verdict.

## Measured instances

| Instance | What the guest did | What was established | Later outer event | What was reported |
| --- | --- | --- | --- | --- |
| Bucket kill | `backend-parity-c/memfd-create` created and checked an anonymous memfd; `c-programs/ioctl-fioclex` applied `FIOCLEX` and checked the result | Canonical ptrace comparison passed: 127/127 INFO with `memfd_create ok=5`; 138/138 INFO with `fioclex-ok` | The category bucket was killed before its remaining work and result publication completed | No cell row for either already-completed result |
| Crash after divergence | `language-runtimes/node-v8-jit` first ran the host Node wrapper and its `scribe_cat perfpipe_nodejs_dev_usage` child, then launched `/bin/node` for a hot integer loop that exercises V8 compilation | The two ptrace logs had already diverged at the wrapper's `wait4` of telemetry child 49 | Run2 later suffered `HERMIT_SKID_OVERSHOOT`; Reverie panicked and the container exited by `SIGSEGV` | `verdict: no_result`, `comparison: null` |
| Report never written | Six DBT cells ran twice, exited zero, printed their expected guest output, and matched the backend-local guest-memory hashes | The DBT-specific comparison established that its two executions agreed under that narrower observation | `run_dbt` returned without invoking the canonical comparator or writing its report | The initial `no_result` stamp remained in place |

These measurements do not make the three established results equivalent.
The first two ptrace results are canonical INFO evidence. The DBT memory-hash
comparison is not canonical and must not be counted as L2; the point is that
the system discarded a real, narrower observation instead of recording what it
had and what it had not established.

## 1. Bucket kill after canonical passes

Two ptrace cells completed canonical comparison before their outer category
bucket was killed:

```text
backend-parity-c/memfd-create
  guest output: memfd_create ok=5
  canonical INFO: 127 / 127

c-programs/ioctl-fioclex
  guest output: fioclex-ok
  canonical INFO: 138 / 138
```

The enclosing buckets stopped later:

```text
backend-parity-c: 33 of 80 rows in 582.4s
c-programs:        36 of 136 rows in 586.7s
```

The completed comparison output existed, but publication was ordered after
work that the outer kill interrupted. The result set therefore described these
cells as absent rather than as completed passes. The ordering correction under
review by `m2audit` addresses this case by publishing a completed row before
subsequent announcement or outer teardown can discard it.

## 2. Node divergence followed by a PMU-skid crash

### Context

Cell: `language-runtimes/node-v8-jit`

Backend and assurance: ptrace, INFO, `--base-env=minimal --strict
--verify-strict --verify`, RCB time enabled, no relaxations, Hermit commit
`da85fa31e1c667a534d12414efbf94f738565174`.

The shell fixture invokes `/usr/local/bin/node`. On this host that is a wrapper
which starts `/usr/local/bin/scribe_cat perfpipe_nodejs_dev_usage`, waits for
that telemetry child, and only then executes `/bin/node -e ...`. The JavaScript
program runs a hot integer loop using `Math.imul` and `Math.random`, exercising
V8 interpreter/compiler and garbage-collector activity.

### Two calibrations, both retained

The first-divergence location moves because the underlying child-reaping result
is host-timed. The movement is evidence of intermittency, not an inconsistency
to tidy away:

| Measurement | First divergence | Completed reference run |
| --- | --- | --- |
| RCB exposure sample | turn 1500, virtual ns `1767225617335485280`, syscall `#220` | 1,585 scheduler turns |
| Retained crash pair | turn 1726, virtual ns `1767225617391577180`, syscall `#220` | 1,811 scheduler turns |

In the retained crash pair, the first differing canonical records were:

```text
run 1:
INFO detcore: DETLOG [syscall][detcore, dtid 25] finish syscall #220: wait4(-1, 0x7fffffffd7a0, WaitPidFlag(0x0), NULL) = Ok(49)

run 2:
INFO detcore::scheduler: [scheduler] >>>>>>>

 NONCOMMIT turn 1727, SKIP dettid 25 polling resource Resources { tid: DetPid(25), resources: {InternalIOPolling: W}, poll_attempt: 59, fyi: "wait4" }
```

At that point dtid 25 is the wrapper shell waiting for telemetry process 49.
Run1 observes the process as physically reapable; Run2 continues polling.
This occurs before `/bin/node` starts.

Run2 eventually reaped child 49, executed `/bin/node`, and continued to turn
2024. Its final completed syscall was `#14812`; the surrounding Node/V8 work
included `clock_gettime`, changing a 245,760-byte region from writable and
executable to executable, and adjusting the program break. The PMU then landed
past its target:

```text
HERMIT_SKID_OVERSHOOT rcb_actual=64404807 rcb_target=64403560 skid_margin=1000 overshoot=1247

Clock perf counter exceeds target value at start of attempted single-step:
64404807 > 64403560. Consider increasing skid margin for this CPU.

Process exited with code: Signaled(SIGSEGV, true)
```

The verification artifact nevertheless contained:

```json
{
  "verified": false,
  "bitwise_parity": false,
  "verdict": "no_result",
  "comparison": null,
  "compared_log_messages": null,
  "first_divergent_scheduler_turn": null,
  "first_divergent_virtual_nanoseconds": null
}
```

### Why the current code discards the established divergence

At `da85fa31`:

- `hermit-cli/src/bin/hermit/main.rs:298-321` atomically writes the initial
  invocation-bound `no_result` stamp.
- `hermit-cli/src/bin/hermit/run.rs:2838-2845` returns immediately when Run2
  returns an error, retaining logs only when requested.
- The canonical `compare_two_runs` call at `run.rs:2871-2900` is therefore not
  reached.
- Terminal report publication at `run.rs:2902-2908` is also not reached, so the
  pending `no_result` remains.
- `hermit-cli/src/bin/hermit/verify.rs:474-488` defines that pending report with
  null comparison, counts, exit information, and divergence calibration.

The bucket-ordering fix cannot cover this path because there is no terminal
verification outcome for the outer runner to publish. The verifier must first
preserve a divergence already proved by the logs, then return the later crash.

### Runnable commands and actual output

The retained crash was produced with:

```sh
env \
  RUSTUP_HOME=/home/newton/.rustup \
  CARGO_HOME=/home/newton/.cargo \
  HERMIT_BIN=/home/newton/work/dev-hermit/hermit/target/release/hermit \
  E2E_RESULT_ROOT=/home/newton/work/dev-hermit/hermit/target/ignored/node-v8-reconcile-da85/retained/results \
  E2E_BUILD_ROOT=/home/newton/work/dev-hermit/hermit/target/ignored/node-v8-reconcile-da85/retained/build \
  E2E_RUN_ID=node-v8-retained-11 \
  E2E_KEEP_VERIFY_LOGS=1 \
  /home/newton/work/dev-hermit/hermit/target/debug/test-harness run \
    --test language-runtimes/node-v8-jit \
    --mode verify \
    --backend ptrace \
    --include-manual
```

The already-established divergence can be reproduced directly from the logs:

```sh
target/release/hermit log-diff \
  --canonical-info --no-color --limit 3 --syscall-history 5 \
  target/ignored/node-v8-reconcile-da85/retained/results/runs/node-v8-retained-11/language-runtimes-node-v8-jit-verify-ptrace/verify-logs/verify-1/run1_log_Fe5p9 \
  target/ignored/node-v8-reconcile-da85/retained/results/runs/node-v8-retained-11/language-runtimes-node-v8-jit-verify-ptrace/verify-logs/verify-1/run2_log_A9rYw
```

Actual summary:

```text
Logs contain 131763 | 130529 INFO messages
first_divergent_scheduler_turn: 1726
first_divergent_virtual_nanoseconds: 1767225617391577180
Done processing logs, differences found.
```

Across the reconciliation measurement, the exact validate-shape command
produced 41 completed divergences, 10 canonical passes, and one PMU crash. A
second 20-attempt bracket differing only by retained-log options produced eight
completed divergences, 11 canonical passes, and one PMU crash. The retained
Run2 crash above proves directly that the PMU crash can occur after the
`wait4` divergence has already been established.

## 3. DBT executes two runs but never writes the canonical report

Six measured DBT cells ran twice, exited zero, produced their expected guest
output, and matched the DBT path's guest-memory hashes. The DBT path printed a
success message for that narrower comparison, but it did not invoke the
canonical INFO comparator and did not overwrite the pending `no_result` report.

The code path is explicit:

- `hermit-cli/src/bin/hermit/main.rs:298-321` writes the pending `no_result`.
- The comment at `main.rs:309-310` records that the DBT arm returns through
  `run_dbt` and never reaches `verify()`.
- `hermit-cli/src/bin/hermit/run.rs:2871-2908` contains the canonical comparator
  and terminal report write that DBT bypasses.

An earlier, separately retained 11-cell measurement documents the same producer
path in `ai_docs/archived/2026-08/DBT_CANONICAL_RERUN_11_20260815.md`: all 11
executed the DBT-specific two-run comparison and matched memory hashes, while
all 11 canonical requests remained `no_result`. That earlier measurement does
not turn the DBT-specific comparison into canonical evidence; it independently
confirms the missing-report mechanism.

Representative actual DBT output from the retained audit was:

```text
:: Comparing DBT observed guest-memory hashes...
5d9478fc8d30b012 | 5d9478fc8d30b012
:: Success: deterministic
```

The honest correction is to route DBT through the canonical comparator or have
`run_dbt` publish exactly what it compared. It must not label matching memory
hashes as canonical INFO parity.

## Required guard and implementation boundary

The correction must preserve the asymmetry between proving a difference and
proving a match:

- One concrete differing canonical record present on both sides is sufficient
  to establish `diverged`, even if a later crash prevents the rest of the run.
- A matching prefix is not sufficient to establish `matched`.
- An unmatched tail caused by a crash is not by itself a determinism
  divergence.
- Zero selected messages, unreadable logs, a Run1 crash before Run2, or a Run2
  crash before any differing paired record remain `no_result`.
- When divergence was established before a crash, both facts must survive:
  the report must retain the canonical divergence and the outer result must
  retain the crash classification/status.

The current log comparator does not yet expose enough structure for that guard:
`detcore/src/logdiff.rs:502-505` detects a differing record present on both
sides, while `logdiff.rs:508-511` and `:742-767` also treat a mere unmatched
tail as `diff_found`. A crash recovery path must distinguish those cases before
it can safely replace `no_result` with `diverged`.

This document scopes the observed failure and the guard. It does not implement
the cross-subsystem correction.

## Evidence locations at measurement time

- Node summary and rows:
  `/home/newton/work/dev-hermit/hermit/target/ignored/node-v8-reconcile-da85/`
- Retained Node crash logs:
  `.../retained/results/runs/node-v8-retained-11/language-runtimes-node-v8-jit-verify-ptrace/verify-logs/verify-1/`
- Earlier DBT producer-path audit:
  `ai_docs/archived/2026-08/DBT_CANONICAL_RERUN_11_20260815.md`

The ignored Node paths are cited for provenance but are not the durable record;
the calibrated values and verbatim records required to understand the finding
are reproduced in this tracked document.
