<!-- tfa-phase2:02e283e14aff4e440db1adfba5feda6e7a1969b76cc75dc79981bc1577038673:followup -->
TFA phase-two candidate fix follow-up (artifact-only preview; not posted; no Diff created)
- Selector: `fbcode//folly/io/async/test:async_ssl_socket_test - AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect`
- Root cause (investigator candidate): Test synchronization race in ObserverAttachedBeforeConnect. TLS 1.3 NewSessionTickets are still unread when the first timestamped write is made; AsyncSocket handleRead consumes the tickets and then the echo in one loop without re-draining the error queue, where the ACK timestamp is already queued (ioReady drains it only on entry), so writeAndReflect returns before the ACK ByteEvent is delivered. Fix drains post-handshake data with an untimestamped writeAndReflect first.
- Local candidate patch sha256 `dc77e59116234fc5610b5006e2d57423016c6eb39cbb38a326572111bb49e92d`, applied only in an isolated controller checkout
- Unfixed test ELF sha256 `47b6c727a7d1e568433883fb6e4693b08348e93ab8b8e41a3c3b8638217acc46`; patched test ELF sha256 `3dd68a50be15bd8f5a456f23ff4a3a278516a37881d423d154f64af51c7e9da9`
- Evidence ledger sha256 `eb8f947917ae0a6ceb8a6517f89542a9905815f9907d202035e3ff288cf4404f`
Hermit replays, each re-judged from its own bytes (CENSORED, NONDETERMINISTIC and OTHER count as neither PASS nor FAIL):
- fail-witness seed 11: unfixed FAILx5, NONDETERMINISTICx2; patched PASSx3
- pass-witness seed 1: unfixed PASSx2; patched PASSx2
- fresh seed 12: unfixed PASSx1; patched PASSx1
- fresh seed 13: unfixed PASSx1; patched PASSx1
- fresh seed 14: unfixed PASSx1; patched PASSx1
- fresh seed 15: unfixed OTHERx2; patched OTHERx1
Verdict: SUPPORTED
  - patched fresh seed 15 was OTHER
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
