/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * Licensed under the BSD-style license in the repository root LICENSE.
 */
//! Absolute first-callback controls, separate from the acquisition vectors.
//! Every command starts a new supervised process with no warmup or baseline
//! subtraction. The COW command requires a second, actual supervisor event.
use core::arch::global_asm;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_preload::trap::raw_syscall6;

const BRANCHES: u64 = 4096;
const CHILD_BRANCHES: u64 = 1 + BRANCHES;
const REQUESTED_DEADLINE: u64 = 8192;
const SIGNAL_BRANCHES: u64 = 64;
const SA_RESTORER: u64 = 0x0400_0000;
static ROOT_EXPECTED: AtomicU64 = AtomicU64::new(0);
static FORK: AtomicBool = AtomicBool::new(false);
static SIGNAL_PROBE: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static SITE: AtomicUsize = AtomicUsize::new(0);
static SETUP_BRANCHES: AtomicU64 = AtomicU64::new(0);
static DROPPED_BRANCHES: AtomicU64 = AtomicU64::new(0);
static ROOT_CALLBACKS: AtomicU64 = AtomicU64::new(0);
#[unsafe(export_name = "rcb_initial_affinity_result")]
static AFFINITY_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);
static PARENT_RCB_FD: AtomicU64 = AtomicU64::new(u64::MAX);
static PARENT_RCB_EVENT_ID: AtomicU64 = AtomicU64::new(0);
static PARENT_RCB_OWNER: AtomicU64 = AtomicU64::new(0);
static PARENT_RCB_CLOCK: AtomicU64 = AtomicU64::new(u64::MAX);
#[unsafe(export_name = "rcb_initial_signal_entries")]
static SIGNAL_ENTRIES: AtomicU64 = AtomicU64::new(0);
#[unsafe(export_name = "rcb_initial_signal_mode")]
static SIGNAL_MODE: AtomicU64 = AtomicU64::new(u64::MAX);
#[unsafe(export_name = "rcb_initial_signal_held")]
static SIGNAL_HELD: AtomicU64 = AtomicU64::new(u64::MAX);
#[unsafe(export_name = "rcb_initial_signal_stack_pointer")]
static SIGNAL_STACK_POINTER: AtomicU64 = AtomicU64::new(0);
static SIGNAL_ALT_STACK_START: AtomicU64 = AtomicU64::new(0);
static SIGNAL_ALT_STACK_END: AtomicU64 = AtomicU64::new(0);
static SIGNAL_ALT_STACK_EXPECTED: AtomicBool = AtomicBool::new(false);
#[unsafe(export_name = "rcb_initial_root_entry_signal_entries")]
static ROOT_ENTRY_SIGNAL_ENTRIES: AtomicU64 = AtomicU64::new(0);
#[unsafe(export_name = "rcb_initial_window_signal_entries")]
static WINDOW_SIGNAL_ENTRIES: AtomicU64 = AtomicU64::new(0);
#[unsafe(export_name = "rcb_initial_window_signal_mode")]
static WINDOW_SIGNAL_MODE: AtomicU64 = AtomicU64::new(u64::MAX);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KernelAction {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}

const _: () = assert!(core::mem::size_of::<KernelAction>() == 32);

#[derive(Clone, Copy)]
pub(super) enum Probe {
    Zero,
    Branches,
    Fork,
    Signal,
}
impl Probe {
    fn name(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::Branches => "4096",
            Self::Fork => "fork",
            Self::Signal => "signal",
        }
    }
}

// Installed sites remain registered until exit. This private mapping must
// remain live for the process lifetime, including the inherited child copy.
struct Site {
    address: usize,
    installed: bool,
}
impl Site {
    fn new(installed: bool) -> Self {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page >= 4096);
        let page = page as usize;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let bytes: &[u8] = if installed {
            &[0x0f, 0x05, 0x90, 0x90, 0x90, 0xc3]
        } else {
            &[0x0f, 0x05, 0xc3]
        };
        let offset = if installed { 64 } else { page - bytes.len() };
        let address = unsafe { mapping.cast::<u8>().add(offset) };
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), address, bytes.len()) };
        assert_eq!(
            unsafe { libc::mprotect(mapping, page, libc::PROT_READ | libc::PROT_EXEC) },
            0
        );
        Self {
            address: address as usize,
            installed,
        }
    }

    fn check(&self, child: bool) -> (u64, u64) {
        // The root first entry installs the eligible site through one trap.
        // Child reconstruction resets site counters and records its inherited
        // fork continuation once; the following getpid is its second entry.
        let expected = match (self.installed, child) {
            (true, false) => (1, 1),
            (false, false) => (0, 1),
            (true, true) => (2, 0),
            (false, true) => (0, 2),
        };
        let actual = (
            reverie_liteinst::reverie_liteinst_site_hook_count(self.address as u64),
            reverie_liteinst::reverie_liteinst_site_trap_count(self.address as u64),
        );
        assert_eq!(actual, expected, "actual first callback site entries");
        if self.installed {
            assert_ne!(
                unsafe { std::slice::from_raw_parts(self.address as *const u8, 5) },
                [0x0f, 0x05, 0x90, 0x90, 0x90]
            );
        } else {
            assert_eq!(
                unsafe { std::slice::from_raw_parts(self.address as *const u8, 3) },
                [0x0f, 0x05, 0xc3]
            );
        }
        actual
    }
}

#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct ThreadState {
    child: bool,
    started: bool,
    post_exec: bool,
    callbacks: u64,
    requested_deadline: u64,
}

#[derive(Default)]
struct InitialTool;

fn perf_mapping_ranges() -> Vec<(u64, u64)> {
    std::fs::read_to_string("/proc/self/maps")
        .unwrap()
        .lines()
        .filter(|line| line.contains("anon_inode:[perf_event]"))
        .map(|line| {
            let mut fields = line.split_whitespace();
            let range = fields.next().unwrap();
            assert_eq!(fields.next(), Some("r--s"));
            assert_eq!(fields.next(), Some("00000000"));
            let (start, end) = range.split_once('-').unwrap();
            (
                u64::from_str_radix(start, 16).unwrap(),
                u64::from_str_radix(end, 16).unwrap(),
            )
        })
        .collect()
}

#[reverie::tool]
impl Tool for InitialTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ThreadState;

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::getpid, Sysno::fork].into_iter().collect()
    }

    fn init_thread_state(
        &self,
        _child: Tid,
        parent: Option<(Tid, &Self::ThreadState)>,
    ) -> Self::ThreadState {
        if let Some((_, state)) = parent {
            assert!(FORK.load(Ordering::Acquire));
            assert!(!state.child && state.started && state.post_exec);
            assert_eq!(state.callbacks, 1, "fork snapshots the first root callback");
            assert_eq!(state.requested_deadline, REQUESTED_DEADLINE);
        }
        ThreadState {
            child: parent.is_some(),
            ..ThreadState::default()
        }
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        let before = guest.read_clock()?;
        assert_eq!(before, 0, "actual initial root or child clock");
        let state = *guest.thread_state();
        assert!(!state.started && !state.post_exec);
        assert_eq!(state.callbacks, 0);
        if !state.child {
            assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
        }
        assert_eq!(SETUP_BRANCHES.load(Ordering::Acquire), BRANCHES);
        assert_eq!(DROPPED_BRANCHES.load(Ordering::Acquire), BRANCHES);
        // This additional real callback loop must remain excluded too.
        unsafe { rcb_initial_setup_branches() };
        let (total, senders) = guest.send_rpc(1).await;
        let expected_rpc = if state.child { (2, 2) } else { (1, 1) };
        assert_eq!((total, senders), expected_rpc, "actual lifecycle RPC");
        // LiteInst currently accepts this request without PMU timer delivery.
        // Keep the requested value explicit; this is not a delivery assertion.
        guest.set_timer_precise(TimerSchedule::Rcbs(REQUESTED_DEADLINE))?;
        guest.thread_state_mut().requested_deadline = REQUESTED_DEADLINE;
        guest.thread_state_mut().started = true;
        assert_eq!(guest.read_clock()?, 0, "lifecycle work remains excluded");
        let pid = guest.tid().as_raw();
        if state.child {
            let proof = reverie_liteinst::inherited_perf_mapping_probe_for_test().unwrap();
            assert!(proof[0] != 0 && proof[1] - proof[0] == 4096);
            assert_eq!(proof[2], libc::ENOMEM as u64);
            assert_eq!(proof[3], libc::EFAULT as u64);
            assert_eq!(proof[4], 0);
            println!(
                "rcb initial start: role=child pid={pid} clock=0 post-setup={BRANCHES} owned-drop={BRANCHES} timer-request-rcbs={REQUESTED_DEADLINE} rpc={total} senders={senders} inherited-perf-map=absent mincore=ENOMEM access=EFAULT maps=absent"
            );
        } else {
            println!(
                "rcb initial start: role=root pid={pid} clock=0 post-setup={BRANCHES} owned-drop={BRANCHES} timer-request-rcbs={REQUESTED_DEADLINE} rpc={total} senders={senders}"
            );
        }
        Ok(())
    }

    async fn handle_post_exec<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Errno> {
        let state = *guest.thread_state();
        assert!(!state.child && state.started && !state.post_exec);
        assert_eq!(state.callbacks, 0);
        assert_eq!(guest.read_clock().expect("required post-exec clock"), 0);
        if FORK.load(Ordering::Acquire) {
            let mappings = perf_mapping_ranges();
            assert_eq!(
                mappings.len(),
                1,
                "one private perf metadata mapping: {mappings:?}",
            );
            let (start, end) = mappings[0];
            assert_eq!(end - start, 4096);
            let snapshot = reverie_liteinst::private_rcb_snapshot_for_test().unwrap();
            assert_eq!(snapshot[2], guest.tid().as_raw() as u64);
            assert_eq!(snapshot[3], 0);
            PARENT_RCB_FD.store(snapshot[0], Ordering::Release);
            PARENT_RCB_EVENT_ID.store(snapshot[1], Ordering::Release);
            PARENT_RCB_OWNER.store(snapshot[2], Ordering::Release);
            PARENT_RCB_CLOCK.store(snapshot[3], Ordering::Release);
            reverie_liteinst::arm_inherited_perf_mapping_probe_for_test(start, end).unwrap();
        }
        guest.thread_state_mut().post_exec = true;
        let pid = guest.tid().as_raw();
        println!("rcb initial post-exec: role=root pid={pid} clock=0");
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let clock = guest.read_clock()?;
        let state = *guest.thread_state();
        assert!(state.started);
        assert_eq!(state.post_exec, !state.child);
        assert_eq!(state.callbacks, 0, "this must be the first ordinary callback");
        assert_eq!(state.requested_deadline, REQUESTED_DEADLINE);
        let expected_clock = if state.child {
            CHILD_BRANCHES
        } else {
            ROOT_EXPECTED.load(Ordering::Acquire)
        };
        assert_eq!(clock, expected_clock, "absolute clock with no warmup");
        if !state.child && SIGNAL_PROBE.load(Ordering::Acquire) {
            assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 1);
            assert_eq!(
                reverie_liteinst::async_sigsys_probe_for_test(),
                [1, 2],
                "asynchronous runtime-owned SIGSYS entered only after physical pause",
            );
            assert_eq!(SIGNAL_MODE.load(Ordering::Acquire), 1, "signal observed RUNNING");
            assert_eq!(SIGNAL_HELD.load(Ordering::Acquire), 0, "signal observed held=0");
            let stack_pointer = SIGNAL_STACK_POINTER.load(Ordering::Acquire);
            assert_ne!(stack_pointer, 0, "signal handler captured its entry stack");
            let alt_start = SIGNAL_ALT_STACK_START.load(Ordering::Acquire);
            let alt_end = SIGNAL_ALT_STACK_END.load(Ordering::Acquire);
            if SIGNAL_ALT_STACK_EXPECTED.load(Ordering::Acquire) {
                assert!(
                    alt_start <= stack_pointer && stack_pointer < alt_end,
                    "SA_ONSTACK handler entry {stack_pointer:#x} outside {alt_start:#x}..{alt_end:#x}",
                );
            } else {
                assert_eq!((alt_start, alt_end), (0, 0), "alternate stack must be disabled");
            }
            // The callback assembly has already blocked asynchronous signals.
            // This second edge signal must stay pending through Rust finish and
            // physical ENABLE, then observe RUNNING only from final mask restore.
            let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
            assert_eq!(pid, i64::from(guest.tid().as_raw()));
            assert_eq!(
                unsafe {
                    raw_syscall6(
                        libc::SYS_tgkill,
                        [
                            pid as u64,
                            pid as u64,
                            libc::SIGUSR1 as u64,
                            0,
                            0,
                            0,
                        ],
                    )
                },
                0
            );
            assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 1);
            let mut pending = 0_u64;
            assert_eq!(
                unsafe {
                    raw_syscall6(
                        libc::SYS_rt_sigpending,
                        [(&raw mut pending) as u64, 8, 0, 0, 0, 0],
                    )
                },
                0
            );
            assert_ne!(pending & (1_u64 << (libc::SIGUSR1 - 1)), 0);
            let window_bit = 1_u64 << (libc::SIGWINCH - 1);
            assert_ne!(
                pending & window_bit,
                0,
                "the supervisor's RUNNING signal is physically held",
            );
            assert_eq!(WINDOW_SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
            let before_window_delivery = guest.read_clock()?;
            assert_eq!(before_window_delivery, clock);
            assert_eq!(
                unsafe {
                    raw_syscall6(
                        libc::SYS_rt_sigprocmask,
                        [libc::SIG_UNBLOCK as u64, (&raw const window_bit) as u64, 0, 8, 0, 0],
                    )
                },
                0,
                "deliver the held signal only while the callback event is paused",
            );
            assert_eq!(WINDOW_SIGNAL_ENTRIES.load(Ordering::Acquire), 1);
            assert_eq!(WINDOW_SIGNAL_MODE.load(Ordering::Acquire), 2, "window signal observed PAUSED");
            assert_eq!(guest.read_clock()?, before_window_delivery);
            assert_eq!(
                unsafe {
                    raw_syscall6(
                        libc::SYS_rt_sigprocmask,
                        [libc::SIG_BLOCK as u64, (&raw const window_bit) as u64, 0, 8, 0, 0],
                    )
                },
                0,
            );
            let affinity = Syscall::from_raw(
                Sysno::sched_setaffinity,
                SyscallArgs::new(0, 8, 0, 0, 0, 0),
            );
            assert_eq!(
                guest.inject(affinity).await.unwrap_err(),
                Errno::EPERM,
                "Tool injection cannot change the counter target CPU",
            );
            assert_eq!(guest.read_clock()?, before_window_delivery);
        } else if !state.child {
            assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
            assert_eq!(SIGNAL_MODE.load(Ordering::Acquire), u64::MAX);
            assert_eq!(SIGNAL_HELD.load(Ordering::Acquire), u64::MAX);
            assert_eq!(SIGNAL_STACK_POINTER.load(Ordering::Acquire), 0);
        }
        let expected_syscall = if !state.child && FORK.load(Ordering::Acquire) {
            Sysno::fork
        } else {
            Sysno::getpid
        };
        assert_eq!(syscall.number(), expected_syscall);
        let site = Site {
            address: SITE.load(Ordering::Acquire),
            installed: INSTALLED.load(Ordering::Acquire),
        };
        let (hooks, traps) = site.check(state.child);
        guest.thread_state_mut().callbacks = 1;
        if !state.child {
            assert_eq!(ROOT_CALLBACKS.fetch_add(1, Ordering::AcqRel), 0);
        }
        let role = if state.child { "child" } else { "root" };
        let pid = guest.tid().as_raw();
        let number = expected_syscall as usize;
        println!(
            "rcb initial callback: role={role} pid={pid} ordinal=0 clock={clock} syscall={number} timer-request-rcbs={REQUESTED_DEADLINE} hooks={hooks} traps={traps}"
        );
        // This is the real fork injection. In the child the runtime abandons
        // this copied future, imports its fresh supervisor event and starts
        // fresh Tool state before resuming the saved assembly continuation.
        let result = guest.inject(syscall).await?;
        if expected_syscall == Sysno::getpid {
            assert_eq!(result, i64::from(pid));
        }
        Ok(result)
    }
}

struct OwnedSetup;
impl Drop for OwnedSetup {
    fn drop(&mut self) {
        unsafe { rcb_initial_setup_branches() };
        DROPPED_BRANCHES.store(BRANCHES, Ordering::Release);
    }
}

struct SetupArgs<'a> {
    path: &'a Path,
    previous_action: KernelAction,
    previous_root_entry_action: KernelAction,
    previous_window_action: KernelAction,
    running_probe: [u64; 9],
    queue_signal: bool,
    use_alt_stack: bool,
}

fn root_entry_action() -> KernelAction {
    KernelAction {
        handler: rcb_initial_root_entry_signal_handler as *const () as u64,
        flags: SA_RESTORER,
        restorer: rcb_initial_signal_restorer as *const () as u64,
        mask: u64::MAX,
    }
}

fn install_root_entry_signal(args: &mut SetupArgs<'_>) {
    let action = root_entry_action();
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    libc::SIGUSR2 as u64,
                    (&raw const action) as u64,
                    (&raw mut args.previous_root_entry_action) as u64,
                    8,
                    0,
                    0,
                ],
            )
        },
        0,
        "install the preexisting root-entry signal action"
    );
    reverie_liteinst::arm_root_entry_signal_probe_for_test(libc::SIGUSR2).unwrap();
}

fn prepare_boundary_signal(args: &mut SetupArgs<'_>) {
    assert_eq!(
        ROOT_ENTRY_SIGNAL_ENTRIES.load(Ordering::Acquire),
        0,
        "the first root Rust operation must run with asynchronous handlers blocked"
    );
    let mut root_pending = 0_u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigpending,
                [(&raw mut root_pending) as u64, 8, 0, 0, 0, 0],
            )
        },
        0
    );
    assert_ne!(root_pending & (1_u64 << (libc::SIGUSR2 - 1)), 0);
    let root_action = root_entry_action();
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    libc::SIGUSR2 as u64,
                    (&raw const root_action) as u64,
                    0,
                    8,
                    0,
                    0,
                ],
            )
        },
        0,
        "restore the preexisting action before root mask release"
    );
    let mut alt_stack: libc::stack_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_sigaltstack,
                [0, (&raw mut alt_stack) as u64, 0, 0, 0, 0],
            )
        },
        0,
        "query the runtime alternate signal stack",
    );
    if args.use_alt_stack {
        assert_eq!(alt_stack.ss_flags & libc::SS_DISABLE, 0);
        let start = alt_stack.ss_sp as usize as u64;
        let end = start
            .checked_add(alt_stack.ss_size as u64)
            .expect("alternate signal stack range");
        assert!(start != 0 && start < end);
        SIGNAL_ALT_STACK_START.store(start, Ordering::Release);
        SIGNAL_ALT_STACK_END.store(end, Ordering::Release);
    } else {
        assert_ne!(alt_stack.ss_flags & libc::SS_DISABLE, 0);
        SIGNAL_ALT_STACK_START.store(0, Ordering::Release);
        SIGNAL_ALT_STACK_END.store(0, Ordering::Release);
    }
    SIGNAL_ALT_STACK_EXPECTED.store(args.use_alt_stack, Ordering::Release);
    let action = KernelAction {
        handler: rcb_initial_signal_handler as *const () as u64,
        flags: SA_RESTORER
            | if args.use_alt_stack {
                libc::SA_ONSTACK as u64
            } else {
                0
            },
        restorer: rcb_initial_signal_restorer as *const () as u64,
        mask: u64::MAX,
    };
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    libc::SIGUSR1 as u64,
                    (&raw const action) as u64,
                    (&raw mut args.previous_action) as u64,
                    8,
                    0,
                    0,
                ],
            )
        },
        0,
        "install isolated root-boundary signal action"
    );
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    assert!(pid > 0 && tid > 0);
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_tgkill,
                [pid as u64, tid as u64, libc::SIGUSR1 as u64, 0, 0, 0],
            )
        },
        0,
        "queue the thread-directed boundary signal"
    );
    let mut pending = 0_u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigpending,
                [(&raw mut pending) as u64, 8, 0, 0, 0, 0],
            )
        },
        0
    );
    assert_ne!(pending & (1_u64 << (libc::SIGUSR1 - 1)), 0);
    assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
    let window_action = KernelAction {
        handler: rcb_initial_window_signal_handler as *const () as u64,
        flags: SA_RESTORER,
        restorer: rcb_initial_signal_restorer as *const () as u64,
        mask: u64::MAX,
    };
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigaction,
                [
                    libc::SIGWINCH as u64,
                    (&raw const window_action) as u64,
                    (&raw mut args.previous_window_action) as u64,
                    8,
                    0,
                    0,
                ],
            )
        },
        0,
        "install the pre-mask window signal action",
    );
    reverie_liteinst::arm_async_sigsys_probe_for_test().unwrap();
    let (fd, packet) = reverie_liteinst::running_signal_probe_for_test().unwrap();
    args.running_probe[0] = fd as u64;
    args.running_probe[1..].copy_from_slice(&packet);
}

unsafe extern "C" fn setup(argument: *mut libc::c_void) {
    let args = unsafe { &mut *argument.cast::<SetupArgs<'_>>() };
    let _owned = OwnedSetup;
    unsafe { reverie_liteinst::install_tool::<InitialTool>(args.path) }.unwrap();
    // Root assembly owns the original/restore masks. The preexisting SIGUSR2
    // queued by the first Rust operation and this SIGUSR1 can first enter only
    // from the assembly-owned final SIG_SETMASK.
    if args.queue_signal {
        prepare_boundary_signal(args);
    }
    // Both this loop after the Result check and the owned Drop must finish
    // before root lifecycle and the physical event enable. Neither is warmup.
    unsafe { rcb_initial_setup_branches() };
    SETUP_BRANCHES.store(BRANCHES, Ordering::Release);
}

fn pin_current_thread() -> usize {
    let mut permitted: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            libc::sched_getaffinity(0, std::mem::size_of_val(&permitted), &raw mut permitted)
        },
        0
    );
    let cpu = (0..libc::CPU_SETSIZE as usize)
        .find(|&cpu| unsafe { libc::CPU_ISSET(cpu, &permitted) })
        .expect("at least one permitted CPU");
    let mut chosen: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_SET(cpu, &mut chosen) };
    assert_eq!(
        unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&chosen), &chosen) },
        0
    );
    assert_eq!(unsafe { libc::sched_getcpu() }, cpu as i32);
    cpu
}

fn record_boundary(name: &str, pid: i32, (address, count): (u64, usize)) {
    use std::fmt::Write;
    assert!((1..=16384).contains(&count));
    let end = address.checked_add(count as u64).unwrap();
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    assert!(
        maps.lines().any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let (start, last) = fields[0].split_once('-').unwrap();
            let start = u64::from_str_radix(start, 16).unwrap();
            let last = u64::from_str_radix(last, 16).unwrap();
            start <= address
                && end <= last
                && fields[1].starts_with('r')
                && fields[1].as_bytes()[2] == b'x'
        }),
        "complete live readable executable range"
    );
    // Every range comes from loaded assembly symbols after all absolute samples
    // were checked. Reading constructor or signal-boundary bytes does not invoke it.
    let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, count) };
    let mut hex = String::new();
    for byte in bytes {
        write!(hex, "{byte:02x}").unwrap();
    }
    println!("rcb {name} pid={pid} address={address:#x} bytes={count} code={hex}");
}

fn signal_handler_boundary() -> (u64, usize) {
    let start = rcb_initial_signal_handler as *const () as usize;
    let end = core::ptr::addr_of!(rcb_initial_signal_handler_end) as usize;
    (start as u64, end.checked_sub(start).expect("signal handler end"))
}

fn signal_restorer_boundary() -> (u64, usize) {
    let start = rcb_initial_signal_restorer as *const () as usize;
    let end = core::ptr::addr_of!(rcb_initial_signal_restorer_end) as usize;
    (start as u64, end.checked_sub(start).expect("signal restorer end"))
}

fn window_signal_handler_boundary() -> (u64, usize) {
    let start = rcb_initial_window_signal_handler as *const () as usize;
    let end = core::ptr::addr_of!(rcb_initial_window_signal_handler_end) as usize;
    (start as u64, end.checked_sub(start).expect("window signal handler end"))
}

pub(super) fn run(path: &Path, installed: bool, probe: Probe) {
    let use_alt_stack = reverie_liteinst::alt_stack_from_env_value(
        std::env::var_os(reverie_liteinst::ALT_STACK_ENV).as_deref(),
    )
    .expect("valid test-selected alternate-stack mode");
    let cpu = pin_current_thread();
    let site = Site::new(installed);
    let pid = unsafe { libc::getpid() };
    assert!(pid > 0);
    assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
    assert_eq!(SIGNAL_MODE.load(Ordering::Acquire), u64::MAX);
    assert_eq!(SIGNAL_HELD.load(Ordering::Acquire), u64::MAX);
    assert_eq!(SIGNAL_STACK_POINTER.load(Ordering::Acquire), 0);
    assert_eq!(ROOT_ENTRY_SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
    let mut original_mask = 0_u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    0,
                    (&raw mut original_mask) as u64,
                    8,
                    0,
                    0,
                ],
            )
        },
        0
    );
    unsafe { reverie_liteinst::with_tool_root!({}); }
    let mut unselected_restore = 0_u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    0,
                    (&raw mut unselected_restore) as u64,
                    8,
                    0,
                    0,
                ],
            )
        },
        0
    );
    assert_eq!(unselected_restore, original_mask);
    assert!(perf_mapping_ranges().is_empty());
    let mut pending = 0_u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigpending,
                [(&raw mut pending) as u64, 8, 0, 0, 0, 0],
            )
        },
        0
    );
    let signal_probe = matches!(probe, Probe::Signal);
    let signal_bits =
        (1_u64 << (libc::SIGUSR1 - 1)) | (1_u64 << (libc::SIGUSR2 - 1));
    let window_bit = 1_u64 << (libc::SIGWINCH - 1);
    assert_eq!(
        pending & (signal_bits | window_bit),
        0,
        "isolated process starts without pending probe signals",
    );
    if signal_probe {
        let root_mask = (original_mask & !signal_bits) | window_bit;
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const root_mask) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
    }
    let first = match probe {
        Probe::Zero | Probe::Fork => 0,
        Probe::Branches => BRANCHES,
        Probe::Signal => SIGNAL_BRANCHES,
    };
    ROOT_EXPECTED.store(first, Ordering::Release);
    FORK.store(matches!(probe, Probe::Fork), Ordering::Release);
    SIGNAL_PROBE.store(signal_probe, Ordering::Release);
    INSTALLED.store(installed, Ordering::Release);
    SITE.store(site.address, Ordering::Release);
    let mut args = SetupArgs {
        path,
        previous_action: KernelAction::default(),
        previous_root_entry_action: KernelAction::default(),
        previous_window_action: KernelAction::default(),
        running_probe: [0; 9],
        queue_signal: signal_probe,
        use_alt_stack,
    };
    if signal_probe {
        install_root_entry_signal(&mut args);
    }
    let argument = (&raw mut args).cast::<libc::c_void>();
    // Mode dispatch occurs before entering the assembly activation wrapper.
    // All returns below happen after the first absolute sample was checked.
    let result = unsafe {
        match probe {
            Probe::Zero => rcb_initial_zero(setup, argument, site.address),
            Probe::Branches => rcb_initial_branches(setup, argument, site.address),
            Probe::Fork => rcb_initial_fork(setup, argument, site.address),
            Probe::Signal => rcb_initial_signal(
                setup,
                argument,
                site.address,
                args.running_probe.as_mut_ptr(),
            ),
        }
    };
    assert_eq!(ROOT_CALLBACKS.load(Ordering::Acquire), 1);
    if signal_probe {
        assert_eq!(
            AFFINITY_RESULT.load(Ordering::Acquire),
            -i64::from(libc::EPERM),
            "an in-guest affinity change must be refused before Linux can migrate the target",
        );
        assert_eq!(unsafe { libc::sched_getcpu() }, cpu as i32);
    }
    if signal_probe {
        assert_eq!(ROOT_ENTRY_SIGNAL_ENTRIES.load(Ordering::Acquire), 1);
        assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 2);
        assert_eq!(SIGNAL_MODE.load(Ordering::Acquire), 1);
        assert_eq!(SIGNAL_HELD.load(Ordering::Acquire), 0);
        assert_eq!(WINDOW_SIGNAL_ENTRIES.load(Ordering::Acquire), 1);
        assert_eq!(WINDOW_SIGNAL_MODE.load(Ordering::Acquire), 2);
        let signal_stack_pointer = SIGNAL_STACK_POINTER.load(Ordering::Acquire);
        assert_ne!(signal_stack_pointer, 0);
        let alt_start = SIGNAL_ALT_STACK_START.load(Ordering::Acquire);
        let alt_end = SIGNAL_ALT_STACK_END.load(Ordering::Acquire);
        if use_alt_stack {
            assert!(alt_start <= signal_stack_pointer && signal_stack_pointer < alt_end);
        } else {
            assert_eq!((alt_start, alt_end), (0, 0));
        }
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [
                        libc::SIGUSR1 as u64,
                        (&raw const args.previous_action) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [
                        libc::SIGWINCH as u64,
                        (&raw const args.previous_window_action) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0,
        );
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigaction,
                    [
                        libc::SIGUSR2 as u64,
                        (&raw const args.previous_root_entry_action) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        let mut current_mask = 0_u64;
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        0,
                        (&raw mut current_mask) as u64,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        // Restore only the test-owned bits. Runtime installation owns its
        // deliberate unblocking of SIGSYS/SIGSEGV and must remain usable.
        let owned_signal_bits = signal_bits | window_bit;
        let restored_signal_mask =
            (current_mask & !owned_signal_bits) | (original_mask & owned_signal_bits);
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const restored_signal_mask) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
    } else {
        assert_eq!(ROOT_ENTRY_SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
        assert_eq!(SIGNAL_ENTRIES.load(Ordering::Acquire), 0);
        assert_eq!(SIGNAL_MODE.load(Ordering::Acquire), u64::MAX);
        assert_eq!(SIGNAL_HELD.load(Ordering::Acquire), u64::MAX);
        assert_eq!(SIGNAL_STACK_POINTER.load(Ordering::Acquire), 0);
    }
    site.check(false);
    let child = if matches!(probe, Probe::Fork) {
        assert!(result > 0, "real parent fork result");
        let child = i32::try_from(result).unwrap();
        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(child, &raw mut status, 0) };
            if waited == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            assert_eq!(waited, child, "wait for the real COW child");
            break;
        }
        assert!(libc::WIFEXITED(status), "child wait status={status}");
        assert_eq!(libc::WEXITSTATUS(status), 0, "child wait status={status}");
        let parent = reverie_liteinst::private_rcb_snapshot_for_test().unwrap();
        assert_eq!(parent[0], PARENT_RCB_FD.load(Ordering::Acquire));
        assert_eq!(parent[1], PARENT_RCB_EVENT_ID.load(Ordering::Acquire));
        assert_eq!(parent[2], PARENT_RCB_OWNER.load(Ordering::Acquire));
        assert!(parent[3] > PARENT_RCB_CLOCK.load(Ordering::Acquire));
        child
    } else {
        assert_eq!(result, i64::from(pid));
        0
    };
    record_boundary("root-activation", pid, reverie_liteinst::root_activation_boundary());
    record_boundary("root-constructor", pid, reverie_liteinst::root_constructor_boundary());
    record_boundary("root-signal-handler", pid, signal_handler_boundary());
    record_boundary("entry-window-signal-handler", pid, window_signal_handler_boundary());
    record_boundary("instruction-signal-entry", pid, reverie_liteinst::instruction_signal_boundary());
    record_boundary("root-signal-restorer", pid, signal_restorer_boundary());
    let kind = if installed { "installed" } else { "fallback" };
    let probe = probe.name();
    let signal_stack_pointer = SIGNAL_STACK_POINTER.load(Ordering::Acquire);
    let signal_stack = if signal_probe {
        if use_alt_stack { "alt" } else { "ordinary" }
    } else {
        "unobserved"
    };
    let signal_stack_pointer = if signal_probe {
        format!("{signal_stack_pointer:#x}")
    } else {
        "unobserved".to_owned()
    };
    let signal_alt_stack = if !signal_probe {
        "unobserved".to_owned()
    } else if use_alt_stack {
        format!(
            "{:#x}-{:#x}",
            SIGNAL_ALT_STACK_START.load(Ordering::Acquire),
            SIGNAL_ALT_STACK_END.load(Ordering::Acquire),
        )
    } else {
        "disabled".to_owned()
    };
    println!(
        "rcb initial: path={kind} probe={probe} actual-pid={pid} first={first} setup={BRANCHES} drop={BRANCHES} unselected-mask=exact root-entry-signal={} signal={} signal-mode={} signal-held={} signal-stack={signal_stack} signal-sp={signal_stack_pointer} signal-alt={signal_alt_stack} callback-edge-signals={} pre-mask-signal={} fork-parent-event={} callbacks=1 cpu={cpu} child-pid={child}",
        if signal_probe { "1" } else { "0" },
        if signal_probe { SIGNAL_BRANCHES } else { 0 },
        if signal_probe { "1" } else { "unobserved" },
        if signal_probe { "0" } else { "unobserved" },
        if signal_probe { "2" } else { "0" },
        if signal_probe { "held-delivered-paused" } else { "unobserved" },
        if matches!(probe, "fork") {
            "continuous"
        } else {
            "unobserved"
        },
    );
}

type Setup = unsafe extern "C" fn(*mut libc::c_void);
unsafe extern "C" {
    fn rcb_initial_zero(setup: Setup, argument: *mut libc::c_void, site: usize) -> i64;
    fn rcb_initial_branches(setup: Setup, argument: *mut libc::c_void, site: usize) -> i64;
    fn rcb_initial_fork(setup: Setup, argument: *mut libc::c_void, site: usize) -> i64;
    fn rcb_initial_signal(
        setup: Setup,
        argument: *mut libc::c_void,
        site: usize,
        running_probe: *mut u64,
    ) -> i64;
    fn rcb_initial_setup_branches();
    fn rcb_initial_signal_handler();
    fn rcb_initial_root_entry_signal_handler();
    fn rcb_initial_window_signal_handler();
    fn rcb_initial_signal_restorer();
    static rcb_initial_signal_handler_end: u8;
    static rcb_initial_window_signal_handler_end: u8;
    static rcb_initial_signal_restorer_end: u8;
}
global_asm!(
    include_str!("rcb_initial.S"),
    root_activation = sym reverie_liteinst::__root_activation,
    signal_entries = sym SIGNAL_ENTRIES,
    signal_mode = sym SIGNAL_MODE,
    signal_held = sym SIGNAL_HELD,
    signal_stack_pointer = sym SIGNAL_STACK_POINTER,
    root_entry_signal_entries = sym ROOT_ENTRY_SIGNAL_ENTRIES,
    window_signal_entries = sym WINDOW_SIGNAL_ENTRIES,
    window_signal_mode = sym WINDOW_SIGNAL_MODE,
    affinity_result = sym AFFINITY_RESULT,
    sendto = const libc::SYS_sendto,
    recvfrom = const libc::SYS_recvfrom,
    sched_setaffinity = const libc::SYS_sched_setaffinity,
    mode_offset = const 16,
    held_offset = const 24,
    signal_branches = const SIGNAL_BRANCHES,
);
