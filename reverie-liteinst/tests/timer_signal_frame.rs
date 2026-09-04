use std::os::fd::AsRawFd;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

use reverie_preload::trap::raw_syscall6;
use reverie_ptrace::InGuestRcbCounter;
use reverie_ptrace::InGuestRcbTimer;

const AUTODISARM: i32 = i32::MIN;
const ITERATIONS: u64 = 1_000_000;
const SENTINEL: u64 = 0x123456789abc;

#[repr(C)]
struct EntryRecord {
    fd: u64,
    mode: u64,
    runtime_top: u64,
    clock: *const libc::c_void,
    timer: *mut libc::c_void,
    alt_low: usize,
    alt_high: usize,
    runtime_low: usize,
    runtime_high: usize,
    outer_count: u64,
    nested_count: u64,
    entry_count: u64,
    workload: u64,
    saved_rbx: u64,
    saved_xmm: u64,
    saved_redzone: u64,
    saved_flags: u64,
}

core::arch::global_asm!(
    include_str!("timer_signal_frame/entry.s"),
    body = sym outer_body,
    saved_rbx = const std::mem::offset_of!(EntryRecord, saved_rbx),
    saved_xmm = const std::mem::offset_of!(EntryRecord, saved_xmm),
    saved_redzone = const std::mem::offset_of!(EntryRecord, saved_redzone),
    saved_flags = const std::mem::offset_of!(EntryRecord, saved_flags),
    sentinel = const SENTINEL,
);

unsafe extern "C" {
    fn timer_frame_set_record(record: *mut EntryRecord);
    fn timer_frame_get_record() -> *mut EntryRecord;
    fn timer_frame_entry(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void);
    fn timer_frame_guest(record: *mut EntryRecord, iterations: u64);
    static timer_frame_loop_begin: u8;
    static timer_frame_loop_end: u8;
}

fn require(condition: bool, status: u64) {
    if !condition {
        unsafe { raw_syscall6(libc::SYS_exit_group, [status, 0, 0, 0, 0, 0]) };
        std::process::abort();
    }
}

unsafe extern "C" fn nested_handler(
    signal: i32,
    _info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    unsafe {
        let record = timer_frame_get_record();
        require(signal == libc::SIGUSR2 && (*record).mode == 1, 81);
        let address = context as usize;
        require(
            address >= (*record).runtime_low && address < (*record).runtime_high,
            82,
        );
        require(
            address < (*record).alt_low || address >= (*record).alt_high,
            83,
        );
        (*record).nested_count += 1;
        core::arch::asm!("pxor xmm0, xmm0", out("xmm0") _, options(nostack));
    }
}

unsafe extern "C" fn outer_body(
    record: *mut EntryRecord,
    info: *mut libc::siginfo_t,
    context: *mut libc::ucontext_t,
) {
    unsafe {
        require((*record).mode == 1, 84);
        require(matches!((*info).si_code, 1 | 6), 85);
        require((*record).outer_count == 0, 86);
        (*record).outer_count += 1;
        require(
            (*(*record).timer.cast::<InGuestRcbTimer>())
                .disarm()
                .is_ok(),
            87,
        );
        let address = context as usize;
        require(
            address >= (*record).alt_low && address < (*record).alt_high,
            88,
        );
        let registers = (*context).uc_mcontext.gregs;
        let instruction = registers[libc::REG_RIP as usize] as usize;
        require(
            instruction >= (&raw const timer_frame_loop_begin) as usize
                && instruction < (&raw const timer_frame_loop_end) as usize,
            89,
        );
        require(registers[libc::REG_EFL as usize] & 0x401 == 0x401, 90);
        (*record).entry_count =
            match (*(*record).clock.cast::<InGuestRcbCounter>()).read_paused_once() {
                Ok(value) => value,
                Err(_) => {
                    require(false, 91);
                    return;
                }
            };
        let mut stack: libc::stack_t = std::mem::zeroed();
        require(
            raw_syscall6(
                libc::SYS_sigaltstack,
                [0, (&raw mut stack) as u64, 0, 0, 0, 0],
            ) == 0
                && stack.ss_flags & libc::SS_DISABLE != 0,
            92,
        );
        let floating = (*context).uc_mcontext.fpregs.cast::<u8>();
        require(!floating.is_null(), 93);
        let mut saved_floating = [0u8; 512];
        std::ptr::copy_nonoverlapping(floating, saved_floating.as_mut_ptr(), 512);
        let mut canaries = [SENTINEL; 32];
        let canary_address = (&raw mut canaries) as usize;
        require(
            canary_address >= (*record).runtime_low
                && canary_address + std::mem::size_of_val(&canaries) <= (*record).runtime_high,
            94,
        );
        for index in 0..canaries.len() {
            std::ptr::write_volatile(canaries.as_mut_ptr().add(index), SENTINEL);
        }
        let pid = raw_syscall6(libc::SYS_getpid, [0; 6]);
        let tid = raw_syscall6(libc::SYS_gettid, [0; 6]);
        require(
            raw_syscall6(
                libc::SYS_tgkill,
                [pid as u64, tid as u64, libc::SIGUSR2 as u64, 0, 0, 0],
            ) == 0,
            95,
        );
        require((*record).nested_count == 1, 96);
        require(
            raw_syscall6(
                libc::SYS_sigaltstack,
                [0, (&raw mut stack) as u64, 0, 0, 0, 0],
            ) == 0
                && stack.ss_flags & libc::SS_DISABLE != 0,
            107,
        );
        for index in 0..canaries.len() {
            require(
                std::ptr::read_volatile(canaries.as_ptr().add(index)) == SENTINEL,
                97,
            );
        }
        require((*context).uc_mcontext.gregs == registers, 98);
        for (index, expected) in saved_floating.iter().enumerate() {
            require(
                std::ptr::read_volatile(floating.add(index)) == *expected,
                99,
            );
        }
        for work in 0..(*record).workload {
            std::hint::black_box(work);
        }
        require(
            (*(*record).clock.cast::<InGuestRcbCounter>()).read_paused_once()
                == Ok((*record).entry_count),
            100,
        );
        core::arch::asm!("pxor xmm0, xmm0", out("xmm0") _, options(nostack));
    }
}

fn guarded_stack() -> (usize, usize) {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let length = 256 * 1024;
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length + 2 * page,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let low = mapping as usize + page;
    assert_eq!(
        unsafe { libc::mprotect(low as *mut _, length, libc::PROT_READ | libc::PROT_WRITE) },
        0
    );
    (low, low + length)
}

fn child(workload: u64) {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGUSR1);
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()),
            0
        );
        let clock = Box::new(
            InGuestRcbCounter::current_thread_disabled_with_syscall_gate(raw_syscall6).unwrap(),
        );
        let mut timer = Box::new(
            InGuestRcbTimer::current_thread_with_syscall_gate(
                raw_syscall6,
                reverie::Signal::SIGUSR1,
            )
            .unwrap(),
        );
        let (alt_low, alt_high) = guarded_stack();
        let (runtime_low, runtime_high) = guarded_stack();
        let alt_stack = libc::stack_t {
            ss_sp: alt_low as *mut _,
            ss_flags: AUTODISARM,
            ss_size: alt_high - alt_low,
        };
        assert_eq!(libc::sigaltstack(&alt_stack, std::ptr::null_mut()), 0);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        libc::sigemptyset(&mut action.sa_mask);
        action.sa_sigaction = timer_frame_entry as *const () as usize;
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &action, std::ptr::null_mut()),
            0
        );
        action.sa_sigaction = nested_handler as *const () as usize;
        assert_eq!(
            libc::sigaction(libc::SIGUSR2, &action, std::ptr::null_mut()),
            0
        );
        let mut record = EntryRecord {
            fd: clock.boundary_fd().as_raw_fd() as u64,
            mode: 0,
            runtime_top: runtime_high as u64,
            clock: (&raw const *clock).cast(),
            timer: (&raw mut *timer).cast(),
            alt_low,
            alt_high,
            runtime_low,
            runtime_high,
            outer_count: 0,
            nested_count: 0,
            entry_count: 0,
            workload,
            saved_rbx: 0,
            saved_xmm: 0,
            saved_redzone: 0,
            saved_flags: 0,
        };
        timer_frame_set_record(&mut record);
        assert_eq!(clock.read_paused_once(), Ok(0));
        timer.arm_after(0, ITERATIONS / 2).unwrap();
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()),
            0
        );
        timer_frame_guest(&mut record, ITERATIONS);
        require(
            record.outer_count == 1 && record.nested_count == 1 && record.mode == 0,
            101,
        );
        require(clock.read_paused_once() == Ok(ITERATIONS), 102);
        require(
            record.saved_rbx == SENTINEL
                && record.saved_xmm == SENTINEL
                && record.saved_redzone == SENTINEL,
            103,
        );
        require(record.saved_flags & 0x401 == 0x401, 104);
        let mut restored: libc::stack_t = std::mem::zeroed();
        require(
            raw_syscall6(
                libc::SYS_sigaltstack,
                [0, (&raw mut restored) as u64, 0, 0, 0, 0],
            ) == 0,
            105,
        );
        require(
            restored.ss_sp == alt_stack.ss_sp && restored.ss_flags == AUTODISARM,
            106,
        );
        println!(
            "prerequisite workload={workload} branches={ITERATIONS} entry={} outer=1 nested=1 canaries=preserved",
            record.entry_count
        );
        raw_syscall6(libc::SYS_exit_group, [0; 6]);
    }
}

#[test]
fn live_frame_clock_and_nested_stack_prerequisite() {
    if let Ok(workload) = std::env::var("REVERIE_TIMER_FRAME_CHILD") {
        child(workload.parse().unwrap());
        unreachable!();
    }
    for workload in [0, 100, 10000] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "live_frame_clock_and_nested_stack_prerequisite",
                "--nocapture",
            ])
            .env("REVERIE_TIMER_FRAME_CHILD", workload.to_string())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "workload={workload}: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("live signal-frame prerequisite exceeded 15 seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
