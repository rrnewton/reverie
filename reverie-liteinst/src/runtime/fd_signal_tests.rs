use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use crate::control::*;

static LOCK: Mutex<()> = Mutex::new(());
static AT_RECEIVE: AtomicBool = AtomicBool::new(false);
static AT_CLOSE: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicI32 = AtomicI32::new(-1);
static DONOR: AtomicI32 = AtomicI32::new(-1);
static HANDLERS: AtomicUsize = AtomicUsize::new(0);
static HANDLER_DUP: AtomicI64 = AtomicI64::new(i64::MIN);
static HANDLER_REUSED: AtomicI64 = AtomicI64::new(i64::MIN);
static HANDLER_RESULT: AtomicI64 = AtomicI64::new(i64::MIN);
static HANDLER_FCNTL: AtomicI64 = AtomicI64::new(i64::MIN);

unsafe extern "C" fn handler(_: i32) {
    let target = TARGET.load(Ordering::Acquire);
    if AT_CLOSE.load(Ordering::Acquire) {
        // Reuse precisely the retired descriptor slot in this handler.
        let duplicated = unsafe {
            host_gate(
                libc::SYS_dup3,
                [
                    DONOR.load(Ordering::Acquire) as u64,
                    target as u64,
                    libc::O_CLOEXEC as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        HANDLER_DUP.store(duplicated, Ordering::Release);
        let reused = unsafe {
            host_gate(
                libc::SYS_fcntl,
                [target as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
            )
        };
        HANDLER_REUSED.store(reused, Ordering::Release);
    }
    let args = [target as u64, target as u64, 0, 0, 0, 0];
    let result = unsafe { crate::runtime::private_descriptor_result(libc::SYS_close_range, args) }
        .unwrap_or_else(|| unsafe { host_gate(libc::SYS_close_range, args) });
    HANDLER_RESULT.store(result, Ordering::Release);
    let valid = unsafe {
        host_gate(
            libc::SYS_fcntl,
            [target as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
        )
    };
    HANDLER_FCNTL.store(valid, Ordering::Release);
    HANDLERS.fetch_add(1, Ordering::AcqRel);
}

unsafe fn queue_signal() {
    let pid = unsafe { host_gate(libc::SYS_getpid, [0; 6]) };
    let tid = unsafe { host_gate(libc::SYS_gettid, [0; 6]) };
    let sent = unsafe {
        host_gate(
            libc::SYS_tgkill,
            [pid as u64, tid as u64, libc::SIGUSR1 as u64, 0, 0, 0],
        )
    };
    assert_eq!(sent, 0);
}

unsafe fn boundary_gate(number: i64, args: [u64; 6]) -> i64 {
    let result = unsafe { host_gate(number, args) };
    if number == libc::SYS_recvmsg && result == 64 && AT_RECEIVE.swap(false, Ordering::AcqRel) {
        let message = unsafe { &*(args[1] as *const libc::msghdr) };
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(message);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    TARGET.store(
                        libc::CMSG_DATA(header).cast::<i32>().read_unaligned(),
                        Ordering::Release,
                    );
                    break;
                }
                header = libc::CMSG_NXTHDR(message, header);
            }
            queue_signal();
        }
        assert_eq!(
            HANDLERS.load(Ordering::Acquire),
            0,
            "handler ran before receipt publication"
        );
    }
    if number == libc::SYS_close
        && args[0] as i32 == TARGET.load(Ordering::Acquire)
        && AT_CLOSE.load(Ordering::Acquire)
    {
        unsafe {
            queue_signal();
        }
        assert_eq!(
            HANDLERS.load(Ordering::Acquire),
            0,
            "handler ran before retired protection was removed"
        );
    }
    result
}

struct SignalState {
    action: libc::sigaction,
    mask: u64,
}
impl SignalState {
    fn install() -> Self {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler as *const () as usize;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
        }
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGUSR1, &action, &mut old) },
            0
        );
        let mut mask = 0_u64;
        let bit = 1_u64 << (libc::SIGUSR1 - 1);
        assert_eq!(
            unsafe {
                host_gate(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_UNBLOCK as u64,
                        (&raw const bit) as u64,
                        (&raw mut mask) as u64,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        Self { action: old, mask }
    }
}
impl Drop for SignalState {
    fn drop(&mut self) {
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGUSR1, &self.action, core::ptr::null_mut()) },
            0
        );
        assert_eq!(
            unsafe {
                host_gate(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const self.mask) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
    }
}
fn reset() {
    HANDLERS.store(0, Ordering::Release);
    HANDLER_DUP.store(i64::MIN, Ordering::Release);
    HANDLER_REUSED.store(i64::MIN, Ordering::Release);
    HANDLER_RESULT.store(i64::MIN, Ordering::Release);
    HANDLER_FCNTL.store(i64::MIN, Ordering::Release);
}

#[test]
fn pending_handler_cannot_close_a_received_private_descriptor() {
    let _lock = LOCK.lock().unwrap();
    let _state = SignalState::install();
    reset();
    AT_CLOSE.store(false, Ordering::Release);
    let (host, target) = pair(boundary_gate, false).unwrap();
    let original = std::fs::File::open("/dev/null").unwrap();
    send(&host, Packet::new(EVENT, 1), &[original.as_raw_fd()], false).unwrap();
    AT_RECEIVE.store(true, Ordering::Release);
    let (_, received) = receive(&target, true, false).unwrap().only_fd().unwrap();
    assert_eq!(HANDLERS.load(Ordering::Acquire), 1);
    assert_eq!(HANDLER_RESULT.load(Ordering::Acquire), 0);
    assert!(
        HANDLER_FCNTL.load(Ordering::Acquire) >= 0,
        "direct close_range lost the protected file"
    );
    drop(received);
    assert!(!protected_fds().contains(&TARGET.load(Ordering::Acquire)));
}

#[test]
fn pending_handler_reuse_does_not_inherit_retired_protection() {
    let _lock = LOCK.lock().unwrap();
    let _state = SignalState::install();
    reset();
    AT_RECEIVE.store(false, Ordering::Release);
    AT_CLOSE.store(false, Ordering::Release);
    let (owned, _peer) = pair(boundary_gate, true).unwrap();
    let donor = std::fs::File::open("/dev/null").unwrap();
    DONOR.store(donor.as_raw_fd(), Ordering::Release);
    TARGET.store(owned.as_raw_fd(), Ordering::Release);
    AT_CLOSE.store(true, Ordering::Release);
    drop(owned);
    AT_CLOSE.store(false, Ordering::Release);
    assert_eq!(HANDLERS.load(Ordering::Acquire), 1);
    assert_eq!(
        HANDLER_DUP.load(Ordering::Acquire),
        i64::from(TARGET.load(Ordering::Acquire)),
        "actual dup3 must reuse exactly the retired slot"
    );
    assert!(
        HANDLER_REUSED.load(Ordering::Acquire) >= 0,
        "the ordinary replacement must exist before guarded close_range"
    );
    assert_eq!(HANDLER_RESULT.load(Ordering::Acquire), 0);
    assert_eq!(
        HANDLER_FCNTL.load(Ordering::Acquire),
        -i64::from(libc::EBADF),
        "ordinary reused descriptor inherited private protection"
    );
}
