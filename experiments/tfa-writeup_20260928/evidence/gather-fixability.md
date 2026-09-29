# TFA fixability gather (sub-tfa2-030, devbig030, 2026-09-28)

Read-only evidence gathering for the kernel-7.1 TFA campaign write-up. The owner's
success criterion for a target is: Hermit produced one passing seed and one failing
seed. Root-causing is not required. This note asks, for every target that did not
get there, (a) what probably makes it flaky natively, (b) why Hermit did not produce
both seeds, and (c) whether a Hermit bugfix or a simple configuration change would
plausibly get it there.

Conventions:

- Repo-relative paths are under `/home/newton/work/dev-hermit`. `K71` means
  `experiments/tfa-kernel71-campaign_20260921`. `TIME` means
  `experiments/tfa-kernel71-time-input-campaign_20260921`. `K613` means the earlier
  kernel-6.13 campaign, `experiments/tfa-flaky-tests_20260917`, which ran an older
  Hermit binary (sha256 `000eeb5e…`, source 79da5231).
- `LOG:` means a retained Hermit verifier log under
  `/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/`. These
  files are ignored and untracked, but they still existed when this note was written.
- **PROVEN** means the records or logs show it directly. **INFERENCE** means my own
  reasoning. **UNVERIFIED** means plausible, but I did not run it or check it.
- Hermit main is the local tracking ref `origin/main` at `fdd8f55a8e7c`, committed
  2026-09-28 13:07 -0700. A fresh `git fetch` failed (`CONNECT tunnel failed, response
  403`), so "on main" means "as of that ref". The campaign binary was built from
  `4630ee2a61f9…`, which was squash-merged to main as `ccbe37e91` ("Accept virtual
  RLIMIT_CORE updates (#3142)"). Reverie pins: `f7bd85e11dd2` at the campaign
  revision and `6297f7154299` on main (hermit `Cargo.lock`, `reverie-ptrace` entry).
- Native failure text comes from read-only TestInfra queries run on 2026-09-28:
  `meta testinfra.result history --test=<ID> --statuses=FAILED,FATAL,TIMEOUT --since=ONE_WEEK --limit=3 --output=json`,
  plus `meta testinfra.run get --id=<result_id> --full` for full traces. Raw outputs
  are in `/tmp/sub-tfa2-030/ti/<test_id>.json`, `/tmp/sub-tfa2-030/ti/run-get-*.txt`
  and `/tmp/sub-tfa2-030/ti/summary.txt`. These cover the week before 2026-09-28,
  not the 2026-09-17 snapshot the registry used, and the returned `detail_short` is
  truncated at about 800 characters.
- Test source was read from `/home/newton/fbsource` at `e79d544a25ca` (2026-09-27).
  That is **not** the campaign's pinned target revision `7f239d5b37df`, so line
  numbers can drift. I cite source only for test structure.

---

## 0. Summary table (7 attempted NOT-DETERMINABLE targets)

| Target | Exact Hermit stop | Proximate category | Blocked by a Hermit defect? | Fixability to reach PASS+FAIL |
|---|---|---|---|---|
| Cachelib EventTracker | 120 s outer timeout during run 1; no oracle line | Too slow under ptrace (vDSO clock reads trapped) | Throughput gap, not a correctness bug | **UNKNOWN**: a longer deadline is an EASY first step (UNVERIFIED); whether any seed FAILs is unknown |
| Folly TimerFD | 9/9 valid PASS, no FAIL | Ran fine; the timing flake was never provoked | No (the related open timerfd-virtualization PR matters) | **MEDIUM**: needs a time-dilation cell and probably virtual timerfd |
| LogDevice log_store_test | loader: `openat` → `EFAULT` right after `inject_fstat` | Hermit defect (scratch stack write) | **Yes. PROVEN same defect as monitor, McRefill and the reproducer** | Loader: **EASY** small fix or `--no-virtualize-metadata` (UNVERIFIED). Both seeds afterwards: **UNKNOWN** (heavy startup, a timing test) |
| LogDevice health_monitor | "Deadlock detected" (2 FutexWait threads), then `container-child-exit` | Hermit defect (**stray SIGSTKFLT killed 96 threads first**), plus ptrace fanout | **Yes (new finding, strong INFERENCE)** | **HARD**: bug fix is MEDIUM, and 657 native TIDs is ptrace fanout |
| Mcrouter McRefill | loader: same `openat` → `EFAULT` | Same Hermit defect | **Yes. PROVEN same signature** | Loader: **EASY**. Both seeds afterwards: **HARD** (>25 s native, 6 McServer processes, timeout-driven failure) |
| Proxygen transport monitor | loader: same `openat` → `EFAULT` | Same Hermit defect | **Yes. PROVEN** | Loader: **EASY**. Both seeds afterwards: **UNKNOWN**, probably no FAIL (selected case is synchronous; flake cause unknown) |
| Proxygen TLS terminator | Cell 1: 15/15 `certificate is not yet valid` at the 2026-01-01 epoch. Cell 2 (real epoch): 16/16 `folly::FutureTimeout`. 29/31 tuples failed strict verify | Fixed default epoch (config), then network/environment plus Hermit verify-determinism gaps | Partly. Epoch is fixed on main. Teardown-order and loopback-epoll nondeterminism are not | **HARD** overall. `--epoch` is EASY but was already tried and was not sufficient. No PASS in 31 tuples |

---

## 1. The seven NOT-DETERMINABLE targets

### 1.1 The loader defect (log_store_test, McRefill, transport monitor): one defect, not three

**Records:**
`K71/services/logdevice/log-store-test--589f359badef/evidence/seed-000-replica-{1,2}.stderr.txt`,
`K71/services/mcrouter/mc-refill-route-wrapper-test--82eb51b9426d/evidence/seed-000-replica-{1,2}.stderr.txt`,
`K71/services/proxygen/proxygen-transport-monitor-test--fcaf56433e7f/evidence/seed-000-replica-{1,2}.stderr.txt`.
All three passed strict verification ("Success: deterministic", BitwiseInfoV1, no
relaxations) and exited 127 with the dynamic-loader message, for example:

```
…/log_store_test: error while loading shared libraries: libconfigerator_structs_warm_storage_thrift_ws_request_tag-cpp2-types.so: cannot open shared object file: No such file or directory
…/mc_refill_route_wrapper_test: error while loading shared libraries: libaxon_common_if_control_plane_service-cpp2-clients.so: cannot open shared object file: No such file or directory
…/proxygen_transport_monitor_test: error while loading shared libraries: libdataswarm_if_dataswarm_thrift_dataset-rust.so: cannot open shared object file: No such file or directory
```

Each `seed-000-replica-1.result.json` records `diagnostic: "status=CENSORED; selected=0; unit_result=exit-code; exit=127; BitwiseInfoV1=matched"`.

**The retained logs show the same signature in all three (PROVEN).** Commands:
`grep -c 'Injecting additional fstat' LOG`, `grep -n EFAULT LOG`, and a `grep -o`
of `openat(-100, 0x…` path-buffer addresses.

| Target | Retained run-1 log (bytes) | `inject_fstat` calls up to and including the failure | Failing guest syscall | Path buffer of failing `openat` | Buffer of preceding successful loader `openat` |
|---|---|---:|---|---|---|
| log_store_test | `brs-tfa-kernel71-logstore-verify-3012349-1790002573993825987/run1_log_A6gHB` (17,813,784) | 3,015 | #24731, log line 67714 | `0x7ffffffde180` | `0x7ffffffde1c0` |
| McRefill | `brs-tfa-kernel71-mcrefill-verify-3174621-1790002707339803842/run1_log_Xc7VF` (16,585,633) | 2,692 | #22977, line 62277 | `0x7ffffffde180` | `0x7ffffffde1a0` |
| transport monitor | `brs-tfa-kernel71-monitor-verify-4145718-1790000155029090422/run1_log_dChe5` (16,800,086) | 2,689 | #23110, line 62511 | `0x7ffffffde180` | `0x7ffffffde1a0` |

The sequence is identical in each case. Quoting log_store, log lines 67711-67714:

```
INFO detcore: DETLOG [syscall][detcore, dtid 3] inbound syscall: openat(-100, 0x7ffffffde180 -> ".../__log_store_test__shared_libs_symlink_tree/libconfigerator_structs_warm_storage_thrift_ws_request_tag-cpp2-types.so", OFlag(O_CLOEXEC)) = ?
INFO detcore::syscalls::files: Injecting additional fstat to retrieve file metadata on fd 3.
INFO detcore: DETLOG [syscall][detcore, dtid 3] finish syscall #24731: openat(... same path ...) = Err(Errno(EFAULT))
```

After that come 10 fallback `openat`s at the same buffer, all `ENOENT`
(#24732–#24749), and then the loader's "cannot open shared object" exit. The loader's
path buffer moves steadily down the stack as it loads successive DSOs
(`…240 → …220 → …200 → …1e0 → …1c0/1a0 → …180`). All three binaries fail at exactly
the same guest address, `0x7ffffffde180`.

**The reproducer is the same code path (PROVEN).** In
`experiments/tfa-openat-stack-size-20260917/`, `tfa_open_stack_probe.c` and `.S` issue
a raw `openat` with a controlled number of writable bytes below `rsp`. The layout is
one `PROT_NONE` page followed by one RW page.

- With 192 bytes: `openat raw_rc=-14 errno=14`
  (`evidence/hermit-906-open-stack-192.stdout`).
- With 512 bytes: `raw_rc=3`.
- The 5,000-iteration open loop control succeeds.

`evidence/hermit-906-open-stack-192-info.stderr` lines 104-109 show the same three
lines (`inbound openat` / `Injecting additional fstat` / `= Err(Errno(EFAULT))`). Its
path argument was a log_store_test shared library. That probe ran under the kernel-6.13
era binary (sha256 `000eeb5e…`, `…-192-info.report`).

**Mechanism.** PROVEN from source; the stack-growth part is INFERENCE.

- At `4630ee2a`, `detcore/src/syscalls/files.rs:688-718` (`inject_fstat`) takes the
  guest stack, reserves a 144-byte `libc::stat`, calls `stack.commit()?` (line 699),
  injects `fstat`, reads the result back and zeroes it.
- `add_fd` (lines 721-731) calls it whenever `config.virtualize_metadata` is set.
- Reverie `reverie-ptrace/src/stack.rs` at `f7bd85e1` sets `REDZONE_SIZE = 128`
  (line 27). `GuestStack::new` sets `top = rsp - 128`, and `commit()` does
  `self.task.write_exact(remote_sp, …)?` (line 169). So the scratch write lands at
  `[rsp-272, rsp-128)`. Any fault becomes `EFAULT`, which `?` turns into the result of
  the guest's `openat`. The real fd is already open, so it leaks (as
  `K71/services/proxygen/…/PROXYGEN-MONITOR-NATIVE-AND-LOADER-DIAGNOSIS-20260921.md`
  also notes).
- In the probe, the unwritable bytes are the explicit `PROT_NONE` page: 192 < 272 ≤ 512.
- In the real binaries, INFERENCE: reverie-ptrace disables ASLR
  (`reverie-ptrace/src/tracer.rs:1656`, `personality(PER_LINUX | ADDR_NO_RANDOMIZE)`).
  The main-thread stack therefore tops out at `0x7ffffffff000`, and its initial VMA
  typically starts at `0x7ffffffde000` (132 KiB).
- The failing buffer `0x7ffffffde180` is only 384 bytes above that boundary. The
  preceding open, 32 or 64 bytes higher, succeeded. That fits the loader's `rsp`
  crossing the point where `rsp-272` falls below the stack VMA for the first time.
- The loader's own pushes fault the stack down normally. Hermit's remote
  `process_vm_writev`/ptrace write below the VMA does not grow the stack. My
  understanding is that Linux ≥ 6.5 removed stack expansion from remote GUP; both
  campaign kernels (6.13, 7.1) are newer than that. UNVERIFIED against this kernel's
  source. A kernel WARN "GUP no longer grows the stack" would confirm it, but it was
  not in the current `dmesg` ring.
- This also explains why these three large `@mode/dev` binaries fail (about 2,700–3,000
  DSO opens push the loader deep). The dev-asan, dev-tsan, libcxx and opt winners did
  not fail (INFERENCE).

**Status on main: NOT fixed (PROVEN from source).**

- `origin/main:detcore/src/syscalls/files.rs:747-777` `inject_fstat` is byte-for-byte
  the same logic (`stack.reserve()`, `stack.commit()?`, inject, read, zero).
- `add_fd` is at lines 780-793.
- Reverie main pin `6297f715`: `reverie-ptrace/src/stack.rs` differs from `f7bd85e1`
  only by an added `write_with_user_access` forwarder and a test
  (`git diff f7bd85e1 6297f715 -- reverie-ptrace/src/stack.rs`; commit 7a072013).
  `REDZONE_SIZE` and `commit()` are unchanged.

**No GitHub issue or PR found.** These searches returned nothing relevant on
rrnewton/hermit (output files `/tmp/sub-tfa2-030/gh-fixability-N.json`):

- Issues: `inject_fstat` (only https://github.com/rrnewton/hermit/issues/1850, an
  unrelated pipe hang), `openat EFAULT` (none), `EFAULT stack` (none), `red zone`
  (none), `stack expansion` (none), `fstat` (unrelated inode issues
  https://github.com/rrnewton/hermit/issues/3306 and
  https://github.com/rrnewton/hermit/issues/3307).
- PRs: `inject_fstat` (only https://github.com/rrnewton/hermit/pull/1851, unrelated),
  `EFAULT` (none about this).
- The defect should get an issue. The tracked write-up is
  `experiments/tfa-openat-stack-size-20260917/README.md` plus the monitor diagnosis
  note.

**Fix options (INFERENCE, UNVERIFIED):**

1. **Configuration workaround:** `--no-virtualize-metadata`, which exists at both
   revisions (`detcore-model/src/config.rs:246` at 4630ee2a, `:252` on main). With it,
   `add_fd` skips `inject_fstat` (main `files.rs:787`). Detcore then also stops
   subscribing `stat`, `fstat`, `newfstatat`, `statx` and `getdents`
   (main `detcore/src/lib.rs:1228-1237`), so the guest sees real inode and mtime
   values. `run.rs:3432-3437` only refuses the opposite combination (virtual metadata
   without virtual time). I found no strict-mode refusal of this flag, but I did not
   run it. Metadata would then be host-dependent, which weakens cross-host
   reproducibility, though within one `--verify` pair on one host it is probably
   stable.
2. **Small fix:** if the stack commit returns `EFAULT`, fall back to an injected
   anonymous scratch mapping; that pattern already exists on main at
   `detcore/src/syscalls/helpers.rs:603-690`. Alternatively, fstat the fd host-side
   through `/proc/<tid>/fd/<n>`, which needs no guest memory. Either way, close the new
   guest fd if `add_fd` fails.
3. **Broader fix:** put the fallback in Reverie `GuestStack::commit`. This matters
   because 24 detcore sites use `guest.stack().await` scratch
   (`git grep -n 'guest.stack().await' origin/main -- detcore/src`), and any of them
   could hit the same boundary.

The probe's 192/512 arms are a ready-made regression test. A stack-growth-only fix
would not pass the 192 arm, because that arm is a `PROT_NONE` page.

**Related K613 target (INFERENCE):** the Manifold Rust client (an unattempted
kernel-7.1 target) failed the same way in K613:
`K613/services/manifold/manifold-client-tests--ffb0d3de4eb5/evidence/seed-000-discovery.stderr.txt`
shows "Determinism verified", then `…manifold_client_tests: error while loading shared
libraries: libprivacy_enforcer_fci_fci_policy_enforcer.so`, with 61,687 INFO messages.
Its log was not retained, so it is only the same presentation.

#### Per-target view after a loader fix

- **log_store_test** (`LogStoreTest.StoreRecordCondWriteFailedSameEpoch_FallbackReadFailsAndRetries`, `@mode/dev`, native 3/32 = 9.38% in `ai_docs/tfa-candidate-sourcing-2026-09-17.md`).
  - Native flake cause: UNKNOWN; no native failure in the last week
    (`/tmp/sub-tfa2-030/ti/281475271465112.json`, n=0). INFERENCE from source
    (`LogStoreTest.cpp:586-626` in /home/newton/fbsource): a mocked write fails, a
    fallback read returns `TIMEDOUT`, the write retries with a 1–10 ms backoff inside a
    100 ms `storeRecord` timeout, and the test then checks a 60-second-window stats
    counter. That makes it a timeout / wall-clock sensitive test.
  - Native screen: "no GoogleTest marker by 13 s"
    (`K71/NATIVE-CANDIDATE-SCREEN-20260921.md`). Under Hermit the loader had issued
    3,015 fstat injections in about 10 s of wall time (log timestamps 14:57:10 → 14:57:20).
  - After the fix, PASS is plausible. FAIL would need time-dilation controls like the
    HTTPCoro cell. **UNKNOWN.**
- **McRefill** (`McRefillWrapperBucketized/0.refillOnMiss`, `@mode/dev-tsan`, native 77/97 = 79.38%).
  - Native failure (`/tmp/sub-tfa2-030/ti/run-get-562950220120091.txt`):
    `McToasterUtils.h:161 … expected: mc_res_stored actual: mc_res_timeout. Reply message: TTransportException: Timed out`,
    then `McRefillRouteWrapperTest.cpp:227 … getStat("total_items") Which is: 0`.
    Two of the last three recorded failures are 600 s TIMEOUTs. The gap audit notes a
    12,366,336-byte binary with no marker by 25 s that starts six McServer instances
    (`K71/TERMINAL-AND-CANDIDATE-GAP-AUDIT-20260921.md`).
  - After the fix: a multiprocess, timeout-driven harness under ptrace. **HARD.**
- **Transport monitor** (`PopulateEdgeConnectionsLoggerEvent/without_response`, `@mode/dev-asan`, native 2/30).
  - Native flake cause: UNKNOWN (no failure in the last week,
    `/tmp/sub-tfa2-030/ti/562950215341224.json` n=0). The selected body is synchronous:
    `ProxygenTransportMonitorTest.cpp:470-543` builds mocks and calls
    `httpTransactionDone`/`socketClose`/`populateEdgeConnectionsLoggerEvent` directly.
    A native `strace` showed 5 TIDs and no eventfd, epoll or timerfd (monitor diagnosis
    note).
  - INFERENCE: if it flakes at all, it is not by thread interleaving. The sampler
    constants in the test (`attachRateOnAccept 0.5`, `samplingRateOnAccept 0.4`) hint at
    randomness, so `--rng-seed` might matter (UNVERIFIED).
  - After the fix: PASS is likely; FAIL is **UNKNOWN / unlikely**.

### 1.2 Proxygen TLS terminator: was the epoch the problem, and is `--epoch` the fix?

**Cell 1, fixed epoch** (`K71/services/proxygen/tls-terminator-test--60719afd988e/`;
`--chaos-target-races`, `--fuzz-seed` 0–14, no `--epoch`):

- **All 15** seeds' stderr contains
  `what():  AsyncSocketException: fizz::FizzVerificationException: certificate verification failed: certificate is not yet valid, type = SSL error`
  (`grep -c 'certificate is not yet valid' evidence/seed-0NN-replica-1.stderr.txt` = 1
  for every seed 000–014).
- The guest's glog timestamps read `E0101 00:00:00.239487`, i.e. 1 January.
- Hermit's default epoch at the campaign revision is
  `DEFAULT_EPOCH_STR = "2026-01-01T00:00:00Z"` (`4630ee2a:detcore-model/src/config.rs:1263`,
  used as the `--epoch`/`HERMIT_EPOCH` default at lines 201-211). Virtual time in other
  campaign logs starts at 1_767_225_600 s, which is 2026-01-01T00:00:00Z.
- Seeds 11 and 12 matched strict verification (exit 134). They were reclassified as
  censored because certificate errors were excluded from the target-owned class
  (`TLS-ORACLE-INTEGRITY-REPAIR-20260921.md`).
- The other 13 failed BitwiseInfoV1. Their first divergence is at the very end, after
  the abort `tgkill(3, 9, 6)`: the order of
  `logically_kill: Scheduler removing all knowledge of [det]tid N` lines differs
  (seed 000 stderr, "Mismatch at log messages 6314 … of 6328").

**Cell 2, realistic epoch**
(`TIME/services/proxygen/tls-terminator-test--60719afd988e/`; `--epoch=2026-09-21T18:54:35Z`,
`--sched-seed` 0–15, no target races; plan `TLS-SCHED-NOW-PLAN-20260921.md`):

- **Zero** of 16 seeds show a certificate error.
- **All 16** abort with `terminate called after throwing an instance of 'folly::FutureTimeout' what(): Timed out`,
  thrown from `proxygen::TlsTerminatorTest_terminateTls_Test::TestBody()::$_0` at
  `TlsTerminatorTest.cpp:308` (e.g. `selection-006-…-replica-1.stderr.txt` lines 480-497).
- **All 16** failed strict verification. Signatures, from a per-file grep of
  `evidence/selection-0NN-*-replica-1.stderr.txt`:
  - 14/16 have a stderr mismatch. Glog timestamps differ between replicas, for example
    `E0921 18:54:35.513854` vs `18:54:35.514585` (seed 000, lines 4 and 9).
  - The first INFO divergence (seed 000) is an `epoll_wait(11, …, -1)` in dtid 11,
    right after `recvfrom(7, …) = Ok(58)`.
  - In the retained log `brs-tfa-kernel71-tls-sched-now-2936582-20260921-1036584-1790016867759891372/run1_log_LwmYh`,
    fd 7 is the accepted end of an in-process IPv6 loopback TCP connection: dtid 3
    `socket(10,1,0)=5; bind; listen(5,10)`, dtid 7 `accept(5)=Ok(7)`, dtid 3
    `connect(6,…)`.
  - The remaining 2/16 (seeds 6 and 14) diverge only in `logically_kill` order.
- Every run also logs
  `Failed to get SMC tiers for localhost … Connection refused`, a Falcon
  `Exception while fetching entity … on host devbig030`, and
  `configerator proxy2+fuse unhealthy`. Hermit's default networking is `Local`,
  loopback only (`4630ee2a:hermit-cli/src/bin/hermit/run.rs:828-832`).

**Native failure:** the three TestInfra failures from 2026-09-28 (STRESS_RUN) end in
`terminate called after throwing an instance of 'std::runtime_error' what(): Closed.`
at `TlsTerminatorTest.cpp:308` (`/tmp/sub-tfa2-030/ti/tls-run-get.txt` lines 34-44).
Native rate is 23/96 (registry). The native profile shows 28 TIDs, 1,157 futexes and
9 epoll instances (`TLS-NATIVE-PROFILE-20260921.md`).

**Answers:**

- **Is the certificate abort caused by the 2026-01-01 default epoch?** Yes. That is a
  strong INFERENCE, close to proven: the error appears in 15/15 fixed-epoch tuples and
  0/16 realistic-epoch tuples. The two cells also differ in their seed flag and
  target-races setting, which are unrelated to certificate validity.
- **Is `--epoch` an easy fix?** It is a flag that already existed. On main, an omitted
  epoch is now captured from host time once per execution: `402ba9737`, "Capture one
  reproducible virtual epoch per execution", 2026-09-22 (`main.rs:244`
  `capture_default_epoch`). So this part is fixed.
- **Would it be enough?** No. Cell 2 already used a realistic epoch, and it produced
  FutureTimeout in all 16 seeds, **no PASS**, and 16/16 strict-verify failures.
- **What remains:**
  - (i) An environment dependency. SMC, Falcon and configerator are unreachable under
    `--network=local`. INFERENCE: this probably drives the timeout. `--network=host`
    would bring in external nondeterminism.
  - (ii) A Hermit determinism gap. Readiness on guest-internal loopback TCP is
    host-timed. No issue found (searches "loopback", "loopback TCP epoll",
    "socket epoll nondeterministic").
  - (iii) A Hermit teardown-order divergence after a signal death. The closest open
    issue is https://github.com/rrnewton/hermit/issues/3247 (exec-time variant). No
    issue found for the signal-death variant (search "logically_kill").
- **Fixability: HARD overall** (MEDIUM Hermit work plus an environment stub).

The native failure (`runtime_error: Closed.`) was never observed under Hermit.

### 1.3 Folly TimerFD (9/9 PASS, no FAIL)

- **Record:** `K71/services/folly/timerfd-timeout-manager-test--246a62382461/`. Case
  `TimerFDTimeoutManagerTest.ReschedTest`, `@mode/dev-tsan`, `--fuzz-seed` 0–8 with
  `--chaos-target-races`. Every replica printed
  `[       OK ] TimerFDTimeoutManagerTest.ReschedTest (10 ms)` and matched BitwiseInfoV1
  (18/18 executions; `current_outcome.kind = NOT-DETERMINABLE`, budget exhausted
  at 9/13).
- **Native failure text** (`/tmp/sub-tfa2-030/ti/ti-timerfd.json`, 3/3 shown):
  `TimerFDTimeoutManagerTest.cpp:307: Failure Expected equality of these values: t2.timestamps.size() Which is: 0 / 1`.
  Native rate 28/93.
- **Test source** (fbsource `TimerFDTimeoutManagerTest.cpp:281-312`): t1 fires at
  128 µs, and its callback schedules t2 at +255 µs. A terminator `et` ends the loop at
  1,000 µs. `T_CHECK_TIMEOUT` allows 5 ms of slop minus schedstat wait
  (`TimeUtil.cpp:243-291`).
- **Native cause, INFERENCE (strong):** a wall-clock timing flake. If t1's callback
  runs about 617 µs or more late, t2 is not due before `et` terminates the loop. Under
  load, or with TSan, that happens.
- **Why Hermit gave only PASS:** the only swept knob was `--fuzz-seed` with target
  races. No time-dilation control was used. INFERENCE: this is a single event-base
  thread, so scheduling choices alone cannot delay t1's callback relative to `et`.
  Also, at the campaign revision and on main, `timerfd_settime` arms a real host timer.
  Open PR https://github.com/rrnewton/hermit/pull/3229, "Drive timerfd readiness from
  the virtual clock", says timerfd "armed a real host timer … expired after host
  milliseconds while the guest advanced by scheduler turns". Under ptrace's slowdown a
  host timer tends to be already expired when polled, which favours PASS. Related open
  issues: https://github.com/rrnewton/hermit/issues/1923,
  https://github.com/rrnewton/hermit/issues/1930 and
  https://github.com/rrnewton/hermit/issues/1560.
- **What would be needed:** a prospectively sealed cell that sweeps `--sched-seed`
  together with virtual-time dilation (`--chaos-per-thread-slowdown`,
  `--chaos-slowdown-max-factor`, `--clock-multiplier`, `--target-timeslice`). These
  controls exist; see `TIME/HERMIT-TIME-KNOB-AUDIT-20260921.md`. They are the same
  kind of controls that made HTTPCoro's timing FAIL selectable (clock multiplier 0.5,
  slowdown factor 10; `K71/RESULTS.md` §3.2). Landing the virtual timerfd PR would
  probably also be needed. **MEDIUM**, UNVERIFIED.

### 1.4 LogDevice health_monitor: the "deadlock" follows a stray SIGSTKFLT (new finding)

- **Record:** `K71/services/logdevice/tests--679930c8e36e/evidence/seed-000-replica-1.stderr.txt`:

  ```
  Deadlock detected: thread(s) waiting on futex, but no runnable threads left.
    turn 39776, committed time 1_767_225_612.776_789_565s
    threads (2), by dettid:  dtid 3: FutexWait: R   dtid 79: FutexWait: R
  HERMIT_INTERNAL_FAILURE class=container-child-exit status=Exited(1)
  ```

  Exit 125 after 38 s (`seed-000-replica-1.safehermit.txt`).
- **What the retained log shows** (`brs-tfa-kernel71-logdevice-verify-2445441-1790002126713644463/run1_log_ZbCv2`,
  91,615,054 bytes, 473,737 lines):
  - At line 473541, dtid 161's `rt_sigprocmask(...)` returns `Err(Errno(ERESTARTSYS))`.
  - Lines 473542 to the end contain **96** lines of
    `ERROR reverie_ptrace::lifecycle: guest terminated by signal tid=N pid=3 signal=SIGSTKFLT core_dumped=false`
    (`grep 'terminated by signal' | grep -o 'signal=…' | uniq -c` → `96 signal=SIGSTKFLT`).
  - Each is followed by a `logically_kill`.
  - The guest made **no** `kill`, `tkill`, `tgkill` or `rt_sigqueueinfo` calls and no
    `rt_sigaction(16, …)`: the grep count for both is 0.
  - The guest had made 97 `clone3` calls, i.e. 98 threads. 96 were killed and the 2
    left (dtid 3 and 79) are exactly the "deadlocked" pair.
- **Where SIGSTKFLT comes from (PROVEN from source):** it is Reverie's PMU timer
  signal: `reverie/src/timer.rs:25` `pub const PERF_EVENT_SIGNAL: Signal = Signal::SIGSTKFLT`.
  `reverie-ptrace/src/timer.rs:572` warns "Stray SIGSTKFLT indicates a bug!". Detcore
  no-ops guest `rt_sigaction` for it (`detcore/src/syscalls/signal.rs:52,888-890`).
- **INFERENCE (strong):** a Hermit timer signal escaped to the guest with its default
  action (terminate). The group death was then recorded per thread, and two threads
  parked in Hermit futex waits were left behind. Hermit's deadlock detector then fired.
  So this is a Hermit defect, not a target deadlock. The RESULTS wording ("two-thread
  scheduler deadlock") is the downstream symptom.
- **Issues:** hermit search "SIGSTKFLT" returned none; hermit "Deadlock detected"
  returned unrelated https://github.com/rrnewton/hermit/issues/2101 and
  https://github.com/rrnewton/hermit/issues/3146; reverie "SIGSTKFLT" returned only
  https://github.com/rrnewton/reverie/issues/726 (precise-timer overshoot at host-timed
  places, the same subsystem but not this symptom). **No record of this defect.**
- **Native cause (PROVEN native symptom):** TestInfra failure trace
  `/tmp/sub-tfa2-030/ti/run-get-281475244092464.txt` line 161:
  `*** Signal 11 (SIGSEGV) … received by PID 2 … linux TID 329` in
  `facebook::logdevice::LogStoreHealthMonitor::executeDueTriggers()` (line 174), after
  "N0 is down … Handler disabled". The native screen saw the same SIGSEGV at 4.56 s and
  a traced PASS at 2.673 s with 657 TIDs, 16,739 futex calls and 493 eventfds
  (`LOGSTORE-HEALTH-NATIVE-PROFILE-20260921.md`). INFERENCE: a thread-interleaving race
  such as use-after-free or teardown. Native rate 65/96 = 67.71%.
- **Fixability: HARD.** Fixing the stray-signal and leftover-thread defect is MEDIUM
  Hermit work. Even then, 657 native threads is ptrace fanout: Hermit reached only 97
  clones in 38 s. In K613 the same target also stopped in a Hermit "deadlock" (turn
  219044, one FutexWait thread) with a different binary; that log was not retained,
  so I cannot tell whether it was the same cause
  (`K613/services/logdevice/tests--679930c8e36e/evidence/seed-000-discovery.deadlock.txt`).

### 1.5 Cachelib EventTracker

- **Record:** `K71/services/cachelib/event-tracker-test--4b6700e9a3a0/evidence/seed-000-replica-1.*`:
  stderr is just `:: Run1...`. safehermit reports
  `DEADLINE: the run reached 120 and was killed by its cgroup`, `exit_code=124`.
  Result: `status=CENSORED; selected=0; unit_result=timeout; exit=124`.
- **Retained log** (`brs-tfa-kernel71-cachelib-verify-2445388-1790002126534780112/run1_log_SIlbZ`,
  183,926,971 bytes; `run2_log_REXdZ` is 0 bytes):
  - 405,129 finished syscalls from 14:52:21 to 14:54:21, about **3,376 syscalls/s**.
  - The mix is 198,361 `clock_gettime` plus 197,894 `time`, about 98% clock reads.
    Hermit traps vDSO time calls as real syscalls.
  - Virtual time reached 1767225631.84 s, i.e. 31.8 s of guest time.
  - It was still making steady progress (last line: `time()` in dtid 3). **It was not
    hung.**
- **Test** (fbsource `EventTrackerTest.cpp:408-500`): 200,000 1 KB inserts, 200,000
  finds, 100,000 removes, then `sleep_for(500 ms)` and an assertion that the background
  event-tracker thread logged events.
- **Native** (registry 1/29 = 3.45%): recent failures are a 600 s TIMEOUT (CONTINUOUS)
  and FATAL UBSan `dto-cachelib/dto.c:1688:13: runtime error: division by zero` in DIFF
  runs (`/tmp/sub-tfa2-030/ti/562950242844554.json`). INFERENCE: timing / slow-run
  sensitivity (a "sleep then check" pattern and slow-host timeouts).
- **Fixability:** reaching an oracle line is plausibly **EASY** by raising the 120 s
  deadline (UNVERIFIED). INFERENCE: if finds and removes cost about as much as inserts,
  one replica needs roughly 5–7 min and the two-replica `--verify` roughly 10–14 min.
  Getting a FAIL is **UNKNOWN**; under virtual time the 500 ms sleep is deterministic,
  so a starvation knob would be needed. Overall **UNKNOWN**; the slow vDSO path is
  real Hermit performance work (MEDIUM). No issue found (searches
  "vdso clock_gettime overhead", "ptrace overhead threads").

---

## 2. The 20 unattempted registry targets

**Sources:** `K71/NATIVE-CANDIDATE-SCREEN-20260921.md` (per-candidate table and the
closing paragraph) and `K71/TERMINAL-AND-CANDIDATE-GAP-AUDIT-20260921.md` ("Candidate-gap
audit"). The gate required ≤10 s native wall, demonstrated competing-thread ordering,
and a thread footprint small enough for ptrace. Native rates are from
`ai_docs/tfa-candidate-sourcing-2026-09-17.md`.

| Category | Target (selected case) | Screen / audit evidence |
|---|---|---|
| **Too slow natively / setup exceeds gate** (10) | ZippyDB `vrt_usage_tracker_test` (BumpWithIOTagTest) | "emitted target-owned counter failure, then remained in retry/sleep past 13 s" |
| | Mcrouter `mux_io_thread_pool_executor_toaster_test` | "selected suite setup exceeded 13 s and launched multiple ucache processes" (also a multiprocess **service harness**) |
| | LogDevice `fb:tests` SslAuthentication | "entered selected case, no completion by 13 s" |
| | Warm Storage `file_service_tests` FileCache | "no GoogleTest marker by 13 s" |
| | Warm Storage `bc_vnext_create_reader_retry_tests` | "entered selected future-retry path, no completion by 13 s" |
| | Manifold `crypto_plugin_integration_test` | "selected case skipped because `CRYPTO_PLUGIN_SO_PATH` was unset; 5.28 s wall and about 2.28 GiB RSS" (target not executed; **environment**) |
| | Manifold `manifold_client_test` payload-timeout | "entered selected case, no completion by 13 s" |
| | Warm Storage `write_time_encoding_selection_test` | "onebox setup/backoff exceeded 13 s" (**service/environment harness**) |
| | ZippyDB `admin_handler_e2e_test` | "no GoogleTest marker by 13 s" (e2e **service harness**) |
| | Manifold Rust `manifold_client_tests` | "entered its exact one-test invocation, no completion by 13 s"; libtest `--test-threads=1`. In K613 it also hit the loader defect (§1.1) |
| **Too many threads for ptrace** (3) | Mcrouter `shadow_thread_affinity_integration_test` | 338 TIDs; strace timed out at 30 s after 337 clones and 24,797 futexes; native 11.22 s wall |
| | Thrift `coroutine_test` | 331 TIDs, 330 clones, 5,210 futexes, 321 eventfds |
| | Thrift `coroutine_test_resource_pools` | "uses the same high-fanout server scaffold" (screen closing paragraph) |
| **Not runnable locally** (5) | Cachelib Cogwheel free-list stress, and its sanitizer variant | "canonical test targets have no local executable output; only `.par` launcher output exists"; the audit counts "two launcher-only Cogwheel targets" |
| | MySQL `mtr_parallel_main_1`, `mtr_parallel_rocksdb_stress`, `mtr_parallel_main_no_tp_mutex_1` | "requires the cohort's AArch64 MTR route" (all standalone commands use `-c 'fbcode.arch=aarch64'`) |
| **Hermit intentionally unsupported** (1) | Folly `io_uring_dynamic_provided_buffer_ring_test` | "io_uring is explicitly excluded because Hermit intentionally returns ENOSYS". Source: `4630ee2a:detcore/src/syscall_classification.rs:400` ("existing io_uring ENOSYS refusal"). K613 attempt stopped earlier on the since-fixed RLIMIT_CORE abort |
| **Other: prior Hermit failure route** (1) | ZippyDB `DRocksDBTest` | "previously filled a 1 GiB retained log while deadlocked". K613 record: first replica "deadlocked with one FutexWait thread", `container-child-exit`, exit 125, retained log truncated at 1,073,742,000 bytes (`K613/services/zippydb/drocksdbtest--af5789c4e317/evidence/seed-000-discovery.deadlock.txt`) |

The counts add up (10 + 3 + 5 + 1 + 1 = 20). They are consistent with the audit's
all-30 partition: 13 runtime/setup, of which 3 were attempted (McRefill, EventTracker,
LogStore retry); 5 fanout, of which 2 were attempted (LogStore health, Thrift
Instrumentation); 7 incompatible routes; and 3 scheduler-insensitive or exhausted.
Overlaps: MuxIO, Warm Storage write-time, ZippyDB admin and Manifold crypto are also
service/environment harnesses. Separately, K613 recorded a ZippyDB VRT attempt whose
two strict replicas diverged in stdout and INFO (`K613/services/zippydb/vrt-usage-tracker-test--5a123a3ebda4/`),
which is a determinism gap with an older binary.

---

## 3. Bucketing all 30 targets by likely flakiness cause

Evidence per row: native text is from TestInfra (`/tmp/sub-tfa2-030/ti/`, last week)
unless marked otherwise. The bucket is **INFERENCE** unless it says "PROVEN native
symptom".

| # | Target (case) | Native evidence | Likely cause bucket | Hermit outcome |
|---|---|---|---|---|
| 1 | Folly `async_ssl_socket_test` ObserverAttachedBeforeConnect (37/96) | `AsyncSSLSocketTest.cpp:3797 … byteEvents Expected: has a size that is >= 4` | Thread/async-I/O ordering race (RESULTS hypothesis: byte-event vs ACK order) | **Both seeds** (WORKING) |
| 2 | Thrift `InstrumentationTest` snapshotStartedProcessingTest (0/29; 1 DIFF failure last week) | `InstrumentationTest.cpp:836 … reqSnapshots.size() Which is: 2` (vs 3) | Thread-interleaving race (callback/baton snapshot) | **Both seeds** (WORKING) |
| 3 | Proxygen `HTTPCoroSessionTests` ResetRateLimit/h2 (1/30) | `HTTPDownstreamCoroSessionTests.cpp:1685] Check failed: bodyEvent.hasException()` | Timer / virtual-time progression (100 ms rate-limiter reset; single-threaded per RESULTS §4.3) | **Both seeds** (WORKING) |
| 4 | Folly `timerfd_timeout_manager_test` ReschedTest (28/93) | `:307 t2.timestamps.size() Which is: 0` | Timer / wall-clock sensitivity | Ran, no failure found (9/9 PASS) |
| 5 | Cachelib `event-tracker-test` NvmCacheWithEventTracker (1/29) | 600 s TIMEOUT; UBSan div-by-zero (DIFF) | Timeout / slow-run ("sleep 500 ms then check"; heavy compute) | Too slow (120 s deadline) |
| 6 | LogDevice `log_store_test` CondWrite…Retries (3/32) | No failure last week; retry with 1–10 ms backoff in a 100 ms timeout | Timer/timeout (INFERENCE from source) | **Blocked by Hermit defect** (loader EFAULT) |
| 7 | LogDevice `health_monitor:tests` DisabledRebuilding (65/96) | PROVEN native symptom: SIGSEGV in `LogStoreHealthMonitor::executeDueTriggers()` | Thread-interleaving race | **Blocked by Hermit defect** (stray SIGSTKFLT) plus too many threads |
| 8 | Mcrouter `mc_refill_route_wrapper_test` refillOnMiss (77/97) | `mc_res_timeout … TTransportException: Timed out`; 600 s TIMEOUTs | Timeout in a multiprocess service harness | **Blocked by Hermit defect** (loader EFAULT); then too slow |
| 9 | Proxygen `proxygen_transport_monitor_test` (2/30) | No failure last week; synchronous body | Unknown (not interleaving; possibly randomness) | **Blocked by Hermit defect** (loader EFAULT) |
| 10 | Proxygen `tls_terminator_test` h1_innerTls_http_connect (23/96) | `std::runtime_error what(): Closed.` at `TlsTerminatorTest.cpp:308` | Thread/async-I/O race between client and server threads | Blocked by config/environment plus Hermit determinism gaps. Failure not predeclared (cert). No PASS |
| 11 | Mcrouter shadow thread affinity (68/98) | `latch.try_wait_for(std::chrono::seconds(60))` false | Timeout/hang under thread-affinity contention | Not attempted (too many threads, 11 s) |
| 12 | Mcrouter MuxIO toaster (50/88) | `McToasterUtils.h:161` result 3 vs 13, `reply.value_ref().has_value()` false | Timeout in a service harness (13 = timeout by analogy with McRefill's `0x0D` = `mc_res_timeout`) | Not attempted (slow, multiprocess) |
| 13 | LogDevice SslAuthentication (33/94) | Traces uninformative; one failure took 243 s against a "timeout 240 s" monitor | Timeout (weak INFERENCE) / unknown | Not attempted (slow) |
| 14 | Thrift `coroutine_test` SumThrowsUserExPrimitive (0/29) | FATAL: `WARNING: ThreadSanitizer: data race` in `async_tm_computeSumThrowsUserExPrimitive` after `[ OK ]` | Thread-interleaving race (TSan) | Not attempted (331 TIDs) |
| 15 | Thrift `coroutine_test_resource_pools` SumThrows (0/22) | No failures last week | Unknown (same scaffold as #14, so possibly a TSan race) | Not attempted (fanout) |
| 16 | Warm Storage FileCache getFilesFromNVM (90/96) | `RegionManager.cpp:718] Check failed: addr.offset() + size <= region.getLastEntryEndOffset()` on many TIDs, SIGABRT | Thread-interleaving race (concurrent region writes) | Not attempted (slow) |
| 17 | Warm Storage CreateReaderRetry (60/93) | `Expected: (elapsed.count()) < (10000), actual: 11351 vs 10000` | Wall-clock timing | Not attempted (slow) |
| 18 | Warm Storage write-time encoding (88/100) | FATAL after 700–1,700 s in onebox environment | External service/environment | Not attempted (onebox setup) |
| 19 | ZippyDB VRT usage (72/75) | `waitForCounter … FCI counter settled at 5.47…, expected 20`; `existsInMap(now, key)` false | Timer/wall-clock (counter windows) | Not attempted (slow); K613 attempt nondeterministic |
| 20 | ZippyDB DRocksDB DestroySucceeds… (90/97) | `DRocksDBTest.cpp:260 destroyDB(uri).ok()` / `:256 openDb` false | External storage backend/resource (weak INFERENCE) | Not attempted (K613 Hermit deadlock, 1 GiB log) |
| 21 | ZippyDB admin handler e2e (38/47) | `AdminHandlerE2ETest.cpp:69` equality, value 0 | Service/environment (e2e) | Not attempted (slow) |
| 22 | Manifold crypto plugin (72/97) | 600 s TIMEOUTs; locally skipped (`CRYPTO_PLUGIN_SO_PATH` unset) | Environment/resource | Not attempted (not executed) |
| 23 | Manifold payload-timeout client (39/84) | `StorageException [504] … HTTP 503 … WSE_CLUSTER_QUOTA_EXCEEDED … Write timed out` | External service | Not attempted (slow) |
| 24 | Manifold Rust client (7/96) | `ManifoldError … [400] Requested object expiration is invalid. Requested: 1790628862 -- Allowed range: [1790628864-…]` | External service plus client/server wall-clock skew | Not attempted (slow); K613 loader failure |
| 25 | Cachelib Cogwheel free-list stress (7/26) | CONTINUOUS failures at ~3,615 s through a remote-Tupperware launcher | Stress/harness; unknown | Not attempted (launcher-only) |
| 26 | Cachelib Cogwheel sanitizer free-list stress (21/60) | Same, ~3,636 s | Stress/harness; unknown | Not attempted (launcher-only) |
| 27 | MySQL main.explain_for_connection (79/96) | `Error running test suite` (MTR) | Harness/unknown | Not attempted (AArch64) |
| 28 | MySQL rocksdb_ha_consistent_snapshot_stress (82/95) | `Error running test suite` (MTR) | Harness/unknown (stress) | Not attempted (AArch64) |
| 29 | MySQL explain (no TP mutex) (21/50) | `Error running test suite` (MTR) | Harness/unknown | Not attempted (AArch64) |
| 30 | Folly io_uring BufferCountCheck (22/100) | `LibUringCallError what(): unable to register provided buffer ring` | Kernel feature/resource | Not attempted (Hermit ENOSYS) |

**Cross-tab (target counts):**

| Cause bucket ↓ / Hermit outcome → | Both seeds | Blocked by Hermit defect | Too slow / too many threads | Not attempted | Ran, no failure | Failure not predeclared | Row total |
|---|---:|---:|---:|---:|---:|---:|---:|
| Thread-interleaving / async-ordering race | 2 (AsyncSSL, Thrift Instr) | 1 (health monitor) | 0 | 3 (Thrift coroutine, WS FileCache, *Thrift resource-pools†*) | 0 | 1 (TLS) | 7 |
| Timer/timeout or wall-clock | 1 (HTTPCoro) | 2 (log_store, McRefill) | 1 (EventTracker) | 6 (Shadow affinity, MuxIO, LogDevice SSL, WS CreateReaderRetry, ZippyDB VRT, *Manifold Rust‡*) | 1 (TimerFD) | 0 | 11 |
| External service / environment / resource | 0 | 0 | 0 | 5 (WS write-time, ZippyDB admin, DRocksDB, Manifold crypto, Manifold payload) | 0 | 0 | 5 |
| Kernel feature | 0 | 0 | 0 | 1 (io_uring) | 0 | 0 | 1 |
| Unknown / harness-only evidence | 0 | 1 (transport monitor) | 0 | 5 (2 Cogwheel, 3 MySQL) | 0 | 0 | 6 |
| **Column total** | **3** | **4** | **1** | **20** | **1** | **1** | **30** |

Notes on the cross-tab:

- † Thrift resource-pools has no failure text; I placed it by analogy with #14. It
  could equally be "unknown".
- ‡ Manifold Rust is both external-service and wall-clock (expiration vs server time);
  I placed it under timer. Moving it to external service makes that row 6 and the
  timer row 10.
- TLS is counted once, under "failure not predeclared". It also sits in "blocked by
  config/Hermit gaps": the epoch default, teardown-order divergence and loopback-epoll
  divergence.
- The health monitor is counted as "blocked by Hermit defect", but it would also be
  "too many threads" (657 TIDs natively).
- McRefill would become "too slow" after a loader fix.
- **Headline, INFERENCE:** of the 10 attempted targets, the 3 successes are 2
  interleaving races and 1 timer case. Of the 7 failures, 4 stopped on Hermit
  defects: 3 on one loader bug and 1 on a stray timer signal. Of the unattempted 20,
  about half are timer or timeout flakes, which are the cases where Hermit's
  virtual-time controls (slowdown, clock multiplier) look most promising once runtime
  permits.

---

## 4. Hermit defects and gaps this campaign exposed, with status on main

1. **Injected-fstat scratch write faults below the guest stack. The guest's `openat`
   returns `EFAULT` and the real fd leaks.**
   - Scope: all 3 loader stops (log_store, McRefill, transport monitor), the
     `tfa-openat-stack-size` reproducer, and probably K613 Manifold Rust.
   - Status: **not fixed on main.** `inject_fstat` is unchanged (main
     `files.rs:747-777`), as is Reverie `GuestStack::commit` (pin 6297f715,
     `stack.rs:160-171`).
   - Issue: **none found** (searches listed in §1.1).
   - Easy workaround to test: `--no-virtualize-metadata`. Proper fix: a scratch-mmap
     fallback or host-side fstat, plus closing the fd on failure.
2. **Fixed default virtual epoch 2026-01-01 made time-validated certificates "not yet
   valid"** (TLS cell 1, 15/15).
   - Status: **fixed on main** by `402ba9737` ("Capture one reproducible virtual epoch
     per execution", 2026-09-22, follow-up `173929ab3`
     https://github.com/rrnewton/hermit/pull/3210). `--epoch` was always available.
   - A separate open issue, https://github.com/rrnewton/hermit/issues/3304 (epoch
     truncated to µs), is tangential.
3. **Thread-teardown `logically_kill` records are emitted in host order after a
   signal/abort group death, so strict verify reports a false divergence.**
   - Seen in TLS 13/15 (cell 1; the first and only divergence) and 16/16 (cell 2;
     present in all 16, and the sole divergence in seeds 6 and 14), and in
     several AsyncSSL continuation tuples (for example
     `experiments/tfa-kernel71-asyncssl-slice20-continuation_20260921/…/selection-004-…-replica-1.stderr.txt`).
   - Status: **open.** The closest record is https://github.com/rrnewton/hermit/issues/3247
     (exec-time variant, filed 2026-09-27). No issue for the abort/signal-death
     variant (search "logically_kill").
4. **Readiness of guest-internal sockets (in-process loopback TCP) is host-timed, so
   `epoll_wait` results and virtual timestamps diverge between replicas.**
   - Seen as the first divergence in TLS cell 2 (fd 7 = accepted in-process IPv6
     loopback connection), and as first divergences at `epoll_wait` (AsyncSSL factor5
     seed 8, slice10 seeds 3 and 4) or `epoll_ctl` (AsyncSSL factor5 seed 13, slice50
     seed 2). Those contexts come from the `Divergent syscall context` blocks in each
     `selection-0NN-…-replica-1.stderr.txt`. That the AsyncSSL cases share the
     loopback cause is INFERENCE.
   - Status: **no record found** (searches "loopback", "loopback TCP epoll", "socket
     epoll nondeterministic").
5. **Stray Reverie PMU-timer signal (SIGSTKFLT) terminated 96 of 98 guest threads.
   The two survivors stayed parked, triggering "Deadlock detected" and
   `container-child-exit`.** Seen in the health monitor, retained log
   `run1_log_ZbCv2` lines 473541-473737.
   - Status: **no record found** (hermit "SIGSTKFLT": 0 hits; reverie: only
     https://github.com/rrnewton/reverie/issues/726, the same subsystem but a
     different symptom; hermit "Deadlock detected": unrelated).
6. **ptrace throughput on clock-heavy code: vDSO time reads are trapped** (EventTracker:
   about 3,376 syscalls/s, 98% `time`/`clock_gettime`, 120 s deadline hit while
   progressing).
   - Status: gap, **no record found** (searches "vdso clock_gettime overhead", "ptrace
     overhead threads").
   - High thread counts (health monitor 657, Shadow 338, Thrift 331 TIDs) are the same
     class of backend limit. The roadmap's non-ptrace production backend is the
     structural answer.
7. **timerfd expiry is host-timed, not virtual** (relevant to TimerFD's inability to
   FAIL).
   - Status: **open PR** https://github.com/rrnewton/hermit/pull/3229 ("Drive timerfd
     readiness from the virtual clock", opened 2026-09-26).
   - Open issues: https://github.com/rrnewton/hermit/issues/1923,
     https://github.com/rrnewton/hermit/issues/1930,
     https://github.com/rrnewton/hermit/issues/1560.
8. **PMU skid-overshoot policy refusal (exit 122).** One Thrift time-input tuple:
   `TIME/services/thrift/instrumentationtest--3e07b8fc0ca0/evidence/selection-016-…-replica-1.stderr.txt`,
   `HERMIT_POLICY_REFUSAL class=policy-refusal cause=skid-overshoot count=2`. Also the
   K613 TLS seed 0.
   - Status: **open issue** https://github.com/rrnewton/hermit/issues/1845 (skid-gated
     retry).
9. **Small virtual-clock divergences between replicas** (INFERENCE: timer-placement
   nondeterminism). Examples: `clock_gettime` values differing by 12 ns in
   `experiments/tfa-kernel71-sched-continuation_20260921/…/selection-001-…-replica-1.stderr.txt`,
   and a `clock_gettime` first divergence in HTTPCoro midpoint seed 1.
   - Status: related open Reverie issue https://github.com/rrnewton/reverie/issues/726.
     Exact match not established.
10. **RLIMIT_CORE refusal aborted ASan/TSan guests before the test** (K613: EventTracker,
    AsyncSSL, TimerFD, Thrift Instrumentation, io_uring).
    - Status: **fixed**, `ccbe37e91` "Accept virtual RLIMIT_CORE updates"
      https://github.com/rrnewton/hermit/pull/3142. This fix is included in the
      kernel-7.1 campaign binary, which is why those targets reached their tests there.
11. **Configuration or policy, not defects:**
    - `io_uring` returns ENOSYS by design (`detcore/src/syscall_classification.rs:400`).
    - Default `--network=local` makes host services (SMC, Falcon, configerator)
      unreachable (TLS).
    - `--verify` timeouts are wall-clock bounded by the 120 s safehermit deadline chosen
      by the campaign.

---

## Searches run (GitHub via herdr-run, one command each; outputs in `/tmp/sub-tfa2-030/gh-fixability-N.json`)

| N | Query | Relevant hits |
|---:|---|---|
| 1 | issues `inject_fstat` | https://github.com/rrnewton/hermit/issues/1850 (unrelated) |
| 2 | issues `openat EFAULT` | none |
| 3 | issues `EFAULT stack` | none |
| 4 | issues `TFA` | https://github.com/rrnewton/hermit/issues/1845 (skid) |
| 5 | issues `flaky` | https://github.com/rrnewton/hermit/issues/3265 (CPU-0 single-step), others CI-related |
| 6 | issues `epoch` | https://github.com/rrnewton/hermit/issues/3304, https://github.com/rrnewton/hermit/issues/1923, https://github.com/rrnewton/hermit/issues/1560 |
| 7 | issues `loader` | none relevant (sabre loader issues) |
| 8 | issues `scheduler deadlock` | https://github.com/rrnewton/hermit/issues/822, https://github.com/rrnewton/hermit/issues/1850 (unrelated) |
| 9 | issues `red zone` | none |
| 10 | issues `fstat` | https://github.com/rrnewton/hermit/issues/3306, https://github.com/rrnewton/hermit/issues/3307 (unrelated inode) |
| 11 | issues `stack expansion` | none |
| 12 | PRs `inject_fstat` | https://github.com/rrnewton/hermit/pull/1851 (unrelated) |
| 13 | issues `certificate` | none relevant |
| 14 | issues `timerfd` | https://github.com/rrnewton/hermit/issues/1930, https://github.com/rrnewton/hermit/issues/1923, https://github.com/rrnewton/hermit/issues/1560 |
| 15 | issues `Deadlock detected` | https://github.com/rrnewton/hermit/issues/3146, https://github.com/rrnewton/hermit/issues/2101 (unrelated) |
| 16 | issues `SIGSTKFLT` (hermit) | none |
| 17 | issues `SIGSTKFLT` (reverie) | https://github.com/rrnewton/reverie/issues/726 |
| 18 | issues `logically_kill` | https://github.com/rrnewton/hermit/issues/3247 |
| 19 | view issue 3247 | exec-time teardown ordering, open |
| 20 | PRs `EFAULT` | https://github.com/rrnewton/hermit/pull/3229 (timerfd) among unrelated |
| 21 | view PR 3229 | open; timerfd armed a real host timer |
| 22–26 | issues `loopback TCP epoll`, `loopback`, `socket epoll nondeterministic`, `vdso clock_gettime overhead`, `ptrace overhead threads` | none relevant |
