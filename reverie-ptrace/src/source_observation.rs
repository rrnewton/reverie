/* Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
 * Licensed under the BSD-style license in the root LICENSE file. */

//! Per-traced-task syscall observation. No seccomp rule or child policy is added.
//! The actual wait owner, not the numeric syscall fields, pairs an attempt.
//! An ENTRY before Tool dispatch is observation only; it holds no native debt.
use std::sync::Arc;
use std::sync::Mutex;

use reverie::Errno;
use reverie::Subscription;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use safeptrace::Event;
use safeptrace::Stopped;
use safeptrace::SyscallEntry;
use safeptrace::SyscallStopInfo;
use safeptrace::TerminalCleanup;
use safeptrace::Wait;

use super::LiteinstRootStopArmer;
use super::RootStopLease;
use super::TraceError;
use super::source_cohort;
use super::source_epoch;
use crate::cp;

pub(super) const X86_64: u32 = 0xc000_003e;
pub(super) const TF: u64 = 0x100;

#[derive(Default)]
pub(super) struct State {
    pending: Option<Attempt>,
    interrupted_step: Option<Step>,
    tool_step: Option<ToolStep>,
    raw_birth: Option<(TerminalCleanup, SyscallEntry)>,
    // Set only by the original initial EXEC/newborn owner. This consumes the
    // startup return, never a completion or a source/native-operation receipt.
    startup: bool,
}
impl State {
    pub(super) fn startup() -> Self {
        Self {
            pending: None,
            interrupted_step: None,
            tool_step: None,
            raw_birth: None,
            startup: true,
        }
    }
    pub(super) fn birth(&self) -> Option<(Sysno, SyscallArgs)> {
        self.pending
            .as_ref()
            .and_then(|a| parts(a.entry))
            .or_else(|| self.raw_birth.as_ref().and_then(|(_, entry)| parts(*entry)))
            .filter(|(nr, _)| {
                matches!(
                    nr,
                    Sysno::clone | Sysno::clone3 | Sysno::fork | Sysno::vfork
                )
            })
    }
    pub(super) fn birth_restored(&mut self) {
        if let Some(attempt) = self.pending.take()
            && attempt.returned
            && let Some(owner) = attempt.native
        {
            owner.child_returned();
        }
    }
    // Called only by the real event dispatcher, immediately after its existing
    // timer.observe_event(). Tool/signal/lifecycle delivery cancels the old timer
    // request; private EXITs must not complete that cancelled instruction owner.
    pub(super) fn retire_interrupted_step(&mut self, task: &Stopped) -> Result<(), TraceError> {
        if let Some(mut step) = self.interrupted_step.take() {
            if !step.task.same_generation(&task.terminal_cleanup()) {
                return Err(Errno::ECHILD.into());
            }
            step.phase.cancel()?;
        }
        Ok(())
    }
    pub(super) fn cancel_tool_for_guest_event(&mut self, task: &Stopped) -> Result<(), TraceError> {
        if let Some(mut step) = self.tool_step.take() {
            if !step.step.task.same_generation(&task.terminal_cleanup()) {
                return Err(Errno::ECHILD.into());
            }
            // The actual dispatcher has cancelled/dropped the borrowed Tool
            // continuation. Cancel its instruction obligation, never count it
            // as a completed step. Native/source retirement owners are separate.
            step.step.phase.cancel()?;
        }
        Ok(())
    }
    pub(super) fn abandon(&mut self) {
        self.pending = None;
        self.startup = false;
        // Exec/death cancels this logical instruction; it is not an EXIT or
        // completion of a private instruction subsequently executed by a Tool.
        self.tool_step = None;
        self.raw_birth = None;
    }

    pub(super) fn tool_needs_original_resume(&self) -> bool {
        self.tool_step
            .as_ref()
            .is_some_and(|step| step.physical == ToolPhysical::Held)
    }

    pub(super) fn finish_tool(&mut self, task: &Stopped) -> Result<Option<Completion>, TraceError> {
        let Some(step) = self.tool_step.as_ref() else {
            return Ok(None);
        };
        if !step.step.task.same_generation(&task.terminal_cleanup())
            || step.physical != ToolPhysical::Returned
        {
            return Err(Errno::EPROTO.into());
        }
        let mut step = self.tool_step.take().unwrap();
        step.step.phase.tool_return()?;
        Ok(Some(Completion {
            _task: step.step.task,
            deferred: step.counter,
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolPhysical {
    Held,
    Running,
    Returned,
}
impl ToolPhysical {
    fn resume(&mut self) -> Result<(), Errno> {
        if *self != Self::Held {
            return Err(Errno::EPROTO);
        }
        *self = Self::Running;
        Ok(())
    }
    fn returned(&mut self, original: bool) -> Result<(), Errno> {
        if *self != Self::Running || !original {
            return Err(Errno::EPROTO);
        }
        *self = Self::Returned;
        Ok(())
    }
}
struct ToolStep {
    step: Step,
    physical: ToolPhysical,
    counter: Option<crate::timer::DeferredStepCounter>,
}

/// A linear handoff from the timer's actual SECCOMP wait, not a completion.
pub(crate) struct ToolTransfer {
    state: Arc<Mutex<State>>,
}
impl ToolTransfer {
    pub(crate) fn bind_counter(
        self,
        counter: crate::timer::DeferredStepCounter,
    ) -> Result<(), TraceError> {
        let mut state = self.state.lock().unwrap();
        let step = state.tool_step.as_mut().ok_or(Errno::EPROTO)?;
        if step.counter.is_some() {
            return Err(Errno::EPROTO.into());
        }
        step.counter = Some(counter);
        Ok(())
    }
}
struct Attempt {
    entry: SyscallEntry,
    task: TerminalCleanup,
    native: Option<source_cohort::NativeOperation>,
    admitted: bool,
    returned: bool,
    child: bool,
}

#[derive(Clone)]
pub(super) struct Context {
    pub(super) state: Arc<Mutex<State>>,
    pub(super) member: source_cohort::Member,
    pub(super) epoch: Arc<source_epoch::SourceEpoch>,
    pub(super) subscriptions: Arc<Subscription>,
    pub(super) armer: Option<LiteinstRootStopArmer>,
}
fn parts(entry: SyscallEntry) -> Option<(Sysno, SyscallArgs)> {
    if entry.arch != X86_64 || entry.number >= 0x4000_0000 {
        return None;
    }
    let nr = Sysno::new(entry.number as usize)?;
    let a = entry.arguments;
    Some((
        nr,
        SyscallArgs::new(
            a[0] as usize,
            a[1] as usize,
            a[2] as usize,
            a[3] as usize,
            a[4] as usize,
            a[5] as usize,
        ),
    ))
}
fn matching_stop(event: &Event, info: SyscallStopInfo) -> bool {
    matches!(
        (event, info),
        (
            Event::Syscall,
            SyscallStopInfo::Entry(_) | SyscallStopInfo::Exit { .. }
        ) | (Event::Seccomp, SyscallStopInfo::Seccomp(_))
    )
}
fn unsupported_entry(entry: SyscallEntry) -> bool {
    entry.arch != X86_64 || ((entry.number as i32) >= 0 && parts(entry).is_none())
}
fn tool_entry(entry: SyscallEntry, subscriptions: &Subscription) -> bool {
    parts(entry).is_some_and(|(nr, _)| {
        nr != Sysno::rt_sigreturn
            && !(cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE
                ..cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE + cp::UD_INSTR_SIZE)
                .contains(&(entry.instruction_pointer as usize))
            && subscriptions.iter_syscalls().any(|n| n == nr)
    })
}
impl Context {
    pub(super) fn close(&self) {
        self.epoch.revoke();
        self.member.close_observation();
    }
    fn observe_entry(&self, entry: SyscallEntry) {
        if unsupported_entry(entry) {
            self.epoch.revoke();
        }
        self.epoch.observe_raw(entry.number, entry.arguments);
        self.member.observe_entry(entry);
    }
    pub(super) fn begin_step(&self, task: &Stopped) -> Result<Option<Step>, TraceError> {
        if !native_step_abi(task.getregs()?.cs) {
            // LDT/compat IP translation belongs to the kernel's step decoder.
            // Close source authority BEFORE falling back to that real owner.
            self.close();
            return Ok(None);
        }
        Step::begin(task).map(Some)
    }
    pub(super) fn retain_interrupted_step(&self, step: Step) -> Result<(), TraceError> {
        let mut state = self.state.lock().unwrap();
        if state.interrupted_step.is_some() {
            return Err(Errno::EPROTO.into());
        }
        state.interrupted_step = Some(step);
        Ok(())
    }
    pub(super) fn transfer_tool(
        &self,
        mut step: Step,
        task: &Stopped,
    ) -> Result<ToolTransfer, TraceError> {
        if !step.task.same_generation(&task.terminal_cleanup()) {
            return Err(Errno::ECHILD.into());
        }
        step.phase.tool_entry()?;
        let mut state = self.state.lock().unwrap();
        if state.tool_step.is_some() {
            return Err(Errno::EPROTO.into());
        }
        state.tool_step = Some(ToolStep {
            step,
            physical: ToolPhysical::Held,
            counter: None,
        });
        Ok(ToolTransfer {
            state: Arc::clone(&self.state),
        })
    }
    /// Only the raw resume of this retained SECCOMP stop admits its original
    /// physical attempt. A later private ENTRY/EXIT cannot create this owner.
    pub(super) fn raw_resume(&self, task: &Stopped) -> Result<(), TraceError> {
        if let SyscallStopInfo::Entry(entry) | SyscallStopInfo::Seccomp(entry) =
            task.syscall_stop_info()?
        {
            // Includes private injection and a Tool's rewritten original
            // effect. clone3 keeps SourceEpoch revoked and carries an unresolved
            // cohort birth debt; no userspace flags snapshot authorizes it.
            self.observe_entry(entry);
            if parts(entry).is_some_and(|(nr, _)| {
                matches!(
                    nr,
                    Sysno::clone | Sysno::clone3 | Sysno::fork | Sysno::vfork
                )
            }) {
                // This is the real raw-resume owner, including non-observing
                // Tools and private injection. It supplies parent completion
                // context, never a Tool notification or source permit.
                self.state.lock().unwrap().raw_birth = Some((task.terminal_cleanup(), entry));
            }
        }
        let mut state = self.state.lock().unwrap();
        if let Some(step) = state.tool_step.as_mut()
            && step.physical == ToolPhysical::Held
            && matches!(task.syscall_stop_info()?, SyscallStopInfo::Seccomp(_))
        {
            if !step.step.task.same_generation(&task.terminal_cleanup()) {
                return Err(Errno::ECHILD.into());
            }
            step.physical.resume()?;
        }
        Ok(())
    }
    pub(super) fn prepare(
        &self,
        task: &Stopped,
    ) -> Result<Option<source_cohort::ResumeOperation>, TraceError> {
        let info = task.syscall_stop_info()?;
        let mut state = self.state.lock().unwrap();
        match info {
            SyscallStopInfo::Entry(entry) if tool_entry(entry, &self.subscriptions) => {
                // Kernel seccomp/Tool opportunity still precedes actual effect.
                self.observe_entry(entry);
                return Ok(self.member.before_entry_observation(task));
            }
            SyscallStopInfo::Entry(entry) | SyscallStopInfo::Seccomp(entry) => {
                self.observe_entry(entry);
                if state.pending.is_none() {
                    state.pending = Some(Attempt {
                        entry,
                        task: task.terminal_cleanup(),
                        native: None,
                        admitted: false,
                        returned: false,
                        child: false,
                    });
                }
                let attempt = state.pending.as_mut().unwrap();
                if !attempt.task.same_generation(&task.terminal_cleanup()) {
                    self.close();
                    return Err(Errno::ECHILD.into());
                }
                if !attempt.admitted {
                    attempt.entry = entry; // effective operands after a Tool rewrite
                    attempt.native =
                        parts(entry).and_then(|(nr, args)| self.member.native(nr, args));
                    attempt.admitted = true;
                }
            }
            _ => {}
        }
        Ok(self.member.before_resume(task))
    }
    pub(super) fn resume(
        &self,
        task: Stopped,
        signal: Option<nix::sys::signal::Signal>,
    ) -> Result<(safeptrace::Running, Option<source_cohort::ResumeOperation>), TraceError> {
        let operation = self.prepare(&task)?;
        let slot = self.armer.as_ref().map(|a| Arc::clone(&a.held_root_stop));
        let running = RootStopLease::new(task, slot).syscall(signal)?;
        Ok((running, operation))
    }
    /// Returns a receipt only for an EXIT paired by this retained task owner.
    /// Raw injection callers observe only an already owned attempt; they do not
    /// enroll private ENTRY stops as a new guest attempt.
    pub(super) fn observe(
        &self,
        wait: &Wait,
        administrative: bool,
    ) -> Result<Option<OriginalExit>, TraceError> {
        let Wait::Stopped(task, event) = wait else {
            self.state.lock().unwrap().abandon();
            return Ok(None);
        };
        if let Some(armer) = &self.armer {
            armer.ensure(task, event)?;
        }
        if matches!(event, Event::Exec(_) | Event::Exit) {
            self.state.lock().unwrap().abandon();
            return Ok(None);
        }
        if matches!(event, Event::NewChild(..))
            && let Some(attempt) = self.state.lock().unwrap().pending.as_mut()
        {
            attempt.child = true;
        }
        if !matches!(event, Event::Syscall | Event::Seccomp) {
            return Ok(None);
        }
        let info = task.syscall_stop_info()?;
        if !matching_stop(event, info) {
            self.close();
            return Err(Errno::EPROTO.into());
        }
        let mut state = self.state.lock().unwrap();
        match info {
            SyscallStopInfo::Entry(entry) if administrative => {
                self.observe_entry(entry);
                if state.pending.is_some() {
                    self.close();
                    return Err(Errno::EPROTO.into());
                }
                state.startup = false;
                state.pending = Some(Attempt {
                    entry,
                    task: task.terminal_cleanup(),
                    native: None,
                    admitted: false,
                    returned: false,
                    child: false,
                });
            }
            SyscallStopInfo::Entry(entry) => self.observe_entry(entry),
            SyscallStopInfo::Seccomp(_) if administrative => {
                // Transfer to the real Tool callback BEFORE native admission.
                // The callback's existing injection owner or final resume owns
                // execution; an administrative observer cannot retire it later.
                state.pending = None;
                state.startup = false;
            }
            SyscallStopInfo::Exit { context, result } => {
                if !administrative
                    && let Some((owner, _)) = state.raw_birth.take()
                    && !owner.same_generation(&task.terminal_cleanup())
                {
                    self.close();
                    return Err(Errno::ECHILD.into());
                }
                if !administrative
                    && let Some(step) = state.tool_step.as_mut()
                    && step.physical == ToolPhysical::Running
                {
                    let original = step.step.task.same_generation(&task.terminal_cleanup())
                        && step.step.entry_arch == Some(context.arch);
                    step.physical.returned(original)?;
                }
                let Some(mut attempt) = state.pending.take() else {
                    if administrative && !state.startup {
                        self.close();
                    }
                    state.startup = false;
                    return Ok(None);
                };
                if !attempt.task.same_generation(&task.terminal_cleanup())
                    || attempt.entry.arch != context.arch
                    || attempt.returned
                {
                    self.close();
                    return Err(Errno::EPROTO.into());
                }
                let task_owner = task.terminal_cleanup();
                if attempt.child && result >= 0 {
                    attempt.returned = true;
                    state.pending = Some(attempt); // parent/child restoration still owns it
                } else if let Some(owner) = attempt.native
                    && let Some(returned) = owner.syscall_return(task, result)
                {
                    returned.restored();
                }
                return Ok(Some(OriginalExit { task: task_owner }));
            }
            _ => {}
        }
        Ok(None)
    }
}

/// Only the observer consuming the original task's pending attempt constructs
/// this receipt. Syscall numbers/results cannot construct one.
pub(super) struct OriginalExit {
    task: TerminalCleanup,
}

/// Linear instruction completion. The timer consumes it exactly once; no
/// administrative wait or numeric SIGTRAP is itself an instruction receipt.
pub(crate) struct Completion {
    _task: TerminalCleanup,
    deferred: Option<crate::timer::DeferredStepCounter>,
}
impl Completion {
    pub(crate) fn consume(self, account: impl FnOnce()) {
        assert!(
            self.deferred.is_none(),
            "deferred Tool completion has a different counter owner"
        );
        account();
    }
    pub(crate) fn consume_deferred(self, clock: impl FnOnce() -> u64) {
        if let Some(counter) = self.deferred {
            counter.complete(clock());
        }
    }
    pub(super) fn legacy(task: &Stopped) -> Result<Option<Self>, TraceError> {
        Ok(matches!(
            task.getsiginfo()?.si_code,
            libc::TRAP_TRACE | libc::TRAP_BRKPT
        )
        .then(|| Self {
            _task: task.terminal_cleanup(),
            deferred: None,
        }))
    }
}

/// No public constructor for a completion: only TaskRunning's consumed wait
/// owner can return this outcome. A real guest event may accompany completion.
pub(crate) struct StepOutcome {
    pub(crate) wait: Wait,
    pub(crate) completion: Option<Completion>,
    pub(crate) guest_event: bool,
    pub(crate) transfer: Option<ToolTransfer>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StepPhase {
    Instruction,
    Native,
    Tool,
    Finished,
    Cancelled,
}
impl StepPhase {
    fn tool_entry(&mut self) -> Result<(), Errno> {
        if *self != Self::Native {
            return Err(Errno::EPROTO);
        }
        *self = Self::Tool;
        Ok(())
    }
    fn tool_return(&mut self) -> Result<(), Errno> {
        if *self != Self::Tool {
            return Err(Errno::EPROTO);
        }
        *self = Self::Finished;
        Ok(())
    }
    fn cancel(&mut self) -> Result<(), Errno> {
        if matches!(self, Self::Finished | Self::Cancelled) {
            return Err(Errno::EPROTO);
        }
        *self = Self::Cancelled;
        Ok(())
    }
    fn entry(&mut self) -> Result<(), Errno> {
        if *self != Self::Instruction {
            return Err(Errno::EPROTO);
        }
        *self = Self::Native;
        Ok(())
    }
    fn exit(&mut self, original_owner: bool) -> Result<(), Errno> {
        if *self != Self::Native || !original_owner {
            return Err(Errno::EPROTO);
        }
        *self = Self::Finished;
        Ok(())
    }
}
fn trap_outcome(
    guest_tf: bool,
    writes_tf: bool,
    owned_tf: bool,
    debug_step: bool,
    other_debug: bool,
) -> (bool, bool) {
    (
        owned_tf && (!debug_step || !writes_tf),
        !debug_step || guest_tf || other_debug,
    )
}
pub(super) struct Step {
    pub(super) phase: StepPhase,
    guest_tf: bool,
    owned_tf: bool,
    writes_tf: bool,
    task: TerminalCleanup,
    entry_arch: Option<u32>,
}
/// Same limited predecode as Linux step.c. No immutable-mm guarantee: both
/// decoders can race other writers; Linux documents the signal-before-POPF case.
pub(super) fn writes_tf(bytes: &[u8], mode64: bool) -> bool {
    for &b in bytes.iter().take(15) {
        match b {
            0x9d | 0xcf => return true,
            0x66 | 0x67 | 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0xf0 | 0xf2 | 0xf3 => {}
            0x40..=0x4f if mode64 => {}
            _ => return false,
        }
    }
    false
}
pub(super) fn clear_entry_tf(regs: &mut libc::user_regs_struct, arch: u32, owned: bool) {
    if owned {
        regs.eflags &= !TF;
        if arch == X86_64 {
            regs.r11 &= !TF;
        }
    }
}
fn debug_causes(dr6: u64) -> (bool, bool) {
    (dr6 & (1 << 14) != 0, dr6 & 15 != 0)
}
fn native_step_abi(cs: u64) -> bool {
    cs == 0x33
}
fn code_byte(
    ip: u64,
    offset: u64,
    mut read_word: impl FnMut(usize) -> Result<u64, Errno>,
) -> Result<u8, Errno> {
    let address = ip.checked_add(offset).ok_or(Errno::EFAULT)? as usize;
    // MemoryAccess's small reads use PEEKDATA, which reads a whole word.
    // Align it so a valid final-page byte does not spuriously cross into an
    // unmapped page. PEEKDATA, like the kernel decoder, uses FOLL_FORCE.
    let word = read_word(address & !7)?;
    Ok((word >> ((address & 7) * 8)) as u8)
}
impl Step {
    pub(super) fn begin(task: &Stopped) -> Result<Self, TraceError> {
        let mut regs = task.getregs()?;
        let guest_tf = regs.eflags & TF != 0;
        let mut bytes = Vec::new();
        // A short read at a mapping boundary follows the kernel's partial-read
        // classification, without requiring all fifteen bytes to be accessible.
        for offset in 0..15 {
            match code_byte(regs.rip, offset, |address| {
                let address = reverie::syscalls::Addr::from_raw(address).ok_or(Errno::EFAULT)?;
                task.read_value::<_, u64>(address)
            }) {
                Ok(byte) => bytes.push(byte),
                Err(_) => break,
            }
        }
        let writes_tf = writes_tf(&bytes, regs.cs == 0x33);
        regs.eflags |= TF;
        task.setregs(&regs)?; // explicitly clears any old kernel FORCED_TF owner
        Ok(Self {
            phase: StepPhase::Instruction,
            guest_tf,
            owned_tf: !guest_tf,
            writes_tf,
            task: task.terminal_cleanup(),
            entry_arch: None,
        })
    }
    pub(super) fn entry(&mut self, task: &Stopped, entry: SyscallEntry) -> Result<(), TraceError> {
        if self.phase != StepPhase::Instruction
            || !self.task.same_generation(&task.terminal_cleanup())
        {
            return Err(Errno::EPROTO.into());
        }
        let mut regs = task.getregs()?;
        clear_entry_tf(&mut regs, entry.arch, self.owned_tf);
        task.setregs(&regs)?;
        self.owned_tf = false;
        self.phase.entry()?;
        self.entry_arch = Some(entry.arch);
        Ok(())
    }
    pub(super) fn finish(
        &mut self,
        wait: &Wait,
        owned_exit: Option<OriginalExit>,
    ) -> Result<(Option<Completion>, bool), TraceError> {
        let Wait::Stopped(task, event) = wait else {
            return Ok((None, true));
        };
        if !self.task.same_generation(&task.terminal_cleanup()) {
            return Err(Errno::ECHILD.into());
        }
        if self.phase == StepPhase::Native
            && matches!(event, Event::Syscall)
            && let Some(exit) = owned_exit
        {
            self.phase.exit(self.task.same_generation(&exit.task))?;
            return Ok((
                Some(Completion {
                    _task: exit.task,
                    deferred: None,
                }),
                false,
            ));
        }
        let trace_signal = self.phase == StepPhase::Instruction
            && matches!(event, Event::Signal(nix::sys::signal::Signal::SIGTRAP))
            && task.getsiginfo()?.si_code == libc::TRAP_TRACE;
        let (debug_step, other_debug) = if trace_signal {
            // get_si_code prioritizes DR_STEP over simultaneous DR_TRAPn.
            // Read the original stopped task's virtual DR6; siginfo alone can
            // hide a real guest hardware breakpoint behind our tracer TF.
            let offset = std::mem::offset_of!(libc::user, u_debugreg) + 6 * 8;
            let dr6 = nix::sys::ptrace::read_user(task.pid().into(), offset as *mut libc::c_void)
                .map_err(|e| Errno::new(e as i32))? as u64;
            debug_causes(dr6)
        } else {
            (false, false)
        };
        // Poststate TF is independent of whether the *preceding* trap is guest
        // owned. POPF can enable TF without making our current trap a guest event.
        let (clear_tf, guest_event) = trap_outcome(
            self.guest_tf,
            self.writes_tf,
            self.owned_tf,
            debug_step,
            other_debug,
        );
        if clear_tf {
            let mut regs = task.getregs()?;
            regs.eflags &= !TF;
            task.setregs(&regs)?;
        }
        self.owned_tf = false;
        if debug_step {
            self.phase = StepPhase::Finished;
        }
        Ok((
            debug_step.then(|| Completion {
                _task: task.terminal_cleanup(),
                deferred: None,
            }),
            guest_event,
        ))
    }
}

#[cfg(test)]
mod source_observation_tests {
    use super::*;

    fn entry(arch: u32, number: u64) -> SyscallEntry {
        SyscallEntry {
            arch,
            number,
            instruction_pointer: 0x10002,
            stack_pointer: 0x20000,
            arguments: [1, 2, 3, 4, 5, 6],
            seccomp: false,
        }
    }
    #[test]
    fn simultaneous_guest_debug_is_not_hidden_by_tracer_dr_step() {
        assert_eq!(debug_causes(1 << 14), (true, false));
        assert_eq!(debug_causes((1 << 14) | 1), (true, true));
        assert_eq!(debug_causes(1), (false, true));
        assert_eq!(debug_causes(0), (false, false));
        assert_eq!(trap_outcome(false, false, true, true, true), (true, true));
        assert_eq!(trap_outcome(false, true, true, true, true), (false, true));
    }
    #[test]
    fn predecode_reads_the_last_mapped_byte_without_crossing_a_word() {
        let read = |address| {
            if address == 4088 {
                Ok(0x9d00_0000_0000_0000)
            } else {
                Err(Errno::EFAULT)
            }
        };
        assert_eq!(code_byte(4095, 0, read), Ok(0x9d));
        assert_eq!(code_byte(4095, 1, read), Err(Errno::EFAULT));
        assert!(writes_tf(&[code_byte(4095, 0, read).unwrap()], true));
        assert_eq!(code_byte(u64::MAX, 1, read), Err(Errno::EFAULT));
        assert!(native_step_abi(0x33));
        assert!(!native_step_abi(0x23));
        assert!(!native_step_abi(0x7));
    }
    #[test]
    fn wait_kind_and_kernel_direction_must_agree() {
        let entry = entry(X86_64, Sysno::getpid as u64);
        assert!(matching_stop(
            &Event::Syscall,
            SyscallStopInfo::Entry(entry)
        ));
        assert!(!matching_stop(
            &Event::Syscall,
            SyscallStopInfo::Seccomp(entry)
        ));
        assert!(!matching_stop(
            &Event::Seccomp,
            SyscallStopInfo::Entry(entry)
        ));
        assert!(!matching_stop(
            &Event::Signal(nix::sys::signal::Signal::SIGTRAP),
            SyscallStopInfo::Entry(entry)
        ));
    }
    #[test]
    fn actual_entry_is_not_completion_and_exit_consumes_once() {
        let mut phase = StepPhase::Instruction;
        assert_eq!(phase.exit(true), Err(Errno::EPROTO));
        phase.entry().unwrap();
        assert_eq!(phase, StepPhase::Native);
        assert_eq!(phase.entry(), Err(Errno::EPROTO));
        assert_eq!(phase.exit(false), Err(Errno::EPROTO));
        assert_eq!(phase, StepPhase::Native);
        phase.exit(true).unwrap();
        assert_eq!(phase, StepPhase::Finished);
        assert_eq!(phase.exit(true), Err(Errno::EPROTO));
    }
    #[test]
    fn seccomp_transfers_instead_of_completing_or_cancelling_the_instruction() {
        let mut step = StepPhase::Instruction;
        assert_eq!(step.tool_entry(), Err(Errno::EPROTO));
        step.entry().unwrap();
        step.tool_entry().unwrap();
        assert_eq!(step, StepPhase::Tool);
        // Neither a private EXIT nor a second ENTRY owns the logical step.
        assert_eq!(step.exit(true), Err(Errno::EPROTO));
        assert_eq!(step.entry(), Err(Errno::EPROTO));
        step.tool_return().unwrap();
        assert_eq!(step, StepPhase::Finished);
        assert_eq!(step.tool_return(), Err(Errno::EPROTO));
        assert_eq!(step.cancel(), Err(Errno::EPROTO));
        let mut interrupted = StepPhase::Native;
        interrupted.tool_entry().unwrap();
        interrupted.cancel().unwrap();
        assert_eq!(interrupted.tool_return(), Err(Errno::EPROTO));
        assert_eq!(interrupted.exit(true), Err(Errno::EPROTO));
    }
    #[test]
    fn tool_original_effect_requires_its_resume_and_one_authenticated_return() {
        let mut physical = ToolPhysical::Held;
        assert_eq!(physical.returned(true), Err(Errno::EPROTO));
        physical.resume().unwrap();
        assert_eq!(physical.returned(false), Err(Errno::EPROTO));
        assert_eq!(physical, ToolPhysical::Running);
        physical.returned(true).unwrap();
        assert_eq!(physical, ToolPhysical::Returned);
        assert_eq!(physical.resume(), Err(Errno::EPROTO));
        assert_eq!(physical.returned(true), Err(Errno::EPROTO));
    }
    #[test]
    fn real_tool_event_cancels_once_and_private_exit_cannot_complete_it() {
        for entered in [false, true] {
            let mut phase = StepPhase::Instruction;
            if entered {
                phase.entry().unwrap();
            }
            phase.cancel().unwrap();
            assert_eq!(phase, StepPhase::Cancelled);
            assert_eq!(phase.cancel(), Err(Errno::EPROTO));
            assert_eq!(phase.entry(), Err(Errno::EPROTO));
            assert_eq!(phase.exit(true), Err(Errno::EPROTO));
        }
    }
    #[test]
    fn clear_tracer_tf_also_clears_only_native_r11_copy() {
        let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        regs.eflags = 0x202 | TF;
        regs.r11 = 0xabcdef00 | TF;
        regs.rcx = 0x1234;
        regs.rip = 0x1234;
        regs.orig_rax = 56;
        regs.rax = (-38i64) as u64;
        regs.rdi = 0x800011;
        regs.rsi = 0x998877;
        let before = regs;
        clear_entry_tf(&mut regs, X86_64, true);
        let mut expected = before;
        expected.eflags &= !TF;
        expected.r11 &= !TF;
        let actual: [u64; 27] = unsafe { std::mem::transmute(regs) };
        let expected: [u64; 27] = unsafe { std::mem::transmute(expected) };
        assert_eq!(actual, expected);
    }
    #[test]
    fn guest_tf_and_r11_are_never_cleaned() {
        let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        regs.eflags = 0x202 | TF;
        regs.r11 = 0x123456ff | TF;
        let before: [u64; 27] = unsafe { std::mem::transmute(regs) };
        clear_entry_tf(&mut regs, X86_64, false);
        let after: [u64; 27] = unsafe { std::mem::transmute(regs) };
        assert_eq!(before, after);
    }
    #[test]
    fn compat_entry_does_not_treat_r11_as_native_flag_copy() {
        let mut regs: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        regs.eflags = TF | 0x202;
        regs.r11 = TF | 0x1234;
        clear_entry_tf(&mut regs, 0x40000003, true);
        assert_eq!(regs.eflags, 0x202);
        assert_eq!(regs.r11, TF | 0x1234);
        assert!(parts(entry(0x40000003, 56)).is_none());
        assert!(parts(entry(X86_64, 0x40000038)).is_none());
        assert!(unsupported_entry(entry(0x40000003, 120)));
        assert!(unsupported_entry(entry(X86_64, 0x40000038)));
        assert!(!unsupported_entry(entry(X86_64, u64::MAX)));
        assert!(!unsupported_entry(entry(X86_64, Sysno::getpid as u64)));
    }
    #[test]
    fn tf_writing_instruction_poststate_and_trap_ownership_are_independent() {
        assert_eq!(trap_outcome(false, true, true, true, false), (false, false));
        assert_eq!(trap_outcome(true, true, false, true, false), (false, true));
        assert_eq!(trap_outcome(false, false, true, true, false), (true, false));
        assert_eq!(trap_outcome(true, false, false, true, false), (false, true));
        // A real pre-delivery signal/fault is never a timer completion.
        assert_eq!(trap_outcome(false, true, true, false, false), (true, true));
        assert_eq!(trap_outcome(true, true, false, false, false), (false, true));
    }
    #[test]
    fn kernel_prefix_predecode_is_bounded_and_abi_sensitive() {
        assert!(writes_tf(&[0x66, 0xf3, 0x48, 0x9d], true));
        assert!(!writes_tf(&[0x48, 0x9d], false));
        assert!(writes_tf(&[0xcf], true));
        assert!(!writes_tf(&[0x0f, 0x05, 0x9d], true));
        assert!(!writes_tf(&[0x9c], true));
        let mut bytes = [0x66; 16];
        bytes[15] = 0x9d;
        assert!(!writes_tf(&bytes, true));
        assert!(!writes_tf(&[], true));
    }
    #[test]
    fn tool_opportunity_precedes_actual_native_admission() {
        let all = Subscription::all();
        assert!(tool_entry(entry(X86_64, Sysno::read as u64), &all));
        assert!(!tool_entry(entry(X86_64, Sysno::rt_sigreturn as u64), &all));
        let mut private = entry(X86_64, Sysno::read as u64);
        private.instruction_pointer = (cp::TRAMPOLINE_BASE + cp::SYSCALL_INSTR_SIZE) as u64;
        assert!(!tool_entry(private, &all));
        assert!(!tool_entry(entry(0x40000003, Sysno::read as u64), &all));
    }
}
