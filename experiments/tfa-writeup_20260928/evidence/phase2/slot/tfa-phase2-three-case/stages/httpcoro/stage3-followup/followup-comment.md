<!-- tfa-phase2:abd4f5d24af2f6750cda9d7237aa52024119d556dface89b47fd55f658ed20b0:followup -->
TFA phase-two candidate fix follow-up (artifact-only preview; not posted; no Diff created)
- Selector: `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests - HTTPDownstreamSessionTest/H2DownstreamSessionTest.ResetRateLimit/h2`
- Root cause (investigator candidate): Test-side timing bug. In ResetRateLimit, loopN(4) uses blocking loopOnce; its idle iterations let Hermit virtual time reach the 1s connection read timeout, so readLoop gets TIMED_OUT and initiateDrain sends GOAWAY with last stream 1. HTTP2Codec then suppresses onMessageBegin and onAbort for all 1001 burst streams, the RSTS limiter never trips, the keepalive stream is never cancelled, and readBodyEvent returns SUSPEND, failing the CHECK at line 1685.
- Local candidate patch sha256 `3806affcce2acc122003133e7c1622cd68025333062a50e13198f73fcaf20952`, applied only in an isolated controller checkout
- Unfixed test ELF sha256 `00066235bcc942498d22862701809d0b04e4ec8d99a587ff220f320681083efc`; patched test ELF sha256 `30ecdc395c8e96f1aa1cc272eab277277cc1bf1166c8447d59887d17f564e0b3`
- Evidence ledger sha256 `6658789a35dece41d53b27820bd4ba2039c763758b2630049fa220dbb7f98337`
Hermit replays, each re-judged from its own bytes (CENSORED, NONDETERMINISTIC and OTHER count as neither PASS nor FAIL):
- fail-witness seed 0: unfixed FAILx3; patched PASSx2
- pass-witness seed 1: unfixed PASSx2; patched PASSx2
- fresh seed 4: unfixed PASSx1; patched PASSx1
- fresh seed 5: unfixed CENSOREDx1; patched PASSx1
- fresh seed 6: unfixed CENSOREDx1; patched PASSx1
- fresh seed 7: unfixed PASSx1; patched PASSx1
- 1 attempt(s) were interrupted without a receipt; they count as neither PASS nor FAIL.
Verdict: SUPPORTED
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
