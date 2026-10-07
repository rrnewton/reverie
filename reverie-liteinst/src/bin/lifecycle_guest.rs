use core::arch::global_asm;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie::Errno;
use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;

const FAST_CALLS: u64 = 8;
const RPC_GETPID: u64 = 1;
const RPC_CLOCK_GETTIME: u64 = 2;
const RPC_GETTIMEOFDAY: u64 = 3;
const RPC_FORK: u64 = 4;
const RPC_CLOSE_RANGE: u64 = 5;
const RPC_BLOCKING: u64 = 6;
/// Parks the sending process: the coordinator never answers it.
const RPC_PARK: u64 = 7;
/// Makes the coordinator's tool report a backend failure, once a process is
/// parked.
const RPC_FAIL: u64 = 8;
/// Set by the `backend-failure` mode: the next getppid callback sends
/// `RPC_PARK`, and the next getpid callback sends `RPC_FAIL`.
static PARK_ON_GETPPID: AtomicBool = AtomicBool::new(false);
static FAIL_ON_GETPID: AtomicBool = AtomicBool::new(false);
/// Set by the `blocking-global-rpc` mode: the next getpid callback probes
/// `blocking_global_rpc` from inside the callback, outside its own send_rpc.
static PROBE_BLOCKING_RPC: AtomicBool = AtomicBool::new(false);
/// The probe's outcome: 1 when both calls behaved as required.
static BLOCKING_RPC_OK: AtomicU64 = AtomicU64::new(0);
/// Set before `install_tool` by the mode that checks a Tool sees the guest's
/// descriptor-closing calls; the Tool then subscribes to `close` and
/// `close_range` too.
static SUBSCRIBE_CLOSES: AtomicBool = AtomicBool::new(false);
/// Tags the one RPC a process sends at `exit_group` with its count of Tool
/// callbacks, so the host can compare the backend's dispatch counts with the
/// callbacks the Tool actually made.
const RPC_CALLBACK_COUNT: u64 = 1 << 32;
static FORCE_WAIT_RESTART: AtomicBool = AtomicBool::new(true);
static READ_CALLS: AtomicUsize = AtomicUsize::new(0);
/// This process's Tool callbacks, one per guest entry: a callback that asks
/// for a restart is re-run for the same entry and is not counted again.
/// Counted in memory, because a counting syscall would itself be trapped and
/// change what is being measured. The fixture's processes are single-threaded.
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
/// Makes the next `getpid` callback call the `getppid` stub, a Tool syscall
/// that reaches a site an earlier guest call patched.
static NESTED_GETPPID: AtomicBool = AtomicBool::new(false);
const TIMER_ACCEPTED: i64 = 0;
const TIMER_NOT_ERRNO: i64 = -1;
const TIMER_NOT_PROBED: i64 = -2;
static PROBE_TIMERS: AtomicBool = AtomicBool::new(false);
static SET_TIMER_OUTCOME: AtomicI64 = AtomicI64::new(TIMER_NOT_PROBED);
static SET_TIMER_PRECISE_OUTCOME: AtomicI64 = AtomicI64::new(TIMER_NOT_PROBED);

global_asm!(
    r#"
    .text
    .p2align 4
    .global reverie_liteinst_lifecycle_getpid
    .hidden reverie_liteinst_lifecycle_getpid
    .type reverie_liteinst_lifecycle_getpid,@function
reverie_liteinst_lifecycle_getpid:
    .cfi_startproc
    mov eax, 39
    .global reverie_liteinst_lifecycle_getpid_site
    .hidden reverie_liteinst_lifecycle_getpid_site
reverie_liteinst_lifecycle_getpid_site:
    syscall
    nop
    nop
    nop
    ret
    .cfi_endproc
    .size reverie_liteinst_lifecycle_getpid, .-reverie_liteinst_lifecycle_getpid

    .p2align 4
    .global reverie_liteinst_lifecycle_read
    .hidden reverie_liteinst_lifecycle_read
    .type reverie_liteinst_lifecycle_read,@function
reverie_liteinst_lifecycle_read:
    .cfi_startproc
    xor eax, eax
    mov edi, -1
    xor esi, esi
    xor edx, edx
    syscall
    nop
    nop
    nop
    ret
    .cfi_endproc
    .size reverie_liteinst_lifecycle_read, .-reverie_liteinst_lifecycle_read

    .p2align 4
    .global reverie_liteinst_lifecycle_getppid
    .hidden reverie_liteinst_lifecycle_getppid
    .type reverie_liteinst_lifecycle_getppid,@function
reverie_liteinst_lifecycle_getppid:
    .cfi_startproc
    mov eax, 110
    .global reverie_liteinst_lifecycle_getppid_site
    .hidden reverie_liteinst_lifecycle_getppid_site
reverie_liteinst_lifecycle_getppid_site:
    syscall
    nop
    nop
    nop
    ret
    .cfi_endproc
    .size reverie_liteinst_lifecycle_getppid, .-reverie_liteinst_lifecycle_getppid

    .p2align 4
    .global reverie_liteinst_lifecycle_fork
    .hidden reverie_liteinst_lifecycle_fork
    .type reverie_liteinst_lifecycle_fork,@function
reverie_liteinst_lifecycle_fork:
    .cfi_startproc
    mov eax, 57
    .global reverie_liteinst_lifecycle_fork_site
    .hidden reverie_liteinst_lifecycle_fork_site
reverie_liteinst_lifecycle_fork_site:
    syscall
    nop
    nop
    nop
    ret
    .cfi_endproc
    .size reverie_liteinst_lifecycle_fork, .-reverie_liteinst_lifecycle_fork
"#
);

unsafe extern "C" {
    fn reverie_liteinst_lifecycle_getpid() -> i64;
    fn reverie_liteinst_lifecycle_read() -> i64;
    fn reverie_liteinst_lifecycle_getppid() -> i64;
    fn reverie_liteinst_lifecycle_fork() -> i64;
    static reverie_liteinst_lifecycle_getpid_site: u8;
    static reverie_liteinst_lifecycle_getppid_site: u8;
    static reverie_liteinst_lifecycle_fork_site: u8;
}

#[derive(Default)]
struct LifecycleGlobal {
    total: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for LifecycleGlobal {
    type Request = u64;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, event: u64) {
        self.total.fetch_add(event, Ordering::Relaxed);
    }
}

/// A global state no Tool in this process uses: `blocking_global_rpc` must
/// refuse it.
#[derive(Default)]
struct UnusedGlobal;

#[reverie::global_tool]
impl GlobalTool for UnusedGlobal {
    type Request = u64;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Tid, _event: u64) {}
}

#[derive(Default)]
struct LifecycleTool;

#[reverie::tool]
impl Tool for LifecycleTool {
    type GlobalState = LifecycleGlobal;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [
            Sysno::getpid,
            Sysno::getppid,
            Sysno::clock_gettime,
            Sysno::gettimeofday,
            Sysno::fork,
            Sysno::clone,
            Sysno::wait4,
            Sysno::read,
            Sysno::exit,
            Sysno::exit_group,
        ]
        .into_iter()
        .chain(
            SUBSCRIBE_CLOSES
                .load(Ordering::Relaxed)
                .then_some([Sysno::close, Sysno::close_range])
                .into_iter()
                .flatten(),
        )
        .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
        if syscall.number() == Sysno::getpid && PROBE_BLOCKING_RPC.swap(false, Ordering::Relaxed) {
            probe_blocking_global_rpc();
        }
        if syscall.number() == Sysno::getppid && PARK_ON_GETPPID.swap(false, Ordering::Relaxed) {
            // Never answered: this process stays parked until the run ends.
            guest.send_rpc(RPC_PARK).await;
        }
        if syscall.number() == Sysno::getpid && FAIL_ON_GETPID.swap(false, Ordering::Relaxed) {
            guest.send_rpc(RPC_FAIL).await;
        }
        if syscall.number() == Sysno::wait4 {
            if FORCE_WAIT_RESTART.swap(false, Ordering::Relaxed) {
                return Err(restart_same_entry());
            }
            return Ok(4242);
        }
        if syscall.number() == Sysno::getpid && PROBE_TIMERS.swap(false, Ordering::Relaxed) {
            let outcome = timer_outcome(guest.set_timer(TimerSchedule::Rcbs(1)));
            SET_TIMER_OUTCOME.store(outcome, Ordering::Relaxed);
            let outcome = timer_outcome(guest.set_timer_precise(TimerSchedule::Rcbs(1)));
            SET_TIMER_PRECISE_OUTCOME.store(outcome, Ordering::Relaxed);
        }
        let output = TOOL_OUTPUT.load(Ordering::Relaxed);
        if syscall.number() == Sysno::getppid && output >= 0 {
            // Tool code writes to its reserved descriptor through its own syscall.
            let socket = reverie_liteinst::tool_output_fd().expect("a reserved Tool output socket");
            if FILL_TOOL_OUTPUT.load(Ordering::Relaxed) {
                // Queue a message without waiting, until the socket is full.
                let sent =
                    unsafe { libc::send(socket, b"fill\n".as_ptr().cast(), 5, libc::MSG_DONTWAIT) };
                if sent == 5 {
                    TOOL_OUTPUT_FILLED.fetch_add(1, Ordering::Relaxed);
                } else {
                    let error = std::io::Error::last_os_error();
                    assert_eq!(error.raw_os_error(), Some(libc::EAGAIN), "{error}");
                    FILL_TOOL_OUTPUT.store(false, Ordering::Relaxed);
                }
            } else {
                let written = unsafe { libc::write(socket, b"tool\n".as_ptr().cast(), 5) };
                assert_eq!(written, 5, "{}", std::io::Error::last_os_error());
            }
        }
        if syscall.number() == Sysno::read {
            if READ_CALLS.fetch_add(1, Ordering::Relaxed) < 2 {
                return Err(restart_same_entry());
            }
            return Ok(4243);
        }
        let event = match syscall.number() {
            Sysno::getpid => RPC_GETPID,
            Sysno::clock_gettime => RPC_CLOCK_GETTIME,
            Sysno::gettimeofday => RPC_GETTIMEOFDAY,
            Sysno::fork | Sysno::clone => RPC_FORK,
            Sysno::wait4 => unreachable!("wait4 is handled before event classification"),
            Sysno::close_range => RPC_CLOSE_RANGE,
            Sysno::getppid | Sysno::close | Sysno::exit | Sysno::exit_group => 0,
            number => panic!("unexpected lifecycle fixture syscall {number}"),
        };
        if event != 0 {
            guest.send_rpc(event).await;
        }
        if event == RPC_GETPID && NESTED_GETPPID.swap(false, Ordering::Relaxed) {
            unsafe { reverie_liteinst_lifecycle_getppid() };
        }
        if syscall.number() == Sysno::exit_group {
            guest
                .send_rpc(RPC_CALLBACK_COUNT | CALLBACKS.load(Ordering::Relaxed))
                .await;
        }
        match syscall {
            // The fixture only proves that ptrace syscallized the vDSO call
            // and the in-guest Tool received it. Zeroed outputs are sufficient.
            Syscall::ClockGettime(_) | Syscall::Gettimeofday(_) => Ok(0),
            Syscall::Fork(_) => Ok(inject_fork(guest, syscall).await?),
            Syscall::Clone(clone) if clone.flags().bits() & libc::CLONE_VM == 0 => {
                Ok(inject_fork(guest, syscall).await?)
            }
            _ => Ok(guest.inject(syscall).await?),
        }
    }
}

/// Asks the driver to re-run this callback for the same guest entry, which the
/// re-run counts again.
fn restart_same_entry() -> Error {
    CALLBACKS.fetch_sub(1, Ordering::Relaxed);
    Errno::ERESTARTSYS.into()
}

/// A new process's callback count starts with the one callback it returns
/// from, which the backend also counts as the child's own first event.
async fn inject_fork<G: Guest<LifecycleTool>>(
    guest: &mut G,
    syscall: Syscall,
) -> Result<i64, Error> {
    let result = guest.inject(syscall).await?;
    if result == 0 {
        CALLBACKS.store(1, Ordering::Relaxed);
    }
    Ok(result)
}

/// Encodes a timer request's result so the guest can report it after the hook.
fn timer_outcome(result: Result<(), Error>) -> i64 {
    match result {
        Ok(()) => TIMER_ACCEPTED,
        Err(error) => error
            .into_errno()
            .map_or(TIMER_NOT_ERRNO, |errno| i64::from(errno.into_raw())),
    }
}

fn timer_label(outcome: i64) -> String {
    match outcome {
        TIMER_ACCEPTED => "accepted".to_owned(),
        TIMER_NOT_ERRNO => "non-errno-error".to_owned(),
        TIMER_NOT_PROBED => "not-probed".to_owned(),
        errno if errno == i64::from(libc::ENOSYS) => "ENOSYS".to_owned(),
        errno => format!("errno-{errno}"),
    }
}

/// Bootstrap bytes the lifecycle tests pass to `*_preload_data` launches.
const LIFECYCLE_TOOL_DATA: &[u8] = b"lifecycle";
/// Set by the tests of the `*_preload_data` launchers. It makes the fixture
/// fail unless it received its coordinator through the bootstrap descriptor,
/// so those tests cannot pass through the environment branch.
const EXPECT_BOOTSTRAP_ENV: &str = "REVERIE_LITEINST_LIFECYCLE_EXPECT_BOOTSTRAP";

fn install_tool() {
    if std::env::var_os(EXPECT_BOOTSTRAP_ENV).is_some() {
        let environ = std::fs::read("/proc/self/environ").unwrap();
        let entry = format!("{}=", reverie_liteinst::COORDINATOR_ENV);
        assert!(
            !environ
                .split(|byte| *byte == 0)
                .any(|variable| variable.starts_with(entry.as_bytes())),
            "a bootstrap launch put {} in the initial environment",
            reverie_liteinst::COORDINATOR_ENV
        );
    }
    let coordinator = match std::env::var_os(reverie_liteinst::COORDINATOR_ENV) {
        Some(coordinator) => std::path::PathBuf::from(coordinator),
        // A `*_preload_data` launch passes the coordinator in an inherited
        // bootstrap descriptor instead of the environment.
        None => {
            // SAFETY: main starts before application-created threads.
            let bootstrap = unsafe { reverie_liteinst::take_preload_bootstrap() }
                .unwrap()
                .expect("lifecycle fixture requires a LiteInst coordinator");
            assert_eq!(bootstrap.tool_data, LIFECYCLE_TOOL_DATA);
            bootstrap.coordinator
        }
    };
    // SAFETY: main starts before application-created threads and installs once.
    unsafe { reverie_liteinst::install_tool_quiescent::<LifecycleTool>(coordinator) }.unwrap();
}

fn fork_or_panic() -> libc::pid_t {
    let child = unsafe { libc::fork() };
    assert_ne!(
        child,
        -1,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    child
}

fn root_exits_first(marker: &Path) -> ! {
    if fork_or_panic() == 0 {
        std::thread::sleep(Duration::from_millis(150));
        std::fs::write(marker, b"descendant-finished\n").unwrap();
        unsafe { libc::_exit(0) };
    }
    unsafe { libc::_exit(23) }
}

fn signaled_descendant(pid_file: &Path) -> ! {
    if fork_or_panic() == 0 {
        std::fs::write(pid_file, format!("{}\n", std::process::id())).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        unsafe {
            libc::raise(libc::SIGTERM);
            libc::_exit(127);
        }
    }
    unsafe { libc::_exit(29) }
}

fn fast_path() {
    let mut expected = None;
    for _ in 0..FAST_CALLS {
        let observed = unsafe { reverie_liteinst_lifecycle_getpid() };
        assert_eq!(*expected.get_or_insert(observed), observed);
    }
    let address = core::ptr::addr_of!(reverie_liteinst_lifecycle_getpid_site) as usize as u64;
    let traps = reverie_liteinst::reverie_liteinst_site_trap_count(address);
    let hooks = reverie_liteinst::reverie_liteinst_site_hook_count(address);
    println!("calls={FAST_CALLS} traps={traps} hooks={hooks}");
    assert_eq!(traps, 1);
    assert_eq!(hooks, FAST_CALLS);
}

/// Returns this process's `TracerPid` from procfs; zero means no ptrace attach.
///
/// Reads with `pread`: the fixture Tool answers `read` itself.
fn tracer_pid() -> u64 {
    let fd = unsafe { libc::open(c"/proc/self/status".as_ptr(), libc::O_RDONLY) };
    assert!(fd >= 0, "open: {}", std::io::Error::last_os_error());
    let mut status = vec![0u8; 64 * 1024];
    let mut len = 0;
    loop {
        let read = unsafe {
            libc::pread(
                fd,
                status[len..].as_mut_ptr().cast(),
                status.len() - len,
                len as libc::off_t,
            )
        };
        assert!(read >= 0, "pread: {}", std::io::Error::last_os_error());
        if read == 0 {
            break;
        }
        len += read as usize;
        assert!(len < status.len(), "procfs status filled the buffer");
    }
    assert_eq!(unsafe { libc::close(fd) }, 0);
    std::str::from_utf8(&status[..len])
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("TracerPid:"))
        .expect("procfs status has a TracerPid line")
        .trim()
        .parse()
        .unwrap()
}

fn patching_off() {
    let mut expected = None;
    for _ in 0..FAST_CALLS {
        let observed = unsafe { reverie_liteinst_lifecycle_getpid() };
        assert_eq!(*expected.get_or_insert(observed), observed);
    }
    // The vDSO fast paths must trap too. The fixture Tool answers both calls
    // with 0 and writes nothing, so the sentinels survive only if the Tool,
    // not the kernel or an unpatched vDSO, ran every call.
    assert_ne!(
        unsafe { libc::getauxval(libc::AT_SYSINFO_EHDR) },
        0,
        "the fixture needs a vDSO"
    );
    for _ in 0..FAST_CALLS {
        let mut time = libc::timespec {
            tv_sec: -1,
            tv_nsec: -1,
        };
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) },
            0
        );
        assert_eq!((time.tv_sec, time.tv_nsec), (-1, -1));
        let mut day = libc::timeval {
            tv_sec: -1,
            tv_usec: -1,
        };
        assert_eq!(
            unsafe { libc::gettimeofday(&mut day, core::ptr::null_mut()) },
            0
        );
        assert_eq!((day.tv_sec, day.tv_usec), (-1, -1));
    }
    let address = core::ptr::addr_of!(reverie_liteinst_lifecycle_getpid_site) as usize as u64;
    // Read every counter before procfs and stdout add their own syscalls.
    let traps = reverie_liteinst::reverie_liteinst_site_trap_count(address);
    let hooks = reverie_liteinst::reverie_liteinst_site_hook_count(address);
    let fallback_getpid =
        reverie_liteinst::reverie_liteinst_fallback_syscall_count(libc::SYS_getpid);
    let fallback_clock_gettime =
        reverie_liteinst::reverie_liteinst_fallback_syscall_count(libc::SYS_clock_gettime);
    let fallback_gettimeofday =
        reverie_liteinst::reverie_liteinst_fallback_syscall_count(libc::SYS_gettimeofday);
    let tracer = tracer_pid();
    println!(
        "calls={FAST_CALLS} traps={traps} hooks={hooks} fallback_getpid={fallback_getpid} \
         fallback_clock_gettime={fallback_clock_gettime} \
         fallback_gettimeofday={fallback_gettimeofday} tracer_pid={tracer}"
    );
}

fn timer_refused() {
    PROBE_TIMERS.store(true, Ordering::Relaxed);
    unsafe { reverie_liteinst_lifecycle_getpid() };
    println!(
        "set_timer={} set_timer_precise={}",
        timer_label(SET_TIMER_OUTCOME.load(Ordering::Relaxed)),
        timer_label(SET_TIMER_PRECISE_OUTCOME.load(Ordering::Relaxed)),
    );
}

fn restart_wait4() {
    let waited = unsafe { libc::waitpid(-1, core::ptr::null_mut(), libc::WNOHANG) };
    assert_eq!(waited, 4242, "wait4 callback was not restarted");
    println!("wait4-restart-ok");
}

fn restart_read() {
    let result = unsafe { reverie_liteinst_lifecycle_read() };
    let calls = READ_CALLS.load(Ordering::Relaxed);
    println!("read-result={result} calls={calls}");
    assert_eq!(result, 4243, "read callback was not restarted");
    assert_eq!(calls, 3, "read callback did not restart repeatedly");
}

fn fallback_fork_stats() {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
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
    // mov eax, SYS_fork; syscall; ret, with the syscall at the page end.
    let entry = unsafe { mapping.cast::<u8>().add(page - 8) };
    unsafe {
        std::ptr::copy_nonoverlapping([0xb8, 57, 0, 0, 0, 0x0f, 0x05, 0xc3].as_ptr(), entry, 8)
    };
    assert_eq!(
        unsafe { libc::mprotect(mapping, page, libc::PROT_READ | libc::PROT_EXEC) },
        0
    );
    let site = unsafe { entry.add(5) } as u64;
    let fork: unsafe extern "C" fn() -> i64 = unsafe { std::mem::transmute(entry) };
    install_tool();
    let child = unsafe { fork() };
    assert!(child >= 0, "fork failed: {child}");
    assert_eq!(reverie_liteinst::reverie_liteinst_site_hook_count(site), 0);
    assert_eq!(reverie_liteinst::reverie_liteinst_site_trap_count(site), 1);
    assert_eq!(
        reverie_liteinst::reverie_liteinst_fallback_syscall_count(libc::SYS_fork),
        1
    );
    if child == 0 {
        unsafe { libc::_exit(0) };
    }
    // The wait4 callback deliberately returns a fixture value in other modes.
    // Use the trusted gate here to observe this actual child's completion.
    let mut status = -1i32;
    loop {
        let waited = unsafe {
            reverie_inguest::trap::raw_syscall6(
                libc::SYS_wait4,
                [child as u64, (&mut status as *mut i32) as u64, 0, 0, 0, 0],
            )
        };
        if waited == -i64::from(libc::EINTR) {
            continue;
        }
        assert_eq!(waited, child);
        break;
    }
    assert_eq!(status, 0);
    println!("fallback fork stats: child=finished");
}

fn site_counts(site: *const u8) -> (u64, u64) {
    let address = site as usize as u64;
    (
        reverie_liteinst::reverie_liteinst_site_trap_count(address),
        reverie_liteinst::reverie_liteinst_site_hook_count(address),
    )
}

fn nested_hook() {
    let site = core::ptr::addr_of!(reverie_liteinst_lifecycle_getppid_site);
    // The guest's own call traps once and patches the site.
    unsafe { reverie_liteinst_lifecycle_getppid() };
    NESTED_GETPPID.store(true, Ordering::Relaxed);
    unsafe { reverie_liteinst_lifecycle_getpid() };
    assert!(!NESTED_GETPPID.load(Ordering::Relaxed));
    let (traps, hooks) = site_counts(site);
    println!("nested getppid traps={traps} hooks={hooks}");
    assert_eq!((traps, hooks), (1, 2));
}

fn hooked_fork_stats() {
    let site = core::ptr::addr_of!(reverie_liteinst_lifecycle_fork_site);
    // The first call traps, installs the hook and forks inside it.
    let child = unsafe { reverie_liteinst_lifecycle_fork() };
    assert!(child >= 0, "fork failed: {child}");
    let (traps, hooks) = site_counts(site);
    if child == 0 {
        // The child's counts were reset, then given back the one hook entry
        // it returns from.
        assert_eq!((traps, hooks), (0, 1));
        unsafe { libc::_exit(0) };
    }
    assert_eq!((traps, hooks), (1, 1));
    let mut status = -1i32;
    loop {
        let waited = unsafe {
            reverie_inguest::trap::raw_syscall6(
                libc::SYS_wait4,
                [child as u64, (&mut status as *mut i32) as u64, 0, 0, 0, 0],
            )
        };
        if waited == -i64::from(libc::EINTR) {
            continue;
        }
        assert_eq!(waited, child);
        break;
    }
    assert_eq!(status, 0);
    println!("hooked fork stats: child=finished");
}

/// The fixture Tool's reserved output descriptor, in mode `tool-output-fd`.
static TOOL_OUTPUT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
/// While set, the fixture Tool's `getppid` callback queues `fill` on its output
/// socket without waiting, and clears this once the socket is full.
static FILL_TOOL_OUTPUT: AtomicBool = AtomicBool::new(false);
/// How many `fill` messages the fixture Tool queued.
static TOOL_OUTPUT_FILLED: AtomicU64 = AtomicU64::new(0);

/// Reserves one end of a `SOCK_SEQPACKET` pair as the Tool's output descriptor,
/// before the Tool is installed, as a Tool's constructor would, and returns the
/// reserved number and the guest's own other end. A regular file (`path`) is
/// refused first.
fn reserve_tool_output_fd(path: &std::ffi::OsStr) -> (libc::c_int, libc::c_int) {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::create(path).unwrap();
    // SAFETY: main runs before any application thread, and before install_tool.
    let refused =
        unsafe { reverie_liteinst::reserve_tool_output_fd(file.as_raw_fd(), b"retired\n") };
    assert_eq!(
        refused.unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput,
        "a regular file must be refused"
    );
    // The refused descriptor is still the caller's.
    assert_ne!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) }, -1);
    drop(file);

    let mut pair = [-1; 2];
    let created = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            pair.as_mut_ptr(),
        )
    };
    assert_eq!(created, 0, "{}", std::io::Error::last_os_error());
    let [original, peer] = pair;
    // SAFETY: as above.
    let reserved =
        unsafe { reverie_liteinst::reserve_tool_output_fd(original, b"retired\n") }.unwrap();
    assert!(reserved >= 1025, "reserved {reserved}");
    TOOL_OUTPUT.store(reserved, Ordering::Relaxed);
    // The original number is closed, so the guest's descriptors are unchanged.
    assert_eq!(unsafe { libc::fcntl(original, libc::F_GETFD) }, -1);
    (reserved, peer)
}

/// The last creation the runtime reported to the hook, and how many it reported.
static CREATION_RESULT: AtomicI64 = AtomicI64::new(0);
static CREATION_IDENTITY: AtomicU64 = AtomicU64::new(0);
static CREATIONS: AtomicU64 = AtomicU64::new(0);

fn record_creation(creation: reverie_liteinst::PhysicalCreation) -> std::io::Result<()> {
    use reverie_liteinst::PhysicalCreation;
    let (result, identity) = match creation {
        PhysicalCreation::Created {
            pid,
            birth_identity,
        } => (i64::from(pid), birth_identity),
        PhysicalCreation::Failed { errno } => (-i64::from(errno), 0),
        PhysicalCreation::Refused { .. } => (i64::MIN, 0),
    };
    CREATION_RESULT.store(result, Ordering::Relaxed);
    CREATION_IDENTITY.store(identity, Ordering::Relaxed);
    CREATIONS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Registers `hook`. Where the kernel has no pidfs (before Linux 6.9) the
/// runtime refuses the hook, which is the correct behaviour there: say so and
/// let the mode end, so the test can require that refusal.
fn register_creation_hook(hook: reverie_liteinst::PhysicalCreationHook) -> bool {
    match reverie_liteinst::set_physical_creation_hook(hook) {
        Ok(()) => true,
        Err(reverie_liteinst::CreationHookError::PidfsUnavailable) => {
            println!("creation hook refused: pidfs unavailable");
            false
        }
        Err(other) => panic!("creation hook not registered: {other:?}"),
    }
}

/// Fails only for a creation that produced a child with an identity, after
/// saying so with a raw write, so that the test can tell a hook error after a
/// real creation from any refusal (both end the process with status 125).
fn fail_after_creation(creation: reverie_liteinst::PhysicalCreation) -> std::io::Result<()> {
    if let reverie_liteinst::PhysicalCreation::Created {
        pid,
        birth_identity,
    } = creation
        && pid > 0
        && birth_identity != 0
    {
        let line = b"created, hook failing\n";
        unsafe { libc::write(1, line.as_ptr().cast(), line.len()) };
        return Err(std::io::Error::other("refused"));
    }
    let line = b"not a creation\n";
    unsafe { libc::write(1, line.as_ptr().cast(), line.len()) };
    Ok(())
}

/// Writes which refusal the runtime reported, with a raw write: the process
/// ends with status 125 as soon as the hook returns.
fn print_refusal(creation: reverie_liteinst::PhysicalCreation) -> std::io::Result<()> {
    use reverie_liteinst::CreationRefusal;
    use reverie_liteinst::PhysicalCreation;
    let line: &[u8] = match creation {
        PhysicalCreation::Refused {
            refusal: CreationRefusal::ArgumentsUnreadable,
            child: None,
        } => b"refused: arguments unreadable, no child\n",
        PhysicalCreation::Refused { .. } => b"refused: another refusal\n",
        _ => b"not refused\n",
    };
    unsafe { libc::write(1, line.as_ptr().cast(), line.len()) };
    Ok(())
}

/// A page-aligned mapping of two pages: the first readable and writable,
/// the second with `second`.
fn two_pages(second: libc::c_int) -> *mut u8 {
    let pages = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            8192,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(pages, libc::MAP_FAILED);
    let pages = pages.cast::<u8>();
    assert_eq!(
        unsafe { libc::mprotect(pages.add(4096).cast(), 4096, second) },
        0
    );
    pages
}

/// Installs an allow-all seccomp filter as a guest would, returning the raw
/// syscall result.
fn allow_all_seccomp_filter() -> i64 {
    let mut allow = libc::sock_filter {
        code: (libc::BPF_RET | libc::BPF_K) as u16,
        jt: 0,
        jf: 0,
        k: libc::SECCOMP_RET_ALLOW,
    };
    let program = libc::sock_fprog {
        len: 1,
        filter: &raw mut allow,
    };
    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0,
            &raw const program,
        )
    }
}

/// A clone3 whose record is `size` bytes at `record`, from the guest's view.
fn raw_clone3(record: *const u8, size: usize) -> i64 {
    unsafe { libc::syscall(libc::SYS_clone3, record, size) }
}

/// The descriptors of this process that are pidfds.
fn pidfd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok())
        .filter(|target| target.to_string_lossy().contains("pidfd"))
        .count()
}

/// With a creation hook registered, every creation form reaches it once, in
/// the parent, with the child's pid and the pidfs inode of the child: the
/// same inode a pidfd opened on the not-yet-reaped child reports. No
/// transient pidfd is left in the parent's table, and legacy clone's
/// CLONE_PARENT_SETTID output still reaches the guest.
fn creation_identity() {
    let check = |form: &str, child: libc::pid_t, before: u64| {
        assert!(child > 0, "{form}: {}", std::io::Error::last_os_error());
        assert_eq!(
            CREATIONS.load(Ordering::Relaxed),
            before + 1,
            "{form}: one report"
        );
        assert_eq!(
            CREATION_RESULT.load(Ordering::Relaxed),
            i64::from(child),
            "{form}"
        );
        assert_eq!(
            pidfd_count(),
            0,
            "{form}: a transient pidfd was left behind"
        );
        // The child has exited but is not reaped yet (its SIGCHLD is default),
        // so a pidfd opened now names the same process.
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, child, 0) } as libc::c_int;
        assert!(
            pidfd >= 0,
            "{form}: pidfd_open: {}",
            std::io::Error::last_os_error()
        );
        let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(pidfd, &mut metadata) }, 0);
        assert_eq!(
            CREATION_IDENTITY.load(Ordering::Relaxed),
            metadata.st_ino,
            "{form}: the reported birth identity"
        );
        unsafe { libc::close(pidfd) };
        // waitid, not wait4: this fixture's Tool answers wait4 itself.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::waitid(libc::P_PID, child as libc::id_t, &mut info, libc::WEXITED) },
            0,
            "{form}: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(unsafe { info.si_status() }, 0, "{form}");
    };
    let exit_child = || unsafe { libc::syscall(libc::SYS_exit_group, 0) };
    let mut identities = Vec::new();

    let before = CREATIONS.load(Ordering::Relaxed);
    let child = unsafe { libc::syscall(libc::SYS_fork) } as libc::pid_t;
    if child == 0 {
        exit_child();
    }
    // Let the child reach its exit before the pidfd_open check.
    std::thread::sleep(std::time::Duration::from_millis(50));
    check("fork", child, before);
    identities.push(CREATION_IDENTITY.load(Ordering::Relaxed));

    let before = CREATIONS.load(Ordering::Relaxed);
    let child = unsafe { libc::syscall(libc::SYS_vfork) } as libc::pid_t;
    if child == 0 {
        exit_child();
    }
    check("vfork", child, before);
    identities.push(CREATION_IDENTITY.load(Ordering::Relaxed));

    let before = CREATIONS.load(Ordering::Relaxed);
    let mut parent_tid: libc::pid_t = 0;
    let flags = (libc::SIGCHLD | libc::CLONE_PARENT_SETTID) as u64;
    let child = unsafe {
        libc::syscall(
            libc::SYS_clone,
            flags,
            0u64,
            &raw mut parent_tid,
            0u64,
            0u64,
        )
    } as libc::pid_t;
    if child == 0 {
        exit_child();
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(parent_tid, child, "clone: CLONE_PARENT_SETTID");
    check("clone", child, before);
    identities.push(CREATION_IDENTITY.load(Ordering::Relaxed));

    let before = CREATIONS.load(Ordering::Relaxed);
    let mut clone_args = [0u64; 8];
    clone_args[4] = libc::SIGCHLD as u64;
    let child = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            clone_args.as_mut_ptr(),
            std::mem::size_of_val(&clone_args),
        )
    } as libc::pid_t;
    if child == 0 {
        exit_child();
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    check("clone3", child, before);
    identities.push(CREATION_IDENTITY.load(Ordering::Relaxed));

    // Each child has its own identity, and none is missing.
    let mut distinct = identities.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert!(
        distinct.len() == 4 && !distinct.contains(&0),
        "identities {identities:?}"
    );

    // A failed clone3 reaches the hook with the errno Linux's own copy of
    // the record returns, and creates no child.
    let failed = |form: &str, record: *const u8, size: usize, errno: i32| {
        let before = CREATIONS.load(Ordering::Relaxed);
        let child = raw_clone3(record, size);
        if child == 0 {
            exit_child();
        }
        assert_eq!(
            (child, std::io::Error::last_os_error().raw_os_error()),
            (-1, Some(errno)),
            "{form}"
        );
        assert_eq!(CREATIONS.load(Ordering::Relaxed), before + 1, "{form}");
        assert_eq!(
            CREATION_RESULT.load(Ordering::Relaxed),
            -i64::from(errno),
            "{form}"
        );
    };
    // 96 bytes with a nonzero byte past the 88 Linux knows: E2BIG.
    let mut long = [0u64; 12];
    long[4] = libc::SIGCHLD as u64;
    long[11] = 1;
    failed("nonzero tail", long.as_ptr().cast(), 96, libc::E2BIG);
    // More than a page: E2BIG before anything is read.
    failed("oversized", long.as_ptr().cast(), 4097, libc::E2BIG);
    // Linux checks a long record's tail before copying its first 88 bytes,
    // in ascending order. A 300-byte record whose tail runs from a readable
    // page into an inaccessible one: EFAULT if the readable part of the tail
    // is zero, E2BIG if it holds a nonzero byte, which Linux reaches first.
    let pages = two_pages(libc::PROT_NONE);
    let record = unsafe { pages.add(4096 - 200) };
    unsafe { *record.add(32).cast::<u64>() = libc::SIGCHLD as u64 };
    failed("unreadable tail", record, 300, libc::EFAULT);
    unsafe { *record.add(150) = 1 };
    failed("nonzero tail before a fault", record, 300, libc::E2BIG);

    println!("creation identity: ok");
}

/// Synchronous Tool code inside a callback, outside the callback's own
/// send_rpc, reaches the coordinator through the process's connection with
/// `blocking_global_rpc`; a global state that is not the installed Tool's is
/// refused without sending. The getpid callback runs the probe.
/// A child makes the coordinator's tool report a backend failure while its
/// parent is parked in a request the tool never answers. The run must end
/// with that failure; without it, the parent would wait forever.
fn backend_failure() {
    let child = unsafe { libc::syscall(libc::SYS_fork) };
    if child == 0 {
        FAIL_ON_GETPID.store(true, Ordering::Relaxed);
        unsafe { libc::syscall(libc::SYS_getpid) };
        unsafe { libc::syscall(libc::SYS_exit_group, 0) };
    }
    PARK_ON_GETPPID.store(true, Ordering::Relaxed);
    unsafe { libc::syscall(libc::SYS_getppid) };
    println!("the parked request was answered");
}

fn blocking_global_rpc() {
    PROBE_BLOCKING_RPC.store(true, Ordering::Relaxed);
    unsafe { libc::getpid() };
    assert_eq!(BLOCKING_RPC_OK.load(Ordering::Relaxed), 1);
    println!("blocking global rpc: ok");
}

/// Run by the getpid callback for the `blocking-global-rpc` mode.
fn probe_blocking_global_rpc() {
    let sent = reverie_liteinst::blocking_global_rpc::<LifecycleGlobal>(RPC_BLOCKING);
    let refused = reverie_liteinst::blocking_global_rpc::<UnusedGlobal>(RPC_BLOCKING);
    let ok = sent.is_ok()
        && refused
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::InvalidInput);
    BLOCKING_RPC_OK.store(u64::from(ok), Ordering::Relaxed);
}

/// A guest `close_range` over every descriptor from 3 up, a fresh pipe's
/// included. The range covers the coordinator connection and any reserved Tool output
/// socket, so the runtime once handled it before Tool dispatch; now the Tool
/// sees it (its callback reports it to the global) and forwards it, and the
/// runtime spares its own descriptors at physical execution. Both pipe ends
/// close, and a later getpid still reaches the coordinator.
fn close_range_through_tool() {
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    // From 3 up: past stdio, and over the coordinator connection, which the
    // runtime opened before the pipe at a lower number.
    let closed = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
    assert_eq!(closed, 0, "{}", std::io::Error::last_os_error());
    for end in pipe {
        assert_eq!(unsafe { libc::fcntl(end, libc::F_GETFD) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EBADF)
        );
    }
    // The coordinator connection survived: this call's RPC reaches it.
    unsafe { libc::getpid() };
    println!("close range through tool: ok");
}

/// The guest tries to close, replace, write, query, truncate, extend and map the
/// Tool's reserved output descriptor, to reach it with high bits set in a
/// descriptor argument, to open, create over or truncate it through procfs, and
/// to close it with `close_range`; every attempt fails or is a no-op. Then the
/// Tool's own write (its `getppid` callback sends `tool`) reaches the guest's
/// end of the pair as the only message.
fn tool_output_fd(reserved: libc::c_int, peer: libc::c_int) {
    let link = || std::fs::read_link(format!("/proc/self/fd/{reserved}")).unwrap();
    let before = link();
    let errno = || std::io::Error::last_os_error().raw_os_error();
    unsafe {
        assert_eq!(libc::close(reserved), 0);
        assert_eq!(libc::write(reserved, b"guest".as_ptr().cast(), 5), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        assert_eq!(libc::fcntl(reserved, libc::F_GETFD), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        // Socket operations: a daemon shutting down every inherited socket
        // must not silence the Tool.
        assert_eq!(libc::shutdown(reserved, libc::SHUT_RDWR), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        assert_eq!(libc::send(reserved, b"guest".as_ptr().cast(), 5, 0), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        let mut kind: libc::c_int = 0;
        let mut kind_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        assert_eq!(
            libc::getsockopt(
                reserved,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &mut kind_len,
            ),
            -1
        );
        assert_eq!(errno(), Some(libc::EBADF));
        // The kernel reads descriptor arguments as 32 bits; high bits must not
        // carry an operation past the protection.
        let aliased = i64::from(reserved) | (1 << 32);
        assert_eq!(
            libc::syscall(libc::SYS_write, aliased, b"guest".as_ptr(), 5),
            -1
        );
        assert_eq!(errno(), Some(libc::EBADF));
        assert_eq!(libc::syscall(libc::SYS_close, aliased), 0);
        // Operations on the file itself.
        assert_eq!(libc::ftruncate(reserved, 0), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        assert_eq!(libc::fallocate(reserved, 0, 0, 4096), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        let mapping = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            reserved,
            0,
        );
        assert_eq!(mapping, libc::MAP_FAILED);
        assert_eq!(errno(), Some(libc::EBADF));
        // A socket cannot be reopened, created over or truncated by name.
        for alias in [
            format!("/proc/self/fd/{reserved}"),
            format!("/dev/fd/{reserved}"),
        ] {
            let path = std::ffi::CString::new(alias).unwrap();
            assert_eq!(
                libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_TRUNC),
                -1
            );
            assert_eq!(libc::creat(path.as_ptr(), 0o600), -1);
            assert_eq!(libc::truncate(path.as_ptr(), 0), -1);
        }
    }
    // A dup2 or dup3 onto the number succeeds, as if it were free; the socket
    // moves to another number, which the Tool keeps using.
    let relocated = unsafe {
        assert_eq!(libc::dup2(libc::STDOUT_FILENO, reserved), reserved);
        let moved = reverie_liteinst::tool_output_fd().unwrap();
        assert!(moved != reserved && moved >= 1025, "moved to {moved}");
        assert_eq!(libc::dup3(libc::STDOUT_FILENO, moved, 0), moved);
        let again = reverie_liteinst::tool_output_fd().unwrap();
        assert!(
            again != moved && again != reserved && again >= 1025,
            "moved to {again}"
        );
        // A dup that Linux refuses (a source that is not open, including a
        // closed number right above the socket, or bad flags) fails the same
        // way, and nothing moves.
        let closed = again + 1;
        assert_eq!(libc::fcntl(closed, libc::F_GETFD), -1);
        for source in [-1, closed] {
            assert_eq!(libc::dup2(source, again), -1);
            assert_eq!(errno(), Some(libc::EBADF));
            assert_eq!(reverie_liteinst::tool_output_fd(), Some(again));
            assert_eq!(libc::fcntl(closed, libc::F_GETFD), -1);
        }
        assert_eq!(libc::dup3(libc::STDOUT_FILENO, again, libc::O_NONBLOCK), -1);
        assert_eq!(errno(), Some(libc::EINVAL));
        assert_eq!(reverie_liteinst::tool_output_fd(), Some(again));
        again
    };
    // Calls that name a number the guest has closed fail as Linux fails them,
    // also when the socket has since moved into that number.
    let relocated = unsafe {
        let stale = libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 1025);
        assert!(stale >= 1025 && stale != relocated, "free number {stale}");
        assert_eq!(libc::close(stale), 0);
        assert_eq!(libc::dup2(libc::STDOUT_FILENO, relocated), relocated);
        assert_eq!(reverie_liteinst::tool_output_fd(), Some(stale));
        assert_eq!(libc::close(relocated), 0);
        stale
    };
    closed_number_calls_fail(relocated);
    let socket_link = || std::fs::read_link(format!("/proc/self/fd/{relocated}")).unwrap();
    assert_eq!(socket_link(), before);
    unsafe { libc::getppid() };
    // The Tool's message is the only one the guest's end receives.
    let mut message = [0u8; 64];
    let mut receive = || unsafe {
        let received = libc::recv(
            peer,
            message.as_mut_ptr().cast(),
            message.len(),
            libc::MSG_DONTWAIT,
        );
        (received > 0).then(|| message[..received as usize].to_vec())
    };
    assert_eq!(receive().as_deref(), Some(&b"tool\n"[..]));
    assert_eq!(receive(), None);
    // The guest's own end of the pair is the one descriptor above stderr it
    // keeps; every other one, the socket's number included, is closed.
    let peer_u = peer as u64;
    unsafe {
        assert_eq!(
            libc::syscall(libc::SYS_close_range, 3u64, peer_u - 1, 0u32),
            0
        );
        assert_eq!(
            libc::syscall(libc::SYS_close_range, peer_u + 1, u32::MAX, 0u32),
            0
        );
        let aliased_first = (peer_u + 1) | (1 << 32);
        assert_eq!(
            libc::syscall(libc::SYS_close_range, aliased_first, u32::MAX, 0u32),
            0
        );
    }
    assert_eq!(socket_link(), before);
    // With the descriptor table full, a dup onto the socket's number still
    // succeeds: the runtime has nowhere to move the socket, so it sends the
    // retirement message and gives the socket up. The socket's queue is full
    // first, and a forked child holding the socket too reads the guest's end
    // only once the dup has had time to begin, so the message has to wait for
    // room and no end-of-file can stand in for it.
    FILL_TOOL_OUTPUT.store(true, Ordering::Relaxed);
    while FILL_TOOL_OUTPUT.load(Ordering::Relaxed) {
        unsafe { libc::getppid() };
    }
    let filled = TOOL_OUTPUT_FILLED.load(Ordering::Relaxed);
    assert!(filled > 0);
    let reader = fork_or_panic();
    if reader == 0 {
        std::thread::sleep(Duration::from_millis(300));
        let wait = libc::timeval {
            tv_sec: 3,
            tv_usec: 0,
        };
        let mut message = [0u8; 64];
        let received = |message: &mut [u8; 64]| unsafe {
            let received = libc::recv(peer, message.as_mut_ptr().cast(), message.len(), 0);
            (received > 0).then(|| message[..received as usize].to_vec())
        };
        let complete = unsafe {
            libc::setsockopt(
                peer,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const wait).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            ) == 0
        } && (0..filled)
            .all(|_| received(&mut message).as_deref() == Some(&b"fill\n"[..]))
            && received(&mut message).as_deref() == Some(&b"retired\n"[..]);
        unsafe { libc::_exit(if complete { 0 } else { 1 }) };
    }
    unsafe {
        let limit = libc::rlimit {
            rlim_cur: relocated as libc::rlim_t + 1,
            rlim_max: libc::RLIM_INFINITY,
        };
        let mut previous = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut previous), 0);
        let lowered = libc::rlimit {
            rlim_max: previous.rlim_max,
            ..limit
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &lowered), 0);
        while libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) >= 0 {}
        assert_eq!(errno(), Some(libc::EMFILE));
        assert_eq!(libc::dup2(libc::STDOUT_FILENO, relocated), relocated);
        assert_eq!(reverie_liteinst::tool_output_fd(), None);
        assert_eq!(
            libc::syscall(libc::SYS_close_range, peer_u + 1, u32::MAX, 0u32),
            0
        );
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &previous), 0);
        let mut status = -1i32;
        loop {
            let waited = reverie_inguest::trap::raw_syscall6(
                libc::SYS_wait4,
                [reader as u64, (&raw mut status) as u64, 0, 0, 0, 0],
            );
            if waited == -i64::from(libc::EINTR) {
                continue;
            }
            assert_eq!(waited, i64::from(reader));
            break;
        }
        assert_eq!(
            status, 0,
            "the reader must receive every fill, then the retirement"
        );
    }
    assert_eq!(receive(), None);
    println!("tool output fd: protected");
}

/// Calls that name `socket`, the Tool output socket's number, in a register
/// argument behave as Linux treats a number the guest has not opened:
/// `epoll_ctl` and `epoll_wait` (after its `EINVAL` count check), `sendfile`'s
/// input, `timerfd_gettime` and `inotify_add_watch` fail with `EBADF`.
fn closed_number_calls_fail(socket: libc::c_int) {
    let errno = || std::io::Error::last_os_error().raw_os_error();
    unsafe {
        let mut pipe = [-1; 2];
        assert_eq!(libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC), 0);
        assert_eq!(libc::write(pipe[1], b"x".as_ptr().cast(), 1), 1);
        let epoll = libc::epoll_create1(libc::EPOLL_CLOEXEC);
        assert!(epoll >= 0);
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0,
        };
        assert_eq!(
            libc::epoll_ctl(epoll, libc::EPOLL_CTL_ADD, socket, &mut event),
            -1
        );
        assert_eq!(errno(), Some(libc::EBADF));
        assert_eq!(libc::epoll_wait(socket, &mut event, 1, 0), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        assert_eq!(libc::epoll_wait(socket, &mut event, 0, 0), -1);
        assert_eq!(errno(), Some(libc::EINVAL));
        assert_eq!(libc::sendfile(pipe[1], socket, std::ptr::null_mut(), 1), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        let mut timer: libc::itimerspec = std::mem::zeroed();
        assert_eq!(libc::timerfd_gettime(socket, &mut timer), -1);
        assert_eq!(errno(), Some(libc::EBADF));
        assert_eq!(
            libc::inotify_add_watch(socket, c"/".as_ptr(), libc::IN_ACCESS),
            -1
        );
        assert_eq!(errno(), Some(libc::EBADF));
        for fd in [pipe[0], pipe[1], epoll] {
            assert_eq!(libc::close(fd), 0);
        }
    }
}

/// Reports whether the kernel randomizes this process's address space.
fn address_randomization() {
    let personality = unsafe { libc::personality(0xffff_ffff) };
    assert_ne!(personality, -1, "{}", std::io::Error::last_os_error());
    let disabled = personality as libc::c_ulong & libc::ADDR_NO_RANDOMIZE as libc::c_ulong != 0;
    println!(
        "address randomization={}",
        if disabled { "disabled" } else { "enabled" }
    );
}

fn main() {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let mode = arguments.next().expect("missing lifecycle fixture mode");
    if mode == "fallback-fork-stats" {
        fallback_fork_stats();
        return;
    }
    if mode == "creation-hook-fails" {
        install_tool();
        if !register_creation_hook(fail_after_creation) {
            return;
        }
        let child = unsafe { libc::syscall(libc::SYS_fork) };
        if child == 0 {
            unsafe { libc::syscall(libc::SYS_exit_group, 0) };
        }
        // The runtime ends this process before the fork returns here.
        println!("creation hook failure did not end the process");
        return;
    }
    if mode == "guest-filter-after-hook" {
        // With a hook registered, the guest cannot attach a filter, and
        // creations keep working.
        install_tool();
        if !register_creation_hook(record_creation) {
            return;
        }
        let refused = allow_all_seccomp_filter();
        let errno = std::io::Error::last_os_error().raw_os_error();
        let child = unsafe { libc::syscall(libc::SYS_fork) } as libc::pid_t;
        if child == 0 {
            unsafe { libc::syscall(libc::SYS_exit_group, 0) };
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let waited =
            unsafe { libc::waitid(libc::P_PID, child as libc::id_t, &mut info, libc::WEXITED) };
        println!(
            "filter={refused} errno={errno:?} created={} waited={waited}",
            CREATION_RESULT.load(Ordering::Relaxed) == i64::from(child) && child > 0
        );
        return;
    }
    if mode == "creation-write-only-args" {
        install_tool();
        if !register_creation_hook(print_refusal) {
            return;
        }
        // The 88 bytes Linux copies first are readable; the rest of a
        // 200-byte record is in a write-only page, which Linux's own read
        // can read but the runtime's process_vm_readv cannot.
        let pages = two_pages(libc::PROT_WRITE);
        let record = unsafe { pages.add(4096 - 88) };
        unsafe { *record.add(32).cast::<u64>() = libc::SIGCHLD as u64 };
        let child = raw_clone3(record, 200);
        if child == 0 {
            unsafe { libc::syscall(libc::SYS_exit_group, 0) };
        }
        // The runtime ends this process before the clone3 returns here.
        println!("refused creation returned {child}");
        return;
    }
    if mode == "creation-identity" {
        install_tool();
        if !register_creation_hook(record_creation) {
            return;
        }
        creation_identity();
        return;
    }
    if mode == "blocking-global-rpc" {
        install_tool();
        blocking_global_rpc();
        return;
    }
    if mode == "backend-failure" {
        install_tool();
        backend_failure();
        return;
    }
    if mode == "close-range-through-tool" {
        SUBSCRIBE_CLOSES.store(true, Ordering::Relaxed);
        install_tool();
        close_range_through_tool();
        return;
    }
    if mode == "tool-output-fd" {
        let (reserved, peer) =
            reserve_tool_output_fd(&arguments.next().expect("missing scratch path"));
        install_tool();
        tool_output_fd(reserved, peer);
        return;
    }
    install_tool();

    match mode.to_str() {
        Some("root-exits-first") => {
            root_exits_first(Path::new(&arguments.next().expect("missing marker path")))
        }
        Some("signaled-descendant") => signaled_descendant(Path::new(
            &arguments.next().expect("missing child pid path"),
        )),
        Some("fast-path") => fast_path(),
        Some("patching-off") => patching_off(),
        Some("timer-refused") => timer_refused(),
        Some("restart-wait4") => restart_wait4(),
        Some("restart-read") => restart_read(),
        Some("nested-hook") => nested_hook(),
        Some("hooked-fork-stats") => hooked_fork_stats(),
        Some("address-randomization") => address_randomization(),
        _ => panic!("unknown lifecycle fixture mode {mode:?}"),
    }
}
