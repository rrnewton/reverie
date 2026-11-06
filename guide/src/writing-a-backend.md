# Writing a backend

This chapter is for the smallest audience of the book: people adding a
*backend*, not a tool. A backend is not a tool, and not a bare
mechanism (a VM, a sandbox, a rewriter). It is a complete
implementation of process supervision and event interception that
hosts an **arbitrary** `T: Tool`.

## The ten duties

Given a command and the tool's static configuration, your backend must:

1. **Initialize global state** — call `GlobalTool::init_global_state`
   once for the tree; keep the singleton reachable for RPC for the
   run's lifetime.
2. **Compute subscriptions** — call `Tool::subscriptions` once; trap
   exactly those streams.
3. **Spawn and supervise the guest** — the root process and its whole
   tree across `fork`/`clone`/`vfork`/`execve`, including stdio.
4. **Allocate state** — `Tool::new` per process,
   `init_thread_state` per thread, at the documented points.
5. **Route every subscribed event** — drive the handlers, each with a
   `Guest` handle for inspection, mutation, injection, and RPC.
6. **Run destructors** — `on_exit_thread` / `on_exit_process` as the
   tree winds down.
7. **Return `(ExitStatus, GlobalState)`** — the root's status and the
   now-uniquely-owned final state.
8. **Report real statistics** — a typed snapshot from an actual
   collector; unsupported measurements are absent, never zero.
9. **Capture output on request** — guest stdout/stderr bytes alongside
   status, state, and stats.
10. **Describe itself** — `fn capabilities()` returns your backend's
    static `BackendCapabilities`. It has *no default implementation*
    (deliberately: inheriting another backend's answer would silently
    misinform the tool), and `type Stats: BackendStatsSnapshot` is
    likewise required. A backend built to duties 1–9 alone does not
    compile — this duty is part of the trait, not garnish.

The `reverie::Backend` trait captures duties 1–10 as `run`,
`run_with_stats`, `run_with_output`, `capabilities()`, and
`type Stats`. A real backend also exposes a
richer native builder (ptrace's `TracerBuilder`/`Tracer` adds a GDB
server, function guests, and lifecycle/stdio control) — but the trait
surface is the peer contract, and it is not optional garnish.

## The three design questions, again

Decide explicitly, and document in your backend's page:

* **Hooking** — your fast path, and its site-discovery story.
* **Trapping** — your completeness backstop and its fail-closed
  default. If you cannot name it, you do not have one.
* **RPC** — local, UDS transport, or your own — with its ordering and
  failure semantics.

Then state your **refusals**: binary shapes, lifecycle cases, signal
policies, `Guest` operations you do not implement. A backend whose
boundaries are discovered by its users has failed its main job.

## Proving it

Run the example tools (`noop`, counters, `strace`, `chaos`, …) on the
shared corpus through your backend and diff against ptrace. The
conformance harness is your entrance exam; passing cells you invented
yourself is not. Expect the failures to be lifecycle-shaped first —
that is normal, and narrowing your documented envelope to what passes
is the honest intermediate state.

## The skeleton project

The fastest honest start is a *delegating* skeleton: it satisfies
every duty above by forwarding supervision to `PtraceBackend`, so
real tools pass on day one, and you replace the delegation piece by
piece — your own spawn, your own event loop, your own `Guest` —
without ever owning a half-backend that runs nothing. Two shortcuts
are marked as such in the code and must be the first things you
replace: `capabilities()` forwards ptrace's answer (correct only
while delegation is total), and the statistics are one real
measurement (elapsed wall time), not a collector.

Save Listing WB-1 as `src/lib.rs` in a crate depending on
`reverie`, `reverie-ptrace`, `serde`, and `tokio`
(`macros,rt,time`). Note the `#[reverie::backend(?Send)]`
attribute: it is part of the trait surface, not decoration.

<span class="filename">Listing WB-1: src/lib.rs</span>

```rust,ignore
//! A minimal, compiling `Backend` skeleton.
//!
//! Supervision is delegated to `PtraceBackend` on day one so the
//! skeleton passes real tools immediately; replace the delegation
//! piece by piece (your own spawn, your own event loop, your own
//! Guest) as the mechanism lands. `capabilities()` forwarding is
//! the one deliberate shortcut, and it is only honest while the
//! delegation is total: the moment your mechanism differs, write
//! your own capabilities — inheriting another backend's answer
//! would silently misinform the tool.

use std::fmt;
use std::time::{Duration, Instant};

use reverie::process::Command;
use reverie::{
    Backend, BackendCapabilities, BackendStatsSnapshot, Error, ExitStatus, GlobalTool, Tool,
};
use reverie_ptrace::PtraceBackend;

/// The skeleton backend. Single-threaded by construction today:
/// it inherits ptrace's supervision, which is also its stated
/// refusal boundary until replaced.
pub struct SkeletonBackend;

/// Real, if modest, statistics: how long the delegated run took.
/// A metric this backend cannot measure is absent from the schema,
/// never reported as zero.
#[derive(Debug)]
pub struct SkeletonStats {
    elapsed: Duration,
}

impl fmt::Display for SkeletonStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "skeleton elapsed={:?}", self.elapsed)
    }
}

impl BackendStatsSnapshot for SkeletonStats {
    const BACKEND_NAME: &'static str = "skeleton";

    fn dispatch_stats(&self) -> Option<reverie::DispatchStats> {
        None // No dispatch counters in a delegating skeleton.
    }
}

#[reverie::backend(?Send)]
impl Backend for SkeletonBackend {
    type Stats = SkeletonStats;

    fn capabilities() -> BackendCapabilities {
        // Deliberate skeleton shortcut (see module docs): correct
        // only while supervision is fully delegated to ptrace.
        PtraceBackend::capabilities()
    }

    async fn run<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ExitStatus, T::GlobalState), Error>
    where
        T: Tool + 'static,
    {
        PtraceBackend::run::<T>(command, config).await
    }

    async fn run_with_stats<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(ExitStatus, T::GlobalState, Self::Stats), Error>
    where
        T: Tool + 'static,
    {
        let started = Instant::now();
        let (status, global) = PtraceBackend::run::<T>(command, config).await?;
        Ok((
            status,
            global,
            SkeletonStats {
                elapsed: started.elapsed(),
            },
        ))
    }

    async fn run_with_output<T>(
        command: Command,
        config: <T::GlobalState as GlobalTool>::Config,
    ) -> Result<(reverie::process::Output, T::GlobalState, Self::Stats), Error>
    where
        T: Tool + 'static,
    {
        let started = Instant::now();
        let (output, global, _ptrace_stats) =
            PtraceBackend::run_with_output::<T>(command, config).await?;
        Ok((
            output,
            global,
            SkeletonStats {
                elapsed: started.elapsed(),
            },
        ))
    }
}
```

Listing WB-2 is the entrance exam from *Proving it*, reduced to
its first two rungs: the unit tool (the `noop` shape) and a
counter. Both must pass before any delegation comes out.

<span class="filename">Listing WB-2: tests/skeleton.rs</span>

```rust,ignore
//! The skeleton's entrance exam: `noop` and `counter1` shapes.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use backend_skeleton::SkeletonBackend;
use reverie::process::Command;
use reverie::syscalls::{Syscall, SyscallInfo};
use reverie::{Backend, Error, GlobalTool, Guest, Pid, Tool};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone)]
struct CounterGlobal {
    num_syscalls: Arc<AtomicU64>,
}
impl CounterGlobal {
    fn total(&self) -> u64 {
        self.num_syscalls.load(Ordering::SeqCst)
    }
}

#[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
struct IncrMsg(reverie::syscalls::Sysno);

#[reverie::global_tool]
impl GlobalTool for CounterGlobal {
    type Request = IncrMsg;
    type Response = ();
    type Config = ();

    async fn init_global_state(_: &()) -> Self {
        Self::default()
    }
    async fn receive_rpc(&self, _from: Pid, _msg: IncrMsg) {
        self.num_syscalls.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone)]
struct CounterLocal;

#[reverie::tool]
impl Tool for CounterLocal {
    type GlobalState = CounterGlobal;
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let _ = guest.send_rpc(IncrMsg(syscall.number())).await;
        guest.tail_inject(syscall).await
    }
}

fn runtime() -> (tokio::runtime::Runtime, tokio::task::LocalSet) {
    (
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime"),
        tokio::task::LocalSet::new(),
    )
}

#[test]
fn noop_passes_on_skeleton() {
    let (rt, local) = runtime();
    let (status, ()) = local
        .block_on(
            &rt,
            SkeletonBackend::run::<()>(
                {
                    let mut command = Command::new("/bin/echo");
                    command.arg("hello");
                    command
                },
                (),
            ),
        )
        .expect("skeleton run");
    assert_eq!(status, reverie::ExitStatus::Exited(0));
}

#[test]
fn counter1_passes_on_skeleton() {
    let (rt, local) = runtime();
    let (output, global, stats) = local
        .block_on(
            &rt,
            SkeletonBackend::run_with_output::<CounterLocal>(
                {
                    let mut command = Command::new("/bin/echo");
                    command.arg("hello");
                    command
                },
                (),
            ),
        )
        .expect("skeleton run with output");
    assert_eq!(output.stdout, b"hello\n");
    assert!(global.total() > 0, "counter observed syscalls");
    assert!(
        stats.to_string().starts_with("skeleton elapsed="),
        "typed stats present: {stats}"
    );
}
```

As printed, `cargo test -- --test-threads=1` reports both tests
passing. From here the order that keeps every intermediate state
runnable is: your own spawn/supervision behind the same `run`
surface, then your own event routing with the ptrace `Guest`
replaced last — statistics and capabilities stop being forwarded
at the moment the corresponding mechanism becomes yours.
