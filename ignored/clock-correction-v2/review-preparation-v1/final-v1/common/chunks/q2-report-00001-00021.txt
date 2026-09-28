# Exact clock and injection-stop qualification

The one authorized successor run attempted 45 of 45 phases, accepted 44 and failed/refused 1. It executed 38 of 38 exact declarations: 37 accepted, 1 failed/refused, 0 unrun. No retry occurred. All earlier compiler/clock/debugger failures remain unchanged.

The source changes after the first failed compile are only the public PathPtr import and removal of the redundant Write import. Production, assertions, guest bytes, selection and bounds are unchanged. Seven new declarations encompass eight modes; actual emitted completion markers are: PostExec, Replace, SameThenPrivate, TailSame, TailOther, PostExecTail, Emulate, EmulateErrno. Markers receive qualification credit only with their accepted exact-test result.

The final legacy-vsyscall declaration returned raw zero and printed its exact test and suite `ok` records, but the original strict JSON-lines reader refused the mixed stdout stream: the existing testing helper also emitted ANSI DEBUG/TRACE/INFO records there. Its accepted=false / JSONDecodeError receipt remains unchanged. The raw trace includes actual time interception and no skip marker. The separate raw-output projection is an audit aid, not a replacement qualification receipt. No case was rerun and no parser was relaxed.

Actual clock observations, in retained stream order:

- test-02 tracer::clock_origin_tests::initial_command_clock_counts_first_branch_and_guest_loop: start=0, post_exec=0, boundary=1, boundary=65, start=0, intercepted_exec=0, post_exec=0, boundary=1, boundary=65.
- test-03 tracer::clock_origin_tests::initial_exec_retires_thread_start_and_intercepted_exec_timers: start=0, post_exec=0, boundary=1, boundary=65, start=0, post_exec=0, boundary=1, boundary=65, start=0, post_exec=0, boundary=1, boundary=65, start=0, intercepted_exec=0, post_exec=0, boundary=1, boundary=65, start=0, intercepted_exec=0, post_exec=0, boundary=1, boundary=65, start=0, intercepted_exec=0, post_exec=0, boundary=1, boundary=65.
- test-04 tracer::clock_origin_tests::initial_post_exec_injection_and_new_timer_remain_live: start=0, post_exec=0, timer=1, boundary=65, boundary=129, start=0, intercepted_exec=0, post_exec=0, timer=1, boundary=65, boundary=129.
- test-05 tracer::clock_origin_tests::initial_command_clock_excludes_added_post_stop_launcher_branches: start=0, post_exec=0, boundary=1, boundary=65, start=0, intercepted_exec=0, post_exec=0, boundary=1, boundary=65.
- test-06 tracer::clock_origin_tests::failed_initial_exec_keeps_clock_and_notifications_inactive: start=0, intercepted_exec=0, failed_exec=0.

Full raw stdout/stderr, exact per-phase argv, retained ELF/library/loader bindings, fresh inventory and terminal receipts are indexed. The original exact guest boundaries remain 1/65 or 65/129; no fixed offset, tolerance or count adjustment was applied. The post-exec 0 / timer 1 change remains the explicitly documented causal-oracle correction, not a relabelled prior pass.

Measured CPU sums to 344.439192000 seconds; observer wall sums to 289.910501011 seconds; exact-test payload wall sums to 0.286275571 seconds. CPU accounting is complete for 45 of 45 attempted phases. Private lease completion is recorded separately; an exclusive read-only probe succeeds with token bytes unchanged.

This is finite component evidence, not independent source approval, a Hermit run, parity, or whole timer/scheduler clearance. Write checks its three meaningful arguments; the mapping control is not a six-register oracle. Existing specialized rejection/unit neighbors do not establish every positive activated frame, converted mapping-tail or interrupted private-syscall path. The unchanged precise-timer parent test has an early capability return and captures its child output internally: its parent result alone is not separately authenticated evidence of both child modes. No claim of those extra executions is made.
