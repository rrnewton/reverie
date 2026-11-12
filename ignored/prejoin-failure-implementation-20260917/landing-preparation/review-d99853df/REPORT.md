# Reverie fatal Tool failure source review

Review the complete committed change 114b309413612fafc2657c74e83811c71aac7b19..d99853df1ab677863f149e6c81dfa2d1147f886d in this registered Reverie slot. The source-v11 binding is 792a1dbe1188e2a306ba556411cac61d033ce257585f7bf03c825bfebea819ac and every committed source file was read back byte-for-byte against it. Tracked worktree/index are clean; no public push occurred. The root coordinator owns public review/landing.

A Tool callback may await a scheduler RPC while its owner joins a failed worker. Returning the failure only after joining cannot break that wait. This change publishes the first typed worker or child cause before retirement/join-dependent cleanup, synchronously calls GlobalTool's terminal publication hook, then wakes local multiwaiter subscriptions used by the actual handler driver. The selected typed cause and process/thread event are chosen together. Child fork preparation preserves the shared failure owner but establishes the child's process identity; host workers retain that process identity and publish their own TID.

Constructed ThreadState has one consuming owner across setup, preamble, spawn, gate cancellation and normal completion. Failure cancels every pending fork gate before any join; cancellation without a run failure consumes state normally rather than inventing RunAborted. Started descendants receive publication through the production driver. Worker hooks remain before leader hooks, normal status 37 and ordinary Start remain covered, and secondary cleanup failures stay separate from the typed primary. Public completion retains GlobalState and normalizes its error to the run's first published primary, following only primary/source chains rather than secondary cleanup entries. No normal errno/Go/exit response is used as fatal notification.

The nondefault native-test-support feature exposes only the small actual production ownership/gate/RPC entry used by Hermit's combined controls. It does not create a separate scheduler or pretend a native control executes guest instructions.

Independent review of source-v2 identified missing host-worker publication, missing cached/missing-status publication before joins, possible mismatched first cause/event, late terminal child-mode selection and nested fork identity. The current source addresses those actual producer boundaries and preserves normal no-context paths. A later review found ordinary fork Cancel erroneously produced RunAborted; that is corrected to consume without a primary unless failure exists. These are source-review findings already resolved, not evidence that every possible failure producer has been covered.

Validation provenance:

- Source-v9: 34 selected Reverie no-VM native controls passed out of an actual 443-test library inventory, all original 27 retained. Aggregate CPU: compile 85.366490 seconds, list 0.215455 seconds, native 0.229698 seconds. The native run took 1.095430646 wall seconds. Expected panic diagnostics from forced-failure controls remain retained.
- Source-v9: three separate KVM controls passed at 0.288700 CPU / 1.894176096 wall seconds with REVERIE_REQUIRE_KVM=1. They are vm::tests::nested_host_fork_failure_uses_descendant_process_and_worker_identity, vm::tests::real_fork_and_thread_wrappers_restore_both_capture_modes and vm::tests::page_fault_action_restores_complete_stopped_context. The last two execute actual vCPU instructions; the first constructs a real VM and checks nested identity.
- Hermit source-v3 plus Reverie source-v9: 51 selected Detcore and 9 selected Hermit native controls passed. The combined controls exercise actual GlobalState/Scheduler registration, terminal selection closure, constructed-child ownership and RPC cleanup through OS-thread handles. They do not execute guest instructions.
- Source-v10: workspace Clippy --locked --offline --workspace --all-targets --all-features -- -D warnings passed at 209.807869 CPU / 78.499523716 wall seconds. Native dependency build-script warnings remain visible. Its format check identified only the final-expression arm layout.
- Committed source-v11: final workspace format check passed at 1.642391 CPU / 2.370763758 wall seconds. The v9-to-v10 change removes an unnecessary return from the function's final match expression; v10-to-v11 is only its rustfmt layout. Existing v9 ELF/test evidence has not been relabelled as v11.

Every execution used the reviewed observer and actual safehermit service, aggregate CPU/wall/output limits, 16 GiB memory and zero swap, with complete accounting and independent inactive/empty service readback. Complete bounded refusals, compile failures and unexecuted preparations remain preserved. The prior whole-library fatal_worker_ro_delayed_waiter failure is still a limitation; no full-suite green is claimed.

Remaining coordinated obligations: Hermit's initialized-VM setup control and the unchanged real CLI pthread control are compiled but not yet executed on their former candidate; the final formatted-source rebuild and runs are being prepared. The latter checks exact ordinary stdout and separate strict verification for each backend, not canonical cross-backend INFO parity. The known virtual-PID SIGCHLD defect and adjacent fork-tree qualification remain outside this repair. The base scratch-hide/callback dual-error cleanup limitation remains unchanged.

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it

Review all changed production paths and tests, not only the displayed increments. In particular, check every fatal producer before joins, synchronization between GlobalTool publication and local notification, ownership after failed spawn/setup, ordinary cancellation with no failure, concurrent first-cause/event identity, parent/child/worker hook order and API compatibility for existing GlobalTool implementations. Test watchdog rescue must fail the test, and publication after the join or removal of the actual driver subscription must not pass. Give concrete source findings and a verdict on this exact commit; this package is implementation evidence, not independent approval.
