//! Read-only runtime predicates and shared observations for the bounded log fixture.
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

use reverie_rpc_transport::guest_log::fixture::Control;

static CONTROL: AtomicPtr<Control> = AtomicPtr::new(std::ptr::null_mut());
static CLOCKED: AtomicBool = AtomicBool::new(false);

pub fn predicates() -> u64 {
    u64::from(crate::clock_control::requested())
        | (u64::from(crate::clock_control::active()) << 1)
        | (u64::from(crate::clock_control::paused()) << 2)
        | (u64::from(crate::runtime_domain::allocation_active()) << 3)
}

/// # Safety
/// One selected constructor, before application threads; the descriptor is the
/// trusted runner's sealed fixture mapping. No concurrent installation is allowed.
pub unsafe fn attach(fd: i32, clocked: bool) -> io::Result<()> {
    let control = Box::new(unsafe { Control::attach(fd)? });
    if !CONTROL.load(Ordering::Acquire).is_null() {
        return Err(io::Error::other("fixture installed twice"));
    }
    CLOCKED.store(clocked, Ordering::Relaxed);
    CONTROL.store(Box::into_raw(control), Ordering::Release);
    Ok(())
}
fn control() -> Option<&'static Control> {
    unsafe { CONTROL.load(Ordering::Acquire).as_ref() }
}
fn valid(bits: u64) -> bool {
    if CLOCKED.load(Ordering::Relaxed) {
        bits & 14 == 14
    } else {
        bits & 3 == 0
    }
}
pub fn startup(after_install: bool) -> bool {
    let Some(control) = control() else {
        return false;
    };
    let bits = predicates();
    control.observations().startup[usize::from(after_install)].store(bits | 16, Ordering::Release);
    let valid = if CLOCKED.load(Ordering::Relaxed) && !after_install {
        bits & 9 == 9
    } else {
        valid(bits)
    };
    if !valid {
        control
            .observations()
            .invalid
            .fetch_add(1, Ordering::Release);
    }
    valid
}

pub fn install_result(result: &io::Result<()>) {
    if let Some(control) = control() {
        let seen = control.observations();
        let mut mask = 0u64;
        let query = unsafe {
            reverie_preload::trap::raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut mask) as u64, 8, 0, 0],
            )
        };
        for (field, value) in seen.signal_policy.iter().zip([
            u64::from(reverie_preload::signal::runtime_signals_configured()),
            reverie_preload::signal::runtime_signal_mask(),
            mask,
            query as u64,
        ]) {
            field.store(value, Ordering::Relaxed);
        }
        seen.record_install_result(result);
    }
}
pub fn callback(index: usize, clock: Option<u64>) {
    let observations = control().expect("fixture attached").observations();
    if let Some(clock) = clock {
        observations.clocks[index].store(clock, Ordering::Relaxed);
    }
    observations.callbacks.fetch_add(1, Ordering::Release);
}
pub fn record_commit(index: usize, order: u64) {
    let seen = control().expect("fixture attached").observations();
    assert_eq!(seen.guest_orders[index].swap(order, Ordering::Release), 0);
}
pub fn record_failed() {
    control()
        .expect("fixture attached")
        .observations()
        .record_failed
        .store(1, Ordering::Release);
}
pub fn await_prefix() {
    let control = control().expect("fixture attached");
    control.observations().v4_gate.store(2, Ordering::Release);
    wait_fixture_gate(control);
}

fn wait_fixture_gate(control: &Control) {
    let seen = control.observations();
    assert!(valid(predicates()));
    let clock = if CLOCKED.load(Ordering::Relaxed) {
        crate::runtime::read_guest_rcb_clock().unwrap()
    } else {
        u64::MAX
    };
    seen.v4_gate_clocks[0].store(clock, Ordering::Release);
    while control.gated() {
        assert!(valid(predicates()));
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        let result = unsafe {
            reverie_preload::trap::raw_syscall6(
                libc::SYS_futex,
                [
                    control.gate_address() as u64,
                    libc::FUTEX_WAIT as u64,
                    1,
                    (&raw const timeout) as u64,
                    0,
                    0,
                ],
            )
        };
        assert!(
            [
                0,
                -i64::from(libc::EINTR),
                -i64::from(libc::EAGAIN),
                -i64::from(libc::ETIMEDOUT)
            ]
            .contains(&result)
        );
        assert!(valid(predicates()));
        if CLOCKED.load(Ordering::Relaxed) {
            assert_eq!(crate::runtime::read_guest_rcb_clock().unwrap(), clock);
        }
        seen.v4_waits.fetch_add(1, Ordering::Release);
    }
    assert!(valid(predicates()));
    seen.v4_gate_clocks[1].store(clock, Ordering::Release);
}
pub fn verified_exit() -> io::Result<Option<u64>> {
    let observations = control()
        .ok_or_else(|| io::Error::other("fixture unattached"))?
        .observations();
    let clock = if CLOCKED.load(Ordering::Relaxed) {
        if predicates() & 14 != 14 {
            return Err(io::Error::other("exit not paused runtime"));
        }
        let count = crate::runtime::read_guest_rcb_clock()?;
        observations.clocks[7].store(count, Ordering::Release);
        Some(count)
    } else {
        if predicates() & 3 != 0 {
            return Err(io::Error::other("inactive exit acquired clock"));
        }
        None
    };
    observations.verified.store(1, Ordering::Release);
    Ok(clock)
}
pub(crate) fn wait_enter() {
    if let Some(control) = control() {
        let bits = predicates();
        let observations = control.observations();
        observations.full.fetch_add(1, Ordering::Relaxed);
        observations.before.store(bits, Ordering::Relaxed);
        if !valid(bits) {
            observations.invalid.fetch_add(1, Ordering::Relaxed);
        }
        observations.wait_enter.fetch_add(1, Ordering::Release);
    }
}
pub(crate) fn wait_exit(result: i64) {
    if let Some(control) = control() {
        let bits = predicates();
        let observations = control.observations();
        observations.after.store(bits, Ordering::Relaxed);
        observations.result.store(result as u64, Ordering::Relaxed);
        if !valid(bits) {
            observations.invalid.fetch_add(1, Ordering::Relaxed);
        }
        observations.wait_exit.fetch_add(1, Ordering::Release);
        if observations.v4_pressure.load(Ordering::Acquire) != 0
            && observations.callbacks.load(Ordering::Acquire) == 2
        {
            observations.v4_gate.store(1, Ordering::Release);
            wait_fixture_gate(control);
        }
    }
}
