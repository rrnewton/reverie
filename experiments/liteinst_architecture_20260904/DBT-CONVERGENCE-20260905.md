# DBT convergence: keep LiteInst/SUD as the implementation lead

## Evidence boundary

Source inspection only, 2026-09-05; no builds, guest executions, network,
agents, product edits, or hybrid-executor work. No runtime assurance level is
claimed. Existing reviews and completion claims were not used as evidence.

Citation roots and immutable revisions:

- **H**: `/home/newton/work/dev-hermit/worktrees/slots/liteinst-owner`,
  HEAD `4d8f866102882b6eeabdf99cc7e81433cc3c95c5`.
- **R**: `/home/newton/work/dev-hermit/worktrees/slots/liteinst-timer-delivery`,
  HEAD/base `8bb9f79350955d791f6f08f315ad0e30bb74afc3`.
- Every R citation refers to `git show 8bb9f79350955d791f6f08f315ad0e30bb74afc3:<path>`,
  not the concurrently edited worktree. Cited H tracked files had no diff from
  HEAD. H pins Reverie `320412c5967790939ebe405c73e394ffd9c41459`
  (`hermit-cli/Cargo.toml:57`): these inspected heads are not a validated pair.

HEAD/status snapshot at `2026-09-05T04:42:49-07:00`:

```text
H staged:   detcore-model/src/config.rs; detcore-model/src/happens_before.rs
H unstaged: scripts/check-reverie-pin.rs
H untracked: liteinst-runtime-build/detcore-runtime/
R unstaged:
  reverie-liteinst/Cargo.toml
  reverie-liteinst/src/bin/rpc_tool_guest.rs
  reverie-liteinst/src/bin/rpc_tool_guest/sud_only.rs
  reverie-liteinst/src/lib.rs
  reverie-liteinst/src/runtime.rs
  reverie-liteinst/src/tool_host.rs
  reverie-liteinst/tests/rpc_tool.rs
  reverie-preload/src/signal/native_frame.rs
  reverie-preload/src/signal/native_frame/tests.rs
R untracked:
  reverie-liteinst/src/bin/rpc_tool_guest/owned_cpuid.rs
  reverie-liteinst/src/owned_context.rs
  reverie-liteinst/tests/timer_signal_frame.rs
  reverie-liteinst/tests/timer_signal_frame/
```

These changes were neither evaluated nor modified. In particular, base-8bb
limitations below are not verdicts on the active owned-context/timer work.

## Seven findings

### 1. DBT really instantiates shared Detcore, but not every path reaches it

H `hermit-cli/src/lib.rs:2141` selects the Detcore native client and
`DbtRunner`; `detcore-dbt/src/lib.rs:1032` supplies its runtime library to the
native link (`R reverie-dbt/native/CMakeLists.txt:19`). Guest `Runtime` owns
`OnceLock<Detcore>` plus per-thread `ThreadRuntime` state
(`H detcore-dbt/src/lib.rs:563`); `Detcore::new` is called at line 1318.
The syscall path is native callback -> H runtime -> generic `DbtGuest<T>` ->
`Tool::handle_syscall_event` (`R reverie-dbt/native/client.c:3785`,
`H detcore-dbt/src/lib.rs:2212`, `R reverie-dbt/src/lib.rs:1608`).

However, copied pre-exec/vfork fallback still bypasses that handler
(`R reverie-dbt/native/client.c:3692`, `H detcore-dbt/src/lib.rs:1789`), and
the H wrapper performs syscall-specific validation/result/PRNG adjustments
outside it (`detcore-dbt/src/lib.rs:1977`, `detcore-dbt/src/lib.rs:2227`).
Do not equate a real Detcore instantiation with identical execution of every
syscall. Reverie's `PrototypeTool` is a separate path, not this CLI proof.

The earlier distinctions remain: H LiteInst CLI is ptrace-hosted Detcore
(`hermit-cli/src/lib.rs:2679`); SaBRe embeds `RemoteReverieAdapter<Detcore>`
(`detcore-sabre/src/lib.rs:196`); e9patch preprocesses then selects ptrace
(`hermit-cli/src/bin/hermit/run.rs:2550`). R direct tool-host APIs are not
automatic evidence of H CLI integration.

### 2. The shared Tool driver is the right starting point; DBT duplicates it

LiteInst, SaBRe and e9patch already call preload's `drive_tool_syscall`
(`R reverie-liteinst/src/tool_host.rs:474`,
`experimental/reverie-sabre/src/reverie_adapter.rs:590`,
`reverie-e9patch/src/tool_host.rs:343`). Its `TailResult` and restart loop
consume private `ERESTARTSYS` rather than exposing it
(`R reverie-preload/src/tool_host.rs:215`, `reverie-preload/src/tool_host.rs:241`).
Their concrete Guest/state holders remain separate.

DBT has its own `TailInjectResult`, `run_ready` and `run_tool_syscall_inner`
(`R reverie-dbt/src/lib.rs:643`, `reverie-dbt/src/lib.rs:1326`,
`reverie-dbt/src/lib.rs:1575`), with no equivalent shared restart loop there.
Actual `run_ready` uses a thread-unpark waker and timed parking, despite the
earlier comment about avoiding TLS park APIs. Do not transplant it into SIGSYS.
The useful DBT idea is its explicit **deferred exact syscall** outcome: drop
the future and Rust borrows before native execution/blocking/lifecycle work
(`R reverie-dbt/src/lib.rs:410`, `reverie-dbt/native/client.c:3808`). Preserve
that outcome in a common driver; a mechanical replacement by the narrower
preload outcome set would lose semantics.

### 3. RPC shares a server/protocol, not one client or ownership policy

DBT's launcher creates the coordinator's `GlobalState`, `RpcServer`, and socket
environment (`R reverie-dbt/src/launcher.rs:546`). `DbtGuest::send_rpc` routes
there when configured, otherwise directly to an in-process global
(`R reverie-dbt/src/lib.rs:289`). H also constructs a guest-local GlobalState
and runs its external scheduler (`detcore-dbt/src/lib.rs:1153`,
`detcore-dbt/src/lib.rs:1183`); this is not a single cleaned-up ownership model.

DBT duplicates framing/`RequestEnvelope` (`R reverie-dbt/src/sync_rpc.rs:55`).
Its ordinary client is TLS-local, reconnecting on PID/path change (line 126);
the actual guest route instead uses a cached global mutex plus uncached
connections on contention, with I/O supplied by `SyscallInvoker` (line 182).
This avoids waiting forever on an inherited locked mutex, but is not the
connection topology to copy wholesale.

LiteInst already uses common `BlockingRpcClient` with fork reconnect and
runtime-domain protection (`R reverie-liteinst/src/rpc.rs:95`,
`reverie-liteinst/src/rpc.rs:126`); SaBRe has protected per-thread clients
(`R experimental/reverie-sabre/src/reverie_adapter.rs:345`); e9patch wraps
preload `CoordinatorClient` (`R reverie-e9patch/src/rpc.rs:18`). Unify codec,
handshake and connection lifecycle, parameterizing transport I/O. Before
multithread support, retain independently blocking per-thread connections;
the common client's rationale is explicit at
`R reverie-rpc-transport/src/blocking_client.rs:32`.

### 4. Reuse explicit context ownership, not DynamoRIO's safety assumptions

DBT carries per-thread Rust state through native scratch/TLS, not a generic
signal-frame pointer (`H detcore-dbt/src/lib.rs:587`,
`R reverie-dbt/native/client.c:3862`). Native storage uses `dr_thread_alloc`
and `drmgr_set_tls_field`; Rust state is separately boxed (H line 2048).
`DbtGuest` uses `LocalMemory`, register callbacks and optional fault-safe
backtrace reads (`R reverie-dbt/src/lib.rs:319`, `reverie-dbt/src/lib.rs:360`,
`H detcore-dbt/src/lib.rs:2208`). Register writes have a genuine DR PC-update
restriction (`R reverie-dbt/src/lib.rs:369`).

Adopt the explicit owned-context/access-callback contract in the leading
LiteInst path, not DR allocation/TLS machinery. LiteInst already isolates
callback allocations in its reusable arena and brackets runtime entry
(`R reverie-liteinst/src/patch_alloc.rs:8`,
`reverie-liteinst/src/runtime_domain.rs:17`). Its SUD contract explicitly does
not qualify asynchronous libc/TLS/Tool reentry
(`R reverie-liteinst/src/tool_host.rs:150`). DBT's signal hook restores a
translated wait argument then returns `DR_SIGNAL_DELIVER`
(`R reverie-dbt/native/client.c:4171`); it is not a reusable Detcore/SUD signal
multiplexer. DR callback context, SIGSYS/native-frame restoration, SaBRe
callbacks and e9patch payload entry need boundary adapters.

### 5. DBT clocks/timers are not a model to transplant

`DbtGuest::read_clock` returns a captured branch count; both timer setters
return `ENOSYS` (`R reverie-dbt/src/lib.rs:421`). Native optional branch-quantum
preemption injects `sched_yield`, not `Tool::handle_timer_event`
(`R reverie-dbt/native/client.c:329`), and native RDTSC advances a separate
fixed-stride counter (line 1255); CPUID also has native tables (line 1189).
These are not shared continuous Detcore instruction/time semantics.

LiteInst already connects preload `BoundaryHooks` to `InGuestRcbCounter`
(`R reverie-liteinst/src/clock_control.rs:122`). At base 8bb its SUD-only
installer rejects instruction/vDSO subscriptions and timer requests
(`R reverie-liteinst/src/tool_host.rs:223`, `reverie-liteinst/src/tool_host.rs:1125`).
SaBRe's zero clock and e9patch direct-host unsupported clock are likewise not
solutions (`R experimental/reverie-sabre/src/reverie_adapter.rs:1397`,
`reverie-e9patch/src/tool_host.rs:547`). Keep continuous time and explicit
capability refusal; defer DBT timer/instruction convergence, never weaken the
LiteInst clock to match these implementations.

### 6. Borrow DBT's lifecycle/evidence contracts, not another logging runtime

DBT distinguishes image generations, thread state, parent handoff and failed
exec (`H detcore-dbt/src/lib.rs:571`, `detcore-dbt/src/lib.rs:608`,
`detcore-dbt/src/lib.rs:1759`). Its callback ABI versions/sizes are checked
(`H detcore-dbt/src/lib.rs:2270`). Its subscriber separates diagnostic and
canonical evidence output (H line 1070); the collector refuses missing image
or child completion (`R reverie-dbt/src/evidence.rs:653`). These are useful
typed contracts for a shared runtime and its tests.

LiteInst already has a dedicated, bounded, independently drained guest-log
channel and refuses missing completion; that is not missing functionality
(`R reverie-liteinst/src/guest_log.rs:26`, `reverie-liteinst/src/guest_log.rs:54`,
`reverie-liteinst/src/guest_log.rs:295`). DBT's additional image/exec/expected-child
accounting is the useful extension when lifecycle scope expands. SaBRe's raw
Detlog forwarder remains another sink (`H detcore-sabre/src/lib.rs:102`).

### 7. Ranked convergence: one runtime, separate mechanism adapters

1. **Apply now:** keep LiteInst/SUD as the implementation lead; make shared
   driver/outcome and RPC codec/ownership contracts engine-neutral. Preserve
   current restart policy and reserved runtime/log descriptor protection.
2. **Apply now:** use DBT's explicit deferred operation and context/access ABI
   ideas to sharpen the owned-context and borrow-release boundaries already
   being developed. Retain LiteInst's allocator/runtime-domain isolation.
3. **Apply next as lifecycle scope grows:** common per-thread connection
   ownership, fork/exec reconnection, generation/readiness and log-completeness
   contracts. Do not copy DBT's local-global fallback or its contended-client
   workaround into the target design.
4. **Defer:** migrating DBT onto that runtime, its copied-child exceptions,
   native identity/instruction policy, and real clock/timer delivery. Preserve
   DR-specific loading, mcontext/PC rules, app-syscall continuation and event
   callbacks as adapters, not competing syscall determinization code.

Owner goal **“100% same syscall handler execution/RPC” is feasible as shared
source and policy**: one Detcore Tool dispatch/restart/lifecycle driver and one
coordinator protocol/connection implementation, with small context, syscall-I/O,
loader and continuation adapters. It is **not true today**, nor a promise of
identical native instruction sequences or all-guest coverage: DBT has explicit
bypasses and outside-Tool policy, while SUD base coverage is deliberately narrow.
The target is one engine-neutral runtime, not four determinism implementations.
The no-patching qualification remains one `guest-local/SUD, patch-engine=none`
configuration; **no SUD-only DBT mode or relabeled DBT test is proposed**.
