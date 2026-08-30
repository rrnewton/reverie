# Reverie backend maturity results — 2026-08-29

These are measured results, not intended status. They use the local B0/B1/B1.5
contract implemented by `scripts/validate-backend-maturity.sh`; they do not
measure Hermit's canonical L2 predicate, INFO-message parity, or the full
Hermit corpus.

## Clean-checkout measurement

The full six-backend matrix ran from clean Reverie commit
`b5be940cdbcd5da0892a57b364f6b9f1cf50129e` with:

```text
CARGO_BUILD_JOBS=1 \
CARGO_TARGET_DIR=/tmp/degraded-lander-2-pr476-target \
BACKEND_MATURITY_REPORT=/tmp/degraded-lander-2-pr476-fresh.tsv \
./scripts/validate-backend-maturity.sh
```

The report recorded `tree_dirty=0`, `partial_selection=0`,
`skipped_runtime_preparation=0`, and Rust
`1.99.0-nightly (26ae60a9e 2026-07-28)`. The command exited 0 because every
backend reached its configured minimum: B1.5 for ptrace, KVM, SaBRe,
LiteInst, and e9patch; B1 for DBT.

| Backend | B0 | B1 | B1.5 | Maximum established |
| --- | --- | --- | --- | --- |
| ptrace | PASS | PASS | PASS | B1.5 |
| KVM | PASS | PASS | PASS | B1.5 |
| DBT | PASS | PASS | FAIL | B1 |
| SaBRe | PASS | PASS | PASS | B1.5 |
| LiteInst | PASS | PASS | PASS with documented process-tree limitation | B1.5 |
| e9patch | PASS | PASS | PASS with documented single-process limitation | B1.5 |

All B0 rows used an independent fresh release target and recorded the exact
repository SHA and Rust version.

## Runtime evidence

- ptrace B1 observed a real guest reconstructing its input after Chaos limited
  reads to one byte. B1.5 repeated exact counter and trace tools twice:
  counter1 was 40, and counter2 was 177 syscalls from 2 processes/2 threads.
- KVM B1 opened `/dev/kvm`, ran a real guest ELF, and observed read replacement
  without ptrace fallback. B1.5 repeated exact counter and trace tools twice:
  counter1 was 40, and counter2 was 40 syscalls from 1 process/1 thread. The
  process-tree test established process/thread totals; it does not compare the
  process-tree syscall total and does not claim canonical INFO comparison.
- DBT B1 used the native DynamoRIO client built by
  `reverie-dbt/scripts/build-client.sh`. A real guest reconstructed its input
  after at least ten one-byte reads. B1.5 remained red on both repetitions:
  counter1 emitted no exact summary, while counter2 exited 255 with
  `DynamoRIO: prototype stack overflow` and therefore emitted no required
  1-process/1-thread summary. The semantic syscall trace was still exercised,
  but it cannot compensate for the missing exact counters.
- SaBRe B1 observed one-byte read replacement through the selected tool, with
  no ptrace fallback. B1.5 repeated exact counters and semantic trace twice:
  counter1 was 88, and counter2 was 184 syscalls from 2 processes/2 threads.
  The trace adapter reports the return value as unknown.
- LiteInst B1 ran the exact Chaos integration test. B1.5 repeated exact
  counters and semantic trace twice: counter1 and counter2 were both 79 with
  1 process/1 thread. The process-tree counter2 workload timed out after 10
  seconds, so process creation remains outside the established B1.5 evidence.
- e9patch B1 ran a real root guest ELF through the direct strace Tool and
  observed the injected write action. B1.5 repeated exact counters, exit
  lifecycle totals, and semantic trace twice. Shared-library sites and process
  creation remain outside this direct AOT path.

## Fail-closed interpretation

The validator does not convert omitted or unavailable evidence into a pass.
A dirty source tree, skipped release build, skipped runtime preparation,
partial backend selection, or recognized host execution-policy denial records
`unmeasurable`; an unmet configured minimum exits 2 unless a real test failure
requires exit 1. `validate.sh` labels exit 2 as `UNMEASURABLE` for the operator
but still counts the check as failed.

B2 and above were not measured. This report does not establish canonical
Hermit backend parity or full-corpus completion.
