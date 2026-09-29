<!-- tfa-phase2:cd46934a017713ee49c4ec4b019ee1122d1564133462abec6c8744c21e9d8044:followup -->
TFA phase-two candidate fix follow-up (artifact-only preview; not posted; no Diff created)
- Selector: `fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
- Root-cause hypothesis (investigator candidate, rank 1 of 4; not proven): Test race, not a server bug. For rocket, getActiveRequests only drops when the request object is destroyed (after it moves to the finished list, possibly after the write flush), which lags the req1 handler posting baton1. The loop waiting for 2 active requests can exit on req1 and req2 before req3 reaches the server, so the snapshot has 2 entries. Fix waits for active requests to reach 0 after baton1.
- Investigator-written text, shown as plain text with each field collapsed to one line and cut at 1000 characters:
```text
Contradicting or unverified evidence the investigator reported: Not directly instrumented. No build is allowed, so which snapshot entry is missing and the activeRequests value at loop exit are inferred from reading the code.
Other ranked hypotheses, which these replays do not exclude:
rank 2: req3 was counted but its DebugStub was not yet registered when the snapshot task ran.
rank 3: req1 was dropped from the registry because the finished-list limit was not applied or it was evicted.
rank 4: Requests span multiple IO workers and one worker's registry is missing from the aggregate.
```
- Local candidate patch sha256 `61dd5a10a6e93cec124ada9d203d9d53f66bd546da55f77b09c86593a4975cb7`, applied only in an isolated controller checkout
- Unfixed test ELF sha256 `2ec5d2128aa5af3ae86d46659054dd72b64ef86a389cf89934901a2e094cd0fc`; patched test ELF sha256 `440b91829732e378dd2a5519869f5c5bb110473218d7eefedd11cd73f50fa15d`
- Evidence ledger sha256 `4ab8dd604addd107234a73503e55fce149316b59078203e290c051339c2a2ed5`
Hermit replays, each re-judged from its own bytes (CENSORED, NONDETERMINISTIC and OTHER count as neither PASS nor FAIL). fail-witness and pass-witness are the manifest FAIL and PASS seeds; discriminator and fresh runs replay further seeds:
- fail-witness seed 1: unfixed PASSx2; patched PASSx2
- pass-witness seed 0: unfixed PASSx2; patched PASSx2
- discriminator seed 3: unfixed FAILx3; patched PASSx3
- discriminator seed 6: unfixed PASSx1; patched not run
- discriminator seed 7: unfixed PASSx1; patched not run
- discriminator seed 8: unfixed FAILx1; patched PASSx2
- discriminator seed 9: unfixed PASSx1; patched not run
- discriminator seed 10: unfixed PASSx1; patched not run
- discriminator seed 11: unfixed PASSx1; patched not run
- fresh seed 2: unfixed PASSx1; patched PASSx1
- fresh seed 3: unfixed FAILx1; patched PASSx1
- fresh seed 4: unfixed PASSx1; patched PASSx1
- fresh seed 5: unfixed PASSx1; patched PASSx1
Verdict: SUPPORTED
  - fail-witness seed 1 did not FAIL unfixed at this checkout path
Verdict rule: SUPPORTED needs at least one seed that FAILed unfixed; each such seed and the PASS witness must have patched replays, all PASS; and no patched replay may FAIL. Any patched FAIL is NOT SUPPORTED, and anything else is INCONCLUSIVE. SUPPORTED means only that the patched replays did not reproduce the FAIL seen unfixed. A patched replay at the same seed does not repeat the unfixed schedule, because the patched code runs differently. SUPPORTED does not prove the hypothesised root cause, and it is not native, CI, landed or production-containment validation of the fix.
Measured replay commands, exactly as recorded in each receipt. Each writes under its own recorded run directory, so point its paths at a fresh directory before rerunning, and first check that the test ELF at its path still has that phase's sha256:
```sh
# unfixed fresh seed 3, judged FAIL
cd /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/prefix-fresh/fresh-seed-003/controller-cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/prefix-fresh/fresh-seed-003/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/stress/thrift/buck-out,target=/home/newton/work/tfa/phase2-cases/stress/thrift/buck-out --mount=type=bind,source=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/prefix-fresh/fresh-seed-003/controller-cwd,target=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/prefix-fresh/fresh-seed-003/controller-cwd --tmp=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/prefix-fresh/fresh-seed-003/guest-tmp --epoch=2026-09-21T21:44:37Z --seed 0 --rng-seed 0 --sched-seed 3 --fuzz-seed 0 --sched-heuristic random --target-timeslice 1000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/prefix-fresh/fresh-seed-003/verify -- /home/newton/work/tfa/phase2-cases/stress/thrift/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest --gtest_filter=RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest
# patched discriminator seed 3, judged PASS
cd /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/postfix/discriminator-seed-003-attempt-1/controller-cwd && /home/newton/work/dev-hermit/bin/safehermit --sh-report /home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/postfix/discriminator-seed-003-attempt-1/safehermit.txt --sh-deadline 120 --sh-max-log-bytes 67108864 --sh-disk-limit 8G /home/newton/work/dev-hermit/ignored/tfa-rlimit-core-rerun-20260920-4630ee2a/stable-5c422b85/hermit-4630ee2a run --backend ptrace --strict --chaos --chaos-per-thread-slowdown --chaos-slowdown-max-factor=10.0 --chaos-epoch-length-ns=0 --base-env=minimal --env LC_ALL=C --env TZ=UTC --mount=type=bind,source=/home/newton/work/tfa/phase2-cases/stress/thrift/buck-out,target=/home/newton/work/tfa/phase2-cases/stress/thrift/buck-out --mount=type=bind,source=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/postfix/discriminator-seed-003-attempt-1/controller-cwd,target=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/postfix/discriminator-seed-003-attempt-1/controller-cwd --tmp=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/postfix/discriminator-seed-003-attempt-1/guest-tmp --epoch=2026-09-21T21:44:37Z --seed 0 --rng-seed 0 --sched-seed 3 --fuzz-seed 0 --sched-heuristic random --target-timeslice 1000000 --max-timeslice 200000000 --clock-multiplier=1.0 --verify --verify-strict --verify-allow both --keep-logs --verify-log-dir=/home/newton/work/dev-hermit/worktrees/slots/tfa-phase2-cyber-opus/ignored/tfa-phase2-three-case/stress/thrift/postfix/discriminator-seed-003-attempt-1/verify -- /home/newton/work/tfa/phase2-cases/stress/thrift/buck-out/v2/art/fbcode/94ec6645b5dc6cae/thrift/lib/cpp2/test/__InstrumentationTest__/InstrumentationTest --gtest_filter=RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest
```
Local diff:
```diff
diff --git a/fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp b/fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp
--- a/fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp
+++ b/fbcode/thrift/lib/cpp2/test/InstrumentationTest.cpp
@@ -803,6 +803,13 @@
   handler()->setCallback([&](TestInterface*) { baton1.post(); });
   auto req1 = client->semifuture_runCallback();
   baton1.wait();
+  // req1 stays counted as active until its request object is destroyed (after
+  // it moves to the finished list), which can lag the handler returning.
+  // Without this, the wait for 2 active requests below could be satisfied by
+  // req1 and req2 before req3 reaches the server.
+  while (thriftServer()->getActiveRequests() > 0) {
+    std::this_thread::yield();
+  }
 
   // "started processing" request
   handler()->setCallback([&](TestInterface*) {
```
Not posted; no Diff created; no reviewers assigned.
