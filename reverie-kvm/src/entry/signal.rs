/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The embedding process reserves Linux signal 64 for KVM entry control.
//! SIGURG remains the independent worker-cancellation signal.
//!
//! A closer interrupts a running vCPU as QEMU does: under the registry lock it
//! sets the vCPU's `kvm_run.immediate_exit` byte and then sends signal 64 to
//! the vCPU's thread. Signal 64 is unblocked on a vCPU thread from its first
//! entry on, and its handler only counts the delivery. An instance arriving
//! inside `KVM_RUN` ends it with EINTR. One arriving before `KVM_RUN` runs
//! the handler outside it, and `immediate_exit` then makes `KVM_RUN` return
//! EINTR without entering the guest. `KVM_RUN` therefore runs under the
//! thread's own signal mask, with no KVM signal mask to keep in sync, and an
//! entry issues no host syscall besides `KVM_RUN` itself.
//!
//! Each thread counts the instances its handler took and the kicks closers
//! recorded against its entries, and the two must agree. An entry refuses to
//! start if more instances arrived than kicks were recorded: one had no
//! sender. A kicked entry's cleanup first makes sure every recorded kick has
//! been delivered, then refuses any instance beyond them, and refuses a kick
//! that was never delivered (signal 64 blocked again on this thread).
//!
//! Two consequences belong to the embedding contract. A foreign signal 64 is
//! not left pending: the handler takes it at once, which can interrupt a host
//! call this thread is making, and the thread's next entry refuses. And code
//! that blocks signal 64 again on a vCPU thread breaks prompt interruption:
//! a kick then cannot end a `KVM_RUN` that is already running. Entries
//! re-check that signal 64 is unblocked every [`RECHECK_ENTRIES`] entries and
//! refuse if it was blocked again, which bounds how long that can go unseen
//! without a per-entry system call.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use kvm_ioctls::VcpuFd;

use super::failure;
use super::protocol_failure;
use crate::Result;

const SIGNAL: libc::c_int = 64;

thread_local! {
    /// Instances of the reserved signal that this thread's handler has taken.
    static DELIVERED: AtomicU64 = const { AtomicU64::new(0) };
    /// Kicks that closers recorded against this thread's finished entries.
    static RECORDED: Cell<u64> = const { Cell::new(0) };
    /// The participant whose entry last unblocked the reserved signal here.
    static UNBLOCKED_FOR: Cell<u64> = const { Cell::new(0) };
    /// Entries since this thread last unblocked or re-checked the signal.
    static SINCE_CHECK: Cell<u32> = const { Cell::new(0) };
}

/// How many entries of one participant on one thread may pass before the next
/// re-checks that signal 64 is still unblocked: one system call per this many
/// entries.
pub(super) const RECHECK_ENTRIES: u32 = 1024;

extern "C" fn interrupt(_signal: libc::c_int) {
    // A const-initialized thread local without a destructor is a plain TLS
    // access, and x86-64's atomic increment performs no allocation or locking.
    DELIVERED.with(|delivered| delivered.fetch_add(1, Ordering::Relaxed));
}

fn delivered() -> u64 {
    DELIVERED.with(|delivered| delivered.load(Ordering::Relaxed))
}

static NEXT_PARTICIPANT_KEY: AtomicU64 = AtomicU64::new(1);

/// A process-unique key for one participant. Its first entry on a thread
/// unblocks the reserved signal there, so a vCPU that moves to another thread,
/// or a new vCPU on a thread whose mask was replaced, unblocks it again.
pub(super) fn participant_key() -> u64 {
    NEXT_PARTICIPANT_KEY.fetch_add(1, Ordering::Relaxed)
}

fn last_error(operation: &'static str) -> crate::Error {
    failure(operation, std::io::Error::last_os_error())
}

fn install_handler() -> Result<()> {
    thread_local! {
        static VERIFIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if VERIFIED.get() {
        return Ok(());
    }
    static INSTALL: OnceLock<std::result::Result<usize, i32>> = OnceLock::new();
    let result = INSTALL.get_or_init(|| {
        // SAFETY: the supplied action storage is initialized and lives through
        // each call. The handler performs no allocation or locking.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(SIGNAL, std::ptr::null(), &mut old) != 0 {
                return Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
            if old.sa_sigaction != libc::SIG_DFL {
                return Err(libc::EBUSY);
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = interrupt as *const () as usize;
            // A kick that lands outside KVM_RUN must not fail an unrelated
            // restartable host syscall on the vCPU thread.
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(SIGNAL, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
            // Retain the exact address passed to the kernel. In optimized
            // builds the linker may fold this handler with another identical
            // handler, while separate function-pointer materializations still
            // name distinct symbols. Re-evaluating `interrupt as usize` below
            // can therefore reject the disposition we just installed.
            Ok(action.sa_sigaction)
        }
    });
    let installed_handler = match result {
        Ok(handler) => *handler,
        Err(errno) => {
            return Err(failure(
                "reserve host signal 64",
                std::io::Error::from_raw_os_error(*errno),
            ));
        }
    };
    // A default-disposition check cannot exclude a future foreign owner. The
    // lifetime reservation is an embedding contract; detect a visible change
    // at each thread's first entry rather than silently replacing a library's
    // handler. A change after that is not seen on that thread; its kicks would
    // then go uncounted, which a kicked entry's cleanup refuses.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(SIGNAL, std::ptr::null(), &mut current) != 0 {
            return Err(last_error("query reserved host signal"));
        }
        if current.sa_sigaction != installed_handler {
            return Err(protocol_failure("reserved host signal disposition changed"));
        }
    }
    VERIFIED.set(true);
    Ok(())
}

fn only_reserved_signal() -> libc::sigset_t {
    // SAFETY: both libc functions receive initialized, writable sigset storage;
    // 64 is a valid signal number on this x86-64 Linux backend.
    unsafe {
        let mut set = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, SIGNAL);
        set
    }
}

/// Unblock the reserved signal for `key`'s first entry on this thread, and
/// again every [`RECHECK_ENTRIES`] entries after that. Linux delivers an
/// instance already pending here before the call returns, so the entry's
/// check that follows counts it. A re-check that finds the signal blocked
/// again by someone else refuses.
fn unblock(key: u64) -> Result<()> {
    let same = UNBLOCKED_FOR.get() == key;
    if same {
        let since = SINCE_CHECK.get() + 1;
        if since < RECHECK_ENTRIES {
            SINCE_CHECK.set(since);
            return Ok(());
        }
    }
    let set = only_reserved_signal();
    // SAFETY: both sets are valid storage; the change affects only this thread.
    let mut previous = unsafe { std::mem::zeroed() };
    let error = unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, &mut previous) };
    if error != 0 {
        return Err(failure(
            "unblock reserved host signal",
            std::io::Error::from_raw_os_error(error),
        ));
    }
    UNBLOCKED_FOR.set(key);
    SINCE_CHECK.set(0);
    // SAFETY: previous is the libc sigset pthread_sigmask just wrote.
    if same && unsafe { libc::sigismember(&previous, SIGNAL) } == 1 {
        return Err(protocol_failure(
            "reserved host signal blocked again on a vCPU thread",
        ));
    }
    Ok(())
}

/// Return to user space through the kernel once. Linux delivers every pending
/// unblocked signal before a system call returns, so after this the handler
/// has taken each instance that was queued for this thread before the call.
fn deliver_pending() {
    // SAFETY: getppid has no arguments and no memory effects. It is a real
    // system call (no vDSO entry), which is all this needs.
    unsafe { libc::syscall(libc::SYS_getppid) };
}

/// The `immediate_exit` byte of one vCPU's shared `kvm_run` page. A closer
/// sets it, under the registry lock, before it sends the reserved signal, so
/// a kick that lands before `KVM_RUN` still keeps the guest from running.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImmediateExit(std::ptr::NonNull<u8>);

// SAFETY: this crate accesses the byte only atomically: stores by a closer
// under the registry lock while its entry is published as running, and by the
// entry's own thread after withdrawal. The kernel reads it with READ_ONCE at
// each KVM_RUN. One access is not atomic: kvm-ioctls' VcpuFd::run forms a
// `&mut kvm_run` over the whole mapped page after KVM_RUN returns, while a
// closer may still store this byte until withdrawal. That is formally a data
// race on memory the kernel also writes; it is the same single-byte store
// QEMU makes, and kvm-ioctls never reads or writes immediate_exit through that
// reference. A shared reference to this type permits nothing more.
unsafe impl Send for ImmediateExit {}
// SAFETY: as for Send.
unsafe impl Sync for ImmediateExit {}

impl ImmediateExit {
    /// # Safety
    ///
    /// The vCPU's `kvm_run` mapping must outlive every entry prepared with the
    /// result. The mapping's address does not change when the fd moves.
    pub(crate) unsafe fn new(fd: &mut VcpuFd) -> Self {
        Self(std::ptr::NonNull::from(
            &mut fd.get_kvm_run().immediate_exit,
        ))
    }

    pub(super) fn set(self, value: bool) {
        // SAFETY: the byte lives in the vCPU's mapped kvm_run page, which the
        // caller's entry keeps alive; see the Send impl for its accesses.
        unsafe { AtomicU8::from_ptr(self.0.as_ptr()) }.store(u8::from(value), Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn get(self) -> bool {
        // SAFETY: as for set.
        unsafe { AtomicU8::from_ptr(self.0.as_ptr()) }.load(Ordering::Relaxed) != 0
    }
}

/// One entry's reserved-signal accounting on its own thread.
pub(super) struct Control {
    _same_thread: PhantomData<Rc<()>>,
}

impl Control {
    pub(super) fn arm(key: u64) -> Result<Self> {
        #[cfg(test)]
        if let Some(error) = fault::take(0) {
            return Err(crate::Error::SharedFailure(error));
        }
        install_handler()?;
        unblock(key)?;
        // Every instance a closer sent to an earlier entry was delivered and
        // counted before that entry acknowledged, and no closer can know this
        // entry before it is published. An extra instance has no sender.
        if delivered() != RECORDED.get() {
            return Err(protocol_failure(
                "unexplained reserved host signal before entry",
            ));
        }
        Ok(Self {
            _same_thread: PhantomData,
        })
    }

    /// The registry must have withdrawn the send target before this call;
    /// `kicks` is what withdrawal reported. Every recorded send completed
    /// under the registry lock, so each successful one is pending for this
    /// thread or already delivered (the signal is real-time, so instances do
    /// not merge). A send that failed after being counted has already failed
    /// the gate. Returns whether the entry was kicked.
    pub(super) fn finish(self, kicks: usize) -> Result<bool> {
        #[cfg(test)]
        if let Some(error) = fault::take(1) {
            // The one-shot is removed before recursion. Perform the real
            // accounting, then exercise its error composition.
            let actual = self.finish(kicks);
            let injected = crate::Error::SharedFailure(error);
            return Err(match actual {
                Ok(_) => injected,
                Err(actual) => injected.with_cleanup(vec![actual]),
            });
        }
        let kicked = kicks != 0;
        if kicked {
            RECORDED.set(RECORDED.get().saturating_add(kicks as u64));
            // Always, even when the counts already agree: a stray delivered
            // during the entry could stand in for a kick still in flight,
            // which would otherwise land later in an unrelated host call.
            deliver_pending();
        }
        let (delivered, recorded) = (delivered(), RECORDED.get());
        if delivered > recorded {
            return Err(protocol_failure(
                "unexplained reserved host signal during entry",
            ));
        }
        if delivered < recorded {
            return Err(protocol_failure("recorded kick was not delivered"));
        }
        Ok(kicked)
    }
}

#[cfg(test)]
pub(super) mod fault {
    use std::cell::RefCell;
    use std::marker::PhantomData;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    thread_local! {
        static ACTIVE: RefCell<Option<Arc<Probe>>> = const { RefCell::new(None) };
    }

    pub(in crate::entry) struct Probe {
        errors: [Option<Arc<crate::Error>>; 2],
        pub(in crate::entry) consumed: [AtomicUsize; 2],
    }

    pub(in crate::entry) struct Guard {
        pub(in crate::entry) probe: Arc<Probe>,
        previous: Option<Arc<Probe>>,
        _same_thread: PhantomData<Rc<()>>,
    }

    impl Guard {
        pub(in crate::entry) fn arm(errors: [Option<Arc<crate::Error>>; 2]) -> Self {
            let probe = Arc::new(Probe {
                errors,
                consumed: std::array::from_fn(|_| AtomicUsize::new(0)),
            });
            let previous = ACTIVE.with(|active| active.replace(Some(probe.clone())));
            assert!(previous.is_none(), "nested signal-operation fault control");
            Self {
                probe,
                previous,
                _same_thread: PhantomData,
            }
        }
    }

    pub(super) fn take(index: usize) -> Option<Arc<crate::Error>> {
        ACTIVE.with(|active| {
            let active = active.borrow();
            let probe = active.as_ref()?;
            let error = probe.errors[index].as_ref()?;
            probe.consumed[index]
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .ok()
                .map(|_| error.clone())
        })
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            ACTIVE.with(|active| {
                active.replace(self.previous.take());
            });
            if !std::thread::panicking() {
                for (index, error) in self.probe.errors.iter().enumerate() {
                    assert_eq!(
                        self.probe.consumed[index].load(Ordering::SeqCst),
                        usize::from(error.is_some())
                    );
                }
            }
        }
    }
}

/// Instances this thread's handler has taken, for tests.
#[cfg(test)]
pub(crate) fn delivered_for_test() -> u64 {
    delivered()
}

/// Treat every instance delivered to this thread so far as explained. A test
/// that deliberately sends an unrecorded instance calls this before the
/// thread's next entry, as it would otherwise refuse every later entry.
#[cfg(test)]
pub(crate) fn accept_delivered_for_test() {
    RECORDED.set(delivered());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_mask() -> libc::sigset_t {
        unsafe {
            let mut mask = std::mem::zeroed();
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask),
                0
            );
            mask
        }
    }

    fn contains(mask: &libc::sigset_t, signal: i32) -> bool {
        unsafe { libc::sigismember(mask, signal) == 1 }
    }

    fn pending(signal: i32) -> bool {
        unsafe {
            let mut mask = std::mem::zeroed();
            assert_eq!(libc::sigpending(&mut mask), 0);
            contains(&mask, signal)
        }
    }

    fn operation(error: &crate::Error) -> &'static str {
        match error.primary() {
            crate::Error::EntryControl { operation, .. } => operation,
            other => panic!("not an entry-control error: {other:?}"),
        }
    }

    extern "C" fn foreign_handler(_: i32) {}

    #[test]
    fn reserved_signal_protocol() {
        const CHILD: &str = "REVERIE_KVM_ENTRY_SIGNAL_CHILD";
        let Ok(case) = std::env::var(CHILD) else {
            // Disposition ownership and unexplained signals are tested in
            // fresh processes. They must not alter the parallel test runner.
            for case in [
                "blocked",
                "unblocked",
                "pending",
                "overflowed",
                "ignored",
                "foreign",
                "replaced",
                "reblocked",
            ] {
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "entry::signal::tests::reserved_signal_protocol",
                        "--nocapture",
                    ])
                    .env(CHILD, case)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{case}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        };
        if matches!(case.as_str(), "ignored" | "foreign") {
            let handler = if case == "ignored" {
                libc::SIG_IGN
            } else {
                foreign_handler as *const () as usize
            };
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler;
                libc::sigemptyset(&mut action.sa_mask);
                assert_eq!(libc::sigaction(SIGNAL, &action, std::ptr::null_mut()), 0);
            }
            let error = match Control::arm(participant_key()) {
                Err(error) => error,
                Ok(_) => panic!("replaced another signal owner"),
            };
            assert!(
                matches!(error.primary(), crate::Error::EntryControl { source, .. } if source.raw_os_error() == Some(libc::EBUSY))
            );
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                assert_eq!(libc::sigaction(SIGNAL, std::ptr::null(), &mut action), 0);
                assert_eq!(action.sa_sigaction, handler);
            }
            return;
        }
        if case == "replaced" {
            let key = participant_key();
            Control::arm(key).unwrap().finish(0).unwrap();
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = foreign_handler as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                assert_eq!(libc::sigaction(SIGNAL, &action, std::ptr::null_mut()), 0);
            }
            // The disposition is checked at each thread's first entry. This
            // thread already passed it; a new thread's first entry refuses.
            Control::arm(key).unwrap().finish(0).unwrap();
            let error = std::thread::spawn(move || match Control::arm(key) {
                Err(error) => error,
                Ok(_) => panic!("accepted a replacement signal owner"),
            })
            .join()
            .unwrap();
            assert_eq!(
                operation(&error),
                "reserved host signal disposition changed"
            );
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                assert_eq!(libc::sigaction(SIGNAL, std::ptr::null(), &mut action), 0);
                assert_eq!(action.sa_sigaction, foreign_handler as *const () as usize);
            }
            return;
        }
        if case == "reblocked" {
            let key = participant_key();
            Control::arm(key).unwrap().finish(0).unwrap();
            // Something else blocks signal 64 again on this vCPU thread.
            unsafe {
                libc::pthread_sigmask(
                    libc::SIG_BLOCK,
                    &only_reserved_signal(),
                    std::ptr::null_mut(),
                );
            }
            // Entries inside the re-check window issue no system call and do
            // not see it; the entry that re-checks refuses, and leaves the
            // signal unblocked again.
            for _ in 1..RECHECK_ENTRIES {
                Control::arm(key).unwrap().finish(0).unwrap();
            }
            assert_eq!(
                operation(&Control::arm(key).err().unwrap()),
                "reserved host signal blocked again on a vCPU thread"
            );
            assert!(!contains(&current_mask(), SIGNAL));
            return;
        }
        let mut original = current_mask();
        unsafe {
            libc::sigaddset(&mut original, libc::SIGUSR1);
            libc::sigaddset(&mut original, libc::SIGURG);
            if case == "unblocked" {
                libc::sigdelset(&mut original, SIGNAL);
            } else {
                libc::sigaddset(&mut original, SIGNAL);
            }
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_SETMASK, &original, std::ptr::null_mut()),
                0
            );
        }
        if case == "overflowed" {
            // An ordinary entry installs the handler; the caller's mask still
            // blocks signal 64 for the next participant on this thread.
            Control::arm(participant_key()).unwrap().finish(0).unwrap();
            unsafe {
                libc::pthread_sigmask(
                    libc::SIG_BLOCK,
                    &only_reserved_signal(),
                    std::ptr::null_mut(),
                );
            }
            // At the pending-signal limit, a stray SI_USER instance can exist
            // only as a pending bit. The next participant's first entry
            // unblocks it, counts its delivery and refuses.
            unsafe {
                let mut old: libc::rlimit = std::mem::zeroed();
                assert_eq!(libc::getrlimit(libc::RLIMIT_SIGPENDING, &mut old), 0);
                let exhausted = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: old.rlim_max,
                };
                assert_eq!(libc::setrlimit(libc::RLIMIT_SIGPENDING, &exhausted), 0);
                let mut info: libc::siginfo_t = std::mem::zeroed();
                info.si_signo = SIGNAL;
                info.si_code = libc::SI_USER;
                let sent = libc::syscall(
                    libc::SYS_rt_tgsigqueueinfo,
                    libc::getpid(),
                    libc::syscall(libc::SYS_gettid),
                    SIGNAL,
                    &info as *const libc::siginfo_t,
                );
                assert_eq!(libc::setrlimit(libc::RLIMIT_SIGPENDING, &old), 0);
                assert_eq!(sent, 0, "{}", std::io::Error::last_os_error());
            }
            assert!(pending(SIGNAL));
            let before = delivered();
            let error = match Control::arm(participant_key()) {
                Err(error) => error,
                Ok(_) => panic!("accepted an unexplained signal at the limit"),
            };
            assert_eq!(
                operation(&error),
                "unexplained reserved host signal before entry"
            );
            assert_eq!(delivered(), before + 1);
            assert!(!pending(SIGNAL));
            return;
        }
        if case == "pending" {
            // Queued while the caller blocks signal 64, before any entry.
            unsafe {
                assert_eq!(libc::pthread_kill(libc::pthread_self(), SIGNAL), 0);
            }
            assert!(pending(SIGNAL));
            let error = match Control::arm(participant_key()) {
                Err(error) => error,
                Ok(_) => panic!("accepted an unexplained pending signal"),
            };
            assert_eq!(
                operation(&error),
                "unexplained reserved host signal before entry"
            );
            assert_eq!(delivered(), 1);
            assert!(!contains(&current_mask(), SIGNAL));
            // Refusal is sticky until the instance is explained.
            assert!(Control::arm(participant_key()).is_err());
            return;
        }
        let send = |count: usize| {
            for _ in 0..count {
                unsafe {
                    assert_eq!(libc::pthread_kill(libc::pthread_self(), SIGNAL), 0);
                }
            }
        };
        let key = participant_key();
        // Instances sent after setup stand in for kicks during the entry. A
        // thread-directed signal to the calling thread is delivered before
        // pthread_kill returns, now that the entry unblocked it.
        let control = Control::arm(key).unwrap();
        assert!(!contains(&current_mask(), SIGNAL));
        send(3);
        unsafe {
            assert_eq!(libc::pthread_kill(libc::pthread_self(), libc::SIGURG), 0);
        }
        assert_eq!(delivered(), 3);
        // Three recorded kicks explain their three instances.
        assert!(control.finish(3).unwrap());
        assert!(pending(libc::SIGURG), "entry cleanup consumed cancellation");
        // Only signal 64 moved, and it stays unblocked after the entry.
        let after = current_mask();
        for signal in 1..=64 {
            assert_eq!(
                contains(&after, signal),
                signal != SIGNAL && contains(&original, signal),
                "signal {signal}"
            );
        }
        // An unkicked entry issues nothing and accepts.
        assert!(!Control::arm(key).unwrap().finish(0).unwrap());
        // An instance during an entry that no closer kicked is refused at
        // its cleanup, and keeps refusing the thread's next entry.
        let control = Control::arm(key).unwrap();
        send(1);
        assert_eq!(
            operation(&control.finish(0).unwrap_err()),
            "unexplained reserved host signal during entry"
        );
        assert_eq!(
            operation(&Control::arm(key).err().unwrap()),
            "unexplained reserved host signal before entry"
        );
        accept_delivered_for_test();
        // An instance beyond the recorded kicks has no sender: one kick with
        // two instances is refused.
        let control = Control::arm(key).unwrap();
        send(2);
        assert_eq!(
            operation(&control.finish(1).unwrap_err()),
            "unexplained reserved host signal during entry"
        );
        accept_delivered_for_test();
        // A kick that cannot be delivered, because something blocked signal
        // 64 on this thread again, is refused rather than presumed handled.
        let control = Control::arm(key).unwrap();
        unsafe {
            libc::pthread_sigmask(
                libc::SIG_BLOCK,
                &only_reserved_signal(),
                std::ptr::null_mut(),
            );
        }
        send(1);
        assert_eq!(
            operation(&control.finish(1).unwrap_err()),
            "recorded kick was not delivered"
        );
        assert!(pending(SIGNAL));
        // A new participant on this thread unblocks it again, which
        // delivers the kick and squares the accounting.
        assert!(!Control::arm(participant_key()).unwrap().finish(0).unwrap());
        assert!(!pending(SIGNAL));
    }
}
