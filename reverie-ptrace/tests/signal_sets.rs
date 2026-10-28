/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `Guest::signal_sets` on a traced guest is the kernel's own five signal
//! sets for the thread named: a pending signal for the thread and for the
//! process, ignored and caught actions, a real-time signal queued twice, a
//! full mask, a handler's mask, and another thread's sets asked from each
//! side. A thread ID outside the process is `ESRCH`.
//!
//! The guest asks at numbered points with `tuxcall`, which Linux does not
//! implement; the tool answers it, and records the fact it read then.

#![cfg(target_arch = "x86_64")]

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::mpsc;

use reverie::Errno;
use reverie::Error;
use reverie::FactError;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::SignalSets;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;

/// The first argument of a guest's `tuxcall` that asks for a fact.
const ASK: u64 = 0x7369_6773_6574;

const SELF_PENDING: u64 = 1;
const SHARED_PENDING: u64 = 2;
const ACTIONS: u64 = 3;
const REALTIME: u64 = 4;
const FULL_MASK: u64 = 5;
const IN_HANDLER: u64 = 6;
const AFTER_HANDLER: u64 = 7;
const SIBLING: u64 = 8;
const LEADER_FROM_SIBLING: u64 = 9;
const OTHER_PROCESS: u64 = 10;
const NO_TASK: u64 = 11;
const ZERO: u64 = 12;
const NEGATIVE: u64 = 13;

type Fact = Result<SignalSets, FactError>;

/// The facts the tool read, by the guest's point number.
#[derive(Default)]
struct FactLog(Mutex<BTreeMap<u64, (i32, Fact)>>);

#[reverie::global_tool]
impl GlobalTool for FactLog {
    type Request = (u64, i32, Fact);
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, request: Self::Request) {
        let (point, tid, fact) = request;
        let previous = self.0.lock().unwrap().insert(point, (tid, fact));
        assert!(previous.is_none(), "point {point} asked twice");
    }
}

#[derive(Default)]
struct FactTool;

#[reverie::tool]
impl Tool for FactTool {
    type GlobalState = FactLog;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::tuxcall].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let (_, args) = syscall.into_parts();
        if args.arg0 as u64 != ASK {
            return Ok(guest.inject(syscall).await?);
        }
        let (tid, point) = (args.arg1 as i32, args.arg2 as u64);
        let fact = guest.signal_sets(Pid::from_raw(tid));
        guest.send_rpc((point, tid, fact)).await;
        Ok(0)
    }
}

fn bit(signal: libc::c_int) -> u64 {
    1 << (signal - 1)
}

/// Asks the tool for thread `tid`'s signal sets at `point`.
fn ask(point: u64, tid: i32) {
    // SAFETY: tuxcall takes no pointers; only the tool answers it.
    unsafe { libc::syscall(libc::SYS_tuxcall, ASK, tid as libc::c_long, point) };
}

fn gettid() -> i32 {
    // SAFETY: gettid takes no arguments and cannot fail.
    unsafe { libc::syscall(libc::SYS_gettid) as i32 }
}

fn check(result: libc::c_long, what: &str) -> libc::c_long {
    assert!(result >= 0, "{what}: {}", std::io::Error::last_os_error());
    result
}

/// The raw `rt_sigprocmask`: the kernel, not glibc, decides what a mask
/// may hold.
fn set_mask(how: libc::c_int, mask: u64) {
    // SAFETY: the kernel reads eight bytes of `mask` and writes nothing.
    check(
        unsafe {
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                how,
                &mask as *const u64,
                std::ptr::null_mut::<u64>(),
                8,
            )
        },
        "rt_sigprocmask",
    );
}

/// `rt_sigaction(signal, handler)` with `mask` as its `sa_mask`; glibc's
/// own restorer, as `sigaction(3)` installs it.
fn act(signal: libc::c_int, handler: libc::sighandler_t, mask: u64) -> libc::c_long {
    // SAFETY: sigaction is plain data; an all-zero value is valid.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    // SAFETY: the set is a valid sigset_t.
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    for signal in 1..=64 {
        if mask & bit(signal) != 0 {
            // SAFETY: as above.
            unsafe { libc::sigaddset(&mut action.sa_mask, signal) };
        }
    }
    // SAFETY: action is a valid sigaction value the call only reads.
    let result = unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) };
    if result == 0 {
        0
    } else {
        -(std::io::Error::last_os_error().raw_os_error().unwrap() as libc::c_long)
    }
}

/// Takes one pending instance of `signal`, which the caller blocks.
fn take(signal: libc::c_int) -> libc::c_int {
    // SAFETY: the set is a valid sigset_t and the timeout a valid timespec.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signal);
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        libc::sigtimedwait(&set, std::ptr::null_mut(), &zero)
    }
}

fn tgkill(pid: i32, tid: i32, signal: libc::c_int) {
    // SAFETY: tgkill takes no pointers.
    check(
        unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, signal) },
        "tgkill",
    );
}

extern "C" fn on_signal(_signal: libc::c_int) {
    ask(IN_HANDLER, gettid());
}

extern "C" fn on_other(_signal: libc::c_int) {}

/// Starts from no handler, no ignored signal and an empty mask, whatever the
/// test process left: a fork keeps both.
fn reset() {
    for signal in 1..=64 {
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        // The kernel's struct: handler, flags, restorer, mask.
        let action = [libc::SIG_DFL as u64, 0, 0, 0];
        // SAFETY: the kernel reads the 32-byte action and writes nothing.
        check(
            unsafe {
                libc::syscall(
                    libc::SYS_rt_sigaction,
                    signal,
                    action.as_ptr(),
                    std::ptr::null_mut::<u64>(),
                    8,
                )
            },
            "rt_sigaction",
        );
    }
    set_mask(libc::SIG_SETMASK, 0);
}

/// The guest: every point in order, as the KVM backend's fixture runs them.
fn guest() {
    reset();
    // SAFETY: getpid takes no arguments.
    let pid = unsafe { libc::getpid() };
    let tid = gettid();
    let (usr1, usr2) = (libc::SIGUSR1, libc::SIGUSR2);

    set_mask(libc::SIG_BLOCK, bit(usr1));
    tgkill(pid, tid, usr1);
    ask(SELF_PENDING, tid);
    assert_eq!(take(usr1), usr1);
    set_mask(libc::SIG_UNBLOCK, bit(usr1));

    set_mask(libc::SIG_BLOCK, bit(usr2));
    // SAFETY: kill takes no pointers.
    check(unsafe { libc::kill(pid, usr2) } as libc::c_long, "kill");
    ask(SHARED_PENDING, tid);
    assert_eq!(take(usr2), usr2);
    set_mask(libc::SIG_UNBLOCK, bit(usr2));

    assert_eq!(act(libc::SIGHUP, libc::SIG_IGN, 0), 0);
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    assert_eq!(act(usr2, handler, bit(libc::SIGWINCH)), 0);
    assert_eq!(act(libc::SIGCHLD, libc::SIG_DFL, 0), 0);
    assert_eq!(
        act(libc::SIGSTOP, libc::SIG_IGN, 0),
        -(libc::EINVAL as libc::c_long)
    );
    ask(ACTIONS, tid);

    let realtime = libc::SIGRTMIN();
    set_mask(libc::SIG_BLOCK, bit(realtime));
    for _ in 0..2 {
        // SAFETY: sigqueue takes the value by copy.
        let value = libc::sigval {
            sival_ptr: std::ptr::null_mut(),
        };
        check(
            unsafe { libc::sigqueue(pid, realtime, value) } as libc::c_long,
            "sigqueue",
        );
    }
    ask(REALTIME, tid);
    assert_eq!(take(realtime), realtime);
    assert_eq!(take(realtime), realtime);
    assert_eq!(take(realtime), -1);
    set_mask(libc::SIG_UNBLOCK, bit(realtime));

    set_mask(libc::SIG_SETMASK, u64::MAX);
    ask(FULL_MASK, tid);
    set_mask(libc::SIG_SETMASK, 0);

    set_mask(libc::SIG_BLOCK, bit(libc::SIGPIPE));
    tgkill(pid, tid, usr2);
    ask(AFTER_HANDLER, tid);
    set_mask(libc::SIG_UNBLOCK, bit(libc::SIGPIPE));

    let other = on_other as extern "C" fn(libc::c_int) as libc::sighandler_t;
    assert_eq!(act(usr1, other, 0), 0);
    let (ready_sender, ready) = mpsc::channel();
    let (release, release_receiver) = mpsc::channel::<()>();
    let sibling = std::thread::spawn(move || {
        set_mask(libc::SIG_SETMASK, bit(usr1) | bit(libc::SIGINT));
        ready_sender.send(gettid()).unwrap();
        release_receiver.recv().unwrap();
        // The leader now waits for this thread; ask about it.
        ask(LEADER_FROM_SIBLING, tid);
        assert_eq!(take(usr1), usr1);
    });
    let sibling_tid = ready.recv().unwrap();
    tgkill(pid, sibling_tid, usr1);
    ask(SIBLING, sibling_tid);
    release.send(()).unwrap();
    sibling.join().unwrap();

    let mut pipe = [0; 2];
    // SAFETY: pipe has room for two descriptors.
    check(
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } as libc::c_long,
        "pipe2",
    );
    // SAFETY: the child only reads and exits, both async-signal-safe.
    let child = unsafe { libc::fork() };
    if child == 0 {
        let mut byte = 0u8;
        // SAFETY: byte is a live one-byte buffer.
        let read = unsafe { libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1) };
        // SAFETY: _exit ends the child.
        unsafe { libc::_exit(if read == 1 { 0 } else { 1 }) };
    }
    check(child as libc::c_long, "fork");
    ask(OTHER_PROCESS, child);
    // SAFETY: the byte is live.
    check(
        unsafe { libc::write(pipe[1], b"r".as_ptr().cast(), 1) } as libc::c_long,
        "write",
    );
    let mut status = 0;
    // SAFETY: status is writable.
    check(
        unsafe { libc::waitpid(child, &mut status, 0) } as libc::c_long,
        "waitpid",
    );
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    ask(NO_TASK, i32::MAX);
    ask(ZERO, 0);
    ask(NEGATIVE, -1);
}

#[test]
fn a_traced_guest_reports_its_threads_signal_sets() {
    let log = reverie_ptrace::testing::check_fn::<FactTool, _>(guest);
    let facts = log.0.into_inner().unwrap();
    assert_eq!(facts.len(), 13, "{facts:?}");
    let fact = |point: u64| match facts[&point].1 {
        Ok(sets) => sets,
        Err(error) => panic!("point {point}: {error:?}"),
    };
    let (usr1, usr2) = (bit(libc::SIGUSR1), bit(libc::SIGUSR2));
    let actions = (bit(libc::SIGHUP), usr2);
    let expect = |pending, shared, blocked, (ignored, caught): (u64, u64)| {
        SignalSets::new(pending, shared, blocked, ignored, caught)
    };
    assert_eq!(fact(SELF_PENDING), expect(usr1, 0, usr1, (0, 0)));
    assert_eq!(fact(SHARED_PENDING), expect(0, usr2, usr2, (0, 0)));
    assert_eq!(fact(ACTIONS), expect(0, 0, 0, actions));
    // Queued twice for the process, shown once.
    let realtime = bit(libc::SIGRTMIN());
    assert_eq!(fact(REALTIME), expect(0, realtime, realtime, actions));
    // Every signal but SIGKILL and SIGSTOP, which the kernel leaves out.
    let full = !(bit(libc::SIGKILL) | bit(libc::SIGSTOP));
    assert_eq!(fact(FULL_MASK), expect(0, 0, full, actions));
    let pipe = bit(libc::SIGPIPE);
    assert_eq!(
        fact(IN_HANDLER),
        expect(0, 0, pipe | bit(libc::SIGWINCH) | usr2, actions)
    );
    assert_eq!(fact(AFTER_HANDLER), expect(0, 0, pipe, actions));
    let caught = (bit(libc::SIGHUP), usr1 | usr2);
    assert_eq!(
        fact(SIBLING),
        expect(usr1, 0, usr1 | bit(libc::SIGINT), caught)
    );
    assert_eq!(fact(LEADER_FROM_SIBLING), expect(0, 0, 0, caught));
    assert_ne!(facts[&SIBLING].0, facts[&LEADER_FROM_SIBLING].0);
    assert_eq!(facts[&LEADER_FROM_SIBLING].0, facts[&SELF_PENDING].0);
    for point in [OTHER_PROCESS, NO_TASK, ZERO, NEGATIVE] {
        assert_eq!(
            facts[&point].1,
            Err(FactError::Errno(Errno::ESRCH)),
            "point {point}"
        );
    }
    assert!(facts[&OTHER_PROCESS].0 > 0);
}
