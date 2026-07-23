# DBI M4: Multi-process support (fork/exec) over the DynamoRIO Guest

Status: implementation + analysis. Milestone DBI-M4 (follows M1 audit, M1.5
simple tools, M2 Guest-trait gaps, M3 Detcore feasibility). Grounded in
`rrnewton/reverie` (`reverie-dbi`) and validated end-to-end under DynamoRIO.

## Question

> Can a multi-process app (a `fork`/`exec` process tree, e.g. a bash pipeline)
> run under the DBI backend, with per-process instrumentation coordinated across
> the tree?

**Short answer:** children already *run* instrumented (DynamoRIO follows them by
default); what M4 adds is the missing **coordination** across those otherwise
independent per-process client instances — a coordinator-IPC channel plus
deterministic process-tree identity. Full cross-process *determinism* (a live
shared global state and a scheduler that spans processes) remains gated by the
same executor/scheduler blockers M3 identified, now compounded by the process
boundary; it is scoped as M5+.

## What was already true (empirical baseline)

Running the pre-M4 client over a genuinely-forking pipeline showed DynamoRIO's
**`-follow_children` is on by default**: the client `.so` is injected into every
process of `bash -c '/bin/echo hi | cat'` (bash, `/bin/echo`, `cat` all load it).
So "following children" was never the gap. The gaps were:

- Each followed process is a **separate address space** with its own fresh
  process-global `static`s; nothing is shared. `send_rpc` in `reverie-dbi` is a
  direct in-process call — there is no wire.
- `ppid` was always `None` (M2 `TODO-STUB(#31)`); there was no process-tree
  identity.
- Nothing aggregated per-process instrumentation into a tree-wide view.
- The launcher relied on DynamoRIO's *implicit* default rather than requesting
  child-following explicitly.

(Note: `bash -c 'echo hello'` — the literal milestone command — is a poor
multi-process test because `echo` is a bash builtin, so bash never forks. We use
`bash -c '/bin/echo hi | cat'`, which forks twice and execs two images, as the
real multi-process workload.)

## DynamoRIO fork/exec model (the design constraints)

- **Per-process clients.** Every injected process runs its own client instance
  with private globals. Cross-process sharing must use OS primitives (files,
  shared memory, pipes/sockets) — `drmgr`/`-multi_client` are *intra*-process
  only.
- **`fork` (clone without `CLONE_VM`)** fires `dr_register_fork_init_event` once
  in the child; the child inherited all of the parent's globals and fds via COW,
  so the callback must reset per-process state and re-open per-process outputs.
- **`execve`** wipes the address space; DR re-injects and `dr_client_main` runs
  fresh in the new image. The **pid is unchanged** across exec. So a
  fork-then-exec child is naturally reset by the fresh init.
- **Threads (`CLONE_VM`)** are handled automatically (`thread_init`); no
  follow_children needed and no cross-process concern.
- **Canonical multi-process pattern** (drcachesim `-offline`, Dr. Memory):
  each process writes its own output file (named by pid) and a **separate
  offline step aggregates** them. Online aggregation instead streams to one
  coordinator process over a named pipe. The offline pattern is the recommended,
  robust default; M4 uses it.

## What M4 delivers (this PR)

A coordinator built on the offline pattern, wired end-to-end:

1. **Explicit `-follow_children`** in the launcher (`launcher.rs`) — the
   whole-process-tree behavior is now intentional and unit-tested, not implicit.

2. **Coordinator IPC** (`reverie-dbi/src/coordinator.rs`, new; unit-tested).
   Because clients cannot share `static`s, coordination flows through a shared
   *coordinator directory* named by `REVERIE_DBI_COORD_DIR`. The path travels in
   the environment, so it survives both `fork` (inherited) and `execve` (env
   preserved) and every followed child finds the same coordinator. Each process
   writes exactly one record — `pid ppid syscalls comm` — at exit, using
   **DynamoRIO-safe I/O** (`dr_open_file`/`dr_write_file`; the Rust side owns all
   logic and the format, the C client supplies only the raw write). The offline
   aggregator reads every record, reconstructs the process **tree** from the
   parent links, and assigns **deterministic in-tree ids** independent of the
   (nondeterministic) OS pids — the "process-tree determinism" primitive — plus a
   tree-wide syscall total.

3. **`fork_init` reset** in the native client (`dr_register_fork_init_event`) so
   a forked child that does *not* exec starts counting from zero instead of
   inheriting the parent's COW counters.

4. **A real recorded `ppid`** in the coordinator records (the process-tree
   identity M2 stubbed as `None`).

### Validation

- `cargo test -p reverie-dbi`: 24 unit tests green, including 9 new coordinator
  tests (wire round-trip, lenient parse, collect/skip-noise, exec dedup,
  tree/preorder id assignment, pid-independent deterministic render).
- End-to-end under DynamoRIO (`tests/multiprocess_coordinator.rs`, gated on
  `DYNAMORIO_HOME`; also `scripts/test-multiprocess.sh`): `bash -c '/bin/echo hi
  | cat'` produces **3 coordinated processes** —
  ```
  reverie-dbi coordinator: 3 process(es), 641 syscalls total
    proc 0 parent - comm bash  syscalls 266
    proc 1 parent 0 comm cat   syscalls 191
    proc 2 parent 0 comm echo  syscalls 184
  ```
  bash is the root (its parent is the untraced launcher); echo and cat are its
  children. Two runs with entirely different OS pids yield a **byte-identical**
  deterministic rendering (the test asserts this).
- No single-process regression: `/bin/echo hello` and the literal
  `bash -c 'echo hello'` both still print and exit 0. Client is release-built
  (debug frames overflow DR's ~56K client stack).

## What M4 does *not* do (and why)

- **No live cross-process shared `GlobalTool` state during execution.** The
  coordinator is *offline* (records aggregated after exit), matching drcachesim
  `-offline`. A live shared global (e.g. a determinism clock consulted by all
  processes mid-run) needs an *online* coordinator process plus the cooperative
  executor and cross-thread scheduler that M3 blocker C/D already require for a
  single process. Deterministic *counts* here hold for identical inputs; they are
  not made input-invariant.
- **No process-spanning scheduler / determinism.** Same root cause as M3: the
  DBI driver polls each handler once and cannot resume mid-handler cross-thread
  (now cross-process) suspensions.
- **Sibling ordering for identical-shape siblings.** Deterministic ids order
  siblings by `(comm, syscalls, pid)`; the trailing pid is only a last-resort
  tie-break between siblings that are otherwise identical. Perfect ordering there
  needs the parent's fork sequence (a live-coordination follow-up).
- **`hermit run --backend dbi`** still does not exist (M3 blocker A: hermit-cli
  hardcodes the ptrace backend). The DBI vehicle remains the standalone `drrun`
  client.
- **Edge cases** noted but not hardened: `vfork`/`posix_spawn` (followed via the
  clone/exec paths), setuid exec (DR generally cannot inject — an instrumentation
  gap), static executables (handled by DR early injection).

## Roadmap to real multi-process determinism

1. **Online coordinator process** (upgrade offline → live): a long-lived
   aggregator the launcher spawns; clients stream over a named pipe/socket
   (`REVERIE_DBI_COORD_DIR` generalizes to a channel address). Enables a shared
   global consulted during execution.
2. **Cooperative executor + cross-thread scheduler in the driver** (M3 steps
   1–2): prerequisite for any live rendezvous, single- or multi-process.
3. **Cross-process scheduler**: extend the scheduler to park/wake guest threads
   across process boundaries via the online coordinator — the multi-process
   analog of Detcore's ptrace scheduler.
4. **Deterministic fork-order capture**: record each child's index among its
   parent's children (needs the pre-exec flush pattern at `SYS_clone`/`fork`) to
   make sibling ids fully deterministic.
5. **Packaging + backend selection** (M3 blockers A/B/F): unchanged.

## Bottom line

DynamoRIO already follows children; M4 makes the process tree **coordinated**:
an env-addressed, exec-surviving coordinator channel, per-process records via
DR-safe I/O, deterministic process-tree reconstruction, and real ppid identity —
proven end-to-end on a live fork/exec pipeline. It isolates the remaining work
to an *online* coordinator and a process-spanning scheduler, the natural M5+
milestones.
