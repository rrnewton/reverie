# First source v14 actual qualification

This diagnostic attempt stopped at the first failing static_elf method. All four exact VM methods passed with independent real KVM API/device admission inside each observed service, exact executable/service identity, no skip, unchanged source and exact one-method output. The held-hook method exercised real fork/thread ordinary and fatal gate cancellation while the terminal hook was returning. These are Reverie VM controls, not Hermit/Detcore guest parity.

- vm::tests::nested_host_fork_failure_uses_descendant_process_and_worker_identity: passed; 0.256363000 CPU seconds, 0.898385443 wall seconds.
- vm::tests::page_fault_action_restores_complete_stopped_context: passed; 0.271591000 CPU seconds, 0.992545488 wall seconds.
- vm::tests::real_fork_and_thread_cancel_keep_status_while_terminal_hook_is_returning: passed; 0.285976000 CPU seconds, 0.967573931 wall seconds.
- vm::tests::real_fork_and_thread_wrappers_restore_both_capture_modes: passed; 0.304482000 CPU seconds, 1.457140539 wall seconds.

The first static method, exec_worker_error_diagnostic::exec_worker_error_still_consumes_root_and_process_hooks, failed in mode 1 after mode 0 succeeded. The final diagnostic contains EIO twice, while the unchanged assertion at tests/support/exec_worker_error_diagnostic.rs:172 requires exactly once. The returned error contains the first published EIO as primary, plus a RunAborted cleanup wrapper containing that same worker EIO again. Full stdout and stderr are retained; no comparator or expected string was changed.

Failing actual service: safehermit-20260917T151201Z-3981893.service. Exit 101, 0.395690 CPU seconds and 0.960938828 wall seconds. Actual accounting was complete, no bound/cap/observer error, service inactive/dead, MainPID 0 and empty ControlGroup; the independent systemctl readback matched. Every earlier VM service also passed terminal/accounting checks. Full input/source and executable checks passed again after the failed attempt.

The remaining 21 static methods were not launched. The other eight modes after mode 1 in the first method also remain unexecuted. Live-sibling status 0 and the source-predicted panic diagnostic mismatch are therefore still unmeasured. No retry or source change occurred in this attempt. All 26 original method identities remain in the retained plan.
