# Minimal source evidence for the systrap comparison

Inspection date: 2026-09-04 America/Los_Angeles. Commands were local reads:
`git rev-parse HEAD`, `git remote -v`, `git status --short`, `git log`,
`git branch -r --contains HEAD`, `git show`, `rg`, and numbered source reads.
No network request was made. The available internet-access skill was read at
`/home/newton/.claude/skills/internet-access/SKILL.md`; the owner's GitHub
policy-denial remains respected, not re-probed or routed around.

## Source bindings

- **G** = `/home/newton/work/dev-hermit/experiments/gvisor`, commit
  `012cf0b0b14d7b8aa2a4424867ecec8e6121e69d`, clean before and after reading.
  Local tracking ref `rrnewton/perf/benchmark-v3-counter2` points to this SHA.
  Configured remotes: `https://github.com/google/gvisor.git` (origin),
  `https://github.com/rrnewton/gvisor.git` (rrnewton). Commit subject:
  `Add counter platforms for cross-backend benchmarks`; author/committer Ryan
  Newton, 2026-07-26T09:40:45-07:00. Its immediate parent is
  `8eb8f9e0d` (abbreviation only; no assertion of current upstream state).
  The HEAD diff adds counter platforms/registration, not systrap changes;
  this does not certify earlier history as unmodified upstream.
- **R** = `/home/newton/work/dev-hermit/worktrees/slots/liteinst-exec`, immutable
  `git show 2df087caf1d90c6f44fc5ae8cb915ede15fdd8df:<path>` reads. Includes
  dependency `e7921d9cb8d1f8f84ce46900a3caeb0827aa7469`.
- **S** = same local repository, immutable
  `49be8655a506cb252f44a714ca0983ebb03ae505` controller reads.
- **H** = `/home/newton/work/dev-hermit/worktrees/slots/liteinst-owner`, immutable
  `4d8f866102882b6eeabdf99cc7e81433cc3c95c5` CLI reads.

Paths below resolve under the corresponding absolute checkout root; line
numbers refer to that commit, not to a moving branch. They identify inspected
implementation, not executed tests. `GVISOR-SYSTRAP-SOURCES.sha256` records the
selected local gVisor files as an additional audit check.

## Primary-source references

| ID | Pinned paths and starting lines | What was checked |
|---|---|---|
| G1 | G `pkg/sentry/platform/systrap/stub_unsafe.go:164`, `:270`, `:315`; `systrap.go:295`, `:362` in the same directory | Reserved mapping layout, blob copy/protection, initialization before creating pooled address spaces. |
| G2 | G `pkg/sentry/platform/systrap/subprocess_linux.go:163`, `:198`, `:224`; `subprocess.go:241`, `:288`, `:1115`, `:1179`, `:1203`; `sysmsg_thread_unsafe.go:41` | Clone/ptrace bootstrap; SUD ON with null selector and reserved stub interval, EINVAL fallback; shared/alternate stacks; seccomp and detach before context execution. |
| G3 | G `pkg/sentry/kernel/task_image.go:141`; `pkg/sentry/loader/loader.go:219`, `:274`, `:363`; `pkg/sentry/loader/elf.go:648`, `:694` | Sentry owns image, script/ELF/interpreter loading and initial registers. |
| G4 | G `pkg/sentry/kernel/task_exec.go:191`, `:213`, `:309`, `:347`, `:382` | Load failure, sibling-exit phase, modeled signals/FDs and image switch on exec. |
| G5 | G `pkg/sentry/platform/systrap/sysmsg_thread_unsafe.go:72`; `sysmsg/sigrestorer_amd64.S:19`; `sysmsg/sighandler_amd64.c:202`, `:221`, `:234`, `:338` in that platform directory | Host handler masks/restorer; owned interrupt deferral/internal faults; real signal-frame context and FS/FP restoration. |
| G6 | G `pkg/sentry/kernel/task_signals.go:269`, `:280`, `:300`; `pkg/sentry/syscalls/linux/sys_signal.go:244` | Separate application signal setup, mask and rt_sigreturn implementation. |
| G7 | G `pkg/sentry/platform/systrap/sysmsg/sighandler_amd64.c:250`, `:306`, `:353`; `systrap.go:180`; `usertrap/usertrap_amd64.go:190`, `:233`, `:292` | First trap requests patching; subsequent patched entry still goes through Sentry; fallback/restart path, guest-tracer restriction and documented caveat. |
| G8 | G `pkg/sentry/platform/systrap/usertrap/usertrap.go:15`; `sysmsg/syshandler_amd64.S:15`, `:39`, `:138`, `:185`, `:208`; `sysmsg/sysmsg_amd64.go:51` | Reserved GS/private stack, concrete register/FP and RSP/flags restoration, TF/GS limitations, AMX exclusion. |
| G9 | G `pkg/sentry/platform/systrap/sysmsg/sysmsg_lib.c:81`, `:163`, `:198`, `:318`, `:382`; `syscall_thread.go:37`; `sysmsg_thread.go:30`, `:150`; `subprocess.go:1163` | Host-timed queues, atomic context exchange, futexes, restricted shared syscall messages, detached application execution and priority choice. |
| G10 | G `runsc/boot/loader.go:665`; `pkg/sentry/time/calibrated_clock.go:240`; `pkg/sentry/time/sampler.go:69`; `pkg/sentry/kernel/kernel.go:260`; `pkg/sentry/kernel/task_run.go:89`, `:241`; `pkg/sentry/platform/systrap/systrap_amd64.go:25` | Calibrated TSC clocks, coarse CPU accounting, task execution/Switch, assumed TSC rate for transport timing—not deterministic retired branches. |
| R1 | R `reverie-liteinst/src/backend.rs:73`, `:237`, `:618`, `:1011`; `reverie-liteinst/src/tool_host.rs:101`, `:188`, `:375`; `reverie-liteinst/src/rpc.rs:17` | Bootstrap/preload native spawn; quiescent install, local Tool/state and shared driver; BlockingRpcClient. |
| R2 | R `reverie-liteinst/src/runtime.rs:998`, `:2841`, `:2885`, `:2929`; `reverie-liteinst/src/tool_host.rs:788` | Hard-coded typed seccomp; trap/patch/restricted fallback; Tool-active internal path and exec/signal restrictions. |
| R3 | R `reverie-liteinst/src/lib.rs:71`; `reverie-liteinst/src/patch_alloc.rs:271`; `reverie-liteinst/src/runtime_domain.rs:68` | Existing PatchAllocator/TOOL_HEAP isolation of runtime Rust allocations; not proof of arbitrary asynchronous Detcore/libc/TLS/lock safety. Independently verified in the review below. |
| S1 | S `reverie-preload/src/user_dispatch.rs:100`, `:121`, `:153`; `reverie-preload/src/signal.rs:62` | Standalone SUD scope and explicit rearm/lifetime obligations; neither loader coverage nor Tool migration. |
| H1 | H `hermit-cli/src/lib.rs:2679`, `:2892` | Both CLI LiteInst routes select host-with-preload Detcore, not the typed in-process path. |

Within rows that abbreviate neighboring paths, the directory remains
`pkg/sentry/platform/systrap/`; none refers to a different checkout.

## Existing Hermit evidence, not new runs

- **E1:** `SUD-FEASIBILITY.md:1`, `sud-feasibility.json` in this folder;
  raw retained observations under
  `/home/newton/work/dev-hermit/ignored/liteinst-sud-20260904`.
  Observation window 2026-09-04T23:51:16Z–23:51:19Z. Explicitly distinguishes
  successful native exec/rearm from untrapped startup and three red TSC cases.
- **E2:** `/home/newton/work/dev-hermit/ignored/liteinst-sud-clock-20260904/composition/README.md:1`.
  Frozen composition patch SHA256
  `8db27c8959913124add2d9cc2304e638d2b9d0bdeb81007ddb9decb20ea117d4`;
  archive SHA256
  `c345edb5e196854a96d01be33a36ee82282d82927c90b88e2e04ec465e54fdaf`.
  The existing 34-file source checksum check still matched during this audit.
  No source, timer work, staged paths, or frozen artifact was changed.

## Canonical source links grounded in local provenance

These use the configured fork and exact locally tracked commit; remote
availability was not tested. Local source is the evidence, not a fetched page.

- https://github.com/rrnewton/gvisor/blob/012cf0b0b14d7b8aa2a4424867ecec8e6121e69d/pkg/sentry/platform/systrap/subprocess.go
- https://github.com/rrnewton/gvisor/blob/012cf0b0b14d7b8aa2a4424867ecec8e6121e69d/pkg/sentry/platform/systrap/sysmsg/sighandler_amd64.c
- https://github.com/rrnewton/gvisor/blob/012cf0b0b14d7b8aa2a4424867ecec8e6121e69d/pkg/sentry/kernel/task_exec.go

Interpretive conclusions are marked as conditions/costs in the comparison.
No host support, performance, full xstate, nested Tool safety, precise PMU,
general signals, or current upstream behavior is inferred from names or docs.

## Narrow review correction

Corrected the overbroad guest-allocator-sharing statement; retained the actual
unqualified asynchronous reentry/ownership limits. Clarified default-enabled,
configurable patching and that SUD allows the stub interval, not its exterior.
No source changes or new research accompany this correction. Review:
`/home/newton/work/dev-hermit/scratch/liteinst-gvisor-systrap-comparison-codex-review-20260904.md`.
The reviewer also verified G `runsc/config/flags.go:123` and
`runsc/boot/loader.go:983` for the patch-disable option; these are not additional
entries in the original 26-file checksum manifest. Original draft/ledger copies
remain under `/home/newton/work/dev-hermit/ignored/liteinst-sud-only-plan-20260904/systrap-before/`.
Revised written-artifact hashes are in `GVISOR-SYSTRAP-REVISION.sha256`.
