# Worker panic controls: author report

Write scope: only the new reverie-kvm/src/vm/worker_panic_tests.rs and this ignored packet. No existing product file was edited by this task. Root owns the vm.rs module declaration and production implementations.

Three additive controls call the actual KvmBackend::finish_panicked_guest_worker, runtime per-consumer panic drain, finish_unstarted_tool, and registered worker join methods. Each constructs a parent Future whose actual Drop resumes a uniquely allocated Send-but-not-Sync payload. The test catches this destructor unwind, invokes production worker completion, verifies that completion resumes exactly the same allocation, then lets the actual thread panic and joins it through GuestThreadGroup. Callback construction is local to the control: it is not drive_handler or a guest Tool callback dispatch.

The first queued consumer owns a real child backend, ElfExecutor and ThreadState. Its real Tool::on_exit_thread polls a oneshot Pending before announcing readiness. Assertions check that parent failure is already published, child retirement precedes its hook, parent lifecycle/transport remain live while the hook is Pending, later consumers remain unpolled, and parent retirement follows every consumer. This establishes the specified ordering; it does not prove any production scheduler dependency on that ordering.

A later consumer returns an exact typed GuestClock error; in the panic cases its destruction also panics. Another panics during polling and destruction separately. The final consumer returns a distinct typed HostIo(EIO) error. Every consumer must be polled and dropped exactly once; both typed Arc causes and each phase-labelled cleanup panic survive aggregation. The original and secondary panic payload allocation identities, ordering, and zero Drop counts survive physical join. Ordinary payload destruction is checked once at final group destruction while cloned typed diagnostics still survive.

join_workers controls require repeated teardown diagnostics to retain the same reported Arc, first published parent cause, and both typed child causes. The discard_unstarted_worker control registers a real cancelled ChildStartGate and JoinHandle, requires its returned SharedFailure to point to the exact reported Arc, requires second discard to find no handle, and checks the same completed-record ownership. Discard returns its error to its caller; this control does not claim it adds that error to worker_errors or that teardown_result reports discarded handles.

All explicit channel waits use a five-second bound. An unwind rescue releases the consuming-hook oneshot and start gate, waits at most five seconds for worker departure, and then attempts the existing join. A rescue timeout is reported rather than silently treated as successful cleanup. Production joins themselves have no timeout API. No guest KVM_RUN occurs; two KvmBackend::new calls perform setup ioctls and own real resources. Missing /dev/kvm is a hard failure, never a skip.

Validation: source read/hash parsing only. No compiler, tests, formatter, network, model, SCM command, or guest payload execution was run by this task. No existing assertion, tolerance, comparator, label, or case was weakened or removed. This is author implementation, not independent approval.

Explicit limits: no actual failed host-spawn injection; no root/fork panic routing; no ordinary-drain panic completion; no drive_handler routing; no fix or proof for a RuntimeError lost when callback destruction panics before returning it; no arbitrary recursively panicking payload destructor at eventual group destruction; no full scheduler, Linux/POSIX, or record/replay parity claim.

Exact selectors:

- vm::worker_panic_tests::caught_callback_drop_panic_drains_pending_tool_hook_before_worker_retirement
- vm::worker_panic_tests::joined_worker_retains_cleanup_poll_and_drop_panics_and_later_typed_errors
- vm::worker_panic_tests::discard_cancelled_worker_preserves_reported_cause_and_all_panic_payloads
