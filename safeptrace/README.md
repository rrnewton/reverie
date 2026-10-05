# safeptrace

Safe Rust wrappers around the Linux `ptrace` API, used by
[Reverie](https://docs.rs/reverie-core), the instrumentation framework beneath
[Hermit](https://hermetic-infra.org).

The state types distinguish a running tracee from a stopped tracee, helping
callers use ptrace operations only when they are valid. Callers own process
lifetime and tracing policy.

```toml
[dependencies]
safeptrace = "0.4"
```

## Optional features

- `memory`: guest-memory access through `reverie-memory`. Memory access requires
  a stopped tracee.
- `notifier`: asynchronous ptrace event notification for runtimes such as Tokio.

Both features are off by default. For asynchronous tracing with memory access:

```toml
[dependencies]
safeptrace = { version = "0.4", features = ["memory", "notifier"] }
```

See the [API documentation](https://docs.rs/safeptrace) for state transitions
and wait contracts. For a complete execution engine and its platform setup,
use [`hermit-run`](https://crates.io/crates/hermit-run).

## Choosing a notifier interface

The standard notifier methods, including `Running::next_state`,
`Running::wait_owned`, and `Running::exit_event`, use native thread pidfds.
Their futures remain `Send` and preserve the existing contract for polling
from a sibling OS thread. `Running::try_new` reports acquisition errors
directly. They require kernel support for `PIDFD_THREAD`;
an unsupported kernel returns the native `pidfd_open` refusal (`EINVAL` for
`PIDFD_THREAD` on Linux before 6.9) without changing
the threading contract. Native acquisition also reports the Linux 6.9
requirement to stderr on a best-effort basis when it returns `EINVAL`;
the explicit fallback does not emit that diagnostic. Linux still requires the
actual ptracer thread for ptrace operations such as resuming a stop.

Regular-file output is skipped when the observed `RLIMIT_FSIZE` is finite or
cannot be read. This check does not pin the limit against concurrent changes.

For older kernels, select the explicit ptracer-thread interface when creating
the state: `Running::new_on_ptracer_thread`,
`Running::attach_on_ptracer_thread`, or `Running::seize_on_ptracer_thread`.
Use `wait_owned_on_ptracer_thread` (also named `next_state_on_ptracer_thread`),
`exit_event_on_ptracer_thread`, and `terminal_cleanup_on_ptracer_thread` to
progress that state. `wait_sync_on_ptracer_thread` provides a retaining
synchronous wait. The original ptracer OS thread must remain alive and drive
these operations.

The explicit wait futures and cleanup wrappers are `!Send` and `!Sync` on
every kernel. Their `into_driver` methods transfer the same unfinished
operation into a `Send` driver. Drivers do not implement `Future`: use their
explicit `poll_on_ptracer_thread`, `wait_on_ptracer_thread`, or
`progress_on_ptracer_thread` methods, as appropriate, on the original ptracer
thread. A foreign-thread refusal retains the original state and wait
authority. Keep that same driver and return it to its owner; an adapter must
not drop it on error or rebuild a state from the numeric PID.

A successful explicit attach or seize can still carry a notifier-capture
error. The returned state retains that refusal, available through its cleanup
handle, and local waits do not retry capture against a possibly reused PID.
Retain the returned state when handling the error.

## Kernel and namespace requirements

Linux introduced `PIDFD_THREAD` in 6.9. The explicit interface can fall back
when that flag returns `EINVAL`, using ordinary leader pidfds and retained
`/proc/<tid>` descriptors for nonleaders. It requires a procfs mount aligned
with the caller's PID namespace; an inherited mount from an outer PID
namespace is refused with `EXDEV`. Mount procfs for the active PID namespace
before selecting this interface.

The explicit operations also authenticate the executing host thread through
the original retained proc mount. Copying a driver into a process with the
same numeric thread ID in another PID namespace does not transfer authority.
A later overmount of `/proc` does not redirect this retained identity check.
The same check applies when converting an already-bound generic state to an
explicit interface, and to numeric ptrace requests, memory access, and stopped
observations through an explicit state. A new host owner is authenticated
through the Event's original proc mount before any queued status is transferred.

Numeric requests compare fresh directory metadata for both the executing
thread and target with their pinned original proc directories. A successful
full target status check is reused until a wait event is published or decoded,
a resume or detach succeeds, or the attachment reconnects. Changes to status
read permissions during an already validated stop are observed at the next
validation; directory lookup failures are still reported immediately. Failed
validation and raw ptrace errors are never cached, so a later request can retry
through the same original capability.

Selecting a host thread alone does not authorize waiting for an untraced task.
The explicit wait interfaces also authenticate the original attachment or the
task's real parent TGID before transferring a queued status or exit capability.
`new_on_ptracer_thread` can select an untraced nonchild for later attachment;
its waits return `EPERM` until an actual attachment establishes that role.
An established role stays with that same task generation through its final
published result, including after its proc directory disappears.

Ptrace requests still address numeric TIDs. The explicit interface checks the
retained target before issuing a request and rejects an already retired target.
A running nonleader can exec, release its TID, and be replaced by another tracee
of the same ptracer between that check and the numeric syscall.
`Running::interrupt` retains this existing concurrent exec/reuse limitation on
both interfaces, including kernels with native thread pidfds. The lifetime
precheck provides no atomic ptrace identity guarantee. See the
[tracked limitation](https://github.com/rrnewton/reverie/issues/860).

These kernel mechanisms do not by themselves establish a qualified minimum
kernel for every SDK operation or for Hermit. Consult the execution engine's
platform requirements for its complete support policy.

`TerminalCleanup::request_sigkill` signals the retained descriptor and can
terminate the tracee's thread group on either path. Signal success or
`ESRCH` does not acknowledge completion: consume the actual exit and terminal
wait status before releasing the original owners.

`TerminalCleanup::request_sigstop` requires a native thread pidfd for an exact
thread-directed stop. A live legacy handle, including a nonleader proc
descriptor, returns `EOPNOTSUPP`; a retired descriptor returns `ESRCH`.
Descriptor signal permission errors remain errors. This refusal does not
establish that SIGSTOP would be permitted, and no stop request proves that a
ptrace stop has occurred.
