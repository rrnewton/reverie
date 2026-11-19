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
//! scheduling facts (turn holder, liveness, virtual stop, delivery point);
//! nothing here performs I/O or names a backend or a consumer.

use std::collections::VecDeque;

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

    /// `__send_signal_locked`'s queue effects, including `prepare_signal`
    /// stop/continue flushing. `rt_depth` bounds each RT signal's queue
    /// (domain-counted `RLIMIT_SIGPENDING`).
    pub fn send(&mut self, info: SigInfo, kind: SendKind, rt_depth: usize) -> QueueResult {
        let signo = info.signo;
        if is_stop(signo) {
            self.entries.retain(|p| p.info.signo != SIGCONT);
        }
        if signo == SIGCONT {
            self.entries.retain(|p| !is_stop(p.info.signo));
        }
        if !is_realtime(signo) {
            if let Some(p) = self.entries.iter().find(|p| p.info.signo == signo) {
                return QueueResult::Coalesced(p.id);
            }
        } else if self
            .entries
            .iter()
            .filter(|p| p.info.signo == signo)
            .count()
            >= rt_depth
        {
            match kind {
                SendKind::Queue => return QueueResult::Overflow,
                SendKind::Kill => {
                    // Deliver one instance with its siginfo lost, as Linux does.
                    if let Some(p) = self.entries.iter().find(|p| p.info.signo == signo) {
                        return QueueResult::Coalesced(p.id);
                    }
                }
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push_back(Pending { id, info });
        QueueResult::Queued(id)
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
    /// The deterministic `task_curr` (e.g. only the sender runs).
    pub running: bool,
    pub exiting: bool,
    /// The guest-visible job-control stop, never a tracer stop.
    pub stopped: bool,
}

fn wants_signal(signo: i32, t: &ThreadState, f: &SchedFact) -> bool {
    if !is_unblockable(signo) && t.effective_blocked() & bit(signo) != 0 {
        return false;
    }
    if f.exiting || f.stopped {
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
    pub delivery: Delivery,
}

/// One process's signal domain: shared dispositions and queue, threads in
/// creation (thread-list) order, and `curr_target` history.
#[derive(Debug, Clone)]
pub struct ProcessDomain {
    pub actions: [SigAction; NSIG as usize],
    pub shared: SignalQueue,
    pub threads: Vec<ThreadState>,
    pub curr_target: Option<TaskGen>,
    pub rt_depth: usize,
}

impl ProcessDomain {
    pub fn new(leader: TaskGen, rt_depth: usize) -> Self {
        Self {
            actions: [SigAction::default(); NSIG as usize],
            shared: SignalQueue::new(),
            threads: vec![ThreadState::new(leader)],
            curr_target: Some(leader),
            rt_depth,
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

    /// Queue a process-directed send on the shared queue.
    pub fn send_process(&mut self, info: SigInfo, kind: SendKind) -> QueueResult {
        self.shared.send(info, kind, self.rt_depth)
    }

    /// Queue a thread-directed send on the target's private queue.
    pub fn send_thread(
        &mut self,
        task: TaskGen,
        info: SigInfo,
        kind: SendKind,
    ) -> Option<QueueResult> {
        let depth = self.rt_depth;
        self.thread_mut(task)
            .map(|t| t.private.send(info, kind, depth))
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
    pub fn next_delivery(&mut self, task: TaskGen, env: &FrameEnv) -> Option<DeliveryPlan> {
        let idx = self.threads.iter().position(|t| t.task == task)?;
        let blocked = self.threads[idx].effective_blocked();
        let pending = self.threads[idx]
            .private
            .dequeue(blocked)
            .or_else(|| self.shared.dequeue(blocked))?;
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
                let fits = if use_alt {
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
/// registered before arrivals so deterministic waits know their limits.
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
    /// A deterministic wait may end only for a signal no producer can make.
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
    fn handler(signo: i32, flags: u32) -> SigAction {
        let _ = signo;
        SigAction {
            kind: DispositionKind::Handler,
            handler: 0x401000,
            mask: 0,
            flags,
            restorer: 0x7f0000,
        }
    }

    #[test]
    fn coalescing_rt_and_overflow() {
        let mut q = SignalQueue::new();
        let u = SigInfo::user(10, 1, 0);
        assert!(matches!(
            q.send(u, SendKind::Kill, 4),
            QueueResult::Queued(_)
        ));
        assert!(matches!(
            q.send(u, SendKind::Kill, 4),
            QueueResult::Coalesced(_)
        ));
        let rt = SigInfo::user(40, 1, 0);
        for _ in 0..2 {
            assert!(matches!(
                q.send(rt, SendKind::Queue, 2),
                QueueResult::Queued(_)
            ));
        }
        assert_eq!(q.send(rt, SendKind::Queue, 2), QueueResult::Overflow);
        assert!(matches!(
            q.send(rt, SendKind::Kill, 2),
            QueueResult::Coalesced(_)
        ));
    }

    #[test]
    fn receiver_walk_and_curr_target() {
        let mut d = ProcessDomain::new(tg(100), 8);
        d.threads.push(ThreadState::new(tg(101)));
        d.threads.push(ThreadState::new(tg(102)));
        let facts = [fact(tg(100)), fact(tg(101)), fact(tg(102))];
        assert_eq!(d.choose_receiver(10, tg(100), &facts), Some(tg(100)));
        d.thread_mut(tg(100)).unwrap().blocked = bit(10);
        assert_eq!(d.choose_receiver(10, tg(100), &facts), Some(tg(101)));
        assert_eq!(d.curr_target, Some(tg(101)));
        // SIGKILL ignores the mask on the named target.
        assert_eq!(d.choose_receiver(SIGKILL, tg(100), &facts), Some(tg(100)));
        // Virtual stop excludes; running does not rescue a blocked thread.
        let mut f2 = facts;
        f2[1].stopped = true;
        assert_eq!(d.choose_receiver(10, tg(100), &f2), Some(tg(102)));
    }

    #[test]
    fn delivery_frame_policy_and_outcomes() {
        let mut d = ProcessDomain::new(tg(100), 8);
        d.actions[9] = handler(10, SA_RESTORER | SA_ONSTACK);
        d.thread_mut(tg(100)).unwrap().altstack = Some(AltStack {
            base: 0x10000,
            size: 2048,
            disabled: false,
            autodisarm: false,
        });
        d.send_thread(tg(100), SigInfo::user(10, 1, 0), SendKind::Kill);
        let p = d
            .next_delivery(
                tg(100),
                &FrameEnv {
                    sp_now: 0x7fff0000,
                    xstate_size: 832,
                    frame_fixed: 512,
                    main_headroom: 0,
                },
            )
            .unwrap();
        assert!(matches!(p.delivery, Delivery::EnterHandler { frame } if frame.use_altstack));
        // Host-sized xstate cannot fit: Linux outcome, not a refusal.
        let mut d2 = ProcessDomain::new(tg(100), 8);
        d2.actions[9] = handler(10, SA_RESTORER | SA_ONSTACK);
        d2.thread_mut(tg(100)).unwrap().altstack = Some(AltStack {
            base: 0x10000,
            size: 2048,
            disabled: false,
            autodisarm: false,
        });
        d2.send_thread(tg(100), SigInfo::user(10, 1, 0), SendKind::Kill);
        let p2 = d2
            .next_delivery(
                tg(100),
                &FrameEnv {
                    sp_now: 0x7fff0000,
                    xstate_size: 2700,
                    frame_fixed: 512,
                    main_headroom: 0,
                },
            )
            .unwrap();
        assert_eq!(p2.delivery, Delivery::ForceSegv);
        // Two threads + fatal default = group exit.
        let mut d3 = ProcessDomain::new(tg(100), 8);
        d3.threads.push(ThreadState::new(tg(101)));
        d3.send_thread(tg(100), SigInfo::user(11, 1, 0), SendKind::Kill);
        let p3 = d3
            .next_delivery(
                tg(100),
                &FrameEnv {
                    sp_now: 0x8000,
                    xstate_size: 832,
                    frame_fixed: 512,
                    main_headroom: 1 << 20,
                },
            )
            .unwrap();
        assert_eq!(p3.delivery, Delivery::GroupExit);
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
        p.arm(ProducerSource::SigioFd(5), 10); // re-arm replaces
        assert!(p.may_end_wait(29));
        assert!(!p.may_end_wait(10));
        p.disarm(ProducerSource::SigioFd(5));
        assert!(p.may_end_wait(10));
    }

    #[test]
    fn seeded_ignored_and_state_clones() {
        let mut d = ProcessDomain::new(tg(100), 8);
        d.seed_initial(bit(13) | bit(SIGKILL));
        assert_eq!(d.actions[12].kind, DispositionKind::Ignore);
        assert_eq!(d.actions[8].kind, DispositionKind::Default);
        let mut copy = d.clone(); // domain state is a plain value
        copy.send_process(SigInfo::user(13, 1, 0), SendKind::Kill);
        assert_eq!(d.shared.len(), 0);
        assert_eq!(copy.shared.len(), 1);
    }
}
