# Native scheduler-sensitive candidate screen — 2026-09-21

These are diagnostic native runs only. They are not formal TFA samples and do
not change any predeclared plan, oracle, or outcome. Every invocation selected
exactly one cohort case (or the one-test target) and ran under bounded wall,
memory, swap, and output controls. Raw artifacts are under
`/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign`.

The current gate requires all of:

1. selected-case native runtime no more than 10 seconds;
2. demonstrated competing threads plus futex/eventfd/epoll/callback ordering;
3. a thread/process footprint small enough that ptrace cost is not the likely
   dominant result.

## Results

| Candidate | Native observation | Concurrency/profile observation | Disposition |
|---|---|---|---|
| Proxygen TLS terminator | PASS, 182 ms test / 0.24 s wall | 28 TIDs, 27 clones, 1,157 futexes, 9 eventfds, 9 epoll instances | Qualifies, but its existing 15-seed formal cell is exhausted with 2 valid FAIL and 13 censored samples, no PASS |
| Proxygen transport monitor | PASS, 36 ms test / 4.25 s wall; traced control with LeakSanitizer disabled PASS, 27 ms test / 7.58 s wall | 5 TIDs, 4 `clone3` calls, 23 futexes, no eventfd/epoll/timerfd; selected source is synchronous | Reject: no competing target-thread ordering; the existing formal route stops before the test when Hermit's post-open `inject_fstat` path converts the enclosing `openat` to `EFAULT` |
| LogStore Health | one `SIGSEGV` at 4.56 s; traced run PASS at 2.673 s | 657 TIDs, 656 clones, 16,739 futexes, 493 eventfds, 475 epoll instances | Reject: ptrace fanout; formal seed 0 stopped before the oracle in a two-thread scheduler deadlock followed by an internal container failure |
| Shadow thread affinity | PASS, 8.641 s test / 11.22 s wall | 338 TIDs; bounded strace timed out at 30 s after 337 clones and 24,797 futexes | Reject: native wall and ptrace fanout |
| Thrift coroutine | PASS, 1.619 s test / 2.61 s wall | 331 TIDs, 330 clones, 5,210 futexes, 321 eventfds | Reject: ptrace fanout |
| Thrift instrumentation | PASS, 909 ms test / 1.51 s wall | 331 TIDs, 330 clones, 4,296 futexes, 321 eventfds | Reject: ptrace fanout |
| McRefill | no GoogleTest marker by 25 s | no qualifying profile | Reject: native runtime; formal seed 0 also failed in the loader |
| Cachelib EventTracker | entered selected case, no completion by 13 s | no qualifying profile | Reject: native runtime; formal seed 0 reached 120-second outer timeout |
| LogStore conditional-write retry | no GoogleTest marker by 13 s | no qualifying profile | Reject: native runtime; formal seed 0 also failed in the loader |
| ZippyDB VRT usage | emitted target-owned counter failure, then remained in retry/sleep past 13 s | no qualifying bounded completion | Reject: native runtime |
| Mcrouter MuxIO thread-pool Toaster | selected suite setup exceeded 13 s and launched multiple ucache processes | multiprocess service harness | Reject: native runtime/process fanout |
| LogDevice SSL authentication | entered selected case, no completion by 13 s | no qualifying bounded completion | Reject: native runtime |
| Warm Storage FileCache | no GoogleTest marker by 13 s | no qualifying profile | Reject: native runtime |
| Warm Storage CreateReaderRetry | entered selected future-retry path, no completion by 13 s | no qualifying bounded completion | Reject: native runtime |
| Manifold crypto plugin | selected case skipped because `CRYPTO_PLUGIN_SO_PATH` was unset; 5.28 s wall and about 2.28 GiB RSS | no target execution | Reject: target not executed |
| Manifold payload-timeout client | entered selected case, no completion by 13 s | no qualifying bounded completion | Reject: native runtime and time-budget focus |
| Warm Storage write-time encoding | onebox setup/backoff exceeded 13 s | service-environment setup | Reject: native runtime/setup |
| ZippyDB admin handler | no GoogleTest marker by 13 s | no qualifying profile | Reject: native runtime |
| Manifold Rust client | entered its exact one-test invocation, no completion by 13 s | libtest fixed to `--test-threads=1`; no qualifying bounded completion | Reject: native runtime and no demonstrated competing worker ordering |
| Cachelib Cogwheel free-list stress | canonical test targets have no local executable output; only `.par` launcher output exists | launcher/stress harness required | Reject: violates local-test-binary rule |

The remaining cohort entries are not stronger immediate choices: io_uring is
explicitly excluded because Hermit intentionally returns `ENOSYS`; MySQL
requires the cohort's AArch64 MTR route; DRocksDB previously filled a 1 GiB
retained log while deadlocked; and the remaining Thrift resource-pool case uses
the same high-fanout server scaffold.

No new formal cell is sealed from this screen. Continuing requires a new
candidate that meets the gate or an explicit change to the current
runtime/thread-footprint priority; diagnostic screens cannot be retrofitted as
formal evidence.
