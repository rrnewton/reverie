# Banked sweep findings: one ranked backlog, 2026-08-13

This document indexes findings already established in TaskGraph notes. It does
not re-investigate them, authorize a fix, or claim that an unobserved failure has
occurred. The sources are:

- `sweep-for-swallowed-error-paths` (nine findings);
- `sweep-for-tests-that-cannot-fail` (five findings);
- `sweep-host-selected-values-leaking-into-verification` (ten findings and five
  refuted families); and
- `which-host-selected-leaks-cause-live-divergence-today` (the ten host-selected
  families split into five live and five latent).

The latter two sweeps were recorded against exact Hermit main
`1f4e430c135204591daa0c0cba84f77dae679a78`. Evidence classes and limitations
below are retained from the source notes rather than upgraded here.

## Ranking rule and state

The ranking crosses all three sweeps. False validation authority comes first,
then direct successful results that hide data loss or missing side effects, then
live tests or workloads that read an unestablished value, then latent paths.

`LIVE` means a current consumer, test, or workload reaches the path described by
the source note. It does not mean the triggering failure has been observed or
that the path has caused a retained validation failure. `LATENT` means the
source notes found no current consumer, or the test is filtered from validation.
Qualified states such as `LIVE, masked` preserve a source finding that a value
is read but cannot currently affect the result.

All 24 findings are in `rrnewton/hermit`. This index currently contains no
parent-side `rrnewton/dev-hermit` finding that can use the parent publication
path. In particular, rank 1's `ci/run-nextest-counted.sh` is inside the Hermit
repository; it is not parent `ci-hub/`. A request to take the highest-ranked
parent-side entry therefore has an empty selection and must not silently
reclassify or reorder a Hermit finding.

## Cross-sweep backlog

| Rank | State | Source task | Established defect and location | What the consumer sees | Ownership |
| ---: | --- | --- | --- | --- | --- |
| 1 | LIVE | `sweep-for-tests-that-cannot-fail` | `ci/run-nextest-counted.sh` accepts a nextest summary with zero tests and emits canonical success; its self-test omits the zero case. | A vacuous nextest node is receipt-counted as `ok`; other executed nodes can hide it in the aggregate count. | Already promoted to `nextest-counted-accepts-zero-tests-as-success` (P0); do not duplicate. |
| 2 | LIVE | `sweep-for-swallowed-error-paths` | `hermit-cli/src/replayer/fs.rs` debug-logs and discards captured stdout/stderr emission and flag-restoration errors, then returns `Ok`. | Replay and the guest report the recorded successful write while host output is truncated or absent. | Already promoted to `replay-returns-success-while-discarding-output-emission-errors` (P1); do not duplicate. |
| 3 | LIVE | `sweep-for-tests-that-cannot-fail` | Receipt-counted LiteInst public-dispatch and SaBRe private-RPC-state tests return before their assertions when backend artifacts are unavailable. | Missing implementations or loaders are reported as passed coverage of the implementation or privacy boundary. | Banked. |
| 4 | LIVE | `sweep-for-swallowed-error-paths` | `recorder.rs` converts post-open `/proc/<pid>/fd/<fd>` metadata failure to `materialize=false`; replay treats that boolean as authoritative. | A successful recorded open becomes a placeholder instead of a real file or directory, while replay returns success. | Banked. |
| 5 | LIVE | `sweep-for-swallowed-error-paths` | `recorder.rs` converts fd `read_link` or metadata errors to `ReplayFdKind::None`. | Replay returns the recorded successful read/write without the required eventfd or regular-file kernel side effect. | Banked. |
| 6 | LIVE | `sweep-for-swallowed-error-paths` | Replayer metadata or canonicalization failures become “not in replay root”; regular-file write materialization then succeeds as a no-op. | Replay acknowledges a successful write that it did not apply. | Banked. |
| 7 | LIVE | `sweep-for-swallowed-error-paths` | `record_start.rs --verify-with-gdbex` discards `wait` failure and nonzero batch-GDB status despite `--return-child-result`. | Successful replay is presented as successful GDB verification even when the requested GDB command failed. | Banked. |
| 8 | LIVE | `sweep-for-swallowed-error-paths` | Recording-deadline setup turns signal-mask query or SIGALRM-unblock failure into an armed `Ok` result. | The advertised timeout appears installed but can remain blocked, allowing a hung recording to continue. | Banked. |
| 9 | LIVE | `sweep-for-swallowed-error-paths` | fdinfo read, parse, or classification failures serialize `replay_kernel_side_effect=false` for `epoll_wait`. | Replay skips consuming real epoll state, writes recorded events, and returns the recorded successful count. | Banked. |
| 10 | LIVE | `sweep-for-swallowed-error-paths` | Supported absolute child-exec manifest, staging, and dependency-copy failures are collapsed into absence or warnings. | Recording or replay preparation succeeds despite omitting an executable that the recorded guest executed. | Banked. |
| 11 | LIVE | `sweep-for-swallowed-error-paths` | Post-exec inherited blocking-descriptor rediscovery errors only warn and have no result channel. | Exec restoration reads as complete while descriptors can remain unregistered or physically nonblocking, later producing wrong I/O or scheduling behavior. | Banked. |
| 12 | LIVE | `sweep-for-tests-that-cannot-fail` | Receipt-counted architecture, cpufreq, hash-seed, key-user, THP, and procfs-hwmon tests return normally when prerequisites are absent. | Capability absence is counted as an executed pass; the measured box had no hwmon input files, so that test performed no Hermit run. | Banked. |
| 13 | LIVE | `sweep-for-tests-that-cannot-fail` | The native nondeterminism comparator accepts changing status/stdout/stderr without requiring successful guest executions. | Changing `/bin/date` failures can establish “native timestamp nondeterminism”; the noop and strict legs remain discriminating. | Banked. |
| 14 | LIVE | `sweep-host-selected-values-leaking-into-verification` | CPU count is read through `sysconf`, `/sys/devices/system/cpu/online`, or `/proc/cpuinfo`; the tracked host-identity fixture prints 316 while virtual affinity reports 1. | Current raw-compared output exposes an internally inconsistent host CPU count and virtual CPU mask. No same-SHA failure is attributed to it. | Banked. |
| 15 | LIVE | `sweep-host-selected-values-leaking-into-verification` | `statfs`/`fstatfs` and stat-family canonicalization retain filesystem geometry; current `df` workloads print values derived from it. | Strict output contains host or mount-selected block geometry. Readable retained `compat.df` runs passed, so no current divergence is attributed. | Banked. |
| 16 | LIVE | `sweep-host-selected-values-leaking-into-verification` | Startup auxiliary-vector HWCAP/HWCAP2/MINSIGSTKSZ values remain host-selected; the current startup fixture prints them and branches on HWCAP. | Cross-machine capability and loader-choice input appears in raw-compared output; the fixture contract currently permits the difference. | Banked. |
| 17 | LIVE, outside default CI | `sweep-host-selected-values-leaking-into-verification` | The host ephemeral-port range feeds Hermit's allocator; current non-CI UNIX and Netlink autobind fixtures print assigned identities. | Strict output can change with the host range, although current default CI does not run these readers. | Banked. |
| 18 | LIVE, masked | `sweep-host-selected-values-leaking-into-verification` | `getrusage(RUSAGE_SELF/RUSAGE_THREAD).ru_maxrss` reads live tracer-side VmHWM/VmRSS. Current readers omit, zero, or only positivity-check the numeric value. | The host-selected value is read, but its variation cannot change current cell output or status. It cannot explain current parity verdicts. | Banked. |
| 19 | LATENT | `sweep-for-tests-that-cannot-fail` | `record_replay_stable_arbitrary_binaries` has no nonempty assertion, so an empty `RECORD_REPLAY_TOOLS` loop executes no assertions; the active DAG also filters the test by name. | If selected empty it would pass without testing a binary; today it supplies no validation evidence at all. | Banked. |
| 20 | LATENT | `sweep-host-selected-values-leaking-into-verification` | Unspecialized `getsockopt` passes through host SO_RCVBUF/SO_SNDBUF defaults. Current socket fixtures explicitly exclude them and traced strict tools did not read them. | A future reader could branch or print host buffer defaults; no current workload was established. | Banked. |
| 21 | LATENT | `sweep-host-selected-values-leaking-into-verification` | `/sys/fs/cgroup` controls such as `memory.max`, `cpu.max`, effective cpusets, and `pids.max` remain real container content. | A future workload could size memory, concurrency, or process creation from host/container limits; current readers were not established. | Banked. |
| 22 | LATENT | `sweep-host-selected-values-leaking-into-verification` | Unrecognized `/proc/sys` inotify, map, fd, listen, pid, thread, and pipe limits expose host values. | A future workload can branch on host thresholds; current tests operate below them and do not read the ranked files. | Banked. |
| 23 | LATENT | `sweep-host-selected-values-leaking-into-verification` | Generic ioctl pass-through includes terminal, network, pty identity, and block-device geometry requests. | A future suitable fd exposes host geometry or allocation identity; current fixtures cover guest state or deterministic refusal instead. | Banked. |
| 24 | LATENT | `sweep-host-selected-values-leaking-into-verification` | `/proc/self/limits` remains host launch content even though `getrlimit`/`prlimit` use virtual limits. | A future procfs reader can observe an answer contradicting the virtual syscall answer; no current reader was found. | Banked. |

The table contains exactly 24 findings: nine swallowed-error paths, five tests
that cannot fail or cannot currently run, and ten host-selected families. The
two promoted findings remain visible in rank order but point at their existing
tasks rather than creating duplicate work.

## Negative controls retained

### Swallowed-error sweep

The sweep did **not** report ignored `remove_file`, kill, signal-handler restore,
or RPC-shutdown results where the primary failure still propagates. It cleared
e9patch and instruction-map cache read failures that become cache misses with a
rebuild or fallback; explicit relative and dirfd-relative exec exclusions that
are documented best-effort guards; documented virtual-descriptor ENOENT/EROFS
fallbacks; the conservative documented `/proc/modules` empty fallback; missing
analysis stacks that print “not available”; and interactive GDB status semantics.
Only the explicit batch-verification GDB path is listed. The sweep did not treat
every discarded `Result` as a defect.

### Tests-that-cannot-fail sweep

The sweep cleared validation stop-path tests that check exact receipt fields and
canonical accept/refuse legs; strict-green authority tests that refuse zero
cells, empty or wrong artifacts, missing evidence, and artifact reuse; Reverie
pin tests that reject backward and off-history pins; the pselect6 fixture that
requires exact EINTR plus its success and retry markers; and random-determinism
checks requiring nonempty output, same-seed equality, and different-seed
inequality. It did not report a fingerprint self-comparison because the same
test independently checks exact material and field sensitivity. KVM early
returns were excluded from the receipt-pass finding because those names are
filtered or undeclared. The dedicated SaBRe examples lane was cleared because a
configured missing artifact panics rather than passes.

### Host-selected-value sweep

Five of fifteen inspected families were explicitly refuted:

1. `getrlimit`/`prlimit` return virtual process-local `ResourceLimits`, and
   mutations remain virtual.
2. `sysinfo` and `sched_getaffinity` canonicalize or virtualize total RAM,
   uptime, process count, and the CPU mask. This does not clear the separate
   `sysconf`/procfs CPU-count finding.
3. `SIOCETHTOOL` returns deterministic `ENODEV` rather than host link state.
4. Recognized procfs AIO counts do not expose the suspected host value, and AIO
   syscalls are refused.
5. `F_GETFL`, `F_GETFD`, `F_GET_SEALS`, and `FIONREAD` on guest-created objects
   report Hermit-tracked or guest-created state rather than host configuration.

The follow-up established five live readers and five latent families, but
confirmed current-divergence causes from these ten are `0/10`. Retained leaf
logs did not name a leak-reading cell; `ru_maxrss`, the same-host-varying
candidate, is masked by current tests. The broad claim that these findings
explain the mixed pass/fail receipt history was therefore refuted wherever
retained evidence permits a conclusion; absent historical leaf logs remain
unclassifiable, not cleared.

## Use of this index

This is one visible ordering for later assignment when validation capacity and
the existing PR queue permit publication. A future owner must still claim one
finding, verify it against then-current code, obtain an explicit write
destination, and satisfy the normal review and validation rules. This index is
not authority to publish, batch unrelated fixes, or treat a latent path as an
observed failure.
