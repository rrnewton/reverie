# Terminal and candidate-gap audit — 2026-09-21

At source anchor `21f1c3388a377c137aa958c2ee0b12e37557ee5e` and
2026-09-21T16:26:37Z, the canonical validator reported:

```text
cohort=30 records=9 planned=0 A=9 W=0 B=0 U=9 S=34 C=18 T=52 R=89
conclusion=neither evidence_authentication=NOT_CHECKED
```

Here A/W/B/U count distinct canonical targets; S counts valid discovery seed
tuples, C/T counts censored/attempted discovery tuples, and R counts
guest-started Hermit executions. The latter four are not target denominators.

## Terminality audit

Only three current records are terminal for a possible NEGATIVE RESULT:

| Target | Valid / attempted | N / cap | Why terminal |
| --- | ---: | ---: | --- |
| AsyncSSL | 7 / 7 | 7 / 11 | valid-tuple requirement reached; no pending measurement |
| TimerFD | 9 / 9 | 9 / 13 | valid-tuple requirement reached; no pending measurement |
| Proxygen TLS terminator | 2 / 15 | 11 / 15 | attempted-tuple cap reached; no pending measurement |

Six MATERIAL NOT-DETERMINABLE records are nonterminal:

| Target | Valid / attempted | N / cap | Remaining contract state |
| --- | ---: | ---: | --- |
| Cachelib EventTracker | 0 / 1 | 86 / 94 | pending |
| LogStore retry | 0 / 1 | 31 / 35 | pending |
| LogDevice health | 0 / 1 | 8 / 12 | pending |
| McRefill | 0 / 1 | 13 / 17 | pending |
| Proxygen transport monitor | 0 / 1 | 44 / 52 | pending |
| HTTPCoro | 16 / 16 | 89 / 100 | pending; owner directed no further seeds |

The other 21 cohort targets are unattempted. Native screens neither create A
nor exhaust a formal budget. A BLOCKED result would instead require failed
durable attempts on every preauthorized route/cell plus a later campaign-owner
OUTSIDE_SCOPE adjudication. None exists.

## Candidate-gap audit

All 30 cohort targets were accounted for under the current admission gate:

- 2 short scheduler-sensitive candidates are already exhausted: AsyncSSL and
  Proxygen TLS terminator;
- 3 selected cases are scheduler-insensitive: TimerFD, Proxygen transport
  monitor, and the owner-rejected single-thread HTTPCoro slowdown;
- 5 have ptrace-dominating fanout: LogStore health (657 TIDs), Shadow affinity
  (338 TIDs and 11.22 s), two Thrift cases with 331 TIDs, and the related
  Thrift resource-pool scaffold;
- 13 exceed the native runtime/setup gate or do not execute the selected case,
  including the 12,366,336-byte McRefill binary, which produced no GoogleTest
  marker by 25 s and starts six McServer instances; and
- 7 have incompatible routes: two launcher-only Cogwheel targets, three
  AArch64 MySQL MTR targets, intentional-`ENOSYS` io_uring, and the prior
  DRocksDB deadlock/1 GiB retained-log route.

No new or resumable canonical target therefore meets all of: at most 10 s
native wall, demonstrated target-owned competing-thread ordering, and a thread
footprint that does not make ptrace cost the likely result. The Proxygen
transport-monitor diagnostic now additionally proves that its selected case is
synchronous; its formal loader stop is Hermit's post-open `inject_fstat` path
converting the enclosing `openat` to `EFAULT`, not an absent DSO.

## Acceptance consequence

SUCCESS is not established because W=0, below the required W>=3. NEGATIVE
RESULT is not established because A=9, only 3 records are terminal, 6 records
are under budget, and 21 targets are unattempted. The explicit stop on further
HTTPCoro and io_uring runs also prevents completing the negative route as
currently sealed. No outcome may be weakened, majority-voted, or retroactively
planned to change that result.

Existing predeclared prefixes for five nonterminal records remain formally
authorized and pending; this audit does not rewrite the candidate-priority gate
as a stop rule for those already sealed cells. Exhausting them could reduce the
remaining NEGATIVE accounting, but they do not meet the current
scheduler-sensitive SUCCESS priority and cannot complete NEGATIVE while the
under-budget HTTPCoro cell is explicitly stopped.

Reaching a terminal campaign result therefore requires either a newly supplied
local cohort binary/case that passes the priority gate, a campaign-owner change
to that gate or the HTTPCoro stop, or the required owner adjudications for
fully evidenced blockers. The current result is a campaign constraint
collision, not a SUCCESS or NEGATIVE RESULT.
