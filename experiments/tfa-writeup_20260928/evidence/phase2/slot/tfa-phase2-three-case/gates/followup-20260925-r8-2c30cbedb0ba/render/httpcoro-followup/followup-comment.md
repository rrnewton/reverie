<!-- tfa-phase2:abd4f5d24af2f6750cda9d7237aa52024119d556dface89b47fd55f658ed20b0:followup -->
TFA phase-two candidate fix follow-up (artifact-only preview; not posted; no Diff created)
- Selector: `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`
- Root-cause hypothesis (investigator candidate, rank 1 of 4; not proven): Test-side timing bug. In ResetRateLimit, loopN(4) uses blocking loopOnce; its idle iterations let Hermit virtual time reach the 1s connection read timeout, so readLoop gets TIMED_OUT and initiateDrain sends GOAWAY with last stream 1. HTTP2Codec then suppresses onMessageBegin and onAbort for all 1001 burst streams, the RSTS limiter never trips, the keepalive stream is never cancelled, and readBodyEvent returns SUSPEND, failing the CHECK at line 1685.
- Contradicting or unverified evidence the investigator reported: No DBG log directly shows 'Initiating drain due to read timeout' or 'Suppressing onAbort'; that the +900ms deadline was the connection read timer comes from timing arithmetic (armed at about .672s, 1s timeout, reached at about 1.677s). The 1s writeLoop writeEvent_ timer and TestCoroTransport ack timers expire in the same window, but handleWriteEventTimeout is inert unless flow-control blocked. Seed 2 has no oracle verdict (CENSORED), so it does not count as an independent FAIL.
- Other ranked hypotheses, which these replays do not exclude:
  - rank 2: The RSTS fixed 100ms window resets during the burst (timeoutExpired zeroes the counter), splitting the 1001 resets across two windows so neither exceeds 1000.
  - rank 3: Ordering race: the verification coroutine (posted through evb_.add / notification queue, which runs in the libevent phase ahead of loop callbacks) runs before the readLoop has processed the burst, because loopN(2) is not enough iterations.
  - rank 4: The keepalive stream's 1s stream read timeout (setStreamReadTimeout) fires during the slowed run and puts the stream into a state where readBodyEvent returns SUSPEND.
- Local candidate patch sha256 `3806affcce2acc122003133e7c1622cd68025333062a50e13198f73fcaf20952`, applied only in an isolated controller checkout
- Unfixed test ELF sha256 `00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc`; patched test ELF sha256 `30ecdc395c8e96f1aa1cc272eab277277cc1bf1166c8447d59887d17f564e0b3`
- Evidence ledger sha256 `6658789a35dece41d53b27820bd4ba2039c763758b2630049fa220dbb7f98337`
Hermit replays, each re-judged from its own bytes (CENSORED, NONDETERMINISTIC and OTHER count as neither PASS nor FAIL). fail-witness and pass-witness are the manifest FAIL and PASS seeds; discriminator and fresh runs replay further seeds:
- fail-witness seed 0: unfixed FAILx3; patched PASSx2
- pass-witness seed 1: unfixed PASSx2; patched PASSx2
- fresh seed 4: unfixed PASSx1; patched PASSx1
- fresh seed 5: unfixed CENSOREDx1; patched PASSx1
- fresh seed 6: unfixed CENSOREDx1; patched PASSx1
- fresh seed 7: unfixed PASSx1; patched PASSx1
- 1 attempt(s) were interrupted without a receipt; they count as neither PASS nor FAIL.
Verdict: SUPPORTED
Verdict rule: SUPPORTED needs at least one seed that FAILed unfixed; each such seed and the PASS witness must have patched replays, all PASS; and no patched replay may FAIL. Any patched FAIL is NOT SUPPORTED, and anything else is INCONCLUSIVE. SUPPORTED means only that the patched replays did not reproduce the FAIL seen unfixed. A patched replay at the same seed does not repeat the unfixed schedule, because the patched code runs differently. SUPPORTED does not prove the hypothesised root cause, and it is not native, CI, landed or production-containment validation of the fix.
Measured replay commands, exactly as recorded in each receipt. Each writes under its own recorded run directory, so point its paths at a fresh directory before rerunning, and first check that the test ELF at its path still has that phase's sha256:
```sh
# unfixed fail-witness seed 0, judged FAIL
cd /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/prefix/fail-witness-seed-000-attempt-1/controller-cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/prefix/fail-witness-seed-000-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/stress/httpcoro/buck-out,target=/home/newton/work/tfa/phase2-cases/stress/httpcoro/buck-out --mount=type=bind,source=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/prefix/fail-witness-seed-000-attempt-1/controller-cwd,target=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/prefix/fail-witness-seed-000-attempt-1/controller-cwd --tmp=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/prefix/fail-witness-seed-000-attempt-1/guest-tmp --epoch=2026-09-21T21:01:52Z --clock-multiplier=0.5 --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --seed 0 --rng-seed 0 --sched-seed 0 --fuzz-seed 0 --sched-heuristic random --max-timeslice 200000000 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/prefix/fail-witness-seed-000-attempt-1/verify -- /home/newton/work/tfa/phase2-cases/stress/httpcoro/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests --gtest_filter=HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2
# patched fail-witness seed 0, judged PASS
cd /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/postfix/fail-witness-seed-000-attempt-1/controller-cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/postfix/fail-witness-seed-000-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/stress/httpcoro/buck-out,target=/home/newton/work/tfa/phase2-cases/stress/httpcoro/buck-out --mount=type=bind,source=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/postfix/fail-witness-seed-000-attempt-1/controller-cwd,target=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/postfix/fail-witness-seed-000-attempt-1/controller-cwd --tmp=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/postfix/fail-witness-seed-000-attempt-1/guest-tmp --epoch=2026-09-21T21:01:52Z --clock-multiplier=0.5 --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --seed 0 --rng-seed 0 --sched-seed 0 --fuzz-seed 0 --sched-heuristic random --max-timeslice 200000000 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/httpcoro/postfix/fail-witness-seed-000-attempt-1/verify -- /home/newton/work/tfa/phase2-cases/stress/httpcoro/buck-out/v2/art/fbcode/31e8493249d09edd/proxygen/lib/http/coro/test/__HTTPCoroSessionTests__/HTTPCoroSessionTests --gtest_filter=HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2
```
Local diff:
```diff
diff --git a/fbcode/proxygen/lib/http/coro/test/HTTPDownstreamCoroSessionTests.cpp b/fbcode/proxygen/lib/http/coro/test/HTTPDownstreamCoroSessionTests.cpp
--- a/fbcode/proxygen/lib/http/coro/test/HTTPDownstreamCoroSessionTests.cpp
+++ b/fbcode/proxygen/lib/http/coro/test/HTTPDownstreamCoroSessionTests.cpp
@@ -1665,7 +1665,12 @@
         keepaliveReqSource.setSource(requestSource.release());
         co_return HTTPFixedSource::makeFixedResponse(200);
       });
-  loopN(4);
+  // Must not block: an idle blocking loopOnce() waits for the next timer, which
+  // can be the 1s connection read timeout. The resulting GOAWAY makes the codec
+  // suppress every later stream, so the resets below would never be counted.
+  for (int i = 0; i < 4; i++) {
+    evb_.loopOnce(EVLOOP_NONBLOCK);
+  }
 
   // saturate reset rate limit, ::onIngressError will be invoked
   EXPECT_CALL(lifecycleObs_, onIngressError(_, kErrorMessage));
```
Not posted; no Diff created; no reviewers assigned.
