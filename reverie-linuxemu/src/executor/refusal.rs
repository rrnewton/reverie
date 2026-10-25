/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Typed refusals for unsupported operations (Reverie's `UnsupportedRefusal`); gVisor reports these as unimplemented syscalls.

use super::*;

pub(super) const MAX_HOST_IO: usize = 16 * 1024 * 1024;
pub(super) const MAX_CAPTURED_OUTPUT: usize = 64 * 1024 * 1024;
pub(super) const PAGE_SIZE: u64 = 4096;
pub(super) const MAX_RW_COUNT: usize = (i32::MAX as usize) & !(PAGE_SIZE as usize - 1);
pub use reverie_kvm_core::layout::GUEST_USER_ADDRESS_LIMIT;
pub use reverie_kvm_core::layout::X86_64_GUEST_USER_LIMIT;
/// Diagnostic carried by this backend's typed refusal of an exec from a thread
/// that is not its group's leader.
pub(crate) const NONLEADER_EXEC_REFUSAL: &str = "KVM nonleader exec is unsupported";

/// The typed refusal, if any, that an exec result from thread `tid` of
/// process `pid` carries.
///
/// Linux's execve never fails with ENOSYS, so every ENOSYS this backend
/// returns to a non-leader exec is its own refusal. That covers the explicit
/// refusal after image preflight and an ENOSYS from a host syscall the
/// preflight makes, such as the host `faccessat2` permission check on a
/// kernel or sandbox that lacks it. Every other result, and every result for
/// a group leader, carries none.
pub(super) fn nonleader_exec_refusal(
    result: i64,
    tid: i32,
    pid: i32,
) -> Option<reverie::UnsupportedRefusal> {
    (result == negative_errno(libc::ENOSYS) && tid != pid).then(|| {
        reverie::UnsupportedRefusal::new(
            reverie::UnsupportedOperation::NonLeaderExec,
            reverie::Errno::ENOSYS,
            NONLEADER_EXEC_REFUSAL,
        )
    })
}

/// Diagnostic carried by this backend's typed refusal of `flock`.
pub(crate) const FLOCK_REFUSAL: &str =
    "KVM flock is unsupported: no host lock can follow the guest description's lifetime";

/// Whether `operation` is a valid `flock(2)` lock operation, which the kernel
/// would apply to the descriptor's open file description: exactly `LOCK_SH`,
/// `LOCK_EX` or `LOCK_UN`, optionally with `LOCK_NB`. Anything else is a
/// malformed operation (EINVAL) or the ignored legacy `LOCK_MAND` bit (0),
/// which the kernel answers before it looks at a descriptor.
pub(super) fn flock_takes_effect(operation: u32) -> bool {
    matches!(
        (operation & !(libc::LOCK_NB as u32)) as libc::c_int,
        libc::LOCK_SH | libc::LOCK_EX | libc::LOCK_UN
    )
}

/// The typed refusal, if any, that a `flock` result carries. Linux's flock
/// never fails with ENOSYS, so every ENOSYS answer to a valid lock operation is
/// this backend's own refusal; every other result carries none.
pub(super) fn flock_refusal(
    request: &SyscallRequest,
    result: i64,
) -> Option<reverie::UnsupportedRefusal> {
    (request.number() == libc::SYS_flock as u64
        && flock_takes_effect(request.args()[1] as u32)
        && result == negative_errno(libc::ENOSYS))
    .then(|| {
        reverie::UnsupportedRefusal::new(
            reverie::UnsupportedOperation::Flock,
            reverie::Errno::ENOSYS,
            FLOCK_REFUSAL,
        )
    })
}

/// The typed refusal for a signal syscall the backend refused (`reason`, set
/// by `refuse_signal`) when it answered ENOSYS. Linux's signal syscalls
/// never return ENOSYS, so the answer cannot be mistaken for a kernel one.
pub(super) fn signal_refusal_record(
    reason: Option<&'static str>,
    result: i64,
) -> Option<reverie::UnsupportedRefusal> {
    reason
        .filter(|_| result == negative_errno(libc::ENOSYS))
        .map(|reason| {
            reverie::UnsupportedRefusal::new(
                reverie::UnsupportedOperation::Signal,
                reverie::Errno::ENOSYS,
                reason,
            )
        })
}

/// The calls whose answer Detcore decides on every backend, which this
/// executor therefore leaves unimplemented: the CPU, affinity, scheduling
/// policy and parameter, priority and I/O-priority calls; `membarrier`;
/// `getrandom`; `prlimit64`; the user and group id queries and setters;
/// `seccomp`; and `getpgrp`. Detcore answers each of them itself; no Detcore
/// path injects one on this backend, except getpgrp after a clone3 argument
/// rewrite races Detcore under --no-sequentialize-threads
/// (https://github.com/rrnewton/hermit/issues/4531), which Detcore then
/// ends as an internal failure. A Tool that injects one gets ENOSYS with the
/// typed refusal `ToolDecided` (see `tool_decided_refusal`); so does a guest
/// of a run without a Tool. `sched_get_priority_min` and
/// `sched_get_priority_max` are not here, because Detcore passes them
/// through to the host.
pub(crate) const TOOL_DECIDED_SYSCALLS: [libc::c_long; 32] = [
    libc::SYS_getcpu,
    libc::SYS_sched_getaffinity,
    libc::SYS_sched_getscheduler,
    libc::SYS_sched_setscheduler,
    libc::SYS_sched_getparam,
    libc::SYS_sched_setparam,
    libc::SYS_sched_getattr,
    libc::SYS_sched_setattr,
    libc::SYS_getpriority,
    libc::SYS_setpriority,
    libc::SYS_ioprio_get,
    libc::SYS_ioprio_set,
    libc::SYS_membarrier,
    libc::SYS_getrandom,
    libc::SYS_prlimit64,
    libc::SYS_getuid,
    libc::SYS_geteuid,
    libc::SYS_getgid,
    libc::SYS_getegid,
    libc::SYS_getresuid,
    libc::SYS_getresgid,
    libc::SYS_setuid,
    libc::SYS_setgid,
    libc::SYS_setreuid,
    libc::SYS_setregid,
    libc::SYS_setresuid,
    libc::SYS_setresgid,
    libc::SYS_setfsuid,
    libc::SYS_setfsgid,
    libc::SYS_setgroups,
    libc::SYS_seccomp,
    libc::SYS_getpgrp,
];

/// Diagnostic carried by this backend's typed refusal of a call in
/// `TOOL_DECIDED_SYSCALLS`.
pub(crate) const TOOL_DECIDED_REFUSAL: &str = "KVM leaves this call to the Tool: Detcore \
     decides it on every backend, so this executor keeps no copy of the answer";

/// The typed refusal, if any, that the result of a call in
/// `TOOL_DECIDED_SYSCALLS` carries. This executor forwards none of these
/// calls and answers each with ENOSYS before any effect of the call itself
/// (the bookkeeping every checked syscall does, such as installing the shared
/// descriptor table, still runs), so every ENOSYS answer to one is its own
/// refusal; every other result carries none.
pub(super) fn tool_decided_refusal(
    request: &SyscallRequest,
    result: i64,
) -> Option<reverie::UnsupportedRefusal> {
    (TOOL_DECIDED_SYSCALLS
        .iter()
        .any(|&number| request.number() == number as u64)
        && result == negative_errno(libc::ENOSYS))
    .then(|| {
        reverie::UnsupportedRefusal::new(
            reverie::UnsupportedOperation::ToolDecided,
            reverie::Errno::ENOSYS,
            TOOL_DECIDED_REFUSAL,
        )
    })
}

/// Diagnostic carried by this backend's typed refusal of `setsid` and
/// `setpgid`.
pub(crate) const PROCESS_GROUP_REFUSAL: &str = "KVM keeps no process groups or sessions a guest can change: setsid and setpgid are unsupported";

/// The typed refusal, if any, that a `setsid` or `setpgid` result carries.
/// Linux never fails either with ENOSYS, so every ENOSYS answer is this
/// backend's own refusal; every other result carries none.
pub(super) fn process_group_refusal(
    request: &SyscallRequest,
    result: i64,
) -> Option<reverie::UnsupportedRefusal> {
    (matches!(
        request.number(),
        n if n == libc::SYS_setsid as u64 || n == libc::SYS_setpgid as u64
    ) && result == negative_errno(libc::ENOSYS))
    .then(|| {
        reverie::UnsupportedRefusal::new(
            reverie::UnsupportedOperation::ProcessGroups,
            reverie::Errno::ENOSYS,
            PROCESS_GROUP_REFUSAL,
        )
    })
}
