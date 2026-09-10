//! Finite fixture launched by `rpc_tool::sud_guest` through a fresh executable.
//! Build with `--no-default-features --features test-owned-cpuid`: both preload
//! constructors are disabled. Main dispatches directly here; setup and CpuidTool
//! use no POSIX timer APIs, and guest RPC is synchronous (the async coordinator
//! is a separate process). No timers are created or armed on the positive path.
//! The launcher must use trusted standard loader/runtime startup without added
//! preload/audit constructors. Exec deletes prior POSIX timers; that fact plus
//! this finite path, not missing procfs inventory, supplies the unsafe guarantee.
//! The interval-timer negative deliberately exercises install-time refusal only.
//! The additional seeded FP profile sets ZMM16–31 to all ones and verifies their
//! zeroing in each callback. Both profiles require full XSAVE byte equality; the
//! original unseeded profile remains a separate, unresolved live test obligation.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::CpuIdResult;
use reverie::Guest;
use reverie::Rdtsc;
use reverie::RdtscResult;
use reverie::Subscription;
use reverie::Tool;
use reverie_preload::trap::raw_syscall6;
#[path = "owned_step_fixture.rs"]
mod owned_step_fixture;
#[path = "owned_syscall.rs"]
mod owned_syscall;
#[path = "owned_timer.rs"]
mod owned_timer;
#[path = "xsave.rs"]
mod xsave;

#[path = "owned_clock.rs"]
mod owned_clock;

static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static FIRST_PC: AtomicU64 = AtomicU64::new(0);
static SEED_HI16_ZMM: AtomicBool = AtomicBool::new(false);
static NATIVE_SIGNALS: AtomicU64 = AtomicU64::new(0);
static MIXED_INSTRUCTIONS: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct CpuidTool;

#[reverie::tool]
impl Tool for CpuidTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = u32;

    fn subscriptions(_: &()) -> Subscription {
        let mut subscriptions = Subscription::default();
        subscriptions.cpuid();
        if MIXED_INSTRUCTIONS.load(Ordering::Relaxed) {
            subscriptions.rdtsc();
        }
        subscriptions
    }

    async fn handle_cpuid_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuIdResult, reverie::Errno> {
        let total = self.callback(guest, None, Some((eax, ecx))).await;
        Ok(CpuIdResult {
            eax: total as u32,
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
        let total = self.callback(guest, Some(request), None).await;
        Ok(RdtscResult {
            tsc: instruction_tsc(total),
            aux: if request == Rdtsc::Tscp {
                instruction_aux(total)
            } else {
                None
            },
        })
    }
}

impl CpuidTool {
    async fn callback<G: Guest<Self>>(
        &self,
        guest: &mut G,
        request: Option<Rdtsc>,
        cpuid_inputs: Option<(u32, u32)>,
    ) -> u64 {
        let ordinal = CALLBACKS.fetch_add(1, Ordering::Relaxed) + 1;
        assert_eq!(u64::from(*guest.thread_state()) + 1, ordinal);
        *guest.thread_state_mut() += 1;
        let odd = ordinal % 2 == 1;
        let mixed = MIXED_INSTRUCTIONS.load(Ordering::Relaxed);
        let position = (ordinal - 1) % 3;
        assert_eq!(
            request,
            if mixed {
                match position {
                    0 => None,
                    1 => Some(Rdtsc::Tsc),
                    _ => Some(Rdtsc::Tscp),
                }
            } else {
                None
            }
        );
        if let Some(inputs) = cpuid_inputs {
            assert!(request.is_none());
            assert_eq!(
                inputs,
                if mixed || odd {
                    (0xfeed, 0xbaad)
                } else {
                    ((ordinal - 1) as u32, 0x3333_3333)
                }
            );
        } else {
            assert!(request.is_some());
        }
        let registers = guest.regs().await;
        if mixed && position != 0 {
            assert_eq!(registers.rax, ordinal - 1);
            assert_eq!(registers.rbx, 0x2222_2222);
            assert_eq!(registers.rcx, 0x3333_3333);
            assert_eq!(
                registers.rdx,
                if position == 1 {
                    0x4444_4444
                } else {
                    0xfedc_ba98
                }
            );
        }
        for field in 0..5 {
            let mut edited = registers;
            match field {
                0 => edited.rbp ^= 8,
                1 => edited.rsp ^= 8,
                2 => edited.eflags ^= 1,
                3 => edited.rip ^= 2,
                4 => edited.rax ^= 1,
                _ => unreachable!(),
            }
            assert!(matches!(
                guest.set_regs(edited).await,
                Err(reverie::Error::Errno(reverie::Errno::EOPNOTSUPP))
            ));
            let unchanged = guest.regs().await;
            assert_eq!(
                (
                    unchanged.rbp,
                    unchanged.rsp,
                    unchanged.eflags,
                    unchanged.rip,
                    unchanged.rax
                ),
                (
                    registers.rbp,
                    registers.rsp,
                    registers.eflags,
                    registers.rip,
                    registers.rax
                )
            );
        }
        assert_eq!(
            registers.rip,
            FIRST_PC.load(Ordering::Relaxed)
                + if mixed {
                    position * 2
                } else if odd {
                    0
                } else {
                    2
                }
        );
        if mixed {
            let native = native_controls();
            assert_eq!(native[3], 1);
            assert_eq!(native[4], libc::PR_TSC_ENABLE as u64);
            assert_ne!(RdtscResult::new(Rdtsc::Tsc).tsc, instruction_tsc(ordinal));
            assert_eq!(CALLBACKS.load(Ordering::Relaxed), ordinal);
        }
        let local = 0u64;
        assert!(registers.rsp.abs_diff((&raw const local) as u64) > 64 * 1024);
        {
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
        }
        let (total, senders) = guest.send_rpc(1).await;
        assert_eq!((total, senders), (ordinal, 1));
        unsafe {
            *libc::__errno_location() = libc::EDOM;
            if SEED_HI16_ZMM.load(Ordering::Relaxed) {
                let mut clobbered = Floating([0xff; 2560]);
                core::arch::asm!(
                    "fninit",
                    "vzeroall",
                    ".irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31",
                    r"vpxord zmm\register, zmm\register, zmm\register",
                    r"vmovdqu64 [{image} + (\register - 16) * 64], zmm\register",
                    ".endr",
                    image = in(reg) clobbered.0.as_mut_ptr(),
                    clobber_abi("C"),
                    options(nostack),
                );
                assert!(
                    clobbered.0[..1024].iter().all(|byte| *byte == 0),
                    "Hi16_ZMM clobber callback={ordinal}"
                );
            } else {
                core::arch::asm!("fninit", "vzeroall", clobber_abi("C"), options(nostack));
            }
        }
        total
    }
}

fn instruction_tsc(total: u64) -> u64 {
    0xfedc_ba98_0000_0000 | total
}

fn instruction_aux(total: u64) -> Option<u32> {
    (total != 6).then_some(0xaaaa_0000 | total as u32)
}

#[repr(align(64))]
struct Floating([u8; 2560]);

unsafe extern "C" {
    fn owned_cpuid_probe(registers: *mut u64, before: *mut u8, after: *mut u8);
    fn owned_cpuid_probe_seeded(registers: *mut u64, before: *mut u8, after: *mut u8);
    fn owned_cpuid_first();
    fn owned_instruction_probe(registers: *mut u64, before: *mut u8, after: *mut u8);
    fn owned_instruction_first();
    fn native_xsave_probe(
        before: *mut u8,
        after: *mut u8,
        signal: u64,
        pid: u64,
        zero_high: u64,
    ) -> i64;
}

extern "C" fn native_xsave_handler(_: libc::c_int) {
    unsafe {
        core::arch::asm!("fninit", "vzeroall", clobber_abi("C"), options(nostack));
    }
    NATIVE_SIGNALS.fetch_add(1, Ordering::Relaxed);
}

fn run_xsave_control(mode: &str) {
    let signal = mode.ends_with("-signal");
    let zero_high = mode.contains("-zero-");
    assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
    let xstate = core::arch::x86_64::__cpuid_count(0xd, 0);
    let high = core::arch::x86_64::__cpuid_count(0xd, 7);
    assert_eq!((xstate.eax, xstate.edx, xstate.ebx), (0x2e7, 0, 2440));
    assert_eq!((high.eax, high.ebx), (1024, 1408));
    let original_controls = native_controls();
    assert_eq!(original_controls[5], 0x2e7);
    let original_mask = mask();
    assert_eq!(original_mask & (1 << (libc::SIGUSR1 - 1)), 0);
    let mut original_action: libc::sigaction = unsafe { core::mem::zeroed() };
    let mut action: libc::sigaction = unsafe { core::mem::zeroed() };
    let mut stack: libc::stack_t = unsafe { core::mem::zeroed() };
    unsafe {
        assert_eq!(libc::sigaltstack(core::ptr::null(), &mut stack), 0);
        assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
        action.sa_sigaction = native_xsave_handler as *const () as usize;
        if signal {
            assert_eq!(
                libc::sigaction(libc::SIGUSR1, &action, &mut original_action),
                0
            );
        }
    }
    let pid = unsafe { libc::getpid() } as u64;
    let mut before = Floating([0; 2560]);
    let mut after = Floating([0; 2560]);
    let result = unsafe {
        native_xsave_probe(
            before.0.as_mut_ptr(),
            after.0.as_mut_ptr(),
            u64::from(signal),
            pid,
            u64::from(zero_high),
        )
    };
    let signals = NATIVE_SIGNALS.load(Ordering::Relaxed);
    let after_mask = mask();
    let after_controls = native_controls();
    if signal {
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGUSR1, &original_action, core::ptr::null_mut()) },
            0
        );
    }
    println!(
        "xsave-control: mode={mode} signals={signals} syscall_result={result} mask={original_mask:#x} altstack_flags={} controls={original_controls:?}",
        stack.ss_flags
    );
    println!("xsave-before: {:?}", &before.0[..2440]);
    println!("xsave-after: {:?}", &after.0[..2440]);
    if zero_high {
        assert_eq!(
            u64::from_le_bytes(before.0[512..520].try_into().unwrap()),
            0x2a7,
            "required starting XSTATE_BV"
        );
        assert_eq!(&before.0[520..528], &[0; 8], "required starting XCOMP_BV");
        assert!(before.0[1408..2432].iter().all(|byte| *byte == 0));
    }
    assert_eq!(result, 0);
    assert_eq!(signals, u64::from(signal));
    assert_eq!(after_mask, original_mask);
    assert_eq!(after_controls, original_controls);
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), 0);
    let cleared = xsave::compare_native_xsave(&before.0[..2440], &after.0[..2440])
        .unwrap_or_else(|error| panic!("control={mode}: {error}"));
    println!(
        "xsave-control: payload_bytes=preserved other_header_fields=preserved initialized_hi16_zmm_bit_cleared={cleared} callbacks=0"
    );
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
    let mut controls = [0u64; 7];
    for (operation, output) in [0x1003, 0x1004, 0x1022].into_iter().zip(&mut controls[..3]) {
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_arch_prctl,
                    [operation, output as *mut u64 as u64, 0, 0, 0, 0],
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

pub(super) fn run(path: &Path, mode: &str) {
    super::sud_only_guest::deny_ptrace();
    if mode == "sud-owned-cpuid-clock-install-failure" {
        owned_clock::run_install_failure(path);
    }
    if matches!(
        mode,
        "sud-owned-cpuid-xsave-baseline"
            | "sud-owned-cpuid-xsave-signal"
            | "sud-owned-cpuid-xsave-zero-baseline"
            | "sud-owned-cpuid-xsave-zero-signal"
    ) {
        run_xsave_control(mode);
        return;
    }
    if mode == "sud-owned-cpuid-blocked" {
        let original = mask();
        let blocked = original | (1u64 << (libc::SIGSEGV - 1));
        let original_cpuid = unsafe { raw_syscall6(libc::SYS_arch_prctl, [0x1011, 0, 0, 0, 0, 0]) };
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const blocked) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        let error = unsafe { reverie_liteinst::__install_owned_cpuid_fixture::<CpuidTool>(path) }
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(mask(), blocked);
        assert_eq!(
            unsafe { raw_syscall6(libc::SYS_arch_prctl, [0x1011, 0, 0, 0, 0, 0]) },
            original_cpuid
        );
        assert_eq!(CALLBACKS.load(Ordering::Relaxed), 0);
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const original) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        println!("owned-cpuid-blocked: refused before activation mask=preserved callbacks=0");
        return;
    }
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        assert_ne!(
            unsafe { libc::signal(signal, libc::SIG_DFL) },
            libc::SIG_ERR
        );
    }
    if let Some(work) = mode.strip_prefix("sud-owned-cpuid-clock-") {
        owned_clock::run(path, work.parse().unwrap());
    }
    if mode == "sud-owned-cpuid-syscall-unarmed" {
        owned_syscall::run(path, false, 0);
    }
    if let Some(work) = mode.strip_prefix("sud-owned-cpuid-syscall-armed-") {
        owned_syscall::run(path, true, work.parse().unwrap());
    }
    if mode == "sud-owned-cpuid-step" {
        owned_step_fixture::run(path);
    }
    if let Some(work) = mode.strip_prefix("sud-owned-cpuid-timer-") {
        owned_timer::run(path, work.parse().unwrap());
    }
    if mode == "sud-owned-cpuid-timer" {
        let timer = libc::itimerval {
            it_interval: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            it_value: libc::timeval {
                tv_sec: 60,
                tv_usec: 0,
            },
        };
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_setitimer,
                    [
                        libc::ITIMER_REAL as u64,
                        (&raw const timer) as u64,
                        0,
                        0,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        let result = unsafe { reverie_liteinst::__install_owned_cpuid_fixture::<CpuidTool>(path) };
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(CALLBACKS.load(Ordering::Relaxed), 0);
        println!("owned-cpuid-timer: refused before callbacks");
        return;
    }
    let native = core::arch::x86_64::__cpuid_count(0xfeed, 0xbaad);
    let mixed = mode == "sud-owned-cpuid-mixed";
    MIXED_INSTRUCTIONS.store(mixed, Ordering::Relaxed);
    let seeded = mode == "sud-owned-cpuid-seeded" || mixed;
    let profile = if seeded {
        "seeded-hi16-zmm"
    } else {
        "unseeded"
    };
    SEED_HI16_ZMM.store(seeded, Ordering::Relaxed);
    if seeded {
        let hi16_zmm = core::arch::x86_64::__cpuid_count(0xd, 7);
        assert_eq!((hi16_zmm.eax, hi16_zmm.ebx), (1024, 1408));
    }
    assert_ne!(
        (native.eax, native.ebx, native.ecx, native.edx),
        (1, 0x2222_2222, 0x3333_3333, 0x4444_4444)
    );
    let site = if mixed {
        owned_instruction_first as *const ()
    } else {
        owned_cpuid_first as *const ()
    } as u64;
    FIRST_PC.store(site, Ordering::Relaxed);
    let instruction_bytes: &[u8] = if mixed {
        &[0x0f, 0xa2, 0x0f, 0x31, 0x0f, 0x01, 0xf9]
    } else {
        &[0x0f, 0xa2, 0x0f, 0xa2]
    };
    let original =
        unsafe { std::slice::from_raw_parts(site as *const u8, instruction_bytes.len()) }.to_vec();
    assert_eq!(original, instruction_bytes);
    let original_mask = mask();
    let mut expected_controls = native_controls();
    expected_controls[3] = 0;
    if mixed {
        expected_controls[4] = libc::PR_TSC_SIGSEGV as u64;
    }
    let timer_inventory = unsafe {
        if mixed {
            reverie_liteinst::__install_owned_instruction_fixture::<CpuidTool>(path)
        } else {
            reverie_liteinst::__install_owned_cpuid_fixture::<CpuidTool>(path)
        }
    }
    .unwrap();
    assert_eq!(native_controls(), expected_controls);
    if mode == "sud-owned-cpuid-control" {
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_prctl,
                    [
                        libc::PR_SET_TSC as u64,
                        libc::PR_TSC_SIGSEGV as u64,
                        0,
                        0,
                        0,
                        0,
                    ],
                )
            },
            0
        );
    } else if mode == "sud-owned-cpuid-source" {
        let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
        let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
        unsafe {
            raw_syscall6(
                libc::SYS_tgkill,
                [pid as u64, tid as u64, libc::SIGSEGV as u64, 0, 0, 0],
            )
        };
        panic!("non-instruction source returned");
    }
    for pair in 1..=3 {
        let mut registers = [0u64; 18];
        let mut before = Floating([0; 2560]);
        let mut after = Floating([0; 2560]);
        unsafe {
            *libc::__errno_location() = libc::E2BIG;
            let probe = if mixed {
                owned_instruction_probe
            } else if seeded {
                owned_cpuid_probe_seeded
            } else {
                owned_cpuid_probe
            };
            probe(
                registers.as_mut_ptr(),
                before.0.as_mut_ptr(),
                after.0.as_mut_ptr(),
            );
        }
        assert_eq!(unsafe { *libc::__errno_location() }, libc::E2BIG);
        assert_eq!(
            &registers[..4],
            &if mixed {
                [
                    pair * 3,
                    0x2222_2222,
                    u64::from(instruction_aux(pair * 3).unwrap_or(0)),
                    0xfedc_ba98,
                ]
            } else {
                [pair * 2, 0x2222_2222, 0x3333_3333, 0x4444_4444]
            }
        );
        assert_eq!(registers[4], before.0.as_ptr() as u64);
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
        if seeded {
            assert_ne!(before.0[512] & 0x80, 0, "Hi16_ZMM input pair={pair}");
            assert!(
                before.0[1408..2432].iter().all(|byte| *byte == 0xff),
                "Hi16_ZMM pre-call image pair={pair} callbacks={}",
                CALLBACKS.load(Ordering::Relaxed)
            );
        }
        assert_eq!(
            &before.0[..2440],
            &after.0[..2440],
            "profile={profile} pair={pair} callbacks={}",
            CALLBACKS.load(Ordering::Relaxed)
        );
        assert_eq!(mask(), original_mask);
        assert_eq!(native_controls(), expected_controls);
    }
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), if mixed { 9 } else { 6 });
    assert_eq!(
        unsafe { std::slice::from_raw_parts(site as *const u8, instruction_bytes.len()) },
        original
    );
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
    if mixed {
        println!(
            "owned-instructions: cpuid=3 rdtsc=3 rdtscp=3 consecutive=9 rpc=9 state=preserved bytes=unchanged patches=0 posix_inventory={timer_inventory:?} fp_profile={profile}"
        );
    } else {
        println!(
            "owned-cpuid: consecutive=6 rpc=6 state=preserved bytes=unchanged patches=0 posix_inventory={timer_inventory:?} fp_profile={profile}"
        );
    }
}

core::arch::global_asm!(
    r#"
.text
.global owned_cpuid_probe_seeded
.hidden owned_cpuid_probe_seeded
.type owned_cpuid_probe_seeded,@function
owned_cpuid_probe_seeded:
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    jmp owned_cpuid_probe
.size owned_cpuid_probe_seeded, .-owned_cpuid_probe_seeded
.macro owned_probe_prefix
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    sub rsp, 40
    mov [rsp], rdi
    mov [rsp + 8], rsi
    mov [rsp + 16], rdx
    stmxcsr [rsp + 24]
    mov [rdi + 128], rsp
    fninit
    fld1
    fldpi
    vpcmpeqd ymm0, ymm0, ymm0
    vpcmpeqd ymm1, ymm1, ymm1
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [rsi]
    mov rax, 0xfedcba980000feed
    mov rcx, 0xfedcba980000baad
    mov edx, 33
    mov rbp, 0x7272727272727272
    mov r8, 0x5555555555555555
    mov r9, 0x6666666666666666
    mov r10, 0x4444444444444444
    mov r11, 0x8888888888888888
    mov r12, 0x7171717171717171
    mov r13, 0x7373737373737373
    mov r14, 0x7474747474747474
    mov r15, 0x7575757575757575
    .irp offset,16,24,32,40,48,56,64,72,80,88,96,104,112,120,128
    mov qword ptr [rsp - \offset], 123456
    .endr
    push 0xed7
    popfq
.endm
.macro owned_probe_suffix pause=0
    mov [rdi], rax
    mov [rdi + 8], rbx
    mov [rdi + 16], rcx
    mov [rdi + 24], rdx
    mov [rdi + 32], rsi
    mov [rdi + 40], rdi
    mov [rdi + 48], rbp
    mov [rdi + 56], r8
    mov [rdi + 64], r9
    mov [rdi + 72], r10
    mov [rdi + 80], r11
    mov [rdi + 88], r12
    mov [rdi + 96], r13
    mov [rdi + 104], r14
    mov [rdi + 112], r15
    mov [rdi + 120], rsp
    pushfq
    pop qword ptr [rdi + 136]
    cld
    mov r11, [rsp + 16]
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [r11]
    .if \pause
    mov eax, 3
8:
    dec eax
    jnz 8b
    xor edi, edi
    lea rsp, [rsp - 128]
    call reverie_liteinst_clock_enter
    lea rsp, [rsp + 128]
    cmp rax, 1
    jne 9f
    .endif
    .irp offset,16,24,32,40,48,56,64,72,80,88,96,104,112,120,128
    cmp qword ptr [rsp - \offset], 123456
    jne 9f
    .endr
    cmp qword ptr [rsp - 8], 0xed7
    jne 9f
    fninit
    ldmxcsr [rsp + 24]
    vzeroupper
    add rsp, 40
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret
9:
    ud2
.endm
.global owned_cpuid_probe
.hidden owned_cpuid_probe
.type owned_cpuid_probe,@function
owned_cpuid_probe:
    owned_probe_prefix
.global owned_cpuid_first
.hidden owned_cpuid_first
owned_cpuid_first:
    cpuid
    cpuid
    owned_probe_suffix
.size owned_cpuid_probe, .-owned_cpuid_probe

.global owned_instruction_probe
.hidden owned_instruction_probe
.type owned_instruction_probe,@function
owned_instruction_probe:
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vpternlogd zmm\register, zmm\register, zmm\register, 0xff
    .endr
    owned_probe_prefix
.global owned_instruction_first
.hidden owned_instruction_first
owned_instruction_first:
    cpuid
    rdtsc
    rdtscp
    owned_probe_suffix
.size owned_instruction_probe, .-owned_instruction_probe

.global native_xsave_probe
.hidden native_xsave_probe
.type native_xsave_probe,@function
native_xsave_probe:
    push r12
    push r13
    push r14
    push r15
    sub rsp, 16
    mov r12, rdi
    mov r13, rsi
    mov r14, rdx
    mov r15, rcx
    stmxcsr [rsp]
    test r8, r8
    jz 7f
    .irp register,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    vmovdqu64 zmm\register, [r13]
    .endr
7:
    fninit
    fld1
    fldpi
    vpcmpeqd ymm0, ymm0, ymm0
    vpcmpeqd ymm1, ymm1, ymm1
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [r12]
    test r14, r14
    jz 8f
    mov eax, {tgkill}
    mov rdi, r15
    mov rsi, r15
    mov edx, {user_signal}
    syscall
    mov r14, rax
8:
    mov eax, 0x2e7
    xor edx, edx
    xsave64 [r13]
    fninit
    ldmxcsr [rsp]
    vzeroupper
    mov rax, r14
    add rsp, 16
    pop r15
    pop r14
    pop r13
    pop r12
    ret
.size native_xsave_probe, .-native_xsave_probe
"#,
    include_str!("owned_clock.s"),
    include_str!("owned_step_fixture.s"),
    include_str!("owned_timer.s"),
    include_str!("owned_syscall.s"),
    syscall_initialize = sym owned_syscall::initialize,
    syscall_verify = sym owned_syscall::verify,
    syscall_registers_offset = const core::mem::offset_of!(owned_syscall::Probe, registers),
    syscall_before_offset = const core::mem::offset_of!(owned_syscall::Probe, before),
    syscall_after_offset = const core::mem::offset_of!(owned_syscall::Probe, after),
    syscall_fd = sym owned_syscall::FD,
    syscall_buffer = sym owned_syscall::BUFFER,
    syscall_results = sym owned_syscall::RESULTS,
    timer_initialize = sym owned_timer::initialize,
    timer_verify = sym owned_timer::verify,
    timer_registers_offset = const core::mem::offset_of!(owned_timer::Probe, registers),
    timer_before_offset = const core::mem::offset_of!(owned_timer::Probe, before),
    timer_after_offset = const core::mem::offset_of!(owned_timer::Probe, after),
    step_initialize = sym owned_step_fixture::initialize,
    step_verify = sym owned_step_fixture::verify,
    step_registers_offset = const core::mem::offset_of!(owned_step_fixture::Probe, registers),
    step_before_offset = const core::mem::offset_of!(owned_step_fixture::Probe, before),
    step_after_offset = const core::mem::offset_of!(owned_step_fixture::Probe, after),
    tgkill = const libc::SYS_tgkill,
    user_signal = const libc::SIGUSR1,
    clock_begin = sym reverie_liteinst::__clock_constructor_begin,
    clock_finish = sym reverie_liteinst::__clock_constructor_finish,
    clock_initialize = sym owned_clock::initialize,
    clock_verify = sym owned_clock::verify,
    clock_outputs = sym owned_clock::OUTPUTS,
    clock_registers_offset = const core::mem::offset_of!(owned_clock::Probe, registers),
    clock_before_offset = const core::mem::offset_of!(owned_clock::Probe, before),
    clock_after_offset = const core::mem::offset_of!(owned_clock::Probe, after),
);
