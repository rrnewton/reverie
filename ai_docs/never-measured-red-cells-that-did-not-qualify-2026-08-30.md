# Why 112 never-measured red cells were not activated

Date: 2026-08-30
Task: `characterise-the-never-measured-cells-that-did-not-qualify-for-activation`
Baseline Hermit: `5b811757980b9cab599206c047560a7962176a6b`
Baseline `cells.json` blob: `a0b7ac33e89deecd6bee05c15922dce52d47ea09`
Repeated-run summary SHA-256: `944bf8af55c1102757a068818e840ee7f8e083f9855f9e302b1eddfd363e4cb4`

This document characterises the 112 cells left after 250 of the original 362 red, `never-measured` cells were selected. It changes no activation state. The 20-repetition campaign covered `verify`; it did not run the `naked/native` or `chaos` cells.

A batch result bounds the opportunity; it does not qualify an individual cell. The activation used 20 of 20 clean canonical repetitions with zero retries. The owner has also stated a 10-of-10 bar, so the partial results below report both the first ten repetitions and any later run of ten consecutive clean repetitions without treating those interpretations as equivalent.

## Exact partition

| What happened | Cells | Why they stayed red |
| --- | ---: | --- |
| Passed 20/20 cleanly, but an earlier structured result remains | 32 | A later clean campaign did not erase an earlier determinism, parity, infrastructure, or unavailable result |
| Passed 20/20 cleanly, but the published series also contains a same-Detcore-tree divergence | 1 | Passing and divergent canonical comparisons conflict |
| Passed some but not all repetitions cleanly | 11 | At least one retry, timeout, no-result, crash, or divergence occurred |
| Produced no clean terminal pass | 33 | Current repeated execution produced crash, divergence, infrastructure/no-result, or unsupported-operation evidence |
| `naked/native`, not run by the verify campaign | 33 | Each still needs repeated runs satisfying its declared outcome oracle |
| `chaos`, prior run did not satisfy its declared diversity minimum | 1 | 32/32 statuses passed, but only 6 distinct observations were seen against a minimum of 16 |
| `chaos`, prior canonical verification diverged | 1 | Diverged independently at two retained seeds |
| **Total** | **112** | |

The verify campaign selected all 337 eligible red verify cells, and every selected cell has 20 repetition records, so none of those verify cells was left unattempted. The summary's `unavailable_cells: 0` has a narrower meaning: no selected red cell lacked an executable pressure-test recipe. It does not describe host capabilities. This campaign therefore identifies no unattempted verify cell and makes no separate claim that a host capability was or was not available. The current backend-path problem includes the nine SaBRe cells below whose execution was incomplete or used fallback/native sites.

## The 10-of-10 question

The 33 cells with 20 clean repetitions satisfy either reading of 10-of-10, but remain red because of other evidence. Among the 11 cells with some clean passes:

- zero passed repetitions 1 through 10 cleanly;
- six later produced at least ten consecutive clean repetitions within the 20-run campaign;
- those six had overall clean rates from 16/20 (80%) through 19/20 (95%), and every one also had a retry, timeout, crash, or divergence in the same campaign.

Thus six cells demonstrate a later ten-clean-repetition streak, but none demonstrates an unqualified first-ten 10/10 result in this campaign. Whether to accept a selected streak from a run that also contains failures is an owner decision, not an activation made here.

| Cell | Clean | Terminal passes | Retried repetitions | Longest clean streak | Retained reason |
| --- | ---: | ---: | ---: | ---: | --- |
| `c-programs/remap-file-pages-tmpfile-enosys/verify/sabre` | 19/20 | 20/20 | 1 | 19 | `timeout`; 1 `timeout` |
| `c-programs/record-replay-file-state/verify/ptrace` | 18/20 | 20/20 | 2 | 17 | `determinism-failure`; 1 `crash-error`, 1 `determinism-failure` |
| `backend-parity-c/fd-duplication/verify/dbt` | 19/20 | 20/20 | 1 | 13 | `determinism-failure`; 1 `determinism-failure` |
| `language-runtimes/example-python-random/verify/dbt` | 16/20 | 20/20 | 4 | 12 | `determinism-failure`; 4 `determinism-failure` |
| `c-programs/remap-file-pages-tmpfile-enosys/verify/dbt` | 19/20 | 20/20 | 1 | 10 | `determinism-failure`; 1 `determinism-failure` |
| `c-programs/so-incoming-cpu-tcp4/verify/sabre` | 19/20 | 20/20 | 1 | 10 | `timeout`; 1 `timeout` |
| `determinism-stress/example-race/verify/kvm` | 14/20 | 19/20 | 6 | 7 | `determinism-failure`; 7 `determinism-failure` |
| `determinism-stress-c/mmap-fork-shared/verify/sabre` | 14/20 | 19/20 | 6 | 6 | `determinism-failure`; 7 `determinism-failure` |
| `language-runtimes/example-python-random/verify/sabre` | 8/20 | 13/20 | 12 | 3 | `determinism-failure`; 19 `determinism-failure` |
| `c-programs/dbt-pid-virtualization/verify/ptrace` | 2/20 | 3/20 | 18 | 2 | `infrastructure-error`; 35 `infrastructure-error` |
| `determinism-stress-c/pipe-chain/verify/sabre` | 4/20 | 6/20 | 16 | 2 | `determinism-failure`; 30 `determinism-failure` |

## Passed 20/20 cleanly but retained earlier evidence: 32

These cells met the current repetition bar. They were not selected because the activation deliberately kept existing structured results separate rather than overwriting them with a later pass.

### Existing `determinism-failure`: 22

Nineteen retain the previously measured LiteInst `/proc/self/maps` startup divergence:

- `backend-parity-c/aio-refusal/verify/liteinst`
- `backend-parity-c/event-delivery-ordering/verify/liteinst`
- `backend-parity-c/eventfd-semantics/verify/liteinst`
- `backend-parity-c/fcntl-owner/verify/liteinst`
- `backend-parity-c/file-io-roundtrip/verify/liteinst`
- `backend-parity-c/membarrier-query/verify/liteinst`
- `backend-parity-c/mkdir-rmdir/verify/liteinst`
- `backend-parity-c/o-tmpfile-anon/verify/liteinst`
- `backend-parity-c/pipe-capacity/verify/liteinst`
- `backend-parity-c/record-lock/verify/liteinst`
- `backend-parity-c/sendfile-copy/verify/liteinst`
- `backend-parity-c/set-tid-address/verify/liteinst`
- `backend-parity-c/signal-delivery-sequence/verify/liteinst`
- `backend-parity-c/symlink-ops/verify/liteinst`
- `backend-parity-c/umask-mode/verify/liteinst`
- `c-programs/ioctl-fioclex/verify/liteinst`
- `c-programs/pause-alarm-interrupt/verify/liteinst`
- `system-utils/clock-determinism/verify/liteinst`
- `system-utils/record-getpid/verify/liteinst`

Two retain the 12-RCB initial drift that accumulates to a 672-RCB virtual-time divergence:

- `backend-parity-c/personality-domain/verify/liteinst`
- `backend-parity-c/vectored-io/verify/liteinst`

One retains the related 672-RCB virtual-time divergence:

- `backend-parity-c/cwd-roundtrip/verify/liteinst`

### Existing `infrastructure-error`: 8

These DBT cells had three older matched runs whose retained log names were not accepted by the evidence readers. They passed this campaign 20/20 cleanly, but the earlier structured evidence was not silently reclassified:

- `c-programs/add-key-enosys/verify/dbt`
- `c-programs/cachestat-enosys/verify/dbt`
- `c-programs/futex-waitv-enosys/verify/dbt`
- `c-programs/get-robust-list-self/verify/dbt`
- `c-programs/ioctl-fioclex/verify/dbt`
- `c-programs/kcmp-eperm/verify/dbt`
- `c-programs/keyctl-enosys/verify/dbt`
- `c-programs/listmount-enosys/verify/dbt`

### Existing `parity-failure`: 1

`c-programs/pipe2-errno-precedence/verify/dbt` previously exposed host pipe capacity 65536 instead of Detcore's fixed 8192-byte capacity. It passed 20/20 now, but that exact backend-parity result remains unresolved.

### Existing `unavailable`: 1

`backend-parity-c/host-identity/verify/dbt` previously completed both DBT guest runs without emitting a terminal canonical comparison. It passed 20/20 now, but the earlier no-result evidence remains to be reconciled.

## Passed 20/20 cleanly but has conflicting published evidence: 1

`backend-parity-c/dup-shared-offset/verify/dbt` passed all 20 campaign repetitions, but the published series at the same Detcore tree also contains a canonical divergence at record 80 and syscall 32. Its current measurement is therefore `diverged`, not `measured-and-passed`.

## Passed some repetitions, but not all 20 cleanly: 11

The table in the 10-of-10 section names all 11. Six have a later run of at least ten clean repetitions; five do not. A terminal pass after retry is counted in the terminal-pass column but not in the clean column.

## No clean terminal pass: 33

### Current `crash-error`: 15

Nine SaBRe cells were retried in every repetition and still produced no pass because the SaBRe execution path was incomplete or used fallback/native sites. Their attempt outcomes are shown because one cell also had infrastructure and timeout attempts:

| Cell | Attempt outcomes |
| --- | --- |
| `c-programs/dbt-execveat-unsupported/verify/sabre` | 40 `crash-error` |
| `c-programs/get-robust-list-thread/verify/sabre` | 40 `crash-error` |
| `c-programs/just-spin/verify/sabre` | 40 `crash-error` |
| `c-programs/nanosleep-threads-nocrash/verify/sabre` | 40 `crash-error` |
| `c-programs/record-replay-fd-close/verify/sabre` | 40 `crash-error` |
| `c-programs/sigtimedwait-timeout-0s/verify/sabre` | 40 `crash-error` |
| `determinism-stress-c/thread-stress/verify/sabre` | 40 `crash-error` |
| `determinism-stress/example-race/verify/sabre` | 40 `crash-error` |
| `system-utils/example-devrand/verify/sabre` | 37 `crash-error`, 2 `infrastructure-error`, 1 `timeout` |

Six cells were retried in every repetition and exited with status 125 before a terminal comparison. Each retained 20 `crash-error` attempts and 20 `infrastructure-error` attempts:

| Cell | Attempt outcomes |
| --- | --- |
| `c-programs/nanosleep-threads-simple/verify/ptrace` | 20 `crash-error`, 20 `infrastructure-error` |
| `c-programs/resource-determinism/verify/ptrace` | 20 `crash-error`, 20 `infrastructure-error` |
| `c-programs/socket-timestamp-edge-cases/verify/liteinst` | 20 `crash-error`, 20 `infrastructure-error` |
| `c-programs/socket-timestamp-edge-cases/verify/ptrace` | 20 `crash-error`, 20 `infrastructure-error` |
| `shared-futex-c/qemu-hello/verify/ptrace` | 20 `crash-error`, 20 `infrastructure-error` |
| `util-c/pmu-skid/verify/ptrace` | 20 `crash-error`, 20 `infrastructure-error` |

### Current `determinism-failure`: 8

Each produced at least one trustworthy canonical divergence and no terminal pass. Some retry attempts also crashed or produced no result; those additional outcomes are shown rather than collapsed:

| Cell | Attempt outcomes |
| --- | --- |
| `c-programs/printf-with-threads/verify/sabre` | 36 `crash-error`, 4 `determinism-failure` |
| `c-programs/prodcons-determinism/verify/sabre` | 39 `crash-error`, 1 `determinism-failure` |
| `c-programs/sched-yield-progress/verify/sabre` | 36 `crash-error`, 4 `determinism-failure` |
| `c-programs/sigmask-preemption/verify/sabre` | 37 `crash-error`, 3 `determinism-failure` |
| `c-programs/thread-self-procfs-handoff/verify/sabre` | 38 `crash-error`, 2 `determinism-failure` |
| `c-programs/threadexhaustion/verify/sabre` | 25 `crash-error`, 15 `determinism-failure` |
| `determinism-stress-c/fork-tree/verify/sabre` | 40 `determinism-failure` |
| `system-utils/procfs-sanitized-paths/verify/ptrace` | 8 `crash-error`, 20 `determinism-failure`, 12 `infrastructure-error` |

### Current `infrastructure-error`: 8

Each was retried twice in every repetition, produced no canonical comparison, and therefore supplied no product verdict:

- `applications/example-timed-progress-bar/verify/kvm`
- `applications/example-timed-progress-bar/verify/liteinst`
- `backend-parity-c/signal-waitstatus-identity/verify/dbt`
- `bin-c/robust-futex-test/verify/ptrace`
- `c-programs/dbt-self-sigqueue/verify/dbt`
- `shared-futex-c/qemu-exec-init/verify/ptrace`
- `shared-futex-c/qemu-init/verify/ptrace`
- `shared-futex-c/qemu-net-init/verify/ptrace`

### Earlier structured evidence plus no current pass: 2

- `backend-parity-c/getrusage-self-accounting/verify/liteinst` — retained `determinism-failure`: Exact-head pipe-capacity canonical verification diverged with its first divergence at a /proc/self/maps COMMIT (turn 10), matching three prior LiteInst demotions; durable population evidence measured 20 of 26 LiteInst candidates diverging at that shared startup point and later saw 2 of the 6 apparent PASSx3 survivors fail, so a single matching pair does not qualify this verify population. Current campaign: 0/20 clean, 0/20 terminal passes, 20 retried repetitions, 40 `infrastructure-error`.
- `c-programs/dbt-unsupported-syscall/verify/ptrace` — retained `unavailable`: This fixture intentionally invokes restart_syscall, which strict ptrace execution rejects as Unsupported and fails closed; the cell needs an expected-refusal contract or syscall support before it can be required validation. Current campaign: 0/20 clean, 0/20 terminal passes, 20 retried repetitions, 40 `infrastructure-error`.

## Not run by the verify campaign: 35

### `naked/native`: 33

These native controls remain outside ordinary validation until repeated runs satisfy each declared outcome oracle:

- `data-handling/jq-json-transform/naked/native`
- `data-handling/sqlite-query-determinism/naked/native`
- `determinism-stress-c/producer-consumer/naked/native`
- `determinism-stress/thread-contention/naked/native`
- `language-runtimes/bash-random/naked/native`
- `language-runtimes/cpp-stl-determinism/naked/native`
- `language-runtimes/gawk-random/naked/native`
- `language-runtimes/lua-random/naked/native`
- `language-runtimes/m4-macro-mkstemp/naked/native`
- `language-runtimes/node-v8-jit/naked/native`
- `language-runtimes/perl-hash-order/naked/native`
- `language-runtimes/perl-random/naked/native`
- `language-runtimes/python-hash-determinism/naked/native`
- `language-runtimes/python-hashseed/naked/native`
- `language-runtimes/python-random/naked/native`
- `language-runtimes/ruby-random/naked/native`
- `language-runtimes/rust-hashmap-iteration/naked/native`
- `language-runtimes/tcl-rand-clock/naked/native`
- `system-utils/date-nanoseconds/naked/native`
- `system-utils/mcookie-random/naked/native`
- `system-utils/mktemp-name/naked/native`
- `system-utils/openssl-enc/naked/native`
- `system-utils/openssl-genpkey/naked/native`
- `system-utils/openssl-passwd/naked/native`
- `system-utils/openssl-rand/naked/native`
- `system-utils/openssl-x509/naked/native`
- `system-utils/proc-random-uuid/naked/native`
- `system-utils/proc-uptime/naked/native`
- `system-utils/random-device/naked/native`
- `system-utils/shuf-permutation/naked/native`
- `system-utils/sort-random/naked/native`
- `system-utils/ssh-keygen-ed25519/naked/native`
- `system-utils/uuidgen-random/naked/native`

### `chaos`: 2

- `c-programs/ipc-determinism/chaos/ptrace` — Measured 2026-08-25 by agent(hermit-007). 32 seeds under --chaos --strict gave 32 of 32 passes and 0 failures, but only 6 distinct observation classes against this cell's own assert.min_distinct of 16. The other two cells in this ratchet met their oracles - thread-sync-determinism 14 distinct, thread-contention 32. Reconcile the 16 before enabling - either diversity regressed from the 32 classes the description records, or the threshold does not describe what the harness hashes, which is status plus stdout.
- `determinism-stress/order-violation/chaos/ptrace` — Canonical chaos verification diverged independently at seed 15 turn 9 with 185/186 INFO messages and at seed 9 turn 8 with 173/174 INFO messages; this cell was never reliably passing.

## What remains to be resolved

- The 32 current 20/20 matches need their earlier structured evidence reconciled explicitly; repeating them was not enough to erase a real earlier result.
- The one current 20/20 match with a same-Detcore-tree divergence needs the rare divergence explained or fixed.
- The six partial cells with a later ten-clean streak are the exact set affected by how the owner applies the 10-of-10 bar; all six also failed or retried within the same 20 repetitions.
- The other five partial cells require more than a threshold choice because none produced ten consecutive clean repetitions.
- The 33 zero-pass verify cells separate into SaBRe path completion, pre-comparison exits, canonical divergence, no-result infrastructure, and one intentionally unsupported syscall contract.
- The 33 native controls need their own repeated outcome-oracle run. The two chaos cells need, respectively, the declared diversity mismatch reconciled and the retained canonical divergence fixed.

## Reproducibility

The machine-readable per-cell derivation is stored beside this document as `ai_docs/evidence/never-measured-red-cells-that-did-not-qualify-2026-08-30.json`, SHA-256 `97ab2d5042b000b734439588da69853393f33338f07e85bb609feb58db3618d7`. It records every cell's baseline and retained reason category, current measurement, clean and terminal pass counts, retry count, first-ten result, longest clean streak, and observed attempt mechanisms.

The derivation is the exact baseline set where `status=red` and `measurement=never-measured`, minus the exact required-plan additions between baseline Hermit and the activation commit. For each verify cell, terminal pass and clean-pass totals are checked against `summary.json`; repetition order supplies the first-ten result and longest clean streak. The document's 112 cell labels are checked against that remainder with zero missing, extra, or duplicate identities.

No cell is activated by this characterisation.
