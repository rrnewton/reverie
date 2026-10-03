/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Separate original Poll callback across a real bounded private timer.
//! Its raw zero supplies no readiness or guest deadline result.
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
                    "retained poll timer abandoned before authenticated EXIT/restoration"
                )
                .into(),
            );
        }
    }
}

pub(super) struct Context {
    pub(super) entry: original_poll::OriginalPollEntry,
    call: (Sysno, SyscallArgs),
    expected: libc::user_regs_struct,
    restored: Option<source_cohort::RestoredNativeContext>,
    invalid: bool,
    pub(super) skipped: bool,
}

impl Context {
    pub(super) fn unfinished(&self) -> bool {
        self.invalid
            || self.entry.state().is_err()
            || self.skipped
                && self
                    .restored
                    .as_ref()
                    .is_none_or(|receipt| !receipt.poll_timer_published())
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
            self.entry.validate(task, call)?;
            if self.skipped {
                self.restored
                    .as_ref()
                    .ok_or(Errno::ESTALE)?
                    .validate(task)?;
            }
            let actual =
                original_context::checked_entry_registers(task).map_err(|_| Errno::ESTALE)?;
            if !safeptrace::ControlStop::registers_equal(&actual, &self.expected) {
                return Err(Errno::ESTALE);
            }
            Ok(())
        };
        let result = checked();
        if result.is_err() {
            self.entry.fail();
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
        let context = self.logical.poll.as_ref().ok_or(Errno::ESTALE)?;
        if !context.invalid || context.restored.is_some() || context.call != self.original {
            return Err(Errno::ESTALE);
        }
        context.entry.state()
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
    async fn skip_followed_poll_entry(
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

    fn followed_poll_context(&self) -> Result<&Context, Errno> {
        self.private_signal
            .logical
            .as_ref()
            .and_then(|logical| logical.poll.as_ref())
            .ok_or(Errno::ESTALE)
    }

    pub(super) fn validate_followed_poll(&self, original: Syscall) -> Result<(), Errno> {
        let context = self.followed_poll_context()?;
        // A different requested API tuple has no authority over this context.
        // Once the request names the retained original, every actual-context
        // failure is sticky even if a Tool catches it and restores registers.
        if original.into_parts() != context.call {
            return Err(Errno::ESTALE);
        }
        let checked = (|| {
            if self.injected_syscall_frame.is_some()
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
            if context.skipped {
                if self.pending_syscall.is_some() {
                    return Err(Errno::ESTALE);
                }
            } else {
                if self.pending_syscall != Some(original.into_parts()) {
                    return Err(Errno::ESTALE);
                }
                original_context::check_entry(
                    &task,
                    context.entry.call.0,
                    context.entry.call.1,
                    context.expected.rip,
                    context.expected.rsp,
                    true,
                )
                .map_err(|_| Errno::ESTALE)?;
            }
            context.validate(&task, original.into_parts())
        })();
        if checked.is_err() {
            context.entry.fail();
        }
        checked
    }

    /// Explicit origin, not inference from a caller's arbitrary Ppoll shape.
    /// This first checkpoint retains the original poll and authenticates
    /// restoration. Joining another task's timer is a separate backend action.
    pub(super) async fn run_followed_poll_timer(
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
        let context = logical.poll.as_ref().ok_or(Errno::ESTALE)?;
        let first = !context.skipped;
        if context.entry.input()?.timeout_millis == 0 {
            return Err(Errno::EOPNOTSUPP.into());
        }
        context.entry.unused()?;
        self.validate_followed_poll(original)?;
        // The original row was copied once under the complete held cohort.
        // This helper touches only its own retained Timespec, not the Poll row.
        // Requiring another whole hold here would prohibit a second genuine
        // peer timer while the first executes. Final output revalidates the
        // original unchanged inputs under a fresh complete hold.
        // Consume the old eligibility BEFORE any mutation/await. A lost helper
        // future leaves this callback closed; it never recreates a receipt.
        let context = self
            .private_signal
            .logical
            .as_mut()
            .ok_or(Errno::ESTALE)?
            .poll
            .as_mut()
            .ok_or(Errno::ESTALE)?;
        context.invalid = true;
        context.skipped = true;
        context.restored = None;
        let original_frame = context.expected;
        let mut attempt = TimerAttempt {
            session: Arc::clone(&session),
            origin: BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase: "retained poll timer cancellation",
            },
            completed: false,
        };
        if first {
            if self.pending_syscall.take() != Some(call) {
                return Err(Errno::ESTALE.into());
            }
            task = self.skip_followed_poll_entry(task, original_frame).await?;
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
        let native = member.native_poll_timer(
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
            .poll
            .as_mut()
            .ok_or(Errno::ESTALE)?;
        context.expected = desired;
        context.restored = Some(restored);
        context.invalid = false;
        self.validate_followed_poll(original)?;
        #[cfg(all(test, cohort_final_test))]
        source_cohort::original_poll_join_tests::pause_publication(&task).await;
        let context = self.followed_poll_context()?;
        context
            .restored
            .as_ref()
            .ok_or(Errno::ESTALE)?
            .publish_poll_timer(TimerPublication {
                task: &task,
                context,
                original: call,
                scratch: &scratch,
            })?;
        attempt.completed = true;
        Ok(())
    }
}

use reverie::syscalls::FollowedPollStore;
use reverie::syscalls::NativeUserReadError as ReadError;
use reverie::syscalls::NativeUserReadRefusal as ReadRefusal;
use reverie::syscalls::NativeUserStoreOutcome as StoreOutcome;
use reverie::syscalls::NativeUserStoreRefusal as StoreRefusal;
use reverie::syscalls::OriginalPollInput;

fn refused(error: Errno) -> ReadError {
    ReadError::Refused(ReadRefusal::TargetState(error))
}
fn store_refused(error: Errno) -> StoreRefusal {
    StoreRefusal::Evidence(ReadRefusal::TargetState(error))
}

impl<L: Tool + 'static> TracedTask<L> {
    pub(super) async fn capture_followed_poll(
        &mut self,
        original: Syscall,
        retention: Box<dyn Send + Sync>,
    ) -> Result<OriginalPollInput, ReadError> {
        let session = Arc::clone(&self.global_state.fatal_session);
        let call = original.into_parts();
        if call.0 != Sysno::poll || call.1.arg1 as u32 != 1 || (call.1.arg2 as i32) < 0 {
            return Err(ReadError::Refused(ReadRefusal::UnsupportedRange));
        }
        if !session.source_jobs.enabled()
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
            return Err(refused(Errno::EBUSY));
        }
        let task = self.assume_stopped();
        let logical = self
            .private_signal
            .logical
            .as_ref()
            .ok_or_else(|| refused(Errno::ESTALE))?;
        if logical.timer_join_pending
            || logical.poll.is_some()
            || logical.receive.is_some()
            || self.pending_syscall != Some(call)
            || !logical.matches_original(&task, call)
        {
            return Err(refused(Errno::ESTALE));
        }
        let regs =
            original_context::checked_entry_registers(&task).map_err(|_| refused(Errno::ESTALE))?;
        if !logical.matches_frame(&regs) {
            return Err(refused(Errno::ESTALE));
        }
        let member = self
            .cohort
            .as_ref()
            .ok_or(ReadError::Refused(ReadRefusal::UnsupportedBackend))?;
        let hold = Arc::new(member.acquire().map_err(refused)?);
        // Linux checks the current original task's limit BEFORE input copy.
        // A denied/failed query remains backend refusal, never guest EFAULT.
        let limit = {
            let sender = hold.sender();
            let permit = sender.begin_native_store().map_err(refused)?;
            let limit = permit.read_nofile_limit().map_err(refused)?;
            if limit.0 < 1 {
                return Err(ReadError::Refused(ReadRefusal::UnsupportedRange));
            }
            limit
        };
        let entry =
            original_poll::OriginalPollEntry::new(&task, call, &regs, limit).map_err(refused)?;
        let plan = safeptrace::FollowedSourceReadPlan::prepare(hold.sender(), call.1.arg0, 8)?;
        self.private_signal
            .logical
            .as_mut()
            .ok_or_else(|| refused(Errno::ESTALE))?
            .poll = Some(Context {
            entry,
            call,
            expected: regs,
            restored: None,
            invalid: true,
            skipped: false,
        });
        let mut attempt = TimerAttempt {
            session: Arc::clone(&session),
            origin: BackendFailure {
                pid: self.pid(),
                tid: self.tid(),
                phase: "original Poll input capture",
            },
            completed: false,
        };
        let observer =
            session
                .source_jobs
                .submit_followed(retention, Arc::clone(&hold), move || plan.run())?;
        let bytes = observer.await?;
        hold.validate().map_err(refused)?;
        if session.is_failed() || self.cancel_handler.load(Ordering::Acquire) {
            return Err(refused(Errno::ECANCELED));
        }
        let context = self
            .private_signal
            .logical
            .as_mut()
            .ok_or_else(|| refused(Errno::ESTALE))?
            .poll
            .as_mut()
            .ok_or_else(|| refused(Errno::ESTALE))?;
        context.entry.captured(&bytes).map_err(refused)?;
        context.invalid = false;
        self.validate_followed_poll(original).map_err(refused)?;
        let input = self
            .followed_poll_context()
            .map_err(refused)?
            .entry
            .input()
            .map_err(refused)?;
        attempt.completed = true;
        Ok(input)
    }

    pub(super) fn with_native_followed_poll_store<R>(
        &self,
        original: Syscall,
        action: impl FnOnce(&mut dyn FollowedPollStore) -> R,
    ) -> Result<R, StoreRefusal> {
        let session = &self.global_state.fatal_session;
        let alive = || {
            if !session.source_jobs.enabled()
                || session.is_failed()
                || self.cancel_handler.load(Ordering::Acquire)
            {
                Err(Errno::ECANCELED)
            } else if !session.source_jobs.idle() {
                Err(Errno::EBUSY)
            } else {
                Ok(())
            }
        };
        alive().map_err(store_refused)?;
        self.validate_followed_poll(original)
            .map_err(store_refused)?;
        let context = self.followed_poll_context().map_err(store_refused)?;
        context.entry.unused().map_err(store_refused)?;
        let input = context.entry.input().map_err(store_refused)?;
        let member = self
            .cohort
            .as_ref()
            .ok_or(StoreRefusal::Evidence(ReadRefusal::UnsupportedBackend))?;
        let hold = member.acquire().map_err(store_refused)?;
        let sender = hold.sender();
        let permit = sender.begin_native_store().map_err(store_refused)?;
        let check = || {
            let result = (|| {
                alive().map_err(store_refused)?;
                hold.validate().map_err(store_refused)?;
                self.validate_followed_poll(original)
                    .map_err(store_refused)?;
                if permit.read_nofile_limit().map_err(store_refused)? != context.entry.limit {
                    return Err(store_refused(Errno::ESTALE));
                }
                let row = permit.read_poll_row(context.call.1.arg0)?;
                if original_poll::OriginalPollEntry::decode(&row, context.call.1.arg2 as i32)
                    .map_err(store_refused)?
                    != input
                {
                    return Err(store_refused(Errno::ESTALE));
                }
                permit.validate().map_err(store_refused)
            })();
            if result.is_err() {
                context.entry.fail();
            }
            result
        };
        check()?;
        let mut writer = PollWriter {
            permit: &permit,
            check: &check,
            entry: &context.entry,
            input,
            used: false,
        };
        Ok(action(&mut writer))
    }
}
struct PollWriter<'a, 'hold> {
    permit: &'a safeptrace::NativeStorePermit<'hold>,
    check: &'a dyn Fn() -> Result<(), StoreRefusal>,
    entry: &'a original_poll::OriginalPollEntry,
    input: OriginalPollInput,
    used: bool,
}
impl FollowedPollStore for PollWriter<'_, '_> {
    fn input(&self) -> OriginalPollInput {
        self.input
    }
    fn validate_context(&self) -> Result<(), StoreRefusal> {
        if self.used {
            return Err(store_refused(Errno::EALREADY));
        }
        self.entry.unused().map_err(store_refused)?;
        (self.check)()
    }
    fn store_revents(&mut self, revents: i16) -> StoreOutcome {
        if std::mem::replace(&mut self.used, true) {
            return StoreOutcome::Refused(store_refused(Errno::EALREADY));
        }
        if revents & !(libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
            return StoreOutcome::Refused(StoreRefusal::Evidence(ReadRefusal::UnsupportedRange));
        }
        if let Err(error) = (self.check)() {
            return StoreOutcome::Refused(error);
        }
        if let Err(error) = self.entry.claim() {
            return StoreOutcome::Refused(store_refused(error));
        }
        match self
            .permit
            .write(self.entry.call.1.arg0 + 6, &revents.to_ne_bytes())
        {
            StoreOutcome::Refused(error) => StoreOutcome::Refused(error),
            StoreOutcome::Attempted { raw, postcheck } => {
                #[cfg(all(test, cohort_final_test))]
                AFTER_POLL_STORE.with(|slot| {
                    if let Some(hook) = slot.borrow_mut().take() {
                        hook();
                    }
                });
                finish_poll_store(
                    raw,
                    postcheck,
                    || {
                        (self.check)()
                            .map_err(|_| Errno::ESTALE)
                            .and_then(|_| self.permit.validate())
                    },
                    || self.entry.fail(),
                )
            }
        }
    }
}

#[cfg(all(test, cohort_final_test))]
std::thread_local! {
    static AFTER_POLL_STORE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(all(test, cohort_final_test))]
pub(super) fn test_after_store(hook: impl FnOnce() + 'static) {
    AFTER_POLL_STORE.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
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

// A physical postcheck can fail before the Poll row/context closure runs. Its
// loss of authority must independently poison callback completion, while the
// raw Linux transfer result remains available to the consuming caller.
fn finish_poll_store(
    raw: Result<usize, Errno>,
    physical: Result<(), Errno>,
    check: impl FnOnce() -> Result<(), Errno>,
    fail: impl FnOnce(),
) -> StoreOutcome {
    let postcheck = physical.and_then(|_| check());
    if postcheck.is_err() {
        fail();
    }
    StoreOutcome::Attempted { raw, postcheck }
}

#[cfg(test)]
mod poll_store_outcome_tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn physical_failure_poisons_before_row_check_without_rewriting_raw() {
        for raw in [Ok(2), Ok(1), Err(Errno::EFAULT)] {
            let checked = Cell::new(false);
            let failed = Cell::new(false);
            let outcome = finish_poll_store(
                raw,
                Err(Errno::ESTALE),
                || {
                    checked.set(true);
                    Ok(())
                },
                || failed.set(true),
            );
            assert_eq!(
                outcome,
                StoreOutcome::Attempted {
                    raw,
                    postcheck: Err(Errno::ESTALE)
                }
            );
            assert!(!checked.get());
            assert!(failed.get());
        }
    }

    #[test]
    fn positive_authority_preserves_caller_owned_partial_and_errno() {
        for raw in [Ok(2), Ok(1), Err(Errno::EFAULT)] {
            let checked = Cell::new(false);
            let failed = Cell::new(false);
            let outcome = finish_poll_store(
                raw,
                Ok(()),
                || {
                    checked.set(true);
                    Ok(())
                },
                || failed.set(true),
            );
            assert_eq!(
                outcome,
                StoreOutcome::Attempted {
                    raw,
                    postcheck: Ok(())
                }
            );
            assert!(checked.get());
            assert!(!failed.get());
        }
    }

    #[test]
    fn later_row_failure_also_poisons_without_rewriting_raw() {
        for raw in [Ok(2), Ok(1), Err(Errno::EFAULT)] {
            let failed = Cell::new(false);
            let outcome = finish_poll_store(raw, Ok(()), || Err(Errno::EBUSY), || failed.set(true));
            assert_eq!(
                outcome,
                StoreOutcome::Attempted {
                    raw,
                    postcheck: Err(Errno::EBUSY)
                }
            );
            assert!(failed.get());
        }
    }
}
