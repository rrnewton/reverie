use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use reverie::Errno;
use reverie::TimerSchedule;
use reverie::pmu::InGuestRcbCounter;
use reverie_preload::trap::raw_syscall6;

use super::notification_tests::guest_interval;
use super::notification_tests::raw_count;

static NOTIFICATION_FD: AtomicU64 = AtomicU64::new(0);
static INJECT: AtomicU64 = AtomicU64::new(0);
static ACTION: AtomicU64 = AtomicU64::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);

#[unsafe(naked)]
unsafe extern "C" fn interrupted_gate() {
    core::arch::naked_asm!(
        "lea r11, [rip + 2f]",
        "lea rcx, [rip + 3f]",
        "cmp eax, 16", "cmovne rcx, r11",
        "cmp esi, 0x2400", "cmovne rcx, r11",
        "cmp rdi, [rip + {descriptor}]", "cmovne rcx, r11",
        "cmp qword ptr [rip + {inject}], 0", "cmove rcx, r11",
        "jmp rcx",
        "2:", "syscall", "ret",
        "3:",
        "mov qword ptr [rip + {inject}], 0",
        "sub rsp, 72",
        "mov [rsp], rax", "mov [rsp + 8], rdi",
        "mov [rsp + 16], rsi", "mov [rsp + 24], rdx",
        "mov [rsp + 32], r10", "mov [rsp + 40], r8",
        "mov [rsp + 48], r9",
        "xor edi, edi", "call {enter}", "mov [rsp + 56], rax",
        "call {body}",
        "mov rdi, [rsp + 56]", "xor esi, esi", "xor edx, edx", "call {leave}",
        "mov rax, [rsp]", "mov rdi, [rsp + 8]",
        "mov rsi, [rsp + 16]", "mov rdx, [rsp + 24]",
        "mov r10, [rsp + 32]", "mov r8, [rsp + 40]",
        "mov r9, [rsp + 48]", "add rsp, 72",
        "syscall", "ret",
        descriptor = sym NOTIFICATION_FD,
        inject = sym INJECT,
        enter = sym super::reverie_liteinst_clock_enter,
        leave = sym super::reverie_liteinst_clock_leave,
        body = sym nested_body,
    );
}

extern "C" fn nested_body() {
    assert!(super::paused());
    CALLS.fetch_add(1, Ordering::Relaxed);
    let owner = unsafe { &*super::notification_owner() };
    let state = super::control();
    match ACTION.load(Ordering::Relaxed) {
        0 => {
            assert!(crate::timer::request(TimerSchedule::Rcbs(100), false).is_err());
            assert_eq!(owner.with_timer(|timer| Ok(timer.arm_id())).unwrap(), None);
            assert_eq!(state.notification_armed.load(Ordering::Relaxed), 0);
        }
        1 => {
            owner
                .with_timer(|timer| {
                    let old = timer.arm_id().unwrap();
                    assert!(timer.prepare_after(0, 2_000_000)? > old);
                    state.notification_armed.store(1, Ordering::Release);
                    Ok(())
                })
                .unwrap();
        }
        2 => {
            owner
                .with_timer(|timer| {
                    let old = timer.arm_id();
                    let nested = unsafe { super::reverie_liteinst_clock_enter(0) };
                    assert_eq!(nested, 0);
                    assert_eq!(owner.with_timer(|timer| timer.disarm()), Err(Errno::EBUSY));
                    assert!(crate::timer::request(TimerSchedule::Rcbs(100), true).is_err());
                    assert_eq!(timer.arm_id(), old);
                    assert_eq!(state.notification_armed.load(Ordering::Relaxed), 1);
                    unsafe { super::reverie_liteinst_clock_leave(nested, 1, 0) };
                    Ok(())
                })
                .unwrap();
        }
        _ => panic!("unexpected injection action"),
    }
    for iteration in 0..10_000 {
        std::hint::black_box(iteration);
    }
}

#[test]
fn interrupted_notification_enable_reconciles_cancel_replace_and_nested_borrow() {
    for action in 0..3 {
        std::thread::spawn(move || {
            let mut mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
            unsafe {
                assert_eq!(libc::sigemptyset(&mut mask), 0);
                assert_eq!(libc::sigaddset(&mut mask, reverie::PERF_EVENT_SIGNAL as i32), 0);
                assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()), 0);
            }
            let clock = unsafe { InGuestRcbCounter::current_thread_disabled_with_syscall_gate(raw_syscall6) }
                .expect("real PMU clock required; hardware failures are not skipped");
            let state = super::control();
            state.descriptor.store(unsafe { clock.boundary_fd() }.as_raw_fd() as u64, Ordering::Relaxed);
            state.gate.store(interrupted_gate as *const () as u64, Ordering::Relaxed);
            state.owner.store(unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as u64, Ordering::Relaxed);
            state.ready.store(1, Ordering::Release);
            assert_eq!(unsafe { super::reverie_liteinst_clock_enter(0) }, 0);
            crate::timer::initialize().unwrap();
            let owner = unsafe { &*super::notification_owner() };
            owner.with_timer(|timer| {
                timer.prepare_after(0, 1_000_000)?;
                state.notification_armed.store(1, Ordering::Release);
                Ok(())
            }).unwrap();
            NOTIFICATION_FD.store(owner.descriptor() as u64, Ordering::Relaxed);
            ACTION.store(action, Ordering::Relaxed);
            CALLS.store(0, Ordering::Relaxed);
            INJECT.store(1, Ordering::Relaxed);
            let mut expected = 0;
            let mut trajectory = vec![0];
            for branches in [1, 2, 17, 33, 1, 9] {
                assert_eq!(unsafe { guest_interval(branches) }, 1);
                expected += branches;
                assert_eq!(unsafe { clock.read_paused_once() }.unwrap(), expected);
                let notification_expected = if action == 0 { 0 } else { expected };
                assert_eq!(raw_count(owner.descriptor()), notification_expected);
                trajectory.push(expected);
            }
            assert_eq!(CALLS.load(Ordering::Relaxed), 1);
            assert_eq!(INJECT.load(Ordering::Relaxed), 0);
            assert_eq!(trajectory, [0, 1, 3, 20, 53, 54, 63]);
            assert!(crate::timer::request(TimerSchedule::Rcbs(100), true).is_err());
            unsafe { super::reverie_liteinst_clock_leave(0, 1, 0) };
            state.ready.store(0, Ordering::Release);
            eprintln!("interrupted enable action={action}: full cumulative trajectory={trajectory:?}; nested calls=1; notification={}", raw_count(owner.descriptor()));
        }).join().unwrap();
    }
}
