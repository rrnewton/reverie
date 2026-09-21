# Proxygen transport-monitor native and loader diagnosis — 2026-09-21

This is a diagnostic screen of the already-MATERIAL target. It is not a TFA
sample, does not change the sealed plan or oracle, and cannot be counted as
formal evidence. No new Hermit invocation was made.

## Exact case and bounds

- canonical target:
  `fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test`
- selected case:
  `PopulateEdgeConnectionsLoggerTests/PopulateEdgeConnectionsLoggerTest.PopulateEdgeConnectionsLoggerEvent/without_response`
- test ELF: the already sealed local binary with SHA-256
  `115eba5170eef7bc39a583a5c3a5df28fb6596ac075994d35a60eb1c399d43ac`
- each native command selected exactly this case and used an outer timeout,
  `MemoryMax=16G`, `MemorySwapMax=0`, and a 64 MiB per-file limit
- diagnostic root:
  `/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign/monitor-native-profile-20260921`

## Native observations

| Control | Selected-test result | Process result | Wall | Max RSS | Relevant syscall counts |
| --- | --- | --- | ---: | ---: | --- |
| ordinary native | PASS in 36 ms | rc 0 | 4.25 s | 1,996,300 KiB | not traced |
| `strace -f -c`, default LSAN | PASS in 37 ms | rc 1 after the test because LeakSanitizer refuses ptrace | 4.74 s | 1,928,752 KiB | 5 clone/clone3, 26 futex |
| `strace -f -c`, `ASAN_OPTIONS=detect_leaks=0` | PASS in 27 ms | rc 0 | 7.58 s | 1,910,244 KiB | 4 clone3, 23 futex, zero eventfd/epoll/timerfd |

The successful traced control made 76,476 syscalls. Its five TIDs are the main
thread plus four `clone3` children. The selected test body does not create a
worker or explicitly hand work to another thread:
`ProxygenTransportMonitorTest.cpp` lines 470–526 constructs mocks and values,
then directly calls `httpTransactionDone`, `socketClose`, and
`populateEdgeConnectionsLoggerEvent` before checking fields. Together with the
absence of eventfd, epoll, and timerfd calls, the observed background threads
are not evidence of a competing target-thread order. This target fails the
current scheduler-sensitive admission gate despite its short wall time.

Primary ignored artifacts and hashes:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `native.stdout.txt` | 844 | `8c68ed5bf1010a8d27edbdeebd6b922254a347adaeb75d6fa7f9fad14d0d4cc5` |
| `native.time.txt` | 1,161 | `b943d5080f05c36e13a25da856a9d4a10b7eb844f5dcd883d6015d72fe4fd433` |
| `strace-nolsan.stdout.txt` | 844 | `8f9cf425581f980b3dbf155da6d5ded0ef3d6a94370563e330c881b15d49fa17` |
| `native.strace-nolsan-summary.txt` | 2,959 | `f8757d23659617b1235f7e88f84dc0f08a3c23baaf0462f5688e56d268e6b0c6` |
| `strace-nolsan.time.txt` | 1,307 | `5b29baaf7d24c2654e88d1045056ec19bf90aef7e356bf2555709526029e36dc` |

## Hermit boundary

The formal stderr's dynamic-loader message says that
`libdataswarm_if_dataswarm_thrift_dataset-rust.so` cannot be opened. That is a
downstream presentation, not the causal result. The sealed RPATH entry is a
symlink to an existing 3,002,840-byte ELF:

`/home/newton/work/tfa/fbsource/buck-out/v2/art/fbcode/3b44333ee2282447/dataswarm/if/__dataswarm_thrift_dataset-rust__/shared/libdataswarm_if_dataswarm_thrift_dataset-rust.so`

Its SHA-256 is
`4ca8a221c061e00db6720def945b7c8c43099af283bc21f011e2c1753a9c99a5`.
Both retained strict runs instead show this sequence:

1. the guest issues `openat` on the exact RPATH symlink;
2. Hermit logs `Injecting additional fstat to retrieve file metadata on fd 3`;
3. the enclosing guest `openat` completes as `Errno(EFAULT)` from that
   post-open path; and
4. the loader searches fallback paths and finally prints the missing-library
   wording.

The run-1 retained log is 16,800,086 bytes with SHA-256
`2dd36f83b77c5ef7a03c3ac4a6056f02bb5699ee9e4be7e43e61da3241d7e919`;
run 2 is the same size with SHA-256
`c439a8aceda3e2c921c36e00c3285e15ae9bd36602ec23e1aa1e06a6900a3e21`.
At pinned Hermit commit `4630ee2a61f93221a43e2baec0a15f34102a2c61`,
the relevant source boundary is
`detcore/src/syscalls/files.rs::inject_fstat` (lines 687–718), called by
`add_fd` after `handle_openat` has obtained the real fd. The function reserves a
144-byte `libc::stat` on the tracee stack, commits that scratch buffer, injects
`fstat`, reads the result, and clears the buffer. Any of those four fallible
steps can surface as the enclosing `openat` result.

The leading source-level hypothesis is the first step: ptrace scratch-stack
commit. Pinned Reverie commit
`f7bd85e11dd258112148ed2cba6531501a1a00d9` reserves a 128-byte red zone and
writes the `stat` at `[rsp-272,rsp-128)`. Its `process_vm_writev` path converts
an inaccessible page into a short write and then `EFAULT`. The preceding
successful loader open uses a stack path buffer 32 bytes higher than the
failing one, and run 2 returns `EFAULT` only 36 microseconds after the injection
message. Those facts fit a scratch range crossing an unmapped or protected
page. They do not prove it: the INFO log does not record `rsp` or the failing
substep, and run 1 spends 409 milliseconds between the same two messages.
Accordingly, scratch commit is a strong causal hypothesis, while the proven
boundary remains `inject_fstat` as a whole.

The concrete fix to test is bounded and reuses an existing internal-scratch
pattern without changing the target oracle or scheduler flags:

1. keep the successful guest-stack `StackGuard` alive through the fast-path
   injection/read/clear sequence;
2. if `stack.commit()` returns `EFAULT`, inject a temporary anonymous RW
   mapping, use it for `fstat`, read the result, and unmap it on every exit;
   `detcore/src/syscalls/helpers.rs` lines 623–690 already implements this
   scratch-mapping pattern for atomic `writev`; and
3. if `add_fd` still fails after a real open, close that newly created guest fd
   before returning, rather than converting the open to an error while leaking
   the descriptor.

A regression should use a ptrace guest with a lower `PROT_NONE` page and an
upper RW page, switch `rsp` to `upper+256`, and issue raw
`openat("/dev/null")`. That forces the 272-byte scratch/red-zone range to cross
16 protected bytes. The unmodified code should return `EFAULT`; the fallback
must return a usable, closable fd in ordinary and strict modes. No source
change or confirmatory Hermit run was made in this campaign.

## Plan and campaign implications

The strict-ancestor plan receipt seals one route and one cell. Adding a rebuilt
ELF, environment override, staged DSO, or fallback cell now would change the
sealed route/cell digests and retroactively plan observed evidence. A second
record for the same canonical target is also rejected by the schema and
validator. The only remaining in-plan action is seeds 1–51 in the unchanged
cell, which encounter this pre-test loader path and do not meet the current
scheduler-sensitive candidate priority.

Accordingly, the formal record remains an honest non-exhausted
NOT-DETERMINABLE result. It is not relabelled as a missing-dependency blocker,
and no diagnostic result above is retrofitted into the formal ledger.
