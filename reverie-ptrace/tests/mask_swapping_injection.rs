/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! An injected mask-swapping syscall waits under its own temporary mask, as
//! the guest's own call would. A pending signal that the thread's mask admits
//! and the call's mask blocks stays pending through the call
//! (<https://github.com/rrnewton/hermit/issues/4149>).

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::Errno;
use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Ppoll;
use reverie::syscalls::RtSigprocmask;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn;

/// A zero-length write to this descriptor, whose buffer is a `WaitArgs`, is
/// replaced with an injected `rt_sigprocmask(SIG_UNBLOCK, &mask, NULL)` and
/// then an injected `ppoll(NULL, 0, &timeout, &mask, 8)`. The tool returns
/// the `ppoll` result.
const UNBLOCK_THEN_PPOLL_FD: i32 = 902;

#[repr(C)]
struct WaitArgs {
    /// The kernel signal mask: the set the thread unblocks, and the `ppoll`'s
    /// temporary mask.
    mask: u64,
    timeout: libc::timespec,
}

#[derive(Default)]
struct Log {
    injected: Mutex<Vec<Result<i64, i32>>>,
}

#[reverie::global_tool]
impl GlobalTool for Log {
    type Request = Result<i64, i32>;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, result: Result<i64, i32>) {
        self.injected.lock().unwrap().push(result);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct MaskSwapTool;

#[reverie::tool]
impl Tool for MaskSwapTool {
    type GlobalState = Log;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        let mut subscription = Subscription::none();
        subscription.syscall(Sysno::write);
        subscription
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        match syscall {
            Syscall::Write(write) if write.fd() == UNBLOCK_THEN_PPOLL_FD && write.len() == 0 => {
                let args = write.buf().map_or(0, |buf| buf.as_raw());
                let mask = Addr::from_raw(args);
                let unblocked = guest
                    .inject(
                        RtSigprocmask::new()
                            .with_how(libc::SIG_UNBLOCK)
                            .with_set(mask)
                            .with_oldset(None)
                            .with_sigsetsize(8),
                    )
                    .await;
                guest.send_rpc(unblocked.map_err(Errno::into_raw)).await;
                let result = guest
                    .inject(
                        Ppoll::new()
                            .with_fds(None)
                            .with_nfds(0)
                            .with_timeout(AddrMut::from_raw(
                                args + std::mem::offset_of!(WaitArgs, timeout),
                            ))
                            .with_sigmask(mask)
                            .with_sigsetsize(8),
                    )
                    .await;
                guest.send_rpc(result.map_err(Errno::into_raw)).await;
                Ok(result?)
            }
            _ => guest.tail_inject(syscall).await,
        }
    }
}

static DELIVERIES: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_delivery(_signal: libc::c_int) {
    DELIVERIES.fetch_add(1, Ordering::SeqCst);
}

/// The thread blocks SIGUSR1 and sends it to itself. The tool unblocks it
/// with an injected `rt_sigprocmask`, so it is pending and admitted, and then
/// injects a 50 ms `ppoll` whose temporary mask blocks it. Linux keeps the
/// signal pending through the guest's own such call, which times out, and
/// runs the handler when the call returns. Before, the injection left the
/// private page under the thread's own mask, the signal stopped it before
/// its `syscall`, the injection reported ERESTARTSYS, and the guest's call
/// returned EINTR without waiting.
#[test]
fn a_signal_the_temporary_mask_blocks_stays_pending_through_an_injected_ppoll() {
    let (output, log) = test_fn::<MaskSwapTool, _>(|| {
        DELIVERIES.store(0, Ordering::SeqCst);
        let signal = libc::SIGUSR1;
        // SAFETY: the action and sets are fully initialized before use; the
        // handler touches only an atomic.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = count_delivery as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, signal);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()),
                0
            );
            let ret = libc::syscall(
                libc::SYS_tgkill,
                libc::getpid(),
                libc::syscall(libc::SYS_gettid),
                signal,
            );
            assert_eq!(ret, 0);
        }
        // The kernel writes the remaining time back to `timeout`.
        let mut args = WaitArgs {
            mask: 1 << (signal - 1),
            timeout: libc::timespec {
                tv_sec: 0,
                tv_nsec: 50_000_000,
            },
        };
        // SAFETY: the tool replaces this zero-length write.
        let ret = unsafe {
            libc::write(
                UNBLOCK_THEN_PPOLL_FD,
                &mut args as *mut WaitArgs as *const libc::c_void,
                0,
            )
        };
        let errno = std::io::Error::last_os_error().raw_os_error();
        assert_eq!(
            (ret, if ret < 0 { errno } else { None }),
            (0, None),
            "the injected ppoll must time out with the signal pending"
        );
        assert_eq!(DELIVERIES.load(Ordering::SeqCst), 1);
    })
    .expect("run mask-swapping injection guest");

    assert_eq!(
        output.status,
        ExitStatus::Exited(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(*log.injected.lock().unwrap(), vec![Ok(0), Ok(0)]);
}
