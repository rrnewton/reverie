# Narf: unmodified Reverie tools inside the kernel (status)

Maintained by sub-narf-opus, a worker under the dev-hermit coordinator
coord2. Last updated 2026-09-30 at 09:28Z. This file is the Narf lane's durable
record from 2026-09-29 on.

Narf is Daniel Hodges' kernel, https://github.com/hodgesds/narf. On
2026-09-29, under the owner's directive that no work may exist only on one
machine, coord2 pushed the lane's branches to forks and opened draft pull
requests: this branch is https://github.com/hodgesds/narf/pull/506 (from
https://github.com/rrnewton-bots/narf), the hooks series is
https://github.com/hodgesds/narf/pull/505, and the Reverie fork branch is
https://github.com/rrnewton/reverie/pull/793, labelled do-not-land. They
are drafts only: no merge, no review request, and no issue filed on
hodgesds/narf. Every Narf commit below is signed.

## Where it stands

- High-water mark: unmodified Reverie tools run inside the Narf kernel,
  which hosts them at its syscall dispatcher. The tools are counter1,
  counter2, strace and chaos from reverie-examples, plus the adapter's test
  tools. They are compiled in only with the `reverie-narf-poc` build
  feature. A boot hosts one only when the kernel command line names it
  (`reverie_tool=<name>`).
- With change 3 of the RDTSC work, a Tool that subscribes to RDTSC events
  also receives the `rdtsc` and `rdtscp` instructions of the tasks it
  hosts, and its value completes them. This is exercised in kernel tests
  only; the boot path does not install the RDTSC interceptor.
- Hermit/Detcore has not run inside Narf. Milestone M2, started on
  2026-09-30, makes Detcore compile without std in Hermit's tree: branch
  `narf/detcore-nostd` of https://github.com/rrnewton/hermit, draft
  https://github.com/rrnewton/hermit/pull/3401.
  - At the branch head, 0ba3414ad, the no-std check has 9 errors left. It
    had 141 when it first reached hermit-detcore. All 9 are the
    scheduler's waits for outside progress.
  - A patch that takes the check to 0 errors and 0 warnings, with the host
    build unchanged, is posted for coord2's decision. It is not committed
    (see "M2" below).
  - The blockers for running Detcore in Narf are listed below.
- Change 3 is done: built at a2720197, all of its local evidence passes
  (below), and sub-review-c3c approved it. The branch head, 502e7808,
  only moves the adapter's Reverie dependency to GitHub at the same rev.

## Where the code and evidence are

- Narf: branch `sub-narf-opus/m1-counter1-boot` of
  https://github.com/rrnewton-bots/narf, head 502e7808 (read back with
  `git ls-remote` at 20:22Z). On devbig030 it is checked out in
  `worktrees/slots/narf-opus-progress/ignored/narf-evidence/fixes-r5/wt-r25`.
  At 502e7808 it is 127 commits on top of its merge base with Narf main,
  26fce9ac.
- Reverie fork: branch `sub-narf-opus/m14-rdtsc` of
  https://github.com/rrnewton/reverie, at 82da2755 (read back at 20:10Z).
- Hermit, for M2: branch `narf/detcore-nostd` of
  https://github.com/rrnewton/hermit.
  - Head 0ba3414adf02fefe24cf054b282d565af81b4615, read back with
    `git ls-remote` at 09:19Z on 2026-09-30. That is 4 commits on hermit
    main e454d6c28.
  - Draft pull request: https://github.com/rrnewton/hermit/pull/3401,
    `Owner: coord2`.
  - On devbig030: `worktrees/slots/narf-detcore`.
- Reverie fork, for M2: branch `sub-narf-opus/m2-nostd-on-fba351a5` of
  https://github.com/rrnewton/reverie, at 2d4e865e, read back at 09:19Z.
  On devbig030, the slot `worktrees/slots/narf-opus-reverie` has this
  branch checked out now.
- Evidence, on devbig030 only and untracked:
  `worktrees/slots/narf-opus-progress/ignored/narf-evidence/`.
  The running log is `backend-6.3/r25/STATUS-sub-narf-opus.md`, and the
  plan is `backend-6.3/m1/PLAN.md`. The RDTSC design, with "As built"
  notes for each change, is `backend-6.3/m1/m1.3/step4-rdtsc-design.md`.
  Paths below that start with `adapter3/` are under
  `backend-6.3/m1/m1.3/step4/`. M2's evidence is under
  `backend-6.3/m2-nostd/`.

## Milestones

| Milestone | What it shows | Narf commits | Reverie fork commits | State |
|---|---|---|---|---|
| M0 (2026-09-25, previous lane) | An unmodified `reverie::Tool` hosted at the syscall dispatcher, in kernel tests only. A parity cell checks the Tool's log, the guest's stdout and its exit status together. | ac688429, 072a466f, 6449b85d and follow-ups | not listed here | done |
| M1.0 counter1 | Shared global state. Hosted at boot on the login session. | ea3ad7f9 | none | done, gated |
| M1.1 counter2 | Thread-local and process-local state, and exit callbacks. | 70169374 | c00ac33b | done, gated |
| M1.2 strace and chaos | strace injects syscalls and prints their results; chaos changes syscall results. | cc3d07bd, 4b2daa30, 9ec8b943, afdf1fbe, ef467fc1 | 07b3c651, f0cad62d | done, gated; chunky_print not started |
| M1.2 musl-demo corpus | The tools over Daniel's musl demo programs (117 cases). | runs at ef467fc1 | none | 2 of 117 cases run (hello, hello_musl), under each of the four tools; the rest are blocked on this host (see Blockers) |
| M1.3 re-pin and sizing | The fork is rebased on Reverie main 7a505332, and Detcore builds with std against it. A no-std Detcore still has at least 116 compile errors. | a45ff625 (lints), 936b8242 (pin) | 07b25501 | done |
| M1.3 step 3 | A Tool callback may wait: its task parks and the callback is polled again later. A global state can run a background future. | 5ef17368, 482bbdc8, a120efa8, b80d65bf, cb5fb1a3, 139e5bd6, 2919a424 | ec69a1dd, 426c6a2c | done, gated, reviewed |
| M1.3 step 4, changes 1 and 2 | Kernel: an instruction interceptor (for `rdtsc`/`rdtscp`) may defer its answer to a callback that may wait. Reverie core: `handle_rdtsc`. | 83408f48, dadaefaa, 797df978 | be4b0d8a, 20d366c7, 82da2755 | done, gated, reviewed (approve) |
| M1.3 step 4, change 3 | The adapter delivers hosted tasks' `rdtsc`/`rdtscp` to a Tool that subscribes. | a2720197 (second amend of f3eb25a8); 502e7808 changes only the pin's URL | pins 82da2755 | done: gated at a2720197, reviewed (approve) |
| M2 Detcore without std | Detcore compiles for x86_64-unknown-none inside Hermit's tree; the std build is unchanged. | none in Narf; Hermit branch `narf/detcore-nostd`: 7598a29e9, 99580b17b, 9bad1bcfe, 0ba3414ad | 2d4e865e: the 31 commits rebased onto fba351a5 (18dac3e8), plus one that builds `RegDisplay` without std and relaxes the no-std `Config` bound | in progress: 9 errors left, all scheduler waits; a patch that reaches 0 awaits coord2's decision |
| Detcore inside Narf | Hermit's deterministic scheduler as an optional kernel capability. | none | none | not started |

## M2: Detcore without std, at 0ba3414ad

### How it is arranged

- Kernel mode is `cfg(target_os = "none")`. There, `detcore` and
  `detcore-model` are `#![no_std]`, and they bind two new workspace
  members as `std` and `libc`:
  - `detcore-std`: the parts of std they use, over core and alloc;
  - `detcore-libc`: Reverie's no-std libc through reverie-core, which is
    Detcore's only allowed Reverie dependency.
- A third new member, `detcore-clap`, is a procedural macro that stands
  in for clap's `Parser` derive: it accepts the `#[clap(...)]` attributes
  and generates nothing.
- Only the no-std check binds the three new members. `detcore-std` and
  `detcore-libc` are empty on every other target.
- The generated manifests `detcore/Cargo.toml` and
  `detcore-model/Cargo.toml` are not edited.
- `scripts/check-detcore-nostd.sh` writes its own manifests under
  `target/detcore-nostd` and type-checks for `x86_64-unknown-none` with
  nightly-2025-09-14 (rustc 1.91.0-nightly 02c7b1a7a).

### Errors in the no-std check

The table counts hermit-detcore. The other four crates have 0 errors from
step 2 on (detcore-model had 104 at step 1).

| Step | Commit | hermit-detcore errors | Warnings |
|---|---|---|---|
| Sizing (M1.3) | none | 116 or more | |
| 1: stand-ins and the check | 7598a29e9 | not reached | |
| 2: detcore-model | 99580b17b | 141 | 1 |
| 3: host-only paths and stand-ins | 9bad1bcfe | 49, after 118, 89 and 67 | 1 |
| 4: process, clock and scheduler-task stand-ins | 0ba3414ad | 9, after 16 and 11 | 0 |
| Proposal, not committed | none | 0 | 0 |

rustc's counts are approximate: an unresolved import hides the errors in
the code that uses it, and some lints, such as dead code, run only once a
crate type-checks.

### Host build at each step

The host gate runs with hermit's toolchain, offline. Its log is
`backend-6.3/m2-nostd/std-gate.log`.

- `cargo build --workspace`, clippy
  `--workspace --all-targets --all-features -D warnings`, fmt, doc
  `-D warnings` and `check-detcore-backend-abstraction.sh` all return
  rc 0.
- The resolved feature graph differs from main's only by the new members.
- Detcore's tests, with the validation DAG's nextest selections, pass
  with main's counts, plus the five unit tests that step 4 adds:
  - unit tests: 848 at main, 853 from step 4;
  - tests_misc: 28 passed, 5 skipped;
  - tests_parallelism: 5 passed, 11 skipped;
  - detcore-model: 98 passed.
- A whole gate run took 121 s at step 3, 79 s at step 4 and 72 s with the
  revised proposal.

### The 9 errors left, and the proposal

All 9 are the scheduler's waits for outside progress:
- 4 are in `Backoff::further`: std's yield and sleep on the blocking path
  that `run_external_scheduler` uses, and tokio's on the spawned
  scheduler, which is host-only.
- 5 are busy-waiting scheduler steps. Each yields the host thread and
  returns `SkipTurn` while it holds the scheduler lock.

The proposal is for coord2 to decide:
https://github.com/rrnewton/hermit/pull/3401#issuecomment-5907877892,
revised in
https://github.com/rrnewton/hermit/pull/3401#issuecomment-5908125748.
- In the kernel build, `Backoff::further` awaits `tool_global::yield_once`,
  which is pending once after waking its own task. The five step waits
  yield only on the host.
- Both drivers of `run_external_scheduler` poll a pending scheduler again
  without waiting for a wake; each polls with a no-op waker. They are
  detcore-dbt's `run_cooperative` and Narf's `spawn_background`.
- The cost: the kernel build never sleeps in real time, so the scheduler's
  CPU does not idle while it waits. Narf's driver already works that way.
- Measured with the revised patch (`wait-proposal-v2.patch`, sha256
  f4f31ea1...): 0 errors and 0 warnings, and the host gate passes with the
  same counts.
- Alternatives considered:
  - `core::hint::spin_loop`: it livelocks when what the scheduler waits
    for needs the scheduler's CPU, and the step waits would spin while
    holding the lock;
  - `yield_now` and `sleep` hooks that the host installs in detcore-std:
    the step waits would call them under the spin lock, and they break
    detcore-std's rule that `thread` is absent;
  - a wait API in reverie-core's backend, which changes Reverie;
  - awaitable step waits, which change the scheduler more than returning
    `SkipTurn` does.

### Hazards

The draft pull request's description lists them all. The main ones:
- `f64::powf` is libm's `pow` in the kernel build and glibc's on the
  host. The two can differ in the last place. This branch does not change
  the host, and changing it needs the owner.
- Where the host exits with a status, the kernel build panics.
- A timed signal that the scheduler delivers through a host pidfd ends a
  kernel run as `HERMIT_DEADLOCK`: a child's `SIGCHLD`, and most alarm
  and POSIX timer expiries.
- There is no real-time clock.
- The kernel build must start the scheduler with
  `init_for_external_scheduler` and `run_external_scheduler`.

A host finding, not changed and not filed: `Backoff::further` sleeps
`2 ^ round` microseconds, and `^` is XOR. After its first 10 yields it
therefore sleeps 3, 0, 1, 6, 7, 4, 5, 10, 11, 8, 9, 14 and 15 µs, then
10,000 µs. Powers of two would give 2 to 8,192 µs. It changes only how
long the host sleeps between polls, not virtual time.

## Change 3: a2720197

- What it does: `ReverieInterceptor::with_rdtsc` builds the Tool host with
  RDTSC delivery. It returns an RDTSC interceptor only when the Tool
  subscribed, and the caller installs it before any guest task exists.
  Every trap is deferred to the callback. The callback runs an untracked
  task's instruction natively and sends a hosted task's instruction to the
  Tool; the Tool's value completes the instruction (EDX:EAX, and for
  `rdtscp` the Tool's aux in ECX, or 0 if it gave none, as reverie-ptrace
  does). The fork's pin moves from 426c6a2c to 82da2755. Against 797df978:
  9 files, +1127/-163.
- Tests: seven new reverie-narf cases on two new guests. The values case
  checks the Tool's log, the guest's stdout and its exit status together.
- History:
  - f3eb25a8, the first build. Its evidence all passed (47 of 47 gate
    steps, nine negative controls, the interception gate 20/0/0).
    sub-review-c3 requested changes: five Low findings, none in the
    delivery logic (a control described as failing a test that in fact
    panics the kernel, a loosely pinned message, an untested leak check,
    an untested "aux or 0" default, and documentation that did not name
    `with_rdtsc` or say that nothing checks the caller's install).
  - c0ca7ead, the fixes (+82/-49). Its evidence all passed (47 of 47
    steps in 27.0 minutes, eleven controls, 20/0/0). sub-review-c3b
    (`adapter3/review-c3b.md`) requested text changes only. The one Low:
    the documentation gave one cause for the fatal `RdtscContextManaged`,
    and the fork has two. Besides a tail inject that parks, an injected
    `rt_sigreturn` also gives it, since there is no signal frame to
    restore. Separately, two controls at c0ca7ead showed that a panic in
    an RDTSC callback left the test run hung (next bullet).
  - a2720197, the text fixes and the harness fix below (+31/-10 against
    c0ca7ead, in the adapter's `src` only). The response, point by point,
    is `adapter3/review-c3b-response.md`. A test for the `rt_sigreturn`
    case was recommended but not required, and is not done; the adapter's
    half of that path is untested.
  - 502e7808, on top of a2720197, at the owner's request: the adapter
    takes its three Reverie crates from https://github.com/rrnewton/reverie
    instead of a `file://` URL naming a clone on devbig030, at the same
    rev, so the branch builds elsewhere. Cargo.lock's six Reverie entries
    move to that source, and nothing else in it changes. `cargo fetch`
    (with network access, through herdr-run) and then `cargo metadata`
    without `--offline` resolve all six to the GitHub source, and
    reverie-narf at 2 CPUs compiled them from it and passed 46/0/0 in
    50 s (`backend-6.3/r25/https-pin/`). The gate chain was not rerun at
    502e7808; coord2 ruled on 2026-09-30 that a source-only change needs
    the fresh-home gate in the next bullet instead.
- 502e7808 from a fresh CARGO_HOME (2026-09-30, `backend-6.3/r25/fresh-home/`,
  script `g1-build.sh`, log `g1.log`). Fresh means: a new CARGO_HOME with
  no config.toml, no git database and no git checkouts, and an empty
  target directory. The crates.io registry was a copy-on-write copy of the
  host's, because this shell cannot reach crates.io; Cargo.lock's
  checksums pin those crates.
  - GitHub source. This shell's own requests to github.com get a proxy
    403. So herdr-run fetched the seven commits the build takes from
    GitHub (Reverie 82da2755 and six nested submodule commits) from
    github.com into local mirrors (`fetch-mirrors.sh`, all rc=0). The build's git
    config maps exactly those seven URLs to the mirrors and every other
    GitHub URL to a path that does not exist, so a fetch from anywhere
    else fails. The URLs drop the `.git` suffix, because the host's global
    git config redirects the `.git` forms to local clones.
  - Result, run 2 (05:53-05:58Z): `cargo fetch` rc=0 in 9 s, leaving one
    git database, whose 82da2755 is "Correct the RDTSC outcome docs from
    the fix-commit review". `cargo xtask build --features reverie-narf-poc`
    rc=0 in 105 s (image sha256 18ce2b8f..., 220871 KASLR sites; the
    hash differs from earlier builds because paths are embedded).
    reverie-narf at 2 CPUs: 46 pass, 0 fail, 0 skip in 168 s. The
    worktree stayed clean at 502e7808 throughout, Cargo.lock included.
  - Run 1 (05:22-05:46Z) does not count, and the fault was the harness's,
    not Narf's (`FINDING-run1.md`). It pointed CARGO_TARGET_DIR at a new
    directory, but xtask always reads and boots `<workspace>/target/...`
    (`build/xtask/src/main.rs` 2604). So it booted a stale non-test image
    in the old target directory, which ran no test and hung to the 1200 s
    QEMU timeout. Its fetch and compile steps did pass (rc=0).
- The hang, and the fix in a2720197. The RDTSC test harness cleared the
  kernel's interceptor after every run. The clear waits until every
  online CPU acknowledges it (`lib/src/smp.rs` lines 481 and 502, no
  deadline). A panicked CPU halts with interrupts masked but stays online,
  so it never acknowledges, and the harness never reached the runner's
  stop line. Controls R3 and R10 at c0ca7ead ran to the 300 s QEMU timeout
  (325 s and 326 s) with no summary. An experiment
  (`adapter3/exp-panic-stop/log`) confirmed the mechanism. With the clear
  skipped after a panic, R10 ended in 27 s with the stop line and a
  summary. R3 still hung, because its panic is on CPU 0, which runs the
  test runner (R10's is on CPU 1). a2720197 skips the clear once a panic
  has been reported and fails the test naming the panic. The leak check
  then lets that failure stand, since the runner stops after the test.
  Still unbounded: a panic on the runner's own CPU, and a panic that lands
  while the clear is already waiting. The documentation and commit message
  say so.
- Results at a2720197 (`adapter3/after-c3.log`, run started
  2026-09-29T19:09:20Z):
  - reverie-narf at 2 CPUs: 46 pass, 0 fail, 0 skip (39 before the
    change). At 1 CPU: 45 pass, 0 fail, 1 skip. The skip is
    `reverie_narf_reclaim_waits_for_a_destructor_on_another_cpu`, which
    needs a second CPU; it predates the change, and the script accepts
    only that skip, with that reason.
  - Thirteen negative controls (`adapter3/controls/log`, outputs in
    `adapter3/controls/at-a2720197/`). Each is a temporary patch to the
    adapter that removes one piece. Each must make the suite fail with the
    exact messages that piece predicts. Some also require a message to be
    absent. All thirteen did, and the tree was clean afterwards.
    - R1, every instruction run natively: 41/5/0, 33 s.
    - R2, the Tool's aux replaced by 0: 45/1/0, 29 s.
    - R11, an aux of 1 where the Tool gave none: 45/1/0, 28 s.
    - R4, a re-poll wait reported unsupported: 36/10/0, 114 s (39/7/0 in
      59 s at c0ca7ead; see "Open items").
    - R5, `killed()` not forwarded: 45/1/0, 29 s.
    - R6, a kill read as a yield: 42/4/0, 40 s.
    - R7, a Tool error run natively: 44/2/0, 29 s.
    - R8, the harness leaves the interceptor installed: 39/7/0, 30 s; the
      first failure is the leak check.
    - R9, `with_rdtsc` answers for every Tool: 45/1/0, 30 s.
    - R10, a Tool errno sorted as an invariant: the kernel panics with
      "reverie-narf fatal in handle_rdtsc: Rdtsc(EPERM)", the errno case
      fails "a CPU panicked while the guest ran", the runner stops after
      it, and the run ends with 2/2/0 in 29 s (326 s to the timeout at
      c0ca7ead).
    - R13, R10 with the leak-check exemption removed: 2/2/0 in 27 s, and
      the errno case names the leaked interceptor instead.
    - R12, R10 with the clear made after a panic: the run hangs to the
      timeout, 325 s, no summary.
    - R3, no hosted-task check: the kernel panics with "UnknownTask(Pid(1))"
      on the runner's CPU, and the run hangs to the timeout, 327 s.
  - Format check: rc 0. Lint (`adapter3/lint-a2720197/summary`): four
    clippy runs rc 0 with 0 lint warnings (the one warning in three of
    them is the build script's "musl-gcc not on PATH" note), plus
    `m1/lint.sh` rc 0 and `m1/lint-frame.sh` rc 0.
  - Gate chain (`fixes-r5/gates-a2720197/chain-summary`): 47 of 47 steps
    exit 0, from 19:32:06Z to 19:58:46Z (26.7 minutes), and the tree was
    clean afterwards. The 16 kernel-test summaries are byte-identical to
    c0ca7ead's. Three are above the 797df978 table below by the seven new
    cases: reverie-narf x86_64 46/0/0, full-poc-x86_64 8618/0/81 and
    slab-audit-poc-x86_64 2401/0/5. The other thirteen equal that table.
  - Syscall-interception gate at 2 CPUs
    (`fixes-r5/gates-a2720197/interception-smp2-v3.summary`): 20 pass, 0
    fail, 0 skip, 121 s.
  - Review: sub-review-c3c (`adapter3/review-c3c.md`), bound to a2720197,
    approved, with nothing rated Low or higher. It recommends two text
    Nits without requiring them, and neither is applied:
    - interceptor.rs 637-640 and lib.rs 88-89 say the abort's `SIGKILL`
      ends the task with wait status 9. After an injected `rt_sigreturn`
      the SIGSEGV that the kernel staged during the callback wins, and the
      status is 0x8b.
    - lib.rs 117-122 misses one way a panicking run still hangs: a guest
      task queued on the runner's CPU that waits for a task the halted
      CPU holds.
    Two optional Nits: lib.rs 347 says "the five signal tests", where
    tests.rs has 12 `SignalTables::init` sites, and the leak-check
    exemption is broader than its stated reason.
- Branches no control covers, each with its reason in the commit message:
  - the ContextManaged arm: the task dies before it returns to user mode,
    so its answer is never observed;
  - the aborted-tree check: reached only by a task adopted after the
    abort's kill, or by one in user mode on another CPU (or preempted)
    when the kill was sent, whose next kernel entry is an rdtsc trap;
  - the arm for an instruction the interceptor did not subscribe to: the
    kernel never delivers one;
  - in `fatal_kind`, RdtscContextManaged (reached by a blocking tail
    inject or an injected `rt_sigreturn` in an RDTSC callback; no test
    makes either) and UnexpectedRdtsc (unreachable through `with_rdtsc`).

## Changes 1 and 2 and tests D and E: gated at 797df978

- Gate chain: 47 of 47 steps exit 0, and the tree was clean afterwards
  (`fixes-r5/gates-797df978/chain-summary`). It ran from 00:38:30Z to
  01:23:59Z on 2026-09-29, 45.5 minutes; the same chain took 39 minutes at
  the previous head, dadaefaa.
- 31 of the steps are the repository's hook checks, 22 clippy runs (20
  kernel feature sets and 2 host crates), the host tests, 2 boot smokes, 2
  IDT self-test runs and 3 repeats of the CI step. The other 16 run kernel
  tests. Each of the 16 summaries matched the expected baseline recorded in
  STATUS:

| Gate step | pass | fail | skip |
|---|---|---|---|
| full-x86_64 | 8572 | 0 | 82 |
| full-aarch64 | 6514 | 0 | 43 |
| full-poc-x86_64 (with `reverie-narf-poc`) | 8611 | 0 | 81 |
| full-poc-aarch64 | 6514 | 0 | 43 |
| slab-audit-poc-x86_64 | 2394 | 0 | 5 |
| focused reverie-narf, x86_64 | 39 | 0 | 0 |
| focused reverie-narf, aarch64 | 1 | 0 | 0 |
| xapic, 4 CPUs | 53 | 0 | 5 |
| focused memory / userspace / syscall_abi / modules, x86_64 | 400/0/12, 544/0/3, 1813/0/2, 54/0/0 | | |
| focused memory / userspace / syscall_abi / modules, aarch64 | 315/0/6, 404/0/2, 1790/0/4, 55/0/2 | | |

- Syscall-interception gate at 2 CPUs: 20 pass, 0 fail, 0 skip.
- The six deferred-instruction tests (`verification/instruction-defer`):
  6 pass, 0 fail, 0 skip at 2 CPUs and at 1 CPU.
- Five negative controls. Each is a temporary patch to the production file
  `userspace/src/syscall.rs` that removes one piece of the fix. Each run
  exits 1 with 5 pass and 1 fail, in 123 to 125 s, and the failing test
  prints the expected message:
  - D1, a withheld signal is never made pending again after the callback:
    "an RDTSC trapped after the script's last".
  - D2, the callback's signal gate refuses only a killed task and withholds
    nothing: "an inject did not answer -ERESTARTSYS while a terminating
    SIGTERM was pending".
  - E1, the context-managed latch removed: "an inject ran after a
    transition had taken the task's context".
  - E2, a termination deferred during the callback is dropped: "an RDTSC
    trapped after the script's last".
  - E3, the staged termination loses its core-dump flag: "the guest did not
    die of SIGSEGV with the core-dump flag". Test E pins Narf's current
    wait status, 0x8b. Linux reports 0x0b when no core file is written; see
    "Open items".
- Review: sub-review-f approved, bound to the exact head
  (`backend-6.3/m1/m1.3/step4/kernel4/review-f.md`).

## A normal build neither requires nor activates Reverie

- At a2720197 (`backend-6.3/r25/normal-build/at-a2720197/summary.txt`),
  seven builds, all exit 0:
  - Without the `reverie-narf-poc` feature (the default, boot-smoke,
    musl-demo and kt builds): 0 Reverie packages in the dependency tree and
    0 Reverie symbols in the image. These four images are byte-identical to
    c0ca7ead's.
  - The feature alone (poc): 8 Reverie packages in the dependency tree,
    and 0 Reverie symbols in the image, which has neither kernel tests nor
    boot-init to host a Tool.
  - Why the poc image changes when only the adapter changes (review-c3c
    left this open: d18c6edd... at a2720197 against 56a6e2e6... at
    c0ca7ead). Only symbol names and the build ID change
    (`backend-6.3/r25/poc-hash/FINDING.md`):
    - Reproduced at 502e7808 by adding one comment line to the adapter's
      lib.rs. .text, .data and every debug section stay byte-identical.
    - 46 kernel symbols get a new ThinLTO `.llvm.<n>` suffix, which changes
      .symtab and .strtab.
    - The only loaded bytes that change are 20 bytes of .rodata: the SHA-1
      build-id note, which the linker computes over its whole output.
    - Restoring the file and rebuilding gives the first image again.
    - Between c0ca7ead and a2720197, likewise, 33 symbols differ, each only
      in that suffix.
    - No DWARF line-table row in the poc image names a Reverie or adapter
      source file. The kt-poc control has live rows from 43 such files.
    - The adapter is in the build only through the `extern crate` at
      frame/src/bare_main.rs lines 37-38.
    - The poc image's .text is 17,504 bytes larger than default's. One
      kernel function, narf_filesystem::sysfs::kobject_add_attr, is
      inlined differently and grows by 17,735 bytes; the other functions
      net -228 bytes.
    - Two build inputs of the kernel crates differ: narf-frame gains the
      feature and the dependency, and Cargo builds bitflags 2.13.2 with its
      serde features. Which of them changes that inlining was not
      isolated.
  - The feature with a user of it: kt-poc links 2696 Reverie symbols
    (2310 at 2919a424) and musl-demo-poc 747 (745).
  - boot-smoke (`at-a2720197/boot-smoke.txt`): exit 0 in 3 s, 0 lines
    mentioning Reverie, 1 clean-exit line, image sha256 ac044050...
- Earlier proofs at c0ca7ead and 2919a424
  (`backend-6.3/r25/normal-build/at-*/summary.txt`), with the same seven
  builds and the same result. At 436d1e5c, the image that boot-smoke
  booted was byte-identical to the boot-smoke image the proof inspected
  (sha256 2de47cd3...).

## Blockers and open items

A failure that also occurs on Narf main still blocks every gate that runs
it; none is skipped.

- musl-demo corpus: 115 of 117 cases cannot run on this host.
  - 26 of the 29 committed ELF programs are dynamically linked, and the
    image has no musl loader. musl-gcc is not installed.
  - The musl 1.2.6 and BusyBox 1.36.1 source tarballs are not on the host,
    and the proxy answers 403 for musl.libc.org and github.com.
  - The lane's base, 436d1e5c, built without Reverie, fails the same
    cases in the same way. It was not measured on Narf main itself. (STATUS
    calls 436d1e5c "narf main", but it is not on Narf main: it is the
    previous lane's 105 commits on top of Narf main 26fce9ac.)
  - Unblocking needs a coordinator decision: install musl-tools, place
    prebuilt programs and a loader in the evidence tree, or run it in CI,
    which means publishing.
- Reverie's own ptrace test suite flakes. The failures come from five races
  in reverie-ptrace's tests, and all five also fail on upstream Reverie
  (`backend-6.3/m1/m1.3/step4/core/rv-bisect/REPORT.md`). About 3.5% of
  whole-binary runs fail below host load 30, and about 30% at load 60 or
  more. This blocks the Reverie test gate; the fix belongs upstream.
- Detcore inside Narf. coord2 decided on 2026-09-28 that Detcore runs in
  the kernel, through its external-scheduler entry, as detcore-dbt does.
  Before that can work:
  - Detcore uses std and tokio throughout: 60 files, 76,428 lines. M2
    has its no-std check at 9 errors (see "M2" above).
  - Detcore's default subscription, `Subscription::all()`, includes CPUID
    events, which the Narf core refuses. RDTSC delivery now exists
    (change 3), through `with_rdtsc` only.
  - The adapter documents Narf's `spawn_background` as the executor that
    Detcore's `run_external_scheduler` expects. Nothing has run Detcore's
    scheduler under it yet.
  - Detcore's thread start cannot run yet. This was found by reading code
    and has not been run:
    - Narf polls `handle_thread_start` only once. If it is still pending,
      the run ends with `ToolSuspended`, which kills the hosted tree
      (reverie-narf-adapter/src/lib.rs, "Run aborts" and "Waiting
      callbacks", at 502e7808).
    - With `sequentialize_threads`, Detcore's thread start awaits the
      global RPC `StartNewThread`. Its handler, `recv_start_new_thread`,
      begins with `yield_once`, so it is pending on its first poll.
    - Narf runs global RPCs inline: reverie-narf-core/src/guest.rs:523 at
      Reverie 82da2755.
    - Fixing it needs one of two changes: the adapter lets
      `handle_thread_start` wait, as it lets syscall callbacks wait, or
      Detcore's thread start stops waiting there.
  - `set_timer`, `set_timer_precise` and `read_clock` return ENOSYS, and
    signal events are not delivered.
  - The port now has a hermit write destination: the slot
    `worktrees/slots/narf-detcore`, branch `narf/detcore-nostd` (M2).
    It builds against a Reverie that can drop std: branch
    `sub-narf-opus/m2-nostd-on-fba351a5` of
    https://github.com/rrnewton/reverie, 18dac3e8. That branch is
    the fork's 31 commits rebased onto fba351a5, Hermit's current pin (read
    back 2026-09-30). On it, Reverie's no-std gate passes (the build, 6 of 6
    execution tests, and four negative controls that fail with E0463).
    A std check of eight crates, all targets, has 0 warnings, and
    reverie-process and reverie-syscalls pass 142 and 29 unit tests
    (`backend-6.3/m2-nostd/r-verify.log`). One conflict was resolved by
    hand, in reverie-process/src/lib.rs. The branch is now at 2d4e865e,
    which builds `RegDisplay` without std and drops the `Default` bound on
    `GlobalTool::Config` without std. Detcore's `Config` has a `Default`
    only through its command-line parser.
- Kernel-test boots drop a SIGKILL without a trace. This was found through
  control R4. The analysis is `adapter3/r4-lost-kill.md` (sub-r4-lost-kill,
  read-only). Its key code citations were re-read, and sub-review-c3c
  verified them too.
  - Cause: a kernel-test boot has no pending-signal tables unless a test
    creates them (the adapter's `SignalTables` guard). A raise into missing
    tables returns without setting anything (`raise_signal_pending`,
    `userspace/src/handlers/compat.inc.rs` 6690-6696 at a2720197), and
    `kill_process` returns true regardless (7110-7119).
  - Three reverie-narf tests really wait and create no tables:
    `rdtsc_values_come_from_the_tool`,
    `waiting_callbacks_meet_through_global_state` and
    `background_future_releases_a_waiting_callback`.
  - When R4 turns their wait into an abort, the abort's `SIGKILL` goes
    nowhere. The task gets -EINTR from its next syscalls and reads the TSC
    natively. Its exit then returns, so it faults on the `ud2` after it:
    5, 5 and 10 such faults at f3eb25a8, c0ca7ead and a2720197, and none
    in any other control or clean run.
  - The two R4 aborts in tests that do create the tables show no fault.
  - A production boot creates the tables (`frame/src/bare_main.rs` 4776).
    So this is a kernel-test limit, and the adapter documents it (lib.rs
    346-354). Two adapter comments, interceptor.rs 251-254 and 637-640,
    still say without qualification that the pending `SIGKILL` is
    delivered first.
  - No clean run is affected: none aborts, and an aborted test fails
    anyway.
  - Follow-ups, none run:
    - (a) rerun R4 with the tables created in those three tests; the
      prediction is no fault;
    - (b) a test in which a legitimate Tool waits and then returns an
      error, with the tables created;
    - (c) make `kill_process` report a raise that went nowhere.
    Item (c) is a Narf kernel change. An issue for it would go on
    hodgesds/narf, which needs the owner's approval.
- A kernel-test run after a panic still hangs to the QEMU timeout when the
  panic halts the CPU that runs the test runner (control R3, 327 s at
  a2720197). The adapter's documentation now says so (lib.rs 118-122 at
  a2720197); before a2720197 it claimed every such run ends "in bounded
  time with a named `[FAIL]`". This is the deferred "panic on the test
  CPU" item below. A panic on any other CPU in an RDTSC test now ends the
  run with the stop line and a summary (R10, 29 s).
- Narf main (read at c0da4f61, 2026-09-29) reports WCOREDUMP (0x80) in a
  wait status when no core file was written. An issue draft for
  hodgesds/narf waits for the owner's approval; it is not filed.
- Deferred by coord2, not gates:
  - a stop signal clobbers a register;
  - a parked syscall re-runs without SA_RESTART;
  - a panic on the test CPU hangs a kernel-test run;
  - four NUMA smoke tests fail at 2 CPUs.
