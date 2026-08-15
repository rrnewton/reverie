# Known Limitations and Future Work

Status snapshot: 2026-07-21. This ledger distinguishes current `main` from
validated but unlanded feature branches. The prioritized dependency plan is in
[hermit-v2-roadmap.md](hermit-v2-roadmap.md).

## Platform and environment

- Production support is x86-64 Linux. Aarch64 is incomplete and macOS is not
  supported.
- Precise preemption needs accessible user-space PMU RCB counters. CPU model,
  perf policy, hypervisor behavior, and counter skid affect correctness and
  performance.
- CPUID faulting varies by host. RDRAND/RDSEED tests must distinguish feature
  exposure from interceptability.
- Hermit does not make a changing filesystem or external network peer
  deterministic. Pin or record all external inputs.
- Default CLI isolation needs nested namespaces and mounts. In the audited
  rootless container, host-network mode worked with relaxed seccomp and
  unmasked paths, while local-network `sysfs` required privileged execution.

Future work:

- land and document a reduced core-only/no-namespace mode;
- virtualize PID/TID and `/proc` semantics before claiming parity in that mode;
- add one capability preflight that reports namespaces, mounts, ptrace, PMU,
  CPUID, Yama, seccomp, and relevant host policy.

## CI and coverage

- The registered Hermit and Reverie self-hosted runners were offline at this
  snapshot, leaving trusted hardware jobs queued.
- A prior namespace probe passed, but full `cargo test -p hermit` still failed
  all six `hermit_modes` cases with mount `EPERM`. The probe did not cover the
  network namespace and nested `sysfs` path used by Hermit.
- Schedule-search CI fails on the current AMD runner because Reverie reports
  `AmdSpecLockMapShouldBeDisabled`, then panics in stack handling and times out.
- Cargo coverage is not equivalent to the historical Buck matrix. PMU,
  namespace, unlanded, ignored, and quarantined tests require separate counts
  at an exact SHA.
- The historical PMU wrapper can falsely skip all tests by matching obsolete
  `perf list` text.

Future work:

- run privileged or host-side self-hosted workers with dedicated state and a
  verified mount/network/sysfs capability matrix;
- land a portable RCB skid benchmark and calibrate per host/CPU;
- publish exact-SHA coverage ledgers and make every skip explicit;
- keep public external PRs off rootful self-hosted runners.

## Deterministic execution and performance

- Ptrace remains roughly 10x on syscall/thread microbenchmarks even with
  precise preemption disabled.
- Precise AMD PMU preemption caused roughly 40x CPU-only overhead in one
  benchmark because of about 1.9 million switches and a large skid margin.
- Serialized guest threads cannot reproduce genuine simultaneous weak-memory
  outcomes. Hermit is stronger for logical ordering, lifetime, locking, and
  filesystem invariants than for weak-memory litmus behavior.
- PID/TID identity, some metadata, memory mappings, descriptor flags, and many
  syscall modes remain partially modeled.
- Real-binary audits found process lifecycle, descriptor tracking, recorder
  ioctl, blocking network, and multiprocess replay/cleanup gaps.

Future work:

- separate PMU skid correction from backend cost and benchmark both;
- add per-syscall mode/flag support ledgers and differential tests;
- repair the minimized arbitrary-binary failure families before expanding
  workload claims.

## Record, replay, and debugging UX

- The recording engine and immediate verify path work for small examples, but
  the saved-recording `hermit replay` CLI had a Clap configuration panic in the
  audited build.
- `hermit run --verify-allow=failure` can report successful verification and
  still return the failing guest status. `hermit-verify
  --allow-nonzero-exit` is easier for automation.
- Trace/chaos replay can reproduce output/status but fail strict log checks on
  register-flag and alignment differences.
- Guest `/tmp` is isolated; summary paths there may disappear from the host.
  Artifact roots and retention are inconsistent across commands.

Future work:

- fix and test all replay CLI entry points;
- standardize automation exit status and structured output;
- define one host-visible artifact root, manifest, retention policy, and
  cleanup command;
- normalize or explain register-flag nondeterminism in strict verification.

## Schedule search

- Event-level search works on `hello_race` and `racewrite_nostdlib`, but replay
  jitter can violate the intended interpolation path.
- A one-swap result can still contain unmatched insert/delete events.
- Explicit schedule endpoints are unimplemented and automatic seed search is
  unbounded.
- Branch-level sub-event refinement is disabled and memory accesses are not
  directly observed.
- Search lacks repeated target-predicate stability sampling.

Future work:

- validate the realized schedule for every candidate and preserve verified
  opposite-outcome endpoints;
- implement stable event identity/alignment and bounded search;
- repair branch-count splitting and final A/B validation before enabling
  sub-event search;
- add memory/instruction instrumentation only as a separately scoped project.

See [schedule-search-guide.md](schedule-search-guide.md).

## QEMU and kernel testing

- A relaxed QEMU/TCG boot succeeds, but default deterministic and record modes
  are too slow or time out.
- RDTSC uses per-thread time while `clock_gettime` uses global time, causing
  guest TSC/device-clock skew. A global-time RDTSC prototype is unlanded.
- `ppoll`, futex absolute deadlines, vectored I/O, vhost-user ancillary data,
  inherited fds, PID/TID/rseq, and JIT mapping behavior need complete landed
  semantics and tests.
- virtme-ng adds host-kernel lookup, `CLONE_VFORK`, initrd-fd, helper, and live
  filesystem state. Vfork and syscall fixes are still draft PRs.
- KVM mode is dominated by stateful ioctls and shared memory effects; generic
  syscall recording cannot reproduce it.
- Hermit is not a replacement for syzkaller, KCSAN, KASAN, LOCKDEP, Razzer,
  Snowboard, or KRACE. It can add deterministic host-QEMU schedule artifacts
  after strict TCG run/verify works.

Future work:

- land coherent time, syscall, and vfork fixes with QEMU regression evidence;
- use pinned read-only VM inputs and remove or fully model live vhost-user
  state;
- reach repeatable TCG run/record/replay/verify before kernel race claims;
- start with known logical kernel concurrency bugs and guest-native oracles.

See [qemu-integration-status.md](qemu-integration-status.md).

## Alternative backends

- SaBRe is a separate synchronous x86-64 API. Basic restored workloads and a
  narrow speed probe work, but signals, lifecycle, state, generated code, PMU,
  CPUID/RDTSCP, and shared Reverie parity remain incomplete.
- The KVM backend is a protocol skeleton. The required gVisor Sentry bridge is
  unimplemented and would expose gVisor rather than host-kernel semantics.
- A fast syscall backend will not fix precise-PMU overhead.

Future work:

- finish bounded SaBRe runtime stabilization without changing shared core
  abstractions merely to claim parity;
- run a time-boxed DynamoRIO prototype against the same conformance suite;
- choose a fast-backend product boundary from workload and lifecycle evidence;
- defer the multi-month Sentry/KVM bridge until benchmarks justify it.

## Documentation maintenance

- Treat [pr-status.md](pr-status.md) as a dated snapshot and refresh live
  GitHub state.
- Mark every claim as landed, draft, local experiment, or proposal.
- Include exact repository SHAs, hardware/kernel/runtime state, commands,
  counts, and known skips in future handoffs.

## Monitors that fail quietly

Two ci-hub gates share a defect class that is not about the thing they monitor:
**a monitor that fails silently is indistinguishable from a healthy system.**
Both were observed on 2026-08-15.

- **`ledger_spool_backlog` emits INTERMITTENTLY, not merely once-skipped.**
  Measured across three consecutive ops ticks on 2026-08-15: **absent, present
  (`depth=8`, `oldest_age=30371.7s`), absent** — while the condition
  demonstrably persisted through at least the first two, confirmed by direct
  inspection during the gaps. **A single missed emission is dismissible; an
  alternating pattern is not.** The third absence is not evidence either way,
  because the spool was drained between ticks — which is precisely the problem:
  once a gate is known to skip, no individual silence from it can be read.
  Earlier detail: the alarm fired every ops tick, was absent for
  exactly one tick, then returned with the same `depth=8` and the same
  `oldest_entry`, its age advanced by one tick interval. Checked directly during
  the gap, the spool was unchanged — depth 8, `published/` empty, directory
  mtime unmoved — and the detector itself ran fine and returned
  `state: "stalled"` with rc 0. So the instrument is healthy and the emission
  path is not. **A reader who happens to check during the gap concludes the
  condition cleared.** That nearly happened.
- **`gate_runner_presence` emitted no-signal all night against an unresolved
  placeholder**, which reads the same way: absence of an alarm consumed as
  absence of a problem.

The operative rule: **for these two gates, an absent alarm is not evidence.
Confirm the underlying condition directly before concluding it cleared** — for
the spool that is `ls` on the spool directory plus a direct run of
`ci-hub/health/ledger_spool_backlog.py`, which is a few seconds and does not
depend on the tick.

Neither has been diagnosed. Recorded so the next reader does not spend the
investigation twice, and so an absent alarm is not mistaken for a resolved one.

- **Related method finding:** three defects this session were caught by
  mechanical checks after the responsible agent had already diagnosed that exact
  failure mode. Recorded with its incidents and its `n=3` caveat under "A machine
  check catches what knowing the lesson does not" in
  [2026-08-15-operating-rules-with-their-incidents.md](transient/2026-08-15-operating-rules-with-their-incidents.md).

- **The ORC liveness probe fails OPEN on a mistyped flag.** Its agent name is
  positional; passed as `--agent` argparse errors, and an exit status read
  through a pipeline reports **rc 0**, which in that probe's vocabulary means
  **verified dead** — the one code that permits reclaim. **Capture the probe's
  status directly, never through a pipe.** **Severity is bounded: the probe is
  ADVISORY and `release-worktree.rs` enforces independently via
  `verify_recorded_owners_absent`, so a mistyped probe misleads a reader but does
  not defeat reclaim enforcement.** Full case, correct invocation and scope in
  [LIVENESS_PROBE_USAGE_ERROR_FAILS_OPEN_20260815.md](LIVENESS_PROBE_USAGE_ERROR_FAILS_OPEN_20260815.md).

- **Before relying on a named authority, grep for a caller.** Surveyed as a set
  in [NAMED_AUTHORITY_CALLER_SURVEY_20260815.md](NAMED_AUTHORITY_CALLER_SURVEY_20260815.md):
  11 of 16 executable authorities checked, **one confirmed defect (the liveness
  probe)**, and three zero-caller CLI tools found HEALTHY because they are
  intended-interactive and each has a verifying oracle. The survey covers
  executables only and structurally cannot find prose authorities. Three instances in
  one session of documentation naming an authority the code does not implement:
  the approval blob rule (named in policy and a skill file, no implementation),
  the `virtualize_time` rationale, and `agent_liveness_probe.py` (named in
  `AGENTS.md:81` as the reclaim authority; no production caller invokes it).
