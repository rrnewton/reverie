[hermit2, degraded-unresolved, gpt-6-astra, devbig014, role=impl]

## Plain Language Summary and Project Impact

A failed KVM worker could leave a Tool RPC and the owner joining that worker waiting on each other. Publish the first typed failure before retirement and child joins, wake subscribed handlers, and consume constructed child state without starting the guest. This is the Reverie half of the coordinated Hermit repair for reliable KVM failure cleanup; Hermit also needs the scheduler terminal transition and completion caller.

## Determinism

One run-owned mutex selects the first typed cause and its process/thread event together. Publication calls the GlobalTool hook synchronously before notifying local waiters, so the paired Hermit implementation can close a selected scheduler transaction before consuming cleanup wakes a joiner. The actual handler driver subscribes before polling a Tool callback and checks terminal state before returning an ordinary result.

Failed cleanup cancels every pending child gate before joining any child. Each constructed Tool state retains one consuming owner through setup, spawn refusal, cancellation and completion. Published fatal errors choose cancellation after local success too; ordinary successful completion retains Start. Public completion preserves the first typed cause, keeps secondary cleanup failures separate and returns GlobalState for the caller's cleanup. No guest clock, runnable ordering or normal syscall result is derived from failure-publication timing.

## Linux Semantics

Normal status, syscall negative returns, worker-before-leader hooks and cancellation behavior retain their existing paths. Cancelling an unstarted fork without a published failure consumes its state normally. Fork descendants establish their own process identity, and a failing host worker publishes its actual thread identity within that process. Fatal runtime failure remains separate from a guest errno or exit status.

## Relationship to gVisor

This repairs Hermit's existing Reverie-to-Detcore failure boundary. KVM continues executing the shared Detcore tool; this change imports no gVisor code and makes no new gVisor compatibility claim.

## Validation

- 34 selected Reverie native controls passed from an actual 443-test library inventory, retaining all 27 original selected controls. The native invocation used 0.229698 aggregate CPU seconds and 1.095430646 wall seconds.
- Three separate KVM controls passed with `REVERIE_REQUIRE_KVM=1`: nested fork/worker identity, real fork/thread capture restoration, and complete stopped-context restoration after a page fault. The last two execute vCPU instructions. Together they used 0.288700 CPU seconds and 1.894176096 wall seconds.
- The paired Hermit candidate passed 51 selected Detcore and 9 selected CLI native controls, including real scheduler registration/selection and OS-thread/RPC cleanup. These do not execute guest instructions.
- Workspace Clippy passed with `--locked --offline --workspace --all-targets --all-features -- -D warnings`, using 209.807869 CPU seconds and 78.499523716 wall seconds. The final workspace format check passed after its formatting-only correction.

The native/VM results precede the final-expression Clippy and formatting-only corrections. Their original source and executable identities remain preserved; they are not relabelled as measurements of this commit. All executions retained actual safehermit service accounting, CPU/wall/output bounds, 16 GiB memory, zero swap, and independent inactive/empty readback.

These selected controls do not establish a whole-suite green or guest parity. The earlier whole-suite `fatal_worker_ro_delayed_waiter` failure remains recorded. Hermit's initialized-VM setup and existing real pthread qualification are separate remaining checks. The known virtual-PID SIGCHLD defect, adjacent fork-tree qualification, and existing scratch-hide/callback dual-error cleanup limitation remain outside this repair.

## Human Review Required

Trigger **2**: this changes the Reverie GlobalTool core API, KVM Tool completion ownership, and fatal failure propagation across runtime workers. The coordinated Hermit scheduler change separately meets trigger **4**. Independent Claude and Codex reviews must bind the actual commit; the label routes subsequent human review.
