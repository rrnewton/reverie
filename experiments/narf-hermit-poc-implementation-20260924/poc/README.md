# Canonical trace acceptance helper

`canonical_trace.rs` is the fail-closed comparator for the bounded runtime POC.
It recognizes only the shared fixture's `write(1, fixed_message, 16)` return and
non-returning `exit(0)`. A missing, extra, malformed, reordered, or changed
record is an error; it does not silently drop unknown guest syscalls.

Build and unit-test it with:

```sh
rustc --edition=2021 -D warnings canonical_trace.rs -o /tmp/canonical-trace
rustc --edition=2021 -D warnings --test canonical_trace.rs -o /tmp/canonical-trace-tests
/tmp/canonical-trace-tests
```

Given complete raw captures, the positive and negative gates are:

```sh
/tmp/canonical-trace canonicalize-hermit HERMIT_INFO_LOG > /tmp/hermit-v1.log
/tmp/canonical-trace canonicalize-narf NARF_SERIAL_LOG > /tmp/narf-v1.log
/tmp/canonical-trace compare /tmp/hermit-v1.log /tmp/narf-v1.log

/tmp/canonical-trace mutate /tmp/narf-v1.log > /tmp/narf-v1-mutated.log
! /tmp/canonical-trace compare /tmp/hermit-v1.log /tmp/narf-v1-mutated.log
```

The 2026-09-24 acceptance run produced two identical 204-byte files with
SHA-256 `c4297f2fa0c65f6de58ab0933c4ceb5773f8d600d2f8db9a2524680c1e9a0be5`.
The negative mutation was rejected at byte 143. Raw logs and binaries remain in
`/tmp` or ignored build trees, outside the durable report.
