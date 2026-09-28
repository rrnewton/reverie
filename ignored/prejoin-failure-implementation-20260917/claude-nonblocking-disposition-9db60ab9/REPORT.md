# Disposition of the final Claude review's nonblocking observations

Target: Reverie `526c21cf06ef9e5098ec9002b93e40e2022e798f..9db60ab95587d4cb5e0438dfeca409471eb9baf5`, tree `dd2542f238ef8f7e3cdfc86509e89b3e2e0d5d24`. Read-only follow-up to the complete actual Claude report, SHA256 `4c003aa043378481ce9370be882208bf74b17ad1f08663e03dd7612acaa6ca34`. This is an implementation author's disposition, not another independent approval or a new landing condition. No source, assertion, selection, bound or ref was changed; no test or guest was run for this disposition.

The approval remains applicable. Do not adopt the proposed publication-lock hoist or blanket HandlerSignal reordering as written. Both proposals omit an ordering obligation that the repair deliberately retains. The useful surviving follow-ups are described individually below.

## M1: publication locking and a missing GlobalState

Keep the current publication serialization at `failure.rs:86-115`. The primary mutex's `get_or_insert_with` selects an error/event pair; it does not establish completion of the synchronous Tool terminal transition. The publication mutex also covers that transition and the run notification. `FailureContext::publish` then immediately publishes the process notification after `RunFailure::publish` returns (`failure.rs:202-221`). Hoisting the alias check removes that explicit serialization before a caller can wake process cleanup.

The current production source does not establish a concrete early first-Arc escape: searches found `primary()` outside `RunFailure::publish`/completion only in tests, and the first publisher returns its shared error after its hook completes. That is narrower than proving an optimization safe across all later aliases, distinct concurrent publishers and future callers. In particular, the review's insertion-race argument alone does not prove the required terminal ordering. Do not present a hypothetical alias schedule as an observed defect in this head. If latency warrants this optimization, first prove the exact caller/alias contract and retain controls where a hook is held while both run and process cleanup subscribers must remain pending.

The weak-owner `expect` is an invariant check. All normal owned worker/child paths retain GlobalState, and the public completion clears `self.tool_failure` before consuming the final Arc (`runtime.rs:2181-2183`). No normal reachable failed upgrade was established here. Silently skipping the report and then marking publication complete would hide an ownership violation rather than implement the terminal contract. The surviving limitation is panic/poison behavior if a Tool's synchronous hook itself panics or ownership is violated. A separately designed panic-safe diagnostic path may be useful; it must not pretend the Tool terminal transition completed.

## M2: simultaneous terminal notification and handler signals

Keep the second failure poll ahead of ordinary handler outcomes (`runtime.rs:1152-1186`). Moving all HandlerSignal handling before it is unsafe as a general remedy: `ThreadCancelled` callers can call `start_pending_tool_children` (`runtime.rs:1313`, `1460`, `2947`), and the syscall TailInjected path proceeds to child starts, return-frame writes and process-action handling (`runtime.rs:2936-3030`). Those are ordinary continuation paths that must not win after the driver observes Tool-global termination. Returning the final run as an error later does not undo such intervening work.

The separate last-writer-wins observation is a real source property: `signal_handler` replaces its slot (`runtime.rs:730-735`). An independent-process callback can select another ready branch after its actual Guest RPC recorded RunAborted, then attempt `cancel_current_thread` or injection before returning to the driver. The existing select and select-then-await controls cover return/re-poll behavior, not that subsequent operation. Keep a bounded follow-up for actual Guest select-then-cancel and select-then-inject controls, with explicit handling of a recorded RuntimeError and no loss of a distinct real error. Giving RuntimeError precedence in the slot alone would not prevent an injection's side effects before it records a later signal, so this needs the operation boundary as well as signal storage.

Likewise, a ready callback error or committed tail metadata coinciding with terminal notification may deserve secondary diagnostic/state retention. Preserve it without accepting ordinary continuation or reopening child gates. This is a nonblocking remaining source question, not a newly measured failing outcome or permission to reorder the whole driver.

## M3: started independent children outside an interruptible Tool operation

The limitation survives. `driver_subscription(false)` observes the child's process; the actual Guest RPC also watches the run; every driver includes the explicit GlobalTool waiter (`failure.rs:194-221`, `runtime.rs:741-784`). A default GlobalTool deliberately does not terminate healthy independent processes. Once a fork gate is Started, fatal gate cancellation cannot stop it, and `finish_child_processes` still physically joins its handle (`executor.rs:2506-2557`). A child that never finishes ordinary work can therefore keep that join waiting.

The original terminal-fork qualification proves finite independent continuation: both writes, natural status 0 and waitability where required. It does not prove arbitrary child termination. Hermit's prepared GlobalState publishes an explicit terminal transition and waiter, addressing its scheduler/RPC dependency path. Merely adding that waiter cannot promise preemption of an arbitrary host operation that never returns to a polling boundary. Keep the latter distinction explicit in any future stuck-child investigation. Do not repair this by again interrupting all default-Tool independent work, which would violate the unchanged real fork continuation contract.

## L1: per-vCPU-exit cost

Retain as an unmeasured performance follow-up. The concrete target is `runtime.rs:2652-2658`: constructing and immediately polling `wait_for_failure` before each vCPU run, plus the callback driver subscriptions. Final control runtimes are correctness-test costs, not a throughput or allocation benchmark. Measure allocation, polling/locking cost and real guest exit counts before choosing a cache or fast-path change. Any optimization must retain before/after RPC checks and explicit Tool-global terminal priority.

## L2: phase attribution in error aggregates

Loss of some returned-error phase framing is a valid diagnostic follow-up. The typed aggregate retains original causes, but several phase labels now live in events rather than every rendered error; the exact old worker diagnostic remains preserved.

One premise in the review is incorrect: `run_with_tool` does emit `BackendFailure` without a RunFailure context. `runtime.rs:2068-2074` reports phase `direct Tool execution`, and the None branches of `notify_tool_exit` report thread/process hook phases (`runtime.rs:1551-1558`, `1582-1589`). A default no-op GlobalTool discards those events, so this correction does not eliminate the diagnostic limitation. If restored, use typed phase wrappers while preserving Arc identity, primary/source traversal, distinct cleanup errors and the unchanged integration strings.

## L3: ordinary Cancel for unstarted fork children

The call-graph qualification is useful and should remain explicit. The production Tool-action error path passes `failed = true` to `cancel_unstarted_tool_children` (`vm.rs:2006`); the ordinary helper is cfg(test). `discard_unstarted_child_process` subsequently calls ordinary cancel on that already-cancelled gate. The ordinary fork Cancel arm is therefore an adjacent negative/control path, not evidence that product code currently initiates that same ordinary fork cancellation. It is still useful to preserve that state-machine contrast. Ordinary CLONE_THREAD cancellation has real exec/group callers and remains separately exercised. No selector, assertion or gate should be removed to erase this distinction.

## L4: feature-only native helper accessors

The compilation concern is superseded by later exact-head evidence, which was outside Claude's source packet cutoff. Final `lint-v14` passed `cargo clippy --locked --offline --workspace --all-targets --all-features -- -D warnings` on this exact head. This includes the `native-test-support`-exclusive accessors, while the default native test population separately passed. It does not establish the combined Detcore control's runtime result.

The feature is intentionally nondefault and has an out-of-repository consumer: the prepared Hermit `kvm-native-test-support` feature forwards to it, and the authored Hermit unit selector includes that feature. Those current Hermit files were inspected read-only and hashed in INPUTS.json; their subsequent current-base compilation, inventories and execution belong to the separately owned integration. No feature exemption or default-feature expansion is needed here.

## L5: cleanup errors under a derived RunAborted primary

Keep the current distinction. A compound tree with RunAborted as its primary does not become a new real failure publication (`failure.rs:77-83`, `202-219`); its distinct cleanup errors remain in completion. This prevents a cleanup marker from claiming a new process failure or replacing the authoritative first cause.

The normal thread/process hook producer reports a real hook error before combining it with the derived cancellation (`runtime.rs:1546-1595`), so the composite early return does not prove that every such hook failure lacks an event. The constructed compound case shows the narrower notification omission described by the review. If consumers need a complete secondary diagnostic stream, design that separately from terminal cause selection and process interruption; do not indiscriminately republish the whole marker-primary tree.

## L6: recovered GlobalState on runtime failure

The specific public capability still needs the coordinated consumer qualification. The legacy Reverie API executes the new completion path but drops returned GlobalState when `completion.result` is Err, so the selected Reverie controls do not assert a downstream consumer using the recovered state.

The prepared Hermit production caller invokes `run_static_elf_with_tool_completion` and passes the completion into `finish_kvm_tool_completion` (`hermit-cli/src/lib.rs:2110-2151`). The separate real-VM setup method asserts that the completion exists despite a primary plus cleanup error, then sends it through that finisher (`hermit-cli/src/kvm_execution_tests.rs:133-160`). This is prepared source, not a passed current-base hardware test. Keep that qualification obligation, with both original setup cases and exact consuming-hook counts.

## L7: never-started fatal cancellation versus a started healthy peer

The asymmetry is intentional. `finish_unstarted_tool` passes `start_permitted = false` (`runtime.rs:1829-1854`), while the healthy-peer status rule requires a started CLONE_THREAD owner and a derived RunAborted primary (`runtime.rs:1489-1499`). Applying the healthy-peer status override to an unstarted fatal gate would again conflate construction failure with ordinary cancellation. The full Err remains authoritative, and a failed fork publishes Failed completion without a successful guest wait event (`vm.rs:2179-2187`). Retain explicit constructed-state consumption and exact task-generation checks in Hermit; do not manufacture registration just to retire such a child.

## Evidence and goalpost check

INPUTS.json SHA256 `a0886a66e274c2910e7ce9465dadd6849c88c610d186bd91cdf41eb38b574941` binds the report, exact Reverie source files and later validation records, plus the read-only Hermit context. No assertions were weakened, tolerances widened, exemptions added, cases skipped, comparisons relaxed, failures relabelled or checks deleted by this disposition. The final-head 44 native and original 26 VM/static results remain separate from the earlier v22 accounting refusal/retry and from future Hermit determinism/parity evidence.

Recommended follow-up order: complete the already prepared Hermit terminal/completion integration; separately reproduce the M2 post-interruption operation boundary; investigate an actual M3 nonreturning child without changing finite independent-process semantics; then measure L1 before optimization and improve L2 diagnostics when compatible. M1's ownership/panic limit remains documented, L3/L5/L7 remain explicit contract/coverage distinctions, and L4's exact-head compilation concern is resolved by the completed all-features check.
