# Terminal disposal of inherited stdin zero-byte reads

The inherited stdin path can make a blocking host `read(fd, staging, 0)`.
SIGURG interrupts a read already in the kernel, but a terminal signal sent
before that read enters the kernel does not prevent it from blocking afterward.
A guest group exit can consequently hang while joining the Rust worker.

The repair covers that existing inherited-fd0/count-zero invocation only.
Descriptor routing, access and guest-range checks still run in their original
order. The helper receives the original host descriptor and the empty Vec's
actual staging pointer, not the numeric guest pointer. Ordinary descriptors
keep the existing zero-count handling; nonzero host reads are unchanged.

## Native lifetime

Each invocation owns one joinable pthread whose entry, cleanup handler and
cancellation frames are C. The Rust worker remains synchronous and is never
the target of `pthread_cancel`. The helper establishes public deferred
cancellation, calls public `read`, then disables cancellation before publishing
its return count and saved errno. Its cancellation cleanup publishes Canceled,
which means unknown kernel progress. It is neither an errno nor a promise that
the endpoint was untouched.

The Rust group registry serializes operation registration with sticky group or
worker cancellation. Each operation carries a monotonically increasing ID,
image and guest task identity, original request, and actual host arguments.
The existing owned stdin File moves into the operation; no duplicate descriptor
or endpoint flag mutation is needed.

Cancellation before creation/publication latches. Publication accounts for a
child that already completed. A sender is admitted only while the handle is
callable and the outcome remains Pending. Its lease lasts through the actual
public `pthread_cancel` return. After outcome publication, the sole Rust owner
disarms admission, drains admitted senders, and calls exactly one `pthread_join`
outside registry and operation locks. Only successful join (or proof that no
thread was created) permits endpoint restoration and operation removal.
Late wakers retain the allocation through Arc ownership and cannot cancel it.

Create, cancel, join and synchronization failures are backend control errors,
never guest results. A failed cancel with a pending read is reported without
waiting for unrelated endpoint activity. An unjoined helper keeps its original
endpoint and C allocation in registry ownership; if the group is destroyed,
process-lifetime retained ownership takes over. There is no detach, retry,
rollback, fallback read or fabricated successful retirement. An uninterruptible
kernel driver can still prevent physical teardown from completing.

## Terminal observation and guest disposition

The original worker waits on a C condition/epoch. It polls only existing entry
failure and Tool terminal observers; an ordinary wake does not establish a
terminal cause. Fork-local driver failures retain their existing scope, while
the traced root also observes run-wide failure.

The Tool observer facade keeps the actual user future allocated even after a
terminal notification or poll panic. It catches polling inside an always-pending
driver, then reports selection outside that driver stack. User destruction runs
after local helper retirement or explicit retained-ownership failure. Poll and
destructor panics have separate catches and retain their original payloads and
typed control errors through the backend's existing panic owner.

An exact TerminalReadCancelled disposition is consumed before Direct result
writeback, Tool injection continuation, or backend-owned Tool result handling.
These paths reuse the existing thread/group terminal protocols. Errors carrying
cleanup remain real failures. The merged worker clear_child_tid-before-terminal
receipt ordering is unchanged. Cancellation of C helpers precedes blocked Rust
worker joins; exec rearm refuses any still-owned old-image helper.

## Focused controls and limits

`tests/terminal_read_protocol.c` uses test-only C gates around creation,
publication, read entry/return, send admission/return, disarm and join. It covers
normal completion, actual inotify blocking, queued-event errno, descriptor reuse
and retained create/cancel/join-error ownership. Compile the C implementation and
test together with `-DRVK_READ_TEST -std=c11 -pthread -fexceptions`; production
builds omit the gates. The delayed-sender controls stop immediately before the
public cancel call and immediately after its return, not inside libc assembly.

The additional context control requires readable LSM current-label attributes.
Its refusal on a host without that interface is a failed qualification, not
evidence of equivalent security policy. Credentials, namespaces, masks and
alternate signal stacks are separately checked. A new thread has a distinct
TID and a disabled alternate signal stack; arbitrary thread-sensitive host
policies are not proven equivalent by these controls.

Rust tests cover sticky registration, worker/root scope, spurious wakes,
nonreturning Tool disposition, and actual Tool-observer allocation/panic lifetime.
State-gated Direct/Tool probes additionally bind the executable, public libc
symbols, host arguments, actual cancel targets and physical join order. A
timeout or rescue remains failure. Evidence from the b9 baseline remains
separate from the refreshed-main baseline and implementation.

This increment does not qualify guest SIGUSR1/restart behavior, virtual timers,
scheduling, record/replay, copyout, endpoint policy changes, or the broader
combined Hermit integration tests.
