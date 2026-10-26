//! Real opt-in selector preflight with a default-order control.

use std::io;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_inguest::guest::runtime_seam::PatcherRef;
use reverie_inguest::guest::tool_install::PatchHost;
use reverie_inguest::guest::tool_install::TrapHost;
use reverie_inguest::guest::tool_install::TrapOptions;
use reverie_inguest::guest::tool_install::TrapRuntime;
use reverie_inguest::lifecycle::RuntimeConfig;
use reverie_inguest::stats::GuestStatsHooks;

static CALLS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct ProbeTool;

#[reverie::tool]
impl Tool for ProbeTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ();

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        assert_eq!(syscall.number(), Sysno::getpid);
        CALLS.fetch_add(1, Ordering::Relaxed);
        guest.inject(syscall).await
    }
}

#[derive(Clone, Copy)]
enum Mode {
    DefaultOrder,
    Accept,
    Refuse,
}

struct ObservedHost {
    host: TrapHost,
    observation: std::fs::File,
    mode: Mode,
}

impl ObservedHost {
    fn record(&self, phase: &str) -> io::Result<()> {
        writeln!(
            &self.observation,
            "{phase} gates={} filters={:?} dispatcher={} callbacks={}",
            reverie_inguest::trap::fixed_gates_published(),
            // SAFETY: this fixture owns the exclusive startup boundary.
            unsafe { reverie_inguest::guest::sigalrm::seccomp_filter_count() },
            reverie_inguest::trap::has_dispatcher(),
            CALLS.load(Ordering::Relaxed),
        )
    }
}

// SAFETY: TrapHost supplies its unchanged no-patcher contract. Observations
// do not execute guest code or install a filter; all startup is single-threaded.
unsafe impl PatchHost for ObservedHost {
    type Runtime = TrapRuntime;
    fn patcher(&self) -> &'static PatcherRef {
        self.host.patcher()
    }
    fn preflight(&self) -> io::Result<()> {
        self.record("preflight")?;
        self.host.preflight()
    }
    fn preflight_site_patching(&self) -> Option<io::Result<bool>> {
        match self.mode {
            Mode::DefaultOrder => None,
            Mode::Accept | Mode::Refuse => Some((|| {
                self.record("selector-preflight")?;
                match self.mode {
                    Mode::Accept => Ok(false),
                    Mode::Refuse => Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "observed selector preflight refusal",
                    )),
                    Mode::DefaultOrder => unreachable!(),
                }
            })()),
        }
    }
    fn initialize_continuation(&self) -> io::Result<()> {
        self.record("continuation")?;
        self.host.initialize_continuation()
    }
    fn prepare_entries(&self) -> io::Result<()> {
        self.record("entries")?;
        self.host.prepare_entries()
    }
    fn site_patching(&self) -> io::Result<bool> {
        assert!(
            matches!(self.mode, Mode::DefaultOrder),
            "preflight selector called twice"
        );
        self.record("selector")?;
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "observed late selector refusal",
        ))
    }
    fn stage_stream(&self) -> io::Result<bool> {
        self.host.stage_stream()
    }
    fn process_forks_allowed(&self) -> io::Result<bool> {
        self.host.process_forks_allowed()
    }
    fn sigalrm_handlers(&self) -> io::Result<bool> {
        self.host.sigalrm_handlers()
    }
    fn runtime_config(&self) -> io::Result<RuntimeConfig> {
        self.host.runtime_config()
    }
    fn runtime(&self, stats: GuestStatsHooks) -> Self::Runtime {
        self.host.runtime(stats)
    }
}

pub fn run(path: &Path, observation: &Path, mode: &str) {
    let mode = match mode {
        "default" => Mode::DefaultOrder,
        "accept" => Mode::Accept,
        "refuse" => Mode::Refuse,
        _ => panic!("unexpected selector mode"),
    };
    let host = ObservedHost {
        host: TrapHost::new(TrapOptions::default()),
        observation: std::fs::File::create_new(observation).unwrap(),
        mode,
    };
    let result = unsafe {
        reverie_inguest::guest::tool_install::install_tool_with::<ProbeTool, _>(path, false, host)
    };
    match mode {
        Mode::DefaultOrder => panic!("a failed published selector resumed the caller: {result:?}"),
        Mode::Accept => {
            result.unwrap();
            assert!(reverie_inguest::trap::fixed_gates_published());
            assert!(reverie_inguest::trap::has_dispatcher());
            assert_eq!(CALLS.load(Ordering::Relaxed), 0);
            assert_eq!(unsafe { libc::syscall(libc::SYS_getpid) }, unsafe {
                reverie_inguest::trap::raw_syscall6(libc::SYS_getpid, [0; 6])
            });
            assert_eq!(CALLS.load(Ordering::Relaxed), 1);
            println!("selector preflight accepted: once gates=published Tool-callbacks=1");
        }
        Mode::Refuse => {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert_eq!(error.to_string(), "observed selector preflight refusal");
            assert!(!reverie_inguest::trap::fixed_gates_published());
            assert!(!reverie_inguest::trap::has_dispatcher());
            assert_eq!(
                unsafe { reverie_inguest::guest::sigalrm::seccomp_filter_count() },
                Some(0)
            );
            assert_eq!(CALLS.load(Ordering::Relaxed), 0);
            let page = 0x5000_0000_0000u64;
            assert_eq!(
                unsafe {
                    reverie_inguest::trap::raw_syscall6(
                        libc::SYS_mmap,
                        [
                            page,
                            4096,
                            (libc::PROT_READ | libc::PROT_WRITE) as u64,
                            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE)
                                as u64,
                            u64::MAX,
                            0,
                        ],
                    )
                },
                page as i64
            );
            assert_eq!(
                unsafe {
                    reverie_inguest::trap::raw_syscall6(libc::SYS_munmap, [page, 4096, 0, 0, 0, 0])
                },
                0
            );
            println!(
                "selector preflight refused: once original-error gates=absent dispatcher=absent callbacks=0"
            );
        }
    }
}
