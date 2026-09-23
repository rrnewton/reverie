/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;

use syscalls::Errno;

use super::ControllerLaunchId;
use super::ControllerSpawnToken;
use super::Pid;

pub(super) const CHILD_STACK_SIZE: usize = 2 * 1024 * 1024;

pub(super) fn child_stack() -> Vec<u8> {
    vec![0u8; CHILD_STACK_SIZE]
}

pub fn clone<F>(cb: F, flags: libc::c_int) -> Result<Pid, Errno>
where
    F: FnMut() -> i32,
{
    // The child runs container setup and libc's exec path on this stack. In an
    // optimized build, Mount::mount alone can reserve PATH_MAX bytes in its
    // frame, so one page cannot hold that call plus its callers. Match the
    // stack size Container::run provides for the same setup path, and allocate
    // it before clone so the child remains allocation-free before exec.
    let mut stack = child_stack();
    clone_with_stack(cb, flags, &mut stack)
}

pub fn clone_with_stack<F>(cb: F, flags: libc::c_int, stack: &mut [u8]) -> Result<Pid, Errno>
where
    F: FnMut() -> i32,
{
    type CloneCb<'a> = Box<dyn FnMut() -> i32 + 'a>;

    extern "C" fn callback(data: *mut CloneCb) -> libc::c_int {
        let cb: &mut CloneCb = unsafe { &mut *data };
        (*cb)() as libc::c_int
    }

    let mut cb: CloneCb = Box::new(cb);

    let res = unsafe {
        let stack = stack.as_mut_ptr().add(stack.len());
        let stack = stack.sub(stack as usize % 16);

        libc::clone(
            core::mem::transmute::<
                extern "C" fn(*mut Box<dyn FnMut() -> i32>) -> i32,
                extern "C" fn(*mut libc::c_void) -> libc::c_int,
            >(callback as extern "C" fn(*mut Box<dyn FnMut() -> i32>) -> i32),
            stack as *mut libc::c_void,
            flags,
            &mut cb as *mut _ as *mut libc::c_void,
        )
    };

    Errno::result(res).map(Pid::from_raw)
}

pub(super) enum ClonePidfdResult {
    /// Clone returned both the child PID and its atomic pidfd.
    Created(ControllerSpawnToken),
    /// Clone left the initialized `-1` pidfd sentinel unchanged.
    UnsupportedKernelContract(Pid),
    /// Clone returned a pidfd, but descriptor validation failed while the
    /// exact token retained authority and the child remained gated.
    PidfdValidationFailed {
        token: ControllerSpawnToken,
        error: Errno,
    },
}

/// Proves the upstream Linux pidfd operations required by controller launch.
pub(super) fn probe_clone_pidfd_support() -> Result<(), Errno> {
    let raw =
        unsafe { syscalls::syscall2(syscalls::Sysno::pidfd_open, libc::getpid() as usize, 0) }?
            as libc::c_int;
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw) };
    let fd_flags = Errno::result(unsafe { libc::fcntl(pidfd.as_raw_fd(), libc::F_GETFD) })?;
    if fd_flags & libc::FD_CLOEXEC == 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    Errno::result(unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            0,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    })?;
    let mut siginfo = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    let wait_result = unsafe {
        libc::waitid(
            libc::P_PIDFD,
            pidfd.as_raw_fd() as libc::id_t,
            siginfo.as_mut_ptr(),
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if wait_result == -1 {
        let error = Errno::last();
        if error == Errno::ECHILD {
            return Ok(());
        }
        return Err(error);
    }
    Err(Errno::EOPNOTSUPP)
}

/// Clones the controller root and requests its pidfd in the parent-tid slot.
pub(super) fn clone_with_pidfd<F>(
    cb: F,
    flags: libc::c_int,
    launch: ControllerLaunchId,
) -> Result<ClonePidfdResult, Errno>
where
    F: FnMut() -> i32,
{
    type CloneCb<'a> = Box<dyn FnMut() -> i32 + 'a>;

    extern "C" fn callback(data: *mut CloneCb) -> libc::c_int {
        let cb: &mut CloneCb = unsafe { &mut *data };
        (*cb)() as libc::c_int
    }

    const EXIT_SIGNAL_MASK: libc::c_int = 0xff;
    const ALLOWED_NAMESPACE_FLAGS: libc::c_int = libc::CLONE_NEWCGROUP
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWUSER
        | libc::CLONE_NEWUTS;
    if flags & EXIT_SIGNAL_MASK != libc::SIGCHLD
        || flags & !EXIT_SIGNAL_MASK & !ALLOWED_NAMESPACE_FLAGS != 0
        || flags
            & (libc::CLONE_PIDFD
                | libc::CLONE_PARENT_SETTID
                | libc::CLONE_PARENT
                | libc::CLONE_THREAD
                | libc::CLONE_FILES
                | libc::CLONE_VM
                | libc::CLONE_SIGHAND
                | libc::CLONE_VFORK
                | libc::CLONE_DETACHED)
            != 0
    {
        return Err(Errno::EINVAL);
    }

    let mut stack = child_stack();
    let mut cb: CloneCb = Box::new(cb);
    let mut pidfd = -1;
    let res = unsafe {
        let stack = stack.as_mut_ptr().add(stack.len());
        let stack = stack.sub(stack as usize % 16);

        libc::clone(
            core::mem::transmute::<
                extern "C" fn(*mut Box<dyn FnMut() -> i32>) -> i32,
                extern "C" fn(*mut libc::c_void) -> libc::c_int,
            >(callback as extern "C" fn(*mut Box<dyn FnMut() -> i32>) -> i32),
            stack as *mut libc::c_void,
            flags | libc::CLONE_PIDFD,
            &mut cb as *mut _ as *mut libc::c_void,
            std::ptr::from_mut(&mut pidfd),
            std::ptr::null_mut::<libc::c_void>(),
            std::ptr::null_mut::<libc::pid_t>(),
        )
    };
    if res == -1 {
        return Err(Errno::last());
    }
    let pid = Pid::from_raw(res);
    if pidfd == -1 {
        // Upstream Linux has honored CLONE_PIDFD since 5.2, while P_PIDFD
        // waitid used by the preflight arrived in 5.4. A kernel which passes
        // all three pidfd probes but silently ignores this legacy clone flag is
        // outside the supported contract. The caller keeps the child behind a
        // pre-exec gate and must contain/reap it without ever releasing exec.
        return Ok(ClonePidfdResult::UnsupportedKernelContract(pid));
    }
    if pidfd < -1 {
        // No exact descriptor authority exists for an out-of-contract negative
        // value, while the gated child is still protected by PDEATHSIG.
        std::process::abort();
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
    let token = ControllerSpawnToken::new(launch, pid, pidfd);
    match token.validate_pidfd_cloexec() {
        Ok(()) => Ok(ClonePidfdResult::Created(token)),
        Err(error) => Ok(ClonePidfdResult::PidfdValidationFailed { token, error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_child_stack_keeps_the_container_run_minimum() {
        assert!(
            child_stack().len() >= 2 * 1024 * 1024,
            "the cloned child runs container setup before exec and needs at least 2 MiB"
        );
    }
}
