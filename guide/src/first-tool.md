# Project: your first tool

We build a syscall counter, run it, and then break it down. The code
is `reverie-examples/counter1_tool.rs` (the tool) and
`reverie-examples/counter1.rs` (its launcher). Run it first:

```console
$ cargo run --bin counter1 -- /bin/echo hello
```

`echo` prints `hello` as usual, and at exit the launcher prints
`counter1-global syscalls=N` — the total the tool accumulated. Success!
Now let's see what you just ran.

## The tool: two halves

Every Reverie tool is a pair:

* the **local half** (`Tool`) — one instance per guest process, with
  per-thread state, runs the event handlers;
* the **global half** (`GlobalTool`) — one instance for the whole
  guest tree, reached from handlers by RPC.

Listing FT-1 is the counter's policy (the full file adds only a
`total()` accessor and a tracing line). It is short because the
framework carries the machinery; your code carries only the policy.

<span class="filename">Listing FT-1: reverie-examples/counter1_tool.rs</span>

```rust,ignore
#[derive(Debug, Default, Clone)]
pub struct CounterGlobal {
    num_syscalls: Arc<AtomicU64>,
}

#[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct IncrMsg(pub Sysno);

#[reverie::global_tool]
impl GlobalTool for CounterGlobal {
    type Request = IncrMsg;
    type Response = ();
    type Config = ();

    async fn init_global_state(_: &Self::Config) -> Self {
        Self::default()
    }

    async fn receive_rpc(&self, _from: Pid, IncrMsg(sysno): IncrMsg) {
        self.num_syscalls.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default, Clone)]
pub struct CounterLocal {}

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
```

Breaking it down:

* `handle_syscall_event` is the only handler this tool overrides. It
  receives the typed `Syscall`, the live `Guest` handle, and must
  produce the syscall's result (`i64`). Everything a handler may do —
  inspect memory, rewrite arguments, inject *other* syscalls,
  suppress this one — is a variation on this signature.
* `guest.send_rpc(IncrMsg(..))` is the only way local handlers reach
  global state. The request type is yours (`IncrMsg`); it must be
  serializable, because on in-guest backends this really crosses a
  process boundary, while on ptrace it compiles down to a local call.
  The tool doesn't know which — and must not care.
* `guest.tail_inject(syscall)` means "now actually perform the syscall
  I was handed" — and it **never returns** (its type is `Never`): the
  guest resumes after the syscall, and the handler frame is gone, so
  the result can neither be inspected nor altered afterwards. Most
  observability tools end most handlers this way. When you *do* need
  the result, use `guest.inject(...)` and return (or transform) it
  yourself. (Return a value without injecting, and you have *elided*
  the syscall; return an error value, and you have injected a failure.
  `chaos` does both.)
* `init_global_state` runs once for the tree. `Config` is `()` here;
  real tools take their run-time configuration through it.

## The launcher: pick a backend, get state back

Running a tool always has the same shape: hand a backend a command and
the tool's config; receive the guest's exit status together with the
final global state. Through the abstract trait:

```rust,ignore
use reverie::Backend;
let (exit_status, global_state) =
    reverie_ptrace::PtraceBackend::run::<MyTool>(command, config).await?;
```

The examples' shared launcher (`reverie-examples/backend.rs`) goes one
step further: `--backend ptrace|liteinst|in-guest-trap|kvm` runs the
*same* `T: Tool` through each backend's public entry point, so one
tool can be compared across mechanisms — with the program eligibility
each backend states (KVM, for example, takes static ELF guests only).
A backend
either runs the tool or refuses by name (`RunError::Refused`) — for
example, the in-guest backends refuse when no preload compiling your
tool in can be found, saying so in the error. Keep that distinction in
mind: it is the difference between a toolkit and a pile of backends.

## Growing this counter, stage by stage

This counter is the book's running project. Each stage below
changes one thing, and each names its full code, so you can stop
at any stage with a complete, runnable tool:

| Stage | What changes | Full code |
|---|---|---|
| 1. Count everything | `counter1`, one RPC per syscall | Listing FT-1 above; `reverie-examples/counter1_tool.rs` (+ launcher `counter1.rs`) |
| 2. Count only `openat` | a narrow subscription | Listing FT-2 below |
| 3. Histogram, not total | per-syscall counts in thread state | Listing FT-3 below |
| 4. Count the distributed way | map in thread state, fold at exits | [State at three scales](state.md), Listings ST-1–ST-3; `reverie-examples/counter2_tool.rs` |
| 5. Same tool, every backend | the launcher picks the mechanism | [Testing](testing.md); `reverie-examples/backend.rs` (`--backend`) |

Stages 2 and 3 are complete tools in the cookbook's sense: put
them in a library module and drive them with the runner from
[Listing CB-0](tool-api.md), asserting on the returned global
state (`/bin/cat /etc/hostname` counts its opens with output
untouched; `/bin/echo`'s histogram contains its `write` and the
loader's `openat`s). With the cookbook's dependencies minus
`raw-cpuid`, they pass `cargo test -- --test-threads=1` (2 tests)
against a checkout.

Stage 2 keeps counter1's shape — count by RPC — and changes
exactly one thing: the subscription. Everything not named is
never trapped, which is the cheapest performance decision a tool
makes ([Performance](performance.md)). Note what the count
includes: the dynamic loader opens libraries before `main`, so
`opens()` counts the guest *process*, not just your guest's
`main`.

<span class="filename">Listing FT-2: count only openat</span>

```rust,ignore
    // ---- Stage 2: subscribe narrowly - count only openat ---------------
    #[derive(Debug, Default, Clone)]
    pub struct OpenatGlobal {
        opens: Arc<AtomicU64>,
    }
    impl OpenatGlobal {
        pub fn opens(&self) -> u64 {
            self.opens.load(Ordering::SeqCst)
        }
    }

    #[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
    pub struct Opened;

    #[reverie::global_tool]
    impl GlobalTool for OpenatGlobal {
        type Request = Opened;
        type Response = ();
        type Config = ();

        async fn init_global_state(_: &()) -> Self {
            Self::default()
        }
        async fn receive_rpc(&self, _from: Pid, _msg: Opened) {
            self.opens.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// counter1, grown one step: same count-by-RPC shape, but the
    /// subscription names the one syscall the tool cares about, so the
    /// backend never traps the rest.
    #[derive(Debug, Default)]
    pub struct OpenatCounter;

    #[reverie::tool]
    impl Tool for OpenatCounter {
        type GlobalState = OpenatGlobal;
        type ThreadState = ();

        fn subscriptions(_cfg: &()) -> Subscription {
            let mut events = Subscription::none();
            events.syscalls([Sysno::openat]);
            events
        }

        async fn handle_syscall_event<T: Guest<Self>>(
            &self,
            guest: &mut T,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            debug_assert_eq!(syscall.number(), Sysno::openat);
            let _ = guest.send_rpc(Opened).await;
            guest.tail_inject(syscall).await
        }
    }
```

Stage 3 keeps *which* syscalls (all of them) and changes *where
the counting lives*: the thread state is a histogram, updated
with no RPC in the handler, folded into global state once per
thread at `on_exit_thread`. This is counter2's map step with a
wider value; stage 4 then does the full map/reduce
honestly across processes. Two constraints from the trait are
visible in the types: `ThreadState` must be serializable and
`Default` (a `BTreeMap` qualifies), and thread state survives
`execve` unchanged — a per-image histogram must reset itself in
`handle_post_exec`, as [State](state.md) warns.

<span class="filename">Listing FT-3: a per-syscall histogram in thread state</span>

```rust,ignore
    // ---- Stage 3: per-syscall histogram in thread state ----------------
    #[derive(Debug, Default, Clone)]
    pub struct HistogramGlobal {
        counts: Arc<Mutex<BTreeMap<Sysno, u64>>>,
    }
    impl HistogramGlobal {
        pub fn count(&self, sysno: Sysno) -> u64 {
            self.counts.lock().unwrap().get(&sysno).copied().unwrap_or(0)
        }
        pub fn snapshot(&self) -> Vec<(Sysno, u64)> {
            self.counts
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (*k, *v))
                .collect()
        }
        pub fn total(&self) -> u64 {
            self.counts.lock().unwrap().values().sum()
        }
    }

    /// One thread's finished histogram, folded into global state once.
    #[derive(PartialEq, Debug, Eq, Clone, Serialize, Deserialize)]
    pub struct ThreadHistogram(pub Vec<(Sysno, u64)>);

    #[reverie::global_tool]
    impl GlobalTool for HistogramGlobal {
        type Request = ThreadHistogram;
        type Response = ();
        type Config = ();

        async fn init_global_state(_: &()) -> Self {
            Self::default()
        }
        async fn receive_rpc(&self, _from: Pid, msg: ThreadHistogram) {
            let mut counts = self.counts.lock().unwrap();
            for (sysno, n) in msg.0 {
                *counts.entry(sysno).or_default() += n;
            }
        }
    }

    /// counter2's map step, one dimension wider: the thread state is
    /// a histogram keyed by syscall number instead of a single total.
    /// No RPC on the hot path - the map is thread-local until exit.
    #[derive(Debug, Default)]
    pub struct Histogram;

    #[reverie::tool]
    impl Tool for Histogram {
        type GlobalState = HistogramGlobal;
        type ThreadState = BTreeMap<Sysno, u64>;

        async fn handle_syscall_event<T: Guest<Self>>(
            &self,
            guest: &mut T,
            syscall: Syscall,
        ) -> Result<i64, Error> {
            *guest
                .thread_state_mut()
                .entry(syscall.number())
                .or_default() += 1;
            guest.tail_inject(syscall).await
        }

        async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: Tid,
            global: &G,
            thread_state: BTreeMap<Sysno, u64>,
            _status: reverie::ExitStatus,
        ) -> Result<(), Error> {
            let rows = thread_state.into_iter().collect();
            let _ = global.send_rpc(ThreadHistogram(rows)).await;
            Ok(())
        }
    }
```

From here the path is the rest of the book, in order: stage 4
replaces the toy fold with counter2's exactly-once accounting,
and stage 5 stops changing the tool at all — the same binary is
run across backends and the *results* are compared, which is
where a Reverie tool is actually finished.

## Summary and next steps

You wrote a two-half tool, counted with RPC, and let the backend own
the guest's lifecycle. [The Tool API](tool-api.md) covers the full
handler surface; [State at three scales](state.md) grows this counter
into `counter2`, where the counting itself is distributed.
