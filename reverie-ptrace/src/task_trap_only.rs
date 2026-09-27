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
//! the same kernel state as ptrace's in-place resume. Every other tool
//! handler shape keeps ptrace's own paths (skip, private inject), which work
//! from an I386 stop unchanged because H0 has already normalized the
//! registers they save and restore.

use reverie::Errno;
#[cfg(test)]
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Sysno;
use safeptrace::Error as TraceError;
use safeptrace::Event;
use safeptrace::Stopped;
use safeptrace::Wait;

use super::TracedTask;
use crate::liteinst_trap_only::DisabledReason;
use crate::liteinst_trap_only::PATCHED_BYTES;
use crate::liteinst_trap_only::RetiredReason;
use crate::liteinst_trap_only::SLOT;
use crate::liteinst_trap_only::SLOT_RET;
use crate::liteinst_trap_only::SYSCALL_BYTES;
use crate::liteinst_trap_only::SiteTable;
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
/// unsubscribed numbers, and in this step nothing updates the table for an
/// address-space change such a call makes. `SitePatching::On` is test-only
/// while that holds.
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

/// The clone flags of the syscall a new-child event stop belongs to. When
/// they cannot be read, the answer is the conservative "shares the address
/// space and is not a vfork".
#[cfg(target_arch = "x86_64")]
fn clone_flags(parent: &Stopped, op: safeptrace::ChildOp) -> u64 {
    const CONSERVATIVE: u64 = libc::CLONE_VM as u64;
    let Ok(regs) = parent.getregs() else {
        return CONSERVATIVE;
    };
    match Sysno::from(regs.orig_rax as i32) {
        Sysno::clone => regs.rdi,
        Sysno::clone3 => {
            use std::os::unix::fs::FileExt;
            let mut flags = [0u8; 8];
            match std::fs::File::open(format!("/proc/{}/mem", parent.pid()))
                .and_then(|mem| mem.read_exact_at(&mut flags, regs.rdi))
            {
                Ok(()) => u64::from_ne_bytes(flags),
                Err(_) => CONSERVATIVE,
            }
        }
        Sysno::fork => 0,
        Sysno::vfork => (libc::CLONE_VM | libc::CLONE_VFORK) as u64,
        _ if op == safeptrace::ChildOp::Vfork => (libc::CLONE_VM | libc::CLONE_VFORK) as u64,
        _ => CONSERVATIVE,
    }
}

/// Off x86_64 no task carries trap-only state (see the fail-closed entry
/// points after the main `impl`), so this only has to keep the conservative
/// answer.
#[cfg(not(target_arch = "x86_64"))]
fn clone_flags(_parent: &Stopped, _op: safeptrace::ChildOp) -> u64 {
    libc::CLONE_VM as u64
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
            return Ok(TrapOnlyRoute::Ordinary {
                task,
                patch_site: site,
            });
        }
        let trap_only = self.trap_only.as_ref().expect("trap-only routing");
        let live = !is_reserved_site(site) && trap_only.lock().is_live(site);
        if !live {
            return self
                .trap_only_foreign_i386(task, regs)
                .await
                .map(TrapOnlyRoute::Done);
        }
        // H0: the registers the original `syscall` would have produced.
        regs.rcx = regs.rip;
        regs.r11 = regs.eflags;
        regs.orig_rax = regs.orig_rax as u32 as i32 as i64 as u64;
        task.setregs(&regs)?;
        let nr = Sysno::from(regs.orig_rax as i32);
        let subscribed = self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == nr);
        if !subscribed || nr == Sysno::rt_sigreturn {
            // Never resume an I386 stop with a live number: fail closed.
            return Err(self.trap_only_failure(
                "trap-only seccomp routing",
                TrapOnlyFailure::AllowClassUnsupported {
                    site,
                    nr: regs.orig_rax as i64,
                },
            ));
        }
        self.trap_only
            .as_mut()
            .expect("trap-only routing")
            .live_entry = Some(regs);
        Ok(TrapOnlyRoute::Patched(task))
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

    /// Runs a live patched site's syscall in place through the slot (H1-H4).
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
        // H1: block everything, skip the int 0x80, and run `syscall` at SLOT.
        let saved_mask = task.getsigmask()?;
        task.setsigmask(!0)?;
        let mut regs = view;
        regs.orig_rax = -1i64 as u64;
        regs.rax = nr;
        regs.rip = SLOT;
        task.setregs(&regs)?;
        let wait = self.resume_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);

        // H2: the slot's own seccomp stop.
        let task = match wait {
            Wait::Stopped(task, Event::Seccomp) => {
                let tag = task.getevent()?;
                let regs = task.getregs()?;
                if tag != TAG_SLOT as i64 || regs.rip != SLOT_RET || regs.orig_rax != nr {
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
                if regs.rip == SLOT_RET {
                    regs.rip = view.rip;
                    regs.rcx = view.rcx;
                    regs.r11 = view.r11;
                    task.setregs(&regs)?;
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
                let _ = self
                    .dispatch_new_task(op, parent, child, Some(view), None)
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

    /// Trap-only bookkeeping at a new-child event stop, before either task
    /// runs: decide whether the child shares the site table, and restore every
    /// site before a second task can execute the address space.
    ///
    /// Both tasks are stopped here, and an auto-attached child executes no
    /// user code before its first stop, so the text writes cannot race.
    pub(super) fn trap_only_new_child(
        &self,
        parent: &Stopped,
        op: safeptrace::ChildOp,
    ) -> Result<Option<crate::liteinst_trap_only::TrapOnlyTask>, TraceError> {
        let Some(trap_only) = self.trap_only.as_ref() else {
            return Ok(None);
        };
        let flags = clone_flags(parent, op);
        let shares_vm = flags & libc::CLONE_VM as u64 != 0;
        if shares_vm && flags & libc::CLONE_VFORK as u64 == 0 {
            trap_only
                .retire_all(
                    nix_pid(parent.pid()),
                    RetiredReason::MultiTask,
                    DisabledReason::MultiTask,
                )
                .map_err(|error| self.trap_only_fail("trap-only multi-task restore", error))?;
        }
        Ok(Some(trap_only.child(shares_vm)))
    }

    /// Gives an exec'ing task an empty table and drops its hop state.
    pub(super) fn trap_only_exec(&mut self, initial_command: bool) {
        let Some(trap_only) = self.trap_only.as_mut() else {
            return;
        };
        trap_only.live_entry = None;
        trap_only.new_child_view = None;
        let mut fresh = SiteTable::new(trap_only.lock().patching());
        if !trap_only.shared.full_subscription {
            fresh.disable(DisabledReason::PartialSubscription);
        }
        if initial_command {
            // The launch exec: keep the table the tracer handle observes.
            *trap_only.lock() = fresh;
        } else {
            trap_only.sites = std::sync::Arc::new(std::sync::Mutex::new(fresh));
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
