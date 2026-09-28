Source release: accepted D/E controls from S/ignored/kvm-entry-gate-design-successor-v1/CONTROLS.md, implemented against Reverie 4ea79f6da7100709fea0e0a5591b0945d438e34f on codex/kvm-entry-ownership-20260919. All seven new test declarations are unexecuted. Root owns independent review, formatting, build and qualification.

Written source

- New reverie-kvm/src/memory/entry_snapshot_tests.rs: four actual sparse-operation/copy-admission controls.
- New reverie-kvm/src/vm/entry_construction_tests.rs: three actual thread-constructor/retirement controls, including tracked and untracked finite entry within the first declaration.
- memory.rs: cfg(test) module include, a per-handle backing-contention observer, and a Weak-based Mapping owner-count closure.
- entry.rs: cfg(test) read-only gate/member snapshots and distinct current copy waiter identities around the unchanged production Condvar wait.
- vm.rs: one include inside its existing cfg(test) tests module. Root concurrently added documentation elsewhere in this file; the actual after snapshot retains it, while AUTHOR-PATCH.patch contains only this author's include.

No clock.rs, executor.rs, existing fixture, production behavior, manifest, runner or test threshold was changed. Existing test bodies are untouched. BEFORE.json, SOURCE.json, before/, after/ and AUTHOR-PATCH.patch identify the exact source. The source snapshots include all bytes in each observed shared file; they do not claim ownership of root's documentation.

D: backing-lock dependency

The admitted-read controls call the actual snapshot_with_sparse_copy boundary and hold the injected host operation while production owns its allocation guard and both backing locks. An independent raw or UserMemory reader takes its actual CopyAccess. The test-only observer reports only an actual try_lock WouldBlock on the source host_access mutex, then execution proceeds to the original unchanged blocking lock. Successful or poisoned probes do not manufacture a contention receipt. The probe drops any temporary lock guard before invoking its observer and does not run a callback under a production mutex.

The controller observes one admitted copy, obtains the actual Closing future, polls it Pending, and retains that same future. A separate observer thread checks that the copy remains admitted and admission is closing, then releases the held host operation without waiting for either the read or close. The reader must return all exact bytes; that admission's retirement must make the retained close Ready. A sparse fallback may itself need fresh admission, so the closed token is dropped before collecting the snapshot or joining workers. The snapshot must have a distinct Mapping gate and preserve the complete source bytes.

The opposing controls close with zero admitted copies while the actual host operation remains held. A subsequent UserMemory reader must appear in the actual copy condvar wait, with zero admitted copies. The host operation is released while the token remains Closed. Ordinary sparse copying is attempted through copy_sparse_file; failure is permitted only through production's original full fallback, not treated as a skipped control. The forced partial fallback writes 257 bytes of 0xa5 to the actual destination fd and returns an error. The fixture then requires two distinct waiting host threads: the external reader and the first real fallback source read. While closed it verifies the dirty prefix and following zero byte directly through an owned duplicate of the actual destination fd. After reopen, both the reader and the complete 1 MiB + 4096-byte snapshot must exactly equal the nonuniform source pattern. This catches a fallback that fails to overwrite the prefix or suffix.

The destination-fd observation is deliberately a host backing operation. No claim is made that the Mapping close excludes kernel backing-fd writers, creates a global snapshot, or gives a latency bound on allocation/backing operations. The separate allocation-only mmap-file-read neighbor remains outside this file and is being covered by the other assigned author.

E: construction and exact retirement

construct_while_closed creates a real KVM parent with a minimal static ELF, captures actual registers/xsave, and closes its shared Mapping. A real host thread calls KvmBackend::from_thread_state, the constructor used by both CLONE_THREAD paths. The controller must see that exact host ThreadId in copy_blocking's actual condvar wait, a newly inserted stopped participant with run generation zero, the unchanged parent participant, zero copy admissions, and four strong Mapping owners (backend plus CountedVcpu for each VM). Observing the state under the real registry mutex after wait releases it establishes registration before the constructor's first admitted memory write. The controller reopens before demanding full construction or joining its thread.

The positive declaration runs this control both without and with the real guest clock. The constructed long-mode child executes `inc byte [rsp-8]; mov eax,SYS_getpid; syscall`; the actual private hypercall must name the child's real frame and getpid request. The test writes response 37 and requires the next actual KVM_RUN to reach the real trampoline return-park Hlt at its exact next RIP, with RAX=37. The original guest byte increments once. Actual run, mask and clock counters are exact (two runs, two masks, and either two or zero clock intervals). The participant is stopped at run generation two before a new close. Dropping the child while Closed removes only that exact participant and releases both of its Mapping owners. A fresh constructor receives the next identity, remains at run zero, and retires without reusing the old generation. Dropping the parent leaves zero participants and zero Mapping owners.

The other two declarations require the same real closed construction and then cover unentered drop and real OS spawn refusal. The refusal uses the existing one-shot spawn_refusal seam with spawn_owned and its impossible stack allocation; the real Builder.spawn must return its actual error. The test compares the retained observed error exactly, keeps the recovered child participant/Mapping owners live until drop, and proves the child closure never executed. Both paths drop the unentered backend while Closed, leave only the parent, require no guest byte effect, and ultimately release all Mapping owners.

These are direct tests of the actual constructor and ownership helpers. They do not claim guest clone syscall dispatch, Tool callback or consuming-hook coverage, syscall side-effect execution for getpid, full pending ELF RPC coverage, or exclusion of constructor configuration ioctls. Constructor configuration is allowed before its first gated copy; the gate observation asserts no entered participant, not zero fd configuration calls. Existing private-hypercall, fork, unstarted-child, spawn-refusal, supported-exit and permission controls remain selected independently.

Stable cfg(test) helper contracts

GuestMemory::test_mapping_owners returns a Send + Sync + 'static closure with precise empty lifetime capture. Its sole capture is Weak<Mapping>; calling it reports the real strong count and never retains a Mapping owner. This allows an exact zero-owner observation after backend destruction.

EntryGate::test_state takes the real state mutex and returns TestGateState { open, closed, copies, copy_waits, copy_waiters, members }. Each member records actual id, run and whether activity is Stopped. copy_waits remains the original raw poll count. copy_waiters contains the actual current host ThreadIds waiting in the copy condvar: insert immediately before wait; remove after reacquiring the mutex. A spurious wake may increase copy_waits, but cannot impersonate a second waiter. The observer does not create or alter admission, participants, notification, signals, error results or production lock ordering. All new fields/methods/statements compile only under cfg(test).

Bounds and cleanup

Every added controller receipt uses the existing five-second handshake bound, or an actual state predicate with a five-second deadline and yield_now. No sleep or absent-event interval is an ordering proof. Snapshot workers retain release senders and owned JoinHandles; rescue releases host-operation waits before joining. Constructor rescue drops the Closed token before joining. The normal paths collect exact results and join all actual workers. Root must retain the external finite service/process cap, because a broken production mutex or unresponsive kernel operation cannot be repaired by a Rust JoinHandle deadline. A timeout, panic, failed receipt, unexpected exit or failed cleanup is a failed control. No reduced timeout, widened tolerance, skip or retry-success rule is introduced.

Exact new selections

- memory::entry_snapshot_tests::sparse_host_operation_keeps_close_pending_for_admitted_raw_read
- memory::entry_snapshot_tests::sparse_host_operation_keeps_close_pending_for_admitted_user_read
- memory::entry_snapshot_tests::sparse_host_operation_without_admitted_copy_allows_close
- memory::entry_snapshot_tests::partial_sparse_fallback_waits_for_reopen_and_overwrites_full_destination
- vm::tests::entry_construction_tests::thread_constructor_registers_while_closed_then_enters_and_retires
- vm::tests::entry_construction_tests::never_entered_thread_constructor_retires_while_closed
- vm::tests::entry_construction_tests::refused_real_thread_spawn_preserves_then_retires_unentered_participant

Verification status

Read the accepted D/E text, actual snapshot/copy/entry/CountedVcpu construction paths, current CLONE_THREAD constructor callers, bootstrap trampoline and existing observation/refusal helpers. Performed source inspection and byte/hash bookkeeping only. No compiler, formatter, tests, guest, SCM mutation or network action was run. There are no execution durations or passing results to report. Source is released for root's separate freeze, review and qualification.
