---
name: progress-rubric
description: "Create evidence-based Hermit progress reports from exact-SHA live measurements, strict unstripped determinism comparisons, and durable validation receipts."
---

# Progress report rubric

Write the report to the durable task-owned path selected by the task. Product
measurements run only in the agent's registered slot at one clean current-main
SHA; never switch a primary or invent a worktree. If relevant product code moves,
rerun affected cells at the new SHA.

## Evidence contract

For every cell record repository, full SHA, UTC time, host/kernel/toolchain,
backend, exact command, exit status, stdout, stderr, INFO log, timeout, and every
relaxation. `BLOCKED`, `NOT RUN`, and `INCOMPLETE` are honest outcomes but never
passes. Unlanded work is a linked footnote and cannot alter main-branch totals.

Strict verification/parity means equality of all observable channels: exit
code, stdout bytes, stderr bytes, and complete INFO-log bytes. Do not delete,
mask, normalize, or regex-strip numbers, addresses, branch counts, virtual-time
values, or durations to obtain equality. A canonicalization is admissible only
when the repository specification explicitly defines the values as equivalent,
the transform is identity/provenance bound and applied uniformly, and planted
positive/negative tests prove that distinguishable executions remain
distinguishable. Otherwise report the mismatch.

For compatibility-scorecard cells, apply the versioned
[strict green standard](../../../ai_docs/2026-08-10-strict-standard-for-green.md)
rather than deriving a verdict from column names. The owner decision (2026-08-10)
is verbatim: **"GREEN MEANS WE RAN TWICE (DETERMINISM OR PARITY) AND GOT FULL
RUNS WITH IDENTICAL OUTPUT."** Incomplete, absent, crashed, divergent, or
errored comparisons are red. `stack_parity` and `heap_parity` are three-valued
invocation-mode fields, not outcomes; historical blank means unknown mode.
This routing remains current until the owner changes that scorecard definition
or the versioned standard is superseded.

## Required report shape

1. Snapshot: full SHAs and environment.
2. Coverage slope: `passed / attempted` per mode and the first lost workload.
3. Same-app matrix: ptrace strict verify, record/replay, DBT, KVM, SaBRe,
   LiteInst, and e9patch as available. Use identical inputs.
4. Repository health: focused tests plus validation classified under skill
   `hermit-validation-authority`; report observational hosted infrastructure separately.
5. Gaps and next actions ordered by the first mode that loses parity.
6. Unlanded footnote, at most three linked items.

Use at least a trivial ELF, file-processing tool, interpreter, concurrent
pipeline, and toolchain frontend when those workloads are available from main.
For a backend-wide preflight failure, run one representative probe, quote the
failure, and mark the rest `BLOCKED` with the shared cause. For record/replay,
distinguish record timeout, replay divergence, channel mismatch, and a complete
round trip.

## Validation and accounting

Run focused probes directly from the assigned product slot with explicit
bounds. Follow skill `hermit-validation-authority` for the full-run producer, green
qualification, evidence fields, and local-versus-GitHub boundary.

Use one denominator for every row in a comparison. Preserve executed, selected,
filtered, ignored, failed, timeout, and skipped counts separately. Recompute
summaries from tracked textual results, keep binaries/log volumes under ignored
storage, inspect explicit staged paths, and never stage recordings, build trees,
or another agent's changes.
