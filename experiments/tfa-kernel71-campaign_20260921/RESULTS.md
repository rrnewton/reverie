# Kernel 7.1 TFA campaign SUCCESS — 2026-09-21

## Conclusion

The formal TFA campaign satisfies **SUCCESS** at evidence HEAD
`d74d1dab5dc865d8854df9649dbd9b36b1f9496a`: three distinct canonical Buck
targets are WORKING.  Each target has, in one prospectively sealed strict-ptrace
cell, a repeatable PASS and a repeatable predeclared target-owned FAIL at
distinct predeclared scheduler seeds.

This conclusion applies the unchanged contract at
`9d7f603c43618715b9245c74acb3e164c17d2ae8:ai_docs/tfa-acceptance-bracket-2026-09-17.md`
(blob `4a53f0c25623ad6371ee86b4556adada1218745d`).  That contract defines SUCCESS
as `W >= 3` distinct canonical targets and counts a canonical target at most
once even when several independent cells were attempted.

Across all 25 MATERIAL kernel-7.1 records committed at the evidence HEAD, the
deduplicated campaign accounting is:

```text
A/W/B/U = 10/3/0/7
S = 201
C/T = 93/294
R = 512
Campaign conclusion: SUCCESS, because W=3 distinct targets.
```

`S` is deduplicated by canonical target, frozen cell, swept seed flag, and seed
value.  `C`, `T`, and `R` include all committed kernel-7.1 cells.  Repeated
cells for AsyncSSL, HTTPCoro, and Thrift do not increase `A` or `W`.

The required target-denominator ratios are `W/A=3/10`, `B/A=0/10`, and
`U/A=7/10`.  The ten distinct attempted targets and their effective outcomes
are:

| Effective outcome | Canonical target |
| --- | --- |
| WORKING | `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests` |
| WORKING | `fbcode//thrift/lib/cpp2/test:InstrumentationTest` |
| WORKING | `fbcode//folly/io/async/test:async_ssl_socket_test` |
| NOT-DETERMINABLE | `fbcode//cachelib/common/tests:event-tracker-test` |
| NOT-DETERMINABLE | `fbcode//folly/io/async/test:timerfd_timeout_manager_test` |
| NOT-DETERMINABLE | `fbcode//logdevice/logservice/server/log_store/test:log_store_test` |
| NOT-DETERMINABLE | `fbcode//logdevice/admin/health_monitor:tests` |
| NOT-DETERMINABLE | `fbcode//mcrouter/facebook/test/toaster_tests:mc_refill_route_wrapper_test` |
| NOT-DETERMINABLE | `fbcode//proxygen/facebook/lib/transport/monitor/test:proxygen_transport_monitor_test` |
| NOT-DETERMINABLE | `fbcode//proxygen/facebook/fwdproxy/tlsterm/test:tls_terminator_test` |

## Three qualifying targets

| Canonical target | Frozen cell | PASS seed | FAIL seed and predeclared class | Plan receipt |
| --- | --- | ---: | --- | --- |
| `fbcode//proxygen/lib/http/coro/test:HTTPCoroSessionTests` | `httpcoro-kernel71-ptrace-strict-midpoint-slowdown-v1` (`c2628d6421a726f0f9dd497cfbed4bc952234c35df621c16e030c2d1ae93988e`) | 3 | 0 — `reset-rate-limit-line1685-body-event-fatal` | commit `910f2ccb4e4341cd9eac8707e93ab4194e49fd7f`, blob `20f2d07ab2453b24d753c46401a02da02dc87465` |
| `fbcode//thrift/lib/cpp2/test:InstrumentationTest` | `thrift-instrumentation-kernel71-ptrace-strict-chaos-sched-slowdown-now-v1` (`e882c837b3fee09e5779325938b2c7090cb09bec01210e5b5e65e5f8be6a6b5e`) | 0 | 1 — `gtest-snapshot-size-2` | commit `000fb6342ac55f9807ed0f927d776cfe94315a70`, blob `3827afb144681c6580aaefbd81a8f9b1227afcd9` |
| `fbcode//folly/io/async/test:async_ssl_socket_test` | `asyncssl-kernel71-ptrace-strict-chaos-sched-slowdown-factor8-slice5000000-now-v1` (`3ef3715f71a7349258292ee5fe3095ef701f1fb8096a17a9975376231504dd11`) | 1 | 11 — `gtest-byte-events-three-missing-ack` | commit `2909759234b6cea4152b488ff3a1cf42a6634b55`, blob `b3900ab9e9ca4b70ad0929610694bde4d1248396` |

Each listed arm has exactly two qualifying Hermit executions: replica 1 is
DISCOVERY and replica 2 is CONFIRMATION within Hermit's same outer `--verify`
invocation.  All twelve witness executions report one selected test, no
timeout, an unfired stdout cap, authenticated safehermit and verifier qgroups,
and `BitwiseInfoV1=matched`.  Their matchers are complete, have
`bitwise_parity=true`, and have an empty relaxation list.

## Validation and authentication

The unchanged schema-v2 validator passes each qualifying root:

```text
HTTPCoro: A=1 W=1 B=0 U=0 S=2 C/T=2/4 R=4
Thrift:   A=1 W=1 B=0 U=0 S=2 C/T=0/2 R=4
AsyncSSL: A=1 W=1 B=0 U=0 S=6 C/T=6/12 R=22
```

Two independent exact-HEAD audits additionally established facts outside the
offline validator's self-authentication boundary:

- every plan receipt commit is a strict ancestor; the recorded PLANNED blob
  matches the exact receipt-commit path, and all three create/run/admit helper
  blobs match both the receipt commit and current HEAD with no intervening
  helper change;
- the plan, route, and cell SHA-256 values recompute exactly;
- all tracked artifacts in the three qualifying records rehash and restat
  correctly: HTTPCoro 38/38, Thrift 26/26, AsyncSSL 149/149, with zero missing,
  size-mismatched, digest-mismatched, truncated, or uncommitted artifacts;
- the sealed Hermit and test ELF sizes and SHA-256 values match their live local
  binaries; and
- the retained winner qgroups remain readable and below their exact
  8,000,000,000-byte limits (53,035,008; 36,397,056; and 75,313,152 referenced
  bytes respectively at audit time).

The goalpost audit found no weakened assertion, widened tolerance, exemption,
skip, comparator relaxation, failure relabelling, or deleted check.  In
particular, AsyncSSL qualifies only on the exact paired line-3797 size-three and
line-3812 missing-ACK failure; the numerous line-3592 failures remain CENSORED.

The repaired historical HTTPCoro attempts absent from their record remain
uncounted.  Diagnostic fixed-binary sweeps remain non-formal and uncounted.

## Publication boundary

This is a clean, committed local campaign result.  No branch was pushed and no
pull request was created, as directed.  The generic validator deliberately
prints `evidence_authentication=NOT_CHECKED`; the independent local audits above
close the local producer/receipt/artifact checks, but remote publication
authentication remains pending until a later explicit push instruction.
