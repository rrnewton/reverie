# Reverie backend maturity results — 2026-08-16

These are measured results, not intended status.

## Clean-checkout validator run

The full validator was run from a clean checkout at Reverie commit
`bce958ac2123ab7e94aa1afcaad6ab48f4e856c0` with:

```text
CARGO_BUILD_JOBS=1 scripts/validate-backend-maturity.sh
```

The run stayed non-green after the host execution policy began denying build
and test children. The raw outcomes were:

| Backend | B0 | B1 | B1.5 | Maximum established by this run |
| --- | --- | --- | --- | --- |
| ptrace | PASS | UNMEASURABLE | Not run after B1 | B0 |
| KVM | PASS | UNMEASURABLE | UNMEASURABLE | B0 |
| DBT | UNMEASURABLE | UNMEASURABLE | Not run after B1 | None |
| SaBRe | FAIL as emitted by `bce958ac`; the classifier's own `grep` was denied | UNMEASURABLE | Not run after B1 | None |
| LiteInst | UNMEASURABLE | PASS | PASS, with process-tree limitation | None, because B0 was unmeasurable |
| e9patch | UNMEASURABLE | PASS | PASS, single-process path | None, because B0 was unmeasurable |

The SaBRe B0 `FAIL` above is retained as the raw output, not reinterpreted.
Commit `f035cab0d531ba2ba1f56797992333c10bd6ad71` corrected the classifier to use
shell built-ins, so a policy denial of the classifier itself is now reported as
`UNMEASURABLE` while still keeping validation non-green.

## Runtime matrix

The runtime matrix was then run at
`f035cab0d531ba2ba1f56797992333c10bd6ad71` with existing artifacts and two
repetitions. It took 25.797 seconds and exited 2 (`unmeasurable`), not green:

| Backend | B1 runtime gate | B1.5 runtime gate | What the evidence compared |
| --- | --- | --- | --- |
| ptrace | PASS | PASS | Exact guest output, one-byte read replacement, stable counter1 total 40, stable counter2 total 177 with 2 processes/2 threads, and semantic write trace. |
| KVM | PASS | UNMEASURABLE | B1 used a real guest ELF and one-byte read replacement. The combined B1.5 run was denied while launching KVM test children. A separate retry returned pass but also emitted host-policy denials outside the test log, so it is not accepted as B1.5 evidence. |
| DBT | PASS | FAIL | B1 preserved exact guest output across at least ten one-byte reads. Exact counter1 emitted no summary in either repeat. Exact counter2 exited 255 in both repeats with DynamoRIO's `prototype stack overflow`, so no process/thread summary existed. The trace adapter passed. |
| SaBRe | PASS | PASS | Exact guest output, one-byte read replacement, stable counter1 total 88, stable counter2 total 179 with 2 processes/2 threads, and semantic write arguments. The trace adapter reports the return value as unknown. |
| LiteInst | PASS | PASS, with documented limitation | Exact guest output, stable counter1 total 79, stable counter2 total 79 with 1 process/1 thread, and semantic write trace. The process-tree counter2 workload timed out after 10 seconds. |
| e9patch | PASS | PASS, single-process path | Exact guest output, observed write arguments/result, exact counter totals, exit lifecycle totals, and semantic write trace. Process creation is outside this direct AOT path. |

## DBT B1.5 conclusion

At commit `b4fd91007ec75f279c06001964c6bb8aaeff269a`, exact counter2 was run with
three DynamoRIO stack sizes:

```text
helper=target/debug/reverie-dbt-dynamorio-path
client=target/debug/reverie-dbt-native/libreverie_dbt_client.so
drrun=$($helper drrun)
for stack in 3M 4M 8M; do
  HERMIT_DBT_COUNTER2_EXACT=1 "$drrun" -quiet -disable_rseq \
    -stack_size "$stack" -c "$client" -- /bin/echo "stack-$stack"
done
```

| DynamoRIO stack size | Exit status |
| --- | --- |
| 3M | 255 |
| 4M | 255 |
| 8M | 255 |

The failure does not go away with more stack, so it is not a stack-size tuning
problem. It is recorded as an upstream DynamoRIO prototype defect and is out of
scope for this work.

No row compared bitwise parity, canonical INFO output, positive INFO counts, or
the full Hermit corpus. B2 and above were not measured or awarded.
