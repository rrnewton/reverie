# Hermit v2 Roadmap

Status: consolidated research and execution plan, 2026-07-21

This document consolidates the Hermit and Reverie research recorded in the
`dev-hermit` task graph. It describes the product goal, the measured current
state, the major technical choices, and a dependency-ordered implementation
plan.

## Executive summary

Hermit v2 should become a robust deterministic execution engine for real Linux
binaries, with reproducible run/record/replay, schedule stress and search, and
a lower-overhead execution backend. The longer-term Linux-kernel milestone is
to run a pinned QEMU/TCG VM under Hermit, combine it with an in-guest race
oracle, and make the host-side QEMU schedule a replayable input.

The project has a functioning base, but several historical status claims need
qualification:

- The current fork builds and its primary local validation passes. A current
  inventory on `main` counted 175 named Cargo cases: 173 pass and two doctests
  are ignored. The often-repeated "300+ ported tests" describes work attempted
  across branches and worktrees, not the authoritative coverage on `main`.
- Hosted CI is healthy. The self-hosted runner executes CPUID and PMU checks,
  but full Hermit integration coverage is not green: nested network namespace
  setup needs a `sysfs` mount that fails in the current runner container.
  Running the runner privileged or outside its container is the proven fix.
- Event-level schedule search works end to end. It localized both
  `hello_race` and `racewrite_nostdlib` to source lines with stack traces.
  Replay jitter, event alignment, disabled sub-event refinement, and weak
  endpoint validation prevent calling it a supported general feature yet.
- The restored SaBRe stack is a real runnable prototype, not just source
  archaeology. Its narrow raw-syscall path was about 13x faster than ptrace,
  but its in-process signal, thread, lifecycle, and API semantics remain the
  dominant risk. A useful synchronous backend is estimated at 5-8 weeks;
  broad parity is 12-20+ weeks with architectural caveats.
- DynamoRIO is the strongest maintained binary-instrumentation alternative.
  It deserves a time-boxed proof of concept before SaBRe becomes a strategic
  dependency.
- A `reverie-kvm` protocol skeleton exists and passes five unit tests, but it
  intentionally returns `SentryBridgeNotImplemented`. A production KVM path
  means integrating with a Linux ABI provider such as gVisor's Sentry, not
  merely issuing KVM ioctls. The estimate is 4-6 engineer-months for an MVP and
  9-15 engineer-months for broad parity and hardening.
- QEMU/TCG can boot under a deliberately relaxed Hermit configuration. Full
  deterministic mode is currently too slow and semantically incomplete. The
  measured boot uses 147,679 syscalls across 54 names; `ppoll`, futex timeout
  correctness, vectored I/O, inherited-file handling, and external helper
  state are the main path to a strict deterministic boot.

The recommended order is: restore trustworthy green gates, fix real-binary
correctness and precise-PMU overhead, harden schedule search, then advance two
parallel bets - a measured fast-backend prototype and deterministic QEMU/TCG.
Do not begin the long KVM/Sentry program or open-ended kernel fuzzing until
those gates provide stable evidence.

## Vision and success criteria

Hermit v2 is a deterministic execution platform for arbitrary Linux ELF
binaries and their process trees. Its product capabilities are:

1. Run arbitrary dynamically linked, multithreaded, signal-heavy programs with
   explicit boundaries for filesystem, network, and external inputs.
2. Record and replay executions faithfully, with actionable divergence
   diagnostics rather than hangs or opaque panics.
3. Explore scheduling interleavings in chaos mode and reproduce a selected
   outcome by seed and artifacts.
4. Search between passing and failing schedules, reduce the difference to a
   validated event ordering, and report stack/source context.
5. Provide at least one production-quality backend that avoids ptrace's
   syscall-stop cost while retaining a documented Reverie capability subset.
6. Allow non-communicating processes to run in parallel without abandoning the
   deterministic model.
7. Maintain comprehensive, resource-bounded CI for ordinary, PMU-dependent,
   namespace-dependent, record/replay, and schedule-search behavior.

The QEMU/Linux milestone is a downstream application of those capabilities:
a pinned, immutable Linux VM under QEMU/TCG whose workload, oracle output, and
Hermit schedule can be replayed and reduced. Hermit supplies schedule control,
not the kernel race oracle; KCSAN, KASAN, LOCKDEP, panic/hang detection, or a
workload invariant must supply the failure predicate.

## Current state

### Repository, build, and workflow

- `rrnewton/hermit` is the maintained product fork. Cargo is the primary
  external build, and `scripts/validate.rs` covers workspace build/tests, clippy,
  formatting, docs, and the evolving integration gates.
- `dev-hermit` is the parent harness. It pins Hermit and Reverie, owns durable
  research and experiments, and provides isolated paired worktrees.
- The fbsource Buck tree remains valuable as the historical test inventory. It
  has 745 test rules and builds fully, but a monolithic run is not a useful
  presubmit: the last full audit reported 520 passes, 36 failures, 301
  timeouts, and one omission, with extensive oversubscription effects.

### Test coverage

The authoritative current-main audit found:

| Surface | Current evidence | Interpretation |
| --- | --- | --- |
| Cargo named cases | 175 | 173 pass; two Hermit doctests ignored |
| Buck test rules | 745 | 744 executable targets plus one suite |
| Buck hardware split | 205 no-hardware, 538 PMU-only, 1 PMU+CPUID | Most historical matrix coverage needs the self-hosted lane |
| Schedule search | Two real E2E workloads pass locally | Not yet a reliable general algorithm or consistently landed CI gate |
| Arbitrary binaries | Simple tools pass; medium/complex failures filed | Current support is useful but not yet "arbitrary" |

Some test-port work exists only on external branches or unreachable/stashed
worktree commits. It must be recovered, reviewed, and landed before being
counted. In particular, the historical wave-two claim cannot be used as a
current-main denominator. The coverage program should report exact tests at an
exact SHA and separate ordinary, PMU, namespace, quarantined, and unlanded
cases.

### Continuous integration

The CI architecture is correct in principle:

- GitHub-hosted Linux runs ordinary build, tests, clippy, formatting, and docs.
- A self-hosted runner supplies CPUID and PMU hardware coverage.
- Namespace and schedule-search tests belong on the self-hosted lane.

The remaining infrastructure blocker is deeper than the original user/mount
namespace probe. Full Hermit mode also creates a network namespace and mounts
`sysfs`. In a rootless Podman reproduction, relaxing seccomp, unmasking system
paths, and adding `SYS_ADMIN`/`NET_ADMIN` still failed that mount; privileged
mode passed. Until the runner is privileged or moved outside the container,
the full `cargo test -p hermit` and schedule-search gates are not trustworthy
CI signals. A green fallback that runs only library/binary tests must not be
reported as full integration coverage.

### Arbitrary binary support

The current compatibility audit on Hermit `74324fff` found:

| Tier | Works | Fails or remains limited |
| --- | --- | --- |
| Simple | `ls`, `cat`, `echo`, `grep`, `sed`, `awk` in run and record/verify; output matched native | Host fixture paths require explicit `--bind`, by design |
| Medium | GCC compile, system Git, Python file/hash/JSON/subprocess under `run` | Record/replay divergence; `ioctl(FIOCLEX)` missing; site Git/Python wrappers hang on `CLONE_VFORK` |
| Complex | Simple pipeline under `run` | rustup proxy FD tracking, direct Cargo/Make `CLONE_VFORK`, pipeline replay and child cleanup |
| Stress | `curl --network=host`; 4-thread/400-signal test in run and record/verify | blocking connect returns `EINPROGRESS`; external inputs remain nondeterministic |

The minimized failure families are tracked as upstream issues #76 through #80:
process creation/cleanup, descriptor tracking, recorder ioctl coverage,
blocking network semantics, and multiprocess replay/cleanup.

## Backend strategy

### Ptrace baseline and performance

Optimized default-mode measurements on an AMD EPYC host show why backend and
timer work must be treated separately:

| Workload | Native | `hermit run` | Run overhead | `hermit record` | Record overhead |
| --- | ---: | ---: | ---: | ---: | ---: |
| `/bin/true`, 32 syscalls | 0.000784 s | 0.009830 s | 12.54x | 0.021831 s | 27.85x |
| CPU-only, 100M iterations | 0.236879 s | 9.409876 s | 39.72x | 9.541134 s | 40.28x |
| 30,032 syscalls | 0.083166 s | 1.751327 s | 21.06x | 1.349976 s | 16.23x |
| 256 thread create/join | 0.008435 s | 1.209288 s | 143.37x | 0.940607 s | 111.51x |

Minimal ptrace with precise preemption disabled was still 10.98x on the
syscall workload and 9.76x on the thread workload, about 27.6 microseconds per
censused syscall and 0.289 ms per created thread. Those numbers are the target
for a faster syscall backend.

The 40x CPU-only result is not syscall interception. Precise retired
conditional branch (RCB) preemption produced about 1.9 million context
switches; disabling preemption or using imprecise timers reduced the workload
to about 1.03x native. The AMD family 0x19 skid margin is 10,000, compared with
100 on Intel in the current model, creating a long single-step tail. Fast
syscall interception will not fix this. Timer calibration/algorithm work is an
independent P0 performance stream.

### SaBRe

What works now:

- The historical nine-package stack was restored.
- Upstream SaBRe `05816ee` builds; 69 tests pass unmodified, three are
  unsupported, and three portability failures pass with scratch-only fixes.
- Vendored plugin API sources restore linking; `reverie-sabre` passes 17/17
  unit tests.
- `riptrace` loads its plugin and traces `/bin/true`, `/bin/echo`, exec,
  fork/wait, exit-status propagation, and summary counting.
- A narrow, single-threaded 200,000-`getpid` probe took about 0.32-0.33 s under
  SaBRe versus 4.13-4.45 s under ptrace, roughly 13x faster. This is directional
  evidence, not a general workload claim.

What blocks production use:

- SaBRe has a separate synchronous in-process `Tool` API, not the shared async
  Reverie `Tool`/`Guest` contract.
- Signal handlers, TLS/guard state, bounded queues, `rt_sigaction`, and signal
  replacement/suppression are incomplete. A Python-class workload exposed
  guard and signal-queue failures during parity research.
- Fork/clone/vfork/clone3/exec lifecycle continuity, parent-aware state, abrupt
  exits, protected FDs, registers, stack allocation, backtraces, PMU timers,
  CPUID, and RDTSCP are incomplete or absent.
- Static rewriting does not cover every static, JIT, self-modifying, or
  dynamically generated syscall path. In-process instrumentation shares the
  guest's failure domain.
- Upstream is dormant: last push in 2022, no releases, no active successor,
  concentrated historical ownership, and GPL-3.0-or-later constraints.

The runtime-stabilization branch has fixed the exit-timeout direction, an exit
race, queue saturation behavior, guarded signal callbacks, and part of SIGCHLD
handling. It is paused for the Reverie fork migration; `rt_sigaction`
virtualization, conformance gates, documentation, and verification remain.

Recommended SaBRe boundary:

- First deliver an explicit synchronous, syscall-centric backend extension.
- Do not change shared Reverie core abstractions merely to claim parity.
- Require common ptrace/SaBRe conformance workloads before promoting a
  capability.
- Budget 5-8 weeks for a credible x86-64 synchronous backend after the initial
  gate, and 12-20+ weeks for broad behavioral parity. Exact async callbacks,
  signal control, abrupt exits, unrewritten code, and aarch64 remain separate
  risks.

### DynamoRIO and other instrumentation choices

DynamoRIO is the strongest maintained alternative. Its active BSD core has
direct pre/post syscall callbacks, filtering, argument/number/result mutation,
suppression, and repeated injection. It handles translated/generated code and
has an active Rust wrapper. Its main risks are whole-process DBI overhead,
synchronous/no-std callbacks, and bridging safely to async tool logic.

Run a time-boxed DynamoRIO proof of concept before selecting SaBRe as the
default fast backend. The acceptance suite must cover modify/suppress/inject,
clone/fork/vfork/exec, signals/EINTR, static and dynamic programs, JIT syscall
instructions, guest memory, and synchronous-to-async reentrancy. Benchmark an
empty client, a no-subscription client, and subscribed callbacks against
native, ptrace, and the SaBRe raw-syscall probe.

Other findings:

- Linux Syscall User Dispatch is the strongest purpose-built non-DBI primitive
  for a custom backend, especially as a fallback for missed/JIT syscalls. It
  requires per-thread setup and difficult signal/alt-stack/exec/clone handling.
- Frida and QBDI are active but require substantial custom syscall semantics.
- Intel Pin is proprietary. E9Patch is GPL and lacks complete dynamic/JIT
  lifecycle coverage. zpoline's page-zero/SELinux requirements rule it out as
  the production default. Pure eBPF cannot implement Reverie's replace,
  injection, memory, lifecycle, and signal contract.

### KVM backend

The KVM design's central result is that KVM provides an execution boundary,
not a Linux syscall implementation. gVisor runs application code at guest ring
3 and routes `SYSCALL` into Sentry code at guest ring 0; the Sentry implements
Linux process, memory, signal, futex, and syscall behavior.

The proposed architecture is a versioned Sentry bridge plus a Rust
`reverie-kvm` frontend. The Rust side owns tools and exposes `Guest`; the Sentry
owns virtual Linux tasks and publishes syscall, signal, thread, exec, and exit
events. A standalone Rust KVM loop would amount to building another Linux ABI
and is not the smaller option.

Current implementation evidence is deliberately modest: a 310-line protocol
skeleton negotiates version, architecture, capabilities, and message bounds;
it passes `cargo check`, five unit tests, formatting, and diff checks. The
actual Sentry bridge is unimplemented. Retain the existing estimate of 4-6
engineer-months for an x86-64 MVP and 9-15 engineer-months for parity and
hardening. Start only after workload and backend benchmarks justify that
investment.

## Schedule search and concurrency testing

### What works

The existing `hermit analyze` event-level search is operational:

- `hello_race` started 6,088,241 adjacent swaps apart, converged to one event
  pair in 22 passes, emitted both stacks, and localized
  `flaky-tests/hello_race.rs:37`.
- `racewrite_nostdlib` converged in six passes with zero desynchronizations,
  isolated adjacent write posthooks, and localized
  `tests/c/simple/racewrite_nostdlib.c:35`.
- Edit-distance tests pass 22/22, schedule-search unit tests pass 3/3, and the
  CLI builds and exposes `analyze`.

The algorithm records normalized schedule-event blocks, constructs an
adjacent-swap path between opposite-outcome schedules, executes a midpoint,
and replaces one endpoint until the boundary is one swap apart. Precise PMU
RCB timers enforce branch blocks, and the final runs capture stacks. This is a
linear-path boundary search, not exhaustive schedule enumeration or direct
memory-race localization.

### Known limitations

- Replay jitter can label a requested midpoint with the outcome of a different
  realized schedule and can move outside the intended interval.
- `swap_distance == 1` does not prove that only one event differs; insertions
  and deletions may remain. Greedy exact-block matching is weak for loops and
  split RCB blocks.
- Partial branch-count consumption, blocked-thread rules, syscall-prehook
  switching, and robust resynchronization are incomplete.
- Advertised explicit endpoint schedule inputs are unimplemented.
- Branch-level `sub_event_search` has been disabled since the original public
  history. Its bounds, applicability, jitter, and final A/B validation are not
  safe enough to enable.
- No stability sampling separates a deterministic ordering violation from
  residual nondeterminism. Current events cannot localize inside branch-free
  instruction regions or identify the actual racing load/store.

The research estimate is 4-6 weeks for a credible x86-64 PMU-backed MVP and
8-12 weeks for a supported feature. Exact memory-access localization is a
separate instrumentation project.

### Chaos stress results

Chaos already exposes logical ordering bugs across thread counts. Ten-seed
matrices found lost atomic updates, publish-ordering errors, missing barriers,
producer/consumer ordering bugs, and lost condvar wakeups at varying rates.
`hello_race` failed 5/20 seeds; `hello_race_mini` failed 11/20. An imprecise
timer profile found `cas_sequence_easy` 31/100 times and reproduced seed 2 in
5/5 attempts, while precise discovery was impractically slow.

Two boundaries matter:

- The current stress wrapper can falsely skip all PMU tests because it searches
  modern `perf list` output for the obsolete phrase `Hardware event` (issue
  #82). This must be fixed before treating the gate as evidence.
- Serialized guest threads do not simulate weak memory. A relaxed-atomic
  store-buffer outcome occurred natively but 0/40 times under Hermit chaos.
  Hermit is best at logical ordering, lifetime, locking, and filesystem
  invariants, not genuine simultaneous weak-memory behavior.

## QEMU and Linux-kernel milestone

### Measured QEMU surface

The validated QEMU 10.1.0 TCG boot made 147,679 calls across 54 syscall names.
The dominant calls were futex (62,456), `ppoll` (33,589), write (22,390), read
(19,856), `writev` (2,889), `mprotect` (2,351), and `madvise` (2,003).

Detcore has dedicated arms for 26 names representing 72.09% of calls, explicit
passthrough for 11 names representing 3.17%, and fallback behavior for 17 names
representing 24.73%. `ppoll` plus `writev` account for 99.87% of fallback call
volume. Dedicated dispatch is not the same as deterministic correctness:

- `FUTEX_WAIT_BITSET|FUTEX_CLOCK_REALTIME` is treated as relative and has a
  nanosecond-conversion bug; QEMU used it 649 times.
- `ppoll` lacks scheduler and record/replay semantics.
- Vectored I/O lacks complete resource/nonblocking/replay behavior.
- `recvmsg` does not record control data or received FDs, which matters for the
  external vhost-user filesystem helper.
- `fcntl`, `mmap`, PID/TID/rseq, file metadata/offset calls, and JIT mapping
  behavior remain partial.

KVM mode is not the first target. It made 54,981 ioctls, 83.6% of all calls;
deterministically modeling KVM fd state, `kvm_run` pages, guest memory effects,
interrupts, and VM/vCPU lifecycle is a months-scale architecture project.

### Current execution result

QEMU/TCG boots and exits successfully under Hermit when precise preemption,
thread sequentialization, time virtualization, metadata virtualization, and
CPUID virtualization are disabled. The same minimal boot can pass
`--panic-on-unsupported-syscalls`. This proves syscall interception reach, not
deterministic VM execution.

Full deterministic mode remains non-viable today:

- sequentialization causes extreme vCPU/I/O-thread slowdown;
- virtualized time causes guest TSC/APIC clock-skew warnings;
- precise preemption adds the measured CPU overhead;
- vng inherited-initrd fd handling and external helper/filesystem state need a
  reproducible launch model.

### Kernel race-testing position

Hermit should be presented as a schedule-control and replay layer, not as a
replacement for syzkaller or KCSAN and not as the first deterministic kernel
scheduler. Razzer, SKI, Snowboard, and KRACE already demonstrate directed or
systematic kernel concurrency testing.

The defensible value is composability: hold a pinned QEMU/TCG image and guest
workload constant, make the host QEMU schedule a first-class replay artifact,
and compare passing/failing schedules with the existing order-search tooling.
Use guest KCSAN/KASAN/LOCKDEP/panic/invariants as the oracle and syzkaller or LTP
as workload generators. Start with known logical concurrency bugs and two TCG
vCPUs. Do not claim weak-memory, DMA, or real-parallel coverage.

## Prioritized work

The following table orders deliverables by dependency and risk. "Planning
estimate" means the range was synthesized from the research when no task had a
formal estimate.

| Priority | Deliverable | Completion evidence | Estimate | Depends on |
| --- | --- | --- | --- | --- |
| P0 | Make self-hosted CI truthful and green | Privileged/outside-container runner passes user+pid+uts+mount+net with proc, sysfs, and bind mounts; full Hermit and PMU lanes pass | 0.5-1 day ops | Runner host access |
| P0 | Establish exact coverage ledger | Exact-SHA report for landed ordinary/PMU/namespace/quarantined tests; recover and review valuable unlanded ports | 3-5 days | Green CI lanes |
| P0 | Fix false PMU stress skip | Modern capability probe; issue #82 closed; at least one chaos workload demonstrably runs in CI | 1-2 days | Self-hosted runner |
| P0 | Fix precise-PMU CPU overhead | Calibrated AMD skid handling or bounded alternative; CPU benchmark substantially below 40x without losing replay guarantee | 1-3 weeks, planning estimate | Dedicated PMU benchmark host |
| P0 | Repair real-binary correctness | Minimized regressions for issues #76-#80 pass in run and record/replay | 3-6 weeks, planning estimate | Coverage ledger |
| P1 | Land schedule-search E2E gate | `hello_race` and `racewrite_nostdlib` bounded gates pass on self-hosted CI | 1-2 days | Truthful PMU/namespace CI |
| P1 | Schedule-search validity MVP | Realized-schedule validation, stable identities/alignment, endpoint invariants, bounded search, repeated predicates | 4-6 weeks | PMU overhead work and CI gate |
| P1 | Complete SaBRe runtime stabilization | Thread/signal/lifecycle conformance gates; no shared core API changes | 2-5 weeks | Reverie fork migration, preserved stash |
| P1 | Time-box DynamoRIO prototype | Required syscall semantics and lifecycle tests plus comparative benchmark | 2-3 weeks, planning estimate | Backend-neutral conformance harness |
| P1 | Select fast-backend product boundary | Evidence-based decision between synchronous SaBRe, DynamoRIO, or custom SUD/patching | 1 week decision gate | SaBRe and DynamoRIO results |
| P1 | Strict deterministic QEMU/TCG boot | `ppoll`, futex timeouts, vectored I/O, fd handoff, immutable launch inputs | 2-3 weeks | Real-binary correctness, PMU/time work |
| P1 | QEMU record/replay/verify | Stable replay including required fd/control-message state; repeated exact output | 4-6 weeks | Strict TCG boot |
| P2 | Supported schedule search | Sub-event refinement where applicable, stress, artifacts/UX, fork/exec/signal coverage | 8-12 weeks total | Validity MVP and fast/reliable replay |
| P2 | Kernel race feasibility demo | Known guest bug, stable oracle, reproducible seed, passing/failing schedule pair, reduced operation delta | 4-8 weeks, planning estimate | QEMU verify and schedule-search MVP |
| P2 | Useful synchronous fast backend | Ordinary syscall inspect/suppress/replace/inject, memory, state, lifecycle, conformance | 5-8 weeks for SaBRe path | Backend selection |
| P3 | Broad fast-backend parity | Signals, PMU, CPUID/RDTSCP, state, diagnostics, architecture decisions | 12-20+ weeks | Useful backend proven on workloads |
| P3 | gVisor/Sentry KVM backend MVP | Implement bridge behind current protocol skeleton and run selected tools | 4-6 engineer-months | Workload justification and dedicated team |
| P3 | KVM parity/hardening | Broader lifecycle, signals, performance, security, operational support | 9-15 engineer-months total | KVM MVP |

## Dependencies and parallelism

The critical dependency graph is:

```text
truthful CI + exact coverage
  -> arbitrary-binary correctness
  -> stable run/record/replay
  -> schedule-search validity
  -> QEMU record/replay
  -> kernel race feasibility demo

truthful CI + backend-neutral conformance
  -> SaBRe stabilization -----+
  -> DynamoRIO prototype -----+-> fast-backend selection
                                  -> useful backend
                                  -> process parallelism/parity

PMU benchmark gate
  -> precise-preemption optimization
  -> practical schedule discovery and QEMU execution

KVM/Sentry design + workload justification
  -> KVM bridge MVP
  -> parity/hardening
```

Three workstreams can run in parallel after the P0 gate:

1. Correctness and coverage: issues #76-#82, CI, port recovery, ordinary
   run/record/replay.
2. Performance and backends: precise PMU optimization, SaBRe stabilization,
   DynamoRIO proof of concept, common conformance benchmarks.
3. Schedule/QEMU: search validity work and strict TCG syscall/launch support.

Integration checkpoints should occur at exact SHAs every one to two weeks.
Each checkpoint must publish test counts, benchmark commands, runner
capabilities, and known skips. Kernel experiments start only after the QEMU
verify checkpoint; KVM implementation starts only after a backend/workload
review explicitly funds the months-scale program.

## Decision gates

1. **CI gate:** no "green" claim while required namespace/PMU tests are skipped
   or the runner cannot execute full Hermit integration targets.
2. **Fast-backend gate:** choose a backend from conformance and workload data,
   not a single raw-syscall microbenchmark. Preserve the synchronous-subset
   option rather than forcing false shared-API parity.
3. **Schedule-search gate:** never attach an outcome to a requested schedule
   that did not execute. Report a critical pair only after a final passing and
   failing A/B replay validates that difference.
4. **QEMU gate:** establish strict TCG replay with immutable inputs before
   adding open-ended fuzzing, more devices, networking, or KVM.
5. **Kernel-race gate:** require a stable guest oracle and a known bug before
   measuring schedules/hour or attempting discovery claims.
6. **KVM gate:** do not treat the protocol skeleton as a backend. Fund the
   Sentry bridge only with a dedicated team and explicit ABI/security scope.

## Evidence index

Primary task-note sources:

- Vision and baseline: `goal-hermit-v2`, `impl-test-coverage-audit`,
  `research-test-status`, `research-arbitrary-binary-support`.
- CI and runner: `impl-enable-selfhosted-ci`, `impl-expand-selfhosted-ci`,
  `impl-runner-namespaces`, `impl-merge-latest-prs`.
- SaBRe and instrumentation: `research-find-sabre`,
  `impl-build-sabre-upstream`, `impl-finish-reverie-sabre`,
  `research-sabre-ptrace-parity`, `research-sabre-upstream`,
  `impl-sabre-runtime-stabilize`.
- KVM and BPF: `research-kvm-backend`, `research-bpf-backend`,
  `impl-reverie-parallel-rampup`.
- Performance: `research-ptrace-overhead`, `debug-cpu-bound-overhead`.
- Schedule/concurrency: `research-schedule-search`,
  `impl-revive-schedule-search`, `impl-schedule-search-ci`,
  `research-hermit-stress-testing`.
- QEMU/kernel: `impl-setup-qemu-vng`, `research-qemu-syscall-surface`,
  `impl-qemu-syscall-gaps`, `impl-qemu-under-hermit`,
  `research-kernel-race-testing`.

Existing detailed documents:

- `ai_docs/kvm_backend_design.md`
- `ai_docs/sabre_backend_assessment.md`
- `ai_docs/qemu_vng_setup.md`

This roadmap should be updated when a gate changes state, not only when code is
written. In particular, test counts, CI status, preserved stash references,
and open-issue state are volatile and must always carry an observation date and
exact repository SHA.
