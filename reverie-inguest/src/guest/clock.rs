/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The in-guest retired-conditional-branch (RCB) clock: a per-thread
//! performance counter read in-process, with the branches retired inside Tool
//! callbacks subtracted, so a Tool reads the guest's own progress. A forked or
//! cloned thread binds a fresh counter on first use.

use core::cell::Cell;
use core::ptr;
use std::io;

use crate::trap::raw_syscall6;

thread_local! {
    static RCB_CLOCK: Cell<*mut reverie_ptrace::InGuestRcbCounter> =
        const { Cell::new(ptr::null_mut()) };
    static RCB_CLOCK_OWNER: Cell<libc::pid_t> = const { Cell::new(0) };
    static RCB_CLOCK_UNAVAILABLE: Cell<bool> = const { Cell::new(false) };
    static RCB_HANDLER_ENTRY: Cell<u64> = const { Cell::new(0) };
    static RCB_HANDLER_DEDUCTION: Cell<u64> = const { Cell::new(0) };
    static RCB_HANDLER_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Install the current thread's in-guest RCB clock before seccomp is active.
/// A host without a usable branch counter leaves the clock unavailable, which
/// is not an error; [`read_guest_rcb_clock`] then reports `Unsupported`.
pub fn initialize_rcb_clock() -> io::Result<()> {
    initialize_rcb_clock_with(|| unsafe {
        reverie_ptrace::InGuestRcbCounter::current_thread_with_syscall_gate(raw_syscall6)
    })
}

fn initialize_rcb_clock_with(
    create: impl FnOnce() -> Result<reverie_ptrace::InGuestRcbCounter, reverie::Errno>,
) -> io::Result<()> {
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
    if owner <= 0 {
        return Err(io::Error::last_os_error());
    }
    // A fork child can first discover that its inherited counter has the wrong
    // owner from inside the still-active fork callback. Preserve that callback
    // depth while replacing the counter; resetting it would make the outer
    // leave underflow after child reconstruction completes.
    let active_depth = RCB_HANDLER_DEPTH.get();
    // Publish an unavailable sentinel before creating the perf event. When a
    // fork child first initializes after seccomp is active, the builder's own
    // syscalls can re-enter an already-patched syscall hook; that nested hook
    // must observe this owner as initialized instead of recursively creating
    // another counter.
    RCB_CLOCK.set(ptr::null_mut());
    RCB_CLOCK_OWNER.set(owner);
    RCB_CLOCK_UNAVAILABLE.set(true);
    RCB_HANDLER_ENTRY.set(0);
    RCB_HANDLER_DEDUCTION.set(0);
    RCB_HANDLER_DEPTH.set(active_depth);
    let clock = match create() {
        Ok(clock) => clock,
        // The in-guest clock is optional. CPU discovery, perf-event setup,
        // mmap, reset, and enable failures all mean unavailable, not a failed
        // Tool installation.
        Err(_) => return Ok(()),
    };
    let active_entry = if active_depth == 0 {
        0
    } else {
        clock
            .read()
            .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?
    };
    RCB_CLOCK.set(Box::into_raw(Box::new(clock)));
    RCB_CLOCK_OWNER.set(owner);
    RCB_CLOCK_UNAVAILABLE.set(false);
    RCB_HANDLER_ENTRY.set(active_entry);
    RCB_HANDLER_DEDUCTION.set(0);
    RCB_HANDLER_DEPTH.set(active_depth);
    Ok(())
}

fn rcb_clock() -> io::Result<Option<&'static reverie_ptrace::InGuestRcbCounter>> {
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
    if RCB_CLOCK_OWNER.get() != owner {
        // A fork/clone child inherits the parent's TLS bytes, including an fd
        // that still measures the parent thread. Leak that inherited handle
        // and bind a fresh PMU event to this calling thread. Binding is the
        // runtime's own code, not the guest's: it reads CPUID (PMU discovery),
        // which must run natively, as inside a Tool callback, rather than be
        // emulated through the fallback continuation, which a fork child can
        // still be running when it first gets here.
        let _runtime_code = crate::guest::support::ToolCallbackGuard::enter();
        initialize_rcb_clock()?;
    }
    let current = RCB_CLOCK.get();
    if current.is_null() {
        debug_assert!(RCB_CLOCK_UNAVAILABLE.get());
        Ok(None)
    } else {
        Ok(Some(unsafe { &*current }))
    }
}

/// Mark entry into an ordinary-context tool callback: branches retired until
/// the matching [`leave_rcb_handler`] are the Tool's, not the guest's.
pub fn enter_rcb_handler() -> io::Result<()> {
    let Some(clock) = rcb_clock()? else {
        RCB_HANDLER_DEPTH.set(RCB_HANDLER_DEPTH.get().saturating_add(1));
        return Ok(());
    };
    let sample = clock
        .read()
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;
    if RCB_HANDLER_DEPTH.get() == 0 {
        RCB_HANDLER_ENTRY.set(sample);
    }
    RCB_HANDLER_DEPTH.set(RCB_HANDLER_DEPTH.get().saturating_add(1));
    Ok(())
}

/// Deduct all RCBs retired while the outermost tool callback was active.
pub fn leave_rcb_handler() -> io::Result<()> {
    let depth = RCB_HANDLER_DEPTH.get();
    if depth == 0 {
        return Err(io::Error::other("LiteInst RCB handler depth underflow"));
    }
    RCB_HANDLER_DEPTH.set(depth - 1);
    if depth != 1 {
        return Ok(());
    }
    let Some(clock) = rcb_clock()? else {
        return Ok(());
    };
    let sample = clock
        .read()
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;
    RCB_HANDLER_DEDUCTION.set(
        RCB_HANDLER_DEDUCTION
            .get()
            .saturating_add(sample.saturating_sub(RCB_HANDLER_ENTRY.get())),
    );
    RCB_HANDLER_ENTRY.set(0);
    Ok(())
}

/// Return guest-only RCB time, excluding all completed and currently-active
/// LiteInst handler branches.
pub fn read_guest_rcb_clock() -> io::Result<u64> {
    let Some(clock) = rcb_clock()? else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "LiteInst in-guest RCB clock is unavailable on this host",
        ));
    };
    let sample = clock
        .read()
        .map_err(|error| io::Error::from_raw_os_error(error.into_raw()))?;
    let active = if RCB_HANDLER_DEPTH.get() == 0 {
        0
    } else {
        sample.saturating_sub(RCB_HANDLER_ENTRY.get())
    };
    Ok(sample
        .saturating_sub(RCB_HANDLER_DEDUCTION.get())
        .saturating_sub(active))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_optional_rcb_setup_error_takes_the_real_unavailable_path() {
        let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as libc::pid_t;
        assert!(owner > 0);
        for error in [
            reverie::Errno::EACCES,
            reverie::Errno::EPERM,
            reverie::Errno::ENODEV,
            reverie::Errno::EOPNOTSUPP,
            reverie::Errno::EINVAL,
            reverie::Errno::EMFILE,
            reverie::Errno::ENFILE,
            reverie::Errno::EBUSY,
            reverie::Errno::EIO,
        ] {
            initialize_rcb_clock_with(|| Err(error)).unwrap();
            assert!(RCB_CLOCK.get().is_null());
            assert!(RCB_CLOCK_UNAVAILABLE.get());
            assert_eq!(RCB_CLOCK_OWNER.get(), owner);
        }
    }
}
