//! Real private TimerEvent fixture; all completions originate in guest execution.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::CpuIdResult;
use reverie::Guest;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;

use super::Floating;
use super::mask;
use super::native_controls;
use super::raw_syscall6;

static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static WORK: AtomicU64 = AtomicU64::new(0);
static CLOCKS: [AtomicU64; 6] = [const { AtomicU64::new(u64::MAX) }; 6];

#[repr(C)]
pub(super) struct Probe {
    path: PathBuf,
    mask: u64,
    controls: [u64; 7],
    inventory: Option<reverie_liteinst::PosixTimerInventory>,
    text: Vec<u8>,
    pub(super) registers: [u64; 18],
    pub(super) before: Floating,
    pub(super) after: Floating,
}

unsafe extern "C" {
    fn owned_timer_entry(probe: *mut libc::c_void) -> !;
    fn owned_timer_first();
    fn owned_timer_one();
    fn owned_timer_two();
    fn owned_timer_interrupt();
    fn owned_timer_three();
    fn owned_timer_last();
}

fn sites() -> [u64; 6] {
    [
        owned_timer_first as *const () as u64,
        owned_timer_one as *const () as u64,
        owned_timer_two as *const () as u64,
        owned_timer_interrupt as *const () as u64,
        owned_timer_three as *const () as u64,
        owned_timer_last as *const () as u64,
    ]
}

fn work() {
    for iteration in 0..WORK.load(Ordering::Relaxed) {
        std::hint::black_box(iteration);
    }
}

struct Cleanup;
impl Drop for Cleanup {
    fn drop(&mut self) {
        work();
    }
}

#[derive(Default)]
struct TimerTool;

#[reverie::tool]
impl Tool for TimerTool {
    type GlobalState = super::super::CounterGlobal;
    type ThreadState = usize;

    fn subscriptions(_: &()) -> Subscription {
        let mut subscriptions = Subscription::default();
        subscriptions.cpuid();
        subscriptions
    }

    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        assert_eq!(guest.read_clock()?, 0);
        guest.set_timer_precise(TimerSchedule::RcbsAndInstructions(2, 3))?;
        Ok(())
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _: u32,
        _: u32,
    ) -> Result<CpuIdResult, reverie::Errno> {
        let index = sample(guest, false).await;
        assert!([0, 3, 5].contains(&index));
        if index == 3 {
            guest
                .set_timer_precise(TimerSchedule::RcbsAndInstructions(0, 1))
                .unwrap();
        }
        Ok(CpuIdResult {
            eax: index as u32 + 1,
            ebx: 0x2222_2222,
            ecx: if index == 0 { 3 } else { 0x3333_3333 },
            edx: 0x4444_4444,
        })
    }

    async fn handle_timer_event<G: Guest<Self>>(&self, guest: &mut G) {
        let index = sample(guest, true).await;
        let position =
            reverie_liteinst::__owned_timer_position().expect("actual consumed timer position");
        let (generation, sequence, target, suffix) = match index {
            1 => (1, 7, 2, 3),
            2 => (2, 2, 3, 2),
            4 => (5, 1, 3, 1),
            _ => panic!("unexpected TimerEvent index={index}"),
        };
        assert_eq!(
            (
                position.generation,
                position.sequence,
                position.target,
                position.suffix
            ),
            (generation, sequence, target, suffix),
            "index={index} position={position:?}"
        );
        assert_eq!((position.rip, position.clock), (sites()[index], 3));
        let output_state = runtime_output_state();
        println!(
            "timer-position: event={index} generation={generation} sequence={sequence} pc_offset={} clock=3 target={target} suffix={suffix}",
            position.rip - sites()[0]
        );
        assert_eq!(
            runtime_output_state(),
            output_state,
            "normal Tool output event={index}"
        );
        assert_eq!(guest.read_clock().unwrap(), position.clock);
        match index {
            1 => guest
                .set_timer_precise(TimerSchedule::RcbsAndInstructions(0, 2))
                .unwrap(),
            2 => {
                guest
                    .set_timer_precise(TimerSchedule::RcbsAndInstructions(0, 2))
                    .unwrap();
                guest
                    .set_timer_precise(TimerSchedule::RcbsAndInstructions(0, 3))
                    .unwrap();
            }
            4 => {}
            _ => unreachable!(),
        }
    }
}

fn runtime_output_state() -> (u64, [u64; 7]) {
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    assert_eq!(
        flags & 0x100,
        0,
        "runtime output must never execute with owned TF"
    );
    let mask = mask();
    let expected = reverie_preload::signal::runtime_ordinary_mask()
        & !((1 << (libc::SIGKILL - 1)) | (1 << (libc::SIGSTOP - 1)));
    assert_eq!(mask, expected);
    assert_ne!(mask & (1 << (libc::SIGTRAP - 1)), 0);
    (mask, native_controls())
}

async fn sample<G: Guest<TimerTool>>(guest: &mut G, timer: bool) -> usize {
    let _cleanup = Cleanup;
    let index = *guest.thread_state();
    assert!(index < CLOCKS.len());
    assert_eq!(CALLBACKS.fetch_add(1, Ordering::Relaxed), index as u64);
    *guest.thread_state_mut() += 1;
    assert_eq!(timer, [1, 2, 4].contains(&index));
    let registers = guest.regs().await;
    assert_eq!(registers.rip, sites()[index]);
    assert_eq!(
        registers.eflags & 0x100,
        0,
        "Tool must not observe owned TF"
    );
    let clock = guest.read_clock().unwrap();
    assert_eq!(clock, if index == 0 { 0 } else { 3 }, "event={index}");
    CLOCKS[index].store(clock, Ordering::Relaxed);
    let local = 0u64;
    assert!(registers.rsp.abs_diff((&raw const local) as u64) > 64 * 1024);
    assert_eq!(guest.send_rpc(1).await, (index as u64 + 1, 1));
    let controls = native_controls();
    assert_eq!((controls[3], controls[4]), (1, libc::PR_TSC_ENABLE as u64));
    let _native = core::arch::x86_64::__cpuid(0);
    let _tsc = reverie::RdtscResult::new(reverie::Rdtsc::Tsc);
    let _tscp = reverie::RdtscResult::new(reverie::Rdtsc::Tscp);
    work();
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
    assert_eq!(native_controls(), controls);
    assert_eq!(guest.read_clock().unwrap(), clock);
    index
}

pub(super) fn run(path: &Path, work: u64) -> ! {
    WORK.store(work, Ordering::Relaxed);
    let text = unsafe {
        std::slice::from_raw_parts(
            sites()[0] as *const u8,
            (sites()[5] + 2 - sites()[0]) as usize,
        )
    }
    .to_vec();
    let mut probe = Probe {
        path: path.to_owned(),
        mask: mask(),
        controls: native_controls(),
        inventory: None,
        text,
        registers: [0; 18],
        before: Floating([0; 2560]),
        after: Floating([0; 2560]),
    };
    unsafe { owned_timer_entry((&raw mut probe).cast()) }
}

pub(super) unsafe extern "C" fn initialize(pointer: *mut libc::c_void) -> i32 {
    let probe = unsafe { &mut *pointer.cast::<Probe>() };
    match unsafe {
        reverie_liteinst::__install_owned_precise_timer_fixture::<TimerTool>(&probe.path)
    } {
        Ok(inventory) => probe.inventory = Some(inventory),
        Err(error) => {
            eprintln!("owned-timer: installer failed; probe not entered: {error}");
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
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), 6);
    assert_eq!(
        unsafe { reverie_liteinst::__owned_step_observation() }.unwrap(),
        (11, sites()[4], 3)
    );
    let mut clocks = CLOCKS
        .iter()
        .map(|sample| sample.load(Ordering::Relaxed))
        .collect::<Vec<_>>();
    clocks.push(unsafe { reverie_liteinst::__read_owned_clock_after_return() }.unwrap());
    assert_eq!(clocks, [0, 3, 3, 3, 3, 3, 6]);
    assert_eq!(
        &probe.registers[..4],
        &[6, 0x2222_2222, 0x3333_3333, 0x4444_4444]
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
        "owned-timer full seeded XSAVE"
    );
    assert_eq!(mask(), probe.mask);
    assert_eq!(native_controls(), probe.controls);
    let text = unsafe { std::slice::from_raw_parts(sites()[0] as *const u8, probe.text.len()) };
    assert_eq!(text, probe.text);
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
        "owned-timer: callbacks=6 rpc=6 timers=3 completed=11 clocks={clocks:?} work={} state=preserved patches=0 posix_inventory={:?} fp_profile=seeded-hi16-zmm",
        WORK.load(Ordering::Relaxed),
        probe.inventory.unwrap()
    );
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [0; 6]);
    }
    std::process::abort()
}
