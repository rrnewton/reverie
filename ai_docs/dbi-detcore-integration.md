# DBI M3: Running Detcore over the DynamoRIO (DBI) Guest — feasibility analysis

Status: analysis + proof-of-concept. Milestone DBI-M3 (follows M1 audit, M1.5
simple tools, M2 Guest-trait gaps). Grounded in `rrnewton/reverie` (this repo,
`reverie-dbi`) and the `hermit` monorepo (`detcore`, `hermit-cli`).

## Question

> Can `hermit run --backend dbi --strict -- /bin/echo hello` run Detcore over
> the DBI Guest and produce deterministic output?

**Short answer: not this milestone.** The Guest *handle* is ready (M2 completed
it), but Detcore-over-DBI is blocked by an execution-model mismatch, a cross-repo
dependency cycle, and the fact that hermit has no backend selection at all. This
doc pins down each blocker with evidence, and delivers the largest step that *is*
provable today: a Detcore-**shaped** tool (real global state + async RPC + thread
lifecycle) running over the live DBI Guest under DynamoRIO, deterministically.

## The two-repo, two-runtime picture

| | ptrace path (works today) | DBI path |
|---|---|---|
| Where the Tool lives | `hermit/detcore` | would need to be in the DR client |
| Backend | `reverie-ptrace::TracerBuilder::<Detcore>` (`hermit-cli/src/lib.rs:81`) | `reverie-dbi` client `.so` loaded by `drrun` |
| Executor | full **tokio** current-thread runtime (`#[tokio::main]`) | **single poll** per syscall (`run_ready`) |
| Global state owner | the tracer process | none (hardwired `()`) |
| Reverie rev | `facebookexperimental@96693397` (git dep) | `rrnewton@69f47d9` (this repo) |

## Blockers (evidence-grounded)

### A. hermit has no `--backend` selection
`hermit-cli` hardcodes `reverie_ptrace::TracerBuilder::<Detcore>`
(`hermit-cli/src/lib.rs:81,103`), under `#[tokio::main(flavor = "current_thread")]`.
`--strict` is merely the default determinism mode, not a backend
(`hermit-cli/src/bin/hermit/run.rs:75-81`). There is no code path that would
instantiate a DBI backend. So the literal command in the task does not exist.

### B. Cross-repo dependency cycle
Detcore lives in `hermit` and depends on `reverie` (git dep). The DBI client
(`libreverie_dbi_client.so`) is built from `reverie-dbi` (this repo) with a
compiled-in `static PROTOTYPE_TOOL`. Putting Detcore *into* the client would
require `reverie-dbi → detcore`, i.e. a cycle (detcore already depends on
reverie). `hermit-cli` does not even depend on `reverie-dbi`. There is no runtime
tool selection in the client — "run tool X on DBI" means *edit the client and
rebuild it* (see the M1 audit).

### C. Execution-model mismatch (the deep one)
Detcore's `handle_syscall_event` **suspends mid-handler** waiting on the central
scheduler, which is how it serializes threads for determinism:
- `self.pre_handler_hook(guest, false).await` (`detcore/src/lib.rs:964`)
- `resource_request(guest, req).await` (`detcore/src/lib.rs:409, 754`)
- `tool_global::thread_start_request(...).await` (`detcore/src/lib.rs:876`)
- `trace_schedevent(...).await`

These awaits are resolved by the *global scheduler* as **other** threads make
progress — they are cross-thread rendezvous, not local computation. The DBI
driver polls each handler **once** (`reverie-dbi/src/lib.rs`, `run_ready`); M2
added handling only for the *terminal* `tail_inject` suspension. A mid-handler
suspension that expects to be resumed later cannot be driven — the future would
be dropped. Driving Detcore needs a cooperative executor plus a scheduler task
that can park/wake guest threads.

### D. Global state + config have no owner in the client
The DBI client hardwires `static GLOBAL_STATE: () = ()` and `static CONFIG: ()`.
Detcore has `type GlobalState = GlobalState` (large) and reads
`guest.config()` (a `DetConfig`) inside the handler (`detcore/src/lib.rs:978`).
Because the DBI backend is in-process, a non-`()` global is actually *callable*
directly (the DBI `send_rpc` already forwards to `receive_rpc` generically) — but
there is no owner instance and no executor to run the scheduler behind it.

### E. Callback coverage
The DBI driver dispatches **only** `handle_syscall_event`. Detcore also needs
`init_thread_state` (deterministic thread id + per-thread logical clock),
lifecycle (`handle_thread_start`, exit hooks, `handle_post_exec`), signals, and
timers (scheduler preemption). None are dispatched today (M1 audit, Table 2).

### F. Reverie revision skew
This repo's DBI work is `rrnewton@69f47d9`; hermit pins
`facebookexperimental@96693397`. The `Tool`/`Guest` trait vintages differ, so
even a compiled-in Detcore would need the revs reconciled first.

## What *is* provable today (this PR)

M2 made the Guest **handle** Detcore-ready (memory, regs, inject, tail_inject,
stack, thread_state, and the generic `send_rpc`/`config`). To show the interface
— not just the individual methods — can host a real determinism tool, this PR
adds `reverie-dbi/src/detcore_proof.rs`: a Detcore-**shaped** tool with

- a real, non-`()` `GlobalTool` state (a logical clock ≈ Detcore's global
  scheduler/clock) consulted per-syscall via **async RPC** through the Guest;
- a real, non-`()` `ThreadState` seeded by **`init_thread_state`** (a
  deterministic `dettid`, like Detcore's);
- a real, non-`()` `Config` read via `guest.config()`.

It is dispatched through the live `DbiGuest` two ways:

1. **Unit tests** (`cargo test -p reverie-dbi`) — the important property is that
   they *compile*: `DbiGuest: Guest<DetProofTool>` for non-`()` associated
   types. They also check the clock advances deterministically.
2. **End-to-end under DynamoRIO**, gated by `HERMIT_DBI_DETPROOF=1`:

   ```
   $ HERMIT_DBI_DETPROOF=1 drrun -disable_rseq -c libreverie_dbi_client.so -- /bin/echo hello
   hello
   reverie-dbi detproof: dettid 1 handled 111 syscalls; logical clock = 111
   ```
   With `HERMIT_DBI_DETPROOF_SEED=1000` the clock ends at `1111` (config
   plumbing), and two runs are byte-identical (determinism). This is the closest
   achievable analog to `hermit run --backend dbi` today.

Because the RPC resolves synchronously in-process, the single-poll driver drives
it to completion. The gap between this and real Detcore is exactly blocker C
(mid-handler cross-thread suspension) plus B/A/F (packaging and wiring).

## Roadmap to real Detcore-over-DBI

1. **Cooperative executor in the driver** (lifts C for one thread): replace the
   single poll with an executor that can resume a handler across suspensions,
   while still dropping the terminal `tail_inject` suspension. `futures`'
   `block_on` is close but cannot by itself coordinate cross-thread wakeups.
2. **Scheduler task + thread parking** (lifts C for many threads): a global
   scheduler that other guest threads advance; guest threads park on the DR
   client side until granted. This is the hard part and mirrors Detcore's
   ptrace scheduler.
3. **Real GlobalState owner + config** (lifts D): a process-global instance
   constructed from a real `Config`; `detcore_proof.rs` shows the in-process
   ownership pattern.
4. **Dispatch the missing callbacks** (lifts E): `init_thread_state` (shown
   here), lifecycle, signals, timers — the last needs the native RCB trap from
   the M2 `TODO-STUB(#31)` follow-up.
5. **Packaging** (lifts B/F): reconcile reverie revs, then either (a) make the
   DBI client a *plugin host* that loads a tool `.so` hermit builds from Detcore,
   or (b) invert control so hermit drives DynamoRIO and Detcore runs in the
   hermit process with the client calling back. Both are large; (a) avoids the
   dependency cycle by keeping Detcore out of `reverie-dbi`.

## Bottom line

The DBI **Guest** is ready for Detcore; the **driver** (executor + scheduler)
and the **integration structure** (cross-repo packaging, backend selection) are
not. This PR proves the Guest can host a Detcore-shaped tool end-to-end and
isolates the remaining work to the executor and packaging, which are the natural
M4+ milestones.
