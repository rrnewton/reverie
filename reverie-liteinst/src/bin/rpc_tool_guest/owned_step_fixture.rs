//! Finite seeded two-NOP primitive, not Tool timer delivery.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::CpuIdResult;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;

use super::Floating;
use super::mask;
use super::native_controls;
use super::raw_syscall6;

static CALLBACKS: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
pub(super) struct Probe {
    path: PathBuf,
    mask: u64,
    controls: [u64; 7],
    inventory: Option<reverie_liteinst::PosixTimerInventory>,
    pub(super) registers: [u64; 18],
    pub(super) before: Floating,
    pub(super) after: Floating,
}

unsafe extern "C" {
    fn owned_step_fixture_entry(probe: *mut libc::c_void) -> !;
    fn owned_step_first();
    fn owned_step_last();
}

#[derive(Default)]
struct StepTool;

#[reverie::tool]
impl Tool for StepTool {
    type GlobalState = super::super::CounterGlobal;
    type ThreadState = usize;

    fn subscriptions(_: &()) -> Subscription {
        let mut subscriptions = Subscription::default();
        subscriptions.cpuid();
        subscriptions
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _: u32,
        _: u32,
    ) -> Result<CpuIdResult, reverie::Errno> {
        let index = *guest.thread_state();
        assert!(index < 2);
        *guest.thread_state_mut() += 1;
        assert_eq!(CALLBACKS.fetch_add(1, Ordering::Relaxed), index as u64);
        let registers = guest.regs().await;
        assert_eq!(
            registers.rip,
            owned_step_first as *const () as u64 + index as u64 * 3
        );
        assert_eq!(guest.read_clock().unwrap(), 0);
        assert_eq!(guest.send_rpc(1).await, (index as u64 + 1, 1));
        assert!(
            guest
                .set_timer_precise(reverie::TimerSchedule::Rcbs(1))
                .is_err()
        );
        assert_eq!(guest.read_clock().unwrap(), 0);
        let controls = native_controls();
        assert_eq!(controls[3], 1);
        let _native = core::arch::x86_64::__cpuid(0);
        assert_eq!(native_controls(), controls);
        let mut clobbered = Floating([0xff; 2560]);
        unsafe {
            *libc::__errno_location() = libc::EDOM;
            core::arch::asm!("fninit", "vzeroall",
                ".irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
                r"vpxord zmm\register, zmm\register, zmm\register",
                r"vmovdqu64 [{image} + (\register - 16) * 64], zmm\register", ".endr",
                image = in(reg) clobbered.0.as_mut_ptr(), clobber_abi("C"), options(nostack));
        }
        assert!(clobbered.0[..1024].iter().all(|byte| *byte == 0));
        assert_eq!(guest.read_clock().unwrap(), 0);
        Ok(CpuIdResult {
            eax: index as u32 + 1,
            ebx: 0x2222_2222,
            ecx: 0x3333_3333,
            edx: 0x4444_4444,
        })
    }
}

pub(super) fn run(path: &Path) -> ! {
    let mut probe = Probe {
        path: path.to_owned(),
        mask: mask(),
        controls: native_controls(),
        inventory: None,
        registers: [0; 18],
        before: Floating([0; 2560]),
        after: Floating([0; 2560]),
    };
    unsafe { owned_step_fixture_entry((&raw mut probe).cast()) }
}

pub(super) unsafe extern "C" fn initialize(pointer: *mut libc::c_void) -> i32 {
    let probe = unsafe { &mut *pointer.cast::<Probe>() };
    match unsafe { reverie_liteinst::__install_owned_single_step_fixture::<StepTool>(&probe.path) }
    {
        Ok(inventory) => probe.inventory = Some(inventory),
        Err(_) => {
            let message = b"owned-step: installer failed; probe not entered\n";
            unsafe {
                raw_syscall6(
                    libc::SYS_write,
                    [2, message.as_ptr() as u64, message.len() as u64, 0, 0, 0],
                );
            }
            return 42;
        }
    }
    probe.controls[3] = 0;
    assert_eq!(native_controls(), probe.controls);
    assert_eq!(mask(), probe.mask);
    unsafe { *libc::__errno_location() = libc::E2BIG };
    1
}

pub(super) unsafe extern "C" fn verify(pointer: *const libc::c_void) -> ! {
    let probe = unsafe { &*pointer.cast::<Probe>() };
    assert_eq!(unsafe { *libc::__errno_location() }, libc::E2BIG);
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), 2);
    assert_eq!(
        unsafe { reverie_liteinst::__owned_step_observation() }.unwrap(),
        (2, owned_step_last as *const () as u64, 0)
    );
    assert_eq!(
        unsafe { reverie_liteinst::__read_owned_clock_after_return() }.unwrap(),
        3
    );
    assert_eq!(
        &probe.registers[..4],
        &[2, 0x2222_2222, 0x3333_3333, 0x4444_4444]
    );
    assert_eq!(probe.registers[4], probe.before.0.as_ptr() as u64);
    assert_eq!(probe.registers[5], probe.registers.as_ptr() as u64);
    assert_eq!(
        &probe.registers[6..15],
        &[
            0x7272727272727272,
            0x5555555555555555,
            0x6666666666666666,
            0x4444444444444444,
            0x8888888888888888,
            0x7171717171717171,
            0x7373737373737373,
            0x7474747474747474,
            0x7575757575757575
        ]
    );
    assert_eq!(probe.registers[15], probe.registers[16]);
    assert_eq!(probe.registers[17], 0xed7);
    assert!(probe.before.0[1408..2432].iter().all(|byte| *byte == 0xff));
    assert_ne!(probe.before.0[512] & 0x80, 0);
    assert_eq!(
        &probe.before.0[..2440],
        &probe.after.0[..2440],
        "owned-step full XSAVE"
    );
    assert_eq!(mask(), probe.mask);
    assert_eq!(native_controls(), probe.controls);
    let text = unsafe { std::slice::from_raw_parts(owned_step_first as *const u8, 6) };
    assert_eq!(text, &[0x0f, 0xa2, 0x90, 0x0f, 0xa2, 0x90]);
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
    println!(
        "owned-step: callbacks=2 rpc=2 completed=2 clocks=[0,0,3] state=preserved patches=0 timers=unsupported posix_inventory={:?} fp_profile=seeded-hi16-zmm",
        probe.inventory.unwrap()
    );
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [0; 6]);
    }
    std::process::abort()
}
