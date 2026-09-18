//! Generation-bound raw-syscall scripting for startup-cleanup tests.
//!
//! This module is compiled only for unit tests.  A script belongs to one exact
//! [`EventGeneration`]; unlike the older PID-keyed fault hooks, it cannot leak
//! into a concurrently running test or a reused numeric PID.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;

use super::*;

pub(super) const MODE_NONE: u8 = 0;
const MODE_INSTALLING: u8 = 1;
const MODE_SCRIPT: u8 = 2;
const MODE_REAL: u8 = 3;

/// Linearizes the first real lifecycle/raw activity against script install.
/// A contender which observes INSTALLING converts it to the permanent REAL
/// tombstone before failing closed, so a rejected install cannot later revert
/// to NONE and admit a Real -> Script transition.
pub(super) fn claim_lifecycle_activity(event: &Event) -> Result<(), Errno> {
    loop {
        match event.startup_script_mode.load(Ordering::Acquire) {
            MODE_NONE => {
                if event
                    .startup_script_mode
                    .compare_exchange(
                        MODE_NONE,
                        MODE_REAL,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return Ok(());
                }
            }
            MODE_SCRIPT | MODE_REAL => return Ok(()),
            MODE_INSTALLING => {
                if event
                    .startup_script_mode
                    .compare_exchange(
                        MODE_INSTALLING,
                        MODE_REAL,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return Err(Errno::EBUSY);
                }
            }
            _ => return Err(Errno::EPROTO),
        }
    }
}

pub(super) fn arm_install_pause(
    event: &Event,
    entered: Arc<Barrier>,
    resume: Arc<Barrier>,
) -> Result<(), Errno> {
    let mut pause = event.startup_script_install_pause.lock();
    if pause.is_some() || event.startup_script_mode.load(Ordering::Acquire) != MODE_NONE {
        return Err(Errno::EBUSY);
    }
    *pause = Some((entered, resume));
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Fidelity {
    LinuxFaithful,
    ImpossibleFault(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct Symbol(pub(super) u16);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SymbolKind {
    WaitAttempt,
    Status,
    Transaction,
    SignalAttempt,
    ResumeAttempt,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SymbolValue {
    kind: SymbolKind,
    raw: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExpectedId {
    Exact { kind: SymbolKind, raw: u64 },
    Bind { symbol: Symbol, kind: SymbolKind },
    Ref { symbol: Symbol, kind: SymbolKind },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BoundSymbol {
    symbol: Symbol,
    value: SymbolValue,
    nonce: NonZeroU64,
    generation: PhysicalEventGenerationId,
    bound_at: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingStatus {
    symbol: Symbol,
    wait_attempt: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransactionSite {
    RetainedBarrierPrepared,
    SetupPrepared,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ActualBindSite {
    WaitAttempt {
        generation: Option<PhysicalEventGenerationId>,
        task: PhysicalTaskIdentity,
        producer: PhysicalWaitProducer,
        flags: i32,
    },
    Transaction(TransactionSite),
    SignalAttempt {
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        transaction: u64,
        pidfd: i32,
        signal: i32,
    },
    ResumeAttempt {
        generation: Option<PhysicalEventGenerationId>,
        task: PhysicalTaskIdentity,
        source_status: Option<u64>,
        operation: PhysicalResumeOperation,
        signal: Option<i32>,
        owner: PhysicalResumeOwner,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExpectedBindSite {
    WaitAttempt {
        generation: Option<PhysicalEventGenerationId>,
        task: PhysicalTaskIdentity,
        producer: PhysicalWaitProducer,
        flags: i32,
    },
    Transaction(TransactionSite),
    SignalAttempt {
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        transaction: ExpectedId,
        pidfd: i32,
        signal: i32,
    },
    ResumeAttempt {
        generation: Option<PhysicalEventGenerationId>,
        task: PhysicalTaskIdentity,
        source_status: Option<ExpectedId>,
        operation: PhysicalResumeOperation,
        signal: Option<i32>,
        owner: PhysicalResumeOwner,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExpectedBind {
    pub symbol: Symbol,
    pub kind: SymbolKind,
    pub site: ExpectedBindSite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExpectedBindStatus {
    pub symbol: Symbol,
    pub wait_attempt: ExpectedId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransactionAuditSite {
    BarrierAuthorizedWorker,
    BarrierUnstarted,
    SetupUnstarted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExpectedTransactionAudit {
    pub site: TransactionAuditSite,
    pub transaction: ExpectedId,
    pub cause_wait: ExpectedId,
    pub barrier_wait: Option<ExpectedId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ActualTransactionAudit {
    pub site: TransactionAuditSite,
    pub transaction: u64,
    pub cause_wait: u64,
    pub barrier_wait: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WaitSite {
    RetainedBarrier,
    ObservedCleanup,
    UnobservedBoundaryProbe,
    UnobservedDrain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SignalSite {
    ObservedCleanup,
    UnobservedCleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PollPurpose {
    ReservedSignalFailure,
    ResumeFailure,
    WaitFailure,
    SignalFailure,
    CapacityFence,
    TerminalEchild,
    SetupNoStatusEchild,
    UnobservedSignalFailure,
    UnobservedBoundary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ContinueSite {
    ObservedCleanup,
    UnobservedCleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CallBinding {
    pub generation: PhysicalEventGenerationId,
    pub pid: Pid,
    pub pidfd: i32,
    pub task: PhysicalTaskIdentity,
    pub transaction: Option<u64>,
    pub source_status: Option<u64>,
    pub caller_tid: Pid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct WaitArgs {
    pub site: WaitSite,
    pub binding: CallBinding,
    pub producer: PhysicalWaitProducer,
    pub attempt: Option<u64>,
    pub idtype: libc::idtype_t,
    pub id: libc::id_t,
    pub options: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SignalArgs {
    pub site: SignalSite,
    pub binding: CallBinding,
    pub attempt: Option<u64>,
    pub signal: i32,
    pub siginfo_is_null: bool,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PollArgs {
    pub purpose: PollPurpose,
    pub binding: CallBinding,
    pub events: i16,
    pub timeout_ms: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ContinueArgs {
    pub site: ContinueSite,
    pub binding: CallBinding,
    pub attempt: Option<u64>,
    pub request: u32,
    pub target_tid: Pid,
    pub addr: usize,
    pub data: usize,
    pub owner: PhysicalResumeOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RawWaitidFrame {
    pub rc: i32,
    pub errno: i32,
    pub siginfo: Option<PhysicalWaitSiginfo>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RawPollFrame {
    pub rc: i32,
    pub errno: i32,
    pub revents: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RawSyscallFrame {
    pub rc: libc::c_long,
    pub errno: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedWait {
    pub args: WaitArgs,
    pub attempt: Option<ExpectedId>,
    pub pending_status: Option<Symbol>,
    pub frame: RawWaitidFrame,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedSignal {
    pub args: SignalArgs,
    pub transaction: Option<ExpectedId>,
    pub attempt: Option<ExpectedId>,
    pub frame: RawSyscallFrame,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedPoll {
    pub args: PollArgs,
    pub transaction: Option<ExpectedId>,
    pub source_status: Option<ExpectedId>,
    pub frame: RawPollFrame,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedContinue {
    pub args: ContinueArgs,
    pub transaction: Option<ExpectedId>,
    pub source_status: Option<ExpectedId>,
    pub attempt: Option<ExpectedId>,
    pub frame: RawSyscallFrame,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Step {
    Bind(ExpectedBind),
    BindStatus(ExpectedBindStatus),
    AuditTransaction(ExpectedTransactionAudit),
    Wait(ExpectedWait),
    Signal(ExpectedSignal),
    Poll(ExpectedPoll),
    Continue(ExpectedContinue),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ActualCall {
    Bind {
        kind: SymbolKind,
        raw: u64,
        site: ActualBindSite,
    },
    AuditTransaction(ActualTransactionAudit),
    Wait(WaitArgs),
    Signal(SignalArgs),
    Poll(PollArgs),
    Continue(ContinueArgs),
    BindStatus { wait_attempt: u64, status: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Violation {
    pub call_index: usize,
    pub message: &'static str,
    pub expected: Option<Step>,
    pub actual: ActualCall,
    pub caller: thread::ThreadId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpectedViolation {
    pub call_index: usize,
    pub message: &'static str,
    pub expected: Option<Step>,
    pub actual: ActualCall,
}

impl Violation {
    fn matches(&self, expected: &ExpectedViolation) -> bool {
        self.call_index == expected.call_index
            && self.message == expected.message
            && self.expected == expected.expected
            && self.actual == expected.actual
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompletionExpectation {
    HarnessOnly,
    ProtocolWorkerDone(RegistryCompletionExpectation),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RegistryCompletionExpectation {
    Removed,
    ConfirmedAbsent,
}

impl RegistryCompletionExpectation {
    pub(super) fn receipt(self) -> u8 {
        match self {
            Self::Removed => 1,
            Self::ConfirmedAbsent => 2,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompletionStage {
    Prepared = 1,
    RegistryRemoved = 2,
    RegistryConfirmedAbsent = 3,
    DoneAfterRemoval = 4,
    DoneAfterConfirmedAbsent = 5,
}

#[derive(Clone, Debug)]
struct Script {
    fidelity: Fidelity,
    completion: CompletionExpectation,
    entries: VecDeque<Step>,
    bound: Vec<BoundSymbol>,
    reverse: HashSet<SymbolValue>,
    pending_status: Option<PendingStatus>,
    spent_signals: HashSet<(Option<u64>, i32)>,
    spent_continues: HashSet<(Option<u64>, Option<u64>)>,
    deferred_audits: HashMap<u64, OperationToken>,
    wait_post_dispatch_pause: Option<(Arc<Barrier>, Arc<Barrier>)>,
    driver_tids: HashSet<i32>,
    first_violation: Option<Violation>,
    fatal_lifecycle: Option<Violation>,
    consumed: usize,
}

impl Script {
    fn new(
        fidelity: Fidelity,
        completion: CompletionExpectation,
        entries: Vec<Step>,
    ) -> Self {
        Self {
            fidelity,
            completion,
            entries: entries.into(),
            bound: Vec::new(),
            reverse: HashSet::new(),
            pending_status: None,
            spent_signals: HashSet::new(),
            spent_continues: HashSet::new(),
            deferred_audits: HashMap::new(),
            wait_post_dispatch_pause: None,
            driver_tids: HashSet::new(),
            first_violation: None,
            fatal_lifecycle: None,
            consumed: 0,
        }
    }

    fn violation(&mut self, message: &'static str, expected: Option<Step>, actual: ActualCall) {
        if self.first_violation.is_none() {
            self.first_violation = Some(Violation {
                call_index: self.consumed,
                message,
                expected,
                actual,
                caller: thread::current().id(),
            });
        }
    }

    fn fatal_lifecycle(&mut self, message: &'static str, actual: ActualCall) {
        if self.fatal_lifecycle.is_none() {
            self.fatal_lifecycle = Some(Violation {
                call_index: self.consumed,
                message,
                expected: self.entries.front().cloned(),
                actual,
                caller: thread::current().id(),
            });
        }
    }

    fn advance_consumed(&mut self, actual: ActualCall) -> bool {
        let Some(next) = self.consumed.checked_add(1) else {
            self.fatal_lifecycle("startup script consumed-call count overflowed", actual);
            return false;
        };
        self.consumed = next;
        true
    }

    fn bind_or_match(
        &mut self,
        expected: ExpectedId,
        actual: SymbolValue,
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
    ) -> bool {
        match expected {
            ExpectedId::Exact { kind, raw } => actual == SymbolValue { kind, raw },
            ExpectedId::Bind { symbol, kind } => {
                if actual.kind != kind
                    || self.bound.iter().any(|bound| bound.symbol == symbol)
                    || self.reverse.contains(&actual)
                {
                    return false;
                }
                self.bound.push(BoundSymbol {
                    symbol,
                    value: actual,
                    nonce,
                    generation,
                    bound_at: self.consumed,
                });
                self.reverse.insert(actual);
                true
            }
            ExpectedId::Ref { symbol, kind } => self.bound.iter().any(|bound| {
                bound.symbol == symbol
                    && bound.value == actual
                    && bound.value.kind == kind
                    && bound.nonce == nonce
                    && bound.generation == generation
                    && bound.bound_at < self.consumed
            }),
        }
    }

    fn bind_generated(
        &mut self,
        symbol: Symbol,
        actual: SymbolValue,
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
    ) -> bool {
        self.bind_or_match(
            ExpectedId::Bind {
                symbol,
                kind: actual.kind,
            },
            actual,
            nonce,
            generation,
        )
    }

    fn ids_match_ref(
        &mut self,
        expected: Option<ExpectedId>,
        actual: Option<u64>,
        kind: SymbolKind,
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
    ) -> bool {
        match (expected, actual) {
            (None, None) => true,
            (Some(ExpectedId::Ref { symbol, kind: expected_kind }), Some(raw))
                if expected_kind == kind => self.bind_or_match(
                    ExpectedId::Ref {
                        symbol,
                        kind: expected_kind,
                    },
                    SymbolValue { kind, raw },
                    nonce,
                    generation,
                ),
            _ => false,
        }
    }

    fn expected_id_already_matches(
        &self,
        expected: ExpectedId,
        raw: u64,
        kind: SymbolKind,
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
    ) -> bool {
        match expected {
            ExpectedId::Exact {
                kind: expected_kind,
                raw: expected_raw,
            } => expected_kind == kind && expected_raw == raw,
            ExpectedId::Ref {
                symbol,
                kind: expected_kind,
            } => {
                expected_kind == kind
                    && self.bound.iter().any(|bound| {
                        bound.symbol == symbol
                            && bound.value == SymbolValue { kind, raw }
                            && bound.nonce == nonce
                            && bound.generation == generation
                            && bound.bound_at < self.consumed
                    })
            }
            ExpectedId::Bind { .. } => false,
        }
    }

    fn bind_site_matches(
        &mut self,
        expected: ExpectedBindSite,
        actual: ActualBindSite,
        nonce: NonZeroU64,
        script_generation: PhysicalEventGenerationId,
    ) -> bool {
        match (expected, actual) {
            (
                ExpectedBindSite::WaitAttempt {
                    generation: expected_generation,
                    task: expected_task,
                    producer: expected_producer,
                    flags: expected_flags,
                },
                ActualBindSite::WaitAttempt {
                    generation: actual_generation,
                    task,
                    producer,
                    flags,
                },
            ) => {
                expected_generation == actual_generation
                    && expected_task == task
                    && expected_producer == producer
                    && expected_flags == flags
            }
            (ExpectedBindSite::Transaction(expected), ActualBindSite::Transaction(actual)) => {
                expected == actual
            }
            (
                ExpectedBindSite::SignalAttempt {
                    generation: expected_generation,
                    task: expected_task,
                    transaction,
                    pidfd: expected_pidfd,
                    signal: expected_signal,
                },
                ActualBindSite::SignalAttempt {
                    generation: actual_generation,
                    task,
                    transaction: actual_transaction,
                    pidfd,
                    signal,
                },
            ) => {
                expected_generation == actual_generation
                    && expected_task == task
                    && expected_pidfd == pidfd
                    && expected_signal == signal
                    && self.ids_match_ref(
                        Some(transaction),
                        Some(actual_transaction),
                        SymbolKind::Transaction,
                        nonce,
                        script_generation,
                    )
            }
            (
                ExpectedBindSite::ResumeAttempt {
                    generation: expected_generation,
                    task: expected_task,
                    source_status,
                    operation: expected_operation,
                    signal: expected_signal,
                    owner: expected_owner,
                },
                ActualBindSite::ResumeAttempt {
                    generation: actual_generation,
                    task,
                    source_status: actual_source_status,
                    operation,
                    signal,
                    owner,
                },
            ) => {
                expected_generation == actual_generation
                    && expected_task == task
                    && expected_operation == operation
                    && expected_signal == signal
                    && expected_owner == owner
                    && self.ids_match_ref(
                        source_status,
                        actual_source_status,
                        SymbolKind::Status,
                        nonce,
                        script_generation,
                    )
            }
            _ => false,
        }
    }

    fn transaction_audit_matches(
        &mut self,
        expected: ExpectedTransactionAudit,
        actual: ActualTransactionAudit,
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
    ) -> bool {
        expected.site == actual.site
            && self.ids_match_ref(
                Some(expected.transaction),
                Some(actual.transaction),
                SymbolKind::Transaction,
                nonce,
                generation,
            )
            && self.ids_match_ref(
                Some(expected.cause_wait),
                Some(actual.cause_wait),
                SymbolKind::WaitAttempt,
                nonce,
                generation,
            )
            && self.ids_match_ref(
                expected.barrier_wait,
                actual.barrier_wait,
                SymbolKind::WaitAttempt,
                nonce,
                generation,
            )
    }

    fn faithful_wait_call(args: WaitArgs) -> bool {
        if args.idtype != libc::P_PIDFD
            || args.binding.pidfd < 0
            || args.id != args.binding.pidfd as libc::id_t
            || args.binding.pid.as_raw() <= 0
            || args.binding.task.tid() != args.binding.pid.as_raw()
            || args.binding.task.pidfd() != Some(args.binding.pidfd)
        {
            return false;
        }

        let blocking_cleanup = libc::WEXITED | libc::WSTOPPED | libc::__WALL;
        let canonical = match (args.site, args.producer) {
            (WaitSite::RetainedBarrier, PhysicalWaitProducer::PreRegistrationBarrier) => {
                blocking_cleanup | libc::WNOWAIT
            }
            (
                WaitSite::ObservedCleanup,
                PhysicalWaitProducer::NotifierWorker
                | PhysicalWaitProducer::SynchronousWait
                | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                | PhysicalWaitProducer::RegisteredCleanup,
            ) => blocking_cleanup,
            (
                WaitSite::ObservedCleanup,
                PhysicalWaitProducer::AuthorizedRootNotifier,
            ) => blocking_cleanup | libc::WCONTINUED,
            (
                WaitSite::ObservedCleanup,
                PhysicalWaitProducer::PreStopContinuedDrain,
            ) => libc::WCONTINUED | libc::WNOHANG | libc::__WALL,
            (
                WaitSite::UnobservedBoundaryProbe,
                PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            ) => blocking_cleanup | libc::WNOHANG,
            (
                WaitSite::UnobservedDrain,
                PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            ) => blocking_cleanup,
            _ => return false,
        };
        args.options == canonical
    }

    fn faithful_wait(frame: RawWaitidFrame, args: WaitArgs) -> bool {
        if !Self::faithful_wait_call(args) {
            return false;
        }
        match (frame.rc, frame.errno, frame.siginfo) {
            (-1, error, None) => matches!(error, libc::EINTR | libc::ECHILD),
            (0, 0, Some(siginfo)) => {
                if siginfo.pid == 0 {
                    args.options & WaitPidFlag::WNOHANG.bits() != 0
                        && matches!(
                            waitid::classify_physical_wait_siginfo(
                                siginfo.signo,
                                siginfo.errno,
                                siginfo.code,
                                siginfo.pid,
                                siginfo.uid,
                                siginfo.status,
                            ),
                            Ok(waitid::PhysicalWaitSiginfoClass::NoStatus)
                        )
                } else {
                    let result_flag = match siginfo.code {
                        libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED => {
                            WaitPidFlag::WEXITED.bits()
                        }
                        libc::CLD_STOPPED | libc::CLD_TRAPPED => {
                            WaitPidFlag::WSTOPPED.bits()
                        }
                        libc::CLD_CONTINUED => WaitPidFlag::WCONTINUED.bits(),
                        _ => return false,
                    };
                    siginfo.pid == args.binding.pid.as_raw()
                        && args.options & result_flag != 0
                        && matches!(
                            waitid::classify_physical_wait_siginfo(
                                siginfo.signo,
                                siginfo.errno,
                                siginfo.code,
                                siginfo.pid,
                                siginfo.uid,
                                siginfo.status,
                            ),
                            Ok(waitid::PhysicalWaitSiginfoClass::Typed(_)
                                | waitid::PhysicalWaitSiginfoClass::ValidButTypedUnsupported)
                        )
                }
            }
            _ => false,
        }
    }

    fn faithful_owned_pidfd(binding: CallBinding) -> bool {
        binding.pid.as_raw() > 0
            && binding.pidfd >= 0
            && binding.task.tid() == binding.pid.as_raw()
            && binding.task.pidfd() == Some(binding.pidfd)
    }

    fn faithful_signal(frame: RawSyscallFrame, args: SignalArgs) -> bool {
        let exact_site = match args.site {
            SignalSite::ObservedCleanup => {
                args.binding.transaction.is_some()
                    && args.binding.source_status.is_none()
                    && args.attempt.is_some()
            }
            SignalSite::UnobservedCleanup => {
                args.binding.transaction.is_none()
                    && args.binding.source_status.is_none()
                    && args.attempt.is_none()
            }
        };
        if !exact_site
            || !Self::faithful_owned_pidfd(args.binding)
            || args.signal != libc::SIGKILL
            || !args.siginfo_is_null
            || args.flags != 0
        {
            return false;
        }
        (frame.rc == 0 && frame.errno == 0)
            || (frame.rc == -1
                && matches!(frame.errno, libc::ESRCH | libc::EPERM | libc::ENOMEM))
    }

    fn faithful_poll(frame: RawPollFrame, args: PollArgs) -> bool {
        let exact_site = match args.purpose {
            PollPurpose::ResumeFailure => {
                args.timeout_ms == 0
                    && args.binding.transaction.is_some()
                    && args.binding.source_status.is_some()
            }
            PollPurpose::ReservedSignalFailure
            | PollPurpose::WaitFailure
            | PollPurpose::SignalFailure
            | PollPurpose::CapacityFence => {
                args.timeout_ms == 0
                    && args.binding.transaction.is_some()
                    && args.binding.source_status.is_none()
            }
            PollPurpose::TerminalEchild | PollPurpose::SetupNoStatusEchild => {
                args.timeout_ms == -1
                    && args.binding.transaction.is_some()
                    && args.binding.source_status.is_none()
            }
            PollPurpose::UnobservedSignalFailure | PollPurpose::UnobservedBoundary => {
                args.timeout_ms == 0
                    && args.binding.transaction.is_none()
                    && args.binding.source_status.is_none()
            }
        };
        if !exact_site
            || !Self::faithful_owned_pidfd(args.binding)
            || args.events != libc::POLLIN
        {
            return false;
        }
        match (frame.rc, frame.errno, frame.revents) {
            (-1, error, 0) => matches!(error, libc::EINTR | libc::ENOMEM),
            (0, 0, 0) => args.timeout_ms == 0,
            (1, 0, revents) => {
                revents & libc::POLLIN != 0
                    && revents & (libc::POLLERR | libc::POLLNVAL) == 0
            }
            _ => false,
        }
    }

    fn faithful_continue(frame: RawSyscallFrame, args: ContinueArgs) -> bool {
        let exact_site = match args.site {
            ContinueSite::ObservedCleanup => {
                args.binding.transaction.is_some()
                    && args.binding.source_status.is_some()
                    && args.attempt.is_some()
                    && matches!(
                        args.owner,
                        PhysicalResumeOwner::StartupBarrierCleanup
                            | PhysicalResumeOwner::AuthorizedRootExternalCleanup
                    )
            }
            ContinueSite::UnobservedCleanup => {
                args.binding.transaction.is_none()
                    && args.binding.source_status.is_none()
                    && args.attempt.is_none()
                    && args.owner == PhysicalResumeOwner::StartupBarrierCleanup
            }
        };
        if !exact_site
            || !Self::faithful_owned_pidfd(args.binding)
            || args.request != libc::PTRACE_CONT
            || args.target_tid != args.binding.pid
            || args.addr != 0
            || args.data != 0
        {
            return false;
        }
        (frame.rc == 0 && frame.errno == 0)
            || (frame.rc == -1
                && matches!(frame.errno, libc::ESRCH | libc::EIO | libc::EPERM))
    }
}

#[derive(Debug)]
pub(super) enum CloseOutcome {
    ProtocolPassed,
    HarnessOnlyPassed,
    ExpectedViolation(Violation),
    FatalLifecycle(Violation),
    Abandoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReceiptOutcome {
    ProtocolPassed,
    HarnessOnlyPassed,
    ExpectedViolation,
}

/// Non-constructible evidence that one exact script generation closed with a
/// specific successful outcome.  Callers may not infer protocol coverage from
/// `Ok(())`; they must explicitly refine this receipt.
#[must_use = "startup script close receipts must be classified"]
#[derive(Debug)]
pub(super) struct ScriptCloseReceipt {
    nonce: NonZeroU64,
    generation: PhysicalEventGenerationId,
    outcome: ReceiptOutcome,
}

/// Refined proof that the script exercised the full production completion
/// protocol, rather than only the raw harness or an expected-negative case.
#[must_use = "protocol pass receipts are the coverage evidence"]
#[derive(Debug)]
pub(super) struct ProtocolPassReceipt {
    receipt: ScriptCloseReceipt,
}

impl ScriptCloseReceipt {
    pub(super) fn nonce(&self) -> u64 {
        self.nonce.get()
    }

    pub(super) fn generation(&self) -> PhysicalEventGenerationId {
        self.generation
    }

    pub(super) fn expect_protocol_passed(
        self,
    ) -> Result<ProtocolPassReceipt, ScriptCloseReceipt> {
        if self.outcome == ReceiptOutcome::ProtocolPassed {
            Ok(ProtocolPassReceipt { receipt: self })
        } else {
            Err(self)
        }
    }

    #[cfg(test)]
    pub(super) fn is_harness_only(&self) -> bool {
        self.outcome == ReceiptOutcome::HarnessOnlyPassed
    }

    #[cfg(test)]
    pub(super) fn is_expected_violation(&self) -> bool {
        self.outcome == ReceiptOutcome::ExpectedViolation
    }

    #[cfg(test)]
    pub(super) fn expect_harness_only(self) {
        assert!(
            self.is_harness_only(),
            "startup close receipt was not HarnessOnlyPassed: {self:?}"
        );
    }

    #[cfg(test)]
    pub(super) fn expect_expected_violation(self) {
        assert!(
            self.is_expected_violation(),
            "startup close receipt was not ExpectedViolation: {self:?}"
        );
    }
}

impl ProtocolPassReceipt {
    pub(super) fn nonce(&self) -> u64 {
        self.receipt.nonce()
    }

    pub(super) fn generation(&self) -> PhysicalEventGenerationId {
        self.receipt.generation()
    }
}

#[derive(Debug)]
pub(super) enum Slot {
    Vacant { next_nonce: NonZeroU64 },
    Real {
        generation: PhysicalEventGenerationId,
        active_operations: usize,
        next_operation: NonZeroU64,
        active_tokens: HashSet<NonZeroU64>,
        fatal_lifecycle: Option<Violation>,
    },
    Open {
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
        script: Script,
        active_dispatchers: usize,
        next_operation: NonZeroU64,
        active_tokens: HashSet<NonZeroU64>,
        driver_leases: usize,
    },
    Closing {
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
        script: Script,
        active_dispatchers: usize,
        next_operation: NonZeroU64,
        active_tokens: HashSet<NonZeroU64>,
        driver_leases: usize,
        late_violation: Option<Violation>,
    },
    Closed {
        nonce: NonZeroU64,
        generation: PhysicalEventGenerationId,
        outcome: CloseOutcome,
        late_violation: Option<Violation>,
    },
}

impl Slot {
    pub(super) fn vacant() -> Self {
        Self::Vacant {
            next_nonce: NonZeroU64::MIN,
        }
    }

    fn record_late(&mut self, actual: ActualCall) {
        let violation = late_violation(actual);
        match self {
            Self::Closing { late_violation, .. } => {
                late_violation.get_or_insert(violation);
            }
            Self::Closed {
                outcome,
                late_violation,
                ..
            } => {
                let violation = late_violation.get_or_insert(violation).clone();
                if matches!(
                    outcome,
                    CloseOutcome::ProtocolPassed
                        | CloseOutcome::HarnessOnlyPassed
                        | CloseOutcome::ExpectedViolation(_)
                ) {
                    *outcome = CloseOutcome::FatalLifecycle(violation);
                }
            }
            _ => {}
        }
    }
}

pub(super) enum Dispatch<'a, T> {
    Real(OperationLease<'a>),
    Scripted(T, OperationLease<'a>),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct OperationToken {
    nonce: Option<NonZeroU64>,
    generation: PhysicalEventGenerationId,
    operation: NonZeroU64,
}

enum OperationKind {
    Real {
        token: OperationToken,
        actual: ActualCall,
    },
    Scripted {
        token: OperationToken,
        actual: ActualCall,
    },
    Rejected,
}

#[must_use = "startup raw operations must be explicitly completed"]
pub(super) struct OperationLease<'a> {
    event: &'a Event,
    kind: Option<OperationKind>,
    origin_tid: Pid,
    not_send: PhantomData<Rc<()>>,
}

fn operation_lease(event: &Event, kind: OperationKind) -> OperationLease<'_> {
    OperationLease {
        event,
        kind: Some(kind),
        origin_tid: caller_tid(),
        not_send: PhantomData,
    }
}

fn allocate_operation_token(
    next_operation: &mut NonZeroU64,
    active_tokens: &mut HashSet<NonZeroU64>,
    nonce: Option<NonZeroU64>,
    generation: PhysicalEventGenerationId,
) -> Option<OperationToken> {
    let operation = *next_operation;
    let next = operation
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)?;
    if !active_tokens.insert(operation) {
        return None;
    }
    *next_operation = next;
    Some(OperationToken {
        nonce,
        generation,
        operation,
    })
}

fn token_matches(
    token: OperationToken,
    nonce: Option<NonZeroU64>,
    generation: PhysicalEventGenerationId,
    active_tokens: &HashSet<NonZeroU64>,
) -> bool {
    token.nonce == nonce
        && token.generation == generation
        && active_tokens.contains(&token.operation)
}

impl<T> Dispatch<'_, T> {
    #[cfg(test)]
    pub(super) fn expect_scripted(self) -> T {
        match self {
            Self::Scripted(value, operation) => {
                operation.complete();
                value
            }
            Self::Real(operation) => {
                operation.complete();
                panic!("startup syscall script unexpectedly selected the real kernel")
            }
        }
    }
}

impl OperationLease<'_> {
    pub(super) fn token(&self) -> Option<OperationToken> {
        match self.kind.as_ref()? {
            OperationKind::Real { token, .. } | OperationKind::Scripted { token, .. } => {
                Some(*token)
            }
            OperationKind::Rejected => None,
        }
    }

    pub(super) fn complete(mut self) {
        self.finish(false);
    }

    pub(super) fn pause_after_wait_dispatch(&self) {
        let pause = {
            let mut slot = self.event.startup_syscall_script.lock();
            match &mut *slot {
                Slot::Open { script, .. } | Slot::Closing { script, .. } => {
                    script.wait_post_dispatch_pause.take()
                }
                Slot::Vacant { .. } | Slot::Real { .. } | Slot::Closed { .. } => None,
            }
        };
        if let Some((entered, resume)) = pause {
            entered.wait();
            resume.wait();
        }
    }

    fn finish(&mut self, abandoned: bool) {
        let Some(kind) = self.kind.take() else {
            return;
        };
        let mut slot = self.event.startup_syscall_script.lock();
        let wrong_thread = caller_tid() != self.origin_tid;
        if wrong_thread {
            match &mut *slot {
                Slot::Open { script, .. } | Slot::Closing { script, .. } => {
                    script.fatal_lifecycle(
                        "startup raw operation lease completed on the wrong thread",
                        lifecycle_actual(self.event),
                    );
                }
                Slot::Real { fatal_lifecycle, .. } => {
                    fatal_lifecycle.get_or_insert_with(|| Violation {
                        call_index: 0,
                        message: "real startup raw operation lease completed on the wrong thread",
                        expected: None,
                        actual: lifecycle_actual(self.event),
                        caller: thread::current().id(),
                    });
                }
                Slot::Vacant { .. } | Slot::Closed { .. } => {}
            }
        }
        match (kind, &mut *slot) {
            (
                OperationKind::Real { token, actual },
                Slot::Real {
                    generation,
                    active_operations,
                    active_tokens,
                    fatal_lifecycle,
                    ..
                },
            ) if token_matches(token, None, *generation, active_tokens) => {
                active_tokens.remove(&token.operation);
                match active_operations.checked_sub(1) {
                    Some(remaining) => *active_operations = remaining,
                    None => {
                        fatal_lifecycle.get_or_insert_with(|| Violation {
                            call_index: 0,
                            message: "real startup operation count underflowed",
                            expected: None,
                            actual: actual.clone(),
                            caller: thread::current().id(),
                        });
                    }
                }
                if abandoned {
                    fatal_lifecycle.get_or_insert_with(|| Violation {
                        call_index: 0,
                        message: "real startup raw operation lease was abandoned",
                        expected: None,
                        actual,
                        caller: thread::current().id(),
                    });
                }
            }
            (
                OperationKind::Scripted { token, actual },
                Slot::Open {
                    nonce,
                    generation,
                    script,
                    active_dispatchers,
                    active_tokens,
                    ..
                }
                | Slot::Closing {
                    nonce,
                    generation,
                    script,
                    active_dispatchers,
                    active_tokens,
                    ..
                },
            ) if token_matches(token, Some(*nonce), *generation, active_tokens) => {
                let deferred_wait = if !abandoned && !wrong_thread {
                    match (&actual, script.entries.front()) {
                        (
                            ActualCall::Wait(WaitArgs {
                                attempt: Some(wait_attempt),
                                ..
                            }),
                            Some(Step::AuditTransaction(expected)),
                        ) if script.expected_id_already_matches(
                            expected.cause_wait,
                            *wait_attempt,
                            SymbolKind::WaitAttempt,
                            *nonce,
                            *generation,
                        ) => Some(*wait_attempt),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(wait_attempt) = deferred_wait {
                    if script.deferred_audits.insert(wait_attempt, token).is_some() {
                        script.fatal_lifecycle(
                            "startup wait operation already had deferred audit ownership",
                            actual,
                        );
                    } else {
                        self.event.startup_syscall_script_changed.notify_all();
                        return;
                    }
                }
                active_tokens.remove(&token.operation);
                match active_dispatchers.checked_sub(1) {
                    Some(remaining) => *active_dispatchers = remaining,
                    None => script.fatal_lifecycle(
                        "startup raw operation count underflowed",
                        actual.clone(),
                    ),
                }
                if abandoned {
                    script.fatal_lifecycle(
                        "startup raw operation lease was abandoned",
                        actual,
                    );
                }
            }
            (OperationKind::Rejected, _) => {}
            (
                OperationKind::Real { actual, .. },
                Slot::Real { fatal_lifecycle, .. },
            ) => {
                fatal_lifecycle.get_or_insert_with(|| Violation {
                    call_index: 0,
                    message: "real startup operation token was not active",
                    expected: None,
                    actual,
                    caller: thread::current().id(),
                });
            }
            (
                OperationKind::Scripted { actual, .. },
                Slot::Open { script, .. } | Slot::Closing { script, .. },
            ) => script.fatal_lifecycle(
                "startup raw operation token was not active",
                actual,
            ),
            (_, late @ (Slot::Closing { .. } | Slot::Closed { .. })) => {
                late.record_late(ActualCall::Poll(PollArgs {
                    purpose: PollPurpose::UnobservedBoundary,
                    binding: CallBinding::new(self.event, Pid::from_raw(0), -1, PhysicalTaskIdentity::direct_child(Pid::from_raw(0)), None, None),
                    events: 0,
                    timeout_ms: 0,
                }));
            }
            _ => {}
        }
        self.event.startup_syscall_script_changed.notify_all();
    }
}

pub(super) fn arm_wait_post_dispatch_pause(
    event: &Event,
    entered: Arc<Barrier>,
    resume: Arc<Barrier>,
) -> Result<(), Errno> {
    let mut slot = event.startup_syscall_script.lock();
    match &mut *slot {
        Slot::Open { script, .. } if script.wait_post_dispatch_pause.is_none() => {
            script.wait_post_dispatch_pause = Some((entered, resume));
            Ok(())
        }
        _ => Err(Errno::EBUSY),
    }
}

impl Drop for OperationLease<'_> {
    fn drop(&mut self) {
        self.finish(true);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct EngineSnapshot {
    pub head: Option<Step>,
    pub remaining: usize,
    pub bound: usize,
    pub reverse: usize,
    pub pending_status: bool,
    pub spent_signals: usize,
    pub spent_continues: usize,
    pub consumed: usize,
    pub first_violation: Option<Violation>,
    pub fatal_lifecycle: Option<Violation>,
}

pub(super) fn snapshot(event: &Event) -> Option<EngineSnapshot> {
    let slot = event.startup_syscall_script.lock();
    let script = match &*slot {
        Slot::Open { script, .. } | Slot::Closing { script, .. } => script,
        Slot::Vacant { .. } | Slot::Real { .. } | Slot::Closed { .. } => return None,
    };
    Some(EngineSnapshot {
        head: script.entries.front().cloned(),
        remaining: script.entries.len(),
        bound: script.bound.len(),
        reverse: script.reverse.len(),
        pending_status: script.pending_status.is_some(),
        spent_signals: script.spent_signals.len(),
        spent_continues: script.spent_continues.len(),
        consumed: script.consumed,
        first_violation: script.first_violation.clone(),
        fatal_lifecycle: script.fatal_lifecycle.clone(),
    })
}

fn caller_tid() -> Pid {
    Pid::from_raw(unsafe { libc::syscall(libc::SYS_gettid) as i32 })
}

fn caller_has_driver(script: &Script) -> bool {
    script.driver_tids.contains(&caller_tid().as_raw())
}

impl CallBinding {
    pub(super) fn new(
        event: &Event,
        pid: Pid,
        pidfd: i32,
        task: PhysicalTaskIdentity,
        transaction: Option<PhysicalCleanupTransaction>,
        source_status: Option<PhysicalStatusId>,
    ) -> Self {
        Self {
            generation: event.generation,
            pid,
            pidfd,
            task,
            transaction: transaction.map(|transaction| transaction.id().get()),
            source_status: source_status.map(PhysicalStatusId::get),
            caller_tid: caller_tid(),
        }
    }
}

fn binding_static_eq(expected: CallBinding, actual: CallBinding) -> bool {
    expected.generation == actual.generation
        && expected.pid == actual.pid
        && expected.pidfd == actual.pidfd
        && expected.task == actual.task
        && expected.caller_tid == actual.caller_tid
}

fn wait_args_static_eq(expected: WaitArgs, actual: WaitArgs) -> bool {
    expected.site == actual.site
        && binding_static_eq(expected.binding, actual.binding)
        && expected.producer == actual.producer
        && expected.idtype == actual.idtype
        && expected.id == actual.id
        && expected.options == actual.options
}

fn signal_args_static_eq(expected: SignalArgs, actual: SignalArgs) -> bool {
    expected.site == actual.site
        && binding_static_eq(expected.binding, actual.binding)
        && expected.signal == actual.signal
        && expected.siginfo_is_null == actual.siginfo_is_null
        && expected.flags == actual.flags
}

fn poll_args_static_eq(expected: PollArgs, actual: PollArgs) -> bool {
    expected.purpose == actual.purpose
        && binding_static_eq(expected.binding, actual.binding)
        && expected.events == actual.events
        && expected.timeout_ms == actual.timeout_ms
}

fn continue_args_static_eq(expected: ContinueArgs, actual: ContinueArgs) -> bool {
    expected.site == actual.site
        && binding_static_eq(expected.binding, actual.binding)
        && expected.request == actual.request
        && expected.target_tid == actual.target_tid
        && expected.addr == actual.addr
        && expected.data == actual.data
        && expected.owner == actual.owner
}

fn eproto_wait() -> RawWaitidFrame {
    RawWaitidFrame {
        rc: -1,
        errno: Errno::EPROTO.into_raw(),
        siginfo: None,
    }
}

fn eproto_poll() -> RawPollFrame {
    RawPollFrame {
        rc: -1,
        errno: Errno::EPROTO.into_raw(),
        revents: 0,
    }
}

fn eproto_syscall() -> RawSyscallFrame {
    RawSyscallFrame {
        rc: -1,
        errno: Errno::EPROTO.into_raw(),
    }
}

fn late_violation(actual: ActualCall) -> Violation {
    Violation {
        call_index: usize::MAX,
        message: "startup syscall after script began closing",
        expected: None,
        actual,
        caller: thread::current().id(),
    }
}

fn block_before_call(script: &mut Script, actual: ActualCall) -> bool {
    if script.fatal_lifecycle.is_some() {
        return true;
    }
    if script.first_violation.is_some() {
        return true;
    }
    if script.pending_status.is_some() {
        script.violation(
            "required BindStatus was not supplied before the next call",
            script.entries.front().cloned(),
            actual,
        );
        return true;
    }
    false
}

fn bind_generated_value(
    event: &Event,
    kind: SymbolKind,
    raw: u64,
    site: ActualBindSite,
) -> Result<(), Errno> {
    claim_lifecycle_activity(event)?;
    let mut slot = event.startup_syscall_script.lock();
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 0,
                next_operation: NonZeroU64::MIN,
                active_tokens: HashSet::new(),
                fatal_lifecycle: None,
            };
            Ok(())
        }
        Slot::Real {
            fatal_lifecycle: Some(_),
            ..
        } => Err(Errno::EPROTO),
        Slot::Real { .. } => Ok(()),
        Slot::Open { nonce, generation, script, .. } => {
            let actual = ActualCall::Bind { kind, raw, site };
            if script.first_violation.is_some() || script.fatal_lifecycle.is_some() {
                return Err(Errno::EPROTO);
            }
            if script.pending_status.is_some() {
                script.violation(
                    "required BindStatus was not supplied before generated allocation",
                    script.entries.front().cloned(),
                    actual,
                );
                return Err(Errno::EPROTO);
            }
            let head = script.entries.front().cloned();
            let mut trial = script.clone();
            let success = match trial.entries.pop_front() {
                Some(Step::Bind(expected)) if expected.kind == kind => {
                    let site_matches = trial.bind_site_matches(
                        expected.site,
                        site,
                        *nonce,
                        *generation,
                    );
                    site_matches
                        && trial.bind_generated(
                            expected.symbol,
                            SymbolValue { kind, raw },
                            *nonce,
                            *generation,
                        )
                }
                _ => false,
            };
            if success {
                let _ = trial.advance_consumed(actual.clone());
                *script = trial;
                Ok(())
            } else {
                script.violation("generated allocation did not match Bind step", head, actual);
                Err(Errno::EPROTO)
            }
        }
        late @ (Slot::Closing { .. } | Slot::Closed { .. }) => {
            late.record_late(ActualCall::Bind { kind, raw, site });
            Err(Errno::EPROTO)
        }
    }
}

pub(super) fn record_completion_stage(event: &Event, stage: CompletionStage) {
    if claim_lifecycle_activity(event).is_err() {
        return;
    }
    let mut slot = event.startup_syscall_script.lock();
    let current = event.startup_script_completion_stage.load(Ordering::Acquire);
    let expected_prior = match stage {
        CompletionStage::Prepared => 0,
        CompletionStage::RegistryRemoved => CompletionStage::Prepared as u8,
        CompletionStage::RegistryConfirmedAbsent => CompletionStage::Prepared as u8,
        CompletionStage::DoneAfterRemoval => CompletionStage::RegistryRemoved as u8,
        CompletionStage::DoneAfterConfirmedAbsent => {
            CompletionStage::RegistryConfirmedAbsent as u8
        }
    };
    let valid = current == expected_prior;
    let mut accepted = valid;
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            let fatal_lifecycle = (!valid).then(|| Violation {
                call_index: 0,
                message: "real startup completion evidence was repeated or out of order",
                expected: None,
                actual: lifecycle_actual(event),
                caller: thread::current().id(),
            });
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 0,
                next_operation: NonZeroU64::MIN,
                active_tokens: HashSet::new(),
                fatal_lifecycle,
            };
        }
        Slot::Real { fatal_lifecycle, .. } => {
            if fatal_lifecycle.is_some() {
                accepted = false;
            } else if !valid {
                fatal_lifecycle.get_or_insert_with(|| Violation {
                    call_index: 0,
                    message: "real startup completion evidence was repeated or out of order",
                    expected: None,
                    actual: lifecycle_actual(event),
                    caller: thread::current().id(),
                });
            }
        }
        Slot::Open { script, .. } => {
            if script.fatal_lifecycle.is_some() {
                accepted = false;
            } else if !valid {
                script.fatal_lifecycle(
                    "startup completion evidence was repeated or out of order",
                    lifecycle_actual(event),
                );
            }
        }
        late @ (Slot::Closing { .. } | Slot::Closed { .. }) => {
            late.record_late(lifecycle_actual(event));
            return;
        }
    }
    if accepted {
        event
            .startup_script_completion_stage
            .store(stage as u8, Ordering::Release);
    }
}

pub(super) fn record_completion_done(event: &Event) {
    let stage = match event.startup_script_completion_stage.load(Ordering::Acquire) {
        current if current == CompletionStage::RegistryRemoved as u8 => {
            CompletionStage::DoneAfterRemoval
        }
        current if current == CompletionStage::RegistryConfirmedAbsent as u8 => {
            CompletionStage::DoneAfterConfirmedAbsent
        }
        _ => CompletionStage::DoneAfterRemoval,
    };
    record_completion_stage(event, stage);
}

fn record_completion_fatal(event: &Event, message: &'static str) {
    if claim_lifecycle_activity(event).is_err() {
        return;
    }
    let actual = lifecycle_actual(event);
    let mut slot = event.startup_syscall_script.lock();
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 0,
                next_operation: NonZeroU64::MIN,
                active_tokens: HashSet::new(),
                fatal_lifecycle: Some(Violation {
                    call_index: 0,
                    message,
                    expected: None,
                    actual,
                    caller: thread::current().id(),
                }),
            };
        }
        Slot::Real { fatal_lifecycle, .. } => {
            fatal_lifecycle.get_or_insert_with(|| Violation {
                call_index: 0,
                message,
                expected: None,
                actual,
                caller: thread::current().id(),
            });
        }
        Slot::Open { script, .. } | Slot::Closing { script, .. } => {
            script.fatal_lifecycle(message, actual);
        }
        Slot::Closed {
            outcome,
            late_violation,
            ..
        } => {
            if matches!(outcome, CloseOutcome::FatalLifecycle(_)) {
                return;
            }
            let violation = late_violation
                .get_or_insert_with(|| Violation {
                    call_index: usize::MAX,
                    message,
                    expected: None,
                    actual,
                    caller: thread::current().id(),
                })
                .clone();
            *outcome = CloseOutcome::FatalLifecycle(violation);
        }
    }
}

pub(super) fn retain_registry_receipt(
    event: &Event,
    pid: Pid,
    receipt: RegistryCompletionExpectation,
) -> Result<(), Errno> {
    claim_lifecycle_activity(event)?;
    let raw_pid = pid.as_raw();
    if raw_pid <= 0
        || event
            .startup_script_removed_pid
            .compare_exchange(0, raw_pid, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        record_completion_fatal(event, "startup registry receipt was repeated or malformed");
        return Err(Errno::EPROTO);
    }
    if event
        .startup_script_registry_receipt
        .compare_exchange(
            0,
            receipt.receipt(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        record_completion_fatal(event, "startup registry receipt was repeated or malformed");
        return Err(Errno::EPROTO);
    }
    Ok(())
}

pub(super) fn publish_registry_receipt(
    event: &Event,
    pid: Pid,
    receipt: RegistryCompletionExpectation,
) -> Result<(), Errno> {
    claim_lifecycle_activity(event)?;
    if event.startup_script_removed_pid.load(Ordering::Acquire) != pid.as_raw()
        || event
            .startup_script_registry_receipt
            .load(Ordering::Acquire)
            != receipt.receipt()
    {
        record_completion_fatal(event, "startup registry result did not match its receipt");
        return Err(Errno::EPROTO);
    }
    record_completion_stage(
        event,
        match receipt {
            RegistryCompletionExpectation::Removed => CompletionStage::RegistryRemoved,
            RegistryCompletionExpectation::ConfirmedAbsent => {
                CompletionStage::RegistryConfirmedAbsent
            }
        },
    );
    Ok(())
}

pub(super) fn reject_registry_completion(event: &Event) {
    record_completion_fatal(event, "startup registry completion lacked an exact receipt");
}

pub(super) fn bind_generated_for_test(
    event: &Event,
    kind: SymbolKind,
    raw: u64,
    site: ActualBindSite,
) -> Result<(), Errno> {
    bind_generated_value(event, kind, raw, site)
}

pub(super) fn causal_token_for_wait(
    event: &Event,
    wait_attempt: PhysicalWaitAttempt,
) -> Option<OperationToken> {
    let slot = event.startup_syscall_script.lock();
    let script = match &*slot {
        Slot::Open { script, .. } | Slot::Closing { script, .. } => script,
        Slot::Vacant { .. } | Slot::Real { .. } | Slot::Closed { .. } => return None,
    };
    script.deferred_audits.get(&wait_attempt.id().get()).copied()
}

fn retire_deferred_audit(
    script: &mut Script,
    active_dispatchers: &mut usize,
    active_tokens: &mut HashSet<NonZeroU64>,
    token: OperationToken,
    cause_wait: u64,
    actual: ActualCall,
) {
    let deferred_wait = script
        .deferred_audits
        .iter()
        .find_map(|(wait, active)| (*active == token).then_some(*wait));
    let Some(deferred_wait) = deferred_wait else {
        return;
    };
    script.deferred_audits.remove(&deferred_wait);
    if !active_tokens.remove(&token.operation) {
        script.fatal_lifecycle("startup deferred audit token was not active", actual.clone());
    }
    match active_dispatchers.checked_sub(1) {
        Some(remaining) => *active_dispatchers = remaining,
        None => script.fatal_lifecycle(
            "startup deferred audit operation count underflowed",
            actual.clone(),
        ),
    }
    if deferred_wait != cause_wait {
        script.fatal_lifecycle(
            "startup deferred audit token named a different wait",
            actual,
        );
    }
}

pub(super) fn audit_transaction(
    event: &Event,
    token: Option<OperationToken>,
    actual_audit: ActualTransactionAudit,
) -> Result<(), Errno> {
    claim_lifecycle_activity(event)?;
    let mut slot = event.startup_syscall_script.lock();
    let actual = ActualCall::AuditTransaction(actual_audit);
    let closing = matches!(&*slot, Slot::Closing { .. });
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 0,
                next_operation: NonZeroU64::MIN,
                active_tokens: HashSet::new(),
                fatal_lifecycle: None,
            };
            Ok(())
        }
        Slot::Real {
            generation,
            active_tokens,
            fatal_lifecycle,
            ..
        } => {
            if fatal_lifecycle.is_some() {
                return Err(Errno::EPROTO);
            }
            if token.is_some_and(|token| {
                !token_matches(token, None, *generation, active_tokens)
            }) {
                fatal_lifecycle.get_or_insert_with(|| Violation {
                    call_index: 0,
                    message: "real startup postprocessing token was not active",
                    expected: None,
                    actual,
                    caller: thread::current().id(),
                });
                return Err(Errno::EPROTO);
            }
            Ok(())
        }
        Slot::Open {
            nonce,
            generation,
            script,
            active_dispatchers,
            active_tokens,
            ..
        }
        | Slot::Closing {
            nonce,
            generation,
            script,
            active_dispatchers,
            active_tokens,
            ..
        } => {
            let token_valid = token.is_some_and(|token| {
                token_matches(token, Some(*nonce), *generation, active_tokens)
            });
            if (closing && !token_valid) || (token.is_some() && !token_valid) {
                script.fatal_lifecycle(
                    "startup postprocessing token was not active",
                    actual.clone(),
                );
                if let Some(deferred) = script
                    .deferred_audits
                    .get(&actual_audit.cause_wait)
                    .copied()
                {
                    retire_deferred_audit(
                        script,
                        active_dispatchers,
                        active_tokens,
                        deferred,
                        actual_audit.cause_wait,
                        actual,
                    );
                    event.startup_syscall_script_changed.notify_all();
                }
                return Err(Errno::EPROTO);
            }
            let result = if script.first_violation.is_some() || script.fatal_lifecycle.is_some() {
                Err(Errno::EPROTO)
            } else if script.pending_status.is_some() {
                script.violation(
                    "required BindStatus was not supplied before transaction audit",
                    script.entries.front().cloned(),
                    actual.clone(),
                );
                Err(Errno::EPROTO)
            } else {
                let head = script.entries.front().cloned();
                let mut trial = script.clone();
                let success = match trial.entries.pop_front() {
                    Some(Step::AuditTransaction(expected)) => trial.transaction_audit_matches(
                        expected,
                        actual_audit,
                        *nonce,
                        *generation,
                    ),
                    _ => false,
                };
                if success {
                    let _ = trial.advance_consumed(actual.clone());
                    *script = trial;
                    Ok(())
                } else {
                    script.violation(
                        "transaction audit did not match symbolic causes",
                        head,
                        actual.clone(),
                    );
                    Err(Errno::EPROTO)
                }
            };
            if let Some(token) = token {
                retire_deferred_audit(
                    script,
                    active_dispatchers,
                    active_tokens,
                    token,
                    actual_audit.cause_wait,
                    actual,
                );
                event.startup_syscall_script_changed.notify_all();
            }
            result
        }
        late @ Slot::Closed { .. } => {
            late.record_late(actual);
            Err(Errno::EPROTO)
        }
    }
}

pub(super) fn bind_status_for_test(
    event: &Event,
    token: Option<OperationToken>,
    wait_attempt: u64,
    status: u64,
) -> Result<(), Errno> {
    claim_lifecycle_activity(event)?;
    let mut slot = event.startup_syscall_script.lock();
    let actual = ActualCall::BindStatus {
        wait_attempt,
        status,
    };
    let closing = matches!(&*slot, Slot::Closing { .. });
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 0,
                next_operation: NonZeroU64::MIN,
                active_tokens: HashSet::new(),
                fatal_lifecycle: None,
            };
            Ok(())
        }
        Slot::Real {
            generation,
            active_tokens,
            fatal_lifecycle,
            ..
        } => {
            if fatal_lifecycle.is_some() {
                return Err(Errno::EPROTO);
            }
            if token.is_some_and(|token| {
                !token_matches(token, None, *generation, active_tokens)
            }) {
                fatal_lifecycle.get_or_insert_with(|| Violation {
                    call_index: 0,
                    message: "real startup postprocessing token was not active",
                    expected: None,
                    actual,
                    caller: thread::current().id(),
                });
                return Err(Errno::EPROTO);
            }
            Ok(())
        }
        Slot::Open {
            nonce,
            generation,
            script,
            active_tokens,
            ..
        }
        | Slot::Closing {
            nonce,
            generation,
            script,
            active_tokens,
            ..
        } => {
            let token_valid = token.is_some_and(|token| {
                token_matches(token, Some(*nonce), *generation, active_tokens)
            });
            if (closing && !token_valid) || (token.is_some() && !token_valid) {
                script.fatal_lifecycle(
                    "startup postprocessing token was not active",
                    actual,
                );
                return Err(Errno::EPROTO);
            }
            if script.first_violation.is_some() || script.fatal_lifecycle.is_some() {
                return Err(Errno::EPROTO);
            }
            let head = script.entries.front().cloned();
            let mut trial = script.clone();
            let success = match (trial.pending_status, trial.entries.pop_front()) {
                (Some(pending), Some(Step::BindStatus(expected)))
                    if pending.symbol == expected.symbol
                        && pending.wait_attempt == wait_attempt
                        && trial.ids_match_ref(
                            Some(expected.wait_attempt),
                            Some(wait_attempt),
                            SymbolKind::WaitAttempt,
                            *nonce,
                            *generation,
                        ) => trial.bind_generated(
                            expected.symbol,
                            SymbolValue {
                                kind: SymbolKind::Status,
                                raw: status,
                            },
                            *nonce,
                            *generation,
                        ),
                _ => false,
            };
            if success {
                trial.pending_status = None;
                if !trial.advance_consumed(actual.clone()) {
                    *script = trial;
                    return Err(Errno::EOVERFLOW);
                }
                *script = trial;
                Ok(())
            } else {
                script.violation("status allocation did not match BindStatus step", head, actual);
                Err(Errno::EPROTO)
            }
        }
        late @ Slot::Closed { .. } => {
            late.record_late(actual);
            Err(Errno::EPROTO)
        }
    }
}

pub(super) fn bind_wait_attempt(event: &Event, attempt: PhysicalWaitAttempt) {
    let context = attempt.context();
    let _ = bind_generated_value(
        event,
        SymbolKind::WaitAttempt,
        attempt.id().get(),
        ActualBindSite::WaitAttempt {
            generation: context.generation,
            task: context.task,
            producer: context.producer,
            flags: context.flags,
        },
    );
}

pub(super) fn bind_transaction(
    event: &Event,
    transaction: PhysicalCleanupTransaction,
    site: TransactionSite,
) {
    let _ = bind_generated_value(
        event,
        SymbolKind::Transaction,
        transaction.id().get(),
        ActualBindSite::Transaction(site),
    );
}

pub(super) fn bind_signal_attempt(event: &Event, attempt: super::PhysicalPidfdSignalAttempt) {
    let context = attempt.context();
    let _ = bind_generated_value(
        event,
        SymbolKind::SignalAttempt,
        attempt.id().get(),
        ActualBindSite::SignalAttempt {
            generation: context.generation,
            task: context.task,
            transaction: context.transaction.get(),
            pidfd: context.pidfd,
            signal: context.signal,
        },
    );
}

pub(super) fn bind_resume_attempt(event: &Event, attempt: PhysicalResumeAttempt) {
    let context = attempt.context();
    let _ = bind_generated_value(
        event,
        SymbolKind::ResumeAttempt,
        attempt.id().get(),
        ActualBindSite::ResumeAttempt {
            generation: context.generation,
            task: context.task,
            source_status: context.source_status.map(PhysicalStatusId::get),
            operation: context.operation,
            signal: context.signal,
            owner: context.owner,
        },
    );
}

pub(super) fn bind_status(
    event: &Event,
    operation: &OperationLease<'_>,
    wait_attempt: PhysicalWaitAttempt,
    status: PhysicalStatusId,
) -> Result<(), Errno> {
    bind_status_for_test(
        event,
        operation.token(),
        wait_attempt.id().get(),
        status.get(),
    )
}

pub(super) fn dispatch_wait(event: &Event, args: WaitArgs) -> Dispatch<'_, RawWaitidFrame> {
    let actual = ActualCall::Wait(args);
    if claim_lifecycle_activity(event).is_err() {
        return Dispatch::Scripted(
            eproto_wait(),
            operation_lease(event, OperationKind::Rejected),
        );
    }
    let mut slot = event.startup_syscall_script.lock();
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            let mut next_operation = NonZeroU64::MIN;
            let mut active_tokens = HashSet::new();
            let token = allocate_operation_token(
                &mut next_operation,
                &mut active_tokens,
                None,
                event.generation,
            )
            .expect("fresh real operation token allocation failed");
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 1,
                next_operation,
                active_tokens,
                fatal_lifecycle: None,
            };
            Dispatch::Real(operation_lease(
                event,
                OperationKind::Real { token, actual },
            ))
        }
        Slot::Real {
            generation,
            active_operations,
            next_operation,
            active_tokens,
            fatal_lifecycle,
            ..
        } => {
            if fatal_lifecycle.is_some() {
                return Dispatch::Scripted(
                    eproto_wait(),
                    operation_lease(event, OperationKind::Rejected),
                );
            }
            let Some(next) = active_operations.checked_add(1) else {
                fatal_lifecycle.get_or_insert_with(|| Violation {
                    call_index: 0,
                    message: "real startup operation count overflowed",
                    expected: None,
                    actual,
                    caller: thread::current().id(),
                });
                return Dispatch::Scripted(eproto_wait(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(
                next_operation,
                active_tokens,
                None,
                *generation,
            ) else {
                fatal_lifecycle.get_or_insert_with(|| Violation {
                    call_index: 0,
                    message: "real startup operation token overflowed",
                    expected: None,
                    actual,
                    caller: thread::current().id(),
                });
                return Dispatch::Scripted(
                    eproto_wait(),
                    operation_lease(event, OperationKind::Rejected),
                );
            };
            *active_operations = next;
            Dispatch::Real(operation_lease(
                event,
                OperationKind::Real { token, actual },
            ))
        }
        Slot::Open {
            nonce,
            generation,
            script,
            active_dispatchers,
            next_operation,
            active_tokens,
            driver_leases,
        } => {
            if *driver_leases == 0 || !caller_has_driver(script) {
                script.fatal_lifecycle("startup raw operation has no live driver", actual);
                return Dispatch::Scripted(eproto_wait(), operation_lease(event, OperationKind::Rejected));
            }
            let Some(next) = active_dispatchers.checked_add(1) else {
                script.fatal_lifecycle("startup raw operation count overflowed", actual);
                return Dispatch::Scripted(eproto_wait(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(
                next_operation,
                active_tokens,
                Some(*nonce),
                *generation,
            ) else {
                script.fatal_lifecycle("startup raw operation token overflowed", actual);
                return Dispatch::Scripted(
                    eproto_wait(),
                    operation_lease(event, OperationKind::Rejected),
                );
            };
            *active_dispatchers = next;
            let lease_actual = actual.clone();
            let frame = if block_before_call(script, actual.clone()) {
                eproto_wait()
            } else {
                let head = script.entries.front().cloned();
                let mut trial = script.clone();
                let result = match trial.entries.pop_front() {
                    Some(Step::Wait(expected)) => {
                        let valid = wait_args_static_eq(expected.args, args)
                            && trial.ids_match_ref(expected.attempt, args.attempt, SymbolKind::WaitAttempt, *nonce, *generation)
                            && (!matches!(trial.fidelity, Fidelity::LinuxFaithful)
                                || Script::faithful_wait(expected.frame, args));
                        if !valid {
                            Err("wait arguments, symbols, or raw frame differ")
                        } else if let Some(symbol) = expected.pending_status {
                            if expected.frame.rc == 0
                                && expected.frame.siginfo.is_some_and(|siginfo| siginfo.pid != 0)
                                && args.attempt.is_some()
                            {
                                trial.pending_status = Some(PendingStatus {
                                    symbol,
                                    wait_attempt: args.attempt.unwrap_or(0),
                                });
                                Ok(expected.frame)
                            } else {
                                Err("status symbol declared for wait without allocatable status")
                            }
                        } else {
                            Ok(expected.frame)
                        }
                    }
                    _ => Err("unexpected wait call"),
                };
                match result {
                    Ok(frame) => {
                        if !trial.advance_consumed(lease_actual.clone()) {
                            *script = trial;
                            active_tokens.remove(&token.operation);
                            *active_dispatchers = active_dispatchers
                                .checked_sub(1)
                                .expect("admitted wait operation count vanished");
                            return Dispatch::Scripted(eproto_wait(), operation_lease(event, OperationKind::Rejected));
                        }
                        *script = trial;
                        frame
                    }
                    Err(message) => {
                        script.violation(message, head, actual);
                        eproto_wait()
                    }
                }
            };
            Dispatch::Scripted(
                frame,
                operation_lease(
                    event,
                    OperationKind::Scripted {
                        token,
                        actual: lease_actual,
                    },
                ),
            )
        }
        late @ (Slot::Closing { .. } | Slot::Closed { .. }) => {
            late.record_late(ActualCall::Wait(args));
            Dispatch::Scripted(eproto_wait(), operation_lease(event, OperationKind::Rejected))
        }
    }
}

pub(super) fn dispatch_signal(event: &Event, args: SignalArgs) -> Dispatch<'_, RawSyscallFrame> {
    let actual = ActualCall::Signal(args);
    if claim_lifecycle_activity(event).is_err() {
        return Dispatch::Scripted(
            eproto_syscall(),
            operation_lease(event, OperationKind::Rejected),
        );
    }
    let mut slot = event.startup_syscall_script.lock();
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            let mut next_operation = NonZeroU64::MIN;
            let mut active_tokens = HashSet::new();
            let token = allocate_operation_token(&mut next_operation, &mut active_tokens, None, event.generation)
                .expect("fresh real operation token allocation failed");
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 1,
                next_operation,
                active_tokens,
                fatal_lifecycle: None,
            };
            Dispatch::Real(operation_lease(event, OperationKind::Real { token, actual }))
        }
        Slot::Real { generation, active_operations, next_operation, active_tokens, fatal_lifecycle } => {
            if fatal_lifecycle.is_some() {
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            }
            let Some(next) = active_operations.checked_add(1) else {
                fatal_lifecycle.get_or_insert_with(|| Violation { call_index: 0, message: "real startup operation count overflowed", expected: None, actual, caller: thread::current().id() });
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(next_operation, active_tokens, None, *generation) else {
                fatal_lifecycle.get_or_insert_with(|| Violation { call_index: 0, message: "real startup operation token overflowed", expected: None, actual, caller: thread::current().id() });
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            *active_operations = next;
            Dispatch::Real(operation_lease(event, OperationKind::Real { token, actual }))
        }
        Slot::Open { nonce, generation, script, active_dispatchers, next_operation, active_tokens, driver_leases } => {
            if *driver_leases == 0 || !caller_has_driver(script) {
                script.fatal_lifecycle("startup raw operation has no live driver", actual);
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            }
            let Some(next) = active_dispatchers.checked_add(1) else {
                script.fatal_lifecycle("startup raw operation count overflowed", actual);
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(next_operation, active_tokens, Some(*nonce), *generation) else {
                script.fatal_lifecycle("startup raw operation token overflowed", actual);
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            *active_dispatchers = next;
            let lease_actual = actual.clone();
            let frame = if block_before_call(script, actual.clone()) {
                eproto_syscall()
            } else {
                let head = script.entries.front().cloned();
                let mut trial = script.clone();
                let result = match trial.entries.pop_front() {
                    Some(Step::Signal(expected)) => {
                        let key = (args.binding.transaction, args.binding.pidfd);
                        let valid = signal_args_static_eq(expected.args, args)
                            && trial.ids_match_ref(expected.transaction, args.binding.transaction, SymbolKind::Transaction, *nonce, *generation)
                            && trial.ids_match_ref(expected.attempt, args.attempt, SymbolKind::SignalAttempt, *nonce, *generation)
                            && !trial.spent_signals.contains(&key)
                            && (!matches!(trial.fidelity, Fidelity::LinuxFaithful)
                                || Script::faithful_signal(expected.frame, args));
                        if valid {
                            trial.spent_signals.insert(key);
                            Ok(expected.frame)
                        } else {
                            Err("signal arguments, symbols, cardinality, or raw frame differ")
                        }
                    }
                    _ => Err("unexpected signal call"),
                };
                match result {
                    Ok(frame) => {
                        if !trial.advance_consumed(lease_actual.clone()) {
                            *script = trial;
                            active_tokens.remove(&token.operation);
                            *active_dispatchers = active_dispatchers
                                .checked_sub(1)
                                .expect("admitted signal operation count vanished");
                            return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
                        }
                        *script = trial;
                        frame
                    }
                    Err(message) => {
                        script.violation(message, head, actual);
                        eproto_syscall()
                    }
                }
            };
            Dispatch::Scripted(frame, operation_lease(event, OperationKind::Scripted { token, actual: lease_actual }))
        }
        late @ (Slot::Closing { .. } | Slot::Closed { .. }) => {
            late.record_late(ActualCall::Signal(args));
            Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected))
        }
    }
}

pub(super) fn dispatch_poll(event: &Event, args: PollArgs) -> Dispatch<'_, RawPollFrame> {
    let actual = ActualCall::Poll(args);
    if claim_lifecycle_activity(event).is_err() {
        return Dispatch::Scripted(
            eproto_poll(),
            operation_lease(event, OperationKind::Rejected),
        );
    }
    let mut slot = event.startup_syscall_script.lock();
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            let mut next_operation = NonZeroU64::MIN;
            let mut active_tokens = HashSet::new();
            let token = allocate_operation_token(&mut next_operation, &mut active_tokens, None, event.generation)
                .expect("fresh real operation token allocation failed");
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 1,
                next_operation,
                active_tokens,
                fatal_lifecycle: None,
            };
            Dispatch::Real(operation_lease(event, OperationKind::Real { token, actual }))
        }
        Slot::Real { generation, active_operations, next_operation, active_tokens, fatal_lifecycle } => {
            if fatal_lifecycle.is_some() {
                return Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected));
            }
            let Some(next) = active_operations.checked_add(1) else {
                fatal_lifecycle.get_or_insert_with(|| Violation { call_index: 0, message: "real startup operation count overflowed", expected: None, actual, caller: thread::current().id() });
                return Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(next_operation, active_tokens, None, *generation) else {
                fatal_lifecycle.get_or_insert_with(|| Violation { call_index: 0, message: "real startup operation token overflowed", expected: None, actual, caller: thread::current().id() });
                return Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected));
            };
            *active_operations = next;
            Dispatch::Real(operation_lease(event, OperationKind::Real { token, actual }))
        }
        Slot::Open { nonce, generation, script, active_dispatchers, next_operation, active_tokens, driver_leases } => {
            if *driver_leases == 0 || !caller_has_driver(script) {
                script.fatal_lifecycle("startup raw operation has no live driver", actual);
                return Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected));
            }
            let Some(next) = active_dispatchers.checked_add(1) else {
                script.fatal_lifecycle("startup raw operation count overflowed", actual);
                return Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(next_operation, active_tokens, Some(*nonce), *generation) else {
                script.fatal_lifecycle("startup raw operation token overflowed", actual);
                return Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected));
            };
            *active_dispatchers = next;
            let lease_actual = actual.clone();
            let frame = if block_before_call(script, actual.clone()) {
                eproto_poll()
            } else {
                let head = script.entries.front().cloned();
                let mut trial = script.clone();
                let result = match trial.entries.pop_front() {
                    Some(Step::Poll(expected)) => {
                        let valid = poll_args_static_eq(expected.args, args)
                            && trial.ids_match_ref(expected.transaction, args.binding.transaction, SymbolKind::Transaction, *nonce, *generation)
                            && trial.ids_match_ref(expected.source_status, args.binding.source_status, SymbolKind::Status, *nonce, *generation)
                            && (!matches!(trial.fidelity, Fidelity::LinuxFaithful)
                                || Script::faithful_poll(expected.frame, args));
                        valid.then_some(expected.frame).ok_or("poll arguments, symbols, or raw frame differ")
                    }
                    _ => Err("unexpected poll call"),
                };
                match result {
                    Ok(frame) => {
                        if !trial.advance_consumed(lease_actual.clone()) {
                            *script = trial;
                            active_tokens.remove(&token.operation);
                            *active_dispatchers = active_dispatchers
                                .checked_sub(1)
                                .expect("admitted poll operation count vanished");
                            return Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected));
                        }
                        *script = trial;
                        frame
                    }
                    Err(message) => {
                        script.violation(message, head, actual);
                        eproto_poll()
                    }
                }
            };
            Dispatch::Scripted(frame, operation_lease(event, OperationKind::Scripted { token, actual: lease_actual }))
        }
        late @ (Slot::Closing { .. } | Slot::Closed { .. }) => {
            late.record_late(ActualCall::Poll(args));
            Dispatch::Scripted(eproto_poll(), operation_lease(event, OperationKind::Rejected))
        }
    }
}

pub(super) fn dispatch_continue(event: &Event, args: ContinueArgs) -> Dispatch<'_, RawSyscallFrame> {
    let actual = ActualCall::Continue(args);
    if claim_lifecycle_activity(event).is_err() {
        return Dispatch::Scripted(
            eproto_syscall(),
            operation_lease(event, OperationKind::Rejected),
        );
    }
    let mut slot = event.startup_syscall_script.lock();
    match &mut *slot {
        vacant @ Slot::Vacant { .. } => {
            let mut next_operation = NonZeroU64::MIN;
            let mut active_tokens = HashSet::new();
            let token = allocate_operation_token(&mut next_operation, &mut active_tokens, None, event.generation)
                .expect("fresh real operation token allocation failed");
            *vacant = Slot::Real {
                generation: event.generation,
                active_operations: 1,
                next_operation,
                active_tokens,
                fatal_lifecycle: None,
            };
            Dispatch::Real(operation_lease(event, OperationKind::Real { token, actual }))
        }
        Slot::Real { generation, active_operations, next_operation, active_tokens, fatal_lifecycle } => {
            if fatal_lifecycle.is_some() {
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            }
            let Some(next) = active_operations.checked_add(1) else {
                fatal_lifecycle.get_or_insert_with(|| Violation { call_index: 0, message: "real startup operation count overflowed", expected: None, actual, caller: thread::current().id() });
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(next_operation, active_tokens, None, *generation) else {
                fatal_lifecycle.get_or_insert_with(|| Violation { call_index: 0, message: "real startup operation token overflowed", expected: None, actual, caller: thread::current().id() });
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            *active_operations = next;
            Dispatch::Real(operation_lease(event, OperationKind::Real { token, actual }))
        }
        Slot::Open { nonce, generation, script, active_dispatchers, next_operation, active_tokens, driver_leases } => {
            if *driver_leases == 0 || !caller_has_driver(script) {
                script.fatal_lifecycle("startup raw operation has no live driver", actual);
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            }
            let Some(next) = active_dispatchers.checked_add(1) else {
                script.fatal_lifecycle("startup raw operation count overflowed", actual);
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            let Some(token) = allocate_operation_token(next_operation, active_tokens, Some(*nonce), *generation) else {
                script.fatal_lifecycle("startup raw operation token overflowed", actual);
                return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
            };
            *active_dispatchers = next;
            let lease_actual = actual.clone();
            let frame = if block_before_call(script, actual.clone()) {
                eproto_syscall()
            } else {
                let head = script.entries.front().cloned();
                let mut trial = script.clone();
                let result = match trial.entries.pop_front() {
                    Some(Step::Continue(expected)) => {
                        let key = (args.binding.transaction, args.binding.source_status);
                        let valid = continue_args_static_eq(expected.args, args)
                            && trial.ids_match_ref(expected.transaction, args.binding.transaction, SymbolKind::Transaction, *nonce, *generation)
                            && trial.ids_match_ref(expected.source_status, args.binding.source_status, SymbolKind::Status, *nonce, *generation)
                            && trial.ids_match_ref(expected.attempt, args.attempt, SymbolKind::ResumeAttempt, *nonce, *generation)
                            && !trial.spent_continues.contains(&key)
                            && (!matches!(trial.fidelity, Fidelity::LinuxFaithful)
                                || Script::faithful_continue(expected.frame, args));
                        if valid {
                            trial.spent_continues.insert(key);
                            Ok(expected.frame)
                        } else {
                            Err("CONT arguments, symbols, cardinality, or raw frame differ")
                        }
                    }
                    _ => Err("unexpected CONT call"),
                };
                match result {
                    Ok(frame) => {
                        if !trial.advance_consumed(lease_actual.clone()) {
                            *script = trial;
                            active_tokens.remove(&token.operation);
                            *active_dispatchers = active_dispatchers
                                .checked_sub(1)
                                .expect("admitted CONT operation count vanished");
                            return Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected));
                        }
                        *script = trial;
                        frame
                    }
                    Err(message) => {
                        script.violation(message, head, actual);
                        eproto_syscall()
                    }
                }
            };
            Dispatch::Scripted(frame, operation_lease(event, OperationKind::Scripted { token, actual: lease_actual }))
        }
        late @ (Slot::Closing { .. } | Slot::Closed { .. }) => {
            late.record_late(ActualCall::Continue(args));
            Dispatch::Scripted(eproto_syscall(), operation_lease(event, OperationKind::Rejected))
        }
    }
}

#[must_use = "startup syscall scripts must be explicitly finished"]
pub(super) struct Guard {
    generation: Arc<EventGeneration>,
    nonce: NonZeroU64,
    root_tid: Pid,
    owns_driver_lease: bool,
    finished: bool,
}

#[derive(Clone)]
pub(super) struct CloseRequest {
    generation: Arc<EventGeneration>,
    nonce: NonZeroU64,
}

impl CloseRequest {
    pub(super) fn request(&self) -> Result<(), Errno> {
        let event = &self.generation.event;
        let mut slot = event.startup_syscall_script.lock();
        let old = std::mem::replace(&mut *slot, Slot::vacant());
        *slot = match old {
            Slot::Open {
                nonce,
                generation,
                script,
                active_dispatchers,
                next_operation,
                active_tokens,
                driver_leases,
            } if nonce == self.nonce && generation == event.generation => Slot::Closing {
                nonce,
                generation,
                script,
                active_dispatchers,
                next_operation,
                active_tokens,
                driver_leases,
                late_violation: None,
            },
            other => {
                *slot = other;
                return Err(Errno::EBUSY);
            }
        };
        event.startup_syscall_script_changed.notify_all();
        Ok(())
    }
}

type PanicPayload = Box<dyn std::any::Any + Send + 'static>;

struct DriverTask {
    handle: thread::JoinHandle<(DriverLease, Result<(), PanicPayload>)>,
}

#[must_use = "startup syscall script scopes must be run to completion"]
pub(super) struct Scope {
    guard: Option<Guard>,
    drivers: Vec<DriverTask>,
    finished: bool,
}

#[must_use = "startup syscall script drivers must be revoked after joining"]
pub(super) struct DriverLease {
    generation: Arc<EventGeneration>,
    nonce: NonZeroU64,
    owner_tid: Option<Pid>,
    revoked: bool,
}

impl DriverLease {
    fn activate_current_thread(&mut self) -> Result<(), Errno> {
        if self.revoked || self.owner_tid.is_some() {
            return Err(Errno::EALREADY);
        }
        let tid = caller_tid();
        let event = &self.generation.event;
        let mut slot = event.startup_syscall_script.lock();
        match &mut *slot {
            Slot::Open {
                nonce,
                generation,
                script,
                ..
            } if *nonce == self.nonce && *generation == event.generation => {
                if !script.driver_tids.insert(tid.as_raw()) {
                    script.fatal_lifecycle(
                        "startup script driver TID was registered twice",
                        lifecycle_actual(event),
                    );
                    return Err(Errno::EALREADY);
                }
                self.owner_tid = Some(tid);
                Ok(())
            }
            _ => Err(Errno::EBUSY),
        }
    }

    fn deactivate_current_thread(&mut self, abandoned: bool) -> Result<(), Errno> {
        let Some(owner_tid) = self.owner_tid else {
            return Err(Errno::EINVAL);
        };
        if caller_tid() != owner_tid {
            return Err(Errno::EPERM);
        }
        let event = &self.generation.event;
        let mut slot = event.startup_syscall_script.lock();
        match &mut *slot {
            Slot::Open { nonce, generation, script, .. }
            | Slot::Closing { nonce, generation, script, .. }
                if *nonce == self.nonce && *generation == event.generation => {
                    if !script.driver_tids.remove(&owner_tid.as_raw()) {
                        script.fatal_lifecycle(
                            "startup script driver TID disappeared before thread exit",
                            lifecycle_actual(event),
                        );
                        return Err(Errno::EPROTO);
                    }
                    if abandoned {
                        script.fatal_lifecycle(
                            "startup script driver unwound",
                            lifecycle_actual(event),
                        );
                    }
                    self.owner_tid = None;
                    Ok(())
                }
            _ => Err(Errno::EPROTO),
        }
    }

    fn revoke_after_join(mut self) -> Result<(), Errno> {
        if self.owner_tid.is_some() {
            return Err(Errno::EBUSY);
        }
        self.revoke_inner()
    }

    fn revoke_inner(&mut self) -> Result<(), Errno> {
        if self.revoked {
            return Ok(());
        }
        let event = &self.generation.event;
        let mut slot = event.startup_syscall_script.lock();
        match &mut *slot {
            Slot::Open {
                nonce,
                generation,
                script,
                driver_leases,
                ..
            }
            | Slot::Closing {
                nonce,
                generation,
                script,
                driver_leases,
                ..
            } if *nonce == self.nonce && *generation == event.generation => {
                let Some(remaining) = driver_leases.checked_sub(1) else {
                    script.fatal_lifecycle(
                        "startup script driver lease count underflowed",
                        lifecycle_actual(event),
                    );
                    self.revoked = true;
                    return Err(Errno::EPROTO);
                };
                *driver_leases = remaining;
                self.revoked = true;
                event.startup_syscall_script_changed.notify_all();
                Ok(())
            }
            Slot::Closed { .. } => {
                self.revoked = true;
                Err(Errno::EPROTO)
            }
            _ => Err(Errno::EPROTO),
        }
    }
}

impl Drop for DriverLease {
    fn drop(&mut self) {
        if self.owner_tid == Some(caller_tid()) {
            let _ = self.deactivate_current_thread(true);
        }
        if !self.revoked {
            let event = &self.generation.event;
            let mut slot = event.startup_syscall_script.lock();
            if let Slot::Open { script, .. } | Slot::Closing { script, .. } = &mut *slot {
                script.fatal_lifecycle(
                    "startup script driver lease disappeared before join receipt",
                    lifecycle_actual(event),
                );
            }
        }
    }
}

fn lifecycle_actual(event: &Event) -> ActualCall {
    ActualCall::Poll(PollArgs {
        purpose: PollPurpose::UnobservedBoundary,
        binding: CallBinding::new(
            event,
            Pid::from_raw(0),
            -1,
            PhysicalTaskIdentity::direct_child(Pid::from_raw(0)),
            None,
            None,
        ),
        events: 0,
        timeout_ms: 0,
    })
}

impl Guard {
    pub(super) fn close_request(&self) -> CloseRequest {
        CloseRequest {
            generation: Arc::clone(&self.generation),
            nonce: self.nonce,
        }
    }

    fn release_root_driver(
        &mut self,
        event: &Event,
        script: &mut Script,
        driver_leases: &mut usize,
    ) {
        if !self.owns_driver_lease {
            return;
        }
        if caller_tid() != self.root_tid {
            script.fatal_lifecycle(
                "startup script root driver closed on the wrong thread",
                lifecycle_actual(event),
            );
        }
        if !script.driver_tids.remove(&self.root_tid.as_raw()) {
            script.fatal_lifecycle(
                "startup script root driver TID disappeared before close",
                lifecycle_actual(event),
            );
        }
        match driver_leases.checked_sub(1) {
            Some(remaining) => *driver_leases = remaining,
            None => script.fatal_lifecycle(
                "startup script root driver lease count underflowed",
                lifecycle_actual(event),
            ),
        }
        self.owns_driver_lease = false;
    }

    fn register_driver(&self) -> Result<DriverLease, Errno> {
        let event = &self.generation.event;
        let mut slot = event.startup_syscall_script.lock();
        match &mut *slot {
            Slot::Open {
                nonce,
                generation,
                script,
                driver_leases,
                ..
            } if *nonce == self.nonce && *generation == event.generation => {
                let Some(next) = driver_leases.checked_add(1) else {
                    script.fatal_lifecycle(
                        "startup script driver lease count overflowed",
                        lifecycle_actual(event),
                    );
                    return Err(Errno::EOVERFLOW);
                };
                *driver_leases = next;
                Ok(DriverLease {
                    generation: Arc::clone(&self.generation),
                    nonce: self.nonce,
                    owner_tid: None,
                    revoked: false,
                })
            }
            _ => Err(Errno::EBUSY),
        }
    }

    fn abandon(mut self) {
        let event = &self.generation.event;
        let mut slot = event.startup_syscall_script.lock();
        let old = std::mem::replace(&mut *slot, Slot::vacant());
        *slot = match old {
            Slot::Open {
                nonce,
                generation,
                script,
                ..
            } if nonce == self.nonce => {
                let outcome = script
                    .fatal_lifecycle
                    .map(CloseOutcome::FatalLifecycle)
                    .unwrap_or(CloseOutcome::Abandoned);
                Slot::Closed {
                    nonce,
                    generation,
                    outcome,
                    late_violation: None,
                }
            }
            Slot::Closing {
                nonce,
                generation,
                script,
                late_violation,
                ..
            } if nonce == self.nonce => {
                let fatal = script.fatal_lifecycle.or_else(|| late_violation.clone());
                Slot::Closed {
                    nonce,
                    generation,
                    outcome: fatal
                        .map(CloseOutcome::FatalLifecycle)
                        .unwrap_or(CloseOutcome::Abandoned),
                    late_violation,
                }
            }
            other => other,
        };
        self.finished = true;
    }

    pub(super) fn finish_ok(mut self) -> Result<ScriptCloseReceipt, Violation> {
        self.finish(None)
    }

    pub(super) fn finish_err(
        mut self,
        expected: ExpectedViolation,
    ) -> Result<ScriptCloseReceipt, Violation> {
        self.finish(Some(expected))
    }

    fn finish(
        &mut self,
        expected_error: Option<ExpectedViolation>,
    ) -> Result<ScriptCloseReceipt, Violation> {
        let event = &self.generation.event;
        let immediate_failure = {
            let mut slot = event.startup_syscall_script.lock();
            let old = std::mem::replace(&mut *slot, Slot::vacant());
            *slot = match old {
                Slot::Open {
                    nonce,
                    generation,
                    mut script,
                    active_dispatchers,
                    next_operation,
                    active_tokens,
                    mut driver_leases,
                } if nonce == self.nonce && generation == event.generation => {
                    self.release_root_driver(event, &mut script, &mut driver_leases);
                    if driver_leases != 0 {
                        script.fatal_lifecycle(
                            "startup script closed before all drivers were joined and revoked",
                            lifecycle_actual(event),
                        );
                    }
                    Slot::Closing {
                        nonce,
                        generation,
                        script,
                        active_dispatchers,
                        next_operation,
                        active_tokens,
                        driver_leases,
                        late_violation: None,
                    }
                }
                Slot::Closing {
                    nonce,
                    generation,
                    mut script,
                    active_dispatchers,
                    next_operation,
                    active_tokens,
                    mut driver_leases,
                    late_violation,
                } if nonce == self.nonce && generation == event.generation => {
                    self.release_root_driver(event, &mut script, &mut driver_leases);
                    if driver_leases != 0 {
                        script.fatal_lifecycle(
                            "startup script closed before all drivers were joined and revoked",
                            lifecycle_actual(event),
                        );
                    }
                    Slot::Closing {
                        nonce,
                        generation,
                        script,
                        active_dispatchers,
                        next_operation,
                        active_tokens,
                        driver_leases,
                        late_violation,
                    }
                }
                other => other,
            };
            match &mut *slot {
                Slot::Closing {
                    nonce,
                    script,
                    active_dispatchers,
                    active_tokens,
                    ..
                } if *nonce == self.nonce => {
                    if !script.deferred_audits.is_empty() {
                        // A raw wait whose causal audit was omitted can never
                        // be made complete by waiting: the sole driver has
                        // already joined or been revoked before final finish.
                        script.fatal_lifecycle(
                            "MissingCausalCompletion",
                            lifecycle_actual(event),
                        );
                    } else if *active_dispatchers != 0
                        || !active_tokens.is_empty()
                        || *active_dispatchers != active_tokens.len()
                    {
                        // Final finish is not a synchronization primitive. A
                        // legitimate interleave requests Closing first and
                        // calls finish only after the admitted operation
                        // retires.
                        script.fatal_lifecycle(
                            "UnfinishedOperation",
                            lifecycle_actual(event),
                        );
                    }
                }
                _ => {}
            }
            let immediate = match &*slot {
                Slot::Closing { script, .. } => script
                    .fatal_lifecycle
                    .as_ref()
                    .filter(|violation| {
                        matches!(
                            violation.message,
                            "MissingCausalCompletion" | "UnfinishedOperation"
                        )
                    })
                    .cloned(),
                _ => None,
            };
            if let Some(violation) = immediate.as_ref() {
                let old = std::mem::replace(&mut *slot, Slot::vacant());
                *slot = match old {
                    Slot::Closing {
                        nonce,
                        generation,
                        late_violation,
                        ..
                    } if nonce == self.nonce => Slot::Closed {
                        nonce,
                        generation,
                        outcome: CloseOutcome::FatalLifecycle(violation.clone()),
                        late_violation,
                    },
                    other => other,
                };
            }
            immediate
        };
        if let Some(violation) = immediate_failure {
            self.finished = true;
            return Err(violation);
        }

        // Never hold the script mutex while taking production locks.
        let startup_clear = event.startup_barrier_cleanup.lock().is_none();
        let unstarted_identity_clear = event.unstarted_cleanup_identity.lock().is_none();
        let worker = event.worker_state.load(Ordering::Acquire);
        let wait_owner = event.wait_owner.load(Ordering::Acquire);
        let authority = event.original_root_cleanup_authority.load(Ordering::Acquire);
        let protocol_error_clear = event.startup_cleanup_protocol_error.lock().is_none();
        let completion_stage = event.startup_script_completion_stage.load(Ordering::Acquire);
        let removed_pid = event.startup_script_removed_pid.load(Ordering::Acquire);
        let exact_registry_absent = if removed_pid > 0 {
            let pids = NOTIFIER.pids.lock();
            !pids.get(&Pid::from_raw(removed_pid)).is_some_and(|current| {
                std::ptr::eq(current.handle.event().as_ref(), event)
            })
        } else {
            false
        };

        let mut slot = event.startup_syscall_script.lock();
        let old = std::mem::replace(&mut *slot, Slot::vacant());
        let (mut script, late, active, active_tokens, leases, generation) = match old {
            Slot::Closing {
                nonce,
                generation,
                script,
                active_dispatchers,
                active_tokens,
                driver_leases,
                late_violation,
                ..
            } if nonce == self.nonce => (
                script,
                late_violation,
                active_dispatchers,
                active_tokens,
                driver_leases,
                generation,
            ),
            other => {
                *slot = other;
                self.finished = true;
                return Err(late_violation(lifecycle_actual(event)));
            }
        };
        let completion_ok = startup_clear
            && wait_owner == WAIT_OWNER_NONE
            && protocol_error_clear
            && match script.completion {
                CompletionExpectation::HarnessOnly => true,
                CompletionExpectation::ProtocolWorkerDone(registry) => {
                    let exact_done_stage = match registry {
                        RegistryCompletionExpectation::Removed => {
                            CompletionStage::DoneAfterRemoval
                        }
                        RegistryCompletionExpectation::ConfirmedAbsent => {
                            CompletionStage::DoneAfterConfirmedAbsent
                        }
                    };
                    matches!(script.fidelity, Fidelity::LinuxFaithful)
                        && worker == WORKER_DONE
                        && authority == ROOT_CLEANUP_AUTHORITY_FINISHED
                        && completion_stage == exact_done_stage as u8
                        && event
                            .startup_script_registry_receipt
                            .load(Ordering::Acquire)
                            == registry.receipt()
                        && exact_registry_absent
                        && unstarted_identity_clear
                }
            }
            && script.driver_tids.is_empty();
        let fatal = script.fatal_lifecycle.take().or(late);
        let first = script.first_violation.take();
        let expecting_error = expected_error.is_some();
        let expected_matches = match (expected_error, first.as_ref()) {
            (None, None) => true,
            (Some(expected), Some(violation)) => violation.matches(&expected),
            _ => false,
        };
        let complete = active == 0
            && active_tokens.is_empty()
            && script.deferred_audits.is_empty()
            && leases == 0
            && fatal.is_none()
            && (expecting_error
                || (script.entries.is_empty() && script.pending_status.is_none()))
            && completion_ok
            && expected_matches;
        let outcome = if complete {
            if let Some(violation) = first.clone() {
                CloseOutcome::ExpectedViolation(violation)
            } else {
                match script.completion {
                    CompletionExpectation::HarnessOnly => CloseOutcome::HarnessOnlyPassed,
                    CompletionExpectation::ProtocolWorkerDone(_) => CloseOutcome::ProtocolPassed,
                }
            }
        } else {
            let violation = fatal.or(first).unwrap_or_else(|| Violation {
                call_index: script.consumed,
                message: "script did not quiesce completely",
                expected: script.entries.front().cloned(),
                actual: lifecycle_actual(event),
                caller: thread::current().id(),
            });
            *slot = Slot::Closed { nonce: self.nonce, generation, outcome: CloseOutcome::FatalLifecycle(violation.clone()), late_violation: None };
            self.finished = true;
            return Err(violation);
        };
        let receipt_outcome = match &outcome {
            CloseOutcome::ProtocolPassed => ReceiptOutcome::ProtocolPassed,
            CloseOutcome::HarnessOnlyPassed => ReceiptOutcome::HarnessOnlyPassed,
            CloseOutcome::ExpectedViolation(_) => ReceiptOutcome::ExpectedViolation,
            CloseOutcome::FatalLifecycle(_) | CloseOutcome::Abandoned => {
                unreachable!("failed close outcome escaped the error path")
            }
        };
        *slot = Slot::Closed {
            nonce: self.nonce,
            generation,
            outcome,
            late_violation: None,
        };
        self.finished = true;
        Ok(ScriptCloseReceipt {
            nonce: self.nonce,
            generation,
            outcome: receipt_outcome,
        })
    }
}

impl Scope {
    pub(super) fn run<F>(guard: Guard, body: F) -> Result<ScriptCloseReceipt, Violation>
    where
        F: FnOnce(&mut Scope),
    {
        Self::run_with_finish(guard, None, body)
    }

    pub(super) fn run_expect_err<F>(
        guard: Guard,
        expected: ExpectedViolation,
        body: F,
    ) -> Result<ScriptCloseReceipt, Violation>
    where
        F: FnOnce(&mut Scope),
    {
        Self::run_with_finish(guard, Some(expected), body)
    }

    fn run_with_finish<F>(
        guard: Guard,
        expected: Option<ExpectedViolation>,
        body: F,
    ) -> Result<ScriptCloseReceipt, Violation>
    where
        F: FnOnce(&mut Scope),
    {
        let mut scope = Self {
            guard: Some(guard),
            drivers: Vec::new(),
            finished: false,
        };
        let root = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut scope)));
        let child_panic = scope.join_drivers();
        match (root, child_panic) {
            (Err(payload), _) | (Ok(_), Some(payload)) => {
                scope
                    .guard
                    .take()
                    .expect("startup script scope lost its guard")
                    .abandon();
                scope.finished = true;
                std::panic::resume_unwind(payload);
            }
            (Ok(()), None) => {
                let guard = scope
                    .guard
                    .take()
                    .expect("startup script scope lost its guard");
                scope.finished = true;
                match expected {
                    Some(expected) => guard.finish_err(expected),
                    None => guard.finish_ok(),
                }
            }
        }
    }

    pub(super) fn spawn<F>(&mut self, body: F) -> Result<(), Errno>
    where
        F: FnOnce() + Send + 'static,
    {
        let mut lease = self
            .guard
            .as_ref()
            .ok_or(Errno::EALREADY)?
            .register_driver()?;
        let handle = thread::spawn(move || {
            if let Err(error) = lease.activate_current_thread() {
                return (lease, Err(Box::new(error) as PanicPayload));
            }
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
            let abandoned = outcome.is_err();
            if let Err(error) = lease.deactivate_current_thread(abandoned) {
                return (lease, Err(Box::new(error) as PanicPayload));
            }
            (lease, outcome)
        });
        self.drivers.push(DriverTask { handle });
        Ok(())
    }

    fn join_drivers(&mut self) -> Option<PanicPayload> {
        let mut first_panic = None;
        for task in self.drivers.drain(..) {
            match task.handle.join() {
                Ok((lease, outcome)) => {
                    if lease.revoke_after_join().is_err() && first_panic.is_none() {
                        first_panic = Some(Box::new("startup driver join/revoke failed") as PanicPayload);
                    }
                    if let Err(payload) = outcome {
                        if first_panic.is_none() {
                            first_panic = Some(payload);
                        }
                    }
                }
                Err(payload) if first_panic.is_none() => first_panic = Some(payload),
                Err(_) => {}
            }
        }
        first_panic
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = self.join_drivers();
        // Dropping the still-armed guard supplies the single fail-fast panic;
        // during an existing unwind Guard::drop observes thread::panicking()
        // and never replaces the original payload.
        let _ = self.guard.take();
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let event = &self.generation.event;
        let mut slot = event.startup_syscall_script.lock();
        let old = std::mem::replace(&mut *slot, Slot::vacant());
        *slot = match old {
            Slot::Open {
                nonce,
                generation,
                script,
                ..
            } if nonce == self.nonce => {
                let outcome = script
                    .fatal_lifecycle
                    .map(CloseOutcome::FatalLifecycle)
                    .unwrap_or(CloseOutcome::Abandoned);
                Slot::Closed {
                    nonce,
                    generation,
                    outcome,
                    late_violation: None,
                }
            }
            Slot::Closing {
                nonce,
                generation,
                script,
                late_violation,
                ..
            } if nonce == self.nonce => {
                let fatal = script.fatal_lifecycle.or_else(|| late_violation.clone());
                Slot::Closed {
                    nonce,
                    generation,
                    outcome: fatal
                        .map(CloseOutcome::FatalLifecycle)
                        .unwrap_or(CloseOutcome::Abandoned),
                    late_violation,
                }
            }
            other => other,
        };
        if !thread::panicking() {
            panic!("startup syscall script guard dropped without finish");
        }
    }
}

fn script_install_is_pristine(generation: &Arc<EventGeneration>) -> bool {
    let event = &generation.event;
    let exact_event_absent = {
        let pids = NOTIFIER.pids.lock();
        !pids.values().any(|entry| {
            Arc::ptr_eq(entry.handle.event(), event)
        })
    };
    exact_event_absent
        && event.worker_state.load(Ordering::Acquire) == WORKER_NOT_STARTED
        && event.wait_owner.load(Ordering::Acquire) == WAIT_OWNER_NONE
        && event.startup_barrier_cleanup.lock().is_none()
        && event.unstarted_cleanup_identity.lock().is_none()
        && event.unstarted_cleanup_cause.get().is_none()
        && matches!(
            &*event.unobserved_startup_cleanup.lock(),
            UnobservedStartupCleanupState::Idle
        )
        && event.original_root_cleanup_authority.load(Ordering::Acquire)
            == ROOT_CLEANUP_AUTHORITY_UNCLAIMED
        && event.startup_cleanup_protocol_error.lock().is_none()
        && event.startup_script_completion_stage.load(Ordering::Acquire) == 0
        && event.startup_script_removed_pid.load(Ordering::Acquire) == 0
        && event.startup_script_registry_receipt.load(Ordering::Acquire) == 0
}

pub(super) fn install(
    generation: Arc<EventGeneration>,
    fidelity: Fidelity,
    completion: CompletionExpectation,
    entries: Vec<Step>,
) -> Result<Guard, Errno> {
    let event = &generation.event;
    event
        .startup_script_mode
        .compare_exchange(
            MODE_NONE,
            MODE_INSTALLING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| Errno::EBUSY)?;
    if let Some((entered, resume)) = event.startup_script_install_pause.lock().take() {
        entered.wait();
        resume.wait();
    }
    if matches!(completion, CompletionExpectation::ProtocolWorkerDone(_)) {
        if !matches!(fidelity, Fidelity::LinuxFaithful) {
            let _ = event.startup_script_mode.compare_exchange(
                MODE_INSTALLING,
                MODE_NONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(Errno::EINVAL);
        }
    }
    // Harness-only scripts share the same fresh-Event installation contract.
    // They may relax completion evidence, but may never replace real lifecycle
    // activity after it has started.
    if !script_install_is_pristine(&generation) {
        let _ = event.startup_script_mode.compare_exchange(
            MODE_INSTALLING,
            MODE_NONE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        return Err(Errno::EBUSY);
    }
    if !event.legacy_wait_siginfo_code_overrides.lock().is_empty() {
        let _ = event.startup_script_mode.compare_exchange(
            MODE_INSTALLING,
            MODE_NONE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        return Err(Errno::EBUSY);
    }
    let mut slot = event.startup_syscall_script.lock();
    let next_nonce = match &*slot {
        Slot::Vacant { next_nonce } => *next_nonce,
        _ => {
            let _ = event.startup_script_mode.compare_exchange(
                MODE_INSTALLING,
                MODE_NONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(Errno::EBUSY);
        }
    };
    let nonce = next_nonce;
    let mut script = Script::new(fidelity, completion, entries);
    script.driver_tids.insert(caller_tid().as_raw());
    *slot = Slot::Open {
        nonce,
        generation: event.generation,
        script,
        active_dispatchers: 0,
        next_operation: NonZeroU64::MIN,
        active_tokens: HashSet::new(),
        driver_leases: 1,
    };
    if event
        .startup_script_mode
        .compare_exchange(
            MODE_INSTALLING,
            MODE_SCRIPT,
            Ordering::Release,
            Ordering::Acquire,
        )
        .is_err()
    {
        // A first lifecycle contender changed INSTALLING -> REAL and failed
        // before mutation.  Restore only the just-created slot; the atomic
        // REAL tombstone is deliberately permanent.
        *slot = Slot::Vacant { next_nonce };
        return Err(Errno::EBUSY);
    }
    Ok(Guard {
        generation,
        nonce,
        root_tid: caller_tid(),
        owns_driver_lease: true,
        finished: false,
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn binding(event: &Event, pidfd: i32) -> CallBinding {
        let pid = Pid::from_raw(71);
        CallBinding {
            generation: event.generation,
            pid,
            pidfd,
            task: PhysicalTaskIdentity::direct_child_with_pidfd(pid, pidfd),
            transaction: None,
            source_status: None,
            caller_tid: caller_tid(),
        }
    }

    fn signal_step(event: &Event, pidfd: i32) -> (SignalArgs, Step) {
        let args = SignalArgs {
            site: SignalSite::UnobservedCleanup,
            binding: binding(event, pidfd),
            attempt: None,
            signal: libc::SIGKILL,
            siginfo_is_null: true,
            flags: 0,
        };
        let step = Step::Signal(ExpectedSignal {
            args,
            transaction: None,
            attempt: None,
            frame: RawSyscallFrame { rc: 0, errno: 0 },
        });
        (args, step)
    }

    #[test]
    fn close_receipts_require_explicit_protocol_refinement() {
        let harness = EventHandle::new();
        let harness_generation = harness.event().generation;
        let harness_receipt = install(
            Arc::clone(&harness.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap()
        .finish_ok()
        .unwrap();
        assert_eq!(harness_receipt.generation(), harness_generation);
        assert_ne!(harness_receipt.nonce(), 0);
        let harness_receipt = harness_receipt
            .expect_protocol_passed()
            .expect_err("HarnessOnly receipt was accepted as ProtocolPassed");
        harness_receipt.expect_harness_only();

        let protocol = EventHandle::new();
        let event = protocol.event();
        let generation = event.generation;
        let pid = Pid::from_raw(900_101);
        let guard = install(
            Arc::clone(&protocol.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::ProtocolWorkerDone(
                RegistryCompletionExpectation::ConfirmedAbsent,
            ),
            Vec::new(),
        )
        .unwrap();
        assert!(event.try_begin_unstarted_completion());
        event.finish_original_root_cleanup_authority();
        retain_registry_receipt(
            event,
            pid,
            RegistryCompletionExpectation::ConfirmedAbsent,
        )
        .unwrap();
        event.prepare_unstarted_worker_done_after_external_finish();
        publish_registry_receipt(
            event,
            pid,
            RegistryCompletionExpectation::ConfirmedAbsent,
        )
        .unwrap();
        event.publish_worker_done();
        let protocol_receipt = guard
            .finish_ok()
            .unwrap()
            .expect_protocol_passed()
            .expect("full protocol receipt did not refine");
        assert_eq!(protocol_receipt.generation(), generation);
        assert_ne!(protocol_receipt.nonce(), 0);
    }

    #[test]
    fn install_race_with_first_worker_activity_is_atomic_and_fail_closed() {
        let handle = EventHandle::new();
        let event = Arc::clone(handle.event());
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        arm_install_pause(&event, Arc::clone(&entered), Arc::clone(&resume)).unwrap();
        let generation = Arc::clone(&handle.0);
        let installer = thread::spawn(move || {
            match install(
                generation,
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ) {
                Ok(guard) => {
                    guard.abandon();
                    Ok(())
                }
                Err(error) => Err(error),
            }
        });
        entered.wait();
        assert!(
            !event.try_begin_worker_start(),
            "worker state mutated while script install owned INSTALLING"
        );
        assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_NOT_STARTED);
        resume.wait();
        assert_eq!(installer.join().unwrap(), Err(Errno::EBUSY));
        assert_eq!(event.startup_script_mode.load(Ordering::Acquire), MODE_REAL);
        assert!(matches!(
            &*event.startup_syscall_script.lock(),
            Slot::Vacant { .. }
        ));
        assert!(matches!(
            install(
                Arc::clone(&handle.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));
    }

    #[test]
    fn install_race_with_unregistered_cleanup_is_zero_mutation_and_fail_closed() {
        let handle = EventHandle::new();
        let event = Arc::clone(handle.event());
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        arm_install_pause(&event, Arc::clone(&entered), Arc::clone(&resume)).unwrap();
        let generation = Arc::clone(&handle.0);
        let install_generation = Arc::clone(&generation);
        let installer = thread::spawn(move || {
            match install(
                install_generation,
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ) {
                Ok(guard) => {
                    guard.abandon();
                    Ok(())
                }
                Err(error) => Err(error),
            }
        });
        entered.wait();
        assert_eq!(
            EventHandle::terminate_unregistered_original_root_generation(
                &generation,
                Pid::from_raw(900_102),
                Errno::EIO,
            ),
            Err(Errno::EBUSY)
        );
        assert!(event.unstarted_cleanup_identity.lock().is_none());
        assert!(event.startup_barrier_cleanup.lock().is_none());
        assert!(matches!(
            &*event.unobserved_startup_cleanup.lock(),
            UnobservedStartupCleanupState::Idle
        ));
        assert!(event.controller_launch.lock().is_none());
        assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_NOT_STARTED);
        assert_eq!(event.wait_owner.load(Ordering::Acquire), WAIT_OWNER_NONE);
        resume.wait();
        assert_eq!(installer.join().unwrap(), Err(Errno::EBUSY));
        assert_eq!(event.startup_script_mode.load(Ordering::Acquire), MODE_REAL);
        assert!(matches!(
            &*event.startup_syscall_script.lock(),
            Slot::Vacant { .. }
        ));
    }

    #[test]
    fn install_race_with_controller_identity_take_is_zero_mutation_and_fail_closed() {
        let handle = EventHandle::new();
        let event = Arc::clone(handle.event());
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        arm_install_pause(&event, Arc::clone(&entered), Arc::clone(&resume)).unwrap();
        let generation = Arc::clone(&handle.0);
        let installer = thread::spawn(move || {
            match install(
                generation,
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ) {
                Ok(guard) => {
                    guard.abandon();
                    Ok(())
                }
                Err(error) => Err(error),
            }
        });
        entered.wait();
        assert!(matches!(
            event.take_controller_launch_identity(Pid::from_raw(900_103), None),
            Err(Errno::EBUSY)
        ));
        assert!(event.controller_launch.lock().is_none());
        assert!(event.unstarted_cleanup_identity.lock().is_none());
        assert!(event.startup_barrier_cleanup.lock().is_none());
        assert_eq!(event.worker_state.load(Ordering::Acquire), WORKER_NOT_STARTED);
        assert_eq!(event.wait_owner.load(Ordering::Acquire), WAIT_OWNER_NONE);
        resume.wait();
        assert_eq!(installer.join().unwrap(), Err(Errno::EBUSY));
        assert_eq!(event.startup_script_mode.load(Ordering::Acquire), MODE_REAL);
        assert!(matches!(
            &*event.startup_syscall_script.lock(),
            Slot::Vacant { .. }
        ));
    }

    #[test]
    fn harness_install_rejects_late_worker_activity() {
        let handle = EventHandle::new();
        assert!(handle.event().try_begin_worker_start());
        assert!(matches!(
            install(
                Arc::clone(&handle.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));
        handle.event().rollback_worker_start();
    }

    #[test]
    fn first_bind_audit_or_status_permanently_selects_real_mode() {
        let bind_handle = EventHandle::new();
        bind_generated_for_test(
            bind_handle.event(),
            SymbolKind::Transaction,
            1,
            ActualBindSite::Transaction(TransactionSite::SetupPrepared),
        )
        .unwrap();
        assert!(matches!(
            install(
                Arc::clone(&bind_handle.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let audit_handle = EventHandle::new();
        audit_transaction(
            audit_handle.event(),
            None,
            ActualTransactionAudit {
                site: TransactionAuditSite::SetupUnstarted,
                transaction: 1,
                cause_wait: 2,
                barrier_wait: None,
            },
        )
        .unwrap();
        assert!(matches!(
            install(
                Arc::clone(&audit_handle.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let status_handle = EventHandle::new();
        bind_status_for_test(status_handle.event(), None, 1, 2).unwrap();
        assert!(matches!(
            install(
                Arc::clone(&status_handle.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::HarnessOnly,
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));
    }

    #[test]
    fn linux_faithful_wait_requires_exact_pid_code_and_enabled_result_class() {
        let handle = EventHandle::new();
        let mut args = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding: binding(handle.event(), 81),
            producer: PhysicalWaitProducer::NotifierWorker,
            attempt: None,
            idtype: libc::P_PIDFD,
            id: 81,
            options: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        };
        let stopped = RawWaitidFrame {
            rc: 0,
            errno: 0,
            siginfo: Some(PhysicalWaitSiginfo {
                signo: libc::SIGCHLD,
                errno: 0,
                code: libc::CLD_STOPPED,
                pid: args.binding.pid.as_raw(),
                uid: 0,
                status: libc::SIGSTOP,
            }),
        };
        assert!(Script::faithful_wait(stopped, args));

        let mut wrong_pid = stopped;
        wrong_pid.siginfo.as_mut().unwrap().pid += 1;
        assert!(!Script::faithful_wait(wrong_pid, args));
        let mut wrong_code = stopped;
        wrong_code.siginfo.as_mut().unwrap().code = i32::MAX;
        assert!(!Script::faithful_wait(wrong_code, args));
        args.options = libc::WSTOPPED | libc::__WALL;
        assert!(!Script::faithful_wait(stopped, args));

        args.options = libc::WEXITED | libc::WSTOPPED | libc::__WALL;

        let realtime_exit = RawWaitidFrame {
            rc: 0,
            errno: 0,
            siginfo: Some(PhysicalWaitSiginfo {
                signo: libc::SIGCHLD,
                errno: 0,
                code: libc::CLD_KILLED,
                pid: args.binding.pid.as_raw(),
                uid: 0,
                status: 34,
            }),
        };
        assert!(Script::faithful_wait(realtime_exit, args));

        let no_status = RawWaitidFrame {
            rc: 0,
            errno: 0,
            siginfo: Some(PhysicalWaitSiginfo {
                signo: 0,
                errno: 0,
                code: 0,
                pid: 0,
                uid: 0,
                status: 0,
            }),
        };
        assert!(!Script::faithful_wait(no_status, args));
        args.site = WaitSite::UnobservedBoundaryProbe;
        args.producer = PhysicalWaitProducer::PreRegistrationBarrierCleanup;
        args.options = libc::WEXITED | libc::WSTOPPED | libc::WNOHANG | libc::__WALL;
        assert!(Script::faithful_wait(no_status, args));
        assert!(Script::faithful_wait(
            RawWaitidFrame {
                rc: -1,
                errno: libc::ECHILD,
                siginfo: None,
            },
            args,
        ));
    }

    #[test]
    fn linux_faithful_wait_rejects_intrinsically_illegal_self_matching_calls() {
        let handle = EventHandle::new();
        let canonical = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding: binding(handle.event(), 181),
            producer: PhysicalWaitProducer::NotifierWorker,
            attempt: None,
            idtype: libc::P_PIDFD,
            id: 181,
            options: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        };
        let error_frame = RawWaitidFrame {
            rc: -1,
            errno: libc::ECHILD,
            siginfo: None,
        };
        assert!(Script::faithful_wait(error_frame, canonical));

        let invalid = [
            WaitArgs {
                idtype: libc::P_PID,
                ..canonical
            },
            WaitArgs {
                id: canonical.id + 1,
                ..canonical
            },
            WaitArgs {
                binding: binding(handle.event(), -1),
                id: (-1_i32) as libc::id_t,
                ..canonical
            },
            WaitArgs {
                options: canonical.options | (1 << 20),
                ..canonical
            },
            WaitArgs {
                options: canonical.options & !libc::__WALL,
                ..canonical
            },
            WaitArgs {
                site: WaitSite::RetainedBarrier,
                producer: PhysicalWaitProducer::PreRegistrationBarrier,
                // A retained barrier requires WNOWAIT at this exact site.
                options: canonical.options,
                ..canonical
            },
        ];
        for args in invalid {
            assert!(
                !Script::faithful_wait(error_frame, args),
                "accepted intrinsically illegal wait call {args:?}"
            );
        }

        // Matching an invalid call byte-for-byte in the scripted expectation
        // cannot lower the Linux-faithful intrinsic-call oracle.
        let args = invalid[3];
        let step = Step::Wait(ExpectedWait {
            args,
            attempt: None,
            pending_status: None,
            frame: error_frame,
        });
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![step.clone()],
        )
        .unwrap();
        assert_eq!(dispatch_wait(handle.event(), args).expect_scripted(), eproto_wait());
        guard
            .finish_err(ExpectedViolation {
                call_index: 0,
                message: "wait arguments, symbols, or raw frame differ",
                expected: Some(step),
                actual: ActualCall::Wait(args),
            })
            .unwrap()
            .expect_expected_violation();
    }

    #[test]
    fn linux_faithful_errno_domains_are_operation_exact() {
        let handle = EventHandle::new();
        let pidfd = 281;
        let base = binding(handle.event(), pidfd);
        let wait = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding: base,
            producer: PhysicalWaitProducer::NotifierWorker,
            attempt: None,
            idtype: libc::P_PIDFD,
            id: pidfd as libc::id_t,
            options: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        };
        for errno in [libc::EINTR, libc::ECHILD] {
            assert!(Script::faithful_wait(
                RawWaitidFrame { rc: -1, errno, siginfo: None },
                wait,
            ));
        }

        let signal = SignalArgs {
            site: SignalSite::ObservedCleanup,
            binding: CallBinding { transaction: Some(1), ..base },
            attempt: Some(2),
            signal: libc::SIGKILL,
            siginfo_is_null: true,
            flags: 0,
        };
        for errno in [libc::ESRCH, libc::EPERM, libc::ENOMEM] {
            assert!(Script::faithful_signal(
                RawSyscallFrame { rc: -1, errno },
                signal,
            ));
        }

        let poll = PollArgs {
            purpose: PollPurpose::SignalFailure,
            binding: CallBinding { transaction: Some(1), ..base },
            events: libc::POLLIN,
            timeout_ms: 0,
        };
        for errno in [libc::EINTR, libc::ENOMEM] {
            assert!(Script::faithful_poll(
                RawPollFrame { rc: -1, errno, revents: 0 },
                poll,
            ));
        }

        let resume = ContinueArgs {
            site: ContinueSite::ObservedCleanup,
            binding: CallBinding {
                transaction: Some(1),
                source_status: Some(3),
                ..base
            },
            attempt: Some(4),
            request: libc::PTRACE_CONT,
            target_tid: base.pid,
            addr: 0,
            data: 0,
            owner: PhysicalResumeOwner::StartupBarrierCleanup,
        };
        for errno in [libc::ESRCH, libc::EIO, libc::EPERM] {
            assert!(Script::faithful_continue(
                RawSyscallFrame { rc: -1, errno },
                resume,
            ));
        }

        for impossible in [libc::EFAULT, libc::ENOSYS, 4095] {
            assert!(!Script::faithful_wait(
                RawWaitidFrame { rc: -1, errno: impossible, siginfo: None },
                wait,
            ));
            assert!(!Script::faithful_signal(
                RawSyscallFrame { rc: -1, errno: impossible },
                signal,
            ));
            assert!(!Script::faithful_poll(
                RawPollFrame { rc: -1, errno: impossible, revents: 0 },
                poll,
            ));
            assert!(!Script::faithful_continue(
                RawSyscallFrame { rc: -1, errno: impossible },
                resume,
            ));
        }
        assert!(!Script::faithful_wait(
            RawWaitidFrame { rc: -1, errno: libc::ESRCH, siginfo: None },
            wait,
        ));
        assert!(!Script::faithful_signal(
            RawSyscallFrame { rc: -1, errno: libc::ECHILD },
            signal,
        ));
        assert!(!Script::faithful_poll(
            RawPollFrame { rc: -1, errno: libc::ESRCH, revents: 0 },
            poll,
        ));
        assert!(!Script::faithful_continue(
            RawSyscallFrame { rc: -1, errno: libc::ENOMEM },
            resume,
        ));

        let impossible = EventHandle::new();
        let impossible_signal = SignalArgs {
            binding: binding(impossible.event(), 282),
            site: SignalSite::UnobservedCleanup,
            attempt: None,
            signal: libc::SIGKILL,
            siginfo_is_null: true,
            flags: 0,
        };
        let guard = install(
            Arc::clone(&impossible.0),
            Fidelity::ImpossibleFault("EFAULT counterexample"),
            CompletionExpectation::HarnessOnly,
            vec![Step::Signal(ExpectedSignal {
                args: impossible_signal,
                transaction: None,
                attempt: None,
                frame: RawSyscallFrame { rc: -1, errno: libc::EFAULT },
            })],
        )
        .unwrap();
        assert_eq!(
            dispatch_signal(impossible.event(), impossible_signal).expect_scripted(),
            RawSyscallFrame { rc: -1, errno: libc::EFAULT }
        );
        guard.finish_ok().unwrap().expect_harness_only();
    }

    #[test]
    fn real_fatal_lifecycle_blocks_every_raw_and_postprocessing_call() {
        let handle = EventHandle::new();
        let (signal, _) = signal_step(handle.event(), 82);
        let operation = match dispatch_signal(handle.event(), signal) {
            Dispatch::Real(operation) => operation,
            Dispatch::Scripted(_, operation) => {
                operation.complete();
                panic!("vacant startup slot did not select real mode")
            }
        };
        drop(operation);

        let wait = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding: binding(handle.event(), 82),
            producer: PhysicalWaitProducer::NotifierWorker,
            attempt: None,
            idtype: libc::P_PIDFD,
            id: 82,
            options: WaitPidFlag::WEXITED.bits(),
        };
        assert_eq!(dispatch_wait(handle.event(), wait).expect_scripted(), eproto_wait());
        assert_eq!(
            dispatch_signal(handle.event(), signal).expect_scripted(),
            eproto_syscall()
        );
        let poll = PollArgs {
            purpose: PollPurpose::UnobservedBoundary,
            binding: binding(handle.event(), 82),
            events: libc::POLLIN,
            timeout_ms: 0,
        };
        assert_eq!(dispatch_poll(handle.event(), poll).expect_scripted(), eproto_poll());
        let resume = ContinueArgs {
            site: ContinueSite::UnobservedCleanup,
            binding: binding(handle.event(), 82),
            attempt: None,
            request: libc::PTRACE_CONT,
            target_tid: Pid::from_raw(71),
            addr: 0,
            data: 0,
            owner: PhysicalResumeOwner::StartupBarrierCleanup,
        };
        assert_eq!(
            dispatch_continue(handle.event(), resume).expect_scripted(),
            eproto_syscall()
        );
        assert_eq!(
            bind_generated_for_test(
                handle.event(),
                SymbolKind::Transaction,
                1,
                ActualBindSite::Transaction(TransactionSite::SetupPrepared),
            ),
            Err(Errno::EPROTO)
        );
        assert_eq!(
            audit_transaction(
                handle.event(),
                None,
                ActualTransactionAudit {
                    site: TransactionAuditSite::SetupUnstarted,
                    transaction: 1,
                    cause_wait: 2,
                    barrier_wait: None,
                },
            ),
            Err(Errno::EPROTO)
        );
        assert_eq!(
            bind_status_for_test(handle.event(), None, 1, 2),
            Err(Errno::EPROTO)
        );
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Real {
                active_operations: 0,
                active_tokens,
                fatal_lifecycle: Some(Violation {
                    message: "real startup raw operation lease was abandoned",
                    ..
                }),
                ..
            } if active_tokens.is_empty()
        ));
    }

    #[test]
    fn admitted_wait_finishes_bind_and_state_after_close_request() {
        let handle = EventHandle::new();
        let observer = PhysicalEventObserver::new(crate::PhysicalEventObserverConfig::new(16, 16))
            .expect("create close-race observer");
        handle
            .attach_physical_observer(&observer)
            .expect("attach close-race observer");
        let pid = Pid::from_raw(71);
        let pidfd = 83;
        let task = PhysicalTaskIdentity::direct_child_with_pidfd(pid, pidfd);
        let flags = WaitPidFlag::from_bits_retain(
            WaitPidFlag::WEXITED.bits() | WaitPidFlag::WSTOPPED.bits() | libc::__WALL,
        );
        let producer = PhysicalWaitProducer::NotifierWorker;
        let args = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding: CallBinding::new(handle.event(), pid, pidfd, task, None, None),
            producer,
            attempt: None,
            idtype: libc::P_PIDFD,
            id: pidfd as libc::id_t,
            options: flags.bits(),
        };
        let steps = vec![
            Step::Bind(ExpectedBind {
                symbol: Symbol(1),
                kind: SymbolKind::WaitAttempt,
                site: ExpectedBindSite::WaitAttempt {
                    generation: Some(handle.event().generation),
                    task,
                    producer,
                    flags: flags.bits(),
                },
            }),
            Step::Wait(ExpectedWait {
                args,
                attempt: Some(ExpectedId::Ref {
                    symbol: Symbol(1),
                    kind: SymbolKind::WaitAttempt,
                }),
                pending_status: Some(Symbol(2)),
                frame: RawWaitidFrame {
                    rc: 0,
                    errno: 0,
                    siginfo: Some(PhysicalWaitSiginfo {
                        signo: libc::SIGCHLD,
                        errno: 0,
                        code: libc::CLD_STOPPED,
                        pid: pid.as_raw(),
                        uid: 0,
                        status: libc::SIGSTOP,
                    }),
                },
            }),
            Step::BindStatus(ExpectedBindStatus {
                symbol: Symbol(2),
                wait_attempt: ExpectedId::Ref {
                    symbol: Symbol(1),
                    kind: SymbolKind::WaitAttempt,
                },
            }),
        ];
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            steps,
        )
        .unwrap();
        let request = guard.close_request();
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        arm_wait_post_dispatch_pause(
            handle.event(),
            Arc::clone(&entered),
            Arc::clone(&resume),
        )
        .unwrap();
        let closer = thread::spawn(move || {
            entered.wait();
            request.request().unwrap();
            resume.wait();
        });
        let observation = super::super::wait_pidfd_once_parts(
            handle.event(),
            pid,
            pidfd,
            task,
            flags,
            producer,
        )
        .expect("scripted wait completed after close request");
        closer.join().unwrap();
        assert!(observation.status.is_some());
        guard.finish_ok().unwrap().expect_harness_only();
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::HarnessOnlyPassed,
                late_violation: None,
                ..
            }
        ));
    }

    #[test]
    fn admitted_wait_audit_uses_exact_deferred_token_during_closing() {
        let handle = EventHandle::new();
        let task = binding(handle.event(), 88).task;
        let flags = WaitPidFlag::from_bits_retain(
            WaitPidFlag::WEXITED.bits() | WaitPidFlag::WSTOPPED.bits() | libc::__WALL,
        );
        let wait_site = ActualBindSite::WaitAttempt {
            generation: Some(handle.event().generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            flags: flags.bits(),
        };
        let wait_args = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding: binding(handle.event(), 88),
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            attempt: Some(102),
            idtype: libc::P_PIDFD,
            id: 88,
            options: flags.bits(),
        };
        let audit = ActualTransactionAudit {
            site: TransactionAuditSite::SetupUnstarted,
            transaction: 101,
            cause_wait: 102,
            barrier_wait: None,
        };
        let steps = vec![
            Step::Bind(ExpectedBind {
                symbol: Symbol(1),
                kind: SymbolKind::Transaction,
                site: ExpectedBindSite::Transaction(TransactionSite::SetupPrepared),
            }),
            Step::Bind(ExpectedBind {
                symbol: Symbol(2),
                kind: SymbolKind::WaitAttempt,
                site: ExpectedBindSite::WaitAttempt {
                    generation: Some(handle.event().generation),
                    task,
                    producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
                    flags: flags.bits(),
                },
            }),
            Step::Wait(ExpectedWait {
                args: wait_args,
                attempt: Some(ExpectedId::Ref {
                    symbol: Symbol(2),
                    kind: SymbolKind::WaitAttempt,
                }),
                pending_status: None,
                frame: RawWaitidFrame {
                    rc: -1,
                    errno: libc::ECHILD,
                    siginfo: None,
                },
            }),
            Step::AuditTransaction(ExpectedTransactionAudit {
                site: TransactionAuditSite::SetupUnstarted,
                transaction: ExpectedId::Ref {
                    symbol: Symbol(1),
                    kind: SymbolKind::Transaction,
                },
                cause_wait: ExpectedId::Ref {
                    symbol: Symbol(2),
                    kind: SymbolKind::WaitAttempt,
                },
                barrier_wait: None,
            }),
        ];
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            steps,
        )
        .unwrap();
        let request = guard.close_request();
        bind_generated_for_test(
            handle.event(),
            SymbolKind::Transaction,
            101,
            ActualBindSite::Transaction(TransactionSite::SetupPrepared),
        )
        .unwrap();
        bind_generated_for_test(handle.event(), SymbolKind::WaitAttempt, 102, wait_site).unwrap();
        let operation = match dispatch_wait(handle.event(), wait_args) {
            Dispatch::Scripted(frame, operation) => {
                assert_eq!(frame.errno, libc::ECHILD);
                operation
            }
            Dispatch::Real(operation) => {
                operation.complete();
                panic!("audit script selected real mode")
            }
        };
        let token = operation.token().unwrap();
        operation.complete();
        request.request().unwrap();
        audit_transaction(handle.event(), Some(token), audit).unwrap();
        guard.finish_ok().unwrap().expect_harness_only();
    }

    #[test]
    fn omitted_wait_audit_fails_final_finish_without_waiting() {
        let handle = EventHandle::new();
        let task = binding(handle.event(), 188).task;
        let flags = libc::WEXITED | libc::WSTOPPED | libc::__WALL;
        let wait_site = ActualBindSite::WaitAttempt {
            generation: Some(handle.event().generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            flags,
        };
        let wait_args = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding: binding(handle.event(), 188),
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            attempt: Some(202),
            idtype: libc::P_PIDFD,
            id: 188,
            options: flags,
        };
        let audit = ExpectedTransactionAudit {
            site: TransactionAuditSite::SetupUnstarted,
            transaction: ExpectedId::Ref {
                symbol: Symbol(1),
                kind: SymbolKind::Transaction,
            },
            cause_wait: ExpectedId::Ref {
                symbol: Symbol(2),
                kind: SymbolKind::WaitAttempt,
            },
            barrier_wait: None,
        };
        let steps = vec![
            Step::Bind(ExpectedBind {
                symbol: Symbol(1),
                kind: SymbolKind::Transaction,
                site: ExpectedBindSite::Transaction(TransactionSite::SetupPrepared),
            }),
            Step::Bind(ExpectedBind {
                symbol: Symbol(2),
                kind: SymbolKind::WaitAttempt,
                site: ExpectedBindSite::WaitAttempt {
                    generation: Some(handle.event().generation),
                    task,
                    producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
                    flags,
                },
            }),
            Step::Wait(ExpectedWait {
                args: wait_args,
                attempt: Some(ExpectedId::Ref {
                    symbol: Symbol(2),
                    kind: SymbolKind::WaitAttempt,
                }),
                pending_status: None,
                frame: RawWaitidFrame {
                    rc: -1,
                    errno: libc::ECHILD,
                    siginfo: None,
                },
            }),
            Step::AuditTransaction(audit),
        ];
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            steps,
        )
        .unwrap();
        bind_generated_for_test(
            handle.event(),
            SymbolKind::Transaction,
            201,
            ActualBindSite::Transaction(TransactionSite::SetupPrepared),
        )
        .unwrap();
        bind_generated_for_test(handle.event(), SymbolKind::WaitAttempt, 202, wait_site).unwrap();
        match dispatch_wait(handle.event(), wait_args) {
            Dispatch::Scripted(frame, operation) => {
                assert_eq!(frame.errno, libc::ECHILD);
                operation.complete();
            }
            Dispatch::Real(operation) => {
                operation.complete();
                panic!("audit script selected real mode")
            }
        }
        let error = guard
            .finish_ok()
            .expect_err("missing causal audit did not close fatally");
        assert_eq!(error.message, "MissingCausalCompletion");
    }

    #[test]
    fn expected_script_error_cannot_mask_abandoned_operation() {
        let handle = EventHandle::new();
        let (args, step) = signal_step(handle.event(), 72);
        let wrong = SignalArgs {
            signal: libc::SIGTERM,
            ..args
        };
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![step.clone()],
        )
        .unwrap();
        drop(dispatch_signal(handle.event(), wrong));
        let error = guard
            .finish_err(ExpectedViolation {
                call_index: 0,
                message: "signal arguments, symbols, cardinality, or raw frame differ",
                expected: Some(step),
                actual: ActualCall::Signal(wrong),
            })
            .expect_err("fatal operation abandonment was masked");
        assert_eq!(error.message, "startup raw operation lease was abandoned");
    }

    #[test]
    fn scoped_expected_error_joins_driver_and_late_calls_remain_excluded() {
        let handle = EventHandle::new();
        let (args, step) = signal_step(handle.event(), 84);
        let wrong = SignalArgs {
            signal: libc::SIGTERM,
            ..args
        };
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![step.clone()],
        )
        .unwrap();
        Scope::run_expect_err(
            guard,
            ExpectedViolation {
                call_index: 0,
                message: "signal arguments, symbols, cardinality, or raw frame differ",
                expected: Some(step),
                actual: ActualCall::Signal(wrong),
            },
            |scope| {
                scope.spawn(|| {}).unwrap();
                assert_eq!(
                    dispatch_signal(handle.event(), wrong).expect_scripted(),
                    eproto_syscall()
                );
            },
        )
        .unwrap()
        .expect_expected_violation();
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::ExpectedViolation(_),
                ..
            }
        ));
        assert_eq!(
            dispatch_signal(handle.event(), args).expect_scripted(),
            eproto_syscall()
        );
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::FatalLifecycle(_),
                late_violation: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn child_unwind_and_guard_drop_preserve_existing_fatal_tombstone() {
        let handle = EventHandle::new();
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = Scope::run(guard, |scope| {
                scope.spawn(|| panic!("driver unwind")).unwrap();
            });
        }))
        .is_err());
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::FatalLifecycle(Violation {
                    message: "startup script driver unwound",
                    ..
                }),
                ..
            }
        ));

        let dropped = EventHandle::new();
        let dropped_guard = install(
            Arc::clone(&dropped.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap();
        let lease = dropped_guard.register_driver().unwrap();
        drop(lease);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(dropped_guard);
        }))
        .is_err());
        assert!(matches!(
            &*dropped.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::FatalLifecycle(Violation {
                    message: "startup script driver lease disappeared before join receipt",
                    ..
                }),
                ..
            }
        ));
    }

    #[test]
    fn wrong_thread_and_retired_operation_tokens_are_fatal_and_retired() {
        let handle = EventHandle::new();
        let (args, step) = signal_step(handle.event(), 85);
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![step],
        )
        .unwrap();
        let mut operation = match dispatch_signal(handle.event(), args) {
            Dispatch::Scripted(_, operation) => operation,
            Dispatch::Real(operation) => {
                operation.complete();
                panic!("script selected real mode")
            }
        };
        let token = operation.token().unwrap();
        operation.origin_tid = Pid::from_raw(-1);
        operation.complete();
        assert_eq!(
            audit_transaction(
                handle.event(),
                Some(token),
                ActualTransactionAudit {
                    site: TransactionAuditSite::SetupUnstarted,
                    transaction: 1,
                    cause_wait: 2,
                    barrier_wait: None,
                },
            ),
            Err(Errno::EPROTO)
        );
        let error = guard.finish_ok().expect_err("wrong-thread token was not fatal");
        assert_eq!(
            error.message,
            "startup raw operation lease completed on the wrong thread"
        );
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::FatalLifecycle(_),
                ..
            }
        ));

        let unbound = EventHandle::new();
        let unbound_guard = install(
            Arc::clone(&unbound.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap();
        let unbound_token = OperationToken {
            nonce: Some(unbound_guard.nonce),
            generation: unbound.event().generation,
            operation: NonZeroU64::MIN,
        };
        assert_eq!(
            audit_transaction(
                unbound.event(),
                Some(unbound_token),
                ActualTransactionAudit {
                    site: TransactionAuditSite::SetupUnstarted,
                    transaction: 1,
                    cause_wait: 2,
                    barrier_wait: None,
                },
            ),
            Err(Errno::EPROTO)
        );
        assert_eq!(
            unbound_guard.finish_ok().unwrap_err().message,
            "startup postprocessing token was not active"
        );
    }

    #[test]
    fn every_late_call_kind_is_rejected_by_the_closed_tombstone() {
        let handle = EventHandle::new();
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap();
        guard.finish_ok().unwrap().expect_harness_only();
        let binding = binding(handle.event(), 86);
        let wait = WaitArgs {
            site: WaitSite::ObservedCleanup,
            binding,
            producer: PhysicalWaitProducer::NotifierWorker,
            attempt: None,
            idtype: libc::P_PIDFD,
            id: 86,
            options: WaitPidFlag::WEXITED.bits(),
        };
        assert_eq!(dispatch_wait(handle.event(), wait).expect_scripted(), eproto_wait());
        let signal = SignalArgs {
            site: SignalSite::UnobservedCleanup,
            binding,
            attempt: None,
            signal: libc::SIGKILL,
            siginfo_is_null: true,
            flags: 0,
        };
        assert_eq!(dispatch_signal(handle.event(), signal).expect_scripted(), eproto_syscall());
        let poll = PollArgs {
            purpose: PollPurpose::UnobservedBoundary,
            binding,
            events: libc::POLLIN,
            timeout_ms: 0,
        };
        assert_eq!(dispatch_poll(handle.event(), poll).expect_scripted(), eproto_poll());
        let resume = ContinueArgs {
            site: ContinueSite::UnobservedCleanup,
            binding,
            attempt: None,
            request: libc::PTRACE_CONT,
            target_tid: binding.pid,
            addr: 0,
            data: 0,
            owner: PhysicalResumeOwner::StartupBarrierCleanup,
        };
        assert_eq!(dispatch_continue(handle.event(), resume).expect_scripted(), eproto_syscall());
        assert!(bind_generated_for_test(
            handle.event(),
            SymbolKind::Transaction,
            1,
            ActualBindSite::Transaction(TransactionSite::SetupPrepared),
        )
        .is_err());
        assert!(audit_transaction(
            handle.event(),
            None,
            ActualTransactionAudit {
                site: TransactionAuditSite::SetupUnstarted,
                transaction: 1,
                cause_wait: 2,
                barrier_wait: None,
            },
        )
        .is_err());
        assert!(bind_status_for_test(handle.event(), None, 1, 2).is_err());
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::FatalLifecycle(_),
                late_violation: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn handle_adoption_does_not_redirect_generation_bound_script_or_token() {
        let requested = EventHandle::new();
        let original = Arc::clone(&requested.0);
        let authoritative = EventHandle::new();
        let (args, step) = signal_step(&original.event, 87);
        let guard = install(
            Arc::clone(&original),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![step],
        )
        .unwrap();
        requested
            .adopt_authoritative(&authoritative)
            .expect("adopt authoritative generation");

        assert_eq!(
            dispatch_signal(&original.event, args).expect_scripted(),
            RawSyscallFrame { rc: 0, errno: 0 }
        );
        let authoritative_args = SignalArgs {
            binding: binding(authoritative.event(), 87),
            ..args
        };
        let real = match dispatch_signal(authoritative.event(), authoritative_args) {
            Dispatch::Real(operation) => operation,
            Dispatch::Scripted(_, operation) => {
                operation.complete();
                panic!("adoption redirected the original script")
            }
        };
        real.complete();
        guard.finish_ok().unwrap().expect_harness_only();
    }

    #[test]
    fn checked_operation_counter_overflow_is_fatal() {
        let handle = EventHandle::new();
        let (args, step) = signal_step(handle.event(), 73);
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![step],
        )
        .unwrap();
        {
            let mut slot = handle.event().startup_syscall_script.lock();
            let Slot::Open { active_dispatchers, .. } = &mut *slot else {
                panic!("script was not open")
            };
            *active_dispatchers = usize::MAX;
        }
        let frame = dispatch_signal(handle.event(), args).expect_scripted();
        assert_eq!(frame.errno, Errno::EPROTO.into_raw());
        {
            let mut slot = handle.event().startup_syscall_script.lock();
            let Slot::Open { active_dispatchers, .. } = &mut *slot else {
                panic!("script was not open")
            };
            *active_dispatchers = 0;
        }
        assert_eq!(
            guard.finish_ok().expect_err("overflow was not fatal").message,
            "startup raw operation count overflowed"
        );

        let driver_handle = EventHandle::new();
        let driver_guard = install(
            Arc::clone(&driver_handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap();
        {
            let mut slot = driver_handle.event().startup_syscall_script.lock();
            let Slot::Open { driver_leases, .. } = &mut *slot else {
                panic!("driver script was not open")
            };
            *driver_leases = usize::MAX;
        }
        assert!(matches!(driver_guard.register_driver(), Err(Errno::EOVERFLOW)));
        {
            let mut slot = driver_handle.event().startup_syscall_script.lock();
            let Slot::Open { driver_leases, .. } = &mut *slot else {
                panic!("driver script was not open")
            };
            *driver_leases = 1;
        }
        assert_eq!(
            driver_guard
                .finish_ok()
                .expect_err("driver overflow was not fatal")
                .message,
            "startup script driver lease count overflowed"
        );
    }

    #[test]
    fn final_finish_rejects_unfinished_operations_without_waiting() {
        let handle = EventHandle::new();
        let (args, step) = signal_step(handle.event(), 74);
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![step],
        )
        .unwrap();
        let operation = match dispatch_signal(handle.event(), args) {
            Dispatch::Scripted(_, operation) => operation,
            Dispatch::Real(operation) => {
                operation.complete();
                panic!("script selected real mode")
            }
        };
        let error = guard
            .finish_ok()
            .expect_err("unfinished signal operation was accepted");
        assert_eq!(error.message, "UnfinishedOperation");
        operation.complete();

        let poll_handle = EventHandle::new();
        let poll_args = PollArgs {
            purpose: PollPurpose::UnobservedBoundary,
            binding: binding(poll_handle.event(), 75),
            events: libc::POLLIN,
            timeout_ms: 0,
        };
        let poll_guard = install(
            Arc::clone(&poll_handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            vec![Step::Poll(ExpectedPoll {
                args: poll_args,
                transaction: None,
                source_status: None,
                frame: RawPollFrame {
                    rc: 0,
                    errno: 0,
                    revents: 0,
                },
            })],
        )
        .unwrap();
        let poll_operation = match dispatch_poll(poll_handle.event(), poll_args) {
            Dispatch::Scripted(_, operation) => operation,
            Dispatch::Real(operation) => {
                operation.complete();
                panic!("poll script selected real mode")
            }
        };
        let error = poll_guard
            .finish_ok()
            .expect_err("unfinished poll operation was accepted");
        assert_eq!(error.message, "UnfinishedOperation");
        poll_operation.complete();
    }

    #[test]
    fn scope_joins_child_driver_and_caught_unwind_closes_abandoned() {
        let handle = EventHandle::new();
        let guard = install(
            Arc::clone(&handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let child_barrier = Arc::clone(&barrier);
        Scope::run(guard, |scope| {
            scope
                .spawn(move || {
                    child_barrier.wait();
                })
                .unwrap();
            barrier.wait();
        })
        .unwrap()
        .expect_harness_only();
        assert!(matches!(
            &*handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::HarnessOnlyPassed,
                ..
            }
        ));

        let unwind_handle = EventHandle::new();
        let unwind_guard = install(
            Arc::clone(&unwind_handle.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::HarnessOnly,
            Vec::new(),
        )
        .unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = Scope::run(unwind_guard, |_| -> () { panic!("scope body panic") });
        }))
        .is_err());
        assert!(matches!(
            &*unwind_handle.event().startup_syscall_script.lock(),
            Slot::Closed {
                outcome: CloseOutcome::Abandoned,
                ..
            }
        ));
    }

    #[test]
    fn protocol_install_and_completion_reject_nonprotocol_evidence() {
        let impossible = EventHandle::new();
        assert!(matches!(
            install(
                Arc::clone(&impossible.0),
                Fidelity::ImpossibleFault("counterexample"),
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EINVAL)
        ));

        let fresh = EventHandle::new();
        let fresh_guard = install(
            Arc::clone(&fresh.0),
            Fidelity::LinuxFaithful,
            CompletionExpectation::ProtocolWorkerDone(
                RegistryCompletionExpectation::Removed,
            ),
            Vec::new(),
        )
        .unwrap();
        assert!(fresh_guard.finish_ok().is_err());

        let protocol_error = EventHandle::new();
        *protocol_error.event().startup_cleanup_protocol_error.lock() = Some(Errno::EPROTO);
        assert!(matches!(
            install(
                Arc::clone(&protocol_error.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));
    }

    #[test]
    fn protocol_install_rejects_late_state_and_prior_real_activity() {
        let worker = EventHandle::new();
        worker
            .event()
            .worker_state
            .store(WORKER_STARTING, Ordering::Release);
        assert!(matches!(
            install(
                Arc::clone(&worker.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let wait_owner = EventHandle::new();
        wait_owner
            .event()
            .wait_owner
            .store(WAIT_OWNER_SYNC, Ordering::Release);
        assert!(matches!(
            install(
                Arc::clone(&wait_owner.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let unstarted_cleanup = EventHandle::new();
        let pid = Pid::from_raw(190);
        let raw_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(raw_fd >= 0);
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        *unstarted_cleanup.event().unstarted_cleanup_identity.lock() =
            Some(OriginalRootPreBarrierIdentity {
                pid,
                task: PhysicalTaskIdentity::direct_child_with_pidfd(pid, raw_fd),
                pidfd,
                launch: None,
            });
        assert!(matches!(
            install(
                Arc::clone(&unstarted_cleanup.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let unobserved_cleanup = EventHandle::new();
        let raw_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(raw_fd >= 0);
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        *unobserved_cleanup.event().unobserved_startup_cleanup.lock() =
            UnobservedStartupCleanupState::Finished {
                pid,
                source_pidfd: raw_fd,
                pidfd,
                cause: Errno::EIO,
                termination: StartupCleanupTermination::TargetAlreadyExited,
                outcome: Some(StartupBarrierConsumeOutcome::Cleaned),
                last_siginfo: None,
                typed_error: None,
            };
        assert!(matches!(
            install(
                Arc::clone(&unobserved_cleanup.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let authority = EventHandle::new();
        authority
            .event()
            .original_root_cleanup_authority
            .store(ROOT_CLEANUP_AUTHORITY_ISSUED, Ordering::Release);
        assert!(matches!(
            install(
                Arc::clone(&authority.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let completion = EventHandle::new();
        completion
            .event()
            .startup_script_completion_stage
            .store(CompletionStage::Prepared as u8, Ordering::Release);
        assert!(matches!(
            install(
                Arc::clone(&completion.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));

        let real = EventHandle::new();
        let (signal, _) = signal_step(real.event(), 189);
        match dispatch_signal(real.event(), signal) {
            Dispatch::Real(operation) => operation.complete(),
            Dispatch::Scripted(_, operation) => {
                operation.complete();
                panic!("fresh Event did not select real mode")
            }
        }
        assert!(matches!(
            install(
                Arc::clone(&real.0),
                Fidelity::LinuxFaithful,
                CompletionExpectation::ProtocolWorkerDone(
                    RegistryCompletionExpectation::Removed,
                ),
                Vec::new(),
            ),
            Err(Errno::EBUSY)
        ));
    }
}
