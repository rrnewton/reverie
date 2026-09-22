use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::path::Path;
use std::sync::Arc;

use liteinst2::patcher::JumpPatchPlan;
use liteinst2::patcher::LiveJumpPatch;
use liteinst2::patcher::PatchError;
use liteinst2::patcher::StalenessBudget;
use liteinst2::scanner::InstructionScanner;

use super::CounterTool;

// This is a narrow signal-router/restorer control, not a supported threaded
// ToolHost configuration. The worker is created before installation, executes
// only the isolated fixture function afterward, and performs no runtime, Tool,
// allocation, or syscall work before exit_group terminates the process.

const PAGE_BYTES: usize = 4096;
const FUNCTION_OFFSET: usize = 56;
const SITE_OFFSET: usize = 60;
const TARGET_OFFSET: usize = 128;
const PUBLICATION_ROUNDS: usize = 5_000;
const STALENESS_TICKS: u64 = 20_000;

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

pub(super) fn run(path: &Path) -> ! {
    let mapping = DualMapping::new();
    mapping.install_fixture();
    let function = mapping.function();
    let running = Arc::new(AtomicBool::new(true));
    let ready = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicU64::new(0));
    let invalid = Arc::new(AtomicBool::new(false));
    let worker_running = Arc::clone(&running);
    let worker_ready = Arc::clone(&ready);
    let worker_calls = Arc::clone(&calls);
    let worker_invalid = Arc::clone(&invalid);
    let worker = std::thread::spawn(move || {
        worker_ready.store(true, Ordering::Release);
        while worker_running.load(Ordering::Acquire) {
            let value = function();
            if value != 1 && value != 2 {
                worker_invalid.store(true, Ordering::Release);
            }
            worker_calls.fetch_add(1, Ordering::Relaxed);
        }
    });
    while !ready.load(Ordering::Acquire) || calls.load(Ordering::Acquire) < 1_000 {
        core::hint::spin_loop();
    }

    unsafe { reverie_liteinst::install_tool::<CounterTool>(path) }
        .expect("install filtered guard runtime");
    let patch = unsafe {
        bind_retry(
            mapping.plan(),
            mapping.writable_site(),
            StalenessBudget::new(STALENESS_TICKS).expect("nonzero staleness budget"),
        )
    };
    for _ in 0..PUBLICATION_ROUNDS {
        unsafe { patch.apply() }.expect("apply filtered guard patch");
        unsafe { patch.revert() }.expect("revert filtered guard patch");
    }

    let guard_traps = patch.handled_guard_traps();
    let valid = !invalid.load(Ordering::Acquire)
        && calls.load(Ordering::Acquire) > 1_000
        && guard_traps > 0
        && function() == 1;
    // Do not let the worker run its thread-exit bookkeeping under the
    // deliberately sole-controller ToolHost. exit_group below ends both tasks
    // after the fixed observation has been emitted.
    core::mem::forget(worker);
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
