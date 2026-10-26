/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! What the in-guest runtime keeps the guest from: the descriptors it owns
//! (the coordinator connection, the Tool's reserved output socket and each
//! thread's performance-counter descriptor) and the process controls it relies
//! on (forking when the launcher forbids it, signal actions and alternate
//! stacks). A guest syscall that would reach one fails as Linux would fail it
//! on a descriptor or control the guest does not have (`close` included, with
//! `EBADF`), and a `close_range` over one closes the rest of its range without
//! touching it.
//!
//! The runtime's descriptors live at or above [`RUNTIME_FD_MIN`] (1025), or
//! just below a soft `RLIMIT_NOFILE` that leaves no number free there (see
//! [`runtime_fd_minimums`]), clear of the numbers a program allocates first,
//! so a guest's own descriptor numbers are the ones it would get without the
//! runtime. [`is_runtime_owned_fd`] answers for the complete set this process
//! holds, its native-exec leases included.

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crate::dispatch::is_fork_like;
use crate::guest::event::SyscallEvent;
use crate::guest::signal::signal_action_supported;
use crate::reserved_fds;
use crate::trap::raw_syscall6;

static COORDINATOR_FD: AtomicI32 = AtomicI32::new(-1);

#[cfg(any(feature = "exec-native-injection", feature = "loader-private-runtime"))]
pub(super) fn coordinator_fd() -> Option<libc::c_int> {
    let fd = COORDINATOR_FD.load(Ordering::Acquire);
    (fd >= 0).then_some(fd)
}

/// An output descriptor the in-guest Tool owns; see [`reserve_tool_output_fd`].
static TOOL_OUTPUT_FD: AtomicI32 = AtomicI32::new(-1);
/// The lowest number a runtime descriptor takes where `RLIMIT_NOFILE` leaves
/// one free: the Tool output socket, the coordinator connection
/// ([`place_runtime_fd`]) and the performance counters
/// ([`record_runtime_fd`]). A program gets the lowest free number for its own
/// descriptors, so these stay out of its way unless it asks for this range.
pub const RUNTIME_FD_MIN: libc::c_int = 1025;
/// How many numbers just below the soft `RLIMIT_NOFILE` the runtime falls back
/// to when no number at or above [`RUNTIME_FD_MIN`] is free under that limit
/// (the common default soft limit is 1024). A process holds at most a few at
/// once (the coordinator connection, the Tool output socket, a counter, a fork
/// child's inherited copies until it closes them).
const RUNTIME_FD_RESERVE: libc::c_int = 8;

/// The minimums the runtime tries, in order, when it places one of its
/// descriptors: [`RUNTIME_FD_MIN`], then, when the soft `RLIMIT_NOFILE` is
/// below `RUNTIME_FD_MIN + RUNTIME_FD_RESERVE`, the top `RUNTIME_FD_RESERVE` (8)
/// numbers below that limit (never below 3), which take a descriptor only when
/// no number at or above `RUNTIME_FD_MIN` is free under the limit. When the
/// limit cannot be read, only `RUNTIME_FD_MIN`. Either way a program's first
/// descriptors are 3, 4 and so on, as without the runtime, whatever limit the
/// process started with; the runtime changes no limit. The guest sees the
/// descriptors' numbers only by asking for one of them (a `dup2` onto it fails
/// with EBADF) or by allocating up to the limit.
pub fn runtime_fd_minimums() -> impl Iterator<Item = libc::c_int> {
    core::iter::once(RUNTIME_FD_MIN).chain(runtime_fd_fallback_floor())
}

/// The fallback band's lowest number; see [`runtime_fd_minimums`].
fn runtime_fd_fallback_floor() -> Option<libc::c_int> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let queried = unsafe {
        raw_syscall6(
            libc::SYS_prlimit64,
            [
                0,
                libc::RLIMIT_NOFILE as u64,
                0,
                (&raw mut limit) as u64,
                0,
                0,
            ],
        )
    };
    let wanted = (RUNTIME_FD_MIN + RUNTIME_FD_RESERVE) as u64;
    (queried == 0 && limit.rlim_cur < wanted)
        .then(|| (limit.rlim_cur as libc::c_int - RUNTIME_FD_RESERVE).max(3))
}

/// Duplicates `fd`, close-on-exec, to the lowest free number at the first of
/// [`runtime_fd_minimums`] that has one. Returns the new number, or the negated
/// errno of the last attempt.
fn dup_into_runtime_range(fd: libc::c_int) -> i64 {
    let mut result = -i64::from(libc::EINVAL);
    for minimum in runtime_fd_minimums() {
        result = unsafe {
            raw_syscall6(
                libc::SYS_fcntl,
                [
                    fd as u64,
                    libc::F_DUPFD_CLOEXEC as u64,
                    minimum as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        if result >= 0 {
            break;
        }
    }
    result
}
/// How many descriptors one process records beside the coordinator connection
/// and the Tool output socket: a performance counter per thread that has one
/// (an in-guest process runs one guest thread, and a fork child holds its
/// parent's counter only until it binds its own), and any old number a
/// placement could not close.
const RECORDED_FD_CAPACITY: usize = 8;
/// The descriptors the runtime owns beside the coordinator connection and the
/// Tool output socket, each with the process that recorded it, packed as
/// `pid << 32 | fd` (0 is a free slot; see [`record_runtime_fd`]). A fork
/// child starts with its parent's record, which matches the copies of those
/// descriptors it inherits, until [`close_inherited_runtime_fds`] closes them.
static RECORDED_FDS: [AtomicU64; RECORDED_FD_CAPACITY] =
    [const { AtomicU64::new(0) }; RECORDED_FD_CAPACITY];

fn recorded_slot(pid: libc::pid_t, fd: libc::c_int) -> u64 {
    (u64::from(pid as u32) << 32) | u64::from(fd as u32)
}

fn recorded_slot_fd(slot: u64) -> Option<libc::c_int> {
    (slot != 0).then_some(slot as u32 as libc::c_int)
}

fn current_pid() -> libc::pid_t {
    unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) as libc::pid_t }
}
/// The message the runtime sends on the Tool output socket if it ever has to
/// give the socket up; see [`reserve_tool_output_fd`].
static TOOL_OUTPUT_RETIREMENT: OnceLock<&'static [u8]> = OnceLock::new();
static PROCESS_FORKS_ALLOWED: AtomicBool = AtomicBool::new(true);

/// Whether the guest may fork. Recorded once at installation from the
/// launcher's setting; [`protect_runtime_control`] refuses fork-like calls when
/// it is false.
pub fn set_process_forks_allowed(allowed: bool) {
    PROCESS_FORKS_ALLOWED.store(allowed, Ordering::Release);
}

/// Records `fd` as the coordinator connection, which the runtime then keeps
/// the guest away from (see [`protect_runtime_descriptors`]). Fails with
/// `AlreadyExists` if one is already recorded.
pub fn reserve_coordinator_fd(fd: libc::c_int) -> io::Result<()> {
    COORDINATOR_FD
        .compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "coordinator FD reserved twice",
            )
        })
}

/// Rebinds the protected coordinator descriptor after a fork child reconnects.
///
/// The recorded descriptor is process-local after fork, so this changes only the
/// child's protection slot; the parent's connection and descriptor are intact.
pub fn replace_coordinator_fd(old: libc::c_int, new: libc::c_int) -> io::Result<()> {
    COORDINATOR_FD
        .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|actual| {
            io::Error::other(format!(
                "coordinator FD changed concurrently: expected {old}, observed {actual}"
            ))
        })
}

/// Adds every signal to the calling thread's mask until the returned guard
/// drops, which restores the mask it found (the process ends with status 126
/// if that fails). Between the runtime's close of a number and its check of the
/// same number ([`record_if_open`]) no guest handler can then run and open a
/// descriptor of its own there, whichever signal and however it is sent. A
/// signal the guest left pending and blocked stays so. Fails when the mask
/// cannot be changed (a seccomp filter the guest installed can refuse it).
///
/// Code inside the window must make every syscall through the runtime's own
/// gate (`raw_syscall6`), never through libc, and must not execute CPUID or
/// RDTSC: a syscall from a patched or trapped site arrives as SIGSYS, and
/// while either instruction is subscribed it faults into SIGSEGV; the kernel
/// forces both, and a blocked forced signal takes its default action, killing
/// the process. This is the one deliberate exception to
/// [`crate::guest::signal::reserved_signal_mask`], which keeps those two
/// signals unblocked everywhere else; it holds because nothing in a window
/// raises either.
pub fn block_all_signals() -> io::Result<crate::guest::support::SignalInstallGuard> {
    let all: u64 = !0;
    let mut previous: u64 = 0;
    crate::guest::support::raw_zero_result(unsafe {
        raw_syscall6(
            libc::SYS_rt_sigprocmask,
            [
                libc::SIG_BLOCK as u64,
                (&raw const all) as u64,
                (&raw mut previous) as u64,
                core::mem::size_of::<u64>() as u64,
                0,
                0,
            ],
        )
    })?;
    Ok(crate::guest::support::SignalInstallGuard::restoring(
        previous,
    ))
}

/// Moves `fd`, a descriptor the runtime is about to own, into the runtime's
/// range ([`runtime_fd_minimums`]), close-on-exec, closes `fd`, and returns the
/// new number. When no number in that range is free, or the limit could not be
/// read and excludes [`RUNTIME_FD_MIN`], returns `fd` unchanged: the runtime
/// still owns and protects it there, and only the guest's own numbering
/// differs (its first descriptor is not 3).
///
/// When `fd` stays open after its close (a seccomp filter the guest installed
/// can report success without running it), the runtime records that number
/// too ([`record_runtime_fd`]), so both are protected and reported. When the
/// record is full that fails, and so does this, with both numbers still open;
/// the caller must not let the guest run on. Signals stay blocked throughout
/// ([`block_all_signals`]).
///
/// The returned number is not protected until the caller records it, and a
/// signal held during the placement is delivered as this returns; use
/// [`place_runtime_fd_and`] to record it first. Installation's coordinator
/// connection is still recorded after its handshake (`tool_install`), so a
/// handler the guest installed before the runtime's constructor could reach it
/// in between; installation resets the guest's handlers shortly after.
pub fn place_runtime_fd(fd: libc::c_int) -> io::Result<libc::c_int> {
    place_runtime_fd_and(fd, |_| Ok(()))
}

/// [`place_runtime_fd`], then `record(placed)` before signals are unblocked,
/// so a handler of a signal held during the placement already finds the
/// number protected. `record` runs inside the window and follows
/// [`block_all_signals`]'s rule; its error is returned with the number still
/// open.
pub fn place_runtime_fd_and(
    fd: libc::c_int,
    record: impl FnOnce(libc::c_int) -> io::Result<()>,
) -> io::Result<libc::c_int> {
    let _blocked = block_all_signals()?;
    let moved = dup_into_runtime_range(fd);
    let placed = if moved < 0 {
        fd
    } else {
        unsafe { raw_syscall6(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) };
        record_if_open(fd)?;
        moved as libc::c_int
    };
    record(placed)?;
    Ok(placed)
}

/// Records `fd` ([`record_runtime_fd`]) if it is still open: the old number of
/// a runtime descriptor the runtime has just closed, which a close a seccomp
/// filter faked or failed would leave as a second name for the descriptor.
/// Fails when the record is full or when the kernel does not say whether `fd`
/// is open ([`descriptor_is_open`]); the caller must not let the guest run on
/// then. The caller keeps signals blocked from the close to this check
/// ([`block_all_signals`]), so an open `fd` here is the old descriptor, not one
/// a guest handler opened at its number.
pub fn record_if_open(fd: libc::c_int) -> io::Result<()> {
    if descriptor_is_open(fd)? {
        record_runtime_fd(fd)?;
    }
    Ok(())
}

/// Whether `fd` is an open descriptor of this process, as the kernel answers
/// it: `F_GETFD` succeeds (open) or fails with `EBADF` (closed). Any other
/// failure (a seccomp filter the guest installed can refuse the query) proves
/// neither and is returned as the error. Linux releases a number even when
/// `close` reports `EINTR`, so a caller that must know asks again.
pub fn descriptor_is_open(fd: libc::c_int) -> io::Result<bool> {
    let flags = unsafe {
        raw_syscall6(
            libc::SYS_fcntl,
            [fd as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
        )
    };
    if flags >= 0 {
        Ok(true)
    } else if flags == -i64::from(libc::EBADF) {
        Ok(false)
    } else {
        Err(io::Error::from_raw_os_error(-flags as i32))
    }
}

/// Records `fd` as a descriptor the runtime owns (a thread's performance
/// counter, or an old number a placement could not close), which the runtime
/// then keeps the guest away from as it does the coordinator connection
/// ([`protect_runtime_descriptors`]) and reports ([`is_runtime_owned_fd`]). A
/// counter is placed first (`InGuestRcbCounter::move_descriptor_at_least`).
/// Fails with `OutOfMemory` when `RECORDED_FD_CAPACITY` (8) descriptors are
/// already recorded.
pub fn record_runtime_fd(fd: libc::c_int) -> io::Result<()> {
    if fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a counter descriptor must be open",
        ));
    }
    let entry = recorded_slot(current_pid(), fd);
    for slot in &RECORDED_FDS {
        if slot
            .compare_exchange(0, entry, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::OutOfMemory,
        "every recorded runtime descriptor slot is in use",
    ))
}

/// Closes the copies of its parent's recorded descriptors that a fork child
/// inherited, and forgets them. A counter measures the parent's thread, so
/// the child has no use for it; it binds a counter of its own. A slot is
/// inherited when the process that recorded it is not this one: threads of
/// one process share the record and their counters stay open.
///
/// A descriptor is forgotten only once the kernel says it is closed, with
/// signals blocked from the close to that check ([`block_all_signals`]). A
/// seccomp filter the guest installed can report success for `close` without
/// running it; then the copy stays recorded, and so protected, rather than
/// becoming a descriptor the guest can reach. When the kernel does not answer
/// whether it is open, this fails rather than guess.
pub fn close_inherited_runtime_fds() -> io::Result<()> {
    let _blocked = block_all_signals()?;
    let pid = current_pid();
    for slot in &RECORDED_FDS {
        let entry = slot.load(Ordering::Acquire);
        let Some(fd) = recorded_slot_fd(entry) else {
            continue;
        };
        if (entry >> 32) as u32 == pid as u32 {
            continue;
        }
        unsafe { raw_syscall6(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) };
        if !descriptor_is_open(fd)? {
            let _ = slot.compare_exchange(entry, 0, Ordering::AcqRel, Ordering::Acquire);
        }
    }
    Ok(())
}

/// Whether `fd` is one of the descriptors the runtime owns in this process:
/// the coordinator connection, the Tool output socket, a recorded descriptor
/// ([`record_runtime_fd`]: a performance counter, or an old number a placement
/// could not close), or a native-exec lease. This is the live set the
/// protections apply to, so
/// a number the runtime has released or never held is never reported.
pub fn is_runtime_owned_fd(fd: libc::c_int) -> bool {
    if fd < 0 {
        return false;
    }
    let (protected, count) = protected_descriptors(true);
    protected[..count].contains(&(fd as u64))
}

/// Moves `fd`, a socket the in-guest Tool owns for its own output (for example
/// one end of a socket pair its log records go to), to a reserved number in
/// the runtime's range ([`runtime_fd_minimums`]), closes `fd`, and returns the
/// reserved number.
/// From then on the runtime keeps the guest away from it exactly as it does
/// from the coordinator connection: a `close_range` over it spares it, and a
/// guest `close`, `read`, `write`, `shutdown`, `send`, `setsockopt` or other
/// descriptor operation on it fails with `EBADF`, as on a number the guest does
/// not have open. A guest `dup2`/`dup3` onto its number succeeds as it would if
/// the number were free: the runtime first moves the socket to another free
/// number in that range, so the Tool must read the current
/// number from [`tool_output_fd`] for each use. Tool code writes to it through
/// its own syscalls, which the runtime does not dispatch to the guest's
/// protections. A forked child inherits the descriptor and its protection.
///
/// A guest dup onto the number needs one more free descriptor in that range
/// to move the socket to. When there is none (the descriptor table is full),
/// the runtime gives the socket up so the guest's call keeps its native
/// outcome: it sends `retirement` on the socket, closes the socket, and
/// [`tool_output_fd`] then returns `None`. The Tool chooses a message its
/// reader treats as "output incomplete". The send waits for room, as a
/// blocking write of any Tool record does, and finishes a partial send, so a
/// reader that keeps reading always receives the whole message, even when a
/// forked child still holds the socket open and no end-of-file comes.
///
/// Only a socket is accepted (`InvalidInput` otherwise, leaving `fd` open), as
/// for the coordinator connection: the guest can reach a regular file, pipe or
/// FIFO through other names, such as `/proc/self/fd/<n>`, and truncate, map or
/// reopen it, none of which a descriptor protection can stop; a socket cannot
/// be reopened, truncated or mapped.
///
/// # Safety
///
/// Call at most once per process, before the runtime is installed (for
/// LiteInst, before `reverie_liteinst::install_tool`), while the process is
/// still single-threaded.
pub unsafe fn reserve_tool_output_fd(
    fd: libc::c_int,
    retirement: &'static [u8],
) -> io::Result<libc::c_int> {
    // Raw syscalls throughout: this runs inside the guest, whose program or
    // preloaded libraries may define libc's wrappers.
    let mut metadata: libc::stat = unsafe { core::mem::zeroed() };
    crate::guest::support::raw_zero_result(unsafe {
        raw_syscall6(
            libc::SYS_fstat,
            [fd as u64, (&raw mut metadata) as u64, 0, 0, 0, 0],
        )
    })?;
    if metadata.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a Tool output descriptor must be a socket",
        ));
    }
    let _blocked = block_all_signals()?;
    let reserved = crate::guest::support::raw_result(dup_into_runtime_range(fd))? as libc::c_int;
    if let Err(actual) =
        TOOL_OUTPUT_FD.compare_exchange(-1, reserved, Ordering::AcqRel, Ordering::Acquire)
    {
        unsafe { raw_syscall6(libc::SYS_close, [reserved as u64, 0, 0, 0, 0, 0]) };
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("a Tool output descriptor is already reserved ({actual})"),
        ));
    }
    let _ = TOOL_OUTPUT_RETIREMENT.set(retirement);
    unsafe { raw_syscall6(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) };
    record_if_open(fd)?;
    Ok(reserved)
}

/// The Tool output socket's current number, if one is reserved
/// ([`reserve_tool_output_fd`]). It can change when the guest `dup2`s onto it,
/// so the Tool reads it for each use.
pub fn tool_output_fd() -> Option<libc::c_int> {
    let fd = TOOL_OUTPUT_FD.load(Ordering::Acquire);
    (fd >= 0).then_some(fd)
}

/// Sends all of `message` on `socket`, waiting for room as a blocking write
/// does (also when the socket was made non-blocking) and resuming a partial
/// send. It stops early only when the socket cannot take the message at all,
/// such as when its reader has gone; `MSG_NOSIGNAL` keeps that from raising
/// SIGPIPE in the guest.
fn send_retirement(socket: libc::c_int, message: &[u8]) {
    let mut rest = message;
    while !rest.is_empty() {
        let sent = unsafe {
            raw_syscall6(
                libc::SYS_sendto,
                [
                    socket as u64,
                    rest.as_ptr() as u64,
                    rest.len() as u64,
                    libc::MSG_NOSIGNAL as u64,
                    0,
                    0,
                ],
            )
        };
        if sent > 0 {
            rest = &rest[sent as usize..];
        } else if sent == -i64::from(libc::EAGAIN) {
            let mut writable = libc::pollfd {
                fd: socket,
                events: libc::POLLOUT,
                revents: 0,
            };
            let polled = unsafe {
                raw_syscall6(
                    libc::SYS_poll,
                    [(&raw mut writable) as u64, 1, -1i64 as u64, 0, 0, 0],
                )
            };
            if polled < 0 && polled != -i64::from(libc::EINTR) {
                return;
            }
        } else if sent != -i64::from(libc::EINTR) {
            return;
        }
    }
}

/// The signals whose new action [`protect_runtime_control`] refused since the
/// Tool last took them ([`take_refused_signal_controls`]), bit `n - 1` for
/// signal `n`.
static REFUSED_SIGNAL_ACTIONS: AtomicU64 = AtomicU64::new(0);

/// Whether [`protect_runtime_control`] refused an alternate signal stack since
/// the Tool last took the refusals.
static REFUSED_ALTERNATE_STACK: AtomicBool = AtomicBool::new(false);

/// The guest signal controls [`protect_runtime_control`] refused with `EPERM`,
/// which the Tool never sees as calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefusedSignalControls {
    /// The signals whose new action was refused, bit `n - 1` for signal `n`.
    pub actions: u64,
    /// Whether an alternate signal stack was refused.
    pub alternate_stack: bool,
}

/// Takes the guest signal controls refused in this process since the last
/// call, so that the Tool can report them (each refusal is reported once:
/// taking clears them). A forked child starts with its parent's record.
pub fn take_refused_signal_controls() -> RefusedSignalControls {
    // The Tool asks at every syscall; almost always nothing was refused.
    if REFUSED_SIGNAL_ACTIONS.load(Ordering::Acquire) == 0
        && !REFUSED_ALTERNATE_STACK.load(Ordering::Acquire)
    {
        return RefusedSignalControls::default();
    }
    RefusedSignalControls {
        actions: REFUSED_SIGNAL_ACTIONS.swap(0, Ordering::AcqRel),
        alternate_stack: REFUSED_ALTERNATE_STACK.swap(false, Ordering::AcqRel),
    }
}

/// Records the refusal of the guest signal control `event` asked for.
fn record_refused_signal_control(event: &SyscallEvent) {
    if event.number == libc::SYS_sigaltstack {
        REFUSED_ALTERNATE_STACK.store(true, Ordering::Release);
        return;
    }
    // Linux reads the signal argument as a C int. A number that is not a
    // signal is never refused (`signal_action_supported`).
    let signal = event.args[0] as i32;
    if (1..=64).contains(&signal) {
        REFUSED_SIGNAL_ACTIONS.fetch_or(1 << (signal - 1), Ordering::AcqRel);
    }
}

// TODO-HUMAN-REVIEW(PR-127): Review process-global preload safety guards.
/// Refuses a guest syscall that would take control from the runtime: a
/// fork-like call when process forks are not allowed (`ENOTSUP`), and a
/// signal action [`signal_action_supported`] refuses or an alternate signal
/// stack (`EPERM`). Returns whether `event` was refused, with its result set.
/// A refused signal control is recorded for the Tool to report
/// ([`take_refused_signal_controls`]).
pub fn protect_runtime_control(event: &mut SyscallEvent) -> bool {
    let unsupported_process =
        is_fork_like(event.number) && !PROCESS_FORKS_ALLOWED.load(Ordering::Acquire);
    let protected_signal =
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-133): Review fail-closed guest signal-handler policy.
        (!signal_action_supported(event.number, event.args)
            && !crate::guest::sigalrm::decides_action(event.number, event.args))
        // AUTONOMOUS-BOT-IMPLEMENTED
        || (event.number == libc::SYS_sigaltstack && event.args[0] != 0);

    if unsupported_process {
        event.result = -i64::from(libc::ENOTSUP);
    } else if protected_signal {
        record_refused_signal_control(event);
        event.result = -i64::from(libc::EPERM);
    } else {
        return false;
    }
    true
}

/// The runtime-owned descriptors a syscall is kept from, sorted: the
/// coordinator connection and the recorded descriptors (counters, old
/// numbers), and for guest syscalls also the Tool output socket and the
/// native-exec leases.
/// Returns the array and how many of its leading entries are in use.
#[cfg(any(feature = "exec-native-injection", feature = "loader-private-runtime"))]
const PROTECTED_FD_CAPACITY: usize = 9 + RECORDED_FD_CAPACITY;
#[cfg(not(any(feature = "exec-native-injection", feature = "loader-private-runtime")))]
const PROTECTED_FD_CAPACITY: usize = 2 + RECORDED_FD_CAPACITY;

fn protected_descriptors(guest_syscall: bool) -> ([u64; PROTECTED_FD_CAPACITY], usize) {
    let mut protected = [u64::MAX; PROTECTED_FD_CAPACITY];
    let mut count = 0;
    let slots: &[&AtomicI32] = if guest_syscall {
        &[&COORDINATOR_FD, &TOOL_OUTPUT_FD]
    } else {
        &[&COORDINATOR_FD]
    };
    for slot in slots {
        let fd = slot.load(Ordering::Acquire);
        if fd >= 0 {
            protected[count] = fd as u64;
            count += 1;
        }
    }
    for slot in &RECORDED_FDS {
        if let Some(fd) = recorded_slot_fd(slot.load(Ordering::Acquire)) {
            protected[count] = fd as u64;
            count += 1;
        }
    }
    #[cfg(any(feature = "exec-native-injection", feature = "loader-private-runtime"))]
    if guest_syscall {
        super::native_exec_fds::append_protected(&mut protected, &mut count);
    }
    protected[..count].sort_unstable();
    (protected, count)
}

/// Keeps the guest away from the descriptors the runtime owns in Tool mode:
/// the coordinator connection, the performance counters and, for a guest
/// syscall, the Tool's reserved output descriptor. A syscall the Tool itself
/// makes (`guest_syscall` false) may use the output descriptor, which is the
/// Tool's.
///
/// Returns whether `event` was handled here, with its result set.
///
/// # Safety
///
/// `event` must be the syscall being dispatched, not yet run. A guest
/// `close_range` over a protected descriptor is run here, closing the rest
/// of its range, and a guest `dup2`/`dup3` onto the Tool output socket moves
/// that socket: both act on the process's descriptor table as the guest's
/// own call would.
/// The caller must hold no live owner or borrow (`OwnedFd`, `File`,
/// `BorrowedFd` and the like) of a descriptor the call closes or moves, other
/// than the runtime's own protected descriptors, which it spares or moves
/// through their slots; a descriptor the guest closes is the guest's to close.
pub unsafe fn protect_runtime_descriptors(event: &mut SyscallEvent, guest_syscall: bool) -> bool {
    unsafe { protect_runtime_descriptors_inner(event, guest_syscall, false, None) }
}

/// [`protect_runtime_descriptors`] for a guest syscall that a Tool dispatches
/// next: a `close` or `close_range` is left to the Tool, so the Tool accounts
/// for the guest descriptors it closes (Detcore's descriptor table, holders
/// and port release). The Tool forwards the call through `inject` or
/// `tail_inject`, which apply [`protect_forwarded_descriptor_change`] at
/// physical execution, so the guest-visible result is the same as when this
/// function handled the call itself. Every other protection is unchanged.
///
/// # Safety
///
/// As [`protect_runtime_descriptors`].
pub unsafe fn protect_runtime_descriptors_before_tool(event: &mut SyscallEvent) -> bool {
    unsafe { protect_runtime_descriptors_inner(event, true, true, None) }
}

/// A guest `close` or `close_range` that a Tool forwards after
/// [`protect_runtime_descriptors_before_tool`] left it to the Tool: the same
/// protection, applied at physical execution. A `close` of a protected
/// descriptor fails with `EBADF` without closing it; a `close_range` over one
/// closes the rest of its range. Returns the result to give the Tool instead of
/// running the call, or `None` to run the call as asked.
///
/// # Safety
///
/// As [`close_range_preserving_fds`]: `number` and `args` are a `close` or
/// `close_range` the Tool runs for the guest, in the guest's own descriptor
/// table, whether the guest's call forwarded unchanged or one the Tool issues
/// on the guest's behalf. Every descriptor it closes, other than the
/// runtime's own (which are spared), is the guest's to close, and the caller
/// holds no live owner or borrow of one.
pub unsafe fn protect_forwarded_descriptor_change(number: i64, args: [u64; 6]) -> Option<i64> {
    if !is_descriptor_change_left_to_tool(number) {
        return None;
    }
    let (mut protected, count) = protected_descriptors(true);
    let protected = &mut protected[..count];
    if protected.is_empty() {
        return None;
    }
    if number == libc::SYS_close && protected.contains(&reserved_fds::descriptor_arg(&args, 0)) {
        Some(-i64::from(libc::EBADF))
    } else if number == libc::SYS_close_range && reserved_fds::close_range_covers(&args, protected)
    {
        Some(unsafe { close_range_preserving_args(args, protected) })
    } else {
        None
    }
}

/// The descriptor-closing calls a Tool must see (see
/// [`protect_runtime_descriptors_before_tool`]).
fn is_descriptor_change_left_to_tool(number: i64) -> bool {
    matches!(number, libc::SYS_close | libc::SYS_close_range)
}

unsafe fn protect_runtime_descriptors_inner(
    event: &mut SyscallEvent,
    guest_syscall: bool,
    leave_descriptor_changes_to_tool: bool,
    extra: Option<u64>,
) -> bool {
    if leave_descriptor_changes_to_tool && is_descriptor_change_left_to_tool(event.number) {
        return false;
    }
    let (slots, mut count) = protected_descriptors(guest_syscall);
    let mut protected = [u64::MAX; PROTECTED_FD_CAPACITY + 1];
    protected[..count].copy_from_slice(&slots[..count]);
    if let Some(fd) = extra
        && !protected[..count].contains(&fd)
    {
        protected[count] = fd;
        count += 1;
    }
    protected[..count].sort_unstable();
    let protected = &mut protected[..count];
    if protected.is_empty() {
        return false;
    }
    if guest_syscall {
        match unsafe { relocate_tool_output_for_dup(event, protected) } {
            DupOntoToolOutput::NotThis => {}
            DupOntoToolOutput::Proceed => return false,
            DupOntoToolOutput::Refuse(result) => {
                event.result = result;
                return true;
            }
        }
    }
    if event.number == libc::SYS_close && protected.contains(&fd_arg(event, 0)) {
        event.result = -i64::from(libc::EBADF);
    } else if event.number == libc::SYS_close_range
        && reserved_fds::close_range_covers(&event.args, protected)
    {
        event.result = unsafe { close_range_preserving_fds(event, protected) };
    } else if let Some(result) = protected
        .iter()
        .find_map(|&fd| reserved_fds::epoll_wait_result(event.number, &event.args, fd))
    {
        event.result = result;
    } else if protected.iter().any(|&fd| {
        syscall_targets_event_fd(event, fd)
            || (guest_syscall
                && reserved_fds::socket_call_names_descriptor(event.number, &event.args, fd))
            || (extra == Some(fd)
                && reserved_fds::socket_call_names_descriptor(event.number, &event.args, fd))
    }) {
        event.result = -i64::from(libc::EBADF);
    } else {
        return false;
    }
    true
}

/// Same runtime descriptor protections plus the mapped owner's actual sealed
/// input lease. The ordinary owned startup socket is not an extra reserved FD.
/// # Safety
/// Same ownership contract as protect_runtime_descriptors; `extra` is borrowed
/// from the sole retained NativeControl, not a new descriptor close authority.
#[cfg(all(feature = "loader-private-runtime", target_env = "musl"))]
pub(crate) unsafe fn protect_tool_init_descriptors(
    event: &mut SyscallEvent,
    extra: Option<u64>,
) -> bool {
    unsafe { protect_runtime_descriptors_inner(event, false, false, extra) }
}

/// What to do with a guest syscall that may be a `dup2`/`dup3` onto the Tool
/// output socket's number.
enum DupOntoToolOutput {
    /// Not such a call: protect as usual.
    NotThis,
    /// The socket's number is free now; run the guest's call.
    Proceed,
    /// The call fails as Linux would fail it, before anything moved.
    Refuse(i64),
}

/// A guest `dup2`/`dup3` onto the Tool output socket's number is an ordinary
/// operation the guest is entitled to: that number is free as far as the guest
/// knows. First applies the kernel's own checks, in its order, without moving
/// anything: `dup3` flags other than `O_CLOEXEC` (`EINVAL`), a target at or
/// above `RLIMIT_NOFILE`, and a source that is not open (`EBADF`). Then moves
/// the socket to another free number in the runtime's range
/// ([`runtime_fd_minimums`]) and closes the old one, so the guest's call runs
/// on a free number with its
/// native outcome. When no descriptor is free to move it to, gives the socket
/// up instead (see [`reserve_tool_output_fd`]). No unprotected copy of the
/// socket is left behind on any path.
unsafe fn relocate_tool_output_for_dup(
    event: &SyscallEvent,
    protected: &[u64],
) -> DupOntoToolOutput {
    if !matches!(event.number, libc::SYS_dup2 | libc::SYS_dup3) {
        return DupOntoToolOutput::NotThis;
    }
    let tool = TOOL_OUTPUT_FD.load(Ordering::Acquire);
    let source = fd_arg(event, 0);
    if tool < 0 || fd_arg(event, 1) != tool as u64 || protected.contains(&source) {
        return DupOntoToolOutput::NotThis;
    }
    if event.number == libc::SYS_dup3 && fd_arg(event, 2) & !(libc::O_CLOEXEC as u64) != 0 {
        return DupOntoToolOutput::Refuse(-i64::from(libc::EINVAL));
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let queried = unsafe {
        raw_syscall6(
            libc::SYS_prlimit64,
            [
                0,
                libc::RLIMIT_NOFILE as u64,
                0,
                (&raw mut limit) as u64,
                0,
                0,
            ],
        )
    };
    if queried == 0 && tool as u64 >= limit.rlim_cur {
        return DupOntoToolOutput::Refuse(-i64::from(libc::EBADF));
    }
    if unsafe { raw_syscall6(libc::SYS_fcntl, [source, libc::F_GETFD as u64, 0, 0, 0, 0]) } < 0 {
        return DupOntoToolOutput::Refuse(-i64::from(libc::EBADF));
    }
    let moved = dup_into_runtime_range(tool);
    let replacement = if moved >= 0 { moved as i32 } else { -1 };
    if TOOL_OUTPUT_FD
        .compare_exchange(tool, replacement, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        if moved >= 0 {
            unsafe { raw_syscall6(libc::SYS_close, [moved as u64, 0, 0, 0, 0, 0]) };
        }
        return DupOntoToolOutput::NotThis;
    }
    if moved < 0 {
        // No descriptor to move the socket to: tell its reader the output is
        // incomplete, then give it up.
        if let Some(message) = TOOL_OUTPUT_RETIREMENT.get() {
            send_retirement(tool, message);
        }
    }
    // The socket lives on as `moved`, or not at all; the guest's call gets the
    // old number.
    unsafe { raw_syscall6(libc::SYS_close, [tool as u64, 0, 0, 0, 0, 0]) };
    DupOntoToolOutput::Proceed
}

/// Runs a guest `close_range` without closing any of `preserved`, which is sorted.
///
/// # Safety
///
/// `event` must be a `close_range` the guest asked for: the descriptors in its
/// range other than `preserved` are closed.
/// The caller must hold no live owner or borrow (`OwnedFd`, `File`,
/// `BorrowedFd` and the like) of a descriptor the call closes, other than
/// those in `preserved`; a descriptor the guest closes is the guest's to
/// close.
pub unsafe fn close_range_preserving_fds(event: &SyscallEvent, preserved: &[u64]) -> i64 {
    unsafe { close_range_preserving_args(event.args, preserved) }
}

/// [`close_range_preserving_fds`] on the call's raw argument registers.
///
/// # Safety
///
/// As [`close_range_preserving_fds`].
unsafe fn close_range_preserving_args(args: [u64; 6], preserved: &[u64]) -> i64 {
    unsafe {
        reserved_fds::close_range_sparing(&args, preserved, |number, args| {
            raw_syscall6(number, args)
        })
    }
}

/// Syscall argument `index` as the kernel reads a descriptor, `close_range`
/// bound or flags argument: the low 32 bits of the register. Comparing the full
/// register would let `fd | 1 << 32` reach the descriptor past a protection.
pub fn fd_arg(event: &SyscallEvent, index: usize) -> u64 {
    reserved_fds::descriptor_arg(&event.args, index)
}

/// Whether `event` operates on descriptor `event_fd` through one of its
/// descriptor arguments, by the shared rule
/// [`reserved_fds::names_descriptor`]. It does not cover the socket calls,
/// close, close_range or epoll readiness; [`protect_runtime_descriptors`]
/// checks all of those for the runtime's own descriptors.
pub fn syscall_targets_event_fd(event: &SyscallEvent, event_fd: u64) -> bool {
    reserved_fds::names_descriptor(event.number, &event.args, event_fd)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::guest::event::SyscallDispatch;

    fn event(number: i64, args: [u64; 6]) -> SyscallEvent {
        SyscallEvent {
            number,
            args,
            instruction_pointer: 0x1234,
            result: 0,
            context: 0,
            dispatch: SyscallDispatch::Trap,
            guest_pkru: None,
        }
    }

    pub(crate) fn open_eventfd() -> libc::c_int {
        let fd = unsafe {
            raw_syscall6(
                libc::SYS_eventfd2,
                [0, libc::EFD_CLOEXEC as u64, 0, 0, 0, 0],
            )
        };
        assert!(fd >= 0, "eventfd2 failed: {fd}");
        fd as libc::c_int
    }

    pub(crate) fn is_open(fd: libc::c_int) -> bool {
        unsafe {
            raw_syscall6(
                libc::SYS_fcntl,
                [fd as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
            ) >= 0
        }
    }

    /// Runs `body` in a forked child and requires it to finish without
    /// panicking. The registry and the descriptor table are process-wide, so a
    /// test that records a descriptor or changes the limit does it in a child,
    /// where it cannot reach a test running beside it. The child's SIGALRM ends
    /// it after 60 seconds, so a child stuck on a lock another test thread held
    /// at the fork fails the test instead of hanging it.
    pub(crate) fn in_child(body: impl FnOnce()) {
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            unsafe { libc::alarm(60) };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
            unsafe { libc::_exit(if result.is_ok() { 0 } else { 125 }) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child status {status:#x}"
        );
    }

    /// [`in_child`] with the child's soft `RLIMIT_NOFILE` set to `soft`.
    pub(crate) fn in_child_with_soft_nofile(soft: libc::rlim_t, body: impl FnOnce()) {
        in_child(|| {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
                0
            );
            assert!(limit.rlim_max >= soft, "hard limit {}", limit.rlim_max);
            limit.rlim_cur = soft;
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
            body();
        });
    }

    fn dup_at_least(fd: libc::c_int, minimum: libc::c_int) -> i64 {
        unsafe {
            raw_syscall6(
                libc::SYS_fcntl,
                [fd as u64, libc::F_DUPFD as u64, minimum as u64, 0, 0, 0],
            )
        }
    }

    /// Takes every free number at or above `minimum` (the child's inherited
    /// descriptors there count as taken too) until the table is full.
    fn fill_from(fd: libc::c_int, minimum: libc::c_int) {
        loop {
            let taken = dup_at_least(fd, minimum);
            if taken == -i64::from(libc::EMFILE) {
                return;
            }
            assert!(taken >= 0, "F_DUPFD failed: {taken}");
        }
    }

    /// A descriptor the runtime records sits at or above [`RUNTIME_FD_MIN`]
    /// where the limit leaves room, is reported as the runtime's, and is kept
    /// from the guest as the coordinator connection is: `close` and a `dup2`
    /// onto it fail with EBADF, and a `close_range` over it closes the rest of
    /// the range only. A fork child closes its copy of its parent's
    /// ([`close_inherited_runtime_fds`]); the parent's stays open and recorded.
    #[test]
    fn recorded_descriptors_are_protected_and_a_fork_child_closes_its_copy() {
        in_child_with_soft_nofile(2048, || {
            let counter = place_runtime_fd(open_eventfd()).unwrap();
            assert!(counter >= RUNTIME_FD_MIN, "placed at {counter}");
            record_runtime_fd(counter).unwrap();
            assert!(is_runtime_owned_fd(counter));
            assert!(!is_runtime_owned_fd(0));
            let neighbour = unsafe {
                raw_syscall6(
                    libc::SYS_fcntl,
                    [
                        counter as u64,
                        libc::F_DUPFD as u64,
                        counter as u64 + 1,
                        0,
                        0,
                        0,
                    ],
                )
            } as libc::c_int;
            assert!(neighbour > counter);

            let mut close = event(libc::SYS_close, [counter as u64, 0, 0, 0, 0, 0]);
            assert!(unsafe { protect_runtime_descriptors(&mut close, true) });
            assert_eq!(close.result, -i64::from(libc::EBADF));
            let mut dup = event(libc::SYS_dup2, [0, counter as u64, 0, 0, 0, 0]);
            assert!(unsafe { protect_runtime_descriptors(&mut dup, true) });
            assert_eq!(dup.result, -i64::from(libc::EBADF));
            let mut range = event(
                libc::SYS_close_range,
                [counter as u64, neighbour as u64, 0, 0, 0, 0],
            );
            assert!(unsafe { protect_runtime_descriptors(&mut range, true) });
            assert_eq!(range.result, 0);
            assert!(is_open(counter));
            assert!(!is_open(neighbour), "the unprotected neighbour is closed");

            let child = unsafe { libc::fork() };
            assert!(child >= 0);
            if child == 0 {
                close_inherited_runtime_fds().unwrap();
                let status = if is_open(counter) {
                    1
                } else if is_runtime_owned_fd(counter) {
                    2
                } else {
                    0
                };
                unsafe { libc::_exit(status) };
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert!(
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                "child status {status:#x}"
            );
            // The parent's own counter is not inherited: it stays open and recorded.
            close_inherited_runtime_fds().unwrap();
            assert!(is_open(counter));
            assert!(is_runtime_owned_fd(counter));
        });
    }

    /// Under the common default soft `RLIMIT_NOFILE` of 1024, which leaves no
    /// number at or above [`RUNTIME_FD_MIN`], a runtime descriptor goes to the
    /// top [`RUNTIME_FD_RESERVE`] numbers below the limit instead, still clear
    /// of the guest's first numbers.
    #[test]
    fn a_descriptor_goes_just_below_a_limit_that_excludes_the_runtime_range() {
        in_child_with_soft_nofile(1024, || {
            assert_eq!(
                runtime_fd_minimums().collect::<Vec<_>>(),
                [RUNTIME_FD_MIN, 1024 - RUNTIME_FD_RESERVE]
            );
            let fd = open_eventfd();
            let placed = place_runtime_fd(fd).unwrap();
            assert!(
                (1024 - RUNTIME_FD_RESERVE..1024).contains(&placed),
                "placed at {placed}"
            );
            assert!(!is_open(fd) && is_open(placed));
        });
    }

    /// A soft limit just above [`RUNTIME_FD_MIN`] still places at or above it
    /// while a number there is free, and falls back to the band below the
    /// limit only once none is.
    #[test]
    fn the_fallback_band_takes_a_descriptor_only_when_none_is_free_above_1025() {
        in_child_with_soft_nofile(1030, || {
            let first = place_runtime_fd(open_eventfd()).unwrap();
            assert!((RUNTIME_FD_MIN..1030).contains(&first), "placed at {first}");
            let spare = open_eventfd();
            fill_from(spare, RUNTIME_FD_MIN);
            let fallback = place_runtime_fd(open_eventfd()).unwrap();
            assert!(
                (1030 - RUNTIME_FD_RESERVE..RUNTIME_FD_MIN).contains(&fallback),
                "placed at {fallback}"
            );
        });
    }

    /// With every number in the runtime's range taken, a descriptor keeps the
    /// number it has (the `EMFILE` fallback) and stays open.
    #[test]
    fn a_descriptor_keeps_its_number_when_the_runtime_range_is_full() {
        in_child_with_soft_nofile(1024, || {
            let fd = open_eventfd();
            fill_from(fd, 1024 - RUNTIME_FD_RESERVE);
            assert_eq!(place_runtime_fd(fd).unwrap(), fd);
            assert!(is_open(fd));
        });
    }

    /// A soft limit so low that the band would reach below 3 starts it at 3.
    #[test]
    fn the_fallback_band_never_reaches_the_standard_descriptors() {
        in_child_with_soft_nofile(8, || {
            assert_eq!(
                runtime_fd_minimums().collect::<Vec<_>>(),
                [RUNTIME_FD_MIN, 3]
            );
        });
    }

    /// Installs a seccomp filter that makes `close(fd)` report success without
    /// closing anything, as a filter the guest installed could.
    pub(crate) fn suppress_close_of(fd: libc::c_int) {
        install_filter(libc::SYS_close, 0, fd as u32, libc::SECCOMP_RET_ERRNO);
    }

    /// Installs a seccomp filter under which a syscall `number` whose argument
    /// `argument` has `value` in its low half returns `action` instead of
    /// running.
    fn install_filter(number: i64, argument: u32, value: u32, action: u32) {
        let mut instructions = [
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 0,
            },
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 3,
                k: number as u32,
            },
            // The low half of the argument (seccomp_data.args starts at 16).
            libc::sock_filter {
                code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                jt: 0,
                jf: 0,
                k: 16 + 8 * argument,
            },
            libc::sock_filter {
                code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                jt: 0,
                jf: 1,
                k: value,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: action,
            },
            libc::sock_filter {
                code: (libc::BPF_RET | libc::BPF_K) as u16,
                jt: 0,
                jf: 0,
                k: libc::SECCOMP_RET_ALLOW,
            },
        ];
        let program = libc::sock_fprog {
            len: instructions.len() as u16,
            filter: instructions.as_mut_ptr(),
        };
        let no_new_privileges = unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [libc::PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0],
            )
        };
        assert_eq!(no_new_privileges, 0);
        let installed = unsafe {
            raw_syscall6(
                libc::SYS_seccomp,
                [
                    libc::SECCOMP_SET_MODE_FILTER as u64,
                    0,
                    (&raw const program) as u64,
                    0,
                    0,
                    0,
                ],
            )
        };
        assert_eq!(installed, 0);
    }

    /// When a placement's close of the old number reports success without
    /// closing it, the runtime records that number too, so neither name for the
    /// descriptor is left reachable or unreported.
    #[test]
    fn an_old_number_that_survives_its_close_is_recorded_too() {
        in_child_with_soft_nofile(2048, || {
            let fd = open_eventfd();
            suppress_close_of(fd);
            let placed = place_runtime_fd(fd).unwrap();
            assert!(placed >= RUNTIME_FD_MIN, "placed at {placed}");
            assert!(is_open(fd), "the filter kept the old number open");
            assert!(
                is_runtime_owned_fd(fd),
                "the surviving old number is recorded"
            );
            // The caller records the placed number as its own, as
            // connect_ordinary does through reserve_coordinator_fd.
            record_runtime_fd(placed).unwrap();
            assert!(is_runtime_owned_fd(placed));
            let mut close = event(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]);
            assert!(unsafe { protect_runtime_descriptors(&mut close, true) });
            assert_eq!(close.result, -i64::from(libc::EBADF));
        });
    }

    /// When the kernel will not say whether the old number is still open (a
    /// filter refuses `F_GETFD`), a placement fails rather than guess: a guess
    /// of "open" would protect a number the guest may reuse, a guess of
    /// "closed" could leave a second name unprotected.
    #[test]
    fn a_placement_fails_when_the_kernel_will_not_say_whether_the_old_number_is_open() {
        in_child_with_soft_nofile(2048, || {
            let fd = open_eventfd();
            install_filter(
                libc::SYS_fcntl,
                1,
                libc::F_GETFD as u32,
                libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
            );
            let refused = place_runtime_fd(fd).unwrap_err();
            assert_eq!(refused.raw_os_error(), Some(libc::EPERM));
            assert!(!is_runtime_owned_fd(fd));
        });
    }

    /// The order Codex's round-4 review described: the runtime really closes
    /// a number, a signal arrives before it checks that number, and the
    /// guest's handler opens a descriptor, which takes the released number.
    /// A supervisor process holds the check itself (a seccomp user
    /// notification on `fcntl(number, F_GETFD)`), reads the child's blocked
    /// set there, and sends SIGFPE, one of the numbers the executing code can
    /// also raise synchronously. Every signal must be blocked at that point,
    /// so the handler runs only after the check, and its descriptor is not
    /// recorded as the runtime's.
    #[test]
    fn a_handler_cannot_run_between_the_close_and_the_check() {
        use std::sync::atomic::AtomicI32;
        static HANDLER_FD: AtomicI32 = AtomicI32::new(-1);
        extern "C" fn open_from_handler(_: libc::c_int) {
            let fd = unsafe {
                raw_syscall6(
                    libc::SYS_eventfd2,
                    [0, libc::EFD_CLOEXEC as u64, 0, 0, 0, 0],
                )
            };
            HANDLER_FD.store(fd as i32, Ordering::SeqCst);
        }
        in_child_with_soft_nofile(2048, || {
            let mut pipe = [-1; 2];
            assert_eq!(
                unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
                0
            );
            let guest = unsafe { libc::fork() };
            assert!(guest >= 0);
            if guest == 0 {
                let status = (|| {
                    let handler = open_from_handler as extern "C" fn(libc::c_int);
                    if unsafe { libc::signal(libc::SIGFPE, handler as libc::sighandler_t) }
                        == libc::SIG_ERR
                    {
                        return 10;
                    }
                    let number = open_eventfd();
                    let listener = notify_on_getfd(number);
                    if listener < 0 {
                        return 11;
                    }
                    let message = [number, listener];
                    let written = unsafe { libc::write(pipe[1], message.as_ptr().cast(), 8) };
                    if written != 8 {
                        return 12;
                    }
                    let Ok(placed) = place_runtime_fd(number) else {
                        return 13;
                    };
                    if placed < RUNTIME_FD_MIN {
                        return 14;
                    }
                    let opened = HANDLER_FD.load(Ordering::SeqCst);
                    if opened != number {
                        // The handler did not run, or did not reuse the
                        // number: the order under test did not happen.
                        return 15;
                    }
                    if is_runtime_owned_fd(opened) {
                        return 16;
                    }
                    0
                })();
                unsafe { libc::_exit(status) };
            }
            let mut message = [-1_i32; 2];
            assert_eq!(
                unsafe { libc::read(pipe[0], message.as_mut_ptr().cast(), 8) },
                8
            );
            let [number, remote_listener] = message;
            let pidfd =
                unsafe { raw_syscall6(libc::SYS_pidfd_open, [guest as u64, 0, 0, 0, 0, 0]) };
            assert!(pidfd >= 0, "pidfd_open: {pidfd}");
            let listener = unsafe {
                raw_syscall6(
                    libc::SYS_pidfd_getfd,
                    [pidfd as u64, remote_listener as u64, 0, 0, 0, 0],
                )
            };
            assert!(listener >= 0, "pidfd_getfd: {listener}");
            let mut blocked_at_check = None;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            let status = loop {
                let mut status = 0;
                let reaped = unsafe { libc::waitpid(guest, &mut status, libc::WNOHANG) };
                if reaped == guest {
                    break status;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the guest never finished"
                );
                let mut ready = libc::pollfd {
                    fd: listener as i32,
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut ready, 1, 50) } <= 0 {
                    continue;
                }
                let mut notification: libc::seccomp_notif = unsafe { core::mem::zeroed() };
                if unsafe {
                    libc::ioctl(
                        listener as i32,
                        libc::SECCOMP_IOCTL_NOTIF_RECV,
                        &mut notification,
                    )
                } != 0
                {
                    continue;
                }
                assert_eq!(notification.data.nr, libc::SYS_fcntl as i32);
                assert_eq!(notification.data.args[0], number as u64);
                if blocked_at_check.is_none() {
                    let status =
                        std::fs::read_to_string(format!("/proc/{}/status", notification.pid))
                            .unwrap();
                    blocked_at_check = status
                        .lines()
                        .find_map(|line| line.strip_prefix("SigBlk:"))
                        .map(|mask| mask.trim().to_owned());
                    assert_eq!(unsafe { libc::kill(guest, libc::SIGFPE) }, 0);
                }
                let response = libc::seccomp_notif_resp {
                    id: notification.id,
                    val: 0,
                    error: 0,
                    flags: libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE as u32,
                };
                unsafe { libc::ioctl(listener as i32, libc::SECCOMP_IOCTL_NOTIF_SEND, &response) };
            };
            // Every signal but SIGKILL and SIGSTOP, which the kernel never blocks.
            assert_eq!(blocked_at_check.as_deref(), Some("fffffffffffbfeff"));
            assert!(
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                "guest status {status:#x}"
            );
        });
    }

    /// Installs a seccomp filter that holds `fcntl(fd, F_GETFD)` for a
    /// supervisor (`SECCOMP_RET_USER_NOTIF`) and returns the listener, or a
    /// negative errno.
    fn notify_on_getfd(fd: libc::c_int) -> i32 {
        let load = |offset: u32| libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: offset,
        };
        let skip_to_allow_unless = |value: u32, skip: u8| libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: skip,
            k: value,
        };
        let ret = |action: u32| libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: action,
        };
        let mut instructions = [
            load(0),
            skip_to_allow_unless(libc::SYS_fcntl as u32, 5),
            load(16),
            skip_to_allow_unless(fd as u32, 3),
            load(24),
            skip_to_allow_unless(libc::F_GETFD as u32, 1),
            ret(libc::SECCOMP_RET_USER_NOTIF),
            ret(libc::SECCOMP_RET_ALLOW),
        ];
        let program = libc::sock_fprog {
            len: instructions.len() as u16,
            filter: instructions.as_mut_ptr(),
        };
        let no_new_privileges = unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [libc::PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0],
            )
        };
        if no_new_privileges != 0 {
            return no_new_privileges as i32;
        }
        unsafe {
            raw_syscall6(
                libc::SYS_seccomp,
                [
                    libc::SECCOMP_SET_MODE_FILTER as u64,
                    libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
                    (&raw const program) as u64,
                    0,
                    0,
                    0,
                ],
            ) as i32
        }
    }

    /// The caller's record runs while every signal is still blocked, so a
    /// handler of a signal held during the placement cannot run before it.
    #[test]
    fn a_placements_record_runs_before_signals_are_unblocked() {
        in_child(|| {
            let mut blocked_in_record = 0_u64;
            let placed = place_runtime_fd_and(open_eventfd(), |_| {
                let read = unsafe {
                    raw_syscall6(
                        libc::SYS_rt_sigprocmask,
                        [
                            libc::SIG_BLOCK as u64,
                            0,
                            (&raw mut blocked_in_record) as u64,
                            8,
                            0,
                            0,
                        ],
                    )
                };
                assert_eq!(read, 0);
                Ok(())
            })
            .unwrap();
            assert!(placed >= runtime_fd_minimums().last().unwrap());
            // Every signal but SIGKILL and SIGSTOP, which the kernel never blocks.
            assert_eq!(blocked_in_record, 0xffff_ffff_fffb_feff);
        });
    }

    /// A placement blocks every signal only while it runs and leaves the
    /// caller's signal mask as it found it.
    #[test]
    fn a_placement_restores_the_signal_mask() {
        in_child(|| {
            let read_mask = || {
                let mut mask: u64 = 0;
                let read = unsafe {
                    raw_syscall6(
                        libc::SYS_rt_sigprocmask,
                        [libc::SIG_BLOCK as u64, 0, (&raw mut mask) as u64, 8, 0, 0],
                    )
                };
                assert_eq!(read, 0);
                mask
            };
            let usr1: u64 = 1 << (libc::SIGUSR1 - 1);
            let set = unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const usr1) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            };
            assert_eq!(set, 0);
            place_runtime_fd(open_eventfd()).unwrap();
            assert_eq!(read_mask(), usr1);
        });
    }

    /// A refused handler or alternate stack is recorded for the Tool, once:
    /// taking the record clears it. A query, a default action and an
    /// alternate-stack query are not refused and not recorded.
    #[test]
    fn refused_signal_controls_are_recorded_for_the_tool_once() {
        // A struct sigaction whose handler is an address, read from this
        // process's own memory as the runtime reads the guest's.
        let handler: [u64; 4] = [0x4000, 0, 0, 0];
        let default: [u64; 4] = [libc::SIG_DFL as u64, 0, 0, 0];
        let stack = [0u64; 3];
        let _ = take_refused_signal_controls();

        let mut install = event(
            libc::SYS_rt_sigaction,
            [libc::SIGUSR1 as u64, handler.as_ptr() as u64, 0, 8, 0, 0],
        );
        assert!(protect_runtime_control(&mut install));
        assert_eq!(install.result, -i64::from(libc::EPERM));
        let mut alternate = event(
            libc::SYS_sigaltstack,
            [stack.as_ptr() as u64, 0, 0, 0, 0, 0],
        );
        assert!(protect_runtime_control(&mut alternate));
        for allowed in [
            event(
                libc::SYS_rt_sigaction,
                [libc::SIGUSR2 as u64, 0, 0, 8, 0, 0],
            ),
            // Not signals: Linux refuses them (EINVAL), so the runtime does
            // not, and there is nothing to record.
            event(
                libc::SYS_rt_sigaction,
                [0, handler.as_ptr() as u64, 0, 8, 0, 0],
            ),
            event(
                libc::SYS_rt_sigaction,
                [65, handler.as_ptr() as u64, 0, 8, 0, 0],
            ),
            event(
                libc::SYS_rt_sigaction,
                [libc::SIGUSR2 as u64, default.as_ptr() as u64, 0, 8, 0, 0],
            ),
            event(
                libc::SYS_sigaltstack,
                [0, stack.as_ptr() as u64, 0, 0, 0, 0],
            ),
        ] {
            let mut allowed = allowed;
            assert!(!protect_runtime_control(&mut allowed), "{:?}", allowed.args);
        }
        assert_eq!(
            take_refused_signal_controls(),
            RefusedSignalControls {
                actions: 1 << (libc::SIGUSR1 - 1),
                alternate_stack: true,
            }
        );
        assert_eq!(
            take_refused_signal_controls(),
            RefusedSignalControls::default(),
            "taken once"
        );
    }

    /// The Tool's own signal controls are refused on their own path, before
    /// `protect_runtime_control`, and are not the guest's: nothing is
    /// recorded for them.
    #[test]
    fn the_tools_own_signal_controls_are_not_recorded() {
        let handler: [u64; 4] = [0x4000, 0, 0, 0];
        let stack = [0u64; 3];
        let _ = take_refused_signal_controls();
        for (number, args) in [
            (
                libc::SYS_rt_sigaction,
                [libc::SIGUSR1 as u64, handler.as_ptr() as u64, 0, 8, 0, 0],
            ),
            (
                libc::SYS_sigaltstack,
                [stack.as_ptr() as u64, 0, 0, 0, 0, 0],
            ),
        ] {
            let mut nested = event(number, args);
            // SAFETY: both calls are refused before anything is forwarded.
            unsafe {
                crate::guest::trap_dispatch::forward_nested_tool_syscall(&mut nested, |_| {})
            };
            assert_eq!(nested.result, -i64::from(libc::EPERM), "{number}");
        }
        assert_eq!(
            take_refused_signal_controls(),
            RefusedSignalControls::default()
        );
    }
}

#[cfg(all(test, feature = "loader-private-runtime", target_env = "musl"))]
mod mapped_input_lease_tests {
    use super::*;
    use crate::guest::event::SyscallDispatch;
    #[test]
    fn actual_input_lease_is_guarded_with_kernel_fd_width_before_publication() {
        let fd = 0x7fff_fffdu64;
        for (number, args) in [
            (libc::SYS_close, [fd | (1u64 << 32), 0, 0, 0, 0, 0]),
            (libc::SYS_dup2, [12, fd | (1u64 << 32), 0, 0, 0, 0]),
            (libc::SYS_fcntl, [fd, libc::F_SETFD as u64, 0, 0, 0, 0]),
            (libc::SYS_sendmsg, [fd, 0, 0, 0, 0, 0]),
        ] {
            let mut event = SyscallEvent {
                number,
                args,
                instruction_pointer: 0,
                result: 0,
                context: 0,
                dispatch: SyscallDispatch::Trap,
                guest_pkru: None,
            };
            // Every case is numerically refused before any physical operation.
            assert!(unsafe { protect_tool_init_descriptors(&mut event, Some(fd)) });
            assert_eq!(event.result, -i64::from(libc::EBADF));
        }
        // An ordinary newly owned stream is not this extra input lease; neither
        // source nor target field is broadened into an all-descriptor guard.
        let stream = fd - 1;
        assert!(!reserved_fds::socket_call_names_descriptor(
            libc::SYS_connect,
            &[stream, 0, 0, 0, 0, 0],
            fd
        ));
        assert!(!reserved_fds::names_descriptor(
            libc::SYS_close,
            &[stream, 0, 0, 0, 0, 0],
            fd
        ));
        assert!(reserved_fds::close_range_covers(
            &[fd - 1, fd + 1, 0, 0, 0, 0],
            &[fd]
        ));
    }
}
