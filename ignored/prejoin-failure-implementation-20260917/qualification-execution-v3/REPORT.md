# Source v19 original qualification first failure

Qualification-execution-v3 passed all four separately selected real VM methods and the first 18 original static_elf methods, then stopped at `terminal_fork::backend_exec_failure_joins_live_fork`. All ten exec-worker error modes and all 17 leader_exit methods passed unchanged, including the previously failing healthy sibling status and the original panic diagnostic. This establishes those specific Reverie Tool/direct behaviors; it is not Hermit strict INFO, repeat-run determinism or canonical cross-backend parity evidence.

The first failure occurred in mode 7, early=false. The already-started fork process 2 reached its consuming on_exit_thread hook with Exited(255); the unchanged assertion at terminal_fork.rs:768 requires Exited(0). The assertion panicked before recording its thread-exit event, so the outer original exactly-one-hook assertion at line 927 also failed. The retained final result still has worker 3's typed EIO as primary, with the child panic as separate cleanup. This is a measured started-fork status boundary, distinct from the now-passing CLONE_THREAD sibling case. No assertion was changed, and no retry ran.

The failing service safehermit-20260917T161601Z-4189212.service, actual main PID 4189577, exited 101 after 0.374309 CPU / 0.962610375 wall seconds. Full stdout (528 bytes) and stderr (2197 bytes) are preserved. Real /dev/kvm API 12 admission and the exact actual ELF/argv were authenticated inside that service. Accounting is complete, cgroup empty, and independent terminal readback inactive/dead with MainPID 0 and empty ControlGroup. No observer error or bound was hit; raw output is bounded and untruncated.

The last three original static methods remain unexecuted; their dispatch and output absence was checked. All 23 executed services have complete inactive/empty accounting, and full live source/input/ELF comparisons passed after terminal failure. These comparison assertions are not separately retained complete after-snapshots. Source v19, all prior first failures, the complete original population, and actual separately copied ELFs remain unchanged.

| Stage | Exact method | Result | CPU seconds | Wall seconds |
| --- | --- | --- | ---: | ---: |
| vm-01 | vm::tests::nested_host_fork_failure_uses_descendant_process_and_worker_identity | passed | 0.269700 | 0.911943843 |
| vm-02 | vm::tests::page_fault_action_restores_complete_stopped_context | passed | 0.278947 | 1.130289767 |
| vm-03 | vm::tests::real_fork_and_thread_cancel_keep_status_while_terminal_hook_is_returning | passed | 0.276065 | 1.052156580 |
| vm-04 | vm::tests::real_fork_and_thread_wrappers_restore_both_capture_modes | passed | 0.296643 | 1.277614390 |
| static-elf-01 | exec_worker_error_diagnostic::exec_worker_error_still_consumes_root_and_process_hooks | passed | 0.560492 | 1.621071742 |
| static-elf-02 | leader_exit::exec_rearms_worker_survival_direct | passed | 0.386490 | 0.929658755 |
| static-elf-03 | leader_exit::exec_rearms_worker_survival_tool | passed | 0.391265 | 0.927527225 |
| static-elf-04 | leader_exit::failed_worker_cancels_live_sibling_after_leader_exit | passed | 0.378083 | 0.922905798 |
| static-elf-05 | leader_exit::failed_worker_preserves_execution_and_cleanup_errors | passed | 0.404440 | 1.269255212 |
| static-elf-06 | leader_exit::later_group_exit_preserves_worker_direct | passed | 0.319455 | 0.841950895 |
| static-elf-07 | leader_exit::later_group_exit_preserves_worker_tool | passed | 0.323376 | 0.869708963 |
| static-elf-08 | leader_exit::leader_exit_preserves_pending_worker_start_callback | passed | 0.319248 | 0.832367308 |
| static-elf-09 | leader_exit::panicked_worker_interrupts_natural_join_without_losing_panic | passed | 0.382644 | 0.961645371 |
| static-elf-10 | leader_exit::parent_wait_observes_final_worker_status_after_direct_child_completion | passed | 0.371990 | 0.944962925 |
| static-elf-11 | leader_exit::parent_wait_observes_final_worker_status_tool | passed | 0.387981 | 1.001854872 |
| static-elf-12 | leader_exit::process_status_precedes_host_join_order | passed | 0.384962 | 1.015504747 |
| static-elf-13 | leader_exit::pthread_exit_preserves_worker_direct | passed | 0.375795 | 0.910787581 |
| static-elf-14 | leader_exit::pthread_exit_preserves_worker_tool | passed | 0.378896 | 0.892117413 |
| static-elf-15 | leader_exit::raw_exit_drains_nested_worker_direct | passed | 0.422698 | 0.968529229 |
| static-elf-16 | leader_exit::raw_exit_drains_nested_worker_tool | passed | 0.416838 | 0.966461696 |
| static-elf-17 | leader_exit::raw_exit_preserves_worker_direct | passed | 0.328705 | 0.981937308 |
| static-elf-18 | leader_exit::raw_exit_preserves_worker_tool | passed | 0.335558 | 0.851721773 |
| static-elf-19 | terminal_fork::backend_exec_failure_joins_live_fork | failed | 0.374309 | 0.962610375 |
| static-elf-20 | terminal_fork::backend_exec_failure_preserves_child_and_owner_errors | unexecuted | — | — |
| static-elf-21 | terminal_fork::injected_exec_failure_joins_live_fork | unexecuted | — | — |
| static-elf-22 | terminal_fork::injected_exec_failure_preserves_child_and_owner_errors | unexecuted | — | — |
