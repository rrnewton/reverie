# Consume Tool state after exec worker cleanup fails

This repairs the error path found while reviewing terminal cancellation at
`c53dcab920cad98b011a7ec2453d32ecd372b1de`. A root exec first cancels and joins its
siblings. If a worker's consuming exit hook fails, that exec must remain an
error, but the root still owes its own consuming thread and process hooks.
Previously the error escaped through `HandlerOutcome::RuntimeError` before
those hooks ran. The unsubscribed exec path had the same problem.

The retained required-KVM control proves the failure: a worker returns EIO
from its exit hook during root exec. The root returns the EIO but its thread
hook count is zero instead of one; its process hook is also absent. The same
source with no forced hook error completes exec, prints exactly
`exec-replacement` followed by a newline, and runs every consuming hook. Native
execution also produces that exact output and exits zero.

## Error and ownership path

`Error::ExecWorkerTeardown` preserves the original error and is produced only
after exec has cancelled and joined its siblings. Existing cleanup of unstarted
children retains this typed disposition when it adds a cleanup diagnostic.

The two reachable owners handle that error: the ordinary syscall callback
outcome and the unsubscribed process-action result. Each moves its `Arc<T>` and
owned `ThreadState` into `finish_exec_teardown_failure`, then returns `Err`
without restoring or executing a guest continuation. The helper joins peers,
retires the exact identity, releases the transport, clears the registered
child TID, and invokes the root thread hook with the existing fatal Tool error
status 255. It then consumes the process Tool even if the thread hook failed.
The original worker diagnostic is returned unchanged if owner cleanup succeeds;
otherwise it is retained alongside every thread/process cleanup error.

Other runtime errors keep their existing policy. Ordinary-exit notification,
worker error ordering, and the existing first-error policy for unstarted-child
rollback are unchanged. This does not change Linux leader-first exit support
or the terminal API's existing process-boundary limitations.

The lifecycle reachability check is explicit. `runtime.rs` routes
`InitialExec`/`Lifecycle` replacement to `exec_process` directly, while
non-exec process actions in that context return
`fork/clone injection requires a guest syscall boundary` before host spawning.
Root thread-start precedes guest execution; a fork child starts with its own
one-thread process state; a nonleader's exec is rejected by `exec_process`'s
`is_guest_thread` guard. Initial post-exec follows those starts; replacement
post-exec follows the completed sibling cancellation/join. These lifecycle
callbacks therefore cannot encounter this new worker-teardown error. Signal,
fault and first-instruction signal injection retain their original refusal
checks. No speculative handler branches or new continuation semantics were
added for those contexts.

## Permanent controls and measured results

The new static-ELF test retains the successful exec neighbor and the original
one-hook-per-thread assertion. Ten modes cover subscribed and unsubscribed exec,
worker EIO alone, root ENOSPC, process EACCES, and both owner errors. Every mode
requires exactly one consuming hook for each started thread and one process
hook after them. Worker status remains zero; error cleanup gives the root and
process status 255. Failed teardown never reaches replacement post-exec. The
original EIO occurs once, with exact unchanged diagnostic when cleanup succeeds.

The new production-boundary unit test checks typed and ordinary errors both
with and without a failing unstarted-child rollback. It requires preservation
of the original error, disposition, cancelled start gate, and cleanup error.

All old terminal controls and both injection guards remain byte-identical to
c53. The new static module is appended to the old complete file. Five
production-only mutations fail the unchanged new tests:

| Restored defect | Status | Seconds including build |
| --- | ---: | ---: |
| Skip syscall callback owner's cleanup | 101 | 5.271 |
| Skip unsubscribed exec owner's cleanup | 101 | 5.486 |
| Replace original worker error | 101 | 5.113 |
| Skip process hook after root hook error | 101 | 5.466 |
| Lose typed disposition during rollback | 101 | 1.654 |

The complete required-KVM package repeat passed **654 tests, zero failed and
zero ignored**, in 17.195 seconds: 18 core and 636 KVM tests, including all
original terminal/lifecycle/injection controls. Clippy with `-D warnings`
passed in 3.343 seconds, format in 1.270 seconds, and locked all-feature
workspace checking in 2.146 seconds. The copied exact static test binary ran
all ten exec modes with complete events retained in 1.690 seconds. This is
backend evidence; it does not qualify the separate full Hermit composition.

The first full run remains a recorded failure, **not erased by the repeat**.
It returned 101 after 12.253 seconds with 374 library tests passing and one
failing: `executor::tests::received_rights_reservation_and_rewrite_failures_are_transactional`.
The unchanged `assert_stream_peer_closed` helper at `executor.rs:13667` observed
`recv(MSG_DONTWAIT) = -1/EAGAIN` rather than EOF at line 13678. Its two call sites
are line 20839 after rollback of a fixture-owned `UnixStream` endpoint and line
20850 after stripping another fixture-owned endpoint encoded as `SCM_PIDFD`.
They are socket objects, not pipes, inherited stdio or capture objects. The
retained panic does not identify which caller failed. Temporary inheritance by
another host process is only a hypothesis; it was not observed.

The relevant closure paths are `install_received_rights` at line 7470 through
its control-write failure and `rollback_received_rights` at line 7463, and
`sanitize_received_control` at line 7349, which owns and drops unsupported
received descriptors. `executor.rs` is byte-identical to c53. Both focused
old/new binaries passed the exact test in about 0.004 seconds; the complete
copied c53 library passed all 374 tests in 7.373 seconds. These checks do not
establish the historical cause. The failure remains unresolved for independent
review; no wait, tolerance, assertion or classification was changed.

## Evidence identity

Raw commands, environments, statuses, output and hashes are in
`/tmp/astra-reverie-terminal-controls/exec-error-*`. The raw original regression
is `exec-worker-error-c53-second.{json,stdout,stderr}`; its diagnostic source is
preserved byte-identically as `exec-worker-error-c53-original.rs`. The initial
compile-only missing-Debug diagnostic is also retained separately.

- Original diagnostic Rust SHA256:
  `77246d823357ff84b8de0d8852028fed904945e24a39da0d087b1d94f404846d`.
- Original failing test binary:
  `target/astra-terminal-cancellation/exec-error-diagnostic/static_elf`, SHA256
  `47459bea01ed3cdac6a3ef0174c78e3106037602055abe9673ff1afd30a31e2a`.
- Final static test binary:
  `target/astra-terminal-cancellation/exec-error-final/static_elf`, SHA256
  `935c16c06e5e0ed7cc690bf4e5ef051c2d21ff53eaeda06597ccbcc940ab3ab9`.
- Library binary used by the failed full run and unchanged repeat:
  `target/astra-terminal-cancellation/exec-error-final/reverie_kvm`, SHA256
  `fccda97a23f683cd00dda9c4b02ddf3804e0c00a544f8c6705c98047e5a1db49`.
- Preserved c53 library binary SHA256:
  `aee428158cf9a09af9838d77b307dda7364677ebe1db0381202c4e01d0206aed`.
- Complete artifact manifest:
  `/tmp/astra-reverie-terminal-controls/exec-error-artifacts.json`.

Every runtime control uses required KVM. The static test runs under the existing
30-second plus two-second kill bound; the guest keeps its 15-second alarm.
Package/compiler commands have separately recorded outer bounds. No Hermit
capability predicate, signal comparator, public PR head or injection guard was
changed. This component remains part of the terminal API's required independent
review; it does not waive the applicable core-abstraction review requirement.
