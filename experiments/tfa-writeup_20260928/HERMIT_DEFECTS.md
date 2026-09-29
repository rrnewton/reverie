# TFA experiment: Hermit defects, discarded trials and cost

Written 2026-09-28 by sub-tfa2-030. Companion to [README.md](README.md): this file holds the detail behind
README section 3.4 (discarded trials and Hermit defects). Nothing was re-run to
write it. Bracketed keys such as [CAMP §5] are the source keys listed in [SOURCES.md](SOURCES.md);
copies of the non-repository sources are in [evidence/](evidence/) (mapping in
[evidence/MANIFEST.md](evidence/MANIFEST.md)). "(inference)" marks a statement the records do not prove.

Terms: a **trial** is one seed tried once under strict verify, which runs the test twice with that seed
and requires the two runs' detailed logs to match exactly. A trial is **discarded** (the records say
"censored") when it ends in anything other than a clean PASS or the failure declared in advance.

## 1. Defects and gaps

Status is as of the local ref `origin/main` `fdd8f55a8e7c` in the `hermit` checkout
(2026-09-28 13:07 -0700; a fresh fetch failed) [FIX, conventions list]. The final completeness
pass re-read the later local `origin/main` `ad21724d5f64` (2026-09-28 16:44 -0700): its 7 newer
commits change only CI and parity tooling (`ci/` and two `.github/workflows/` files), so no status
below changed.

| # | Defect or gap | Effect in this experiment | Evidence | Status on Hermit main | GitHub record |
|---:|---|---|---|---|---|
| 1 | Hermit's post-open metadata fetch (`inject_fstat`) turns a successful `openat` into EFAULT, and the opened file descriptor leaks (proven). The likely cause is that its scratch write at [rsp-272, rsp-128) falls below the mapped guest stack (hypothesis: the logs record neither `rsp` nor which step failed) | Stopped 3 targets in the loader: LogStore, McRefill, transport monitor | Same log sequence at guest address `0x7ffffffde180` in all three [LOGS]. Hermit was fetching metadata for the newly opened fd 3; `add_fd` runs only after the real `openat` returned a descriptor (`detcore/src/syscalls/files.rs:794-804` at `4630ee2a`), so the library existed and Hermit turned a successful open into EFAULT. The record's own diagnosis calls the scratch-write cause "a strong causal hypothesis" (`PROXYGEN-MONITOR-NATIVE-AND-LOADER-DIAGNOSIS-20260921.md` lines 80-91 in the transport-monitor record). Reproducer fails with 192 writable bytes and succeeds with 512 [PROBE]; it forces the fault with a `PROT_NONE` page rather than the unmapped region below the stack, and it ran on the pilot binary `79da5231`, whose `detcore/src/syscalls/files.rs` is identical to `4630ee2a`'s | **Not fixed on main** as of `fdd8f55a8e7c`: `inject_fstat` unchanged (`detcore/src/syscalls/files.rs:747-777`), as is Reverie `GuestStack::commit` (pin `6297f715`). A fix is approved but not landed as of 2026-09-29: https://github.com/rrnewton/hermit/pull/3374 (see [ROUND2.md](ROUND2.md) section 2) | Filed 2026-09-28: https://github.com/rrnewton/hermit/issues/3328 (earlier searches for inject_fstat, openat EFAULT, EFAULT stack, red zone and stack expansion found nothing) |
| 2 | A stray SIGSTKFLT (Reverie's PMU timer signal, `reverie/src/timer.rs:25`) reaches the guest | Killed 96 of 98 threads of the health monitor; Hermit then reported "Deadlock detected". The guest sent no signals, so Hermit's own timer signal escaped (strong inference) | Retained log [LOGS]. A second occurrence: on 2026-09-18 the TLS target under `--clock-multiplier=0.1` (pilot binary) ended with exit 144, signal 16 (SIGSTKFLT) and no test verdict (TaskGraph task `tfa-diagnose-futuretimeout-strict-hermit`, note of 2026-09-18 19:52; TaskGraph is not a durable record, so the `tg -v` output is copied to [evidence/verify/tg-futuretimeout-v.txt](evidence/verify/tg-futuretimeout-v.txt), line 46) | Not investigated; no fix known | Same symptom as open https://github.com/rrnewton/reverie/issues/747 (filed 2026-09-29 00:01Z from a LiteInst probe); this campaign's evidence was added as https://github.com/rrnewton/reverie/issues/747#issuecomment-5881331809. Related: https://github.com/rrnewton/reverie/issues/726 |
| 3 | Thread-teardown events after an abort are logged in host order | 14 of the 44 run-to-run differences (TLS) | First difference at `logically_kill` after `tgkill(..., 6)` [CAMP §5] | Open | Same mechanism as https://github.com/rrnewton/hermit/issues/3247 (exec-time variant); abort-time variant added as https://github.com/rrnewton/hermit/issues/3247#issuecomment-5881331489 |
| 4 | Readiness of in-process loopback sockets follows host timing | 15 of the 44 first differences, at epoll calls (TLS, AsyncSSL) | Divergent-syscall context in tracked stderr [FIX §4]; shown for TLS, inferred for AsyncSSL | Open | Filed 2026-09-28: https://github.com/rrnewton/hermit/issues/3330 |
| 5 | The virtual clock differs by a few nanoseconds between the two runs under per-thread slowdown | 10 of the 44, all in slowdown configurations | e.g. 388175690 vs 388175685 ns, then `recvmsg` EAGAIN vs 0 [CAMP §5] | Open | Filed 2026-09-28: https://github.com/rrnewton/hermit/issues/3329 |
| 6 | Fixed default guest date of 2026-01-01 | TLS "certificate not yet valid" in 15 of 15 trials without a real date; 0 of 16 with one | [FIX §1.2] | **Fixed** by `402ba9737` (2026-09-22), which captures the date once per run | https://github.com/rrnewton/hermit/pull/3151 (closed; integrated by `402ba9737`), follow-up https://github.com/rrnewton/hermit/pull/3210 |
| 7 | RLIMIT_CORE refusal aborted sanitizer builds | Stopped 5 of 10 pilot targets | [K613] | **Fixed** by `ccbe37e91` | https://github.com/rrnewton/hermit/pull/3142 |
| 8 | PMU timer overshoot refusal | 1 Thrift trial; the pilot TLS trial | `HERMIT_POLICY_REFUSAL cause=skid-overshoot` | Open | https://github.com/rrnewton/hermit/issues/1845 |
| 9 | timerfd is armed with a real host timer | Probably why TimerFD never failed (inference) | PR description | Open PR | https://github.com/rrnewton/hermit/pull/3229; https://github.com/rrnewton/hermit/issues/1923, https://github.com/rrnewton/hermit/issues/1930, https://github.com/rrnewton/hermit/issues/1560 |
| 10 | ptrace throughput: vDSO clock reads are trapped, and high thread counts are slow | EventTracker timeout; 3 targets excluded for thread count (338 and 331 measured, the third by analogy); health monitor has 657 | [LOGS]; [SCR] | Structural; the roadmap's non-ptrace backend is the answer | None found |

Every defect except 10 (structural) now has a GitHub record (section 1 table, last column). Defect 1 is the cheapest to act on: it blocks 3
targets, logs and source prove that `inject_fstat` turns successful opens into EFAULT
(where inside it the fault occurs is a hypothesis), and the 192/512-byte probe is a
ready-made regression test [PROBE; FIX §1.1].

Filing needed no owner decision: project policy (dev-hermit `AGENTS.md`: real product defects
always go to a GitHub issue in the repository that owns the code) requires it. sub-tfa2-030 filed
or extended the records on 2026-09-28 and read each body back byte-identical; the list is in
[ROUND2.md](ROUND2.md) and [README.md](README.md) section 3.4.

## 2. Why trials were discarded

| Cause | Trials | Whose problem |
|---|---:|---|
| The two runs of one seed differed | 44 (TLS 29, AsyncSSL 14, HTTPCoro 1) | Hermit determinism gaps |
| A real test failure other than the declared one | 42 (AsyncSSL line 3592: 24; HTTPCoro line 1685 with a mis-written pattern: 16; TLS "certificate not yet valid": 2) | Test, or the declared pattern; the 2 TLS aborts come from Hermit's fixed default date (strong inference) |
| Loader `openat` returned EFAULT after an injected `fstat` | 3 | Hermit defect |
| Stray Hermit timer signal, then a "deadlock" report | 1 | Hermit defect |
| PMU timer overshoot refusal | 1 | Hermit limitation |
| Outer 120 s timeout | 2 | Speed of Hermit's ptrace path |
| **Total** | **93** | |

Sources: each discarded trial's tracked stderr, classified in [CAMP §5] and [CONT §1]. About
half the discarded trials (49 of 93: the first row plus the three Hermit rows) were stopped by
Hermit itself. The 2 TLS certificate aborts trace to Hermit's old fixed default date (strong
inference), which makes 51, and the 2 outer timeouts (EventTracker, and HTTPCoro seed 2 at
121 s) came from Hermit's slow ptrace path, which makes 53. For the 44 differing
trials, the first difference was at thread teardown after an abort (14), at an epoll call
(15), at a clock read (10, all in slowdown configurations; one example differs by 5 ns), at
a `read` (1), or only in stderr text (4) [CAMP §5].

## 3. Cost and slowdown

The 294 trials took 4,562 s of Hermit time (about 76 min): median 5 s, range 3-121 s
[CONT §3]. The calendar span was 8 h 22 min (14:27-22:49 UTC); the rest went to planning,
committing configurations and review. A trial is two runs in sequence, so one run is
roughly half a trial. On that basis (inference from [CONT §3]):

- AsyncSSL: one Hermit run (about 2 s) was about 1.5 times its one ordinary native run
  (1.29 s outer wall, a native FAIL). Against a traced native PASS run with leak checking
  off (0.21 s) it is about 10 times, but the Hermit cells kept the default sanitizer
  settings (guest environment `LC_ALL` and `TZ` only), so that baseline is not like for like
  (`experiments/tfa-kernel71-sched-continuation_20260921/services/folly/async-ssl-socket-test--cefc173a09b4/ASYNCSSL-SCHED-SLOWDOWN-PLAN-20260921.md`
  lines 8-15).
- Thrift: 13 times (7.5 s against 0.55-0.59 s on one CPU).
- HTTPCoro passes: 5-7 times (3.5 s against 0.48-0.69 s).
- HTTPCoro abort path: 75-110 s per trial, about 38-55 s per run. Natively the same
  line-1685 abort took 7.6 and 9.6 s in CI, most of it after the abort, while the stack is
  symbolized [TI 562950215319283]; its 200 most recent passing CI results took 0.29-0.87 s,
  median 0.45 s (`meta testinfra.result history --test=562950215319283`, saved by the verify
  pass as `/tmp/sub-tfa2-030/verify/ti-hist-562950215319283.json`; copy
  [evidence/verify/ti-hist-562950215319283.json](evidence/verify/ti-hist-562950215319283.json)). So the abort path is
  roughly 4-7 times native, like the passes. Its failing seed took 110 s against the 120 s
  limit.
