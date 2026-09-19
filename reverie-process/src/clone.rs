/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use syscalls::Errno;

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

// The owned Container route deliberately has no arbitrary-flags interface.
// In particular, VM/files/signal handlers and the parent-tid output are private.
pub(super) struct OwnedClone {
    pub(super) pid: Pid,
    pub(super) pidfd: Option<std::os::fd::OwnedFd>,
}

pub(super) fn clone_with_stack_owned<F>(
    cb: F,
    namespaces: super::Namespace,
    stack: &mut [u8],
) -> Result<OwnedClone, Errno>
where
    F: FnMut() -> i32,
{
    use std::os::fd::FromRawFd;

    if namespaces.bits() & !super::Namespace::all().bits() != 0 {
        return Err(Errno::EINVAL);
    }
    // CLONE_PIDFD reused a formerly ignored bit. An actual pidfd_open probe
    // refuses unsupported kernels before clone, rather than guessing a version.
    #[cfg(test)]
    if let OwnedCloneTestFault::Probe(error) = OWNED_CLONE_FAULT.with(|f| f.get()) {
        return Err(error);
    }
    drop(super::fd::Fd::pidfd_open(unsafe { libc::getpid() }, 0)?);
    #[cfg(test)]
    if OWNED_CLONE_FAULT.with(|f| f.get()) == OwnedCloneTestFault::ExhaustAtClone {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        Errno::result(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) })?;
    }

    type CloneCb<'a> = Box<dyn FnMut() -> i32 + 'a>;
    extern "C" fn callback(data: *mut CloneCb) -> libc::c_int {
        let cb: &mut CloneCb = unsafe { &mut *data };
        (*cb)() as libc::c_int
    }
    // As in the legacy helper, this box is destroyed in O immediately on
    // return. Container supplies a borrowing adapter, never teardown owners.
    let mut cb: CloneCb = Box::new(cb);
    let mut pidfd = -1;
    let result = unsafe {
        let top = stack.as_mut_ptr().add(stack.len());
        let top = top.sub(top as usize % 16);
        libc::clone(
            core::mem::transmute::<
                extern "C" fn(*mut CloneCb) -> i32,
                extern "C" fn(*mut libc::c_void) -> libc::c_int,
            >(callback),
            top.cast(),
            namespaces.bits() | libc::SIGCHLD | libc::CLONE_PIDFD,
            (&mut cb as *mut CloneCb).cast::<libc::c_void>(),
            // libc's parent_tid (first vararg), NOT the raw syscall order.
            &mut pidfd as *mut libc::c_int,
            std::ptr::null_mut::<libc::c_void>(),
            std::ptr::null_mut::<libc::c_int>(),
        )
    };
    let pid = Pid::from_raw(Errno::result(result)?);
    let pidfd = if pidfd >= 0 {
        // SAFETY: successful CLONE_PIDFD created exactly this parent-owned FD.
        Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(pidfd) })
    } else {
        // A violated kernel contract still leaves an actual child wait owner.
        // The caller must refuse permission, not manufacture pidfd authority.
        None
    };
    #[cfg(test)]
    let pidfd = if OWNED_CLONE_FAULT.with(|f| f.get()) == OwnedCloneTestFault::MissingPidfd {
        drop(pidfd);
        None
    } else {
        pidfd
    };
    Ok(OwnedClone { pid, pidfd })
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OwnedCloneTestFault {
    None,
    Probe(Errno),
    ExhaustAtClone,
    MissingPidfd,
}
#[cfg(test)]
thread_local! {
    pub(super) static OWNED_CLONE_FAULT: std::cell::Cell<OwnedCloneTestFault> = const { std::cell::Cell::new(OwnedCloneTestFault::None) };
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
