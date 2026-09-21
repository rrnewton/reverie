# TLS terminator native concurrency profile

This diagnostic profile was collected after the cell was sealed and does not
alter its plan, oracle, seed population, or evidence classification.

The exact selected case
`TlsTerminatorTest/TlsTerminatorTest.terminateTls/h1_innerTls_http_connect`
passed natively in 182 ms (0.24 s outer wall, 168,272 KiB maximum RSS). A
bounded passing `strace` run completed the same case in 225 ms and recorded:

- 28 thread IDs and 27 `clone3` calls;
- 1,157 futex calls: 411 waits and 746 wakes;
- nine `eventfd2` calls and nine epoll instances;
- 86 `epoll_ctl` calls and 438 `epoll_wait` calls.

The raw profile is retained under
`/home/newton/work/dev-hermit/ignored/tfa-kernel71-campaign/screen-proxygen-tls`.
This is a short, genuinely multithreaded client/server callback workload, not a
single-thread wall-time assertion. It therefore passes the campaign's revised
candidate-mechanism screen, although its completed formal 15-seed cell has no
PASS arm.
