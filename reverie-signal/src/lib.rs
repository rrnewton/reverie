/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

#![allow(
    unexpected_cfgs,
    reason = "fbcode_build is supplied by the internal Buck build"
)]

//! A pure model of Linux signal semantics (kernel/signal.c), shared by
//! every backend. The domain owns masks, pending instances, dispositions,
//! alternate stacks, `curr_target` and the restart mapping. Callers supply
//! scheduling facts (running thread, liveness, job-control stop, delivery point);
//! nothing here performs I/O or names a backend or a consumer.

use std::collections::VecDeque;

pub mod frame;

pub const SIGKILL: i32 = 9;
pub const SIGSTOP: i32 = 19;
pub const SIGCONT: i32 = 18;
pub const NSIG: i32 = 64;

pub const SA_NOCLDSTOP: u32 = 0x0000_0001;
pub const SA_NOCLDWAIT: u32 = 0x0000_0002;
pub const SA_SIGINFO: u32 = 0x0000_0004;
pub const SA_ONSTACK: u32 = 0x0800_0000;
pub const SA_RESTART: u32 = 0x1000_0000;
pub const SA_NODEFER: u32 = 0x4000_0000;
pub const SA_RESETHAND: u32 = 0x8000_0000;
pub const SA_RESTORER: u32 = 0x0400_0000;

const UNBLOCKABLE_MASK: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));

pub fn is_valid(signo: i32) -> bool {
    (1..=NSIG).contains(&signo)
}
pub fn is_realtime(signo: i32) -> bool {
    signo >= 32
}
pub fn is_stop(signo: i32) -> bool {
    matches!(signo, 19..=22)
}
pub fn is_unblockable(signo: i32) -> bool {
    signo == SIGKILL || signo == SIGSTOP
}
fn bit(signo: i32) -> u64 {
    1u64 << (signo - 1)
}

/// Linux default-action classes (`sig_kernel_*`, kernel/signal.c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultAction {
    Terminate,
    TerminateCore,
    Ignore,
    Stop,
    Continue,
}

pub fn default_action(signo: i32) -> DefaultAction {
    match signo {
        3 | 4 | 5 | 6 | 7 | 8 | 11 | 24 | 25 | 31 => DefaultAction::TerminateCore,
        17 | 23 | 28 => DefaultAction::Ignore,
        19..=22 => DefaultAction::Stop,
        18 => DefaultAction::Continue,
        _ => DefaultAction::Terminate,
    }
}

/// The siginfo fields the semantics need. Backends convert to/from the
/// guest ABI (`siginfo_t`) at their edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    pub value: i64,
    pub pid: i32,
    pub uid: u32,
    pub addr: u64,
    /// `si_overrun` for POSIX timer records; 0 otherwise.
    pub overrun: i32,
}

impl SigInfo {
    pub fn user(signo: i32, pid: i32, uid: u32) -> Self {
        Self {
            signo,
            code: 0,
            value: 0,
            pid,
            uid,
            addr: 0,
            overrun: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigAction {
    pub kind: DispositionKind,
    pub handler: u64,
    pub mask: u64,
    pub flags: u32,
    pub restorer: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispositionKind {
    Default,
    Ignore,
    Handler,
}

impl Default for SigAction {
    fn default() -> Self {
        Self {
            kind: DispositionKind::Default,
            handler: 0,
            mask: 0,
            flags: 0,
            restorer: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AltStack {
    pub base: u64,
    pub size: u64,
    pub disabled: bool,
    pub autodisarm: bool,
}

/// How a pending instance is accounted. These are four distinct Linux
/// things and must never be conflated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceKind {
    /// Holds one sigqueue allocation charged to this receiver UID
    /// (`q->ucounts`), freed to that same owner however it leaves.
    Charged(u32),
    /// A pending bit with no sigqueue record (allocation-failure
    /// fallback); a later allocation is absorbed into it.
    FallbackBit,
    /// A POSIX timer's preallocated record, by timer id. Repeated
    /// expiry raises `si_overrun` on this one record.
    TimerPrealloc(u64),
    /// SIGKILL: delivered without any allocation.
    UnchargedKill,
}

/// One pending instance; `id` is unique within its queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: u64,
    pub info: SigInfo,
    pub kind: InstanceKind,
}

/// A keyed reference to one pending instance on one task generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingRef {
    pub task: TaskGen,
    pub queue: QueueKind,
    pub instance: u64,
    pub signo: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueKind {
    Private,
    Shared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskGen {
    pub tid: i32,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueResult {
    Queued(u64),
    /// Standard signal already pending; the first siginfo is kept.
    Coalesced(u64),
    /// Allocation failed and Linux refuses this source (EAGAIN).
    Overflow,
    /// Discarded at generation: ignored disposition, unblocked.
    DiscardedIgnored,
}

/// A send's result: the queue effect plus SIGCONT's unconditional
/// generation-time continuation, independent of later delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendOutcome {
    pub queue: QueueResult,
    pub continued: bool,
}

/// A send's origin, which decides allocation-failure behaviour
/// (`__send_signal_locked`): the limit may be overridden, a pending bit
/// may be preserved without siginfo, or the send fails EAGAIN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendSource {
    /// kill(2) (SI_USER = 0).
    UserKill,
    /// tgkill/tkill (SI_TKILL = -6).
    ThreadKill,
    /// sigqueue/message (SI_QUEUE/SI_MESGQ < 0).
    SigQueue,
    /// POSIX timer expiry (SI_TIMER) for this timer id: the sigqueue
    /// record is preallocated at timer_create, so delivery never
    /// allocates; repeated expiry raises si_overrun on it.
    TimerPrealloc(u64),
    /// Ordinary kernel siginfo (si_code > 0, e.g. child-exit CLD_*).
    KernelOrdinary,
    /// SEND_SIG_PRIV: the kernel's special input, distinct from the above.
    KernelSpecial,
}

/// Signals in Linux's SYNCHRONOUS_MASK (`next_signal`): SIGTRAP,
/// SIGBUS, SIGFPE, SIGILL, SIGSEGV, SIGSYS. (SIGXCPU/SIGXFSZ are not
/// in the mask.)
pub fn is_synchronous(signo: i32) -> bool {
    matches!(signo, 4 | 5 | 7 | 8 | 11 | 31)
}

/// One pending queue: a thread's private queue, or the shared queue.
#[derive(Debug, Clone, Default)]
pub struct SignalQueue {
    entries: VecDeque<Pending>,
    next_id: u64,
}

impl SignalQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// The pending instance of `signo`, if one is queued here.
    pub fn find(&self, signo: i32) -> Option<u64> {
        self.entries
            .iter()
            .find(|p| p.info.signo == signo)
            .map(|p| p.id)
    }

    /// Append one instance. Coalescing, flushing and allocation
    /// accounting are the domain's (they span queues and users).
    pub fn push(&mut self, info: SigInfo, kind: InstanceKind) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push_back(Pending { id, info, kind });
        id
    }

    /// The unallocated fallback instance of `signo`, if any (a pending
    /// bit without a sigqueue record). Preallocated timer records are
    /// real records and never match.
    pub fn find_fallback_mut(&mut self, signo: i32) -> Option<&mut Pending> {
        self.entries
            .iter_mut()
            .find(|p| p.info.signo == signo && p.kind == InstanceKind::FallbackBit)
    }

    /// Drop every instance whose signo is in `mask`; returns the count of
    /// dropped instances that held an allocation (`flush_sigqueue_mask`).
    pub fn flush_mask(&mut self, mask: u64) -> Vec<u32> {
        let mut released = Vec::new();
        self.entries.retain(|p| {
            if mask & bit(p.info.signo) != 0 {
                if let InstanceKind::Charged(uid) = p.kind {
                    released.push(uid);
                }
                false
            } else {
                true
            }
        });
        released
    }

    /// Dequeue for an rt_sigtimedwait-style wait: among entries whose
    /// signo is in `wait_set` (SIGKILL/SIGSTOP stripped by the caller),
    /// next_signal order. The thread's blocked mask plays no part.
    pub fn dequeue_waited(&mut self, wait_set: u64) -> Option<Pending> {
        let mut best: Option<usize> = None;
        for (i, p) in self.entries.iter().enumerate() {
            if is_unblockable(p.info.signo) || wait_set & bit(p.info.signo) == 0 {
                continue;
            }
            best = Some(match best {
                None => i,
                Some(b) => {
                    let cur = &self.entries[b];
                    let key = |x: &Pending| (!is_synchronous(x.info.signo), x.info.signo, x.id);
                    if key(p) < key(cur) { i } else { b }
                }
            });
        }
        best.and_then(|i| self.entries.remove(i))
    }

    /// First (FIFO) entry matching `pred` and not blocked.
    pub fn dequeue_first(
        &mut self,
        blocked: u64,
        pred: impl Fn(&Pending) -> bool,
    ) -> Option<Pending> {
        let i = self.entries.iter().position(|p| {
            (is_unblockable(p.info.signo) || blocked & bit(p.info.signo) == 0) && pred(p)
        })?;
        self.entries.remove(i)
    }

    /// `next_signal` order: synchronous signals first, then lowest
    /// signal number, then FIFO. Blocked signals wait; SIGKILL/SIGSTOP
    /// ignore the mask.
    pub fn dequeue(&mut self, blocked: u64) -> Option<Pending> {
        let mut best: Option<usize> = None;
        for (i, p) in self.entries.iter().enumerate() {
            if !is_unblockable(p.info.signo) && blocked & bit(p.info.signo) != 0 {
                continue;
            }
            best = Some(match best {
                None => i,
                Some(b) => {
                    let cur = &self.entries[b];
                    // next_signal order within one queue: synchronous
                    // class, then lowest number, then FIFO. The
                    // positive-code synchronous pass is private-only and
                    // lives in the domain (dequeue_first), never here.
                    let key = |x: &Pending| (!is_synchronous(x.info.signo), x.info.signo, x.id);
                    if key(p) < key(cur) { i } else { b }
                }
            });
        }
        best.and_then(|i| self.entries.remove(i))
    }

    pub fn peek_mask(&self) -> u64 {
        self.entries.iter().fold(0, |m, p| m | bit(p.info.signo))
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Per-thread signal state owned by the domain.
#[derive(Debug, Clone)]
pub struct ThreadState {
    pub task: TaskGen,
    pub private: SignalQueue,
    pub blocked: u64,
    /// `real_blocked`: set only by rt_sigtimedwait-family waits;
    /// sig_ignored retention consults it.
    pub real_blocked: Option<u64>,
    /// `saved_sigmask`: set by sigsuspend-family waits; restored at
    /// sigreturn. Retention does NOT consult it.
    pub saved_sigmask: Option<u64>,
    pub altstack: Option<AltStack>,
    /// Per-thread notification (`TIF_SIGPENDING`): set when a send
    /// targets this thread or receiver selection chooses it; cleared
    /// when no unblocked work remains for it (recalc_sigpending).
    pub sigpending: bool,
}

impl ThreadState {
    pub fn new(task: TaskGen) -> Self {
        Self {
            task,
            private: SignalQueue::new(),
            blocked: 0,
            real_blocked: None,
            saved_sigmask: None,
            altstack: None,
            sigpending: false,
        }
    }
    /// The mask that decides delivery: the temporary mask while in such a
    /// wait, else the ordinary blocked mask.
    pub fn effective_blocked(&self) -> u64 {
        // During a temporary-mask wait `blocked` already holds the wait mask;
        // `real_blocked` preserves the pre-wait mask for sigreturn.
        self.blocked & !UNBLOCKABLE_MASK
    }
}

/// Scheduling facts the caller supplies at a delivery point. Nothing here
/// is signal state: masks and pending live in the domain.
#[derive(Debug, Clone, Copy)]
pub struct SchedFact {
    pub task: TaskGen,
    /// Currently running on a CPU (`task_curr`); a caller that serializes
    /// execution reports its one running thread.
    pub running: bool,
    pub exiting: bool,
    /// The guest-visible job-control stop, never a tracer stop.
    pub stopped: bool,
}

/// Unblocked pending work by mask alone (the recalc_sigpending sum).
fn unblocked_pending(t: &ThreadState, shared: &SignalQueue) -> bool {
    (t.private.peek_mask() | shared.peek_mask()) & !t.effective_blocked() != 0
}

fn has_unblocked_pending(t: &ThreadState, shared: &SignalQueue) -> bool {
    // task_sigpending is the per-thread notification flag, not a scan of
    // the shared queue: a fresh shared send has notified nobody yet.
    t.sigpending && unblocked_pending(t, shared)
}

fn wants_signal(signo: i32, t: &ThreadState, shared: &SignalQueue, f: &SchedFact) -> bool {
    if f.exiting {
        return false;
    }
    // Linux returns true for SIGKILL here, before the stopped and
    // pending tests (wants_signal, kernel/signal.c:946ff): it wakes
    // stopped tasks and is never masked out.
    if signo == SIGKILL {
        return true;
    }
    if t.effective_blocked() & bit(signo) != 0 {
        return false;
    }
    // SIGCONT also wakes stopped tasks (TASK_WAKEKILL).
    if f.stopped && signo != SIGCONT {
        return false;
    }
    // task_sigpending: pending work counts only when unblocked.
    f.running || !has_unblocked_pending(t, shared)
}

/// Caller-supplied frame environment at a delivery point.
#[derive(Debug, Clone, Copy)]
pub struct FrameEnv {
    pub sp_now: u64,
    /// The presented xstate description (never the host's).
    pub xstate: frame::XstateDesc,
    /// Lowest usable main-stack address (altstack bounds govern there).
    pub stack_lower_bound: u64,
}

pub const SS_AUTODISARM: i32 = i32::MIN;

/// Frame-entry policy produced by the domain; frame bytes are the
/// backend's. Fit is decided here from the presented xstate size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequest {
    pub handler: u64,
    pub handler_mask: u64,
    /// Written to `uc_sigmask`; the pre-wait mask for temporary-mask waits.
    pub restore_mask: u64,
    pub use_altstack: bool,
    pub on_altstack_now: bool,
    pub restorer: Option<u64>,
    pub xstate_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartCode {
    RestartSys,
    RestartNoHand,
    RestartNoIntr,
    RestartRestartBlock,
    Intr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResolution {
    Eintr,
    Restart,
    /// RESTARTBLOCK resumes at the original absolute deadline.
    RestartAbs {
        deadline_ns: u64,
    },
    /// RESTARTBLOCK with no deadline (poll with infinite timeout):
    /// restart through restart_syscall. Distinct from Restart and
    /// never an invented EINTR.
    RestartSyscall,
    ContinueWait,
}

/// Map an executor-supplied ERESTART* code to an outcome. The executor
/// (kernel, emulated wait, or emulation layer) supplies the code because
/// it depends on the interrupted call's arguments and progress.
pub fn map_restart(
    code: RestartCode,
    has_handler: bool,
    sa_restart: bool,
    restart_deadline_ns: Option<u64>,
) -> WaitResolution {
    if !has_handler {
        return match code {
            RestartCode::Intr => WaitResolution::Eintr,
            // No deadline (e.g. poll with infinite timeout): still a
            // restart through restart_syscall, never an invented EINTR.
            RestartCode::RestartRestartBlock => match restart_deadline_ns {
                Some(d) => WaitResolution::RestartAbs { deadline_ns: d },
                None => WaitResolution::RestartSyscall,
            },
            _ => WaitResolution::Restart,
        };
    }
    match code {
        RestartCode::RestartNoIntr => WaitResolution::Restart,
        RestartCode::RestartSys if sa_restart => WaitResolution::Restart,
        RestartCode::RestartRestartBlock => WaitResolution::Eintr,
        _ => WaitResolution::Eintr,
    }
}

/// What the domain resolved for one pending instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    EnterHandler {
        frame: FrameRequest,
    },
    ApplyDefault {
        action: DefaultAction,
    },
    GroupExit,
    Discard,
    /// Frame does not fit / no restorer: a Linux outcome, not a refusal.
    ForceSegv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryPlan {
    pub instance: PendingRef,
    pub receiver: TaskGen,
    /// The full siginfo of the dequeued instance, `si_value` included.
    pub info: SigInfo,
    pub delivery: Delivery,
}

/// `RLIMIT_SIGPENDING` accounting: sigqueue allocations are charged to
/// the *receiver's* real-UID accounting and checked against the
/// receiver's limit (`__sigqueue_alloc`, kernel/signal.c:402ff), shared
/// by every process of that user — never per signal number or queue.
/// One ledger serves a whole run; domains allocate through it.
#[derive(Debug, Clone, Default)]
pub struct Sigpending {
    used: std::collections::BTreeMap<u32, usize>,
}

impl Sigpending {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn try_alloc(&mut self, receiver_uid: u32, receiver_limit: usize) -> bool {
        let n = self.used.entry(receiver_uid).or_insert(0);
        if *n >= receiver_limit {
            return false;
        }
        *n += 1;
        true
    }
    /// Charge without testing the limit (Linux overrides it for
    /// kernel-originated and standard SI_USER sends).
    pub fn alloc_over_limit(&mut self, receiver_uid: u32) {
        *self.used.entry(receiver_uid).or_insert(0) += 1;
    }
    pub fn release(&mut self, uid: u32, count: usize) {
        if let Some(n) = self.used.get_mut(&uid) {
            *n = n.saturating_sub(count);
        }
    }
    pub fn used_by(&self, uid: u32) -> usize {
        self.used.get(&uid).copied().unwrap_or(0)
    }
}

/// One process's signal domain: shared dispositions and queue, threads in
/// creation (thread-list) order, and `curr_target` history.
///
/// Provisional, named: periodic-timer ignore/unignore transitions
/// (posixtimer_sig_ignore) are not modelled yet; send_thread notifies
/// without consulting job-control stop state (receiver selection is
/// where stopped targets are excluded); and __set_task_blocked's
/// retarget_shared_pending (waking another thread for a shared signal
/// the mask change strands) is not modelled. Tracked for step 3.
#[derive(Debug, Clone)]
pub struct ProcessDomain {
    pub actions: [SigAction; NSIG as usize],
    pub shared: SignalQueue,
    pub threads: Vec<ThreadState>,
    pub curr_target: Option<TaskGen>,
    /// Real UID this process's sigqueue allocations charge to.
    pub uid: u32,
    /// This process's `RLIMIT_SIGPENDING`.
    pub sigpending_limit: usize,
}

impl ProcessDomain {
    pub fn new(leader: TaskGen, uid: u32, sigpending_limit: usize) -> Self {
        Self {
            actions: [SigAction::default(); NSIG as usize],
            shared: SignalQueue::new(),
            threads: vec![ThreadState::new(leader)],
            curr_target: Some(leader),
            uid,
            sigpending_limit,
        }
    }

    /// Seed initial dispositions (the exec/startup table). Only the kinds
    /// are taken for ignored signals, as Linux preserves SIG_IGN at exec.
    pub fn seed_initial(&mut self, ignored_mask: u64) {
        for signo in 1..=NSIG {
            if ignored_mask & bit(signo) != 0 && !is_unblockable(signo) {
                self.actions[(signo - 1) as usize].kind = DispositionKind::Ignore;
            }
        }
    }

    /// Install a disposition (`do_sigaction`): installing SIG_IGN flushes
    /// that signal's pending instances from the shared queue and the
    /// installing thread's private queue.
    pub fn set_action(&mut self, signo: i32, act: SigAction, by: TaskGen, ledger: &mut Sigpending) {
        if !is_valid(signo) || is_unblockable(signo) {
            return;
        }
        self.actions[(signo - 1) as usize] = act;
        let _ = by;
        // do_sigaction flushes for SIG_IGN and for default-ignored
        // SIG_DFL alike, in the shared queue and EVERY private queue.
        let ignored = self.disposition_ignored(signo);
        if ignored {
            let mut released = self.shared.flush_mask(bit(signo));
            for t in &mut self.threads {
                released.extend(t.private.flush_mask(bit(signo)));
            }
            for uid in released {
                ledger.release(uid, 1);
            }
        }
    }

    pub fn thread_mut(&mut self, task: TaskGen) -> Option<&mut ThreadState> {
        self.threads.iter_mut().find(|t| t.task == task)
    }
    pub fn thread(&self, task: TaskGen) -> Option<&ThreadState> {
        self.threads.iter().find(|t| t.task == task)
    }

    /// `prepare_signal`: a stop signal flushes SIGCONT, SIGCONT flushes
    /// every stop signal — in the shared queue and every private queue
    /// (`flush_sigqueue_mask` on both, kernel/signal.c). Released
    /// allocations are returned per sender uid below, at dequeue time
    /// the ledger only ever decremented by counted instances; flushed
    /// instances release theirs here.
    fn prepare_flush(&mut self, signo: i32, ledger: &mut Sigpending) {
        let mask = if is_stop(signo) {
            bit(SIGCONT)
        } else if signo == SIGCONT {
            bit(19) | bit(20) | bit(21) | bit(22)
        } else {
            return;
        };
        // Each flushed instance frees its original charged owner,
        // which survives later credential changes (q->ucounts).
        let mut released = self.shared.flush_mask(mask);
        for t in &mut self.threads {
            released.extend(t.private.flush_mask(mask));
        }
        for uid in released {
            ledger.release(uid, 1);
        }
    }

    /// `__send_signal_locked` (kernel/signal.c:1070ff): coalesce,
    /// allocate against the receiver, queue. Linux's limit-override
    /// predicate is `sig < SIGRTMIN && (is_si_special || si_code >= 0)`;
    /// SIGKILL never allocates. Without an allocation, RT sigqueue and
    /// thread-kill sends and ordinary positive-code kernel siginfo fail,
    /// while kill(2) and special input preserve one pending instance
    /// with synthesized siginfo. A later successful allocation is
    /// absorbed into that fallback instance (collect_signal clears the
    /// bit while delivering the allocated record).
    fn send_into(
        queue: &mut SignalQueue,
        info: SigInfo,
        source: SendSource,
        ledger: &mut Sigpending,
        uid: u32,
        limit: usize,
    ) -> QueueResult {
        let signo = info.signo;
        // Timer identity precedes ordinary coalescing: each timer's
        // preallocated record is distinct, standard signal or not.
        if let SendSource::TimerPrealloc(timer_id) = source {
            // One record per timer: re-expiry bumps si_overrun in place.
            if let Some(p) = queue
                .entries
                .iter_mut()
                .find(|p| p.kind == InstanceKind::TimerPrealloc(timer_id))
            {
                p.info.overrun = p.info.overrun.saturating_add(1);
                return QueueResult::Queued(p.id);
            }
            // A fallback bit for this signo is absorbed into the timer
            // record: one delivery, not two (collect_signal).
            if let Some(p) = queue.find_fallback_mut(signo) {
                p.info = info;
                p.kind = InstanceKind::TimerPrealloc(timer_id);
                return QueueResult::Queued(p.id);
            }
            return QueueResult::Queued(queue.push(info, InstanceKind::TimerPrealloc(timer_id)));
        }
        if !is_realtime(signo)
            && let Some(id) = queue.find(signo)
        {
            return QueueResult::Coalesced(id);
        }
        // Absorb an unallocated fallback of the same signo.
        let has_fallback = queue.find_fallback_mut(signo).is_some();
        if signo == SIGKILL {
            if has_fallback {
                let p = queue.find_fallback_mut(signo).unwrap();
                p.info = info;
                return QueueResult::Queued(p.id);
            }
            return QueueResult::Queued(queue.push(info, InstanceKind::UnchargedKill));
        }
        let overrides = !is_realtime(signo)
            && matches!(
                source,
                SendSource::KernelSpecial | SendSource::KernelOrdinary | SendSource::UserKill
            );
        let allocated = ledger.try_alloc(uid, limit);
        if allocated || overrides {
            if !allocated {
                ledger.alloc_over_limit(uid);
            }
            if has_fallback {
                let p = queue.find_fallback_mut(signo).unwrap();
                p.info = info;
                p.kind = InstanceKind::Charged(uid);
                return QueueResult::Queued(p.id);
            }
            return QueueResult::Queued(queue.push(info, InstanceKind::Charged(uid)));
        }
        match source {
            SendSource::UserKill | SendSource::KernelSpecial => match queue.find(signo) {
                Some(id) => QueueResult::Coalesced(id),
                None => QueueResult::Queued(queue.push(
                    SigInfo {
                        signo,
                        code: 0,
                        value: 0,
                        pid: 0,
                        uid: 0,
                        addr: 0,
                        overrun: 0,
                    },
                    InstanceKind::FallbackBit,
                )),
            },
            SendSource::ThreadKill
            | SendSource::SigQueue
            | SendSource::KernelOrdinary
            | SendSource::TimerPrealloc(_) => {
                if is_realtime(signo) {
                    QueueResult::Overflow
                } else {
                    // Standard queue/thread-kill at the limit: pending
                    // instance preserved without siginfo.
                    match queue.find(signo) {
                        Some(id) => QueueResult::Coalesced(id),
                        None => QueueResult::Queued(queue.push(
                            SigInfo {
                                signo,
                                code: 0,
                                value: 0,
                                pid: 0,
                                uid: 0,
                                addr: 0,
                                overrun: 0,
                            },
                            InstanceKind::FallbackBit,
                        )),
                    }
                }
            }
        }
    }

    /// Pending-delivery ignore classification: explicit SIG_IGN, or
    /// SIG_DFL with a kernel-ignore default (SIG_KERNEL_IGNORE_MASK:
    /// CHLD, CONT, URG, WINCH — SIGCONT's default *action* is Continue,
    /// but for retention it classifies as ignore).
    fn disposition_ignored(&self, signo: i32) -> bool {
        let act = self.actions[(signo - 1) as usize];
        act.kind == DispositionKind::Ignore
            || act.kind == DispositionKind::Default && matches!(signo, 17 | 18 | 23 | 28)
    }

    /// sig_ignored retention: blocked in the live OR the saved mask
    /// retains the signal (signal.c:106ff).
    fn retained_for(t: &ThreadState, signo: i32) -> bool {
        (t.blocked | t.real_blocked.unwrap_or(0)) & bit(signo) != 0
    }

    /// Queue a process-directed send on the shared queue.
    pub fn send_process(
        &mut self,
        info: SigInfo,
        source: SendSource,
        ledger: &mut Sigpending,
    ) -> SendOutcome {
        // Generation effects first (prepare_signal): cancellation and
        // SIGCONT continuation happen even for ignored dispositions;
        // retention is decided afterwards.
        self.prepare_flush(info.signo, ledger);
        let continued = info.signo == SIGCONT;
        // sig_ignored tests the suggested (named) target — here the
        // leader — before receiver selection, not every thread.
        let leader_retains = self
            .threads
            .first()
            .map(|t| Self::retained_for(t, info.signo))
            .unwrap_or(false);
        if self.disposition_ignored(info.signo) && !leader_retains {
            return SendOutcome {
                queue: QueueResult::DiscardedIgnored,
                continued,
            };
        }
        SendOutcome {
            queue: Self::send_into(
                &mut self.shared,
                info,
                source,
                ledger,
                self.uid,
                self.sigpending_limit,
            ),
            continued,
        }
    }

    /// Queue a thread-directed send on the target's private queue.
    pub fn send_thread(
        &mut self,
        task: TaskGen,
        info: SigInfo,
        source: SendSource,
        ledger: &mut Sigpending,
    ) -> Option<SendOutcome> {
        // Resolve the target before any preparation: a stale generation
        // must not flush live signals (do_send_specific resolves first).
        let target = self.thread(task)?;
        let retained = Self::retained_for(target, info.signo);
        let unblocked = target.effective_blocked() & bit(info.signo) == 0;
        // Generation effects (flush + continuation) precede retention.
        self.prepare_flush(info.signo, ledger);
        let continued = info.signo == SIGCONT;
        if self.disposition_ignored(info.signo) && !retained {
            return Some(SendOutcome {
                queue: QueueResult::DiscardedIgnored,
                continued,
            });
        }
        let (uid, limit) = (self.uid, self.sigpending_limit);
        let t = self.thread_mut(task)?;
        let result = Self::send_into(&mut t.private, info, source, ledger, uid, limit);
        // Notification only when something queued AND the signal is
        // unblocked there: allocation failure returns before wake-up,
        // and a blocked private send sets no TIF_SIGPENDING.
        if matches!(result, QueueResult::Queued(_) | QueueResult::Coalesced(_)) && unblocked {
            t.sigpending = true;
        }
        Some(SendOutcome {
            queue: result,
            continued,
        })
    }

    /// `complete_signal` for process-directed sends (PIDTYPE_TGID):
    /// the named target first, then the walk from
    /// `curr_target`. Returns the receiver; updates `curr_target` only
    /// when the walk chose (as Linux does after a successful queue).
    // Thread-directed sends never come here: tgkill delivery is direct
    // to its named thread (PIDTYPE_PID returns early in Linux).
    pub fn choose_receiver(
        &mut self,
        signo: i32,
        named: TaskGen,
        facts: &[SchedFact],
    ) -> Option<TaskGen> {
        let fact = |task: TaskGen| facts.iter().find(|f| f.task == task).copied();
        if let (Some(t), Some(f)) = (self.thread(named), fact(named))
            && wants_signal(signo, t, &self.shared, &f)
        {
            self.thread_mut(named).unwrap().sigpending = true;
            return Some(named);
        }
        let start = self
            .curr_target
            .and_then(|ct| self.threads.iter().position(|t| t.task == ct))
            .unwrap_or(0);
        for t in self.threads[start..]
            .iter()
            .chain(self.threads[..start].iter())
        {
            if let Some(f) = fact(t.task)
                && wants_signal(signo, t, &self.shared, &f)
            {
                self.curr_target = Some(t.task);
                let chosen = t.task;
                self.thread_mut(chosen).unwrap().sigpending = true;
                return Some(chosen);
            }
        }
        None
    }

    /// Dequeue for one thread (private queue before shared, as Linux does)
    /// and resolve the delivery under the caller's frame environment.
    /// The timedwait wait-exit transition: Linux restores
    /// blocked := real_blocked and clears real_blocked when
    /// do_sigtimedwait returns — before any handler selection or
    /// masking, and independent of later frame success.
    /// This lifetime is separate from saved_sigmask (sigsuspend),
    /// which a successful frame consumes at delivery.
    pub fn exit_timedwait(&mut self, task: TaskGen) {
        // __set_task_blocked: assignment plus recalc_sigpending, so a
        // still-pending unblocked signal keeps the notification set.
        let idx = self.threads.iter().position(|t| t.task == task);
        if let Some(idx) = idx
            && let Some(saved) = self.threads[idx].real_blocked.take()
        {
            self.threads[idx].blocked = saved;
            let pending = unblocked_pending(&self.threads[idx], &self.shared);
            self.threads[idx].sigpending = pending;
        }
    }

    /// Dequeue a signal for an rt_sigtimedwait-style wait: selection
    /// happens under the wait mask, then the wait-exit transition runs.
    /// Returns the pending instance (its allocation is released).
    pub fn dequeue_for_timedwait(
        &mut self,
        task: TaskGen,
        wait_set: u64,
        ledger: &mut Sigpending,
    ) -> Option<Pending> {
        let idx = self.threads.iter().position(|t| t.task == task)?;
        // do_sigtimedwait strips SIGKILL/SIGSTOP from the set and
        // dequeues under its complement, ignoring tsk->blocked.
        let waited = wait_set & !UNBLOCKABLE_MASK;
        let pending = self.threads[idx]
            .private
            .dequeue_waited(waited)
            .or_else(|| self.shared.dequeue_waited(waited));
        // dequeue_signal always recalcs (signal.c:639), on a match AND
        // on no-match — separately from the wait exit: a no-match must
        // not end the wait or touch either mask.
        let idx = self.threads.iter().position(|t| t.task == task).unwrap();
        self.threads[idx].sigpending = unblocked_pending(&self.threads[idx], &self.shared);
        let pending = pending?;
        if let InstanceKind::Charged(uid) = pending.kind {
            ledger.release(uid, 1);
        }
        self.exit_timedwait(task);
        Some(pending)
    }

    pub fn next_delivery(
        &mut self,
        task: TaskGen,
        env: &FrameEnv,
        ledger: &mut Sigpending,
    ) -> Option<DeliveryPlan> {
        // A timedwait wait exits exactly when a delivery is produced:
        // peek first (probing ends nothing), then run the wait-exit
        // transition so masking uses the restored mask. Consumers may
        // also run exit_timedwait explicitly; it is idempotent.
        {
            let idx0 = self.threads.iter().position(|t| t.task == task)?;
            let t0 = &self.threads[idx0];
            // Deliverability is judged under the POST-EXIT mask: a
            // waited signal pending under the temporary wait mask is
            // blocked again once the wait exits, so it must not trigger
            // the exit (probe u44).
            let post_exit_blocked = t0.real_blocked.unwrap_or(t0.blocked) & !UNBLOCKABLE_MASK;
            let deliverable =
                (t0.private.peek_mask() | self.shared.peek_mask()) & !post_exit_blocked != 0;
            if !deliverable {
                return None;
            }
        }
        self.exit_timedwait(task);
        let idx = self.threads.iter().position(|t| t.task == task)?;
        let blocked = self.threads[idx].effective_blocked();
        // dequeue_synchronous_signal: the FIRST qualifying *private*
        // entry (FIFO) with a positive si_code, before ordinary
        // next_signal selection. Never applies to the shared queue.
        let (pending, queue_kind) = self.threads[idx]
            .private
            .dequeue_first(blocked, |p| p.info.code > 0 && is_synchronous(p.info.signo))
            .map(|p| (p, QueueKind::Private))
            .or_else(|| {
                self.threads[idx]
                    .private
                    .dequeue(blocked)
                    .map(|p| (p, QueueKind::Private))
            })
            .or_else(|| self.shared.dequeue(blocked).map(|p| (p, QueueKind::Shared)))?;
        // Dequeuing frees the allocation to its original owner.
        if let InstanceKind::Charged(uid) = pending.kind {
            ledger.release(uid, 1);
        }
        let signo = pending.info.signo;
        let instance = PendingRef {
            task,
            queue: queue_kind,
            instance: pending.id,
            signo,
        };
        let act = self.actions[(signo - 1) as usize];
        let delivery = self.resolve(idx, signo, act, env);
        // recalc_sigpending: clear notification when no unblocked work
        // remains for this thread.
        self.threads[idx].sigpending = unblocked_pending(&self.threads[idx], &self.shared);
        Some(DeliveryPlan {
            instance,
            receiver: task,
            info: pending.info,
            delivery,
        })
    }

    fn resolve(&mut self, idx: usize, signo: i32, act: SigAction, env: &FrameEnv) -> Delivery {
        let fatal_default = matches!(
            default_action(signo),
            DefaultAction::Terminate | DefaultAction::TerminateCore
        );
        match act.kind {
            DispositionKind::Ignore => Delivery::Discard,
            DispositionKind::Handler if !is_unblockable(signo) => {
                let t = &self.threads[idx];
                let mut handler_mask = t.blocked | act.mask;
                if act.flags & SA_NODEFER == 0 {
                    handler_mask |= bit(signo);
                }
                handler_mask &= !UNBLOCKABLE_MASK;
                // __on_sig_stack: (base, base+size], and always false for
                // an SS_AUTODISARM stack (sched/signal.h:583ff).
                let on_sig_stack = |sp: u64| {
                    t.altstack
                        .map(|a| {
                            !a.disabled
                                && !a.autodisarm
                                && sp > a.base
                                && sp <= a.base.saturating_add(a.size)
                        })
                        .unwrap_or(false)
                };
                let on_now = on_sig_stack(env.sp_now);
                // get_sigframe subtracts the red zone before testing
                // whether to switch: near the base that tips a nested
                // frame into a fresh switch to the stack top.
                let adj_sp = env.sp_now.saturating_sub(frame::SIGNAL_RED_ZONE);
                let switch_to_alt = matches!(t.altstack, Some(a) if !a.disabled)
                    && act.flags & SA_ONSTACK != 0
                    && !on_sig_stack(adj_sp);
                let use_alt = switch_to_alt || on_now;
                // Fit is frame::FrameLayout against the active stack's
                // remaining extent (get_sigframe): fresh altstack switch
                // discards the red-zone reservation; nested and main
                // frames keep it. Overflow is a Linux SIGSEGV outcome.
                let fits = if switch_to_alt {
                    let a = t.altstack.unwrap();
                    frame::FrameLayout::below(a.base.saturating_add(a.size), false, env.xstate)
                        .map(|l| l.fits_above(a.base))
                        .unwrap_or(false)
                } else if on_now {
                    let a = t.altstack.unwrap();
                    frame::FrameLayout::below(env.sp_now, true, env.xstate)
                        .map(|l| l.fits_above(a.base))
                        .unwrap_or(false)
                } else {
                    frame::FrameLayout::below(env.sp_now, true, env.xstate)
                        .map(|l| l.frame_address >= env.stack_lower_bound)
                        .unwrap_or(false)
                };
                let restorer = (act.flags & SA_RESTORER != 0).then_some(act.restorer);
                if act.flags & SA_RESETHAND != 0 {
                    // Only sa_handler resets (signal.c:2941); flags, mask
                    // and restorer survive.
                    let a = &mut self.actions[(signo - 1) as usize];
                    a.kind = DispositionKind::Default;
                    a.handler = 0;
                }
                if restorer.is_none() || !fits {
                    return Delivery::ForceSegv;
                }
                let frame = FrameRequest {
                    handler: act.handler,
                    handler_mask,
                    restore_mask: t.saved_sigmask.unwrap_or(t.blocked),
                    use_altstack: use_alt,
                    on_altstack_now: on_now && !switch_to_alt,
                    restorer,
                    xstate_size: env.xstate.size(),
                };
                // The handler runs with handler_mask blocked: a second
                // queued copy cannot enter until sigreturn restores.
                // Delivering consumes saved_sigmask (signal_delivered
                // clears it): a nested frame saves the *current* mask.
                self.threads[idx].blocked = handler_mask;
                // saved_sigmask is consumed by this successful frame;
                // a ForceSegv path above returns before this point and
                // preserves it. (real_blocked ended at wait-exit.)
                self.threads[idx].saved_sigmask = None;
                // SS_AUTODISARM: EVERY successful handler delivery
                // resets an autodisarm altstack until rt_sigreturn,
                // even one that did not use it (signal.c:3057ff).
                if let Some(a) = self.threads[idx].altstack.as_mut()
                    && a.autodisarm
                {
                    a.disabled = true;
                }
                Delivery::EnterHandler { frame }
            }
            _ => {
                // Retained only because blocked: pending-delivery ignore
                // classification (incl. default SIGCONT) discards here.
                if self.disposition_ignored(signo) {
                    return Delivery::Discard;
                }
                let action = default_action(signo);
                if action == DefaultAction::Ignore {
                    Delivery::Discard
                } else if fatal_default && self.threads.len() > 1 {
                    Delivery::GroupExit
                } else {
                    Delivery::ApplyDefault { action }
                }
            }
        }
    }
}

/// Registered signal producers (terminal, SIGIO fds, parent-death):
/// registered before arrivals so waits know which signals can end them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerSource {
    Terminal,
    SigioFd(i32),
    ParentDeath,
}

#[derive(Debug, Clone, Default)]
pub struct Producers {
    armed: Vec<(ProducerSource, i32)>,
}

impl Producers {
    pub fn arm(&mut self, source: ProducerSource, signo: i32) {
        self.armed.retain(|(s, _)| *s != source);
        self.armed.push((source, signo));
    }
    pub fn disarm(&mut self, source: ProducerSource) {
        self.armed.retain(|(s, _)| *s != source);
    }
    /// Signal numbers some armed producer can generate. What a caller
    /// does with this (e.g. its own wait policy) is the caller's.
    pub fn armed_mask(&self) -> u64 {
        self.armed.iter().fold(0, |m, (_, s)| m | bit(*s))
    }
    pub fn is_armed(&self, source: ProducerSource) -> Option<i32> {
        self.armed
            .iter()
            .find(|(s, _)| *s == source)
            .map(|(_, signo)| *signo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tg(tid: i32) -> TaskGen {
        TaskGen { tid, generation: 1 }
    }
    fn fact(task: TaskGen) -> SchedFact {
        SchedFact {
            task,
            running: false,
            exiting: false,
            stopped: false,
        }
    }
    fn handler(flags: u32) -> SigAction {
        SigAction {
            kind: DispositionKind::Handler,
            handler: 0x401000,
            mask: 0,
            flags,
            restorer: 0x7f0000,
        }
    }
    fn env(sp: u64, xs: usize, lower: u64) -> FrameEnv {
        FrameEnv {
            sp_now: sp,
            xstate: frame::XstateDesc::new(frame::SUPPORTED_XFEATURES, xs, 0xffbf).unwrap(),
            stack_lower_bound: lower,
        }
    }

    #[test]
    fn sigpending_is_per_real_uid_across_processes() {
        // RLIMIT_SIGPENDING counts allocations per real UID (ucounts),
        // shared by every process of that user — not per queue.
        let mut ledger = Sigpending::new();
        let mut a = ProcessDomain::new(tg(100), 1000, 2);
        let mut b = ProcessDomain::new(tg(200), 1000, 2);
        let rt = |uid| SigInfo {
            signo: 40,
            code: -1,
            value: 7,
            pid: 1,
            uid,
            addr: 0,
            overrun: 0,
        };
        assert!(matches!(
            a.send_process(rt(1000), SendSource::SigQueue, &mut ledger)
                .queue,
            QueueResult::Queued(_)
        ));
        assert!(matches!(
            b.send_process(rt(1000), SendSource::SigQueue, &mut ledger)
                .queue,
            QueueResult::Queued(_)
        ));
        assert_eq!(
            a.send_process(rt(1000), SendSource::SigQueue, &mut ledger)
                .queue,
            QueueResult::Overflow
        );
        // The charge follows the RECEIVER uid, not si_uid: a different
        // sender to the same receiver still overflows (kernel ucounts).
        assert_eq!(
            a.send_process(rt(1001), SendSource::SigQueue, &mut ledger)
                .queue,
            QueueResult::Overflow
        );
        // Dequeue frees the receiver's allocation.
        let p = b
            .next_delivery(tg(200), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(p.info.value, 7, "plan carries si_value");
        assert_eq!(ledger.used_by(1000), 1);
        // kill-family on exhaustion still delivers once, value lost.
        assert!(matches!(
            a.send_process(rt(1000), SendSource::SigQueue, &mut ledger)
                .queue,
            QueueResult::Queued(_)
        ));
        assert!(matches!(
            a.send_process(rt(1000), SendSource::UserKill, &mut ledger)
                .queue,
            QueueResult::Coalesced(_)
        ));
        // Standard signals coalesce without allocating again.
        let u = SigInfo::user(10, 1, 1000);
        a.send_process(u, SendSource::UserKill, &mut ledger);
        let before = ledger.used_by(1000);
        assert!(matches!(
            a.send_process(u, SendSource::UserKill, &mut ledger),
            SendOutcome {
                queue: QueueResult::Coalesced(_),
                ..
            }
        ));
        assert_eq!(ledger.used_by(1000), before);
    }

    #[test]
    fn overflow_kill_synthesizes_zeroed_siginfo() {
        // No sigqueue record: Linux synthesizes SI_USER, pid/uid/value 0.
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 0); // limit 0
        let info = SigInfo {
            signo: 40,
            code: -1,
            value: 99,
            pid: 42,
            uid: 7,
            addr: 0,
            overrun: 0,
        };
        let out = d.send_process(info, SendSource::UserKill, &mut ledger);
        assert!(matches!(out.queue, QueueResult::Queued(_)));
        let p = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(
            (p.info.code, p.info.pid, p.info.uid, p.info.value),
            (0, 0, 0, 0)
        );
        assert_eq!(
            ledger.used_by(1000),
            0,
            "siginfo-less delivery charged nothing"
        );
    }

    #[test]
    fn synchronous_signals_dequeue_first() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        d.send_thread(
            tg(100),
            SigInfo::user(11, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let first = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(
            first.instance.signo, 11,
            "SIGSEGV (synchronous) before SIGUSR1"
        );
        let priv_ref = first.instance;
        // Same signo/id space in the shared queue must not alias.
        d.send_process(SigInfo::user(11, 1, 0), SendSource::UserKill, &mut ledger);
        d.actions[10] = handler(SA_RESTORER);
        let _usr1 = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        let shared = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(shared.instance.signo, 11);
        assert_ne!(shared.instance.queue, priv_ref.queue);
        assert_ne!(shared.instance, priv_ref);
    }

    #[test]
    fn stop_cont_flush_covers_shared_and_private() {
        // prepare_signal flushes both queues (flush_sigqueue_mask).
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        d.send_thread(
            tg(100),
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        d.send_process(SigInfo::user(20, 1, 0), SendSource::UserKill, &mut ledger); // SIGTSTP
        assert_eq!(d.shared.peek_mask() & bit(SIGCONT), 0);
        assert_eq!(
            d.thread(tg(100)).unwrap().private.peek_mask() & bit(SIGCONT),
            0
        );
        let cont = d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert!(
            cont.continued,
            "SIGCONT continues at generation even before delivery"
        );
        assert_eq!(d.shared.peek_mask() & bit(20), 0);
    }

    #[test]
    fn receiver_walk_curr_target_and_kill_wakes_stopped() {
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.threads.push(ThreadState::new(tg(102)));
        let facts = [fact(tg(100)), fact(tg(101)), fact(tg(102))];
        assert_eq!(d.choose_receiver(10, tg(100), &facts), Some(tg(100)));
        d.thread_mut(tg(100)).unwrap().blocked = bit(10);
        assert_eq!(d.choose_receiver(10, tg(100), &facts), Some(tg(101)));
        assert_eq!(d.curr_target, Some(tg(101)));
        // SIGKILL/SIGCONT wake stopped tasks (TASK_WAKEKILL): a stopped
        // named target still takes them.
        let mut f2 = facts;
        f2[0].stopped = true;
        f2[1].stopped = true;
        f2[2].stopped = true;
        assert_eq!(d.choose_receiver(10, tg(100), &f2), None);
        assert_eq!(d.choose_receiver(SIGKILL, tg(100), &f2), Some(tg(100)));
        assert_eq!(d.choose_receiver(SIGCONT, tg(101), &f2), Some(tg(101)));
    }

    #[test]
    fn delivery_frame_policy_nested_altstack_and_group_exit() {
        let mut ledger = Sigpending::new();
        let alt = AltStack {
            base: 0x10000,
            size: 2048,
            disabled: false,
            autodisarm: false,
        };
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p = d
            .next_delivery(tg(100), &env(0x7fff0000, 832, 0), &mut ledger)
            .unwrap();
        assert!(matches!(p.delivery, Delivery::EnterHandler { frame } if frame.use_altstack));
        // Host-sized xstate on a fresh altstack: SIGSEGV outcome.
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        d2.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d2.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d2.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert_eq!(
            d2.next_delivery(tg(100), &env(0x7fff0000, 2700, 0), &mut ledger)
                .unwrap()
                .delivery,
            Delivery::ForceSegv
        );
        // Nested: already on the altstack, little room below SP even
        // though the main stack has headroom to spare.
        let mut d3 = ProcessDomain::new(tg(100), 1000, 64);
        d3.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d3.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d3.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p3 = d3
            .next_delivery(tg(100), &env(0x10100, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(
            p3.delivery,
            Delivery::ForceSegv,
            "nested frame must fit below SP on the altstack"
        );
        // Two threads + fatal default = group exit.
        let mut d4 = ProcessDomain::new(tg(100), 1000, 64);
        d4.threads.push(ThreadState::new(tg(101)));
        d4.send_thread(
            tg(100),
            SigInfo::user(11, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert_eq!(
            d4.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .unwrap()
                .delivery,
            Delivery::GroupExit
        );
    }

    #[test]
    fn restart_mapping_absolute_deadline() {
        assert_eq!(
            map_restart(RestartCode::RestartRestartBlock, false, false, Some(5_000)),
            WaitResolution::RestartAbs { deadline_ns: 5_000 }
        );
        assert_eq!(
            map_restart(RestartCode::RestartRestartBlock, false, false, None),
            WaitResolution::RestartSyscall
        );
        assert_eq!(
            map_restart(RestartCode::RestartSys, true, false, None),
            WaitResolution::Eintr
        );
        assert_eq!(
            map_restart(RestartCode::RestartSys, true, true, None),
            WaitResolution::Restart
        );
        assert_eq!(
            map_restart(RestartCode::RestartNoIntr, true, false, None),
            WaitResolution::Restart
        );
    }

    #[test]
    fn producers_expose_generic_state() {
        let mut p = Producers::default();
        assert_eq!(p.armed_mask(), 0);
        p.arm(ProducerSource::SigioFd(5), 29);
        assert_eq!(p.armed_mask(), bit(29));
        assert_eq!(p.is_armed(ProducerSource::SigioFd(5)), Some(29));
        p.arm(ProducerSource::SigioFd(5), 10);
        assert_eq!(p.armed_mask(), bit(10));
        p.disarm(ProducerSource::SigioFd(5));
        assert_eq!(p.armed_mask(), 0);
    }

    #[test]
    fn installing_ignore_flushes_pending() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert_eq!(ledger.used_by(1000), 2);
        d.set_action(
            10,
            SigAction {
                kind: DispositionKind::Ignore,
                ..SigAction::default()
            },
            tg(100),
            &mut ledger,
        );
        assert_eq!(d.shared.peek_mask() & bit(10), 0);
        assert_eq!(d.thread(tg(100)).unwrap().private.peek_mask() & bit(10), 0);
        assert_eq!(ledger.used_by(1000), 0);
    }

    #[test]
    fn allocation_failure_taxonomy() {
        // One row per Linux case (__send_signal_locked): a qualifying
        // delivery passes, a failing allocation refuses or falls back.
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 0); // receiver limit 0
        let rt = SigInfo {
            signo: 40,
            code: -1,
            value: 5,
            pid: 9,
            uid: 7,
            addr: 0,
            overrun: 0,
        };
        // RT sigqueue + thread-kill refuse (EAGAIN).
        assert_eq!(
            d.send_process(rt, SendSource::SigQueue, &mut ledger).queue,
            QueueResult::Overflow
        );
        assert_eq!(
            d.send_process(rt, SendSource::ThreadKill, &mut ledger)
                .queue,
            QueueResult::Overflow
        );
        // RT kill(2) preserves one siginfo-less instance.
        assert!(matches!(
            d.send_process(rt, SendSource::UserKill, &mut ledger).queue,
            QueueResult::Queued(_)
        ));
        // Standard SI_QUEUE preserves a pending instance without siginfo.
        let std_q = SigInfo {
            signo: 12,
            code: -1,
            value: 5,
            pid: 9,
            uid: 7,
            addr: 0,
            overrun: 0,
        };
        assert!(matches!(
            d.send_process(std_q, SendSource::SigQueue, &mut ledger)
                .queue,
            QueueResult::Queued(_)
        ));
        // Standard SI_USER and kernel sends override the limit, charged.
        let std_u = SigInfo::user(10, 9, 7);
        assert!(matches!(
            d.send_process(std_u, SendSource::UserKill, &mut ledger)
                .queue,
            QueueResult::Queued(_)
        ));
        assert_eq!(ledger.used_by(1000), 1);
        let kern = SigInfo {
            signo: 41,
            code: 1,
            value: 0,
            pid: 0,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        // Ordinary positive-code kernel siginfo does NOT override.
        assert_eq!(
            d.send_process(kern, SendSource::KernelOrdinary, &mut ledger)
                .queue,
            QueueResult::Overflow
        );
        assert_eq!(ledger.used_by(1000), 1);
        // SEND_SIG_PRIV (special) preserves an uncharged fallback.
        let priv_rt = SigInfo {
            signo: 42,
            code: -4,
            value: 3,
            pid: 0,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        assert!(matches!(
            d.send_process(priv_rt, SendSource::KernelSpecial, &mut ledger)
                .queue,
            QueueResult::Queued(_)
        ));
        assert_eq!(ledger.used_by(1000), 1, "special fallback is uncharged");
        // SIGKILL never allocates.
        assert!(matches!(
            d.send_process(
                SigInfo::user(SIGKILL, 9, 7),
                SendSource::UserKill,
                &mut ledger
            )
            .queue,
            QueueResult::Queued(_)
        ));
        assert_eq!(
            ledger.used_by(1000),
            1,
            "only the overriding standard send charged"
        );
        // Different senders, one receiver: accounting follows the receiver.
        let mut ledger2 = Sigpending::new();
        let mut e = ProcessDomain::new(tg(300), 2000, 1);
        let from_a = SigInfo {
            signo: 40,
            code: -1,
            value: 0,
            pid: 1,
            uid: 111,
            addr: 0,
            overrun: 0,
        };
        let from_b = SigInfo {
            signo: 40,
            code: -1,
            value: 0,
            pid: 2,
            uid: 222,
            addr: 0,
            overrun: 0,
        };
        assert!(matches!(
            e.send_process(from_a, SendSource::SigQueue, &mut ledger2)
                .queue,
            QueueResult::Queued(_)
        ));
        assert_eq!(
            e.send_process(from_b, SendSource::SigQueue, &mut ledger2)
                .queue,
            QueueResult::Overflow
        );
        assert_eq!(ledger2.used_by(2000), 1);
        assert_eq!(ledger2.used_by(111), 0);
    }

    #[test]
    fn stale_target_flushes_nothing() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.send_process(SigInfo::user(20, 1, 0), SendSource::UserKill, &mut ledger); // SIGTSTP shared
        let before = ledger.used_by(1000);
        let stale = TaskGen {
            tid: 100,
            generation: 99,
        };
        assert!(
            d.send_thread(
                stale,
                SigInfo::user(SIGCONT, 1, 0),
                SendSource::UserKill,
                &mut ledger
            )
            .is_none()
        );
        assert_ne!(
            d.shared.peek_mask() & bit(20),
            0,
            "stale send must not flush the live stop"
        );
        assert_eq!(
            ledger.used_by(1000),
            before,
            "stale send must not release accounting"
        );
        // The same send to the live generation does flush + continue.
        let out = d
            .send_thread(
                tg(100),
                SigInfo::user(SIGCONT, 1, 0),
                SendSource::UserKill,
                &mut ledger,
            )
            .unwrap();
        assert!(out.continued);
        assert_eq!(d.shared.peek_mask() & bit(20), 0);
    }

    #[test]
    fn sigkill_takes_stopped_thread_with_pending_and_cont_blocked() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.threads.push(ThreadState::new(tg(102)));
        // Restored round-1 case: stopped 101 is skipped for an ordinary
        // signal, runnable 102 is chosen by the walk.
        d.thread_mut(tg(100)).unwrap().blocked = bit(10);
        let mut f = [fact(tg(100)), fact(tg(101)), fact(tg(102))];
        f[1].stopped = true;
        assert_eq!(d.choose_receiver(10, tg(100), &f), Some(tg(102)));
        // SIGKILL takes a stopped non-running target that already has
        // private pending work (wants_signal returns before those tests).
        d.send_thread(
            tg(101),
            SigInfo::user(12, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert_eq!(d.choose_receiver(SIGKILL, tg(101), &f), Some(tg(101)));
        // Blocked SIGCONT still continues at generation.
        d.thread_mut(tg(100)).unwrap().blocked |= bit(SIGCONT);
        let out = d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert!(out.continued);
    }

    #[test]
    fn fallback_bit_absorbed_by_later_allocation() {
        // limit 0 then 1: Linux delivers ONLY the allocated record.
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 0);
        d.thread_mut(tg(100)).unwrap().blocked = bit(40);
        let rt0 = SigInfo {
            signo: 40,
            code: -1,
            value: 0,
            pid: 1,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        d.send_process(rt0, SendSource::UserKill, &mut ledger);
        d.sigpending_limit = 1;
        let rt8 = SigInfo {
            signo: 40,
            code: -1,
            value: 8,
            pid: 1,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        d.send_process(rt8, SendSource::SigQueue, &mut ledger);
        d.thread_mut(tg(100)).unwrap().blocked = 0;
        let p = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(p.info.value, 8);
        assert!(
            d.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .is_none(),
            "no extra zero record"
        );
    }

    #[test]
    fn charge_freed_to_original_owner_after_credential_change() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.send_process(SigInfo::user(40, 1, 0), SendSource::SigQueue, &mut ledger);
        assert_eq!(ledger.used_by(1000), 1);
        d.uid = 1001; // credential change after queueing
        let _ = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(ledger.used_by(1000), 0, "original owner freed");
        assert_eq!(ledger.used_by(1001), 0, "new identity untouched");
    }

    #[test]
    fn fresh_shared_send_still_selects_nonrunning_receiver() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        let facts = [fact(tg(100)), fact(tg(101))]; // nobody running
        assert_eq!(d.choose_receiver(10, tg(100), &facts), Some(tg(100)));
        assert!(d.thread(tg(100)).unwrap().sigpending, "selection notifies");
    }

    #[test]
    fn ignore_flushes_siblings_and_default_ignored_discards_at_generation() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.send_thread(
            tg(101),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        d.set_action(
            10,
            SigAction {
                kind: DispositionKind::Ignore,
                ..SigAction::default()
            },
            tg(100),
            &mut ledger,
        );
        assert_eq!(
            d.thread(tg(101)).unwrap().private.peek_mask() & bit(10),
            0,
            "sibling flushed"
        );
        // Default-ignored SIGCHLD discarded at generation when unblocked...
        let out = d.send_process(SigInfo::user(17, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(out.queue, QueueResult::DiscardedIgnored);
        // ...but retained while blocked.
        d.thread_mut(tg(100)).unwrap().blocked = bit(17);
        d.thread_mut(tg(101)).unwrap().blocked = bit(17);
        let out2 = d.send_process(SigInfo::user(17, 1, 0), SendSource::UserKill, &mut ledger);
        assert!(matches!(out2.queue, QueueResult::Queued(_)));
    }

    #[test]
    fn thread_directed_blocked_target_keeps_it_private() {
        // PIDTYPE_PID: a blocked tgkill target is not retargeted; the
        // signal waits on its private queue (provisional-receiver note).
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.thread_mut(tg(101)).unwrap().blocked = bit(10);
        d.send_thread(
            tg(101),
            SigInfo::user(10, 1, 0),
            SendSource::ThreadKill,
            &mut ledger,
        );
        assert!(
            d.next_delivery(tg(101), &env(0x8000, 832, 0), &mut ledger)
                .is_none()
        );
        assert!(
            d.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .is_none(),
            "not retargeted"
        );
        d.thread_mut(tg(101)).unwrap().blocked = 0;
        assert!(
            d.next_delivery(tg(101), &env(0x8000, 832, 0), &mut ledger)
                .is_some()
        );
    }

    #[test]
    fn failed_and_blocked_sends_do_not_notify() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 0); // limit 0: RT fails
        let out = d
            .send_thread(
                tg(100),
                SigInfo::user(40, 1, 0),
                SendSource::SigQueue,
                &mut ledger,
            )
            .unwrap();
        assert_eq!(out.queue, QueueResult::Overflow);
        assert!(
            !d.thread(tg(100)).unwrap().sigpending,
            "failed send notifies nobody"
        );
        d.sigpending_limit = 64;
        // Fresh shared send still selects the nonrunning receiver.
        d.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(
            d.choose_receiver(10, tg(100), &[fact(tg(100))]),
            Some(tg(100))
        );
        // Blocked private send queues without notifying.
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        d2.thread_mut(tg(100)).unwrap().blocked = bit(12);
        d2.send_thread(
            tg(100),
            SigInfo::user(12, 1, 0),
            SendSource::ThreadKill,
            &mut ledger,
        );
        assert!(!d2.thread(tg(100)).unwrap().sigpending);
        d2.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(
            d2.choose_receiver(10, tg(100), &[fact(tg(100))]),
            Some(tg(100))
        );
    }

    #[test]
    fn ignored_cont_and_stop_still_run_generation_effects() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.set_action(
            SIGCONT,
            SigAction {
                kind: DispositionKind::Ignore,
                ..SigAction::default()
            },
            tg(100),
            &mut ledger,
        );
        d.send_process(SigInfo::user(20, 1, 0), SendSource::UserKill, &mut ledger); // pending stop
        let out = d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert!(out.continued, "ignored SIGCONT still continues");
        assert_eq!(out.queue, QueueResult::DiscardedIgnored);
        assert_eq!(
            d.shared.peek_mask() & bit(20),
            0,
            "ignored SIGCONT still flushes stops"
        );
        // Ignored stop cancels a pending SIGCONT.
        d.set_action(
            20,
            SigAction {
                kind: DispositionKind::Ignore,
                ..SigAction::default()
            },
            tg(100),
            &mut ledger,
        );
        d.thread_mut(tg(100)).unwrap().blocked = bit(SIGCONT); // retain CONT
        d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert_ne!(d.shared.peek_mask() & bit(SIGCONT), 0);
        let stop = d.send_process(SigInfo::user(20, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(stop.queue, QueueResult::DiscardedIgnored);
        assert_eq!(
            d.shared.peek_mask() & bit(SIGCONT),
            0,
            "ignored stop still cancels CONT"
        );
    }

    #[test]
    fn synchronous_private_fifo_and_shared_scope() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        let segv = SigInfo {
            signo: 11,
            code: 1,
            value: 0,
            pid: 0,
            uid: 0,
            addr: 0xdead,
            overrun: 0,
        };
        let ill = SigInfo {
            signo: 4,
            code: 2,
            value: 0,
            pid: 0,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        d.send_thread(tg(100), segv, SendSource::KernelOrdinary, &mut ledger);
        d.send_thread(tg(100), ill, SendSource::KernelOrdinary, &mut ledger);
        let first = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(
            first.instance.signo, 11,
            "first qualifying private entry (FIFO), not lowest"
        );
        // Shared positive-code SIGSEGV does NOT get the synchronous pass:
        // ordinary next_signal order picks SIGILL(4) first there.
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        d2.send_process(
            SigInfo {
                signo: 11,
                code: 1,
                value: 0,
                pid: 0,
                uid: 0,
                addr: 0,
                overrun: 0,
            },
            SendSource::KernelOrdinary,
            &mut ledger,
        );
        d2.send_process(SigInfo::user(4, 1, 0), SendSource::UserKill, &mut ledger);
        let f2 = d2
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(f2.instance.signo, 4);
        assert_eq!(f2.instance.queue, QueueKind::Shared);
    }

    #[test]
    fn autodisarm_switches_and_disarms_on_any_delivery() {
        let mut ledger = Sigpending::new();
        let alt = AltStack {
            base: 0x10000,
            size: 2048,
            disabled: false,
            autodisarm: true,
        };
        // Geometrically on-stack + autodisarm: switch to top and fit.
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p = d
            .next_delivery(tg(100), &env(0x10590, 832, 0), &mut ledger)
            .unwrap();
        assert!(
            matches!(p.delivery, Delivery::EnterHandler { .. }),
            "autodisarm switches to top: {p:?}"
        );
        assert!(
            d.thread(tg(100)).unwrap().altstack.unwrap().disabled,
            "delivery disarmed it"
        );
        // Red-zone boundary: sp 128 above base switches (adjusted SP).
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        d2.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d2.thread_mut(tg(100)).unwrap().altstack = Some(AltStack {
            autodisarm: false,
            ..alt
        });
        d2.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p2 = d2
            .next_delivery(tg(100), &env(0x10080, 832, 0), &mut ledger)
            .unwrap();
        assert!(
            matches!(p2.delivery, Delivery::EnterHandler { frame } if frame.use_altstack && !frame.on_altstack_now)
        );
        // Non-SA_ONSTACK delivery also disarms.
        let mut d3 = ProcessDomain::new(tg(100), 1000, 64);
        d3.actions[9] = handler(SA_RESTORER);
        d3.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d3.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p3 = d3
            .next_delivery(tg(100), &env(0x7fff0000, 832, 0), &mut ledger)
            .unwrap();
        assert!(matches!(p3.delivery, Delivery::EnterHandler { .. }));
        assert!(d3.thread(tg(100)).unwrap().altstack.unwrap().disabled);
    }

    #[test]
    fn synchronous_mask_excludes_xcpu_xfsz_and_tif_lifecycle() {
        assert!(!is_synchronous(24) && !is_synchronous(25));
        assert!(is_synchronous(11) && is_synchronous(4));
        // Walk choice sets notification; delivery with nothing left clears it.
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.send_process(SigInfo::user(24, 1, 0), SendSource::UserKill, &mut ledger);
        d.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(
            d.choose_receiver(10, tg(100), &[fact(tg(100))]),
            Some(tg(100))
        );
        assert!(d.thread(tg(100)).unwrap().sigpending);
        let p1 = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(p1.instance.signo, 10, "SIGXCPU must not beat lower SIGUSR1");
        let _ = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert!(
            !d.thread(tg(100)).unwrap().sigpending,
            "recalc clears when drained"
        );
        // Standard thread-kill at the limit: fallback, never an override.
        let mut ledger0 = Sigpending::new();
        let mut e = ProcessDomain::new(tg(300), 2000, 0);
        let out = e.send_process(
            SigInfo::user(12, 9, 7),
            SendSource::ThreadKill,
            &mut ledger0,
        );
        assert!(matches!(out.queue, QueueResult::Queued(_)));
        assert_eq!(ledger0.used_by(2000), 0);
        let got = e
            .next_delivery(tg(300), &env(0x8000, 832, 0), &mut ledger0)
            .unwrap();
        assert_eq!((got.info.pid, got.info.uid), (0, 0), "synthesized siginfo");
    }

    #[test]
    fn handler_mask_union_and_resethand_one_shot() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        let mut act = handler(SA_RESTORER | SA_RESETHAND);
        act.mask = bit(12);
        d.actions[9] = act;
        d.thread_mut(tg(100)).unwrap().blocked = bit(17);
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        let Delivery::EnterHandler { frame } = p.delivery else {
            panic!("{p:?}")
        };
        assert_eq!(frame.handler_mask, bit(17) | bit(12) | bit(10));
        assert_eq!(frame.restore_mask, bit(17));
        assert_eq!(
            d.actions[9].kind,
            DispositionKind::Default,
            "RESETHAND consumed"
        );
        // Second send waits while the handler mask is applied...
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert!(
            d.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .is_none()
        );
        // ...sigreturn restores frame.restore_mask, then the reset
        // disposition takes the default action (terminate, one thread).
        d.thread_mut(tg(100)).unwrap().blocked = frame.restore_mask;
        let p2 = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(
            p2.delivery,
            Delivery::ApplyDefault {
                action: DefaultAction::Terminate
            }
        );
    }

    #[test]
    fn timer_repeated_expiry_raises_overrun_on_one_record() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.thread_mut(tg(100)).unwrap().blocked = bit(40);
        let t = SigInfo {
            signo: 40,
            code: -2,
            value: 5,
            pid: 0,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        d.send_process(t, SendSource::TimerPrealloc(3), &mut ledger);
        d.send_process(t, SendSource::TimerPrealloc(3), &mut ledger);
        d.send_process(t, SendSource::TimerPrealloc(3), &mut ledger);
        d.thread_mut(tg(100)).unwrap().blocked = 0;
        let p = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(p.info.overrun, 2);
        assert_eq!(p.info.value, 5);
        assert!(
            d.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .is_none()
        );
    }

    #[test]
    fn process_discard_checks_suggested_target_only() {
        // Leader unblocked (discard) even though a sibling blocks;
        // leader blocked retains.
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.thread_mut(tg(101)).unwrap().blocked = bit(17);
        let out = d.send_process(SigInfo::user(17, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(out.queue, QueueResult::DiscardedIgnored);
        d.thread_mut(tg(100)).unwrap().blocked = bit(17);
        let out2 = d.send_process(SigInfo::user(17, 1, 0), SendSource::UserKill, &mut ledger);
        assert!(matches!(out2.queue, QueueResult::Queued(_)));
    }

    #[test]
    fn nested_frames_consume_saved_sigmask_once() {
        // signal_delivered clears saved_sigmask: frame 1 restores the
        // pre-wait mask; frame 2 (nested) saves the outer handler mask.
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        let mut act = handler(SA_RESTORER);
        act.mask = bit(12);
        d.actions[9] = act;
        let t = d.thread_mut(tg(100)).unwrap();
        t.blocked = 0; // sigsuspend temporary mask
        t.saved_sigmask = Some(bit(13));
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p1 = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        let Delivery::EnterHandler { frame: f1 } = p1.delivery else {
            panic!("{p1:?}")
        };
        assert_eq!(f1.restore_mask, bit(13));
        assert_eq!(d.thread(tg(100)).unwrap().saved_sigmask, None);
        // Unblock signo in the outer handler (as a handler may), deliver nested.
        d.thread_mut(tg(100)).unwrap().blocked &= !bit(10);
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let p2 = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        let Delivery::EnterHandler { frame: f2 } = p2.delivery else {
            panic!("{p2:?}")
        };
        // Current mask after unblocking signo is bit(12) (from sa_mask);
        // a stale pre-wait save would wrongly repeat bit(13).
        assert_eq!(f2.restore_mask, bit(12));
    }

    #[test]
    fn b15_mutant_pins() {
        // 2. SA_RESETHAND keeps flags/mask/restorer (only handler resets).
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        let mut act = handler(SA_RESTORER | SA_RESETHAND);
        act.mask = bit(12);
        d.actions[9] = act;
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        let _ = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        let after = d.actions[9];
        assert_eq!(after.kind, DispositionKind::Default);
        assert_eq!(
            (after.mask, after.flags, after.restorer),
            (bit(12), SA_RESTORER | SA_RESETHAND, 0x7f0000)
        );
        // 3a. Retention does NOT consult saved_sigmask.
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        d2.set_action(
            10,
            SigAction {
                kind: DispositionKind::Ignore,
                ..SigAction::default()
            },
            tg(100),
            &mut ledger,
        );
        d2.thread_mut(tg(100)).unwrap().saved_sigmask = Some(bit(10));
        assert_eq!(
            d2.send_thread(
                tg(100),
                SigInfo::user(10, 1, 0),
                SendSource::UserKill,
                &mut ledger
            )
            .unwrap()
            .queue,
            QueueResult::DiscardedIgnored
        );
        // 3b. Retention DOES consult real_blocked (timedwait mask).
        d2.thread_mut(tg(100)).unwrap().saved_sigmask = None;
        d2.thread_mut(tg(100)).unwrap().real_blocked = Some(bit(10));
        assert!(matches!(
            d2.send_thread(
                tg(100),
                SigInfo::user(10, 1, 0),
                SendSource::UserKill,
                &mut ledger
            )
            .unwrap()
            .queue,
            QueueResult::Queued(_)
        ));
        // 7. TIF set on a WALK choice (named ineligible).
        let mut d3 = ProcessDomain::new(tg(100), 1000, 64);
        d3.threads.push(ThreadState::new(tg(101)));
        d3.thread_mut(tg(100)).unwrap().blocked = bit(10);
        d3.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(
            d3.choose_receiver(10, tg(100), &[fact(tg(100)), fact(tg(101))]),
            Some(tg(101))
        );
        assert!(d3.thread(tg(101)).unwrap().sigpending);
        assert!(!d3.thread(tg(100)).unwrap().sigpending);
        // 5+6. Timer absorbs fallback; allocation never absorbs a timer.
        let mut ledger0 = Sigpending::new();
        let mut d4 = ProcessDomain::new(tg(100), 1000, 0);
        d4.thread_mut(tg(100)).unwrap().blocked = bit(40);
        d4.send_process(SigInfo::user(40, 1, 0), SendSource::UserKill, &mut ledger0); // fallback bit
        let tinfo = SigInfo {
            signo: 40,
            code: -2,
            value: 8,
            pid: 0,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        d4.send_process(tinfo, SendSource::TimerPrealloc(9), &mut ledger0); // absorbs bit
        d4.thread_mut(tg(100)).unwrap().blocked = 0;
        let p = d4
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger0)
            .unwrap();
        assert_eq!(p.info.value, 8);
        assert!(
            d4.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger0)
                .is_none(),
            "one delivery"
        );
        // restart: NOHAND with handler is EINTR.
        assert_eq!(
            map_restart(RestartCode::RestartNoHand, true, true, None),
            WaitResolution::Eintr
        );
    }

    #[test]
    fn standard_signal_timers_keep_distinct_records() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.thread_mut(tg(100)).unwrap().blocked = bit(10);
        let t = |v: i64| SigInfo {
            signo: 10,
            code: -2,
            value: v,
            pid: 0,
            uid: 0,
            addr: 0,
            overrun: 0,
        };
        d.send_process(t(3), SendSource::TimerPrealloc(3), &mut ledger);
        d.send_process(t(4), SendSource::TimerPrealloc(4), &mut ledger);
        d.send_process(t(3), SendSource::TimerPrealloc(3), &mut ledger); // overrun
        d.thread_mut(tg(100)).unwrap().blocked = 0;
        let p1 = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!((p1.info.value, p1.info.overrun), (3, 1));
        let p2 = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(p2.info.value, 4);
    }

    #[test]
    fn sigcont_kernel_ignore_class_trio() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        // (1) Unblocked SIG_DFL SIGCONT: discarded, still continues.
        let out = d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert!(out.continued);
        assert_eq!(out.queue, QueueResult::DiscardedIgnored);
        // (2) set_action(SIGCONT, Default) flushes a pending blocked CONT.
        d.thread_mut(tg(100)).unwrap().blocked = bit(SIGCONT);
        d.actions[17] = handler(SA_RESTORER);
        d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert_ne!(d.shared.peek_mask() & bit(SIGCONT), 0);
        d.set_action(SIGCONT, SigAction::default(), tg(100), &mut ledger);
        assert_eq!(
            d.shared.peek_mask() & bit(SIGCONT),
            0,
            "SIG_DFL SIGCONT flushes"
        );
        // (3) Blocked SIG_DFL SIGCONT retained, then Discard at delivery.
        d.actions[17] = handler(SA_RESTORER);
        d.send_process(
            SigInfo::user(SIGCONT, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        d.actions[17] = SigAction::default();
        d.thread_mut(tg(100)).unwrap().blocked = 0;
        let p = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        assert_eq!(p.delivery, Delivery::Discard);
        // Missing SA_RESTORER forces SIGSEGV (x86-64 has no vDSO restorer).
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        d2.actions[9] = handler(0);
        d2.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert_eq!(
            d2.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .unwrap()
                .delivery,
            Delivery::ForceSegv
        );
        // curr_target: the walk resumes where the last choice landed.
        let mut d3 = ProcessDomain::new(tg(100), 1000, 64);
        d3.threads.push(ThreadState::new(tg(101)));
        d3.threads.push(ThreadState::new(tg(102)));
        d3.thread_mut(tg(100)).unwrap().blocked = bit(10);
        let f3 = [fact(tg(100)), fact(tg(101)), fact(tg(102))];
        assert_eq!(d3.choose_receiver(10, tg(100), &f3), Some(tg(101)));
        d3.thread_mut(tg(101)).unwrap().blocked = bit(10);
        assert_eq!(
            d3.choose_receiver(10, tg(100), &f3),
            Some(tg(102)),
            "walk resumes at curr_target"
        );
    }

    #[test]
    fn timedwait_exit_restores_mask_before_handler_masking() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.actions[11] = handler(SA_RESTORER); // SIGUSR2 = 12
        let t = d.thread_mut(tg(100)).unwrap();
        t.blocked = 0; // timedwait wait mask
        t.real_blocked = Some(bit(10)); // SIGUSR1 blocked originally
        d.send_thread(
            tg(100),
            SigInfo::user(12, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        d.exit_timedwait(tg(100)); // consumer runs the wait-exit transition
        let p = d
            .next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
            .unwrap();
        let Delivery::EnterHandler { frame } = p.delivery else {
            panic!("{p:?}")
        };
        assert_eq!(
            frame.handler_mask,
            bit(10) | bit(12),
            "2560: USR1 stays blocked in handler"
        );
        assert_eq!(
            d.thread(tg(100)).unwrap().real_blocked,
            None,
            "wait-exit consumed it"
        );
    }

    #[test]
    fn curr_target_walk_resumes_from_cursor() {
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.threads.push(ThreadState::new(tg(102)));
        d.curr_target = Some(tg(102));
        d.thread_mut(tg(100)).unwrap().blocked = bit(10);
        let f = [fact(tg(100)), fact(tg(101)), fact(tg(102))];
        // Both 101 and 102 eligible; only a cursor-resuming walk picks 102.
        assert_eq!(d.choose_receiver(10, tg(100), &f), Some(tg(102)));
    }

    #[test]
    fn timedwait_dequeue_semantics_and_probe_purity() {
        // u40: block-then-wait returns the signal (blocked is ignored).
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.thread_mut(tg(100)).unwrap().blocked = bit(40);
        d.send_process(SigInfo::user(40, 1, 0), SendSource::UserKill, &mut ledger);
        let used_before = ledger.used_by(1000);
        let got = d
            .dequeue_for_timedwait(tg(100), bit(40), &mut ledger)
            .unwrap();
        assert_eq!(got.info.signo, 40);
        assert_eq!(ledger.used_by(1000), used_before - 1, "charge released");
        // u42: sigtimedwait never returns SIGKILL.
        d.send_process(
            SigInfo::user(SIGKILL, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert!(
            d.dequeue_for_timedwait(tg(100), bit(SIGKILL), &mut ledger)
                .is_none()
        );
        // u43: probing with next_delivery does not end a live wait.
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        let t = d2.thread_mut(tg(100)).unwrap();
        t.blocked = 0;
        t.real_blocked = Some(0x200);
        assert!(
            d2.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .is_none()
        );
        let t = d2.thread(tg(100)).unwrap();
        assert_eq!(
            (t.blocked, t.real_blocked),
            (0, Some(0x200)),
            "wait untouched by probe"
        );
    }

    #[test]
    fn timedwait_dequeue_recalcs_sigpending() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.send_thread(
            tg(100),
            SigInfo::user(40, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        assert!(d.thread(tg(100)).unwrap().sigpending);
        let got = d
            .dequeue_for_timedwait(tg(100), bit(40), &mut ledger)
            .unwrap();
        assert_eq!(got.info.signo, 40);
        assert!(
            !d.thread(tg(100)).unwrap().sigpending,
            "last pending consumed: flag cleared"
        );
    }

    #[test]
    fn timedwait_peek_gate_uses_post_exit_mask() {
        // u44: waited signal pending under the wait mask must not end
        // the wait via a next_delivery probe.
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        let t = d.thread_mut(tg(100)).unwrap();
        t.blocked = 0;
        t.real_blocked = Some(bit(10));
        d.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        assert!(
            d.next_delivery(tg(100), &env(0x8000, 832, 0), &mut ledger)
                .is_none()
        );
        let t = d.thread(tg(100)).unwrap();
        assert_eq!(
            (t.blocked, t.real_blocked),
            (0, Some(bit(10))),
            "wait untouched"
        );
        // u45 twin: sigtimedwait({10}) with only 12 pending returns None
        // and leaves 12 pending.
        let mut d2 = ProcessDomain::new(tg(100), 1000, 64);
        d2.send_process(SigInfo::user(12, 1, 0), SendSource::UserKill, &mut ledger);
        assert!(
            d2.dequeue_for_timedwait(tg(100), bit(10), &mut ledger)
                .is_none()
        );
        assert_ne!(d2.shared.peek_mask() & bit(12), 0);
        // curr_target walk-resume is distinguished in
        // synchronous_mask_excludes_xcpu_xfsz_and_tif_lifecycle and
        // receiver_walk_and_curr_target; altstack top is on-stack:
        let alt = AltStack {
            base: 0x10000,
            size: 2048,
            disabled: false,
            autodisarm: false,
        };
        let mut d3 = ProcessDomain::new(tg(100), 1000, 64);
        d3.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d3.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d3.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendSource::UserKill,
            &mut ledger,
        );
        // SP exactly at the altstack top counts as on-stack (nested).
        let p3 = d3
            .next_delivery(tg(100), &env(0x10800, 832, 0), &mut ledger)
            .unwrap();
        assert!(matches!(p3.delivery, Delivery::EnterHandler { frame } if frame.on_altstack_now));
    }

    #[test]
    fn explicit_timedwait_exit_recalculates_notification() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        let t = d.thread_mut(tg(100)).unwrap();
        t.blocked = bit(10);
        t.real_blocked = Some(bit(10));
        t.sigpending = false;
        d.send_process(SigInfo::user(12, 1, 0), SendSource::UserKill, &mut ledger);
        d.exit_timedwait(tg(100));
        let t = d.thread(tg(100)).unwrap();
        assert_eq!(t.blocked, bit(10));
        assert!(
            t.sigpending,
            "another unblocked signal remains: notification set by recalc"
        );
    }

    #[test]
    fn no_match_timedwait_recalculates_notification_without_ending_wait() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.threads.push(ThreadState::new(tg(101)));
        d.send_process(SigInfo::user(12, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(
            d.choose_receiver(12, tg(100), &[fact(tg(100)), fact(tg(101))]),
            Some(tg(100))
        );
        let t = d.thread_mut(tg(101)).unwrap();
        t.blocked = bit(10);
        t.sigpending = false;
        assert!(
            d.dequeue_for_timedwait(tg(101), bit(10), &mut ledger)
                .is_none()
        );
        let t = d.thread(tg(101)).unwrap();
        assert!(t.sigpending, "recalc sets notification for pending USR2");
        assert_eq!(
            (t.blocked, t.real_blocked),
            (bit(10), None),
            "masks untouched, wait not ended"
        );
        assert_ne!(d.shared.peek_mask() & bit(12), 0, "USR2 preserved");
    }

    #[test]
    fn seeded_ignored_and_state_clones() {
        let mut ledger = Sigpending::new();
        let mut d = ProcessDomain::new(tg(100), 1000, 64);
        d.seed_initial(bit(13) | bit(SIGKILL));
        assert_eq!(d.actions[12].kind, DispositionKind::Ignore);
        assert_eq!(d.actions[8].kind, DispositionKind::Default);
        let mut copy = d.clone();
        copy.send_process(SigInfo::user(10, 1, 0), SendSource::UserKill, &mut ledger);
        assert_eq!(d.shared.len(), 0);
        assert_eq!(copy.shared.len(), 1);
    }
}
