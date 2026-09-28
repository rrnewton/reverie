Source-ready author release; no execution or approval

Implemented ten bounded wait-registration declarations on the dispatched Reverie branch codex/kvm-parity-land-20260918, underlying base 91110d249ffd8957267d71fab8c83d9636105efe. BASELINE.json and before/ bind the exact runtime and action-author vm source used. The vm basis aff8fdbdfe2472ab9baf6aae82471d15e8d3f8cc31d04c7833b54034ed730203 was reauthenticated before applying the separately proposed Host/parking hooks after root explicitly released that file to this author. SOURCE.json and after/ bind this unformatted author release; AUTHOR.patch contains the full changes. VM-HOOKS.patch preserves the separate original proposal. Root owns module includes, formatting, compilation and qualification.

Files released

- NEW reverie-kvm/src/vm/entry_wait_tests.rs: ten declarations below.
- reverie-kvm/src/runtime.rs: cfg(test) scoped observation module and two cfg(test) calls in the actual Tool ELF main retry loop, immediately before gate subscription and after gate plus cancellation subscription, before the unchanged admission/stop predicates.
- reverie-kvm/src/vm.rs: the exact analogous two hooks in Host ELF main and two in park_process_action. These were applied only after root authorized the released vm file. The action author's observations remain intact. The complete old vm::tests suffix is byte-identical to the captured action-author basis.

Every preexisting source line is retained in runtime.rs and vm.rs: a line-by-line comparison reports insertions only. No non-test statement, old test, raw-main stop contract, production API, comparison or selection was changed. Root should include vm/entry_wait_tests.rs inside vm::tests. Hook code is cfg(test), current-host scoped, restores prior registration through RAII, does not carry a scope across host migration, and invokes closures outside the TLS borrow and production gate/group locks.

Declarations (prefix vm::tests::entry_wait_tests::)

host_elf_stop_before_retry_subscription_does_no_entry_work
host_elf_stop_after_retry_subscription_does_no_entry_work
host_elf_same_run_second_close_waits_before_fresh_reopen
tool_elf_group_stop_before_retry_subscription_does_no_entry_work
tool_elf_group_stop_after_retry_subscription_does_no_entry_work
tool_elf_global_stop_before_retry_subscription_does_no_entry_work
tool_elf_global_stop_after_retry_subscription_does_no_entry_work
tool_elf_same_run_second_close_waits_before_fresh_reopen
parking_stop_before_retry_subscription_preserves_disposition
parking_stop_after_retry_subscription_preserves_disposition

The two parking declarations each cover both real group cancellation and a real non-fused one-shot async stop future, so ten declarations contain twelve concrete variants. This is inventory, not a pass count.

Exact measured intervals encoded by the source

Each main control first performs ordinary setup and reaches the existing CountedVcpu RunProbe.before_run seam. That hook arms the existing separate descriptor-access, mask, RUN, clock-begin/interval and exit observations, then finishes close 1 while every participant is stopped. The actual main wait must return Pending through ObservedChange and its existing PrepareProbe notice. Because the initial close can itself wake the old subscription, the test records the established denied-admission/wait counts rather than claiming an invented first-poll count.

Only after that real closed wait does the controller arm a registration-window action and notify the unchanged gate. The exact next loop's before-subscription hook requests stop before registration, or its after-subscription hook requests stop after both real subscriptions and before the existing predicate check. Exact hook event sequences must show which window ran. The stop variants require no additional denied admission/wait after stop, zero descriptor access/mask/RUN/clock/exit work across the entire armed interval, and terminal selection while close 1 remains held. Host returns its exact group exit code before reopen. Tool enters and actually polls its consuming hook to Pending with exact group code or 255 for scoped GlobalTool failure; that hook remains held until the controller has verified the selected terminal state and reopened the unchanged Mapping. Final Tool completion must preserve exact RunAborted versus success, once-only consuming hooks and GlobalState destruction, no extra report_backend_failure call, and empty owned worker handles.

The two same-run positives release close 1, then acquire and finish a separate close 2 at the next before-subscription hook. A new real pending wait is required with exactly one additional denied admission and one additional Pending observation. There is no counter reset and no second backend invocation. Close 2 is retained through the zero-work check; its fresh reopen must cause exactly one further subscription/recheck and one finite exit_group guest operation, one mask installation, one untracked Host RUN or tracked Tool RUN, and the corresponding exact Tool clock begin/interval counts. An extra main retry, extra guest exit or lost second wait fails.

The parking controls close after ordinary fixture setup and directly poll the real park_process_action future. In this closed-before-preparation path its only awaited suspension is the real gate/cancellation/stop select; therefore a delegated Pending together with the observed before/after registration binds the first suspension to that actual select. No fabricated pending future replaces it. After the unchanged-gate wake, the chosen registration hook requests group cancellation or completes the actual one-shot stop. The helper must return exactly Ok(false) versus Err(RunAborted) while the close token remains held, with zero descriptor/mask/RUN/clock/exit work. The one-shot body must complete exactly once; using an ordinary async future exposes any invalid repoll after completion. Register/trampoline preservation is inspected only after terminal return and reopen. The counter interval is explicitly ended before those fixture-only reads; counters are not reset. These cases qualify the helper's registration slice, not preparation after internal-HLT staging or the four caller branches.

Bounds and rescue

All waits use the existing five-second fixture bound. Host runs on its own real thread, returns its still-owned backend to the controller before any gated destruction, and is physically joined after completion. Controller assertion failures reopen, release cleanup/stop dependencies and request group stop. If the actor still does not return within the finite rescue bound, the test fails before entering a potentially blocking join; it does not claim cleanup success. Tool and parking futures are manually polled on their owning test host, with the existing finite completion helper. Every success path drops its close before ordinary dependent cleanup or memory inspection. No sleep chooses the registration order.

Goalpost assessment and limitations

No assertion weakened; no tolerance widened; no backend/case exempted or skipped; no failure renamed as success; no existing check deleted. KVM setup failure is an assertion, not a skip. Descriptor observations are an upper bound on accesses, not an asserted numeric ioctl total. Exact zero remains the requirement. This work explicitly targets retry registration after a proven wait; it does not relabel that as an unobserved first-poll race. Separate-run isolation is not substituted for close 2 in the same run. Existing main and parking/caller positives remain required.

No compiler, formatter, test binary, guest, model, network or SCM command was executed by this author. This is uncompiled source and an author handoff, not self-approval or executed evidence. Root must include, format, compile and qualify all ten declarations alongside prior selected controls, then obtain independent source/evidence review. No full scheduler/Linux semantics, determinism/parity or landing verdict is claimed. The prior bounded grounding limitations remain unchanged.
