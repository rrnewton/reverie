use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use liteinst2::patcher::JumpPatchPlan;
use liteinst2::patcher::LiveJumpPatch;
use liteinst2::patcher::PatchError;
use liteinst2::patcher::PatchStrategy;
use liteinst2::patcher::StalenessBudget;
use liteinst2::scanner::InstructionScanner;

use super::CounterTool;

// This is a narrow signal-router/restorer control. Runtime preparation happens
// while the process is single-threaded. Only afterward do two forked processes
// use the shared dual-mapped memfd: one publishes patches and the other executes
// them concurrently. No unsupported pre-installation thread is hidden here.

const PAGE_BYTES: usize = 4096;
const FUNCTION_OFFSET: usize = 56;
const SITE_OFFSET: usize = 60;
const TARGET_OFFSET: usize = 128;
const CONTROL_OFFSET: usize = 256;
const PUBLICATION_ROUNDS: usize = 5_000;
const MIN_CALLS: u64 = 1_000;
const STALENESS_TICKS: u64 = 20_000;
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(8);
const EXECUTOR_TIMEOUT_SECONDS: libc::time_t = 8;
const EXECUTOR_SETUP_FAILURE_STATUS: i32 = 85;

#[repr(C, align(64))]
struct SharedControl {
    ready: AtomicBool,
    calls: AtomicU64,
    invalid: AtomicBool,
    done: AtomicBool,
    guard_traps: AtomicU64,
}

impl SharedControl {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            calls: AtomicU64::new(0),
            invalid: AtomicBool::new(false),
            done: AtomicBool::new(false),
            guard_traps: AtomicU64::new(0),
        }
    }
}

struct DualMapping {
    writable: *mut u8,
    executable: *mut u8,
}

impl DualMapping {
    fn new() -> Self {
        let name = b"reverie-liteinst-filtered-guard\0";
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                name.as_ptr().cast::<libc::c_char>(),
                libc::MFD_CLOEXEC,
            ) as libc::c_int
        };
        assert!(fd >= 0, "memfd_create filtered guard fixture");
        assert_eq!(unsafe { libc::ftruncate(fd, PAGE_BYTES as libc::off_t) }, 0);
        let writable = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                PAGE_BYTES,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(writable, libc::MAP_FAILED);
        let executable = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                PAGE_BYTES,
                libc::PROT_READ | libc::PROT_EXEC,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(executable, libc::MAP_FAILED);
        assert_eq!(unsafe { libc::close(fd) }, 0);
        Self {
            writable: writable.cast(),
            executable: executable.cast(),
        }
    }

    fn install_fixture(&self) {
        let entry = [0xf3, 0x0f, 0x1e, 0xfa];
        let original = [0xb8, 1, 0, 0, 0, 0xc3, 0x90, 0x90];
        let target = [0xb8, 2, 0, 0, 0, 0xc3];
        unsafe {
            self.writable
                .add(CONTROL_OFFSET)
                .cast::<SharedControl>()
                .write(SharedControl::new());
            core::ptr::copy_nonoverlapping(
                entry.as_ptr(),
                self.writable.add(FUNCTION_OFFSET),
                entry.len(),
            );
            core::ptr::copy_nonoverlapping(
                original.as_ptr(),
                self.writable.add(SITE_OFFSET),
                original.len(),
            );
            core::ptr::copy_nonoverlapping(
                target.as_ptr(),
                self.writable.add(TARGET_OFFSET),
                target.len(),
            );
        }
    }

    fn plan(&self) -> JumpPatchPlan {
        let scanner = InstructionScanner::default();
        let code = unsafe { core::slice::from_raw_parts(self.writable.add(SITE_OFFSET), 8) };
        let site = self.executable as u64 + SITE_OFFSET as u64;
        let target = self.executable as u64 + TARGET_OFFSET as u64;
        let scan = scanner
            .scan(code, site)
            .expect("scan filtered guard fixture");
        JumpPatchPlan::from_scan(&scanner, &scan, code, site, site, target)
            .expect("plan filtered guard fixture")
    }

    fn function(&self) -> extern "C" fn() -> u32 {
        let address = self.executable as usize + FUNCTION_OFFSET;
        unsafe { core::mem::transmute(address) }
    }

    fn writable_site(&self) -> *mut u8 {
        unsafe { self.writable.add(SITE_OFFSET) }
    }

    fn control(&self) -> &SharedControl {
        unsafe { &*self.writable.add(CONTROL_OFFSET).cast::<SharedControl>() }
    }
}

unsafe fn bind_retry(
    plan: JumpPatchPlan,
    writable_address: *mut u8,
    staleness: StalenessBudget,
) -> LiveJumpPatch {
    loop {
        match unsafe { LiveJumpPatch::bind(plan.clone(), writable_address, staleness) } {
            Ok(patch) => return patch,
            Err(PatchError::Contended) => std::thread::yield_now(),
            Err(error) => panic!("bind filtered guard fixture: {error}"),
        }
    }
}

fn poll_executor(child: libc::pid_t) -> Result<Option<i32>, i32> {
    let mut status = 0;
    let waited = unsafe { libc::waitpid(child, &mut status, libc::WNOHANG) };
    if waited == child {
        return Ok(Some(status));
    }
    if waited == 0 {
        return Ok(None);
    }
    let error = std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO);
    if error == libc::EINTR {
        Ok(None)
    } else {
        Err(error)
    }
}

fn wait_executor_until(child: libc::pid_t, deadline: Instant) -> Result<Option<i32>, i32> {
    loop {
        if let Some(status) = poll_executor(child)? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn terminate_executor(child: libc::pid_t) -> Result<i32, i32> {
    if unsafe { libc::kill(child, libc::SIGKILL) } != 0 {
        let error = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        if error != libc::ESRCH {
            return Err(error);
        }
    }
    wait_executor_until(child, Instant::now() + Duration::from_secs(1))?.ok_or(libc::ETIMEDOUT)
}

unsafe fn configure_executor_lifetime(parent: libc::pid_t) -> bool {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) } != 0
        || unsafe { libc::getppid() } != parent
    {
        return false;
    }

    // SIGKILL cannot be blocked by the all-signals installation mask, so this
    // kernel timer bounds installation itself as well as bind and execution.
    let mut event: libc::sigevent = unsafe { core::mem::zeroed() };
    event.sigev_notify = libc::SIGEV_SIGNAL;
    event.sigev_signo = libc::SIGKILL;
    let mut timer: libc::timer_t = unsafe { core::mem::zeroed() };
    if unsafe { libc::timer_create(libc::CLOCK_MONOTONIC, &raw mut event, &mut timer) } != 0 {
        return false;
    }
    let timeout = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: EXECUTOR_TIMEOUT_SECONDS,
            tv_nsec: 0,
        },
    };
    if unsafe { libc::timer_settime(timer, 0, &timeout, core::ptr::null_mut()) } != 0 {
        let _ = unsafe { libc::timer_delete(timer) };
        return false;
    }
    true
}

fn wait_staleness(staleness: StalenessBudget, deadline: Instant) -> Result<(), ()> {
    use core::arch::x86_64::_mm_lfence;
    use core::arch::x86_64::_rdtsc;

    core::sync::atomic::compiler_fence(Ordering::SeqCst);
    unsafe { _mm_lfence() };
    let start = unsafe { _rdtsc() };
    loop {
        unsafe { _mm_lfence() };
        if unsafe { _rdtsc() }.wrapping_sub(start) >= staleness.cycles() {
            break;
        }
        if Instant::now() >= deadline {
            return Err(());
        }
        core::hint::spin_loop();
    }
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
    Ok(())
}

unsafe fn publish_shared_cross_line(
    plan: &JumpPatchPlan,
    writable_address: *mut u8,
    expected: [u8; 8],
    replacement: [u8; 8],
    staleness: StalenessBudget,
    deadline: Instant,
) -> Result<(), ()> {
    let PatchStrategy::GuardedSplit {
        front_len,
        back_len,
    } = plan.strategy()
    else {
        return Err(());
    };
    let boundary = unsafe { writable_address.add(front_len) };
    let front = unsafe { &*boundary.sub(8).cast::<AtomicU64>() };
    let back = unsafe { &*boundary.cast::<AtomicU64>() };
    let current_front = front.load(Ordering::SeqCst);
    let current_back = back.load(Ordering::SeqCst);
    let front_offset = 8 - front_len;
    let current_front_bytes = current_front.to_le_bytes();
    let current_back_bytes = current_back.to_le_bytes();
    let mut current = [0_u8; 8];
    current[..front_len].copy_from_slice(&current_front_bytes[front_offset..]);
    current[front_len..].copy_from_slice(&current_back_bytes[..back_len]);
    if current != expected {
        return Err(());
    }

    let mut guarded_front_bytes = current_front_bytes;
    for offset in plan.guarded_byte_offsets() {
        if offset < front_len {
            guarded_front_bytes[front_offset + offset] = 0xcc;
        }
    }
    let mut new_back_bytes = current_back_bytes;
    new_back_bytes[..back_len].copy_from_slice(&replacement[front_len..]);
    let mut new_front_bytes = current_front_bytes;
    new_front_bytes[front_offset..].copy_from_slice(&replacement[..front_len]);
    front
        .compare_exchange(
            current_front,
            u64::from_le_bytes(guarded_front_bytes),
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .map_err(|_| ())?;
    wait_staleness(staleness, deadline)?;
    back.store(u64::from_le_bytes(new_back_bytes), Ordering::SeqCst);
    wait_staleness(staleness, deadline)?;
    front.store(u64::from_le_bytes(new_front_bytes), Ordering::SeqCst);
    Ok(())
}

pub(super) fn run(path: &Path) -> ! {
    let mapping = DualMapping::new();
    mapping.install_fixture();
    let function = mapping.function();
    let plan = mapping.plan();
    let staleness = StalenessBudget::new(STALENESS_TICKS).expect("nonzero staleness budget");
    let parent = unsafe { libc::getpid() };
    let deadline = Instant::now() + FIXTURE_TIMEOUT;
    let child = unsafe { libc::syscall(libc::SYS_fork) as libc::pid_t };
    assert!(child >= 0, "fork filtered guard publisher");
    if child == 0 {
        if !unsafe { configure_executor_lifetime(parent) } {
            unsafe { libc::_exit(EXECUTOR_SETUP_FAILURE_STATUS) };
        }
        unsafe { reverie_liteinst::install_tool::<CounterTool>(path) }
            .expect("install filtered guard runtime");
        let control = mapping.control();
        let patch = unsafe { bind_retry(plan, mapping.writable_site(), staleness) };
        control.ready.store(true, Ordering::Release);
        while !control.done.load(Ordering::Acquire) {
            let value = function();
            if value != 1 && value != 2 {
                control.invalid.store(true, Ordering::Release);
            }
            control.calls.fetch_add(1, Ordering::Relaxed);
        }
        control
            .guard_traps
            .store(patch.handled_guard_traps(), Ordering::Release);
        let status = if !control.invalid.load(Ordering::Acquire)
            && control.calls.load(Ordering::Acquire) > MIN_CALLS
            && function() == 1
        {
            0
        } else {
            84
        };
        unsafe { libc::_exit(status) };
    }

    let control = mapping.control();
    let mut child_status = None;
    let mut child_owned = true;
    let mut lifecycle_ok = true;
    while !control.ready.load(Ordering::Acquire) && Instant::now() < deadline {
        match poll_executor(child) {
            Ok(Some(status)) => {
                child_status = Some(status);
                lifecycle_ok = false;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(_) => {
                // A wait error loses proof that this numeric PID is still our
                // child. The process-group owner will perform final cleanup.
                child_owned = false;
                lifecycle_ok = false;
                break;
            }
        }
    }
    let mut published =
        lifecycle_ok && child_status.is_none() && control.ready.load(Ordering::Acquire);
    if published {
        let original = plan.original_bytes();
        let replacement = plan.replacement_bytes();
        for _ in 0..PUBLICATION_ROUNDS {
            if Instant::now() >= deadline
                || unsafe {
                    publish_shared_cross_line(
                        &plan,
                        mapping.writable_site(),
                        original,
                        replacement,
                        staleness,
                        deadline,
                    )
                }
                .is_err()
                || unsafe {
                    publish_shared_cross_line(
                        &plan,
                        mapping.writable_site(),
                        replacement,
                        original,
                        staleness,
                        deadline,
                    )
                }
                .is_err()
            {
                published = false;
                break;
            }
        }
    }
    control.done.store(true, Ordering::Release);
    if child_status.is_none() && child_owned {
        match wait_executor_until(child, deadline) {
            Ok(Some(status)) => child_status = Some(status),
            Ok(None) => {
                lifecycle_ok = false;
                child_status = terminate_executor(child).ok();
            }
            Err(_) => {
                lifecycle_ok = false;
            }
        }
    }
    let status = child_status.unwrap_or_default();
    let valid = lifecycle_ok
        && child_status.is_some()
        && libc::WIFEXITED(status)
        && libc::WEXITSTATUS(status) == 0
        && published
        && !control.invalid.load(Ordering::Acquire)
        && control.calls.load(Ordering::Acquire) > MIN_CALLS
        && control.guard_traps.load(Ordering::Acquire) > 0
        && function() == 1;
    let (message, status): (&[u8], u64) = if valid {
        (b"filtered-guard-restorer=returned\n", 0)
    } else {
        (b"filtered-guard-restorer=failed\n", 83)
    };
    unsafe {
        let _ = reverie_preload::trap::raw_syscall6(
            libc::SYS_write,
            [
                libc::STDOUT_FILENO as u64,
                message.as_ptr() as u64,
                message.len() as u64,
                0,
                0,
                0,
            ],
        );
        let _ = reverie_preload::trap::raw_syscall6(libc::SYS_exit_group, [status, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}
