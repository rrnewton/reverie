# TFA phase 2: thrift `snapshotStartedProcessingTest` (candidate-only)

## Symptom (trigger)
`InstrumentationTest.cpp:836` `ASSERT_EQ(reqSnapshots.size(), 3)` fails with 2 under
Hermit sched-seed 1.

## Originating invariant violation
The test assumes that once `baton1` fires (the handler for req1 has run), req1 is no
longer counted by `ThriftServer::getActiveRequests()`. So it treats
`while (getActiveRequests() < 2)` as meaning "req2 and req3 are both at the server".
That assumption is false for rocket request-response:

- `decActiveRequests` runs only from `~RocketServerFrameContext`, which runs inside the
  request destructor. That destructor runs after `RequestsRegistry::Deleter` has
  already called `moveToFinishedList`.
- The decrement can be deferred further until the response write is flushed
  (`RocketServerConnection::requestComplete` goes through `writeBatcher_` and
  `inflightWritesQueue_`).
- `baton1` is posted inside the handler, before serialization, the IO hop, request
  destruction and the socket write.

Deduction that req3 is the missing entry:

- `getServerSnapshot` reads the registry on the worker EventBase.
- That same IO thread increments `activeRequests` and registers each rocket request in
  one synchronous callback. So every counted request is visible to the snapshot.
- req1 is always present (active, or in the finished list with limit 1), and req2 is
  blocked in its handler. A size of 2 therefore means req3 had not reached the server.
- So the loop must have exited on {req1, req2}.

## Fix (patch.diff, test-only)
After `baton1.wait()`, wait until `getActiveRequests()` reaches 0. This happens only
after req1's request object is destroyed, and therefore after it is in the finished
list. After that, the existing `< 2` loop really does mean req2 and req3.

This also makes the test's "finished request" precondition hold before the snapshot;
before, the finished-timestamp check was racy too. No assertion, count or timeout was
changed. The spin-yield matches the loop already in the test. It is a condition wait,
not a sleep or retry.

## What I ran
- `witness/pass-a1`: pre-fix replay, seed 0 → PASS, determinism verified, 16.6s.
- `witness/fail-a1`: pre-fix replay, seed 1 → FAIL `gtest-snapshot-size-2`, determinism
  verified, 17.9s. Its stdout sha256 matches the historical evidence (`c8a68dbd…`).
- No CENSORED runs. No other seeds run. Fresh seeds 2-5 were not touched.

## Not run (the controller runs these)
Buck build, arc lint, post-fix PASS/FAIL replays, fresh stress seeds 2-5, and the unit
test. The investigator is not permitted to build, so the fix has not been demonstrated.
No direct instrumentation confirms which snapshot entry is missing; that conclusion
comes from reading the code (see `hypotheses.json`, H1 `next_experiment`).
