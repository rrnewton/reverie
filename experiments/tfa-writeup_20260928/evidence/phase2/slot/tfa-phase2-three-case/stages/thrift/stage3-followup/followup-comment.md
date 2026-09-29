<!-- tfa-phase2:cd46934a017713ee49c4ec4b019ee1122d1564133462abec6c8744c21e9d8044:followup -->
TFA phase-two candidate fix follow-up (artifact-only preview; not posted; no Diff created)
- Selector: `fbcode//thrift/lib/cpp2/test:InstrumentationTest - RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`
- Root cause (investigator candidate): Test race, not a server bug. For rocket, getActiveRequests only drops when the request object is destroyed (after it moves to the finished list, possibly after the write flush), which lags the req1 handler posting baton1. The loop waiting for 2 active requests can exit on req1 and req2 before req3 reaches the server, so the snapshot has 2 entries. Fix waits for active requests to reach 0 after baton1.
- Local candidate patch sha256 `61dd5a10a6e93cec124ada9d203d9d53f66bd546da55f77b09c86593a4975cb7`, applied only in an isolated controller checkout
- Unfixed test ELF sha256 `2ec5d2128aa5af3ae86d46659054dd72b64ef86a389cf89934901a2e094cd0fc`; patched test ELF sha256 `440b91829732e378dd2a5519869f5c5bb110473218d7eefedd11cd73f50fa15d`
- Evidence ledger sha256 `4ab8dd604addd107234a73503e55fce149316b59078203e290c051339c2a2ed5`
Hermit replays, each re-judged from its own bytes (CENSORED, NONDETERMINISTIC and OTHER count as neither PASS nor FAIL):
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
