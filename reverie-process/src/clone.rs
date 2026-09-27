/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use syscalls::Errno;

use super::Pid;

/// Usable bytes in a cloned child's stack.
///
/// The in-container tracer's main thread runs on this stack, including the
/// tokio `block_on` frames and everything the tool does underneath them, so it
/// needs what an ordinary main thread gets: 8 MiB is the default
/// `RLIMIT_STACK`. The previous 2 MiB was measured to be too small: in a debug
/// build of Hermit, the LiteInst statistics path behind
/// `--backend-engagement-json` used exactly 0x200000 bytes of it at the crash,
/// about 1.1 MB in Hermit's own async frames alone, and the overflow silently
/// corrupted the heap below the allocation until glibc aborted with
/// `malloc(): invalid next size`. With 8 MiB the same runs pass. Only the
/// pages the child actually touches are backed by memory.
pub(super) const CHILD_STACK_SIZE: usize = 8 * 1024 * 1024;

/// A stack for a cloned child: an anonymous private mapping whose lowest page
/// is `PROT_NONE`, so running off the end faults instead of overwriting
/// whatever lies below. A heap `Vec` has no such page: once glibc has raised
/// its dynamic mmap threshold (it does after freeing a large mapped chunk), a
/// 2 MiB buffer is served from an ordinary malloc heap, where an overflow
/// lands directly on live allocator metadata.
///
/// Dereferences to the usable bytes only; the guard page is never part of the
/// slice. Allocate it before `clone` so the child stays allocation-free.
pub(super) struct ChildStack {
    /// Start of the whole mapping, i.e. of the guard page.
    mapping: std::ptr::NonNull<u8>,
    mapping_len: usize,
    guard_len: usize,
}

impl ChildStack {
    /// Maps `usable` bytes (rounded up to whole pages) above one guard page.
    pub(super) fn new(usable: usize) -> Result<Self, Errno> {
        let page = Errno::result(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })? as usize;
        let usable = usable
            .checked_next_multiple_of(page)
            .filter(|&n| n > 0)
            .ok_or(Errno::EINVAL)?;
        let mapping_len = usable.checked_add(page).ok_or(Errno::ENOMEM)?;
        // Map everything inaccessible first, then open up the part above the
        // guard, so there is never a moment where the guard is writable.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapping_len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_STACK,
                -1,
                0,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(Errno::last());
        }
        let mapping = std::ptr::NonNull::new(mapping.cast::<u8>()).ok_or(Errno::ENOMEM)?;
        // From here on Drop owns the mapping, including on the error path.
        let stack = Self {
            mapping,
            mapping_len,
            guard_len: page,
        };
        Errno::result(unsafe {
            libc::mprotect(
                stack.usable_ptr().cast(),
                usable,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        })?;
        Ok(stack)
    }

    fn usable_ptr(&self) -> *mut u8 {
        // SAFETY: guard_len < mapping_len, so this stays inside the mapping.
        unsafe { self.mapping.as_ptr().add(self.guard_len) }
    }

    fn usable_len(&self) -> usize {
        self.mapping_len - self.guard_len
    }
}

impl std::ops::Deref for ChildStack {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: the usable range is mapped readable and writable for the
        // lifetime of self and is only reachable through self.
        unsafe { std::slice::from_raw_parts(self.usable_ptr(), self.usable_len()) }
    }
}

impl std::ops::DerefMut for ChildStack {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in deref, and &mut self makes the borrow unique.
        unsafe { std::slice::from_raw_parts_mut(self.usable_ptr(), self.usable_len()) }
    }
}

impl Drop for ChildStack {
    fn drop(&mut self) {
        // A failure here would mean the range is no longer a mapping we own;
        // there is nothing useful to do about it in a destructor.
        unsafe { libc::munmap(self.mapping.as_ptr().cast(), self.mapping_len) };
    }
}

pub(super) fn child_stack() -> Result<ChildStack, Errno> {
    ChildStack::new(CHILD_STACK_SIZE)
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
    let mut stack = child_stack()?;
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
        // The caller must retain that wait and refuse success, not manufacture
        // pidfd authority or assume that the child has not started.
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
            child_stack().unwrap().len() >= 2 * 1024 * 1024,
            "the cloned child runs container setup before exec and needs at least 2 MiB"
        );
    }

    #[test]
    fn default_child_stack_is_a_main_thread_stack() {
        assert!(
            child_stack().unwrap().len() >= 8 * 1024 * 1024,
            "the in-container tracer's main thread runs on the child stack and needs the \
             8 MiB a main thread gets; 2 MiB was measured to overflow"
        );
    }

    fn page_size() -> usize {
        Errno::result(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap() as usize
    }

    /// Returns (start, end, permissions) of every mapping in /proc/self/maps.
    fn mappings() -> Vec<(usize, usize, String)> {
        std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .map(|line| {
                let mut fields = line.split_whitespace();
                let (start, end) = fields.next().unwrap().split_once('-').unwrap();
                (
                    usize::from_str_radix(start, 16).unwrap(),
                    usize::from_str_radix(end, 16).unwrap(),
                    fields.next().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn child_stack_sits_directly_above_an_inaccessible_guard_page() {
        let stack = child_stack().unwrap();
        let start = stack.as_ptr() as usize;
        let end = start + stack.len();
        let maps = mappings();

        let below = maps.iter().find(|(_, hi, _)| *hi == start);
        let Some((lo, _, perms)) = below else {
            let containing = maps.iter().find(|(lo, hi, _)| *lo < start && start <= *hi);
            panic!(
                "no mapping ends at the child stack's first byte {start:#x}, so the byte below it \
                 is not a guard page; the mapping around it is {containing:x?}"
            );
        };
        assert_eq!(
            perms, "---p",
            "the mapping directly below the child stack must be private and inaccessible"
        );
        assert!(
            start - lo >= page_size(),
            "the guard below the child stack must be at least one page"
        );

        let usable = maps
            .iter()
            .find(|(lo, hi, _)| *lo <= start && end <= *hi)
            .expect("the usable child stack must lie inside a single mapping");
        assert_eq!(
            usable.2, "rw-p",
            "the usable child stack must be read-write"
        );
    }

    #[test]
    fn a_cloned_child_writing_below_its_stack_is_killed_by_sigsegv() {
        let mut stack = child_stack().unwrap();
        let below = (stack.as_ptr() as usize) - 1;
        // No CLONE_VM: the child gets its own copy of the mapping, so the write
        // (when it does not fault) cannot disturb the test process.
        let pid = clone_with_stack(
            move || {
                unsafe {
                    // An intended crash should not leave a core file behind.
                    libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
                    std::ptr::write_volatile(below as *mut u8, 0xa5);
                    // Reached only if the write did not fault. _exit skips the
                    // libc exit handlers, which are unsafe in a child forked
                    // from the threaded test harness.
                    libc::_exit(0)
                }
            },
            libc::SIGCHLD,
            &mut stack,
        )
        .unwrap();

        let mut status = 0;
        loop {
            let waited = unsafe { libc::waitpid(pid.as_raw(), &mut status, 0) };
            if waited == pid.as_raw() {
                break;
            }
            assert_eq!(waited, -1);
            assert_eq!(Errno::last(), Errno::EINTR);
        }
        assert!(
            libc::WIFSIGNALED(status) && libc::WTERMSIG(status) == libc::SIGSEGV,
            "a child that writes one byte below its stack must be killed by SIGSEGV, \
             but its wait status was {status:#x}"
        );
    }
}
