//! One original scalar Read through actual ENTRY/EXIT stops. The existing
//! Tool callback, task, provider Call, signal dispatcher and timer own progress.
use reverie::InjectedReadResult;
use reverie::InterruptedSyscall;

use super::*;

pub(super) struct InterruptedRead {
    ticket: InterruptedSyscall,
    stop: Stopped,
    signal: Signal,
    context: libc::user_regs_struct,
    injected_frame: Option<usize>,
    ready: bool,
    completed: Option<i64>,
}

use super::original_context::check_entry;

fn check_stub(task: &Stopped) -> Result<(), TraceError> {
    let mut bytes = [0; 4];
    task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut bytes)?;
    if bytes != [0x0f, 0x05, 0x0f, 0x0b] {
        return Err(Errno::EPROTO.into());
    }
    Ok(())
}

impl<L: Tool + 'static> TracedTask<L> {
    pub(super) async fn inject_read_boundaries(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<InjectedReadResult, TraceError> {
        if nr != Sysno::read || self.interrupted_read.is_some() || self.pending_signal.is_some() {
            return Err(Errno::EPROTO.into());
        }
        let observe = L::observe_injected_syscalls(&self.global_state.cfg);
        let observation = observe.then_some((nr, args));
        let preparation = (observe
            && L::observe_injected_syscall_preparation(&self.global_state.cfg))
        .then_some((nr, args));
        if let Some((task, context)) = self.take_original_entry(nr, args, None)? {
            // Prepared observes actual operands; common EXIT restores the
            // logical context before Linux signal/restart continuation.
            self.observe_injected_syscall(preparation, InjectedSyscallEvent::Prepared);
            self.observe_injected_syscall(observation, InjectedSyscallEvent::Entered);
            return self
                .finish_entered_original(task, nr, args, context, observation)
                .await
                .map(InjectedReadResult::Complete);
        }
        let mut task = self.assume_stopped();
        let pending = self.pending_syscall.take();
        let native = self.injected_syscall_frame.is_none() && !self.pending_syscall_already_skipped;
        self.observe_injected_syscall(preparation, InjectedSyscallEvent::Prepared);
        if native && pending.is_some() {
            // Skip the pending *different* syscall without executing a signal
            // handler under the Read's short table permit. Its actual exit is
            // not a Read receipt and does not pass through the Tool observer.
            let saved = task.getregs()?;
            let mut skipped = saved;
            *skipped.orig_syscall_mut() = -1i64 as u64;
            task.setregs(&skipped)?;
            let wait = self
                .syscall_stopped(task, None)?
                .next_state_with_owner(&self.ptracer_waits)
                .await?;
            self.arm_liteinst_wait(&wait);
            task = match wait {
                Wait::Stopped(stopped, Event::Syscall)
                    if stopped.syscall_exit_result()? == -i64::from(Errno::ENOSYS.into_raw()) =>
                {
                    stopped
                }
                Wait::Exited(_, status) => self.exit(status).await,
                other => self.abort(Ok(other)).await,
            };
            restore_context(&task, saved, None, false)?;
        }
        let context = task.getregs()?;
        let mut regs = if self.injected_syscall_frame.is_some() {
            self.read_guest_registers(&task)?
        } else {
            context
        };
        *regs.syscall_mut() = nr as u64;
        *regs.orig_syscall_mut() = nr as u64;
        regs.set_args((
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ));
        *regs.ip_mut() = cp::PRIVATE_PAGE_OFFSET as u64;
        check_stub(&task)?;
        task.setregs(&regs)?;
        let wait = self
            .syscall_stopped(task, None)?
            .next_state_with_owner(&self.ptracer_waits)
            .await?;
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(stopped, Event::Syscall) => {
                check_stub(&stopped)?;
                check_entry(
                    &stopped,
                    nr,
                    args,
                    (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
                    regs.stack_ptr(),
                    false,
                )?;
                self.observe_injected_syscall(observation, InjectedSyscallEvent::Entered);
                self.finish_entered_original(stopped, nr, args, Some(context), observation)
                    .await
                    .map(InjectedReadResult::Complete)
            }
            Wait::Stopped(stopped, Event::Signal(signal)) => {
                // We still own the FIRST resume: no ENTRY/SECCOMP stop has
                // been consumed. NONE plus exact unchanged pre-instruction
                // operands/frame/stub distinguishes this from an exit signal.
                let actual = stopped.getregs()?;
                check_stub(&stopped)?;
                if !stopped.syscall_info_is_none()?
                    || actual.ip() != regs.ip()
                    || actual.stack_ptr() != regs.stack_ptr()
                    || actual.ret() != nr as u64
                    || actual.args() != regs.args()
                {
                    return Err(Errno::EPROTO.into());
                }
                let ticket = InterruptedSyscall::with_signal(signal);
                self.interrupted_read = Some(InterruptedRead {
                    ticket: ticket.clone(),
                    stop: stopped,
                    signal,
                    context,
                    injected_frame: self.injected_syscall_frame,
                    ready: false,
                    completed: None,
                });
                self.observe_injected_syscall(
                    observation,
                    InjectedSyscallEvent::InterruptedBeforeEntry,
                );
                Ok(InjectedReadResult::Interrupted(ticket))
            }
            Wait::Exited(_, status) => self.exit(status).await,
            other => self.abort(Ok(other)).await,
        }
    }

    pub(super) async fn observe_recorded_read_signal(
        &mut self,
        call: reverie::syscalls::Read,
        expected: Signal,
    ) -> Result<InterruptedSyscall, TraceError> {
        // This await occupies exactly the delegate attempt's existing grant or
        // background ownership. Do not move it after Call retirement, external
        // completion, or the Tool post-hook: the sender may need that window.
        if self.injected_syscall_frame.is_some()
            || self.interrupted_read.is_some()
            || self.pending_signal.is_some()
            || self.pending_syscall_already_skipped
        {
            return Err(Errno::EPROTO.into());
        }
        let (nr, original_args) = self.pending_syscall.ok_or(Errno::EPROTO)?;
        if nr != Sysno::read {
            return Err(Errno::EPROTO.into());
        }
        let mut task = self.assume_stopped();
        let original = task.getregs()?;
        check_entry(
            &task,
            nr,
            original_args,
            original.ip(),
            original.stack_ptr(),
            true,
        )?;
        // Retire the intercepted entry without executing a physical Read.
        task = self.skip_recorded_read_entry(task).await?;
        self.pending_syscall = None;
        let (_, args) = call.into_parts();
        let mut waiting = original;
        *waiting.ip_mut() = cp::PRIVATE_PAGE_OFFSET as u64;
        *waiting.syscall_mut() = Sysno::read as u64;
        *waiting.orig_syscall_mut() = -1i64 as u64;
        waiting.set_args((
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ));
        loop {
            // Run no guest instruction or native Read while waiting. Each
            // private-page entry is caught BEFORE execution and skipped. A
            // logical -ERESTARTSYS at an arbitrary EXIT would not ensure that
            // Linux performs restart handling before returning to userspace.
            check_stub(&task)?;
            task.setregs(&waiting)?;
            let wait = self
                .syscall_stopped(task, None)?
                .next_state_with_owner(&self.ptracer_waits)
                .await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(stopped, Event::Signal(signal)) => {
                    let actual = stopped.getregs()?;
                    let info = stopped.getsiginfo()?;
                    if signal != expected
                        || info.si_signo != signal as i32
                        || !stopped.syscall_info_is_none()?
                        || actual.ip() != waiting.ip()
                        || actual.stack_ptr() != waiting.stack_ptr()
                        || actual.ret() != Sysno::read as u64
                        || actual.args() != waiting.args()
                    {
                        return Err(Errno::EPROTO.into());
                    }
                    let ticket = InterruptedSyscall::with_signal(signal);
                    self.interrupted_read = Some(InterruptedRead {
                        ticket: ticket.clone(),
                        stop: stopped,
                        signal,
                        context: original,
                        injected_frame: None,
                        ready: false,
                        completed: None,
                    });
                    // Keep the actual kernel siginfo untouched. The shared
                    // finish path restores logical regs only AFTER the Tool
                    // has retired its recorded Call, then the usual signal
                    // dispatcher applies the live disposition and SA_RESTART.
                    return Ok(ticket);
                }
                Wait::Stopped(stopped, Event::Syscall) => {
                    check_entry(
                        &stopped,
                        nr,
                        args,
                        (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
                        waiting.stack_ptr(),
                        false,
                    )?;
                    task = self.skip_recorded_read_entry(stopped).await?;
                }
                Wait::Exited(_, status) => self.exit(status).await,
                other => self.abort(Ok(other)).await,
            }
        }
    }

    async fn skip_recorded_read_entry(&mut self, task: Stopped) -> Result<Stopped, TraceError> {
        let mut skipped = task.getregs()?;
        *skipped.orig_syscall_mut() = -1i64 as u64;
        task.setregs(&skipped)?;
        let wait = self
            .syscall_stopped(task, None)?
            .next_state_with_owner(&self.ptracer_waits)
            .await?;
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(stopped, Event::Syscall) => {
                let raw = stopped.syscall_exit_result()?;
                let actual = stopped.getregs()?;
                if raw != -i64::from(Errno::ENOSYS.into_raw())
                    || actual.orig_syscall() != -1i64 as u64
                    || actual.args() != skipped.args()
                    || actual.ip() != skipped.ip()
                    || actual.stack_ptr() != skipped.stack_ptr()
                {
                    return Err(Errno::EPROTO.into());
                }
                Ok(stopped)
            }
            Wait::Exited(_, status) => self.exit(status).await,
            other => self.abort(Ok(other)).await,
        }
    }

    pub(super) fn prepare_interrupted_read_handback(
        &mut self,
        ticket: &InterruptedSyscall,
        completed: Option<i64>,
    ) -> Result<(), TraceError> {
        let state = self.interrupted_read.as_mut().ok_or(Errno::EPROTO)?;
        if !state.ticket.same(ticket)
            || state.ready
            || completed.is_some_and(|value| value <= 0)
            || state.context.orig_syscall() != Sysno::read as u64
        {
            return Err(Errno::EPROTO.into());
        }
        // E9's frame has read-only control flow and a trampoline-owned saved
        // state. This boundary must not abandon that frame or leak a private
        // restart errno. Its signal handback remains a separate required join.
        if state.injected_frame.is_some() {
            return Err(Errno::ENOTSUPP.into());
        }
        restore_context(&state.stop, state.context, None, false)?;
        state.completed = completed;
        state.ready = true;
        Ok(())
    }

    pub(super) fn finish_original_read_callback(
        &mut self,
        returned: &Option<Result<i64, reverie::Error>>,
    ) -> Result<Wait, TraceError> {
        let state = self.interrupted_read.as_ref().ok_or(Errno::EPROTO)?;
        let matches = match (state.completed, returned) {
            (Some(expected), Some(Ok(value))) => expected == *value,
            (None, Some(Err(reverie::Error::Tool(error)))) => error
                .downcast_ref::<InterruptedSyscall>()
                .is_some_and(|ticket| state.ticket.same(ticket)),
            _ => false,
        };
        if !state.ready
            || !matches
            || self.pending_signal.is_some()
            || self.injected_syscall_frame.is_some()
        {
            return Err(Errno::EPROTO.into());
        }
        let state = self.interrupted_read.take().unwrap();
        let mut regs = state.stop.getregs()?;
        if regs.orig_syscall() != state.context.orig_syscall()
            || regs.args() != state.context.args()
            || regs.ip() != state.context.ip()
            || regs.stack_ptr() != state.context.stack_ptr()
        {
            return Err(Errno::EPROTO.into());
        }
        if let Some(completed) = state.completed {
            *regs.ret_mut() = completed as u64;
        } else {
            // The logical Read already crossed its original seccomp entry.
            // A private attempt that never entered does not undo that fact.
            // Hand the actual signal-delivery stop back with the original
            // logical syscall context so Linux applies SA_RESTART, including
            // its EINTR conversion. This is a modeled logical interruption,
            // not the unentered attempt's native result: no Returned event is
            // emitted and its provider Call must already have been cancelled.
            *regs.ret_mut() = -i64::from(Errno::ERESTARTSYS.into_raw()) as u64;
        }
        state.stop.setregs(&regs)?;
        self.pending_syscall = None;
        self.pending_syscall_already_skipped = false;
        self.timer.finalize_requests();
        Ok(Wait::Stopped(state.stop, Event::Signal(state.signal)))
    }
}
