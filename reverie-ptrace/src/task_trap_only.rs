/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Trap-only LiteInst site patching inside the ptrace task loop.
//!
//! A patched site holds `int 0x80` instead of `syscall`. Its seccomp stop
//! arrives tagged [`TAG_I386`]; H0 normalizes the registers to the x86_64
//! view the original `syscall` would have produced (rcx = S+2, r11 = RFLAGS,
//! a sign-extended 64-bit number) before any tool code runs. When the tool
//! runs the syscall in place, the "masked hop" (H1-H4) runs it through the
//! private page's traced `syscall` stub ([`SLOT`]) with every signal blocked
//! until the slot's own seccomp stop, so that signals become deliverable at
//! the same kernel state as ptrace's in-place resume. When the site-table
//! lifecycle restored the site at this very stop (a task-creating, mapping
//! or guest-install syscall), the same hop runs the syscall at the restored
//! site instead of the slot, so that a child the tracer never sees
//! (`CLONE_UNTRACED`) starts after the guest's own instruction. Every other tool
//! handler shape keeps ptrace's own paths (skip, private inject), which work
//! from an I386 stop unchanged because H0 has already normalized the
//! registers they save and restore.

use reverie::Errno;
#[cfg(test)]
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use safeptrace::Error as TraceError;
use safeptrace::Event;
use safeptrace::Stopped;
use safeptrace::Wait;

use super::TracedTask;
use crate::liteinst_trap_only::ALL_ADDRESSES;
use crate::liteinst_trap_only::DisabledReason;
use crate::liteinst_trap_only::PATCHED_BYTES;
use crate::liteinst_trap_only::RetiredReason;
use crate::liteinst_trap_only::SLOT;
use crate::liteinst_trap_only::SLOT_RET;
use crate::liteinst_trap_only::SYSCALL_BYTES;
use crate::liteinst_trap_only::TAG_I386;
use crate::liteinst_trap_only::TAG_SLOT;
use crate::liteinst_trap_only::TrapOnlyFailure;
use crate::liteinst_trap_only::is_reserved_site;
use crate::liteinst_trap_only::read_site;
use crate::liteinst_trap_only::site_mapping_is_patchable;
use crate::liteinst_trap_only::write_site;

/// How `handle_seccomp` continues after trap-only routing.
pub(super) enum TrapOnlyRoute {
    /// An ordinary x86_64 stop. `patch_site` is the site to consider patching
    /// once the tool has handled the stop.
    Ordinary { task: Stopped, patch_site: u64 },
    /// A live patched site, normalized to its x86_64 view. The task holds the
    /// view in `live_entry` until the syscall is consumed.
    Patched(Stopped),
    /// Handled entirely by trap-only code (foreign `int 0x80`).
    Done(Wait),
}

/// Where the hop left the task.
pub(super) enum HopOutcome {
    /// At the slot syscall's exit stop, with rip, rcx and r11 rewritten to the
    /// site's view.
    ExitStop(Stopped),
    /// A stop the caller forwards: a new child, exec, exit or death.
    Other(Wait),
}

/// x86_64 syscall numbers that seccomp passes through without running the
/// filter (upstream `__secure_computing` design): 335 is `uretprobe` and
/// 336 is `uprobe` (measured on this host's 7.1 kernel). Plain ptrace runs them at the
/// original site with no stop and no Tool event, while the slot's own
/// `syscall` would also bypass the filter and produce no TAG_SLOT stop, so
/// the hop cannot run them. They are matched by number, independently of
/// whether the `syscalls` crate knows them (0.6.18 stops at `rseq`, 334),
/// so that a crate bump can never dispatch them to the Tool.
const SECCOMP_BYPASSING_NUMBERS: [i32; 2] = [335, 336];

/// Whether a normalized patched-site number is one of
/// [`SECCOMP_BYPASSING_NUMBERS`].
fn bypasses_seccomp(orig_rax: u64) -> bool {
    SECCOMP_BYPASSING_NUMBERS.contains(&(orig_rax as i64 as i32))
}

fn nix_pid(pid: reverie::Pid) -> nix::unistd::Pid {
    nix::unistd::Pid::from_raw(pid.as_raw())
}

/// Numbers whose site is never patched: executing them changes the table or
/// the address space (P2 spec section 3 rule 3).
///
/// This screens only the number a site carries at the stop that would patch
/// it. A generic site (a libc `syscall()` wrapper, say) can later carry any
/// number through its `int 0x80`. Such a call is routed like any other
/// live-site call: `trap_only_route` fails closed on rt_sigreturn and on
/// unsubscribed numbers, and `trap_only_lifecycle` then runs for it at the
/// same stop, before any Tool code, exactly as for an x86_64 stop. A
/// mapping change carried this way restores the sites it reaches; a clone
/// with `CLONE_VM` but not `CLONE_VFORK`, a `CLONE_UNTRACED` clone and a
/// guest install restore every site; a site restored at its own stop runs
/// the syscall in place at the site. Syscalls a Tool injects do not pass
/// through that lifecycle (an injected clone is caught only later, at the
/// new-child stop); `SitePatching::On` stays test-only.
fn is_patchable_number(nr: Sysno) -> bool {
    #[cfg(target_arch = "x86_64")]
    if matches!(nr, Sysno::fork | Sysno::vfork) {
        return false;
    }
    !matches!(
        nr,
        Sysno::clone
            | Sysno::clone3
            | Sysno::mmap
            | Sysno::munmap
            | Sysno::mremap
            | Sysno::mprotect
            | Sysno::pkey_mprotect
            | Sysno::madvise
            | Sysno::shmat
            | Sysno::remap_file_pages
            | Sysno::seccomp
            | Sysno::prctl
            | Sysno::execve
            | Sysno::execveat
            | Sysno::exit
            | Sysno::exit_group
            | Sysno::rt_sigreturn
    )
}

const CLONE_VM: u64 = libc::CLONE_VM as u64;
const CLONE_VFORK: u64 = libc::CLONE_VFORK as u64;
const CLONE_UNTRACED: u64 = libc::CLONE_UNTRACED as u64;
const PAGE_SIZE: u64 = 4096;

/// The clone flags of a task-creating syscall, decoded at its creating stop,
/// before it runs. `clone3`'s flags are read from the guest while the address
/// space is single-task (when it is not, the table is already disabled and
/// the flags decide nothing). When they cannot be read the answer is the
/// conservative "shares the address space and is not a vfork"; the syscall
/// itself then fails with EFAULT.
fn creating_flags(tid: nix::unistd::Pid, nr: Sysno, args: &SyscallArgs) -> Option<u64> {
    match nr {
        Sysno::clone => Some(args.arg0 as u64),
        Sysno::clone3 => {
            use std::os::unix::fs::FileExt;
            let mut flags = [0u8; 8];
            Some(
                match std::fs::File::open(format!("/proc/{tid}/mem"))
                    .and_then(|mem| mem.read_exact_at(&mut flags, args.arg0 as u64))
                {
                    Ok(()) => u64::from_ne_bytes(flags),
                    Err(_) => CLONE_VM,
                },
            )
        }
        #[cfg(target_arch = "x86_64")]
        Sysno::fork => Some(0),
        #[cfg(target_arch = "x86_64")]
        Sysno::vfork => Some(CLONE_VM | CLONE_VFORK),
        _ => None,
    }
}

/// The page-rounded range `[start, start + len)`, saturating at the top of
/// the address space.
fn page_range(start: u64, len: u64) -> (u64, u64) {
    let end = start
        .checked_add(len)
        .and_then(|end| end.checked_add(PAGE_SIZE - 1))
        .map_or(u64::MAX, |end| end & !(PAGE_SIZE - 1));
    (start & !(PAGE_SIZE - 1), end)
}

/// The address ranges a mapping-changing syscall can reach, decided before it
/// runs (P2 spec section 4). Empty when it cannot change an existing mapping.
fn mapping_ranges(nr: Sysno, args: &SyscallArgs) -> Vec<(u64, u64)> {
    const MREMAP_FIXED: u64 = libc::MREMAP_FIXED as u64;
    const SHM_REMAP: u64 = libc::SHM_REMAP as u64;
    let (a0, a1, a2, a3, a4) = (
        args.arg0 as u64,
        args.arg1 as u64,
        args.arg2 as u64,
        args.arg3 as u64,
        args.arg4 as u64,
    );
    match nr {
        // Without MAP_FIXED the address is a hint and no existing mapping is
        // replaced (MAP_FIXED_NOREPLACE fails instead of replacing).
        Sysno::mmap if a3 & libc::MAP_FIXED as u64 != 0 => vec![page_range(a0, a1)],
        // PROT_GROWSDOWN extends the change down to the start of the
        // mapping, which the tracer does not know: reach every lower address.
        Sysno::mprotect | Sysno::pkey_mprotect if a2 & libc::PROT_GROWSDOWN as u64 != 0 => {
            vec![(0, page_range(a0, a1).1)]
        }
        Sysno::munmap
        | Sysno::mprotect
        | Sysno::pkey_mprotect
        | Sysno::madvise
        | Sysno::remap_file_pages => vec![page_range(a0, a1)],
        Sysno::mremap => {
            let mut ranges = vec![page_range(a0, a1.max(a2))];
            if a3 & MREMAP_FIXED != 0 {
                ranges.push(page_range(a4, a2));
            }
            ranges
        }
        // The segment's size is unknown to the tracer: SHM_REMAP may replace
        // any mapping. Without it an overlapping attach fails with EINVAL.
        Sysno::shmat if a2 & SHM_REMAP != 0 => vec![ALL_ADDRESSES],
        _ => Vec::new(),
    }
}

/// Whether the syscall installs (or tries to install) a guest seccomp filter
/// or syscall user dispatch (P2 spec section 5). `prctl`'s option and
/// `seccomp`'s operation are C `int`s, so only their low 32 bits count.
fn guest_install(nr: Sysno, args: &SyscallArgs) -> Option<DisabledReason> {
    const PR_SET_SECCOMP: u32 = 22;
    const PR_SET_SYSCALL_USER_DISPATCH: u32 = 59;
    const SECCOMP_GET_ACTION_AVAIL: u32 = 2;
    const SECCOMP_GET_NOTIF_SIZES: u32 = 3;
    match nr {
        // SET_MODE_STRICT, SET_MODE_FILTER and any operation this tracer does
        // not know; only the two read-only queries are excluded.
        Sysno::seccomp
            if !matches!(
                args.arg0 as u32,
                SECCOMP_GET_ACTION_AVAIL | SECCOMP_GET_NOTIF_SIZES
            ) =>
        {
            Some(DisabledReason::GuestSeccomp)
        }
        Sysno::prctl if args.arg0 as u32 == PR_SET_SECCOMP => Some(DisabledReason::GuestSeccomp),
        // Any mode other than PR_SYS_DISPATCH_OFF turns dispatch on.
        Sysno::prctl if args.arg0 as u32 == PR_SET_SYSCALL_USER_DISPATCH && args.arg1 != 0 => {
            Some(DisabledReason::Sud)
        }
        _ => None,
    }
}

/// Whether `parent` and `child` share one address space, from the kernel
/// (`kcmp(KCMP_VM)`); `None` when the kernel cannot answer.
fn shares_address_space(parent: reverie::Pid, child: reverie::Pid) -> Option<bool> {
    const KCMP_VM: libc::c_long = 1;
    let result = unsafe {
        libc::syscall(
            libc::SYS_kcmp,
            parent.as_raw() as libc::c_long,
            child.as_raw() as libc::c_long,
            KCMP_VM,
            0 as libc::c_long,
            0 as libc::c_long,
        )
    };
    match result {
        0 => Some(true),
        1..=3 => Some(false),
        _ => None,
    }
}

#[cfg(test)]
static STEP_COUNTS_GLOBAL: std::sync::Mutex<std::collections::BTreeMap<i32, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Test-only: counts every tracer single-step request per tid, so a test
/// tool can compare the forced-SIGTRAP profile of two runs.
#[cfg(test)]
pub(crate) fn record_step_for_test(tid: Pid) {
    *STEP_COUNTS_GLOBAL
        .lock()
        .unwrap()
        .entry(tid.as_raw())
        .or_default() += 1;
}

/// Test-only: the number of single-step requests issued for `tid` so far.
#[cfg(test)]
pub(crate) fn step_count_for_test(tid: Pid) -> u64 {
    STEP_COUNTS_GLOBAL
        .lock()
        .unwrap()
        .get(&tid.as_raw())
        .copied()
        .unwrap_or(0)
}

#[cfg(test)]
static STEPPED_SECCOMP_GLOBAL: std::sync::Mutex<std::collections::BTreeMap<i32, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Test-only: counts, per tid and on either backend, every timer
/// single-step that ended at a seccomp stop, so that a test Tool can tell a
/// stepped syscall entry from any other.
#[cfg(test)]
pub(crate) fn record_stepped_seccomp_for_test(tid: Pid) {
    *STEPPED_SECCOMP_GLOBAL
        .lock()
        .unwrap()
        .entry(tid.as_raw())
        .or_default() += 1;
}

/// Test-only: the number of timer single-steps of `tid` so far that ended
/// at a seccomp stop.
#[cfg(test)]
pub(crate) fn stepped_seccomp_count_for_test(tid: Pid) -> u64 {
    STEPPED_SECCOMP_GLOBAL
        .lock()
        .unwrap()
        .get(&tid.as_raw())
        .copied()
        .unwrap_or(0)
}

impl<L: Tool + 'static> TracedTask<L> {
    /// Publishes a trap-only failure as the run's failure and returns the
    /// error that unwinds the current handler.
    pub(super) fn trap_only_fail(&self, phase: &'static str, error: anyhow::Error) -> TraceError {
        self.publish_ordinary_failure(phase, reverie::Error::Tool(error));
        Errno::ECANCELED.into()
    }

    /// Maps a trap-only error for `handle_seccomp`: a published failure ends
    /// the run as that failure; anything else is an ordinary tracee error.
    pub(super) fn trap_only_error(
        &self,
        tid: reverie::Pid,
        error: TraceError,
        operation: &'static str,
    ) -> crate::error::Error {
        if self.global_state.fatal_session.is_failed() {
            return crate::error::Error::RunFailed;
        }
        crate::error::Error::Tracee {
            operation,
            pid: tid,
            source: error,
        }
    }

    fn trap_only_failure(&self, phase: &'static str, failure: TrapOnlyFailure) -> TraceError {
        self.trap_only_fail(phase, anyhow::Error::new(failure))
    }

    /// Routes a seccomp stop of a patching run (H0), before the mapping
    /// shortcut and before any tool code.
    #[cfg(target_arch = "x86_64")]
    pub(super) async fn trap_only_route(
        &mut self,
        task: Stopped,
    ) -> Result<TrapOnlyRoute, TraceError> {
        // `stepped_entry` describes only the stop that `handle_timer` just
        // re-dispatched; take it before any return so that it can never
        // survive into a later, unstepped entry.
        let stepped = std::mem::take(
            &mut self
                .trap_only
                .as_mut()
                .expect("trap-only routing")
                .stepped_entry,
        );
        let tag = task.getevent()?;
        let mut regs = task.getregs()?;
        if tag == TAG_SLOT as i64 {
            return Err(self.trap_only_failure(
                "trap-only seccomp routing",
                TrapOnlyFailure::StraySlotStop {
                    rip: regs.rip,
                    orig_rax: regs.orig_rax as i64,
                },
            ));
        }
        let site = regs.rip.wrapping_sub(2);
        if tag != TAG_I386 as i64 {
            self.timer.observe_event(&Event::Seccomp);
            return Ok(TrapOnlyRoute::Ordinary {
                task,
                patch_site: site,
            });
        }
        let trap_only = self.trap_only.as_ref().expect("trap-only routing");
        let live = !is_reserved_site(site) && trap_only.lock().is_live(site);
        if !live {
            // Plain ptrace reports no stop here: its filter kills the process
            // (SECCOMP_RET_KILL_PROCESS), which `trap_only_foreign_i386`
            // reproduces. The tick is not Tool-observable, because the
            // process dies of SIGSYS before any Tool event.
            self.timer.observe_event(&Event::Seccomp);
            return self
                .trap_only_foreign_i386(task, regs)
                .await
                .map(TrapOnlyRoute::Done);
        }
        // H0: the registers the original `syscall` would have produced. A
        // `syscall` executed under a timer single-step saves RFLAGS with TF set
        // into r11, and plain ptrace's timer clears that TF again at the step's
        // seccomp stop (`remove_stepping_trap_flag` in timer.rs), so r11 is
        // `eflags` whether or not the entry was stepped (O4, G).
        regs.rcx = regs.rip;
        regs.r11 = regs.eflags;
        regs.orig_rax = regs.orig_rax as u32 as i32 as i64 as u64;
        task.setregs(&regs)?;
        if bypasses_seccomp(regs.orig_rax) {
            return Err(self.trap_only_failure(
                "trap-only seccomp routing",
                TrapOnlyFailure::SeccompBypassingNumber {
                    site,
                    nr: regs.orig_rax as i64,
                },
            ));
        }
        match self.trap_only_decode(regs.orig_rax) {
            Some(_) => self.timer.observe_event(&Event::Seccomp),
            None if stepped => {
                // Rule 4. `handle_timer` classifies this stop first; this is
                // the backstop for any other route to a stepped entry.
                return Err(self.trap_only_failure(
                    "trap-only seccomp routing",
                    TrapOnlyFailure::AllowClassInTimerStep {
                        site,
                        nr: regs.orig_rax as i64,
                    },
                ));
            }
            None => {
                // O4 rule 3: plain ptrace has no stop here, so this internal
                // stop leaves the timer's cancellation state untouched: a
                // precise timer armed before the site stays armed across it.
                return self
                    .trap_only_allow_class(task, regs, site)
                    .await
                    .map(TrapOnlyRoute::Done);
            }
        }
        self.trap_only
            .as_mut()
            .expect("trap-only routing")
            .live_entry = Some(regs);
        Ok(TrapOnlyRoute::Patched(task))
    }

    /// The checked decode of a normalized patched-site number: `Some` for a
    /// number plain ptrace's filter traces (a known syscall the tool
    /// subscribes to, other than `rt_sigreturn`), and `None` for the Allow
    /// class, which plain ptrace runs without a stop: `rt_sigreturn`, an
    /// unsubscribed number, and any number the syscall table does not know
    /// (a gap, a negative number, or one past the table).
    fn trap_only_decode(&self, orig_rax: u64) -> Option<Sysno> {
        let nr = Sysno::new(orig_rax as i64 as i32 as usize)?;
        let subscribed = self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == nr);
        (subscribed && nr != Sysno::rt_sigreturn).then_some(nr)
    }

    /// O4 rule 3: an Allow-class number at a live patched site, outside a
    /// timer step. Plain ptrace's filter runs it without a stop and without
    /// a Tool event, so the tracer runs it through the masked hop without any
    /// Tool dispatch, then restores the site (reading the write back) and
    /// retires it as [`RetiredReason::AllowClass`].
    #[cfg(target_arch = "x86_64")]
    async fn trap_only_allow_class(
        &mut self,
        task: Stopped,
        view: libc::user_regs_struct,
        site: u64,
    ) -> Result<Wait, TraceError> {
        // The run loop already counted this I386 stop, which plain ptrace
        // never produces; the P2 comparator accounts for it explicitly.
        #[cfg(test)]
        if let Some(stats) = &self.global_state.backend_stats {
            stats.mark_last_stop_internal(self.tid);
        }
        match self.trap_only_hop(task, view).await? {
            HopOutcome::ExitStop(task) => {
                let restored = self
                    .trap_only
                    .as_ref()
                    .expect("trap-only routing")
                    .lock()
                    .restore_sites(
                        &[nix_pid(self.tid)],
                        &[(site, site.wrapping_add(2))],
                        RetiredReason::AllowClass,
                    );
                match restored {
                    Ok(1) => {}
                    Ok(count) => {
                        return Err(self.trap_only_fail(
                            "trap-only allow-class restore",
                            anyhow::anyhow!(
                                "restored {count} sites for site {site:#x}, expected 1"
                            ),
                        ));
                    }
                    Err(error) => {
                        return Err(self.trap_only_fail("trap-only allow-class restore", error));
                    }
                }
                #[cfg(test)]
                self.trap_only
                    .as_ref()
                    .expect("trap-only routing")
                    .shared
                    .hooks
                    .record(format!(
                        "allow-class site={site:#x} nr={}",
                        view.orig_rax as i64
                    ));
                self.resume_stopped(task, None)?.next_state().await
            }
            HopOutcome::Other(wait) => Ok(wait),
        }
    }

    /// O4 rule 4, called by `handle_timer` when a timer single-step ended at
    /// a seccomp stop. An Allow-class number at a live patched site fails
    /// closed with `TrapOnlyAllowClassInTimerStep`: under plain ptrace that
    /// step runs the syscall without a stop and ends after it. Any other
    /// patched-site number is re-dispatched as usual, with H0 told that the
    /// entry was stepped.
    #[cfg(target_arch = "x86_64")]
    pub(super) fn trap_only_stepped_seccomp(&mut self, task: &Stopped) -> Result<(), TraceError> {
        if self.trap_only.is_none() || task.getevent()? != TAG_I386 as i64 {
            return Ok(());
        }
        let regs = task.getregs()?;
        let site = regs.rip.wrapping_sub(2);
        let trap_only = self.trap_only.as_ref().expect("checked above");
        if is_reserved_site(site) || !trap_only.lock().is_live(site) {
            return Ok(());
        }
        let nr = regs.orig_rax as u32 as i32 as i64 as u64;
        if bypasses_seccomp(nr) {
            return Err(self.trap_only_failure(
                "trap-only timer step",
                TrapOnlyFailure::SeccompBypassingNumber {
                    site,
                    nr: nr as i64,
                },
            ));
        }
        if self.trap_only_decode(nr).is_none() {
            return Err(self.trap_only_failure(
                "trap-only timer step",
                TrapOnlyFailure::AllowClassInTimerStep {
                    site,
                    nr: nr as i64,
                },
            ));
        }
        self.trap_only
            .as_mut()
            .expect("checked above")
            .stepped_entry = true;
        Ok(())
    }

    /// Removes and returns the live patched-site entry, if any.
    pub(super) fn trap_only_take_live_entry(&mut self) -> Option<libc::user_regs_struct> {
        self.trap_only
            .as_mut()
            .and_then(|trap_only| trap_only.live_entry.take())
    }

    /// Takes the view stashed by a tail hop that ended at a new-child stop.
    pub(super) fn trap_only_take_new_child_view(&mut self) -> Option<libc::user_regs_struct> {
        self.trap_only
            .as_mut()
            .and_then(|trap_only| trap_only.new_child_view.take())
    }

    /// Patches `site` after the tool has handled its first ordinary stop, if
    /// every rule of the patch decision holds (P2 spec section 3).
    pub(super) fn trap_only_maybe_patch(&self, site: u64, nr: Sysno) -> Result<(), TraceError> {
        let Some(trap_only) = self.trap_only.as_ref() else {
            return Ok(());
        };
        if !trap_only.shared.full_subscription || !is_patchable_number(nr) || is_reserved_site(site)
        {
            return Ok(());
        }
        let mut table = trap_only.lock();
        if !table.accepts_new_sites() || table.knows(site) {
            return Ok(());
        }
        let tid = nix_pid(self.tid);
        match site_mapping_is_patchable(tid, site) {
            Ok(Ok(())) => {}
            Ok(Err(_decline)) => return Ok(()),
            Err(error) => {
                return Err(self.trap_only_fail(
                    "trap-only site patch",
                    anyhow::Error::new(error).context(format!("read /proc/{tid}/maps")),
                ));
            }
        }
        match read_site(tid, site) {
            Ok(bytes) if bytes == SYSCALL_BYTES => {}
            Ok(_) => return Ok(()),
            Err(error) => {
                return Err(self.trap_only_fail(
                    "trap-only site patch",
                    anyhow::Error::new(error).context(format!("read site {site:#x}")),
                ));
            }
        }
        #[cfg(test)]
        let skip_write = trap_only
            .shared
            .hooks
            .skip_patch_write
            .load(std::sync::atomic::Ordering::SeqCst);
        #[cfg(not(test))]
        let skip_write = false;
        write_site(tid, site, PATCHED_BYTES, skip_write)
            .map_err(|error| self.trap_only_fail("trap-only site patch", error))?;
        table.record_live(site, SYSCALL_BYTES);
        Ok(())
    }

    /// Runs a patched site's syscall in place (H1-H4): through the slot, or
    /// at the site itself when the site-table lifecycle restored it at this
    /// stop.
    #[cfg(target_arch = "x86_64")]
    pub(super) async fn trap_only_hop(
        &mut self,
        task: Stopped,
        view: libc::user_regs_struct,
    ) -> Result<HopOutcome, TraceError> {
        let site = view.rip.wrapping_sub(2);
        let nr = view.orig_rax;
        if let Some(trap_only) = self.trap_only.as_mut() {
            trap_only.in_hop = true;
        }
        let outcome = self.trap_only_hop_legs(task, view, site, nr).await;
        if let Some(trap_only) = self.trap_only.as_mut() {
            trap_only.in_hop = false;
        }
        outcome
    }

    #[cfg(target_arch = "x86_64")]
    async fn trap_only_hop_legs(
        &mut self,
        task: Stopped,
        view: libc::user_regs_struct,
        site: u64,
        nr: u64,
    ) -> Result<HopOutcome, TraceError> {
        // A site restored at this very stop (by the site-table lifecycle, for
        // a task-creating, mapping or install syscall) holds `syscall` again:
        // run the syscall there, in place, so that everything the kernel
        // derives from the instruction pointer is ptrace's. That includes the
        // start address of a child no new-child stop fixes up (CLONE_UNTRACED).
        let in_place = self
            .trap_only
            .as_ref()
            .is_some_and(|trap_only| !trap_only.lock().is_live(site));
        let (target, expected_tag, expected_rip) = if in_place {
            let bytes = read_site(nix_pid(self.tid), site);
            if !matches!(bytes, Ok(bytes) if bytes == SYSCALL_BYTES) {
                return Err(self.trap_only_failure(
                    "trap-only hop",
                    TrapOnlyFailure::HopUnexpectedStop {
                        phase: "H1 in-place site",
                        site,
                        stop: format!("restored site reads {bytes:02x?}"),
                    },
                ));
            }
            (site, 0, view.rip)
        } else {
            (SLOT, TAG_SLOT as i64, SLOT_RET)
        };

        // rt_sigreturn legitimately leaves the hop somewhere else: at the
        // signal frame's saved rip, which H4 must then leave alone (the frame's
        // registers win, as under ptrace). The kernel reads the frame at the
        // same rsp the hop runs with.
        let sigreturn = if nr == Sysno::rt_sigreturn as u64 {
            sigreturn_frame(nix_pid(self.tid), view.rsp)
        } else {
            None
        };

        // H1: block everything, skip the int 0x80, and run `syscall` at SLOT
        // (or at the restored site).
        let saved_mask = task.getsigmask()?;
        task.setsigmask(!0)?;
        let mut regs = view;
        regs.orig_rax = -1i64 as u64;
        regs.rax = nr;
        regs.rip = target;
        task.setregs(&regs)?;
        let wait = self.resume_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);

        // H2: the slot's (or the restored site's) own seccomp stop.
        let task = match wait {
            Wait::Stopped(task, Event::Seccomp) => {
                let tag = task.getevent()?;
                let regs = task.getregs()?;
                if tag != expected_tag || regs.rip != expected_rip || regs.orig_rax != nr {
                    return Err(self.trap_only_failure(
                        "trap-only hop",
                        TrapOnlyFailure::HopUnexpectedStop {
                            phase: "H2 slot stop",
                            site,
                            stop: format!(
                                "seccomp stop tag {tag:#x} rip {:#x} orig_rax {}",
                                regs.rip, regs.orig_rax as i64
                            ),
                        },
                    ));
                }
                task
            }
            wait @ (Wait::Exited(..) | Wait::Stopped(_, Event::Exit)) => {
                return Ok(HopOutcome::Other(wait));
            }
            Wait::Stopped(task, event) => {
                let rip = task.getregs().map(|regs| regs.rip).unwrap_or(0);
                return Err(self.trap_only_failure(
                    "trap-only hop",
                    TrapOnlyFailure::HopUnexpectedStop {
                        phase: "H2 slot stop",
                        site,
                        stop: format!("{event:?} at rip {rip:#x}"),
                    },
                ));
            }
        };

        // H3: the original mask, then run the syscall to its exit stop.
        task.setsigmask(saved_mask)?;
        let wait = self.syscall_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);

        // H4.
        match wait {
            Wait::Stopped(task, Event::Syscall) => {
                let mut regs = task.getregs()?;
                #[cfg(test)]
                if self.trap_only.as_ref().is_some_and(|trap_only| {
                    trap_only
                        .shared
                        .hooks
                        .displace_hop_exit_rip
                        .load(std::sync::atomic::Ordering::SeqCst)
                }) {
                    regs.rip = regs.rip.wrapping_add(1);
                }
                if sigreturn.is_some_and(|frame| frame.rip == regs.rip && frame.rsp == regs.rsp) {
                    // rt_sigreturn loaded the frame's registers, which win
                    // even when the frame's own rip is the slot's return.
                } else if regs.rip == expected_rip {
                    regs.rip = view.rip;
                    regs.rcx = view.rcx;
                    regs.r11 = view.r11;
                    task.setregs(&regs)?;
                } else {
                    // Even a restarting syscall's exit stop still has rip at
                    // the return address (the rewind happens later, at signal
                    // delivery), and exec is forwarded before its exit stop.
                    return Err(self.trap_only_failure(
                        "trap-only hop",
                        TrapOnlyFailure::HopExitRip {
                            site,
                            nr: nr as i64,
                            rip: regs.rip,
                            expected: match sigreturn {
                                Some(frame) => format!(
                                    "{expected_rip:#x} or frame rip {:#x} (rsp {:#x})",
                                    frame.rip, frame.rsp
                                ),
                                None => format!("{expected_rip:#x}"),
                            },
                        },
                    ));
                }
                Ok(HopOutcome::ExitStop(task))
            }
            wait @ (Wait::Stopped(_, Event::NewChild(..))
            | Wait::Stopped(_, Event::Exec(_))
            | Wait::Stopped(_, Event::Exit)
            | Wait::Exited(..)) => Ok(HopOutcome::Other(wait)),
            Wait::Stopped(task, event) => {
                let rip = task.getregs().map(|regs| regs.rip).unwrap_or(0);
                Err(self.trap_only_failure(
                    "trap-only hop",
                    TrapOnlyFailure::HopUnexpectedStop {
                        phase: "H4 exit stop",
                        site,
                        stop: format!("{event:?} at rip {rip:#x}"),
                    },
                ))
            }
        }
    }

    /// The in-place inject of a live patched site's exact syscall.
    #[cfg(target_arch = "x86_64")]
    pub(super) async fn trap_only_inject_hop(
        &mut self,
        task: Stopped,
        view: libc::user_regs_struct,
    ) -> Result<Result<i64, Errno>, TraceError> {
        match self.trap_only_hop(task, view).await? {
            HopOutcome::ExitStop(task) => {
                let regs = task.getregs()?;
                Ok(Errno::from_ret(regs.rax as usize).map(|x| x as i64))
            }
            HopOutcome::Other(Wait::Stopped(parent, Event::NewChild(op, child))) => {
                let ret = child.pid().as_raw() as i64;
                // Restore the site's view in the parent as ptrace's exact
                // inject leaves it (rip S+2, the arguments, orig_rax, rcx and
                // r11) except rax, which the kernel writes when the call
                // returns. Passing the view as the parent context would also
                // write the child's id into rax, and a vfork parent, still in
                // the call at its vfork-done stop after `handle_new_task`'s
                // step, would show that id where ptrace shows the entry's
                // -ENOSYS. The child still gets the view as its context.
                super::restore_context(&parent, view, None, false)?;
                let _ = self
                    .dispatch_new_task(op, parent, child, None, Some(view))
                    .await?;
                Ok(Ok(ret))
            }
            HopOutcome::Other(Wait::Stopped(task, Event::Exec(former_tid))) => {
                let next_state = self.handle_exec_event(task, former_tid).await?;
                self.execve(next_state).await
            }
            HopOutcome::Other(Wait::Exited(_pid, exit_status)) => self.exit(exit_status).await,
            HopOutcome::Other(wait) => self.abort(Ok(wait)).await,
        }
    }

    /// Replaces `handle_seccomp`'s final resume after the tool tail-injected a
    /// live patched site's exact syscall.
    ///
    /// The exit stop is resumed without a signal. ptrace's final resume is
    /// from the seccomp stop, where the kernel ignores a resume signal
    /// (`ptrace_event` discards `ptrace_notify`'s result), while a signal
    /// passed at a syscall-exit stop is sent (`ptrace_report_syscall`). So
    /// dropping it is what keeps the two backends equal.
    pub(super) async fn trap_only_tail_hop(
        &mut self,
        task: Stopped,
        view: libc::user_regs_struct,
    ) -> Result<Wait, TraceError> {
        match self.trap_only_hop(task, view).await? {
            HopOutcome::ExitStop(task) => self.resume_stopped(task, None)?.next_state().await,
            HopOutcome::Other(wait @ Wait::Stopped(_, Event::NewChild(..))) => {
                // The run loop dispatches this stop; its handler restores both
                // tasks from the view, as the inject path does directly.
                if let Some(trap_only) = self.trap_only.as_mut() {
                    trap_only.new_child_view = Some(view);
                }
                Ok(wait)
            }
            HopOutcome::Other(wait) => Ok(wait),
        }
    }

    /// Site-table lifecycle at a seccomp stop, before the syscall runs and
    /// before any Tool code sees it, whether it arrived as an x86_64 or a
    /// (normalized) I386 stop (P2 spec sections 4 and 5).
    ///
    /// - A task-creating syscall records its clone flags for the new-child
    ///   stop. When it would create a second executing task (`CLONE_VM`
    ///   without `CLONE_VFORK`) or an untraced one (`CLONE_UNTRACED`, which
    ///   produces no new-child stop at all), every site is restored and the
    ///   table disabled now, while this is the only task that can execute
    ///   the address space.
    /// - A mapping change restores the sites it can reach.
    /// - A guest seccomp filter or syscall user dispatch restores every site
    ///   and disables the lineage, whether or not the install then succeeds.
    ///
    /// The call then proceeds exactly as under plain ptrace.
    pub(super) fn trap_only_lifecycle(
        &mut self,
        nr: Sysno,
        args: &SyscallArgs,
    ) -> Result<(), TraceError> {
        let tid = nix_pid(self.tid);
        let Some(trap_only) = self.trap_only.as_mut() else {
            return Ok(());
        };
        trap_only.pending_clone_flags = None;
        if !trap_only.lock().patching().rewrites_sites() {
            return Ok(());
        }
        let result = if let Some(flags) = creating_flags(tid, nr, args) {
            trap_only.pending_clone_flags = Some(flags);
            let second_executor = flags & CLONE_VM != 0 && flags & CLONE_VFORK == 0;
            if second_executor || flags & CLONE_UNTRACED != 0 {
                let restored = trap_only.retire_all(
                    &[tid],
                    RetiredReason::MultiTask,
                    DisabledReason::MultiTask,
                );
                #[cfg(test)]
                if let Ok(restored) = &restored {
                    trap_only.shared.hooks.record(format!(
                        "creating {nr} flags={flags:#x} restored={restored}"
                    ));
                }
                restored.map(drop)
            } else {
                Ok(())
            }
        } else if let Some(reason) = guest_install(nr, args) {
            let restored = {
                let mut table = trap_only.lock();
                table.mark_lineage(reason);
                table.restore_sites(&[tid], &[ALL_ADDRESSES], retired_for(reason))
            };
            #[cfg(test)]
            if let Ok(restored) = &restored {
                trap_only
                    .shared
                    .hooks
                    .record(format!("install {nr} {reason:?} restored={restored}"));
            }
            restored.map(drop)
        } else {
            let ranges = mapping_ranges(nr, args);
            if ranges.is_empty() {
                Ok(())
            } else {
                let restored =
                    trap_only
                        .lock()
                        .restore_sites(&[tid], &ranges, RetiredReason::Mapping);
                #[cfg(test)]
                if let Ok(restored) = &restored {
                    trap_only
                        .shared
                        .hooks
                        .record(format!("mapping {nr} restored={restored}"));
                }
                restored.map(drop)
            }
        };
        result.map_err(|error| self.trap_only_fail("trap-only site-table lifecycle", error))
    }

    /// Trap-only bookkeeping at a new-child event stop, before either task
    /// runs: give the child the table of its address space.
    ///
    /// The decision uses the clone flags decoded at the creating stop, never
    /// `ChildOp`. A second executing task was already made safe there. When
    /// no creating stop decoded these flags (a Tool-injected clone), or they
    /// disagree with the kernel (the stop's `CLONE_VFORK` or `kcmp(KCMP_VM)`),
    /// every site is restored now, in both tasks, and the two tasks share the
    /// disabled table: both are stopped, and an auto-attached child executes
    /// no user code before its first stop, so the writes cannot race.
    pub(super) fn trap_only_new_child(
        &mut self,
        parent: &Stopped,
        child: reverie::Pid,
        op: safeptrace::ChildOp,
    ) -> Result<Option<crate::liteinst_trap_only::TrapOnlyTask>, TraceError> {
        let Some(trap_only) = self.trap_only.as_mut() else {
            return Ok(None);
        };
        let recorded = trap_only.pending_clone_flags.take();
        #[cfg(test)]
        let recorded = recorded.filter(|_| {
            !trap_only
                .shared
                .hooks
                .forget_clone_flags
                .load(std::sync::atomic::Ordering::SeqCst)
        });
        #[cfg(test)]
        let recorded = recorded.map(|flags| {
            if trap_only
                .shared
                .hooks
                .flip_recorded_clone_vm
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                flags ^ CLONE_VM
            } else {
                flags
            }
        });
        let vfork = op == safeptrace::ChildOp::Vfork;
        let shared_vm = shares_address_space(parent.pid(), child);
        let flags = recorded.filter(|flags| {
            (flags & CLONE_VFORK != 0) == vfork
                && shared_vm.is_none_or(|shared| shared == (flags & CLONE_VM != 0))
        });
        let trap_only = self.trap_only.as_ref().expect("checked above");
        let Some(flags) = flags else {
            let restored = trap_only
                .retire_all(
                    &[nix_pid(parent.pid()), nix_pid(child)],
                    RetiredReason::MultiTask,
                    DisabledReason::MultiTask,
                )
                .map_err(|error| self.trap_only_fail("trap-only new-child restore", error))?;
            #[cfg(test)]
            trap_only.shared.hooks.record(format!(
                "new-child {op:?} undecided recorded={recorded:x?} restored={restored}"
            ));
            let _ = restored;
            return Ok(Some(trap_only.child(true)));
        };
        let shares = flags & CLONE_VM != 0;
        if shares && flags & CLONE_VFORK == 0 {
            let table = trap_only.lock();
            let live = table.patched_sites();
            if live != 0 || table.accepts_new_sites() {
                let state = format!("{:?}", table.state());
                drop(table);
                return Err(self.trap_only_failure(
                    "trap-only new-child",
                    TrapOnlyFailure::MultiTaskLiveSites {
                        child: child.as_raw(),
                        live,
                        state,
                    },
                ));
            }
        }
        #[cfg(test)]
        trap_only
            .shared
            .hooks
            .record(format!("new-child {op:?} flags={flags:#x} shares={shares}"));
        Ok(Some(trap_only.child(shares)))
    }

    /// Gives an exec'ing task the empty table of its new address space and
    /// drops its hop state. A vfork child's exec detaches it from its
    /// parent's table.
    pub(super) fn trap_only_exec(&mut self, initial_command: bool) {
        let Some(trap_only) = self.trap_only.as_mut() else {
            return;
        };
        trap_only.live_entry = None;
        trap_only.new_child_view = None;
        trap_only.pending_clone_flags = None;
        let mut fresh = trap_only.lock().after_exec();
        if !trap_only.shared.full_subscription {
            fresh.disable(DisabledReason::PartialSubscription);
        }
        #[cfg(test)]
        trap_only.shared.hooks.record(format!(
            "exec initial={initial_command} entries={} state={:?}",
            fresh.entries(),
            fresh.state()
        ));
        if initial_command {
            // The launch exec: keep the table the tracer handle observes.
            *trap_only.lock() = fresh;
        } else {
            trap_only.sites = std::sync::Arc::new(std::sync::Mutex::new(fresh));
            #[cfg(test)]
            trap_only
                .shared
                .hooks
                .record_table("exec".to_owned(), &trap_only.sites);
        }
    }

    /// An I386 stop that is not a live patched site: guest code really using
    /// the IA-32 ABI. Plain ptrace's filter kills the process with an
    /// uncatchable SIGSYS; reproduce that without any tool dispatch.
    #[cfg(target_arch = "x86_64")]
    async fn trap_only_foreign_i386(
        &mut self,
        task: Stopped,
        regs: libc::user_regs_struct,
    ) -> Result<Wait, TraceError> {
        let rip = regs.rip;
        let foreign = |reason: String| TrapOnlyFailure::ForeignI386 { rip, reason };
        // Consume the int 0x80 itself (orig_rax = -1).
        let task = self.skip_seccomp_syscall(task).await?;
        // A zeroed kernel `struct sigaction` (SIG_DFL, no flags, empty mask),
        // written below the red zone of the guest stack.
        let act = (regs.rsp.wrapping_sub(256)) & !15;
        {
            use std::os::unix::fs::FileExt;
            let written = std::fs::OpenOptions::new()
                .write(true)
                .open(format!("/proc/{}/mem", task.pid()))
                .and_then(|mem| mem.write_all_at(&[0u8; 32], act));
            if let Err(error) = written {
                return Err(self.trap_only_failure(
                    "trap-only foreign int 0x80",
                    foreign(format!("write SIG_DFL sigaction at {act:#x}: {error}")),
                ));
            }
        }
        let restored = self
            .untraced_syscall(
                task,
                Sysno::rt_sigaction,
                reverie::syscalls::SyscallArgs::new(
                    libc::SIGSYS as usize,
                    act as usize,
                    0,
                    8,
                    0,
                    0,
                ),
            )
            .await?;
        if let Err(errno) = restored {
            return Err(self.trap_only_failure(
                "trap-only foreign int 0x80",
                foreign(format!("rt_sigaction(SIGSYS, SIG_DFL) failed: {errno}")),
            ));
        }
        let task = self.assume_stopped();
        // Only SIGSYS may be delivered from here on, so nothing runs first.
        task.setsigmask(!(1u64 << (libc::SIGSYS - 1)))?;
        let tid = self.tid;
        let sent = unsafe {
            libc::syscall(
                libc::SYS_tgkill,
                self.pid.as_raw(),
                tid.as_raw(),
                libc::SIGSYS,
            )
        };
        if sent != 0 {
            return Err(self.trap_only_failure(
                "trap-only foreign int 0x80",
                foreign(format!(
                    "tgkill(SIGSYS) failed: {}",
                    std::io::Error::last_os_error()
                )),
            ));
        }
        let mut wait = self.resume_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        let mut delivered = false;
        loop {
            match wait {
                Wait::Stopped(task, Event::Signal(nix::sys::signal::Signal::SIGSYS))
                    if !delivered =>
                {
                    delivered = true;
                    wait = self
                        .resume_stopped(task, nix::sys::signal::Signal::SIGSYS)?
                        .next_state()
                        .await?;
                    self.arm_liteinst_wait(&wait);
                }
                wait @ (Wait::Exited(..) | Wait::Stopped(_, Event::Exit)) => return Ok(wait),
                Wait::Stopped(_task, event) => {
                    return Err(self.trap_only_failure(
                        "trap-only foreign int 0x80",
                        foreign(format!("unexpected {event:?} while dying of SIGSYS")),
                    ));
                }
            }
        }
    }
}

/// The registers an `rt_sigreturn` at stack pointer `rsp` loads from its
/// `rt_sigframe`, if the kernel's loads before `restore_sigcontext` sets the
/// registers can succeed. The frame starts one word below rsp (the
/// handler's `ret` popped `pretcode`), so its `ucontext` is at rsp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SigreturnFrame {
    rip: u64,
    rsp: u64,
}

/// Reads [`SigreturnFrame`] the way x86_64 `sys_rt_sigreturn` does before
/// any register changes: `access_ok` over the whole frame, then
/// `uc_sigmask`, `uc_flags` and the `uc_mcontext` sigcontext, each through
/// the guest's own page protections. `process_vm_readv`, like the kernel's
/// user copies and unlike `/proc/<tid>/mem` (FOLL_FORCE), fails on a
/// PROT_NONE page. `None` means the kernel fails (`badframe`) before loading
/// any register, leaving rip at the hop's return address. A later failure
/// (the FPU state or `uc_stack`) happens after the registers are loaded, and
/// then the frame's registers are what ptrace shows too.
fn sigreturn_frame(tid: nix::unistd::Pid, rsp: u64) -> Option<SigreturnFrame> {
    // struct ucontext: uc_flags (8), uc_link (8), uc_stack (24),
    // uc_mcontext (struct sigcontext, 256), uc_sigmask (8).
    const UC_FLAGS: u64 = 0;
    const UC_MCONTEXT: u64 = 8 + 8 + 24;
    const SIGCONTEXT_SIZE: usize = 256;
    const UC_SIGMASK: u64 = UC_MCONTEXT + SIGCONTEXT_SIZE as u64;
    // struct rt_sigframe: pretcode (8), ucontext (304), siginfo (128).
    const RT_SIGFRAME_SIZE: u64 = 8 + UC_SIGMASK + 8 + 128;
    // x86_64 TASK_SIZE_MAX with 4-level paging; a frame above it fails
    // `access_ok`. (With 5-level paging a frame above 2^47 is judged
    // unreadable here, which fails closed at H4 unless its rip is SLOT_RET.)
    const TASK_SIZE_MAX: u64 = (1 << 47) - 4096;
    let frame = rsp.checked_sub(8)?;
    if frame.checked_add(RT_SIGFRAME_SIZE)? > TASK_SIZE_MAX {
        return None;
    }
    let read = |offset: u64, bytes: &mut [u8]| -> Option<()> {
        let local = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let remote = libc::iovec {
            iov_base: rsp.checked_add(offset)? as *mut libc::c_void,
            iov_len: bytes.len(),
        };
        // SAFETY: `local` describes `bytes`, which outlives the call.
        let copied = unsafe { libc::process_vm_readv(tid.as_raw(), &local, 1, &remote, 1, 0) };
        (copied == bytes.len() as isize).then_some(())
    };
    read(UC_SIGMASK, &mut [0u8; 8])?;
    read(UC_FLAGS, &mut [0u8; 8])?;
    let mut sigcontext = [0u8; SIGCONTEXT_SIZE];
    read(UC_MCONTEXT, &mut sigcontext)?;
    // struct sigcontext_64 general registers: r8..r15, rdi, rsi, rbp, rbx,
    // rdx, rax, rcx, rsp (15), rip (16).
    let register = |index: usize| {
        u64::from_ne_bytes(
            sigcontext[index * 8..index * 8 + 8]
                .try_into()
                .expect("an 8-byte slice"),
        )
    };
    Some(SigreturnFrame {
        rip: register(16),
        rsp: register(15),
    })
}

/// The site retirement reason for a guest install.
fn retired_for(reason: DisabledReason) -> RetiredReason {
    match reason {
        DisabledReason::Sud => RetiredReason::Sud,
        _ => RetiredReason::GuestSeccomp,
    }
}

/// Off x86_64 there is no `int 0x80`: the IA-32 probe reports it unavailable,
/// `require_ia32_emulation` refuses every trap-only launch, and so no task
/// carries trap-only state. These entry points are unreachable there; they
/// fail closed rather than resume a stop.
#[cfg(not(target_arch = "x86_64"))]
impl<L: Tool + 'static> TracedTask<L> {
    fn trap_only_unsupported(&self, phase: &'static str) -> TraceError {
        self.trap_only_fail(
            phase,
            anyhow::anyhow!("trap-only LiteInst site patching exists only on x86_64"),
        )
    }

    pub(super) async fn trap_only_route(
        &mut self,
        _task: Stopped,
    ) -> Result<TrapOnlyRoute, TraceError> {
        Err(self.trap_only_unsupported("trap-only seccomp routing"))
    }

    /// `handle_timer` calls this for every run; without trap-only state it
    /// must leave the stop to plain ptrace, as the x86_64 version does.
    pub(super) fn trap_only_stepped_seccomp(&mut self, _task: &Stopped) -> Result<(), TraceError> {
        if self.trap_only.is_none() {
            return Ok(());
        }
        Err(self.trap_only_unsupported("trap-only timer step"))
    }

    pub(super) async fn trap_only_hop(
        &mut self,
        _task: Stopped,
        _view: libc::user_regs_struct,
    ) -> Result<HopOutcome, TraceError> {
        Err(self.trap_only_unsupported("trap-only hop"))
    }

    pub(super) async fn trap_only_inject_hop(
        &mut self,
        _task: Stopped,
        _view: libc::user_regs_struct,
    ) -> Result<Result<i64, Errno>, TraceError> {
        Err(self.trap_only_unsupported("trap-only hop"))
    }
}

/// Asserts (in debug builds) that the task is not inside the masked hop.
pub(super) fn assert_not_in_hop(trap_only: Option<&crate::liteinst_trap_only::TrapOnlyTask>) {
    debug_assert!(
        !trap_only.is_some_and(|trap_only| trap_only.in_hop),
        "the masked hop must never single-step"
    );
}

#[cfg(test)]
mod mapping_range_tests {
    use super::*;

    fn ranges(nr: Sysno, args: [usize; 5]) -> Vec<(u64, u64)> {
        let [a0, a1, a2, a3, a4] = args;
        mapping_ranges(nr, &SyscallArgs::new(a0, a1, a2, a3, a4, 0))
    }

    #[test]
    fn mapping_ranges_cover_every_address_a_change_can_reach() {
        let rx = (libc::PROT_READ | libc::PROT_EXEC) as usize;
        let page = PAGE_SIZE as usize;
        // pkey_mprotect and remap_file_pages: the page-rounded range.
        assert_eq!(
            ranges(Sysno::pkey_mprotect, [0x5000_0010, 1, rx, 0, 0]),
            vec![(0x5000_0000, 0x5000_1000)]
        );
        assert_eq!(
            ranges(Sysno::remap_file_pages, [0x5000_0000, 2 * page, 0, 0, 0]),
            vec![(0x5000_0000, 0x5000_2000)]
        );
        // PROT_GROWSDOWN reaches down to the (unknown) start of the mapping.
        let growsdown = rx | libc::PROT_GROWSDOWN as usize;
        for nr in [Sysno::mprotect, Sysno::pkey_mprotect] {
            assert_eq!(
                ranges(nr, [0x5000_1000, page, growsdown, 0, 0]),
                vec![(0, 0x5000_2000)],
                "{nr}"
            );
        }
        // mremap: the source up to the larger size, and the MREMAP_FIXED
        // destination at the new size.
        let fixed = (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as usize;
        assert_eq!(
            ranges(
                Sysno::mremap,
                [0x5000_0000, page, 2 * page, fixed, 0x6000_0000]
            ),
            vec![(0x5000_0000, 0x5000_2000), (0x6000_0000, 0x6000_2000)]
        );
        assert_eq!(
            ranges(Sysno::mremap, [0x5000_0000, page, 2 * page, 1, 0x6000_0000]),
            vec![(0x5000_0000, 0x5000_2000)]
        );
        // shmat with SHM_REMAP may replace any mapping; without it, none.
        assert_eq!(
            ranges(
                Sysno::shmat,
                [3, 0x5000_0000, libc::SHM_REMAP as usize, 0, 0]
            ),
            vec![ALL_ADDRESSES]
        );
        assert!(ranges(Sysno::shmat, [3, 0x5000_0000, 0, 0, 0]).is_empty());
        // mmap replaces a mapping only with MAP_FIXED.
        let private = libc::MAP_PRIVATE as usize;
        assert!(ranges(Sysno::mmap, [0x5000_0000, page, rx, private, 0]).is_empty());
        assert_eq!(
            ranges(
                Sysno::mmap,
                [0x5000_0000, page, rx, private | libc::MAP_FIXED as usize, 0]
            ),
            vec![(0x5000_0000, 0x5000_1000)]
        );
        // The end saturates at the top of the address space.
        assert_eq!(
            ranges(Sysno::munmap, [!(page - 1), 2 * page, 0, 0, 0]),
            vec![(!(PAGE_SIZE - 1), u64::MAX)]
        );
    }
}
