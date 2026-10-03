//! Finite, explicitly opted-in private continuation. Never park a Tool future
//! through guest handler execution. Unknown dispositions/frames stay contained.
use reverie::PrivateInterruption;
use reverie::PrivateInterruptionAction;
use reverie::PrivateReadCompletion;

use super::*;

#[derive(Default)]
pub(super) struct State {
    pub(super) logical: Option<Logical>,
    pub(super) consulting: bool,
    offered: Option<PrivateInterruption>,
    claimed: bool,
    pub(super) frame: Option<HandlerFrame>,
    pub(super) read: Option<ReadHandback>,
    pub(super) completing: Option<ReadCompletion>,
}
pub(super) struct Logical {
    task: TerminalCleanup,
    call: (Sysno, SyscallArgs),
    regs: libc::user_regs_struct,
    pub(super) receive: Option<followed_receive::Context>,
    pub(super) timer_join_pending: bool,
}
impl Logical {
    pub(super) fn unfinished(&self) -> bool {
        self.timer_join_pending
            || self
                .receive
                .as_ref()
                .is_some_and(followed_receive::Context::unfinished)
    }
    pub(super) fn matches_original(&self, task: &Stopped, call: (Sysno, SyscallArgs)) -> bool {
        self.call == call && self.task.same_generation(&task.terminal_cleanup())
    }
    pub(super) fn matches_frame(&self, actual: &libc::user_regs_struct) -> bool {
        safeptrace::ControlStop::registers_equal(actual, &self.regs)
    }
}
pub(super) struct ReadHandback {
    stop: Stopped,
    signal: Signal,
    context: libc::user_regs_struct,
    completed: Option<i64>,
}
pub(super) struct ReadCompletion {
    offer: PrivateReadCompletion,
    claimed: bool,
    context: libc::user_regs_struct,
}

pub(super) struct ContinuationOptions {
    pub(super) observe_tool: bool,
    pub(super) recorded: bool,
}

const KUC_SIZE: usize = 304;
const FRAME_SIZE: usize = 8 + KUC_SIZE + 128;
const GREGS: usize = 8 + std::mem::offset_of!(libc::ucontext_t, uc_mcontext);
const FP_POINTER: usize = GREGS + std::mem::offset_of!(libc::mcontext_t, fpregs);
const FP_MAX: usize = 65536;
fn word(bytes: &[u8], at: usize) -> u64 {
    u64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap())
}
fn greg(bytes: &[u8], register: usize) -> u64 {
    word(bytes, GREGS + register * 8)
}
fn set_greg(bytes: &mut [u8], register: usize, value: u64) {
    let at = GREGS + register * 8;
    bytes[at..at + 8].copy_from_slice(&value.to_ne_bytes());
}
fn gprs(r: &libc::user_regs_struct) -> [(usize, u64); 18] {
    [
        (libc::REG_R8 as _, r.r8),
        (libc::REG_R9 as _, r.r9),
        (libc::REG_R10 as _, r.r10),
        (libc::REG_R11 as _, r.r11),
        (libc::REG_R12 as _, r.r12),
        (libc::REG_R13 as _, r.r13),
        (libc::REG_R14 as _, r.r14),
        (libc::REG_R15 as _, r.r15),
        (libc::REG_RDI as _, r.rdi),
        (libc::REG_RSI as _, r.rsi),
        (libc::REG_RBP as _, r.rbp),
        (libc::REG_RBX as _, r.rbx),
        (libc::REG_RDX as _, r.rdx),
        (libc::REG_RAX as _, r.rax),
        (libc::REG_RCX as _, r.rcx),
        (libc::REG_RSP as _, r.rsp),
        (libc::REG_RIP as _, r.rip),
        (libc::REG_EFL as _, r.eflags),
    ]
}
fn same_regs(a: &libc::user_regs_struct, b: &libc::user_regs_struct) -> bool {
    gprs(a) == gprs(b)
        && a.orig_rax == b.orig_rax
        && a.cs == b.cs
        && a.ss == b.ss
        && a.ds == b.ds
        && a.es == b.es
        && a.fs == b.fs
        && a.gs == b.gs
        && a.fs_base == b.fs_base
        && a.gs_base == b.gs_base
}

pub(super) struct HandlerFrame {
    task: TerminalCleanup,
    handler: libc::user_regs_struct,
    bytes: [u8; FRAME_SIZE],
    fp_address: usize,
    fp: Vec<u8>,
    mask: u64,
    // Retained original information. It is never recreated with SI_USER or
    // compared to uninitialized frame->info for a non-SA_SIGINFO disposition.
    _signal_info: libc::siginfo_t,
    result: Option<Result<i64, Errno>>,
}
impl HandlerFrame {
    fn capture(
        task: &Stopped,
        delivery: &libc::user_regs_struct,
        signal: Signal,
        info: libc::siginfo_t,
    ) -> Result<Self, TraceError> {
        let administrative = task.getsiginfo()?;
        let h = task.getregs()?;
        if administrative.si_signo != libc::SIGTRAP
            || administrative.si_code != 5
            || !task.syscall_info_is_none()?
            || h.cs != 0x33
            || delivery.cs != 0x33
            || h.rdi != signal as u64
            || h.rax != 0
            || h.rdx != h.rsp.checked_add(8).ok_or(Errno::EOVERFLOW)?
            || h.rsi != h.rdx.checked_add(KUC_SIZE as u64).ok_or(Errno::EOVERFLOW)?
            || h.rip == delivery.rip
        {
            return Err(Errno::EPROTO.into());
        }
        let mut bytes = [0; FRAME_SIZE];
        task.read_exact(h.rsp as usize, &mut bytes)?;
        // Exact pre-delivery registers are the join, not si_code alone. In
        // particular rseq-adjusted RIP, guest TF changes or another frame refuse;
        // they are not overwritten to make the expected frame appear present.
        if gprs(delivery)
            .into_iter()
            .any(|(r, v)| greg(&bytes, r) != v)
            || word(&bytes, 8) & 6 != 6
            || word(&bytes, 16) != 0
        {
            return Err(Errno::EPROTO.into());
        }
        let fp_address = word(&bytes, FP_POINTER) as usize;
        if fp_address == 0 {
            return Err(Errno::EPROTO.into());
        }
        let mut fp = vec![0; 512];
        task.read_exact(fp_address, &mut fp)?;
        let magic = u32::from_ne_bytes(fp[464..468].try_into().unwrap());
        let size = u32::from_ne_bytes(fp[468..472].try_into().unwrap()) as usize;
        if magic == 0x46505853 {
            if !(512..=FP_MAX).contains(&size) {
                return Err(Errno::EOVERFLOW.into());
            }
            fp.resize(size, 0);
            task.read_exact(fp_address, &mut fp)?;
            if u32::from_ne_bytes(fp[size - 4..].try_into().unwrap()) != 0x46505845 {
                return Err(Errno::EPROTO.into());
            }
        } else if magic != 0 {
            return Err(Errno::EPROTO.into());
        }
        Ok(Self {
            task: task.terminal_cleanup(),
            handler: h,
            bytes,
            fp_address,
            fp,
            mask: task.getsigmask_native()?,
            _signal_info: info,
            result: None,
        })
    }

    fn check_unchanged(&self, task: &Stopped) -> Result<(), TraceError> {
        if !self.task.same_generation(&task.terminal_cleanup())
            || task.getsigmask_native()? != self.mask
        {
            return Err(Errno::EPROTO.into());
        }
        let mut bytes = [0; FRAME_SIZE];
        let mut fp = vec![0; self.fp.len()];
        task.read_exact(self.handler.rsp as usize, &mut bytes)?;
        task.read_exact(self.fp_address, &mut fp)?;
        if bytes != self.bytes || fp != self.fp {
            return Err(Errno::EPROTO.into());
        }
        Ok(())
    }

    fn commit(self, task: &mut Stopped, logical: &Logical, raw: i64) -> Result<(), TraceError> {
        self.check_unchanged(task)?;
        if !logical.task.same_generation(&task.terminal_cleanup()) {
            return Err(Errno::ECHILD.into());
        }
        let mut expected = self.bytes;
        // Only fields changed by private injection and the actual completed
        // logical result are authorized. SP, flags/guest TF, mask, altstack,
        // siginfo, reserved bytes and FP/xstate are left exactly as Linux made
        // them. All other general registers must ALREADY match the logical call.
        let edited = [
            libc::REG_RAX,
            libc::REG_RIP,
            libc::REG_RDI,
            libc::REG_RSI,
            libc::REG_RDX,
            libc::REG_R10,
            libc::REG_R8,
            libc::REG_R9,
            libc::REG_RCX,
            libc::REG_R11,
        ];
        for (register, value) in gprs(&logical.regs) {
            if edited.contains(&(register as i32)) {
                set_greg(
                    &mut expected,
                    register,
                    if register == libc::REG_RAX as usize {
                        raw as u64
                    } else {
                        value
                    },
                );
            } else if greg(&expected, register) != value {
                return Err(Errno::EPROTO.into());
            }
        }
        // Write individual authorized words, not a whole copied ucontext. An
        // error is fatal while the real handler is still held; no handler is
        // released with a partially committed frame.
        for register in edited {
            let at = GREGS + register as usize * 8;
            let address = (self.handler.rsp as usize)
                .checked_add(at)
                .ok_or(Errno::EOVERFLOW)?;
            task.write_value(
                AddrMut::from_raw(address).ok_or(Errno::EFAULT)?,
                &word(&expected, at),
            )?;
        }
        let mut actual = [0; FRAME_SIZE];
        task.read_exact(self.handler.rsp as usize, &mut actual)?;
        let mut fp = vec![0; self.fp.len()];
        task.read_exact(self.fp_address, &mut fp)?;
        if actual != expected || fp != self.fp || task.getsigmask_native()? != self.mask {
            return Err(Errno::EPROTO.into());
        }
        task.setregs(&self.handler)?;
        Ok(())
    }
}

impl<L: Tool + 'static> TracedTask<L> {
    pub(super) fn private_logical_registers(&self) -> Option<libc::user_regs_struct> {
        (self.private_signal.consulting || self.private_signal.frame.is_some())
            .then(|| self.private_signal.logical.as_ref().map(|l| l.regs))
            .flatten()
    }

    pub(super) fn set_private_logical_registers(
        &mut self,
        requested: libc::user_regs_struct,
    ) -> Result<bool, reverie::Error> {
        if let Some(completing) = self.private_signal.completing.as_ref() {
            // This phase exposes real registers. Preserve the exact installed
            // result and all original operands; only existing clobber
            // canonicalization may change them, with an actual readback.
            let current = completing.context;
            let mut permitted = current;
            permitted.rcx = requested.rcx;
            permitted.r11 = requested.r11;
            let task = self.assume_stopped();
            let refusal = |error: TraceError| {
                reverie::Error::Tool(anyhow::anyhow!(
                    "private Read completion register access: {error}"
                ))
            };
            if !same_regs(&permitted, &requested)
                || !same_regs(&task.getregs().map_err(refusal)?, &current)
            {
                return Err(reverie::Error::Tool(anyhow::anyhow!(
                    "private Read completion permits only actual RCX/R11 canonicalization"
                )));
            }
            task.setregs(&permitted).map_err(refusal)?;
            if !same_regs(&task.getregs().map_err(refusal)?, &permitted) {
                return Err(reverie::Error::Tool(anyhow::anyhow!(
                    "private Read completion clobber readback changed"
                )));
            }
            self.private_signal.completing.as_mut().unwrap().context = permitted;
            return Ok(true);
        }
        let Some(current) = self.private_logical_registers() else {
            return Ok(false);
        };
        let mut permitted = current;
        permitted.rcx = requested.rcx;
        permitted.r11 = requested.r11;
        if !same_regs(&permitted, &requested) {
            return Err(reverie::Error::Tool(anyhow::anyhow!(
                "private continuation only permits logical syscall-clobber updates"
            )));
        }
        self.private_signal.logical.as_mut().unwrap().regs = permitted;
        Ok(true)
    }

    pub(super) fn claim_held_private_interruption(
        &mut self,
        ticket: &PrivateInterruption,
    ) -> Result<(), reverie::Error> {
        let state = &mut self.private_signal;
        if !state.consulting
            || state.claimed
            || state.logical.is_none()
            || !state.offered.as_ref().is_some_and(|held| held.same(ticket))
        {
            return Err(reverie::Error::Tool(anyhow::anyhow!(
                "foreign, stale or already claimed private interruption"
            )));
        }
        // The original Stopped value is retained in continue_private_signal;
        // its task/call/stop join was validated before publishing this offer.
        // Guest injection is forbidden until that local owner consumes action.
        state.claimed = true;
        Ok(())
    }

    pub(super) fn claim_held_private_read_completion(
        &mut self,
        ticket: &PrivateReadCompletion,
    ) -> Result<(), reverie::Error> {
        let refused = || {
            reverie::Error::Tool(anyhow::anyhow!(
                "foreign, stale or already claimed private Read completion"
            ))
        };
        let state = self
            .private_signal
            .completing
            .as_ref()
            .ok_or_else(refused)?;
        if state.claimed
            || !state.offer.same(ticket)
            || self.private_signal.consulting
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
        {
            return Err(refused());
        }
        let logical = self.private_signal.logical.as_ref().ok_or_else(refused)?;
        let task = self.assume_stopped();
        if !logical.task.same_generation(&task.terminal_cleanup())
            || logical.call != ticket.logical_call()
            || state.context.rax
                != ticket
                    .completed()
                    .unwrap_or(-i64::from(Errno::ERESTARTSYS.into_raw())) as u64
            || !same_regs(&task.getregs().map_err(|_| refused())?, &state.context)
            || !task.syscall_info_is_none().map_err(|_| refused())?
        {
            return Err(refused());
        }
        self.private_signal.completing.as_mut().unwrap().claimed = true;
        Ok(())
    }

    pub(super) fn begin_private_logical_call(
        &mut self,
        task: &Stopped,
        call: (Sysno, SyscallArgs),
    ) -> Result<(), TraceError> {
        if self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
            || self.private_signal.consulting
            || self.private_signal.completing.is_some()
        {
            return Err(Errno::EPROTO.into());
        }
        self.private_signal.logical = Some(Logical {
            task: task.terminal_cleanup(),
            call,
            regs: task.getregs()?,
            receive: None,
            timer_join_pending: false,
        });
        Ok(())
    }

    pub(super) async fn wait_recorded_private_signal(
        &mut self,
        helper: reverie::syscalls::Syscall,
        expected: Signal,
    ) -> Result<reverie::Never, TraceError> {
        if self.private_signal.consulting
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
            || self.private_signal.completing.is_some()
            || self.injected_syscall_frame.is_some()
            || self.interrupted_read.is_some()
            || self.pending_signal.is_some()
        {
            return Err(Errno::EPROTO.into());
        }
        let logical = self.private_signal.logical.as_ref().ok_or(Errno::EPROTO)?;
        if logical.call.0 != Sysno::read {
            return Err(Errno::EPROTO.into());
        }
        let original = logical.regs;
        let logical_call = logical.call;
        let mut task = self.assume_stopped();
        if !logical.task.same_generation(&task.terminal_cleanup()) {
            return Err(Errno::ECHILD.into());
        }
        if let Some(pending) = self.pending_syscall {
            if pending != logical_call {
                return Err(Errno::EPROTO.into());
            }
            original_context::check_entry(
                &task,
                pending.0,
                pending.1,
                original.rip,
                original.rsp,
                true,
            )?;
            task = self.skip_recorded_read_entry(task).await?;
            self.pending_syscall = None;
            self.original_read_entry = None;
        } else {
            // Earlier real attempts may have completed bytes. Their effects
            // remain; this branch only accepts their current physical EXIT.
            task.syscall_exit_result()?;
        }
        // Both admitted routes must still own a genuine EXIT before any
        // helper registers are installed. The initial SECCOMP-only skip above
        // is not interchangeable with a skip from an ordinary syscall ENTRY.
        task.syscall_exit_result()?;
        let (nr, args) = helper.into_parts();
        let mut waiting = original;
        waiting.rip = cp::PRIVATE_PAGE_OFFSET as u64;
        waiting.rax = nr as u64;
        waiting.orig_rax = -1i64 as u64;
        waiting.set_args((
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ));
        let mut stub = [0; 4];
        task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut stub)?;
        if stub != [0x0f, 0x05, 0x0f, 0x0b] {
            return Err(Errno::EPROTO.into());
        }
        task.setregs(&waiting)?;
        let installed = task.getregs()?;
        if installed.rip != waiting.rip
            || installed.rsp != waiting.rsp
            || installed.rax != waiting.rax
            || installed.orig_rax != waiting.orig_rax
            || installed.args() != waiting.args()
        {
            return Err(Errno::EPROTO.into());
        }
        // Select emulation before executing the private syscall instruction.
        // This is one attempt, not a polling loop that holds the Tool's turn.
        let wait = self.sysemu_from_exit_stopped(task)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(stopped, Event::Signal(signal)) => {
                if signal != expected {
                    return Err(Errno::EPROTO.into());
                }
                // A real signal, same held task and exact pre-instruction
                // context are checked by the same one-use offer path.
                // FinishRead cancels the original callback, so no helper
                // result (not even a placeholder) can be returned here.
                return match self
                    .continue_private_signal(
                        stopped,
                        signal,
                        nr,
                        args,
                        original,
                        ContinuationOptions {
                            observe_tool: false,
                            recorded: true,
                        },
                    )
                    .await?
                {
                    Ok(_) | Err(_) => Err(Errno::EPROTO.into()),
                };
            }
            Wait::Stopped(stopped, Event::Syscall) => {
                original_context::check_entry(
                    &stopped,
                    nr,
                    args,
                    (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
                    waiting.rsp,
                    false,
                )?;
                // No matching signal was delivered before the emulated
                // entry. Keep the real held stop for fatal cleanup. This
                // backend refusal is never a guest errno or helper result;
                // do not resume, manufacture an EXIT, or spin for a sender.
                Err(Errno::EAGAIN.into())
            }
            Wait::Exited(_, status) => self.exit(status).await,
            _ => Err(Errno::EPROTO.into()),
        }
    }

    pub(super) async fn continue_private_signal(
        &mut self,
        stopped: Stopped,
        signal: Signal,
        nr: Sysno,
        args: SyscallArgs,
        oldregs: libc::user_regs_struct,
        options: ContinuationOptions,
    ) -> Result<Result<i64, Errno>, TraceError> {
        let ContinuationOptions {
            observe_tool,
            recorded,
        } = options;
        // Each caller has armed this exact newly returned signal stop.
        let logical = self
            .private_signal
            .logical
            .as_ref()
            .ok_or(Errno::ENOTSUPP)?;
        let delivery = stopped.getregs()?;
        let info = stopped.getsiginfo()?;
        let mut stub = [0; 4];
        stopped.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut stub)?;
        let expected_args = (
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        );
        if self.private_signal.consulting
            || self.private_signal.frame.is_some()
            || self.private_signal.read.is_some()
            || self.private_signal.completing.is_some()
            || self.injected_syscall_frame.is_some()
            || self.pending_signal.is_some()
            || self.interrupted_read.is_some()
            || !logical.task.same_generation(&stopped.terminal_cleanup())
            || !stopped.syscall_info_is_none()?
            || info.si_signo != signal as i32
            || delivery.rip != cp::PRIVATE_PAGE_OFFSET as u64
            || delivery.rsp != oldregs.rsp
            || delivery.rax != nr as u64
            || delivery.args() != expected_args
            || stub != [0x0f, 0x05, 0x0f, 0x0b]
        {
            return Err(Errno::EPROTO.into());
        }
        let view = if recorded {
            PrivateInterruption::new_recorded_backend(logical.call, (nr, args), signal)
        } else {
            PrivateInterruption::new_backend(logical.call, (nr, args), signal)
        };
        self.private_signal.offered = Some(view.clone());
        self.private_signal.claimed = false;
        self.private_signal.consulting = true;
        let action = self
            .process_state
            .clone()
            .handle_private_interruption(self, &view)
            .await;
        self.private_signal.consulting = false;
        self.private_signal.offered = None;
        let claimed = std::mem::take(&mut self.private_signal.claimed);
        let action = match action {
            Ok(action) => action,
            Err(error) => {
                self.publish_ordinary_failure("private interruption finalizer", error);
                return Err(Errno::ECANCELED.into());
            }
        };
        if !matches!(action, PrivateInterruptionAction::Unsupported) && !claimed {
            return Err(Errno::EPROTO.into());
        }
        let after = stopped.getregs()?;
        if !same_regs(&after, &delivery) || !stopped.syscall_info_is_none()? {
            return Err(Errno::EPROTO.into());
        }
        let logical_context = self
            .private_signal
            .logical
            .as_ref()
            .ok_or(Errno::EPROTO)?
            .regs;
        match action {
            PrivateInterruptionAction::Unsupported => return Err(Errno::ENOTSUPP.into()),
            PrivateInterruptionAction::FinishRead { completed } => {
                if view.logical_call().0 != Sysno::read
                    || completed
                        .is_some_and(|n| n <= 0 || n as u64 > view.logical_call().1.arg2 as u64)
                    || logical_context.orig_rax != Sysno::read as u64
                {
                    return Err(Errno::EPROTO.into());
                }
                // Settlement is not the register-observing tail. Install the
                // actual logical boundary on the original held signal stop
                // first, then authenticate a second one-use offer. No physical
                // resume, native Read ticket or helper return is invented.
                let mut context = logical_context;
                context.rax = completed.unwrap_or(-i64::from(Errno::ERESTARTSYS.into_raw())) as u64;
                stopped.setregs(&context)?;
                if !same_regs(&stopped.getregs()?, &context) || !stopped.syscall_info_is_none()? {
                    return Err(Errno::EPROTO.into());
                }
                let completion = PrivateReadCompletion::new_backend(view, completed);
                self.private_signal.completing = Some(ReadCompletion {
                    offer: completion.clone(),
                    claimed: false,
                    context,
                });
                // consulting is false: Guest::regs uses the physical reader.
                // The separate completing state blocks every injection path
                // and restricts set_regs to read-back RCX/R11 changes.
                let result = self
                    .process_state
                    .clone()
                    .handle_private_read_completion(self, &completion)
                    .await;
                let completing = self.private_signal.completing.take().ok_or(Errno::EPROTO)?;
                if let Err(error) = result {
                    self.publish_ordinary_failure("private Read completion tail", error);
                    return Err(Errno::ECANCELED.into());
                }
                if !completing.claimed
                    || !completing.offer.same(&completion)
                    || !same_regs(&stopped.getregs()?, &completing.context)
                    || !stopped.syscall_info_is_none()?
                {
                    return Err(Errno::EPROTO.into());
                }
                self.private_signal.read = Some(ReadHandback {
                    stop: stopped,
                    signal,
                    context: completing.context,
                    completed,
                });
                self.cancel_handler.store(true, Ordering::SeqCst);
                return future::pending().await;
            }
            PrivateInterruptionAction::DrainHelperResult if !recorded => {}
            PrivateInterruptionAction::DrainHelperResult => return Err(Errno::EPROTO.into()),
        }
        self.private_signal.consulting = true;
        let forwarded = self
            .process_state
            .clone()
            .handle_signal_event(self, signal)
            .await;
        self.private_signal.consulting = false;
        // Suppression/replacement has distinct siginfo/disposition semantics.
        // It remains OPEN, not silently treated as a successful caught frame.
        if !matches!(forwarded, Ok(Some(s)) if s == signal) {
            return Err(Errno::ENOTSUPP.into());
        }
        self.ordinary_trace_continuation()?;
        let actual = stopped.getregs()?;
        if !same_regs(&actual, &delivery) {
            return Err(Errno::EPROTO.into());
        }
        // This is signal-frame installation, NOT a guest instruction step. Do
        // not use step_stopped's unsupported-source fallback or count code 5 as
        // TRAP_TRACE. The original cohort resume and terminal owner remain live.
        self.global_state
            .fatal_session
            .source_epoch
            .observe_resume(&stopped);
        let operation = self.cohort.as_ref().and_then(|m| m.before_resume(&stopped));
        let running = self.lease_liteinst_root_stop(stopped).step(Some(signal))?;
        let wait = self.task_running(running, operation).next_state().await?;
        self.arm_liteinst_wait(&wait);
        let task = match wait {
            Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)) => task,
            Wait::Exited(_, status) => self.exit(status).await,
            _ => return Err(Errno::ENOTSUPP.into()),
        };
        let frame = HandlerFrame::capture(&task, &delivery, signal, info)?;
        self.private_signal.frame = Some(frame);
        // This remains inside the suspended original inject. Actual ENTRY/EXIT
        // and its NativeOperation are the ordinary existing injection path.
        let result = Box::pin(self.untraced_syscall(task, nr, args, observe_tool)).await?;
        let frame = self.private_signal.frame.as_mut().ok_or(Errno::EPROTO)?;
        frame.result = Some(result);
        Ok(result)
    }

    pub(super) async fn finish_private_signal_callback(
        &mut self,
        returned: &Option<Result<i64, reverie::Error>>,
    ) -> Result<Option<Wait>, TraceError> {
        if self.private_signal.completing.is_some() {
            return Err(Errno::EPROTO.into());
        }
        if let Some(read) = self.private_signal.read.take() {
            if returned.is_some() {
                return Err(Errno::EPROTO.into());
            }
            // Registers were installed before the common tail, not fabricated
            // for its observer. They must still be exactly that verified state.
            if !same_regs(&read.stop.getregs()?, &read.context)
                || !read.stop.syscall_info_is_none()?
            {
                return Err(Errno::EPROTO.into());
            }
            self.pending_syscall = None;
            self.pending_syscall_already_skipped = false;
            self.original_read_entry = None;
            self.private_signal.logical = None;
            if read.completed.is_some() {
                let done = self
                    .source_observer
                    .lock()
                    .unwrap()
                    .finish_tool(&read.stop)?;
                if let Some(done) = done {
                    self.timer.complete_tool_step(done);
                }
            }
            self.timer.finalize_requests();
            return Ok(Some(Wait::Stopped(read.stop, Event::Signal(read.signal))));
        }
        let Some(frame) = self.private_signal.frame.take() else {
            self.private_signal.logical = None;
            return Ok(None);
        };
        let actual = match returned {
            Some(Ok(value)) => Ok(*value),
            Some(Err(reverie::Error::Errno(error))) => Err(*error),
            _ => return Err(Errno::EPROTO.into()),
        };
        if frame.result != Some(actual) {
            return Err(Errno::EPROTO.into());
        }
        let logical = self.private_signal.logical.take().ok_or(Errno::EPROTO)?;
        let mut task = self.assume_stopped();
        let raw = actual.unwrap_or_else(|e| -i64::from(e.into_raw()));
        frame.commit(&mut task, &logical, raw)?;
        self.pending_syscall = None;
        self.pending_syscall_already_skipped = false;
        self.original_read_entry = None;
        self.original_setsockopt_entry = None;
        let done = self.source_observer.lock().unwrap().finish_tool(&task)?;
        if let Some(done) = done {
            self.timer.complete_tool_step(done);
        }
        self.timer.finalize_requests();
        // Callback already returned and frame committed. From this point no
        // suspended Tool future survives handler edits, rt_sigreturn or escape.
        Ok(Some(self.resume_stopped(task, None)?.next_state().await?))
    }
}
