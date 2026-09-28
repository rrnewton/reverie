# Reverie source v22 original qualification continuation

All 18 continuation methods passed with complete observed accounting. Together with the 8 accepted methods from execution v4, the unchanged original population has 4 VM and 22 static methods accepted. There are 25 accepted first attempts and one accepted retry on v22; this is not 26 clean first attempts.

The retry is leader_exit::failed_worker_preserves_execution_and_cleanup_errors. Its original v4 result remains an accounting refusal: the raw method passed, but observer_error was OSError: [Errno 19] No such device and comparison_eligible was false. Recovered final accounting and independent terminal readback do not relabel it. The observer was not modified, and no identity, assertion, admission rule, ordering or bound changed.

The original backend_exec_failure_joins_live_fork now passes with both actual child writes, natural status 0, successful wait event, all consuming hooks and physical completion before return. Its raw result retains worker 3 EIO. The adjacent backend/injected failure methods preserve the distinct ENOSPC, EACCES and E2BIG cleanup causes. All four original terminal_fork methods passed. The original ten exec-worker error modes and all 17 leader-exit methods are included in the combined population.

The continuation consumed 6.576396 CPU seconds and 19.448820224 summed observed stage wall seconds. The 26 accepted services consumed 9.701618 CPU seconds and 31.953927814 summed stage wall seconds. The refused attempt is additional, not included in these accepted totals. All 27 dispatched services, including the original refusal, received fresh inactive/empty readback (MainPID 0, empty ControlGroup). Both emitted ELFs still match separately retained bytes and full source/input checks passed.

Each service kept the original 30 CPU / 60 wall seconds, 16 GiB / zero swap and 1 MiB output bound, serial exact selection, REVERIE_REQUIRE_KVM=1 and real /dev/kvm API admission inside the observed service. All raw stdout/stderr were read in full within their bounds. No skipped method or truncated output was admitted.

Source binding: e8f8b012bb8a570b853f6390d5ad0400eace79233b761e4c284075cd4cb8cea7
Plan: c02613357b235659aeb685245207f333c210bf603d0db90749101a81597dc5fc
Caller: eadfd1530021f7129282c43a6f7086f59b69df84bbdfb2364de64d69bdb3a184
Actual launch: 2e7498839fd081240c67a246dcb18fcaac23344eaf8d15af9099e300bc959d08
Attempt RESULT: 0c756d9bab57134eb21143f4f72a227b3d2237968bf23a27eca2f9fe6e635419
UNION-RESULT: 0df23ecacca62bdcb5a2c0add45d92652f6465fb4402c726c6ac07d3c3a648e9
Independent terminal readback: 06e1c8ec739f3f7f53c5f7deb344e7bcd8438b960055960a25ee7b337e4be502

These results qualify the selected Reverie mechanisms only. The earlier whole-suite fatal_worker_ro_delayed_waiter failure, successful-exec sibling RPC cancellation limitation, broader panic cleanup limits and virtual SIGCHLD defect remain separate. Actual Hermit scheduler/initialized-VM/pthread integration, strict INFO comparisons, repeat determinism and canonical cross-backend parity are not inferred. Earlier first failures and all unexecuted preparations remain intact.

| Original stage | Method | CPU seconds | Observed wall seconds | Attempt |
|---|---|---:|---:|---|
| vm-01 | vm::tests::nested_host_fork_failure_uses_descendant_process_and_worker_identity | 0.279062 | 1.042331655 | v4 first attempt |
| vm-02 | vm::tests::page_fault_action_restores_complete_stopped_context | 0.290669 | 1.385377989 | v4 first attempt |
| vm-03 | vm::tests::real_fork_and_thread_cancel_keep_status_while_terminal_hook_is_returning | 0.289110 | 1.529658762 | v4 first attempt |
| vm-04 | vm::tests::real_fork_and_thread_wrappers_restore_both_capture_modes | 0.317554 | 2.048241500 | v4 first attempt |
| static-elf-01 | exec_worker_error_diagnostic::exec_worker_error_still_consumes_root_and_process_hooks | 0.701062 | 2.945406442 | v4 first attempt |
| static-elf-02 | leader_exit::exec_rearms_worker_survival_direct | 0.420298 | 1.131003920 | v4 first attempt |
| static-elf-03 | leader_exit::exec_rearms_worker_survival_tool | 0.415603 | 1.245202753 | v4 first attempt |
| static-elf-04 | leader_exit::failed_worker_cancels_live_sibling_after_leader_exit | 0.411864 | 1.177884569 | v4 first attempt |
| static-elf-05 | leader_exit::failed_worker_preserves_execution_and_cleanup_errors | 0.382752 | 0.982448481 | v5 retry after accounting refusal |
| static-elf-06 | leader_exit::later_group_exit_preserves_worker_direct | 0.322817 | 2.502865124 | v5 first attempt |
| static-elf-07 | leader_exit::later_group_exit_preserves_worker_tool | 0.321437 | 0.845704649 | v5 first attempt |
| static-elf-08 | leader_exit::leader_exit_preserves_pending_worker_start_callback | 0.323484 | 0.824264512 | v5 first attempt |
| static-elf-09 | leader_exit::panicked_worker_interrupts_natural_join_without_losing_panic | 0.389774 | 1.103983984 | v5 first attempt |
| static-elf-10 | leader_exit::parent_wait_observes_final_worker_status_after_direct_child_completion | 0.369892 | 0.958228068 | v5 first attempt |
| static-elf-11 | leader_exit::parent_wait_observes_final_worker_status_tool | 0.387099 | 1.403621178 | v5 first attempt |
| static-elf-12 | leader_exit::process_status_precedes_host_join_order | 0.379113 | 0.906197222 | v5 first attempt |
| static-elf-13 | leader_exit::pthread_exit_preserves_worker_direct | 0.368188 | 0.909595736 | v5 first attempt |
| static-elf-14 | leader_exit::pthread_exit_preserves_worker_tool | 0.385007 | 0.926362314 | v5 first attempt |
| static-elf-15 | leader_exit::raw_exit_drains_nested_worker_direct | 0.409146 | 0.962432482 | v5 first attempt |
| static-elf-16 | leader_exit::raw_exit_drains_nested_worker_tool | 0.411004 | 0.974184231 | v5 first attempt |
| static-elf-17 | leader_exit::raw_exit_preserves_worker_direct | 0.318575 | 0.841527737 | v5 first attempt |
| static-elf-18 | leader_exit::raw_exit_preserves_worker_tool | 0.326551 | 0.877145615 | v5 first attempt |
| static-elf-19 | terminal_fork::backend_exec_failure_joins_live_fork | 0.369766 | 1.020705433 | v5 first attempt |
| static-elf-20 | terminal_fork::backend_exec_failure_preserves_child_and_owner_errors | 0.370343 | 1.405312934 | v5 first attempt |
| static-elf-21 | terminal_fork::injected_exec_failure_joins_live_fork | 0.369739 | 1.011869312 | v5 first attempt |
| static-elf-22 | terminal_fork::injected_exec_failure_preserves_child_and_owner_errors | 0.371709 | 0.992371212 | v5 first attempt |
