# Reverie backend maturity results — 2026-08-16

These are measured results, not intended status. The runtime matrix was run at
Reverie commit `f035cab0d531ba2ba1f56797992333c10bd6ad71` with:

```text
CARGO_BUILD_JOBS=1 \
BACKEND_MATURITY_SKIP_RELEASE_BUILD=1 \
BACKEND_MATURITY_SKIP_PREPARE=1 \
BACKEND_MATURITY_REPEATS=2 \
scripts/validate-backend-maturity.sh
```

The run took 25.797 seconds and exited 2 (`unmeasurable`), not green. Clean
release B0 was deliberately omitted from this runtime run. A separate clean
release attempt was stopped by the host execution policy after ptrace and KVM
B0 passed; the remaining B0 rows are therefore not awarded here.

| Backend | B1 runtime gate | B1.5 runtime gate | What the evidence compared |
| --- | --- | --- | --- |
| ptrace | PASS | PASS | Exact guest output, one-byte read replacement, stable counter1 total 40, stable counter2 total 177 with 2 processes/2 threads, and semantic write trace. |
| KVM | PASS | UNMEASURABLE | B1 used a real guest ELF and one-byte read replacement. The combined B1.5 run was denied while launching KVM test children. A separate retry returned pass but also emitted host-policy denials outside the test log, so it is not accepted as B1.5 evidence for this commit. |
| DBT | PASS | FAIL | B1 preserved exact guest output across at least ten one-byte reads. Exact counter1 emitted no summary in either repeat. Exact counter2 exited 255 in both repeats with DynamoRIO's `prototype stack overflow`, so no process/thread summary existed. The trace adapter passed. |
| SaBRe | PASS | PASS | Exact guest output, one-byte read replacement, stable counter1 total 88, stable counter2 total 179 with 2 processes/2 threads, and semantic write arguments. The trace adapter reports the return value as unknown. |
| LiteInst | PASS | PASS, with documented limitation | Exact guest output, stable counter1 total 79, stable counter2 total 79 with 1 process/1 thread, and semantic write trace. The process-tree counter2 workload timed out after 10 seconds. |
| e9patch | PASS | PASS, single-process path | Exact guest output, observed write arguments/result, exact counter totals, exit lifecycle totals, and semantic write trace. Process creation is outside this direct AOT path. |

No row in this run compared bitwise parity, canonical INFO output, positive INFO
counts, or the full Hermit corpus. B2 and above were not measured or awarded.
