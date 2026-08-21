# Full-profile findings from 2026-08-20

## Scope and result

These findings describe the full-profile run of `rrnewton/hermit` commit
`bec7551586ae449658b0320ba1374186dda82bb5` on `devbig030`. They are not a
claim about a later Hermit commit.

The 60-node full plan exited 1 after 4m00s wall time and 1h06m12s aggregate
CPU time. It executed 34 nodes: 31 passed, `e2e.manifest_c_programs` failed,
two were aborted, two were dependency-skipped, and 24 produced no outcome.
The failing node ran 138 cells in 117.97s: 126 passed, three failed, and nine
ended with `verdict=no_result`. No receipt was published.

The run's durable host-local evidence is:

- `hermit/ignored/validate/validate-full-bec7551586ae-20260821T025233Z.log`
- `hermit/ignored/validate/e2e/validate-full-bec7551586ae-20260821T025233Z/`

The run and its node-by-node result were recorded on `rrnewton/dev-hermit`
main by commit `fa2f49bac6690e3f287545c5dac6d91d326e0b3b`.

## What made main red

The non-pass population is not one failure repeated. Three mechanisms were
established, and one additional ptrace error remains unlocalized.

### DBT produces a weaker result but not the required verdict

Eight DBT cells completed both executions successfully but ended with
`verdict=no_result` and `comparison=null`:

- `add-key-enosys`
- `cachestat-enosys`
- `futex-waitv-enosys`
- `get-robust-list-self`
- `ioctl-fioclex`
- `kcmp-eperm`
- `keyctl-enosys`
- `listmount-enosys`

The cause is structural at the tested Hermit commit. The top-level command
writes a pending `no_result` before backend dispatch. The DBT arm returns
directly through `run_dbt`, bypassing the ordinary `verify()` path and its
`compare_two_runs` / `write_verification_json` result channel. `run_dbt`
performs its own comparison of allowed exit status, stdout, native counters,
and memory hash, then prints `Determinism verified`. It has no `verify_json`
channel and never replaces the pending verdict. The consumer is therefore not
losing a completed canonical comparison: DBT never produces one.

This is a producer/consumer contract mismatch, not evidence that DBT cannot
ever produce the required evidence. It is also not permission to accept the
weaker comparison as equivalent. The unlanded PR #2241 would provide a typed
guest-level verdict while declaring `compare_logs=false`,
`guest_stderr_compared=false`, `internal_log_messages_compared=false`, and
`bitwise_parity=false`. Treating that result as the current canonical INFO
comparison would satisfy the manifest by weakening the evidence contract.

The ninth `no_result` row, `dbt-unsupported-syscall` under ptrace, is separate.
Its sandbox exited status 1 after 64ms with empty stdout and no comparison or
guest-exit record. The retained artifact does not establish whether guest code
started, and it does not have the earlier `CapEff=0` mount-refusal signature.
Its cause remains unresolved.

### LiteInst charges bootstrap map processing to committed virtual time

Two LiteInst verify cells completed both guest runs with exit 0 but diverged:

- `ioctl-fioclex`: 953 versus 953 INFO records; first difference at turn 28.
- `pause-alarm-interrupt`: 937 versus 937 INFO records; first difference at
  turn 10.

Both first differ on a COMMIT associated with reading `/proc/self/maps`: the
turn, thread, and resource agree, while committed virtual time differs. The
first delta is 120ns, corresponding to 12 retired conditional branches. The
magnitude reproduced across observations while its sign varied.

The pinned Reverie runtime initializes and enables the retired-conditional-
branch counter before `initialize_reverie_tool` reaches LiteInst's
`prepare_instrumentation`. That bootstrap repeatedly reads and parses raw
`/proc/self/maps`. For each executable mapping it takes before and after map
snapshots and calls `TrampolineArena::allocate_near`; LiteInst's
`near_candidates` separately rereads the file, parses address ranges, branches
over gaps and candidates, sorts and deduplicates candidates, and probes
mappings. This work occurs outside the callback handler-depth deduction, so
its branches flow into the next scheduler clock sample and committed virtual
time.

The retained artifacts rule out the simple size/count/order explanation for
these pairs. Each run performed 24 map opens. Corresponding scans had the same
10 read calls and byte totals, and the complete DETLOG syscall streams,
including mmap arguments and results, were identical after removal of the
wall-clock prefix. Different map count, byte length, or syscall-level
allocation order is not supported.

The originating field remains unresolved. `/proc/self/maps` payload bytes are
neither normalized nor retained, while parser and candidate branches depend on
addresses, ranges, permissions, offsets, device/inode fields, pathname text,
and ordering. Different raw content or address layout can make either run
retire more branches and therefore fits the varying sign, but the artifacts do
not prove which field differs.

The smallest proposed evidence change is conditional rather than general raw
retention:

1. For each bootstrap map read, record its ordinal and call site, SHA-256 of
   the exact payload, byte and line counts, and the after-minus-before RCB
   processing delta. A compact record is roughly 64 bytes per scan, about
   1.5KiB per run or 3KiB per two-run cell.
2. Only when a verify pair fails, retain the exact raw payload pair for the
   first scan whose processing delta differs. For the observed scans that is
   8,982--12,312 bytes.
3. Do not retain all snapshots by default. The 24 snapshots total 127,320
   bytes per `ioctl-fioclex` run and 128,280 bytes per
   `pause-alarm-interrupt` run: 254,640 and 256,560 bytes per two-run cell,
   respectively.

A normalized projection is not equivalent here because normalization can
erase the exact text widths, separators, pathname scanning, and numeric parse
work whose branches are being measured. A hash alone can identify the first
different input but cannot name its differing field. Even raw retention has a
stated limit: if several fields differ in the selected scan, it localizes the
inputs but does not prove which one accounts for 12 RCB without a separate
controlled test or finer measurement.

### SaBRe differs on host physical-exit observation

`getsockopt-null / verify / sabre` completed both guests with exit 0 but
produced 122 versus 121 INFO records. Both runs have identical COMMIT turns
0--3, the same turn-3 Exit resource and committed virtual time, the same
logical kill, and the same final report. The only INFO difference is:

`scheduler (step2_process_blocked): zero threads left anywhere, fizzling`

The scheduler loop first checks queues and `pending_physical_process_exits`
under one lock. `step2_process_blocked` later reacquires the lock after draining
the deterministic removal. The ptrace supervisor may observe the final kernel
wait status and remove the physical-exit barrier between those checks. If it
does, step 2 emits the extra line before returning `SkipTurn`; otherwise the
next outer check exits directly. Both paths later emit the normal
`run queue empty, exiting sched_loop` line.

This is a host-timing race in physical-exit/reaping bookkeeping, not a
different committed guest state. Suppressing or ignoring the extra INFO line
would remove evidence and alter the comparator result; it would not fix the
race.

## CPUID finding supersedes the historical host claim

The running host reported kernel
`6.19.2-0_fbk0_hardened_rc25_0_g1d99d2f6053c`. More importantly, a direct
`arch_prctl(ARCH_SET_CPUID, 0)` returned raw 0 with errno 0.
`rdrand_rdseed_is_masked` executed and passed 1/1. In the full-profile run,
`privileged-cpuid.faulting`, `privileged-pmu.preemption`, and `gate.manifest`
all passed.

The earlier `privileged-cpuid.faulting` red on this host was not a capability
failure: the wrapper inherited a 10 CPU-second default and killed a cold Cargo
build after 11.6s with zero tests executed. It must not be cited as evidence
that CPUID faulting is unavailable on this boot.

## Unresolved

- No full-profile receipt exists for the tested Hermit commit because
  `e2e.manifest_c_programs` failed and later nodes did not all execute.
- The canonical DBT verdict producer/consumer contract needs a decision and
  implementation that does not weaken the INFO comparison.
- The ptrace `dbt-unsupported-syscall` `no_result` cause is unlocalized.
- LiteInst's propagation mechanism is established, but the raw map field that
  originates the 12-RCB delta is unlocalized.
- SaBRe's host reaping race is characterized, but no scheduler change was made
  or proposed here.
- Issue #2352's direct release-binary LiteInst activation preflight hung twice
  before the guest started, while the full-run debug artifact completed the
  same cell. Runtime and fixture bytes match; existing evidence cannot separate
  release versus debug Hermit, direct versus systemd/safe-ci execution context,
  or concurrency. The hang is not unconditional and is not established as an
  `ioctl-fioclex` guest defect.
