# Thrift boundary-recovery preflight

This is a prospective execution-boundary guard for the first admissible seed-0
tuple. It does not admit or reinterpret the rejected 2026-09-21T17:19:56Z
launch. That launch remains an ineligible `LAUNCH` event because its report said
`bound.disk=NOT_APPLIED`.

The already sealed route and guest cell remain unchanged. The retry adds this
host-only wrapper environment assignment:

```text
SAFEHERMIT_LOG_ROOT=/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit
```

It controls where the parent-repository `bin/safehermit` places its bounded
wrapper log; it is not a guest environment variable or Hermit argument. The
sealed route still invokes `bin/safehermit`, uses an 8G disk guard, selects only
`RequestInstrumentationTestWithFinishedDebugPayload.snapshotStartedProcessingTest`,
and varies only `--fuzz-seed`.

At 2026-09-21T17:39:54Z a disposable preflight qgroup was created at:

```text
/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-thrift-safehermit-preflight-2616359-1790012394920997581
```

The creation applied subvolume qgroup `0/406` with limit `8000000000`. A direct
report returned:

```text
referenced=16384 exclusive=16384 limit=8000000000
```

The destroy operation returned:

```text
bounded-run-space: destroyed: /home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-kernel71-thrift-safehermit-preflight-2616359-1790012394920997581  reclaimed about 16384 bytes (qgroup exclusive; ~1.5% optimistic)
```

The path was then verified absent. `findmnt` identifies the containing mount as
`/dev/loop1 btrfs`, and the retained verifier qgroup independently reports
`referenced=13860864 exclusive=13860864 limit=8000000000`.

The one allowed recovery retry is admissible only if its safehermit report says
the disk bound is applied, along with the sealed wall, stderr, memory, swap, and
retained-log bounds. Any second `bound.disk=NOT_APPLIED` result or other wrapper
boundary failure ends this candidate's retry; it must not be sampled repeatedly.
