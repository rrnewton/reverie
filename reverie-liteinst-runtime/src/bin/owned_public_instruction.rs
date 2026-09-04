//! Public default-feature regression for owned CPUID/RDTSC instruction events.
//!
//! This is a test consumer of the exported public API, not a production
//! installation route. It calls `install_tool_owned_native_from_bootstrap` with
//! default features only: no private feature, no private installer, no private
//! observation export and no forged clock readiness. Evidence comes from the
//! Tool's own callbacks and coordinator RPC and from the guest's own results
//! array, exactly as the qualified private native fixture obtains it.
//!
//! The `begin -> initializer -> finish -> guest` sequence is one assembly
//! bracket in `owned_public_instruction/entry.s`. No compiler-generated Rust
//! runs between the physical enable and guest entry, and the guest closes
//! through `reverie_liteinst_clock_enter` before any verification runs.
//!
//! # The linked constructor, stated accurately
//!
//! An earlier revision of this comment claimed this binary has "no loader or
//! preload constructor of its own". That was **false**. Built with default
//! features, it links the `preload-constructor` `.init_array` entry
//! `REVERIE_LITEINST_INIT` (`src/lib.rs`), which runs
//! `reverie_liteinst_initialize` before `main` and calls
//! `runtime::initialize_from_environment`. That function selects the host runtime
//! when `REVERIE_LITEINST_HOST_RUNTIME` is `1`, or a tool / shared built-in when
//! `REVERIE_LITEINST_TOOL` is set, and **otherwise returns `Ok(())` without
//! installing any runtime, signal, seccomp or SUD state**.
//!
//! So the fixture does not rely on the absence of a constructor. It relies on that
//! audited unselected early return, and [`selectors`] makes the reliance checkable:
//! the parent refuses a conflicting environment immediately before spawning **each**
//! role and retains a role-labelled decision. The check below in `main` is a late
//! backstop only — by the time `main` runs, a selected constructor has already
//! executed, so it can report but not prevent.
//!
//! The binding is not taken on trust from this comment. A host control reads the
//! linked ELF: `.init_array` must contain the `REVERIE_LITEINST_INIT` slot, and the
//! relative relocation applied to that slot must install the address of
//! `reverie_liteinst_initialize`. The unselected early-return branch is retained
//! from the pinned `runtime.rs` source alongside it.
//!
//! # Environment, and why nothing is sanitized
//!
//! `cargo test` injects `LD_LIBRARY_PATH` into a test process. That variable is
//! **refused, never removed**: silently unsetting it would hide exactly the
//! interposition this fixture must exclude. A future native attempt therefore must
//! not run through `cargo test`. It must invoke the frozen prebuilt harness binary
//! directly, from an environment that is already clean, and any conflicting
//! incoming variable is refused at the spawn boundary rather than repaired.
//!
//! The remaining unsafe caller duties are discharged by construction and remain the
//! caller's, unchanged: a freshly execed, single-threaded, dynamically linked
//! process, no POSIX timer created since that exec, default `SIGSEGV`/`SIGBUS`
//! dispositions, stable RX mappings, TLS, xstate and native controls, owned signal
//! dispositions and masks, no asynchronous callback or additional thread, and a
//! finite Tool with synchronous RPC fitting the 512 KiB runtime stack, with a guest
//! region that is pure assembly.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::CpuIdResult;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_preload::trap::raw_syscall6;

#[path = "owned_public_instruction/selectors.rs"]
mod selectors;

#[path = "owned_public_instruction/verifier.rs"]
mod verifier;

use verifier::BRANCHES;
use verifier::KIND_CPUID;
use verifier::KIND_GETPID;
use verifier::KIND_RDTSC;
use verifier::KIND_RDTSCP;
use verifier::KIND_READ;
use verifier::RDTSC_ECX_SEED;
use verifier::READ_BYTES;
use verifier::raw_write;

const EVIDENCE_BYTES: usize = 8 * 1024 * 1024;

static STARTS: AtomicU64 = AtomicU64::new(0);
static EVENTS: AtomicU64 = AtomicU64::new(0);
static INJECTIONS: AtomicU64 = AtomicU64::new(0);
static CLOCKS: [AtomicU64; 8] = [const { AtomicU64::new(u64::MAX) }; 8];
static KINDS: [AtomicU64; 8] = [const { AtomicU64::new(u64::MAX) }; 8];
static PIPE_FD: AtomicU64 = AtomicU64::new(0);
static INVENTORY: AtomicU64 = AtomicU64::new(u64::MAX);
static mut BUFFER: [u8; READ_BYTES] = [0xcc; READ_BYTES];
static mut RESULTS: [u64; 15] = [0; 15];

struct Probe {
    socket: PathBuf,
    pid: i64,
}

unsafe extern "C" {
    fn public_entry(probe: *mut libc::c_void) -> !;
    fn public_first();
}

core::arch::global_asm!(
    include_str!("owned_public_instruction/entry.s"),
    clock_begin = sym reverie_liteinst_runtime::__clock_constructor_begin,
    clock_finish = sym reverie_liteinst_runtime::__clock_constructor_finish,
    initialize = sym initialize,
    verify = sym verify,
    results = sym RESULTS,
    buffer = sym BUFFER,
    pipe_fd = sym PIPE_FD,
    branches = const BRANCHES,
    read_bytes = const READ_BYTES,
    rdtsc_ecx_seed = const RDTSC_ECX_SEED,
);

#[derive(Default)]
struct Coordinator(AtomicU64);

#[reverie::global_tool]
impl GlobalTool for Coordinator {
    type Request = u64;
    type Response = u64;
    type Config = ();
    async fn receive_rpc(&self, _: reverie::Tid, request: u64) -> u64 {
        assert_eq!(self.0.fetch_add(1, Ordering::Relaxed) + 1, request);
        request
    }
}

#[derive(Default)]
struct PublicTool;

async fn sample<G: Guest<PublicTool>>(guest: &mut G, kind: u64) {
    assert_eq!(
        STARTS.load(Ordering::Relaxed),
        1,
        "handle_thread_start must precede every event"
    );
    let ordinal = EVENTS.fetch_add(1, Ordering::Relaxed);
    assert!(ordinal < CLOCKS.len() as u64);
    assert_eq!(*guest.thread_state(), ordinal);
    *guest.thread_state_mut() += 1;
    let clock = guest.read_clock().unwrap();
    CLOCKS[ordinal as usize].store(clock, Ordering::Relaxed);
    KINDS[ordinal as usize].store(kind, Ordering::Relaxed);
    let total = guest.send_rpc(ordinal + 1).await;
    assert_eq!(total, ordinal + 1);
}

#[reverie::tool]
impl Tool for PublicTool {
    type GlobalState = Coordinator;
    type ThreadState = u64;

    fn subscriptions(_: &()) -> Subscription {
        let mut subscriptions: Subscription = [Sysno::getpid, Sysno::read].into_iter().collect();
        subscriptions.cpuid();
        subscriptions.rdtsc();
        subscriptions
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        assert_eq!(STARTS.fetch_add(1, Ordering::Relaxed), 0);
        assert_eq!(EVENTS.load(Ordering::Relaxed), 0);
        assert_eq!(*guest.thread_state(), 0);
        assert_eq!(guest.read_clock()?, 0);
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let ordinal = INJECTIONS.fetch_add(1, Ordering::Relaxed);
        assert!(ordinal < 3);
        let read = ordinal == 1;
        assert_eq!(
            syscall.number(),
            if read { Sysno::read } else { Sysno::getpid }
        );
        let registers = guest.regs().await;
        assert_eq!(registers.rcx, registers.rip + 2);
        assert_eq!(registers.r11, registers.eflags);
        assert_eq!(registers.eflags & 0x100, 0);
        if ordinal == 0 {
            assert_eq!(registers.rip, public_first as *const () as u64);
        }
        sample(guest, if read { KIND_READ } else { KIND_GETPID }).await;
        let result = guest.inject(syscall).await?;
        assert_eq!(guest.regs().await, registers);
        if read {
            assert_eq!(result, READ_BYTES as i64);
        }
        Ok(result)
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuIdResult, reverie::Errno> {
        assert_eq!((eax, ecx), (0xfeed, 0xbaad));
        sample(guest, KIND_CPUID).await;
        Ok(CpuIdResult {
            eax: eax ^ ecx,
            ebx: 0x2222_2222,
            ecx: 0x3333_3333,
            edx: 0x4444_4444,
        })
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Rdtsc,
    ) -> Result<RdtscResult, reverie::Errno> {
        let tscp = request == Rdtsc::Tscp;
        sample(guest, if tscp { KIND_RDTSCP } else { KIND_RDTSC }).await;
        Ok(RdtscResult {
            tsc: 0xfedc_ba98_0000_0000 | if tscp { 4 } else { 3 },
            aux: tscp.then_some(0xaaaa_5555),
        })
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(args.len() >= 3);
    if let Some(report) = selectors::report(&selectors::refusals(selectors::current_names())) {
        panic!("{report}");
    }
    if args[1] == "server" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        runtime.block_on(async {
            let server = reverie_rpc_transport::RpcServer::bind(
                std::path::Path::new(&args[2]),
                Arc::new(Coordinator::default()),
                (),
            )
            .unwrap();
            server.serve().await.unwrap();
        });
        return;
    }
    assert_eq!(args[1], "guest");
    assert_eq!(args.len(), 3);
    assert_eq!(
        std::fs::symlink_metadata("/etc/ld.so.preload")
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        assert_ne!(
            unsafe { libc::signal(signal, libc::SIG_DFL) },
            libc::SIG_ERR
        );
    }
    let mut descriptors = [-1; 2];
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    let block = [0x5au8; READ_BYTES];
    assert_eq!(
        unsafe { libc::write(descriptors[1], block.as_ptr().cast(), block.len()) },
        READ_BYTES as isize
    );
    PIPE_FD.store(descriptors[0] as u64, Ordering::Relaxed);
    let mut probe = Probe {
        socket: (&args[2]).into(),
        pid: unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) },
    };
    unsafe { public_entry((&raw mut probe).cast()) }
}

unsafe extern "C" fn initialize(probe: *mut Probe) -> i32 {
    let probe = unsafe { &mut *probe };
    match unsafe {
        reverie_liteinst_runtime::install_tool_owned_native_from_bootstrap::<PublicTool>(
            &probe.socket,
            EVIDENCE_BYTES,
        )
    } {
        Ok(inventory) => {
            INVENTORY.store(
                match inventory {
                    reverie_liteinst_runtime::PosixTimerInventory::Empty => 0,
                    reverie_liteinst_runtime::PosixTimerInventory::Unavailable => 1,
                },
                Ordering::Relaxed,
            );
            1
        }
        Err(error) => {
            raw_write(2, b"owned-public: install refused: ");
            raw_write(2, error.to_string().as_bytes());
            raw_write(2, b"\n");
            42
        }
    }
}

/// Captures the observation, checks it purely, and terminates.
///
/// Nothing here allocates or panics while SUD is armed, and nothing issues an
/// ordinary syscall: both records go through the trusted raw gate. A failing check
/// is terminal and names itself with its actual and expected values, instead of
/// unwinding into the panic machinery, a reentrant Tool injection and the
/// deadline. A failed diagnostic write is reported, never treated as written.
unsafe extern "C" fn verify(probe: *const Probe) -> ! {
    let probe = unsafe { &*probe };
    let mut observation = verifier::Observation {
        starts: STARTS.load(Ordering::Relaxed),
        events: EVENTS.load(Ordering::Relaxed),
        injections: INJECTIONS.load(Ordering::Relaxed),
        kinds: [0; 8],
        clocks: [0; 8],
        results: unsafe { RESULTS },
        buffer: unsafe { BUFFER },
        stats: [0; 4],
        inventory: INVENTORY.load(Ordering::Relaxed),
        pid: probe.pid as u64,
    };
    for (slot, source) in observation.kinds.iter_mut().zip(KINDS.iter()) {
        *slot = source.load(Ordering::Relaxed);
    }
    for (slot, source) in observation.clocks.iter_mut().zip(CLOCKS.iter()) {
        *slot = source.load(Ordering::Relaxed);
    }
    let stats = reverie_liteinst_runtime::syscall_mode_stats();
    observation.stats = [
        stats.planning_attempts,
        stats.patch_attempts,
        stats.installed_patches,
        stats.vdso_rewrite_attempts,
    ];

    verifier::terminate_on(&observation, 1, 2)
}
