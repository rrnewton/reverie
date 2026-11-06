# The Tool API

A tool is two trait implementations. This chapter tours the surface
your handlers can use; the rustdoc on the [`Tool`] trait is the
authoritative reference, and this chapter is the map to it.

[`Tool`]: https://docs.rs/reverie-core

## Handlers: one per event stream

Each trappable event has a handler with a default implementation, so
you override only what you subscribe to. The table lists the handlers
most tools touch; the full surface (including the structured-signal
family — `handle_raw_signal_event`, `handle_signal_dequeue`,
`handle_structured_signal_event` — and capability-gated extras) is
the `Tool` trait's rustdoc, which is authoritative:

| Handler | Event | Default |
|---|---|---|
| `handle_syscall_event` | a syscall at entry | `tail_inject` (perform it unchanged) — override only to do more |
| `handle_thread_start` | a new thread | no-op |
| `handle_post_exec` | after a successful `execve`, in the new image (thread/process state carried across unchanged) | no-op |
| `handle_exec_commit` | *before* the image is replaced, once an injected exec can no longer fail; old image still live; **KVM-family only today**, and injecting a syscall here is refused | no-op |
| `handle_signal_event` (+ raw/structured variants) | an inbound signal | deliver normally |
| `handle_timer_event` | a tool-scheduled timer | no-op |
| `handle_cpuid_event`, `handle_rdtsc_event` | those instructions (x86-64, subscription-gated) | execute natively |
| `on_exit_thread`, `on_exit_process` | teardown | no-op |

Lifecycle points are contractual: backends call `Tool::new` per new
process, `init_thread_state` per new thread, and the exit hooks as
threads and processes wind down. If your accounting depends on a
destructor running, say so in tests on every backend you support —
lifecycle coverage is precisely where backends differ today (see
[Backends](backends.md)).

## The Guest handle: your hands inside the handler

Every handler receives `guest: &mut T` where `T: Guest<Self>`. Through
it a tool can:

* read and write guest memory, and read registers (with typed access
  to syscall arguments — next chapter but one). Writing the full
  register file (`set_regs`) is *not* universal: its default returns
  `Errno::ENOSYS`, and only backends that implement it accept it —
  the fail-closed pattern for every optional `Guest` operation;
* **inject** syscalls: perform this syscall and never come back
  (`tail_inject` → `Never`), perform one and get its result
  (`inject`), or perform none and fabricate the result;
* **send RPC** to global state (`send_rpc`), typed by your
  `GlobalTool::Request`/`Response`;
* schedule timers, query task identity, and — where the backend
  supports it — the richer `Guest` operations documented per-method.

Handlers are `async`: a blocking syscall in one guest thread does not
block the tool's handling of other threads. Write handlers as if they
run concurrently, because on most backends they do.

## Subscriptions: trap only what you need

`Tool::subscriptions` declares what to trap: the specific syscall
numbers, plus (on x86-64) the CPUID and RDTSC instruction classes.
Other events (signals, timers, lifecycle) arrive by their own rules,
not via subscription. The backend turns that
into its cheapest exact filter — a seccomp filter, a patch set, a
hypercall mask. Subscribing to everything when you only need `openat`
is the single most common self-inflicted slowdown; the
[Performance](performance.md) chapter quantifies it. Subscriptions are
also a *correctness* surface: an event you did not subscribe to is an
event whose behaviour you did not specify, and backends are entitled
to let it run natively.

Note what a subscription is *not*: a mechanism choice. A tool declares
*events*, never *mechanisms* — and its needs as `ToolRequirements`
(window, instruction classes, lifecycle, placement; strictest by
default), which admission checks against the backend on offer without
the tool ever naming one. See
[Mechanisms](mechanisms.md#selection-requirements-probes-refusals).

## Errors

Most handlers return `Result<_, Error>` (or `Errno` where the guest
must see a specific errno); a few return `()` and cannot fail
(`handle_timer_event` is one — design timer work accordingly).
Distinguish three intents that novices merge:

* **Guest-visible failure** — you *want* the syscall to fail: return
  the errno as the syscall result. The guest continues, chastened.
* **Tool failure** — your handler itself cannot proceed: return
  `Error`. The run fails; the guest does not continue under a broken
  tool.
* **Backend refusal** — the backend cannot host this tool/program
  combination at all: surfaced before or at launch, by name. Never
  discovered mid-run as silent misbehaviour.

## Cookbook: four handlers beyond syscalls

Each recipe is a complete tool plus the test that proves it, run
below on the ptrace backend (the handlers are the same trait surface
every backend drives; CPUID is x86-64-only and subscription-gated).
Save the listings in order as one library module and one
`tests/cookbook.rs`; they pass `cargo test -- --test-threads=1`
(4 tests) against a checkout, needing `reverie`, `reverie-ptrace`,
`serde`, `tokio`, and `raw-cpuid`.

Listing CB-0 is the shared runner the tests use:

<span class="filename">Listing CB-0: tests/cookbook.rs — runner</span>

```rust,ignore
//! The four cookbook recipes, proven against the ptrace backend.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use reverie::Backend;
use reverie::process::Command;
use reverie_ptrace::PtraceBackend;
use typed_scratch::recipes::*;

fn run_typed<T: reverie::Tool + 'static>(
    config: <T::GlobalState as reverie::GlobalTool>::Config,
    program: &str,
    args: &[&str],
) -> (Vec<u8>, T::GlobalState) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let local = tokio::task::LocalSet::new();
    let mut command = Command::new(program);
    for a in args {
        command.arg(a);
    }
    let (output, global, _stats) = local
        .block_on(
            &runtime,
            PtraceBackend::run_with_output::<T>(command, config),
        )
        .expect("ptrace backend run");
    assert_eq!(
        output.status,
        reverie::ExitStatus::Exited(0),
        "guest must exit cleanly"
    );
    (output.stdout, global)
}

#[test]
```

### Recipe 1: suppress one signal

`handle_signal_event` returning `None` swallows the signal; the
guest never sees it. Proven on ptrace below. Note the observation
goes to global state by RPC — the handler itself stays on the
signal path and does no reporting work.

```rust,ignore
    // ---- Recipe 1: signal rewriting (suppress one signal) -------------
    #[derive(Debug, Default, Clone)]
    pub struct MuteGlobal {
        suppressed: Arc<AtomicU64>,
    }
    impl MuteGlobal {
        pub fn suppressed(&self) -> u64 {
            self.suppressed.load(Ordering::SeqCst)
        }
    }

    #[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
    pub struct MuteMsg;

    #[reverie::global_tool]
    impl GlobalTool for MuteGlobal {
        type Request = MuteMsg;
        type Response = ();
        type Config = ();

        async fn init_global_state(_: &()) -> Self {
            Self::default()
        }
        async fn receive_rpc(&self, _from: Pid, _msg: MuteMsg) {
            self.suppressed.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Debug, Default)]
    pub struct MuteSignal;

    #[reverie::tool]
    impl Tool for MuteSignal {
        type GlobalState = MuteGlobal;
        type ThreadState = ();

        async fn handle_signal_event<T: Guest<Self>>(
            &self,
            guest: &mut T,
            signal: Signal,
        ) -> Result<Option<Signal>, Errno> {
            if signal == Signal::SIGUSR1 {
                let _ = guest.send_rpc(MuteMsg).await;
                Ok(None)
            } else {
                Ok(Some(signal))
            }
        }
    }

```

```rust,ignore
fn recipe_signal_suppression() {
    let script = "trap 'echo caught' USR1; kill -USR1 $$; echo after";
    let (stdout, global) = run_typed::<MuteSignal>((), "/bin/sh", &["-c", script]);
    assert_eq!(stdout, b"after\n", "SIGUSR1 must be suppressed by the tool");
    assert_eq!(global.suppressed(), 1);
}

#[test]
```

### Recipe 2: re-arm after exec

State survives `execve` unchanged, so a per-image tool must reset
*itself* in `handle_post_exec`. One subtlety the test pins down:
the hook also fires for the launch exec, so the first image report
is exactly the execve itself.

```rust,ignore
    // ---- Recipe 2: post-exec re-arming (reset image-local counts) ------
    #[derive(Debug, Default, Clone)]
    pub struct ExecGlobal {
        per_image: Arc<Mutex<Vec<u64>>>,
        images: Arc<AtomicU64>,
    }
    impl ExecGlobal {
        pub fn per_image(&self) -> Vec<u64> {
            self.per_image.lock().unwrap().clone()
        }
        pub fn images(&self) -> u64 {
            self.images.load(Ordering::SeqCst)
        }
    }

    #[derive(PartialEq, Debug, Eq, Clone, Serialize, Deserialize)]
    pub enum ExecMsg {
        ImageDone(u64),
        ExecSeen,
    }

    #[reverie::global_tool]
    impl GlobalTool for ExecGlobal {
        type Request = ExecMsg;
        type Response = ();
        type Config = ();

        async fn init_global_state(_: &()) -> Self {
            Self::default()
        }
        async fn receive_rpc(&self, _from: Pid, msg: ExecMsg) {
            match msg {
                ExecMsg::ImageDone(n) => self.per_image.lock().unwrap().push(n),
                ExecMsg::ExecSeen => {
                    self.images.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    }

    #[derive(Debug, Default)]
    pub struct ImageCounter {
        calls: AtomicU64,
    }

    #[reverie::tool]
    impl Tool for ImageCounter {
        type GlobalState = ExecGlobal;
        type ThreadState = ();

        async fn handle_syscall_event<T: Guest<Self>>(
            &self,
            guest: &mut T,
            c: Syscall,
        ) -> Result<i64, Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            guest.tail_inject(c).await
        }

        /// Runs in the new image, with state carried over unchanged:
        /// this is where a per-image tool must re-arm itself.
        async fn handle_post_exec<T: Guest<Self>>(&self, guest: &mut T) -> Result<(), Errno> {
            let finished = self.calls.swap(0, Ordering::SeqCst);
            let _ = guest.send_rpc(ExecMsg::ImageDone(finished)).await;
            let _ = guest.send_rpc(ExecMsg::ExecSeen).await;
            Ok(())
        }

        async fn on_exit_process<G: GlobalRPC<Self::GlobalState>>(
            self,
            _pid: Pid,
            global: &G,
            _status: ExitStatus,
        ) -> Result<(), Error> {
            let last = self.calls.load(Ordering::SeqCst);
            let _ = global.send_rpc(ExecMsg::ImageDone(last)).await;
            Ok(())
        }
    }

```

```rust,ignore
fn recipe_post_exec_reset() {
    // sh runs some syscalls, then execs echo as its last command.
    let script = "echo first; exec /bin/echo second";
    let (stdout, global) = run_typed::<ImageCounter>((), "/bin/sh", &["-c", script]);
    assert_eq!(stdout, b"first\nsecond\n");
    // handle_post_exec also fires for the launch exec itself, so the
    // shell's exec is the second image boundary.
    assert_eq!(global.images(), 2, "launch exec + script exec");
    let per_image = global.per_image();
    assert_eq!(
        per_image.len(),
        3,
        "launch report + per-image reports: {per_image:?}"
    );
    assert_eq!(
        per_image[0], 1,
        "the launch execve itself is the first counted syscall"
    );
    assert!(
        per_image[1] > 0 && per_image[2] > 0,
        "both images ran syscalls: {per_image:?}"
    );
}

#[test]
```

### Recipe 3: sample on a timer

Precise timers count retired conditional branches. Two placement
rules the hard way: arm **after** exec (the launch exec retires
timers armed at thread start), and give the guest branches to
retire — a blocked guest retires none. Rearm inside the handler.

```rust,ignore
    // ---- Recipe 3: timer sampling --------------------------------------
    #[derive(Debug, Default, Clone)]
    pub struct TickGlobal {
        ticks: Arc<AtomicU64>,
    }
    impl TickGlobal {
        pub fn ticks(&self) -> u64 {
            self.ticks.load(Ordering::SeqCst)
        }
    }

    #[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
    pub struct Tick;

    #[reverie::global_tool]
    impl GlobalTool for TickGlobal {
        type Request = Tick;
        type Response = ();
        type Config = ();

        async fn init_global_state(_: &()) -> Self {
            Self::default()
        }
        async fn receive_rpc(&self, _from: Pid, _msg: Tick) {
            self.ticks.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Debug, Default)]
    pub struct TickSampler;

    #[reverie::tool]
    impl Tool for TickSampler {
        type GlobalState = TickGlobal;
        type ThreadState = bool;

        fn subscriptions(_cfg: &()) -> Subscription {
            let mut events = Subscription::none();
            events.syscalls([reverie::syscalls::Sysno::getpid]);
            events
        }

        /// Arm post-exec: the initial exec retires timers armed at
        /// thread start, so a sampler that must outlive exec arms here.
        async fn handle_post_exec<T: Guest<Self>>(&self, guest: &mut T) -> Result<(), Errno> {
            guest
                .set_timer_precise(reverie::TimerSchedule::Rcbs(1))
                .map_err(|_| Errno::EIO)?;
            Ok(())
        }

        async fn handle_timer_event<T: Guest<Self>>(&self, guest: &mut T) {
            let _ = guest.send_rpc(Tick).await;
            let _ = guest.set_timer_precise(reverie::TimerSchedule::Rcbs(1_000_000));
        }
    }

```

```rust,ignore
fn recipe_timer_sampling() {
    // The sampler subscribes to getpid only; the guest hammers it, so
    // the 1-RCB precise timer armed at thread start expires at once.
    let (stdout, global) = run_typed::<TickSampler>(
        (),
        "/usr/bin/python3",
        &[
            "-c",
            "import os; [os.getpid() for _ in range(2000)]; print('done')",
        ],
    );
    assert_eq!(stdout, b"done\n");
    assert!(
        global.ticks() >= 1,
        "expected at least one timer sample, got {}",
        global.ticks()
    );
}

#[test]
```

### Recipe 4: spoof one CPUID leaf

Subscription-gated and x86-64-only. Spoof *one leaf*, never the
whole result: overwriting EBX on every leaf corrupts leaf 7's
feature word and glibc rejects the CPU at startup
(`Fatal glibc error: CPU does not support x86-64-v2`, guest exit
127 — observed while proving this recipe). Counts are kept
process-locally and reported once at process exit; no RPC happens
inside the CPUID handler.

```rust,ignore
    // ---- Recipe 4: CPUID spoofing (subscription-gated, x86_64) --------
    #[derive(Debug, Default, Clone)]
    pub struct CpuidGlobal {
        spoofed: Arc<AtomicU64>,
    }
    impl CpuidGlobal {
        pub fn spoofed(&self) -> u64 {
            self.spoofed.load(Ordering::SeqCst)
        }
    }

    #[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
    pub struct CpuidSeen(pub u64);

    #[reverie::global_tool]
    impl GlobalTool for CpuidGlobal {
        type Request = CpuidSeen;
        type Response = ();
        type Config = ();

        async fn init_global_state(_: &()) -> Self {
            Self::default()
        }
        async fn receive_rpc(&self, _from: Pid, msg: CpuidSeen) {
            self.spoofed.fetch_add(msg.0, Ordering::SeqCst);
        }
    }

    #[derive(Debug, Default)]
    pub struct CpuidSpoof {
        local: AtomicU64,
    }

    pub const SPOOF_CPUID_EBX: u32 = 0x1234_5678;

    #[reverie::tool]
    impl Tool for CpuidSpoof {
        type GlobalState = CpuidGlobal;
        type ThreadState = ();

        fn subscriptions(_cfg: &()) -> Subscription {
            let mut s = Subscription::all_syscalls();
            s.cpuid();
            s
        }

        #[cfg(target_arch = "x86_64")]
        async fn handle_cpuid_event<T: Guest<Self>>(
            &self,
            guest: &mut T,
            eax: u32,
            ecx: u32,
        ) -> Result<raw_cpuid::CpuIdResult, Errno> {
            let mut res = raw_cpuid::cpuid!(eax, ecx);
            // Only leaf 1: its EBX is brand/APIC topology. Leaf 7's EBX
            // is a feature word - overwriting it makes glibc reject
            // the CPU at startup ("CPU does not support x86-64-v2").
            if eax == 1 {
                self.local.fetch_add(1, Ordering::SeqCst);
                res.ebx = SPOOF_CPUID_EBX;
            }
            Ok(res)
        }

        async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
            self,
            _pid: Pid,
            global: &G,
            _status: reverie::ExitStatus,
        ) -> Result<(), Error> {
            let n = self.local.load(Ordering::SeqCst);
            let _ = global.send_rpc(CpuidSeen(n)).await;
            Ok(())
        }
    }
}
```

```rust,ignore
fn recipe_cpuid_spoofing() {
    let guest = concat!(env!("CARGO_BIN_EXE_cpuid_guest"));
    let oracle = std::process::Command::new(guest).output().unwrap();
    assert!(oracle.status.success());
    let oracle = String::from_utf8(oracle.stdout).unwrap();

    let (stdout, global) = run_typed::<CpuidSpoof>((), guest, &[]);
    let spoofed = String::from_utf8(stdout).unwrap();
    assert_ne!(
        spoofed, oracle,
        "tool must change the reported CPUID values"
    );
    let leaf1 = spoofed.lines().nth(1).expect("leaf 1 line");
    assert!(
        leaf1.contains("ebx=12345678"),
        "spoofed leaf-1 EBX missing: {leaf1}"
    );
    assert!(
        spoofed.lines().next().unwrap().contains("ebx=68747541"),
        "leaf 0 vendor EBX must pass through untouched"
    );
    assert!(
        global.spoofed() >= 1,
        "leaf 1 spoofed at least once (glibc also asks): {}",
        global.spoofed()
    );
}
```

The guest in recipe 4 is twelve lines: print
`__cpuid_count(leaf, 0)` as hex for leaves 0 and 1
(`src/bin/cpuid_guest.rs` next to the test). Leaf 0's vendor EBX
passes through untouched; only leaf 1's EBX is forged.
>
> 🚧 **Under construction — Guest capability matrix.** TODO: tabulate
> which `Guest` operations each backend implements vs refuses, sourced
> from the conformance suite rather than from reading four
> implementations by hand. <!-- UC: api-guest-matrix -->
> Tracking: [reverie#1272](https://github.com/rrnewton/reverie/issues/1272) (`api-guest-matrix`).
