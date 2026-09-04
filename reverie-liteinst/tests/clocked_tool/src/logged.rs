//! Matched V3/V4 logger fixture. Compile diagnostics never execute the constructor.
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::Guest;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::guest_log_fixture as observation;
use reverie_preload::trap::raw_syscall6;

pub mod v4;

pub const PREFIX: &[u8] = b"log\0\xffprefix";
pub const BURST: [u8; 12288] = [0xa5; 12288];
pub const TRAJECTORY: [u64; 7] = [7, 8, 9, 10, 17, 18, 21];
pub const SAMPLES: usize = 7;
static SELECTED: AtomicBool = AtomicBool::new(false);
static CASE: AtomicU64 = AtomicU64::new(0);
static COUNT: AtomicU64 = AtomicU64::new(0);
static PREVIOUS: AtomicU64 = AtomicU64::new(0);
static RESPONSE: AtomicU64 = AtomicU64::new(0);
static LOGGER: Mutex<Option<reverie_liteinst::GuestLogWriter>> = Mutex::new(None);
static CONTROLS: Mutex<[u64; 7]> = Mutex::new([0; 7]);
static MASK: AtomicU64 = AtomicU64::new(0);
pub static GLOBAL_RPCS: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
pub static mut LOG_FIXTURE_ERRNO: *mut i32 = std::ptr::null_mut();

#[repr(C, align(64))]
pub struct Floating(pub [u8; 2560]);
#[repr(C, align(64))]
pub struct Image {
    pub registers: [u64; 18],
    pub fp: Floating,
}
#[repr(C, align(64))]
pub struct Proof {
    pub before: Image,
    pub after: Image,
    pub next_pc: u64,
    pub errno: [i32; 2],
    pub masks: [u64; 2],
    pub controls: [[u64; 7]; 2],
    pub redzone: [[u64; 16]; 2],
    pub query_results: [[i64; 5]; 2],
}

pub fn selected() -> bool {
    SELECTED.load(Ordering::Relaxed)
}
pub fn clocked() -> bool {
    cfg!(feature = "logged-clocked")
}

/// # Safety
/// The loader owns initialization before application threads, using the matched
/// sealed bootstrap. Clocked callers use the existing assembly constructor scope.
/// The clocked profile is a fresh-exec, single-thread, count-only fixture: no
/// POSIX timer creation, interval timers, asynchronous delivery, source arming,
/// fork/exec, or signal-policy changes during its lifetime. Platform startup is
/// trusted; the runner checks loader settings, pending signals and timers. An
/// unavailable POSIX timer inventory is not evidence of an empty inventory.
/// The guest uses only the fixed assembly stream and its synchronous Tool/RPC.
pub unsafe fn initialize() -> Option<i32> {
    let bootstrap = match unsafe { reverie_liteinst::take_preload_bootstrap() } {
        Ok(Some(bootstrap)) => bootstrap,
        Ok(None) => return None,
        Err(_) => return Some(42),
    };
    let installed = (|| -> std::io::Result<()> {
        let data = bootstrap.tool_data;
        if data.len() != 32
            || (&data[..8] != b"LOGTEST1" && &data[..8] != b"LOGTEST4")
            || data[8] != u8::from(clocked())
            || data[9] > if &data[..8] == b"LOGTEST4" { 5 } else { 3 }
        {
            return Err(std::io::Error::other(
                "logged fixture bootstrap/profile mismatch",
            ));
        }
        let fd = i32::from_le_bytes(data[12..16].try_into().unwrap());
        unsafe {
            observation::attach(fd, clocked())?;
        }
        v4::select_guest(&data)?;
        assert!(observation::startup(false));
        assert_eq!(std::fs::read_dir("/proc/self/task")?.count(), 1);
        let xstate = core::arch::x86_64::__cpuid_count(0xd, 0);
        assert_eq!((xstate.eax, xstate.edx, xstate.ebx), (0x2e7, 0, 2440));
        let high = core::arch::x86_64::__cpuid_count(0xd, 7);
        assert_eq!((high.eax, high.ebx), (1024, 1408));
        *CONTROLS.lock().unwrap() = native_controls();
        MASK.store(mask(), Ordering::Relaxed);
        unsafe {
            LOG_FIXTURE_ERRNO = libc::__errno_location();
        }
        CASE.store(u64::from(data[9]), Ordering::Relaxed);
        super::WORK.store(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            Ordering::Relaxed,
        );
        let logger = bootstrap
            .log
            .ok_or_else(|| std::io::Error::other("V3 log required"))?;
        *LOGGER.lock().unwrap() = Some(unsafe { logger.install()? });
        SELECTED.store(true, Ordering::Relaxed);
        super::SUD.store(1, Ordering::Relaxed);
        if clocked() {
            unsafe { reverie_preload::signal::configure_runtime_signals(&[])? };
        }
        unsafe {
            reverie_liteinst::install_tool_from_bootstrap_with_mode::<super::ClockTool>(
                bootstrap.coordinator,
                reverie_liteinst::SyscallMode::UserDispatchWithoutPatching,
            )?;
        }
        assert!(observation::startup(true));
        Ok(())
    })();
    observation::install_result(&installed);
    Some(if installed.is_ok() { 1 } else { 42 })
}

fn mask() -> u64 {
    let mut mask = 0u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut mask) as u64, 8, 0, 0],
            )
        },
        0
    );
    mask
}
fn native_controls() -> [u64; 7] {
    let mut controls = [0; 7];
    for (operation, value) in [0x1003, 0x1004, 0x1022].into_iter().zip(&mut controls[..3]) {
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_arch_prctl,
                    [operation, value as *mut u64 as u64, 0, 0, 0, 0],
                )
            },
            0
        );
    }
    controls[3] = unsafe { raw_syscall6(libc::SYS_arch_prctl, [0x1011, 0, 0, 0, 0, 0]) } as u64;
    let mut tsc = 0i32;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [libc::PR_GET_TSC as u64, (&raw mut tsc) as u64, 0, 0, 0, 0],
            )
        },
        0
    );
    controls[4] = tsc as u64;
    controls[5] = unsafe { core::arch::x86_64::_xgetbv(0) };
    let pkru: u32;
    unsafe {
        core::arch::asm!("rdpkru", in("ecx") 0u32, out("eax") pkru, out("edx") _, options(nostack));
    }
    controls[6] = u64::from(pkru);
    controls
}

fn verify_previous() {
    let address = PREVIOUS.load(Ordering::Relaxed);
    if address == 0 {
        return;
    }
    let proof = unsafe { &*(address as *const Proof) };
    let mut expected = proof.before.registers;
    expected[0] = RESPONSE.load(Ordering::Relaxed);
    expected[2] = proof.next_pc;
    expected[10] = proof.before.registers[17];
    assert_eq!(
        proof.after.registers,
        expected,
        "full GPR/flags/RSP callback={}",
        COUNT.load(Ordering::Relaxed)
    );
    assert_eq!(
        &proof.before.fp.0[1408..2432],
        &[0xff; 1024],
        "seeded Hi16_ZMM required"
    );
    assert_eq!(
        &proof.before.fp.0[..2440],
        &proof.after.fp.0[..2440],
        "full raw FP callback={}",
        COUNT.load(Ordering::Relaxed)
    );
    assert_eq!(proof.errno, [42, 42]);
    assert_eq!(proof.masks, [MASK.load(Ordering::Relaxed); 2]);
    assert_eq!(proof.controls, [*CONTROLS.lock().unwrap(); 2]);
    assert_eq!(proof.query_results, [[0; 5]; 2]);
    assert_eq!(proof.redzone[0], proof.redzone[1]);
    assert_eq!(proof.redzone[0][0], 0xed7);
    assert_eq!(&proof.redzone[0][1..], &[123456; 15]);
}

pub(super) async fn callback<G: Guest<super::ClockTool>>(
    guest: &mut G,
    syscall: Syscall,
) -> Result<i64, Error> {
    assert_eq!(syscall.number(), Sysno::getpid);
    verify_previous();
    let (_, args) = syscall.into_parts();
    let index = COUNT.fetch_add(1, Ordering::Relaxed) as usize;
    assert_eq!(args.arg1, index);
    assert!(index < SAMPLES);
    PREVIOUS.store(args.arg0 as u64, Ordering::Relaxed);
    let before = if clocked() {
        let count = guest
            .read_clock()
            .expect("real paused hardware clock required");
        assert_eq!(count, TRAJECTORY[index], "whole trajectory index={index}");
        assert_eq!(observation::predicates() & 14, 14);
        count
    } else {
        assert_eq!(observation::predicates() & 3, 0);
        index as u64 + 100
    };
    observation::callback(index, clocked().then_some(before));
    assert!(guest.set_timer(reverie::TimerSchedule::Rcbs(100)).is_err());
    assert!(
        guest
            .set_timer_precise(reverie::TimerSchedule::RcbsAndInstructions(0, 1))
            .is_err()
    );
    let runtime_mask = mask();
    let controls = native_controls();
    let errno = unsafe { *libc::__errno_location() };
    if index == 0 {
        let read = Syscall::from_raw(Sysno::read, SyscallArgs::new(0, args.arg2, 5, 0, 0, 0));
        assert_eq!(guest.inject(read).await, Ok(5));
        assert_eq!(
            unsafe { std::slice::from_raw_parts(args.arg2 as *const u8, 5) },
            b"IN\0\xfe!"
        );
    }
    let request = if CASE.load(Ordering::Relaxed) == 2 && index == 1 {
        u64::MAX
    } else {
        before
    };
    if v4::selected() {
        v4::emit(index, LOGGER.lock().unwrap().as_mut().unwrap());
    }
    let reply = guest.send_rpc(request).await;
    assert_eq!(reply, before + 1);
    super::work();
    if !v4::selected() {
        let mut logger = LOGGER.lock().unwrap();
        let writer = logger.as_mut().unwrap();
        if index == 0 {
            writer.write_all(PREFIX).unwrap();
        } else if index == 1 {
            writer.write_all(&BURST).unwrap();
        } else {
            writer.write_all(&[0, index as u8, 0xff]).unwrap();
        }
    }
    let mut clobbered = [0xff_u8; 1024];
    unsafe {
        core::arch::asm!(
            "fninit",
            "vzeroall",
            ".set high_offset, 0",
            ".irp reg,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
            "vpxord zmm\\reg, zmm\\reg, zmm\\reg",
            "vmovdqu64 [rdi + high_offset], zmm\\reg",
            ".set high_offset, high_offset + 64",
            ".endr",
            in("rdi") clobbered.as_mut_ptr(),
            clobber_abi("C"),
            options(nostack)
        );
    }
    assert_eq!(
        clobbered, [0; 1024],
        "actual distinct high-register clobber"
    );
    assert_eq!(mask(), runtime_mask);
    assert_eq!(native_controls(), controls);
    assert_eq!(unsafe { *libc::__errno_location() }, errno);
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags);
    }
    assert_eq!(flags & 0x100, 0);
    if clocked() {
        assert_eq!(guest.read_clock().unwrap(), before);
    } else {
        assert_eq!(observation::predicates() & 3, 0);
    }
    RESPONSE.store(reply, Ordering::Relaxed);
    Ok(reply as i64)
}

pub(super) fn verify_exit() {
    verify_previous();
    assert_eq!(COUNT.load(Ordering::Relaxed), SAMPLES as u64);
    let stats = reverie_liteinst::syscall_mode_stats();
    assert_eq!(
        (
            stats.planning_attempts,
            stats.patch_attempts,
            stats.installed_patches,
            stats.vdso_rewrite_attempts
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        observation::verified_exit().unwrap(),
        clocked().then_some(21)
    );
}

const _: () = {
    assert!(std::mem::size_of::<Image>() == 2752);
    assert!(std::mem::offset_of!(Image, fp) == 192);
    assert!(std::mem::offset_of!(Proof, next_pc) == 5504);
    assert!(std::mem::offset_of!(Proof, errno) == 5512);
    assert!(std::mem::offset_of!(Proof, masks) == 5520);
    assert!(std::mem::offset_of!(Proof, controls) == 5536);
    assert!(std::mem::offset_of!(Proof, redzone) == 5648);
    assert!(std::mem::offset_of!(Proof, query_results) == 5904);
    assert!(std::mem::size_of::<Proof>() == 6016);
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn assembly_layout() {
        assert_eq!(std::mem::size_of::<Image>(), 2752);
        assert_eq!(std::mem::offset_of!(Image, fp), 192);
        assert_eq!(std::mem::offset_of!(Proof, next_pc), 5504);
        assert_eq!(std::mem::offset_of!(Proof, errno), 5512);
        assert_eq!(std::mem::offset_of!(Proof, masks), 5520);
        assert_eq!(std::mem::offset_of!(Proof, controls), 5536);
        assert_eq!(std::mem::offset_of!(Proof, redzone), 5648);
        assert_eq!(std::mem::offset_of!(Proof, query_results), 5904);
        assert_eq!(std::mem::size_of::<Proof>(), 6016);
    }
}
