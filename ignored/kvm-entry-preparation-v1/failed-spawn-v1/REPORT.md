Retain failed-spawn child consumers until the parent callback returns

The two Tool-owned failed-spawn arms now transfer their constructed child into an unpolled Send + 'static consuming future on the parent ElfExecutor, then immediately return the original Error::HostIo. The closure retains the child backend/executor, Tool, thread state, global state, config and exact guest identity. It does not run a guest or mark a start gate successful. The successful-spawn paths and their handles/start gates are unchanged.

ElfExecutor owns Mutex<Vec<UnstartedToolCleanup>>. Initial, fork and thread constructors each initialize an independent empty queue; child construction never copies the parent's pending consumers. retain_unstarted_tool_cleanup only pushes through exclusive get_mut access. take_unstarted_tool_cleanup uses mem::take to transfer the whole queue once, without polling or a held registry guard. The mutex permits the executor's existing Send + Sync requirement while each captured future requires only Send.

The outer runtime owner must finish the parent callback and publish its original failure before taking and polling every consumer. Each child then receives Some(Error::RunAborted), the same failure-cancellation marker used by CancelAfterFailure. Do not pass a newly wrapped copy of the parent's cause: RunFailure retains its own outer Arc, and a separately wrapped SharedFailure could cause duplicate publication. A plain RunAborted result is a derived marker, not a second failure; every real cleanup error or aggregate must survive outer aggregation. This author does not alter the runtime owner or claim its integration is complete.

Calling the existing finish_unstarted_tool after parent publication retains failed-child lifecycle and consuming-hook behavior: the child still has its original identities and owned state, gets no fabricated handle_thread_start, follows terminal retirement/file/TID cleanup and consumes Tool state. The future is owned until this happens. Dropping a parent without draining is not an alternative consuming path; runtime integration must drain after callback release. The existing impossible-success check remains inside each future, so an unexpected successful return cannot become guest success.

One new host control is prepared: executor::tests::unstarted_tool_cleanup_retains_children_unpolled_and_transfers_once. It verifies an empty initial queue, empty fork/thread queues even when the parent retains work, zero polls during retain/take, live child lifecycle records until consuming the retained future, exactly one transfer and one poll per child, and exact typed primary/cleanup Arc preservation. A deliberately Send-but-not-Sync Cell crosses an await inside the stored future, and an explicit compile-time assertion preserves ElfExecutor: Send + Sync. This is a queue/ownership control, not a forced host-spawn or real Tool-hook execution test.

PRESERVATION.json proves all old test-suffix bytes are unchanged (executor direct declarations 276 to 277; vm 74 unchanged). The complete 254-line patch contains only executor imports/storage/constructors/accessors/new test and the two authorized VM failure arms. No existing assertion, tolerance, comparator, skipped case or failure classification is weakened. No warning suppression was added.

Pinned single-file formatting succeeded. No compilation, test, guest, KVM_RUN, model invocation, source commit or network operation was performed. Root must bind the composed runtime drain and finite validation separately. Source ownership is released with this frozen packet.

GOALPOST-MOVING REVIEW RULE

Adversarial reviewers must look explicitly for goalpost moving. We are extremely skeptical of any goalpost moving. YOU DO NOT CLEAR THE BAR BY SIMPLY LOWERING THE BAR.

Treat each of these as an explicit review target:
- weakening an assertion so a test passes
- widening a tolerance · adding an exemption · skipping a case · relaxing a comparator
- renaming or relabelling so a failure reads as a pass
- deleting a check rather than satisfying it
