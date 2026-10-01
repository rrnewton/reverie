<!-- Saved by the coordinator lane (claude-coord-mega-lander) at 2026-10-01T13:08Z from the attribution worker's returned text. The worker's own write of this file was refused by the harness. The coordinator re-ran recount.awk over /tmp/cml-attr749-1227/runs/*.err at 13:07Z: A markers=3840 inflight_ge1=147 inflight_ge2=0 max_excess=+7; B 3840/123/0/+7; C 3840/6/0/+7. Files containing "test result: ok. 1 passed; 0 failed": A 40, B 40, C 40. -->

# Attribution run: reverie PR 749, CapacityTool::on_exit_thread, test (2)

Measured 2026-10-01, 12:29Z-12:59Z, on a shared 316-CPU host. Measurement only: nothing was committed, pushed or posted.

## Summary (from the worker's hand-back)

| Variant | Runs | Passes | Markers | Markers with a child in flight | Largest excess |
|---|---|---|---|---|---|
| A (head 410312f4) | 40 | 40 | 3840 | 147 (38 of 40 runs) | +7 |
| B (head minus override) | 40 | 40 | 3840 | 123 (39 of 40 runs) | +7 |
| C (main dbf2b5c8) | 40 | 40 | 3840 | 6 (6 of 40 runs) | +7 |

- **Runs:** all 120 exited with rc 0, passed and parsed. The baseline was 18 fds in every run. No marker had 2 or more children in flight. A marker had a child in flight exactly when its excess was +5 or more, in 3840 of 3840 markers for each variant.
- **Default hook:** the default `Tool::on_exit_thread` only returns `Ok(())` (reverie/src/tool.rs:682-690, the same file at both revisions).
- **Is the hook called on every exit?** Yes, in all three variants. reverie-ptrace calls the tool's `on_exit_thread` unconditionally on every ordinary thread exit (task.rs:7949 head / 7939 main), after `retire_ordinary_terminal` and before `session.finished`. A runs the override, which takes its no-op branch in this test (held_hook=false in every A run); B and C run the default.
- **Builds:** all rc 0, taking 115.6 s, 20.8 s and 49.0 s, with 0 rustc warnings. The three sha256 values differ.
- **Disk:** target/ is removed; bin/ and runs/ are kept. Final `df` avail on /data: 604,161,966,080 bytes at 12:59:24Z.

## Background and terms
- **Pull request:** https://github.com/rrnewton/reverie/pull/749 changes two things:
  - the safeptrace notifier's retirement order (product code);
  - the reverie-ptrace fd-capacity tests (test code).
- **Test (2):** `tracer::tests::fatal_capacity_tests::retired_threads_release_descriptors_under_reduced_nofile`.
  - The outer test re-executes the binary for itself with a soft RLIMIT_NOFILE of 128.
  - The guest runs 96 iterations: spawn a thread, join it, call getpgid(iteration).
  - The tool's getpgid handler yields once, counts the tracer's /proc/self/fd, and prints one `capacity marker:` line.
- **Marker:** one such line. A complete run has 96.
- **In flight at marker i:** (i+1) minus the number of children whose tracer task has reached `FatalSession::finished` (the FINISHED record).
  - A and B print a range a..b, so finished = b.
  - C prints the whole list, so finished = len(list).
- **Baseline:** iteration 0's fds, the test's own `captured[0].1`. **Excess** = marker fds minus baseline.
- **Variants:**
  - A = head, unchanged.
  - B = head with only `async fn on_exit_thread` deleted (fatal_capacity_tests.rs 231-268).
  - C = main, unchanged.

## 1. Revisions
- **A:** 410312f4f7faa3bf3c6e329be6acfedbd5c7df85, tree da21a2c36c30ec59c861c10736aa0f6fe75abe58. Clean (porcelain 0 lines).
- **B:** same commit, with 1 modified path (` M reverie-ptrace/src/tracer/fatal_capacity_tests.rs`). The blob goes from 8b51c217 to eff08cec, a 38-line deletion (B-deletion.diff).
- **C:** dbf2b5c8880bac5fd6a06fa1f296b3eec3590569, tree da8092cf5972faefdea783b1dea7a96f544540c6. Clean.
- **main..head:** 9 commits (26d4c8a8, 79860dc7, 0fab06b6, 84823a82, 3995611a, 96ffe60d, a3b27a40, 7b5d87cc, 410312f4). 6 files, +868/-17:
  - task.rs +10
  - tracer.rs +5
  - fatal_capacity_tests.rs +385
  - safeptrace completion_wakeup_tests.rs +120
  - notifier.rs +179
  - retirement_ack_tests.rs +186
- No Cargo.toml differs between the revisions.

## 2. Toolchain and environment
- `env -C /tmp/cml-attr749-1227/repo rustc -V` = rustc 1.99.0-nightly (26ae60a9e 2026-07-28).
- cargo 1.99.0-nightly (3efb1f477 2026-07-17), from rust-toolchain.toml (nightly-2026-07-29).
- nproc 316. `ulimit -Sn` and `ulimit -Hn` are both 524288.
- Of the RUST/REVERIE/CARGO/TOKIO variables, only RUSTPLATFORM=platform010 was set.
- The driver refuses to start if any REVERIE_FATAL_CAPACITY* variable is set.

## 3. Commands
/tmp/cml-rev749 was only cloned from and read; no other git command touched it.
```
git clone --no-checkout /tmp/cml-rev749 /tmp/cml-attr749-1227/repo                       # rc 0
git -C /tmp/cml-attr749-1227/repo checkout --detach 410312f4f7faa3bf3c6e329be6acfedbd5c7df85   # rc 0; build A
sed -i '231,268d' /tmp/cml-attr749-1227/repo/reverie-ptrace/src/tracer/fatal_capacity_tests.rs  # rc 0; build B
git -C /tmp/cml-attr749-1227/repo checkout -- reverie-ptrace/src/tracer/fatal_capacity_tests.rs # rc 0
git -C /tmp/cml-attr749-1227/repo checkout --detach dbf2b5c8880bac5fd6a06fa1f296b3eec3590569   # rc 0; build C
```
- B's diff after its build is byte-identical to the diff taken right after sed (cmp rc 0).
- B built with 0 warnings under -D warnings, so no `#[allow(dead_code)]` was added.

Build command (build.sh), identical for all three:
```
env -C /tmp/cml-attr749-1227/repo CARGO_TARGET_DIR=/tmp/cml-attr749-1227/target RUSTFLAGS="-D warnings -C link-arg=-llzma" RUSTDOCFLAGS="-D warnings" nice -n 19 cargo test --workspace --all-features --no-run --offline -j 8 > logs/build-<V>.log 2>&1 ; rc=$?
```
- After each build, a no-op rerun with `--message-format=json` served as a cross-check.
- The binary was target/debug/deps/reverie_ptrace-c5403e0a5df6195b in all three builds. It was copied with `cp -p` to bin/{A,B,C} right after each build.

## 4. Builds
| Variant | UTC | rc | Wall s | Crates compiled | JSON no-op | df avail before |
|---|---|---|---|---|---|---|
| A | 12:33:24-12:35:20 | 0 | 115.645694163 | 239 ("Finished 1m 55s") | rc 0, 0.184 s | 617,353,334,784 |
| B | 12:37:15-12:37:35 | 0 | 20.840979415 | 1 (reverie-ptrace; "20.75s") | rc 0, 0.179 s | 607,663,181,824 |
| C | 12:42:15-12:43:04 | 0 | 49.048532326 | 6 (safeptrace, reverie-ptrace, reverie-liteinst, reverie-e9patch, reverie-sabre-strace, reverie-examples; "48.97s") | rc 0, 0.219 s | 606,595,166,208 |

- **Warnings:** 0 rustc warnings and 0 errors in every build. Each log has the same 19 build-script `warning:` lines: 15 from reverie-sabre (vendored libelf fallthrough), 3 from reverie-dbt (cache) and 1 from reverie-e9patch.
- **Cargo.lock:** it is untracked and ignored, so it was generated offline by the first build ("Locking 263 packages"). The same file served all three builds (byte-identical to the copy taken after A). It differs from /tmp/cml-rev749/Cargo.lock in two places:
  - lazy_static is 1.5.1 here, 1.5.0 there;
  - here, reverie-liteinst has an extra "iced-x86" dependency, which matches reverie-liteinst/Cargo.toml:31.

**Binaries:**
| Variant | sha256 | Bytes | `--list tracer::tests::fatal_capacity_tests` |
|---|---|---|---|
| A | b5fb01660d8b777d1ca59d8a933b243f2bc4fd1381e5ac0e9c8bd4550050650f | 394,316,728 | 6 tests |
| B | a34fcb66a9c3b6aea69274331a72806091e5bbfb31d832c6c40ebfdf8933f030 | 394,331,080 | 6 tests |
| C | 5dccb14221b4ab9e2152bfe96ff767089c0ecde130df4e0e3058372c5bac957e | 393,942,256 | 2 tests |

- A and B list: nonretiring_child_hook_falsifies_original_deadline, retained_child_owners_falsify_capacity_bound, retired_processes_release_descriptors_under_reduced_nofile, retired_threads_release_descriptors_under_reduced_nofile, stalled_hook_control_rejects_non_timeout_original_outcomes, stalled_hook_control_requires_unique_pre_rescue_outcome.
- C lists retired_processes_release_descriptors_under_reduced_nofile and retired_threads_release_descriptors_under_reduced_nofile.

## 5. Run method (measure.py)
- **Order:** for k=1..40, the variants rotate (k=1 A,B,C; k=2 B,C,A; k=3 C,A,B; ...). One invocation at a time.
- **Command:** `nice -n 19 timeout 120 /tmp/cml-attr749-1227/bin/<V> --exact tracer::tests::fatal_capacity_tests::retired_threads_release_descriptors_under_reduced_nofile --test-threads=1 -Z unstable-options --report-time --format json --show-output`
- **Setup:** cwd = the empty /tmp/cml-attr749-1227/cwd, stdin = /dev/null. Stdout and stderr go to separate files, runs/<V>-k<NN>.{out,err}.
- **Recorded per run** (invocations.csv): /proc/loadavg before and after, rc, libtest exec_time, and monotonic wall time.
- **Window:**
  - First start 12:43:32.819Z; last start 12:43:49.937Z (that run took 0.173 s). Summed wall time 17.252 s.
  - Driver rc 0, printed DONE.
- **loadavg resolution:** /proc/loadavg updates about every 5 s, so before and after are identical in 39 (A), 38 (B) and 39 (C) of 40 runs.

## 6. Parsing (parse_runs.py; rc 0, empty stderr)
- **A run counts as parsed only if:**
  - it has exactly 96 marker lines;
  - each marker starts at the beginning of a line (no interleaving);
  - each line fully matches the variant's regex;
  - iterations run 0..95 in order;
  - for A and B, the ranges are contiguous and each range's tuple count equals b-a.
- **Cross-checks:**
  - For A and B, the printed baseline equals iteration 0's fds.
  - In every variant, the RPC samples in the "original predicate" line equal the marker fds.
- **Independent recount:** recount.awk reads the raw .err files without the parser. It gives 3840/147/0/+7 (A), 3840/123/0/+7 (B) and 3840/6/0/+7 (C) for markers / in flight >=1 / in flight >=2 / largest excess, identical to the parser.

## 7. Results
| | A | B | C |
|---|---|---|---|
| runs/passes/failures/hangs | 40/40/0/0 | 40/40/0/0 | 40/40/0/0 |
| parsed runs | 40 | 40 | 40 |
| baseline | 18 (40 runs) | 18 (40) | 18 (40) |
| printed baseline = iteration-0 fds | 40/40 | 40/40 | not printed |
| RPC samples = marker fds | 40/40 | 40/40 | 40/40 |
| total markers | 3840 | 3840 | 3840 |
| markers with exactly 1 in flight | 147 (3.83%) | 123 (3.20%) | 6 (0.16%) |
| markers with >=2 in flight | 0 | 0 | 0 |
| runs with >=1 in-flight marker | 38 | 39 | 6 |
| in-flight markers per run, mean/median/max | 3.675/3/8 | 3.075/2.5/9 | 0.15/0/1 |
| excess at in-flight markers | +5:143, +6:1, +7:3 | +5:118, +7:5 | +5:5, +7:1 |
| excess at 0-in-flight markers | +0:3693 | +0:3717 | +0:3834 |
| largest excess per run (value:runs) | +0:2, +5:34, +6:1, +7:3 | +0:1, +5:34, +7:5 | +0:34, +5:5, +7:1 |
| exec_time s min/median/max (mean) | 0.098/0.106/0.171 (0.110) | 0.099/0.107/0.156 (0.111) | 0.135/0.157/0.509 (0.19) |
| runs with exec_time > 0.2 s | 0 | 0 | 13 |
| wall s min/median/max | 0.102/0.111/0.178 | 0.104/0.112/0.163 | 0.140/0.162/0.515 |
| load1 range | 24.67-26.02 | 24.67-26.02 | 24.35-26.02 |
| load5 / load15 range | 27.59-27.95 / 25.62-25.71 | same as A | 27.43-27.95 / 25.59-25.71 |
| stderr bytes per run min/median/max | 16,562/16,591/16,601 | 16,566/16,589/16,601 | 141,955/141,984/141,984 |

- There were no failures, so there is no failure text. The largest observed excess, +7, is within the baseline+8 assertion that both revisions make.
- **Runs by number of in-flight markers:**
  - A: 0:2, 1:5, 2:3, 3:12, 4:5, 5:4, 6:6, 8:3.
  - B: 0:1, 1:11, 2:8, 3:6, 4:4, 5:5, 6:1, 7:2, 8:1, 9:1.
  - C: 0:34, 1:6.
- **C's in-flight markers** (k, iteration): (1,7), (7,79), (11,23; fds 25, +7), (21,88), (28,91), (30,68). All others were fds 23, +5.
- **Markers with excess +6 or more** (k, iteration, excess), each with exactly 1 in flight:
  - A: (14,6,+7), (15,6,+6), (19,7,+7), (20,27,+7).
  - B: (7,92,+7), (12,53,+7), (13,73,+7), (16,91,+7), (40,27,+7).
  - C: (11,23,+7).
- **Iteration 95:**
  - A k=25 and B k=2 each had 1 in flight at marker 95, with fds 23 (+5). C had none in 40 runs.
  - Main's final assertion `captured[TASKS-1].1 <= baseline+2` (main fatal_capacity_tests.rs:184-187) is false for such a sample (23 > 20).
  - The head asserts on steady_fds instead (head 416-420).
- **Lines only A and B print** (80 runs):
  - The checkpoint reads `registered=97, completed=96, retained=0, held_hook=false, steady_fds=Some(18)` in 40/40 runs of each.
  - The steady sample (iteration 95) is fds=18 (+0) in 40/40 of each.
  - maximum_immediate: A 18:2, 23:34, 24:1, 25:3; B 18:1, 23:34, 25:5.
  - The "original checkpoint" line lists 97 FINISHED tuples, all (tid,true,true,true), in 80/80 runs.
  - For comparison, C's iteration-95 marker lists 96 FINISHED tuples in 40/40 runs.
- **By rotation position**, (runs, in-flight markers) for positions 1/2/3:
  - A: (14,48) (13,55) (13,44)
  - B: (13,30) (14,53) (13,40)
  - C: (13,2) (13,1) (14,3)
- **By half** (k01-20 / k21-40): A 82/65; B 73/50; C 3/3.

## 8. 2x2: in flight >=1 versus excess >=+5 (all markers)
| | in flight>=1 & excess>=5 | in flight>=1 & excess<5 | in flight 0 & excess>=5 | in flight 0 & excess<5 | agreement |
|---|---|---|---|---|---|
| A | 147 | 0 | 0 | 3693 | 3840/3840 |
| B | 123 | 0 | 0 | 3717 | 3840/3840 |
| C (supplementary) | 6 | 0 | 0 | 3834 | 3840/3840 |

## 9. Code facts (head line / main line; a single number means the same at both)
**9.1 Default Tool::on_exit_thread.** reverie/src/tool.rs is blob 596a62cf at both revisions.
- The trait is `#[async_trait] pub trait Tool`, at 424-425.
- The default is at 682-690. It takes `_thread_state: Self::ThreadState` by move, and its body is `Ok(())` (689).
- The doc comment (674-681) reads: "Called when a thread will exit shortly or has exited", "Serves as a \"destructor\" for the thread state, and thus takes it by move", "Panic unwinding may bypass the hook".
- `#[reverie::tool]` is async_trait (reverie/src/lib.rs:104). Every call therefore builds a boxed future, and callers do not test whether the tool overrides the method.

**9.2 Which body runs.**
- **A:** the override at head fatal_capacity_tests.rs:231-268 (ThreadState = (), line 170).
  - REVERIE_FATAL_CAPACITY_FAULT is set only in capacity_negative_control (fn at 451, set at 469). git grep finds it only at 274 and 469.
  - Test (2) re-executes with only REVERIE_FATAL_CAPACITY_TEST and REVERIE_FATAL_CAPACITY_DEADLINE_NS (438-441), so fault = CapacityFault::None (274-275).
  - With fault None, `hold` is false: the override does one RETIREMENT borrow and a compare, then returns Ok(()) without awaiting. Every A run prints held_hook=false.
- **B:** the default.
- **C:** the default. Main's fatal_capacity_tests.rs has no on_exit_thread (0 matches).
- **Symbols** (`nm -C`, names containing on_exit_thread and CapacityTool or fatal_capacity_tests):
  - A has 16, including PollFn and the RETIREMENT LocalKey closures.
  - B and C have the same 3: the method, its {closure#0}, and that closure's drop glue (diff: identical).

**9.3 Is it called on every thread exit?** Yes, in all three variants; this code is identical across revisions.
- **Test (2) takes the ordinary path:**
  - spawn_fn_with_config (tracer.rs:4936/4931) sets liteinst_runtime: None (5011/5006).
  - ordinary_failure_enabled() = liteinst_runtime.is_none() (task.rs:7428-7431/7418-7421), so it is true.
  - The root's `run` (8022/8012) returns run_ordinary early (8023-8024).
  - Children: `ordinary_failure = self.ordinary_failure_enabled().then(...)` (6596-6598). The child body calls run_ordinary_newborn (6622) and is spawned with tokio::task::spawn_local (6826-6828).
  - The test is `#[tokio::test(flavor = "current_thread")]` (head 602-603 / main 223-224).
- **run_ordinary_owned** (7530/7520) matches drive_ordinary's result. OrdinaryTerminal has only Exited and Exec (tracer.rs:421-429). The arms:
  - **None** (exec transfer, head 7617-7622): no hook. The code comment reads "No on_exit hook or fabricated ExitStatus for the surviving former thread".
  - **Exited** (7624-7646/7614-7636; the 23 lines are byte-identical): retire_ordinary_terminal (7631/7621), then `self.tool_exit_ordinary(status).await` (7644/7634), then `session.finished(&stop)` (7645/7635).
  - **Exec:** retire_replaced_leader (7760/7750) calls on_exit_thread for the replaced leader (7787/7777).
- **tool_exit_ordinary** (7909/7899) calls `self.process_state.on_exit_thread(tid, &wrapped, self.thread_state, status).await` unconditionally (7949/7939). An Err is recorded with phase "ptrace on_exit_thread" (7956/7946). The first 52 lines are byte-identical across revisions.
- **All `.on_exit_thread(` call sites in reverie-ptrace/src**, 4 at each revision:
  - task.rs 7034/7024 and 7078/7068, in `tool_exit` (6981/6971). tool_exit is called only from `run` at 8135 and 8182, after the ordinary early return, so not in test (2).
  - task.rs 7787/7777 (exec).
  - task.rs 7949/7939 (ordinary exit).
- **Test (2) in practice:**
  - All 97 threads (96 joined children plus the root, which exits 0) exit normally; none execs, so all 97 go through the Exited arm and await the tool's on_exit_thread before session.finished.
  - FINISHED is pushed from FatalSession::finished (task.rs:1798-1815; the cfg(test) call is at 1812-1813, both) through record_capacity_finished_for_test (tracer.rs:464-466).
  - So "in flight" = joined by the guest but not yet through retire, hook and finished.

**9.4 Product change, safeptrace/src/notifier.rs** (non-test code; in A and B, not in C).
- **worker_thread** (1994/1875) receives `identity: Arc<WorkerIdentity>`:
  - head: NOTIFIER.remove(pid,&event) (2038), then drop(identity) (2043), then event.mark_worker_done() (2044);
  - main: mark_worker_done (1916), then NOTIFIER.remove (1920), and identity is dropped at return.
- WorkerIdentity holds pidfd: OwnedFd and proc_dir: OwnedFd (1766-1772/1647-1653).
- **mark_worker_done** (1511-1522/1400-1405): the head holds worker_done_lock across the WORKER_DONE swap and notify_all.
- **Also reordered** so that registry removal comes before DONE: finish_sync_terminal (1366-1373/1256-1262), the unstarted completion in resolve_echild (2504-2531/2380-2388), and finish_unstarted_raw_cleanup (3239-3243/3093-3097).
- **retire_ordinary_terminal** (tracer.rs:483-493/478-488, same body) spins `while !terminal.wait(Duration::ZERO) { yield_now }`.
  - TerminalCleanup::wait (3258-3259/3109-3110) calls wait_worker_done (1524-1544/1407-1425).
  - With a zero timeout, that is a single WORKER_DONE check.
- safeptrace's own #[cfg(test)] pauses (head 2039-2040, 1536-1537) are not compiled into the reverie-ptrace test binary, because cfg(test) applies only to the crate under test.

**9.5 PR test-only changes on test (2)'s path** (in A and B, not in C).
- **task.rs 6829-6839:** under cfg(test), the child body is pinned and awaited, then `record_capacity_body_dropped_for_test(id, result)` runs before the send.
- **tracer.rs:** a new record_capacity_body_dropped_for_test (467-470), and record_registered is now called inside record_fatal_task_for_test (473).
- **fatal_capacity_tests.rs:**
  - The handler (176-229) prints only new FINISHED tuples; main clones and prints the whole list.
  - After the marker-95 RPC, it awaits all_child_bodies_dropped() and takes a steady sample (205-227).
  - New: RETIREMENT state, checkpoint and bounds lines, and 4 extra tests.
  - The final assertion changed from captured[95].1 <= baseline+2 (main 184-187) to steady_fds <= baseline+2 (head 416-420).
  - Both revisions assert every sample <= baseline+8 (head 412-415 / main 180-183).

## 10. Factual comparison
**A vs B (isolates the hook).** The only source difference is the 38-line deletion. Only reverie-ptrace recompiled, with the same Cargo.lock; B's binary is 14,352 bytes larger.

| | A | B |
|---|---|---|
| in-flight markers | 147 (3.83%) | 123 (3.20%) |
| runs with >=1 | 38 | 39 |
| per-run median/max | 3/8 | 2.5/9 |
| markers with >=2 in flight | 0 | 0 |
| largest excess | +7 | +7 |
| median exec_time | 0.106 s | 0.107 s |

A's override, on this path, is one thread-local borrow and a compare, then Ok(()). B's default is Ok(()). Both are awaited at the same call site, task.rs:7949.

**B vs C (product change plus the other test-tool changes).**

| | B | C |
|---|---|---|
| in-flight markers | 123 | 6 |
| runs with >=1 | 39 | 6 |
| largest excess | +7 | +7 |
| markers with >=2 in flight | 0 | 0 |
| median exec_time | 0.107 s | 0.157 s |

Confounds in B vs C:
1. The notifier reorder (9.4).
2. cfg(test) exit-path instrumentation present only in B: the task.rs pin and body-dropped receipt, and tracer.rs record_registered.
3. Handler output: C clones and prints the cumulative FINISHED list in each marker after the fd count.
   - That is up to 4,656 tuples per run (96*97/2 minus in-flight entries), against at most 96 for B.
   - Stderr is about 142 KB per run for C, against about 16.6 KB.
   - B also adds the all_child_bodies_dropped wait and steady sample after marker 95.
4. C has a longer exec_time tail: 13 of 40 runs above 0.2 s, against 0 of 40 for B.
5. Binary layout: B has 6 tests in the module and 481 filtered out; C has 2 and 477 filtered out. C is 388,824 bytes smaller than B.

Confounds common to all comparisons:
- A shared host: load1 24.35-26.02 on 316 CPUs, and every run under nice 19.
- One session of about 17 s, with 40 runs per variant.
- Rotated order, not randomized.
- loadavg resolves only to about 5 s.

## 11. Files
In /tmp/claude-coord-notes/attr749/:
- runs.csv: one row per run.
- markers.csv: 11,520 marker rows.
- invocations.csv.
- measure.py and parse_runs.py.
- recount.awk.
- build.sh: its header comment was corrected after the runs. It had claimed the script copies the binary to bin/<variant>; it does not, and the copy, sha256 and --list were done by hand after each build. No code changed.
- B-deletion.diff.
- raw-runs.tar.gz (240 files).
- evidence/: build records, sha256-all.txt, list-{A,B,C}.txt, measure.log, parse.md (parser output), the Cargo.lock diff, nm-*-oet.txt, notifier.diff (main to head), clone and checkout logs, B-state.txt.

Kept in /tmp/cml-attr749-1227/:
- bin/{A,B,C}: sha256 re-checked after cleanup, unchanged.
- runs/ (240 files).
- logs/.
- repo/: detached at dbf2b5c8, clean.

## 12. Cleanup and disk
- `rm -rf /tmp/cml-attr749-1227/dc` (the rustc lint probe): rc 0.
- `rm -rf /tmp/cml-attr749-1227/target`: rc 0. Before removal, `du -sb` measured it at 32,598,184,255 bytes.
- `df -B1 --output=avail /data`: 596,661,280,768 before the cleanup and 604,161,966,080 at 12:59:24Z. The disk is shared with other agents' concurrent activity, so the difference is not attributable to the removal alone.
