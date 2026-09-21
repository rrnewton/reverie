# Thrift slowdown cell preflight

Captured before the schema-v2 PLANNED record was committed and before any
Hermit execution.

- verifier-log subvolume: `/home/newton/work/dev-hermit/ignored/safehermit-loopback/mnt/safehermit/brs-tfa-k71-thrift-slowdown-2936582-1344290-1790027049611882514`
- creation bound: `8G`
- `bounded-run-space report`: `referenced=0 exclusive=0 limit=8000000000`
- backing mount: `/dev/loop1 btrfs`
- explicit run epoch captured from UTC NOW: `2026-09-21T21:44:37Z`
- material executions performed while preparing this record: none

The route preserves the existing Thrift test ELF, exact selected gtest, strict
BitwiseInfoV1 matcher, target-owned line-836 failure class, host identity, and
resource limits. It adds one prospectively frozen scheduler-sensitive cell:
per-thread slowdown with factor 10 and epoch length 0, a 1,000,000 ns target
timeslice, a 200,000,000 ns maximum timeslice, clock multiplier 1.0, fixed
seed/rng/fuzz roots 0, no `--chaos-target-races`, and the finite scheduler-seed
population 0 through 15.
