# Thrift instrumentation native profile — 2026-09-21

This is prospective candidate-screen evidence only. It is not a formal Hermit
sample and is not reused as a PASS or FAIL witness.

## Frozen identity and controls

- Canonical target: `fbcode//thrift/lib/cpp2/test:InstrumentationTest`
- Exact case:
  `RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
- Source revision: `7f239d5b37dfbecf32fed144468da461f998add5`
- ELF: 11,514,064 bytes, SHA-256
  `2ec5d2128aa5af3ae86d46659054dd72b64ef86a389cf89934901a2e094cd0fc`
- Source file SHA-256:
  `99a6c74f02f855dd55ff2e07cd1a20ca281c55d568423ff212262eaf5c2a1795`
- Affinity: CPU 0 via `taskset -c 0`
- Per-repeat bounds: 12 seconds wall, 16 GiB memory, zero swap, 64 MiB
  file-size limit, no core dump; 12-repeat maximum.
- Strace bound: 20 seconds under the same affinity, memory, swap, output,
  and core limits.

The recaptured manifest, exact argv, before/after ELF digests, exit status,
timing, output, syscall summary, and per-artifact digests are retained under
`/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign/screen-thrift-instrumentation-affinity1-recapture-20260921`.
`ROOT-MANIFEST.sha256` enumerates every pre-manifest file in sorted relative
path order and has SHA-256
`a3fed6752924e6a2901fd5785e7b0eb53037b873c70b40c11550336575b67ab6`.
The complete directory is 164 KiB. The earlier affinity screen is diagnostic
context only and is not used here because its repeat status capture was
ambiguous.

## Results

The strace control passed in 0.70 seconds wall with a 138 ms selected-test
runtime and 160,084 KiB maximum RSS. It recorded 11,662 syscalls: 15 successful
`clone3` calls (16 TIDs including the leader), 307 `futex` calls (11 errors),
6 `eventfd2` calls, 4 `epoll_create` calls, 45 `epoll_ctl` calls, and 43
`epoll_wait` calls.

The first three untraced executions passed in 0.59, 0.56, and 0.55 seconds
wall, with selected-test runtimes of 125, 126, and 128 ms. Repeat 4 terminated
by SIGABRT in 1.57 seconds wall and had true captured exit status 134. Its
stdout contains the unchanged target assertion:

```text
fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp:836: Failure
Expected equality of these values:
  reqSnapshots.size()
    Which is: 2
  3
```

The later TSan report and Baton assertion vary from the earlier diagnostic
failure and occur after the fatal size assertion skips cleanup. They are
treated as teardown fallout, not as independent target-owned FAIL classes.

## Scheduler sensitivity

The selected test issues three RPCs. One callback completes, a second posts
`baton2` and blocks on `baton3`, and the third is submitted while the main
thread spins until at least two requests are active. The main thread snapshots
request state before releasing `baton3`; line 836 requires all three requests
to be represented. The observed two-snapshot result therefore depends on
ordering among the main thread, request callbacks, worker threads, batons,
futexes, and event-base wakeups. This is materially different from a
single-thread wall-time or virtual-time budget assertion.

The unrestricted native profile's 331 TIDs reflected host-sized worker fanout.
The affinity-pinned profile demonstrates the same exact test with 16 TIDs and
sub-second PASS executions, so ptrace fanout is not the dominant native
mechanism in the topology used to qualify this candidate. Prior retained logs
from the exact fixed Hermit binary record `sched_getaffinity` being virtualized
to a mask containing only CPU 0. The formal route therefore keeps its exact
Hermit argv and does not add a separate `taskset` wrapper.
