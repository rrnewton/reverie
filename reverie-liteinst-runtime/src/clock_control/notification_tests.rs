use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;

use reverie::TimerSchedule;
use reverie::pmu::InGuestRcbCounter;
use reverie::pmu::InGuestRcbDeadlineStatus;
use reverie_preload::trap::raw_syscall6;

#[unsafe(naked)]
pub(super) unsafe extern "C" fn guest_interval(_branches: u64) -> u64 {
    core::arch::naked_asm!(
        "push r12", "mov r12, rdi",
        "mov edi, 1", "xor esi, esi", "xor edx, edx", "call {leave}",
        "mov rcx, r12", "2:", "dec rcx", "jnz 2b",
        "xor edi, edi", "call {enter}", "pop r12", "ret",
        leave = sym super::reverie_liteinst_clock_leave,
        enter = sym super::reverie_liteinst_clock_enter,
    );
}

pub(super) fn raw_count(descriptor: i32) -> u64 {
    let mut count = 0_u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_read,
                [descriptor as u64, (&raw mut count) as u64, 8, 0, 0, 0],
            )
        },
        8
    );
    count
}

#[test]
fn real_notification_boundaries_preserve_clock_cancel_and_pending_ownership() {
    std::thread::spawn(|| {
        let signal = reverie::PERF_EVENT_SIGNAL as i32;
        let mut mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        unsafe {
            assert_eq!(libc::sigemptyset(&mut mask), 0);
            assert_eq!(libc::sigaddset(&mut mask, signal), 0);
            assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()), 0);
        }
        let clock = unsafe { InGuestRcbCounter::current_thread_disabled_with_syscall_gate(raw_syscall6) }
            .expect("real PMU clock required; hardware failures are not skipped");
        let state = super::control();
        state.descriptor.store(unsafe { clock.boundary_fd() }.as_raw_fd() as u64, Ordering::Relaxed);
        state.gate.store(reverie_preload::trap::trusted_gate().syscall_ip, Ordering::Relaxed);
        state.owner.store(unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as u64, Ordering::Relaxed);
        state.ready.store(1, Ordering::Release);
        assert_eq!(unsafe { super::reverie_liteinst_clock_enter(0) }, 0);
        crate::timer::initialize().unwrap();
        let owner = unsafe { &*super::notification_owner() };
        assert!(crate::timer::initialize().is_err());
        let descriptor = owner.descriptor();
        assert_eq!(super::notification_descriptor(), descriptor);
        let first = owner.with_timer(|timer| timer.prepare_after(0, 1_000_000)).unwrap();
        state.notification_armed.store(1, Ordering::Release);
        let mut expected = 0;
        let mut trajectory = vec![0];
        for branches in [1, 2, 17, 33, 1, 9] {
            for iteration in 0..10_000 {
                std::hint::black_box(iteration);
            }
            assert_eq!(raw_count(descriptor), expected);
            assert_eq!(unsafe { clock.read_paused_once() }.unwrap(), expected);
            assert_eq!(unsafe { guest_interval(branches) }, 1);
            expected += branches;
            assert_eq!(raw_count(descriptor), expected);
            assert_eq!(unsafe { clock.read_paused_once() }.unwrap(), expected);
            trajectory.push(expected);
        }
        assert_eq!(trajectory, [0, 1, 3, 20, 53, 54, 63]);
        assert!(crate::timer::request(TimerSchedule::Rcbs(100), true).is_err());
        assert_eq!(state.notification_armed.load(Ordering::Relaxed), 0);
        assert_eq!(owner.with_timer(|timer| Ok(timer.arm_id())).unwrap(), None);
        assert_eq!(unsafe { guest_interval(1) }, 1);
        expected += 1;
        assert_eq!(raw_count(descriptor), 63);
        assert_eq!(unsafe { clock.read_paused_once() }.unwrap(), expected);

        let second = owner.with_timer(|timer| timer.prepare_after(expected, 1_000_000)).unwrap();
        assert!(second > first);
        state.notification_armed.store(1, Ordering::Release);
        assert_eq!(owner.with_timer(|timer| timer.observe_notification(first, expected)).unwrap(), InGuestRcbDeadlineStatus::Cancelled);
        assert_eq!(owner.with_timer(|timer| Ok(timer.arm_id())).unwrap(), Some(second));
        assert_eq!(unsafe { guest_interval(2) }, 1);
        expected += 2;
        assert_eq!(raw_count(descriptor), 2);
        assert_eq!(unsafe { clock.read_paused_once() }.unwrap(), expected);

        let queued = owner.with_timer(|timer| timer.prepare_after(expected, 100)).unwrap();
        state.notification_armed.store(1, Ordering::Release);
        assert_eq!(unsafe { guest_interval(100_000) }, 1);
        expected += 100_000;
        assert_eq!(unsafe { clock.read_paused_once() }.unwrap(), expected);
        assert!(crate::timer::request(TimerSchedule::Rcbs(100), false).is_err());
        let current = owner.with_timer(|timer| timer.prepare_after(expected, 1_000_000)).unwrap();
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let timeout = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        assert_eq!(unsafe { libc::sigtimedwait(&mask, &mut info, &timeout) }, signal);
        assert!(matches!(info.si_code, 1 | 6));
        assert_eq!(owner.with_timer(|timer| timer.observe_notification(queued, expected)).unwrap(), InGuestRcbDeadlineStatus::Cancelled);
        assert_eq!(owner.with_timer(|timer| Ok(timer.arm_id())).unwrap(), Some(current));
        assert!(crate::timer::request(TimerSchedule::RcbsAndInstructions(0, 1), true).is_err());
        assert_eq!(unsafe { libc::sigtimedwait(&mask, &mut info, &timeout) }, -1);
        assert_eq!(unsafe { *libc::__errno_location() }, libc::EAGAIN);
        assert_eq!(unsafe { clock.read_paused_once() }.unwrap(), expected);
        assert_eq!(descriptor, owner.descriptor());
        unsafe { super::reverie_liteinst_clock_leave(0, 1, 0) };
        state.ready.store(0, Ordering::Release);
        eprintln!("paired notification/clock trajectory={trajectory:?}; final clock={expected}; cancellation and queued old arm retained");
    }).join().unwrap();
}
