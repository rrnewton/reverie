# Reverie PR 749: does the test tool's `on_exit_thread` hook cause the in-flight children?

## Background
- **Pull request:** https://github.com/rrnewton/reverie/pull/749 changes two things:
  - the order in which safeptrace's notifier retires a traced thread (product code);
  - the reverie-ptrace fd-capacity tests (test code).
- **Test (2):** `retired_threads_release_descriptors_under_reduced_nofile`. It spawns, joins and retires 96 threads under a soft open-file limit of 128, and counts the tracer's open descriptors at each step (a "marker").
- **In flight:** a child is in flight at a marker when the guest has already joined it but its tracer task has not yet finished retiring it.
- **The question:** at the 749 head, about 3.8% of markers show one child in flight; on reverie main it is about 0.2%. The head adds an `on_exit_thread` override to the test tool. Is that override the cause?

## Result
Each variant was run 40 times on one shared host, in rotated order.

| Variant | What it is | Markers with a child in flight | Runs with at least one |
|---|---|---|---|
| A | 749 head 410312f4f7faa3bf3c6e329be6acfedbd5c7df85 | 147 of 3840 (3.83%) | 38 of 40 |
| B | A minus the override (fatal_capacity_tests.rs lines 231-268) | 123 of 3840 (3.20%) | 39 of 40 |
| C | reverie main dbf2b5c8880bac5fd6a06fa1f296b3eec3590569 | 6 of 3840 (0.16%) | 6 of 40 |

- **Outcomes:** all 120 runs passed. No marker had two children in flight. The largest excess over the baseline was +7 in every variant.
- **Conclusion:** removing the override leaves most of the rise in place, so the override is not the main cause. B and C still differ in three ways at once (the notifier reorder, the test-only instrumentation, and how much the handler prints), and this run cannot separate them.

## Files
- **REPORT.md:** the full report. The worker that ran the measurement wrote it, and the coordinator saved it from the worker's returned text. Its own paths name scratch locations on the measurement host, which map to this directory as follows:
  - `/tmp/claude-coord-notes/attr749/<file>` is `<file>` here.
  - `/tmp/cml-attr749-1227/runs/` (240 files), and its archive `raw-runs.tar.gz`, are `raw-runs/` here. The repository ignores `.gz` files under `ai_docs/`, so the logs are stored uncompressed. The sha256 of all 240 files matches the originals.
  - The binaries `/tmp/cml-attr749-1227/bin/{A,B,C}` (about 394 MB each) are not kept. Their sha256 values are in `evidence/sha256-all.txt`.
- **runs.csv:** one row per run.
- **markers.csv:** one row per marker.
- **invocations.csv:** one row per invocation, with loadavg before and after.
- **measure.py:** the run driver.
- **parse_runs.py:** the parser.
- **recount.awk:** an independent count taken from the raw stderr files.
- **build.sh:** the build command.
- **B-deletion.diff:** the only source change in B.
- **evidence/:** build records, test lists, symbol listings, the notifier diff (main to head), and the parser output.

## Recount
To recount from the raw logs:

```
awk -f recount.awk raw-runs/*.err
```

The coordinator ran this recount at 2026-10-01T13:07Z, against the original files, and got:

```
A markers=3840 inflight_ge1=147 inflight_ge2=0 max_excess=+7
B markers=3840 inflight_ge1=123 inflight_ge2=0 max_excess=+7
C markers=3840 inflight_ge1=6 inflight_ge2=0 max_excess=+7
```
