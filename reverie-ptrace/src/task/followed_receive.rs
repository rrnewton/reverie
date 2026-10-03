/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Opt-in original receive callback across a real, bounded private timer.
//! An EXIT restoration receipt never recreates the consumed SECCOMP entry.
//! AUTONOMOUS-BOT-IMPLEMENTED; TODO-HUMAN-REVIEW:
//! https://github.com/rrnewton/reverie/pull/897

use reverie::Stack;

use super::*;

struct TimerAttempt {
    session: Arc<FatalSession>,
    origin: BackendFailure,
    completed: bool,
}
impl Drop for TimerAttempt {
    fn drop(&mut self) {
        if !self.completed {
            self.session.fail_at(
                self.origin,
                anyhow::anyhow!(
                    "retained receive timer abandoned before authenticated EXIT/restoration"
                )
                .into(),
            );
        }
    }
}

pub(super) struct Context {
    pub(super) entry: original_context::OriginalReadEntry,
    call: (Sysno, SyscallArgs),
    expected: libc::user_regs_struct,
    restored: Option<source_cohort::RestoredNativeContext>,
    invalid: bool,
}

impl Context {
    pub(super) fn unfinished(&self) -> bool {
        self.invalid
            || self
                .restored
                .as_ref()
                .is_none_or(|receipt| !receipt.receive_timer_published())
    }

    pub(super) fn invalidate(&mut self) {
        self.invalid = true;
        self.restored = None;
    }

    fn validate(&self, task: &Stopped, call: (Sysno, SyscallArgs)) -> Result<(), Errno> {
        let checked = || {
            if self.invalid || call != self.call {
                return Err(Errno::ESTALE);
            }
            self.entry.retained_store_state()?;
            self.restored
                .as_ref()
                .ok_or(Errno::ESTALE)?
                .validate(task)?;
            let actual =
                original_context::checked_entry_registers(task).map_err(|_| Errno::ESTALE)?;
            if !safeptrace::ControlStop::registers_equal(&actual, &self.expected) {
                return Err(Errno::ESTALE);
            }
            Ok(())
        };
        let result = checked();
        if result.is_err() {
            self.entry.retain_store_failure();
        }
        result
    }
}

// Only this module constructs these borrows, after authenticating the real
// original callback and retaining the same FatalTaskStop scratch owner.
pub(super) struct TimerOrigin<'a> {
    logical: &'a private_signal::Logical,
    original: (Sysno, SyscallArgs),
    scratch: &'a Arc<crate::tracer::ReceiveTimerScratch>,
}
impl TimerOrigin<'_> {
    pub(super) fn validate(&self, stopped: &Stopped) -> Result<(), Errno> {
        if !self.logical.matches_original(stopped, self.original) {
            return Err(Errno::ESTALE);
        }
        let context = self.logical.receive.as_ref().ok_or(Errno::ESTALE)?;
        if !context.invalid || context.restored.is_some() || context.call != self.original {
            return Err(Errno::ESTALE);
        }
        context.entry.retained_store_state()
    }
    pub(super) fn original(&self) -> (Sysno, SyscallArgs) {
        self.original
    }
    pub(super) fn scratch(&self) -> Arc<crate::tracer::ReceiveTimerScratch> {
        Arc::clone(self.scratch)
    }
}

// Constructed only after the exact scratch retirement succeeded. The borrowed
// context must contain this exact receipt and still validate its entire frame.
pub(super) struct TimerPublication<'a> {
    task: &'a Stopped,
    context: &'a Context,
    original: (Sysno, SyscallArgs),
    scratch: &'a Arc<crate::tracer::ReceiveTimerScratch>,
}
impl TimerPublication<'_> {
    pub(super) fn validate(
        &self,
        receipt: &source_cohort::RestoredNativeContext,
    ) -> Result<(), Errno> {
        if !self
            .context
            .restored
            .as_ref()
            .is_some_and(|current| std::ptr::eq(current, receipt))
        {
            return Err(Errno::ESTALE);
        }
        self.context.validate(self.task, self.original)
    }
    pub(super) fn original(&self) -> (Sysno, SyscallArgs) {
        self.original
    }
    pub(super) fn scratch(&self) -> &Arc<crate::tracer::ReceiveTimerScratch> {
        self.scratch
    }
}

impl<L: Tool + 'static> TracedTask<L> {
    async fn skip_followed_receive_entry(
        &mut self,
        task: Stopped,
        original: libc::user_regs_struct,
    ) -> Result<Stopped, TraceError> {
        let generation = task.terminal_cleanup();
        let mut skipped = original;
        skipped.orig_rax = u64::MAX;
        task.setregs(&skipped)?;
        let wait = self.syscall_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        let task = match wait {
            Wait::Stopped(task, Event::Syscall) => task,
            other => self.abort(Ok(other)).await,
        };
        #[cfg(all(test, cohort_final_test))]
        test_mutate(1, &task)?;
        if !generation.same_generation(&task.terminal_cleanup())
            || task.syscall_exit_result()? != -(libc::ENOSYS as i64)
        {
            return Err(Errno::EPROTO.into());
        }
        let actual = original_context::checked_entry_registers(&task).map_err(|_| Errno::ESTALE)?;
        skipped.rax = -(libc::ENOSYS as i64) as u64;
        if !safeptrace::ControlStop::registers_equal(&actual, &skipped) {
            return Err(Errno::EPROTO.into());
        }
        task.setregs(&original)?;
        Ok(task)
    }

    fn followed_receive_context(&self) -> Result<&Context, Errno> {
        self.private_signal
            .logical
            .as_ref()
            .and_then(|logical| logical.receive.as_ref())
            .ok_or(Errno::ESTALE)
    }

    pub(super) fn validate_restored_receive(&self, original: Syscall) -> Result<(), Errno> {
        if self.pending_syscall.is_some()
            || self.injected_syscall_frame.is_some()
            || self.pending_syscall_already_skipped
            || self.interrupted_read.is_some()
            || self.pending_signal.is_some()
            || self.private_signal.consulting
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
            || self.private_signal.completing.is_some()
        {
            return Err(Errno::ESTALE);
        }
        let task = self.assume_stopped();
        let logical = self.private_signal.logical.as_ref().ok_or(Errno::ESTALE)?;
        if !logical.matches_original(&task, original.into_parts()) {
            return Err(Errno::ESTALE);
        }
        self.followed_receive_context()?
            .validate(&task, original.into_parts())
    }

    pub(super) fn restored_receive_entry(
        &self,
    ) -> Result<&original_context::OriginalReadEntry, Errno> {
        Ok(&self.followed_receive_context()?.entry)
    }

    /// Explicit origin, not inference from a caller's arbitrary Ppoll shape.
    /// This first checkpoint retains the original receive and authenticates
    /// restoration. Joining another task's timer is a separate backend action.
    pub(super) async fn run_followed_receive_timer(
        &mut self,
        original: Syscall,
        duration: std::time::Duration,
    ) -> Result<(), TraceError> {
        if duration.is_zero() || duration > std::time::Duration::from_millis(1) {
            return Err(Errno::EINVAL.into());
        }
        let session = Arc::clone(&self.global_state.fatal_session);
        if self
            .private_signal
            .logical
            .as_ref()
            .is_some_and(|logical| logical.timer_join_pending)
            || !session.source_jobs.enabled()
            || session.is_failed()
            || !session.source_jobs.idle()
            || self.cancel_handler.load(Ordering::Acquire)
            || self.injected_syscall_frame.is_some()
            || self.pending_syscall_already_skipped
            || self.interrupted_read.is_some()
            || self.pending_signal.is_some()
            || self.private_signal.consulting
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
            || self.private_signal.completing.is_some()
        {
            return Err(Errno::EBUSY.into());
        }
        let member = self.cohort.as_ref().ok_or(Errno::EOPNOTSUPP)?.clone();
        let call = original.into_parts();
        let mut task = self.assume_stopped();
        let logical = self.private_signal.logical.as_ref().ok_or(Errno::ESTALE)?;
        if !logical.matches_original(&task, call) {
            return Err(Errno::ESTALE.into());
        }
        let first = logical.receive.is_none();
        if first {
            // The original inspector remains literal: this is still its real
            // unconsumed entry. Moving that owner preserves failure/once bits.
            if self
                .inspect_native_scalar_receive_range(call.0, call.1)
                .map_err(|_| Errno::ESTALE)?
                != reverie::OriginalReadRangeVerdict::Allowed
            {
                return Err(Errno::EOPNOTSUPP.into());
            }
            let actual =
                original_context::checked_entry_registers(&task).map_err(|_| Errno::ESTALE)?;
            if !logical.matches_frame(&actual) {
                return Err(Errno::ESTALE.into());
            }
            self.original_scalar_store_unused()?;
            let entry = self
                .original_read_entry
                .take()
                .ok_or(Errno::ESTALE)?
                .map_err(|_| Errno::ESTALE)?;
            self.private_signal
                .logical
                .as_mut()
                .ok_or(Errno::ESTALE)?
                .receive = Some(Context {
                entry,
                call,
                expected: actual,
                restored: None,
                invalid: false,
            });
        } else {
            self.validate_restored_receive(original)?;
        }
        // Consume the old eligibility BEFORE any mutation/await. A lost helper
        // future leaves this callback closed; it never recreates a receipt.
        let context = self
            .private_signal
            .logical
            .as_mut()
            .ok_or(Errno::ESTALE)?
            .receive
            .as_mut()
            .ok_or(Errno::ESTALE)?;
        context.invalid = true;
        context.restored = None;
        let original_frame = context.expected;
        let mut attempt = TimerAttempt {
            session: Arc::clone(&session),
            origin: BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase: "retained receive timer cancellation",
            },
            completed: false,
        };
        if first {
            if self.pending_syscall.take() != Some(call) {
                return Err(Errno::ESTALE.into());
            }
            task = self
                .skip_followed_receive_entry(task, original_frame)
                .await?;
        }
        #[cfg(all(test, cohort_final_test))]
        test_mutate(2, &task)?;
        let before = original_context::checked_entry_registers(&task).map_err(|_| Errno::ESTALE)?;
        // The finite timer profile excludes TF/RF. Native SYSCALL clears RF
        // and ptrace can hide forced TF; neither may be silently normalized.
        if before.eflags & ((1 << 8) | (1 << 16)) != 0
            || !safeptrace::ControlStop::registers_equal(&before, &original_frame)
        {
            return Err(Errno::EPROTO.into());
        }
        let mut stack = GuestStack::new(self.tid, self.stack_checked_out.clone())?;
        let timeout = stack.push(reverie::syscalls::Timespec {
            tv_sec: 0,
            tv_nsec: duration.subsec_nanos() as libc::c_long,
        });
        let guard = stack.commit()?;
        let scratch_owner = session
            .tree
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|owner| {
                owner.tid == task.pid() && owner.terminal.same_generation(&task.terminal_cleanup())
            })
            .cloned()
            .ok_or(Errno::ESTALE)?;
        let scratch = scratch_owner.bind_receive_scratch(&task, guard)?;
        let timer = reverie::syscalls::Ppoll::new()
            .with_fds(None)
            .with_nfds(0)
            .with_timeout(AddrMut::from_raw(timeout.as_raw()))
            .with_sigmask(None)
            .with_sigsetsize(0);
        let (nr, args) = timer.into_parts();
        let mut entered = before;
        *entered.syscall_mut() = nr as u64;
        *entered.orig_syscall_mut() = nr as u64;
        entered.set_args((
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ));
        *entered.ip_mut() = cp::PRIVATE_PAGE_OFFSET as u64;
        let mut stub = [0u8; 4];
        task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut stub)?;
        if stub != [0x0f, 0x05, 0x0f, 0x0b] {
            return Err(Errno::EPROTO.into());
        }
        let observe = L::observe_injected_syscalls(&self.global_state.cfg);
        self.observe_injected_syscall(
            (observe && L::observe_injected_syscall_preparation(&self.global_state.cfg))
                .then_some((nr, args)),
            InjectedSyscallEvent::Prepared,
        );
        session.source_epoch.observe(nr, args);
        task.setregs(&entered)?;
        let wait = self.syscall_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        let task = match wait {
            Wait::Stopped(task, Event::Syscall) => task,
            other => self.abort(Ok(other)).await,
        };
        #[cfg(all(test, cohort_final_test))]
        test_mutate(3, &task)?;
        original_context::check_entry(
            &task,
            nr,
            args,
            (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
            entered.stack_ptr(),
            false,
        )?;
        let actual_entry =
            original_context::checked_entry_registers(&task).map_err(|_| Errno::ESTALE)?;
        // Native SYSCALL defines only these entry clobbers. No unrelated
        // register mutation can be overwritten and called restoration.
        entered.rax = -(libc::ENOSYS as i64) as u64;
        entered.rip += cp::SYSCALL_INSTR_SIZE as u64;
        entered.rcx = entered.rip;
        entered.r11 = entered.eflags;
        if !safeptrace::ControlStop::registers_equal(&actual_entry, &entered) {
            return Err(Errno::EPROTO.into());
        }
        let native = member.native_receive_timer(
            &task,
            TimerOrigin {
                logical: self.private_signal.logical.as_ref().ok_or(Errno::ESTALE)?,
                original: call,
                scratch: &scratch,
            },
            (nr, args),
            &entered,
        )?;
        self.observe_injected_syscall(observe.then_some((nr, args)), InjectedSyscallEvent::Entered);
        let wait = self.syscall_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        let task = match wait {
            Wait::Stopped(task, Event::Syscall) => task,
            other => self.abort(Ok(other)).await,
        };
        #[cfg(all(test, cohort_final_test))]
        test_mutate(4, &task)?;
        let raw = task.syscall_exit_result()?;
        let actual = original_context::checked_entry_registers(&task).map_err(|_| Errno::ESTALE)?;
        let mut expected = entered;
        expected.rax = raw as u64;
        if raw != 0 || !safeptrace::ControlStop::registers_equal(&actual, &expected) {
            return Err(Errno::EPROTO.into());
        }
        let receipt = native.syscall_return(&task, raw).ok_or(Errno::ESTALE)?;
        self.observe_injected_syscall(
            observe.then_some((nr, args)),
            InjectedSyscallEvent::Returned(raw),
        );
        #[cfg(all(test, cohort_final_test))]
        test_mutate(5, &task)?;
        let desired = restored_context_registers(actual, original_frame, None, false);
        let restored = receipt.restore_checked(&task, &actual, &desired)?;
        scratch_owner.retire_receive_scratch(&scratch, &task, &restored)?;
        let context = self
            .private_signal
            .logical
            .as_mut()
            .ok_or(Errno::ESTALE)?
            .receive
            .as_mut()
            .ok_or(Errno::ESTALE)?;
        context.expected = desired;
        context.restored = Some(restored);
        context.invalid = false;
        self.validate_restored_receive(original)?;
        #[cfg(all(test, cohort_final_test))]
        {
            assert!(self.followed_receive_context()?.unfinished());
            source_cohort::timer_join_tests::pause_publication(&task).await;
        }
        let context = self.followed_receive_context()?;
        context
            .restored
            .as_ref()
            .ok_or(Errno::ESTALE)?
            .publish_receive_timer(TimerPublication {
                task: &task,
                context,
                original: call,
                scratch: &scratch,
            })?;
        #[cfg(all(test, cohort_final_test))]
        assert!(!self.followed_receive_context()?.unfinished());
        attempt.completed = true;
        Ok(())
    }
}

#[cfg(all(test, cohort_final_test))]
std::thread_local! {
    static TEST_MUTATION: std::cell::RefCell<Option<(u8, usize)>> = const { std::cell::RefCell::new(None) };
    static TEST_MUTATION_HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(all(test, cohort_final_test))]
pub(super) fn test_mutation(stage: u8, field: usize) {
    TEST_MUTATION.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some((stage, field));
    });
    TEST_MUTATION_HITS.with(|hits| hits.set(0));
}
#[cfg(all(test, cohort_final_test))]
pub(super) fn test_mutation_finish() -> usize {
    TEST_MUTATION.with(|slot| *slot.borrow_mut() = None);
    TEST_MUTATION_HITS.with(|hits| hits.replace(0))
}
#[cfg(all(test, cohort_final_test))]
fn test_mutate(stage: u8, task: &Stopped) -> Result<(), TraceError> {
    let selected = TEST_MUTATION.with(|slot| {
        let mut mutation = slot.borrow_mut();
        if mutation.is_some_and(|(selected, _)| selected == stage) {
            mutation.take()
        } else {
            None
        }
    });
    if let Some((_, field)) = selected {
        let mut r = task.getregs()?;
        let changed = match field {
            0 => &mut r.r15,
            1 => &mut r.r14,
            2 => &mut r.r13,
            3 => &mut r.r12,
            4 => &mut r.rbp,
            5 => &mut r.rbx,
            6 => &mut r.r11,
            7 => &mut r.r10,
            8 => &mut r.r9,
            9 => &mut r.r8,
            10 => &mut r.rax,
            11 => &mut r.rcx,
            12 => &mut r.rdx,
            13 => &mut r.rsi,
            14 => &mut r.rdi,
            15 => &mut r.orig_rax,
            16 => &mut r.rip,
            17 => &mut r.cs,
            18 => &mut r.eflags,
            19 => &mut r.rsp,
            20 => &mut r.ss,
            21 => &mut r.fs_base,
            22 => &mut r.gs_base,
            23 => &mut r.ds,
            24 => &mut r.es,
            25 => &mut r.fs,
            26 => &mut r.gs,
            _ => panic!("invalid controlled field"),
        };
        *changed ^= 1;
        task.setregs(&r)?;
        TEST_MUTATION_HITS.with(|hits| hits.set(hits.get() + 1));
    }
    Ok(())
}
