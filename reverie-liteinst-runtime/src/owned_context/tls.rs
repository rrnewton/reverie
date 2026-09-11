use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

#[repr(C)]
struct Control {
    private_fs: AtomicU64,
    owner: AtomicU64,
    guest_fs: AtomicU64,
    deferred: AtomicU64,
    phase: AtomicU64,
    private_gs: AtomicU64,
    guest_gs: AtomicU64,
}

const _: () = {
    assert!(std::mem::size_of::<Control>() == 56);
    assert!(std::mem::offset_of!(Control, private_fs) == 0);
    assert!(std::mem::offset_of!(Control, owner) == 8);
    assert!(std::mem::offset_of!(Control, guest_fs) == 16);
    assert!(std::mem::offset_of!(Control, deferred) == 24);
    assert!(std::mem::offset_of!(Control, phase) == 32);
    assert!(std::mem::offset_of!(Control, private_gs) == 40);
    assert!(std::mem::offset_of!(Control, guest_gs) == 48);
};

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Observation {
    result: i64,
    guest: [u64; 2],
}

const _: () = {
    assert!(std::mem::size_of::<Observation>() == 24);
    assert!(std::mem::offset_of!(Observation, result) == 0);
    assert!(std::mem::offset_of!(Observation, guest) == 8);
};

unsafe extern "C" {
    static pl_tls: Control;
    fn pl_guest_arch_prctl(operation: u64, argument: u64, output: *mut Observation);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct State {
    private: [u64; 2],
    guest: [u64; 2],
    owner: u64,
    phase: u64,
    deferred: u64,
}

fn snapshot(control: &Control) -> State {
    let phase = control.phase.load(Ordering::Acquire);
    State {
        phase,
        private: [
            control.private_fs.load(Ordering::Relaxed),
            control.private_gs.load(Ordering::Relaxed),
        ],
        guest: [
            control.guest_fs.load(Ordering::Relaxed),
            control.guest_gs.load(Ordering::Relaxed),
        ],
        owner: control.owner.load(Ordering::Relaxed),
        deferred: control.deferred.load(Ordering::Relaxed),
    }
}

fn owned(state: State, tid: i64, phase: u8) -> bool {
    state.phase == 2
        && state.deferred == 1
        && tid > 0
        && state.owner == tid as u64
        && phase == super::RUNTIME
        && state.private[0] != 0
        && state.private[0] != state.guest[0]
}

fn kernel_tid() -> i64 {
    unsafe { reverie_preload::trap::raw_syscall6(libc::SYS_gettid, [0; 6]) }
}

fn state() -> State {
    snapshot(unsafe { &pl_tls })
}

pub(crate) fn installed() -> bool {
    unsafe { pl_tls.phase.load(Ordering::Acquire) != 0 }
}

pub(crate) fn ready() -> bool {
    owned(
        state(),
        kernel_tid(),
        super::PHASE.with(|slot| slot.load(Ordering::Acquire)),
    )
}

pub(crate) fn bases() -> Result<[u64; 2], &'static str> {
    let current = state();
    if !owned(
        current,
        kernel_tid(),
        super::PHASE.with(|slot| slot.load(Ordering::Acquire)),
    ) {
        return Err("TLS owner/continuation unavailable");
    }
    Ok(current.guest)
}

fn request(state: State, operation: u64, argument: u64) -> Result<(), &'static str> {
    match operation {
        0x1001 | 0x1002 => {
            if operation == 0x1002 && argument == state.private[0] {
                return Err("guest/private FS identity collision");
            }
            Ok(())
        }
        0x1003 | 0x1004 => Ok(()),
        _ => Err("unsupported arch_prctl operation"),
    }
}

fn completion(
    before: State,
    operation: u64,
    argument: u64,
    observed: Observation,
) -> Result<[u64; 2], &'static str> {
    if observed.result > 0 || observed.result < -4095 {
        return Err("invalid arch_prctl kernel result");
    }
    let mut expected = before.guest;
    if observed.result == 0 {
        match operation {
            0x1001 => expected[1] = argument,
            0x1002 => expected[0] = argument,
            0x1003 | 0x1004 => {}
            _ => return Err("unsupported arch_prctl completion"),
        }
    }
    if observed.guest != expected {
        return Err("arch_prctl guest controls disagree with kernel result");
    }
    Ok(expected)
}

fn execute_with(
    before: State,
    tid: i64,
    phase: u8,
    operation: u64,
    argument: u64,
    execute: impl FnOnce() -> Observation,
) -> Result<Observation, &'static str> {
    if !owned(before, tid, phase) {
        return Err("TLS owner/continuation unavailable");
    }
    request(before, operation, argument)?;
    let observed = execute();
    completion(before, operation, argument, observed)?;
    Ok(observed)
}

fn publish(
    control: &Control,
    before: State,
    operation: u64,
    argument: u64,
    observed: Observation,
) -> Result<i64, &'static str> {
    let guest = completion(before, operation, argument, observed)?;
    if snapshot(control) != before {
        return Err("TLS owner changed during kernel transaction");
    }
    control.guest_fs.store(guest[0], Ordering::Relaxed);
    control.guest_gs.store(guest[1], Ordering::Release);
    Ok(observed.result)
}

pub(crate) fn inject(args: [u64; 6]) -> Result<i64, &'static str> {
    let before = state();
    let execute = || {
        execute_with(
            before,
            kernel_tid(),
            super::PHASE.with(|slot| slot.load(Ordering::Acquire)),
            args[0],
            args[1],
            || {
                let mut observed = Observation {
                    result: i64::MIN,
                    guest: [u64::MAX; 2],
                };
                unsafe { pl_guest_arch_prctl(args[0], args[1], &mut observed) };
                observed
            },
        )
    };
    let observed = if matches!(args[0], 0x1003 | 0x1004) {
        crate::mapping::with_guest_output(args[1], 8, execute).map_err(|error| error.reason)?
    } else {
        execute()
    }?;
    publish(unsafe { &pl_tls }, before, args[0], args[1], observed)
}

#[cfg(test)]
mod tests;
