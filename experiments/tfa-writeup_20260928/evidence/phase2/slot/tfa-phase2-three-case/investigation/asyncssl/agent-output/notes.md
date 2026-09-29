# TFA phase 2 — asyncssl (candidate-only)

Selector: `AsyncSSLSocketByteEventTest.ObserverAttachedBeforeConnect`
(`fbcode/folly/io/async/test/AsyncSSLSocketTest.cpp`). Revision: `7f239d5b37df`.

## Root cause (top hypothesis)
This is a test synchronization race. The server's TLS 1.3 post-handshake
NewSessionTicket records are still unread by the client when the test issues
its first timestamped write. The client ends up in `AsyncSocket::handleRead`
consuming those tickets at the same moment that the server's echo, which
carries the TCP ACK, arrives. `handleRead` keeps looping (up to
`maxReadsPerEvent_`) and reads the echo. It never goes back to the error queue,
where the kernel already queued the ACK timestamp. `ioReady` drains the error
queue only on entry. So `writeAndReflect` sees the reflected byte and returns
before the ACK `ByteEvent` reaches the observer.

- Trigger (fatal symptom): `AsyncSSLSocketTest.cpp:3797` (`byteEvents` size 3,
  expected >= 4) and `:3812` (no ACK offset; expected 501).
- Originating invariant violated: `writeAndReflect` assumes that "reflection
  read ⇒ all byte events for the write delivered". That holds only when no other
  server→client data is pending at the time of the timestamped write. The ACK is
  late, not lost: it is drained after write 2, and the second block passes.

## Evidence
- Replays: `witness/prefix-pass-1` gave PASS (seed 1) and `witness/prefix-fail-1`
  gave FAIL (seed 11, `gtest-byte-events-three-missing-ack`). Determinism was
  verified for both, and neither was CENSORED.
- Syscall trace (`witness/syscalls-fd16-fd17-prefix-fail-1.txt`, client fd 16,
  server fd 17, recvmsg flag 8192 = `MSG_ERRQUEUE`). The server sends two
  239-byte tickets (#90, #102) before the client's timestamped 23-byte
  `sendmsg` (#3151). The client drains SCHED and TX (#3162, #3164). At `ioReady`
  entry the error queue is empty (#3171). The client then reads ticket 1
  (#3172-3173) and ticket 2 (#3176-3177). The server echoes (#119), and the
  client reads the echo (#3179-3180) with no error-queue read in between. After
  write 2 the error queue yields 3 messages (#3198-#3202), which is the late ACK
  plus SCHED and TX.
- In PASS (seed 1) the client writes before the server finishes the handshake.
  The tickets then carry the ACK, and it is drained (#3159-#3165) before any
  read.
- The sibling test `ObserverAttachedAfterConnect` documents the identical
  sequence and mitigates it the same way this patch does.

## Fix (`patch.diff`, test-only, +6 lines)
Before the first timestamped write:
1. Call `serverHandshakeCb_->waitForHandshake()`.
2. Do one untimestamped `writeAndReflect(wbuf, WriteFlags::NONE)`, which drains
   the post-handshake tickets.

`WriteFlags::NONE` requests no timestamps, so it creates no `ByteEvent`s.
Offsets are computed from `getRawBytesWritten()` after the fact, and the second
block counts relative to `startNumByteEvents`. No assertion, timeout, or
expected verdict changed, and no sleeps or retries were added.

Not changed: `MultiByteWrites` and `MultiByteWritesEnableSecondWrite` use the
same connect-then-timestamped-write pattern and look exposed to the same latent
race. I left them alone to keep this patch scoped to the selected case.

## What I ran
- The two pre-fix Hermit replays above, one at a time, via `witness.sh`. No
  other seeds were used, and none of the reserved seeds 12-15.
- Read-only source inspection, and extraction of the syscall trace from the
  replays' own verify logs.

## What I did not run
Buck build, lint, and any post-fix replay or stress run. The brief forbids them.
`validation.json` lists these as NOT RUN, and the controller's results are
authoritative.

## Search-method disclosure
Before the coordinator's search-method correction, I ran `grep -n` on three
single, explicitly named files: `AsyncSSLSocketTest.cpp`, `AsyncSSLSocketTest.h`
and `AsyncSocket.cpp`. I also used `grep` on my own Hermit verify logs in
agent-output. None of these was recursive, and I never used `find`, `rg` or
recursive globbing over fbsource. After the correction I made no further fbsource
searches.
