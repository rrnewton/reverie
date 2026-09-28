/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The embedding process reserves Linux signal 64 for KVM entry control.
//! SIGURG remains the independent worker-cancellation signal.

use std::marker::PhantomData;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::OnceLock;

use kvm_ioctls::VcpuFd;

use super::failure;
use super::protocol_failure;
use crate::Result;

const SIGNAL: libc::c_int = 64;
// _IOW(KVMIO, 0x8b, struct kvm_signal_mask). The fixed header is four
// bytes; the eight-byte sigset is the struct's flexible-array payload.
const KVM_SET_SIGNAL_MASK: libc::c_ulong = 0x4004_ae8b;

extern "C" fn interrupt(_signal: libc::c_int) {}

fn last_error(operation: &'static str) -> crate::Error {
    failure(operation, std::io::Error::last_os_error())
}

fn install_handler() -> Result<()> {
    static INSTALL: OnceLock<std::result::Result<(), i32>> = OnceLock::new();
    let result = INSTALL.get_or_init(|| {
        // SAFETY: the supplied action storage is initialized and lives through
        // each call. The handler performs no work, allocation or locking.
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
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(SIGNAL, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO));
            }
            Ok(())
        }
    });
    if let Err(errno) = result {
        return Err(failure(
            "reserve host signal 64",
            std::io::Error::from_raw_os_error(*errno),
        ));
    }
    // A default-disposition check cannot exclude a future foreign owner. The
    // lifetime reservation is an embedding contract; detect a visible change
    // rather than silently replacing a library's handler on later entries.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(SIGNAL, std::ptr::null(), &mut current) != 0 {
            return Err(last_error("query reserved host signal"));
        }
        if current.sa_sigaction != interrupt as *const () as usize {
            return Err(protocol_failure("reserved host signal disposition changed"));
        }
    }
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

fn ioctl_mask(saved: &libc::sigset_t) -> Result<[u8; 12]> {
    let mut bits = 0u64;
    for signal in 1..=64 {
        // SAFETY: saved is a valid libc sigset and the queried number is valid.
        match unsafe { libc::sigismember(saved, signal) } {
            0 => {}
            1 => bits |= 1u64 << (signal - 1),
            _ => return Err(last_error("read saved signal mask")),
        }
    }
    bits &= !(1u64 << (SIGNAL - 1));
    let mut bytes = [0; 12];
    bytes[..4].copy_from_slice(&8u32.to_ne_bytes());
    bytes[4..].copy_from_slice(&bits.to_ne_bytes());
    Ok(bytes)
}

pub(super) struct Mask {
    saved: libc::sigset_t,
    finished: bool,
    _same_thread: PhantomData<Rc<()>>,
}

impl Mask {
    pub(super) fn block() -> Result<Self> {
        install_handler()?;
        let set = only_reserved_signal();
        // SAFETY: the mask and saved-mask storage are valid. pthread_sigmask
        // affects only this thread, whose guard is deliberately not Send.
        let mut saved = unsafe { std::mem::zeroed() };
        let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut saved) };
        if error != 0 {
            return Err(failure(
                "block reserved host signal",
                std::io::Error::from_raw_os_error(error),
            ));
        }
        let mut mask = Self {
            saved,
            finished: false,
            _same_thread: PhantomData,
        };
        let check = (|| {
            // No gate sender can know this thread until entry publication.
            // An already pending instance therefore has no legitimate sender.
            let mut pending = unsafe { std::mem::zeroed() };
            if unsafe { libc::sigpending(&mut pending) } != 0 {
                return Err(last_error("query pending reserved host signal"));
            }
            if unsafe { libc::sigismember(&pending, SIGNAL) } != 0 {
                return Err(protocol_failure(
                    "unexplained reserved host signal before entry",
                ));
            }
            Ok(())
        })();
        if let Err(error) = check {
            // This signal was not sent by us. Do not synchronously consume it.
            let cleanup = mask.restore();
            mask.finished = true;
            return Err(match cleanup {
                Ok(()) => error,
                Err(cleanup) => error.with_cleanup(vec![cleanup]),
            });
        }
        Ok(mask)
    }

    pub(super) fn install(&self, fd: &VcpuFd) -> Result<()> {
        let bytes = ioctl_mask(&self.saved)?;
        // SAFETY: Linux copies the four-byte length followed immediately by
        // eight mask bytes. There is no padded u64 field or unaligned reference.
        // fd is live, and the kernel only reads this buffer during the ioctl.
        if unsafe { libc::ioctl(fd.as_raw_fd(), KVM_SET_SIGNAL_MASK, bytes.as_ptr()) } != 0 {
            return Err(last_error("install KVM temporary signal mask"));
        }
        Ok(())
    }

    fn drain(&self) -> Result<()> {
        let set = only_reserved_signal();
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            // SAFETY: the mask and timespec are live; no siginfo is requested.
            // The reserved signal stays blocked on this thread through drain.
            let signal = unsafe { libc::sigtimedwait(&set, std::ptr::null_mut(), &timeout) };
            if signal == SIGNAL {
                continue;
            }
            if signal != -1 {
                return Err(protocol_failure("drain returned an unrequested signal"));
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) => return Ok(()),
                _ => return Err(last_error("drain reserved host signal")),
            }
        }
    }

    fn restore(&self) -> Result<()> {
        // SAFETY: saved is this same thread's complete original libc sigset.
        let error =
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.saved, std::ptr::null_mut()) };
        if error == 0 {
            Ok(())
        } else {
            Err(failure(
                "restore host signal mask",
                std::io::Error::from_raw_os_error(error),
            ))
        }
    }

    /// The registry must have withdrawn the send target before this call.
    pub(super) fn finish(mut self) -> Result<()> {
        let drain = self.drain();
        let restore = self.restore();
        self.finished = true;
        match (drain, restore) {
            (Ok(()), result) | (result, Ok(())) => result,
            (Err(primary), Err(cleanup)) => Err(primary.with_cleanup(vec![cleanup])),
        }
    }
}

impl Drop for Mask {
    fn drop(&mut self) {
        if !self.finished {
            // The owning RunEntry normally performs both operations and keeps
            // their errors. This fallback only prevents an unwind from leaving
            // an avoidably changed thread mask; it does not acknowledge entry.
            let _ = self.restore();
        }
    }
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

    #[test]
    fn kernel_mask_uses_the_flexible_array_offset() {
        let mut saved = only_reserved_signal();
        unsafe {
            assert_eq!(libc::sigaddset(&mut saved, libc::SIGHUP), 0);
            assert_eq!(libc::sigaddset(&mut saved, libc::SIGUSR1), 0);
            assert_eq!(libc::sigaddset(&mut saved, libc::SIGURG), 0);
        }
        let bytes = ioctl_mask(&saved).unwrap();
        assert_eq!(bytes.len(), 12);
        assert_eq!(u32::from_ne_bytes(bytes[0..4].try_into().unwrap()), 8);
        let bits = u64::from_ne_bytes(bytes[4..12].try_into().unwrap());
        assert_eq!(
            bits,
            1 | (1 << (libc::SIGUSR1 - 1)) | (1 << (libc::SIGURG - 1))
        );
        assert_eq!(bits & (1u64 << 63), 0);
    }

    extern "C" fn foreign_handler(_: i32) {}

    #[test]
    fn reserved_signal_protocol() {
        const CHILD: &str = "REVERIE_KVM_ENTRY_SIGNAL_CHILD";
        let Ok(case) = std::env::var(CHILD) else {
            // Disposition ownership and unexplained pending signals are tested
            // in fresh processes. They must not alter the parallel test runner.
            for case in ["blocked", "unblocked", "pending", "ignored", "foreign"] {
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
            let error = match Mask::block() {
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
        if case == "pending" {
            unsafe {
                assert_eq!(libc::pthread_kill(libc::pthread_self(), SIGNAL), 0);
            }
            assert!(Mask::block().is_err());
            assert!(pending(SIGNAL), "refusal consumed an unexplained signal");
            assert!(contains(&current_mask(), SIGNAL));
            return;
        }
        let mask = Mask::block().unwrap();
        assert!(contains(&current_mask(), SIGNAL));
        for _ in 0..3 {
            unsafe {
                assert_eq!(libc::pthread_kill(libc::pthread_self(), SIGNAL), 0);
            }
        }
        unsafe {
            assert_eq!(libc::pthread_kill(libc::pthread_self(), libc::SIGURG), 0);
        }
        assert!(pending(SIGNAL));
        mask.finish().unwrap();
        assert!(!pending(SIGNAL));
        assert!(pending(libc::SIGURG), "entry cleanup consumed cancellation");
        let restored = current_mask();
        for signal in 1..=64 {
            assert_eq!(
                contains(&restored, signal),
                contains(&original, signal),
                "signal {signal}"
            );
        }
        // A new entry must not observe a stale queued gate signal.
        Mask::block().unwrap().finish().unwrap();
    }
}
