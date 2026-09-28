Source-ready author handoff; no execution or approval

Implemented the accepted four-case real A/B/C plan in a new included test module. Repository is /home/newton/work/dev-hermit/worktrees/slots/kvm-reverie-landing-20260918, dispatched branch codex/kvm-parity-land-20260918, underlying base 91110d249ffd8957267d71fab8c83d9636105efe. The four edited existing files were individually authenticated against frozen source manifest f74effb8842eaa3d9a8178c96fdde293cf210bfc36fe824a1f38f8c3f096dd55 before editing. BASELINE.json and before/ preserve those exact bytes. SOURCE.json, after/ and AUTHOR.patch bind the unformatted author release. Root owns the vm.rs include, formatting, compilation and qualification.

Files released

- NEW reverie-kvm/src/vm/entry_multi_owner_tests.rs.
- reverie-kvm/src/entry/driver.rs: cfg(test) invocation-scoped observer registry keyed by actual host thread; instrument the real foreign select's returned Pending without extra polling or altered result. Registration guards are retained through public completion and removed by fixture RAII. Observer callbacks execute outside the registry lock.
- reverie-kvm/src/memory.rs: the existing cfg(test) before-vector-copy boundary additionally calls the scoped observer with this actual view's gate and EntryOrigin. The older instance observer is preserved. This scoped form is needed for the independent fork's new Mapping, which does not inherit the root's installed instance observer. No snapshot or origin rebinding is changed.
- reverie-kvm/src/vm/worker_join.rs: cfg(test) observations immediately before and after the direct terminal worker JoinHandle::join, after registry/ledger locks are released. Helper/discard paths are unchanged and are not claimed as new coverage.
- reverie-kvm/src/executor.rs: corresponding cfg(test) observations around the actual ChildProcessHandle join, preserving joined result and panic payload. The original join expression is assigned to a local solely so the observation occurs after physical return and before matching the identical result branches.

Root should include the new file inside vm::tests, like entry_public_tests.rs and entry_spawn_tests.rs. No module include or unauthorized path was edited.

Exact declarations

vm::tests::entry_multi_owner_tests::public_elf_owner_a_publishes_after_real_peer_observation
vm::tests::entry_multi_owner_tests::public_elf_independent_c_publication_releases_peer_before_owner_a
vm::tests::entry_multi_owner_tests::public_elf_peer_b_keeps_own_error_before_owner_a_publication
vm::tests::entry_multi_owner_tests::public_elf_multi_owner_healthy_callbacks_complete_once

What the authored controls require

The installed ELF performs a real fork before two actual CLONE_THREAD clone syscalls, using separate inherited-stack offsets. Child roles come from real Tool process/thread initialization and parent links, not assumed TID numbers. Each actual task enters one subscribed getpid callback. No helper ProcessAction, manufactured FailureContext, fabricated SignalEffects, saved output or manual publication substitutes for those routes. A and B must share a gate, have different actual driver-bound callback identities, and share C's RunFailure while C has a distinct gate/process journal.

The controller first lets C generate and acknowledge its own effect, then B, then A. Every effect comes from successful injected rt_sigprocmask, exact self tgkill and rt_sigtimedwait; observation records the real SignalDequeue and SignalTaskIdentity. EffectsReady is recorded after the final injection returns. B holds one actual synchronous poll until A's adapter poisons/captures its own bound origin. A then holds its actual poll until separately released. The callback holds no guest stack checkout, RPC, journal, memory admission, registry or backend lock while blocked. Every hold uses the existing five-second bound and independent rescue release. Timeout remains a failure.

A-first and C-first require B's actual memory adapter to return EIO without changing the target bytes, followed by real callback destruction, callback-generation acknowledgement and a Pending poll of the real foreign select. Before publication, A is still alive, the actual RunFailure notification is pending, consuming cleanup has not started, and no physical terminal join has begun.

C-first additionally holds C's actual report hook synchronously. The controller checks that published_primary is absent and the real subscription remains pending while that hook is held; this explicitly does not count GlobalTool::report_backend_failure as a publication receipt. It releases the hook independently, awaits the real receipt, then waits for B's state consumption AND B's actual outer thread-exit error-report boundary before releasing A. That latter boundary follows the consuming future's completion/destruction, avoiding a claim based solely on the fixture's local consumed marker. B-first returns its own Ready typed Io error after the real EIO, preserving it through the actual post-poll private watch without a foreign Pending wait.

A's callback guard destruction and actual OperationOrigin callback_dropped receipt are both required before any failure report attributed to A. All failure-driven physical joins require a real run notification receipt. The first cause is A, C or B as specified. A's later consuming-hook Io allocation and the exact intended independent B/C Io allocations must survive once in the final typed graph. Error traversal visits actual error allocations once to avoid double-counting shared Arc aliases; it does not collapse distinct owned effect ledgers by dequeue ID. Exactly one nonempty retained ledger for each failed effect owner is required, with exact dequeues, raw SIGUSR1 result, process-wide acknowledgement watermark, empty publication vector and absent parked context. Unexpected diagnostics are rejected.

Healthy C in A-first/B-first waits for actual publication, then finishes local guest code with status 17, a still-pending process-local notification and an unpoisoned Mapping. Healthy-all uses the same guest creation, real effects, memory adapter and callback barriers, requires exact ABCD reads, four single callback/state-consumption paths, no publication or foreign failure wait, and public status/output (0, empty, empty). The controller lets A/B/C finish before releasing the root's successful callback. Final root completion must follow one actual physical join per A/B/C target; thread state weak witnesses must be dead, process consuming hooks counted exactly once, group handle ownership empty, and the real public GlobalState unwrap must have succeeded.

Rescue and observation limits

The run future is driven on its own host thread. The controller catches its own assertion failures, releases every finite hold, then awaits bounded public completion before joining that host. The outer qualification service remains responsible for a product path that fails to return despite all releases. Rescue cannot turn a failed assertion or timeout into a pass. Observers are passive except for the explicitly armed one-shot A copy fault; they do not publish or replace the adapter. Unarmed copy observations return immediately.

These are source-only controls, not executed evidence. No compiler, formatter, test binary, guest, model, network or SCM command was executed by this author. Rust/API/guest setup and exact aggregate expectations remain subject to root compilation and actual qualification. No product defect is claimed from the source-only pass. No tests are skipped for unavailable KVM or unsupported fixture operations. Existing tests/checks were not edited. Normal-library behavior is unchanged by the cfg(test) hooks; the only unconditional expression refactors preserve the original select/join values and branches.

Goalpost check

No existing assertion, tolerance, comparator, selection, exemption, label or gate was changed. All four accepted cases are present. No fabricated no-effect wrapper satisfies the real-effect requirement; no report-hook event satisfies actual publication; no main-closure return substitutes for physical join. Callback guard Drop and actual callback-generation acknowledgement are recorded separately. A shared C receipt is not called A's publication. Existing 198 declarations must remain selected along with these four; inventory is not a claim of execution.

This author handoff supplies neither self-approval nor a full scheduler/Linux/parity/landing verdict. It reuses the prior bounded source/design grounding with its recorded limitations; it does not claim a new complete primary-source scheduler review.
