# Hermit controlled-time input audit

This is a read-only audit of the sealed Hermit binary and its exact source
revision `4630ee2a61f93221a43e2baec0a15f34102a2c61` (`Accept virtual RLIMIT_CORE
updates`).  The object was read from the parent repository's local Hermit object
store; no network or source mutation was needed.  This audit does not change
Hermit.

## Existing controls

`detcore-model/src/config.rs` exposes the guest-visible realtime origin through
`--epoch` (or `HERMIT_EPOCH`), guest logical-clock rate through
`--clock-multiplier`, RCB accounting through `--no-rcb-time`, and virtual-time
and file-metadata opt-outs through `--no-virtualize-time` and
`--no-virtualize-metadata`.  Scheduling/time progression can also be controlled
with `--max-timeslice`, `--target-timeslice`, `--sched-seed`, the scheduler
heuristic, preemption record/replay, and the chaos slowdown vector
`--chaos-per-thread-slowdown`, `--chaos-slowdown-max-factor`, and
`--chaos-epoch-length-ns`.  `--fuzz-seed`, `--fuzz-futexes`, and
`--chaos-target-races` control different nondeterminism surfaces.

The remaining time/scheduling controls are `--imprecise-timers`,
`--sysinfo-uptime-offset`, explicit PMU `--interrupt-at=tid:rcbs` points,
`--skid-margin` (early PMU overflow and single-step compensation),
`--pin-threads`, strict thread sequentialization, `--happens-before`,
`--runs-post-fork`, and sticky-random scheduler probability.  They remain at
their defaults or unset in this cell: HTTPCoro does not consult sysinfo uptime,
has no relevant fork choice or competing guest thread/HB eligibility, and
imprecise timers, manual interrupt points, or a changed skid policy would
replace rather than isolate the 100 ms virtual timer mechanism under test.
Hermit's `--timeout` is a wall-time termination input, not guest virtual time;
this cell leaves it unset and uses the separately authenticated 120-second
safehermit outer deadline.

There is no independent virtual file-mtime seed or offset.  With metadata
virtualization enabled, the initial virtual timestamps derive from the epoch
and subsequent writes advance with the same virtual clock.  Therefore epoch,
file times, and logical-clock progression remain replayable when their complete
effective configuration is retained.

## Current epoch gap

`detcore-model/src/config.rs:204-211,1259-1270` gives `--epoch` a fixed default
of `2026-01-01T00:00:00Z`.  The `Display` implementation at approximately
`detcore-model/src/config.rs:888-891` omits that value when it equals the
default.  `hermit-cli/src/bin/hermit/run.rs:4832-4837` writes `--save-config` as
Rust debug text rather than a reloadable execution manifest.  The run evidence
and record/replay metadata do not currently persist an explicit resolved
run-start epoch.  Thus a default invocation neither captures a realistic host
epoch nor emits enough information for a future replay to prove which epoch it
used.

The prospective HTTPCoro cell works around this gap without changing product
behavior: it samples host UTC once at the planning boundary, commits the literal
RFC3339 value, and passes it explicitly to both verification replicas.

## Proposed production behavior

At the top-level CLI run boundary, resolve an omitted epoch exactly once from
host UTC before any backend or verification replica launches.  Carry the
resolved value as immutable effective configuration; render it in the effective
command/run summary and persist it in a versioned run/record manifest.  Verify,
record, and replay must give every replica the same stored value.  An explicitly
provided `--epoch` or `HERMIT_EPOCH` remains authoritative.

Keep `detcore::Config` construction deterministic for library callers and
tests: ambient `now` belongs at the CLI boundary, not in `Config::default()` or
`Config::from_env()`.  Do not reset, quantize, or freeze continuous virtual-time
progression, and do not infer replay epoch from replay wall time.  Add tests for
one-time resolution, explicit-value precedence, emitted/persisted value
identity, and replay reuse.
