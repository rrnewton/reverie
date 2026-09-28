[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]

## Plain Language Summary and Project Impact

A fatal KVM worker error could leave a Tool RPC and an owner joining that worker waiting on each other. Publish the typed failure before retirement and joins, interrupt pending ordinary RPCs, and consume constructed child state exactly once. Keep healthy independent processes able to finish their writes and exit normally when their Tool has not terminated them. This is the Reverie half of the coordinated Hermit repair; Hermit also needs the scheduler terminal transition and completion caller.

## Determinism

One run-owned mutex selects the first typed cause and its real process/thread event together. The GlobalTool failure hook completes synchronously before local or process subscribers wake, allowing Hermit to close a selected scheduler transaction before cleanup proceeds. Explicit Tool-wide terminal notification retains priority over ordinary callbacks. Root callbacks observe the whole run; independent process callbacks observe their own process failure, while ordinary Guest RPCs separately observe run-wide failure before and after request polling. Consuming GlobalRPC remains available for exit hooks.

The handler driver consumes recorded runtime errors before accepting a callback's ready alternative. An interrupted RPC drops its ordinary request and failure wait, then stays pending safely if a callback polls it again. Failed cleanup cancels all pending child gates before joining any child; explicit ordinary and fatal commands preserve their different cleanup outcomes. Public completion preserves the first typed cause and valid worker context without duplicate diagnostics, and retains every distinct cleanup error.

## Linux Semantics

Fatal runtime failure remains separate from a guest errno, signal or ordinary exit. Started healthy thread peers retain existing pending/group cancellation status. An already-started independent fork can perform both original writes, naturally exit with status 0 and become waitable; its parent's fatal failure remains an error. Constructed children cancelled before admission are consumed without entering guest code. Normal successful child Start behavior, original worker-before-leader hooks, syscall negative returns, stdout/stderr and clock semantics retain their existing paths.

Caught worker panics publish before join-dependent cleanup and retain their original panic payload and diagnostic. This does not guarantee consuming hooks during arbitrary panic unwinding. The GlobalTool and consuming-hook documentation states the multi-publisher, synchronous-terminal and callback-drop contract.

## Validation

The final head is `9db60ab95587d4cb5e0438dfeca409471eb9baf5`, rebased normally onto main `526c21cf06ef9e5098ec9002b93e40e2022e798f`. Its 11 candidate files and complete main-to-head patch match qualified source v22 exactly; all 31 upstream changed paths are preserved. The old two-commit history and actual binaries remain retained. Final-head workspace/all-feature checks and emitted-executable comparison are still pending at this preparation cutoff; this body must be superseded with their actual outcomes before publication.

- Source v22 passed 44 selected native controls from an actual 453-method library inventory, including pending Tool RPC plus owned OS join, explicit gate causes, typed cleanup identity, process ownership and both select regressions. The native service used 0.244696 CPU / 1.610203477 wall seconds.
- The unchanged original qualification has accepted results for 4 VM and 22 static methods, including all ten exec diagnostic modes, all 17 leader-exit methods and all four terminal-fork methods. There were 25 accepted first attempts and one accepted retry after an observer accounting refusal on v22. The refused record remains refused; the observer and all assertions, admission rules, order and bounds were unchanged. Accepted services used 9.701618 CPU / 31.953927814 summed observed stage wall seconds, excluding the additional refused attempt.
- Workspace formatting and default-feature `reverie-kvm` all-targets Clippy with `-D warnings` passed on v22. Structured compiler stdout had no diagnostic messages. Real VM/static qualification required `REVERIE_REQUIRE_KVM=1` and actual `/dev/kvm` admission inside each observed service. Every accepted service retained complete CPU accounting, unchanged memory/output/deadline bounds and independent inactive/empty readback. All 27 qualification service instances, including the refusal, are terminal.

This is selected Reverie evidence, not whole-suite success or guest parity. The previous `fatal_worker_ro_delayed_waiter` whole-suite failure remains recorded. Successful exec cancelling an already-started sibling parked in ordinary RPC remains a separate existing defect; this repair does not fabricate a fatal error on successful exec. Scratch-hide/callback dual-error cleanup, arbitrary panic cleanup, the virtual-PID SIGCHLD defect and adjacent fork-tree qualification remain separate. Per-vCPU-exit async allocation and mutex cost are unmeasured. The final combined Hermit scheduler, initialized-VM setup and pthread qualification remain separate; no strict INFO, repeat determinism or canonical cross-backend parity result is inferred.

## Relationship to gVisor

This repairs the existing Reverie-to-Detcore failure boundary. KVM continues using the shared Detcore tool. The change imports no gVisor code and makes no new gVisor compatibility claim.

## Human Review Required

Trigger **2** applies: the new GlobalTool failure hooks, completion ownership and error propagation change a core abstraction. The coordinated Hermit scheduler transition separately meets trigger **4**. Retain the `post-facto-human-review` disclosure and labels. The actual Claude review of the earlier head requested changes; its findings and every first failure remain recorded. Independent Codex and actual Claude reviews must bind this exact final head before approval labels or landing. This preparation claims neither pending review approval nor pending execution success.
