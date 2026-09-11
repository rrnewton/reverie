//! Additional finite seeded clock profile; original unseeded obligations remain.
//! The assembly-owned initializer excludes all Rust setup before first enable.
//! This uses the shared non-sampling RCB event, not a notification timer. Fresh
//! exec/trusted startup and the no-async/no-POSIX-timer caller contract still apply.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::CpuIdResult;
use reverie::Guest;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie::Subscription;
use reverie::Tool;

use super::Floating;
use super::mask;
use super::native_controls;
use super::raw_syscall6;

const TRAJECTORY: [u64; 9] = [7, 8, 9, 10, 17, 18, 19, 19, 21];
static SAMPLES: [AtomicU64; 9] = [const { AtomicU64::new(0) }; 9];
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static WORK: AtomicU64 = AtomicU64::new(0);
pub(super) static mut OUTPUTS: [[u64; 4]; 9] = [[0; 4]; 9];

#[repr(C)]
pub(super) struct Probe {
    path: PathBuf,
    original_mask: u64,
    controls: [u64; 7],
    inventory: Option<reverie_liteinst::PosixTimerInventory>,
    pub(super) registers: [u64; 18],
    pub(super) before: Floating,
    pub(super) after: Floating,
}

unsafe extern "C" {
    fn owned_clock_entry(probe: *mut libc::c_void) -> !;
    fn owned_clock_site_0();
    fn owned_clock_site_1();
    fn owned_clock_site_2();
    fn owned_clock_site_3();
    fn owned_clock_site_4();
    fn owned_clock_site_5();
    fn owned_clock_site_6();
    fn owned_clock_site_7();
    fn owned_clock_site_8();
}

fn sites() -> [u64; 9] {
    [
        owned_clock_site_0 as *const () as u64,
        owned_clock_site_1 as *const () as u64,
        owned_clock_site_2 as *const () as u64,
        owned_clock_site_3 as *const () as u64,
        owned_clock_site_4 as *const () as u64,
        owned_clock_site_5 as *const () as u64,
        owned_clock_site_6 as *const () as u64,
        owned_clock_site_7 as *const () as u64,
        owned_clock_site_8 as *const () as u64,
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
struct ClockCounterTool;

#[reverie::tool]
impl Tool for ClockCounterTool {
    type GlobalState = super::super::CounterGlobal;
    type ThreadState = usize;

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscriptions = Subscription::default();
        subscriptions.cpuid().rdtsc();
        subscriptions
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuIdResult, reverie::Errno> {
        assert_eq!((eax, ecx), (0xfeed, 0xbaad));
        let count = sample(guest, None).await;
        Ok(CpuIdResult {
            eax: count as u32,
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
        let count = sample(guest, Some(request)).await;
        Ok(RdtscResult {
            tsc: count,
            aux: (request == Rdtsc::Tscp && count != 18).then_some(0xaaaa_0000 | count as u32),
        })
    }
}

async fn sample<G: Guest<ClockCounterTool>>(guest: &mut G, request: Option<Rdtsc>) -> u64 {
    let _cleanup = Cleanup;
    let index = *guest.thread_state();
    assert!(index < TRAJECTORY.len());
    assert_eq!(CALLBACKS.fetch_add(1, Ordering::Relaxed), index as u64);
    *guest.thread_state_mut() += 1;
    assert_eq!(
        request,
        match index % 3 {
            0 => None,
            1 => Some(Rdtsc::Tsc),
            _ => Some(Rdtsc::Tscp),
        }
    );
    let registers = guest.regs().await;
    assert_eq!(registers.rip, sites()[index]);
    let before = guest.read_clock().expect("real paused guest RCB clock");
    SAMPLES[index].store(before, Ordering::Relaxed);
    assert_eq!(
        before, TRAJECTORY[index],
        "clock callback={index} request={request:?}"
    );
    work();
    assert_eq!(guest.send_rpc(1).await, (index as u64 + 1, 1));
    for precise in [false, true] {
        let result = if precise {
            guest.set_timer_precise(reverie::TimerSchedule::Rcbs(100))
        } else {
            guest.set_timer(reverie::TimerSchedule::Rcbs(100))
        };
        assert!(
            result.is_err(),
            "counting-only path must refuse timer delivery"
        );
    }
    assert!(
        guest
            .set_timer_precise(reverie::TimerSchedule::RcbsAndInstructions(0, 1))
            .is_err()
    );
    let controls = native_controls();
    assert_eq!((controls[3], controls[4]), (1, libc::PR_TSC_ENABLE as u64));
    let _native_cpuid = core::arch::x86_64::__cpuid(0);
    assert_ne!(RdtscResult::new(Rdtsc::Tsc).tsc, before);
    assert_ne!(RdtscResult::new(Rdtsc::Tscp).tsc, before);
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), index as u64 + 1);
    let local = 0u64;
    assert!(registers.rsp.abs_diff((&raw const local) as u64) > 64 * 1024);
    let mut stack: libc::stack_t = unsafe { core::mem::zeroed() };
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_sigaltstack,
                [0, (&raw mut stack) as u64, 0, 0, 0, 0],
            )
        },
        0
    );
    assert_eq!(stack.ss_flags, 0);
    assert!(
        !(stack.ss_sp as usize..stack.ss_sp as usize + stack.ss_size)
            .contains(&((&raw const local) as usize))
    );
    assert_eq!(
        guest.read_clock().unwrap(),
        before,
        "runtime/Tool/RPC/native work counted callback={index}"
    );
    unsafe {
        *libc::__errno_location() = libc::EDOM;
        let mut clobbered = Floating([0xff; 2560]);
        core::arch::asm!(
            "fninit", "vzeroall",
            ".irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
            r"vpxord zmm\register, zmm\register, zmm\register",
            r"vmovdqu64 [{image} + (\register - 16) * 64], zmm\register", ".endr",
            image = in(reg) clobbered.0.as_mut_ptr(), clobber_abi("C"), options(nostack),
        );
        assert!(
            clobbered.0[..1024].iter().all(|byte| *byte == 0),
            "Hi16_ZMM clobber callback={index}"
        );
    }
    before
}

pub(super) fn run(path: &Path, work: u64) -> ! {
    assert!(work <= 20_000);
    WORK.store(work, Ordering::Relaxed);
    let mut probe = Probe {
        path: path.to_owned(),
        original_mask: mask(),
        controls: native_controls(),
        inventory: None,
        registers: [0; 18],
        before: Floating([0; 2560]),
        after: Floating([0; 2560]),
    };
    unsafe { owned_clock_entry((&raw mut probe).cast()) }
}

pub(super) fn run_install_failure(path: &Path) -> ! {
    extern "C" fn nondefault_handler(_signal: i32) {}
    assert_ne!(
        unsafe { libc::signal(libc::SIGBUS, nondefault_handler as *const () as usize) },
        libc::SIG_ERR
    );
    run(path, 0)
}

pub(super) unsafe extern "C" fn initialize(probe: *mut libc::c_void) -> i32 {
    let probe = unsafe { &mut *probe.cast::<Probe>() };
    check_text();
    let installed = unsafe {
        reverie_liteinst::__install_owned_clocked_instruction_fixture::<ClockCounterTool>(
            &probe.path,
        )
    };
    match installed {
        Ok(inventory) => probe.inventory = Some(inventory),
        Err(error) => {
            let message: &[u8] = match error.kind() {
                std::io::ErrorKind::Unsupported => {
                    b"owned-clock: installer refused Unsupported; probe not entered\n"
                }
                std::io::ErrorKind::InvalidInput => {
                    b"owned-clock: installer refused InvalidInput; probe not entered\n"
                }
                _ => b"owned-clock: installer failed; probe not entered\n",
            };
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
    probe.controls[4] = libc::PR_TSC_SIGSEGV as u64;
    assert_eq!(native_controls(), probe.controls);
    assert_eq!(mask(), probe.original_mask);
    work();
    unsafe { *libc::__errno_location() = libc::E2BIG };
    1
}

fn check_text() {
    for (index, site) in sites().into_iter().enumerate() {
        let expected: &[u8] = match index % 3 {
            0 => &[0x0f, 0xa2],
            1 => &[0x0f, 0x31],
            _ => &[0x0f, 0x01, 0xf9],
        };
        assert_eq!(
            unsafe { std::slice::from_raw_parts(site as *const u8, expected.len()) },
            expected
        );
    }
}

pub(super) unsafe extern "C" fn verify(probe: *const libc::c_void) -> ! {
    let probe = unsafe { &*probe.cast::<Probe>() };
    assert_eq!(unsafe { *libc::__errno_location() }, libc::E2BIG);
    let mut samples: Vec<_> = SAMPLES
        .iter()
        .map(|value| value.load(Ordering::Relaxed))
        .collect();
    assert_eq!(samples, TRAJECTORY);
    assert_eq!(&samples[..6], &[7, 8, 9, 10, 17, 18]);
    let final_count = unsafe { reverie_liteinst::__read_owned_clock_after_return() }.unwrap();
    assert_eq!(
        final_count, 24,
        "three guest branches after final owned return"
    );
    samples.push(final_count);
    let deltas: Vec<_> = samples.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert_eq!(deltas, [1, 1, 1, 7, 1, 1, 0, 2, 3]);
    let outputs = unsafe { std::ptr::read(&raw const OUTPUTS) };
    for (index, output) in outputs.into_iter().enumerate() {
        let count = samples[index];
        let expected = match index % 3 {
            0 => [count, 0x2222_2222, 0x3333_3333, 0x4444_4444],
            1 => [count, 0x2222_2222, 0x3333_3333, 0],
            _ => [
                count,
                0x2222_2222,
                if count == 18 { 0 } else { 0xaaaa_0000 | count },
                0,
            ],
        };
        assert_eq!(output, expected, "typed clock output callback={index}");
    }
    let registers = &probe.registers;
    assert_eq!(&registers[..4], &[21, 0x2222_2222, 0xaaaa_0015, 0]);
    assert_eq!(registers[4], probe.before.0.as_ptr() as u64);
    assert_eq!(registers[5], registers.as_ptr() as u64);
    assert_eq!(
        &registers[6..15],
        &[
            0x7272_7272_7272_7272,
            0x5555_5555_5555_5555,
            0x6666_6666_6666_6666,
            0x4444_4444_4444_4444,
            0x8888_8888_8888_8888,
            0x7171_7171_7171_7171,
            0x7373_7373_7373_7373,
            0x7474_7474_7474_7474,
            0x7575_7575_7575_7575,
        ]
    );
    assert_eq!(registers[15], registers[16]);
    assert_eq!(registers[17], 0xed7);
    assert_ne!(probe.before.0[512] & 0x80, 0);
    assert!(probe.before.0[1408..2432].iter().all(|byte| *byte == 0xff));
    assert_eq!(
        &probe.before.0[..2440],
        &probe.after.0[..2440],
        "clock seeded full FP callbacks=9"
    );
    assert_eq!(mask(), probe.original_mask);
    assert_eq!(native_controls(), probe.controls);
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), 9);
    check_text();
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
        "owned-clock: samples={samples:?} deltas={deltas:?} callbacks=9 rpc=9 work={} state=preserved patches=0 timers=unsupported posix_inventory={:?} fp_profile=seeded-hi16-zmm",
        WORK.load(Ordering::Relaxed),
        probe.inventory.unwrap()
    );
    unsafe { raw_syscall6(libc::SYS_exit_group, [0; 6]) };
    unreachable!()
}
