use std::cell::UnsafeCell;
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

use reverie_rpc_transport::guest_log::FAILURE;
use reverie_rpc_transport::guest_log::Producer;
use reverie_rpc_transport::guest_log::PublishError;
use reverie_rpc_transport::guest_log::SharedBuffer;
use reverie_rpc_transport::guest_log::ordered;

use super::fail;
use super::raw_syscall6;

static STATE: AtomicPtr<Buffered> = AtomicPtr::new(std::ptr::null_mut());

pub(super) struct Buffered {
    mapping: Mapping,
    owner: AtomicI32,
    producer: UnsafeCell<Writer>,
}

enum Mapping {
    Legacy(SharedBuffer),
    Ordered(Arc<ordered::Buffer>),
}
enum Writer {
    Legacy(Producer),
    Ordered(ordered::Writer),
}

impl Mapping {
    fn stopped(&self) -> bool {
        match self {
            Self::Legacy(mapping) => mapping.stopped(),
            Self::Ordered(mapping) => mapping.guest_stopped(),
        }
    }
    fn reserve_child(&self, parent: usize) -> Result<usize, PublishError> {
        match self {
            Self::Legacy(mapping) => mapping.reserve_child(parent),
            Self::Ordered(mapping) => mapping.reserve_child(parent),
        }
    }
    fn resolve_fork(&self, index: usize, result: i64) -> Result<(), PublishError> {
        match self {
            Self::Legacy(mapping) => mapping.resolve_fork(index, result),
            Self::Ordered(mapping) => mapping.resolve_fork(index, result),
        }
    }
    unsafe fn activate(&self, index: usize, pid: i64) -> Result<Writer, PublishError> {
        match self {
            Self::Legacy(mapping) => unsafe { mapping.activate(index, pid) }.map(Writer::Legacy),
            Self::Ordered(mapping) => mapping.activate(index, pid).map(Writer::Ordered),
        }
    }
}

impl Writer {
    fn index(&self) -> usize {
        match self {
            Self::Legacy(writer) => writer.index(),
            Self::Ordered(writer) => writer.index(),
        }
    }
}

fn state() -> Option<&'static Buffered> {
    unsafe { STATE.load(Ordering::Acquire).as_ref() }
}
pub(super) fn installed() -> bool {
    state().is_some()
}
pub(super) fn ordered() -> bool {
    state().is_some_and(|state| matches!(state.mapping, Mapping::Ordered(_)))
}
pub(super) fn mark_failed() {
    if let Some(state) = state() {
        match &state.mapping {
            Mapping::Legacy(mapping) => mapping.fail(FAILURE),
            Mapping::Ordered(mapping) => {
                mapping.fail_guest();
            }
        }
    }
}

pub(super) fn attach(fd: i32) -> io::Result<Box<Buffered>> {
    let mapping = SharedBuffer::receive(fd)?;
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let producer = unsafe { mapping.activate(0, pid) }
        .map_err(|error| io::Error::other(format!("guest-log activation: {error:?}")))?;
    Ok(Box::new(Buffered {
        mapping: Mapping::Legacy(mapping),
        owner: AtomicI32::new(0),
        producer: UnsafeCell::new(Writer::Legacy(producer)),
    }))
}

pub(super) fn attach_ordered(fd: i32) -> io::Result<Box<Buffered>> {
    let mapping = ordered::Buffer::receive(fd)?;
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let producer = mapping
        .activate(1, pid)
        .map_err(|error| io::Error::other(format!("ordered guest-log activation: {error:?}")))?;
    Ok(Box::new(Buffered {
        mapping: Mapping::Ordered(mapping),
        owner: AtomicI32::new(0),
        producer: UnsafeCell::new(Writer::Ordered(producer)),
    }))
}

pub(super) unsafe fn install(state: Box<Buffered>) {
    STATE.store(Box::into_raw(state), Ordering::Release);
}

fn qualified() -> bool {
    (!crate::clock_control::requested() && !crate::clock_control::active())
        || (crate::clock_control::paused() && crate::runtime_domain::allocation_active())
}

fn wait_word(address: *const u32, expected: u32, ring_full: bool) -> Result<(), PublishError> {
    if !qualified() {
        return Err(PublishError::Invalid);
    }
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 100_000_000,
    };
    #[cfg(feature = "test-guest-log")]
    if ring_full {
        crate::guest_log_fixture::wait_enter();
    }
    #[cfg(not(feature = "test-guest-log"))]
    let _ = ring_full;
    let result = unsafe {
        raw_syscall6(
            libc::SYS_futex,
            [
                address as u64,
                libc::FUTEX_WAIT as u64,
                u64::from(expected),
                (&raw const timeout) as u64,
                0,
                0,
            ],
        )
    };
    #[cfg(feature = "test-guest-log")]
    if ring_full {
        crate::guest_log_fixture::wait_exit(result);
    }
    if result == 0
        || [
            -i64::from(libc::EINTR),
            -i64::from(libc::EAGAIN),
            -i64::from(libc::ETIMEDOUT),
        ]
        .contains(&result)
    {
        Ok(())
    } else {
        Err(PublishError::Invalid)
    }
}

fn wait(mapping: &SharedBuffer, expected: u32) -> Result<(), PublishError> {
    if mapping.stopped() {
        return Err(PublishError::Stopped);
    }
    wait_word(mapping.progress_address(), expected, true)
}

fn lock(state: &Buffered) {
    if !qualified() {
        fail();
    }
    let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as i32;
    if tid <= 0 {
        fail();
    }
    loop {
        match state
            .owner
            .compare_exchange(0, tid, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => return,
            Err(owner) => {
                if owner == tid
                    || state.mapping.stopped()
                    || wait_word(state.owner.as_ptr().cast(), owner as u32, false).is_err()
                {
                    fail();
                }
            }
        }
    }
}

fn unlock(state: &Buffered) {
    state.owner.store(0, Ordering::Release);
    unsafe {
        raw_syscall6(
            libc::SYS_futex,
            [
                state.owner.as_ptr() as u64,
                libc::FUTEX_WAKE as u64,
                i32::MAX as u64,
                0,
                0,
                0,
            ],
        );
    }
}

pub(super) fn write(bytes: &[u8]) -> bool {
    let Some(state) = state() else {
        return false;
    };
    lock(state);
    let result = match (&state.mapping, unsafe { &mut *state.producer.get() }) {
        (Mapping::Legacy(mapping), Writer::Legacy(producer)) => {
            producer.write_record(mapping, bytes, wait)
        }
        _ => Err(PublishError::Invalid),
    };
    unlock(state);
    if result.is_err() {
        fail();
    }
    true
}

pub(super) fn write_ordered(bytes: &[u8]) -> io::Result<ordered::RecordCommit> {
    let state = state()
        .filter(|state| matches!(state.mapping, Mapping::Ordered(_)))
        .ok_or_else(|| {
            io::Error::other("complete-record source order requires prepared capture")
        })?;
    lock(state);
    let result = match unsafe { &mut *state.producer.get() } {
        Writer::Ordered(producer) => producer.write_record(bytes, wait),
        _ => Err(PublishError::Invalid),
    };
    unlock(state);
    match result {
        Ok(commit) => Ok(commit),
        Err(_) => fail(),
    }
}

pub(super) fn finish() -> bool {
    let Some(state) = state() else {
        return false;
    };
    lock(state);
    let result = match (&state.mapping, unsafe { &mut *state.producer.get() }) {
        (Mapping::Legacy(mapping), Writer::Legacy(producer)) => producer.finish(mapping, wait),
        (Mapping::Ordered(_), Writer::Ordered(producer)) => producer.finish(wait),
        _ => Err(PublishError::Invalid),
    };
    unlock(state);
    if result.is_err() {
        fail();
    }
    true
}

pub(crate) struct ForkLog(Option<usize>);

pub(crate) fn prepare_fork() -> ForkLog {
    let Some(state) = state() else {
        return ForkLog(None);
    };
    lock(state);
    let parent = unsafe { &*state.producer.get() }.index();
    let child = state
        .mapping
        .reserve_child(parent)
        .unwrap_or_else(|_| fail());
    ForkLog(Some(child))
}

pub(crate) fn complete_fork(reservation: ForkLog, result: i64) {
    let Some(child) = reservation.0 else {
        super::fork_result(result);
        return;
    };
    let state = state().unwrap_or_else(|| fail());
    if result == 0 {
        let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
        let producer = unsafe { state.mapping.activate(child, pid) }.unwrap_or_else(|_| fail());
        unsafe {
            *state.producer.get() = producer;
        }
    } else if state.mapping.resolve_fork(child, result).is_err() {
        fail();
    }
    unlock(state);
}
