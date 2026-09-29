<!-- tfa-phase2:02e283e14aff4e440db1adfba5feda6e7a1969b76cc75dc79981bc1577038673:followup -->
TFA phase-two candidate fix follow-up (artifact-only preview; not posted; no Diff created)
- Selector: `fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect`
- Root-cause hypothesis (investigator candidate, rank 1 of 4; not proven): Test synchronization race in ObserverAttachedBeforeConnect. TLS 1.3 NewSessionTickets are still unread when the first timestamped write is made; AsyncSocket handleRead consumes the tickets and then the echo in one loop without re-draining the error queue, where the ACK timestamp is already queued (ioReady drains it only on entry), so writeAndReflect returns before the ACK ByteEvent is delivered. Fix drains post-handshake data with an untimestamped writeAndReflect first.
- Contradicting or unverified evidence the investigator reported: No post-fix run exists, because the investigator cannot build. That the ACK timestamp is queued when the server echoes is inferred from how kernel TCP handles ACKs; the syscall trace does not observe it directly.
- Other ranked hypotheses, which these replays do not exclude:
  - rank 2: The kernel had not yet generated the ACK timestamp when the echo was read, for example because of delayed ACK on the server.
  - rank 3: Production defect: AsyncSocket should drain the error queue between reads inside handleRead.
  - rank 4: Hermit artifact: chaos scheduling or epoll emulation hides EPOLLERR, or produces an interleaving that cannot happen natively.
- Local candidate patch sha256 `dc77e59116234fc5610b5006e2d57423016c6eb39cbb38a326572111bb49e92d`, applied only in an isolated controller checkout
- Unfixed test ELF sha256 `47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46`; patched test ELF sha256 `3dd68a50be15bd8f5a456f23ff4a3a278516a37881d423d154f64af51c7e9da9`
- Evidence ledger sha256 `eb8f947917ae0a6ceb8a6517f89542a9905815f9907d202035e3ff288cf4404f`
Hermit replays, each re-judged from its own bytes (CENSORED, NONDETERMINISTIC and OTHER count as neither PASS nor FAIL). fail-witness and pass-witness are the manifest FAIL and PASS seeds; discriminator and fresh runs replay further seeds:
- fail-witness seed 11: unfixed FAILx5, NONDETERMINISTICx2; patched PASSx3
- pass-witness seed 1: unfixed PASSx2; patched PASSx2
- fresh seed 12: unfixed PASSx1; patched PASSx1
- fresh seed 13: unfixed PASSx1; patched PASSx1
- fresh seed 14: unfixed PASSx1; patched PASSx1
- fresh seed 15: unfixed OTHERx2; patched OTHERx1
Verdict: SUPPORTED
  - patched fresh seed 15 was OTHER
Verdict rule: SUPPORTED needs at least one seed that FAILed unfixed; each such seed and the PASS witness must have patched replays, all PASS; and no patched replay may FAIL. Any patched FAIL is NOT SUPPORTED, and anything else is INCONCLUSIVE. SUPPORTED means only that the candidate patch removed the observed FAIL in these Hermit replays. It does not prove the hypothesised root cause, and it is not native, CI, landed or production-containment validation of the fix.
Measured replay commands, exactly as recorded in each receipt. Each writes under its own recorded run directory, so point its paths at a fresh directory before rerunning, and first check that the test ELF at its path still has that phase's sha256:
```sh
# unfixed fail-witness seed 11, judged FAIL
cd /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/prefix/fail-witness-seed-011-attempt-1/controller-cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/prefix/fail-witness-seed-011-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=8.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/stress/asyncssl/buck-out,target=/home/newton/work/tfa/phase2-cases/stress/asyncssl/buck-out --mount=type=bind,source=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/prefix/fail-witness-seed-011-attempt-1/controller-cwd,target=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/prefix/fail-witness-seed-011-attempt-1/controller-cwd --tmp=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/prefix/fail-witness-seed-011-attempt-1/guest-tmp --epoch=2026-09-21T22:13:10Z --seed 0 --rng-seed 0 --sched-seed 11 --fuzz-seed 0 --sched-heuristic random --target-timeslice 5000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/prefix/fail-witness-seed-011-attempt-1/verify -- /home/newton/work/tfa/phase2-cases/stress/asyncssl/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test --gtest_filter=AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect --gtest_also_run_disabled_tests
# patched fail-witness seed 11, judged PASS
cd /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/postfix/fail-witness-seed-011-attempt-1/controller-cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/postfix/fail-witness-seed-011-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=8.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/stress/asyncssl/buck-out,target=/home/newton/work/tfa/phase2-cases/stress/asyncssl/buck-out --mount=type=bind,source=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/postfix/fail-witness-seed-011-attempt-1/controller-cwd,target=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/postfix/fail-witness-seed-011-attempt-1/controller-cwd --tmp=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/postfix/fail-witness-seed-011-attempt-1/guest-tmp --epoch=2026-09-21T22:13:10Z --seed 0 --rng-seed 0 --sched-seed 11 --fuzz-seed 0 --sched-heuristic random --target-timeslice 5000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/asyncssl/postfix/fail-witness-seed-011-attempt-1/verify -- /home/newton/work/tfa/phase2-cases/stress/asyncssl/buck-out/v2/art/fbcode/d45633ec019a63e6/folly/io/async/test/__async_ssl_socket_test__/async_ssl_socket_test --gtest_filter=AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect --gtest_also_run_disabled_tests
```
Local diff:
```diff
diff --git a/fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp b/fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp
--- a/fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp
+++ b/fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp
@@ -3788,6 +3788,12 @@
   EXPECT_FALSE(observer->byteEventsUnavailableCalledEx.has_value());
   clientConn.netOpsVerifyAndClearExpectations();
 
+  // Drain the server's post-handshake data with an untimestamped write before
+  // the first timestamped write; otherwise the ACK timestamp can be left on the
+  // error queue. See ObserverAttachedAfterConnect for the full sequence.
+  serverHandshakeCb_->waitForHandshake();
+  clientConn.writeAndReflect(wbuf, WriteFlags::NONE);
+
   {
     clientConn.netOpsExpectWriteWithFlags(dropWriteFromFlags(flags));
     clientConn.writeAndReflect(wbuf, flags);
```
Not posted; no Diff created; no reviewers assigned.
