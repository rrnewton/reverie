/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A pure model of Linux signal semantics (kernel/signal.c), shared by
//! every backend. The domain owns masks, pending instances, dispositions,
//! alternate stacks, `curr_target` and the restart mapping. Callers supply
//! scheduling facts (turn holder, liveness, job-control stop, delivery point);
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

/// One pending instance; `id` is unique within its queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub id: u64,
    pub info: SigInfo,
    /// Whether this instance holds one `RLIMIT_SIGPENDING` allocation.
    pub counted: bool,
}

/// A keyed reference to one pending instance on one task generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingRef {
    pub task: TaskGen,
    pub instance: u64,
    pub signo: i32,
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
    /// Real-time send beyond `RLIMIT_SIGPENDING` (`kill` still delivers
    /// one siginfo-less instance instead; see `SendKind`).
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendKind {
    /// `kill`-family (SI_USER/SI_TKILL): overflow still delivers once.
    Kill,
    /// sigqueue-like: overflow is EAGAIN.
    Queue,
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
    pub fn push(&mut self, info: SigInfo, counted: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push_back(Pending { id, info, counted });
        id
    }

    /// Drop every instance whose signo is in `mask`; returns the count of
    /// dropped instances that held an allocation (`flush_sigqueue_mask`).
    pub fn flush_mask(&mut self, mask: u64) -> usize {
        let mut released = 0;
        self.entries.retain(|p| {
            if mask & bit(p.info.signo) != 0 {
                if p.counted {
                    released += 1;
                }
                false
            } else {
                true
            }
        });
        released
    }

    /// `dequeue_signal` order: lowest signal number, then FIFO. Blocked
    /// signals wait; SIGKILL/SIGSTOP ignore the mask.
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
                    if (p.info.signo, p.id) < (cur.info.signo, cur.id) {
                        i
                    } else {
                        b
                    }
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
    /// Mask before a temporary-mask wait; restored into `uc_sigmask`.
    pub real_blocked: Option<u64>,
    pub altstack: Option<AltStack>,
}

impl ThreadState {
    pub fn new(task: TaskGen) -> Self {
        Self {
            task,
            private: SignalQueue::new(),
            blocked: 0,
            real_blocked: None,
            altstack: None,
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

fn wants_signal(signo: i32, t: &ThreadState, f: &SchedFact) -> bool {
    if !is_unblockable(signo) && t.effective_blocked() & bit(signo) != 0 {
        return false;
    }
    // SIGKILL and SIGCONT wake stopped tasks (TASK_WAKEKILL in
    // `signal_wake_up`), so a job-control stop never excludes them.
    if f.exiting || f.stopped && signo != SIGKILL && signo != SIGCONT {
        return false;
    }
    f.running || t.private.is_empty()
}

/// Caller-supplied frame environment at a delivery point. The xstate
/// size comes from the presented CPU description, never the host's.
#[derive(Debug, Clone, Copy)]
pub struct FrameEnv {
    pub sp_now: u64,
    pub xstate_size: usize,
    /// Fixed frame bytes excluding the xstate area.
    pub frame_fixed: usize,
    /// Main-stack headroom in bytes (the altstack size governs on-stack).
    pub main_headroom: usize,
}

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
            RestartCode::RestartRestartBlock => WaitResolution::RestartAbs {
                deadline_ns: restart_deadline_ns.unwrap_or(0),
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

/// `RLIMIT_SIGPENDING` accounting: sigqueue allocations are counted per
/// real UID across every process of that user (kernel ucounts,
/// `__sigqueue_alloc`), not per signal number or per queue. One ledger
/// serves a whole run; each process domain allocates through it.
#[derive(Debug, Clone, Default)]
pub struct Sigpending {
    pub limit: usize,
    used: std::collections::BTreeMap<u32, usize>,
}

impl Sigpending {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            used: std::collections::BTreeMap::new(),
        }
    }
    pub fn try_alloc(&mut self, uid: u32) -> bool {
        let n = self.used.entry(uid).or_insert(0);
        if *n >= self.limit {
            return false;
        }
        *n += 1;
        true
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
#[derive(Debug, Clone)]
pub struct ProcessDomain {
    pub actions: [SigAction; NSIG as usize],
    pub shared: SignalQueue,
    pub threads: Vec<ThreadState>,
    pub curr_target: Option<TaskGen>,
}

impl ProcessDomain {
    pub fn new(leader: TaskGen) -> Self {
        Self {
            actions: [SigAction::default(); NSIG as usize],
            shared: SignalQueue::new(),
            threads: vec![ThreadState::new(leader)],
            curr_target: Some(leader),
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
        // Flushed instances may belong to several sender uids; the kernel
        // frees each sigqueue to its owner's ucount. This model stores the
        // sender uid in siginfo, so release per instance below.
        let mut released: Vec<u32> = Vec::new();
        // temporarily take queues apart via index walk
        let flush = |q: &mut SignalQueue| {
            let mut uids = Vec::new();
            q.entries.retain(|e| {
                if mask & bit(e.info.signo) != 0 {
                    if e.counted {
                        uids.push(e.info.uid);
                    }
                    false
                } else {
                    true
                }
            });
            uids
        };
        released.extend(flush(&mut self.shared));
        for t in &mut self.threads {
            released.extend(flush(&mut t.private));
        }
        for uid in released {
            ledger.release(uid, 1);
        }
    }

    /// `__send_signal_locked`: flush, coalesce, allocate, queue.
    /// Allocation is charged to the sender's real UID (`info.uid`).
    /// On allocation failure a sigqueue-like send is EAGAIN
    /// (`QueueResult::Overflow`); a `kill`-family send still delivers
    /// one instance with its siginfo value lost, as Linux does.
    fn send_into(
        queue: &mut SignalQueue,
        info: SigInfo,
        kind: SendKind,
        ledger: &mut Sigpending,
    ) -> QueueResult {
        if !is_realtime(info.signo)
            && let Some(id) = queue.find(info.signo)
        {
            return QueueResult::Coalesced(id);
        }
        if ledger.try_alloc(info.uid) {
            return QueueResult::Queued(queue.push(info, true));
        }
        match kind {
            SendKind::Queue => QueueResult::Overflow,
            SendKind::Kill => match queue.find(info.signo) {
                Some(id) => QueueResult::Coalesced(id),
                None => {
                    let mut lost = info;
                    lost.value = 0;
                    QueueResult::Queued(queue.push(lost, false))
                }
            },
        }
    }

    /// Queue a process-directed send on the shared queue.
    pub fn send_process(
        &mut self,
        info: SigInfo,
        kind: SendKind,
        ledger: &mut Sigpending,
    ) -> QueueResult {
        self.prepare_flush(info.signo, ledger);
        Self::send_into(&mut self.shared, info, kind, ledger)
    }

    /// Queue a thread-directed send on the target's private queue.
    pub fn send_thread(
        &mut self,
        task: TaskGen,
        info: SigInfo,
        kind: SendKind,
        ledger: &mut Sigpending,
    ) -> Option<QueueResult> {
        self.prepare_flush(info.signo, ledger);
        let t = self.thread_mut(task)?;
        Some(Self::send_into(&mut t.private, info, kind, ledger))
    }

    /// `complete_signal`: the named target first, then the walk from
    /// `curr_target`. Returns the receiver; updates `curr_target` only
    /// when the walk chose (as Linux does after a successful queue).
    pub fn choose_receiver(
        &mut self,
        signo: i32,
        named: TaskGen,
        facts: &[SchedFact],
    ) -> Option<TaskGen> {
        let fact = |task: TaskGen| facts.iter().find(|f| f.task == task).copied();
        if let (Some(t), Some(f)) = (self.thread(named), fact(named))
            && wants_signal(signo, t, &f)
        {
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
                && wants_signal(signo, t, &f)
            {
                self.curr_target = Some(t.task);
                return Some(t.task);
            }
        }
        None
    }

    /// Dequeue for one thread (private queue before shared, as Linux does)
    /// and resolve the delivery under the caller's frame environment.
    pub fn next_delivery(
        &mut self,
        task: TaskGen,
        env: &FrameEnv,
        ledger: &mut Sigpending,
    ) -> Option<DeliveryPlan> {
        let idx = self.threads.iter().position(|t| t.task == task)?;
        let blocked = self.threads[idx].effective_blocked();
        let pending = self.threads[idx]
            .private
            .dequeue(blocked)
            .or_else(|| self.shared.dequeue(blocked))?;
        // Dequeuing frees the sigqueue allocation (`__sigqueue_free`).
        if pending.counted {
            ledger.release(pending.info.uid, 1);
        }
        let signo = pending.info.signo;
        let instance = PendingRef {
            task,
            instance: pending.id,
            signo,
        };
        let act = self.actions[(signo - 1) as usize];
        let delivery = self.resolve(idx, signo, act, env);
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
                let on_now = t
                    .altstack
                    .map(|a| !a.disabled && env.sp_now >= a.base && env.sp_now < a.base + a.size)
                    .unwrap_or(false);
                let use_alt = matches!(t.altstack, Some(a) if !a.disabled)
                    && act.flags & SA_ONSTACK != 0
                    && !on_now;
                let need = env.frame_fixed + env.xstate_size;
                // Already on the altstack (nested): the frame grows down
                // from the current SP (get_sigframe's on-stack path), so
                // the room is sp - base, not main-stack headroom.
                let fits = if on_now {
                    let a = t.altstack.unwrap();
                    need as u64 <= env.sp_now.saturating_sub(a.base)
                } else if use_alt {
                    need <= t.altstack.map(|a| a.size as usize).unwrap_or(0)
                } else {
                    need <= env.main_headroom
                };
                let restorer = (act.flags & SA_RESTORER != 0).then_some(act.restorer);
                if act.flags & SA_RESETHAND != 0 {
                    self.actions[(signo - 1) as usize] = SigAction::default();
                }
                if restorer.is_none() || !fits {
                    return Delivery::ForceSegv;
                }
                Delivery::EnterHandler {
                    frame: FrameRequest {
                        handler: act.handler,
                        handler_mask,
                        restore_mask: t.real_blocked.unwrap_or(t.blocked),
                        use_altstack: use_alt,
                        on_altstack_now: on_now,
                        restorer,
                        xstate_size: env.xstate_size,
                    },
                }
            }
            _ => {
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
    /// Whether a wait may be ended for `signo`: only when no armed
    /// producer can make it.
    pub fn may_end_wait(&self, signo: i32) -> bool {
        !self.armed.iter().any(|(_, s)| *s == signo)
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
    fn env(sp: u64, xs: usize, headroom: usize) -> FrameEnv {
        FrameEnv {
            sp_now: sp,
            xstate_size: xs,
            frame_fixed: 512,
            main_headroom: headroom,
        }
    }

    #[test]
    fn sigpending_is_per_real_uid_across_processes() {
        // RLIMIT_SIGPENDING counts allocations per real UID (ucounts),
        // shared by every process of that user — not per queue.
        let mut ledger = Sigpending::new(2);
        let mut a = ProcessDomain::new(tg(100));
        let mut b = ProcessDomain::new(tg(200));
        let rt = |uid| SigInfo {
            signo: 40,
            code: -1,
            value: 7,
            pid: 1,
            uid,
            addr: 0,
        };
        assert!(matches!(
            a.send_process(rt(1000), SendKind::Queue, &mut ledger),
            QueueResult::Queued(_)
        ));
        assert!(matches!(
            b.send_process(rt(1000), SendKind::Queue, &mut ledger),
            QueueResult::Queued(_)
        ));
        assert_eq!(
            a.send_process(rt(1000), SendKind::Queue, &mut ledger),
            QueueResult::Overflow
        );
        // Another user is unaffected.
        assert!(matches!(
            a.send_process(rt(1001), SendKind::Queue, &mut ledger),
            QueueResult::Queued(_)
        ));
        // Dequeue frees the sender's allocation.
        let p = b
            .next_delivery(tg(200), &env(0x8000, 832, 1 << 20), &mut ledger)
            .unwrap();
        assert_eq!(p.info.value, 7, "plan carries si_value");
        assert_eq!(ledger.used_by(1000), 1);
        // kill-family on exhaustion still delivers once, value lost.
        assert!(matches!(
            a.send_process(rt(1000), SendKind::Queue, &mut ledger),
            QueueResult::Queued(_)
        ));
        assert!(matches!(
            a.send_process(rt(1000), SendKind::Kill, &mut ledger),
            QueueResult::Coalesced(_)
        ));
        // Standard signals coalesce without allocating again.
        let u = SigInfo::user(10, 1, 1000);
        a.send_process(u, SendKind::Kill, &mut ledger);
        let before = ledger.used_by(1000);
        assert!(matches!(
            a.send_process(u, SendKind::Kill, &mut ledger),
            QueueResult::Coalesced(_)
        ));
        assert_eq!(ledger.used_by(1000), before);
    }

    #[test]
    fn stop_cont_flush_covers_shared_and_private() {
        // prepare_signal flushes both queues (flush_sigqueue_mask).
        let mut ledger = Sigpending::new(64);
        let mut d = ProcessDomain::new(tg(100));
        d.send_process(SigInfo::user(SIGCONT, 1, 0), SendKind::Kill, &mut ledger);
        d.send_thread(
            tg(100),
            SigInfo::user(SIGCONT, 1, 0),
            SendKind::Kill,
            &mut ledger,
        );
        d.send_process(SigInfo::user(20, 1, 0), SendKind::Kill, &mut ledger); // SIGTSTP
        assert_eq!(d.shared.peek_mask() & bit(SIGCONT), 0);
        assert_eq!(
            d.thread(tg(100)).unwrap().private.peek_mask() & bit(SIGCONT),
            0
        );
        d.send_process(SigInfo::user(SIGCONT, 1, 0), SendKind::Kill, &mut ledger);
        assert_eq!(d.shared.peek_mask() & bit(20), 0);
    }

    #[test]
    fn receiver_walk_curr_target_and_kill_wakes_stopped() {
        let mut d = ProcessDomain::new(tg(100));
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
        let mut ledger = Sigpending::new(64);
        let alt = AltStack {
            base: 0x10000,
            size: 2048,
            disabled: false,
            autodisarm: false,
        };
        let mut d = ProcessDomain::new(tg(100));
        d.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendKind::Kill,
            &mut ledger,
        );
        let p = d
            .next_delivery(tg(100), &env(0x7fff0000, 832, 0), &mut ledger)
            .unwrap();
        assert!(matches!(p.delivery, Delivery::EnterHandler { frame } if frame.use_altstack));
        // Host-sized xstate on a fresh altstack: SIGSEGV outcome.
        let mut d2 = ProcessDomain::new(tg(100));
        d2.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d2.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d2.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendKind::Kill,
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
        let mut d3 = ProcessDomain::new(tg(100));
        d3.actions[9] = handler(SA_RESTORER | SA_ONSTACK);
        d3.thread_mut(tg(100)).unwrap().altstack = Some(alt);
        d3.send_thread(
            tg(100),
            SigInfo::user(10, 1, 0),
            SendKind::Kill,
            &mut ledger,
        );
        let p3 = d3
            .next_delivery(tg(100), &env(0x10100, 832, 1 << 20), &mut ledger)
            .unwrap();
        assert_eq!(
            p3.delivery,
            Delivery::ForceSegv,
            "nested frame must fit below SP on the altstack"
        );
        // Two threads + fatal default = group exit.
        let mut d4 = ProcessDomain::new(tg(100));
        d4.threads.push(ThreadState::new(tg(101)));
        d4.send_thread(
            tg(100),
            SigInfo::user(11, 1, 0),
            SendKind::Kill,
            &mut ledger,
        );
        assert_eq!(
            d4.next_delivery(tg(100), &env(0x8000, 832, 1 << 20), &mut ledger)
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
    fn producers_gate_waits() {
        let mut p = Producers::default();
        assert!(p.may_end_wait(29));
        p.arm(ProducerSource::SigioFd(5), 29);
        assert!(!p.may_end_wait(29));
        p.arm(ProducerSource::SigioFd(5), 10);
        assert!(p.may_end_wait(29));
        assert!(!p.may_end_wait(10));
        p.disarm(ProducerSource::SigioFd(5));
        assert!(p.may_end_wait(10));
    }

    #[test]
    fn seeded_ignored_and_state_clones() {
        let mut ledger = Sigpending::new(8);
        let mut d = ProcessDomain::new(tg(100));
        d.seed_initial(bit(13) | bit(SIGKILL));
        assert_eq!(d.actions[12].kind, DispositionKind::Ignore);
        assert_eq!(d.actions[8].kind, DispositionKind::Default);
        let mut copy = d.clone();
        copy.send_process(SigInfo::user(13, 1, 0), SendKind::Kill, &mut ledger);
        assert_eq!(d.shared.len(), 0);
        assert_eq!(copy.shared.len(), 1);
    }
}
