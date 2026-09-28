# Consuming Tool hook construction correction

This is an implementation packet for the concrete P1 identified in the preceding consuming-hook draft. It is not an independent review or test verdict. The Tool ABI permits synchronous work before a method returns its boxed future. Calling that method as an argument to `catch_owned_future` left construction outside the catch, so a thread constructor panic skipped the process hook and a process constructor panic could discard the retained thread result.

The private helper `catch_owned_future_from` in the frozen `after/reverie-kvm/src/failure/owned_future.rs:33` catches the builder first. A construction panic returns no output and the original panic Box. A returned future goes through the unchanged `catch_owned_future`, retaining its separate polling and destruction handling. The helper adds only `FnOnce() -> F` and `F: Future`; it adds no public bounds or publication policy.

Both actual hook calls in the frozen `after/reverie-kvm/src/runtime.rs:2317` and `:2353` now occur inside builder closures. Existing `ToolPanics::finish`, phase names, error publication, process-hook ordering, and returned error aggregation are unchanged. The older wrapper and production callers were not edited in this increment.

Two appended controls call the actual production consuming helper through a separate manual Tool ABI implementation:

- `runtime::consuming_panic_tests::thread_constructor_panic_is_published_before_process_hook_runs` throws the exact payload before the thread future is returned. It requires zero thread-future polls/destructions, one process-future poll/destruction, and thread failure publication before process construction and polling.
- `runtime::consuming_panic_tests::process_constructor_panic_retains_prior_thread_error_and_drop_payload` first returns a typed thread error and then throws a separate thread-future destructor payload. The process method subsequently throws before returning its future. It requires the original typed error allocation, both original panic Boxes in order, no process-future polls/destructions, exact phase order, and preservation of the first published error.

Each new declaration runs its scenario with and without `FailureContext`. The payload fixture is Send but not Sync. The returned completion future must remain Send. Panic payload destruction is checked to occur once only after the test explicitly drops the retained payload vector. These are source assertions, not executed results.

The full preceding consuming test file is an exact byte prefix of the new file, preserving all three existing declarations and assertions. Removing just the new helper restores the previous `owned_future.rs` bytes, including its four existing controls. `PRESERVATION.json` records these static checks. No assertion, comparator, tolerance, exception, skip, or outcome label was weakened. No previous failure is relabelled as a pass.

The authorized write scope was the free consuming helper in `runtime.rs`, additive test source, the new private constructor helper, and this artifact directory. `AUTHOR.patch` contains only those changes. `CONCURRENT.patch` separates any observed runtime changes outside that region; it is empty at this freeze. Complete before/after snapshots and source digests are retained. `failure.rs` and `failure/tool_panics.rs` are read-only context snapshots, not edits.

No compiler, formatter, test runner, guest, external model, network, or SCM operation ran for this packet. There are five consuming-hook test declarations after the change, two newly added; zero were executed here. Root owns composition, validation, and production call-site changes. The child-wait-hook author was informed to put actual hook construction inside the same helper.

This does not recover an abort caused by a second panic during unwinding, guarantee cleanup after arbitrary cancellation of the outer future, or settle the separate final owner/destructor integration. It makes no Hermit, Detcore, parity, scheduler, or landing approval claim.
