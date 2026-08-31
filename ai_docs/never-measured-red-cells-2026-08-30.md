# Never-measured red compatibility cells

Date: 2026-08-30
Task: `list-the-never-measured-red-cells-by-test-id-mode-backend`
Hermit source: `5b811757980b9cab599206c047560a7962176a6b`
Tracked cells blob: `a0b7ac33e89deecd6bee05c15922dce52d47ea09`

This is the exact committed population whose scorecard status is `red` and whose measurement is `never-measured`. It contains 362 cells across 186 test IDs. The list is nested by test ID, active mode, then backend as requested.

`never-measured` means no qualifying observation has been imported into `ci/compat-envelope/cells.json`; it does not prove that nobody ever ran the cell. In particular, a retained comparison without the required current evidence is historical, not a measured match. A previous projection collapsed those states and made all 304 selected cells report `never-measured` even though each held a retained canonical comparison.

## Counts

| Dimension | Count |
| --- | ---: |
| Cells | 362 |
| Distinct test IDs | 186 |
| `verify` | 327 |
| `naked` | 33 |
| `chaos` | 2 |
| `ptrace` | 119 |
| `sabre` | 82 |
| `dbt` | 60 |
| `liteinst` | 48 |
| `native` | 33 |
| `kvm` | 20 |
| Portable lane | 361 |
| Privileged lane | 1 |

The totals mechanically reconcile to the scorecard: 372 red cells = 362 `never-measured` + 2 `measured-and-passed` + 8 `diverged`.

## Existing red reasons

These are separate populations even though all 362 currently have measurement `never-measured`:

| Existing reason class | Cells |
| --- | ---: |
| Generic repeated-qualification reason | 284 |
| Legacy evidence: ptrace passed while an enabled sibling backend failed | 35 |
| Structured `determinism-failure` | 23 |
| Structured `infrastructure-error` | 8 |
| Explicitly not measured | 3 |
| Measured but not qualified | 3 |
| Structured `unavailable` | 2 |
| Chaos qualification known insufficient or divergent | 2 |
| Structured `parity-failure` | 1 |
| Expected-plan drift with no reason | 1 |
| **Total** | **362** |

The reason text is not a substitute for a current run. It is retained here to prevent a clean pass in one backend from sweeping in a sibling backend with a different known result.

## Qualification and activation rule

A batch result bounds the opportunity; it does not qualify any individual
cell. One passing observation is not enough to turn a cell on.

A cell qualifies only after 20 of 20 clean canonical repetitions at one clean source commit. Any retry, infrastructure error, no-result, timeout, crash, or divergence keeps it red. Twenty repetitions are the project's existing L4 count; this threshold does not prove a rare 2-in-124 failure is absent. Selection turns a passing cell into a continuing ordinary-validation requirement.

Only the exact qualified test, mode, and backend cell is selected. A manifest with a uniform `ci: false` is converted to a complete per-backend map when sibling backends differ. Manifest-disabled cells remain not applicable. The 33 `naked/native` controls remain separate because current manifest policy keeps naked mode outside ordinary validation. The two chaos rows retain their existing qualification requirements.

## Qualification and activation result

The official repeated-run campaign executed all 6,740 planned runs: 337 red
`verify` cells times 20 repetitions. It ran at Hermit
`5b811757980b9cab599206c047560a7962176a6b`; the retained summary is
`hermit/ignored/compat-envelope/cell-activation-verify-20x-run/summary.json`,
SHA-256
`944bf8af55c1102757a068818e840ee7f8e083f9855f9e302b1eddfd363e4cb4`.

Of the 327 scorecard rows labelled never-measured in `verify`, 283 passed all 20
repetitions cleanly with no retry. Thirty-two of those nevertheless retain an
older structured red reason: 22 `determinism-failure`, 8
`infrastructure-error`, 1 `parity-failure`, and 1 `unavailable`. A later clean
sample does not silently erase those distinct findings. The published series
also contains a same-Detcore-tree divergence for
`backend-parity-c/dup-shared-offset/verify/dbt` at record 80 and syscall 32, so
that cell remains red as well. The other 250 cells are now selected. The
required plan grows from 307 to 557 cells, exactly those 250 additions. By
backend:

| Backend | Selected after 20/20 clean repetitions |
| --- | ---: |
| `ptrace` | 104 |
| `sabre` | 61 |
| `dbt` | 44 |
| `liteinst` | 23 |
| `kvm` | 18 |
| **Total** | **250** |

The remaining 112 cells from the original census stay red for separate reasons:

| Population | Cells | Disposition |
| --- | ---: | --- |
| `verify`, did not pass 20/20 cleanly | 42 | Kept red with the repeated-run result recorded per backend |
| `verify`, already had a structured failure or unavailable result | 34 | Kept red; 32 later passed 20/20 and 2 did not, but the existing reason is not collapsed into never-measured qualification |
| `verify`, conflicting published comparison at the same Detcore tree | 1 | Kept red with the retained divergence position |
| `naked/native` | 33 | Not part of ordinary validation under current manifest policy |
| `chaos` | 2 | Existing chaos qualification requirements retained |
| **Total** | **112** | |

The repeated run's 44 non-clean never-measured `verify` results divide into 16
`determinism-failure`, 15 `crash-error`, 11 `infrastructure-error`, and 2
`timeout` results. Two belong to the already-separated structured-reason
population, leaving 42 held by this qualification pass. A retry is a hold even
when the retry passed. The pressure summary contains 6,546 usable
comparison rows across 320 cells and 369 rows that supplied no usable
comparison; those no-result rows are not measured matches.

One separately `measured-and-passed` red cell also passed 20/20 cleanly, but it
is outside this never-measured promotion. One cell with a retained historical
divergence passed 20/20 cleanly in this campaign and remains red: the later run
does not erase the earlier divergence.

The campaign occupies 621 compact series rows representing all 6,217
comparable runs: 334 pass rows representing 5,911 passes and 287 divergence
rows representing 306 determinism failures. Parent commit
`90a55d8190dd2551179a9394be3504ee89a6a5a9` published the first 283 rows,
representing 5,660 clean runs. Parent commit
`4feaa165d519d28d763fe0869637cef8e733be9c` added the remaining 338 rows,
representing 557 runs, and completed the campaign history. Projecting the
latter commit into the scorecard records every one of the 250 selected cells as
`measured-and-passed`, and records all 23 cells with a comparison failure as
`diverged`, without embedding the raw 113 MB repeated-run payload. The
generated scorecard contains 554 green, 122 red, and 5,068 not-applicable
cells. Colour and measurement remain separate: green means selected, while the
measurement label describes comparison evidence imported from the published
series.

## Exact census

- `applications/example-timed-progress-bar`
  - `verify`: `dbt`, `kvm`, `liteinst`
- `applications/kvm-shell-environment`
  - `verify`: `kvm`
- `backend-parity-c/aio-refusal`
  - `verify`: `liteinst`
- `backend-parity-c/cpu-virtualization`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/cwd-roundtrip`
  - `verify`: `liteinst`
- `backend-parity-c/dup-shared-offset`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/event-delivery-ordering`
  - `verify`: `liteinst`
- `backend-parity-c/eventfd-semantics`
  - `verify`: `liteinst`
- `backend-parity-c/fcntl-owner`
  - `verify`: `liteinst`
- `backend-parity-c/fd-duplication`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/file-io-roundtrip`
  - `verify`: `liteinst`
- `backend-parity-c/getcpu-identity`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/getpriority-identity`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/getrusage-self-accounting`
  - `verify`: `liteinst`
- `backend-parity-c/hardware-trap-identity`
  - `verify`: `ptrace`
- `backend-parity-c/host-identity`
  - `verify`: `dbt`
- `backend-parity-c/lseek-positioning`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/membarrier-query`
  - `verify`: `liteinst`
- `backend-parity-c/mkdir-rmdir`
  - `verify`: `liteinst`
- `backend-parity-c/numa-node-identity`
  - `verify`: `dbt`, `ptrace`
- `backend-parity-c/o-tmpfile-anon`
  - `verify`: `liteinst`
- `backend-parity-c/personality-domain`
  - `verify`: `liteinst`
- `backend-parity-c/pid-probe`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `backend-parity-c/pidfd-open-self`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/pipe-capacity`
  - `verify`: `liteinst`
- `backend-parity-c/prctl-identity`
  - `verify`: `dbt`, `ptrace`
- `backend-parity-c/pthread-lifecycle`
  - `verify`: `ptrace`
- `backend-parity-c/record-lock`
  - `verify`: `liteinst`
- `backend-parity-c/rlimit-identity`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/sched-getaffinity-identity`
  - `verify`: `dbt`, `kvm`, `ptrace`
- `backend-parity-c/sendfile-copy`
  - `verify`: `liteinst`
- `backend-parity-c/set-tid-address`
  - `verify`: `liteinst`
- `backend-parity-c/signal-delivery-sequence`
  - `verify`: `liteinst`
- `backend-parity-c/signal-waitstatus-identity`
  - `verify`: `dbt`, `ptrace`
- `backend-parity-c/socket-epoll-ordering`
  - `verify`: `ptrace`
- `backend-parity-c/symlink-ops`
  - `verify`: `liteinst`
- `backend-parity-c/umask-mode`
  - `verify`: `liteinst`
- `backend-parity-c/vectored-io`
  - `verify`: `liteinst`
- `bin-c/posix-timer-test`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `bin-c/robust-futex-test`
  - `verify`: `ptrace`
- `c-programs/acct-refusal-probe`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/add-key-enosys`
  - `verify`: `dbt`
- `c-programs/adjtimex-deterministic`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/bpf-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/cachestat-enosys`
  - `verify`: `dbt`
- `c-programs/clock-adjtime-deterministic`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/copy-file-range-refusal-probe`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/dbt-copied-tiocgpgrp`
  - `verify`: `ptrace`, `sabre`
- `c-programs/dbt-execveat-unsupported`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/dbt-pid-virtualization`
  - `verify`: `ptrace`
- `c-programs/dbt-prlimit-self`
  - `verify`: `ptrace`, `sabre`
- `c-programs/dbt-self-sigqueue`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/dbt-unsupported-syscall`
  - `verify`: `ptrace`
- `c-programs/dbt-wait-lifecycle`
  - `verify`: `ptrace`, `sabre`
- `c-programs/epoll-determinism`
  - `verify`: `kvm`, `liteinst`, `ptrace`, `sabre`
- `c-programs/futex-requeue-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/futex-waitv-enosys`
  - `verify`: `dbt`
- `c-programs/futex-wake-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/get-robust-list-child`
  - `verify`: `ptrace`, `sabre`
- `c-programs/get-robust-list-self`
  - `verify`: `dbt`
- `c-programs/get-robust-list-thread`
  - `verify`: `ptrace`, `sabre`
- `c-programs/ioctl-fioclex`
  - `verify`: `dbt`, `liteinst`
- `c-programs/ipc-determinism`
  - `chaos`: `ptrace`
- `c-programs/just-spin`
  - `verify`: `ptrace`, `sabre`
- `c-programs/kcmp-eperm`
  - `verify`: `dbt`
- `c-programs/keyctl-enosys`
  - `verify`: `dbt`
- `c-programs/listmount-enosys`
  - `verify`: `dbt`
- `c-programs/lsm-get-self-attr-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/lsm-list-modules-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/lsm-set-self-attr-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/madvise-determinism`
  - `verify`: `kvm`, `ptrace`, `sabre`
- `c-programs/map-shadow-stack-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/memfd-secret-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/mmap-determinism`
  - `verify`: `kvm`, `liteinst`, `ptrace`, `sabre`
- `c-programs/name-to-handle-at-eopnotsupp`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/name-to-handle-directory-eopnotsupp`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/name-to-handle-empty-path-eopnotsupp`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/nanosleep-threads-nocrash`
  - `verify`: `ptrace`, `sabre`
- `c-programs/nanosleep-threads-simple`
  - `verify`: `ptrace`
- `c-programs/netlink-autobind-generic`
  - `verify`: `kvm`, `liteinst`, `ptrace`, `sabre`
- `c-programs/netlink-autobind-route`
  - `verify`: `kvm`, `liteinst`, `ptrace`, `sabre`
- `c-programs/netlink-autobind-usersock`
  - `verify`: `kvm`, `liteinst`, `ptrace`, `sabre`
- `c-programs/pause-alarm-interrupt`
  - `verify`: `liteinst`
- `c-programs/perf-event-hardware-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/perf-event-open-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/perf-event-software-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/perf-event-watchpoint-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/pidfd-waitid-child`
  - `verify`: `ptrace`, `sabre`
- `c-programs/pipe2-errno-precedence`
  - `verify`: `dbt`
- `c-programs/ppoll-simulation`
  - `verify`: `ptrace`
- `c-programs/printf-with-threads`
  - `verify`: `ptrace`, `sabre`
- `c-programs/proc-fd-link-aliases`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/process-mrelease-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/process-vm-readv-refusal-probe`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/process-vm-writev-refusal-probe`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/procfs-identity-agreement`
  - `verify`: `ptrace`, `sabre`
- `c-programs/prodcons-determinism`
  - `verify`: `ptrace`, `sabre`
- `c-programs/ptrace-attach-eperm`
  - `verify`: `ptrace`, `sabre`
- `c-programs/ptrace-eperm`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/ptrace-seize-eperm`
  - `verify`: `ptrace`, `sabre`
- `c-programs/ptrace-traceme-eperm`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/record-replay-fd-close`
  - `verify`: `ptrace`, `sabre`
- `c-programs/record-replay-file-state`
  - `verify`: `ptrace`
- `c-programs/remap-file-pages-anonymous-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/remap-file-pages-memfd-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/remap-file-pages-tmpfile-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/request-key-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/resource-determinism`
  - `verify`: `ptrace`
- `c-programs/sched-yield-progress`
  - `verify`: `ptrace`, `sabre`
- `c-programs/sigmask-preemption`
  - `verify`: `ptrace`, `sabre`
- `c-programs/sigtimedwait-timeout-0s`
  - `verify`: `ptrace`, `sabre`
- `c-programs/so-incoming-cpu-tcp4`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/so-incoming-cpu-tcp6`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/so-incoming-cpu-udp4`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/socket-cookie-tcp`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/socket-cookie-udp`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/socket-cookie-unix`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/socket-ioctl-timestamp`
  - `verify`: `liteinst`, `ptrace`
- `c-programs/socket-timestamp-edge-cases`
  - `verify`: `liteinst`, `ptrace`
- `c-programs/socket-timestamp-timespec`
  - `verify`: `liteinst`, `ptrace`
- `c-programs/socket-timestamp-timeval`
  - `verify`: `liteinst`, `ptrace`
- `c-programs/splice-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/statmount-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/sysfs-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/syslog-deterministic`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/sysv-sem-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/sysv-shm-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/tee-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/thread-self-procfs-handoff`
  - `verify`: `ptrace`, `sabre`
- `c-programs/thread-sync-determinism`
  - `verify`: `ptrace`
- `c-programs/threadexhaustion`
  - `verify`: `ptrace`, `sabre`
- `c-programs/unix-autobind-dgram`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/unix-autobind-seqpacket`
  - `verify`: `liteinst`, `ptrace`, `sabre`
- `c-programs/ustat-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `c-programs/vmsplice-enosys`
  - `verify`: `dbt`, `ptrace`, `sabre`
- `chaos-c/lock-granularity`
  - `verify`: `ptrace`
- `data-handling/jq-json-transform`
  - `naked`: `native`
- `data-handling/sqlite-query-determinism`
  - `naked`: `native`
- `determinism-stress-c/fork-tree`
  - `verify`: `ptrace`, `sabre`
- `determinism-stress-c/mmap-fork-shared`
  - `verify`: `ptrace`, `sabre`
- `determinism-stress-c/pipe-chain`
  - `verify`: `ptrace`, `sabre`
- `determinism-stress-c/producer-consumer`
  - `naked`: `native`
- `determinism-stress-c/thread-stress`
  - `verify`: `ptrace`, `sabre`
- `determinism-stress/example-race`
  - `verify`: `kvm`, `ptrace`, `sabre`
- `determinism-stress/order-violation`
  - `chaos`: `ptrace`
- `determinism-stress/thread-contention`
  - `naked`: `native`
- `language-runtimes/bash-random`
  - `naked`: `native`
- `language-runtimes/cpp-stl-determinism`
  - `naked`: `native`
- `language-runtimes/example-python-random`
  - `verify`: `dbt`, `kvm`, `liteinst`, `ptrace`, `sabre`
- `language-runtimes/gawk-random`
  - `naked`: `native`
- `language-runtimes/lua-random`
  - `naked`: `native`
- `language-runtimes/m4-macro-mkstemp`
  - `naked`: `native`
- `language-runtimes/node-v8-jit`
  - `naked`: `native`
- `language-runtimes/perl-hash-order`
  - `naked`: `native`
- `language-runtimes/perl-random`
  - `naked`: `native`
- `language-runtimes/python-hash-determinism`
  - `naked`: `native`
- `language-runtimes/python-hashseed`
  - `naked`: `native`
- `language-runtimes/python-random`
  - `naked`: `native`
- `language-runtimes/ruby-random`
  - `naked`: `native`
- `language-runtimes/rust-hashmap-iteration`
  - `naked`: `native`
- `language-runtimes/tcl-rand-clock`
  - `naked`: `native`
- `shared-futex-c/qemu-exec-init`
  - `verify`: `ptrace`
- `shared-futex-c/qemu-hello`
  - `verify`: `ptrace`
- `shared-futex-c/qemu-init`
  - `verify`: `ptrace`
- `shared-futex-c/qemu-net-init`
  - `verify`: `ptrace`
- `system-utils/clock-determinism`
  - `verify`: `liteinst`
- `system-utils/date-nanoseconds`
  - `naked`: `native`
- `system-utils/example-devrand`
  - `verify`: `kvm`, `ptrace`, `sabre`
- `system-utils/mcookie-random`
  - `naked`: `native`
- `system-utils/mktemp-name`
  - `naked`: `native`
- `system-utils/openssl-enc`
  - `naked`: `native`
- `system-utils/openssl-genpkey`
  - `naked`: `native`
- `system-utils/openssl-passwd`
  - `naked`: `native`
- `system-utils/openssl-rand`
  - `naked`: `native`
- `system-utils/openssl-x509`
  - `naked`: `native`
- `system-utils/proc-random-uuid`
  - `naked`: `native`
- `system-utils/proc-uptime`
  - `naked`: `native`
- `system-utils/procfs-sanitized-paths`
  - `verify`: `ptrace`
- `system-utils/random-device`
  - `naked`: `native`
- `system-utils/record-getpid`
  - `verify`: `liteinst`
- `system-utils/shuf-permutation`
  - `naked`: `native`
- `system-utils/sort-random`
  - `naked`: `native`
- `system-utils/ssh-keygen-ed25519`
  - `naked`: `native`
- `system-utils/startup-tls-guards`
  - `verify`: `ptrace`
- `system-utils/uuidgen-random`
  - `naked`: `native`
- `util-c/pmu-skid`
  - `verify`: `ptrace`
