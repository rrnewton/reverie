use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::GlobalTool;
use reverie::Guest;
use reverie::Stack;
use reverie::Tool;
use reverie_preload::trap::raw_syscall6;

pub(crate) const ENTRY: u64 = 1;
pub(crate) const RETURN: u64 = 2;
pub(crate) const HEAP: u64 = 4;
pub(crate) const SCRATCH_ENTER: u64 = 8;
pub(crate) const SCRATCH_DROP: u64 = 16;
pub(crate) const TOOL: u64 = 32;
pub(crate) const STATES: u64 = 64;
pub(crate) const STACK_COMMIT: u64 = 128;
pub(crate) const RPC: u64 = 256;
const LOG: u64 = 512;
const ALL: u64 = 1023;

#[test]
fn preload_handler_origin_uses_interrupted_domain_not_signal_mask() {
    assert_eq!(
        unsafe { super::interrupted_runtime_in_clocked_preload_handler() },
        None
    );
    with_clocked_preload(|| {
        assert_eq!(
            unsafe { super::interrupted_runtime_in_clocked_preload_handler() },
            Some(false)
        );
        with_clocked_preload(|| {
            assert_eq!(
                unsafe { super::interrupted_runtime_in_clocked_preload_handler() },
                Some(true)
            );
        });
        assert_eq!(
            unsafe { super::interrupted_runtime_in_clocked_preload_handler() },
            Some(false)
        );
    });
    {
        let _runtime = super::Entry::enter();
        assert_eq!(
            unsafe { super::interrupted_runtime_in_clocked_preload_handler() },
            None
        );
        with_clocked_preload(|| {
            assert_eq!(
                unsafe { super::interrupted_runtime_in_clocked_preload_handler() },
                Some(true)
            );
        });
    }
    assert!(!super::allocation_active());
}

pub(crate) fn with_clocked_preload<Result>(body: impl FnOnce() -> Result) -> Result {
    struct Scope(u64);
    impl Drop for Scope {
        fn drop(&mut self) {
            unsafe {
                (super::PRELOAD_HOOKS.leave)();
                crate::clock_control::reverie_liteinst_clock_leave(self.0, 0, 0);
            }
        }
    }
    assert!(!crate::clock_control::active());
    let token = unsafe { crate::clock_control::reverie_liteinst_clock_enter(0) };
    assert_eq!(token, 0);
    unsafe { (super::PRELOAD_HOOKS.enter)() };
    let _scope = Scope(token);
    body()
}

static TARGET_TID: AtomicU64 = AtomicU64::new(0);
static TARGET_PID: AtomicU64 = AtomicU64::new(0);
static SITE: AtomicU64 = AtomicU64::new(0);
static SEEN: AtomicU64 = AtomicU64::new(0);
static SIGNALS: AtomicU64 = AtomicU64::new(0);
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static RPCS: AtomicU64 = AtomicU64::new(0);
static LOGS: AtomicU64 = AtomicU64::new(0);
static WORK: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    fn reverie_liteinst_domain_phase() -> u64;
}

pub(crate) fn at(site: u64) {
    let target = TARGET_TID.load(Ordering::Relaxed);
    if target == 0 || unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as u64 != target {
        return;
    }
    SITE.store(site, Ordering::Relaxed);
    let result = unsafe {
        raw_syscall6(
            libc::SYS_tgkill,
            [
                TARGET_PID.load(Ordering::Relaxed),
                target,
                libc::SIGUSR2 as u64,
                0,
                0,
                0,
            ],
        )
    };
    if result != 0 {
        unsafe { raw_syscall6(libc::SYS_exit_group, [111, 0, 0, 0, 0, 0]) };
    }
}

unsafe extern "C" fn notification(_signal: i32) {
    let phase = unsafe { reverie_liteinst_domain_phase() };
    if phase != 1 && phase != 2 {
        unsafe { raw_syscall6(libc::SYS_exit_group, [112, 0, 0, 0, 0, 0]) };
        return;
    }
    SEEN.fetch_or(SITE.load(Ordering::Relaxed), Ordering::Relaxed);
    SIGNALS.fetch_add(1, Ordering::Relaxed);
}

struct CountedWriter;

impl Write for CountedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        at(LOG);
        LOGS.fetch_add(1, Ordering::Relaxed);
        std::io::stderr().write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct DomainGlobal;

#[reverie::global_tool]
impl GlobalTool for DomainGlobal {
    type Request = u64;
    type Response = u64;
    type Config = ();

    async fn receive_rpc(&self, _from: reverie::Tid, request: u64) -> u64 {
        RPCS.fetch_add(1, Ordering::Relaxed);
        request + 1
    }
}

#[derive(Default)]
pub(crate) struct DomainTool;

async fn callback<G: Guest<DomainTool>>(guest: &mut G) {
    CALLBACKS.fetch_add(1, Ordering::Relaxed);
    let before = guest
        .read_clock()
        .expect("hardware RCB clock required, not skipped");
    let mut stack = guest.stack().await;
    stack.push(1234u64);
    drop(stack.commit().unwrap());
    assert_eq!(
        crate::runtime::domain_test_syscall(libc::SYS_getuid),
        unsafe { raw_syscall6(libc::SYS_getuid, [0; 6]) }
    );
    for work in 0..WORK.load(Ordering::Relaxed) {
        std::hint::black_box(work);
    }
    assert_eq!(guest.send_rpc(41).await, 42);
    tracing::info!(target: "reverie_liteinst::runtime_domain::tests", "runtime-domain actual Tool callback");
    assert_eq!(
        guest.read_clock().unwrap(),
        before,
        "nested runtime work must be excluded exactly once"
    );
}

#[reverie::tool]
impl Tool for DomainTool {
    type GlobalState = DomainGlobal;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _syscall: reverie::syscalls::Syscall,
    ) -> Result<i64, reverie::Error> {
        callback(guest).await;
        Ok(4242)
    }

    async fn handle_rdtsc_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _request: reverie::Rdtsc,
    ) -> Result<reverie::RdtscResult, reverie::Errno> {
        callback(guest).await;
        Ok(reverie::RdtscResult {
            tsc: 4242,
            aux: None,
        })
    }
}

fn child() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rpc.sock");
    let server_path = path.clone();
    let (ready, wait) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let server =
                reverie_rpc_transport::RpcServer::bind(server_path, Arc::new(DomainGlobal), ())
                    .unwrap();
            ready.send(()).unwrap();
            server.serve_one().await.unwrap();
        });
    });
    wait.recv().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(CountedWriter))
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    crate::tool_host::install_domain_test_tool(
        crate::rpc::CoordinatorRpc::<DomainGlobal>::connect(&path).unwrap(),
    );
    crate::runtime::initialize_rcb_clock().unwrap();
    assert!(
        crate::runtime::read_guest_rcb_clock().is_ok(),
        "hardware PMU required"
    );
    unsafe {
        reverie_preload::trap::register_runtime_entry_hooks(&super::PRELOAD_HOOKS).unwrap();
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = notification as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGUSR2, &action, std::ptr::null_mut()),
            0
        );
    }
    TARGET_PID.store(
        unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) } as u64,
        Ordering::Relaxed,
    );
    TARGET_TID.store(
        unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as u64,
        Ordering::Relaxed,
    );
    let mut trajectories = Vec::new();
    for work in [0, 100, 10000] {
        WORK.store(work, Ordering::Relaxed);
        let before = crate::runtime::read_guest_rcb_clock().unwrap();
        assert_eq!(crate::runtime::domain_test_syscall(libc::SYS_getpid), 4242);
        let after_syscall = crate::runtime::read_guest_rcb_clock().unwrap();
        assert_eq!(crate::runtime::domain_test_instruction(), 4242);
        let after_instruction = crate::runtime::read_guest_rcb_clock().unwrap();
        trajectories.push([before, after_syscall, after_instruction]);
    }
    TARGET_TID.store(0, Ordering::Relaxed);
    assert_eq!(SEEN.load(Ordering::Relaxed), ALL);
    assert!(SIGNALS.load(Ordering::Relaxed) >= 10);
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), 6);
    assert_eq!(RPCS.load(Ordering::Relaxed), 6);
    assert_eq!(LOGS.load(Ordering::Relaxed), 6);
    assert_eq!(unsafe { super::reverie_liteinst_domain_depth() }, 0);
    assert_eq!(unsafe { reverie_liteinst_domain_phase() }, 2);
    assert!(
        !super::allocation_active(),
        "Returning must not redirect guest allocation"
    );
    for samples in &trajectories {
        assert!(samples[1] > samples[0] && samples[2] > samples[1]);
    }
    let baseline = [
        trajectories[0][1] - trajectories[0][0],
        trajectories[0][2] - trajectories[0][0],
    ];
    for samples in &trajectories {
        assert_eq!(
            [samples[1] - samples[0], samples[2] - samples[0]],
            baseline,
            "runtime workload changed accounted callback trajectory: {trajectories:?}"
        );
    }
    println!("raw callback trajectories={trajectories:?}");
    println!(
        "actual-sites=10 notifications={} Tool=6 RPC=6 log=6 callback-clock=unchanged return=excluded",
        SIGNALS.load(Ordering::Relaxed)
    );
}

#[test]
fn runtime_locks_defer_notifications() {
    if std::env::var_os("REVERIE_DOMAIN_TEST_CHILD").is_some() {
        child();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime_domain::tests::runtime_locks_defer_notifications",
            "--nocapture",
        ])
        .env("REVERIE_DOMAIN_TEST_CHILD", "1")
        .spawn()
        .unwrap();
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "actual-lock notification child: {status}");
            break;
        }
        if std::time::Instant::now() >= limit {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("actual runtime lock test exceeded 20 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

core::arch::global_asm!(
    ".pushsection .text",
    ".global reverie_liteinst_test_step_enter",
    ".hidden reverie_liteinst_test_step_enter",
    ".type reverie_liteinst_test_step_enter,@function",
    "reverie_liteinst_test_step_enter:",
    "pushfq",
    "or qword ptr [rsp], 0x100",
    "popfq",
    "jmp reverie_liteinst_domain_enter",
    ".size reverie_liteinst_test_step_enter, .-reverie_liteinst_test_step_enter",
    ".popsection",
);

unsafe extern "C" {
    fn reverie_liteinst_test_step_enter();
    fn reverie_liteinst_domain_enter_depth();
}

static ENTRY_STEPS: AtomicU64 = AtomicU64::new(0);
static ENTRY_INTERRUPTS: AtomicU64 = AtomicU64::new(0);
static ENTRY_BEFORE: AtomicU64 = AtomicU64::new(u64::MAX);
static ENTRY_NESTED: AtomicU64 = AtomicU64::new(u64::MAX);
static ENTRY_RELEASED: AtomicU64 = AtomicU64::new(u64::MAX);

fn ownership_snapshot() -> u64 {
    let phase = unsafe { reverie_liteinst_domain_phase() };
    let depth = unsafe { super::reverie_liteinst_domain_depth() };
    phase | (depth << 32)
}

unsafe extern "C" fn interrupt_entry(
    signal: i32,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    let steps = ENTRY_STEPS.fetch_add(1, Ordering::Relaxed);
    if signal != libc::SIGTRAP || unsafe { (*info).si_code } != libc::TRAP_TRACE || steps >= 16 {
        unsafe { raw_syscall6(libc::SYS_exit_group, [113, 0, 0, 0, 0, 0]) };
        return;
    }
    let frame = unsafe { &mut *context.cast::<libc::ucontext_t>() };
    if frame.uc_mcontext.gregs[libc::REG_RIP as usize] as usize
        != reverie_liteinst_domain_enter_depth as *const () as usize
    {
        return;
    }
    frame.uc_mcontext.gregs[libc::REG_EFL as usize] &= !0x100;
    ENTRY_BEFORE.store(ownership_snapshot(), Ordering::Relaxed);
    let nested = super::Entry::enter();
    ENTRY_NESTED.store(ownership_snapshot(), Ordering::Relaxed);
    drop(nested);
    ENTRY_RELEASED.store(ownership_snapshot(), Ordering::Relaxed);
    ENTRY_INTERRUPTS.fetch_add(1, Ordering::Relaxed);
}

fn interrupted_entry_child() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = interrupt_entry as *const () as usize;
    action.sa_flags = libc::SA_SIGINFO;
    assert_eq!(unsafe { libc::sigemptyset(&mut action.sa_mask) }, 0);
    assert_eq!(
        unsafe { libc::sigaction(libc::SIGTRAP, &action, std::ptr::null_mut()) },
        0
    );
    assert_eq!(unsafe { super::reverie_liteinst_domain_depth() }, 0);
    unsafe { reverie_liteinst_test_step_enter() };
    let entered = ownership_snapshot();
    unsafe { super::reverie_liteinst_domain_leave() };
    let returned = ownership_snapshot();
    assert_eq!(ENTRY_INTERRUPTS.load(Ordering::Relaxed), 1);
    assert_eq!(ENTRY_BEFORE.load(Ordering::Relaxed), 1);
    assert_eq!(ENTRY_NESTED.load(Ordering::Relaxed), 1 | (1 << 32));
    assert_eq!(ENTRY_RELEASED.load(Ordering::Relaxed), 2);
    assert_eq!(
        entered,
        1 | (1 << 32),
        "completed interrupted entry must own Runtime at depth one"
    );
    assert_eq!(
        returned, 2,
        "outer leave must publish Returning at depth zero"
    );
    println!(
        "exact-window ownership: before=Runtime/0 nested=Runtime/1 released=Returning/0 outer=Runtime/1 return=Returning/0"
    );
}

#[test]
fn interrupted_entry_retains_runtime_ownership() {
    if std::env::var_os("REVERIE_INTERRUPTED_ENTRY_CHILD").is_some() {
        interrupted_entry_child();
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "runtime_domain::tests::interrupted_entry_retains_runtime_ownership",
            "--nocapture",
        ])
        .env("REVERIE_INTERRUPTED_ENTRY_CHILD", "1")
        .spawn()
        .unwrap();
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "interrupted-entry child: {status}");
            break;
        }
        if std::time::Instant::now() >= limit {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("interrupted-entry test exceeded 20 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
