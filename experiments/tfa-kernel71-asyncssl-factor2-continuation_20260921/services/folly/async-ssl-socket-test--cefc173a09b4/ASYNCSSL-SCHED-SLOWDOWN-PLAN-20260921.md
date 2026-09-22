# AsyncSSL ObserverAttachedBeforeConnect factor-2 scheduler continuation plan

## Scheduler-sensitive causal boundary

The exact selected case attaches its observer before connecting a client to a
`TestSSLServer` whose EventBase runs on a dedicated thread. The timestamped
write is observed through `SO_TIMESTAMPING`, epoll error readiness, and
`recvmsg(MSG_ERRQUEUE)`. A bounded native diagnostic found an ordinary target
FAIL at 17 ms selected-test time (1.29 seconds outer wall) and a clean traced
PASS at 20 ms (0.21 seconds outer wall). During the traced selected-test phase,
two TIDs made one `clone3`, 34 futex, three `eventfd2`, two `epoll_create`, 24
`epoll_ctl`, and 15 `epoll_wait` calls, with no timer or sleep syscall. These
native observations are planning diagnostics only, not formal evidence or a
native-to-Hermit rate claim. The diagnostic-only `ASAN_OPTIONS=detect_leaks=0`
used to make strace teardown clean is not part of this cell.

## Frozen input vector

The single cell uses strict ptrace chaos, random scheduling, deterministic
per-thread slowdown with factor bound 2.0 and epoch length zero, no
`--target-timeslice` override, and a 200,000,000-ns maximum timeslice. Seed,
RNG seed, and fuzz seed are fixed to zero. Only `--sched-seed` varies in the
explicit without-replacement order 0 through 15. This is the complete declared
finite population; it makes no probability or independence claim.

The explicit epoch `2026-09-21T21:44:37Z` was captured at the planning boundary
and is replayed literally. Normal continuous virtual-time progression remains
enabled with explicit multiplier 1.0; no frozen time, metadata-time override, or
diagnostic sanitizer environment is used. The exact disabled test is selected
with one gtest filter and `--gtest_also_run_disabled_tests`.

The launcher retains stdout through a constant-memory streaming reader with a
lethal 67,108,864-byte cap. On overflow it kills the safehermit transient
cgroup, continues draining without retaining further bytes, and records the
cap and kill result; incomplete stdout is always censored. The plan receipt
also seals the exact Git blobs of the AsyncSSL create, run, and admission
helpers, which every later run and admission must match.

## Oracle integrity

PASS requires the exact selected `[ OK ]` line, one-passed-test summary, clean
exit 0, strict BitwiseInfoV1 parity, and no sanitizer verdict or source-pinned
failure block.

The sole FAIL class is `gtest-byte-events-three-missing-ack`. After the exact
selected RUN line, stdout must contain exactly one
`AsyncSSLSocketTest.cpp:3797` block saying `observer->byteEvents` was expected
to have size at least four but actually had size three, followed by exactly one
line-3812 equality block saying `offsetExpected` is 501 while the ACK
max-offset is the all-zero 16-byte object. It must then contain the exact timed
selected `[ FAILED ]` line and one-failed-test summary and exit with code 1.

Either failure block alone, a different source line or value, a missing
summary, signal, sanitizer, Hermit, wrapper, loader, dependency,
infrastructure, OOM, truncation, and outer timeout result is censored. No older
diagnostic or formal attempt is imported into this prospective cell.
