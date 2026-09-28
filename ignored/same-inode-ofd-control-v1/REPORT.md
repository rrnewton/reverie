# Same-inode replacement control

This is an additive, unexecuted unit-control proposal against Reverie `000c15a1161ea2d58749431b5ddaaa97f7aa37d5`, tree `a12d56466eed51cdd7d12087b5fafa9d66d8c864`. Product source, HEAD and index remain unchanged. The only proposed product path is `reverie-kvm/src/executor.rs`; the patch adds one test and changes no production or existing test bytes. Parent retains SCM. No compiler, formatter, test or guest was executed.

`candidate.patch` SHA256 `057f7702ff26f2c57de10e98d88a2e987a63736c76a43c613ac05460795d0ee9` is 5,494 bytes. Complete before and after files, exact insertion reconstruction, source context, existing local lockfile binding and before/after SCM observations are retained here. The separate caller proposal does not modify the existing runner.

## Production path and oracle

`FileTableState::install` at base lines 1647–1656 chooses a previous executor File solely by the filesystem-object identity Arc. `allocate_fd_object_inode` at 6643–6697 deliberately retains this identity across separate opens of a linked inode. `thread_child` at 2296 shares the authoritative table but clones executor state; `execute` at 4161–4178 installs the shared table before each syscall. An executor can therefore retain its old description after its sibling replaces a slot with another open of the same inode. This is still a source inference awaiting the proposed actual negative-before measurement, not a measured failure or attribution to a historical census/readv change.

The new exact selector is:

`executor::tests::shared_file_table_reopen_same_inode_replaces_description`

It uses the existing FdinfoFixture, writes distinct `0123456789` bytes before opening, and drives both executors through real `execute` calls. Before replacement, it confirms that an actual dup shares offsets with the original target and restores their offset to 1. It creates the sibling with actual `thread_child(2)`. The sibling closes the target, reopens the same pathname into the same fd, seeks its new OFD to 5 and sets O_APPEND. No parent execute occurs between close and reopen. No internal file table, inode identity or executor snapshot is edited by the test.

The parent then collects the following exact required observations:

| Observation | Required result |
| --- | --- |
| Target position before read | 5 |
| Target full status flags | Original full flags OR O_APPEND |
| Actual scalar read count and byte | 1 and `5` |
| Target position after read | 6 |
| Retained dup position | 1 |
| Retained dup full status flags | Original full flags, still without O_APPEND |
| Target and retained dup fstat identity | Both equal the original `(st_dev, st_ino)` |

The initial access mode and absence of O_APPEND are explicitly checked. Comparing the later **complete** status word to the initial native word preserves legitimate platform bits; it does not mask away status differences. Offset/read observations and direct fstat identity observations precede parent F_GETFL: current `mutates_file_table` classifies every fcntl as mutating, so execute republishes that installed snapshot afterward. No `f.info` or additional open is used to obtain the decisive oracle.

A source-predicted uncorrected observation is target position 1, byte `1`, target/alias position 2 and old flags. This is a prediction only. The future report must identify the actual first failed assertion and raw terminal result. Earlier setup failure, absent/ignored selection, or failure outside this asserted behavior is not the intended negative observation. The final tuple is collected before its assertion, but no claim that later checks ran may be made if an earlier assertion stopped execution.

Two existing selectors are planned unchanged: the shared-dup fdinfo offset control and the forked filesystem-object identity namespace control. The former checks an existing same-OFD alias path; the latter prevents treating a new description as a new filesystem object. Neither is relabelled as a new same-inode replacement test. Total planned selection: three unique declarations, exactly one newly added.

## Caller changes and limits

`caller-change.patch` SHA256 `ad31849d396910cbca5de86ebc71317710eafdf808efd53616280730a2f72318` adapts the existing source-bound preparation helper only: compile/select the library harness; check formatting against the exact reviewed snapshot after file instead of an unrelated live worktree diff; and give the phase its correct scope. The observer, phase launcher, lease, source/ELF checks, terminal accounting and test-result acceptance remain byte-identical. In particular a real negative test remains `accepted=false`, with its original failed event and raw status; the plan proposes no expected-failure success class.

The intended snapshot and qualification paths, exact selectors, reused runner hashes and original per-phase budgets are in `CALLER_PLAN.json` and `TEST_PLAN.md`. The source snapshot, dependency closure, actual executable/list and per-phase plans have **not** been produced. No historical mutable compiler-path record is claimed to be a current executable. Execution is held for review and handoff.

The first artifact-preparation attempt discovered that Cargo.lock is local and ignored, not a blob of this commit. Its failed read is retained in `PREPARATION-NOTE.json`; the script now binds the actual 67,056-byte local lock separately (SHA256 `1c09663e46bf21ad7c07eedd7821cccb72ae21f42485192649ff5473962bc856`). Partial before/after artifacts were reused only after exact byte equality. This was an artifact-preparation issue, not a product/test result.

## Goalpost check and limits

No existing assertion, comparator, test name, tolerance, skip or resource limit is changed. The new test requires correct independent-open behavior and preserves actual same-description sharing and filesystem-object identity. It has no should_panic, ignored marker, tolerated error or alternative accepted result. The full original failure remains a failure in the runner.

There is no OFD identity fix, managed-FIFO implementation, descriptor-width change, new public interface, scheduler approval or runtime qualification in this packet. The earlier FIFO/source reports remain immutable. Compile compatibility and the actual negative observation are pending.
