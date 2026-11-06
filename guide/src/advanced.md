# Advanced handlers

Beyond syscalls, four handler families cover most sophisticated
tools. Each is subscription-gated and each has backend-dependent
reach; this chapter says what they are for and where the edges are.

## Signals

`handle_signal_event` (with raw and structured variants, and a
dedicated dequeue hook) lets a tool observe, rewrite, delay, or
suppress signal delivery. The hard part is ownership: `SIGTRAP` and
`SIGSEGV` are load-bearing for several mechanisms (guarded patching,
page gates, breakpoints), and a guest's own handlers compete for the
same dispositions. Backends state their signal-ownership rules
explicitly; a tool that installs guest-visible handlers must be
tested against guests that install their own. The in-guest LiteInst
path currently *rejects* guest-installed callable signal handlers —
an honest boundary, and the kind this guide will never quietly drop.

## Timers

`handle_timer_event` delivers tool-scheduled timers, the basis for
sampling profilers and watchdogs. Timer *precision* is
backend-dependent in ways that matter (a ptrace stop is not a
hardware interrupt); tools that need precise preemption should read
the performance chapter's honesty box before designing around timers.

## CPUID and RDTSC

On x86-64, `handle_cpuid_event` and `handle_rdtsc_event` intercept the
instructions determinism and fingerprinting tools care about most.
Mechanisms differ — CPUID faulting, `PR_SET_TSC`, patching, trapping —
with different availability per environment, which is why these are
subscriptions with refusals attached rather than ambient features.
aarch64 lacks both today.

## Injection beyond the current syscall

Handlers may inject *additional* syscalls (open a log file in the
guest, map a scratch page) and must then reason about what the guest
observes: register clobbers, errno visibility, restart semantics.
Clone/fork-family injection is refused by some backends outright.
Keep injections minimal, restore what you borrowed, and test against
a guest that inspects its own register state — the examples' guest
fixtures are good templates.

## The four families, in code

Both listings below continue that cookbook file (same imports
and runner). Two families already have full listings in
[The tool API surface](tool-api.md#cookbook-four-handlers-beyond-syscalls):
timer sampling (arm post-exec, precise RCB schedule) and CPUID
spoofing (leaf-1 only, for the glibc reason given there). The two
remaining families are below; all four are proven by the same test
file shape (6 tests, ptrace backend).

### Signal rewriting

Suppression (cookbook recipe 1) returns `None`; rewriting returns
a *different* signal. The guest traps `SIGUSR2`, sends itself
`SIGUSR1`, and sees only the rewritten delivery:

```rust,ignore
// ---- Recipe 5: signal rewriting (deliver a different signal) -------
#[derive(Debug, Default, Clone)]
pub struct SigGlobal {
    rewritten: Arc<AtomicU64>,
}
impl SigGlobal {
    pub fn rewritten(&self) -> u64 {
        self.rewritten.load(Ordering::SeqCst)
    }
}

#[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct SigMsg;

#[reverie::global_tool]
impl GlobalTool for SigGlobal {
    type Request = SigMsg;
    type Response = ();
    type Config = ();

    async fn init_global_state(_: &()) -> Self {
        Self::default()
    }
    async fn receive_rpc(&self, _from: Pid, _msg: SigMsg) {
        self.rewritten.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
pub struct RewriteSignal;

#[reverie::tool]
impl Tool for RewriteSignal {
    type GlobalState = SigGlobal;
    type ThreadState = ();

    async fn handle_signal_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        signal: Signal,
    ) -> Result<Option<Signal>, Errno> {
        if signal == Signal::SIGUSR1 {
            let _ = guest.send_rpc(SigMsg).await;
            Ok(Some(Signal::SIGUSR2))
        } else {
            Ok(Some(signal))
        }
    }
}
```

### Scratch-map injection

One extra syscall the guest never issued: map a private anonymous
page, write a marker, read it back, report — then let the guest's
own syscall proceed with its registers untouched. The injected
`mmap` result is consumed by the tool, never observed by the guest
as its own return value; that separation is the whole discipline
of injection.

```rust,ignore
// ---- Recipe 6: scratch-map injection -------------------------------
#[derive(Debug, Default, Clone)]
pub struct ScratchGlobal {
    mapped: Arc<AtomicU64>,
}
impl ScratchGlobal {
    pub fn mapped(&self) -> u64 {
        self.mapped.load(Ordering::SeqCst)
    }
}

#[derive(PartialEq, Debug, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct ScratchMsg(pub u64);

#[reverie::global_tool]
impl GlobalTool for ScratchGlobal {
    type Request = ScratchMsg;
    type Response = ();
    type Config = ();

    async fn init_global_state(_: &()) -> Self {
        Self::default()
    }
    async fn receive_rpc(&self, _from: Pid, msg: ScratchMsg) {
        self.mapped.store(msg.0, Ordering::SeqCst);
    }
}

#[derive(Debug, Default)]
pub struct ScratchMapper {
    done: AtomicU64,
}

#[reverie::tool]
impl Tool for ScratchMapper {
    type GlobalState = ScratchGlobal;
    type ThreadState = ();

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: reverie::syscalls::Syscall,
    ) -> Result<i64, Error> {
        if self.done.swap(1, Ordering::SeqCst) == 0 {
            // One extra syscall the guest never issued: map a
            // private anonymous scratch page, mark it, read the
            // mark back, and only then let the guest's own
            // syscall proceed with its registers untouched.
            let addr = guest
                .inject(
                    Mmap::new()
                        .with_len(4096)
                        .with_prot(ProtFlags::PROT_READ | ProtFlags::PROT_WRITE)
                        .with_flags(MapFlags::MAP_PRIVATE | MapFlags::MAP_ANONYMOUS)
                        .with_fd(-1),
                )
                .await?;
            let marker = b"scratch-ok";
            let mut memory = guest.memory();
            let target = AddrMut::from_ptr(addr as *const u8).expect("mmap result");
            memory.write_exact(target, marker).map_err(Error::from)?;
            let mut back = [0u8; 10];
            memory
                .read_exact(target.to_owned(), &mut back)
                .map_err(Error::from)?;
            assert_eq!(&back, marker);
            let _ = guest.send_rpc(ScratchMsg(addr as u64)).await;
        }
        guest.tail_inject(syscall).await
    }
}

pub fn _unused(_: AU64) {}
```

Both are proven against the ptrace backend:

```rust,ignore
#[test]
fn recipe_signal_rewrite() {
    use typed_scratch::advanced::RewriteSignal;
    let script = "trap 'echo caught2' USR2; trap 'echo caught1' USR1; kill -USR1 $$; echo after";
    let (stdout, global) = run_typed::<RewriteSignal>((), "/bin/sh", &["-c", script]);
    assert_eq!(stdout, b"caught2\nafter\n");
    assert_eq!(global.rewritten(), 1);
}

#[test]
fn recipe_scratch_map_injection() {
    use typed_scratch::advanced::ScratchMapper;
    let (stdout, global) = run_typed::<ScratchMapper>((), "/bin/echo", &["hello"]);
    assert_eq!(stdout, b"hello\n", "injection must not disturb the guest");
    assert!(global.mapped() > 0, "scratch page was mapped and reported");
}
```

Backend support, stated narrowly: signal rewriting and injection
are `Guest`-trait surface, so every backend that drives the trait
accepts the code; whether a given backend *delivers* signals and
injected syscalls with ptrace's fidelity is per-backend (see the
lifecycle matrix in [Safety](safety.md)). CPUID events are
x86-64-only and subscription-gated everywhere; timers are only as
precise as the backend's clock (ptrace's RCB timers are the
reference the cookbook measures against).
