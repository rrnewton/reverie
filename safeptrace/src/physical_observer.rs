/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Bounded diagnostic observation of physical wait and ptrace transitions.
//!
//! The observer is deliberately independent of guest scheduling. Recording is
//! append-only, uses storage allocated by [`PhysicalEventObserver::new`], and
//! never acquires a lock. The resulting IDs are diagnostic identities only;
//! they are not scheduler order, replay input, or guest-visible time.

use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;
use reverie_process::ControllerLaunchId;

use crate::Pid;

const OBSERVER_OPEN: u8 = 0;
const OBSERVER_CLOSING: u8 = 1;
const OBSERVER_CLOSED: u8 = 2;

static NEXT_OBSERVER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_GENERATION_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_WAIT_ATTEMPT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_STATUS_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_RESERVATION_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_RESUME_ATTEMPT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_PIDFD_SIGNAL_ATTEMPT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_CLEANUP_TRANSACTION_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_ORIGINAL_ROOT_LAUNCH_ID: AtomicU64 = AtomicU64::new(1);

/// Identity of one observer session.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalObserverId(u64);

impl PhysicalObserverId {
    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one immutable notifier event generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalEventGenerationId(u64);

impl PhysicalEventGenerationId {
    pub(crate) fn allocate() -> Self {
        Self(next_nonzero(&NEXT_GENERATION_ID))
    }

    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one actual kernel wait attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalWaitAttemptId(u64);

impl PhysicalWaitAttemptId {
    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one status returned by the kernel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalStatusId(u64);

impl PhysicalStatusId {
    pub(crate) fn from_raw(raw: u64) -> Option<Self> {
        (raw != 0).then_some(Self(raw))
    }

    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one FIFO or terminal-status reservation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalReservationId(u64);

impl PhysicalReservationId {
    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one actual ptrace resume attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalResumeAttemptId(u64);

impl PhysicalResumeAttemptId {
    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one generation-bound `pidfd_send_signal` attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalPidfdSignalAttemptId(u64);

impl PhysicalPidfdSignalAttemptId {
    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one registered-cleanup drain transaction.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalCleanupTransactionId(u64);

impl PhysicalCleanupTransactionId {
    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of the one controller-spawned original-root launch link.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalOriginalRootLaunchId(u64);

impl PhysicalOriginalRootLaunchId {
    /// Returns the nonzero integer representation.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Linear proof that an observer was attached to the exact `Running` value
/// created for the controller's original `Command::spawn` child.
#[derive(Debug)]
pub struct OriginalRootLaunchToken {
    pub(crate) observer: PhysicalObserverId,
    pub(crate) link: PhysicalOriginalRootLaunchId,
    pub(crate) generation: PhysicalEventGenerationId,
    pub(crate) task: PhysicalTaskIdentity,
    pub(crate) controller_launch: ControllerLaunchId,
}

/// Session-bound token for one allocation-free cleanup transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalCleanupTransaction {
    observer: PhysicalObserverId,
    id: PhysicalCleanupTransactionId,
}

/// Causal owner of one physical cleanup transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalCleanupTransactionKind {
    /// Cleanup after a notifier or synchronous wait failure.
    Registered,
    /// Pre-worker cleanup of the original-root retained startup barrier.
    StartupBarrier {
        /// Immutable original-root Event generation.
        generation: PhysicalEventGenerationId,
        /// Exact retained `WNOWAIT` barrier attempt B.
        barrier: PhysicalWaitAttemptId,
        /// Exact pidfd/procfs task identity used by every cleanup wait.
        task: PhysicalTaskIdentity,
        /// Runtime owner which consumed the retained startup status.
        owner: PhysicalStartupCleanupOwner,
    },
    /// Cleanup selected before a retained startup barrier existed.
    StartupSetup {
        /// Immutable original-root Event generation.
        generation: PhysicalEventGenerationId,
        /// Exact pidfd-bound task retained before the first cleanup wait.
        task: PhysicalTaskIdentity,
        /// Original setup error which selected cleanup.
        error: i32,
        /// Spawn-sourced pidfd capability bound before any cleanup wait.
        launch: PhysicalOriginalRootLaunchId,
    },
}

/// Runtime owner of one original-root startup cleanup transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalStartupCleanupOwner {
    /// Cleanup selected before any notifier worker started.
    Unstarted,
    /// The authorized root worker consumed a mismatching first status.
    AuthorizedWorker,
}

impl PhysicalCleanupTransaction {
    /// Returns the globally unique transaction identity.
    pub fn id(self) -> PhysicalCleanupTransactionId {
        self.id
    }
}

fn next_nonzero(counter: &AtomicU64) -> u64 {
    let id = counter.fetch_add(1, Ordering::Relaxed);
    if id == 0 || id == u64::MAX {
        // Exhausting a 64-bit diagnostic identity space is unrecoverable. It
        // cannot be repaired by wrapping because that would silently merge two
        // physical events.
        std::process::abort();
    }
    id
}

/// Fixed capacities allocated before observation begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalEventObserverConfig {
    ordinary_capacity: usize,
    cleanup_capacity: usize,
}

impl PhysicalEventObserverConfig {
    /// Creates a configuration with separate ordinary and cleanup capacity.
    ///
    /// Cleanup capacity is never borrowed by ordinary records, so cleanup can
    /// continue to leave evidence after ordinary recording overflows.
    pub fn new(ordinary_capacity: usize, cleanup_capacity: usize) -> Self {
        Self {
            ordinary_capacity,
            cleanup_capacity,
        }
    }

    /// Returns the number of ordinary record slots.
    pub fn ordinary_capacity(self) -> usize {
        self.ordinary_capacity
    }

    /// Returns the number of cleanup record slots reserved from ordinary use.
    pub fn cleanup_capacity(self) -> usize {
        self.cleanup_capacity
    }
}

impl Default for PhysicalEventObserverConfig {
    fn default() -> Self {
        Self::new(8192, 1024)
    }
}

/// Failure to construct a bounded observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalObserverCreateError {
    /// At least one of the two reserved buffers had zero capacity.
    ZeroCapacity,
}

/// Failure to attach an observer to an immutable notifier generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalObserverAttachError {
    /// A different observer is already attached to this generation.
    DifferentObserverAlreadyAttached,
    /// Kernel wait ownership or worker startup began before attachment.
    WaitAlreadyStarted,
    /// The observer was already closed.
    ObserverClosed,
    /// This Event lacks the exact unclaimed controller-spawn capability, or
    /// the observer session already claimed its sole original-root launch.
    InvalidOriginalRootLaunch,
    /// The observer could not reserve both mandatory attachment records before
    /// mutating the Event.
    InsufficientCapacity,
}

/// The physical producer that issued a wait call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalWaitProducer {
    /// The per-task asynchronous notifier worker.
    NotifierWorker,
    /// The sole original after-loader root worker, authorized from generation
    /// start to include `WCONTINUED` in its blocking wait.
    AuthorizedRootNotifier,
    /// The authorized root worker's nonblocking stale-continued drain before
    /// publishing a stopped status.
    PreStopContinuedDrain,
    /// The synchronous `Running::wait` producer.
    SynchronousWait,
    /// The one retained `P_PIDFD` startup observation made before notifier
    /// registration for the immutable original after-loader root.
    PreRegistrationBarrier,
    /// The bounded direct-child cleanup before notifier registration.
    PreRegistrationCleanup,
    /// Exact-pidfd cleanup which consumes a retained original-root startup
    /// barrier after setup failed or the child was already terminal.
    PreRegistrationBarrierCleanup,
    /// Cleanup after notifier registration.
    RegisteredCleanup,
}

impl PhysicalWaitProducer {
    fn is_cleanup(self) -> bool {
        matches!(
            self,
            Self::PreRegistrationCleanup
                | Self::PreRegistrationBarrierCleanup
                | Self::RegisteredCleanup
        )
    }
}

/// Exact task identity available at a physical operation boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalTaskIdentity {
    tid: i32,
    tgid: Option<i32>,
    ppid: Option<i32>,
    tracer_pid: Option<i32>,
    start_time: Option<u64>,
    proc_inode: Option<u64>,
    pidfd: Option<i32>,
}

impl PhysicalTaskIdentity {
    /// Creates the narrow identity available for an unreaped direct child.
    pub fn direct_child(tid: Pid) -> Self {
        Self {
            tid: tid.as_raw(),
            tgid: None,
            ppid: None,
            tracer_pid: None,
            start_time: None,
            proc_inode: None,
            pidfd: None,
        }
    }

    pub(crate) fn direct_child_with_pidfd(tid: Pid, pidfd: i32) -> Self {
        Self {
            tid: tid.as_raw(),
            tgid: None,
            ppid: None,
            tracer_pid: None,
            start_time: None,
            proc_inode: None,
            pidfd: Some(pidfd),
        }
    }

    /// Creates a fully captured pidfd/procfs identity.
    pub fn captured(tid: Pid, tgid: Pid, start_time: u64, proc_inode: u64, pidfd: i32) -> Self {
        Self {
            tid: tid.as_raw(),
            tgid: Some(tgid.as_raw()),
            ppid: None,
            tracer_pid: None,
            start_time: Some(start_time),
            proc_inode: Some(proc_inode),
            pidfd: Some(pidfd),
        }
    }

    /// Creates a fully captured pidfd/procfs identity including the mutable
    /// parent/tracer snapshot authenticated at this observation boundary.
    pub fn captured_with_controller(
        tid: Pid,
        tgid: Pid,
        ppid: Pid,
        tracer_pid: Pid,
        start_time: u64,
        proc_inode: u64,
        pidfd: i32,
    ) -> Self {
        Self {
            tid: tid.as_raw(),
            tgid: Some(tgid.as_raw()),
            ppid: Some(ppid.as_raw()),
            tracer_pid: Some(tracer_pid.as_raw()),
            start_time: Some(start_time),
            proc_inode: Some(proc_inode),
            pidfd: Some(pidfd),
        }
    }

    /// Returns the physical thread ID.
    pub fn tid(self) -> i32 {
        self.tid
    }

    /// Returns the thread-group ID when it was captured.
    pub fn tgid(self) -> Option<i32> {
        self.tgid
    }

    /// Returns the real parent captured at this observation boundary.
    pub fn ppid(self) -> Option<i32> {
        self.ppid
    }

    /// Returns the ptracer captured at this observation boundary.
    pub fn tracer_pid(self) -> Option<i32> {
        self.tracer_pid
    }

    /// Returns the proc start-time field when it was captured.
    pub fn start_time(self) -> Option<u64> {
        self.start_time
    }

    /// Returns the inode of the captured proc directory when available.
    pub fn proc_inode(self) -> Option<u64> {
        self.proc_inode
    }

    /// Returns the diagnostic raw pidfd number when available.
    pub fn pidfd(self) -> Option<i32> {
        self.pidfd
    }

    pub(crate) fn with_pidfd(mut self, pidfd: i32) -> Self {
        self.pidfd = Some(pidfd);
        self
    }

    fn is_direct_child(self) -> bool {
        self.tid > 0
            && self.tgid.is_none()
            && self.ppid.is_none()
            && self.tracer_pid.is_none()
            && self.start_time.is_none()
            && self.proc_inode.is_none()
            && self.pidfd.is_none()
    }

    fn is_pidfd_bound_direct_child(self) -> bool {
        self.tid > 0
            && self.tgid.is_none()
            && self.ppid.is_none()
            && self.tracer_pid.is_none()
            && self.start_time.is_none()
            && self.proc_inode.is_none()
            && self.pidfd.is_some_and(|pidfd| pidfd >= 0)
    }

    fn is_captured(self) -> bool {
        self.tid > 0
            && self.tgid.is_some_and(|tgid| tgid > 0)
            && self.start_time.is_some()
            && self.proc_inode.is_some_and(|inode| inode != 0)
            && self.pidfd.is_some_and(|pidfd| pidfd >= 0)
    }

    fn same_stable_task(self, other: Self) -> bool {
        self.is_captured()
            && other.is_captured()
            && self.tid == other.tid
            && self.tgid == other.tgid
            && self.start_time == other.start_time
            && self.proc_inode == other.proc_inode
    }

    fn merge_authority(self, other: Self) -> Option<Self> {
        match (
            self.is_direct_child(),
            self.is_captured(),
            other.is_direct_child(),
            other.is_captured(),
        ) {
            (true, false, true, false) => (self == other).then_some(self),
            (true, false, false, true) => (self.tid == other.tid).then_some(other),
            (false, true, true, false) => (self.tid == other.tid).then_some(self),
            (false, true, false, true) => self.same_stable_task(other).then_some(self),
            _ => None,
        }
    }

    fn authorizes(self, observed: Self, allow_direct_narrowing: bool) -> bool {
        observed == self
            || self.same_stable_task(observed)
            || (allow_direct_narrowing
                && self.is_direct_child()
                && observed.is_pidfd_bound_direct_child()
                && self.tid == observed.tid)
            || (allow_direct_narrowing
                && observed.is_direct_child()
                && self.is_captured()
                && observed.tid == self.tid)
    }
}

/// Raw fields returned by a successful `waitid` syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalWaitSiginfo {
    /// Signal number in `siginfo_t`.
    pub signo: i32,
    /// Error field in `siginfo_t`.
    pub errno: i32,
    /// Child-status code in `siginfo_t`.
    pub code: i32,
    /// Reported child PID.
    pub pid: i32,
    /// Reported child UID.
    pub uid: u32,
    /// Lossless `si_status` value before conversion.
    pub status: i32,
}

/// Description of one kernel wait attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalWaitContext {
    /// Immutable Event generation, absent only before notifier registration.
    pub generation: Option<PhysicalEventGenerationId>,
    /// Exact identity available at the call boundary.
    pub task: PhysicalTaskIdentity,
    /// Code path owning the kernel wait.
    pub producer: PhysicalWaitProducer,
    /// Raw wait option bits passed to the kernel.
    pub flags: i32,
}

/// Token returned before an actual wait call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalWaitAttempt {
    observer: PhysicalObserverId,
    id: PhysicalWaitAttemptId,
    context: PhysicalWaitContext,
}

impl PhysicalWaitAttempt {
    /// Returns this call's unique attempt ID.
    pub fn id(self) -> PhysicalWaitAttemptId {
        self.id
    }

    /// Returns the context fixed before entering the kernel.
    pub fn context(self) -> PhysicalWaitContext {
        self.context
    }
}

/// Kernel outcome recorded for a wait attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalWaitOutcome {
    /// `WNOWAIT` observed a real status but deliberately retained it in the
    /// kernel for one later, separately recorded consuming wait.
    RetainedStatus {
        /// Lossless compact wait-status word reconstructed from siginfo.
        raw_status: i32,
        /// Exact raw waitid fields used to link the later consumption.
        siginfo: PhysicalWaitSiginfo,
    },
    /// `WNOWAIT` retained a real status whose raw siginfo could not be
    /// converted into the compact wait-status representation.  The retained
    /// kernel status still has to be consumed and linked by exact raw siginfo;
    /// a conversion failure is not permission to forget it.
    RetainedUndecodableStatus {
        /// Exact raw waitid fields retained in the kernel.
        siginfo: PhysicalWaitSiginfo,
        /// Exact conversion errno returned to the startup barrier.
        error: i32,
    },
    /// The syscall returned a real status.
    Status {
        /// Unique identity allocated before status conversion.
        id: PhysicalStatusId,
        /// Lossless raw wait-status word.
        raw_status: i32,
        /// Raw `waitid` fields, absent for the external `waitpid` path.
        siginfo: Option<PhysicalWaitSiginfo>,
    },
    /// A real `waitid` status whose raw fields could not be converted.
    UndecodableStatus {
        /// Unique identity allocated at the kernel boundary.
        id: PhysicalStatusId,
        /// Lossless raw fields returned by `waitid`.
        siginfo: PhysicalWaitSiginfo,
        /// Exact conversion errno returned to the waiter.
        error: i32,
    },
    /// A nonblocking wait completed successfully with no status.
    NoStatus {
        /// Raw `waitid` fields when the producer used `waitid`.
        siginfo: Option<PhysicalWaitSiginfo>,
    },
    /// The call failed with `EINTR` and may be retried.
    Interrupted,
    /// The call failed with `ECHILD`.
    NoChild,
    /// The call failed with another errno.
    Error(i32),
}

/// Destination in which one physical status was published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalContinuedStatusRoute {
    /// No stop-resolution watch was armed on the authorized original root.
    UnwatchedRoot,
    /// Continued status arrived after D was armed but before its first stop.
    BeforeFirstStop,
    /// Continued status arrived after the first stopped candidate but before
    /// that candidate was acknowledged as G.
    AfterFirstStop,
    /// Continued status arrived after exact G acknowledgment.
    AfterAcknowledgedGroupStop,
    /// Stale continued status drained before publishing a stopped frontier.
    PreStopDrain {
        /// Already allocated physical identity of that stopped frontier.
        before: PhysicalStatusId,
    },
}

/// Destination in which one physical status was published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalStatusPublication {
    /// Ordered regular notifier FIFO.
    RegularFifo,
    /// `WCONTINUED` status consumed by the generation-bound stop-resolution
    /// side channel instead of typed Wait decoding.
    ContinuedSideChannel {
        /// Exact route taken by this process-wide continued status.
        route: PhysicalContinuedStatusRoute,
    },
    /// Retained final exit status.
    RetainedTerminal,
    /// Independently retained `PTRACE_EVENT_EXIT` capability.
    ExitCapability,
    /// Synchronous path's rollback-safe regular FIFO.
    SynchronousFifo,
    /// Direct pre-registration status transferred into a typed `Stopped`.
    DirectStopped,
    /// Terminal status consumed by external pre-notifier cleanup.
    ExternalCleanup,
    /// Terminal status drained by registered notifier-failure cleanup.
    CleanupTerminal,
    /// Terminal status drained by exact original-root startup cleanup.
    StartupBarrierCleanupTerminal,
    /// Nonterminal stopped status transferred to registered controller cleanup.
    CleanupStopped,
    /// Plain SIGSTOP candidate retained exclusively for controller cleanup
    /// after its pre-publication continued drain failed.
    PreStopDrainFailureCleanup,
    /// First notifier consumption did not match the retained original-root
    /// startup barrier and transferred the consumed stop to terminal cleanup.
    StartupBarrierFailureCleanup,
    /// Exact retained startup stop matched and was then transferred to
    /// pre-worker cleanup because a later setup step failed.
    StartupBarrierCleanupStopped,
}

/// Result of a decode transaction for one reserved status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalDecodeOutcome {
    /// A typed `Wait` was returned to the consumer.
    Returned,
    /// `Error::Died` consumed the otherwise permanently undecodable status.
    DiedConsumed,
    /// A retryable error rolled the reservation and owner back.
    RetryRolledBack,
    /// Cancellation took ownership before decode.
    Cancelled,
}

/// Ownership path performing a fallible status decode.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PhysicalDecodeOwner {
    /// Asynchronous notifier consumer.
    Notifier,
    /// Synchronous `Running::wait` consumer or retained replay.
    Synchronous,
    /// Whole-session cancellation cleanup.
    Cleanup,
}

impl PhysicalDecodeOwner {
    fn is_cleanup(self) -> bool {
        matches!(self, Self::Cleanup)
    }
}

/// Final disposition when no successful ptrace transition consumes a stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalStatusDisposition {
    /// Cancellation cleanup consumed or made the stop unreachable.
    CancellationCleanup,
    /// Ordinary handling completed without a ptrace resume.
    OrdinaryHandled,
    /// A `WCONTINUED` status was delivered solely to the generation-bound
    /// stop-resolution side channel.
    ContinuedSideChannel {
        /// Exact route that consumed or preserved this continued status.
        route: PhysicalContinuedStatusRoute,
    },
    /// A decode race consumed the status as `Error::Died`.
    DecodeDied,
    /// An unclaimed exit capability expired during terminal cleanup.
    ExitCapabilityExpired,
    /// A later kernel exit stop replaced an earlier ordinary stop from the
    /// same immutable notifier generation.
    KernelSupersededByExitStop,
    /// A recorded ESRCH/EIO continuation became unreachable only after a
    /// trusted later wait publication or exact terminal proof. This one record
    /// is both the causal link and the old source status's sole disposition;
    /// the failed ptrace call itself is not treated as successful.
    AmbiguousResumeCausallyResolved {
        /// Exact resume attempt whose raw result must be ESRCH or EIO.
        attempt: PhysicalResumeAttemptId,
        /// Later kernel evidence making the old source stop unreachable.
        proof: PhysicalAmbiguousResumeProof,
    },
}

/// Trusted kernel-wait evidence that made an ambiguously failed resume's
/// source stop unreachable without repeating the ptrace request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PhysicalAmbiguousResumeProof {
    /// A distinct later stopped status was committed in the same generation.
    LaterStatus(PhysicalStatusId),
    /// A distinct real final status was retained in the same generation.
    FinalStatus(PhysicalStatusId),
    /// Exact pidfd/proc evidence proved a same-generation ECHILD terminal.
    ProvenEchild(PhysicalWaitAttemptId),
}

/// Ptrace operation that can move a stopped tracee.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalResumeOperation {
    /// `PTRACE_CONT`.
    Continue,
    /// `PTRACE_SINGLESTEP`.
    SingleStep,
    /// `PTRACE_SYSCALL`.
    Syscall,
    /// `PTRACE_DETACH` (which also resumes the tracee).
    Detach,
}

/// Source ownership for a physical ptrace transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalResumeOwner {
    /// A typed [`crate::Stopped`] capability.
    TypedStopped,
    /// Synchronous-wait cancellation cleanup.
    SynchronousCancellation,
    /// Direct-child cleanup before notifier registration.
    PreRegistrationCleanup,
    /// Linear cleanup of one retained original-root startup transaction.
    StartupBarrierCleanup,
    /// Controller-thread continuation after a notifier worker atomically
    /// transferred an authorized-root startup cleanup transaction.
    AuthorizedRootExternalCleanup,
    /// Root tracee whole-session cleanup.
    RootCleanup,
    /// Descendant whole-session cleanup.
    DescendantCleanup,
}

impl PhysicalResumeOwner {
    fn is_cleanup(self) -> bool {
        !matches!(self, Self::TypedStopped)
    }

    fn is_registered_controller_cleanup(self) -> bool {
        matches!(
            self,
            Self::SynchronousCancellation | Self::RootCleanup | Self::DescendantCleanup
        )
    }

    fn is_startup_barrier_cleanup(self) -> bool {
        matches!(
            self,
            Self::StartupBarrierCleanup | Self::AuthorizedRootExternalCleanup
        )
    }
}

/// Description fixed before one actual ptrace transition call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalResumeContext {
    /// Immutable Event generation when one is available.
    pub generation: Option<PhysicalEventGenerationId>,
    /// Task identity available at the call boundary.
    pub task: PhysicalTaskIdentity,
    /// Physical stop that authorizes this transition, if observed.
    pub source_status: Option<PhysicalStatusId>,
    /// Exact ptrace request.
    pub operation: PhysicalResumeOperation,
    /// Signal delivered with the request, or no signal.
    pub signal: Option<i32>,
    /// Ownership path issuing the request.
    pub owner: PhysicalResumeOwner,
}

/// Token returned before an actual ptrace transition call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalResumeAttempt {
    observer: PhysicalObserverId,
    id: PhysicalResumeAttemptId,
    context: PhysicalResumeContext,
}

pub(crate) struct StartupCleanupWaitFailureExitProof {
    pub(crate) transaction: PhysicalCleanupTransaction,
    pub(crate) generation: PhysicalEventGenerationId,
    pub(crate) task: PhysicalTaskIdentity,
    pub(crate) pidfd: i32,
    pub(crate) failed_wait: PhysicalWaitAttempt,
    pub(crate) error: i32,
    pub(crate) revents: i16,
}

pub(crate) struct StartupCleanupResumeFailureExitProof {
    pub(crate) transaction: PhysicalCleanupTransaction,
    pub(crate) generation: PhysicalEventGenerationId,
    pub(crate) task: PhysicalTaskIdentity,
    pub(crate) pidfd: i32,
    pub(crate) resume: PhysicalResumeAttempt,
    pub(crate) source_status: PhysicalStatusId,
    pub(crate) error: i32,
    pub(crate) revents: i16,
}

impl PhysicalResumeAttempt {
    /// Returns this call's unique attempt ID.
    pub fn id(self) -> PhysicalResumeAttemptId {
        self.id
    }

    /// Returns the context fixed before entering the kernel.
    pub fn context(self) -> PhysicalResumeContext {
        self.context
    }
}

/// Raw result of an actual ptrace transition call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalResumeOutcome {
    /// The kernel accepted the request.
    Success,
    /// The kernel returned this errno.
    Error(i32),
}

/// Description fixed before one exact-pidfd cleanup signal syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalPidfdSignalContext {
    /// Immutable Event generation owning the pidfd.
    pub generation: PhysicalEventGenerationId,
    /// Exact task identity retained by the cleanup transaction.
    pub task: PhysicalTaskIdentity,
    /// Cleanup transaction whose terminal authority issues the signal.
    pub transaction: PhysicalCleanupTransactionId,
    /// Exact process-local pidfd used by the syscall.
    pub pidfd: i32,
    /// Signal passed to `pidfd_send_signal`.
    pub signal: i32,
}

/// Token returned before one exact-pidfd cleanup signal syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalPidfdSignalAttempt {
    observer: PhysicalObserverId,
    id: PhysicalPidfdSignalAttemptId,
    context: PhysicalPidfdSignalContext,
}

impl PhysicalPidfdSignalAttempt {
    /// Returns this call's unique attempt ID.
    pub fn id(self) -> PhysicalPidfdSignalAttemptId {
        self.id
    }

    /// Returns the context fixed before entering the kernel.
    pub fn context(self) -> PhysicalPidfdSignalContext {
        self.context
    }
}

/// Raw result of one exact-pidfd cleanup signal syscall.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalPidfdSignalOutcome {
    /// The kernel accepted the signal request.
    Success,
    /// The kernel returned this errno.
    Error(i32),
}

/// One append-only physical observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalEventRecord {
    sequence: u64,
    observer: PhysicalObserverId,
    kind: PhysicalEventRecordKind,
}

impl PhysicalEventRecord {
    /// Returns the observer-local append order.
    pub fn sequence(self) -> u64 {
        self.sequence
    }

    /// Returns the observer that owns the IDs in this record.
    pub fn observer(self) -> PhysicalObserverId {
        self.observer
    }

    /// Returns the physical transition represented by this record.
    pub fn kind(self) -> PhysicalEventRecordKind {
        self.kind
    }
}

/// Payload of an append-only physical observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalEventRecordKind {
    /// Observation was attached before this Event generation began waiting.
    GenerationAttached(PhysicalEventGenerationId),
    /// The observer was attached through the linear original-root launch API.
    OriginalRootLaunchLinked {
        /// Unique launch-link identity.
        link: PhysicalOriginalRootLaunchId,
        /// Raw immutable Event generation; adoption is not permitted.
        generation: PhysicalEventGenerationId,
        /// Direct-child identity derived from the exact `Running` value.
        task: PhysicalTaskIdentity,
        /// Spawning controller thread-group ID.
        controller_tgid: Pid,
        /// Spawning controller task ID.
        controller_tid: Pid,
        /// Sequence unique within the spawning controller process.
        controller_sequence: u64,
    },
    /// Exact fallback resources were installed before the retained startup
    /// status could be consumed by a notifier or external cleanup owner.
    StartupBarrierFallbackPrepared {
        /// Original controller-launch provenance.
        launch: PhysicalOriginalRootLaunchId,
        /// Raw immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// Retained `WNOWAIT` barrier attempt.
        barrier: PhysicalWaitAttemptId,
        /// Exact stable task retained by the fallback.
        task: PhysicalTaskIdentity,
        /// Transaction identity preallocated before the consuming wait.
        transaction: PhysicalCleanupTransactionId,
    },
    /// The authorized worker's exact first consumption released the neutral
    /// fallback without selecting cleanup.
    StartupBarrierFallbackReleased {
        /// Raw immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// Retained `WNOWAIT` barrier attempt.
        barrier: PhysicalWaitAttemptId,
        /// Exact first consuming AuthorizedRoot wait.
        consuming_wait: PhysicalWaitAttemptId,
        /// Status allocated by that consuming wait.
        status: PhysicalStatusId,
    },
    /// The controller granted this immutable original-root generation sole
    /// process-wide continued-status wait authority.
    ContinuedAuthorityEnabled {
        /// Authorized Event generation.
        generation: PhysicalEventGenerationId,
        /// Original after-loader root TID/TGID.
        root: i32,
        /// Controller process that is both real parent and tracer.
        controller_tgid: i32,
        /// Exact controller thread which owns ptrace for this root.
        controller_tracer_tid: i32,
    },
    /// The first consuming wait matched the exact retained startup barrier.
    PreRegistrationBarrierConsumed {
        /// Immutable original-root generation.
        generation: PhysicalEventGenerationId,
        /// Retained `WNOWAIT` attempt.
        barrier: PhysicalWaitAttemptId,
        /// Later consuming wait attempt.
        consuming_wait: PhysicalWaitAttemptId,
        /// Ordinary status identity allocated only by the consuming wait.
        consumed_status: PhysicalStatusId,
    },
    /// A consuming root-worker wait differed from the retained startup peek
    /// and transferred the consumed stop into exact registered cleanup.
    PreRegistrationBarrierFailureLinked {
        /// Immutable original-root generation.
        generation: PhysicalEventGenerationId,
        /// Retained `WNOWAIT` attempt.
        barrier: PhysicalWaitAttemptId,
        /// Mismatching consuming wait attempt.
        consuming_wait: PhysicalWaitAttemptId,
        /// Consumed status transferred to cleanup.
        consumed_status: PhysicalStatusId,
        /// Sole cleanup transaction owning the consumed stop.
        transaction: PhysicalCleanupTransactionId,
    },
    /// The authorized worker's first consuming wait failed before returning a
    /// status.  The retained WNOWAIT barrier and its preallocated transaction
    /// nevertheless move atomically into registered cleanup ownership.
    PreRegistrationBarrierStatuslessFailureLinked {
        /// Immutable original-root generation.
        generation: PhysicalEventGenerationId,
        /// Retained `WNOWAIT` attempt.
        barrier: PhysicalWaitAttemptId,
        /// Exact failing first AuthorizedRoot wait.
        cause_wait: PhysicalWaitAttemptId,
        /// Exact nonzero wait errno.
        error: i32,
        /// Sole preallocated cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
    },
    /// The later cleanup wait which consumed or terminally superseded a
    /// retained barrier after a statusless first AuthorizedRoot failure.
    PreRegistrationBarrierStatuslessCleanupResolved {
        /// Immutable original-root generation.
        generation: PhysicalEventGenerationId,
        /// Retained `WNOWAIT` attempt.
        barrier: PhysicalWaitAttemptId,
        /// Later exact registered-cleanup wait.
        cleanup_wait: PhysicalWaitAttemptId,
        /// Returned status, absent only for exact terminal `ECHILD` proof.
        status: Option<PhysicalStatusId>,
        /// Sole preallocated cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
    },
    /// Startup failed after an exact pidfd was retained but before the
    /// WNOWAIT barrier could be issued.
    PreRegistrationBarrierSetupFailed {
        /// Unstarted Event generation.
        generation: PhysicalEventGenerationId,
        /// Exact direct-child task bound by the retained pidfd.
        task: PhysicalTaskIdentity,
        /// Exact setup errno which forced cleanup.
        error: i32,
        /// Spawn-sourced pidfd capability retained for cleanup.
        launch: PhysicalOriginalRootLaunchId,
    },
    /// Fixed cleanup resources were prepared before the first wait for a
    /// startup failure which occurred before any retained barrier existed.
    StartupSetupCleanupPrepared {
        /// Immutable original-root generation.
        generation: PhysicalEventGenerationId,
        /// Exact pidfd-bound cleanup task.
        task: PhysicalTaskIdentity,
        /// Original setup errno.
        error: i32,
        /// Preallocated cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
        /// Spawn-sourced pidfd capability retained by the fixed owner.
        launch: PhysicalOriginalRootLaunchId,
    },
    /// The first setup-cleanup status was captured into the prepared owner.
    StartupSetupCleanupLinked {
        /// Immutable original-root generation.
        generation: PhysicalEventGenerationId,
        /// Original setup errno.
        error: i32,
        /// First consuming cleanup wait.
        consuming_wait: PhysicalWaitAttemptId,
        /// Physical status captured by that wait.
        status: PhysicalStatusId,
        /// Sole preallocated cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
        /// Spawn-sourced pidfd capability owning the consumed status.
        launch: PhysicalOriginalRootLaunchId,
    },
    /// The first setup-cleanup wait returned exact statusless `ECHILD`.
    StartupSetupCleanupNoStatusLinked {
        /// Immutable original-root generation.
        generation: PhysicalEventGenerationId,
        /// Original setup errno.
        error: i32,
        /// Exact statusless consuming wait.
        consuming_wait: PhysicalWaitAttemptId,
        /// Sole preallocated cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
        /// Spawn-sourced pidfd capability used for the terminal proof.
        launch: PhysicalOriginalRootLaunchId,
    },
    /// A controller armed stop resolution from the exact SIGSTOP delivery.
    StopResolutionWatchArmed {
        /// Immutable authorized-root generation.
        generation: PhysicalEventGenerationId,
        /// Delivery-stop status identity D.
        delivery: PhysicalStatusId,
    },
    /// The notifier published the first stopped candidate after D.
    StopResolutionFirstStopped {
        /// Immutable authorized-root generation.
        generation: PhysicalEventGenerationId,
        /// Delivery-stop status identity D.
        delivery: PhysicalStatusId,
        /// First later stopped status identity.
        stopped: PhysicalStatusId,
    },
    /// Exact GETSIGINFO EINVAL authentication acknowledged candidate G.
    StopResolutionGroupAcknowledged {
        /// Immutable authorized-root generation.
        generation: PhysicalEventGenerationId,
        /// Delivery-stop status identity D.
        delivery: PhysicalStatusId,
        /// Authenticated group-stop status identity G.
        group_stop: PhysicalStatusId,
    },
    /// The controller linearly claimed the one C following authenticated G.
    StopResolutionContinuedClaimed {
        /// Immutable authorized-root generation.
        generation: PhysicalEventGenerationId,
        /// Authenticated group-stop status identity G.
        group_stop: PhysicalStatusId,
        /// Claimed continued-status identity C.
        continued: PhysicalStatusId,
    },
    /// The controller closed one armed stop-resolution transaction.
    StopResolutionWatchClosed {
        /// Immutable authorized-root generation.
        generation: PhysicalEventGenerationId,
        /// Delivery-stop status identity D.
        delivery: PhysicalStatusId,
    },
    /// Failed resume A was causally resolved by later FIFO status S and the
    /// exact D/G/C transaction was retired.
    StopResolutionResumeCausallyClosed {
        /// Immutable authorized-root generation.
        generation: PhysicalEventGenerationId,
        /// Delivery status D.
        delivery: PhysicalStatusId,
        /// Authenticated group stop G.
        group_stop: PhysicalStatusId,
        /// Diverted continued status C.
        continued: PhysicalStatusId,
        /// Exact failed resume attempt A.
        attempt: PhysicalResumeAttemptId,
        /// Trusted later regular-FIFO status S.
        successor: PhysicalStatusId,
    },
    /// New-child publication permanently revoked future watches for the
    /// original-root authority.
    ContinuedAuthorityRevoked {
        /// Revoked Event generation.
        generation: PhysicalEventGenerationId,
    },
    /// An exact nonblocking `WCONTINUED` drain reached its zero-siginfo
    /// barrier before a plain SIGSTOP delivery candidate was published.
    PreStopContinuedDrainCompleted {
        /// Original-root Event generation owning the process-wide drain.
        generation: PhysicalEventGenerationId,
        /// Already allocated identity of the fenced plain SIGSTOP status.
        stopped: PhysicalStatusId,
        /// Exact final no-status wait attempt establishing the drain barrier.
        final_no_status_attempt: PhysicalWaitAttemptId,
    },
    /// A plain-SIGSTOP stale-C drain failed and transferred the exact stopped
    /// status into one registered-cleanup transaction.
    PreStopContinuedDrainFailed {
        /// Original-root Event generation owning the failed drain.
        generation: PhysicalEventGenerationId,
        /// Exact authorized-root task used by the drain wait.
        task: PhysicalTaskIdentity,
        /// Plain SIGSTOP status D withheld from normal FIFO publication.
        stopped: PhysicalStatusId,
        /// Exact fatal drain wait attempt.
        cause_wait: PhysicalWaitAttemptId,
        /// Sole registered-cleanup transaction owning D.
        transaction: PhysicalCleanupTransactionId,
    },
    /// A requested Event generation adopted an already authoritative one.
    GenerationAdopted {
        /// Requested generation.
        from: PhysicalEventGenerationId,
        /// Authoritative generation.
        to: PhysicalEventGenerationId,
    },
    /// A direct-child launch identity transferred to its immutable Event.
    PreRegistrationLinked {
        /// Narrow direct-child identity established at launch.
        task: PhysicalTaskIdentity,
        /// Event generation that assumed notifier ownership.
        generation: PhysicalEventGenerationId,
    },
    /// Exact procfs/pidfd identity became available for a generation.
    IdentityBound {
        /// Immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// Captured kernel task identity.
        task: PhysicalTaskIdentity,
    },
    /// Exact identity capture failed because the observed task disappeared.
    GenerationCaptureFailed {
        /// Event generation whose capture failed.
        generation: PhysicalEventGenerationId,
        /// Narrow task identity used for the failed capture.
        task: PhysicalTaskIdentity,
        /// Exact procfs/pidfd capture errno.
        error: i32,
    },
    /// A previously bound identity no longer matched a fresh capture.
    GenerationIdentityMismatch {
        /// Event generation whose bound identity became stale.
        generation: PhysicalEventGenerationId,
        /// Exact identity already bound to the Event.
        bound: PhysicalTaskIdentity,
        /// Fresh identity captured for the same numeric TID.
        current: PhysicalTaskIdentity,
    },
    /// The pidfd in the identity already bound to an Event was no longer live.
    GenerationBoundPidfdDead {
        /// Event generation whose bound pidfd returned `ESRCH`.
        generation: PhysicalEventGenerationId,
        /// Exact bound identity containing the probed pidfd.
        bound: PhysicalTaskIdentity,
    },
    /// The pidfd in a fresh identity capture was no longer live.
    GenerationCurrentPidfdDead {
        /// Event generation whose fresh pidfd returned `ESRCH`.
        generation: PhysicalEventGenerationId,
        /// Fresh identity containing the probed pidfd.
        current: PhysicalTaskIdentity,
    },
    /// Registry state for the same numeric PID named a different generation.
    GenerationRegistryMismatch {
        /// Requested Event generation invalidated by the registry conflict.
        generation: PhysicalEventGenerationId,
        /// Identity retained in the PID registry.
        registered: PhysicalTaskIdentity,
        /// Fresh identity bound to the requested Event.
        current: PhysicalTaskIdentity,
    },
    /// A direct pre-registration liveness probe proved the task disappeared.
    PreRegistrationTaskGone {
        /// Event generation whose cleanup wait returned `ECHILD`.
        generation: PhysicalEventGenerationId,
        /// Exact direct-child or captured identity probed by the caller.
        task: PhysicalTaskIdentity,
        /// Exact liveness-probe errno; production requires `ESRCH`.
        error: i32,
    },
    /// The per-task notifier worker began owning kernel waits.
    NotifierWorkerStarted(PhysicalEventGenerationId),
    /// The generation reached terminal notifier or external completion.
    NotifierGenerationFinished {
        /// Completed immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// External wait that proved completion, absent for notifier-owned completion.
        external_wait: Option<PhysicalWaitAttemptId>,
    },
    /// Caller bound an exec generation to an immutable Event generation.
    ExecGenerationBound {
        /// Immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// Caller-defined monotonic exec generation.
        exec_generation: u64,
    },
    /// A kernel wait call is about to be attempted.
    WaitAttempt {
        /// Unique call identity.
        id: PhysicalWaitAttemptId,
        /// Context fixed before the syscall.
        context: PhysicalWaitContext,
    },
    /// A successful `waitid` returned raw siginfo before status conversion.
    WaitSiginfoReturned {
        /// Call identity created by `WaitAttempt`.
        attempt: PhysicalWaitAttemptId,
        /// Raw fields preserved before any conversion panic.
        siginfo: PhysicalWaitSiginfo,
        /// Identity allocated at the kernel boundary for a real status.
        status: Option<PhysicalStatusId>,
    },
    /// Raw result of a kernel wait call.
    WaitResult {
        /// Call identity created by `WaitAttempt`.
        attempt: PhysicalWaitAttemptId,
        /// Kernel outcome.
        outcome: PhysicalWaitOutcome,
    },
    /// Registered cleanup began an allocation-free terminal-drain transaction.
    RegisteredCleanupTransactionStarted {
        /// Globally unique transaction identity.
        transaction: PhysicalCleanupTransactionId,
        /// Exact fatal wait result that caused fail-closed cleanup.
        cause_wait: PhysicalWaitAttemptId,
        /// Typed transaction owner; wait producer identities remain physical.
        kind: PhysicalCleanupTransactionKind,
    },
    /// One status became owned by a registered-cleanup transaction.
    RegisteredCleanupStatusLinked {
        /// Transaction owning the cleanup proof.
        transaction: PhysicalCleanupTransactionId,
        /// Exact physical status closed by the transaction.
        status: PhysicalStatusId,
    },
    /// One tolerated cleanup resume became owned by a transaction.
    RegisteredCleanupToleratedResumeLinked {
        /// Transaction owning the cleanup proof.
        transaction: PhysicalCleanupTransactionId,
        /// Exact resume attempt whose error was tolerated.
        resume: PhysicalResumeAttemptId,
    },
    /// A zero-timeout poll proved the exact registered-cleanup pidfd exited.
    RegisteredCleanupPidfdExited {
        /// Transaction whose `ECHILD` terminal wait required liveness proof.
        transaction: PhysicalCleanupTransactionId,
        /// Exact registered-cleanup wait that returned `ECHILD`.
        terminal_wait: PhysicalWaitAttemptId,
        /// Immutable Event generation owning the wait and pidfd.
        generation: PhysicalEventGenerationId,
        /// Exact captured identity containing the polled pidfd.
        task: PhysicalTaskIdentity,
        /// Lossless `poll(2)` result bits; `POLLIN` proves exit.
        revents: i16,
        /// Exact spawn-sourced pidfd capability, for original-root cleanup.
        launch: Option<PhysicalOriginalRootLaunchId>,
    },
    /// A bound pidfd proved a notifier/synchronous `ECHILD` task exited.
    EchildPidfdExited {
        /// Exact wait attempt that returned `ECHILD`.
        wait: PhysicalWaitAttemptId,
        /// Immutable Event generation owning the wait and pidfd.
        generation: PhysicalEventGenerationId,
        /// Exact captured identity containing the polled pidfd.
        task: PhysicalTaskIdentity,
        /// Lossless zero-timeout `poll(2)` result bits.
        revents: i16,
    },
    /// A fresh same-generation proc snapshot proved no local tracer owns it.
    EchildTracerDetached {
        /// Exact wait attempt that returned `ECHILD`.
        wait: PhysicalWaitAttemptId,
        /// Immutable Event generation owning the wait and proc identity.
        generation: PhysicalEventGenerationId,
        /// Exact captured identity revalidated by the proc snapshot.
        task: PhysicalTaskIdentity,
        /// Fresh `TracerPid`; zero means detached, positive means an absent
        /// non-local tracer task.
        observed_tracer_pid: i32,
    },
    /// Exact registered-cleanup wait completed a transaction.
    RegisteredCleanupTransactionCompleted {
        /// Transaction whose linked evidence is complete.
        transaction: PhysicalCleanupTransactionId,
        /// Exact registered-cleanup wait proving terminal drain.
        terminal_wait: PhysicalWaitAttemptId,
    },
    /// One physical status was published into existing ownership state.
    StatusPublished {
        /// Physical status identity.
        status: PhysicalStatusId,
        /// Event generation that owns publication, absent before registration.
        generation: Option<PhysicalEventGenerationId>,
        /// Existing retained location.
        destination: PhysicalStatusPublication,
    },
    /// Synthetic terminal publication, distinct from a physical exit status.
    SyntheticEchildPublished {
        /// Event generation receiving the terminal marker.
        generation: PhysicalEventGenerationId,
        /// Actual wait attempt that returned `ECHILD`, when one caused it.
        cause: Option<PhysicalWaitAttemptId>,
    },
    /// A consumer reserved a physical status.
    StatusReserved {
        /// Reservation identity.
        reservation: PhysicalReservationId,
        /// Reserved physical status.
        status: PhysicalStatusId,
        /// Event generation owning the status.
        generation: PhysicalEventGenerationId,
    },
    /// A reservation began fallible typed decoding.
    DecodeStarted {
        /// Reservation identity.
        reservation: PhysicalReservationId,
        /// Physical status being decoded.
        status: PhysicalStatusId,
        /// Existing owner performing the decode.
        owner: PhysicalDecodeOwner,
    },
    /// A decode transaction ended or transferred to cancellation.
    DecodeFinished {
        /// Reservation identity.
        reservation: PhysicalReservationId,
        /// Physical status being decoded.
        status: PhysicalStatusId,
        /// Exact outcome preserving the async/sync distinction.
        outcome: PhysicalDecodeOutcome,
        /// Existing owner performing the decode.
        owner: PhysicalDecodeOwner,
    },
    /// A reservation was removed from the FIFO.
    ReservationCommitted {
        /// Reservation identity.
        reservation: PhysicalReservationId,
        /// Physical status removed or released.
        status: PhysicalStatusId,
    },
    /// Drop restored a reservation without changing FIFO order.
    ReservationRolledBack {
        /// Reservation identity.
        reservation: PhysicalReservationId,
        /// Physical status retained.
        status: PhysicalStatusId,
    },
    /// A retained terminal status was returned without another kernel wait.
    TerminalReplayed {
        /// Reservation identity used for the replay.
        reservation: PhysicalReservationId,
        /// Original physical status.
        status: PhysicalStatusId,
    },
    /// Exit-stop capability ownership changed.
    ExitCapability {
        /// Original physical exit-stop status.
        status: PhysicalStatusId,
        /// Capability transition.
        transition: PhysicalExitCapabilityTransition,
    },
    /// A ptrace transition call is about to be attempted.
    ResumeAttempt {
        /// Unique call identity.
        id: PhysicalResumeAttemptId,
        /// Context fixed before the syscall.
        context: PhysicalResumeContext,
    },
    /// Raw result of a ptrace transition call.
    ResumeResult {
        /// Attempt identity.
        attempt: PhysicalResumeAttemptId,
        /// Kernel result before error conversion.
        outcome: PhysicalResumeOutcome,
    },
    /// A generation-bound cleanup `pidfd_send_signal` is about to be issued.
    PidfdSignalAttempt {
        /// Unique call identity.
        id: PhysicalPidfdSignalAttemptId,
        /// Exact transaction, task, descriptor, and signal.
        context: PhysicalPidfdSignalContext,
    },
    /// Raw result of one cleanup `pidfd_send_signal` call.
    PidfdSignalResult {
        /// Attempt identity.
        attempt: PhysicalPidfdSignalAttemptId,
        /// Kernel result before errno classification.
        outcome: PhysicalPidfdSignalOutcome,
    },
    /// A nonblocking poll proved the exact startup-cleanup pidfd exited after
    /// a definitive `pidfd_send_signal` error.
    StartupCleanupPidfdExitProved {
        /// Cleanup transaction retaining terminal authority.
        transaction: PhysicalCleanupTransactionId,
        /// Immutable Event generation owning the pidfd.
        generation: PhysicalEventGenerationId,
        /// Exact task identity containing the pidfd.
        task: PhysicalTaskIdentity,
        /// Exact process-local descriptor passed to poll.
        pidfd: i32,
        /// Lossless poll result; `POLLIN` proves final exit.
        revents: i16,
    },
    /// A non-retried registered-cleanup wait failure was followed by exact
    /// pidfd terminal readability before a later terminal wait.
    StartupCleanupWaitFailureExitProved {
        /// Sole startup cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
        /// Immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// Exact cleanup task.
        task: PhysicalTaskIdentity,
        /// Exact pidfd used for the proof.
        pidfd: i32,
        /// Failed wait which is never replayed.
        failed_wait: PhysicalWaitAttemptId,
        /// Exact nonzero wait errno.
        error: i32,
        /// Poll readiness including `POLLIN`.
        revents: i16,
    },
    /// A failed one-shot startup-cleanup resume was followed by exact pidfd
    /// readability, proving that terminal drain may continue without another
    /// resume or signal attempt.
    StartupCleanupResumeFailureExitProved {
        /// Sole startup cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
        /// Immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// Exact cleanup task containing the pidfd.
        task: PhysicalTaskIdentity,
        /// Exact pidfd used by the zero-time poll.
        pidfd: i32,
        /// Spent physical resume attempt.
        resume: PhysicalResumeAttemptId,
        /// Physical stopped status named by the attempt.
        source_status: PhysicalStatusId,
        /// Exact nonzero resume errno.
        error: i32,
        /// Poll readiness including `POLLIN`.
        revents: i16,
    },
    /// The notifier worker transferred a fixed startup transaction to the
    /// original controller before publishing its terminal-error wake.
    StartupCleanupExecutorTransferred {
        /// Sole startup cleanup transaction.
        transaction: PhysicalCleanupTransactionId,
        /// Immutable Event generation.
        generation: PhysicalEventGenerationId,
        /// Exact controller-traced task.
        task: PhysicalTaskIdentity,
    },
    /// Cleanup deliberately tolerated a raw ptrace error after recording it.
    ResumeErrorTolerated {
        /// Attempt whose error was tolerated.
        attempt: PhysicalResumeAttemptId,
        /// Exact tolerated errno.
        errno: i32,
    },
    /// Explicit final disposition not represented by successful resume.
    StatusDisposition {
        /// Physical status identity.
        status: PhysicalStatusId,
        /// Final disposition.
        disposition: PhysicalStatusDisposition,
    },
    /// Caller declared whole-session observation complete.
    ObserverClosed,
}

/// State transition of the separately retained exit-stop capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalExitCapabilityTransition {
    /// Exit stop and its capability were published.
    Published,
    /// Exactly one `ExitFuture` claimed the capability.
    Claimed,
    /// Terminal cleanup expired an unclaimed capability.
    Expired,
    /// Cancellation revoked an unclaimed capability.
    Revoked,
    /// Cancellation took ownership of a previously claimed capability.
    TransferredToCleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordClass {
    Ordinary,
    Cleanup,
}

#[derive(Debug)]
struct RecordSlot {
    ready: AtomicBool,
    record: UnsafeCell<MaybeUninit<PhysicalEventRecord>>,
}

// A slot has one writer selected by fetch_add. The writer publishes with
// Release only after the record is fully initialized; readers require Acquire.
unsafe impl Sync for RecordSlot {}

impl RecordSlot {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            record: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

#[derive(Debug)]
struct RecordBuffer {
    slots: Box<[RecordSlot]>,
    claimed: AtomicUsize,
    lost: AtomicU64,
}

impl RecordBuffer {
    fn new(capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, RecordSlot::new);
        Self {
            slots: slots.into_boxed_slice(),
            claimed: AtomicUsize::new(0),
            lost: AtomicU64::new(0),
        }
    }

    fn push(&self, record: PhysicalEventRecord) -> bool {
        let slot = self.claimed.fetch_add(1, Ordering::Relaxed);
        let Some(destination) = self.slots.get(slot) else {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        unsafe {
            (*destination.record.get()).write(record);
        }
        destination.ready.store(true, Ordering::Release);
        true
    }

    fn reserve(&self, count: usize) -> Option<usize> {
        let mut claimed = self.claimed.load(Ordering::Acquire);
        loop {
            let end = claimed.checked_add(count)?;
            if end > self.slots.len() {
                return None;
            }
            match self.claimed.compare_exchange_weak(
                claimed,
                end,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(claimed),
                Err(current) => claimed = current,
            }
        }
    }

    fn push_reserved(&self, slot: usize, record: PhysicalEventRecord) {
        let destination = self
            .slots
            .get(slot)
            .expect("pre-reserved physical observer slot disappeared");
        debug_assert!(!destination.ready.load(Ordering::Acquire));
        unsafe {
            (*destination.record.get()).write(record);
        }
        destination.ready.store(true, Ordering::Release);
    }

    fn release_reserved_tail(&self, first: usize, count: usize) -> bool {
        self.claimed
            .compare_exchange(first + count, first, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn snapshot(&self, output: &mut Vec<PhysicalEventRecord>) -> u64 {
        let claimed = self.claimed.load(Ordering::Acquire).min(self.slots.len());
        for slot in &self.slots[..claimed] {
            if slot.ready.load(Ordering::Acquire) {
                output.push(unsafe { (*slot.record.get()).assume_init() });
            }
        }
        self.lost.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct PhysicalObserverInner {
    id: PhysicalObserverId,
    lifecycle: Mutex<()>,
    original_root_launch: AtomicU64,
    ordinary: RecordBuffer,
    cleanup: RecordBuffer,
    next_sequence: AtomicU64,
    active_writers: AtomicUsize,
    #[cfg(test)]
    pause_after_writer_registration: AtomicBool,
    #[cfg(test)]
    writer_registration_paused: AtomicBool,
    state: AtomicU8,
    after_close: AtomicU64,
    sticky_failure: AtomicBool,
}

/// Cloneable handle to one bounded observation session.
#[derive(Clone, Debug)]
pub struct PhysicalEventObserver {
    inner: Arc<PhysicalObserverInner>,
}

/// Uncommitted observer-global claim for the sole original-root launch.
///
/// This private linear reservation precedes Event attachment. Dropping it
/// rolls back only its own still-uncommitted claim.
#[derive(Debug)]
struct OriginalRootLaunchReservation {
    observer: PhysicalEventObserver,
    link: Option<PhysicalOriginalRootLaunchId>,
    ordinary_slot: usize,
}

impl OriginalRootLaunchReservation {
    fn commit(
        mut self,
        task: PhysicalTaskIdentity,
        generation: PhysicalEventGenerationId,
        controller_launch: ControllerLaunchId,
        install_physical_link: impl FnOnce(PhysicalOriginalRootLaunchId),
    ) -> OriginalRootLaunchToken {
        let link = self
            .link
            .take()
            .expect("original-root launch reservation was already committed");
        let generation_record = PhysicalEventRecord {
            sequence: next_nonzero(&self.observer.inner.next_sequence),
            observer: self.observer.id(),
            kind: PhysicalEventRecordKind::GenerationAttached(generation),
        };
        let launch_record = PhysicalEventRecord {
            sequence: next_nonzero(&self.observer.inner.next_sequence),
            observer: self.observer.id(),
            kind: PhysicalEventRecordKind::OriginalRootLaunchLinked {
                link,
                generation,
                task,
                controller_tgid: controller_launch.controller_tgid(),
                controller_tid: controller_launch.controller_tid(),
                controller_sequence: controller_launch.sequence(),
            },
        };
        self.observer
            .inner
            .ordinary
            .push_reserved(self.ordinary_slot, generation_record);
        self.observer
            .inner
            .ordinary
            .push_reserved(self.ordinary_slot + 1, launch_record);
        install_physical_link(link);
        OriginalRootLaunchToken {
            observer: self.observer.id(),
            link,
            generation,
            task,
            controller_launch,
        }
    }
}

impl Drop for OriginalRootLaunchReservation {
    fn drop(&mut self) {
        if let Some(link) = self.link {
            let link_rolled_back = self.observer.inner.original_root_launch.compare_exchange(
                link.get(),
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            let slots_rolled_back = self
                .observer
                .inner
                .ordinary
                .release_reserved_tail(self.ordinary_slot, 2);
            if link_rolled_back.is_err() || !slots_rolled_back {
                self.observer
                    .inner
                    .sticky_failure
                    .store(true, Ordering::Release);
            }
        }
    }
}

impl PhysicalEventObserver {
    /// Allocates all record storage before the observer can be attached.
    pub fn new(config: PhysicalEventObserverConfig) -> Result<Self, PhysicalObserverCreateError> {
        if config.ordinary_capacity == 0 || config.cleanup_capacity == 0 {
            return Err(PhysicalObserverCreateError::ZeroCapacity);
        }
        Ok(Self {
            inner: Arc::new(PhysicalObserverInner {
                id: PhysicalObserverId(next_nonzero(&NEXT_OBSERVER_ID)),
                lifecycle: Mutex::new(()),
                original_root_launch: AtomicU64::new(0),
                ordinary: RecordBuffer::new(config.ordinary_capacity),
                cleanup: RecordBuffer::new(config.cleanup_capacity),
                next_sequence: AtomicU64::new(1),
                active_writers: AtomicUsize::new(0),
                #[cfg(test)]
                pause_after_writer_registration: AtomicBool::new(false),
                #[cfg(test)]
                writer_registration_paused: AtomicBool::new(false),
                state: AtomicU8::new(OBSERVER_OPEN),
                after_close: AtomicU64::new(0),
                sticky_failure: AtomicBool::new(false),
            }),
        })
    }

    /// Returns the identity shared by all clones of this observer.
    pub fn id(&self) -> PhysicalObserverId {
        self.inner.id
    }

    /// Returns true when both handles refer to the same observer session.
    pub fn same_observer(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Returns whether overflow, an after-close write, or API misuse occurred.
    pub fn failed(&self) -> bool {
        self.inner.sticky_failure.load(Ordering::Acquire)
    }

    pub(crate) fn is_open(&self) -> bool {
        self.inner.state.load(Ordering::Acquire) == OBSERVER_OPEN
    }

    /// Records the separate exec generation selected by the ptracer.
    pub fn bind_exec_generation(
        &self,
        generation: PhysicalEventGenerationId,
        exec_generation: u64,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::ExecGenerationBound {
                generation,
                exec_generation,
            },
        );
    }

    /// Links pre-notifier direct-child ownership to the installed generation.
    pub fn link_pre_registration_task(
        &self,
        task: PhysicalTaskIdentity,
        generation: PhysicalEventGenerationId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::PreRegistrationLinked { task, generation },
        );
    }

    pub(crate) fn attach_original_root_launch(
        &self,
        task: PhysicalTaskIdentity,
        generation: PhysicalEventGenerationId,
        controller_launch: ControllerLaunchId,
        attach_event: impl FnOnce(),
        install_physical_link: impl FnOnce(PhysicalOriginalRootLaunchId),
    ) -> Result<OriginalRootLaunchToken, PhysicalObserverAttachError> {
        // close() takes the same lifecycle lock before OPEN -> CLOSING.  This
        // guard therefore keeps the observer OPEN through Event attachment,
        // both pre-reserved records, and physical-link installation.
        let _lifecycle = self.inner.lifecycle.lock();
        if self.inner.state.load(Ordering::SeqCst) != OBSERVER_OPEN {
            return Err(PhysicalObserverAttachError::ObserverClosed);
        }
        let link = PhysicalOriginalRootLaunchId(next_nonzero(&NEXT_ORIGINAL_ROOT_LAUNCH_ID));
        if self
            .inner
            .original_root_launch
            .compare_exchange(0, link.get(), Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(PhysicalObserverAttachError::InvalidOriginalRootLaunch);
        }
        let Some(ordinary_slot) = self.inner.ordinary.reserve(2) else {
            let rollback = self.inner.original_root_launch.compare_exchange(
                link.get(),
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            debug_assert!(rollback.is_ok());
            return Err(PhysicalObserverAttachError::InsufficientCapacity);
        };
        let reservation = OriginalRootLaunchReservation {
            observer: self.clone(),
            link: Some(link),
            ordinary_slot,
        };
        // The caller established every fallible Event/token precondition
        // before entering this helper.  From here through physical-link
        // installation the private lifecycle lock makes the transaction
        // non-escapable and close cannot advance to CLOSING.
        attach_event();
        Ok(reservation.commit(task, generation, controller_launch, install_physical_link))
    }

    pub(crate) fn record_startup_barrier_fallback_prepared(
        &self,
        launch: PhysicalOriginalRootLaunchId,
        generation: PhysicalEventGenerationId,
        barrier: PhysicalWaitAttempt,
        task: PhysicalTaskIdentity,
        transaction: PhysicalCleanupTransaction,
    ) {
        self.record_checked(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StartupBarrierFallbackPrepared {
                launch,
                generation,
                barrier: barrier.id,
                task,
                transaction: transaction.id,
            },
            &[barrier.observer, transaction.observer],
        );
    }

    pub(crate) fn record_startup_barrier_fallback_released(
        &self,
        generation: PhysicalEventGenerationId,
        barrier: PhysicalWaitAttempt,
        consuming_wait: PhysicalWaitAttempt,
        status: PhysicalStatusId,
    ) {
        self.record_checked(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StartupBarrierFallbackReleased {
                generation,
                barrier: barrier.id,
                consuming_wait: consuming_wait.id,
                status,
            },
            &[barrier.observer, consuming_wait.observer],
        );
    }

    /// Records completion of a generation proven by an external cleanup wait.
    pub fn finish_unregistered_generation(
        &self,
        generation: PhysicalEventGenerationId,
        terminal_wait: PhysicalWaitAttemptId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::NotifierGenerationFinished {
                generation,
                external_wait: Some(terminal_wait),
            },
        );
    }

    /// Begins an allocation-free registered-cleanup terminal-drain transaction.
    pub(crate) fn begin_registered_cleanup_transaction(
        &self,
        cause_wait: PhysicalWaitAttempt,
    ) -> PhysicalCleanupTransaction {
        let transaction = PhysicalCleanupTransaction {
            observer: self.id(),
            id: PhysicalCleanupTransactionId(next_nonzero(&NEXT_CLEANUP_TRANSACTION_ID)),
        };
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::RegisteredCleanupTransactionStarted {
                transaction: transaction.id,
                cause_wait: cause_wait.id,
                kind: PhysicalCleanupTransactionKind::Registered,
            },
            &[cause_wait.observer],
        );
        transaction
    }

    pub(crate) fn prepare_startup_barrier_cleanup_transaction(&self) -> PhysicalCleanupTransaction {
        PhysicalCleanupTransaction {
            observer: self.id(),
            id: PhysicalCleanupTransactionId(next_nonzero(&NEXT_CLEANUP_TRANSACTION_ID)),
        }
    }

    pub(crate) fn record_startup_setup_cleanup_prepared(
        &self,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        error: i32,
        transaction: PhysicalCleanupTransaction,
        launch: PhysicalOriginalRootLaunchId,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StartupSetupCleanupPrepared {
                generation,
                task,
                error,
                transaction: transaction.id,
                launch,
            },
            &[transaction.observer],
        );
    }

    pub(crate) fn begin_startup_setup_cleanup_transaction(
        &self,
        transaction: PhysicalCleanupTransaction,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        error: i32,
        consuming_wait: PhysicalWaitAttempt,
        launch: PhysicalOriginalRootLaunchId,
    ) -> PhysicalCleanupTransaction {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::RegisteredCleanupTransactionStarted {
                transaction: transaction.id,
                cause_wait: consuming_wait.id,
                kind: PhysicalCleanupTransactionKind::StartupSetup {
                    generation,
                    task,
                    error,
                    launch,
                },
            },
            &[transaction.observer, consuming_wait.observer],
        );
        transaction
    }

    pub(crate) fn record_startup_setup_cleanup_linked(
        &self,
        generation: PhysicalEventGenerationId,
        error: i32,
        consuming_wait: PhysicalWaitAttempt,
        status: PhysicalStatusId,
        transaction: PhysicalCleanupTransaction,
        launch: PhysicalOriginalRootLaunchId,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StartupSetupCleanupLinked {
                generation,
                error,
                consuming_wait: consuming_wait.id,
                status,
                transaction: transaction.id,
                launch,
            },
            &[consuming_wait.observer, transaction.observer],
        );
    }

    pub(crate) fn record_startup_setup_cleanup_no_status_linked(
        &self,
        generation: PhysicalEventGenerationId,
        error: i32,
        consuming_wait: PhysicalWaitAttempt,
        transaction: PhysicalCleanupTransaction,
        launch: PhysicalOriginalRootLaunchId,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StartupSetupCleanupNoStatusLinked {
                generation,
                error,
                consuming_wait: consuming_wait.id,
                transaction: transaction.id,
                launch,
            },
            &[consuming_wait.observer, transaction.observer],
        );
    }

    /// Begins the sole cleanup transaction for a retained original-root
    /// startup barrier.  The consuming wait remains physically attributed to
    /// `PreRegistrationBarrierCleanup`; this typed kind carries ownership.
    pub(crate) fn begin_startup_barrier_cleanup_transaction(
        &self,
        transaction: PhysicalCleanupTransaction,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        barrier: PhysicalWaitAttempt,
        consuming_wait: PhysicalWaitAttempt,
        owner: PhysicalStartupCleanupOwner,
    ) -> PhysicalCleanupTransaction {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::RegisteredCleanupTransactionStarted {
                transaction: transaction.id,
                cause_wait: consuming_wait.id,
                kind: PhysicalCleanupTransactionKind::StartupBarrier {
                    generation,
                    barrier: barrier.id,
                    task,
                    owner,
                },
            },
            &[
                transaction.observer,
                barrier.observer,
                consuming_wait.observer,
            ],
        );
        transaction
    }

    /// Links one exact physical status to a registered-cleanup transaction.
    pub(crate) fn link_registered_cleanup_status(
        &self,
        transaction: PhysicalCleanupTransaction,
        status: PhysicalStatusId,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::RegisteredCleanupStatusLinked {
                transaction: transaction.id,
                status,
            },
            &[transaction.observer],
        );
    }

    /// Links one exact tolerated resume error to a cleanup transaction.
    pub(crate) fn link_registered_cleanup_tolerated_resume(
        &self,
        transaction: PhysicalCleanupTransaction,
        resume: PhysicalResumeAttempt,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::RegisteredCleanupToleratedResumeLinked {
                transaction: transaction.id,
                resume: resume.id,
            },
            &[transaction.observer, resume.observer],
        );
    }

    /// Records that the exact registered-cleanup pidfd reported `POLLIN`.
    ///
    /// The caller must use a zero-timeout poll after `terminal_wait` returned
    /// `ECHILD`. Validation binds the captured pidfd identity, wait,
    /// generation, and transaction and rejects any non-`POLLIN` result.
    pub(crate) fn record_registered_cleanup_pidfd_exited(
        &self,
        transaction: PhysicalCleanupTransaction,
        terminal_wait: PhysicalWaitAttempt,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        revents: i16,
        launch: Option<PhysicalOriginalRootLaunchId>,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::RegisteredCleanupPidfdExited {
                transaction: transaction.id,
                terminal_wait: terminal_wait.id,
                generation,
                task,
                revents,
                launch,
            },
            &[transaction.observer, terminal_wait.observer],
        );
    }

    pub(crate) fn record_startup_cleanup_pidfd_exit_proved(
        &self,
        transaction: PhysicalCleanupTransaction,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        pidfd: i32,
        revents: i16,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StartupCleanupPidfdExitProved {
                transaction: transaction.id,
                generation,
                task,
                pidfd,
                revents,
            },
            &[transaction.observer],
        );
    }

    pub(crate) fn record_startup_cleanup_wait_failure_exit_proved(
        &self,
        proof: StartupCleanupWaitFailureExitProof,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StartupCleanupWaitFailureExitProved {
                transaction: proof.transaction.id,
                generation: proof.generation,
                task: proof.task,
                pidfd: proof.pidfd,
                failed_wait: proof.failed_wait.id,
                error: proof.error,
                revents: proof.revents,
            },
            &[proof.transaction.observer, proof.failed_wait.observer],
        );
    }

    pub(crate) fn record_startup_cleanup_resume_failure_exit_proved(
        &self,
        proof: StartupCleanupResumeFailureExitProof,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StartupCleanupResumeFailureExitProved {
                transaction: proof.transaction.id,
                generation: proof.generation,
                task: proof.task,
                pidfd: proof.pidfd,
                resume: proof.resume.id,
                source_status: proof.source_status,
                error: proof.error,
                revents: proof.revents,
            },
            &[proof.transaction.observer, proof.resume.observer],
        );
    }

    pub(crate) fn record_startup_cleanup_executor_transferred(
        &self,
        transaction: PhysicalCleanupTransaction,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StartupCleanupExecutorTransferred {
                transaction: transaction.id,
                generation,
                task,
            },
            &[transaction.observer],
        );
    }

    /// Records a zero-timeout `POLLIN` proof for notifier/synchronous ECHILD.
    pub(crate) fn record_echild_pidfd_exited(
        &self,
        wait: PhysicalWaitAttempt,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        revents: i16,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::EchildPidfdExited {
                wait: wait.id,
                generation,
                task,
                revents,
            },
            &[wait.observer],
        );
    }

    /// Records fresh same-generation evidence that no local tracer owns a task.
    ///
    /// The caller must revalidate the captured proc identity and prove a
    /// positive `observed_tracer_pid` absent from `/proc/self/task`; zero is an
    /// explicitly detached task. Indeterminate procfs results must not call
    /// this method.
    pub(crate) fn record_echild_tracer_detached(
        &self,
        wait: PhysicalWaitAttempt,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        observed_tracer_pid: i32,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::EchildTracerDetached {
                wait: wait.id,
                generation,
                task,
                observed_tracer_pid,
            },
            &[wait.observer],
        );
    }

    /// Completes a cleanup transaction with an exact registered terminal wait.
    pub(crate) fn finish_registered_cleanup_transaction(
        &self,
        transaction: PhysicalCleanupTransaction,
        terminal_wait: PhysicalWaitAttempt,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::RegisteredCleanupTransactionCompleted {
                transaction: transaction.id,
                terminal_wait: terminal_wait.id,
            },
            &[transaction.observer, terminal_wait.observer],
        );
    }

    /// Begins an actual wait call, including an external cleanup wait.
    pub fn begin_wait(&self, context: PhysicalWaitContext) -> PhysicalWaitAttempt {
        let id = PhysicalWaitAttemptId(next_nonzero(&NEXT_WAIT_ATTEMPT_ID));
        let attempt = PhysicalWaitAttempt {
            observer: self.id(),
            id,
            context,
        };
        self.record(
            if context.producer.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::WaitAttempt { id, context },
        );
        attempt
    }

    /// Finishes a wait that returned one real physical status.
    pub fn finish_wait_status(
        &self,
        attempt: PhysicalWaitAttempt,
        raw_status: i32,
        siginfo: Option<PhysicalWaitSiginfo>,
    ) -> PhysicalStatusId {
        let id = self.allocate_status();
        // Preserve the public wrapper's historical validation of the token in
        // addition to the explicit-ID recorder's validation, but publish both
        // mismatch checks inside one registered writer transaction.
        self.finish_wait_status_with_observers(
            attempt,
            id,
            raw_status,
            siginfo,
            &[attempt.observer, attempt.observer],
        );
        id
    }

    /// Finishes the original-root `WNOWAIT` barrier without allocating an
    /// ordinary physical status identity. The retained result must later be
    /// linked one-to-one to its consuming wait.
    pub(crate) fn finish_wait_retained_status(
        &self,
        attempt: PhysicalWaitAttempt,
        raw_status: i32,
        siginfo: PhysicalWaitSiginfo,
    ) {
        self.record_checked(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::WaitResult {
                attempt: attempt.id,
                outcome: PhysicalWaitOutcome::RetainedStatus {
                    raw_status,
                    siginfo,
                },
            },
            &[attempt.observer],
        );
    }

    pub(crate) fn finish_wait_retained_undecodable_status(
        &self,
        attempt: PhysicalWaitAttempt,
        siginfo: PhysicalWaitSiginfo,
        error: i32,
    ) {
        self.record_checked(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::WaitResult {
                attempt: attempt.id,
                outcome: PhysicalWaitOutcome::RetainedUndecodableStatus { siginfo, error },
            },
            &[attempt.observer],
        );
    }

    pub(crate) fn allocate_status(&self) -> PhysicalStatusId {
        PhysicalStatusId(next_nonzero(&NEXT_STATUS_ID))
    }

    pub(crate) fn finish_wait_status_with_id(
        &self,
        attempt: PhysicalWaitAttempt,
        id: PhysicalStatusId,
        raw_status: i32,
        siginfo: Option<PhysicalWaitSiginfo>,
    ) {
        self.finish_wait_status_with_observers(
            attempt,
            id,
            raw_status,
            siginfo,
            &[attempt.observer],
        );
    }

    fn finish_wait_status_with_observers(
        &self,
        attempt: PhysicalWaitAttempt,
        id: PhysicalStatusId,
        raw_status: i32,
        siginfo: Option<PhysicalWaitSiginfo>,
        token_observers: &[PhysicalObserverId],
    ) {
        self.record_checked(
            if attempt.context.producer.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::WaitResult {
                attempt: attempt.id,
                outcome: PhysicalWaitOutcome::Status {
                    id,
                    raw_status,
                    siginfo,
                },
            },
            token_observers,
        );
    }

    /// Finishes a wait that returned a real status rejected by typed conversion.
    pub(crate) fn finish_wait_undecodable_status(
        &self,
        attempt: PhysicalWaitAttempt,
        id: PhysicalStatusId,
        siginfo: PhysicalWaitSiginfo,
        error: i32,
    ) {
        self.record_checked(
            if attempt.context.producer.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::WaitResult {
                attempt: attempt.id,
                outcome: PhysicalWaitOutcome::UndecodableStatus { id, siginfo, error },
            },
            &[attempt.observer],
        );
    }

    pub(crate) fn record_wait_siginfo(
        &self,
        attempt: PhysicalWaitAttempt,
        siginfo: PhysicalWaitSiginfo,
        status: Option<PhysicalStatusId>,
    ) {
        self.record_checked(
            if attempt.context.producer.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::WaitSiginfoReturned {
                attempt: attempt.id,
                siginfo,
                status,
            },
            &[attempt.observer],
        );
    }

    /// Finishes a successful nonblocking wait that returned no status.
    pub fn finish_wait_no_status(
        &self,
        attempt: PhysicalWaitAttempt,
        siginfo: Option<PhysicalWaitSiginfo>,
    ) {
        self.finish_wait_outcome(attempt, PhysicalWaitOutcome::NoStatus { siginfo });
    }

    /// Finishes a failed wait while retaining the exact errno category.
    pub fn finish_wait_error(&self, attempt: PhysicalWaitAttempt, errno: i32) {
        let outcome = match errno {
            libc::EINTR => PhysicalWaitOutcome::Interrupted,
            libc::ECHILD => PhysicalWaitOutcome::NoChild,
            other => PhysicalWaitOutcome::Error(other),
        };
        self.finish_wait_outcome(attempt, outcome);
    }

    fn finish_wait_outcome(&self, attempt: PhysicalWaitAttempt, outcome: PhysicalWaitOutcome) {
        self.record_checked(
            if attempt.context.producer.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::WaitResult {
                attempt: attempt.id,
                outcome,
            },
            &[attempt.observer],
        );
    }

    /// Begins an actual ptrace transition, including a raw cleanup call.
    pub fn begin_resume(&self, context: PhysicalResumeContext) -> PhysicalResumeAttempt {
        let id = PhysicalResumeAttemptId(next_nonzero(&NEXT_RESUME_ATTEMPT_ID));
        let attempt = PhysicalResumeAttempt {
            observer: self.id(),
            id,
            context,
        };
        self.record(
            if context.owner.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::ResumeAttempt { id, context },
        );
        attempt
    }

    /// Records the raw result before any ptrace error conversion.
    pub fn finish_resume(&self, attempt: PhysicalResumeAttempt, outcome: PhysicalResumeOutcome) {
        self.record_checked(
            if attempt.context.owner.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::ResumeResult {
                attempt: attempt.id,
                outcome,
            },
            &[attempt.observer],
        );
    }

    pub(crate) fn begin_pidfd_signal(
        &self,
        context: PhysicalPidfdSignalContext,
    ) -> PhysicalPidfdSignalAttempt {
        let id = PhysicalPidfdSignalAttemptId(next_nonzero(&NEXT_PIDFD_SIGNAL_ATTEMPT_ID));
        let attempt = PhysicalPidfdSignalAttempt {
            observer: self.id(),
            id,
            context,
        };
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PidfdSignalAttempt { id, context },
        );
        attempt
    }

    pub(crate) fn finish_pidfd_signal(
        &self,
        attempt: PhysicalPidfdSignalAttempt,
        outcome: PhysicalPidfdSignalOutcome,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PidfdSignalResult {
                attempt: attempt.id,
                outcome,
            },
            &[attempt.observer],
        );
    }

    /// Records that cleanup tolerated an already recorded raw error.
    pub fn tolerate_resume_error(&self, attempt: PhysicalResumeAttempt, errno: i32) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::ResumeErrorTolerated {
                attempt: attempt.id,
                errno,
            },
            &[attempt.observer],
        );
    }

    fn resolve_ambiguous_resume(
        &self,
        attempt: PhysicalResumeAttempt,
        proof: PhysicalAmbiguousResumeProof,
        proof_wait: Option<PhysicalWaitAttempt>,
    ) {
        let Some(source_status) = attempt.context.source_status else {
            self.inner.sticky_failure.store(true, Ordering::Release);
            return;
        };
        let mut token_observers = [attempt.observer, attempt.observer];
        if let Some(wait) = proof_wait {
            token_observers[1] = wait.observer;
        }
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusDisposition {
                status: source_status,
                disposition: PhysicalStatusDisposition::AmbiguousResumeCausallyResolved {
                    attempt: attempt.id,
                    proof,
                },
            },
            &token_observers,
        );
    }

    pub(crate) fn resolve_ambiguous_resume_with_later_status(
        &self,
        attempt: PhysicalResumeAttempt,
        successor: PhysicalStatusId,
    ) {
        self.resolve_ambiguous_resume(
            attempt,
            PhysicalAmbiguousResumeProof::LaterStatus(successor),
            None,
        );
    }

    pub(crate) fn resolve_ambiguous_resume_with_final_status(
        &self,
        attempt: PhysicalResumeAttempt,
        final_status: PhysicalStatusId,
    ) {
        self.resolve_ambiguous_resume(
            attempt,
            PhysicalAmbiguousResumeProof::FinalStatus(final_status),
            None,
        );
    }

    pub(crate) fn resolve_ambiguous_resume_with_proven_echild(
        &self,
        attempt: PhysicalResumeAttempt,
        terminal_wait: PhysicalWaitAttempt,
    ) {
        self.resolve_ambiguous_resume(
            attempt,
            PhysicalAmbiguousResumeProof::ProvenEchild(terminal_wait.id),
            Some(terminal_wait),
        );
    }

    /// Supplies an explicit final disposition for a physical status.
    pub fn finish_status(&self, status: PhysicalStatusId, disposition: PhysicalStatusDisposition) {
        self.record(
            if matches!(
                disposition,
                PhysicalStatusDisposition::CancellationCleanup
                    | PhysicalStatusDisposition::ExitCapabilityExpired
                    | PhysicalStatusDisposition::KernelSupersededByExitStop
                    | PhysicalStatusDisposition::AmbiguousResumeCausallyResolved { .. }
            ) {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::StatusDisposition {
                status,
                disposition,
            },
        );
    }

    /// Records a terminal physical status consumed by pre-notifier cleanup.
    pub fn finish_external_cleanup_status(&self, status: PhysicalStatusId) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation: None,
                destination: PhysicalStatusPublication::ExternalCleanup,
            },
        );
        self.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
    }

    /// Records a terminal status drained by registered notifier-failure cleanup.
    pub(crate) fn finish_registered_cleanup_terminal_status(
        &self,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation: Some(generation),
                destination: PhysicalStatusPublication::CleanupTerminal,
            },
        );
        self.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
    }

    /// Records the exact terminal status drained by an original-root startup
    /// cleanup transaction.
    pub(crate) fn finish_startup_barrier_cleanup_terminal_status(
        &self,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation: Some(generation),
                destination: PhysicalStatusPublication::StartupBarrierCleanupTerminal,
            },
        );
        self.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
    }

    /// Publishes a registered-cleanup stop for controller-side transition.
    pub(crate) fn publish_registered_cleanup_stop(
        &self,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation: Some(generation),
                destination: PhysicalStatusPublication::CleanupStopped,
            },
        );
    }

    /// Publishes a nonterminal status retained by direct pre-notifier cleanup.
    ///
    /// The caller must subsequently record either the actual ptrace transition
    /// that consumes this stop or an explicit cancellation disposition.
    pub fn publish_external_cleanup_stop(
        &self,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation: Some(generation),
                destination: PhysicalStatusPublication::DirectStopped,
            },
        );
    }

    /// Stops accepting records after all notifier and cleanup owners finish.
    ///
    /// The method first excludes the one original-root attachment transaction,
    /// then waits only for already-entered atomic writers; it acquires no
    /// notifier or registry lock. Any later attempted record is counted and
    /// makes validation fail.
    pub fn close(&self) {
        let _lifecycle = self.inner.lifecycle.lock();
        if self
            .inner
            .state
            .compare_exchange(
                OBSERVER_OPEN,
                OBSERVER_CLOSING,
                Ordering::SeqCst,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        while self.inner.active_writers.load(Ordering::SeqCst) != 0 {
            std::hint::spin_loop();
        }
        self.record_closing(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::ObserverClosed,
        );
        self.inner.state.store(OBSERVER_CLOSED, Ordering::Release);
    }

    /// Takes a read-only copy of all currently published records.
    pub fn snapshot(&self) -> PhysicalEventSnapshot {
        let mut records =
            Vec::with_capacity(self.inner.ordinary.slots.len() + self.inner.cleanup.slots.len());
        let ordinary_lost = self.inner.ordinary.snapshot(&mut records);
        let cleanup_lost = self.inner.cleanup.snapshot(&mut records);
        records.sort_unstable_by_key(|record| record.sequence);
        PhysicalEventSnapshot {
            observer: self.id(),
            records,
            ordinary_lost,
            cleanup_lost,
            after_close: self.inner.after_close.load(Ordering::Acquire),
            closed: self.inner.state.load(Ordering::Acquire) == OBSERVER_CLOSED,
            sticky_failure: self.inner.sticky_failure.load(Ordering::Acquire),
        }
    }

    pub(crate) fn attach_generation(&self, generation: PhysicalEventGenerationId) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::GenerationAttached(generation),
        );
    }

    pub(crate) fn record_continued_authority_enabled(
        &self,
        generation: PhysicalEventGenerationId,
        root: crate::Pid,
        controller_tgid: crate::Pid,
        controller_tracer_tid: crate::Pid,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::ContinuedAuthorityEnabled {
                generation,
                root: root.as_raw(),
                controller_tgid: controller_tgid.as_raw(),
                controller_tracer_tid: controller_tracer_tid.as_raw(),
            },
        );
    }

    pub(crate) fn record_continued_authority_revoked(&self, generation: PhysicalEventGenerationId) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::ContinuedAuthorityRevoked { generation },
        );
    }

    pub(crate) fn record_pre_registration_barrier_consumed(
        &self,
        generation: PhysicalEventGenerationId,
        barrier: PhysicalWaitAttempt,
        consuming_wait: PhysicalWaitAttempt,
        consumed_status: PhysicalStatusId,
    ) {
        self.record_checked(
            if consuming_wait.context.producer.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::PreRegistrationBarrierConsumed {
                generation,
                barrier: barrier.id,
                consuming_wait: consuming_wait.id,
                consumed_status,
            },
            &[barrier.observer, consuming_wait.observer],
        );
    }

    pub(crate) fn record_pre_registration_barrier_failure_linked(
        &self,
        generation: PhysicalEventGenerationId,
        barrier: PhysicalWaitAttempt,
        consuming_wait: PhysicalWaitAttempt,
        consumed_status: PhysicalStatusId,
        transaction: PhysicalCleanupTransaction,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PreRegistrationBarrierFailureLinked {
                generation,
                barrier: barrier.id(),
                consuming_wait: consuming_wait.id(),
                consumed_status,
                transaction: transaction.id(),
            },
            &[
                barrier.observer,
                consuming_wait.observer,
                transaction.observer,
            ],
        );
    }

    pub(crate) fn record_pre_registration_barrier_statusless_failure_linked(
        &self,
        generation: PhysicalEventGenerationId,
        barrier: PhysicalWaitAttempt,
        cause_wait: PhysicalWaitAttempt,
        error: i32,
        transaction: PhysicalCleanupTransaction,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PreRegistrationBarrierStatuslessFailureLinked {
                generation,
                barrier: barrier.id(),
                cause_wait: cause_wait.id(),
                error,
                transaction: transaction.id(),
            },
            &[barrier.observer, cause_wait.observer, transaction.observer],
        );
    }

    pub(crate) fn record_pre_registration_barrier_statusless_cleanup_resolved(
        &self,
        generation: PhysicalEventGenerationId,
        barrier: PhysicalWaitAttempt,
        cleanup_wait: PhysicalWaitAttempt,
        status: Option<PhysicalStatusId>,
        transaction: PhysicalCleanupTransaction,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PreRegistrationBarrierStatuslessCleanupResolved {
                generation,
                barrier: barrier.id(),
                cleanup_wait: cleanup_wait.id(),
                status,
                transaction: transaction.id(),
            },
            &[
                barrier.observer,
                cleanup_wait.observer,
                transaction.observer,
            ],
        );
    }

    pub(crate) fn record_pre_registration_barrier_setup_failed(
        &self,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        error: i32,
        launch: PhysicalOriginalRootLaunchId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PreRegistrationBarrierSetupFailed {
                generation,
                task,
                error,
                launch,
            },
        );
    }

    pub(crate) fn record_stop_resolution_watch_armed(
        &self,
        generation: PhysicalEventGenerationId,
        delivery: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StopResolutionWatchArmed {
                generation,
                delivery,
            },
        );
    }

    pub(crate) fn record_stop_resolution_first_stopped(
        &self,
        generation: PhysicalEventGenerationId,
        delivery: PhysicalStatusId,
        stopped: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StopResolutionFirstStopped {
                generation,
                delivery,
                stopped,
            },
        );
    }

    pub(crate) fn record_stop_resolution_group_acknowledged(
        &self,
        generation: PhysicalEventGenerationId,
        delivery: PhysicalStatusId,
        group_stop: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StopResolutionGroupAcknowledged {
                generation,
                delivery,
                group_stop,
            },
        );
    }

    pub(crate) fn record_stop_resolution_continued_claimed(
        &self,
        generation: PhysicalEventGenerationId,
        group_stop: PhysicalStatusId,
        continued: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StopResolutionContinuedClaimed {
                generation,
                group_stop,
                continued,
            },
        );
    }

    pub(crate) fn record_stop_resolution_watch_closed(
        &self,
        generation: PhysicalEventGenerationId,
        delivery: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StopResolutionWatchClosed {
                generation,
                delivery,
            },
        );
    }

    pub(crate) fn record_stop_resolution_resume_causally_closed(
        &self,
        generation: PhysicalEventGenerationId,
        delivery: PhysicalStatusId,
        group_stop: PhysicalStatusId,
        continued: PhysicalStatusId,
        attempt: PhysicalResumeAttempt,
        successor: PhysicalStatusId,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StopResolutionResumeCausallyClosed {
                generation,
                delivery,
                group_stop,
                continued,
                attempt: attempt.id(),
                successor,
            },
            &[attempt.observer],
        );
    }

    pub(crate) fn record_pre_stop_continued_drain_completed(
        &self,
        generation: PhysicalEventGenerationId,
        stopped: PhysicalStatusId,
        final_no_status_attempt: PhysicalWaitAttempt,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::PreStopContinuedDrainCompleted {
                generation,
                stopped,
                final_no_status_attempt: final_no_status_attempt.id,
            },
        );
    }

    pub(crate) fn record_pre_stop_continued_drain_failed(
        &self,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        stopped: PhysicalStatusId,
        cause_wait: PhysicalWaitAttempt,
        transaction: PhysicalCleanupTransaction,
    ) {
        self.record_checked(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PreStopContinuedDrainFailed {
                generation,
                task,
                stopped,
                cause_wait: cause_wait.id(),
                transaction: transaction.id(),
            },
            &[cause_wait.observer, transaction.observer],
        );
    }

    pub(crate) fn adopt_generation(
        &self,
        from: PhysicalEventGenerationId,
        to: PhysicalEventGenerationId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::GenerationAdopted { from, to },
        );
    }

    pub(crate) fn bind_identity(
        &self,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::IdentityBound { generation, task },
        );
    }

    pub(crate) fn record_generation_capture_failed(
        &self,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        error: i32,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::GenerationCaptureFailed {
                generation,
                task,
                error,
            },
        );
    }

    pub(crate) fn record_generation_identity_mismatch(
        &self,
        generation: PhysicalEventGenerationId,
        bound: PhysicalTaskIdentity,
        current: PhysicalTaskIdentity,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::GenerationIdentityMismatch {
                generation,
                bound,
                current,
            },
        );
    }

    pub(crate) fn record_generation_bound_pidfd_dead(
        &self,
        generation: PhysicalEventGenerationId,
        bound: PhysicalTaskIdentity,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::GenerationBoundPidfdDead { generation, bound },
        );
    }

    pub(crate) fn record_generation_current_pidfd_dead(
        &self,
        generation: PhysicalEventGenerationId,
        current: PhysicalTaskIdentity,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::GenerationCurrentPidfdDead {
                generation,
                current,
            },
        );
    }

    pub(crate) fn record_generation_registry_mismatch(
        &self,
        generation: PhysicalEventGenerationId,
        registered: PhysicalTaskIdentity,
        current: PhysicalTaskIdentity,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::GenerationRegistryMismatch {
                generation,
                registered,
                current,
            },
        );
    }

    /// Records an ordered liveness probe proving an `ECHILD` direct task gone.
    pub fn record_pre_registration_task_gone(
        &self,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        error: i32,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::PreRegistrationTaskGone {
                generation,
                task,
                error,
            },
        );
    }

    pub(crate) fn record_worker_started(&self, generation: PhysicalEventGenerationId) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::NotifierWorkerStarted(generation),
        );
    }

    pub(crate) fn record_generation_finished(&self, generation: PhysicalEventGenerationId) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::NotifierGenerationFinished {
                generation,
                external_wait: None,
            },
        );
    }

    pub(crate) fn next_reservation(&self) -> PhysicalReservationId {
        PhysicalReservationId(next_nonzero(&NEXT_RESERVATION_ID))
    }

    pub(crate) fn record_status_published(
        &self,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
        destination: PhysicalStatusPublication,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation: Some(generation),
                destination,
            },
        );
    }

    pub(crate) fn record_cleanup_status_published(
        &self,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
        destination: PhysicalStatusPublication,
    ) {
        debug_assert!(matches!(
            destination,
            PhysicalStatusPublication::PreStopDrainFailureCleanup
                | PhysicalStatusPublication::StartupBarrierFailureCleanup
                | PhysicalStatusPublication::StartupBarrierCleanupStopped
        ));
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation: Some(generation),
                destination,
            },
        );
    }

    pub(crate) fn record_synthetic_echild(
        &self,
        generation: PhysicalEventGenerationId,
        cause: Option<PhysicalWaitAttemptId>,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::SyntheticEchildPublished { generation, cause },
        );
    }

    pub(crate) fn record_reserved(
        &self,
        generation: PhysicalEventGenerationId,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::StatusReserved {
                reservation,
                status,
                generation,
            },
        );
    }

    pub(crate) fn record_cleanup_reserved(
        &self,
        generation: PhysicalEventGenerationId,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::StatusReserved {
                reservation,
                status,
                generation,
            },
        );
    }

    pub(crate) fn record_decode_started(
        &self,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
        owner: PhysicalDecodeOwner,
    ) {
        self.record(
            if owner.is_cleanup() {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::DecodeStarted {
                reservation,
                status,
                owner,
            },
        );
    }

    pub(crate) fn record_decode_finished(
        &self,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
        outcome: PhysicalDecodeOutcome,
        owner: PhysicalDecodeOwner,
    ) {
        self.record(
            if owner.is_cleanup() || matches!(outcome, PhysicalDecodeOutcome::Cancelled) {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::DecodeFinished {
                reservation,
                status,
                outcome,
                owner,
            },
        );
    }

    pub(crate) fn record_reservation_committed(
        &self,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::ReservationCommitted {
                reservation,
                status,
            },
        );
    }

    pub(crate) fn record_cleanup_reservation_committed(
        &self,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::ReservationCommitted {
                reservation,
                status,
            },
        );
    }

    pub(crate) fn record_ordinary_reservation_rolled_back(
        &self,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::ReservationRolledBack {
                reservation,
                status,
            },
        );
    }

    pub(crate) fn record_cleanup_reservation_rolled_back(
        &self,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Cleanup,
            PhysicalEventRecordKind::ReservationRolledBack {
                reservation,
                status,
            },
        );
    }

    pub(crate) fn record_terminal_replayed(
        &self,
        reservation: PhysicalReservationId,
        status: PhysicalStatusId,
    ) {
        self.record(
            RecordClass::Ordinary,
            PhysicalEventRecordKind::TerminalReplayed {
                reservation,
                status,
            },
        );
    }

    pub(crate) fn record_exit_capability(
        &self,
        status: PhysicalStatusId,
        transition: PhysicalExitCapabilityTransition,
    ) {
        self.record(
            if matches!(
                transition,
                PhysicalExitCapabilityTransition::Expired
                    | PhysicalExitCapabilityTransition::Revoked
                    | PhysicalExitCapabilityTransition::TransferredToCleanup
            ) {
                RecordClass::Cleanup
            } else {
                RecordClass::Ordinary
            },
            PhysicalEventRecordKind::ExitCapability { status, transition },
        );
    }

    fn record(&self, class: RecordClass, kind: PhysicalEventRecordKind) {
        self.record_checked(class, kind, &[]);
    }

    fn record_checked(
        &self,
        class: RecordClass,
        kind: PhysicalEventRecordKind,
        token_observers: &[PhysicalObserverId],
    ) {
        // Registration is the writer's first atomic boundary.  Together with
        // close's sequentially consistent state transition and count load,
        // this gives every concurrent call one side of the close boundary:
        // either close observes and drains it, or its state check observes
        // CLOSING/CLOSED and records the rejected write.
        self.inner.active_writers.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)]
        if self
            .inner
            .pause_after_writer_registration
            .load(Ordering::SeqCst)
        {
            self.inner
                .writer_registration_paused
                .store(true, Ordering::SeqCst);
            while self
                .inner
                .pause_after_writer_registration
                .load(Ordering::SeqCst)
            {
                std::thread::yield_now();
            }
        }
        for observer in token_observers {
            if *observer != self.id() {
                self.inner.after_close.fetch_add(1, Ordering::Relaxed);
                self.inner.sticky_failure.store(true, Ordering::Release);
            }
        }
        if self.inner.state.load(Ordering::SeqCst) != OBSERVER_OPEN {
            self.inner.after_close.fetch_add(1, Ordering::Relaxed);
            self.inner.sticky_failure.store(true, Ordering::Release);
            // Publish failure evidence before making the writer invisible to
            // close's drain loop.
            self.inner.active_writers.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        self.record_open(class, kind);
        self.inner.active_writers.fetch_sub(1, Ordering::SeqCst);
    }

    fn record_open(&self, class: RecordClass, kind: PhysicalEventRecordKind) {
        let sequence = next_nonzero(&self.inner.next_sequence);
        let record = PhysicalEventRecord {
            sequence,
            observer: self.id(),
            kind,
        };
        let stored = match class {
            RecordClass::Ordinary => self.inner.ordinary.push(record),
            RecordClass::Cleanup => self.inner.cleanup.push(record),
        };
        if !stored {
            self.inner.sticky_failure.store(true, Ordering::Release);
        }
    }

    fn record_closing(&self, class: RecordClass, kind: PhysicalEventRecordKind) {
        self.record_open(class, kind);
    }

    #[cfg(test)]
    fn inject_for_test(&self, kind: PhysicalEventRecordKind) {
        self.record(RecordClass::Ordinary, kind);
    }

    #[cfg(test)]
    fn pause_next_writer_after_registration_for_test(&self) {
        self.inner
            .writer_registration_paused
            .store(false, Ordering::SeqCst);
        self.inner
            .pause_after_writer_registration
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn writer_is_paused_after_registration_for_test(&self) -> bool {
        self.inner.writer_registration_paused.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn release_writer_after_registration_for_test(&self) {
        self.inner
            .pause_after_writer_registration
            .store(false, Ordering::SeqCst);
    }
}

/// Immutable copy of one observer's retained evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalEventSnapshot {
    observer: PhysicalObserverId,
    records: Vec<PhysicalEventRecord>,
    ordinary_lost: u64,
    cleanup_lost: u64,
    after_close: u64,
    closed: bool,
    sticky_failure: bool,
}

impl PhysicalEventSnapshot {
    /// Returns the observer identity.
    pub fn observer(&self) -> PhysicalObserverId {
        self.observer
    }

    /// Returns the retained records in observer-local append order.
    pub fn records(&self) -> &[PhysicalEventRecord] {
        &self.records
    }

    /// Returns the count of ordinary records lost after capacity was exhausted.
    pub fn ordinary_lost(&self) -> u64 {
        self.ordinary_lost
    }

    /// Returns the count of cleanup records lost after its reserve was exhausted.
    pub fn cleanup_lost(&self) -> u64 {
        self.cleanup_lost
    }

    /// Returns the count of writes attempted after observation closed.
    pub fn after_close(&self) -> u64 {
        self.after_close
    }

    /// Returns whether the caller closed the observer after cleanup.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Validates that physical statuses, reservations, and transitions partition.
    pub fn validate(&self) -> PhysicalPartitionValidation {
        validate_partition(self)
    }
}

/// Machine-readable physical partition failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalPartitionViolation {
    /// Validation ran before the session closed.
    ObserverOpen,
    /// An observer-internal invariant failure made the evidence sticky-invalid
    /// independently of the bounded-buffer counters.
    ObserverStickyFailure,
    /// At least one record was lost or attempted after close.
    Overflow {
        /// Lost ordinary records.
        ordinary: u64,
        /// Lost cleanup records.
        cleanup: u64,
        /// Writes attempted after close.
        after_close: u64,
    },
    /// A wait result had no matching attempt.
    WaitResultWithoutAttempt(PhysicalWaitAttemptId),
    /// One wait attempt received more than one result.
    DuplicateWaitResult(PhysicalWaitAttemptId),
    /// Raw waitid siginfo had no matching wait attempt.
    WaitSiginfoWithoutAttempt(PhysicalWaitAttemptId),
    /// Raw waitid siginfo named a different status than its wait result.
    WaitSiginfoStatusMismatch(PhysicalWaitAttemptId),
    /// Raw waitid siginfo named a task other than its wait context.
    WaitSiginfoTaskMismatch(PhysicalWaitAttemptId),
    /// A wait producer used option bits outside its exact production call.
    InvalidWaitFlags(PhysicalWaitAttemptId),
    /// A wait result class was impossible for its requested option bits.
    InvalidWaitOutcomeForFlags(PhysicalWaitAttemptId),
    /// A wait attempt never received a raw result.
    WaitAttemptWithoutResult(PhysicalWaitAttemptId),
    /// A physical status ID was created by more than one wait result.
    DuplicatePhysicalStatus(PhysicalStatusId),
    /// An undecodable outcome did not preserve the exact conversion boundary.
    InvalidUndecodableStatus(PhysicalStatusId),
    /// An undecodable status entered typed publication, reservation, or capability state.
    UndecodableStatusEscaped(PhysicalStatusId),
    /// An undecodable status lacked one exact cleanup disposition or cleanup resume.
    InvalidUndecodableStatusLifecycle(PhysicalStatusId),
    /// A registered-cleanup transaction was missing, duplicated, or incomplete.
    InvalidRegisteredCleanupTransaction(PhysicalCleanupTransactionId),
    /// A registered-cleanup wait was not owned by exactly one ordered transaction.
    InvalidRegisteredCleanupWaitOwnership(PhysicalWaitAttemptId),
    /// One status was linked more than once or to multiple cleanup transactions.
    DuplicateRegisteredCleanupStatus(PhysicalStatusId),
    /// One tolerated resume was linked more than once or to multiple transactions.
    DuplicateRegisteredCleanupResume(PhysicalResumeAttemptId),
    /// One terminal wait was reused for multiple cleanup transactions.
    DuplicateRegisteredCleanupTerminalWait(PhysicalWaitAttemptId),
    /// One fatal cause wait was reused for multiple cleanup transactions.
    DuplicateRegisteredCleanupCauseWait(PhysicalWaitAttemptId),
    /// A fatal notifier/synchronous wait lacked one valid cleanup transaction.
    FatalWaitWithoutCleanupTransaction(PhysicalWaitAttemptId),
    /// A status requiring terminal-drain causality lacked exact transaction evidence.
    MissingRegisteredCleanupEvidence(PhysicalStatusId),
    /// A tolerated controller-cleanup resume lacked exact transaction evidence.
    MissingRegisteredCleanupResumeEvidence(PhysicalResumeAttemptId),
    /// A synthetic ECHILD marker did not link to an ECHILD wait result.
    SyntheticEchildWithoutNoChild(PhysicalWaitAttemptId),
    /// Notifier/synchronous `ECHILD` lacked one exact post-wait liveness proof.
    InvalidEchildTerminalProof(PhysicalWaitAttemptId),
    /// A generation had no direct-child or captured task authority.
    MissingTaskAuthority(PhysicalEventGenerationId),
    /// Task claims for a generation or its adopted authority conflicted.
    ConflictingTaskAuthority(PhysicalEventGenerationId),
    /// One raw Event generation emitted more than one identity binding.
    DuplicateIdentityBinding(PhysicalEventGenerationId),
    /// An adoption chain was cyclic or assigned two different authorities.
    InvalidGenerationAdoption(PhysicalEventGenerationId),
    /// Attachment, worker start, finish, adoption, or activity order was invalid.
    InvalidGenerationLifecycle(PhysicalEventGenerationId),
    /// Original-root process-wide continued-status authority was malformed,
    /// duplicated, inherited, or used outside its immutable generation.
    InvalidContinuedAuthority(PhysicalEventGenerationId),
    /// The opaque original-root launch provenance was absent, duplicated,
    /// adopted, or ordered after a generic pre-registration link.
    InvalidOriginalRootLaunch(PhysicalEventGenerationId),
    /// One raw Event generation emitted more than one generic pre-registration
    /// ownership link, including otherwise-unused generations.
    DuplicatePreRegistrationLink(PhysicalEventGenerationId),
    /// The retained original-root startup peek was missing, duplicated, or
    /// failed one-to-one byte-exact linkage to its consuming wait.
    InvalidPreRegistrationBarrier(PhysicalWaitAttemptId),
    /// Exact-pidfd startup failed before the retained barrier without a valid
    /// unstarted-generation cleanup chain.
    InvalidPreRegistrationSetupFailure(PhysicalEventGenerationId),
    /// Stop-resolution arm/G/C causal records did not justify a continued
    /// side-channel route.
    InvalidStopResolutionEvidence(PhysicalStatusId),
    /// A plain SIGSTOP stale-continued drain lacked its exact zero-status
    /// barrier, status linkage, or ordered publication boundary.
    InvalidPreStopContinuedDrain(PhysicalStatusId),
    /// A pre-stop drain wait was not owned by exactly one completed fence or
    /// one fail-closed registered cleanup transaction.
    InvalidPreStopContinuedDrainAttempt(PhysicalWaitAttemptId),
    /// A wait was not bound to the exact task authority for its generation.
    WrongWaitTask(PhysicalWaitAttemptId),
    /// A wait began before its canonical generation had captured task identity.
    WaitBeforeIdentityBound(PhysicalWaitAttemptId),
    /// A wait omitted its generation outside exact pre-registration cleanup.
    WaitWithoutGeneration(PhysicalWaitAttemptId),
    /// A later record referred to an unknown physical status.
    UnknownPhysicalStatus(PhysicalStatusId),
    /// Publication changed the immutable Event generation of a status.
    WrongGeneration(PhysicalStatusId),
    /// Producer, destination, or raw status shape did not match.
    InvalidStatusPublication(PhysicalStatusId),
    /// A physical status was never published.
    StatusNotPublished(PhysicalStatusId),
    /// One physical status was published more than once.
    DuplicateStatusPublication(PhysicalStatusId),
    /// External terminal cleanup tried to finalize a nonterminal status.
    ExternalCleanupNonterminal(PhysicalStatusId),
    /// Registered cleanup tried to finalize a nonterminal status.
    CleanupTerminalNonterminal(PhysicalStatusId),
    /// A physical stopped status had no final disposition.
    StatusWithoutDisposition(PhysicalStatusId),
    /// A physical status acquired more than one final disposition.
    DuplicateStatusDisposition(PhysicalStatusId),
    /// A reservation had no matching status.
    ReservationForUnknownStatus(PhysicalReservationId),
    /// A reservation identity was reused.
    DuplicateReservation(PhysicalReservationId),
    /// Reservation began before the exact status was created and published.
    InvalidReservationOrder(PhysicalReservationId),
    /// A status destination has no production reservation plane.
    InvalidReservationDestination(PhysicalReservationId),
    /// A reservation never committed, replayed, or rolled back.
    LiveReservation(PhysicalReservationId),
    /// A reservation ended more than once.
    DuplicateReservationOutcome(PhysicalReservationId),
    /// Decode referred to a reservation/status pair that was never reserved.
    DecodeWithoutReservation(PhysicalReservationId),
    /// Decode completion had no matching in-flight decode start.
    DecodeFinishedWithoutStart(PhysicalReservationId),
    /// A reservation began decoding more than once.
    DuplicateDecodeStart(PhysicalReservationId),
    /// A reservation finished decoding more than once.
    DuplicateDecodeFinish(PhysicalReservationId),
    /// A reservation completed without one matching decode transaction.
    ReservationWithoutDecode(PhysicalReservationId),
    /// Decode result and reservation commit/rollback outcome were inconsistent.
    InvalidDecodeReservationOutcome(PhysicalReservationId),
    /// Two reservations concurrently claimed the same non-retained status.
    OverlappingStatusReservations(PhysicalStatusId),
    /// More than one reservation consumed the same physical status.
    DuplicateStatusConsumption(PhysicalStatusId),
    /// A consuming typed delivery did not authorize exactly one later typed resume attempt.
    InvalidTypedResumeCardinality(PhysicalReservationId),
    /// A non-retained status was reserved again after it was consumed.
    ReservationAfterStatusConsumption(PhysicalStatusId),
    /// A FIFO status was reserved before every earlier publication was consumed.
    OutOfOrderStatusReservation(PhysicalStatusId),
    /// Observation closed while a fallible decode remained in flight.
    LiveDecode(PhysicalReservationId),
    /// A resume result had no matching attempt.
    ResumeResultWithoutAttempt(PhysicalResumeAttemptId),
    /// One resume attempt received more than one raw result.
    DuplicateResumeResult(PhysicalResumeAttemptId),
    /// A resume attempt never received a raw result.
    ResumeAttemptWithoutResult(PhysicalResumeAttemptId),
    /// A pidfd-signal result had no matching attempt.
    PidfdSignalResultWithoutAttempt(PhysicalPidfdSignalAttemptId),
    /// One pidfd-signal attempt received more than one raw result.
    DuplicatePidfdSignalResult(PhysicalPidfdSignalAttemptId),
    /// A pidfd-signal attempt never received a raw result.
    PidfdSignalAttemptWithoutResult(PhysicalPidfdSignalAttemptId),
    /// A startup cleanup transaction had missing, repeated, or mismatched
    /// generation-bound pidfd-signal evidence.
    InvalidStartupCleanupPidfdSignal(PhysicalCleanupTransactionId),
    /// Cleanup tolerated an error different from the recorded raw result.
    InvalidToleratedResumeError(PhysicalResumeAttemptId),
    /// One resume attempt received more than one tolerance record.
    DuplicateToleratedResumeError(PhysicalResumeAttemptId),
    /// An ESRCH/EIO resolution lacked one exact later wait-side proof, matching
    /// tolerance record, or final source-status disposition.
    InvalidAmbiguousResumeResolution(PhysicalResumeAttemptId),
    /// One ambiguous resume attempt or source status was resolved more than once.
    DuplicateAmbiguousResumeResolution(PhysicalResumeAttemptId),
    /// More than one successful resume consumed the same physical stop.
    DuplicateSuccessfulResume(PhysicalStatusId),
    /// A successful resume did not name an observed source stop.
    SuccessfulResumeWithoutStatus(PhysicalResumeAttemptId),
    /// A successful resume named a terminal, continued, or unauthenticated source.
    InvalidResumeSourceStatus(PhysicalResumeAttemptId),
    /// A resume attempt preceded publication of its source status.
    ResumeBeforeStatusPublication(PhysicalResumeAttemptId),
    /// A cleanup owner used a ptrace request or signal outside its exact path.
    InvalidCleanupResumeShape(PhysicalResumeAttemptId),
    /// A resume was not bound to the exact task authority and source status.
    WrongResumeTask(PhysicalResumeAttemptId),
    /// A resume began before its canonical generation had captured task identity.
    ResumeBeforeIdentityBound(PhysicalResumeAttemptId),
    /// A resume omitted its immutable generation.
    ResumeWithoutGeneration(PhysicalResumeAttemptId),
    /// An exit-capability record referred to an unknown status.
    ExitCapabilityWithoutStatus(PhysicalStatusId),
    /// An exit capability was published more than once.
    DuplicateExitCapabilityPublication(PhysicalStatusId),
    /// More than one consumer claimed the same exit capability.
    DuplicateExitCapabilityClaim(PhysicalStatusId),
    /// An exit capability remained live or had contradictory final ownership.
    InvalidExitCapabilityFinalization(PhysicalStatusId),
    /// Exit-capability transitions violated their causal state machine.
    InvalidExitCapabilityTransition(PhysicalStatusId),
    /// A status disposition lacked its required causal predecessor.
    InvalidStatusDisposition(PhysicalStatusId),
    /// A notifier worker remained live when observation closed.
    LiveNotifierWorker(PhysicalEventGenerationId),
    /// An attached Event generation never reached completion or adoption.
    LiveEventGeneration(PhysicalEventGenerationId),
    /// External generation completion did not cite terminal wait evidence.
    InvalidGenerationFinishEvidence(PhysicalWaitAttemptId),
}

/// Counts and violations produced by physical partition validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalPartitionValidation {
    /// Number of real physical statuses created at kernel boundaries.
    pub physical_statuses: usize,
    /// Number of successful ptrace transitions.
    pub successful_resumes: usize,
    /// Number of retained or explicit final dispositions.
    pub explicit_dispositions: usize,
    /// Complete nonempty failure list when the partition is invalid.
    pub violations: Vec<PhysicalPartitionViolation>,
}

impl PhysicalPartitionValidation {
    /// Returns true only when the observer closed without any partition failure.
    pub fn is_valid(&self) -> bool {
        self.violations.is_empty()
    }
}

#[derive(Default)]
struct StatusTrack {
    generation: Option<PhysicalEventGenerationId>,
    task: Option<PhysicalTaskIdentity>,
    producer: Option<PhysicalWaitProducer>,
    created_sequence: u64,
    raw_status: Option<i32>,
    undecodable: bool,
    undecodable_siginfo: Option<PhysicalWaitSiginfo>,
    published: usize,
    publication_destination: Option<PhysicalStatusPublication>,
    publication_sequence: Option<u64>,
    dispositions: usize,
    last_disposition_sequence: Option<u64>,
    cancellation_cleanup_dispositions: usize,
    cancellation_cleanup_sequence: Option<u64>,
    ordinary_handled_dispositions: usize,
    ordinary_handled_sequence: Option<u64>,
    continued_side_channel_dispositions: usize,
    continued_side_channel_sequence: Option<u64>,
    continued_side_channel_route: Option<PhysicalContinuedStatusRoute>,
    decode_died_dispositions: usize,
    decode_died_sequence: Option<u64>,
    exit_capability_expired_dispositions: usize,
    exit_capability_expired_sequence: Option<u64>,
    kernel_superseded_dispositions: usize,
    kernel_superseded_sequence: Option<u64>,
    ambiguous_resume_resolved_dispositions: usize,
    ambiguous_resume_resolved_sequence: Option<u64>,
    successful_resumes: usize,
    registered_controller_cleanup_resumes: usize,
    successful_resume_sequence: Option<u64>,
    successful_resume_owner: Option<PhysicalResumeOwner>,
}

impl StatusTrack {
    fn startup_typed_unsupported_terminal(&self) -> bool {
        self.undecodable_siginfo
            .is_some_and(siginfo_is_startup_typed_unsupported_terminal)
    }

    fn startup_typed_unsupported_stopped(&self) -> bool {
        self.undecodable_siginfo
            .is_some_and(siginfo_is_startup_typed_unsupported_stopped)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReservationCompletion {
    Committed,
    RolledBack,
    TerminalReplayed,
}

#[derive(Default)]
struct ReservationTrack {
    status: Option<PhysicalStatusId>,
    reserved_sequence: Option<u64>,
    decode_started: Option<(PhysicalDecodeOwner, u64)>,
    decode_finished: Option<(PhysicalDecodeOwner, PhysicalDecodeOutcome, u64)>,
    completion: Option<(ReservationCompletion, u64)>,
}

#[derive(Clone, Copy)]
struct StatusReservationInterval {
    reserved: u64,
    completion: Option<(ReservationCompletion, u64)>,
    consuming: bool,
}

#[derive(Default)]
struct ExitCapabilityTrack {
    transitions: Vec<(u64, PhysicalExitCapabilityTransition)>,
}

#[derive(Default)]
struct CleanupTransactionTrack {
    starts: usize,
    start_sequence: Option<u64>,
    cause_wait: Option<PhysicalWaitAttemptId>,
    kind: Option<PhysicalCleanupTransactionKind>,
    statuses: BTreeMap<PhysicalStatusId, u64>,
    resumes: BTreeMap<PhysicalResumeAttemptId, u64>,
    pidfd_exit_proof: Option<(
        PhysicalWaitAttemptId,
        PhysicalEventGenerationId,
        PhysicalTaskIdentity,
        i16,
        Option<PhysicalOriginalRootLaunchId>,
        u64,
    )>,
    terminal_wait: Option<PhysicalWaitAttemptId>,
    completion_sequence: Option<u64>,
}

fn resolve_generation(
    generation: PhysicalEventGenerationId,
    adoptions: &BTreeMap<PhysicalEventGenerationId, PhysicalEventGenerationId>,
) -> Option<PhysicalEventGenerationId> {
    let mut current = generation;
    let mut seen = BTreeSet::new();
    while let Some(next) = adoptions.get(&current).copied() {
        if !seen.insert(current) {
            return None;
        }
        current = next;
    }
    seen.insert(current).then_some(current)
}

fn canonical_generation(
    generation: PhysicalEventGenerationId,
    adoptions: &BTreeMap<PhysicalEventGenerationId, PhysicalEventGenerationId>,
    invalid_adoptions: &BTreeSet<PhysicalEventGenerationId>,
) -> Option<PhysicalEventGenerationId> {
    let mut current = generation;
    let mut seen = BTreeSet::new();
    loop {
        if invalid_adoptions.contains(&current) || !seen.insert(current) {
            return None;
        }
        let Some(next) = adoptions.get(&current).copied() else {
            return Some(current);
        };
        current = next;
    }
}

fn shared_waitid_status(siginfo: PhysicalWaitSiginfo) -> Result<Option<i32>, crate::Errno> {
    crate::waitid::physical_wait_siginfo_to_status(
        siginfo.signo,
        siginfo.errno,
        siginfo.code,
        siginfo.pid,
        siginfo.uid,
        siginfo.status,
    )
}

fn siginfo_matches_wait_status(siginfo: PhysicalWaitSiginfo, raw_status: i32) -> bool {
    matches!(shared_waitid_status(siginfo), Ok(Some(raw)) if raw == raw_status)
}

fn siginfo_is_rejected_by_waitid(siginfo: PhysicalWaitSiginfo) -> bool {
    matches!(shared_waitid_status(siginfo), Err(crate::Errno::EPROTO))
}

fn siginfo_is_startup_typed_unsupported_terminal(siginfo: PhysicalWaitSiginfo) -> bool {
    siginfo.code == libc::CLD_KILLED
        && matches!(
            crate::waitid::classify_physical_wait_siginfo(
                siginfo.signo,
                siginfo.errno,
                siginfo.code,
                siginfo.pid,
                siginfo.uid,
                siginfo.status,
            ),
            Ok(crate::waitid::PhysicalWaitSiginfoClass::ValidButTypedUnsupported)
        )
}

fn siginfo_is_startup_typed_unsupported_stopped(siginfo: PhysicalWaitSiginfo) -> bool {
    siginfo.code == libc::CLD_TRAPPED
        && matches!(
            crate::waitid::classify_physical_wait_siginfo(
                siginfo.signo,
                siginfo.errno,
                siginfo.code,
                siginfo.pid,
                siginfo.uid,
                siginfo.status,
            ),
            Ok(crate::waitid::PhysicalWaitSiginfoClass::ValidButTypedUnsupported)
        )
}

fn siginfo_is_exact_no_status(siginfo: PhysicalWaitSiginfo) -> bool {
    siginfo
        == (PhysicalWaitSiginfo {
            signo: 0,
            errno: 0,
            code: 0,
            pid: 0,
            uid: 0,
            status: 0,
        })
        && matches!(shared_waitid_status(siginfo), Ok(None))
}

fn production_wait_flags(producer: PhysicalWaitProducer) -> i32 {
    match producer {
        PhysicalWaitProducer::AuthorizedRootNotifier => {
            libc::WEXITED | libc::WSTOPPED | libc::WCONTINUED | libc::__WALL
        }
        PhysicalWaitProducer::PreStopContinuedDrain => {
            libc::WCONTINUED | libc::WNOHANG | libc::__WALL
        }
        PhysicalWaitProducer::NotifierWorker
        | PhysicalWaitProducer::SynchronousWait
        | PhysicalWaitProducer::RegisteredCleanup => libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        PhysicalWaitProducer::PreRegistrationBarrier => {
            libc::WEXITED | libc::WSTOPPED | libc::WNOWAIT | libc::__WALL
        }
        PhysicalWaitProducer::PreRegistrationBarrierCleanup => {
            libc::WEXITED | libc::WSTOPPED | libc::WCONTINUED | libc::__WALL
        }
        PhysicalWaitProducer::PreRegistrationCleanup => libc::__WALL | libc::WNOHANG,
    }
}

fn wait_outcome_matches_flags(context: PhysicalWaitContext, outcome: PhysicalWaitOutcome) -> bool {
    match outcome {
        PhysicalWaitOutcome::RetainedStatus {
            raw_status,
            siginfo,
        } => {
            context.producer == PhysicalWaitProducer::PreRegistrationBarrier
                && context.flags & libc::WNOWAIT != 0
                && siginfo_matches_wait_status(siginfo, raw_status)
                && !libc::WIFCONTINUED(raw_status)
        }
        PhysicalWaitOutcome::RetainedUndecodableStatus { siginfo, error } => {
            context.producer == PhysicalWaitProducer::PreRegistrationBarrier
                && context.flags & libc::WNOWAIT != 0
                && error == libc::EPROTO
                && siginfo.pid == context.task.tid
                && siginfo.code != libc::CLD_CONTINUED
                && siginfo_is_rejected_by_waitid(siginfo)
        }
        PhysicalWaitOutcome::Status {
            raw_status,
            siginfo: Some(siginfo),
            ..
        } if context.producer != PhysicalWaitProducer::PreRegistrationCleanup => {
            (match siginfo.code {
                libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED => {
                    context.flags & libc::WEXITED != 0
                }
                libc::CLD_STOPPED | libc::CLD_TRAPPED => context.flags & libc::WSTOPPED != 0,
                libc::CLD_CONTINUED => context.flags & libc::WCONTINUED != 0,
                _ => false,
            }) && (libc::WIFCONTINUED(raw_status) == (siginfo.code == libc::CLD_CONTINUED))
        }
        PhysicalWaitOutcome::Status {
            raw_status,
            siginfo: None,
            ..
        } if context.producer == PhysicalWaitProducer::PreRegistrationCleanup => {
            crate::waitid::physical_raw_wait_status(context.task.tid(), raw_status).is_ok()
        }
        PhysicalWaitOutcome::Status { .. } => false,
        PhysicalWaitOutcome::UndecodableStatus { .. } => {
            context.producer != PhysicalWaitProducer::PreRegistrationCleanup
        }
        PhysicalWaitOutcome::NoStatus { .. } => context.flags & libc::WNOHANG != 0,
        PhysicalWaitOutcome::Interrupted | PhysicalWaitOutcome::NoChild => true,
        PhysicalWaitOutcome::Error(error) => error != 0,
    }
}

fn wait_outcome_matches_errno(outcome: Option<&PhysicalWaitOutcome>, error: i32) -> bool {
    match outcome {
        Some(PhysicalWaitOutcome::Interrupted) => error == libc::EINTR,
        Some(PhysicalWaitOutcome::NoChild) => error == libc::ECHILD,
        Some(PhysicalWaitOutcome::Error(observed)) => *observed == error,
        _ => false,
    }
}

fn is_terminal_raw_status(raw_status: i32) -> bool {
    libc::WIFEXITED(raw_status) || libc::WIFSIGNALED(raw_status)
}

fn is_ptrace_exit_stop(raw_status: i32) -> bool {
    libc::WIFSTOPPED(raw_status)
        && libc::WSTOPSIG(raw_status) == libc::SIGTRAP
        && ((raw_status >> 16) & 0xffff) == libc::PTRACE_EVENT_EXIT
}

fn is_plain_sigstop(raw_status: i32) -> bool {
    libc::WIFSTOPPED(raw_status)
        && libc::WSTOPSIG(raw_status) == libc::SIGSTOP
        && ((raw_status as u32 >> 16) & 0xffff) == 0
}

fn is_new_child_stop(raw_status: i32) -> bool {
    libc::WIFSTOPPED(raw_status)
        && matches!(
            ((raw_status as u32 >> 16) & 0xffff) as i32,
            libc::PTRACE_EVENT_FORK | libc::PTRACE_EVENT_VFORK | libc::PTRACE_EVENT_CLONE
        )
}

fn is_fallible_getevent_stop(raw_status: i32) -> bool {
    if !libc::WIFSTOPPED(raw_status) || libc::WSTOPSIG(raw_status) != libc::SIGTRAP {
        return false;
    }
    let si_status = (raw_status as u32 >> 8) as i32;
    matches!(
        crate::waitid::physical_trapped_status(si_status),
        Ok((signal, event))
            if signal as i32 == libc::SIGTRAP
                && matches!(
                    event,
                    libc::PTRACE_EVENT_FORK
                        | libc::PTRACE_EVENT_VFORK
                        | libc::PTRACE_EVENT_CLONE
                        | libc::PTRACE_EVENT_EXEC
                )
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenerationLifecycleState {
    Attached,
    WorkerStarted,
    Finished,
    Adopted,
}

#[derive(Clone, Copy, Debug)]
struct ContinuedAuthorityTrack {
    root: i32,
    controller_tgid: i32,
    controller_tracer_tid: i32,
    enabled_sequence: u64,
    revoked_sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct PreStopDrainTrack {
    generation: PhysicalEventGenerationId,
    final_no_status_attempt: PhysicalWaitAttemptId,
    completed_sequence: u64,
}

fn validate_partition(snapshot: &PhysicalEventSnapshot) -> PhysicalPartitionValidation {
    let mut violations = Vec::new();
    if !snapshot.closed {
        violations.push(PhysicalPartitionViolation::ObserverOpen);
    }
    if snapshot.sticky_failure {
        violations.push(PhysicalPartitionViolation::ObserverStickyFailure);
    }
    if snapshot.ordinary_lost != 0 || snapshot.cleanup_lost != 0 || snapshot.after_close != 0 {
        violations.push(PhysicalPartitionViolation::Overflow {
            ordinary: snapshot.ordinary_lost,
            cleanup: snapshot.cleanup_lost,
            after_close: snapshot.after_close,
        });
    }

    let mut adoptions = BTreeMap::<PhysicalEventGenerationId, PhysicalEventGenerationId>::new();
    let mut invalid_adoptions = BTreeSet::new();
    let mut mentioned_generations = BTreeSet::new();
    let mut authority_required = BTreeSet::new();
    for record in &snapshot.records {
        match record.kind {
            PhysicalEventRecordKind::GenerationAdopted { from, to } => {
                mentioned_generations.extend([from, to]);
                authority_required.extend([from, to]);
                if from == to {
                    invalid_adoptions.insert(from);
                }
                match adoptions.get(&from).copied() {
                    Some(existing) if existing != to => {
                        invalid_adoptions.insert(from);
                    }
                    Some(_) => {}
                    None => {
                        adoptions.insert(from, to);
                    }
                }
            }
            PhysicalEventRecordKind::GenerationAttached(generation)
            | PhysicalEventRecordKind::OriginalRootLaunchLinked { generation, .. }
            | PhysicalEventRecordKind::StartupBarrierFallbackPrepared { generation, .. }
            | PhysicalEventRecordKind::StartupBarrierFallbackReleased { generation, .. }
            | PhysicalEventRecordKind::ContinuedAuthorityEnabled { generation, .. }
            | PhysicalEventRecordKind::ContinuedAuthorityRevoked { generation }
            | PhysicalEventRecordKind::PreRegistrationBarrierConsumed { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationBarrierFailureLinked { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationBarrierStatuslessFailureLinked {
                generation,
                ..
            }
            | PhysicalEventRecordKind::PreRegistrationBarrierStatuslessCleanupResolved {
                generation,
                ..
            }
            | PhysicalEventRecordKind::PreRegistrationBarrierSetupFailed { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupPrepared { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupLinked { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupNoStatusLinked { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupPidfdExitProved { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupWaitFailureExitProved { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupResumeFailureExitProved {
                generation, ..
            }
            | PhysicalEventRecordKind::StartupCleanupExecutorTransferred { generation, .. }
            | PhysicalEventRecordKind::StopResolutionWatchArmed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionFirstStopped { generation, .. }
            | PhysicalEventRecordKind::StopResolutionGroupAcknowledged { generation, .. }
            | PhysicalEventRecordKind::StopResolutionContinuedClaimed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionWatchClosed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionResumeCausallyClosed { generation, .. }
            | PhysicalEventRecordKind::PreStopContinuedDrainCompleted { generation, .. }
            | PhysicalEventRecordKind::PreStopContinuedDrainFailed { generation, .. }
            | PhysicalEventRecordKind::NotifierWorkerStarted(generation)
            | PhysicalEventRecordKind::NotifierGenerationFinished { generation, .. }
            | PhysicalEventRecordKind::ExecGenerationBound { generation, .. }
            | PhysicalEventRecordKind::SyntheticEchildPublished { generation, .. } => {
                mentioned_generations.insert(generation);
                authority_required.insert(generation);
            }
            PhysicalEventRecordKind::PreRegistrationLinked { generation, .. }
            | PhysicalEventRecordKind::IdentityBound { generation, .. }
            | PhysicalEventRecordKind::GenerationCaptureFailed { generation, .. }
            | PhysicalEventRecordKind::GenerationIdentityMismatch { generation, .. }
            | PhysicalEventRecordKind::GenerationBoundPidfdDead { generation, .. }
            | PhysicalEventRecordKind::GenerationCurrentPidfdDead { generation, .. }
            | PhysicalEventRecordKind::GenerationRegistryMismatch { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationTaskGone { generation, .. }
            | PhysicalEventRecordKind::RegisteredCleanupPidfdExited { generation, .. }
            | PhysicalEventRecordKind::EchildPidfdExited { generation, .. }
            | PhysicalEventRecordKind::EchildTracerDetached { generation, .. }
            | PhysicalEventRecordKind::StatusReserved { generation, .. } => {
                mentioned_generations.insert(generation);
                authority_required.insert(generation);
            }
            PhysicalEventRecordKind::WaitAttempt {
                context:
                    PhysicalWaitContext {
                        generation: Some(generation),
                        ..
                    },
                ..
            }
            | PhysicalEventRecordKind::ResumeAttempt {
                context:
                    PhysicalResumeContext {
                        generation: Some(generation),
                        ..
                    },
                ..
            }
            | PhysicalEventRecordKind::PidfdSignalAttempt {
                context: PhysicalPidfdSignalContext { generation, .. },
                ..
            }
            | PhysicalEventRecordKind::StatusPublished {
                generation: Some(generation),
                ..
            } => {
                mentioned_generations.insert(generation);
                authority_required.insert(generation);
            }
            _ => {}
        }
    }
    for generation in mentioned_generations.iter().copied() {
        if resolve_generation(generation, &adoptions).is_none() {
            invalid_adoptions.insert(generation);
        }
    }
    for generation in invalid_adoptions.iter().copied() {
        violations.push(PhysicalPartitionViolation::InvalidGenerationAdoption(
            generation,
        ));
    }

    let mut generation_lifecycles =
        BTreeMap::<PhysicalEventGenerationId, GenerationLifecycleState>::new();
    let mut generation_finish_records = Vec::new();
    let mut worker_start_sequences = BTreeMap::new();
    let mut registered_cleanup_without_worker = Vec::new();
    let mut generation_closed_sequences = BTreeMap::new();
    let mut invalid_generation_lifecycles = BTreeSet::new();
    for record in &snapshot.records {
        let active = |generation: PhysicalEventGenerationId,
                      lifecycles: &BTreeMap<
            PhysicalEventGenerationId,
            GenerationLifecycleState,
        >| {
            matches!(
                lifecycles.get(&generation),
                Some(GenerationLifecycleState::Attached | GenerationLifecycleState::WorkerStarted)
            )
        };
        match record.kind {
            PhysicalEventRecordKind::GenerationAttached(generation) => {
                if generation_lifecycles
                    .insert(generation, GenerationLifecycleState::Attached)
                    .is_some()
                {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::NotifierWorkerStarted(generation) => {
                if generation_lifecycles.get(&generation)
                    != Some(&GenerationLifecycleState::Attached)
                {
                    invalid_generation_lifecycles.insert(generation);
                } else {
                    generation_lifecycles
                        .insert(generation, GenerationLifecycleState::WorkerStarted);
                }
                if worker_start_sequences
                    .insert(generation, record.sequence)
                    .is_some()
                {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::NotifierGenerationFinished {
                generation,
                external_wait,
            } => {
                if !matches!(
                    generation_lifecycles.get(&generation),
                    Some(
                        GenerationLifecycleState::Attached
                            | GenerationLifecycleState::WorkerStarted
                    )
                ) {
                    invalid_generation_lifecycles.insert(generation);
                }
                generation_finish_records.push((generation, external_wait, record.sequence));
                generation_lifecycles.insert(generation, GenerationLifecycleState::Finished);
                generation_closed_sequences
                    .entry(generation)
                    .or_insert(record.sequence);
            }
            PhysicalEventRecordKind::GenerationAdopted { from, to } => {
                if generation_lifecycles.get(&from) != Some(&GenerationLifecycleState::Attached)
                    || !active(to, &generation_lifecycles)
                {
                    invalid_generation_lifecycles.extend([from, to]);
                }
                generation_lifecycles.insert(from, GenerationLifecycleState::Adopted);
                generation_closed_sequences.insert(from, record.sequence);
            }
            PhysicalEventRecordKind::WaitAttempt { id, context } => {
                if let Some(generation) = context.generation {
                    if !active(generation, &generation_lifecycles) {
                        invalid_generation_lifecycles.insert(generation);
                    }
                    match context.producer {
                        PhysicalWaitProducer::NotifierWorker
                        | PhysicalWaitProducer::AuthorizedRootNotifier
                        | PhysicalWaitProducer::PreStopContinuedDrain
                            if generation_lifecycles.get(&generation)
                                != Some(&GenerationLifecycleState::WorkerStarted) =>
                        {
                            invalid_generation_lifecycles.insert(generation);
                        }
                        PhysicalWaitProducer::SynchronousWait
                            if generation_lifecycles.get(&generation)
                                != Some(&GenerationLifecycleState::Attached) =>
                        {
                            invalid_generation_lifecycles.insert(generation);
                        }
                        PhysicalWaitProducer::PreRegistrationCleanup
                        | PhysicalWaitProducer::PreRegistrationBarrier
                        | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                            if generation_lifecycles.get(&generation)
                                != Some(&GenerationLifecycleState::Attached) =>
                        {
                            invalid_generation_lifecycles.insert(generation);
                        }
                        PhysicalWaitProducer::RegisteredCleanup
                            if generation_lifecycles.get(&generation)
                                != Some(&GenerationLifecycleState::WorkerStarted) =>
                        {
                            registered_cleanup_without_worker.push((
                                generation,
                                id,
                                record.sequence,
                            ));
                        }
                        _ => {}
                    }
                }
            }
            PhysicalEventRecordKind::GenerationCaptureFailed { generation, .. } => {
                if generation_lifecycles.get(&generation)
                    != Some(&GenerationLifecycleState::Attached)
                {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::ContinuedAuthorityEnabled { generation, .. }
            | PhysicalEventRecordKind::OriginalRootLaunchLinked { generation, .. }
            | PhysicalEventRecordKind::StartupBarrierFallbackPrepared { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupPrepared { generation, .. } => {
                if generation_lifecycles.get(&generation)
                    != Some(&GenerationLifecycleState::Attached)
                {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::ContinuedAuthorityRevoked { generation }
            | PhysicalEventRecordKind::StartupBarrierFallbackReleased { generation, .. }
            | PhysicalEventRecordKind::PreStopContinuedDrainCompleted { generation, .. }
            | PhysicalEventRecordKind::PreStopContinuedDrainFailed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionWatchArmed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionFirstStopped { generation, .. }
            | PhysicalEventRecordKind::StopResolutionGroupAcknowledged { generation, .. }
            | PhysicalEventRecordKind::StopResolutionContinuedClaimed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionWatchClosed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionResumeCausallyClosed { generation, .. } => {
                if generation_lifecycles.get(&generation)
                    != Some(&GenerationLifecycleState::WorkerStarted)
                {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::GenerationIdentityMismatch { generation, .. }
            | PhysicalEventRecordKind::GenerationBoundPidfdDead { generation, .. }
            | PhysicalEventRecordKind::GenerationCurrentPidfdDead { generation, .. }
            | PhysicalEventRecordKind::GenerationRegistryMismatch { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationTaskGone { generation, .. } => {
                if generation_lifecycles.get(&generation)
                    != Some(&GenerationLifecycleState::Attached)
                {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::PreRegistrationLinked { generation, .. }
            | PhysicalEventRecordKind::IdentityBound { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationBarrierConsumed { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationBarrierFailureLinked { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationBarrierStatuslessFailureLinked {
                generation,
                ..
            }
            | PhysicalEventRecordKind::PreRegistrationBarrierStatuslessCleanupResolved {
                generation,
                ..
            }
            | PhysicalEventRecordKind::PreRegistrationBarrierSetupFailed { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupLinked { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupNoStatusLinked { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupPidfdExitProved { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupWaitFailureExitProved { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupResumeFailureExitProved {
                generation, ..
            }
            | PhysicalEventRecordKind::StartupCleanupExecutorTransferred { generation, .. }
            | PhysicalEventRecordKind::ExecGenerationBound { generation, .. }
            | PhysicalEventRecordKind::SyntheticEchildPublished { generation, .. } => {
                if !active(generation, &generation_lifecycles) {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::StatusReserved { generation, .. } => {
                if !active(generation, &generation_lifecycles)
                    && generation_lifecycles.get(&generation)
                        != Some(&GenerationLifecycleState::Finished)
                {
                    invalid_generation_lifecycles.insert(generation);
                }
            }
            PhysicalEventRecordKind::StatusPublished {
                generation: Some(generation),
                ..
            }
            | PhysicalEventRecordKind::RegisteredCleanupPidfdExited { generation, .. }
            | PhysicalEventRecordKind::EchildPidfdExited { generation, .. }
            | PhysicalEventRecordKind::EchildTracerDetached { generation, .. }
            | PhysicalEventRecordKind::ResumeAttempt {
                context:
                    PhysicalResumeContext {
                        generation: Some(generation),
                        ..
                    },
                ..
            }
            | PhysicalEventRecordKind::PidfdSignalAttempt {
                context: PhysicalPidfdSignalContext { generation, .. },
                ..
            } if !active(generation, &generation_lifecycles) => {
                invalid_generation_lifecycles.insert(generation);
            }
            _ => {}
        }
    }
    for (generation, state) in &generation_lifecycles {
        if matches!(
            state,
            GenerationLifecycleState::Attached | GenerationLifecycleState::WorkerStarted
        ) {
            invalid_generation_lifecycles.insert(*generation);
        }
    }
    let mut authorities = BTreeMap::<PhysicalEventGenerationId, PhysicalTaskIdentity>::new();
    let mut identity_binding_sequences =
        BTreeMap::<PhysicalEventGenerationId, Vec<(PhysicalTaskIdentity, u64)>>::new();
    let mut pre_registration_link_sequences =
        BTreeMap::<PhysicalEventGenerationId, Vec<(PhysicalTaskIdentity, u64)>>::new();
    let mut conflicting_authorities = BTreeSet::new();
    let mut raw_identity_bindings = BTreeSet::new();
    for record in &snapshot.records {
        let (generation, task, valid_shape, identity_binding, pre_registration_link) =
            match record.kind {
                PhysicalEventRecordKind::OriginalRootLaunchLinked {
                    task, generation, ..
                } => (generation, task, task.is_direct_child(), false, false),
                PhysicalEventRecordKind::PreRegistrationLinked { task, generation } => {
                    (generation, task, task.is_direct_child(), false, true)
                }
                PhysicalEventRecordKind::IdentityBound { generation, task } => {
                    (generation, task, task.is_captured(), true, false)
                }
                PhysicalEventRecordKind::GenerationCaptureFailed {
                    generation,
                    task,
                    error,
                } if task.is_direct_child() => (
                    generation,
                    task,
                    matches!(error, libc::ENOENT | libc::ESRCH),
                    false,
                    false,
                ),
                PhysicalEventRecordKind::GenerationCaptureFailed { .. } => continue,
                _ => continue,
            };
        let Some(canonical) = canonical_generation(generation, &adoptions, &invalid_adoptions)
        else {
            continue;
        };
        if !valid_shape {
            conflicting_authorities.insert(canonical);
            continue;
        }
        if identity_binding {
            if !raw_identity_bindings.insert(generation) {
                violations.push(PhysicalPartitionViolation::DuplicateIdentityBinding(
                    generation,
                ));
            }
            identity_binding_sequences
                .entry(canonical)
                .or_default()
                .push((task, record.sequence));
        }
        if pre_registration_link {
            pre_registration_link_sequences
                .entry(canonical)
                .or_default()
                .push((task, record.sequence));
        }
        match authorities.get(&canonical).copied() {
            Some(existing) => match existing.merge_authority(task) {
                Some(merged) => {
                    authorities.insert(canonical, merged);
                }
                None => {
                    conflicting_authorities.insert(canonical);
                }
            },
            None => {
                authorities.insert(canonical, task);
            }
        }
    }
    for generation in conflicting_authorities.iter().copied() {
        violations.push(PhysicalPartitionViolation::ConflictingTaskAuthority(
            generation,
        ));
        authorities.remove(&generation);
    }
    for (generation, links) in &pre_registration_link_sequences {
        if links.len() > 1 {
            violations.push(PhysicalPartitionViolation::DuplicatePreRegistrationLink(
                *generation,
            ));
        }
    }
    // Generic direct-child links are deliberately insufficient here: a sole
    // CLONE_PARENT descendant can have the same scalar procfs shape as the
    // original child.  Only the opaque token minted while attaching the exact
    // `Running` returned from Command::spawn may authorize startup.
    let mut original_root_launch_ids = BTreeSet::new();
    let original_root_launches = snapshot
        .records
        .iter()
        .filter_map(|record| match record.kind {
            PhysicalEventRecordKind::OriginalRootLaunchLinked {
                link,
                generation,
                task,
                controller_tgid,
                controller_tid,
                controller_sequence,
            } => Some((
                link,
                generation,
                task,
                controller_tgid,
                controller_tid,
                controller_sequence,
                record.sequence,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let global_first_link_sequence = original_root_launches
        .iter()
        .map(|(_, _, _, _, _, _, sequence)| *sequence)
        .chain(
            pre_registration_link_sequences
                .values()
                .flat_map(|links| links.iter().map(|(_, sequence)| *sequence)),
        )
        .min();
    let original_root_launch = if original_root_launches.len() == 1 {
        let (
            link,
            generation,
            task,
            controller_tgid,
            controller_tid,
            controller_sequence,
            sequence,
        ) = original_root_launches[0];
        let valid = original_root_launch_ids.insert(link)
            && task.is_direct_child()
            && controller_tgid.as_raw() > 0
            && controller_tid.as_raw() > 0
            && controller_sequence != 0
            && canonical_generation(generation, &adoptions, &invalid_adoptions) == Some(generation)
            && global_first_link_sequence == Some(sequence);
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidOriginalRootLaunch(
                generation,
            ));
            None
        } else {
            Some((
                link,
                generation,
                task,
                controller_tgid,
                controller_tid,
                sequence,
            ))
        }
    } else {
        for (link, generation, ..) in &original_root_launches {
            if !original_root_launch_ids.insert(*link) {
                violations.push(PhysicalPartitionViolation::InvalidOriginalRootLaunch(
                    *generation,
                ));
            }
        }
        if let Some((_, generation, ..)) = original_root_launches.first() {
            violations.push(PhysicalPartitionViolation::InvalidOriginalRootLaunch(
                *generation,
            ));
        }
        None
    };
    let exact_original_root_launch_captured =
        |expected_launch: Option<PhysicalOriginalRootLaunchId>,
         generation: PhysicalEventGenerationId,
         task: PhysicalTaskIdentity,
         before_sequence: u64| {
            original_root_launch.is_some_and(
                |(
                    launch,
                    launch_generation,
                    launch_task,
                    controller_tgid,
                    controller_tid,
                    launch_sequence,
                )| {
                    expected_launch.is_none_or(|expected| expected == launch)
                        && launch_generation == generation
                        && launch_sequence < before_sequence
                        && launch_task.is_direct_child()
                        && task.is_captured()
                        && task.tid == launch_task.tid
                        && task.tgid == Some(launch_task.tid)
                        && task.ppid == Some(controller_tgid.as_raw())
                        && task.tracer_pid.is_some_and(|tracer_pid| {
                            tracer_pid == 0 || tracer_pid == controller_tid.as_raw()
                        })
                },
            )
        };
    let exact_original_root_launch_pidfd_direct =
        |expected_launch: PhysicalOriginalRootLaunchId,
         generation: PhysicalEventGenerationId,
         task: PhysicalTaskIdentity,
         before_sequence: u64| {
            original_root_launch.is_some_and(
                |(launch, launch_generation, launch_task, _, _, launch_sequence)| {
                    launch == expected_launch
                        && launch_generation == generation
                        && launch_sequence < before_sequence
                        && launch_task.is_direct_child()
                        && task.is_pidfd_bound_direct_child()
                        && task.tid == launch_task.tid
                },
            )
        };
    let mut missing_authorities = BTreeSet::new();
    for generation in authority_required {
        if let Some(canonical) = canonical_generation(generation, &adoptions, &invalid_adoptions)
            && !authorities.contains_key(&canonical)
            && missing_authorities.insert(canonical)
        {
            violations.push(PhysicalPartitionViolation::MissingTaskAuthority(canonical));
        }
    }

    let mut continued_authorities =
        BTreeMap::<PhysicalEventGenerationId, ContinuedAuthorityTrack>::new();
    let mut invalid_continued_authorities = BTreeSet::new();
    let mut continued_authority_tasks =
        Vec::<(PhysicalTaskIdentity, PhysicalEventGenerationId)>::new();
    for record in &snapshot.records {
        match record.kind {
            PhysicalEventRecordKind::ContinuedAuthorityEnabled {
                generation,
                root,
                controller_tgid,
                controller_tracer_tid,
            } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let authority =
                    canonical.and_then(|canonical| authorities.get(&canonical).copied());
                let identity_precedes = canonical.is_some_and(|canonical| {
                    identity_binding_sequences
                        .get(&canonical)
                        .is_some_and(|bindings| {
                            bindings.iter().any(|(task, sequence)| {
                                *sequence < record.sequence
                                    && task.tid == root
                                    && task.tgid == Some(root)
                                    && task.ppid == Some(controller_tgid)
                                    && task.tracer_pid == Some(controller_tracer_tid)
                            })
                        })
                });
                let launch_precedes = canonical.is_some_and(|canonical| {
                    original_root_launch.is_some_and(
                        |(
                            _,
                            launch_generation,
                            launch_task,
                            launch_controller_tgid,
                            launch_controller_tid,
                            launch_sequence,
                        )| {
                            launch_generation == canonical
                                && launch_task.tid == root
                                && launch_controller_tgid.as_raw() == controller_tgid
                                && launch_controller_tid.as_raw() == controller_tracer_tid
                                && launch_sequence < record.sequence
                        },
                    )
                });
                let before_worker = worker_start_sequences
                    .get(&generation)
                    .is_none_or(|started| record.sequence < *started);
                let valid = canonical == Some(generation)
                    && root > 0
                    && controller_tgid > 0
                    && controller_tracer_tid > 0
                    && authority.is_some_and(|task| {
                        task.is_captured()
                            && task.tid == root
                            && task.tgid == Some(root)
                            && task.ppid == Some(controller_tgid)
                            && task.tracer_pid == Some(controller_tracer_tid)
                    })
                    && identity_precedes
                    && launch_precedes
                    && before_worker;
                if !valid
                    || continued_authorities
                        .insert(
                            generation,
                            ContinuedAuthorityTrack {
                                root,
                                controller_tgid,
                                controller_tracer_tid,
                                enabled_sequence: record.sequence,
                                revoked_sequence: None,
                            },
                        )
                        .is_some()
                {
                    invalid_continued_authorities.insert(generation);
                }
                if let Some(task) = authority {
                    if continued_authority_tasks.iter().any(|(existing, owner)| {
                        *owner != generation && existing.same_stable_task(task)
                    }) {
                        invalid_continued_authorities.insert(generation);
                    }
                    continued_authority_tasks.push((task, generation));
                }
            }
            PhysicalEventRecordKind::ContinuedAuthorityRevoked { generation } => {
                let valid = continued_authorities
                    .get_mut(&generation)
                    .is_some_and(|track| {
                        if track.revoked_sequence.is_some()
                            || record.sequence <= track.enabled_sequence
                            || worker_start_sequences
                                .get(&generation)
                                .is_none_or(|started| *started >= record.sequence)
                        {
                            return false;
                        }
                        track.revoked_sequence = Some(record.sequence);
                        true
                    });
                if !valid {
                    invalid_continued_authorities.insert(generation);
                }
            }
            _ => {}
        }
    }
    for (_, generation) in continued_authority_tasks {
        if invalid_continued_authorities.contains(&generation) {
            continue;
        }
        let Some(track) = continued_authorities.get(&generation) else {
            continue;
        };
        if track.root <= 0 || track.controller_tgid <= 0 || track.controller_tracer_tid <= 0 {
            invalid_continued_authorities.insert(generation);
        }
    }
    if continued_authorities.len() > 1 {
        invalid_continued_authorities.extend(continued_authorities.keys().copied());
    }
    for generation in invalid_continued_authorities.iter().copied() {
        violations.push(PhysicalPartitionViolation::InvalidContinuedAuthority(
            generation,
        ));
    }

    let mut valid_generation_invalidations = BTreeMap::new();
    for record in &snapshot.records {
        let (generation, valid) = match record.kind {
            PhysicalEventRecordKind::GenerationCaptureFailed {
                generation,
                task,
                error,
            } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let valid = matches!(error, libc::ENOENT | libc::ESRCH)
                    && canonical.is_some_and(|canonical| {
                        let prior_bindings = identity_binding_sequences.get(&canonical);
                        let unbound_direct = task.is_direct_child()
                            && authorities.get(&canonical) == Some(&task)
                            && prior_bindings.is_none_or(|bindings| {
                                bindings
                                    .iter()
                                    .all(|(_, sequence)| *sequence >= record.sequence)
                            });
                        let exactly_bound = task.is_captured()
                            && authorities
                                .get(&canonical)
                                .is_some_and(|authority| authority.authorizes(task, false))
                            && prior_bindings.is_some_and(|bindings| {
                                bindings.iter().any(|(bound, sequence)| {
                                    *sequence < record.sequence && *bound == task
                                })
                            });
                        unbound_direct || exactly_bound
                    });
                (generation, valid)
            }
            PhysicalEventRecordKind::GenerationIdentityMismatch {
                generation,
                bound,
                current,
            } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let valid = bound.is_captured()
                    && current.is_captured()
                    && bound.tid == current.tid
                    && !bound.same_stable_task(current)
                    && canonical.is_some_and(|canonical| {
                        authorities
                            .get(&canonical)
                            .is_some_and(|authority| authority.authorizes(bound, false))
                            && identity_binding_sequences
                                .get(&canonical)
                                .is_some_and(|bindings| {
                                    bindings.iter().any(|(task, sequence)| {
                                        *sequence < record.sequence && task.authorizes(bound, false)
                                    })
                                })
                    });
                (generation, valid)
            }
            PhysicalEventRecordKind::GenerationBoundPidfdDead { generation, bound } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let valid = bound.is_captured()
                    && canonical.is_some_and(|canonical| {
                        authorities
                            .get(&canonical)
                            .is_some_and(|authority| authority.authorizes(bound, false))
                            && identity_binding_sequences
                                .get(&canonical)
                                .is_some_and(|bindings| {
                                    bindings.iter().any(|(task, sequence)| {
                                        *sequence < record.sequence && *task == bound
                                    })
                                })
                    });
                (generation, valid)
            }
            PhysicalEventRecordKind::GenerationCurrentPidfdDead {
                generation,
                current,
            } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let valid = current.is_captured()
                    && canonical.is_some_and(|canonical| {
                        authorities
                            .get(&canonical)
                            .is_some_and(|authority| authority.authorizes(current, false))
                            && identity_binding_sequences
                                .get(&canonical)
                                .is_some_and(|bindings| {
                                    bindings.iter().any(|(task, sequence)| {
                                        *sequence < record.sequence
                                            && task.authorizes(current, false)
                                    })
                                })
                    });
                (generation, valid)
            }
            PhysicalEventRecordKind::GenerationRegistryMismatch {
                generation,
                registered,
                current,
            } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let valid = registered.is_captured()
                    && current.is_captured()
                    && registered.tid == current.tid
                    && !registered.same_stable_task(current)
                    && canonical.is_some_and(|canonical| {
                        authorities
                            .get(&canonical)
                            .is_some_and(|authority| authority.authorizes(registered, false))
                            && identity_binding_sequences
                                .get(&canonical)
                                .is_some_and(|bindings| {
                                    bindings.iter().any(|(task, sequence)| {
                                        *sequence < record.sequence
                                            && task.authorizes(registered, false)
                                    })
                                })
                    });
                (generation, valid)
            }
            _ => continue,
        };
        let Some(canonical) = canonical_generation(generation, &adoptions, &invalid_adoptions)
        else {
            invalid_generation_lifecycles.insert(generation);
            continue;
        };
        if !valid
            || valid_generation_invalidations
                .insert(canonical, record.sequence)
                .is_some()
        {
            invalid_generation_lifecycles.insert(generation);
        }
    }

    let mut valid_pre_registration_task_gone =
        BTreeMap::<PhysicalEventGenerationId, Vec<(PhysicalTaskIdentity, u64)>>::new();
    for record in &snapshot.records {
        let PhysicalEventRecordKind::PreRegistrationTaskGone {
            generation,
            task,
            error,
        } = record.kind
        else {
            continue;
        };
        let Some(canonical) = canonical_generation(generation, &adoptions, &invalid_adoptions)
        else {
            invalid_generation_lifecycles.insert(generation);
            continue;
        };
        let authority_matches = authorities
            .get(&canonical)
            .is_some_and(|authority| authority.authorizes(task, true));
        let provenance_precedes = if task.is_direct_child() {
            pre_registration_link_sequences
                .get(&canonical)
                .is_some_and(|links| {
                    links
                        .iter()
                        .any(|(linked, sequence)| *sequence < record.sequence && *linked == task)
                })
        } else if task.is_captured() {
            identity_binding_sequences
                .get(&canonical)
                .is_some_and(|bindings| {
                    bindings.iter().any(|(bound, sequence)| {
                        *sequence < record.sequence && bound.authorizes(task, false)
                    })
                })
        } else {
            false
        };
        if error == libc::ESRCH && authority_matches && provenance_precedes {
            valid_pre_registration_task_gone
                .entry(canonical)
                .or_default()
                .push((task, record.sequence));
        } else {
            invalid_generation_lifecycles.insert(generation);
        }
    }

    let mut wait_attempts = BTreeMap::<PhysicalWaitAttemptId, PhysicalWaitContext>::new();
    let mut wait_attempt_sequences = BTreeMap::<PhysicalWaitAttemptId, u64>::new();
    let mut wait_generations = BTreeMap::<PhysicalWaitAttemptId, PhysicalEventGenerationId>::new();
    let mut wait_siginfos =
        BTreeMap::<PhysicalWaitAttemptId, (PhysicalWaitSiginfo, Option<PhysicalStatusId>)>::new();
    let mut wait_siginfo_sequences = BTreeMap::<PhysicalWaitAttemptId, u64>::new();
    let mut siginfo_status_attempts = BTreeMap::<PhysicalStatusId, PhysicalWaitAttemptId>::new();
    let mut wait_results = BTreeSet::new();
    let mut wait_outcomes = BTreeMap::<PhysicalWaitAttemptId, PhysicalWaitOutcome>::new();
    let mut wait_result_sequences = BTreeMap::<PhysicalWaitAttemptId, u64>::new();
    let mut statuses = BTreeMap::<PhysicalStatusId, StatusTrack>::new();
    let mut cleanup_transactions =
        BTreeMap::<PhysicalCleanupTransactionId, CleanupTransactionTrack>::new();
    let mut cleanup_status_transactions =
        BTreeMap::<PhysicalStatusId, PhysicalCleanupTransactionId>::new();
    let mut cleanup_resume_transactions =
        BTreeMap::<PhysicalResumeAttemptId, PhysicalCleanupTransactionId>::new();
    let mut cleanup_terminal_wait_transactions =
        BTreeMap::<PhysicalWaitAttemptId, PhysicalCleanupTransactionId>::new();
    let mut cleanup_cause_wait_transactions =
        BTreeMap::<PhysicalWaitAttemptId, PhysicalCleanupTransactionId>::new();
    let mut pre_stop_drain_failure_records = Vec::<(
        PhysicalEventGenerationId,
        PhysicalTaskIdentity,
        PhysicalStatusId,
        PhysicalWaitAttemptId,
        PhysicalCleanupTransactionId,
        u64,
    )>::new();
    let mut reservations = BTreeMap::<PhysicalReservationId, ReservationTrack>::new();
    let mut reservation_generations =
        BTreeMap::<PhysicalReservationId, PhysicalEventGenerationId>::new();
    let mut resume_attempts = BTreeMap::<PhysicalResumeAttemptId, PhysicalResumeContext>::new();
    let mut resume_attempt_sequences = BTreeMap::<PhysicalResumeAttemptId, u64>::new();
    let mut resume_generations =
        BTreeMap::<PhysicalResumeAttemptId, PhysicalEventGenerationId>::new();
    let mut resume_results = BTreeMap::<PhysicalResumeAttemptId, PhysicalResumeOutcome>::new();
    let mut resume_result_sequences = BTreeMap::<PhysicalResumeAttemptId, u64>::new();
    let mut pidfd_signal_attempts =
        BTreeMap::<PhysicalPidfdSignalAttemptId, PhysicalPidfdSignalContext>::new();
    let mut pidfd_signal_attempt_sequences = BTreeMap::<PhysicalPidfdSignalAttemptId, u64>::new();
    let mut pidfd_signal_results =
        BTreeMap::<PhysicalPidfdSignalAttemptId, PhysicalPidfdSignalOutcome>::new();
    let mut pidfd_signal_result_sequences = BTreeMap::<PhysicalPidfdSignalAttemptId, u64>::new();
    let mut startup_pidfd_exit_proofs = BTreeMap::<
        PhysicalCleanupTransactionId,
        (
            PhysicalEventGenerationId,
            PhysicalTaskIdentity,
            i32,
            i16,
            u64,
        ),
    >::new();
    let mut startup_wait_failure_exit_proofs = BTreeMap::<
        PhysicalCleanupTransactionId,
        (
            PhysicalEventGenerationId,
            PhysicalTaskIdentity,
            i32,
            PhysicalWaitAttemptId,
            i32,
            i16,
            u64,
        ),
    >::new();
    let mut startup_resume_failure_exit_proofs = BTreeMap::<
        PhysicalCleanupTransactionId,
        (
            PhysicalEventGenerationId,
            PhysicalTaskIdentity,
            i32,
            PhysicalResumeAttemptId,
            PhysicalStatusId,
            i32,
            i16,
            u64,
        ),
    >::new();
    let mut startup_executor_transfers = BTreeMap::<
        PhysicalCleanupTransactionId,
        (PhysicalEventGenerationId, PhysicalTaskIdentity, u64),
    >::new();
    let mut tolerated = BTreeMap::<PhysicalResumeAttemptId, i32>::new();
    let mut tolerated_sequences = BTreeMap::<PhysicalResumeAttemptId, u64>::new();
    let mut ambiguous_resume_resolutions = Vec::<(
        PhysicalStatusId,
        PhysicalResumeAttemptId,
        PhysicalAmbiguousResumeProof,
        u64,
    )>::new();
    let mut exit_capabilities = BTreeMap::<PhysicalStatusId, ExitCapabilityTrack>::new();
    let mut live_workers = BTreeSet::<PhysicalEventGenerationId>::new();
    let mut live_generations = BTreeSet::<PhysicalEventGenerationId>::new();
    let mut external_generation_finishes =
        Vec::<(PhysicalEventGenerationId, PhysicalWaitAttemptId, u64)>::new();
    let mut external_finish_wait_owners =
        BTreeMap::<PhysicalWaitAttemptId, PhysicalEventGenerationId>::new();
    let mut synthetic_echild_evidence =
        BTreeMap::<PhysicalWaitAttemptId, (PhysicalEventGenerationId, u64)>::new();
    let mut echild_terminal_proofs = BTreeMap::<
        PhysicalWaitAttemptId,
        (PhysicalEventGenerationId, PhysicalTaskIdentity, u64),
    >::new();
    let mut used_echild_terminal_proofs = BTreeSet::<PhysicalWaitAttemptId>::new();
    let mut uncaused_synthetic_echild = BTreeMap::<PhysicalEventGenerationId, Vec<u64>>::new();
    let mut startup_barrier_consumptions = Vec::<(
        PhysicalEventGenerationId,
        PhysicalWaitAttemptId,
        PhysicalWaitAttemptId,
        PhysicalStatusId,
        u64,
    )>::new();
    let mut startup_barrier_failures = Vec::<(
        PhysicalEventGenerationId,
        PhysicalWaitAttemptId,
        PhysicalWaitAttemptId,
        PhysicalStatusId,
        PhysicalCleanupTransactionId,
        u64,
    )>::new();
    let mut startup_barrier_statusless_failures = Vec::<(
        PhysicalEventGenerationId,
        PhysicalWaitAttemptId,
        PhysicalWaitAttemptId,
        i32,
        PhysicalCleanupTransactionId,
        u64,
    )>::new();
    let mut startup_barrier_statusless_resolutions = Vec::<(
        PhysicalEventGenerationId,
        PhysicalWaitAttemptId,
        PhysicalWaitAttemptId,
        Option<PhysicalStatusId>,
        PhysicalCleanupTransactionId,
        u64,
    )>::new();
    let mut startup_barrier_setup_failures = Vec::<(
        PhysicalEventGenerationId,
        PhysicalTaskIdentity,
        i32,
        PhysicalOriginalRootLaunchId,
        u64,
    )>::new();
    let mut startup_setup_prepared = Vec::<(
        PhysicalEventGenerationId,
        PhysicalTaskIdentity,
        i32,
        PhysicalCleanupTransactionId,
        PhysicalOriginalRootLaunchId,
        u64,
    )>::new();
    let mut startup_setup_linked = Vec::<(
        PhysicalEventGenerationId,
        i32,
        PhysicalWaitAttemptId,
        PhysicalStatusId,
        PhysicalCleanupTransactionId,
        PhysicalOriginalRootLaunchId,
        u64,
    )>::new();
    let mut startup_setup_no_status_linked = Vec::<(
        PhysicalEventGenerationId,
        i32,
        PhysicalWaitAttemptId,
        PhysicalCleanupTransactionId,
        PhysicalOriginalRootLaunchId,
        u64,
    )>::new();
    let mut stop_resolution_arms =
        BTreeMap::<PhysicalStatusId, (PhysicalEventGenerationId, u64)>::new();
    let mut stop_resolution_first = BTreeMap::<PhysicalStatusId, (PhysicalStatusId, u64)>::new();
    let mut stop_resolution_acks = BTreeMap::<PhysicalStatusId, (PhysicalStatusId, u64)>::new();
    let mut stop_resolution_claims = BTreeMap::<PhysicalStatusId, (PhysicalStatusId, u64)>::new();
    let mut stop_resolution_closes = BTreeMap::<PhysicalStatusId, u64>::new();
    let mut stop_resolution_causal_closes = Vec::<(
        PhysicalEventGenerationId,
        PhysicalStatusId,
        PhysicalStatusId,
        PhysicalStatusId,
        PhysicalResumeAttemptId,
        PhysicalStatusId,
        u64,
    )>::new();

    for record in &snapshot.records {
        match record.kind {
            PhysicalEventRecordKind::GenerationAttached(generation) => {
                live_generations.insert(generation);
            }
            PhysicalEventRecordKind::GenerationAdopted { from, .. } => {
                live_generations.remove(&from);
            }
            PhysicalEventRecordKind::WaitAttempt { id, context } => {
                wait_attempts.insert(id, context);
                wait_attempt_sequences.insert(id, record.sequence);
                if context.flags != production_wait_flags(context.producer) {
                    violations.push(PhysicalPartitionViolation::InvalidWaitFlags(id));
                }
                let generation = context.generation.and_then(|generation| {
                    canonical_generation(generation, &adoptions, &invalid_adoptions)
                });
                let Some(generation) = generation else {
                    violations.push(PhysicalPartitionViolation::WaitWithoutGeneration(id));
                    continue;
                };
                wait_generations.insert(id, generation);
                let continued_authority = continued_authorities.get(&generation);
                let continued_mode_valid = match context.producer {
                    PhysicalWaitProducer::AuthorizedRootNotifier => continued_authority
                        .is_some_and(|authority| authority.enabled_sequence < record.sequence),
                    PhysicalWaitProducer::PreStopContinuedDrain => {
                        continued_authority.is_some_and(|authority| {
                            authority.enabled_sequence < record.sequence
                                && authority
                                    .revoked_sequence
                                    .is_none_or(|revoked| record.sequence < revoked)
                        })
                    }
                    PhysicalWaitProducer::NotifierWorker
                    | PhysicalWaitProducer::SynchronousWait => continued_authority.is_none(),
                    PhysicalWaitProducer::PreRegistrationBarrier => continued_authority
                        .is_none_or(|authority| record.sequence < authority.enabled_sequence),
                    PhysicalWaitProducer::PreRegistrationCleanup
                    | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                    | PhysicalWaitProducer::RegisteredCleanup => true,
                };
                if !continued_mode_valid {
                    violations.push(PhysicalPartitionViolation::InvalidContinuedAuthority(
                        generation,
                    ));
                }
                let allow_direct_narrowing = matches!(
                    context.producer,
                    PhysicalWaitProducer::PreRegistrationCleanup
                        | PhysicalWaitProducer::PreRegistrationBarrier
                        | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                );
                let original_root_launch_was_linked = original_root_launch.is_some_and(
                    |(
                        _,
                        launch_generation,
                        launch_task,
                        controller_tgid,
                        controller_tid,
                        launch_sequence,
                    )| {
                        let captured_root = context.task.is_captured()
                            && context.task.tid == launch_task.tid
                            && context.task.tgid == Some(launch_task.tid)
                            && context.task.ppid == Some(controller_tgid.as_raw())
                            && context.task.tracer_pid.is_some_and(|tracer_pid| {
                                tracer_pid == 0 || tracer_pid == controller_tid.as_raw()
                            });
                        context.generation == Some(launch_generation)
                            && launch_task.is_direct_child()
                            && context.task.tid == launch_task.tid
                            && launch_sequence < record.sequence
                            && match context.producer {
                                PhysicalWaitProducer::PreRegistrationBarrier => captured_root,
                                PhysicalWaitProducer::PreRegistrationBarrierCleanup => {
                                    captured_root || context.task.is_pidfd_bound_direct_child()
                                }
                                PhysicalWaitProducer::PreRegistrationCleanup
                                | PhysicalWaitProducer::AuthorizedRootNotifier
                                | PhysicalWaitProducer::PreStopContinuedDrain
                                | PhysicalWaitProducer::NotifierWorker
                                | PhysicalWaitProducer::SynchronousWait
                                | PhysicalWaitProducer::RegisteredCleanup => false,
                            }
                    },
                );
                if !authorities.get(&generation).is_some_and(|authority| {
                    authority.authorizes(context.task, allow_direct_narrowing)
                }) && !original_root_launch_was_linked
                {
                    violations.push(PhysicalPartitionViolation::WrongWaitTask(id));
                }
                let pre_registration_was_linked = pre_registration_link_sequences
                    .get(&generation)
                    .is_some_and(|links| {
                        links.iter().any(|(linked, sequence)| {
                            *sequence < record.sequence
                                && (linked.authorizes(context.task, true)
                                    || linked.tid == context.task.tid)
                        })
                    });
                let identity_was_bound =
                    identity_binding_sequences
                        .get(&generation)
                        .is_some_and(|bindings| {
                            bindings.iter().any(|(bound, sequence)| {
                                *sequence < record.sequence
                                    && bound.authorizes(context.task, allow_direct_narrowing)
                            })
                        });
                let operation_was_authorized_in_time = match context.producer {
                    PhysicalWaitProducer::PreRegistrationCleanup => {
                        (context.task.is_direct_child() && pre_registration_was_linked)
                            || identity_was_bound
                    }
                    PhysicalWaitProducer::PreRegistrationBarrier
                    | PhysicalWaitProducer::PreRegistrationBarrierCleanup => {
                        original_root_launch_was_linked
                    }
                    PhysicalWaitProducer::AuthorizedRootNotifier
                    | PhysicalWaitProducer::PreStopContinuedDrain
                    | PhysicalWaitProducer::NotifierWorker
                    | PhysicalWaitProducer::SynchronousWait
                    | PhysicalWaitProducer::RegisteredCleanup => identity_was_bound,
                };
                if !operation_was_authorized_in_time {
                    violations.push(PhysicalPartitionViolation::WaitBeforeIdentityBound(id));
                }
            }
            PhysicalEventRecordKind::NotifierWorkerStarted(generation) => {
                live_workers.insert(generation);
            }
            PhysicalEventRecordKind::NotifierGenerationFinished {
                generation,
                external_wait,
            } => {
                live_workers.remove(&generation);
                live_generations.remove(&generation);
                if let Some(attempt) = external_wait {
                    external_generation_finishes.push((generation, attempt, record.sequence));
                    if external_finish_wait_owners
                        .insert(attempt, generation)
                        .is_some()
                    {
                        violations.push(
                            PhysicalPartitionViolation::InvalidGenerationFinishEvidence(attempt),
                        );
                    }
                    let canonical =
                        canonical_generation(generation, &adoptions, &invalid_adoptions);
                    let context = wait_attempts.get(&attempt);
                    let valid_outcome = match wait_outcomes.get(&attempt) {
                        Some(PhysicalWaitOutcome::NoChild) => canonical.is_some_and(|canonical| {
                            context.is_some_and(|context| {
                                wait_result_sequences.get(&attempt).is_some_and(|result| {
                                    let pre_registration_gone = valid_pre_registration_task_gone
                                        .get(&canonical)
                                        .is_some_and(|evidence| {
                                            evidence.iter().any(|(task, sequence)| {
                                                context.task.authorizes(*task, true)
                                                    && result < sequence
                                                    && *sequence < record.sequence
                                            })
                                        });
                                    let typed_startup_cleanup = cleanup_transactions.values().any(
                                        |cleanup| {
                                            cleanup.starts == 1
                                                && cleanup.terminal_wait == Some(attempt)
                                                && (matches!(
                                                    cleanup.kind,
                                                    Some(
                                                        PhysicalCleanupTransactionKind::StartupBarrier {
                                                            generation: cleanup_generation,
                                                            owner: PhysicalStartupCleanupOwner::Unstarted,
                                                            ..
                                                        }
                                                    ) if cleanup_generation == canonical
                                                ) || matches!(
                                                    cleanup.kind,
                                                    Some(
                                                        PhysicalCleanupTransactionKind::StartupSetup {
                                                            generation: cleanup_generation,
                                                            ..
                                                        }
                                                    ) if cleanup_generation == canonical
                                                ))
                                                && cleanup.completion_sequence.is_some_and(
                                                    |completed| {
                                                        cleanup.pidfd_exit_proof.is_some_and(
                                                            |(
                                                                proof_wait,
                                                                proof_generation,
                                                                proof_task,
                                                                revents,
                                                                proof_launch,
                                                                proof_sequence,
                                                            )| {
                                                                proof_wait == attempt
                                                                    && proof_generation == canonical
                                                                    && proof_task == context.task
                                                                    && (proof_task.is_captured()
                                                                        || matches!(
                                                                            cleanup.kind,
                                                                            Some(PhysicalCleanupTransactionKind::StartupSetup {
                                                                                generation,
                                                                                task,
                                                                                launch,
                                                                                ..
                                                                            }) if generation == canonical
                                                                                && task == proof_task
                                                                                && proof_task.is_pidfd_bound_direct_child()
                                                                                && proof_launch == Some(launch)
                                                                                && original_root_launch.is_some_and(
                                                                                    |(root_launch, root_generation, root_task, _, _, root_sequence)| {
                                                                                        root_launch == launch
                                                                                            && root_generation == generation
                                                                                            && root_task.authorizes(proof_task, true)
                                                                                            && root_sequence < proof_sequence
                                                                                    }
                                                                                )
                                                                        ))
                                                                    && revents & libc::POLLIN != 0
                                                                    && *result < proof_sequence
                                                                    && proof_sequence < completed
                                                                    && completed < record.sequence
                                                            },
                                                        )
                                                    },
                                                )
                                        },
                                    );
                                    pre_registration_gone || typed_startup_cleanup
                                })
                            })
                        }),
                        Some(PhysicalWaitOutcome::Status { raw_status, .. }) => {
                            libc::WIFEXITED(*raw_status) || libc::WIFSIGNALED(*raw_status)
                        }
                        Some(PhysicalWaitOutcome::UndecodableStatus { siginfo, .. }) => {
                            siginfo_is_startup_typed_unsupported_terminal(*siginfo)
                        }
                        _ => false,
                    };
                    let valid_context = canonical.is_some_and(|canonical| {
                        wait_generations.get(&attempt) == Some(&canonical)
                            && context.is_some_and(|context| {
                                matches!(
                                    context.producer,
                                    PhysicalWaitProducer::PreRegistrationCleanup
                                        | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                ) && (authorities.get(&canonical).is_some_and(|authority| {
                                    authority.authorizes(context.task, true)
                                }) || (context.producer
                                    == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                    && context.generation.is_some_and(|raw_generation| {
                                        wait_attempt_sequences.get(&attempt).is_some_and(
                                            |wait_sequence| {
                                                exact_original_root_launch_captured(
                                                    None,
                                                    raw_generation,
                                                    context.task,
                                                    *wait_sequence,
                                                )
                                            },
                                        )
                                    })))
                            })
                    });
                    if !valid_outcome || !valid_context {
                        violations.push(
                            PhysicalPartitionViolation::InvalidGenerationFinishEvidence(attempt),
                        );
                    }
                }
            }
            PhysicalEventRecordKind::WaitSiginfoReturned {
                attempt,
                siginfo,
                status,
            } => {
                if !wait_attempts.contains_key(&attempt) {
                    violations.push(PhysicalPartitionViolation::WaitSiginfoWithoutAttempt(
                        attempt,
                    ));
                }
                let task_matches =
                    wait_attempts
                        .get(&attempt)
                        .is_some_and(|context| match status {
                            Some(_) => context.task.tid == siginfo.pid,
                            None => siginfo.pid == 0,
                        });
                if !task_matches {
                    violations.push(PhysicalPartitionViolation::WaitSiginfoTaskMismatch(attempt));
                }
                if let Some(status) = status
                    && siginfo_status_attempts
                        .insert(status, attempt)
                        .is_some_and(|owner| owner != attempt)
                {
                    violations.push(PhysicalPartitionViolation::DuplicatePhysicalStatus(status));
                }
                if wait_siginfos.insert(attempt, (siginfo, status)).is_some() {
                    violations.push(PhysicalPartitionViolation::WaitSiginfoStatusMismatch(
                        attempt,
                    ));
                }
                wait_siginfo_sequences.insert(attempt, record.sequence);
            }
            PhysicalEventRecordKind::WaitResult { attempt, outcome } => {
                if !wait_attempts.contains_key(&attempt) {
                    violations.push(PhysicalPartitionViolation::WaitResultWithoutAttempt(
                        attempt,
                    ));
                }
                if !wait_results.insert(attempt) {
                    violations.push(PhysicalPartitionViolation::DuplicateWaitResult(attempt));
                }
                wait_outcomes.insert(attempt, outcome);
                wait_result_sequences.insert(attempt, record.sequence);
                let status_boundary = match outcome {
                    PhysicalWaitOutcome::Status { id, raw_status, .. } => {
                        Some((id, Some(raw_status), false, None))
                    }
                    PhysicalWaitOutcome::UndecodableStatus { id, siginfo, error } => {
                        if error != libc::EPROTO || !siginfo_is_rejected_by_waitid(siginfo) {
                            violations
                                .push(PhysicalPartitionViolation::InvalidUndecodableStatus(id));
                        }
                        Some((id, None, true, Some(siginfo)))
                    }
                    _ => None,
                };
                if let Some((id, raw_status, undecodable, undecodable_siginfo)) = status_boundary {
                    if siginfo_status_attempts
                        .get(&id)
                        .is_some_and(|owner| *owner != attempt)
                    {
                        violations.push(PhysicalPartitionViolation::DuplicatePhysicalStatus(id));
                    }
                    let generation = wait_generations.get(&attempt).copied();
                    let context = wait_attempts.get(&attempt).copied();
                    if statuses
                        .insert(
                            id,
                            StatusTrack {
                                generation,
                                task: context.map(|context| context.task),
                                producer: context.map(|context| context.producer),
                                created_sequence: record.sequence,
                                raw_status,
                                undecodable,
                                undecodable_siginfo,
                                ..StatusTrack::default()
                            },
                        )
                        .is_some()
                    {
                        violations.push(PhysicalPartitionViolation::DuplicatePhysicalStatus(id));
                    }
                }
            }
            PhysicalEventRecordKind::PreRegistrationBarrierConsumed {
                generation,
                barrier,
                consuming_wait,
                consumed_status,
            } => {
                startup_barrier_consumptions.push((
                    generation,
                    barrier,
                    consuming_wait,
                    consumed_status,
                    record.sequence,
                ));
            }
            PhysicalEventRecordKind::PreRegistrationBarrierFailureLinked {
                generation,
                barrier,
                consuming_wait,
                consumed_status,
                transaction,
            } => startup_barrier_failures.push((
                generation,
                barrier,
                consuming_wait,
                consumed_status,
                transaction,
                record.sequence,
            )),
            PhysicalEventRecordKind::PreRegistrationBarrierStatuslessFailureLinked {
                generation,
                barrier,
                cause_wait,
                error,
                transaction,
            } => startup_barrier_statusless_failures.push((
                generation,
                barrier,
                cause_wait,
                error,
                transaction,
                record.sequence,
            )),
            PhysicalEventRecordKind::PreRegistrationBarrierStatuslessCleanupResolved {
                generation,
                barrier,
                cleanup_wait,
                status,
                transaction,
            } => startup_barrier_statusless_resolutions.push((
                generation,
                barrier,
                cleanup_wait,
                status,
                transaction,
                record.sequence,
            )),
            PhysicalEventRecordKind::PreRegistrationBarrierSetupFailed {
                generation,
                task,
                error,
                launch,
            } => startup_barrier_setup_failures.push((
                generation,
                task,
                error,
                launch,
                record.sequence,
            )),
            PhysicalEventRecordKind::StartupSetupCleanupPrepared {
                generation,
                task,
                error,
                transaction,
                launch,
            } => startup_setup_prepared.push((
                generation,
                task,
                error,
                transaction,
                launch,
                record.sequence,
            )),
            PhysicalEventRecordKind::StartupSetupCleanupLinked {
                generation,
                error,
                consuming_wait,
                status,
                transaction,
                launch,
            } => startup_setup_linked.push((
                generation,
                error,
                consuming_wait,
                status,
                transaction,
                launch,
                record.sequence,
            )),
            PhysicalEventRecordKind::StartupSetupCleanupNoStatusLinked {
                generation,
                error,
                consuming_wait,
                transaction,
                launch,
            } => startup_setup_no_status_linked.push((
                generation,
                error,
                consuming_wait,
                transaction,
                launch,
                record.sequence,
            )),
            PhysicalEventRecordKind::StopResolutionWatchArmed {
                generation,
                delivery,
            } => {
                if stop_resolution_arms
                    .insert(delivery, (generation, record.sequence))
                    .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                        delivery,
                    ));
                }
            }
            PhysicalEventRecordKind::StopResolutionFirstStopped {
                delivery, stopped, ..
            } => {
                if stop_resolution_first
                    .insert(stopped, (delivery, record.sequence))
                    .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                        stopped,
                    ));
                }
            }
            PhysicalEventRecordKind::StopResolutionGroupAcknowledged {
                delivery,
                group_stop,
                ..
            } => {
                if stop_resolution_acks
                    .insert(group_stop, (delivery, record.sequence))
                    .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                        group_stop,
                    ));
                }
            }
            PhysicalEventRecordKind::StopResolutionContinuedClaimed {
                group_stop,
                continued,
                ..
            } => {
                if stop_resolution_claims
                    .insert(continued, (group_stop, record.sequence))
                    .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                        continued,
                    ));
                }
            }
            PhysicalEventRecordKind::StopResolutionWatchClosed { delivery, .. } => {
                if stop_resolution_closes
                    .insert(delivery, record.sequence)
                    .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                        delivery,
                    ));
                }
            }
            PhysicalEventRecordKind::StopResolutionResumeCausallyClosed {
                generation,
                delivery,
                group_stop,
                continued,
                attempt,
                successor,
            } => stop_resolution_causal_closes.push((
                generation,
                delivery,
                group_stop,
                continued,
                attempt,
                successor,
                record.sequence,
            )),
            PhysicalEventRecordKind::PreStopContinuedDrainFailed {
                generation,
                task,
                stopped,
                cause_wait,
                transaction,
            } => pre_stop_drain_failure_records.push((
                generation,
                task,
                stopped,
                cause_wait,
                transaction,
                record.sequence,
            )),
            PhysicalEventRecordKind::RegisteredCleanupTransactionStarted {
                transaction,
                cause_wait,
                kind,
            } => {
                let track = cleanup_transactions.entry(transaction).or_default();
                track.starts += 1;
                if track.start_sequence.is_none() {
                    track.start_sequence = Some(record.sequence);
                    track.cause_wait = Some(cause_wait);
                    track.kind = Some(kind);
                }
                if track.starts > 1 {
                    violations.push(
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(
                            transaction,
                        ),
                    );
                }
                if cleanup_cause_wait_transactions
                    .insert(cause_wait, transaction)
                    .is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::DuplicateRegisteredCleanupCauseWait(cause_wait),
                    );
                }
            }
            PhysicalEventRecordKind::StartupCleanupWaitFailureExitProved {
                transaction,
                generation,
                task,
                pidfd,
                failed_wait,
                error,
                revents,
            } => {
                if startup_wait_failure_exit_proofs
                    .insert(
                        transaction,
                        (
                            generation,
                            task,
                            pidfd,
                            failed_wait,
                            error,
                            revents,
                            record.sequence,
                        ),
                    )
                    .is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(
                            transaction,
                        ),
                    );
                }
            }
            PhysicalEventRecordKind::StartupCleanupResumeFailureExitProved {
                transaction,
                generation,
                task,
                pidfd,
                resume,
                source_status,
                error,
                revents,
            } => {
                if startup_resume_failure_exit_proofs
                    .insert(
                        transaction,
                        (
                            generation,
                            task,
                            pidfd,
                            resume,
                            source_status,
                            error,
                            revents,
                            record.sequence,
                        ),
                    )
                    .is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(
                            transaction,
                        ),
                    );
                }
            }
            PhysicalEventRecordKind::StartupCleanupExecutorTransferred {
                transaction,
                generation,
                task,
            } => {
                if startup_executor_transfers
                    .insert(transaction, (generation, task, record.sequence))
                    .is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(
                            transaction,
                        ),
                    );
                }
            }
            PhysicalEventRecordKind::RegisteredCleanupStatusLinked {
                transaction,
                status,
            } => {
                let track = cleanup_transactions.entry(transaction).or_default();
                if track.statuses.insert(status, record.sequence).is_some()
                    || cleanup_status_transactions
                        .insert(status, transaction)
                        .is_some()
                {
                    violations
                        .push(PhysicalPartitionViolation::DuplicateRegisteredCleanupStatus(status));
                }
            }
            PhysicalEventRecordKind::RegisteredCleanupToleratedResumeLinked {
                transaction,
                resume,
            } => {
                let track = cleanup_transactions.entry(transaction).or_default();
                if track.resumes.insert(resume, record.sequence).is_some()
                    || cleanup_resume_transactions
                        .insert(resume, transaction)
                        .is_some()
                {
                    violations
                        .push(PhysicalPartitionViolation::DuplicateRegisteredCleanupResume(resume));
                }
            }
            PhysicalEventRecordKind::RegisteredCleanupPidfdExited {
                transaction,
                terminal_wait,
                generation,
                task,
                revents,
                launch,
            } => {
                let track = cleanup_transactions.entry(transaction).or_default();
                if track
                    .pidfd_exit_proof
                    .replace((
                        terminal_wait,
                        generation,
                        task,
                        revents,
                        launch,
                        record.sequence,
                    ))
                    .is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(
                            transaction,
                        ),
                    );
                }
            }
            PhysicalEventRecordKind::EchildPidfdExited {
                wait,
                generation,
                task,
                revents,
            } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let context = wait_attempts.get(&wait);
                let valid = canonical.is_some_and(|canonical| {
                    wait_generations.get(&wait) == Some(&canonical)
                        && wait_outcomes.get(&wait) == Some(&PhysicalWaitOutcome::NoChild)
                        && wait_result_sequences
                            .get(&wait)
                            .is_some_and(|result| *result < record.sequence)
                        && context.is_some_and(|context| {
                            matches!(
                                context.producer,
                                PhysicalWaitProducer::NotifierWorker
                                    | PhysicalWaitProducer::AuthorizedRootNotifier
                                    | PhysicalWaitProducer::SynchronousWait
                            ) && task.is_captured()
                                && task == context.task
                        })
                        && authorities
                            .get(&canonical)
                            .is_some_and(|authority| authority.authorizes(task, false))
                        && revents & libc::POLLIN != 0
                });
                if !valid
                    || echild_terminal_proofs
                        .insert(
                            wait,
                            (canonical.unwrap_or(generation), task, record.sequence),
                        )
                        .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidEchildTerminalProof(wait));
                }
            }
            PhysicalEventRecordKind::EchildTracerDetached {
                wait,
                generation,
                task,
                observed_tracer_pid,
            } => {
                let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let context = wait_attempts.get(&wait);
                let valid = canonical.is_some_and(|canonical| {
                    wait_generations.get(&wait) == Some(&canonical)
                        && wait_outcomes.get(&wait) == Some(&PhysicalWaitOutcome::NoChild)
                        && wait_result_sequences
                            .get(&wait)
                            .is_some_and(|result| *result < record.sequence)
                        && context.is_some_and(|context| {
                            matches!(
                                context.producer,
                                PhysicalWaitProducer::NotifierWorker
                                    | PhysicalWaitProducer::AuthorizedRootNotifier
                                    | PhysicalWaitProducer::SynchronousWait
                            ) && task.is_captured()
                                && task == context.task
                        })
                        && authorities
                            .get(&canonical)
                            .is_some_and(|authority| authority.authorizes(task, false))
                        && observed_tracer_pid >= 0
                });
                if !valid
                    || echild_terminal_proofs
                        .insert(
                            wait,
                            (canonical.unwrap_or(generation), task, record.sequence),
                        )
                        .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidEchildTerminalProof(wait));
                }
            }
            PhysicalEventRecordKind::RegisteredCleanupTransactionCompleted {
                transaction,
                terminal_wait,
            } => {
                let track = cleanup_transactions.entry(transaction).or_default();
                if track.terminal_wait.replace(terminal_wait).is_some()
                    || track.completion_sequence.replace(record.sequence).is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(
                            transaction,
                        ),
                    );
                }
                if cleanup_terminal_wait_transactions
                    .insert(terminal_wait, transaction)
                    .is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::DuplicateRegisteredCleanupTerminalWait(
                            terminal_wait,
                        ),
                    );
                }
            }
            PhysicalEventRecordKind::StatusPublished {
                status,
                generation,
                destination,
            } => match statuses.get_mut(&status) {
                Some(track) => {
                    let recorded_generation = generation;
                    let generation = recorded_generation.and_then(|generation| {
                        canonical_generation(generation, &adoptions, &invalid_adoptions)
                    });
                    let external_cleanup =
                        destination == PhysicalStatusPublication::ExternalCleanup;
                    let cleanup_terminal =
                        destination == PhysicalStatusPublication::CleanupTerminal;
                    let startup_cleanup_terminal =
                        destination == PhysicalStatusPublication::StartupBarrierCleanupTerminal;
                    let cleanup_stopped = destination == PhysicalStatusPublication::CleanupStopped;
                    let pre_stop_drain_failure =
                        destination == PhysicalStatusPublication::PreStopDrainFailureCleanup;
                    let startup_barrier_failure =
                        destination == PhysicalStatusPublication::StartupBarrierFailureCleanup;
                    let startup_cleanup_stopped =
                        destination == PhysicalStatusPublication::StartupBarrierCleanupStopped;
                    let typed_unsupported_startup_terminal =
                        startup_cleanup_terminal && track.startup_typed_unsupported_terminal();
                    let typed_unsupported_startup_stopped =
                        startup_cleanup_stopped && track.startup_typed_unsupported_stopped();
                    if track.undecodable
                        && !startup_barrier_failure
                        && !typed_unsupported_startup_terminal
                        && !typed_unsupported_startup_stopped
                    {
                        violations
                            .push(PhysicalPartitionViolation::UndecodableStatusEscaped(status));
                    }
                    if (recorded_generation.is_some() && generation.is_none())
                        || (external_cleanup
                            && !matches!(
                                (recorded_generation, track.producer),
                                (None, Some(PhysicalWaitProducer::PreRegistrationCleanup))
                                    | (
                                        Some(_),
                                        Some(PhysicalWaitProducer::PreRegistrationBarrierCleanup)
                                    )
                            ))
                        || (cleanup_terminal || cleanup_stopped)
                            && (generation.is_none()
                                || track.producer != Some(PhysicalWaitProducer::RegisteredCleanup))
                        || startup_cleanup_terminal
                            && (generation.is_none()
                                || !matches!(
                                    track.producer,
                                    Some(
                                        PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                            | PhysicalWaitProducer::AuthorizedRootNotifier
                                    )
                                ))
                        || pre_stop_drain_failure
                            && (generation.is_none()
                                || track.producer
                                    != Some(PhysicalWaitProducer::AuthorizedRootNotifier))
                        || startup_barrier_failure
                            && (generation.is_none()
                                || !matches!(
                                    track.producer,
                                    Some(
                                        PhysicalWaitProducer::AuthorizedRootNotifier
                                            | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                            | PhysicalWaitProducer::RegisteredCleanup
                                    )
                                ))
                        || startup_cleanup_stopped
                            && (generation.is_none()
                                || track.producer
                                    != Some(PhysicalWaitProducer::PreRegistrationBarrierCleanup))
                        || (!external_cleanup
                            && !cleanup_terminal
                            && !startup_cleanup_terminal
                            && !cleanup_stopped
                            && !pre_stop_drain_failure
                            && !startup_barrier_failure
                            && !startup_cleanup_stopped
                            && generation.is_none())
                        || matches!(destination, PhysicalStatusPublication::DirectStopped)
                            && !matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::PreRegistrationCleanup
                                        | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                )
                            )
                        || matches!((track.generation, generation), (Some(expected), Some(observed)) if expected != observed)
                    {
                        violations.push(PhysicalPartitionViolation::WrongGeneration(status));
                    }
                    let raw_status = track.raw_status;
                    let producer_and_shape_valid = match destination {
                        PhysicalStatusPublication::RegularFifo => {
                            matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::NotifierWorker
                                        | PhysicalWaitProducer::AuthorizedRootNotifier
                                        | PhysicalWaitProducer::SynchronousWait
                                )
                            ) && raw_status.is_some_and(|raw_status| {
                                libc::WIFSTOPPED(raw_status) && !is_ptrace_exit_stop(raw_status)
                            })
                        }
                        PhysicalStatusPublication::ContinuedSideChannel { route } => {
                            let producer_matches = match route {
                                PhysicalContinuedStatusRoute::PreStopDrain { .. } => {
                                    track.producer
                                        == Some(PhysicalWaitProducer::PreStopContinuedDrain)
                                }
                                PhysicalContinuedStatusRoute::UnwatchedRoot
                                | PhysicalContinuedStatusRoute::BeforeFirstStop
                                | PhysicalContinuedStatusRoute::AfterFirstStop
                                | PhysicalContinuedStatusRoute::AfterAcknowledgedGroupStop => {
                                    track.producer
                                        == Some(PhysicalWaitProducer::AuthorizedRootNotifier)
                                }
                            };
                            producer_matches
                                && raw_status
                                    .is_some_and(|raw_status| libc::WIFCONTINUED(raw_status))
                        }
                        PhysicalStatusPublication::RetainedTerminal => {
                            matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::NotifierWorker
                                        | PhysicalWaitProducer::AuthorizedRootNotifier
                                        | PhysicalWaitProducer::SynchronousWait
                                )
                            ) && raw_status.is_some_and(is_terminal_raw_status)
                        }
                        PhysicalStatusPublication::ExitCapability => {
                            matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::NotifierWorker
                                        | PhysicalWaitProducer::AuthorizedRootNotifier
                                )
                            ) && raw_status.is_some_and(is_ptrace_exit_stop)
                        }
                        PhysicalStatusPublication::SynchronousFifo => {
                            track.producer == Some(PhysicalWaitProducer::SynchronousWait)
                                && raw_status.is_some_and(is_ptrace_exit_stop)
                        }
                        PhysicalStatusPublication::DirectStopped => {
                            matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::PreRegistrationCleanup
                                        | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                )
                            ) && raw_status.is_some_and(|raw_status| libc::WIFSTOPPED(raw_status))
                        }
                        PhysicalStatusPublication::ExternalCleanup => {
                            matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::PreRegistrationCleanup
                                        | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                )
                            ) && raw_status.is_some_and(is_terminal_raw_status)
                        }
                        PhysicalStatusPublication::CleanupTerminal => {
                            track.producer == Some(PhysicalWaitProducer::RegisteredCleanup)
                                && raw_status.is_some_and(is_terminal_raw_status)
                        }
                        PhysicalStatusPublication::StartupBarrierCleanupTerminal => {
                            matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                        | PhysicalWaitProducer::AuthorizedRootNotifier
                                )
                            ) && (raw_status.is_some_and(is_terminal_raw_status)
                                || track.startup_typed_unsupported_terminal())
                        }
                        PhysicalStatusPublication::CleanupStopped => {
                            track.producer == Some(PhysicalWaitProducer::RegisteredCleanup)
                                && raw_status.is_some_and(|raw_status| libc::WIFSTOPPED(raw_status))
                        }
                        PhysicalStatusPublication::PreStopDrainFailureCleanup => {
                            track.producer == Some(PhysicalWaitProducer::AuthorizedRootNotifier)
                                && raw_status.is_some_and(|raw_status| {
                                    libc::WIFSTOPPED(raw_status)
                                        && libc::WSTOPSIG(raw_status) == libc::SIGSTOP
                                        && ((raw_status as u32 >> 16) & 0xffff) == 0
                                })
                        }
                        PhysicalStatusPublication::StartupBarrierFailureCleanup => {
                            matches!(
                                track.producer,
                                Some(
                                    PhysicalWaitProducer::AuthorizedRootNotifier
                                        | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                        | PhysicalWaitProducer::RegisteredCleanup
                                )
                            ) && (track.undecodable
                                || raw_status.is_some_and(|raw_status| {
                                    (libc::WIFSTOPPED(raw_status)
                                        && !is_ptrace_exit_stop(raw_status))
                                        || libc::WIFCONTINUED(raw_status)
                                }))
                        }
                        PhysicalStatusPublication::StartupBarrierCleanupStopped => {
                            track.producer
                                == Some(PhysicalWaitProducer::PreRegistrationBarrierCleanup)
                                && (raw_status
                                    .is_some_and(|raw_status| libc::WIFSTOPPED(raw_status))
                                    || track.startup_typed_unsupported_stopped())
                        }
                    };
                    if !producer_and_shape_valid || track.created_sequence >= record.sequence {
                        violations
                            .push(PhysicalPartitionViolation::InvalidStatusPublication(status));
                    }
                    if track.generation.is_none() {
                        track.generation = generation;
                    }
                    track.published += 1;
                    if track.publication_destination.is_none() {
                        track.publication_destination = Some(destination);
                        track.publication_sequence = Some(record.sequence);
                    }
                    if destination == PhysicalStatusPublication::RetainedTerminal {
                        track.dispositions += 1;
                    }
                    let terminal = track.raw_status.is_some_and(|raw_status| {
                        libc::WIFEXITED(raw_status) || libc::WIFSIGNALED(raw_status)
                    }) || typed_unsupported_startup_terminal;
                    if !terminal {
                        if external_cleanup {
                            violations.push(
                                PhysicalPartitionViolation::ExternalCleanupNonterminal(status),
                            );
                        }
                        if cleanup_terminal {
                            violations.push(
                                PhysicalPartitionViolation::CleanupTerminalNonterminal(status),
                            );
                        }
                        if startup_cleanup_terminal {
                            violations.push(
                                PhysicalPartitionViolation::CleanupTerminalNonterminal(status),
                            );
                        }
                    }
                }
                None => violations.push(PhysicalPartitionViolation::UnknownPhysicalStatus(status)),
            },
            PhysicalEventRecordKind::SyntheticEchildPublished {
                generation,
                cause: Some(cause),
            } => {
                let generation = canonical_generation(generation, &adoptions, &invalid_adoptions);
                let context = wait_attempts.get(&cause);
                let exact_terminal_proof = generation.is_some_and(|generation| {
                    echild_terminal_proofs.get(&cause).is_some_and(
                        |(proof_generation, proof_task, proof_sequence)| {
                            *proof_generation == generation
                                && context.is_some_and(|context| {
                                    matches!(
                                        context.producer,
                                        PhysicalWaitProducer::NotifierWorker
                                            | PhysicalWaitProducer::AuthorizedRootNotifier
                                            | PhysicalWaitProducer::SynchronousWait
                                    ) && context.task == *proof_task
                                })
                                && wait_result_sequences.get(&cause).is_some_and(|result| {
                                    result < proof_sequence && *proof_sequence < record.sequence
                                })
                        },
                    )
                });
                if wait_outcomes.get(&cause) != Some(&PhysicalWaitOutcome::NoChild)
                    || generation.is_none()
                    || generation.as_ref() != wait_generations.get(&cause)
                    || !exact_terminal_proof
                {
                    violations.push(PhysicalPartitionViolation::SyntheticEchildWithoutNoChild(
                        cause,
                    ));
                    if !exact_terminal_proof {
                        violations.push(PhysicalPartitionViolation::InvalidEchildTerminalProof(
                            cause,
                        ));
                    }
                }
                let first_proof_use =
                    exact_terminal_proof && used_echild_terminal_proofs.insert(cause);
                if exact_terminal_proof && !first_proof_use {
                    violations.push(PhysicalPartitionViolation::InvalidEchildTerminalProof(
                        cause,
                    ));
                }
                if let Some(generation) = generation
                    && first_proof_use
                {
                    synthetic_echild_evidence.insert(cause, (generation, record.sequence));
                }
            }
            PhysicalEventRecordKind::SyntheticEchildPublished {
                generation,
                cause: None,
            } => {
                if let Some(generation) =
                    canonical_generation(generation, &adoptions, &invalid_adoptions)
                {
                    uncaused_synthetic_echild
                        .entry(generation)
                        .or_default()
                        .push(record.sequence);
                }
            }
            PhysicalEventRecordKind::StatusReserved {
                reservation,
                status,
                generation,
            } => {
                let generation = canonical_generation(generation, &adoptions, &invalid_adoptions);
                if let Some(generation) = generation {
                    reservation_generations.insert(reservation, generation);
                }
                match statuses.get(&status) {
                    None => violations.push(
                        PhysicalPartitionViolation::ReservationForUnknownStatus(reservation),
                    ),
                    Some(track) => {
                        if track.undecodable {
                            violations
                                .push(PhysicalPartitionViolation::UndecodableStatusEscaped(status));
                        }
                        if generation.is_none()
                            || matches!(
                                (track.generation, generation),
                                (Some(expected), Some(observed)) if expected != observed
                            )
                        {
                            violations.push(PhysicalPartitionViolation::WrongGeneration(status));
                        }
                        if !track.publication_sequence.is_some_and(|published| {
                            track.created_sequence < published && published < record.sequence
                        }) {
                            violations.push(PhysicalPartitionViolation::InvalidReservationOrder(
                                reservation,
                            ));
                        }
                        if !matches!(
                            track.publication_destination,
                            Some(
                                PhysicalStatusPublication::RegularFifo
                                    | PhysicalStatusPublication::SynchronousFifo
                                    | PhysicalStatusPublication::RetainedTerminal
                            )
                        ) {
                            violations.push(
                                PhysicalPartitionViolation::InvalidReservationDestination(
                                    reservation,
                                ),
                            );
                        }
                    }
                }
                if let std::collections::btree_map::Entry::Vacant(e) =
                    reservations.entry(reservation)
                {
                    e.insert(ReservationTrack {
                        status: Some(status),
                        reserved_sequence: Some(record.sequence),
                        ..ReservationTrack::default()
                    });
                } else {
                    violations.push(PhysicalPartitionViolation::DuplicateReservation(
                        reservation,
                    ));
                }
            }
            PhysicalEventRecordKind::ReservationCommitted {
                reservation,
                status,
            }
            | PhysicalEventRecordKind::ReservationRolledBack {
                reservation,
                status,
            }
            | PhysicalEventRecordKind::TerminalReplayed {
                reservation,
                status,
            } => {
                let completion = match record.kind {
                    PhysicalEventRecordKind::ReservationCommitted { .. } => {
                        ReservationCompletion::Committed
                    }
                    PhysicalEventRecordKind::ReservationRolledBack { .. } => {
                        ReservationCompletion::RolledBack
                    }
                    PhysicalEventRecordKind::TerminalReplayed { .. } => {
                        ReservationCompletion::TerminalReplayed
                    }
                    _ => unreachable!(),
                };
                match reservations.get_mut(&reservation) {
                    Some(track) if track.status == Some(status) => {
                        if track.completion.is_some() {
                            violations.push(
                                PhysicalPartitionViolation::DuplicateReservationOutcome(
                                    reservation,
                                ),
                            );
                        } else {
                            track.completion = Some((completion, record.sequence));
                        }
                    }
                    _ => violations.push(PhysicalPartitionViolation::ReservationForUnknownStatus(
                        reservation,
                    )),
                }
            }
            PhysicalEventRecordKind::DecodeStarted {
                reservation,
                status,
                owner,
            } => match reservations.get_mut(&reservation) {
                Some(track) if track.status == Some(status) => {
                    if statuses.get(&status).is_some_and(|track| track.undecodable) {
                        violations
                            .push(PhysicalPartitionViolation::UndecodableStatusEscaped(status));
                    }
                    if track.decode_started.is_some() {
                        violations.push(PhysicalPartitionViolation::DuplicateDecodeStart(
                            reservation,
                        ));
                    } else {
                        track.decode_started = Some((owner, record.sequence));
                    }
                }
                _ => violations.push(PhysicalPartitionViolation::DecodeWithoutReservation(
                    reservation,
                )),
            },
            PhysicalEventRecordKind::DecodeFinished {
                reservation,
                status,
                outcome,
                owner,
            } => match reservations.get_mut(&reservation) {
                Some(track) if track.status == Some(status) => {
                    if statuses.get(&status).is_some_and(|track| track.undecodable) {
                        violations
                            .push(PhysicalPartitionViolation::UndecodableStatusEscaped(status));
                    }
                    if track.decode_finished.is_some() {
                        violations.push(PhysicalPartitionViolation::DuplicateDecodeFinish(
                            reservation,
                        ));
                    } else if track.decode_started.map(|(started_owner, _)| started_owner)
                        != Some(owner)
                    {
                        violations.push(PhysicalPartitionViolation::DecodeFinishedWithoutStart(
                            reservation,
                        ));
                    } else {
                        track.decode_finished = Some((owner, outcome, record.sequence));
                    }
                }
                _ => violations.push(PhysicalPartitionViolation::DecodeWithoutReservation(
                    reservation,
                )),
            },
            PhysicalEventRecordKind::StatusDisposition {
                status,
                disposition,
            } => {
                if let Some(track) = statuses.get_mut(&status) {
                    track.dispositions += 1;
                    track.last_disposition_sequence = Some(record.sequence);
                    if disposition == PhysicalStatusDisposition::CancellationCleanup {
                        track.cancellation_cleanup_dispositions += 1;
                        track.cancellation_cleanup_sequence = Some(record.sequence);
                    }
                    if disposition == PhysicalStatusDisposition::OrdinaryHandled {
                        track.ordinary_handled_dispositions += 1;
                        track.ordinary_handled_sequence = Some(record.sequence);
                    }
                    if let PhysicalStatusDisposition::ContinuedSideChannel { route } = disposition {
                        track.continued_side_channel_dispositions += 1;
                        track.continued_side_channel_sequence = Some(record.sequence);
                        if track.continued_side_channel_route.replace(route).is_some() {
                            violations
                                .push(PhysicalPartitionViolation::InvalidStatusDisposition(status));
                        }
                    }
                    if disposition == PhysicalStatusDisposition::DecodeDied {
                        track.decode_died_dispositions += 1;
                        track.decode_died_sequence = Some(record.sequence);
                    }
                    if disposition == PhysicalStatusDisposition::ExitCapabilityExpired {
                        track.exit_capability_expired_dispositions += 1;
                        track.exit_capability_expired_sequence = Some(record.sequence);
                    }
                    if disposition == PhysicalStatusDisposition::KernelSupersededByExitStop {
                        track.kernel_superseded_dispositions += 1;
                        track.kernel_superseded_sequence = Some(record.sequence);
                    }
                    if let PhysicalStatusDisposition::AmbiguousResumeCausallyResolved {
                        attempt,
                        proof,
                    } = disposition
                    {
                        track.ambiguous_resume_resolved_dispositions += 1;
                        track.ambiguous_resume_resolved_sequence = Some(record.sequence);
                        ambiguous_resume_resolutions.push((
                            status,
                            attempt,
                            proof,
                            record.sequence,
                        ));
                    }
                } else {
                    violations.push(PhysicalPartitionViolation::UnknownPhysicalStatus(status));
                }
            }
            PhysicalEventRecordKind::ExitCapability { status, transition } => {
                match statuses.get(&status) {
                    None => violations.push(
                        PhysicalPartitionViolation::ExitCapabilityWithoutStatus(status),
                    ),
                    Some(track) if track.undecodable => violations
                        .push(PhysicalPartitionViolation::UndecodableStatusEscaped(status)),
                    Some(_) => {}
                }
                exit_capabilities
                    .entry(status)
                    .or_default()
                    .transitions
                    .push((record.sequence, transition));
            }
            PhysicalEventRecordKind::ResumeAttempt { id, context } => {
                resume_attempts.insert(id, context);
                resume_attempt_sequences.insert(id, record.sequence);
                let Some(generation) = context.generation.and_then(|generation| {
                    canonical_generation(generation, &adoptions, &invalid_adoptions)
                }) else {
                    violations.push(PhysicalPartitionViolation::ResumeWithoutGeneration(id));
                    continue;
                };
                resume_generations.insert(id, generation);
                let allow_direct_narrowing = matches!(
                    context.owner,
                    PhysicalResumeOwner::PreRegistrationCleanup
                        | PhysicalResumeOwner::StartupBarrierCleanup
                );
                let opaque_launch_retained_barrier_resume = context.owner
                    == PhysicalResumeOwner::StartupBarrierCleanup
                    && context.generation.is_some_and(|raw_generation| {
                        exact_original_root_launch_captured(
                            None,
                            raw_generation,
                            context.task,
                            record.sequence,
                        )
                    })
                    && context.source_status.is_some_and(|status| {
                        statuses.get(&status).is_some_and(|status_track| {
                            status_track.producer
                                == Some(PhysicalWaitProducer::PreRegistrationBarrierCleanup)
                        }) && cleanup_status_transactions
                            .get(&status)
                            .and_then(|transaction| cleanup_transactions.get(transaction))
                            .is_some_and(|cleanup| {
                                cleanup.statuses.contains_key(&status)
                                    && matches!(
                                        cleanup.kind,
                                        Some(
                                            PhysicalCleanupTransactionKind::StartupBarrier {
                                                generation: transaction_generation,
                                                task,
                                                owner: PhysicalStartupCleanupOwner::Unstarted,
                                                ..
                                            }
                                        ) if context.generation
                                            == Some(transaction_generation)
                                            && task == context.task
                                    )
                            })
                    });
                let opaque_launch_startup_setup_resume = context.owner
                    == PhysicalResumeOwner::StartupBarrierCleanup
                    && context.source_status.is_some_and(|status| {
                        let Some(transaction) = cleanup_status_transactions.get(&status) else {
                            return false;
                        };
                        let Some(cleanup) = cleanup_transactions.get(transaction) else {
                            return false;
                        };
                        let Some(PhysicalCleanupTransactionKind::StartupSetup {
                            generation: transaction_generation,
                            task,
                            error,
                            launch,
                        }) = cleanup.kind
                        else {
                            return false;
                        };
                        if context.generation != Some(transaction_generation)
                            || context.task != task
                            || !task.is_pidfd_bound_direct_child()
                            || cleanup.cause_wait.is_none()
                            || !cleanup.statuses.contains_key(&status)
                        {
                            return false;
                        }
                        let cause_wait = cleanup.cause_wait.expect("checked setup cause wait");
                        let matching_failures = startup_barrier_setup_failures
                            .iter()
                            .filter(|(generation, candidate_task, candidate_error, candidate_launch, _)| {
                                *generation == transaction_generation
                                    && *candidate_task == task
                                    && *candidate_error == error
                                    && *candidate_launch == launch
                            })
                            .collect::<Vec<_>>();
                        let matching_prepared = startup_setup_prepared
                            .iter()
                            .filter(|(generation, candidate_task, candidate_error, candidate_transaction, candidate_launch, _)| {
                                *generation == transaction_generation
                                    && *candidate_task == task
                                    && *candidate_error == error
                                    && *candidate_transaction == *transaction
                                    && *candidate_launch == launch
                            })
                            .collect::<Vec<_>>();
                        let matching_links = startup_setup_linked
                            .iter()
                            .filter(|(generation, candidate_error, wait, candidate_status, candidate_transaction, candidate_launch, _)| {
                                *generation == transaction_generation
                                    && *candidate_error == error
                                    && *wait == cause_wait
                                    && *candidate_status == status
                                    && *candidate_transaction == *transaction
                                    && *candidate_launch == launch
                            })
                            .collect::<Vec<_>>();
                        let ([failure], [prepared], [linked]) = (
                            matching_failures.as_slice(),
                            matching_prepared.as_slice(),
                            matching_links.as_slice(),
                        ) else {
                            return false;
                        };
                        let (_, _, _, _, failure_sequence) = **failure;
                        let (_, _, _, _, _, prepared_sequence) = **prepared;
                        let (_, _, _, _, _, _, linked_sequence) = **linked;
                        exact_original_root_launch_pidfd_direct(
                            launch,
                            transaction_generation,
                            task,
                            failure_sequence,
                        ) && matches!(
                            wait_outcomes.get(&cause_wait),
                            Some(PhysicalWaitOutcome::Status { id, .. })
                                | Some(PhysicalWaitOutcome::UndecodableStatus { id, .. })
                                if *id == status
                        ) && wait_attempts.get(&cause_wait).is_some_and(|wait_context| {
                            wait_context.generation == Some(transaction_generation)
                                && wait_context.task == task
                                && wait_context.producer
                                    == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                        }) && wait_attempt_sequences.get(&cause_wait).is_some_and(|attempt| {
                            wait_result_sequences.get(&cause_wait).is_some_and(|result| {
                                cleanup.start_sequence.is_some_and(|start| {
                                    cleanup.statuses.get(&status).is_some_and(|status_link| {
                                        statuses.get(&status).is_some_and(|status_track| {
                                            status_track.producer
                                                == Some(PhysicalWaitProducer::PreRegistrationBarrierCleanup)
                                                && status_track.task == Some(task)
                                                && status_track.publication_sequence.is_some_and(|published| {
                                                    failure_sequence < prepared_sequence
                                                        && prepared_sequence < *attempt
                                                        && *attempt < *result
                                                        && *result < start
                                                        && start < *status_link
                                                        && *status_link < linked_sequence
                                                        && linked_sequence < published
                                                        && published < record.sequence
                                                })
                                        })
                                    })
                                })
                            })
                        })
                    });
                let opaque_launch_startup_resume =
                    opaque_launch_retained_barrier_resume || opaque_launch_startup_setup_resume;
                let authority_matches =
                    if context.owner == PhysicalResumeOwner::StartupBarrierCleanup {
                        opaque_launch_startup_resume
                    } else {
                        authorities.get(&generation).is_some_and(|authority| {
                            authority.authorizes(context.task, allow_direct_narrowing)
                        })
                    };
                let identity_was_bound =
                    identity_binding_sequences
                        .get(&generation)
                        .is_some_and(|bindings| {
                            bindings.iter().any(|(bound, sequence)| {
                                *sequence < record.sequence
                                    && bound.authorizes(context.task, allow_direct_narrowing)
                            })
                        });
                let pre_registration_was_linked = pre_registration_link_sequences
                    .get(&generation)
                    .is_some_and(|links| {
                        links.iter().any(|(linked, sequence)| {
                            *sequence < record.sequence
                                && (linked.authorizes(context.task, true)
                                    || linked.tid == context.task.tid)
                        })
                    });
                let startup_barrier_cleanup = context.source_status.is_some_and(|status| {
                    statuses.get(&status).is_some_and(|track| {
                        track.producer == Some(PhysicalWaitProducer::PreRegistrationBarrierCleanup)
                    })
                });
                let status_matches = context.source_status.is_none_or(|status| {
                    statuses.get(&status).is_some_and(|track| {
                        track.generation == Some(generation)
                            && track
                                .task
                                .is_some_and(|task| task.authorizes(context.task, false))
                    })
                });
                let publication_precedes_attempt = context.source_status.is_some_and(|status| {
                    statuses.get(&status).is_some_and(|track| {
                        track.undecodable
                            || track
                                .publication_sequence
                                .is_some_and(|published| published < record.sequence)
                    })
                });
                let registered_wait_failure_handoff = context.source_status.is_some_and(|status| {
                    cleanup_status_transactions.contains_key(&status)
                        || statuses.get(&status).is_some_and(|track| {
                            track.undecodable
                                || track.producer == Some(PhysicalWaitProducer::RegisteredCleanup)
                        })
                });
                let startup_wait_failure_handoff = context.source_status.is_some_and(|status| {
                    cleanup_status_transactions
                        .get(&status)
                        .and_then(|transaction| cleanup_transactions.get(transaction))
                        .is_some_and(|track| {
                            matches!(
                                track.kind,
                                Some(PhysicalCleanupTransactionKind::StartupBarrier {
                                    owner: PhysicalStartupCleanupOwner::AuthorizedWorker,
                                    ..
                                })
                            )
                        })
                });
                let authorized_root_external_cleanup =
                    context.source_status.is_some_and(|status| {
                        cleanup_status_transactions
                            .get(&status)
                            .and_then(|transaction| {
                                cleanup_transactions
                                    .get(transaction)
                                    .zip(startup_executor_transfers.get(transaction))
                            })
                            .is_some_and(
                                |(track, (transfer_generation, transfer_task, transfer))| {
                                    matches!(
                                        track.kind,
                                        Some(PhysicalCleanupTransactionKind::StartupBarrier {
                                            generation: transaction_generation,
                                            task,
                                            owner: PhysicalStartupCleanupOwner::AuthorizedWorker,
                                            ..
                                        }) if transaction_generation == generation
                                            && *transfer_generation == generation
                                            && task.authorizes(*transfer_task, false)
                                            && transfer_task.authorizes(context.task, false)
                                    ) && *transfer < record.sequence
                                        && startup_barrier_statusless_resolutions.iter().any(
                                            |(_, _, _, resolved_status, transaction, resolved)| {
                                                *transaction == cleanup_status_transactions[&status]
                                                    && *resolved_status == Some(status)
                                                    && *resolved < record.sequence
                                            },
                                        )
                                },
                            )
                    });
                let cleanup_shape_valid = match context.owner {
                    PhysicalResumeOwner::TypedStopped => context
                        .signal
                        .is_none_or(|signal| crate::Signal::try_from(signal).is_ok()),
                    PhysicalResumeOwner::PreRegistrationCleanup => {
                        context.operation == PhysicalResumeOperation::Continue
                            && if startup_barrier_cleanup {
                                context.signal == Some(libc::SIGKILL)
                            } else {
                                context.signal.is_none()
                            }
                    }
                    PhysicalResumeOwner::StartupBarrierCleanup => {
                        context.operation == PhysicalResumeOperation::Continue
                            && context.signal.is_none()
                            && startup_barrier_cleanup
                    }
                    PhysicalResumeOwner::AuthorizedRootExternalCleanup => {
                        context.operation == PhysicalResumeOperation::Continue
                            && context.signal.is_none()
                            && authorized_root_external_cleanup
                    }
                    PhysicalResumeOwner::SynchronousCancellation
                    | PhysicalResumeOwner::RootCleanup
                    | PhysicalResumeOwner::DescendantCleanup => {
                        context.operation == PhysicalResumeOperation::Continue
                            && if startup_wait_failure_handoff {
                                context.signal.is_none()
                            } else if registered_wait_failure_handoff {
                                context.signal == Some(libc::SIGKILL)
                            } else {
                                context.signal.is_none()
                            }
                    }
                };
                if let Some(status) = context.source_status
                    && statuses.get(&status).is_some_and(|track| track.undecodable)
                    && !context.owner.is_registered_controller_cleanup()
                    && !context.owner.is_startup_barrier_cleanup()
                {
                    violations.push(
                        PhysicalPartitionViolation::InvalidUndecodableStatusLifecycle(status),
                    );
                }
                if !authority_matches || !status_matches {
                    violations.push(PhysicalPartitionViolation::WrongResumeTask(id));
                }
                if !publication_precedes_attempt {
                    violations.push(PhysicalPartitionViolation::ResumeBeforeStatusPublication(
                        id,
                    ));
                }
                if !cleanup_shape_valid {
                    violations.push(PhysicalPartitionViolation::InvalidCleanupResumeShape(id));
                }
                let resume_was_authorized_in_time =
                    if context.owner == PhysicalResumeOwner::StartupBarrierCleanup {
                        opaque_launch_startup_resume
                    } else {
                        identity_was_bound
                            || (context.owner == PhysicalResumeOwner::PreRegistrationCleanup
                                && pre_registration_was_linked)
                            || (context.owner == PhysicalResumeOwner::TypedStopped
                                && context.task.is_direct_child()
                                && pre_registration_was_linked
                                && context.source_status.is_some_and(|status| {
                                    statuses.get(&status).is_some_and(|track| {
                                        track.publication_destination
                                            == Some(PhysicalStatusPublication::DirectStopped)
                                    })
                                }))
                    };
                if !resume_was_authorized_in_time {
                    violations.push(PhysicalPartitionViolation::ResumeBeforeIdentityBound(id));
                }
            }
            PhysicalEventRecordKind::ResumeResult { attempt, outcome } => {
                if !resume_attempts.contains_key(&attempt) {
                    violations.push(PhysicalPartitionViolation::ResumeResultWithoutAttempt(
                        attempt,
                    ));
                }
                if outcome == PhysicalResumeOutcome::Error(0) {
                    violations.push(PhysicalPartitionViolation::InvalidCleanupResumeShape(
                        attempt,
                    ));
                }
                if resume_results.insert(attempt, outcome).is_some() {
                    violations.push(PhysicalPartitionViolation::DuplicateResumeResult(attempt));
                }
                resume_result_sequences.insert(attempt, record.sequence);
            }
            PhysicalEventRecordKind::PidfdSignalAttempt { id, context } => {
                let duplicate_context = pidfd_signal_attempts.insert(id, context).is_some();
                let duplicate_sequence = pidfd_signal_attempt_sequences
                    .insert(id, record.sequence)
                    .is_some();
                if duplicate_context || duplicate_sequence {
                    violations.push(
                        PhysicalPartitionViolation::InvalidStartupCleanupPidfdSignal(
                            context.transaction,
                        ),
                    );
                }
            }
            PhysicalEventRecordKind::PidfdSignalResult { attempt, outcome } => {
                if !pidfd_signal_attempts.contains_key(&attempt) {
                    violations.push(PhysicalPartitionViolation::PidfdSignalResultWithoutAttempt(
                        attempt,
                    ));
                }
                let duplicate_result = pidfd_signal_results.insert(attempt, outcome).is_some();
                let duplicate_sequence = pidfd_signal_result_sequences
                    .insert(attempt, record.sequence)
                    .is_some();
                if duplicate_result || duplicate_sequence {
                    violations.push(PhysicalPartitionViolation::DuplicatePidfdSignalResult(
                        attempt,
                    ));
                }
            }
            PhysicalEventRecordKind::StartupCleanupPidfdExitProved {
                transaction,
                generation,
                task,
                pidfd,
                revents,
            } => {
                if startup_pidfd_exit_proofs
                    .insert(
                        transaction,
                        (generation, task, pidfd, revents, record.sequence),
                    )
                    .is_some()
                {
                    violations.push(
                        PhysicalPartitionViolation::InvalidStartupCleanupPidfdSignal(transaction),
                    );
                }
            }
            PhysicalEventRecordKind::ResumeErrorTolerated { attempt, errno } => {
                if tolerated.contains_key(&attempt) || tolerated_sequences.contains_key(&attempt) {
                    violations.push(PhysicalPartitionViolation::DuplicateToleratedResumeError(
                        attempt,
                    ));
                } else {
                    tolerated.insert(attempt, errno);
                    tolerated_sequences.insert(attempt, record.sequence);
                }
            }
            _ => {}
        }
    }

    for wait in echild_terminal_proofs.keys() {
        if !used_echild_terminal_proofs.contains(wait) {
            violations.push(PhysicalPartitionViolation::InvalidEchildTerminalProof(
                *wait,
            ));
        }
    }

    let mut startup_fallback_prepared = BTreeMap::<
        PhysicalWaitAttemptId,
        (
            PhysicalOriginalRootLaunchId,
            PhysicalEventGenerationId,
            PhysicalTaskIdentity,
            PhysicalCleanupTransactionId,
            u64,
        ),
    >::new();
    let mut startup_fallback_released = BTreeMap::<
        PhysicalWaitAttemptId,
        (
            PhysicalEventGenerationId,
            PhysicalWaitAttemptId,
            PhysicalStatusId,
            u64,
        ),
    >::new();
    let mut startup_prepared_transactions =
        BTreeMap::<PhysicalCleanupTransactionId, Option<PhysicalWaitAttemptId>>::new();
    for record in &snapshot.records {
        match record.kind {
            PhysicalEventRecordKind::StartupBarrierFallbackPrepared {
                launch,
                generation,
                barrier,
                task,
                transaction,
            } => {
                let launch_valid = exact_original_root_launch_captured(
                    Some(launch),
                    generation,
                    task,
                    record.sequence,
                );
                let barrier_valid = wait_attempts.get(&barrier).is_some_and(|context| {
                    context.generation == Some(generation)
                        && context.producer == PhysicalWaitProducer::PreRegistrationBarrier
                        && task.authorizes(context.task, false)
                }) && wait_result_sequences
                    .get(&barrier)
                    .is_some_and(|result| *result < record.sequence)
                    && matches!(
                        wait_outcomes.get(&barrier),
                        Some(
                            PhysicalWaitOutcome::RetainedStatus { .. }
                                | PhysicalWaitOutcome::RetainedUndecodableStatus { .. }
                        )
                    );
                if !launch_valid
                    || !barrier_valid
                    || startup_prepared_transactions
                        .insert(transaction, Some(barrier))
                        .is_some()
                    || startup_fallback_prepared
                        .insert(
                            barrier,
                            (launch, generation, task, transaction, record.sequence),
                        )
                        .is_some()
                {
                    violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                        barrier,
                    ));
                }
            }
            PhysicalEventRecordKind::StartupBarrierFallbackReleased {
                generation,
                barrier,
                consuming_wait,
                status,
            } if startup_fallback_released
                .insert(
                    barrier,
                    (generation, consuming_wait, status, record.sequence),
                )
                .is_some() =>
            {
                violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                    barrier,
                ));
            }
            _ => {}
        }
    }
    for (generation, _, _, transaction, _, _) in &startup_setup_prepared {
        if startup_prepared_transactions
            .insert(*transaction, None)
            .is_some()
        {
            violations
                .push(PhysicalPartitionViolation::InvalidPreRegistrationSetupFailure(*generation));
        }
    }
    let mut startup_barrier_owners =
        BTreeMap::<PhysicalWaitAttemptId, PhysicalWaitAttemptId>::new();
    let mut startup_consuming_waits = BTreeSet::new();
    let mut startup_barrier_failure_transactions =
        BTreeMap::<PhysicalCleanupTransactionId, PhysicalWaitAttemptId>::new();
    for (generation, barrier, consuming_wait, consumed_status, link_sequence) in
        &startup_barrier_consumptions
    {
        let barrier_context = wait_attempts.get(barrier);
        let consuming_context = wait_attempts.get(consuming_wait);
        let barrier_result = wait_result_sequences.get(barrier).copied();
        let consuming_attempt = wait_attempt_sequences.get(consuming_wait).copied();
        let consuming_result = wait_result_sequences.get(consuming_wait).copied();
        let retained = wait_outcomes.get(barrier);
        let consumed = wait_outcomes.get(consuming_wait);
        let byte_exact = matches!(
            (retained, consumed),
            (
                Some(PhysicalWaitOutcome::RetainedStatus {
                    raw_status: retained_raw,
                    siginfo: retained_siginfo,
                }),
                Some(PhysicalWaitOutcome::Status {
                    id,
                    raw_status,
                    siginfo: Some(siginfo),
                }),
            ) if id == consumed_status
                && raw_status == retained_raw
                && siginfo == retained_siginfo
        ) || matches!(
            (retained, consumed),
            (
                Some(PhysicalWaitOutcome::RetainedUndecodableStatus {
                    siginfo: retained_siginfo,
                    error: retained_error,
                }),
                Some(PhysicalWaitOutcome::UndecodableStatus {
                    id,
                    siginfo,
                    error,
                }),
            ) if id == consumed_status
                && siginfo == retained_siginfo
                && error == retained_error
        );
        let same_generation_task = matches!((barrier_context, consuming_context),
            (Some(barrier_context), Some(consuming_context))
                if barrier_context.generation == Some(*generation)
                    && consuming_context.generation == Some(*generation)
                    && barrier_context.task.same_stable_task(consuming_context.task)
        );
        let ordered = barrier_result.is_some_and(|barrier_result| {
            consuming_attempt.is_some_and(|consuming_attempt| {
                consuming_result.is_some_and(|consuming_result| {
                    barrier_result < consuming_attempt
                        && consuming_attempt < consuming_result
                        && consuming_result < *link_sequence
                })
            })
        });
        let producer_order = consuming_context.is_some_and(|context| match context.producer {
            PhysicalWaitProducer::AuthorizedRootNotifier => {
                let identity_bound =
                    identity_binding_sequences
                        .get(generation)
                        .is_some_and(|bindings| {
                            bindings.iter().any(|(_, sequence)| {
                                barrier_result.is_some_and(|barrier_result| {
                                    barrier_result < *sequence
                                        && continued_authorities.get(generation).is_some_and(
                                            |authority| {
                                                *sequence < authority.enabled_sequence
                                                    && worker_start_sequences
                                                        .get(generation)
                                                        .is_some_and(|started| {
                                                            authority.enabled_sequence < *started
                                                                && *started
                                                                    < consuming_attempt
                                                                        .unwrap_or(u64::MAX)
                                                        })
                                            },
                                        )
                                })
                            })
                        });
                let first_authorized_wait = wait_attempts
                    .iter()
                    .filter(|(_, candidate)| {
                        candidate.generation == Some(*generation)
                            && candidate.producer == PhysicalWaitProducer::AuthorizedRootNotifier
                    })
                    .filter_map(|(attempt, _)| {
                        wait_attempt_sequences
                            .get(attempt)
                            .copied()
                            .map(|sequence| (*attempt, sequence))
                    })
                    .min_by_key(|(_, sequence)| *sequence)
                    .map(|(attempt, _)| attempt);
                identity_bound && first_authorized_wait == Some(*consuming_wait)
            }
            PhysicalWaitProducer::PreRegistrationBarrierCleanup => {
                exact_original_root_launch_captured(
                    None,
                    *generation,
                    context.task,
                    barrier_result.unwrap_or(u64::MAX),
                ) && !worker_start_sequences.contains_key(generation)
                    && wait_attempts.values().all(|candidate| {
                        candidate.generation != Some(*generation)
                            || candidate.producer != PhysicalWaitProducer::AuthorizedRootNotifier
                    })
            }
            _ => false,
        });
        let prepared_order = startup_fallback_prepared.get(barrier).is_some_and(
            |(_, prepared_generation, prepared_task, prepared_transaction, prepared_sequence)| {
                *prepared_generation == *generation
                    && consuming_context
                        .is_some_and(|context| prepared_task.authorizes(context.task, false))
                    && barrier_result.is_some_and(|result| result < *prepared_sequence)
                    && consuming_attempt.is_some_and(|attempt| *prepared_sequence < attempt)
                    && consuming_context.is_some_and(|context| match context.producer {
                        PhysicalWaitProducer::AuthorizedRootNotifier => {
                            !cleanup_transactions.contains_key(prepared_transaction)
                        }
                        PhysicalWaitProducer::PreRegistrationBarrierCleanup => cleanup_transactions
                            .get(prepared_transaction)
                            .is_some_and(|cleanup| {
                                cleanup.starts == 1
                                    && cleanup.cause_wait == Some(*consuming_wait)
                                    && cleanup.statuses.contains_key(consumed_status)
                                    && matches!(
                                        cleanup.kind,
                                        Some(
                                            PhysicalCleanupTransactionKind::StartupBarrier {
                                                generation: transaction_generation,
                                                barrier: transaction_barrier,
                                                task,
                                                owner: PhysicalStartupCleanupOwner::Unstarted,
                                            }
                                        ) if transaction_generation == *generation
                                            && transaction_barrier == *barrier
                                            && prepared_task.authorizes(task, false)
                                    )
                            }),
                        _ => false,
                    })
            },
        );
        let release_order = consuming_context.is_some_and(|context| match context.producer {
            PhysicalWaitProducer::AuthorizedRootNotifier => {
                startup_fallback_released.get(barrier).is_some_and(
                    |(released_generation, released_wait, released_status, released)| {
                        *released_generation == *generation
                            && *released_wait == *consuming_wait
                            && *released_status == *consumed_status
                            && *link_sequence < *released
                            && statuses.get(consumed_status).is_some_and(|status| {
                                status
                                    .publication_sequence
                                    .is_some_and(|published| *released < published)
                            })
                    },
                )
            }
            PhysicalWaitProducer::PreRegistrationBarrierCleanup => {
                !startup_fallback_released.contains_key(barrier)
            }
            _ => false,
        });
        let unique = startup_barrier_owners
            .insert(*barrier, *consuming_wait)
            .is_none()
            && startup_consuming_waits.insert(*consuming_wait);
        if !byte_exact
            || !same_generation_task
            || !ordered
            || !producer_order
            || !prepared_order
            || !release_order
            || !unique
        {
            violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                *barrier,
            ));
        }
    }
    for (generation, barrier, consuming_wait, consumed_status, transaction, link_sequence) in
        &startup_barrier_failures
    {
        let barrier_context = wait_attempts.get(barrier);
        let consuming_context = wait_attempts.get(consuming_wait);
        let barrier_result = wait_result_sequences.get(barrier).copied();
        let consuming_attempt = wait_attempt_sequences.get(consuming_wait).copied();
        let consuming_result = wait_result_sequences.get(consuming_wait).copied();
        let retained = wait_outcomes.get(barrier);
        let consumed = wait_outcomes.get(consuming_wait);
        let mismatched = match (retained, consumed) {
            (
                Some(PhysicalWaitOutcome::RetainedStatus {
                    raw_status: retained_raw,
                    siginfo: retained_siginfo,
                }),
                Some(PhysicalWaitOutcome::Status {
                    id,
                    raw_status,
                    siginfo: Some(siginfo),
                }),
            ) => {
                id == consumed_status && (raw_status != retained_raw || siginfo != retained_siginfo)
            }
            (
                Some(PhysicalWaitOutcome::RetainedStatus { .. }),
                Some(PhysicalWaitOutcome::UndecodableStatus { id, .. }),
            ) => id == consumed_status,
            (
                Some(PhysicalWaitOutcome::RetainedUndecodableStatus { .. }),
                Some(PhysicalWaitOutcome::Status { id, .. }),
            ) => id == consumed_status,
            (
                Some(PhysicalWaitOutcome::RetainedUndecodableStatus { .. }),
                Some(PhysicalWaitOutcome::UndecodableStatus { id, .. }),
            ) => id == consumed_status,
            _ => false,
        };
        let same_generation_task = matches!((barrier_context, consuming_context),
            (Some(barrier_context), Some(consuming_context))
                if barrier_context.generation == Some(*generation)
                    && consuming_context.generation == Some(*generation)
                    && barrier_context.task.same_stable_task(consuming_context.task)
                    && barrier_context.producer == PhysicalWaitProducer::PreRegistrationBarrier
                    && matches!(
                        consuming_context.producer,
                        PhysicalWaitProducer::AuthorizedRootNotifier
                            | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                    )
        );
        let cleanup = cleanup_transactions.get(transaction);
        let ordered = barrier_result.is_some_and(|barrier_result| {
            consuming_attempt.is_some_and(|consuming_attempt| {
                consuming_result.is_some_and(|consuming_result| {
                    cleanup.is_some_and(|cleanup| {
                        cleanup.start_sequence.is_some_and(|started| {
                            cleanup.statuses.get(consumed_status).is_some_and(|linked| {
                                statuses.get(consumed_status).is_some_and(|status| {
                                    status.publication_sequence.is_some_and(|published| {
                                        barrier_result < consuming_attempt
                                            && consuming_attempt < consuming_result
                                            && consuming_result < started
                                            && started < *linked
                                            && match cleanup.kind {
                                                Some(
                                                    PhysicalCleanupTransactionKind::Registered,
                                                ) => {
                                                    *linked < published
                                                        && published < *link_sequence
                                                }
                                                Some(
                                                    PhysicalCleanupTransactionKind::StartupBarrier {
                                                        ..
                                                    },
                                                ) => {
                                                    *linked < *link_sequence
                                                        && *link_sequence < published
                                                }
                                                Some(
                                                    PhysicalCleanupTransactionKind::StartupSetup {
                                                        ..
                                                    },
                                                ) => false,
                                                None => false,
                                            }
                                    })
                                })
                            })
                        })
                    })
                })
            })
        });
        let authority_ordered = consuming_context.is_some_and(|context| match context.producer {
            PhysicalWaitProducer::AuthorizedRootNotifier => continued_authorities
                .get(generation)
                .is_some_and(|authority| {
                    identity_binding_sequences
                        .get(generation)
                        .is_some_and(|bindings| {
                            bindings.iter().any(|(_, identity)| {
                                barrier_result.is_some_and(|barrier_result| {
                                    barrier_result < *identity
                                        && *identity < authority.enabled_sequence
                                        && worker_start_sequences.get(generation).is_some_and(
                                            |started| {
                                                authority.enabled_sequence < *started
                                                    && *started
                                                        < consuming_attempt.unwrap_or(u64::MAX)
                                            },
                                        )
                                })
                            })
                        })
                }),
            PhysicalWaitProducer::PreRegistrationBarrierCleanup => {
                !continued_authorities.contains_key(generation)
                    && !worker_start_sequences.contains_key(generation)
                    && exact_original_root_launch_captured(
                        None,
                        *generation,
                        context.task,
                        consuming_attempt.unwrap_or(u64::MAX),
                    )
            }
            _ => false,
        });
        let prepared_order = startup_fallback_prepared.get(barrier).is_some_and(
            |(_, prepared_generation, prepared_task, prepared_transaction, prepared_sequence)| {
                *prepared_generation == *generation
                    && consuming_context
                        .is_some_and(|context| prepared_task.authorizes(context.task, false))
                    && barrier_result.is_some_and(|result| result < *prepared_sequence)
                    && consuming_attempt.is_some_and(|attempt| *prepared_sequence < attempt)
                    && transaction == prepared_transaction
                    && !startup_fallback_released.contains_key(barrier)
            },
        );
        let first_authorized_wait = wait_attempts
            .iter()
            .filter(|(_, candidate)| {
                candidate.generation == Some(*generation)
                    && candidate.producer == PhysicalWaitProducer::AuthorizedRootNotifier
            })
            .filter_map(|(attempt, _)| {
                wait_attempt_sequences
                    .get(attempt)
                    .copied()
                    .map(|sequence| (*attempt, sequence))
            })
            .min_by_key(|(_, sequence)| *sequence)
            .map(|(attempt, _)| attempt);
        let first_startup_cleanup_wait = wait_attempts
            .iter()
            .filter(|(_, candidate)| {
                candidate.generation == Some(*generation)
                    && candidate.producer == PhysicalWaitProducer::PreRegistrationBarrierCleanup
            })
            .filter_map(|(attempt, _)| {
                wait_attempt_sequences
                    .get(attempt)
                    .copied()
                    .map(|sequence| (*attempt, sequence))
            })
            .min_by_key(|(_, sequence)| *sequence)
            .map(|(attempt, _)| attempt);
        let exact_transaction = cleanup.is_some_and(|cleanup| {
            cleanup.starts == 1
                && cleanup.cause_wait == Some(*consuming_wait)
                && cleanup.statuses.contains_key(consumed_status)
                && match cleanup.kind {
                    Some(PhysicalCleanupTransactionKind::Registered) => consuming_context
                        .is_some_and(|context| {
                            context.producer == PhysicalWaitProducer::AuthorizedRootNotifier
                        }),
                    Some(PhysicalCleanupTransactionKind::StartupBarrier {
                        generation: transaction_generation,
                        barrier: transaction_barrier,
                        task,
                        owner,
                    }) => {
                        transaction_generation == *generation
                            && transaction_barrier == *barrier
                            && consuming_context.is_some_and(|context| {
                                context.producer
                                    == match owner {
                                        PhysicalStartupCleanupOwner::Unstarted => {
                                            PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                        }
                                        PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                            PhysicalWaitProducer::AuthorizedRootNotifier
                                        }
                                    }
                                    && task.authorizes(context.task, false)
                            })
                    }
                    Some(PhysicalCleanupTransactionKind::StartupSetup { .. }) => false,
                    None => false,
                }
        });
        let unique = startup_barrier_owners
            .insert(*barrier, *consuming_wait)
            .is_none()
            && startup_consuming_waits.insert(*consuming_wait)
            && startup_barrier_failure_transactions
                .insert(*transaction, *barrier)
                .is_none();
        if !mismatched
            || !same_generation_task
            || !ordered
            || !authority_ordered
            || !prepared_order
            || !consuming_context.is_some_and(|context| match context.producer {
                PhysicalWaitProducer::AuthorizedRootNotifier => {
                    first_authorized_wait == Some(*consuming_wait)
                }
                PhysicalWaitProducer::PreRegistrationBarrierCleanup => {
                    first_startup_cleanup_wait == Some(*consuming_wait)
                }
                _ => false,
            })
            || !exact_transaction
            || !unique
        {
            violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                *barrier,
            ));
        }
    }
    let mut valid_startup_barrier_statusless_transactions = BTreeSet::new();
    let mut valid_startup_barrier_statusless_resolution_sequences = BTreeSet::new();
    for (generation, barrier, cause_wait, error, transaction, link_sequence) in
        &startup_barrier_statusless_failures
    {
        let barrier_context = wait_attempts.get(barrier);
        let cause_context = wait_attempts.get(cause_wait);
        let barrier_result = wait_result_sequences.get(barrier).copied();
        let cause_attempt = wait_attempt_sequences.get(cause_wait).copied();
        let cause_result = wait_result_sequences.get(cause_wait).copied();
        let cleanup = cleanup_transactions.get(transaction);
        let authorized_wait_count = wait_attempts
            .iter()
            .filter(|(_, candidate)| {
                candidate.generation == Some(*generation)
                    && candidate.producer == PhysicalWaitProducer::AuthorizedRootNotifier
            })
            .count();
        let mut authorized_waits = wait_attempts
            .iter()
            .filter(|(_, candidate)| {
                candidate.generation == Some(*generation)
                    && candidate.producer == PhysicalWaitProducer::AuthorizedRootNotifier
            })
            .filter_map(|(attempt, _)| {
                Some((
                    *attempt,
                    wait_attempt_sequences.get(attempt).copied()?,
                    wait_result_sequences.get(attempt).copied()?,
                ))
            })
            .collect::<Vec<_>>();
        authorized_waits.sort_by_key(|(_, attempt, _)| *attempt);
        let authorized_wait_prefix_valid = authorized_waits.len() == authorized_wait_count
            && authorized_waits
                .last()
                .is_some_and(|(attempt, _, _)| *attempt == *cause_wait)
            && authorized_waits.iter().enumerate().all(
                |(index, (attempt, attempt_sequence, result_sequence))| {
                    *attempt_sequence < *result_sequence
                        && authorized_waits
                            .get(index + 1)
                            .is_none_or(|(_, next_attempt, _)| *result_sequence < *next_attempt)
                        && if *attempt == *cause_wait {
                            index + 1 == authorized_waits.len()
                                && wait_outcome_matches_errno(wait_outcomes.get(attempt), *error)
                        } else {
                            matches!(
                                wait_outcomes.get(attempt),
                                Some(PhysicalWaitOutcome::Interrupted)
                            )
                        }
                },
            );
        let first_authorized_attempt_sequence =
            authorized_waits.first().map(|(_, attempt, _)| *attempt);
        let exact_prepared = startup_fallback_prepared.get(barrier).is_some_and(
            |(_, prepared_generation, prepared_task, prepared_transaction, prepared_sequence)| {
                *prepared_generation == *generation
                    && *prepared_transaction == *transaction
                    && barrier_result.is_some_and(|result| result < *prepared_sequence)
                    && first_authorized_attempt_sequence
                        .is_some_and(|attempt| *prepared_sequence < attempt)
                    && cause_context
                        .is_some_and(|context| prepared_task.authorizes(context.task, false))
                    && !startup_fallback_released.contains_key(barrier)
            },
        );
        let exact_transaction = cleanup.is_some_and(|cleanup| {
            cleanup.starts == 1
                && cleanup.cause_wait == Some(*cause_wait)
                && cleanup.start_sequence.is_some_and(|started| {
                    cause_result.is_some_and(|result| result < started && started < *link_sequence)
                })
                && matches!(
                    cleanup.kind,
                    Some(PhysicalCleanupTransactionKind::StartupBarrier {
                        generation: transaction_generation,
                        barrier: transaction_barrier,
                        task,
                        owner: PhysicalStartupCleanupOwner::AuthorizedWorker,
                    }) if transaction_generation == *generation
                        && transaction_barrier == *barrier
                        && cause_context
                            .is_some_and(|context| task.authorizes(context.task, false))
                )
        });
        let authority_ordered = barrier_result.is_some_and(|barrier_result| {
            continued_authorities
                .get(generation)
                .is_some_and(|authority| {
                    identity_binding_sequences
                        .get(generation)
                        .is_some_and(|bindings| {
                            bindings.iter().any(|(_, identity)| {
                                worker_start_sequences
                                    .get(generation)
                                    .is_some_and(|started| {
                                        barrier_result < *identity
                                            && *identity < authority.enabled_sequence
                                            && authority.enabled_sequence < *started
                                            && first_authorized_attempt_sequence
                                                .is_some_and(|attempt| *started < attempt)
                                    })
                            })
                        })
                })
        });
        let matching_resolutions = startup_barrier_statusless_resolutions
            .iter()
            .filter(
                |(resolved_generation, resolved_barrier, _, _, resolved_transaction, _)| {
                    *resolved_generation == *generation
                        && *resolved_barrier == *barrier
                        && *resolved_transaction == *transaction
                },
            )
            .collect::<Vec<_>>();
        let exact_resolution = match matching_resolutions.as_slice() {
            [(_, _, cleanup_wait, resolved_status, _, resolution_sequence)] => {
                let cleanup_wait = *cleanup_wait;
                let resolution_sequence = *resolution_sequence;
                let cleanup_context = wait_attempts.get(&cleanup_wait);
                let cleanup_attempt = wait_attempt_sequences.get(&cleanup_wait).copied();
                let cleanup_result = wait_result_sequences.get(&cleanup_wait).copied();
                let signal_sequences = pidfd_signal_attempts
                    .iter()
                    .filter(|(_, context)| context.transaction == *transaction)
                    .filter_map(|(attempt, _)| {
                        pidfd_signal_attempt_sequences
                            .get(attempt)
                            .copied()
                            .zip(pidfd_signal_result_sequences.get(attempt).copied())
                    })
                    .collect::<Vec<_>>();
                let ordered = matches!(signal_sequences.as_slice(), [(signal, result)]
                    if *link_sequence < *signal
                        && *signal < *result
                        && cleanup_attempt.is_some_and(|attempt| *result < attempt)
                        && cleanup_result.is_some_and(|result| {
                            cleanup_attempt.is_some_and(|attempt| {
                                attempt < result && result < resolution_sequence
                            })
                        })
                        && cleanup.and_then(|cleanup| cleanup.completion_sequence)
                            .is_some_and(|completed| resolution_sequence < completed));
                let exact_shape = match *resolved_status {
                    Some(status) => {
                        let retained = wait_outcomes.get(barrier);
                        let consumed = wait_outcomes.get(&cleanup_wait);
                        let exact_retained = matches!(
                            (retained, consumed),
                            (
                                Some(PhysicalWaitOutcome::RetainedStatus {
                                    raw_status: retained_raw,
                                    siginfo: retained_siginfo,
                                }),
                                Some(PhysicalWaitOutcome::Status {
                                    id,
                                    raw_status,
                                    siginfo: Some(siginfo),
                                }),
                            ) if *id == status
                                && raw_status == retained_raw
                                && siginfo == retained_siginfo
                        ) || matches!(
                            (retained, consumed),
                            (
                                Some(PhysicalWaitOutcome::RetainedUndecodableStatus {
                                    siginfo: retained_siginfo,
                                    error: retained_error,
                                }),
                                Some(PhysicalWaitOutcome::UndecodableStatus {
                                    id,
                                    siginfo,
                                    error,
                                }),
                            ) if *id == status
                                && siginfo == retained_siginfo
                                && error == retained_error
                        );
                        let terminal_superseded = matches!(
                            consumed,
                            Some(PhysicalWaitOutcome::Status { id, raw_status, .. })
                                if *id == status && is_terminal_raw_status(*raw_status)
                        );
                        let exit_stop_signal_attempts = pidfd_signal_attempts
                            .iter()
                            .filter(|(_, context)| context.transaction == *transaction)
                            .collect::<Vec<_>>();
                        let accepted_exit_signal = matches!(
                            exit_stop_signal_attempts.as_slice(),
                            [(signal_attempt, _)] if pidfd_signal_results
                                .get(signal_attempt)
                                .is_some_and(|outcome| {
                                    matches!(
                                        outcome,
                                        PhysicalPidfdSignalOutcome::Success
                                            | PhysicalPidfdSignalOutcome::Error(libc::ESRCH)
                                    )
                                }) && pidfd_signal_result_sequences
                                    .get(signal_attempt)
                                    .is_some_and(|signal| {
                                        cleanup_attempt.is_some_and(|wait| *signal < wait)
                                    })
                        );
                        let exit_stop_resumes = resume_attempts
                            .iter()
                            .filter(|(_, context)| context.source_status == Some(status))
                            .collect::<Vec<_>>();
                        let exact_exit_resume = matches!(
                            exit_stop_resumes.as_slice(),
                            [(resume, context)] if context.owner
                                == PhysicalResumeOwner::AuthorizedRootExternalCleanup
                                && context.operation == PhysicalResumeOperation::Continue
                                && context.signal.is_none()
                                && resume_attempt_sequences.get(resume).is_some_and(|attempt| {
                                    resolution_sequence < *attempt
                                        && resume_result_sequences.get(resume).is_some_and(
                                            |result| {
                                                *attempt < *result
                                                    && cleanup
                                                        .and_then(|cleanup| {
                                                            cleanup.completion_sequence
                                                        })
                                                        .is_some_and(|completed| {
                                                            *result < completed
                                                        })
                                            },
                                        )
                                })
                        );
                        let exit_stop_superseded = matches!(
                            consumed,
                            Some(PhysicalWaitOutcome::Status { id, raw_status, .. })
                                if *id == status && is_ptrace_exit_stop(*raw_status)
                        ) && accepted_exit_signal
                            && exact_exit_resume;
                        (exact_retained || terminal_superseded || exit_stop_superseded)
                            && cleanup.is_some_and(|cleanup| {
                                cleanup.statuses.get(&status).is_some_and(|linked| {
                                    statuses.get(&status).is_some_and(|status| {
                                        status.publication_sequence.is_some_and(|published| {
                                            *linked < published && published < resolution_sequence
                                        })
                                    })
                                })
                            })
                    }
                    None => {
                        matches!(
                            wait_outcomes.get(&cleanup_wait),
                            Some(PhysicalWaitOutcome::NoChild)
                        ) && cleanup.is_some_and(|cleanup| {
                            cleanup.pidfd_exit_proof.is_some_and(
                                |(proof_wait, _, _, revents, _, proof_sequence)| {
                                    proof_wait == cleanup_wait
                                        && revents & libc::POLLIN != 0
                                        && cleanup_result.is_some_and(|result| {
                                            result < proof_sequence
                                                && proof_sequence < resolution_sequence
                                        })
                                },
                            )
                        })
                    }
                };
                ordered
                    && exact_shape
                    && cleanup_context.is_some_and(|context| {
                        context.generation == Some(*generation)
                            && context.producer == PhysicalWaitProducer::RegisteredCleanup
                            && cause_context
                                .is_some_and(|cause| cause.task.authorizes(context.task, false))
                    })
            }
            _ => false,
        };
        let valid = *error != 0
            && *error != libc::EINTR
            && authorized_wait_prefix_valid
            && matches!(
                (barrier_context, cause_context),
                (Some(barrier_context), Some(cause_context))
                    if barrier_context.generation == Some(*generation)
                        && barrier_context.producer
                            == PhysicalWaitProducer::PreRegistrationBarrier
                        && cause_context.generation == Some(*generation)
                        && cause_context.producer
                            == PhysicalWaitProducer::AuthorizedRootNotifier
                        && barrier_context.task.same_stable_task(cause_context.task)
            )
            && matches!(
                wait_outcomes.get(barrier),
                Some(
                    PhysicalWaitOutcome::RetainedStatus { .. }
                        | PhysicalWaitOutcome::RetainedUndecodableStatus { .. }
                )
            )
            && wait_outcome_matches_errno(wait_outcomes.get(cause_wait), *error)
            && barrier_result.is_some_and(|barrier_result| {
                cause_attempt.is_some_and(|cause_attempt| {
                    cause_result.is_some_and(|cause_result| {
                        barrier_result < cause_attempt
                            && cause_attempt < cause_result
                            && cause_result < *link_sequence
                    })
                })
            })
            && exact_prepared
            && exact_transaction
            && authority_ordered
            && exact_resolution
            && startup_barrier_owners
                .insert(*barrier, *cause_wait)
                .is_none()
            && startup_consuming_waits.insert(*cause_wait)
            && startup_barrier_failure_transactions
                .insert(*transaction, *barrier)
                .is_none();
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                *barrier,
            ));
        } else {
            valid_startup_barrier_statusless_transactions.insert(*transaction);
            if let [(_, _, _, _, _, resolution_sequence)] = matching_resolutions.as_slice() {
                valid_startup_barrier_statusless_resolution_sequences.insert(*resolution_sequence);
            }
        }
    }
    for barrier in startup_fallback_prepared.keys() {
        if !startup_barrier_owners.contains_key(barrier) {
            violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                *barrier,
            ));
        }
    }
    for (_, barrier, _, _, transaction, sequence) in &startup_barrier_statusless_resolutions {
        if !valid_startup_barrier_statusless_transactions.contains(transaction)
            || !valid_startup_barrier_statusless_resolution_sequences.contains(sequence)
        {
            violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                *barrier,
            ));
        }
    }
    for barrier in startup_fallback_released.keys() {
        if !startup_barrier_consumptions
            .iter()
            .any(|(_, consumed_barrier, ..)| consumed_barrier == barrier)
        {
            violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                *barrier,
            ));
        }
    }
    for (attempt, context) in &wait_attempts {
        if context.producer == PhysicalWaitProducer::PreRegistrationBarrier {
            let retained_owner = startup_barrier_owners.contains_key(attempt)
                && matches!(
                    wait_outcomes.get(attempt),
                    Some(
                        PhysicalWaitOutcome::RetainedStatus { .. }
                            | PhysicalWaitOutcome::RetainedUndecodableStatus { .. }
                    )
                );
            let ordered_interrupted_predecessor = matches!(
                wait_outcomes.get(attempt),
                Some(PhysicalWaitOutcome::Interrupted)
            ) && startup_barrier_consumptions.iter().any(
                |(generation, barrier, _, _, _)| {
                    let Some(barrier_context) = wait_attempts.get(barrier) else {
                        return false;
                    };
                    context.generation == Some(*generation)
                        && barrier_context.generation == context.generation
                        && barrier_context.task.same_stable_task(context.task)
                        && wait_result_sequences
                            .get(attempt)
                            .is_some_and(|interrupted| {
                                wait_attempt_sequences
                                    .get(barrier)
                                    .is_some_and(|retained| interrupted < retained)
                            })
                },
            ) || matches!(
                wait_outcomes.get(attempt),
                Some(PhysicalWaitOutcome::Interrupted)
            ) && startup_barrier_failures.iter().any(
                |(generation, barrier, _, _, _, _)| {
                    let Some(barrier_context) = wait_attempts.get(barrier) else {
                        return false;
                    };
                    context.generation == Some(*generation)
                        && barrier_context.generation == context.generation
                        && barrier_context.task.same_stable_task(context.task)
                        && wait_result_sequences
                            .get(attempt)
                            .is_some_and(|interrupted| {
                                wait_attempt_sequences
                                    .get(barrier)
                                    .is_some_and(|retained| interrupted < retained)
                            })
                },
            ) || matches!(
                wait_outcomes.get(attempt),
                Some(PhysicalWaitOutcome::Interrupted)
            ) && startup_barrier_statusless_failures
                .iter()
                .any(|(generation, barrier, _, _, _, _)| {
                    let Some(barrier_context) = wait_attempts.get(barrier) else {
                        return false;
                    };
                    context.generation == Some(*generation)
                        && barrier_context.generation == context.generation
                        && barrier_context.task.same_stable_task(context.task)
                        && wait_result_sequences
                            .get(attempt)
                            .is_some_and(|interrupted| {
                                wait_attempt_sequences
                                    .get(barrier)
                                    .is_some_and(|retained| interrupted < retained)
                            })
                });
            let setup_failure_owned = startup_barrier_setup_failures.iter().any(
                |(generation, task, _, _, failure_sequence)| {
                    context.generation == Some(*generation)
                        && context.task.authorizes(*task, true)
                        && wait_result_sequences
                            .get(attempt)
                            .is_some_and(|result| *result < *failure_sequence)
                        && match wait_outcomes.get(attempt) {
                            Some(PhysicalWaitOutcome::Interrupted) => true,
                            Some(PhysicalWaitOutcome::Error(error)) => *error != libc::EINTR,
                            _ => false,
                        }
                },
            );
            if !retained_owner && !ordered_interrupted_predecessor && !setup_failure_owned {
                violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                    *attempt,
                ));
            }
        }
    }
    let mut valid_startup_setup_failures = BTreeSet::new();
    for (generation, task, error, launch, failure_sequence) in &startup_barrier_setup_failures {
        let linked_once = original_root_launch.is_some_and(
            |(launch_id, launch_generation, launch_task, _, _, launch_sequence)| {
                launch_id == *launch
                    && launch_generation == *generation
                    && launch_task.authorizes(*task, true)
                    && launch_sequence < *failure_sequence
            },
        );
        let matching_prepared = startup_setup_prepared
            .iter()
            .filter(
                |(
                    candidate_generation,
                    candidate_task,
                    candidate_error,
                    _,
                    candidate_launch,
                    _,
                )| {
                    candidate_generation == generation
                        && candidate_task == task
                        && candidate_error == error
                        && candidate_launch == launch
                },
            )
            .collect::<Vec<_>>();
        let typed_cleanup = if matching_prepared.len() == 1 {
            let (_, _, _, transaction, _, prepared_sequence) = *matching_prepared[0];
            let matching_status_linked = startup_setup_linked
                .iter()
                .filter(
                    |(
                        linked_generation,
                        linked_error,
                        _,
                        _,
                        linked_transaction,
                        linked_launch,
                        _,
                    )| {
                        *linked_generation == *generation
                            && *linked_error == *error
                            && *linked_transaction == transaction
                            && *linked_launch == *launch
                    },
                )
                .collect::<Vec<_>>();
            let matching_no_status_linked = startup_setup_no_status_linked
                .iter()
                .filter(
                    |(linked_generation, linked_error, _, linked_transaction, linked_launch, _)| {
                        *linked_generation == *generation
                            && *linked_error == *error
                            && *linked_transaction == transaction
                            && *linked_launch == *launch
                    },
                )
                .collect::<Vec<_>>();
            match (
                matching_status_linked.as_slice(),
                matching_no_status_linked.as_slice(),
            ) {
                ([linked], []) => {
                    let (_, _, consuming_wait, status, _, _, linked_sequence) = **linked;
                    let cleanup = cleanup_transactions.get(&transaction);
                    cleanup.is_some_and(|cleanup| {
                        cleanup.starts == 1
                            && cleanup.cause_wait == Some(consuming_wait)
                            && cleanup.statuses.get(&status).is_some_and(|status_link| {
                                statuses.get(&status).is_some_and(|status_track| {
                                    status_track.publication_sequence.is_some_and(|published| {
                                        wait_attempt_sequences.get(&consuming_wait).is_some_and(
                                            |attempt| {
                                                wait_result_sequences
                                                    .get(&consuming_wait)
                                                    .is_some_and(|result| {
                                                        *failure_sequence < prepared_sequence
                                                            && prepared_sequence < *attempt
                                                            && *attempt < *result
                                                            && cleanup.start_sequence.is_some_and(
                                                                |started| {
                                                                    *result < started
                                                                        && started < *status_link
                                                                        && *status_link
                                                                            < linked_sequence
                                                                        && linked_sequence
                                                                            < published
                                                                },
                                                            )
                                                    })
                                            },
                                        )
                                    })
                                })
                            })
                            && matches!(
                                cleanup.kind,
                                Some(PhysicalCleanupTransactionKind::StartupSetup {
                                    generation: transaction_generation,
                                    task: transaction_task,
                                    error: transaction_error,
                                    launch: transaction_launch,
                                }) if transaction_generation == *generation
                                    && transaction_task == *task
                                    && transaction_error == *error
                                    && transaction_launch == *launch
                            )
                    }) && wait_attempts.get(&consuming_wait).is_some_and(|context| {
                        context.generation == Some(*generation)
                            && context.producer
                                == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                            && context.task == *task
                    })
                }
                ([], [linked]) => {
                    let (_, _, consuming_wait, _, _, linked_sequence) = **linked;
                    let cleanup = cleanup_transactions.get(&transaction);
                    cleanup.is_some_and(|cleanup| {
                        cleanup.starts == 1
                            && cleanup.cause_wait == Some(consuming_wait)
                            && cleanup.statuses.is_empty()
                            && cleanup.start_sequence.is_some_and(|started| {
                                wait_attempt_sequences
                                    .get(&consuming_wait)
                                    .is_some_and(|attempt| {
                                        wait_result_sequences.get(&consuming_wait).is_some_and(
                                            |result| {
                                                *failure_sequence < prepared_sequence
                                                    && prepared_sequence < *attempt
                                                    && *attempt < *result
                                                    && *result < started
                                                    && started < linked_sequence
                                            },
                                        )
                                    })
                            })
                            && cleanup
                                .completion_sequence
                                .is_some_and(|completed| linked_sequence < completed)
                            && matches!(
                                cleanup.kind,
                                Some(PhysicalCleanupTransactionKind::StartupSetup {
                                    generation: transaction_generation,
                                    task: transaction_task,
                                    error: transaction_error,
                                    launch: transaction_launch,
                                }) if transaction_generation == *generation
                                    && transaction_task == *task
                                    && transaction_error == *error
                                    && transaction_launch == *launch
                            )
                    }) && wait_attempts.get(&consuming_wait).is_some_and(|context| {
                        context.generation == Some(*generation)
                            && context.producer
                                == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                            && context.task == *task
                    }) && matches!(
                        wait_outcomes.get(&consuming_wait),
                        Some(PhysicalWaitOutcome::NoChild)
                    )
                }
                _ => false,
            }
        } else {
            false
        };
        let terminal_cleanup = external_generation_finishes.iter().any(
            |(finished_generation, terminal_wait, finished_sequence)| {
                if finished_generation != generation || *finished_sequence <= *failure_sequence {
                    return false;
                }
                wait_attempts.get(terminal_wait).is_some_and(|context| {
                    context.generation == Some(*generation)
                        && context.producer == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                        && context.task.authorizes(*task, true)
                        && wait_result_sequences
                            .get(terminal_wait)
                            .is_some_and(|result| {
                                *failure_sequence < *result && *result < *finished_sequence
                            })
                })
            },
        );
        let barrier_attempts = wait_attempts
            .iter()
            .filter(|(_, context)| {
                context.generation == Some(*generation)
                    && context.producer == PhysicalWaitProducer::PreRegistrationBarrier
                    && context.task.authorizes(*task, true)
            })
            .map(|(attempt, _)| *attempt)
            .collect::<Vec<_>>();
        let fatal_attempts = barrier_attempts
            .iter()
            .filter(|attempt| {
                matches!(wait_outcomes.get(attempt), Some(PhysicalWaitOutcome::Error(error)) if *error != libc::EINTR)
            })
            .copied()
            .collect::<Vec<_>>();
        let barrier_failure_epoch = if barrier_attempts.is_empty() {
            true
        } else {
            fatal_attempts.len() == 1
                && barrier_attempts.iter().all(|attempt| {
                    wait_result_sequences.get(attempt).is_some_and(|result| {
                        *result < *failure_sequence
                            && (Some(*attempt) == fatal_attempts.first().copied()
                                || matches!(
                                    wait_outcomes.get(attempt),
                                    Some(PhysicalWaitOutcome::Interrupted)
                                ))
                    })
                })
                && fatal_attempts.first().is_some_and(|fatal| {
                    wait_attempt_sequences
                        .get(fatal)
                        .is_some_and(|fatal_started| {
                            barrier_attempts.iter().all(|attempt| {
                                *attempt == *fatal
                                    || wait_result_sequences
                                        .get(attempt)
                                        .is_some_and(|result| result < fatal_started)
                            })
                        })
                })
        };
        let exact_setup_error = if let Some(fatal) = fatal_attempts.first() {
            fatal_attempts.len() == 1
                && wait_outcomes.get(fatal) == Some(&PhysicalWaitOutcome::Error(*error))
        } else {
            barrier_attempts.is_empty()
                || barrier_attempts.iter().all(|attempt| {
                    matches!(
                        wait_outcomes.get(attempt),
                        Some(PhysicalWaitOutcome::Interrupted)
                    )
                })
        };
        let no_started_authority = !continued_authorities.contains_key(generation)
            && !worker_start_sequences.contains_key(generation)
            && !wait_attempts.values().any(|context| {
                context.generation == Some(*generation)
                    && context.producer == PhysicalWaitProducer::AuthorizedRootNotifier
            });
        let unique = valid_startup_setup_failures.insert(*generation);
        if *error == 0
            || (!task.is_pidfd_bound_direct_child() && !task.is_captured())
            || !linked_once
            || !typed_cleanup
            || !terminal_cleanup
            || !barrier_failure_epoch
            || !exact_setup_error
            || !no_started_authority
            || !unique
        {
            violations
                .push(PhysicalPartitionViolation::InvalidPreRegistrationSetupFailure(*generation));
        }
    }
    for generation in startup_setup_prepared
        .iter()
        .map(|(generation, ..)| *generation)
        .chain(
            startup_setup_linked
                .iter()
                .map(|(generation, ..)| *generation)
                .chain(
                    startup_setup_no_status_linked
                        .iter()
                        .map(|(generation, ..)| *generation),
                ),
        )
    {
        if !valid_startup_setup_failures.contains(&generation) {
            violations
                .push(PhysicalPartitionViolation::InvalidPreRegistrationSetupFailure(generation));
        }
    }
    for (generation, error, consuming_wait, transaction, launch, _) in
        &startup_setup_no_status_linked
    {
        let matching_failures = startup_barrier_setup_failures
            .iter()
            .filter(|(failed_generation, _, failed_error, failed_launch, _)| {
                failed_generation == generation && failed_error == error && failed_launch == launch
            })
            .collect::<Vec<_>>();
        let exact_owner = match matching_failures.as_slice() {
            [failure] => {
                let (_, task, _, failure_launch, _) = **failure;
                startup_setup_prepared
                    .iter()
                    .filter(
                        |(
                            prepared_generation,
                            prepared_task,
                            prepared_error,
                            prepared_transaction,
                            prepared_launch,
                            _,
                        )| {
                            prepared_generation == generation
                                && *prepared_task == task
                                && prepared_error == error
                                && prepared_transaction == transaction
                                && *prepared_launch == failure_launch
                        },
                    )
                    .count()
                    == 1
                    && cleanup_transactions
                        .get(transaction)
                        .is_some_and(|cleanup| {
                            cleanup.cause_wait == Some(*consuming_wait)
                                && matches!(
                                    cleanup.kind,
                                    Some(PhysicalCleanupTransactionKind::StartupSetup {
                                        generation: cleanup_generation,
                                        task: cleanup_task,
                                        error: cleanup_error,
                                        launch: cleanup_launch,
                                    }) if cleanup_generation == *generation
                                        && cleanup_task == task
                                        && cleanup_error == *error
                                        && cleanup_launch == failure_launch
                                )
                        })
                    && wait_attempts.get(consuming_wait).is_some_and(|context| {
                        context.generation == Some(*generation)
                            && context.producer
                                == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                            && context.task == task
                    })
                    && matches!(
                        wait_outcomes.get(consuming_wait),
                        Some(PhysicalWaitOutcome::NoChild)
                    )
            }
            _ => false,
        };
        if !exact_owner {
            violations
                .push(PhysicalPartitionViolation::InvalidPreRegistrationSetupFailure(*generation));
        }
    }
    let mut startup_generations = BTreeSet::new();
    startup_generations.extend(
        wait_attempts
            .values()
            .filter(|context| context.producer == PhysicalWaitProducer::PreRegistrationBarrier)
            .filter_map(|context| context.generation),
    );
    startup_generations.extend(
        startup_barrier_setup_failures
            .iter()
            .map(|(generation, ..)| *generation),
    );
    for generation in startup_generations {
        if valid_startup_setup_failures.contains(&generation) {
            continue;
        }
        let owned_barriers = startup_barrier_owners
            .keys()
            .filter(|barrier| {
                wait_attempts
                    .get(barrier)
                    .is_some_and(|context| context.generation == Some(generation))
            })
            .count();
        let cleanup_owned = startup_barrier_owners.iter().any(|(barrier, consuming)| {
            wait_attempts
                .get(barrier)
                .is_some_and(|context| context.generation == Some(generation))
                && wait_attempts.get(consuming).is_some_and(|context| {
                    context.producer == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                })
        });
        let authority_shape = if cleanup_owned {
            !continued_authorities.contains_key(&generation)
                && !worker_start_sequences.contains_key(&generation)
        } else {
            continued_authorities.contains_key(&generation)
        };
        if owned_barriers != 1 || !authority_shape {
            violations.push(PhysicalPartitionViolation::InvalidContinuedAuthority(
                generation,
            ));
        }
    }

    let stop_resolution_close_sequence = |delivery: &PhysicalStatusId| {
        stop_resolution_closes.get(delivery).copied().or_else(|| {
            stop_resolution_causal_closes
                .iter()
                .filter(|(_, candidate, ..)| candidate == delivery)
                .map(|(_, _, _, _, _, _, sequence)| *sequence)
                .min()
        })
    };
    for (delivery, (generation, armed)) in &stop_resolution_arms {
        let valid = statuses.get(delivery).is_some_and(|track| {
            track.generation == Some(*generation)
                && track.producer == Some(PhysicalWaitProducer::AuthorizedRootNotifier)
                && track.raw_status.is_some_and(is_plain_sigstop)
                && track.publication_destination == Some(PhysicalStatusPublication::RegularFifo)
                && track
                    .publication_sequence
                    .is_some_and(|published| published < *armed)
        }) && continued_authorities
            .get(generation)
            .is_some_and(|authority| {
                authority.enabled_sequence < *armed
                    && authority
                        .revoked_sequence
                        .is_none_or(|revoked| *armed < revoked)
            })
            && stop_resolution_close_sequence(delivery).is_some_and(|closed| *armed < closed);
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                *delivery,
            ));
        }
    }
    for (stopped, (delivery, first_sequence)) in &stop_resolution_first {
        let valid = stop_resolution_arms
            .get(delivery)
            .is_some_and(|(generation, armed)| {
                statuses.get(stopped).is_some_and(|track| {
                    track.generation == Some(*generation)
                        && track
                            .raw_status
                            .is_some_and(|raw_status| libc::WIFSTOPPED(raw_status))
                        && track.created_sequence < *first_sequence
                        && track
                            .publication_sequence
                            .is_some_and(|published| *first_sequence < published)
                        && *armed < *first_sequence
                })
            });
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                *stopped,
            ));
        }
    }
    for (group_stop, (delivery, acknowledged)) in &stop_resolution_acks {
        let valid = stop_resolution_first
            .get(group_stop)
            .is_some_and(|(first_delivery, first)| {
                first_delivery == delivery
                    && *first < *acknowledged
                    && statuses.get(group_stop).is_some_and(|track| {
                        track.raw_status.is_some_and(is_plain_sigstop)
                            && track
                                .publication_sequence
                                .is_some_and(|published| published < *acknowledged)
                    })
            });
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                *group_stop,
            ));
        }
    }
    for (continued, (group_stop, claimed)) in &stop_resolution_claims {
        let valid = stop_resolution_acks
            .get(group_stop)
            .is_some_and(|(_, acknowledged)| {
                statuses.get(continued).is_some_and(|track| {
                    track
                        .raw_status
                        .is_some_and(|raw_status| libc::WIFCONTINUED(raw_status))
                        && track.publication_destination
                            == Some(PhysicalStatusPublication::ContinuedSideChannel {
                                route: PhysicalContinuedStatusRoute::AfterAcknowledgedGroupStop,
                            })
                        && *acknowledged < track.created_sequence
                        && track.publication_sequence.is_some_and(|published| {
                            track
                                .continued_side_channel_sequence
                                .is_some_and(|disposed| published < disposed && disposed < *claimed)
                        })
                })
            });
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                *continued,
            ));
        }
    }
    for (generation, delivery, group_stop, continued, attempt, successor, closed) in
        &stop_resolution_causal_closes
    {
        let valid = stop_resolution_arms
            .get(delivery)
            .is_some_and(|(arm_generation, armed)| {
                *arm_generation == *generation
                    && stop_resolution_acks
                        .get(group_stop)
                        .is_some_and(|(ack_delivery, acknowledged)| {
                            *ack_delivery == *delivery
                                && *armed < *acknowledged
                                && stop_resolution_claims
                                    .get(continued)
                                    .is_some_and(|(claim_group, claimed)| {
                                        *claim_group == *group_stop
                                            && *acknowledged < *claimed
                                            && resume_attempt_sequences
                                                .get(attempt)
                                                .is_some_and(|resume_started| {
                                                    *claimed < *resume_started
                                                        && statuses.get(successor).is_some_and(
                                                            |successor_track| {
                                                                successor_track.generation
                                                                    == Some(*generation)
                                                                    && successor_track
                                                                        .publication_destination
                                                                        == Some(
                                                                            PhysicalStatusPublication::RegularFifo,
                                                                        )
                                                                    && *resume_started
                                                                        < successor_track
                                                                            .created_sequence
                                                                    && successor_track
                                                                        .publication_sequence
                                                                        .is_some_and(|published| {
                                                                            published < *closed
                                                                        })
                                                            },
                                                        )
                                                })
                                    })
                        })
            })
            && resume_results.get(attempt).is_some_and(|outcome| {
                matches!(
                    outcome,
                    PhysicalResumeOutcome::Error(error)
                        if matches!(*error, libc::ESRCH | libc::EIO)
                )
            })
            && ambiguous_resume_resolutions.iter().any(
                |(source, resolution_attempt, proof, resolved)| {
                    *source == *group_stop
                        && *resolution_attempt == *attempt
                        && *proof == PhysicalAmbiguousResumeProof::LaterStatus(*successor)
                        && *resolved < *closed
                },
            )
            && !stop_resolution_closes.contains_key(delivery);
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                *successor,
            ));
        }
    }
    for (status, track) in &statuses {
        let Some(PhysicalStatusPublication::ContinuedSideChannel { route }) =
            track.publication_destination
        else {
            continue;
        };
        let valid_route = match route {
            PhysicalContinuedStatusRoute::AfterAcknowledgedGroupStop => {
                stop_resolution_claims.contains_key(status)
            }
            PhysicalContinuedStatusRoute::BeforeFirstStop => {
                stop_resolution_arms
                    .iter()
                    .any(|(delivery, (generation, armed))| {
                        track.generation == Some(*generation)
                            && *armed < track.created_sequence
                            && stop_resolution_first
                                .values()
                                .all(|(first_delivery, first)| {
                                    first_delivery != delivery || track.created_sequence < *first
                                })
                    })
            }
            PhysicalContinuedStatusRoute::AfterFirstStop => {
                stop_resolution_first
                    .iter()
                    .any(|(group, (delivery, first))| {
                        statuses.get(group).is_some_and(|group_track| {
                            group_track.generation == track.generation
                                && *first < track.created_sequence
                                && stop_resolution_acks
                                    .get(group)
                                    .is_none_or(|(_, ack)| track.created_sequence < *ack)
                                && stop_resolution_closes
                                    .get(delivery)
                                    .is_none_or(|closed| track.created_sequence < *closed)
                        })
                    })
            }
            PhysicalContinuedStatusRoute::UnwatchedRoot => {
                let generation = track.generation;
                !stop_resolution_arms
                    .iter()
                    .any(|(delivery, (arm_generation, armed))| {
                        generation == Some(*arm_generation)
                            && *armed < track.created_sequence
                            && stop_resolution_closes
                                .get(delivery)
                                .is_none_or(|closed| track.created_sequence < *closed)
                    })
            }
            PhysicalContinuedStatusRoute::PreStopDrain { .. } => true,
        };
        if !valid_route {
            violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                *status,
            ));
        }
    }

    let pre_stop_epoch_is_contiguous =
        |generation: PhysicalEventGenerationId,
         task: PhysicalTaskIdentity,
         stopped: PhysicalStatusId,
         stopped_created: u64,
         boundary: u64,
         final_attempt: PhysicalWaitAttemptId| {
            let source_attempt = siginfo_status_attempts.get(&stopped).copied();
            let no_straddling_wait = wait_attempts.iter().all(|(attempt, context)| {
                if Some(*attempt) == source_attempt
                    || context.generation != Some(generation)
                    || !context.task.authorizes(task, false)
                {
                    return true;
                }
                let Some(started) = wait_attempt_sequences.get(attempt).copied() else {
                    return false;
                };
                let result = wait_result_sequences.get(attempt).copied();
                let overlaps =
                    started < boundary && result.is_none_or(|result| stopped_created < result);
                !overlaps
                    || (context.producer == PhysicalWaitProducer::PreStopContinuedDrain
                        && stopped_created < started
                        && result.is_some_and(|result| result < boundary))
            });
            let mut epoch = wait_attempts
                .iter()
                .filter_map(|(attempt, context)| {
                    let started = wait_attempt_sequences.get(attempt).copied()?;
                    (context.generation == Some(generation)
                        && context.task.authorizes(task, false)
                        && stopped_created < started
                        && started < boundary)
                        .then_some((*attempt, *context, started))
                })
                .collect::<Vec<_>>();
            epoch.sort_unstable_by_key(|(_, _, started)| *started);
            no_straddling_wait
                && source_attempt.is_some()
                && !epoch.is_empty()
                && epoch
                    .last()
                    .is_some_and(|(attempt, _, _)| *attempt == final_attempt)
                && epoch.iter().all(|(_, context, _)| {
                    context.producer == PhysicalWaitProducer::PreStopContinuedDrain
                })
                && epoch
                    .iter()
                    .enumerate()
                    .all(|(index, (attempt, _, started))| {
                        wait_result_sequences.get(attempt).is_some_and(|result| {
                            *started < *result
                                && *result < boundary
                                && epoch
                                    .get(index + 1)
                                    .is_none_or(|(_, _, next_started)| *result < *next_started)
                        })
                    })
        };
    let pre_stop_epoch_start = |generation: PhysicalEventGenerationId,
                                task: PhysicalTaskIdentity,
                                stopped_created: u64,
                                boundary: u64| {
        wait_attempts
            .iter()
            .filter(|(_, context)| {
                context.generation == Some(generation)
                    && context.producer == PhysicalWaitProducer::PreStopContinuedDrain
                    && context.task.authorizes(task, false)
            })
            .filter_map(|(attempt, _)| wait_attempt_sequences.get(attempt).copied())
            .filter(|started| stopped_created < *started && *started < boundary)
            .min()
    };
    let mut pre_stop_drains = BTreeMap::<PhysicalStatusId, PreStopDrainTrack>::new();
    let mut pre_stop_barriers = BTreeMap::<PhysicalWaitAttemptId, PhysicalStatusId>::new();
    for record in &snapshot.records {
        let PhysicalEventRecordKind::PreStopContinuedDrainCompleted {
            generation,
            stopped,
            final_no_status_attempt,
        } = record.kind
        else {
            continue;
        };
        let canonical = canonical_generation(generation, &adoptions, &invalid_adoptions);
        let stopped_track = statuses.get(&stopped);
        let barrier_context = wait_attempts.get(&final_no_status_attempt);
        let barrier_attempt_sequence = wait_attempt_sequences.get(&final_no_status_attempt);
        let barrier_result_sequence = wait_result_sequences.get(&final_no_status_attempt);
        let authority = continued_authorities.get(&generation);
        let valid = canonical == Some(generation)
            && authority.is_some_and(|authority| {
                authority.enabled_sequence
                    < stopped_track.map_or(u64::MAX, |track| track.created_sequence)
                    && authority
                        .revoked_sequence
                        .is_none_or(|revoked| record.sequence < revoked)
            })
            && stopped_track.is_some_and(|track| {
                track.generation == Some(generation)
                    && track.producer == Some(PhysicalWaitProducer::AuthorizedRootNotifier)
                    && track.raw_status.is_some_and(is_plain_sigstop)
                    && track.publication_destination == Some(PhysicalStatusPublication::RegularFifo)
                    && track
                        .publication_sequence
                        .is_some_and(|published| record.sequence < published)
            })
            && barrier_context.is_some_and(|context| {
                context.generation == Some(generation)
                    && context.producer == PhysicalWaitProducer::PreStopContinuedDrain
                    && stopped_track.is_some_and(|track| {
                        track
                            .task
                            .is_some_and(|task| task.authorizes(context.task, false))
                    })
            })
            && matches!(
                wait_outcomes.get(&final_no_status_attempt),
                Some(PhysicalWaitOutcome::NoStatus { siginfo: Some(siginfo) })
                    if siginfo_is_exact_no_status(*siginfo)
            )
            && barrier_attempt_sequence.is_some_and(|attempt| {
                stopped_track.is_some_and(|track| track.created_sequence < *attempt)
            })
            && barrier_result_sequence.is_some_and(|result| {
                barrier_attempt_sequence
                    .is_some_and(|attempt| attempt < result && *result < record.sequence)
            })
            && stopped_track.is_some_and(|track| {
                barrier_context.is_some_and(|context| {
                    pre_stop_epoch_is_contiguous(
                        generation,
                        context.task,
                        stopped,
                        track.created_sequence,
                        record.sequence,
                        final_no_status_attempt,
                    )
                })
            })
            && !stop_resolution_arms
                .iter()
                .any(|(delivery, (armed_generation, armed))| {
                    *armed_generation == generation
                        && stopped_track.is_some_and(|track| {
                            track.publication_sequence.is_some_and(|published| {
                                *armed < published
                                    && track.task.is_some_and(|task| {
                                        pre_stop_epoch_start(
                                            generation,
                                            task,
                                            track.created_sequence,
                                            record.sequence,
                                        )
                                        .is_some_and(
                                            |epoch_start| {
                                                stop_resolution_close_sequence(delivery)
                                                    .is_none_or(|closed| epoch_start < closed)
                                            },
                                        )
                                    })
                            })
                        })
                });
        let duplicate_barrier = pre_stop_barriers
            .insert(final_no_status_attempt, stopped)
            .filter(|existing| *existing != stopped);
        if let Some(existing) = duplicate_barrier {
            violations.push(PhysicalPartitionViolation::InvalidPreStopContinuedDrain(
                existing,
            ));
        }
        if !valid || duplicate_barrier.is_some() || pre_stop_drains.contains_key(&stopped) {
            violations.push(PhysicalPartitionViolation::InvalidPreStopContinuedDrain(
                stopped,
            ));
        } else {
            pre_stop_drains.insert(
                stopped,
                PreStopDrainTrack {
                    generation,
                    final_no_status_attempt,
                    completed_sequence: record.sequence,
                },
            );
        }
    }
    // Runtime consumes exactly one completed stale-C fence when it arms a
    // delivery-stop watcher.  The plain-SIGSTOP shape and FIFO publication do
    // not by themselves prove that the WCONTINUED zero barrier happened.
    for (delivery, (generation, armed)) in &stop_resolution_arms {
        let valid = pre_stop_drains.get(delivery).is_some_and(|drain| {
            drain.generation == *generation
                && statuses.get(delivery).is_some_and(|track| {
                    track.publication_sequence.is_some_and(|published| {
                        drain.completed_sequence < published && published < *armed
                    })
                })
        });
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidStopResolutionEvidence(
                *delivery,
            ));
        }
    }
    let mut pre_stop_drain_failures = BTreeMap::<
        PhysicalCleanupTransactionId,
        (
            PhysicalEventGenerationId,
            PhysicalStatusId,
            PhysicalWaitAttemptId,
            u64,
        ),
    >::new();
    let mut failed_pre_stop_statuses =
        BTreeMap::<PhysicalStatusId, PhysicalCleanupTransactionId>::new();
    for (generation, task, stopped, cause_wait, transaction, failure_sequence) in
        &pre_stop_drain_failure_records
    {
        let stopped_track = statuses.get(stopped);
        let cause_context = wait_attempts.get(cause_wait);
        let cause_attempt_sequence = wait_attempt_sequences.get(cause_wait).copied();
        let cause_result_sequence = wait_result_sequences.get(cause_wait).copied();
        let cleanup = cleanup_transactions.get(transaction);
        let fatal_drain_outcome = match wait_outcomes.get(cause_wait) {
            Some(PhysicalWaitOutcome::Error(error)) => *error != libc::EINTR,
            Some(PhysicalWaitOutcome::NoChild | PhysicalWaitOutcome::UndecodableStatus { .. }) => {
                true
            }
            Some(PhysicalWaitOutcome::Status { raw_status, .. }) => {
                !libc::WIFCONTINUED(*raw_status)
            }
            _ => false,
        };
        let valid = canonical_generation(*generation, &adoptions, &invalid_adoptions)
            == Some(*generation)
            && continued_authorities
                .get(generation)
                .is_some_and(|authority| {
                    authority.enabled_sequence
                        < stopped_track.map_or(u64::MAX, |track| track.created_sequence)
                        && authority
                            .revoked_sequence
                            .is_none_or(|revoked| *failure_sequence < revoked)
                })
            && stopped_track.is_some_and(|track| {
                track.generation == Some(*generation)
                    && track
                        .task
                        .is_some_and(|owner| owner.authorizes(*task, false))
                    && track.producer == Some(PhysicalWaitProducer::AuthorizedRootNotifier)
                    && track.raw_status.is_some_and(is_plain_sigstop)
                    && track.publication_destination
                        == Some(PhysicalStatusPublication::PreStopDrainFailureCleanup)
                    && track
                        .publication_sequence
                        .is_some_and(|published| published < *failure_sequence)
            })
            && cause_context.is_some_and(|context| {
                context.generation == Some(*generation)
                    && context.producer == PhysicalWaitProducer::PreStopContinuedDrain
                    && context.task == *task
            })
            && fatal_drain_outcome
            && stopped_track.is_some_and(|track| {
                pre_stop_epoch_is_contiguous(
                    *generation,
                    *task,
                    *stopped,
                    track.created_sequence,
                    *failure_sequence,
                    *cause_wait,
                )
            })
            && !stop_resolution_arms
                .iter()
                .any(|(delivery, (armed_generation, armed))| {
                    *armed_generation == *generation
                        && *armed < *failure_sequence
                        && stopped_track.is_some_and(|track| {
                            pre_stop_epoch_start(
                                *generation,
                                *task,
                                track.created_sequence,
                                *failure_sequence,
                            )
                            .is_some_and(|epoch_start| {
                                stop_resolution_close_sequence(delivery)
                                    .is_none_or(|closed| epoch_start < closed)
                            })
                        })
                })
            && cleanup.is_some_and(|cleanup| {
                cleanup.starts == 1
                    && cleanup.cause_wait == Some(*cause_wait)
                    && cleanup.statuses.get(stopped).is_some_and(|linked| {
                        cleanup.start_sequence.is_some_and(|started| {
                            cause_result_sequence.is_some_and(|cause_result| {
                                cause_result < started
                                    && stopped_track.is_some_and(|track| {
                                        track.created_sequence
                                            < cause_attempt_sequence.unwrap_or_default()
                                            && started < *linked
                                            && track.publication_sequence.is_some_and(|published| {
                                                *linked < published && published < *failure_sequence
                                            })
                                    })
                            })
                        })
                    })
            });
        let unique = !pre_stop_drain_failures.contains_key(transaction)
            && !failed_pre_stop_statuses.contains_key(stopped);
        if !valid || !unique || pre_stop_drains.contains_key(stopped) {
            violations.push(PhysicalPartitionViolation::InvalidPreStopContinuedDrain(
                *stopped,
            ));
        } else {
            pre_stop_drain_failures.insert(
                *transaction,
                (*generation, *stopped, *cause_wait, *failure_sequence),
            );
            failed_pre_stop_statuses.insert(*stopped, *transaction);
        }
    }
    for (status, track) in &statuses {
        match track.publication_destination {
            Some(PhysicalStatusPublication::PreStopDrainFailureCleanup)
                if !failed_pre_stop_statuses.contains_key(status) =>
            {
                violations.push(PhysicalPartitionViolation::InvalidPreStopContinuedDrain(
                    *status,
                ));
            }
            Some(PhysicalStatusPublication::RegularFifo)
                if pre_stop_drains.contains_key(status)
                    && failed_pre_stop_statuses.contains_key(status) =>
            {
                violations.push(PhysicalPartitionViolation::InvalidPreStopContinuedDrain(
                    *status,
                ));
            }
            _ => {}
        }
    }
    for (continued, track) in &statuses {
        let Some(PhysicalStatusPublication::ContinuedSideChannel {
            route: PhysicalContinuedStatusRoute::PreStopDrain { before },
        }) = track.publication_destination
        else {
            continue;
        };
        let exact_route_prefix =
            |generation: PhysicalEventGenerationId,
             boundary: u64,
             final_attempt: PhysicalWaitAttemptId| {
                let before_track = statuses.get(&before);
                before_track.is_some_and(|before_track| {
                    track.generation == Some(generation)
                        && before_track.generation == track.generation
                        && before_track.task.is_some_and(|before_task| {
                            track.task.is_some_and(|continued_task| {
                                before_task.authorizes(continued_task, false)
                            })
                        })
                        && before_track.created_sequence < track.created_sequence
                        && siginfo_status_attempts
                            .get(continued)
                            .is_some_and(|attempt| {
                                wait_result_sequences.get(attempt).is_some_and(|result| {
                                    let next_attempt = wait_attempts
                                        .iter()
                                        .filter(|(_, context)| {
                                            context.generation == Some(generation)
                                                && context.producer
                                                    == PhysicalWaitProducer::PreStopContinuedDrain
                                                && before_track.task.is_some_and(|task| {
                                                    task.authorizes(context.task, false)
                                                })
                                        })
                                        .filter_map(|(candidate, _)| {
                                            wait_attempt_sequences
                                                .get(candidate)
                                                .copied()
                                                .filter(|started| {
                                                    *result < *started && *started < boundary
                                                })
                                                .map(|started| (*candidate, started))
                                        })
                                        .min_by_key(|(_, started)| *started);
                                    next_attempt.is_some_and(|(next, next_started)| {
                                        (next == final_attempt
                                            || wait_attempt_sequences
                                                .get(&final_attempt)
                                                .is_some_and(|final_started| {
                                                    next_started < *final_started
                                                }))
                                            && track.publication_sequence.is_some_and(|published| {
                                                track.created_sequence < published
                                                    && track
                                                        .continued_side_channel_sequence
                                                        .is_some_and(|disposed| {
                                                            published < disposed
                                                                && disposed < next_started
                                                        })
                                            })
                                    })
                                })
                            })
                })
            };
        let valid_success = pre_stop_drains.get(&before).is_some_and(|drain| {
            exact_route_prefix(
                drain.generation,
                drain.completed_sequence,
                drain.final_no_status_attempt,
            ) && statuses.get(&before).is_some_and(|before_track| {
                before_track
                    .publication_sequence
                    .is_some_and(|published| drain.completed_sequence < published)
            })
        });
        let valid_failure = failed_pre_stop_statuses
            .get(&before)
            .and_then(|transaction| pre_stop_drain_failures.get(transaction))
            .is_some_and(|(generation, _, cause, failed)| {
                exact_route_prefix(*generation, *failed, *cause)
                    && statuses.get(&before).is_some_and(|before_track| {
                        before_track
                            .publication_sequence
                            .is_some_and(|published| published < *failed)
                    })
            });
        let valid = valid_success ^ valid_failure;
        if !valid {
            violations.push(PhysicalPartitionViolation::InvalidPreStopContinuedDrain(
                *continued,
            ));
        }
    }
    // Every root-worker plain SIGSTOP is covered by exactly one branch of the
    // runtime channel decision.  With no active watcher it must own a complete
    // stale-C drain (success or explicit cleanup failure); with an older
    // watcher active it is the G/next-stop candidate and must not start a
    // second drain.
    for (stopped, track) in &statuses {
        let Some(generation) = track.generation else {
            continue;
        };
        if track.producer != Some(PhysicalWaitProducer::AuthorizedRootNotifier)
            || !track.raw_status.is_some_and(is_plain_sigstop)
            || !matches!(
                track.publication_destination,
                Some(
                    PhysicalStatusPublication::RegularFifo
                        | PhysicalStatusPublication::PreStopDrainFailureCleanup
                )
            )
            || !continued_authorities
                .get(&generation)
                .is_some_and(|authority| {
                    authority.enabled_sequence < track.created_sequence
                        && authority.revoked_sequence.is_none_or(|revoked| {
                            track
                                .publication_sequence
                                .is_some_and(|published| published < revoked)
                        })
                })
        {
            continue;
        }
        let success = pre_stop_drains.contains_key(stopped);
        let failure = failed_pre_stop_statuses.contains_key(stopped);
        let watched = track.publication_sequence.is_some_and(|published| {
            stop_resolution_arms
                .iter()
                .any(|(delivery, (arm_generation, armed))| {
                    delivery != stopped
                        && *arm_generation == generation
                        && *armed < published
                        && stop_resolution_close_sequence(delivery)
                            .is_some_and(|closed| published < closed)
                })
        });
        let covered = match track.publication_destination {
            Some(PhysicalStatusPublication::RegularFifo) => success ^ watched,
            Some(PhysicalStatusPublication::PreStopDrainFailureCleanup) => {
                failure && !success && !watched
            }
            _ => false,
        };
        if !covered {
            violations.push(PhysicalPartitionViolation::InvalidPreStopContinuedDrain(
                *stopped,
            ));
        }
    }
    for (generation, authority) in &continued_authorities {
        let terminal_boundary = wait_outcomes
            .iter()
            .filter_map(|(attempt, outcome)| {
                if wait_generations.get(attempt) != Some(generation)
                    || !wait_attempts.get(attempt).is_some_and(|context| {
                        context.producer == PhysicalWaitProducer::AuthorizedRootNotifier
                    })
                {
                    return None;
                }
                let result = wait_result_sequences.get(attempt).copied()?;
                match outcome {
                    PhysicalWaitOutcome::Status { id, raw_status, .. }
                        if is_terminal_raw_status(*raw_status) =>
                    {
                        statuses.get(id).and_then(|track| {
                            (track.publication_destination
                                == Some(PhysicalStatusPublication::RetainedTerminal))
                            .then_some(track.publication_sequence)
                            .flatten()
                            .filter(|published| result < *published)
                        })
                    }
                    PhysicalWaitOutcome::NoChild => synthetic_echild_evidence
                        .get(attempt)
                        .and_then(|(evidence_generation, sequence)| {
                            (*evidence_generation == *generation && result < *sequence)
                                .then_some(*sequence)
                        }),
                    _ => None,
                }
            })
            .min();
        let statusless_startup_cleanup_boundary = startup_barrier_statusless_resolutions
            .iter()
            .filter_map(
                |(resolved_generation, _, _, _, transaction, resolution_sequence)| {
                    (*resolved_generation == *generation
                        && valid_startup_barrier_statusless_transactions.contains(transaction))
                    .then_some(*resolution_sequence)
                },
            )
            .min();
        let terminal_boundary = [terminal_boundary, statusless_startup_cleanup_boundary]
            .into_iter()
            .flatten()
            .min();
        let first_new_child = statuses
            .values()
            .filter(|track| {
                track.generation == Some(*generation)
                    && track.producer == Some(PhysicalWaitProducer::AuthorizedRootNotifier)
                    && track.raw_status.is_some_and(is_new_child_stop)
            })
            .min_by_key(|track| track.created_sequence);
        let exact_revoke = match (authority.revoked_sequence, first_new_child) {
            (None, None) => true,
            (Some(revoked), Some(first)) => first
                .publication_sequence
                .is_some_and(|published| first.created_sequence < revoked && revoked < published),
            (Some(revoked), None) => terminal_boundary.is_some_and(|terminal| {
                authority.enabled_sequence < terminal && terminal < revoked
            }),
            (None, Some(_)) => false,
        };
        if !exact_revoke {
            violations.push(PhysicalPartitionViolation::InvalidContinuedAuthority(
                *generation,
            ));
        }
    }

    let mut successful_resumes = 0;
    for (attempt, outcome) in &wait_outcomes {
        let requires_siginfo = wait_attempts.get(attempt).is_some_and(|context| {
            context.producer != PhysicalWaitProducer::PreRegistrationCleanup
        });
        let recorded = wait_siginfos.get(attempt).copied();
        let shape_valid = match outcome {
            PhysicalWaitOutcome::RetainedStatus {
                raw_status,
                siginfo,
            } => {
                recorded.is_none()
                    && wait_attempts.get(attempt).is_some_and(|context| {
                        context.producer == PhysicalWaitProducer::PreRegistrationBarrier
                    })
                    && siginfo_matches_wait_status(*siginfo, *raw_status)
            }
            PhysicalWaitOutcome::RetainedUndecodableStatus { siginfo, error } => {
                recorded.is_none()
                    && *error == libc::EPROTO
                    && wait_attempts.get(attempt).is_some_and(|context| {
                        context.producer == PhysicalWaitProducer::PreRegistrationBarrier
                            && siginfo.pid == context.task.tid
                    })
                    && siginfo_is_rejected_by_waitid(*siginfo)
            }
            PhysicalWaitOutcome::Status {
                id,
                raw_status,
                siginfo,
            } if requires_siginfo => recorded.is_some_and(|(recorded_siginfo, recorded_status)| {
                recorded_status == Some(*id)
                    && *siginfo == Some(recorded_siginfo)
                    && siginfo_matches_wait_status(recorded_siginfo, *raw_status)
            }),
            PhysicalWaitOutcome::Status { siginfo, .. } => recorded.is_none() && siginfo.is_none(),
            PhysicalWaitOutcome::NoStatus { siginfo } if requires_siginfo => {
                recorded.is_some_and(|(recorded_siginfo, recorded_status)| {
                    recorded_status.is_none()
                        && *siginfo == Some(recorded_siginfo)
                        && siginfo_is_exact_no_status(recorded_siginfo)
                })
            }
            PhysicalWaitOutcome::NoStatus { siginfo } => recorded.is_none() && siginfo.is_none(),
            PhysicalWaitOutcome::UndecodableStatus { id, siginfo, error } => {
                requires_siginfo
                    && *error == libc::EPROTO
                    && siginfo_is_rejected_by_waitid(*siginfo)
                    && recorded.is_some_and(|(recorded_siginfo, recorded_status)| {
                        recorded_status == Some(*id) && *siginfo == recorded_siginfo
                    })
            }
            PhysicalWaitOutcome::Interrupted
            | PhysicalWaitOutcome::NoChild
            | PhysicalWaitOutcome::Error(_) => recorded.is_none(),
        };
        let ordered = wait_attempt_sequences
            .get(attempt)
            .is_some_and(|attempt_sequence| {
                wait_result_sequences
                    .get(attempt)
                    .is_some_and(|result_sequence| {
                        attempt_sequence < result_sequence
                            && wait_attempts
                                .get(attempt)
                                .and_then(|context| context.generation)
                                .and_then(|generation| generation_closed_sequences.get(&generation))
                                .is_none_or(|closed| result_sequence < closed)
                            && wait_siginfo_sequences
                                .get(attempt)
                                .is_none_or(|siginfo_sequence| {
                                    attempt_sequence < siginfo_sequence
                                        && siginfo_sequence < result_sequence
                                })
                    })
            });
        if !shape_valid || !ordered {
            violations.push(PhysicalPartitionViolation::WaitSiginfoStatusMismatch(
                *attempt,
            ));
        }
        if !wait_attempts
            .get(attempt)
            .is_some_and(|context| wait_outcome_matches_flags(*context, *outcome))
        {
            violations.push(PhysicalPartitionViolation::InvalidWaitOutcomeForFlags(
                *attempt,
            ));
        }
    }
    for attempt in wait_attempts.keys() {
        if !wait_results.contains(attempt) {
            violations.push(PhysicalPartitionViolation::WaitAttemptWithoutResult(
                *attempt,
            ));
        }
    }
    for attempt in resume_attempts.keys() {
        if !resume_results.contains_key(attempt) {
            violations.push(PhysicalPartitionViolation::ResumeAttemptWithoutResult(
                *attempt,
            ));
        }
    }
    for attempt in pidfd_signal_attempts.keys() {
        if !pidfd_signal_results.contains_key(attempt) {
            violations.push(PhysicalPartitionViolation::PidfdSignalAttemptWithoutResult(
                *attempt,
            ));
        }
    }
    for (attempt, outcome) in &resume_results {
        let context = resume_attempts.get(attempt);
        if let Some(generation) = context.and_then(|context| context.generation)
            && generation_closed_sequences
                .get(&generation)
                .is_some_and(|closed| {
                    resume_result_sequences
                        .get(attempt)
                        .is_none_or(|result| result >= closed)
                })
        {
            invalid_generation_lifecycles.insert(generation);
        }
        if *outcome != PhysicalResumeOutcome::Success {
            continue;
        }
        successful_resumes += 1;
        let Some(context) = context else {
            continue;
        };
        let Some(status) = context.source_status else {
            violations.push(PhysicalPartitionViolation::SuccessfulResumeWithoutStatus(
                *attempt,
            ));
            continue;
        };
        if let Some(track) = statuses.get_mut(&status) {
            if track.generation != resume_generations.get(attempt).copied() {
                violations.push(PhysicalPartitionViolation::WrongGeneration(status));
            }
            if !track
                .task
                .is_some_and(|task| task.authorizes(context.task, false))
            {
                violations.push(PhysicalPartitionViolation::WrongResumeTask(*attempt));
            }
            track.successful_resumes += 1;
            track.successful_resume_sequence = resume_result_sequences.get(attempt).copied();
            track.successful_resume_owner = Some(context.owner);
            if context.owner.is_registered_controller_cleanup() {
                track.registered_controller_cleanup_resumes += 1;
            }
        } else {
            violations.push(PhysicalPartitionViolation::UnknownPhysicalStatus(status));
        }
    }
    for (attempt, errno) in &tolerated {
        if resume_results.get(attempt) != Some(&PhysicalResumeOutcome::Error(*errno)) {
            violations.push(PhysicalPartitionViolation::InvalidToleratedResumeError(
                *attempt,
            ));
        }
    }
    let mut valid_ambiguous_resolution_attempts = BTreeSet::new();
    let mut valid_ambiguous_resolution_sources = BTreeSet::new();
    let mut seen_ambiguous_resolution_attempts = BTreeSet::new();
    let mut seen_ambiguous_resolution_sources = BTreeSet::new();
    let mut seen_ambiguous_resolution_proofs = BTreeSet::new();
    for (source, attempt, proof, resolution_sequence) in &ambiguous_resume_resolutions {
        let unique_attempt = seen_ambiguous_resolution_attempts.insert(*attempt);
        let unique_source = seen_ambiguous_resolution_sources.insert(*source);
        let unique_proof = seen_ambiguous_resolution_proofs.insert(*proof);
        if !unique_attempt || !unique_source || !unique_proof {
            violations
                .push(PhysicalPartitionViolation::DuplicateAmbiguousResumeResolution(*attempt));
        }

        let Some(source_track) = statuses.get(source) else {
            violations.push(PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(*attempt));
            continue;
        };
        let Some(context) = resume_attempts.get(attempt) else {
            violations.push(PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(*attempt));
            continue;
        };
        let Some(generation) = resume_generations.get(attempt).copied() else {
            violations.push(PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(*attempt));
            continue;
        };
        let Some(result_sequence) = resume_result_sequences.get(attempt).copied() else {
            violations.push(PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(*attempt));
            continue;
        };
        let resume_started = resume_attempt_sequences.get(attempt).copied();
        let exit_source = source_track.publication_destination
            == Some(PhysicalStatusPublication::ExitCapability)
            && source_track.raw_status.is_some_and(is_ptrace_exit_stop);
        let group_source = source_track.publication_destination
            == Some(PhysicalStatusPublication::RegularFifo)
            && source_track.raw_status.is_some_and(is_plain_sigstop)
            && stop_resolution_acks.contains_key(source)
            && stop_resolution_claims
                .values()
                .any(|(group_stop, claimed)| {
                    *group_stop == *source
                        && resume_started.is_some_and(|resume_started| *claimed < resume_started)
                })
            && context.owner == PhysicalResumeOwner::TypedStopped
            && context.signal.is_none();
        let source_valid = source_track.generation == Some(generation)
            && (exit_source || group_source)
            && source_track
                .publication_sequence
                .is_some_and(|published| published < result_sequence)
            && context.source_status == Some(*source)
            && context.operation == PhysicalResumeOperation::Continue
            && source_track
                .task
                .is_some_and(|task| task.authorizes(context.task, false))
            && matches!(
                resume_results.get(attempt),
                Some(&PhysicalResumeOutcome::Error(error))
                    if matches!(error, libc::ESRCH | libc::EIO)
            )
            && result_sequence < *resolution_sequence
            && source_track.ambiguous_resume_resolved_dispositions == 1
            && source_track.ambiguous_resume_resolved_sequence == Some(*resolution_sequence)
            && !tolerated.contains_key(attempt)
            && (!group_source || matches!(proof, PhysicalAmbiguousResumeProof::LaterStatus(_)));

        let proof_valid = match proof {
            PhysicalAmbiguousResumeProof::LaterStatus(proof_status)
            | PhysicalAmbiguousResumeProof::FinalStatus(proof_status) => {
                statuses.get(proof_status).is_some_and(|proof_track| {
                    let expected_shape = match proof {
                        PhysicalAmbiguousResumeProof::LaterStatus(_) => {
                            proof_track
                                .raw_status
                                .is_some_and(|status| libc::WIFSTOPPED(status))
                                && matches!(
                                    proof_track.publication_destination,
                                    Some(
                                        PhysicalStatusPublication::RegularFifo
                                            | PhysicalStatusPublication::SynchronousFifo
                                    )
                                )
                        }
                        PhysicalAmbiguousResumeProof::FinalStatus(_) => {
                            proof_track.raw_status.is_some_and(is_terminal_raw_status)
                                && proof_track.publication_destination
                                    == Some(PhysicalStatusPublication::RetainedTerminal)
                        }
                        PhysicalAmbiguousResumeProof::ProvenEchild(_) => unreachable!(),
                    };
                    *proof_status != *source
                        && proof_track.generation == Some(generation)
                        && source_track.task.is_some_and(|source_task| {
                            proof_track
                                .task
                                .is_some_and(|proof_task| source_task.authorizes(proof_task, false))
                        })
                        && resume_attempt_sequences
                            .get(attempt)
                            .is_some_and(|resume_started| {
                                *resume_started < proof_track.created_sequence
                            })
                        && source_track.created_sequence < proof_track.created_sequence
                        && source_track
                            .publication_sequence
                            .is_some_and(|source_published| {
                                proof_track
                                    .publication_sequence
                                    .is_some_and(|proof_published| {
                                        source_published < proof_track.created_sequence
                                            && proof_track.created_sequence < proof_published
                                            && proof_published < *resolution_sequence
                                    })
                            })
                        && expected_shape
                })
            }
            PhysicalAmbiguousResumeProof::ProvenEchild(wait) => {
                wait_generations.get(wait) == Some(&generation)
                    && wait_outcomes.get(wait) == Some(&PhysicalWaitOutcome::NoChild)
                    && wait_attempt_sequences
                        .get(wait)
                        .is_some_and(|wait_started| {
                            source_track
                                .publication_sequence
                                .is_some_and(|source_published| source_published < *wait_started)
                        })
                    && wait_result_sequences.get(wait).is_some_and(|wait_result| {
                        resume_attempt_sequences
                            .get(attempt)
                            .is_some_and(|resume_started| *resume_started < *wait_result)
                            && echild_terminal_proofs.get(wait).is_some_and(
                                |(proof_generation, proof_task, proof_sequence)| {
                                    *proof_generation == generation
                                        && source_track.task.is_some_and(|source_task| {
                                            source_task.authorizes(*proof_task, false)
                                        })
                                        && *wait_result < *proof_sequence
                                        && synthetic_echild_evidence.get(wait).is_some_and(
                                            |(synthetic_generation, synthetic_sequence)| {
                                                *synthetic_generation == generation
                                                    && *proof_sequence < *synthetic_sequence
                                                    && *synthetic_sequence < *resolution_sequence
                                            },
                                        )
                                },
                            )
                    })
            }
        };
        if !source_valid || !proof_valid {
            violations.push(PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(*attempt));
        } else if unique_attempt && unique_source && unique_proof {
            valid_ambiguous_resolution_attempts.insert(*attempt);
            valid_ambiguous_resolution_sources.insert(*source);
        }
    }

    // Once ESRCH/EIO makes an exit-source resume ambiguous, no identity or
    // liveness check can prove that another ptrace call would still target the
    // same kernel stop. Reject every later attempt naming that physical source.
    for (attempt, outcome) in &resume_results {
        if !matches!(
            *outcome,
            PhysicalResumeOutcome::Error(libc::ESRCH | libc::EIO)
        ) {
            continue;
        }
        let Some(context) = resume_attempts.get(attempt) else {
            continue;
        };
        let Some(source) = context.source_status else {
            continue;
        };
        if statuses.get(&source).is_none_or(|track| {
            track.publication_destination != Some(PhysicalStatusPublication::ExitCapability)
        }) {
            continue;
        }
        let Some(_result_sequence) = resume_result_sequences.get(attempt) else {
            continue;
        };
        let source_attempts = resume_attempts
            .values()
            .filter(|candidate| candidate.source_status == Some(source))
            .count();
        let resolved = valid_ambiguous_resolution_attempts.contains(attempt)
            && valid_ambiguous_resolution_sources.contains(&source);
        if source_attempts != 1 || tolerated.contains_key(attempt) || !resolved {
            violations.push(PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(*attempt));
        }
    }
    let mut status_reservations =
        BTreeMap::<PhysicalStatusId, Vec<StatusReservationInterval>>::new();
    for track in reservations.values() {
        let (Some(status), Some(reserved)) = (track.status, track.reserved_sequence) else {
            continue;
        };
        let consuming = matches!(
            track.completion,
            Some((ReservationCompletion::Committed, _))
        ) || matches!(
            track.decode_finished,
            Some((_, PhysicalDecodeOutcome::DiedConsumed, _))
        );
        status_reservations
            .entry(status)
            .or_default()
            .push(StatusReservationInterval {
                reserved,
                completion: track.completion,
                consuming,
            });
    }
    for (status, intervals) in &mut status_reservations {
        intervals.sort_unstable_by_key(|interval| interval.reserved);
        let retained_terminal = statuses.get(status).is_some_and(|track| {
            track.publication_destination == Some(PhysicalStatusPublication::RetainedTerminal)
        });
        if !retained_terminal {
            let overlapping = intervals.windows(2).any(|pair| {
                pair[0]
                    .completion
                    .is_none_or(|(_, completed)| completed >= pair[1].reserved)
            });
            if overlapping {
                violations.push(PhysicalPartitionViolation::OverlappingStatusReservations(
                    *status,
                ));
            }
            let mut consumed_at = None;
            let mut reservation_after_consumption = false;
            for interval in intervals.iter() {
                reservation_after_consumption |=
                    consumed_at.is_some_and(|completed| completed < interval.reserved);
                if interval.consuming {
                    consumed_at = interval.completion.map(|(_, completed)| completed);
                }
            }
            if reservation_after_consumption {
                violations
                    .push(PhysicalPartitionViolation::ReservationAfterStatusConsumption(*status));
            }
        }
        if intervals
            .iter()
            .filter(|interval| interval.consuming)
            .count()
            > 1
        {
            violations.push(PhysicalPartitionViolation::DuplicateStatusConsumption(
                *status,
            ));
        }
    }
    for (reservation, track) in &reservations {
        let Some(status) = track.status else {
            continue;
        };
        let Some((ReservationCompletion::Committed, committed)) = track.completion else {
            continue;
        };
        if !matches!(
            track.decode_finished,
            Some((_, PhysicalDecodeOutcome::Returned, _))
        ) {
            continue;
        }
        let typed_attempts = resume_attempts
            .iter()
            .filter(|(_, context)| {
                context.owner == PhysicalResumeOwner::TypedStopped
                    && context.source_status == Some(status)
            })
            .collect::<Vec<_>>();
        let superseded_after_delivery = typed_attempts.is_empty()
            && statuses.get(&status).is_some_and(|status_track| {
                status_track.kernel_superseded_dispositions == 1
                    && status_track
                        .kernel_superseded_sequence
                        .is_some_and(|disposed| committed < disposed)
            });
        if !superseded_after_delivery
            && (typed_attempts.len() != 1
                || !resume_attempt_sequences
                    .get(typed_attempts[0].0)
                    .is_some_and(|attempt| committed < *attempt))
        {
            violations.push(PhysicalPartitionViolation::InvalidTypedResumeCardinality(
                *reservation,
            ));
        }
    }
    let mut fifo_publications =
        BTreeMap::<PhysicalEventGenerationId, Vec<(PhysicalStatusId, u64)>>::new();
    for (status, track) in &statuses {
        if matches!(
            track.publication_destination,
            Some(
                PhysicalStatusPublication::RegularFifo | PhysicalStatusPublication::SynchronousFifo
            )
        ) && let (Some(generation), Some(published)) =
            (track.generation, track.publication_sequence)
        {
            fifo_publications
                .entry(generation)
                .or_default()
                .push((*status, published));
        }
    }
    for publications in fifo_publications.values_mut() {
        publications.sort_unstable_by_key(|(_, published)| *published);
        for (index, (status, _)) in publications.iter().enumerate() {
            let Some(first_reservation) = status_reservations
                .get(status)
                .and_then(|intervals| intervals.first())
                .map(|interval| interval.reserved)
            else {
                continue;
            };
            let prior_publications_consumed =
                publications[..index].iter().all(|(prior_status, _)| {
                    status_reservations
                        .get(prior_status)
                        .and_then(|intervals| {
                            intervals.iter().find_map(|interval| {
                                (interval.consuming)
                                    .then_some(interval.completion)
                                    .flatten()
                                    .map(|(_, completed)| completed)
                            })
                        })
                        .is_some_and(|completed| completed < first_reservation)
                });
            if !prior_publications_consumed {
                violations.push(PhysicalPartitionViolation::OutOfOrderStatusReservation(
                    *status,
                ));
            }
        }
    }
    let valid_synchronous_cancellation = |status: PhysicalStatusId,
                                          decode_finished: u64,
                                          completed: u64| {
        let Some(status_track) = statuses.get(&status) else {
            return false;
        };
        let worker_started_before_completion =
            status_track.generation.is_some_and(|status_generation| {
                worker_start_sequences.iter().any(|(generation, sequence)| {
                    canonical_generation(*generation, &adoptions, &invalid_adoptions)
                        == Some(status_generation)
                        && *sequence < completed
                })
            });
        if decode_finished >= completed
            || status_track.producer != Some(PhysicalWaitProducer::SynchronousWait)
            || worker_started_before_completion
        {
            return false;
        }
        let stopped = status_track
            .raw_status
            .is_some_and(|raw_status| libc::WIFSTOPPED(raw_status));
        let matching_attempts = resume_attempts
            .iter()
            .filter(|(attempt, context)| {
                context.owner == PhysicalResumeOwner::SynchronousCancellation
                    && context.source_status == Some(status)
                    && resume_attempt_sequences
                        .get(attempt)
                        .is_some_and(|sequence| {
                            decode_finished < *sequence && *sequence < completed
                        })
            })
            .collect::<Vec<_>>();

        if !stopped || matching_attempts.len() != 1 {
            false
        } else {
            let attempt = *matching_attempts[0].0;
            let result_is_ordered = resume_result_sequences
                .get(&attempt)
                .is_some_and(|sequence| {
                    resume_attempt_sequences
                        .get(&attempt)
                        .is_some_and(|started| started < sequence && *sequence < completed)
                });
            result_is_ordered
                && match resume_results.get(&attempt) {
                    Some(PhysicalResumeOutcome::Success) => {
                        !tolerated.contains_key(&attempt)
                            && status_track.cancellation_cleanup_dispositions == 0
                    }
                    Some(PhysicalResumeOutcome::Error(error))
                        if *error == libc::ESRCH
                            || (*error == libc::EIO
                                && !status_track.raw_status.is_some_and(is_ptrace_exit_stop)) =>
                    {
                        tolerated.get(&attempt) == Some(error)
                            && resume_result_sequences.get(&attempt).is_some_and(|result| {
                                tolerated_sequences.get(&attempt).is_some_and(|tolerated| {
                                    status_track.cancellation_cleanup_sequence.is_some_and(
                                        |disposition| {
                                            result < tolerated
                                                && tolerated < &disposition
                                                && disposition < completed
                                        },
                                    )
                                })
                            })
                    }
                    _ => false,
                }
        }
    };
    for (reservation, track) in &reservations {
        if track.completion.is_none() {
            violations.push(PhysicalPartitionViolation::LiveReservation(*reservation));
        }
        if track.decode_started.is_none() {
            if !matches!(
                track.completion,
                Some((ReservationCompletion::RolledBack, _))
            ) {
                violations.push(PhysicalPartitionViolation::ReservationWithoutDecode(
                    *reservation,
                ));
            }
        } else if track.decode_finished.is_none() {
            violations.push(PhysicalPartitionViolation::LiveDecode(*reservation));
        }
        let ordered = match (
            track.reserved_sequence,
            track.decode_started,
            track.decode_finished,
            track.completion,
        ) {
            (Some(reserved), Some((_, started)), Some((_, _, finished)), Some((_, completed))) => {
                reserved < started && started < finished && finished < completed
            }
            (Some(reserved), None, None, Some((ReservationCompletion::RolledBack, completed))) => {
                reserved < completed
            }
            _ => false,
        };
        let valid_cross_product = ordered
            && match (track.decode_finished, track.completion) {
                (Some((owner, PhysicalDecodeOutcome::Returned, _)), Some((completion, _))) => {
                    track.status.is_some_and(|status| {
                        let retained_terminal = statuses.get(&status).is_some_and(|status_track| {
                            status_track.publication_destination
                                == Some(PhysicalStatusPublication::RetainedTerminal)
                        });
                        if retained_terminal {
                            matches!(
                                owner,
                                PhysicalDecodeOwner::Notifier | PhysicalDecodeOwner::Synchronous
                            ) && completion == ReservationCompletion::TerminalReplayed
                        } else {
                            completion == ReservationCompletion::Committed
                        }
                    })
                }
                (
                    Some((owner, PhysicalDecodeOutcome::DiedConsumed, finished)),
                    Some((ReservationCompletion::Committed, completed)),
                ) => {
                    owner == PhysicalDecodeOwner::Notifier
                        && track.status.is_some_and(|status| {
                            statuses.get(&status).is_some_and(|status_track| {
                                status_track
                                    .raw_status
                                    .is_some_and(is_fallible_getevent_stop)
                                    && status_track.decode_died_dispositions == 1
                                    && status_track.decode_died_sequence.is_some_and(|sequence| {
                                        finished < sequence && sequence < completed
                                    })
                            })
                        })
                }
                (
                    Some((_, PhysicalDecodeOutcome::RetryRolledBack, _)),
                    Some((ReservationCompletion::RolledBack, _)),
                ) => track.status.is_some_and(|status| {
                    statuses.get(&status).is_some_and(|status_track| {
                        status_track
                            .raw_status
                            .is_some_and(is_fallible_getevent_stop)
                    })
                }),
                (
                    Some((owner, PhysicalDecodeOutcome::Cancelled, finished)),
                    Some((ReservationCompletion::RolledBack, rolled_back)),
                ) => {
                    matches!(
                        owner,
                        PhysicalDecodeOwner::Notifier
                            | PhysicalDecodeOwner::Synchronous
                            | PhysicalDecodeOwner::Cleanup
                    ) && track.status.is_some_and(|status| {
                        let synchronous_source_valid =
                            statuses.get(&status).is_some_and(|status_track| {
                                status_track.producer == Some(PhysicalWaitProducer::SynchronousWait)
                                    && status_track.generation.is_some_and(|status_generation| {
                                        worker_start_sequences.iter().all(
                                            |(generation, sequence)| {
                                                canonical_generation(
                                                    *generation,
                                                    &adoptions,
                                                    &invalid_adoptions,
                                                ) != Some(status_generation)
                                                    || *sequence >= rolled_back
                                            },
                                        )
                                    })
                            });
                        let prior_notifier_cancellation = reservations.values().any(|prior| {
                            prior.status == Some(status)
                                && matches!(
                                    prior.decode_finished,
                                    Some((
                                        PhysicalDecodeOwner::Notifier,
                                        PhysicalDecodeOutcome::Cancelled,
                                        _
                                    ))
                                )
                                && matches!(
                                    prior.completion,
                                    Some((ReservationCompletion::RolledBack, completed))
                                        if completed < finished
                                )
                        });
                        let matching_resumes = resume_attempts
                            .iter()
                            .filter(|(attempt, context)| {
                                context.owner == PhysicalResumeOwner::SynchronousCancellation
                                    && context.source_status == Some(status)
                                    && resume_attempt_sequences.get(attempt).is_some_and(
                                        |sequence| finished < *sequence && *sequence < rolled_back,
                                    )
                            })
                            .collect::<Vec<_>>();
                        if owner == PhysicalDecodeOwner::Notifier {
                            let notifier_cancellation_hands_off_only_to_cleanup =
                                reservations.iter().all(|(other_reservation, other)| {
                                    other_reservation == reservation
                                        || other.status != Some(status)
                                        || other
                                            .reserved_sequence
                                            .is_none_or(|reserved| reserved <= finished)
                                        || other.decode_finished.is_none()
                                        || matches!(
                                            other.decode_finished,
                                            Some((_, PhysicalDecodeOutcome::Cancelled, _))
                                                | Some((PhysicalDecodeOwner::Cleanup, _, _))
                                        )
                                });
                            return matching_resumes.is_empty()
                                && notifier_cancellation_hands_off_only_to_cleanup;
                        }
                        if owner == PhysicalDecodeOwner::Cleanup {
                            return matching_resumes.is_empty();
                        }
                        if !synchronous_source_valid && !prior_notifier_cancellation {
                            return false;
                        }
                        match matching_resumes.as_slice() {
                            [] => true,
                            [(attempt, _)] => {
                                let attempt = **attempt;
                                resume_attempt_sequences
                                    .get(&attempt)
                                    .is_some_and(|sequence| {
                                        finished < *sequence && *sequence < rolled_back
                                    })
                                    && resume_result_sequences.get(&attempt).is_some_and(
                                        |sequence| {
                                            resume_attempt_sequences.get(&attempt).is_some_and(
                                                |started| {
                                                    started < sequence && *sequence < rolled_back
                                                },
                                            )
                                        },
                                    )
                                    && matches!(
                                        resume_results.get(&attempt),
                                        Some(PhysicalResumeOutcome::Error(_))
                                    )
                                    && !tolerated.contains_key(&attempt)
                            }
                            _ => false,
                        }
                    })
                }
                (
                    Some((owner, PhysicalDecodeOutcome::Cancelled, finished)),
                    Some((ReservationCompletion::Committed, completed)),
                ) => {
                    owner == PhysicalDecodeOwner::Synchronous
                        && track.status.is_some_and(|status| {
                            valid_synchronous_cancellation(status, finished, completed)
                        })
                }
                (None, Some((ReservationCompletion::RolledBack, _))) => true,
                _ => false,
            };
        if !valid_cross_product {
            violations.push(PhysicalPartitionViolation::InvalidDecodeReservationOutcome(
                *reservation,
            ));
        }
    }

    let mut valid_cleanup_transactions = BTreeSet::new();
    for (transaction, track) in &cleanup_transactions {
        let mut valid = track.starts == 1 && track.kind.is_some();
        let (
            Some(start_sequence),
            Some(completion_sequence),
            Some(terminal_wait),
            Some(cause_wait),
        ) = (
            track.start_sequence,
            track.completion_sequence,
            track.terminal_wait,
            track.cause_wait,
        )
        else {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
            continue;
        };
        let terminal_context = wait_attempts.get(&terminal_wait);
        let terminal_generation = wait_generations.get(&terminal_wait).copied();
        let terminal_result_sequence = wait_result_sequences.get(&terminal_wait).copied();
        let terminal_outcome = wait_outcomes.get(&terminal_wait);
        let typed_unsupported_terminal = matches!(
            terminal_outcome,
            Some(PhysicalWaitOutcome::UndecodableStatus { siginfo, .. })
                if siginfo_is_startup_typed_unsupported_terminal(*siginfo)
        );
        let terminal = matches!(terminal_outcome, Some(PhysicalWaitOutcome::NoChild))
            || matches!(
                terminal_outcome,
                Some(PhysicalWaitOutcome::Status { raw_status, .. })
                    if is_terminal_raw_status(*raw_status)
            )
            || typed_unsupported_terminal;
        let pidfd_exit_proof = match terminal_outcome {
            Some(PhysicalWaitOutcome::NoChild) => track.pidfd_exit_proof.is_some_and(
                |(
                    proof_wait,
                    proof_generation,
                    proof_task,
                    revents,
                    proof_launch,
                    proof_sequence,
                )| {
                    proof_wait == terminal_wait
                        && canonical_generation(proof_generation, &adoptions, &invalid_adoptions)
                            == terminal_generation
                        && terminal_context.is_some_and(|context| {
                            (proof_task.is_captured() && proof_task == context.task)
                                || matches!(
                                    track.kind,
                                    Some(PhysicalCleanupTransactionKind::StartupSetup {
                                        generation,
                                        task,
                                        launch,
                                        ..
                                    }) if generation == proof_generation
                                        && task == proof_task
                                        && proof_task.is_pidfd_bound_direct_child()
                                        && proof_task == context.task
                                        && proof_launch == Some(launch)
                                )
                        })
                        && revents & libc::POLLIN != 0
                        && terminal_result_sequence.is_some_and(|result_sequence| {
                            result_sequence < proof_sequence && proof_sequence < completion_sequence
                        })
                },
            ),
            Some(PhysicalWaitOutcome::Status { raw_status, .. })
                if is_terminal_raw_status(*raw_status) =>
            {
                track.pidfd_exit_proof.is_none()
            }
            Some(PhysicalWaitOutcome::UndecodableStatus { .. }) if typed_unsupported_terminal => {
                track.pidfd_exit_proof.is_none()
            }
            _ => false,
        };
        let terminal_boundary_sequence = match terminal_outcome {
            Some(PhysicalWaitOutcome::NoChild) => track
                .pidfd_exit_proof
                .map(|(_, _, _, _, _, proof_sequence)| proof_sequence),
            Some(PhysicalWaitOutcome::Status { raw_status, .. })
                if is_terminal_raw_status(*raw_status) =>
            {
                terminal_result_sequence
            }
            Some(PhysicalWaitOutcome::UndecodableStatus { .. }) if typed_unsupported_terminal => {
                terminal_result_sequence
            }
            _ => None,
        };
        let cause_context = wait_attempts.get(&cause_wait);
        let startup_kind = match track.kind {
            Some(PhysicalCleanupTransactionKind::StartupBarrier {
                generation,
                barrier,
                task,
                owner,
            }) => Some((generation, barrier, task, owner)),
            _ => None,
        };
        let setup_kind = match track.kind {
            Some(PhysicalCleanupTransactionKind::StartupSetup {
                generation,
                task,
                error,
                launch,
            }) => Some((generation, task, error, launch)),
            _ => None,
        };
        let startup_failure_link_sequences = startup_kind
            .map(|(generation, barrier, _, owner)| {
                startup_barrier_failures
                    .iter()
                    .filter_map(
                        |(
                            linked_generation,
                            linked_barrier,
                            linked_wait,
                            _,
                            linked_transaction,
                            linked_sequence,
                        )| {
                            (*linked_generation == generation
                                && *linked_barrier == barrier
                                && *linked_wait == cause_wait
                                && *linked_transaction == *transaction
                                && owner == PhysicalStartupCleanupOwner::AuthorizedWorker)
                                .then_some(*linked_sequence)
                        },
                    )
                    .chain(startup_barrier_statusless_failures.iter().filter_map(
                        |(
                            linked_generation,
                            linked_barrier,
                            linked_wait,
                            _,
                            linked_transaction,
                            linked_sequence,
                        )| {
                            (*linked_generation == generation
                                && *linked_barrier == barrier
                                && *linked_wait == cause_wait
                                && *linked_transaction == *transaction
                                && owner == PhysicalStartupCleanupOwner::AuthorizedWorker)
                                .then_some(*linked_sequence)
                        },
                    ))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let executor_transfer_valid =
            match (startup_kind, startup_executor_transfers.get(transaction)) {
                (
                    Some((generation, _, task, PhysicalStartupCleanupOwner::AuthorizedWorker)),
                    Some((transfer_generation, transfer_task, transfer_sequence)),
                ) => {
                    *transfer_generation == generation
                        && task.authorizes(*transfer_task, false)
                        && matches!(startup_failure_link_sequences.as_slice(), [failure_link]
                        if *failure_link < *transfer_sequence)
                        && *transfer_sequence < completion_sequence
                }
                (Some((_, _, _, PhysicalStartupCleanupOwner::AuthorizedWorker)), None) => false,
                (_, None) => true,
                (_, Some(_)) => false,
            };
        if !executor_transfer_valid {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
        }
        valid &= executor_transfer_valid;
        let setup_no_status_links = setup_kind
            .map(|(generation, _, error, launch)| {
                startup_setup_no_status_linked
                    .iter()
                    .filter(
                        |(
                            linked_generation,
                            linked_error,
                            linked_wait,
                            linked_transaction,
                            linked_launch,
                            _,
                        )| {
                            *linked_generation == generation
                                && *linked_error == error
                                && *linked_wait == cause_wait
                                && *linked_transaction == *transaction
                                && *linked_launch == launch
                        },
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let setup_no_status_link_sequence = match setup_no_status_links.as_slice() {
            [linked] => Some(linked.5),
            _ => None,
        };
        let pre_stop_failure = pre_stop_drain_failures.get(transaction);
        let contains_pre_stop_failure_status = track.statuses.keys().any(|status| {
            statuses.get(status).is_some_and(|status| {
                status.publication_destination
                    == Some(PhysicalStatusPublication::PreStopDrainFailureCleanup)
            })
        });
        let startup_barrier_statusless_cause =
            startup_kind.and_then(|(generation, barrier, task, owner)| {
                if !valid_startup_barrier_statusless_transactions.contains(transaction) {
                    return None;
                }
                let matches = startup_barrier_statusless_failures
                    .iter()
                    .filter(
                        |(
                            linked_generation,
                            linked_barrier,
                            linked_wait,
                            linked_error,
                            linked_transaction,
                            linked_sequence,
                        )| {
                            *linked_generation == generation
                                && *linked_barrier == barrier
                                && *linked_wait == cause_wait
                                && *linked_error != 0
                                && *linked_error != libc::EINTR
                                && *linked_transaction == *transaction
                                && start_sequence < *linked_sequence
                                && owner == PhysicalStartupCleanupOwner::AuthorizedWorker
                                && cause_context.is_some_and(|context| {
                                    context.producer == PhysicalWaitProducer::AuthorizedRootNotifier
                                        && task.authorizes(context.task, false)
                                })
                                && wait_outcome_matches_errno(
                                    wait_outcomes.get(&cause_wait),
                                    *linked_error,
                                )
                        },
                    )
                    .count();
                (matches == 1).then_some(())
            });
        let fatal_cause = if let Some((generation, barrier, task, owner)) = startup_kind {
            (startup_barrier_failure_transactions.get(transaction) == Some(&barrier)
                || startup_barrier_owners.get(&barrier) == Some(&cause_wait))
                && wait_generations.get(&cause_wait) == Some(&generation)
                && cause_context.is_some_and(|context| {
                    context.producer
                        == match owner {
                            PhysicalStartupCleanupOwner::Unstarted => {
                                PhysicalWaitProducer::PreRegistrationBarrierCleanup
                            }
                            PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                PhysicalWaitProducer::AuthorizedRootNotifier
                            }
                        }
                        && task.authorizes(context.task, false)
                })
                && (matches!(
                    wait_outcomes.get(&cause_wait),
                    Some(
                        PhysicalWaitOutcome::Status { .. }
                            | PhysicalWaitOutcome::UndecodableStatus { .. }
                    )
                ) || startup_barrier_statusless_cause.is_some())
        } else if let Some((generation, task, error, launch)) = setup_kind {
            let status_cause = startup_setup_linked.iter().any(
                |(
                    linked_generation,
                    linked_error,
                    linked_wait,
                    linked_status,
                    linked_transaction,
                    linked_launch,
                    _,
                )| {
                    *linked_generation == generation
                        && *linked_error == error
                        && *linked_wait == cause_wait
                        && *linked_transaction == *transaction
                        && *linked_launch == launch
                        && track.statuses.contains_key(linked_status)
                },
            ) && matches!(
                wait_outcomes.get(&cause_wait),
                Some(
                    PhysicalWaitOutcome::Status { .. }
                        | PhysicalWaitOutcome::UndecodableStatus { .. }
                )
            );
            let no_status_cause = setup_no_status_link_sequence.is_some()
                && track.statuses.is_empty()
                && matches!(
                    wait_outcomes.get(&cause_wait),
                    Some(PhysicalWaitOutcome::NoChild)
                );
            (status_cause || no_status_cause)
                && startup_barrier_setup_failures.iter().any(
                    |(failed_generation, failed_task, failed_error, failed_launch, _)| {
                        *failed_generation == generation
                            && *failed_task == task
                            && *failed_error == error
                            && *failed_launch == launch
                    },
                )
                && wait_generations.get(&cause_wait) == Some(&generation)
                && cause_context.is_some_and(|context| {
                    context.producer == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                        && task.authorizes(context.task, false)
                })
        } else if let Some((generation, stopped, recorded_cause, _)) = pre_stop_failure {
            *recorded_cause == cause_wait
                && track.statuses.contains_key(stopped)
                && wait_generations.get(&cause_wait) == Some(generation)
                && cause_context.is_some_and(|context| {
                    context.producer == PhysicalWaitProducer::PreStopContinuedDrain
                })
                && match wait_outcomes.get(&cause_wait) {
                    Some(PhysicalWaitOutcome::Error(error)) => *error != libc::EINTR,
                    Some(
                        PhysicalWaitOutcome::NoChild
                        | PhysicalWaitOutcome::UndecodableStatus { .. },
                    ) => true,
                    Some(PhysicalWaitOutcome::Status { raw_status, .. }) => {
                        !libc::WIFCONTINUED(*raw_status)
                    }
                    _ => false,
                }
        } else {
            !contains_pre_stop_failure_status
                && matches!(
                    wait_outcomes.get(&cause_wait),
                    Some(
                        PhysicalWaitOutcome::UndecodableStatus { .. }
                            | PhysicalWaitOutcome::Error(_)
                    )
                )
                && cause_context.is_some_and(|context| {
                    context.producer != PhysicalWaitProducer::PreStopContinuedDrain
                })
        };
        let statusless_error_to_echild =
            matches!(
                wait_outcomes.get(&cause_wait),
                Some(PhysicalWaitOutcome::Error(_))
            ) && matches!(terminal_outcome, Some(PhysicalWaitOutcome::NoChild));
        let setup_statusless_terminal = setup_kind.is_some()
            && cause_wait == terminal_wait
            && matches!(terminal_outcome, Some(PhysicalWaitOutcome::NoChild))
            && setup_no_status_link_sequence.is_some_and(|linked| {
                terminal_result_sequence.is_some_and(|result| {
                    result < start_sequence && start_sequence < linked
                }) && track.pidfd_exit_proof.is_some_and(
                    |(
                        proof_wait,
                        proof_generation,
                        proof_task,
                        revents,
                        proof_launch,
                        proof_sequence,
                    )| {
                        proof_wait == terminal_wait
                            && canonical_generation(
                                proof_generation,
                                &adoptions,
                                &invalid_adoptions,
                            ) == terminal_generation
                            && terminal_context.is_some_and(|context| {
                                (proof_task.is_captured() && proof_task == context.task)
                                    || setup_kind.is_some_and(
                                        |(generation, task, _, launch)| {
                                            generation == proof_generation
                                                && task == proof_task
                                                && proof_task.is_pidfd_bound_direct_child()
                                                && proof_task == context.task
                                                && proof_launch == Some(launch)
                                                && original_root_launch.is_some_and(
                                                    |(
                                                        root_launch,
                                                        root_generation,
                                                        root_task,
                                                        _,
                                                        _,
                                                        root_sequence,
                                                    )| {
                                                        root_launch == launch
                                                            && root_generation == generation
                                                            && root_task.authorizes(proof_task, true)
                                                            && root_sequence < proof_sequence
                                                    },
                                                )
                                        },
                                    )
                            })
                            && revents & libc::POLLIN != 0
                            && linked < proof_sequence
                            && proof_sequence < completion_sequence
                    },
                )
            });
        let startup_source_is_terminal = setup_statusless_terminal
            || ((startup_kind.is_some() || setup_kind.is_some())
                && cause_wait == terminal_wait
                && (matches!(
                    terminal_outcome,
                    Some(PhysicalWaitOutcome::Status { raw_status, .. })
                        if is_terminal_raw_status(*raw_status)
                ) || typed_unsupported_terminal));
        let transaction_pidfd_signals = pidfd_signal_attempts
            .iter()
            .filter(|(_, context)| context.transaction == *transaction)
            .collect::<Vec<_>>();
        let pidfd_signal_valid = if startup_kind.is_none() && setup_kind.is_none() {
            match transaction_pidfd_signals.as_slice() {
                [] => true,
                [(attempt, context)] => {
                    let attempt = **attempt;
                    let attempt_sequence = pidfd_signal_attempt_sequences.get(&attempt).copied();
                    let result_sequence = pidfd_signal_result_sequences.get(&attempt).copied();
                    let exact_identity = context.signal == libc::SIGKILL
                        && context.pidfd >= 0
                        && context.task.pidfd() == Some(context.pidfd)
                        && terminal_generation == Some(context.generation)
                        && terminal_context.is_some_and(|terminal| terminal.task == context.task);
                    let exact_result = matches!(
                        pidfd_signal_results.get(&attempt),
                        Some(
                            PhysicalPidfdSignalOutcome::Success
                                | PhysicalPidfdSignalOutcome::Error(libc::ESRCH)
                        )
                    );
                    let causally_before_registered_drain = attempt_sequence
                        .zip(result_sequence)
                        .is_some_and(|(attempt_sequence, result_sequence)| {
                            start_sequence < attempt_sequence
                                && wait_result_sequences
                                    .get(&cause_wait)
                                    .is_some_and(|cause| *cause < attempt_sequence)
                                && attempt_sequence < result_sequence
                                && result_sequence < completion_sequence
                                && wait_attempts.iter().all(|(wait, wait_context)| {
                                    wait_context.producer != PhysicalWaitProducer::RegisteredCleanup
                                        || wait_context.generation != terminal_generation
                                        || terminal_context.is_none_or(|terminal| {
                                            !context.task.authorizes(terminal.task, false)
                                        })
                                        || wait_attempt_sequences.get(wait).is_none_or(
                                            |wait_sequence| {
                                                *wait_sequence <= start_sequence
                                                    || completion_sequence <= *wait_sequence
                                                    || result_sequence < *wait_sequence
                                            },
                                        )
                                })
                        });
                    exact_identity && exact_result && causally_before_registered_drain
                }
                _ => false,
            }
        } else if startup_source_is_terminal {
            transaction_pidfd_signals.is_empty()
        } else {
            match transaction_pidfd_signals.as_slice() {
                [(attempt, context)] => {
                    let attempt = **attempt;
                    let attempt_sequence = pidfd_signal_attempt_sequences.get(&attempt).copied();
                    let result_sequence = pidfd_signal_result_sequences.get(&attempt).copied();
                    let result_ordered = attempt_sequence.zip(result_sequence).is_some_and(
                        |(attempt_sequence, result_sequence)| {
                            attempt_sequence < result_sequence
                                && result_sequence < completion_sequence
                                && terminal_result_sequence
                                    .is_some_and(|terminal| result_sequence < terminal)
                        },
                    );
                    let identity_matches = context.signal == libc::SIGKILL
                        && context.pidfd >= 0
                        && terminal_generation == Some(context.generation)
                        && terminal_context.is_some_and(|terminal_context| {
                            context.task.authorizes(terminal_context.task, false)
                        })
                        && context.task.pidfd() == Some(context.pidfd);
                    let result_shape = match pidfd_signal_results.get(&attempt) {
                        Some(PhysicalPidfdSignalOutcome::Success) => {
                            !startup_pidfd_exit_proofs.contains_key(transaction)
                        }
                        Some(PhysicalPidfdSignalOutcome::Error(libc::ESRCH)) => {
                            !startup_pidfd_exit_proofs.contains_key(transaction)
                        }
                        Some(PhysicalPidfdSignalOutcome::Error(error)) if *error != 0 => {
                            startup_pidfd_exit_proofs.get(transaction).is_some_and(
                                |(
                                    proof_generation,
                                    proof_task,
                                    proof_pidfd,
                                    revents,
                                    proof_sequence,
                                )| {
                                    *proof_generation == context.generation
                                        && *proof_task == context.task
                                        && *proof_pidfd == context.pidfd
                                        && revents & libc::POLLIN != 0
                                        && result_sequence.is_some_and(|result| {
                                            result < *proof_sequence
                                                && *proof_sequence < completion_sequence
                                                && wait_attempt_sequences
                                                    .get(&terminal_wait)
                                                    .is_some_and(|wait| *proof_sequence < *wait)
                                                && wait_attempts.iter().all(
                                                    |(wait_attempt, wait_context)| {
                                                        wait_attempt_sequences
                                                            .get(wait_attempt)
                                                            .is_none_or(|wait_sequence| {
                                                                *wait_sequence <= result
                                                                    || wait_context.generation
                                                                        != Some(context.generation)
                                                                    || !context.task.authorizes(
                                                                        wait_context.task,
                                                                        false,
                                                                    )
                                                                    || *proof_sequence
                                                                        < *wait_sequence
                                                            })
                                                    },
                                                )
                                        })
                                        && !resume_attempts.values().any(|resume| {
                                            resume.generation == Some(context.generation)
                                                && context.task.authorizes(resume.task, false)
                                                && resume.source_status.is_some_and(|status| {
                                                    track.statuses.contains_key(&status)
                                                })
                                        })
                                },
                            )
                        }
                        _ => false,
                    };
                    let causal_order = if let Some((generation, _, task, owner)) = startup_kind {
                        attempt_sequence.is_some_and(|attempt_sequence| match owner {
                            PhysicalStartupCleanupOwner::Unstarted => {
                                start_sequence < attempt_sequence
                            }
                            PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                startup_executor_transfers.get(transaction).is_some_and(
                                    |(transfer_generation, transfer_task, transfer_sequence)| {
                                        *transfer_generation == generation
                                            && task.authorizes(*transfer_task, false)
                                            && *transfer_sequence < attempt_sequence
                                    },
                                )
                            }
                        })
                    } else {
                        startup_setup_prepared.iter().any(
                            |(_, _, _, prepared_transaction, _, prepared_sequence)| {
                                *prepared_transaction == *transaction
                                    && attempt_sequence.is_some_and(|attempt_sequence| {
                                        *prepared_sequence < attempt_sequence
                                            && start_sequence < attempt_sequence
                                            && wait_result_sequences.get(&cause_wait).is_some_and(
                                                |cause_result| *cause_result < attempt_sequence,
                                            )
                                    })
                            },
                        )
                    };
                    let external_cleanup_after_signal = if let Some((
                        generation,
                        _,
                        task,
                        PhysicalStartupCleanupOwner::AuthorizedWorker,
                    )) = startup_kind
                    {
                        result_sequence.is_some_and(|signal_result| {
                            wait_attempts.iter().all(|(wait, context)| {
                                context.producer != PhysicalWaitProducer::RegisteredCleanup
                                    || context.generation != Some(generation)
                                    || !task.authorizes(context.task, false)
                                    || wait_attempt_sequences.get(wait).is_none_or(
                                        |wait_sequence| {
                                            *wait_sequence <= start_sequence
                                                || completion_sequence <= *wait_sequence
                                                || signal_result < *wait_sequence
                                        },
                                    )
                            }) && startup_barrier_statusless_resolutions.iter().all(
                                |(_, _, _, _, resolved_transaction, resolved_sequence)| {
                                    *resolved_transaction != *transaction
                                        || signal_result < *resolved_sequence
                                },
                            )
                        })
                    } else {
                        true
                    };
                    identity_matches
                        && causal_order
                        && result_ordered
                        && result_shape
                        && external_cleanup_after_signal
                }
                _ => false,
            }
        };
        if !pidfd_signal_valid {
            violations
                .push(PhysicalPartitionViolation::InvalidStartupCleanupPidfdSignal(*transaction));
        }
        valid &= pidfd_signal_valid;
        let mut transaction_registered_cleanup_waits = wait_attempts
            .iter()
            .filter(|(_, context)| {
                context.producer == PhysicalWaitProducer::RegisteredCleanup
                    && terminal_generation == context.generation
                    && terminal_context
                        .is_some_and(|terminal| context.task.authorizes(terminal.task, false))
            })
            .filter_map(|(wait, _)| {
                let attempt = wait_attempt_sequences.get(wait).copied()?;
                let result = wait_result_sequences.get(wait).copied()?;
                (start_sequence < attempt && result < completion_sequence)
                    .then_some((*wait, attempt, result))
            })
            .collect::<Vec<_>>();
        transaction_registered_cleanup_waits.sort_by_key(|(_, attempt, _)| *attempt);
        let registered_cleanup_waits_serialized = transaction_registered_cleanup_waits
            .iter()
            .all(|(_, attempt, result)| attempt < result)
            && transaction_registered_cleanup_waits.windows(2).all(|pair| {
                let (_, _, first_result) = pair[0];
                let (_, second_attempt, _) = pair[1];
                first_result < second_attempt
            });
        valid &= registered_cleanup_waits_serialized;
        let transaction_wait_failures = transaction_registered_cleanup_waits
            .iter()
            .copied()
            .filter(|(wait, _, _)| {
                matches!(wait_outcomes.get(wait), Some(PhysicalWaitOutcome::Error(error))
                    if *error != libc::EINTR)
            })
            .collect::<Vec<_>>();
        let wait_failure_exit_proof_valid = match startup_wait_failure_exit_proofs.get(transaction)
        {
            None => transaction_wait_failures.is_empty(),
            Some((
                proof_generation,
                proof_task,
                proof_pidfd,
                failed_wait,
                failed_error,
                revents,
                proof_sequence,
            )) => {
                let exact_failed_wait = matches!(transaction_wait_failures.as_slice(),
                    [(wait, attempt, result)]
                        if wait == failed_wait
                            && wait_outcomes.get(wait).is_some_and(|outcome| {
                                matches!(outcome, PhysicalWaitOutcome::Error(error)
                                    if error == failed_error && *error != libc::EINTR)
                            })
                            && *result < *proof_sequence
                            && wait_attempts.get(wait).is_some_and(|context| {
                                context.generation == Some(*proof_generation)
                                    && context.task.authorizes(*proof_task, false)
                            })
                            && *attempt < *result);
                let exact_identity = terminal_generation == Some(*proof_generation)
                    && terminal_context
                        .is_some_and(|context| proof_task.authorizes(context.task, false))
                    && proof_task.pidfd() == Some(*proof_pidfd)
                    && *revents & libc::POLLIN != 0;
                let other_cleanup_waits_outside_failure_to_proof = transaction_wait_failures
                    .iter()
                    .find_map(|(wait, attempt, result)| {
                        (*wait == *failed_wait).then_some((*attempt, *result))
                    })
                    .is_some_and(|(failed_attempt, failed_result)| {
                        failed_result < *proof_sequence
                            && transaction_registered_cleanup_waits.iter().all(
                                |(wait, attempt, result)| {
                                    *wait == *failed_wait
                                        || *result < failed_attempt
                                        || *proof_sequence < *attempt
                                },
                            )
                    });
                let exact_signal = transaction_pidfd_signals.as_slice().first().is_some_and(
                    |(signal_attempt, signal_context)| {
                        signal_context.pidfd == *proof_pidfd
                            && signal_context.task == *proof_task
                            && pidfd_signal_results
                                .get(signal_attempt)
                                .is_some_and(|outcome| {
                                    matches!(
                                        outcome,
                                        PhysicalPidfdSignalOutcome::Success
                                            | PhysicalPidfdSignalOutcome::Error(libc::ESRCH)
                                    )
                                })
                            && pidfd_signal_result_sequences
                                .get(signal_attempt)
                                .is_some_and(|signal| {
                                    transaction_wait_failures
                                        .first()
                                        .is_some_and(|(_, wait, _)| *signal < *wait)
                                })
                    },
                );
                let exact_order = terminal_result_sequence.is_some_and(|terminal_result| {
                    wait_attempt_sequences
                        .get(&terminal_wait)
                        .is_some_and(|terminal_attempt| {
                            *proof_sequence < *terminal_attempt
                                && *terminal_attempt < terminal_result
                                && terminal_result < completion_sequence
                        })
                });
                startup_kind.is_some()
                    && exact_failed_wait
                    && exact_identity
                    && other_cleanup_waits_outside_failure_to_proof
                    && exact_signal
                    && exact_order
                    && (!matches!(
                        startup_kind,
                        Some((_, _, _, PhysicalStartupCleanupOwner::AuthorizedWorker))
                    ) || transaction_registered_cleanup_waits
                        .last()
                        .is_some_and(|(wait, _, _)| *wait == terminal_wait))
            }
        };
        if !wait_failure_exit_proof_valid {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
        }
        valid &= wait_failure_exit_proof_valid;
        let startup_pidfd_signal_result_sequence = transaction_pidfd_signals
            .first()
            .and_then(|(attempt, _)| pidfd_signal_result_sequences.get(attempt).copied());
        let startup_resume_proof_owner = startup_kind
            .map(|(generation, _, task, owner)| {
                (
                    generation,
                    task,
                    match owner {
                        PhysicalStartupCleanupOwner::Unstarted => {
                            PhysicalResumeOwner::StartupBarrierCleanup
                        }
                        PhysicalStartupCleanupOwner::AuthorizedWorker => {
                            PhysicalResumeOwner::AuthorizedRootExternalCleanup
                        }
                    },
                )
            })
            .or_else(|| {
                setup_kind.map(|(generation, task, _, _)| {
                    (generation, task, PhysicalResumeOwner::StartupBarrierCleanup)
                })
            });
        let startup_resume_failure_proof_valid = match startup_resume_failure_exit_proofs
            .get(transaction)
        {
            None => true,
            Some((
                proof_generation,
                proof_task,
                proof_pidfd,
                failed_resume,
                source_status,
                failed_error,
                revents,
                proof_sequence,
            )) => {
                let resume_context = resume_attempts.get(failed_resume);
                let resume_attempt_sequence = resume_attempt_sequences.get(failed_resume).copied();
                let resume_result_sequence = resume_result_sequences.get(failed_resume).copied();
                let source_track = statuses.get(source_status);
                startup_resume_proof_owner.is_some_and(|(generation, task, resume_owner)| {
                    generation == *proof_generation
                        && task == *proof_task
                        && resume_context.is_some_and(|context| context.owner == resume_owner)
                }) && *failed_error != 0
                    && terminal_generation == Some(*proof_generation)
                    && proof_task.pidfd() == Some(*proof_pidfd)
                    && *revents & libc::POLLIN != 0
                    && track.statuses.contains_key(source_status)
                    && (!matches!(
                        startup_kind,
                        Some((_, _, _, PhysicalStartupCleanupOwner::AuthorizedWorker))
                    ) || transaction_registered_cleanup_waits
                        .last()
                        .is_some_and(|(wait, _, _)| *wait == terminal_wait))
                    && source_track.is_some_and(|status| {
                        status
                            .raw_status
                            .is_some_and(|raw_status| libc::WIFSTOPPED(raw_status))
                            && status.cancellation_cleanup_dispositions == 1
                            && status
                                .cancellation_cleanup_sequence
                                .is_some_and(|disposition| {
                                    terminal_boundary_sequence.is_some_and(|terminal_boundary| {
                                        terminal_boundary < disposition
                                            && disposition < completion_sequence
                                    })
                                })
                    })
                    && resume_context.is_some_and(|context| {
                        context.generation == Some(*proof_generation)
                            && context.task == *proof_task
                            && context.source_status == Some(*source_status)
                            && context.operation == PhysicalResumeOperation::Continue
                            && context.signal.is_none()
                            && startup_resume_proof_owner
                                .is_some_and(|(_, _, resume_owner)| context.owner == resume_owner)
                    })
                    && resume_results.get(failed_resume)
                        == Some(&PhysicalResumeOutcome::Error(*failed_error))
                    && !tolerated.contains_key(failed_resume)
                    && !track.resumes.contains_key(failed_resume)
                    && startup_pidfd_signal_result_sequence.is_some_and(|signal_result| {
                        resume_attempt_sequence.is_some_and(|resume_attempt| {
                            resume_result_sequence.is_some_and(|resume_result| {
                                wait_attempt_sequences.get(&terminal_wait).is_some_and(
                                    |terminal_attempt| {
                                        signal_result < resume_attempt
                                            && resume_attempt < resume_result
                                            && resume_result < *proof_sequence
                                            && *proof_sequence < *terminal_attempt
                                            && terminal_result_sequence.is_some_and(
                                                |terminal_result| {
                                                    *terminal_attempt < terminal_result
                                                        && terminal_result < completion_sequence
                                                },
                                            )
                                    },
                                )
                            })
                        })
                    })
                    && resume_attempt_sequence.is_some_and(|failed_attempt| {
                        transaction_registered_cleanup_waits.iter().all(
                            |(_, wait_attempt, wait_result)| {
                                *wait_result < failed_attempt || *proof_sequence < *wait_attempt
                            },
                        )
                    })
                    && resume_attempt_sequence
                        .zip(resume_result_sequence)
                        .is_some_and(|(failed_attempt, _)| {
                            resume_attempts.iter().all(|(attempt, context)| {
                                context.source_status.is_none_or(|status| {
                                    !track.statuses.contains_key(&status)
                                        || *attempt == *failed_resume
                                        || resume_result_sequences
                                            .get(attempt)
                                            .is_some_and(|result| *result < failed_attempt)
                                })
                            })
                        })
            }
        };
        if !startup_resume_failure_proof_valid {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
        }
        valid &= startup_resume_failure_proof_valid;
        if let Some((generation, barrier, task, _)) = startup_kind {
            let source_status = match wait_outcomes.get(&cause_wait) {
                Some(PhysicalWaitOutcome::Status { id, .. })
                | Some(PhysicalWaitOutcome::UndecodableStatus { id, .. }) => Some(*id),
                _ => None,
            };
            valid &= terminal_generation == Some(generation)
                && (source_status.is_some_and(|status| track.statuses.contains_key(&status))
                    || startup_barrier_statusless_cause.is_some())
                && startup_barrier_owners.get(&barrier) == Some(&cause_wait)
                && exact_original_root_launch_captured(None, generation, task, start_sequence);
        }
        if let Some((generation, task, error, launch)) = setup_kind {
            let source_status = match wait_outcomes.get(&cause_wait) {
                Some(PhysicalWaitOutcome::Status { id, .. })
                | Some(PhysicalWaitOutcome::UndecodableStatus { id, .. }) => Some(*id),
                _ => None,
            };
            valid &= terminal_generation == Some(generation)
                && (source_status.is_some_and(|status| track.statuses.contains_key(&status))
                    || setup_statusless_terminal)
                && startup_setup_prepared.iter().any(
                    |(
                        prepared_generation,
                        prepared_task,
                        prepared_error,
                        prepared_transaction,
                        prepared_launch,
                        prepared_sequence,
                    )| {
                        *prepared_generation == generation
                            && *prepared_task == task
                            && *prepared_error == error
                            && *prepared_transaction == *transaction
                            && *prepared_launch == launch
                            && *prepared_sequence < start_sequence
                    },
                )
                && original_root_launch.is_some_and(
                    |(root_launch, launch_generation, launch_task, _, _, launch_sequence)| {
                        root_launch == launch
                            && launch_generation == generation
                            && launch_task.authorizes(task, true)
                            && launch_sequence < start_sequence
                    },
                );
        }
        valid &= terminal
            && pidfd_exit_proof
            && terminal_context.is_some_and(|context| match track.kind {
                Some(PhysicalCleanupTransactionKind::Registered) => {
                    context.producer == PhysicalWaitProducer::RegisteredCleanup
                }
                Some(PhysicalCleanupTransactionKind::StartupBarrier { task, owner, .. }) => {
                    context.producer
                        == if startup_source_is_terminal {
                            match owner {
                                PhysicalStartupCleanupOwner::Unstarted => {
                                    PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                }
                                PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                    PhysicalWaitProducer::AuthorizedRootNotifier
                                }
                            }
                        } else {
                            match owner {
                                PhysicalStartupCleanupOwner::Unstarted => {
                                    PhysicalWaitProducer::PreRegistrationBarrierCleanup
                                }
                                PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                    PhysicalWaitProducer::RegisteredCleanup
                                }
                            }
                        }
                        && task.authorizes(context.task, false)
                }
                Some(PhysicalCleanupTransactionKind::StartupSetup { task, .. }) => {
                    context.producer == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                        && task.authorizes(context.task, false)
                }
                None => false,
            })
            && terminal_generation.is_some()
            && terminal_result_sequence.is_some_and(|sequence| {
                if startup_source_is_terminal {
                    sequence < start_sequence && start_sequence < completion_sequence
                } else {
                    start_sequence < sequence && sequence < completion_sequence
                }
            })
            && (!track.statuses.is_empty()
                || statusless_error_to_echild
                || setup_statusless_terminal);
        if let Some((_, _, _, failure_record_sequence)) = pre_stop_failure {
            valid &= wait_attempt_sequences
                .get(&terminal_wait)
                .is_some_and(|started| *failure_record_sequence < *started)
                && terminal_result_sequence.is_some_and(|result| *failure_record_sequence < result);
        }
        valid &= (cause_wait != terminal_wait || startup_source_is_terminal)
            && fatal_cause
            && wait_generations.get(&cause_wait).copied() == terminal_generation
            && wait_result_sequences
                .get(&cause_wait)
                .is_some_and(|sequence| *sequence < start_sequence)
            && cause_context.is_some_and(|context| {
                if let Some((_, _, _, owner)) = startup_kind {
                    context.producer
                        == match owner {
                            PhysicalStartupCleanupOwner::Unstarted => {
                                PhysicalWaitProducer::PreRegistrationBarrierCleanup
                            }
                            PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                PhysicalWaitProducer::AuthorizedRootNotifier
                            }
                        }
                } else if setup_kind.is_some() {
                    context.producer == PhysicalWaitProducer::PreRegistrationBarrierCleanup
                } else {
                    matches!(
                        context.producer,
                        PhysicalWaitProducer::NotifierWorker
                            | PhysicalWaitProducer::AuthorizedRootNotifier
                            | PhysicalWaitProducer::SynchronousWait
                    ) || (pre_stop_failure.is_some()
                        && context.producer == PhysicalWaitProducer::PreStopContinuedDrain)
                }
            })
            && cause_context.is_some_and(|context| {
                terminal_context.is_some_and(|terminal_context| {
                    context.task.authorizes(terminal_context.task, false)
                })
            });
        if let Some(PhysicalWaitOutcome::UndecodableStatus { id, .. }) =
            wait_outcomes.get(&cause_wait)
        {
            valid &= track.statuses.contains_key(id);
        }

        for (status, link_sequence) in &track.statuses {
            let terminal_status = matches!(
                terminal_outcome,
                Some(PhysicalWaitOutcome::Status { id, .. }) if id == status
            ) || matches!(
                terminal_outcome,
                Some(PhysicalWaitOutcome::UndecodableStatus { id, siginfo, .. })
                    if id == status
                        && siginfo_is_startup_typed_unsupported_terminal(*siginfo)
            );
            let status_matches = statuses.get(status).is_some_and(|status_track| {
                status_track.generation == terminal_generation
                    && status_track.created_sequence < *link_sequence
                    && terminal_context.is_some_and(|context| {
                        status_track
                            .task
                            .is_some_and(|task| task.authorizes(context.task, false))
                    })
            });
            let lifecycle_ordered = statuses.get(status).is_some_and(|status_track| {
                let lifecycle_sequence =
                    match (status_track.dispositions, status_track.successful_resumes) {
                        (1, 0) => status_track.last_disposition_sequence,
                        (0, 1) => status_track.successful_resume_sequence,
                        _ => None,
                    };
                lifecycle_sequence.is_some_and(|sequence| {
                    *link_sequence < sequence
                        && pre_stop_failure.is_none_or(|(_, _, _, failed)| *failed < sequence)
                        && sequence < completion_sequence
                        && (status_track.dispositions == 0
                            || terminal_boundary_sequence
                                .is_some_and(|terminal| terminal < sequence))
                })
            });
            let registered_publication_ordered = statuses.get(status).is_some_and(|status_track| {
                !matches!(
                    status_track.publication_destination,
                    Some(
                        PhysicalStatusPublication::CleanupStopped
                            | PhysicalStatusPublication::PreStopDrainFailureCleanup
                            | PhysicalStatusPublication::StartupBarrierFailureCleanup
                            | PhysicalStatusPublication::StartupBarrierCleanupStopped
                            | PhysicalStatusPublication::StartupBarrierCleanupTerminal
                    )
                ) || status_track.publication_sequence.is_some_and(|sequence| {
                    *link_sequence < sequence && sequence < completion_sequence
                })
            });
            let registered_resumes = resume_attempts
                .iter()
                .filter(|(attempt, context)| {
                    context.source_status == Some(*status)
                        && match track.kind {
                            Some(PhysicalCleanupTransactionKind::Registered) => {
                                context.owner.is_registered_controller_cleanup()
                            }
                            Some(PhysicalCleanupTransactionKind::StartupBarrier {
                                generation,
                                task,
                                owner,
                                ..
                            }) => match owner {
                                PhysicalStartupCleanupOwner::Unstarted => {
                                    context.owner.is_startup_barrier_cleanup()
                                }
                                PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                    context.owner
                                        == PhysicalResumeOwner::AuthorizedRootExternalCleanup
                                        && startup_executor_transfers.get(transaction).is_some_and(
                                            |(transfer_generation, transfer_task, transfer)| {
                                                *transfer_generation == generation
                                                    && task.authorizes(*transfer_task, false)
                                                    && transfer_task.authorizes(context.task, false)
                                                    && resume_attempt_sequences
                                                        .get(attempt)
                                                        .is_some_and(|resume| *transfer < *resume)
                                            },
                                        )
                                }
                            },
                            Some(PhysicalCleanupTransactionKind::StartupSetup { .. }) => {
                                context.owner.is_startup_barrier_cleanup()
                            }
                            None => false,
                        }
                })
                .collect::<Vec<_>>();
            let exact_pre_stop_resume_cardinality =
                pre_stop_failure.is_none_or(|(_, failed_status, _, _)| {
                    failed_status != status || registered_resumes.len() <= 1
                });
            let exact_startup_resume_cardinality = (startup_kind.is_none() && setup_kind.is_none())
                || if terminal_status
                    || !statuses.get(status).is_some_and(|status| {
                        status
                            .raw_status
                            .is_some_and(|raw_status| libc::WIFSTOPPED(raw_status))
                            || status.startup_typed_unsupported_stopped()
                    })
                {
                    registered_resumes.is_empty()
                } else {
                    registered_resumes.len() == 1
                        || (registered_resumes.is_empty()
                            && statuses.get(status).is_some_and(|status| {
                                status.successful_resumes == 0
                                    && status.dispositions == 1
                                    && status.cancellation_cleanup_dispositions == 1
                                    && status.cancellation_cleanup_sequence
                                        == status.last_disposition_sequence
                            }))
                };
            let registered_resumes_ordered =
                registered_resumes.into_iter().all(|(resume, context)| {
                    let attempt_sequence = resume_attempt_sequences.get(resume).copied();
                    let result_sequence = resume_result_sequences.get(resume).copied();
                    let startup_cleanup = startup_kind.is_some() || setup_kind.is_some();
                    let base_ordered = attempt_sequence.zip(result_sequence).is_some_and(
                        |(attempt_sequence, result_sequence)| {
                            start_sequence < *link_sequence
                                && *link_sequence < attempt_sequence
                                && ((startup_kind.is_none() && setup_kind.is_none())
                                    || startup_pidfd_signal_result_sequence
                                        .is_some_and(|signal| signal < attempt_sequence))
                                && pre_stop_failure
                                    .is_none_or(|(_, _, _, failed)| *failed < attempt_sequence)
                                && attempt_sequence < result_sequence
                                && result_sequence < completion_sequence
                                && resume_generations.get(resume).copied() == terminal_generation
                                && terminal_context.is_some_and(|terminal_context| {
                                    context.task.authorizes(terminal_context.task, false)
                                })
                        },
                    );
                    base_ordered
                        && match resume_results.get(resume) {
                            Some(PhysicalResumeOutcome::Success) => {
                                !tolerated.contains_key(resume)
                                    && !track.resumes.contains_key(resume)
                                    && (!startup_cleanup
                                        || startup_resume_failure_exit_proofs
                                            .get(transaction)
                                            .is_none_or(|(_, _, _, failed, ..)| failed != resume))
                            }
                            Some(PhysicalResumeOutcome::Error(error)) if startup_cleanup => {
                                *error != 0
                                    && !tolerated.contains_key(resume)
                                    && !track.resumes.contains_key(resume)
                                    && startup_resume_failure_exit_proofs
                                        .get(transaction)
                                        .is_some_and(
                                            |(_, _, _, failed, failed_status, failed_error, ..)| {
                                                failed == resume
                                                    && failed_status == status
                                                    && failed_error == error
                                            },
                                        )
                            }
                            Some(PhysicalResumeOutcome::Error(error))
                                if matches!(*error, libc::ESRCH | libc::EIO) =>
                            {
                                track.resumes.get(resume).is_some_and(|resume_link| {
                                    result_sequence.is_some_and(|result_sequence| {
                                        tolerated_sequences.get(resume).is_some_and(
                                            |tolerated_sequence| {
                                                result_sequence < *tolerated_sequence
                                                    && *tolerated_sequence < *resume_link
                                                    && *resume_link < completion_sequence
                                            },
                                        )
                                    })
                                })
                            }
                            Some(PhysicalResumeOutcome::Error(error)) => {
                                let _exact_error = error;
                                false
                            }
                            None => false,
                        }
                });
            valid &= status_matches
                && lifecycle_ordered
                && registered_publication_ordered
                && exact_pre_stop_resume_cardinality
                && exact_startup_resume_cardinality
                && registered_resumes_ordered
                && start_sequence < *link_sequence
                && *link_sequence < completion_sequence;
        }
        if let Some(PhysicalWaitOutcome::Status { id, .. }) = terminal_outcome {
            valid &= track.statuses.contains_key(id);
        }
        for (resume, link_sequence) in &track.resumes {
            let context = resume_attempts.get(resume);
            let error = match resume_results.get(resume) {
                Some(PhysicalResumeOutcome::Error(error)) => Some(*error),
                _ => None,
            };
            let source_linked = context
                .and_then(|context| context.source_status)
                .and_then(|status| track.statuses.get(&status).copied());
            let attempt_sequence = resume_attempt_sequences.get(resume).copied();
            let resume_matches = context.is_some_and(|context| {
                (match track.kind {
                    Some(PhysicalCleanupTransactionKind::Registered) => {
                        context.owner.is_registered_controller_cleanup()
                    }
                    Some(PhysicalCleanupTransactionKind::StartupBarrier {
                        generation,
                        task,
                        owner,
                        ..
                    }) => match owner {
                        PhysicalStartupCleanupOwner::Unstarted => {
                            context.owner.is_startup_barrier_cleanup()
                        }
                        PhysicalStartupCleanupOwner::AuthorizedWorker => {
                            context.owner == PhysicalResumeOwner::AuthorizedRootExternalCleanup
                                && startup_executor_transfers.get(transaction).is_some_and(
                                    |(transfer_generation, transfer_task, transfer)| {
                                        *transfer_generation == generation
                                            && task.authorizes(*transfer_task, false)
                                            && transfer_task.authorizes(context.task, false)
                                            && attempt_sequence
                                                .is_some_and(|resume| *transfer < resume)
                                    },
                                )
                        }
                    },
                    Some(PhysicalCleanupTransactionKind::StartupSetup { .. }) => {
                        context.owner.is_startup_barrier_cleanup()
                    }
                    None => false,
                }) && resume_generations.get(resume).copied() == terminal_generation
                    && terminal_context.is_some_and(|terminal_context| {
                        context.task.authorizes(terminal_context.task, false)
                    })
            }) && error
                .is_some_and(|error| matches!(error, libc::ESRCH | libc::EIO))
                && tolerated.get(resume) == error.as_ref()
                && source_linked.is_some()
                && resume_result_sequences.get(resume).is_some_and(|sequence| {
                    tolerated_sequences
                        .get(resume)
                        .is_some_and(|tolerated_sequence| {
                            source_linked.is_some_and(|status_link| {
                                attempt_sequence.is_some_and(|attempt_sequence| {
                                    start_sequence < status_link
                                        && status_link < attempt_sequence
                                        && attempt_sequence < *sequence
                                        && sequence < tolerated_sequence
                                        && tolerated_sequence < link_sequence
                                })
                            })
                        })
                });
            valid &= resume_matches
                && start_sequence < *link_sequence
                && *link_sequence < completion_sequence;
        }
        if valid {
            valid_cleanup_transactions.insert(*transaction);
        } else {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
        }
    }
    for context in pidfd_signal_attempts.values() {
        if !cleanup_transactions.contains_key(&context.transaction) {
            violations.push(
                PhysicalPartitionViolation::InvalidStartupCleanupPidfdSignal(context.transaction),
            );
        }
    }
    for transaction in startup_pidfd_exit_proofs.keys() {
        if !cleanup_transactions.contains_key(transaction)
            || !pidfd_signal_attempts
                .values()
                .any(|context| context.transaction == *transaction)
        {
            violations
                .push(PhysicalPartitionViolation::InvalidStartupCleanupPidfdSignal(*transaction));
        }
    }
    for transaction in startup_wait_failure_exit_proofs.keys() {
        if !valid_cleanup_transactions.contains(transaction) {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
        }
    }
    for transaction in startup_resume_failure_exit_proofs.keys() {
        if !valid_cleanup_transactions.contains(transaction) {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
        }
    }
    for transaction in startup_executor_transfers.keys() {
        if !valid_cleanup_transactions.contains(transaction) {
            violations.push(
                PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(*transaction),
            );
        }
    }
    for (transaction, barrier) in &startup_barrier_failure_transactions {
        if !valid_cleanup_transactions.contains(transaction) {
            violations.push(PhysicalPartitionViolation::InvalidPreRegistrationBarrier(
                *barrier,
            ));
        }
    }
    for (attempt, context) in &wait_attempts {
        if context.producer != PhysicalWaitProducer::PreStopContinuedDrain {
            continue;
        }
        let attempt_sequence = wait_attempt_sequences.get(attempt).copied();
        let result_sequence = wait_result_sequences.get(attempt).copied();
        let generation = wait_generations.get(attempt).copied();
        let matching_drains = pre_stop_drains
            .iter()
            .filter(|(before, drain)| {
                generation == Some(drain.generation)
                    && statuses.get(before).is_some_and(|before_track| {
                        before_track.created_sequence < attempt_sequence.unwrap_or_default()
                            && result_sequence
                                .is_some_and(|result| result < drain.completed_sequence)
                            && before_track
                                .task
                                .is_some_and(|task| task.authorizes(context.task, false))
                    })
            })
            .collect::<Vec<_>>();
        let completed_drain_attempt = if matching_drains.len() == 1 {
            let (before, drain) = matching_drains[0];
            if *attempt == drain.final_no_status_attempt {
                matches!(
                    wait_outcomes.get(attempt),
                    Some(PhysicalWaitOutcome::NoStatus { siginfo: Some(siginfo) })
                        if siginfo_is_exact_no_status(*siginfo)
                )
            } else {
                match wait_outcomes.get(attempt) {
                    Some(PhysicalWaitOutcome::Interrupted) => true,
                    Some(PhysicalWaitOutcome::Status { id, raw_status, .. }) => {
                        libc::WIFCONTINUED(*raw_status)
                            && statuses.get(id).is_some_and(|status| {
                                status.publication_destination
                                    == Some(PhysicalStatusPublication::ContinuedSideChannel {
                                        route: PhysicalContinuedStatusRoute::PreStopDrain {
                                            before: *before,
                                        },
                                    })
                            })
                    }
                    _ => false,
                }
            }
        } else {
            false
        };
        let matching_failed_drains = pre_stop_drain_failures
            .iter()
            .filter(|(transaction, (failure_generation, before, cause, _))| {
                valid_cleanup_transactions.contains(transaction)
                    && generation == Some(*failure_generation)
                    && statuses.get(before).is_some_and(|before_track| {
                        before_track.created_sequence < attempt_sequence.unwrap_or_default()
                            && wait_result_sequences
                                .get(cause)
                                .is_some_and(|cause_result| {
                                    result_sequence.is_some_and(|result| result <= *cause_result)
                                })
                            && before_track
                                .task
                                .is_some_and(|task| task.authorizes(context.task, false))
                    })
            })
            .collect::<Vec<_>>();
        let failed_drain_attempt = if matching_failed_drains.len() == 1 {
            let (_, (_, before, cause, _)) = matching_failed_drains[0];
            if *attempt == *cause {
                cleanup_cause_wait_transactions
                    .get(attempt)
                    .is_some_and(|transaction| valid_cleanup_transactions.contains(transaction))
            } else {
                match wait_outcomes.get(attempt) {
                    Some(PhysicalWaitOutcome::Interrupted) => true,
                    Some(PhysicalWaitOutcome::Status { id, raw_status, .. }) => {
                        libc::WIFCONTINUED(*raw_status)
                            && statuses.get(id).is_some_and(|status| {
                                status.publication_destination
                                    == Some(PhysicalStatusPublication::ContinuedSideChannel {
                                        route: PhysicalContinuedStatusRoute::PreStopDrain {
                                            before: *before,
                                        },
                                    })
                            })
                    }
                    _ => false,
                }
            }
        } else {
            false
        };
        if !completed_drain_attempt && !failed_drain_attempt {
            violations
                .push(PhysicalPartitionViolation::InvalidPreStopContinuedDrainAttempt(*attempt));
        }
    }
    for (attempt, context) in &wait_attempts {
        if context.producer != PhysicalWaitProducer::RegisteredCleanup {
            continue;
        }
        let attempt_sequence = wait_attempt_sequences.get(attempt).copied();
        let result_sequence = wait_result_sequences.get(attempt).copied();
        let outcome = wait_outcomes.get(attempt);
        let matching_transactions = cleanup_transactions
            .iter()
            .filter(|(transaction, track)| {
                if !valid_cleanup_transactions.contains(transaction) {
                    return false;
                }
                let (
                    Some(start_sequence),
                    Some(completion_sequence),
                    Some(terminal_wait),
                    Some(attempt_sequence),
                    Some(result_sequence),
                ) = (
                    track.start_sequence,
                    track.completion_sequence,
                    track.terminal_wait,
                    attempt_sequence,
                    result_sequence,
                )
                else {
                    return false;
                };
                if !(start_sequence < attempt_sequence
                    && attempt_sequence < result_sequence
                    && result_sequence < completion_sequence)
                {
                    return false;
                }
                let Some(terminal_context) = wait_attempts.get(&terminal_wait) else {
                    return false;
                };
                if wait_generations.get(attempt) != wait_generations.get(&terminal_wait)
                    || !context.task.authorizes(terminal_context.task, false)
                {
                    return false;
                }
                if *attempt == terminal_wait {
                    return matches!(outcome, Some(PhysicalWaitOutcome::NoChild))
                        || matches!(
                            outcome,
                            Some(PhysicalWaitOutcome::Status { raw_status, .. })
                                if is_terminal_raw_status(*raw_status)
                        );
                }
                match outcome {
                    Some(PhysicalWaitOutcome::Status { id, raw_status, .. }) => {
                        libc::WIFSTOPPED(*raw_status) && track.statuses.contains_key(id)
                    }
                    Some(PhysicalWaitOutcome::Interrupted) => true,
                    Some(
                        PhysicalWaitOutcome::RetainedStatus { .. }
                        | PhysicalWaitOutcome::RetainedUndecodableStatus { .. }
                        | PhysicalWaitOutcome::UndecodableStatus { .. }
                        | PhysicalWaitOutcome::NoStatus { .. }
                        | PhysicalWaitOutcome::NoChild
                        | PhysicalWaitOutcome::Error(_),
                    )
                    | None => false,
                }
            })
            .count();
        if matching_transactions != 1 {
            violations
                .push(PhysicalPartitionViolation::InvalidRegisteredCleanupWaitOwnership(*attempt));
        }
    }
    for (attempt, outcome) in &wait_outcomes {
        let fatal = matches!(
            outcome,
            PhysicalWaitOutcome::UndecodableStatus { .. } | PhysicalWaitOutcome::Error(_)
        ) && wait_attempts.get(attempt).is_some_and(|context| {
            matches!(
                context.producer,
                PhysicalWaitProducer::NotifierWorker
                    | PhysicalWaitProducer::AuthorizedRootNotifier
                    | PhysicalWaitProducer::PreStopContinuedDrain
                    | PhysicalWaitProducer::SynchronousWait
            )
        }) || *outcome == PhysicalWaitOutcome::NoChild
            && wait_attempts.get(attempt).is_some_and(|context| {
                context.producer == PhysicalWaitProducer::PreStopContinuedDrain
            });
        if fatal
            && !cleanup_cause_wait_transactions
                .get(attempt)
                .is_some_and(|transaction| valid_cleanup_transactions.contains(transaction))
        {
            violations
                .push(PhysicalPartitionViolation::FatalWaitWithoutCleanupTransaction(*attempt));
        }
    }
    for attempt in resume_results.keys() {
        let Some(context) = resume_attempts.get(attempt) else {
            continue;
        };
        let Some(status) = context.source_status else {
            continue;
        };
        let Some(attempt_sequence) = resume_attempt_sequences.get(attempt).copied() else {
            continue;
        };
        let valid_source = statuses.get(&status).is_some_and(|track| {
            if track.undecodable {
                (context.owner.is_registered_controller_cleanup()
                    || context.owner.is_startup_barrier_cleanup())
                    && cleanup_status_transactions
                        .get(&status)
                        .is_some_and(|transaction| valid_cleanup_transactions.contains(transaction))
            } else {
                let published = track
                    .publication_sequence
                    .is_some_and(|published| published < attempt_sequence);
                let stopped = track
                    .raw_status
                    .is_some_and(|raw_status| libc::WIFSTOPPED(raw_status));
                let one_consuming_delivery = reservations
                    .values()
                    .filter(|reservation| {
                        reservation.status == Some(status)
                            && matches!(
                                reservation.decode_finished,
                                Some((_, PhysicalDecodeOutcome::Returned, _))
                            )
                            && matches!(
                                reservation.completion,
                                Some((ReservationCompletion::Committed, completed))
                                    if completed < attempt_sequence
                            )
                    })
                    .count()
                    == 1;
                let claimed_exit_capability =
                    exit_capabilities.get(&status).is_some_and(|capability| {
                        capability.transitions.iter().any(|(sequence, transition)| {
                            *sequence < attempt_sequence
                                && *transition == PhysicalExitCapabilityTransition::Claimed
                        })
                    });
                let cleanup_exit_capability =
                    exit_capabilities.get(&status).is_some_and(|capability| {
                        let claimed =
                            capability
                                .transitions
                                .iter()
                                .find_map(|(sequence, transition)| {
                                    (*transition == PhysicalExitCapabilityTransition::Claimed
                                        && *sequence < attempt_sequence)
                                        .then_some(*sequence)
                                });
                        let revoked =
                            capability.transitions.iter().any(|(sequence, transition)| {
                                *sequence < attempt_sequence
                                    && *transition == PhysicalExitCapabilityTransition::Revoked
                            });
                        let transferred =
                            capability.transitions.iter().any(|(sequence, transition)| {
                                *sequence < attempt_sequence
                                    && *transition
                                        == PhysicalExitCapabilityTransition::TransferredToCleanup
                                    && claimed.is_some_and(|claimed| claimed < *sequence)
                            });
                        revoked || transferred
                    });
                let synchronous_cancel_delivery = context.owner
                    == PhysicalResumeOwner::SynchronousCancellation
                    && reservations.values().any(|reservation| {
                        reservation.status == Some(status)
                            && matches!(
                                reservation.decode_finished,
                                Some((
                                    PhysicalDecodeOwner::Synchronous,
                                    PhysicalDecodeOutcome::Cancelled,
                                    finished
                                )) if finished < attempt_sequence
                            )
                            && matches!(
                                reservation.completion,
                                Some((ReservationCompletion::Committed, completed))
                                    if attempt_sequence < completed
                                        && reservation.decode_finished.is_some_and(
                                        |(_, _, finished)| {
                                            valid_synchronous_cancellation(
                                                status,
                                                finished,
                                                completed,
                                            )
                                        }
                                    )
                            )
                    });
                let synchronous_cancel_retry_rollback = context.owner
                    == PhysicalResumeOwner::SynchronousCancellation
                    && track.producer == Some(PhysicalWaitProducer::SynchronousWait)
                    && reservations.values().any(|reservation| {
                        reservation.status == Some(status)
                            && matches!(
                                reservation.decode_finished,
                                Some((
                                    PhysicalDecodeOwner::Synchronous,
                                    PhysicalDecodeOutcome::Cancelled,
                                    finished
                                )) if finished < attempt_sequence
                            )
                            && matches!(
                                reservation.completion,
                                Some((ReservationCompletion::RolledBack, rolled_back))
                                    if worker_start_sequences.iter().all(
                                        |(generation, sequence)| {
                                            canonical_generation(
                                                *generation,
                                                &adoptions,
                                                &invalid_adoptions,
                                            ) != track.generation
                                                || *sequence >= rolled_back
                                        },
                                    ) && resume_result_sequences.get(attempt).is_some_and(
                                        |result| {
                                            attempt_sequence < *result
                                                && *result < rolled_back
                                                && !tolerated.contains_key(attempt)
                                        }
                                    )
                            )
                    });
                let owner_and_delivery = match track.publication_destination {
                    Some(
                        PhysicalStatusPublication::CleanupStopped
                        | PhysicalStatusPublication::PreStopDrainFailureCleanup
                        | PhysicalStatusPublication::StartupBarrierFailureCleanup
                        | PhysicalStatusPublication::StartupBarrierCleanupStopped,
                    ) => {
                        cleanup_status_transactions
                                .get(&status)
                                .is_some_and(|transaction| {
                                    valid_cleanup_transactions.contains(transaction)
                                        && cleanup_transactions.get(transaction).is_some_and(
                                            |cleanup| match cleanup.kind {
                                                Some(
                                                    PhysicalCleanupTransactionKind::Registered,
                                                ) => context
                                                    .owner
                                                    .is_registered_controller_cleanup(),
                                                Some(
                                                    PhysicalCleanupTransactionKind::StartupBarrier {
                                                        generation,
                                                        task,
                                                        owner,
                                                        ..
                                                    },
                                                ) => match owner {
                                                    PhysicalStartupCleanupOwner::Unstarted => context
                                                        .owner
                                                        .is_startup_barrier_cleanup(),
                                                    PhysicalStartupCleanupOwner::AuthorizedWorker => {
                                                        context.owner
                                                            == PhysicalResumeOwner::AuthorizedRootExternalCleanup
                                                            && startup_executor_transfers
                                                                .get(transaction)
                                                                .is_some_and(
                                                                    |(
                                                                        transfer_generation,
                                                                        transfer_task,
                                                                        transfer_sequence,
                                                                    )| {
                                                                        *transfer_generation
                                                                            == generation
                                                                            && task.authorizes(
                                                                                *transfer_task,
                                                                                false,
                                                                            )
                                                                            && transfer_task
                                                                                .authorizes(
                                                                                    context.task,
                                                                                    false,
                                                                                )
                                                                            && *transfer_sequence
                                                                                < attempt_sequence
                                                                    },
                                                                )
                                                    }
                                                },
                                                Some(
                                                    PhysicalCleanupTransactionKind::StartupSetup {
                                                        ..
                                                    },
                                                ) => context
                                                    .owner
                                                    .is_startup_barrier_cleanup(),
                                                None => false,
                                            },
                                        )
                                })
                    }
                    Some(PhysicalStatusPublication::DirectStopped) => matches!(
                        context.owner,
                        PhysicalResumeOwner::TypedStopped
                            | PhysicalResumeOwner::PreRegistrationCleanup
                    ),
                    Some(
                        PhysicalStatusPublication::RegularFifo
                        | PhysicalStatusPublication::SynchronousFifo,
                    ) => {
                        (context.owner == PhysicalResumeOwner::TypedStopped
                            && one_consuming_delivery)
                            || synchronous_cancel_delivery
                            || synchronous_cancel_retry_rollback
                            || (context.owner.is_registered_controller_cleanup()
                                && context.owner != PhysicalResumeOwner::SynchronousCancellation
                                && cleanup_status_transactions.get(&status).is_some_and(
                                    |transaction| valid_cleanup_transactions.contains(transaction),
                                ))
                    }
                    Some(PhysicalStatusPublication::ExitCapability) => {
                        (context.owner == PhysicalResumeOwner::TypedStopped
                            && claimed_exit_capability)
                            || (context.owner.is_registered_controller_cleanup()
                                && cleanup_exit_capability)
                    }
                    Some(
                        PhysicalStatusPublication::RetainedTerminal
                        | PhysicalStatusPublication::ContinuedSideChannel { .. }
                        | PhysicalStatusPublication::ExternalCleanup
                        | PhysicalStatusPublication::CleanupTerminal
                        | PhysicalStatusPublication::StartupBarrierCleanupTerminal,
                    )
                    | None => false,
                };
                published && stopped && owner_and_delivery
            }
        });
        if !valid_source {
            violations.push(PhysicalPartitionViolation::InvalidResumeSourceStatus(
                *attempt,
            ));
        }
    }
    for (generation, wait, sequence) in registered_cleanup_without_worker {
        let valid_synchronous_cleanup = cleanup_transactions.iter().any(|(transaction, track)| {
            valid_cleanup_transactions.contains(transaction)
                && track.start_sequence.is_some_and(|start| start < sequence)
                && track
                    .completion_sequence
                    .is_some_and(|completion| sequence < completion)
                && track.terminal_wait == Some(wait)
                && track.cause_wait.is_some_and(|cause| {
                    wait_attempts.get(&cause).is_some_and(|context| {
                        (match (track.kind, context.producer) {
                            (
                                Some(PhysicalCleanupTransactionKind::Registered),
                                PhysicalWaitProducer::SynchronousWait,
                            ) => true,
                            (
                                Some(PhysicalCleanupTransactionKind::StartupBarrier {
                                    generation: transaction_generation,
                                    owner: PhysicalStartupCleanupOwner::Unstarted,
                                    ..
                                }),
                                PhysicalWaitProducer::PreRegistrationBarrierCleanup,
                            ) => transaction_generation == generation,
                            (
                                Some(PhysicalCleanupTransactionKind::StartupSetup {
                                    generation: transaction_generation,
                                    ..
                                }),
                                PhysicalWaitProducer::PreRegistrationBarrierCleanup,
                            ) => transaction_generation == generation,
                            _ => false,
                        }) && wait_generations.get(&cause) == Some(&generation)
                    })
                })
                && wait_generations.get(&wait) == Some(&generation)
        });
        if !valid_synchronous_cleanup {
            invalid_generation_lifecycles.insert(generation);
        }
    }
    let mut generation_activity_records = BTreeMap::<PhysicalEventGenerationId, Vec<u64>>::new();
    for record in &snapshot.records {
        let mut record_generations = BTreeSet::new();
        let mut add_generation = |generation| {
            if let Some(canonical) =
                canonical_generation(generation, &adoptions, &invalid_adoptions)
            {
                record_generations.insert(canonical);
            }
        };
        match record.kind {
            PhysicalEventRecordKind::GenerationAttached(generation)
            | PhysicalEventRecordKind::OriginalRootLaunchLinked { generation, .. }
            | PhysicalEventRecordKind::StartupBarrierFallbackPrepared { generation, .. }
            | PhysicalEventRecordKind::StartupBarrierFallbackReleased { generation, .. }
            | PhysicalEventRecordKind::ContinuedAuthorityEnabled { generation, .. }
            | PhysicalEventRecordKind::ContinuedAuthorityRevoked { generation }
            | PhysicalEventRecordKind::PreRegistrationBarrierConsumed { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationBarrierFailureLinked { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationBarrierStatuslessFailureLinked {
                generation,
                ..
            }
            | PhysicalEventRecordKind::PreRegistrationBarrierStatuslessCleanupResolved {
                generation,
                ..
            }
            | PhysicalEventRecordKind::PreRegistrationBarrierSetupFailed { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupPrepared { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupLinked { generation, .. }
            | PhysicalEventRecordKind::StartupSetupCleanupNoStatusLinked { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupPidfdExitProved { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupWaitFailureExitProved { generation, .. }
            | PhysicalEventRecordKind::StartupCleanupResumeFailureExitProved {
                generation, ..
            }
            | PhysicalEventRecordKind::StartupCleanupExecutorTransferred { generation, .. }
            | PhysicalEventRecordKind::StopResolutionWatchArmed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionFirstStopped { generation, .. }
            | PhysicalEventRecordKind::StopResolutionGroupAcknowledged { generation, .. }
            | PhysicalEventRecordKind::StopResolutionContinuedClaimed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionWatchClosed { generation, .. }
            | PhysicalEventRecordKind::StopResolutionResumeCausallyClosed { generation, .. }
            | PhysicalEventRecordKind::PreStopContinuedDrainCompleted { generation, .. }
            | PhysicalEventRecordKind::PreStopContinuedDrainFailed { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationLinked { generation, .. }
            | PhysicalEventRecordKind::IdentityBound { generation, .. }
            | PhysicalEventRecordKind::GenerationCaptureFailed { generation, .. }
            | PhysicalEventRecordKind::GenerationIdentityMismatch { generation, .. }
            | PhysicalEventRecordKind::GenerationBoundPidfdDead { generation, .. }
            | PhysicalEventRecordKind::GenerationCurrentPidfdDead { generation, .. }
            | PhysicalEventRecordKind::GenerationRegistryMismatch { generation, .. }
            | PhysicalEventRecordKind::PreRegistrationTaskGone { generation, .. }
            | PhysicalEventRecordKind::RegisteredCleanupPidfdExited { generation, .. }
            | PhysicalEventRecordKind::EchildPidfdExited { generation, .. }
            | PhysicalEventRecordKind::EchildTracerDetached { generation, .. }
            | PhysicalEventRecordKind::NotifierWorkerStarted(generation)
            | PhysicalEventRecordKind::ExecGenerationBound { generation, .. }
            | PhysicalEventRecordKind::StatusReserved { generation, .. } => {
                add_generation(generation);
            }
            PhysicalEventRecordKind::GenerationAdopted { from, to } => {
                add_generation(from);
                add_generation(to);
            }
            PhysicalEventRecordKind::NotifierGenerationFinished { .. }
            | PhysicalEventRecordKind::SyntheticEchildPublished { cause: None, .. }
            | PhysicalEventRecordKind::ObserverClosed => {}
            PhysicalEventRecordKind::SyntheticEchildPublished { generation, .. } => {
                add_generation(generation);
            }
            PhysicalEventRecordKind::WaitAttempt { id, .. } => {
                record_generations.extend(wait_generations.get(&id).copied());
            }
            PhysicalEventRecordKind::WaitSiginfoReturned { attempt, .. }
            | PhysicalEventRecordKind::WaitResult { attempt, .. } => {
                record_generations.extend(wait_generations.get(&attempt).copied());
            }
            PhysicalEventRecordKind::RegisteredCleanupTransactionStarted { cause_wait, .. } => {
                record_generations.extend(wait_generations.get(&cause_wait).copied());
            }
            PhysicalEventRecordKind::RegisteredCleanupStatusLinked { status, .. }
            | PhysicalEventRecordKind::StatusPublished { status, .. }
            | PhysicalEventRecordKind::StatusDisposition { status, .. }
            | PhysicalEventRecordKind::ExitCapability { status, .. } => {
                record_generations.extend(statuses.get(&status).and_then(|track| track.generation));
            }
            PhysicalEventRecordKind::RegisteredCleanupToleratedResumeLinked { resume, .. } => {
                record_generations.extend(resume_generations.get(&resume).copied());
            }
            PhysicalEventRecordKind::RegisteredCleanupTransactionCompleted {
                terminal_wait,
                ..
            } => {
                record_generations.extend(wait_generations.get(&terminal_wait).copied());
            }
            PhysicalEventRecordKind::DecodeStarted { reservation, .. }
            | PhysicalEventRecordKind::DecodeFinished { reservation, .. }
            | PhysicalEventRecordKind::ReservationCommitted { reservation, .. }
            | PhysicalEventRecordKind::ReservationRolledBack { reservation, .. }
            | PhysicalEventRecordKind::TerminalReplayed { reservation, .. } => {
                record_generations.extend(reservation_generations.get(&reservation).copied());
            }
            PhysicalEventRecordKind::ResumeAttempt { id, .. } => {
                record_generations.extend(resume_generations.get(&id).copied());
            }
            PhysicalEventRecordKind::ResumeResult { attempt, .. }
            | PhysicalEventRecordKind::ResumeErrorTolerated { attempt, .. } => {
                record_generations.extend(resume_generations.get(&attempt).copied());
            }
            PhysicalEventRecordKind::PidfdSignalAttempt { context, .. } => {
                add_generation(context.generation);
            }
            PhysicalEventRecordKind::PidfdSignalResult { attempt, .. } => {
                record_generations.extend(
                    pidfd_signal_attempts
                        .get(&attempt)
                        .map(|context| context.generation),
                );
            }
        }
        for generation in record_generations {
            generation_activity_records
                .entry(generation)
                .or_default()
                .push(record.sequence);
        }
    }
    let mut canonical_worker_starts = BTreeMap::new();
    for (generation, sequence) in &worker_start_sequences {
        if let Some(canonical) = canonical_generation(*generation, &adoptions, &invalid_adoptions)
            && canonical_worker_starts
                .insert(canonical, *sequence)
                .is_some()
        {
            invalid_generation_lifecycles.insert(*generation);
        }
    }
    for (generation, publications) in &uncaused_synthetic_echild {
        if publications.len() != 1
            || !valid_generation_invalidations
                .get(generation)
                .is_some_and(|invalidation| *invalidation < publications[0])
        {
            invalid_generation_lifecycles.insert(*generation);
        }
    }
    let mut allowed_finish_replay_sequences =
        BTreeMap::<(PhysicalEventGenerationId, u64), BTreeSet<u64>>::new();
    for &(generation, external_wait, finish_sequence) in &generation_finish_records {
        let Some(canonical) = canonical_generation(generation, &adoptions, &invalid_adoptions)
        else {
            invalid_generation_lifecycles.insert(generation);
            continue;
        };
        let external_boundary = external_wait.and_then(|attempt| {
            if wait_generations.get(&attempt) != Some(&canonical)
                || !wait_attempts.get(&attempt).is_some_and(|context| {
                    matches!(
                        context.producer,
                        PhysicalWaitProducer::PreRegistrationCleanup
                            | PhysicalWaitProducer::PreRegistrationBarrierCleanup
                    )
                })
            {
                return None;
            }
            let result = wait_result_sequences.get(&attempt).copied()?;
            if result >= finish_sequence {
                return None;
            }
            match wait_outcomes.get(&attempt) {
                Some(PhysicalWaitOutcome::NoChild) => {
                    let startup_completion = cleanup_terminal_wait_transactions
                        .get(&attempt)
                        .filter(|transaction| valid_cleanup_transactions.contains(transaction))
                        .and_then(|transaction| cleanup_transactions.get(transaction))
                        .and_then(|cleanup| {
                            matches!(
                                cleanup.kind,
                                Some(PhysicalCleanupTransactionKind::StartupBarrier {
                                    owner: PhysicalStartupCleanupOwner::Unstarted,
                                    ..
                                }) | Some(PhysicalCleanupTransactionKind::StartupSetup { .. })
                            )
                            .then_some(cleanup.completion_sequence)
                            .flatten()
                        })
                        .filter(|completed| *completed < finish_sequence);
                    startup_completion.or_else(|| {
                        wait_attempts.get(&attempt).and_then(|context| {
                            valid_pre_registration_task_gone
                                .get(&canonical)
                                .and_then(|evidence| {
                                    evidence
                                        .iter()
                                        .filter_map(|(task, sequence)| {
                                            (context.task.authorizes(*task, true)
                                                && result < *sequence
                                                && *sequence < finish_sequence)
                                                .then_some(*sequence)
                                        })
                                        .min()
                                })
                        })
                    })
                }
                Some(PhysicalWaitOutcome::Status { id, raw_status, .. })
                    if is_terminal_raw_status(*raw_status) =>
                {
                    statuses.get(id).and_then(|track| {
                        let published = track.publication_sequence?;
                        let disposition = track.last_disposition_sequence?;
                        if track.publication_destination
                            == Some(PhysicalStatusPublication::ExternalCleanup)
                        {
                            (result < published
                                && published < disposition
                                && disposition < finish_sequence)
                                .then_some(disposition)
                        } else if track.publication_destination
                            == Some(PhysicalStatusPublication::StartupBarrierCleanupTerminal)
                        {
                            cleanup_terminal_wait_transactions
                                .get(&attempt)
                                .filter(|transaction| {
                                    valid_cleanup_transactions.contains(transaction)
                                })
                                .and_then(|transaction| cleanup_transactions.get(transaction))
                                .and_then(|cleanup| cleanup.completion_sequence)
                                .filter(|completed| {
                                    result < published
                                        && published < disposition
                                        && disposition < *completed
                                        && *completed < finish_sequence
                                })
                        } else {
                            None
                        }
                    })
                }
                Some(PhysicalWaitOutcome::UndecodableStatus { id, siginfo, .. })
                    if siginfo_is_startup_typed_unsupported_terminal(*siginfo) =>
                {
                    statuses.get(id).and_then(|track| {
                        let published = track.publication_sequence?;
                        let disposition = track.last_disposition_sequence?;
                        if track.publication_destination
                            != Some(PhysicalStatusPublication::StartupBarrierCleanupTerminal)
                        {
                            return None;
                        }
                        cleanup_terminal_wait_transactions
                            .get(&attempt)
                            .filter(|transaction| valid_cleanup_transactions.contains(transaction))
                            .and_then(|transaction| cleanup_transactions.get(transaction))
                            .and_then(|cleanup| cleanup.completion_sequence)
                            .filter(|completed| {
                                result < published
                                    && published < disposition
                                    && disposition < *completed
                                    && *completed < finish_sequence
                            })
                    })
                }
                _ => None,
            }
        });
        let terminal_boundary = wait_outcomes
            .iter()
            .filter_map(|(attempt, outcome)| {
                if wait_generations.get(attempt) != Some(&canonical)
                    || !wait_attempts.get(attempt).is_some_and(|context| {
                        matches!(
                            context.producer,
                            PhysicalWaitProducer::NotifierWorker
                                | PhysicalWaitProducer::AuthorizedRootNotifier
                                | PhysicalWaitProducer::SynchronousWait
                        )
                    })
                {
                    return None;
                }
                let result = wait_result_sequences.get(attempt).copied()?;
                match outcome {
                    PhysicalWaitOutcome::Status { id, raw_status, .. }
                        if is_terminal_raw_status(*raw_status) =>
                    {
                        statuses.get(id).and_then(|track| {
                            (track.publication_destination
                                == Some(PhysicalStatusPublication::RetainedTerminal))
                            .then_some(track.publication_sequence)
                            .flatten()
                            .filter(|published| result < *published && *published < finish_sequence)
                        })
                    }
                    PhysicalWaitOutcome::NoChild => synthetic_echild_evidence
                        .get(attempt)
                        .and_then(|(evidence_generation, sequence)| {
                            (*evidence_generation == canonical
                                && result < *sequence
                                && *sequence < finish_sequence)
                                .then_some(*sequence)
                        }),
                    _ => None,
                }
            })
            .min();
        let cleanup_boundary = cleanup_transactions
            .iter()
            .filter_map(|(transaction, track)| {
                (valid_cleanup_transactions.contains(transaction)
                    && track.terminal_wait.is_some_and(|terminal_wait| {
                        wait_generations.get(&terminal_wait) == Some(&canonical)
                    }))
                .then_some(track.completion_sequence)
                .flatten()
                .filter(|sequence| *sequence < finish_sequence)
            })
            .min();
        let invalidation_boundary = valid_generation_invalidations
            .get(&canonical)
            .copied()
            .filter(|sequence| *sequence < finish_sequence);
        let evidence_boundary = if external_wait.is_some() {
            external_boundary
        } else {
            [terminal_boundary, cleanup_boundary, invalidation_boundary]
                .into_iter()
                .flatten()
                .min()
        };
        let evidence_is_after_worker_start = evidence_boundary.is_some_and(|evidence| {
            canonical_worker_starts
                .get(&canonical)
                .is_none_or(|started| *started < evidence)
        });
        let mut allowed_replay_sequences = BTreeSet::new();
        if let Some(evidence) = evidence_boundary {
            for (reservation, track) in &reservations {
                if reservation_generations.get(reservation) != Some(&canonical) {
                    continue;
                }
                let Some(status) = track.status else {
                    continue;
                };
                let retained_terminal = statuses.get(&status).is_some_and(|status_track| {
                    status_track.publication_destination
                        == Some(PhysicalStatusPublication::RetainedTerminal)
                        && status_track
                            .publication_sequence
                            .is_some_and(|published| published <= evidence)
                });
                let exact_replay = retained_terminal
                    && track
                        .reserved_sequence
                        .is_some_and(|sequence| sequence > evidence)
                    && matches!(
                        track.decode_started,
                        Some((
                            PhysicalDecodeOwner::Notifier | PhysicalDecodeOwner::Synchronous,
                            _
                        ))
                    )
                    && matches!(
                        track.decode_finished,
                        Some((
                            PhysicalDecodeOwner::Notifier | PhysicalDecodeOwner::Synchronous,
                            PhysicalDecodeOutcome::Returned,
                            _
                        ))
                    )
                    && matches!(
                        track.completion,
                        Some((ReservationCompletion::TerminalReplayed, _))
                    );
                if exact_replay {
                    allowed_replay_sequences.extend(track.reserved_sequence);
                    allowed_replay_sequences
                        .extend(track.decode_started.map(|(_, sequence)| sequence));
                    allowed_replay_sequences
                        .extend(track.decode_finished.map(|(_, _, sequence)| sequence));
                    allowed_replay_sequences.extend(track.completion.map(|(_, sequence)| sequence));
                }
            }
            if terminal_boundary == Some(evidence) || cleanup_boundary == Some(evidence) {
                if let Some(authority) = continued_authorities.get(&canonical)
                    && authority.enabled_sequence < evidence
                    && authority
                        .revoked_sequence
                        .is_some_and(|revoked| evidence < revoked && revoked < finish_sequence)
                {
                    allowed_replay_sequences.extend(authority.revoked_sequence);
                }
                for (status, status_track) in &statuses {
                    let Some(disposition) = status_track.cancellation_cleanup_sequence else {
                        continue;
                    };
                    let consumed_before_terminal = reservations
                        .values()
                        .filter(|reservation| {
                            reservation.status == Some(*status)
                                && matches!(
                                    reservation.decode_finished,
                                    Some((_, PhysicalDecodeOutcome::Returned, _))
                                )
                                && matches!(
                                    reservation.completion,
                                    Some((ReservationCompletion::Committed, completed))
                                        if completed < evidence
                                )
                        })
                        .count()
                        == 1;
                    let exact_deferred_cancellation = status_track.generation == Some(canonical)
                        && matches!(
                            status_track.producer,
                            Some(
                                PhysicalWaitProducer::NotifierWorker
                                    | PhysicalWaitProducer::AuthorizedRootNotifier
                                    | PhysicalWaitProducer::SynchronousWait
                            )
                        )
                        && status_track.created_sequence < evidence
                        && evidence < disposition
                        && consumed_before_terminal;
                    if exact_deferred_cancellation {
                        allowed_replay_sequences.insert(disposition);
                    }
                }
                for (source, attempt, proof, resolution_sequence) in &ambiguous_resume_resolutions {
                    if !valid_ambiguous_resolution_attempts.contains(attempt)
                        || !valid_ambiguous_resolution_sources.contains(source)
                        || statuses
                            .get(source)
                            .and_then(|track| track.generation)
                            .and_then(|generation| {
                                canonical_generation(generation, &adoptions, &invalid_adoptions)
                            })
                            != Some(canonical)
                    {
                        continue;
                    }
                    let exact_terminal_proof = match proof {
                        PhysicalAmbiguousResumeProof::FinalStatus(proof_status) => {
                            statuses.get(proof_status).is_some_and(|proof_track| {
                                proof_track.publication_destination
                                    == Some(PhysicalStatusPublication::RetainedTerminal)
                                    && proof_track.publication_sequence == Some(evidence)
                                    && proof_track.generation.and_then(|generation| {
                                        canonical_generation(
                                            generation,
                                            &adoptions,
                                            &invalid_adoptions,
                                        )
                                    }) == Some(canonical)
                            })
                        }
                        PhysicalAmbiguousResumeProof::ProvenEchild(wait) => {
                            synthetic_echild_evidence.get(wait).is_some_and(
                                |(proof_generation, proof_sequence)| {
                                    *proof_sequence == evidence
                                        && canonical_generation(
                                            *proof_generation,
                                            &adoptions,
                                            &invalid_adoptions,
                                        ) == Some(canonical)
                                },
                            )
                        }
                        PhysicalAmbiguousResumeProof::LaterStatus(_) => false,
                    };
                    if exact_terminal_proof
                        && evidence < *resolution_sequence
                        && *resolution_sequence < finish_sequence
                    {
                        allowed_replay_sequences.insert(*resolution_sequence);
                    }
                }
            }
        }
        let no_activity_after_evidence = evidence_boundary.is_some_and(|evidence| {
            generation_activity_records
                .get(&canonical)
                .is_none_or(|activity| {
                    activity.iter().all(|sequence| {
                        *sequence <= evidence
                            || *sequence >= finish_sequence
                            || allowed_replay_sequences.contains(sequence)
                    })
                })
        });
        allowed_finish_replay_sequences
            .insert((canonical, finish_sequence), allowed_replay_sequences);
        if !evidence_is_after_worker_start || !no_activity_after_evidence {
            invalid_generation_lifecycles.insert(generation);
        }
    }
    for &(generation, _, finish_sequence) in &generation_finish_records {
        let Some(canonical) = canonical_generation(generation, &adoptions, &invalid_adoptions)
        else {
            continue;
        };
        let allowed_late_replay_sequences = allowed_finish_replay_sequences
            .get(&(canonical, finish_sequence))
            .cloned()
            .unwrap_or_default();
        if generation_activity_records
            .get(&canonical)
            .is_some_and(|activity| {
                activity.iter().any(|sequence| {
                    *sequence > finish_sequence && !allowed_late_replay_sequences.contains(sequence)
                })
            })
        {
            invalid_generation_lifecycles.insert(generation);
        }
    }
    for (attempt, context) in &wait_attempts {
        if !matches!(
            context.producer,
            PhysicalWaitProducer::SynchronousWait | PhysicalWaitProducer::PreRegistrationCleanup
        ) {
            continue;
        }
        let Some(generation) = wait_generations.get(attempt).copied() else {
            continue;
        };
        let worker_start = worker_start_sequences
            .iter()
            .filter_map(|(raw_generation, sequence)| {
                (canonical_generation(*raw_generation, &adoptions, &invalid_adoptions)
                    == Some(generation))
                .then_some(*sequence)
            })
            .min();
        let Some(worker_start) = worker_start else {
            continue;
        };
        if context.producer == PhysicalWaitProducer::PreRegistrationCleanup {
            // `drain_unregistered_child` owns the task until it observes an
            // exact terminal status or ECHILD and closes the generation.  It
            // never hands a successfully resumed, still-live task to a later
            // notifier worker, even when this wait/result pair itself
            // completed before the worker-start record.
            invalid_generation_lifecycles.insert(generation);
            continue;
        }
        let result_before_start = wait_attempt_sequences
            .get(attempt)
            .zip(wait_result_sequences.get(attempt))
            .is_some_and(|(attempted, result)| *attempted < *result && *result < worker_start);
        let status_completed_before_start = match wait_outcomes.get(attempt) {
            Some(PhysicalWaitOutcome::Status { id, .. }) => {
                let published_before_start = statuses.get(id).is_some_and(|track| {
                    track
                        .publication_sequence
                        .is_some_and(|published| published < worker_start)
                });
                published_before_start
                    && reservations
                        .values()
                        .filter(|reservation| {
                            reservation.status == Some(*id)
                                && matches!(
                                    reservation.decode_finished,
                                    Some((
                                        PhysicalDecodeOwner::Synchronous,
                                        PhysicalDecodeOutcome::Returned
                                            | PhysicalDecodeOutcome::RetryRolledBack,
                                        _
                                    ))
                                )
                                && match (reservation.decode_finished, reservation.completion) {
                                    (
                                        Some((
                                            PhysicalDecodeOwner::Synchronous,
                                            PhysicalDecodeOutcome::Returned,
                                            _,
                                        )),
                                        Some((ReservationCompletion::Committed, completed)),
                                    )
                                    | (
                                        Some((
                                            PhysicalDecodeOwner::Synchronous,
                                            PhysicalDecodeOutcome::RetryRolledBack,
                                            _,
                                        )),
                                        Some((ReservationCompletion::RolledBack, completed)),
                                    ) => completed < worker_start,
                                    _ => false,
                                }
                        })
                        .count()
                        == 1
            }
            Some(_) => true,
            None => false,
        };
        if !result_before_start || !status_completed_before_start {
            invalid_generation_lifecycles.insert(generation);
        }
    }
    for (attempt, context) in &resume_attempts {
        let Some(status) = context.source_status else {
            continue;
        };
        let Some(status_track) = statuses.get(&status) else {
            continue;
        };
        if context.owner != PhysicalResumeOwner::PreRegistrationCleanup
            && status_track.producer != Some(PhysicalWaitProducer::PreRegistrationCleanup)
        {
            continue;
        }
        let Some(generation) = resume_generations
            .get(attempt)
            .copied()
            .or(status_track.generation)
        else {
            continue;
        };
        if worker_start_sequences.iter().any(|(raw_generation, _)| {
            canonical_generation(*raw_generation, &adoptions, &invalid_adoptions)
                == Some(generation)
        }) {
            invalid_generation_lifecycles.insert(generation);
        }
    }
    for generation in invalid_generation_lifecycles {
        violations.push(PhysicalPartitionViolation::InvalidGenerationLifecycle(
            generation,
        ));
    }

    let has_valid_status_transaction = |status: PhysicalStatusId| {
        cleanup_status_transactions
            .get(&status)
            .is_some_and(|transaction| valid_cleanup_transactions.contains(transaction))
    };
    let has_valid_resume_transaction = |resume: PhysicalResumeAttemptId| {
        cleanup_resume_transactions
            .get(&resume)
            .is_some_and(|transaction| valid_cleanup_transactions.contains(transaction))
    };
    let has_future_external_terminal_evidence =
        |generation: PhysicalEventGenerationId, task: PhysicalTaskIdentity, after: u64| {
            external_generation_finishes.iter().any(
                |(finished_generation, terminal_wait, finish_sequence)| {
                    if canonical_generation(*finished_generation, &adoptions, &invalid_adoptions)
                        != Some(generation)
                        || wait_generations.get(terminal_wait) != Some(&generation)
                        || !wait_attempts.get(terminal_wait).is_some_and(|context| {
                            context.producer == PhysicalWaitProducer::PreRegistrationCleanup
                                && task.authorizes(context.task, true)
                        })
                    {
                        return false;
                    }
                    let Some(result) = wait_result_sequences.get(terminal_wait).copied() else {
                        return false;
                    };
                    let boundary = match wait_outcomes.get(terminal_wait) {
                        Some(PhysicalWaitOutcome::NoChild) => valid_pre_registration_task_gone
                            .get(&generation)
                            .and_then(|evidence| {
                                evidence
                                    .iter()
                                    .filter_map(|(observed_task, sequence)| {
                                        (task.authorizes(*observed_task, true)
                                            && result < *sequence
                                            && *sequence < *finish_sequence)
                                            .then_some(*sequence)
                                    })
                                    .min()
                            }),
                        Some(PhysicalWaitOutcome::Status { id, raw_status, .. })
                            if is_terminal_raw_status(*raw_status) =>
                        {
                            statuses
                                .get(id)
                                .and_then(|track| track.last_disposition_sequence)
                                .filter(|sequence| {
                                    result < *sequence && *sequence < *finish_sequence
                                })
                        }
                        _ => None,
                    };
                    boundary.is_some_and(|boundary| after < boundary)
                },
            )
        };
    let has_prior_terminal_evidence = |generation: PhysicalEventGenerationId,
                                       task: PhysicalTaskIdentity,
                                       created: u64,
                                       before: u64| {
        wait_outcomes.iter().any(|(attempt, outcome)| {
            let same_boundary = wait_generations.get(attempt) == Some(&generation)
                && wait_attempts.get(attempt).is_some_and(|context| {
                    matches!(
                        context.producer,
                        PhysicalWaitProducer::NotifierWorker
                            | PhysicalWaitProducer::AuthorizedRootNotifier
                            | PhysicalWaitProducer::SynchronousWait
                    ) && task.authorizes(context.task, false)
                })
                && wait_result_sequences
                    .get(attempt)
                    .is_some_and(|sequence| created < *sequence && *sequence < before);
            let terminal = matches!(
                outcome,
                PhysicalWaitOutcome::Status { raw_status, .. }
                    if is_terminal_raw_status(*raw_status)
            ) || matches!(outcome, PhysicalWaitOutcome::NoChild)
                && synthetic_echild_evidence.get(attempt).is_some_and(
                    |(evidence_generation, sequence)| {
                        *evidence_generation == generation && *sequence < before
                    },
                );
            same_boundary && terminal
        })
    };

    for (attempt, errno) in &tolerated {
        let valid_error = matches!(*errno, libc::ESRCH | libc::EIO);
        let registered_controller = resume_attempts
            .get(attempt)
            .is_some_and(|context| context.owner.is_registered_controller_cleanup());
        let synchronous_cancellation_commit = resume_attempts.get(attempt).is_some_and(|context| {
            context.owner == PhysicalResumeOwner::SynchronousCancellation
                && context.source_status.is_some_and(|status| {
                    reservations.values().any(|reservation| {
                        reservation.status == Some(status)
                            && matches!(
                                reservation.decode_finished,
                                Some((
                                    PhysicalDecodeOwner::Synchronous,
                                    PhysicalDecodeOutcome::Cancelled,
                                    finished
                                )) if resume_attempt_sequences
                                    .get(attempt)
                                    .is_some_and(|sequence| finished < *sequence)
                            )
                            && matches!(
                                reservation.completion,
                                Some((ReservationCompletion::Committed, completed))
                                    if resume_attempt_sequences
                                        .get(attempt)
                                        .is_some_and(|sequence| *sequence < completed)
                                        && reservation.decode_finished.is_some_and(
                                        |(_, _, finished)| {
                                            valid_synchronous_cancellation(
                                                status,
                                                finished,
                                                completed,
                                            )
                                        }
                                    )
                            )
                    })
                })
        });
        let valid_cause = resume_attempts.get(attempt).is_some_and(|context| {
            if context.owner.is_registered_controller_cleanup() {
                has_valid_resume_transaction(*attempt) || synchronous_cancellation_commit
            } else if context.owner == PhysicalResumeOwner::TypedStopped {
                false
            } else if context.owner == PhysicalResumeOwner::PreRegistrationCleanup {
                let Some(generation) = resume_generations.get(attempt).copied() else {
                    return false;
                };
                context.source_status.is_some_and(|status| {
                    statuses.get(&status).is_some_and(|status_track| {
                        resume_result_sequences.get(attempt).is_some_and(|result| {
                            tolerated_sequences.get(attempt).is_some_and(|tolerated| {
                                status_track.cancellation_cleanup_sequence.is_some_and(
                                    |disposition| {
                                        result < tolerated
                                            && tolerated < &disposition
                                            && has_future_external_terminal_evidence(
                                                generation,
                                                context.task,
                                                disposition,
                                            )
                                    },
                                )
                            })
                        })
                    })
                })
            } else {
                false
            }
        });
        if !valid_error || !valid_cause {
            violations.push(PhysicalPartitionViolation::InvalidToleratedResumeError(
                *attempt,
            ));
        }
        if registered_controller
            && !has_valid_resume_transaction(*attempt)
            && !synchronous_cancellation_commit
        {
            violations
                .push(PhysicalPartitionViolation::MissingRegisteredCleanupResumeEvidence(*attempt));
        }
    }
    for generation in live_workers {
        violations.push(PhysicalPartitionViolation::LiveNotifierWorker(generation));
    }
    for generation in live_generations {
        violations.push(PhysicalPartitionViolation::LiveEventGeneration(generation));
    }
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum ExitCapabilityState {
        Initial,
        Available,
        Claimed,
        Revoked,
        Expired,
        Transferred,
    }
    let mut valid_exit_capabilities = BTreeSet::new();
    for (status, capability) in &exit_capabilities {
        let published_count = capability
            .transitions
            .iter()
            .filter(|(_, transition)| *transition == PhysicalExitCapabilityTransition::Published)
            .count();
        let claimed_count = capability
            .transitions
            .iter()
            .filter(|(_, transition)| *transition == PhysicalExitCapabilityTransition::Claimed)
            .count();
        if published_count > 1 {
            violations
                .push(PhysicalPartitionViolation::DuplicateExitCapabilityPublication(*status));
        }
        if claimed_count > 1 {
            violations.push(PhysicalPartitionViolation::DuplicateExitCapabilityClaim(
                *status,
            ));
        }
        let Some(status_track) = statuses.get(status) else {
            violations.push(PhysicalPartitionViolation::InvalidExitCapabilityFinalization(*status));
            continue;
        };
        let Some(publication_sequence) = status_track.publication_sequence else {
            violations.push(PhysicalPartitionViolation::InvalidExitCapabilityFinalization(*status));
            continue;
        };
        let mut state = ExitCapabilityState::Initial;
        let mut last_transition_sequence = publication_sequence;
        let mut transitions_valid =
            status_track.publication_destination == Some(PhysicalStatusPublication::ExitCapability);
        for (sequence, transition) in &capability.transitions {
            transitions_valid &= *sequence > publication_sequence;
            let next = match (state, transition) {
                (ExitCapabilityState::Initial, PhysicalExitCapabilityTransition::Published) => {
                    Some(ExitCapabilityState::Available)
                }
                (
                    ExitCapabilityState::Initial | ExitCapabilityState::Available,
                    PhysicalExitCapabilityTransition::Revoked,
                ) => Some(ExitCapabilityState::Revoked),
                (ExitCapabilityState::Available, PhysicalExitCapabilityTransition::Claimed) => {
                    Some(ExitCapabilityState::Claimed)
                }
                (ExitCapabilityState::Available, PhysicalExitCapabilityTransition::Expired) => {
                    Some(ExitCapabilityState::Expired)
                }
                (
                    ExitCapabilityState::Claimed,
                    PhysicalExitCapabilityTransition::TransferredToCleanup,
                ) => Some(ExitCapabilityState::Transferred),
                _ => None,
            };
            if let Some(next) = next {
                state = next;
                last_transition_sequence = *sequence;
            } else {
                transitions_valid = false;
            }
        }
        if !transitions_valid {
            violations.push(PhysicalPartitionViolation::InvalidExitCapabilityTransition(
                *status,
            ));
        }
        let final_count = status_track.dispositions + status_track.successful_resumes;
        let cleanup_resume_sequence = status_track
            .successful_resume_owner
            .is_some_and(PhysicalResumeOwner::is_registered_controller_cleanup)
            .then_some(status_track.successful_resume_sequence)
            .flatten();
        let final_valid = match state {
            ExitCapabilityState::Claimed => {
                status_track.successful_resumes == 1
                    && status_track.successful_resume_owner
                        == Some(PhysicalResumeOwner::TypedStopped)
                    && status_track
                        .successful_resume_sequence
                        .is_some_and(|sequence| sequence > last_transition_sequence)
            }
            ExitCapabilityState::Revoked | ExitCapabilityState::Transferred => {
                final_count == 1
                    && cleanup_resume_sequence
                        .or(status_track.cancellation_cleanup_sequence)
                        .or(status_track.ambiguous_resume_resolved_sequence)
                        .is_some_and(|sequence| sequence > last_transition_sequence)
            }
            ExitCapabilityState::Expired => {
                status_track.exit_capability_expired_dispositions == 1
                    && final_count == 1
                    && status_track
                        .exit_capability_expired_sequence
                        .is_some_and(|sequence| sequence > last_transition_sequence)
            }
            ExitCapabilityState::Initial | ExitCapabilityState::Available => false,
        };
        if transitions_valid && final_valid {
            valid_exit_capabilities.insert(*status);
        } else {
            violations.push(PhysicalPartitionViolation::InvalidExitCapabilityFinalization(*status));
        }
    }
    let mut explicit_dispositions = 0;
    for (status, track) in &statuses {
        let final_count = track.dispositions + track.successful_resumes;
        let has_cleanup_transaction = has_valid_status_transaction(*status);
        if track.undecodable {
            let disposed_by_cleanup = track.dispositions == 1
                && track.cancellation_cleanup_dispositions == 1
                && track.successful_resumes == 0;
            let resumed_by_cleanup = track.dispositions == 0
                && track.successful_resumes == 1
                && track.successful_resume_owner.is_some_and(|owner| {
                    owner.is_registered_controller_cleanup() || owner.is_startup_barrier_cleanup()
                });
            if !disposed_by_cleanup && !resumed_by_cleanup {
                violations
                    .push(PhysicalPartitionViolation::InvalidUndecodableStatusLifecycle(*status));
            }
            if !has_cleanup_transaction {
                violations
                    .push(PhysicalPartitionViolation::MissingRegisteredCleanupEvidence(*status));
            }
        } else {
            if track.published == 0 {
                violations.push(PhysicalPartitionViolation::StatusNotPublished(*status));
            }
            if track.published > 1 {
                violations.push(PhysicalPartitionViolation::DuplicateStatusPublication(
                    *status,
                ));
            }
            let terminal = track.raw_status.is_some_and(|raw_status| {
                libc::WIFEXITED(raw_status) || libc::WIFSIGNALED(raw_status)
            });
            if final_count == 0 && !terminal {
                violations.push(PhysicalPartitionViolation::StatusWithoutDisposition(
                    *status,
                ));
            }
        }
        if (track.producer == Some(PhysicalWaitProducer::RegisteredCleanup)
            || track.publication_destination
                == Some(PhysicalStatusPublication::PreStopDrainFailureCleanup)
            || track.publication_destination
                == Some(PhysicalStatusPublication::StartupBarrierFailureCleanup)
            || track.publication_destination
                == Some(PhysicalStatusPublication::StartupBarrierCleanupStopped)
            || track.publication_destination
                == Some(PhysicalStatusPublication::StartupBarrierCleanupTerminal))
            && !has_cleanup_transaction
        {
            violations.push(PhysicalPartitionViolation::MissingRegisteredCleanupEvidence(*status));
        }
        let continued_publication_route = match track.publication_destination {
            Some(PhysicalStatusPublication::ContinuedSideChannel { route }) => Some(route),
            _ => None,
        };
        let continued_publication = continued_publication_route.is_some();
        let continued_producer_matches_route =
            continued_publication_route.is_some_and(|route| match route {
                PhysicalContinuedStatusRoute::PreStopDrain { .. } => {
                    track.producer == Some(PhysicalWaitProducer::PreStopContinuedDrain)
                }
                PhysicalContinuedStatusRoute::UnwatchedRoot
                | PhysicalContinuedStatusRoute::BeforeFirstStop
                | PhysicalContinuedStatusRoute::AfterFirstStop
                | PhysicalContinuedStatusRoute::AfterAcknowledgedGroupStop => {
                    track.producer == Some(PhysicalWaitProducer::AuthorizedRootNotifier)
                }
            });
        let exact_continued_side_channel = track
            .raw_status
            .is_some_and(|raw_status| libc::WIFCONTINUED(raw_status))
            && continued_producer_matches_route
            && track.published == 1
            && track.dispositions == 1
            && track.continued_side_channel_dispositions == 1
            && track.continued_side_channel_route == continued_publication_route
            && track.successful_resumes == 0
            && track.publication_sequence.is_some_and(|published| {
                track
                    .continued_side_channel_sequence
                    .is_some_and(|disposed| published < disposed)
            });
        if continued_publication != exact_continued_side_channel
            || (track.continued_side_channel_dispositions != 0 && !exact_continued_side_channel)
        {
            violations.push(PhysicalPartitionViolation::InvalidStatusDisposition(
                *status,
            ));
        }
        if track.ordinary_handled_dispositions != 0 {
            let exact_failed_resumes = track
                .generation
                .zip(track.task)
                .zip(track.publication_sequence)
                .zip(track.ordinary_handled_sequence)
                .map_or(0, |(((generation, task), published), disposition)| {
                    resume_attempts
                        .iter()
                        .filter(|(attempt, context)| {
                            context.owner == PhysicalResumeOwner::TypedStopped
                                && context.source_status == Some(*status)
                                && resume_generations.get(attempt) == Some(&generation)
                                && task.authorizes(context.task, false)
                                && resume_results.get(attempt)
                                    == Some(&PhysicalResumeOutcome::Error(libc::ESRCH))
                                && resume_attempt_sequences
                                    .get(attempt)
                                    .is_some_and(|sequence| published < *sequence)
                                && resume_result_sequences
                                    .get(attempt)
                                    .is_some_and(|sequence| *sequence < disposition)
                        })
                        .count()
                });
            if exact_failed_resumes != 1 {
                violations.push(PhysicalPartitionViolation::InvalidStatusDisposition(
                    *status,
                ));
            }
        }
        if track.cancellation_cleanup_dispositions != 0 {
            let synchronous_cancellation = reservations.values().any(|reservation| {
                reservation.status == Some(*status)
                    && matches!(
                        reservation.decode_finished,
                        Some((
                            PhysicalDecodeOwner::Synchronous,
                            PhysicalDecodeOutcome::Cancelled,
                            _
                        ))
                    )
                    && matches!(
                        reservation.completion,
                        Some((ReservationCompletion::Committed, completed))
                            if reservation.decode_finished.is_some_and(|(_, _, finished)| {
                                valid_synchronous_cancellation(*status, finished, completed)
                            })
                    )
            });
            let evidence = match track.producer {
                Some(PhysicalWaitProducer::PreRegistrationCleanup) => track
                    .generation
                    .zip(track.task)
                    .zip(track.cancellation_cleanup_sequence)
                    .is_some_and(|((generation, task), sequence)| {
                        track.raw_status.is_some_and(is_terminal_raw_status)
                            || has_future_external_terminal_evidence(generation, task, sequence)
                    }),
                Some(
                    PhysicalWaitProducer::PreRegistrationBarrierCleanup
                    | PhysicalWaitProducer::RegisteredCleanup,
                ) => has_cleanup_transaction,
                Some(PhysicalWaitProducer::PreStopContinuedDrain) => has_cleanup_transaction,
                Some(PhysicalWaitProducer::PreRegistrationBarrier) => false,
                Some(
                    PhysicalWaitProducer::NotifierWorker
                    | PhysicalWaitProducer::AuthorizedRootNotifier
                    | PhysicalWaitProducer::SynchronousWait,
                ) => {
                    let consumed_before_disposition = track
                        .cancellation_cleanup_sequence
                        .is_some_and(|disposition| {
                            reservations
                                .values()
                                .filter(|reservation| {
                                    reservation.status == Some(*status)
                                        && matches!(
                                            reservation.decode_finished,
                                            Some((_, PhysicalDecodeOutcome::Returned, _))
                                        )
                                        && matches!(
                                            reservation.completion,
                                            Some((ReservationCompletion::Committed, completed))
                                                if completed < disposition
                                        )
                                })
                                .count()
                                == 1
                        });
                    synchronous_cancellation
                        || has_cleanup_transaction
                        || (consumed_before_disposition
                            && track
                                .generation
                                .zip(track.task)
                                .zip(track.cancellation_cleanup_sequence)
                                .is_some_and(|((generation, task), sequence)| {
                                    track.raw_status.is_some_and(is_terminal_raw_status)
                                        || has_prior_terminal_evidence(
                                            generation,
                                            task,
                                            track.created_sequence,
                                            sequence,
                                        )
                                }))
                }
                None => false,
            };
            if !evidence {
                violations.push(PhysicalPartitionViolation::InvalidStatusDisposition(
                    *status,
                ));
            }
        }
        if track.exit_capability_expired_dispositions != 0
            && !valid_exit_capabilities.contains(status)
        {
            violations.push(PhysicalPartitionViolation::InvalidStatusDisposition(
                *status,
            ));
        }
        if track.kernel_superseded_dispositions != 0 {
            let consumed_before_disposition =
                track.kernel_superseded_sequence.is_some_and(|disposition| {
                    reservations
                        .values()
                        .filter(|reservation| {
                            reservation.status == Some(*status)
                                && matches!(
                                    reservation.decode_finished,
                                    Some((_, PhysicalDecodeOutcome::Returned, _))
                                )
                                && matches!(
                                    reservation.completion,
                                    Some((ReservationCompletion::Committed, completed))
                                        if completed < disposition
                                )
                        })
                        .count()
                        == 1
                });
            let exit_stop_preceded_disposition = track
                .generation
                .zip(track.task)
                .zip(track.kernel_superseded_sequence)
                .is_some_and(|((generation, task), sequence)| {
                    statuses.iter().any(|(candidate, candidate_track)| {
                        candidate != status
                            && candidate_track.generation == Some(generation)
                            && track.created_sequence < candidate_track.created_sequence
                            && candidate_track.created_sequence < sequence
                            && candidate_track.raw_status.is_some_and(is_ptrace_exit_stop)
                            && candidate_track.publication_destination
                                == Some(PhysicalStatusPublication::ExitCapability)
                            && candidate_track
                                .publication_sequence
                                .is_some_and(|published| published < sequence)
                            && valid_exit_capabilities.contains(candidate)
                            && exit_capabilities.get(candidate).is_some_and(|capability| {
                                capability
                                    .transitions
                                    .iter()
                                    .any(|(transition, _)| *transition < sequence)
                            })
                            && candidate_track.task.is_some_and(|candidate_task| {
                                task.authorizes(candidate_task, false)
                            })
                    })
                });
            if !consumed_before_disposition || !exit_stop_preceded_disposition {
                violations.push(PhysicalPartitionViolation::InvalidStatusDisposition(
                    *status,
                ));
            }
        }
        if track.ambiguous_resume_resolved_dispositions != 0
            && !valid_ambiguous_resolution_sources.contains(status)
        {
            violations.push(PhysicalPartitionViolation::InvalidStatusDisposition(
                *status,
            ));
        }
        if track.publication_destination == Some(PhysicalStatusPublication::ExitCapability)
            && !valid_exit_capabilities.contains(status)
            && !exit_capabilities.contains_key(status)
        {
            violations.push(PhysicalPartitionViolation::InvalidExitCapabilityFinalization(*status));
        }
        if final_count > 1 {
            violations.push(PhysicalPartitionViolation::DuplicateStatusDisposition(
                *status,
            ));
        }
        if track.successful_resumes > 1 {
            violations.push(PhysicalPartitionViolation::DuplicateSuccessfulResume(
                *status,
            ));
        }
        explicit_dispositions += track.dispositions;
    }

    PhysicalPartitionValidation {
        physical_statuses: statuses.len(),
        successful_resumes,
        explicit_dispositions,
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observer() -> PhysicalEventObserver {
        PhysicalEventObserver::new(PhysicalEventObserverConfig::new(128, 32))
            .expect("create physical observer")
    }

    fn generation() -> PhysicalEventGenerationId {
        PhysicalEventGenerationId::allocate()
    }

    fn wait_context(generation: PhysicalEventGenerationId) -> PhysicalWaitContext {
        PhysicalWaitContext {
            generation: Some(generation),
            task: PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        }
    }

    fn stopped_status() -> i32 {
        (libc::SIGSTOP << 8) | 0x7f
    }

    fn captured_task(tid: i32, pidfd: i32) -> PhysicalTaskIdentity {
        PhysicalTaskIdentity::captured(Pid::from_raw(tid), Pid::from_raw(tid), 101, 103, pidfd)
    }

    fn wait_siginfo(pid: i32) -> PhysicalWaitSiginfo {
        PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_STOPPED,
            pid,
            uid: 1000,
            status: libc::SIGSTOP,
        }
    }

    fn original_root_task() -> PhysicalTaskIdentity {
        PhysicalTaskIdentity::direct_child(Pid::from_raw(7))
    }

    fn captured_original_root_task(
        tid: i32,
        tgid: i32,
        ppid: i32,
        tracer_pid: i32,
    ) -> PhysicalTaskIdentity {
        PhysicalTaskIdentity::captured_with_controller(
            Pid::from_raw(tid),
            Pid::from_raw(tgid),
            Pid::from_raw(ppid),
            Pid::from_raw(tracer_pid),
            101,
            103,
            11,
        )
    }

    fn inject_original_root_launch(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        link: u64,
    ) {
        inject_original_root_launch_with(
            observer,
            generation,
            PhysicalOriginalRootLaunchId(link),
            original_root_task(),
            Pid::from_raw(41),
            Pid::from_raw(42),
            1,
        );
    }

    fn inject_original_root_launch_with(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        link: PhysicalOriginalRootLaunchId,
        task: PhysicalTaskIdentity,
        controller_tgid: Pid,
        controller_tid: Pid,
        controller_sequence: u64,
    ) {
        observer.inject_for_test(PhysicalEventRecordKind::OriginalRootLaunchLinked {
            link,
            generation,
            task,
            controller_tgid,
            controller_tid,
            controller_sequence,
        });
    }

    fn finish_interrupted_wait(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        producer: PhysicalWaitProducer,
    ) -> PhysicalWaitAttempt {
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer,
            flags: production_wait_flags(producer),
        });
        observer.finish_wait_error(wait, libc::EINTR);
        wait
    }

    fn has_wait_before_identity_bound(
        validation: &PhysicalPartitionValidation,
        wait: PhysicalWaitAttempt,
    ) -> bool {
        validation
            .violations
            .contains(&PhysicalPartitionViolation::WaitBeforeIdentityBound(
                wait.id(),
            ))
    }

    fn has_wrong_wait_task(
        validation: &PhysicalPartitionValidation,
        wait: PhysicalWaitAttempt,
    ) -> bool {
        validation
            .violations
            .contains(&PhysicalPartitionViolation::WrongWaitTask(wait.id()))
    }

    fn record_valid_original_root_barrier_session(
        observer: &PhysicalEventObserver,
        tracer_pid: i32,
        prepared_launch: Option<PhysicalOriginalRootLaunchId>,
        prepared_task: Option<PhysicalTaskIdentity>,
    ) -> PhysicalWaitAttempt {
        let generation = generation();
        let barrier_task = captured_original_root_task(7, 7, 41, tracer_pid);
        let task = captured_original_root_task(7, 7, 41, 42);
        let launch = PhysicalOriginalRootLaunchId(7_001 + tracer_pid as u64);
        let raw_status = (libc::SIGTRAP << 8) | 0x7f;
        let siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_TRAPPED,
            pid: 7,
            uid: 1000,
            status: libc::SIGTRAP,
        };

        observer.attach_generation(generation);
        inject_original_root_launch(observer, generation, launch.get());
        let barrier = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: barrier_task,
            producer: PhysicalWaitProducer::PreRegistrationBarrier,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrier),
        });
        observer.finish_wait_retained_status(barrier, raw_status, siginfo);
        let transaction = observer.prepare_startup_barrier_cleanup_transaction();
        observer.record_startup_barrier_fallback_prepared(
            prepared_launch.unwrap_or(launch),
            generation,
            barrier,
            prepared_task.unwrap_or(barrier_task),
            transaction,
        );
        observer.bind_identity(generation, task);
        observer.record_continued_authority_enabled(
            generation,
            Pid::from_raw(7),
            Pid::from_raw(41),
            Pid::from_raw(42),
        );
        observer.record_worker_started(generation);

        let consuming_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::AuthorizedRootNotifier,
            flags: production_wait_flags(PhysicalWaitProducer::AuthorizedRootNotifier),
        });
        let status = observer.allocate_status();
        observer.record_wait_siginfo(consuming_wait, siginfo, Some(status));
        observer.finish_wait_status_with_id(consuming_wait, status, raw_status, Some(siginfo));
        observer.record_pre_registration_barrier_consumed(
            generation,
            barrier,
            consuming_wait,
            status,
        );
        observer.record_startup_barrier_fallback_released(
            generation,
            barrier,
            consuming_wait,
            status,
        );
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(observer, generation, status);
        resume_typed_status(observer, generation, task, status);

        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::AuthorizedRootNotifier,
            flags: production_wait_flags(PhysicalWaitProducer::AuthorizedRootNotifier),
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        observer.record_echild_pidfd_exited(terminal_wait, generation, task, libc::POLLIN);
        observer.record_synthetic_echild(generation, Some(terminal_wait.id()));
        observer.record_continued_authority_revoked(generation);
        observer.record_generation_finished(generation);
        observer.close();
        barrier
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum StartupResumeMutation {
        None,
        Task(PhysicalTaskIdentity),
        WrongGeneration,
        Owner(PhysicalResumeOwner),
        WrongSourceProducer,
        LaunchAfterResume,
        GenericLinkBeforeLateLaunch,
    }

    fn record_valid_unstarted_original_root_barrier_cleanup(
        observer: &PhysicalEventObserver,
        matched_barrier: bool,
        cleanup_task: Option<PhysicalTaskIdentity>,
        resume_mutation: StartupResumeMutation,
    ) -> (PhysicalWaitAttempt, PhysicalResumeAttempt) {
        let generation = generation();
        let barrier_task = captured_original_root_task(7, 7, 41, 42);
        let task = cleanup_task.unwrap_or(barrier_task);
        let launch = PhysicalOriginalRootLaunchId(7_500 + u64::from(matched_barrier));
        let delayed_launch = matches!(
            resume_mutation,
            StartupResumeMutation::LaunchAfterResume
                | StartupResumeMutation::GenericLinkBeforeLateLaunch
        );
        let retained_raw = (libc::SIGTRAP << 8) | 0x7f;
        let retained_siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_TRAPPED,
            pid: 7,
            uid: 1000,
            status: libc::SIGTRAP,
        };
        let (source_raw, source_siginfo) = if matched_barrier {
            (retained_raw, retained_siginfo)
        } else {
            (stopped_status(), wait_siginfo(7))
        };

        observer.attach_generation(generation);
        if resume_mutation == StartupResumeMutation::GenericLinkBeforeLateLaunch {
            observer.link_pre_registration_task(original_root_task(), generation);
        }
        if !delayed_launch {
            inject_original_root_launch(observer, generation, launch.get());
        }
        let barrier = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: barrier_task,
            producer: PhysicalWaitProducer::PreRegistrationBarrier,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrier),
        });
        observer.finish_wait_retained_status(barrier, retained_raw, retained_siginfo);
        let transaction = observer.prepare_startup_barrier_cleanup_transaction();
        observer.record_startup_barrier_fallback_prepared(
            launch,
            generation,
            barrier,
            barrier_task,
            transaction,
        );

        let consuming_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrierCleanup),
        });
        let source = observer.allocate_status();
        observer.record_wait_siginfo(consuming_wait, source_siginfo, Some(source));
        observer.finish_wait_status_with_id(
            consuming_wait,
            source,
            source_raw,
            Some(source_siginfo),
        );
        if matched_barrier {
            observer.record_pre_registration_barrier_consumed(
                generation,
                barrier,
                consuming_wait,
                source,
            );
        }
        observer.begin_startup_barrier_cleanup_transaction(
            transaction,
            generation,
            task,
            barrier,
            consuming_wait,
            PhysicalStartupCleanupOwner::Unstarted,
        );
        observer.link_registered_cleanup_status(transaction, source);
        if !matched_barrier {
            observer.record_pre_registration_barrier_failure_linked(
                generation,
                barrier,
                consuming_wait,
                source,
                transaction,
            );
        }
        observer.record_cleanup_status_published(
            generation,
            source,
            if matched_barrier {
                PhysicalStatusPublication::StartupBarrierCleanupStopped
            } else {
                PhysicalStatusPublication::StartupBarrierFailureCleanup
            },
        );

        let signal = observer.begin_pidfd_signal(PhysicalPidfdSignalContext {
            generation,
            task,
            transaction: transaction.id(),
            pidfd: 11,
            signal: libc::SIGKILL,
        });
        observer.finish_pidfd_signal(signal, PhysicalPidfdSignalOutcome::Success);
        let wrong_generation =
            (resume_mutation == StartupResumeMutation::WrongGeneration).then(|| {
                let wrong = PhysicalEventGenerationId::allocate();
                observer.attach_generation(wrong);
                wrong
            });
        let wrong_source =
            (resume_mutation == StartupResumeMutation::WrongSourceProducer).then(|| {
                let wait = observer.begin_wait(PhysicalWaitContext {
                    generation: Some(generation),
                    task,
                    producer: PhysicalWaitProducer::PreRegistrationCleanup,
                    flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationCleanup),
                });
                let status = observer.allocate_status();
                observer.finish_wait_status_with_id(wait, status, stopped_status(), None);
                observer.record_status_published(
                    generation,
                    status,
                    PhysicalStatusPublication::DirectStopped,
                );
                status
            });
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(wrong_generation.unwrap_or(generation)),
            task: match resume_mutation {
                StartupResumeMutation::Task(task) => task,
                _ => task,
            },
            source_status: Some(wrong_source.unwrap_or(source)),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: match resume_mutation {
                StartupResumeMutation::Owner(owner) => owner,
                _ => PhysicalResumeOwner::StartupBarrierCleanup,
            },
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        if delayed_launch {
            inject_original_root_launch(observer, generation, launch.get());
        }

        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrierCleanup),
        });
        let terminal = finish_exited_wait(observer, terminal_wait);
        observer.link_registered_cleanup_status(transaction, terminal);
        observer.finish_startup_barrier_cleanup_terminal_status(generation, terminal);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.finish_unregistered_generation(generation, terminal_wait.id());
        if let Some(wrong_generation) = wrong_generation {
            observer.record_generation_finished(wrong_generation);
        }
        observer.close();
        (barrier, resume)
    }

    fn record_valid_pidfd_bound_startup_setup_cleanup(
        observer: &PhysicalEventObserver,
    ) -> PhysicalWaitAttempt {
        let generation = generation();
        let task = PhysicalTaskIdentity::direct_child_with_pidfd(Pid::from_raw(7), 11);
        let launch = PhysicalOriginalRootLaunchId(7_600);
        let error = libc::EIO;

        observer.attach_generation(generation);
        inject_original_root_launch(observer, generation, launch.get());
        observer.record_pre_registration_barrier_setup_failed(generation, task, error, launch);
        let transaction = observer.prepare_startup_barrier_cleanup_transaction();
        observer.record_startup_setup_cleanup_prepared(
            generation,
            task,
            error,
            transaction,
            launch,
        );
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrierCleanup),
        });
        let terminal = finish_exited_wait(observer, terminal_wait);
        observer.begin_startup_setup_cleanup_transaction(
            transaction,
            generation,
            task,
            error,
            terminal_wait,
            launch,
        );
        observer.link_registered_cleanup_status(transaction, terminal);
        observer.record_startup_setup_cleanup_linked(
            generation,
            error,
            terminal_wait,
            terminal,
            transaction,
            launch,
        );
        observer.finish_startup_barrier_cleanup_terminal_status(generation, terminal);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.finish_unregistered_generation(generation, terminal_wait.id());
        observer.close();
        terminal_wait
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum SetupResumeMutation {
        None,
        WrongLaunch,
        WrongGeneration,
        LaunchAfterResume,
        Task(PhysicalTaskIdentity),
        WrongTransaction,
        WrongSourceProducer,
        WrongCauseStatus,
        SignalBeforeCauseWaitResult,
        Owner(PhysicalResumeOwner),
    }

    fn record_valid_pidfd_bound_startup_setup_resume(
        observer: &PhysicalEventObserver,
        mutation: SetupResumeMutation,
    ) -> PhysicalResumeAttempt {
        let generation = generation();
        let task = PhysicalTaskIdentity::direct_child_with_pidfd(Pid::from_raw(7), 11);
        let launch = PhysicalOriginalRootLaunchId(7_700);
        let transaction_launch = if mutation == SetupResumeMutation::WrongLaunch {
            PhysicalOriginalRootLaunchId(7_701)
        } else {
            launch
        };
        let delayed_launch = mutation == SetupResumeMutation::LaunchAfterResume;
        let error = libc::EIO;

        observer.attach_generation(generation);
        if !delayed_launch {
            inject_original_root_launch(observer, generation, launch.get());
        }
        observer.record_pre_registration_barrier_setup_failed(generation, task, error, launch);
        let transaction = observer.prepare_startup_barrier_cleanup_transaction();
        observer.record_startup_setup_cleanup_prepared(
            generation,
            task,
            error,
            transaction,
            launch,
        );
        let source_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrierCleanup),
        });
        let source = if mutation == SetupResumeMutation::SignalBeforeCauseWaitResult {
            let source = observer.allocate_status();
            let siginfo = wait_siginfo(source_wait.context().task.tid());
            observer.record_wait_siginfo(source_wait, siginfo, Some(source));
            let signal = observer.begin_pidfd_signal(PhysicalPidfdSignalContext {
                generation,
                task,
                transaction: transaction.id(),
                pidfd: 11,
                signal: libc::SIGKILL,
            });
            observer.finish_pidfd_signal(signal, PhysicalPidfdSignalOutcome::Success);
            observer.finish_wait_status_with_id(
                source_wait,
                source,
                stopped_status(),
                Some(siginfo),
            );
            source
        } else {
            finish_stopped_wait(observer, source_wait)
        };
        let transaction_source = if mutation == SetupResumeMutation::WrongCauseStatus {
            let other_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
                flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrierCleanup),
            });
            finish_stopped_wait(observer, other_wait)
        } else {
            source
        };
        observer.begin_startup_setup_cleanup_transaction(
            transaction,
            generation,
            task,
            error,
            source_wait,
            transaction_launch,
        );
        observer.link_registered_cleanup_status(transaction, transaction_source);
        observer.record_startup_setup_cleanup_linked(
            generation,
            error,
            source_wait,
            transaction_source,
            transaction,
            transaction_launch,
        );
        observer.record_cleanup_status_published(
            generation,
            transaction_source,
            PhysicalStatusPublication::StartupBarrierFailureCleanup,
        );
        if mutation != SetupResumeMutation::SignalBeforeCauseWaitResult {
            let signal = observer.begin_pidfd_signal(PhysicalPidfdSignalContext {
                generation,
                task,
                transaction: transaction.id(),
                pidfd: 11,
                signal: libc::SIGKILL,
            });
            observer.finish_pidfd_signal(signal, PhysicalPidfdSignalOutcome::Success);
        }

        if mutation == SetupResumeMutation::WrongTransaction {
            let wrong_transaction = observer.prepare_startup_barrier_cleanup_transaction();
            observer.link_registered_cleanup_status(wrong_transaction, source);
        }
        let wrong_source = (mutation == SetupResumeMutation::WrongSourceProducer).then(|| {
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::PreRegistrationCleanup,
                flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationCleanup),
            });
            let status = observer.allocate_status();
            observer.finish_wait_status_with_id(wait, status, stopped_status(), None);
            observer.record_status_published(
                generation,
                status,
                PhysicalStatusPublication::DirectStopped,
            );
            status
        });
        let wrong_generation = (mutation == SetupResumeMutation::WrongGeneration).then(|| {
            let wrong = PhysicalEventGenerationId::allocate();
            observer.attach_generation(wrong);
            wrong
        });
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(wrong_generation.unwrap_or(generation)),
            task: match mutation {
                SetupResumeMutation::Task(task) => task,
                _ => task,
            },
            source_status: Some(wrong_source.unwrap_or(transaction_source)),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: match mutation {
                SetupResumeMutation::Owner(owner) => owner,
                _ => PhysicalResumeOwner::StartupBarrierCleanup,
            },
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        if delayed_launch {
            inject_original_root_launch(observer, generation, launch.get());
        }

        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrierCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrierCleanup),
        });
        let terminal = finish_exited_wait(observer, terminal_wait);
        observer.link_registered_cleanup_status(transaction, terminal);
        observer.finish_startup_barrier_cleanup_terminal_status(generation, terminal);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.finish_unregistered_generation(generation, terminal_wait.id());
        if let Some(wrong_generation) = wrong_generation {
            observer.record_generation_finished(wrong_generation);
        }
        observer.close();
        resume
    }

    fn undecodable_wait_siginfo(pid: i32) -> PhysicalWaitSiginfo {
        PhysicalWaitSiginfo {
            code: i32::MAX,
            ..wait_siginfo(pid)
        }
    }

    fn record_undecodable_status(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
    ) -> (PhysicalStatusId, PhysicalWaitAttempt) {
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = observer.allocate_status();
        let siginfo = undecodable_wait_siginfo(task.tid());
        observer.record_wait_siginfo(wait, siginfo, Some(status));
        observer.finish_wait_undecodable_status(wait, status, siginfo, libc::EPROTO);
        (status, wait)
    }

    fn record_fatal_wait_error(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        error: i32,
    ) -> PhysicalWaitAttempt {
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(wait, error);
        wait
    }

    fn prove_registered_cleanup_echild(
        observer: &PhysicalEventObserver,
        transaction: PhysicalCleanupTransaction,
        terminal_wait: PhysicalWaitAttempt,
    ) {
        let context = terminal_wait.context();
        observer.record_registered_cleanup_pidfd_exited(
            transaction,
            terminal_wait,
            context.generation.expect("registered cleanup generation"),
            context.task,
            libc::POLLIN,
            None,
        );
    }

    fn finish_stopped_wait(
        observer: &PhysicalEventObserver,
        wait: PhysicalWaitAttempt,
    ) -> PhysicalStatusId {
        let status = observer.allocate_status();
        let siginfo = wait_siginfo(wait.context().task.tid());
        observer.record_wait_siginfo(wait, siginfo, Some(status));
        observer.finish_wait_status_with_id(wait, status, stopped_status(), Some(siginfo));
        status
    }

    fn finish_exited_wait(
        observer: &PhysicalEventObserver,
        wait: PhysicalWaitAttempt,
    ) -> PhysicalStatusId {
        let status = observer.allocate_status();
        let siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_EXITED,
            pid: wait.context().task.tid(),
            uid: 1000,
            status: 0,
        };
        observer.record_wait_siginfo(wait, siginfo, Some(status));
        observer.finish_wait_status_with_id(wait, status, 0, Some(siginfo));
        status
    }

    fn finish_ptrace_event_wait(
        observer: &PhysicalEventObserver,
        wait: PhysicalWaitAttempt,
        event: i32,
    ) -> PhysicalStatusId {
        let status = observer.allocate_status();
        let siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_TRAPPED,
            pid: wait.context().task.tid(),
            uid: 1000,
            status: libc::SIGTRAP | (event << 8),
        };
        let raw_status = (siginfo.status << 8) | 0x7f;
        observer.record_wait_siginfo(wait, siginfo, Some(status));
        observer.finish_wait_status_with_id(wait, status, raw_status, Some(siginfo));
        status
    }

    fn finish_ptrace_exit_wait(
        observer: &PhysicalEventObserver,
        wait: PhysicalWaitAttempt,
    ) -> PhysicalStatusId {
        finish_ptrace_event_wait(observer, wait, libc::PTRACE_EVENT_EXIT)
    }

    fn finish_continued_wait(
        observer: &PhysicalEventObserver,
        wait: PhysicalWaitAttempt,
    ) -> PhysicalStatusId {
        let status = observer.allocate_status();
        let siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_CONTINUED,
            pid: wait.context().task.tid(),
            uid: 1000,
            status: libc::SIGCONT,
        };
        observer.record_wait_siginfo(wait, siginfo, Some(status));
        observer.finish_wait_status_with_id(wait, status, 0xffff, Some(siginfo));
        status
    }

    fn finish_notifier_generation_with_echild(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
    ) {
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        observer.record_echild_pidfd_exited(terminal_wait, generation, task, libc::POLLIN);
        observer.record_synthetic_echild(generation, Some(terminal_wait.id()));
        observer.record_generation_finished(generation);
    }

    fn begin_ambiguous_exit_resume(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        error: i32,
    ) -> (PhysicalStatusId, PhysicalResumeAttempt) {
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
        });
        let source = finish_ptrace_exit_wait(observer, wait);
        observer.record_status_published(
            generation,
            source,
            PhysicalStatusPublication::ExitCapability,
        );
        observer.record_exit_capability(source, PhysicalExitCapabilityTransition::Published);
        observer.record_exit_capability(source, PhysicalExitCapabilityTransition::Revoked);
        let attempt = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task,
            source_status: Some(source),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(attempt, PhysicalResumeOutcome::Error(error));
        (source, attempt)
    }

    fn finish_synchronous_generation_with_echild(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
    ) {
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::SynchronousWait,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        observer.record_echild_pidfd_exited(terminal_wait, generation, task, libc::POLLIN);
        observer.record_synthetic_echild(generation, Some(terminal_wait.id()));
        observer.record_generation_finished(generation);
    }

    fn resume_typed_status(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        status: PhysicalStatusId,
    ) -> PhysicalResumeAttempt {
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task,
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        resume
    }

    fn deliver_status(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
    ) -> PhysicalReservationId {
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            reservation,
            status,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Notifier,
        );
        observer.record_reservation_committed(reservation, status);
        reservation
    }

    fn publish_fifo_stop(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
        producer: PhysicalWaitProducer,
        destination: PhysicalStatusPublication,
    ) -> PhysicalStatusId {
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(observer, wait);
        observer.record_status_published(generation, status, destination);
        status
    }

    fn begin_cancelled_decode(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        status: PhysicalStatusId,
        owner: PhysicalDecodeOwner,
    ) -> PhysicalReservationId {
        let reservation = observer.next_reservation();
        if owner == PhysicalDecodeOwner::Cleanup {
            observer.record_cleanup_reserved(generation, reservation, status);
        } else {
            observer.record_reserved(generation, reservation, status);
        }
        observer.record_decode_started(reservation, status, owner);
        observer.record_decode_finished(
            reservation,
            status,
            PhysicalDecodeOutcome::Cancelled,
            owner,
        );
        reservation
    }

    fn publish_expired_exit_capability(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
    ) -> PhysicalStatusId {
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_ptrace_exit_wait(observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::ExitCapability,
        );
        observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Published);
        observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Expired);
        observer.finish_status(status, PhysicalStatusDisposition::ExitCapabilityExpired);
        status
    }

    fn publish_retained_terminal(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
        task: PhysicalTaskIdentity,
    ) -> PhysicalStatusId {
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_exited_wait(observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RetainedTerminal,
        );
        status
    }

    fn publish_and_resume(
        observer: &PhysicalEventObserver,
        generation: PhysicalEventGenerationId,
    ) -> PhysicalStatusId {
        let task = captured_task(7, 10);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 11),
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            reservation,
            status,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Notifier,
        );
        observer.record_reservation_committed(reservation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        finish_notifier_generation_with_echild(observer, generation, captured_task(7, 13));
        status
    }

    fn validate_unstarted_invalidation(
        bound: Option<PhysicalTaskIdentity>,
        record: impl FnOnce(&PhysicalEventObserver, PhysicalEventGenerationId),
    ) -> PhysicalPartitionValidation {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        if let Some(bound) = bound {
            observer.bind_identity(generation, bound);
        }
        record(&observer, generation);
        observer.record_synthetic_echild(generation, None);
        observer.record_generation_finished(generation);
        observer.close();
        observer.snapshot().validate()
    }

    fn replacement_task(tid: i32, pidfd: i32) -> PhysicalTaskIdentity {
        PhysicalTaskIdentity::captured(Pid::from_raw(tid), Pid::from_raw(tid), 202, 203, pidfd)
    }

    #[test]
    fn partition_accepts_original_root_barrier_before_identity_binding() {
        for tracer_pid in [0, 42] {
            let observer = observer();
            let wait =
                record_valid_original_root_barrier_session(&observer, tracer_pid, None, None);
            let validation = observer.snapshot().validate();
            assert!(
                validation.is_valid(),
                "production-shaped original-root launch did not authorize tracer_pid={tracer_pid}: {:?}",
                validation.violations,
            );
            assert!(!has_wait_before_identity_bound(&validation, wait));
            assert!(!has_wrong_wait_task(&validation, wait));
        }
    }

    #[test]
    fn partition_rejects_original_root_barrier_after_continued_authority_enabled() {
        let observer = observer();
        let generation = generation();
        let task = captured_original_root_task(7, 7, 41, 42);
        let launch = PhysicalOriginalRootLaunchId(7_099);
        let raw_status = (libc::SIGTRAP << 8) | 0x7f;
        let siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_TRAPPED,
            pid: 7,
            uid: 1000,
            status: libc::SIGTRAP,
        };

        observer.attach_generation(generation);
        inject_original_root_launch(&observer, generation, launch.get());
        observer.bind_identity(generation, task);
        observer.record_continued_authority_enabled(
            generation,
            Pid::from_raw(7),
            Pid::from_raw(41),
            Pid::from_raw(42),
        );
        let barrier = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrier,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrier),
        });
        observer.finish_wait_retained_status(barrier, raw_status, siginfo);
        let transaction = observer.prepare_startup_barrier_cleanup_transaction();
        observer.record_startup_barrier_fallback_prepared(
            launch,
            generation,
            barrier,
            task,
            transaction,
        );
        observer.record_worker_started(generation);

        let consuming_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::AuthorizedRootNotifier,
            flags: production_wait_flags(PhysicalWaitProducer::AuthorizedRootNotifier),
        });
        let status = observer.allocate_status();
        observer.record_wait_siginfo(consuming_wait, siginfo, Some(status));
        observer.finish_wait_status_with_id(consuming_wait, status, raw_status, Some(siginfo));
        observer.record_pre_registration_barrier_consumed(
            generation,
            barrier,
            consuming_wait,
            status,
        );
        observer.record_startup_barrier_fallback_released(
            generation,
            barrier,
            consuming_wait,
            status,
        );
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, status);
        resume_typed_status(&observer, generation, task, status);

        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::AuthorizedRootNotifier,
            flags: production_wait_flags(PhysicalWaitProducer::AuthorizedRootNotifier),
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        observer.record_echild_pidfd_exited(terminal_wait, generation, task, libc::POLLIN);
        observer.record_synthetic_echild(generation, Some(terminal_wait.id()));
        observer.record_continued_authority_revoked(generation);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert_eq!(validation.violations.len(), 2, "{validation:#?}");
        assert!(validation.violations.contains(
            &PhysicalPartitionViolation::InvalidContinuedAuthority(generation),
        ));
        assert!(validation.violations.contains(
            &PhysicalPartitionViolation::InvalidPreRegistrationBarrier(barrier.id()),
        ));
        assert_eq!(
            validation
                .violations
                .iter()
                .filter(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidContinuedAuthority(observed)
                        if *observed == generation
                ))
                .count(),
            1,
            "{validation:#?}",
        );
    }

    #[test]
    fn partition_rejects_original_root_fallback_launch_and_captured_shape_mutations() {
        let mutations = [
            (Some(PhysicalOriginalRootLaunchId(8_001)), None, "launch-id"),
            (
                None,
                Some(captured_original_root_task(7, 8, 41, 42)),
                "tgid",
            ),
            (
                None,
                Some(captured_original_root_task(7, 7, 40, 42)),
                "ppid",
            ),
            (
                None,
                Some(captured_original_root_task(7, 7, 41, 43)),
                "tracer",
            ),
            (
                None,
                Some(PhysicalTaskIdentity::captured(
                    Pid::from_raw(7),
                    Pid::from_raw(7),
                    101,
                    103,
                    11,
                )),
                "controller-shape",
            ),
        ];
        for (prepared_launch, prepared_task, field) in mutations {
            let observer = observer();
            let barrier = record_valid_original_root_barrier_session(
                &observer,
                42,
                prepared_launch,
                prepared_task,
            );
            let validation = observer.snapshot().validate();
            assert!(
                validation.violations.contains(
                    &PhysicalPartitionViolation::InvalidPreRegistrationBarrier(barrier.id()),
                ),
                "fallback {field} mutation was accepted: {:?}",
                validation.violations,
            );
        }
    }

    #[test]
    fn partition_accepts_pidfd_bound_original_root_barrier_cleanup_before_capture() {
        let observer = observer();
        let wait = record_valid_pidfd_bound_startup_setup_cleanup(&observer);
        let validation = observer.snapshot().validate();
        assert!(
            validation.is_valid(),
            "spawn-sourced pidfd-bound cleanup evidence was invalid: {:?}",
            validation.violations,
        );
        assert!(!has_wait_before_identity_bound(&validation, wait));
        assert!(!has_wrong_wait_task(&validation, wait));
    }

    #[test]
    fn partition_accepts_pidfd_bound_startup_setup_stop_resume_and_terminal_finish() {
        let observer = observer();
        let resume =
            record_valid_pidfd_bound_startup_setup_resume(&observer, SetupResumeMutation::None);
        let validation = observer.snapshot().validate();
        assert!(
            validation.is_valid(),
            "pidfd-bound StartupSetup resume evidence was invalid: {:?}",
            validation.violations,
        );
        assert!(
            !validation
                .violations
                .contains(&PhysicalPartitionViolation::WrongResumeTask(resume.id()))
        );
        assert!(!validation.violations.contains(
            &PhysicalPartitionViolation::ResumeBeforeIdentityBound(resume.id()),
        ));
    }

    #[test]
    fn partition_rejects_pidfd_bound_startup_setup_resume_tuple_mutations() {
        let mutations = [
            SetupResumeMutation::WrongLaunch,
            SetupResumeMutation::WrongGeneration,
            SetupResumeMutation::LaunchAfterResume,
            SetupResumeMutation::Task(PhysicalTaskIdentity::direct_child_with_pidfd(
                Pid::from_raw(8),
                11,
            )),
            SetupResumeMutation::Task(PhysicalTaskIdentity::direct_child_with_pidfd(
                Pid::from_raw(7),
                12,
            )),
            SetupResumeMutation::WrongTransaction,
            SetupResumeMutation::WrongSourceProducer,
            SetupResumeMutation::WrongCauseStatus,
            SetupResumeMutation::Owner(PhysicalResumeOwner::RootCleanup),
        ];
        for (index, mutation) in mutations.into_iter().enumerate() {
            let observer = observer();
            let resume = record_valid_pidfd_bound_startup_setup_resume(&observer, mutation);
            let validation = observer.snapshot().validate();
            assert!(
                validation
                    .violations
                    .contains(&PhysicalPartitionViolation::WrongResumeTask(resume.id())),
                "StartupSetup resume tuple mutation {index} retained task authority: {:?}",
                validation.violations,
            );
            assert!(
                validation.violations.contains(
                    &PhysicalPartitionViolation::ResumeBeforeIdentityBound(resume.id()),
                ),
                "StartupSetup resume tuple mutation {index} retained temporal authority: {:?}",
                validation.violations,
            );
        }
    }

    #[test]
    fn partition_rejects_startup_setup_signal_before_cause_wait_result() {
        let observer = observer();
        record_valid_pidfd_bound_startup_setup_resume(
            &observer,
            SetupResumeMutation::SignalBeforeCauseWaitResult,
        );
        let validation = observer.snapshot().validate();
        assert!(
            validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidStartupCleanupPidfdSignal(_)
            )),
            "startup setup signal preceding its cause wait result was accepted: {:?}",
            validation.violations,
        );
    }

    #[test]
    fn partition_accepts_exact_and_mismatched_unstarted_original_root_cleanup() {
        for matched_barrier in [true, false] {
            let observer = observer();
            let (barrier, _) = record_valid_unstarted_original_root_barrier_cleanup(
                &observer,
                matched_barrier,
                None,
                StartupResumeMutation::None,
            );
            let validation = observer.snapshot().validate();
            assert!(
                validation.is_valid(),
                "unstarted original-root cleanup matched={matched_barrier} was invalid: {:?}",
                validation.violations,
            );
            assert!(!has_wait_before_identity_bound(&validation, barrier));
            assert!(!has_wrong_wait_task(&validation, barrier));
        }
    }

    #[test]
    fn partition_rejects_unstarted_original_root_cleanup_controller_shape_mutations() {
        let mutations = [
            captured_original_root_task(7, 8, 41, 42),
            captured_original_root_task(7, 7, 40, 42),
            captured_original_root_task(7, 7, 41, 43),
            PhysicalTaskIdentity::captured(Pid::from_raw(7), Pid::from_raw(7), 101, 103, 11),
        ];
        for matched_barrier in [true, false] {
            for (index, task) in mutations.into_iter().enumerate() {
                let observer = observer();
                let (barrier, _) = record_valid_unstarted_original_root_barrier_cleanup(
                    &observer,
                    matched_barrier,
                    Some(task),
                    StartupResumeMutation::None,
                );
                let validation = observer.snapshot().validate();
                assert!(
                    validation.violations.contains(
                        &PhysicalPartitionViolation::InvalidPreRegistrationBarrier(barrier.id()),
                    ),
                    "cleanup controller-shape mutation {index}, matched={matched_barrier} was accepted: {:?}",
                    validation.violations,
                );
            }
        }
    }

    #[test]
    fn partition_rejects_opaque_launch_resume_binding_mutations() {
        let mutations = [
            StartupResumeMutation::Task(captured_original_root_task(7, 8, 41, 42)),
            StartupResumeMutation::Task(captured_original_root_task(7, 7, 40, 42)),
            StartupResumeMutation::Task(captured_original_root_task(7, 7, 41, 43)),
            StartupResumeMutation::WrongGeneration,
            StartupResumeMutation::Owner(PhysicalResumeOwner::RootCleanup),
            StartupResumeMutation::WrongSourceProducer,
            StartupResumeMutation::LaunchAfterResume,
            StartupResumeMutation::GenericLinkBeforeLateLaunch,
        ];
        for (index, mutation) in mutations.into_iter().enumerate() {
            let observer = observer();
            let (_, resume) = record_valid_unstarted_original_root_barrier_cleanup(
                &observer, true, None, mutation,
            );
            let validation = observer.snapshot().validate();
            assert!(
                validation
                    .violations
                    .contains(&PhysicalPartitionViolation::WrongResumeTask(resume.id())),
                "resume binding mutation {index} retained task authority: {:?}",
                validation.violations,
            );
            assert!(
                validation.violations.contains(
                    &PhysicalPartitionViolation::ResumeBeforeIdentityBound(resume.id()),
                ),
                "resume binding mutation {index} retained temporal authority: {:?}",
                validation.violations,
            );
        }
    }

    #[test]
    fn partition_rejects_original_root_launch_recorded_after_wait_attempt() {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_original_root_task(7, 7, 41, 42),
            producer: PhysicalWaitProducer::PreRegistrationBarrier,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrier),
        });
        inject_original_root_launch(&observer, generation, 7_003);
        observer.finish_wait_error(wait, libc::EINTR);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(has_wait_before_identity_bound(&validation, wait));
        assert!(has_wrong_wait_task(&validation, wait));
    }

    #[test]
    fn partition_rejects_original_root_launch_from_wrong_or_adopted_raw_generation() {
        for adopted in [false, true] {
            let observer = observer();
            let launch_generation = generation();
            let wait_generation = generation();
            observer.attach_generation(launch_generation);
            observer.attach_generation(wait_generation);
            inject_original_root_launch(&observer, launch_generation, 7_004);
            if adopted {
                observer.adopt_generation(launch_generation, wait_generation);
            }
            let wait = finish_interrupted_wait(
                &observer,
                wait_generation,
                captured_original_root_task(7, 7, 41, 42),
                PhysicalWaitProducer::PreRegistrationBarrier,
            );
            observer.record_generation_finished(wait_generation);
            if !adopted {
                observer.record_generation_finished(launch_generation);
            }
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(
                has_wait_before_identity_bound(&validation, wait),
                "raw-generation mismatch was authorized when adopted={adopted}: {:?}",
                validation.violations,
            );
            assert!(has_wrong_wait_task(&validation, wait));
        }
    }

    #[test]
    fn partition_rejects_duplicate_original_root_launch_authority_for_barrier() {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        inject_original_root_launch(&observer, generation, 7_005);
        inject_original_root_launch(&observer, generation, 7_006);
        let wait = finish_interrupted_wait(
            &observer,
            generation,
            captured_original_root_task(7, 7, 41, 42),
            PhysicalWaitProducer::PreRegistrationBarrier,
        );
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(has_wait_before_identity_bound(&validation, wait));
        assert!(has_wrong_wait_task(&validation, wait));
        assert!(validation.violations.contains(
            &PhysicalPartitionViolation::InvalidOriginalRootLaunch(generation),
        ));
    }

    #[test]
    fn partition_rejects_original_root_launch_record_field_mutations() {
        let mutations = [
            (
                original_root_task(),
                Pid::from_raw(40),
                Pid::from_raw(42),
                1,
                false,
                "controller-tgid",
            ),
            (
                original_root_task(),
                Pid::from_raw(41),
                Pid::from_raw(43),
                1,
                false,
                "controller-tid",
            ),
            (
                original_root_task(),
                Pid::from_raw(41),
                Pid::from_raw(42),
                0,
                true,
                "controller-sequence",
            ),
            (
                PhysicalTaskIdentity::direct_child_with_pidfd(Pid::from_raw(7), 11),
                Pid::from_raw(41),
                Pid::from_raw(42),
                1,
                true,
                "launch-task-shape",
            ),
        ];
        for (
            index,
            (launch_task, controller_tgid, controller_tid, sequence, invalid_launch, field),
        ) in mutations.into_iter().enumerate()
        {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            inject_original_root_launch_with(
                &observer,
                generation,
                PhysicalOriginalRootLaunchId(7_400 + index as u64),
                launch_task,
                controller_tgid,
                controller_tid,
                sequence,
            );
            let wait = finish_interrupted_wait(
                &observer,
                generation,
                captured_original_root_task(7, 7, 41, 42),
                PhysicalWaitProducer::PreRegistrationBarrier,
            );
            observer.record_generation_finished(generation);
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(
                has_wait_before_identity_bound(&validation, wait)
                    && has_wrong_wait_task(&validation, wait),
                "launch record {field} mutation retained wait authority: {:?}",
                validation.violations,
            );
            assert_eq!(
                validation.violations.contains(
                    &PhysicalPartitionViolation::InvalidOriginalRootLaunch(generation),
                ),
                invalid_launch,
            );
        }
    }

    #[test]
    fn partition_rejects_original_root_barrier_controller_shape_mutations() {
        let mutations = [
            captured_original_root_task(8, 7, 41, 42),
            captured_original_root_task(7, 8, 41, 42),
            captured_original_root_task(7, 7, 40, 42),
            captured_original_root_task(7, 7, 41, 43),
            PhysicalTaskIdentity::captured(Pid::from_raw(7), Pid::from_raw(7), 101, 103, 11),
            PhysicalTaskIdentity::direct_child_with_pidfd(Pid::from_raw(7), 11),
        ];
        for (index, task) in mutations.into_iter().enumerate() {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            inject_original_root_launch(&observer, generation, 7_100 + index as u64);
            let wait = finish_interrupted_wait(
                &observer,
                generation,
                task,
                PhysicalWaitProducer::PreRegistrationBarrier,
            );
            observer.record_generation_finished(generation);
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(
                has_wait_before_identity_bound(&validation, wait),
                "controller-shape mutation {index} was authorized: {:?}",
                validation.violations,
            );
            if index + 1 != mutations.len() {
                assert!(has_wrong_wait_task(&validation, wait));
            }
        }
    }

    #[test]
    fn partition_rejects_original_root_launch_for_all_other_wait_producers() {
        let excluded = [
            PhysicalWaitProducer::PreRegistrationCleanup,
            PhysicalWaitProducer::AuthorizedRootNotifier,
            PhysicalWaitProducer::PreStopContinuedDrain,
            PhysicalWaitProducer::NotifierWorker,
            PhysicalWaitProducer::SynchronousWait,
            PhysicalWaitProducer::RegisteredCleanup,
        ];
        for (index, producer) in excluded.into_iter().enumerate() {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            inject_original_root_launch(&observer, generation, 7_200 + index as u64);
            let wait = finish_interrupted_wait(
                &observer,
                generation,
                captured_original_root_task(7, 7, 41, 42),
                producer,
            );
            observer.record_generation_finished(generation);
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(
                has_wait_before_identity_bound(&validation, wait),
                "opaque launch authorized excluded producer {producer:?}: {:?}",
                validation.violations,
            );
            assert!(has_wrong_wait_task(&validation, wait));
        }
    }

    #[test]
    fn partition_rejects_generic_link_for_live_original_root_barrier_waits() {
        let cases = [
            (
                PhysicalWaitProducer::PreRegistrationBarrier,
                captured_original_root_task(7, 7, 41, 42),
                true,
            ),
            (
                PhysicalWaitProducer::PreRegistrationBarrierCleanup,
                PhysicalTaskIdentity::direct_child_with_pidfd(Pid::from_raw(7), 11),
                false,
            ),
        ];
        for (producer, task, wrong_task_expected) in cases {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            observer.link_pre_registration_task(original_root_task(), generation);
            let wait = finish_interrupted_wait(&observer, generation, task, producer);
            observer.record_generation_finished(generation);
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(
                has_wait_before_identity_bound(&validation, wait),
                "generic link authorized live {producer:?} wait: {:?}",
                validation.violations,
            );
            assert_eq!(has_wrong_wait_task(&validation, wait), wrong_task_expected);
        }
    }

    #[test]
    fn partition_rejects_generic_link_as_original_root_fallback_provenance() {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        observer.link_pre_registration_task(original_root_task(), generation);
        let task = captured_original_root_task(7, 7, 41, 42);
        let barrier = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationBarrier,
            flags: production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrier),
        });
        observer.finish_wait_retained_status(barrier, stopped_status(), wait_siginfo(7));
        let transaction = observer.prepare_startup_barrier_cleanup_transaction();
        observer.record_startup_barrier_fallback_prepared(
            PhysicalOriginalRootLaunchId(7_300),
            generation,
            barrier,
            task,
            transaction,
        );
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.violations.contains(
            &PhysicalPartitionViolation::InvalidPreRegistrationBarrier(barrier.id()),
        ));
    }

    #[test]
    fn partition_accepts_one_wait_one_delivery_one_resume() {
        let observer = observer();
        publish_and_resume(&observer, generation());
        observer.close();
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_accepts_exact_typed_unstarted_generation_invalidations() {
        let direct = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        assert!(
            validate_unstarted_invalidation(None, |observer, generation| {
                observer.record_generation_capture_failed(generation, direct, libc::ENOENT);
            })
            .is_valid()
        );

        let bound = captured_task(7, 11);
        assert!(
            validate_unstarted_invalidation(Some(bound), |observer, generation| {
                observer.record_generation_capture_failed(generation, bound, libc::ESRCH);
            })
            .is_valid()
        );
        assert!(
            validate_unstarted_invalidation(Some(bound), |observer, generation| {
                observer.record_generation_identity_mismatch(
                    generation,
                    bound,
                    replacement_task(7, 12),
                );
            })
            .is_valid()
        );
        assert!(
            validate_unstarted_invalidation(Some(bound), |observer, generation| {
                observer.record_generation_bound_pidfd_dead(generation, bound);
            })
            .is_valid()
        );
        assert!(
            validate_unstarted_invalidation(Some(bound), |observer, generation| {
                observer.record_generation_current_pidfd_dead(generation, captured_task(7, 12));
            })
            .is_valid()
        );
        assert!(
            validate_unstarted_invalidation(Some(bound), |observer, generation| {
                observer.record_generation_registry_mismatch(
                    generation,
                    bound,
                    replacement_task(7, 12),
                );
            })
            .is_valid()
        );
    }

    #[test]
    fn partition_accepts_bound_pidfd_invalidation_after_equivalent_adoption() {
        let observer = observer();
        let requested = generation();
        let authoritative = generation();
        let canonical_identity = captured_task(7, 10);
        let adopted_bound = captured_task(7, 11);
        observer.attach_generation(requested);
        observer.attach_generation(authoritative);
        observer.bind_identity(authoritative, canonical_identity);
        observer.bind_identity(requested, adopted_bound);
        observer.adopt_generation(requested, authoritative);
        observer.record_generation_bound_pidfd_dead(authoritative, adopted_bound);
        observer.record_synthetic_echild(authoritative, None);
        observer.record_generation_finished(authoritative);
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_wrong_or_duplicate_generation_invalidation_causes() {
        let direct = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        let invalid_capture = validate_unstarted_invalidation(None, |observer, generation| {
            observer.record_generation_capture_failed(generation, direct, libc::EINVAL);
        });
        assert!(invalid_capture.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidGenerationLifecycle(_)
        )));

        let bound = captured_task(7, 11);
        let same_stable_task = captured_task(7, 12);
        let invalid_mismatch =
            validate_unstarted_invalidation(Some(bound), |observer, generation| {
                observer.record_generation_identity_mismatch(generation, bound, same_stable_task);
            });
        assert!(invalid_mismatch.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidGenerationLifecycle(_)
        )));

        let wrong_current = validate_unstarted_invalidation(Some(bound), |observer, generation| {
            observer.record_generation_current_pidfd_dead(generation, replacement_task(7, 12));
        });
        assert!(wrong_current.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidGenerationLifecycle(_)
        )));

        let current = replacement_task(7, 12);
        let current_authority_cannot_close_old =
            validate_unstarted_invalidation(Some(current), |observer, generation| {
                observer.record_generation_registry_mismatch(generation, bound, current);
            });
        assert!(
            current_authority_cannot_close_old
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationLifecycle(_)
                ))
        );

        let duplicate = validate_unstarted_invalidation(Some(bound), |observer, generation| {
            observer.record_generation_bound_pidfd_dead(generation, bound);
            observer.record_generation_bound_pidfd_dead(generation, bound);
        });
        assert!(duplicate.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidGenerationLifecycle(_)
        )));
    }

    #[test]
    fn partition_accepts_external_stop_resume_and_echild_completion() {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        observer.link_pre_registration_task(
            PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            generation,
        );
        let context = PhysicalWaitContext {
            generation: Some(generation),
            task: PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        };
        let wait = observer.begin_wait(context);
        let status = observer.finish_wait_status(wait, stopped_status(), None);
        observer.publish_external_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: context.task,
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::PreRegistrationCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        let terminal = observer.begin_wait(context);
        observer.finish_wait_error(terminal, libc::ECHILD);
        observer.record_pre_registration_task_gone(generation, context.task, libc::ESRCH);
        observer.finish_unregistered_generation(generation, terminal.id());
        observer.close();
        let validation = observer.snapshot().validate();
        assert!(validation.is_valid(), "{validation:#?}");
    }

    #[test]
    fn partition_accepts_direct_stop_transfer_to_typed_stopped() {
        let observer = observer();
        let generation = generation();
        let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        observer.attach_generation(generation);
        observer.link_pre_registration_task(task, generation);
        let context = PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        };
        let wait = observer.begin_wait(context);
        let status = observer.finish_wait_status(wait, stopped_status(), None);
        observer.publish_external_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task,
            source_status: Some(status),
            operation: PhysicalResumeOperation::Syscall,
            signal: Some(libc::SIGSTOP),
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        let terminal = observer.begin_wait(context);
        observer.finish_wait_error(terminal, libc::ECHILD);
        observer.record_pre_registration_task_gone(generation, task, libc::ESRCH);
        observer.finish_unregistered_generation(generation, terminal.id());
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.is_valid(), "{validation:#?}");
    }

    #[test]
    fn partition_accepts_pre_registration_esrch_before_later_task_gone_proof() {
        let observer = observer();
        let generation = generation();
        let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        observer.attach_generation(generation);
        observer.link_pre_registration_task(task, generation);
        let context = PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        };
        let wait = observer.begin_wait(context);
        let status = observer.finish_wait_status(wait, stopped_status(), None);
        observer.publish_external_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task,
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::PreRegistrationCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Error(libc::ESRCH));
        observer.tolerate_resume_error(resume, libc::ESRCH);
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        let terminal = observer.begin_wait(context);
        observer.finish_wait_error(terminal, libc::ECHILD);
        observer.record_pre_registration_task_gone(generation, task, libc::ESRCH);
        observer.finish_unregistered_generation(generation, terminal.id());
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.is_valid(), "{validation:#?}");
    }

    #[test]
    fn partition_rejects_duplicate_tolerated_resume_records() {
        for duplicate_errno in [libc::ESRCH, libc::EIO] {
            let observer = observer();
            let generation = generation();
            let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
            observer.attach_generation(generation);
            observer.link_pre_registration_task(task, generation);
            let context = PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::PreRegistrationCleanup,
                flags: libc::__WALL | libc::WNOHANG,
            };
            let wait = observer.begin_wait(context);
            let status = observer.finish_wait_status(wait, stopped_status(), None);
            observer.publish_external_cleanup_stop(generation, status);
            let resume = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task,
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::PreRegistrationCleanup,
            });
            observer.finish_resume(resume, PhysicalResumeOutcome::Error(libc::ESRCH));
            observer.tolerate_resume_error(resume, libc::ESRCH);
            observer.tolerate_resume_error(resume, duplicate_errno);
            observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
            let terminal = observer.begin_wait(context);
            observer.finish_wait_error(terminal, libc::ECHILD);
            observer.record_pre_registration_task_gone(generation, task, libc::ESRCH);
            observer.finish_unregistered_generation(generation, terminal.id());
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::DuplicateToleratedResumeError(observed)
                            if *observed == resume.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_pre_registration_wait_without_prior_generation_link() {
        for missing_generation in [false, true] {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: (!missing_generation).then_some(generation),
                task,
                producer: PhysicalWaitProducer::PreRegistrationCleanup,
                flags: libc::__WALL | libc::WNOHANG,
            });
            observer.finish_wait_error(wait, libc::ECHILD);
            observer.link_pre_registration_task(task, generation);
            observer.record_pre_registration_task_gone(generation, task, libc::ESRCH);
            observer.finish_unregistered_generation(generation, wait.id());
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(validation.violations.iter().any(|violation| {
                if missing_generation {
                    matches!(
                        violation,
                        PhysicalPartitionViolation::WaitWithoutGeneration(observed)
                            if *observed == wait.id()
                    )
                } else {
                    matches!(
                        violation,
                        PhysicalPartitionViolation::WaitBeforeIdentityBound(observed)
                            if *observed == wait.id()
                    )
                }
            }));
        }
    }

    #[test]
    fn partition_rejects_pre_registration_wait_after_or_across_worker_start() {
        for attempt_after_start in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
            observer.attach_generation(generation);
            observer.link_pre_registration_task(task, generation);
            observer.bind_identity(generation, captured_task(7, 11));
            let context = PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::PreRegistrationCleanup,
                flags: libc::__WALL | libc::WNOHANG,
            };
            let wait = if attempt_after_start {
                observer.record_worker_started(generation);
                observer.begin_wait(context)
            } else {
                let wait = observer.begin_wait(context);
                observer.record_worker_started(generation);
                wait
            };
            observer.finish_wait_no_status(wait, None);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidGenerationLifecycle(observed)
                            if *observed == generation
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_pre_registration_resume_after_or_across_worker_start() {
        for attempt_after_start in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
            observer.attach_generation(generation);
            observer.link_pre_registration_task(task, generation);
            observer.bind_identity(generation, captured_task(7, 11));
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::PreRegistrationCleanup,
                flags: libc::__WALL | libc::WNOHANG,
            });
            let status = observer.finish_wait_status(wait, stopped_status(), None);
            observer.publish_external_cleanup_stop(generation, status);
            let resume_context = PhysicalResumeContext {
                generation: Some(generation),
                task,
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::PreRegistrationCleanup,
            };
            let resume = if attempt_after_start {
                observer.record_worker_started(generation);
                observer.begin_resume(resume_context)
            } else {
                let resume = observer.begin_resume(resume_context);
                observer.record_worker_started(generation);
                resume
            };
            observer.finish_resume(resume, PhysicalResumeOutcome::Success);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidGenerationLifecycle(observed)
                            if *observed == generation
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_completed_pre_registration_resume_then_worker_start() {
        let observer = observer();
        let generation = generation();
        let direct = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        observer.attach_generation(generation);
        observer.link_pre_registration_task(direct, generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: direct,
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        });
        let status = observer.finish_wait_status(wait, stopped_status(), None);
        observer.publish_external_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: direct,
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::PreRegistrationCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);

        observer.bind_identity(generation, captured_task(7, 11));
        observer.record_worker_started(generation);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationLifecycle(observed)
                        if *observed == generation
                ))
        );
    }

    #[test]
    fn partition_rejects_malformed_pre_registration_raw_statuses() {
        for raw_status in [
            (255 << 8) | 0x7f,
            (0x1234 << 16) | (libc::SIGTRAP << 8) | 0x7f,
        ] {
            let observer = observer();
            let generation = generation();
            let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
            observer.attach_generation(generation);
            observer.link_pre_registration_task(task, generation);
            let context = PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::PreRegistrationCleanup,
                flags: libc::__WALL | libc::WNOHANG,
            };
            let wait = observer.begin_wait(context);
            let status = observer.finish_wait_status(wait, raw_status, None);
            observer.publish_external_cleanup_stop(generation, status);
            observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
            let terminal = observer.begin_wait(context);
            observer.finish_wait_error(terminal, libc::ECHILD);
            observer.record_pre_registration_task_gone(generation, task, libc::ESRCH);
            observer.finish_unregistered_generation(generation, terminal.id());
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidWaitOutcomeForFlags(observed)
                            if *observed == wait.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_accepts_synchronous_wait_completion_before_worker_start() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::SynchronousWait,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Synchronous);
        observer.record_decode_finished(
            reservation,
            status,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Synchronous,
        );
        observer.record_reservation_committed(reservation, status);
        observer.record_worker_started(generation);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_accepts_synchronous_retry_rollback_before_worker_start() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::SynchronousWait,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_ptrace_event_wait(&observer, wait, libc::PTRACE_EVENT_FORK);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let retry = observer.next_reservation();
        observer.record_reserved(generation, retry, status);
        observer.record_decode_started(retry, status, PhysicalDecodeOwner::Synchronous);
        observer.record_decode_finished(
            retry,
            status,
            PhysicalDecodeOutcome::RetryRolledBack,
            PhysicalDecodeOwner::Synchronous,
        );
        observer.record_ordinary_reservation_rolled_back(retry, status);
        observer.record_worker_started(generation);
        deliver_status(&observer, generation, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_synchronous_wait_straddling_worker_start() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::SynchronousWait,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.record_worker_started(generation);
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Synchronous);
        observer.record_decode_finished(
            reservation,
            status,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Synchronous,
        );
        observer.record_reservation_committed(reservation, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationLifecycle(observed)
                        if *observed == generation
                ))
        );
    }

    #[test]
    fn partition_rejects_wait_and_resume_before_late_identity_binding() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task,
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.bind_identity(generation, task);
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::WaitBeforeIdentityBound(attempt)
                if *attempt == wait.id()
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::ResumeBeforeIdentityBound(attempt)
                if *attempt == resume.id()
        )));
    }

    #[test]
    fn partition_adoption_merges_direct_and_exact_task_authority() {
        let observer = observer();
        let requested = generation();
        let authoritative = generation();
        let direct = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        let captured = captured_task(7, 11);
        observer.attach_generation(requested);
        observer.attach_generation(authoritative);
        observer.link_pre_registration_task(direct, requested);
        observer.bind_identity(requested, captured_task(7, 10));
        observer.bind_identity(authoritative, captured);
        observer.adopt_generation(requested, authoritative);
        observer.record_worker_started(authoritative);

        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(authoritative),
            task: captured,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            authoritative,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, authoritative, status);
        resume_typed_status(&observer, authoritative, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, authoritative, captured_task(7, 13));
        observer.close();
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_accepts_long_adoption_chain() {
        let observer = observer();
        let first = generation();
        let second = generation();
        let third = generation();
        let authoritative = generation();
        let direct = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        let captured = captured_task(7, 12);
        observer.attach_generation(first);
        observer.attach_generation(second);
        observer.attach_generation(third);
        observer.attach_generation(authoritative);
        observer.link_pre_registration_task(direct, first);
        observer.bind_identity(authoritative, captured_task(7, 10));
        observer.adopt_generation(first, second);
        observer.adopt_generation(second, third);
        observer.adopt_generation(third, authoritative);
        observer.record_worker_started(authoritative);

        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(authoritative),
            task: captured,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            authoritative,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, authoritative, status);
        resume_typed_status(&observer, authoritative, captured_task(7, 13), status);
        finish_notifier_generation_with_echild(&observer, authoritative, captured_task(7, 14));
        observer.close();
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_adoption_cycle() {
        let observer = observer();
        let prefix = generation();
        let first = generation();
        let second = generation();
        let third = generation();
        observer.attach_generation(prefix);
        observer.adopt_generation(prefix, first);
        observer.adopt_generation(first, second);
        observer.adopt_generation(second, third);
        observer.adopt_generation(third, first);
        observer.close();
        let validation = observer.snapshot().validate();
        for expected in [prefix, first, second, third] {
            assert!(validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidGenerationAdoption(observed)
                    if *observed == expected
            )));
        }
    }

    #[test]
    fn partition_rejects_adoption_through_conflicting_descendant() {
        let observer = observer();
        let first = generation();
        let conflicting = generation();
        let left = generation();
        let right = generation();
        let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        observer.attach_generation(first);
        observer.link_pre_registration_task(task, first);
        observer.adopt_generation(first, conflicting);
        observer.adopt_generation(conflicting, left);
        observer.adopt_generation(conflicting, right);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(first),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(wait, libc::ECHILD);
        observer.record_generation_finished(first);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidGenerationAdoption(observed)
                if *observed == conflicting
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::WaitWithoutGeneration(attempt)
                if *attempt == wait.id()
        )));
    }

    #[test]
    fn partition_rejects_conflicting_captured_stable_identity_fields() {
        let baseline = captured_task(7, 11);
        let mut changed = Vec::new();
        changed.push(PhysicalTaskIdentity { tid: 8, ..baseline });
        changed.push(PhysicalTaskIdentity {
            tgid: Some(8),
            ..baseline
        });
        changed.push(PhysicalTaskIdentity {
            start_time: Some(102),
            ..baseline
        });
        changed.push(PhysicalTaskIdentity {
            proc_inode: Some(104),
            ..baseline
        });
        for candidate in changed {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            observer.bind_identity(generation, baseline);
            observer.bind_identity(generation, candidate);
            observer.record_generation_finished(generation);
            observer.close();
            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::ConflictingTaskAuthority(observed)
                            if *observed == generation
                    )),
                "stable captured field mismatch was accepted: {candidate:?}"
            );
        }
    }

    #[test]
    fn partition_rejects_malformed_captured_identity_but_allows_zero_start_time() {
        let baseline = captured_task(7, 11);
        let malformed = [
            PhysicalTaskIdentity { tid: 0, ..baseline },
            PhysicalTaskIdentity {
                tgid: Some(0),
                ..baseline
            },
            PhysicalTaskIdentity {
                proc_inode: Some(0),
                ..baseline
            },
            PhysicalTaskIdentity {
                pidfd: Some(-1),
                ..baseline
            },
            PhysicalTaskIdentity {
                tgid: None,
                ..baseline
            },
            PhysicalTaskIdentity {
                start_time: None,
                ..baseline
            },
            PhysicalTaskIdentity {
                proc_inode: None,
                ..baseline
            },
            PhysicalTaskIdentity {
                pidfd: None,
                ..baseline
            },
        ];
        for candidate in malformed {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            observer.bind_identity(generation, candidate);
            observer.record_generation_finished(generation);
            observer.close();
            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::ConflictingTaskAuthority(observed)
                            if *observed == generation
                    )),
                "malformed captured identity was accepted: {candidate:?}"
            );
        }

        let observer = observer();
        let generation = generation();
        let zero_start_time =
            PhysicalTaskIdentity::captured(Pid::from_raw(7), Pid::from_raw(7), 0, 103, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, zero_start_time);
        observer.record_worker_started(generation);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: zero_start_time,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        observer.record_echild_pidfd_exited(
            terminal_wait,
            generation,
            zero_start_time,
            libc::POLLIN,
        );
        observer.record_synthetic_echild(generation, Some(terminal_wait.id()));
        observer.record_generation_finished(generation);
        observer.close();
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_duplicate_identity_binding_on_one_raw_generation() {
        let first = captured_task(7, 11);
        for duplicate in [first, captured_task(7, 12)] {
            let observer = observer();
            let generation = generation();
            observer.attach_generation(generation);
            observer.bind_identity(generation, first);
            observer.bind_identity(generation, duplicate);
            observer.record_worker_started(generation);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::DuplicateIdentityBinding(observed)
                            if *observed == generation
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_wait_and_resume_for_wrong_task() {
        let observer = observer();
        let generation = generation();
        let exact = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, exact);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(8, 13),
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: exact,
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        observer.record_generation_finished(generation);
        observer.close();
        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::WrongWaitTask(id) if *id == wait.id()
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::WrongResumeTask(id) if *id == resume.id()
        )));
    }

    #[test]
    fn partition_accepts_equivalent_captured_pidfds_across_wait_and_resume() {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        observer.bind_identity(generation, captured_task(7, 10));
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 11),
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_wait_siginfo_wrong_pid_and_raw_mismatch() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);

        let wrong_pid_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let wrong_pid_status = observer.allocate_status();
        let wrong_pid_siginfo = wait_siginfo(8);
        observer.record_wait_siginfo(wrong_pid_wait, wrong_pid_siginfo, Some(wrong_pid_status));
        observer.finish_wait_status_with_id(
            wrong_pid_wait,
            wrong_pid_status,
            stopped_status(),
            Some(wrong_pid_siginfo),
        );
        observer.record_status_published(
            generation,
            wrong_pid_status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, wrong_pid_status);
        resume_typed_status(
            &observer,
            generation,
            captured_task(7, 12),
            wrong_pid_status,
        );

        let mismatched_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let mismatched_status = observer.allocate_status();
        let recorded = wait_siginfo(7);
        let mut embedded = recorded;
        embedded.uid += 1;
        observer.record_wait_siginfo(mismatched_wait, recorded, Some(mismatched_status));
        observer.finish_wait_status_with_id(
            mismatched_wait,
            mismatched_status,
            stopped_status(),
            Some(embedded),
        );
        observer.record_status_published(
            generation,
            mismatched_status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, mismatched_status);
        resume_typed_status(
            &observer,
            generation,
            captured_task(7, 13),
            mismatched_status,
        );
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 14));
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::WaitSiginfoTaskMismatch(id)
                if *id == wrong_pid_wait.id()
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::WaitSiginfoStatusMismatch(id)
                if *id == mismatched_wait.id()
        )));
    }

    #[test]
    fn partition_rejects_arithmetic_status_rejected_by_real_converter() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = observer.allocate_status();
        let siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_STOPPED,
            pid: task.tid(),
            uid: 1000,
            status: libc::SIGTRAP | (libc::PTRACE_EVENT_FORK << 8),
        };
        let raw_status = (siginfo.status << 8) | 0x7f;
        assert_eq!(shared_waitid_status(siginfo), Err(crate::Errno::EPROTO));
        observer.record_wait_siginfo(wait, siginfo, Some(status));
        observer.finish_wait_status_with_id(wait, status, raw_status, Some(siginfo));
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::WaitSiginfoStatusMismatch(attempt)
                        if *attempt == wait.id()
                ))
        );
    }

    #[test]
    fn partition_accepts_pre_registration_no_status_with_wnohang() {
        let observer = observer();
        let generation = generation();
        let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        observer.attach_generation(generation);
        observer.link_pre_registration_task(task, generation);
        let context = PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        };
        let wait = observer.begin_wait(context);
        let siginfo = PhysicalWaitSiginfo {
            signo: 0,
            errno: 0,
            code: 0,
            pid: 0,
            uid: 0,
            status: 0,
        };
        assert!(siginfo_is_exact_no_status(siginfo));
        observer.finish_wait_no_status(wait, None);
        let terminal = observer.begin_wait(context);
        observer.finish_wait_error(terminal, libc::ECHILD);
        observer.record_pre_registration_task_gone(generation, task, libc::ESRCH);
        observer.finish_unregistered_generation(generation, terminal.id());
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.is_valid(), "{validation:#?}");
        assert_eq!(validation.physical_statuses, 0);
    }

    #[test]
    fn partition_rejects_wrong_wait_option_bits_for_each_producer() {
        for producer in [
            PhysicalWaitProducer::NotifierWorker,
            PhysicalWaitProducer::SynchronousWait,
            PhysicalWaitProducer::PreRegistrationCleanup,
            PhysicalWaitProducer::RegisteredCleanup,
        ] {
            let observer = observer();
            let generation = generation();
            let task = if producer == PhysicalWaitProducer::PreRegistrationCleanup {
                PhysicalTaskIdentity::direct_child(Pid::from_raw(7))
            } else {
                captured_task(7, 11)
            };
            observer.attach_generation(generation);
            if task.is_direct_child() {
                observer.link_pre_registration_task(task, generation);
            } else {
                observer.bind_identity(generation, task);
            }
            if matches!(
                producer,
                PhysicalWaitProducer::NotifierWorker | PhysicalWaitProducer::RegisteredCleanup
            ) {
                observer.record_worker_started(generation);
            }
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer,
                flags: production_wait_flags(producer) ^ libc::__WALL,
            });
            observer.finish_wait_error(wait, libc::EINVAL);
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidWaitFlags(observed)
                            if *observed == wait.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_cross_owner_continued_wait_flags() {
        for (producer, flags) in [
            (
                PhysicalWaitProducer::PreRegistrationBarrierCleanup,
                production_wait_flags(PhysicalWaitProducer::PreRegistrationBarrierCleanup)
                    & !libc::WCONTINUED,
            ),
            (
                PhysicalWaitProducer::RegisteredCleanup,
                production_wait_flags(PhysicalWaitProducer::RegisteredCleanup) | libc::WCONTINUED,
            ),
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            if producer == PhysicalWaitProducer::RegisteredCleanup {
                observer.record_worker_started(generation);
            }
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer,
                flags,
            });
            observer.finish_wait_error(wait, libc::EINVAL);
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidWaitFlags(observed)
                            if *observed == wait.id()
                    )),
                "cross-owner WCONTINUED flags were accepted for {producer:?}"
            );
        }
    }

    #[test]
    fn partition_rejects_no_status_without_wnohang_and_continued_status() {
        for continued in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::SynchronousWait,
                flags: production_wait_flags(PhysicalWaitProducer::SynchronousWait),
            });
            if continued {
                finish_continued_wait(&observer, wait);
            } else {
                let siginfo = PhysicalWaitSiginfo {
                    signo: 0,
                    errno: 0,
                    code: 0,
                    pid: 0,
                    uid: 0,
                    status: 0,
                };
                observer.record_wait_siginfo(wait, siginfo, None);
                observer.finish_wait_no_status(wait, Some(siginfo));
            }
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidWaitOutcomeForFlags(observed)
                            if *observed == wait.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_wait_siginfo_no_status_with_nonzero_pid() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let siginfo = wait_siginfo(7);
        observer.record_wait_siginfo(wait, siginfo, None);
        observer.finish_wait_no_status(wait, Some(siginfo));
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::WaitSiginfoTaskMismatch(attempt)
                        if *attempt == wait.id()
                ))
        );
    }

    #[test]
    fn partition_rejects_zero_pid_no_status_with_nonzero_fields() {
        let zero = PhysicalWaitSiginfo {
            signo: 0,
            errno: 0,
            code: 0,
            pid: 0,
            uid: 0,
            status: 0,
        };
        let malformed = [
            PhysicalWaitSiginfo { signo: 1, ..zero },
            PhysicalWaitSiginfo { errno: 1, ..zero },
            PhysicalWaitSiginfo { code: 1, ..zero },
            PhysicalWaitSiginfo { pid: 7, ..zero },
            PhysicalWaitSiginfo { uid: 1, ..zero },
            PhysicalWaitSiginfo { status: 1, ..zero },
        ];
        for siginfo in malformed {
            assert_eq!(shared_waitid_status(siginfo), Err(crate::Errno::EPROTO));
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            observer.record_wait_siginfo(wait, siginfo, None);
            observer.finish_wait_no_status(wait, Some(siginfo));
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::WaitSiginfoStatusMismatch(attempt)
                            if *attempt == wait.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_inflight_wait_result_after_finish_boundary() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let inflight = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let terminal = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal, libc::ECHILD);
        observer.record_echild_pidfd_exited(
            terminal,
            generation,
            terminal.context().task,
            libc::POLLIN,
        );
        observer.record_synthetic_echild(generation, Some(terminal.id()));
        let no_status = PhysicalWaitSiginfo {
            signo: 0,
            errno: 0,
            code: 0,
            pid: 0,
            uid: 0,
            status: 0,
        };
        observer.record_wait_siginfo(inflight, no_status, None);
        observer.finish_wait_no_status(inflight, Some(no_status));
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationLifecycle(observed)
                        if *observed == generation
                ))
        );
    }

    #[test]
    fn partition_accepts_exact_worker_and_synchronous_echild_proofs() {
        for producer in [
            PhysicalWaitProducer::NotifierWorker,
            PhysicalWaitProducer::SynchronousWait,
        ] {
            for proof_kind in 0..3 {
                let observer = observer();
                let generation = generation();
                let task = captured_task(7, 11);
                observer.attach_generation(generation);
                observer.bind_identity(generation, task);
                if producer == PhysicalWaitProducer::NotifierWorker {
                    observer.record_worker_started(generation);
                }
                let wait = observer.begin_wait(PhysicalWaitContext {
                    generation: Some(generation),
                    task,
                    producer,
                    flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
                });
                observer.finish_wait_error(wait, libc::ECHILD);
                if proof_kind == 0 {
                    observer.record_echild_pidfd_exited(wait, generation, task, libc::POLLIN);
                } else {
                    observer.record_echild_tracer_detached(
                        wait,
                        generation,
                        task,
                        if proof_kind == 1 { 0 } else { 12345 },
                    );
                }
                observer.record_synthetic_echild(generation, Some(wait.id()));
                observer.record_generation_finished(generation);
                observer.close();

                assert!(observer.snapshot().validate().is_valid());
            }
        }
    }

    #[test]
    fn partition_accepts_sync_wait_before_later_worker_start() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::SynchronousWait,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_ptrace_exit_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::SynchronousFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Synchronous);
        observer.record_decode_finished(
            reservation,
            status,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Synchronous,
        );
        observer.record_reservation_committed(reservation, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        observer.record_worker_started(generation);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_synchronous_wait_after_worker_start() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::SynchronousWait,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(wait, libc::ECHILD);
        observer.record_echild_pidfd_exited(wait, generation, task, libc::POLLIN);
        observer.record_synthetic_echild(generation, Some(wait.id()));
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationLifecycle(observed)
                        if *observed == generation
                ))
        );
    }

    #[test]
    fn partition_rejects_bare_or_mismatched_echild_terminal_proofs() {
        #[derive(Clone, Copy)]
        enum Malformation {
            Missing,
            BeforeResult,
            NonPollin,
            WrongTask,
            WrongGeneration,
            NegativeTracer,
            Duplicate,
            AfterPublication,
        }

        for malformation in [
            Malformation::Missing,
            Malformation::BeforeResult,
            Malformation::NonPollin,
            Malformation::WrongTask,
            Malformation::WrongGeneration,
            Malformation::NegativeTracer,
            Malformation::Duplicate,
            Malformation::AfterPublication,
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::SynchronousWait,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            if matches!(malformation, Malformation::BeforeResult) {
                observer.record_echild_pidfd_exited(wait, generation, task, libc::POLLIN);
            }
            observer.finish_wait_error(wait, libc::ECHILD);
            match malformation {
                Malformation::Missing | Malformation::BeforeResult => {}
                Malformation::NonPollin => {
                    observer.record_echild_pidfd_exited(wait, generation, task, libc::POLLERR);
                }
                Malformation::WrongTask => {
                    observer.record_echild_pidfd_exited(
                        wait,
                        generation,
                        captured_task(7, 12),
                        libc::POLLIN,
                    );
                }
                Malformation::WrongGeneration => {
                    observer.record_echild_pidfd_exited(
                        wait,
                        PhysicalEventGenerationId::allocate(),
                        task,
                        libc::POLLIN,
                    );
                }
                Malformation::NegativeTracer => {
                    observer.record_echild_tracer_detached(wait, generation, task, -1);
                }
                Malformation::Duplicate => {
                    observer.record_echild_pidfd_exited(wait, generation, task, libc::POLLIN);
                    observer.record_echild_tracer_detached(wait, generation, task, 0);
                }
                Malformation::AfterPublication => {}
            }
            observer.record_synthetic_echild(generation, Some(wait.id()));
            if matches!(malformation, Malformation::AfterPublication) {
                observer.record_echild_pidfd_exited(wait, generation, task, libc::POLLIN);
            }
            observer.record_generation_finished(generation);
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidEchildTerminalProof(observed)
                            if *observed == wait.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_accepts_exact_retained_terminal_replay_after_finish() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let terminal = finish_exited_wait(&observer, wait);
        observer.record_status_published(
            generation,
            terminal,
            PhysicalStatusPublication::RetainedTerminal,
        );
        observer.record_generation_finished(generation);
        let replay = observer.next_reservation();
        observer.record_reserved(generation, replay, terminal);
        observer.record_decode_started(replay, terminal, PhysicalDecodeOwner::Synchronous);
        observer.record_decode_finished(
            replay,
            terminal,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Synchronous,
        );
        observer.record_terminal_replayed(replay, terminal);
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_stopped_status_as_retained_terminal() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RetainedTerminal,
        );
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidStatusPublication(observed)
                        if *observed == status
                ))
        );
    }

    #[test]
    fn partition_accepts_exact_retained_terminal_replay_before_finish() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let terminal = publish_retained_terminal(&observer, generation, task);
        let replay = observer.next_reservation();
        observer.record_reserved(generation, replay, terminal);
        observer.record_decode_started(replay, terminal, PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            replay,
            terminal,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Notifier,
        );
        observer.record_terminal_replayed(replay, terminal);
        observer.record_generation_finished(generation);
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_accepts_retained_terminal_replay_straddling_finish() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let terminal = publish_retained_terminal(&observer, generation, task);
        let replay = observer.next_reservation();
        observer.record_reserved(generation, replay, terminal);
        observer.record_decode_started(replay, terminal, PhysicalDecodeOwner::Synchronous);
        observer.record_generation_finished(generation);
        observer.record_decode_finished(
            replay,
            terminal,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Synchronous,
        );
        observer.record_terminal_replayed(replay, terminal);
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_non_replay_reservation_after_finish() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let terminal = finish_exited_wait(&observer, wait);
        observer.record_status_published(
            generation,
            terminal,
            PhysicalStatusPublication::RetainedTerminal,
        );
        observer.record_generation_finished(generation);
        let replay = observer.next_reservation();
        observer.record_reserved(generation, replay, terminal);
        observer.record_decode_started(replay, terminal, PhysicalDecodeOwner::Cleanup);
        observer.record_decode_finished(
            replay,
            terminal,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Cleanup,
        );
        observer.record_terminal_replayed(replay, terminal);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationLifecycle(observed)
                        if *observed == generation
                ))
        );
    }

    #[test]
    fn partition_rejects_undecodable_status_without_cleanup_lifecycle() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let (status, cause_wait) = record_undecodable_status(&observer, generation, task);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert_eq!(validation.physical_statuses, 1);
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidUndecodableStatusLifecycle(observed)
                if *observed == status
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::MissingRegisteredCleanupEvidence(observed)
                if *observed == status
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::FatalWaitWithoutCleanupTransaction(observed)
                if *observed == cause_wait.id()
        )));
    }

    #[test]
    fn partition_rejects_undecodable_status_accepted_by_real_converter() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = observer.allocate_status();
        let siginfo = wait_siginfo(task.tid());
        observer.record_wait_siginfo(wait, siginfo, Some(status));
        observer.finish_wait_undecodable_status(wait, status, siginfo, libc::EPROTO);
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidUndecodableStatus(observed)
                        if *observed == status
                ))
        );
    }

    #[test]
    fn partition_accepts_undecodable_status_disposition_and_exact_echild_drain() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let (status, cause_wait) = record_undecodable_status(&observer, generation, task);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        observer.link_registered_cleanup_status(transaction, status);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.is_valid());
        assert_eq!(validation.physical_statuses, 1);
        assert_eq!(validation.explicit_dispositions, 1);
    }

    #[test]
    fn partition_accepts_registered_cleanup_pidfd_sigkill_before_exact_echild_drain() {
        for outcome in [
            PhysicalPidfdSignalOutcome::Success,
            PhysicalPidfdSignalOutcome::Error(libc::ESRCH),
        ] {
            let observer = observer();
            let generation = generation();
            let worker = captured_task(7, 11);
            let cleanup = captured_task(7, 12);
            observer.attach_generation(generation);
            observer.bind_identity(generation, worker);
            observer.record_worker_started(generation);
            let (status, cause_wait) = record_undecodable_status(&observer, generation, worker);
            let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
            observer.link_registered_cleanup_status(transaction, status);
            let signal = observer.begin_pidfd_signal(PhysicalPidfdSignalContext {
                generation,
                task: cleanup,
                transaction: transaction.id(),
                pidfd: cleanup.pidfd().expect("captured cleanup pidfd"),
                signal: libc::SIGKILL,
            });
            observer.finish_pidfd_signal(signal, outcome);
            let terminal_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task: cleanup,
                producer: PhysicalWaitProducer::RegisteredCleanup,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            observer.finish_wait_error(terminal_wait, libc::ECHILD);
            prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
            observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
            observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
            observer.record_generation_finished(generation);
            observer.close();

            assert!(observer.snapshot().validate().is_valid());
        }
    }

    #[test]
    fn partition_rejects_malformed_registered_cleanup_pidfd_sigkill() {
        #[derive(Clone, Copy)]
        enum Malformation {
            WrongGeneration,
            WrongTask,
            WrongPidfd,
            WrongSignal,
            WrongResult,
            AfterDrainStarted,
            Duplicate,
        }

        for malformation in [
            Malformation::WrongGeneration,
            Malformation::WrongTask,
            Malformation::WrongPidfd,
            Malformation::WrongSignal,
            Malformation::WrongResult,
            Malformation::AfterDrainStarted,
            Malformation::Duplicate,
        ] {
            let observer = observer();
            let generation = generation();
            let worker = captured_task(7, 11);
            let cleanup = captured_task(7, 12);
            observer.attach_generation(generation);
            observer.bind_identity(generation, worker);
            observer.record_worker_started(generation);
            let (status, cause_wait) = record_undecodable_status(&observer, generation, worker);
            let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
            observer.link_registered_cleanup_status(transaction, status);
            let terminal_wait =
                matches!(malformation, Malformation::AfterDrainStarted).then(|| {
                    observer.begin_wait(PhysicalWaitContext {
                        generation: Some(generation),
                        task: cleanup,
                        producer: PhysicalWaitProducer::RegisteredCleanup,
                        flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
                    })
                });
            let context = PhysicalPidfdSignalContext {
                generation: if matches!(malformation, Malformation::WrongGeneration) {
                    PhysicalEventGenerationId::allocate()
                } else {
                    generation
                },
                task: if matches!(malformation, Malformation::WrongTask) {
                    captured_task(8, 12)
                } else {
                    cleanup
                },
                transaction: transaction.id(),
                pidfd: if matches!(malformation, Malformation::WrongPidfd) {
                    13
                } else {
                    12
                },
                signal: if matches!(malformation, Malformation::WrongSignal) {
                    libc::SIGTERM
                } else {
                    libc::SIGKILL
                },
            };
            let signal = observer.begin_pidfd_signal(context);
            observer.finish_pidfd_signal(
                signal,
                if matches!(malformation, Malformation::WrongResult) {
                    PhysicalPidfdSignalOutcome::Error(libc::EIO)
                } else {
                    PhysicalPidfdSignalOutcome::Error(libc::ESRCH)
                },
            );
            if matches!(malformation, Malformation::Duplicate) {
                let duplicate = observer.begin_pidfd_signal(context);
                observer
                    .finish_pidfd_signal(duplicate, PhysicalPidfdSignalOutcome::Error(libc::ESRCH));
            }
            let terminal_wait = terminal_wait.unwrap_or_else(|| {
                observer.begin_wait(PhysicalWaitContext {
                    generation: Some(generation),
                    task: cleanup,
                    producer: PhysicalWaitProducer::RegisteredCleanup,
                    flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
                })
            });
            observer.finish_wait_error(terminal_wait, libc::ECHILD);
            prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
            observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
            observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
            observer.record_generation_finished(generation);
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidStartupCleanupPidfdSignal(observed)
                            if *observed == transaction.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_accepts_undecodable_status_controller_resume_and_terminal_drain() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let (status, cause_wait) = record_undecodable_status(&observer, generation, task);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        observer.link_registered_cleanup_status(transaction, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: Some(libc::SIGKILL),
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);

        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let terminal_status = observer.allocate_status();
        let terminal_siginfo = PhysicalWaitSiginfo {
            signo: libc::SIGCHLD,
            errno: 0,
            code: libc::CLD_EXITED,
            pid: 7,
            uid: 1000,
            status: 0,
        };
        observer.record_wait_siginfo(terminal_wait, terminal_siginfo, Some(terminal_status));
        observer.finish_wait_status_with_id(
            terminal_wait,
            terminal_status,
            0,
            Some(terminal_siginfo),
        );
        observer.link_registered_cleanup_status(transaction, terminal_status);
        observer.finish_registered_cleanup_terminal_status(generation, terminal_status);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.is_valid());
        assert_eq!(validation.physical_statuses, 2);
        assert_eq!(validation.successful_resumes, 1);
        assert_eq!(validation.explicit_dispositions, 1);
    }

    #[test]
    fn partition_accepts_fatal_error_to_exact_registered_echild_transaction() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_missing_or_mismatched_registered_echild_pidfd_proof() {
        #[derive(Clone, Copy)]
        enum Malformation {
            Missing,
            NonPollin,
            WrongTask,
            WrongGeneration,
            BeforeWaitResult,
            Duplicate,
        }

        for malformation in [
            Malformation::Missing,
            Malformation::NonPollin,
            Malformation::WrongTask,
            Malformation::WrongGeneration,
            Malformation::BeforeWaitResult,
            Malformation::Duplicate,
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
            let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
            let terminal_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                producer: PhysicalWaitProducer::RegisteredCleanup,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            if matches!(malformation, Malformation::BeforeWaitResult) {
                prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
            }
            observer.finish_wait_error(terminal_wait, libc::ECHILD);
            match malformation {
                Malformation::Missing | Malformation::BeforeWaitResult => {}
                Malformation::NonPollin => observer.record_registered_cleanup_pidfd_exited(
                    transaction,
                    terminal_wait,
                    generation,
                    terminal_wait.context().task,
                    libc::POLLERR,
                    None,
                ),
                Malformation::WrongTask => observer.record_registered_cleanup_pidfd_exited(
                    transaction,
                    terminal_wait,
                    generation,
                    captured_task(7, 13),
                    libc::POLLIN,
                    None,
                ),
                Malformation::WrongGeneration => observer.record_registered_cleanup_pidfd_exited(
                    transaction,
                    terminal_wait,
                    PhysicalEventGenerationId::allocate(),
                    terminal_wait.context().task,
                    libc::POLLIN,
                    None,
                ),
                Malformation::Duplicate => {
                    prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
                    prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
                }
            }
            observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
            observer.record_generation_finished(generation);
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(observed)
                            if *observed == transaction.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_empty_cleanup_transaction_without_fatal_cause() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(cause_wait, libc::ECHILD);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| {
                    matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(observed)
                            if *observed == transaction.id()
                    )
                })
        );
    }

    #[test]
    fn partition_rejects_unlinked_registered_cleanup_success() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let cleanup_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, cleanup_wait);
        observer.publish_registered_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: Some(libc::SIGKILL),
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 14),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| {
                    matches!(
                        violation,
                        PhysicalPartitionViolation::MissingRegisteredCleanupEvidence(observed)
                            if *observed == status
                    )
                })
        );
    }

    #[test]
    fn partition_rejects_registered_cleanup_resume_predating_transaction_or_status_link() {
        for attempt_before_start in [false, true] {
            for tolerated_error in [false, true] {
                let observer = observer();
                let generation = generation();
                let task = captured_task(7, 11);
                observer.attach_generation(generation);
                observer.bind_identity(generation, task);
                observer.record_worker_started(generation);
                let (status, cause_wait) = record_undecodable_status(&observer, generation, task);
                let transaction;
                let resume;
                if attempt_before_start {
                    resume = observer.begin_resume(PhysicalResumeContext {
                        generation: Some(generation),
                        task: captured_task(7, 12),
                        source_status: Some(status),
                        operation: PhysicalResumeOperation::Continue,
                        signal: Some(libc::SIGKILL),
                        owner: PhysicalResumeOwner::RootCleanup,
                    });
                    transaction = observer.begin_registered_cleanup_transaction(cause_wait);
                    observer.link_registered_cleanup_status(transaction, status);
                } else {
                    transaction = observer.begin_registered_cleanup_transaction(cause_wait);
                    resume = observer.begin_resume(PhysicalResumeContext {
                        generation: Some(generation),
                        task: captured_task(7, 12),
                        source_status: Some(status),
                        operation: PhysicalResumeOperation::Continue,
                        signal: Some(libc::SIGKILL),
                        owner: PhysicalResumeOwner::RootCleanup,
                    });
                    observer.link_registered_cleanup_status(transaction, status);
                }
                if tolerated_error {
                    observer.finish_resume(resume, PhysicalResumeOutcome::Error(libc::ESRCH));
                    observer.tolerate_resume_error(resume, libc::ESRCH);
                    observer.link_registered_cleanup_tolerated_resume(transaction, resume);
                } else {
                    observer.finish_resume(resume, PhysicalResumeOutcome::Success);
                }
                let terminal_wait = observer.begin_wait(PhysicalWaitContext {
                    generation: Some(generation),
                    task: captured_task(7, 13),
                    producer: PhysicalWaitProducer::RegisteredCleanup,
                    flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
                });
                observer.finish_wait_error(terminal_wait, libc::ECHILD);
                prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
                if tolerated_error {
                    observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
                }
                observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
                observer.record_generation_finished(generation);
                observer.close();

                assert!(observer.snapshot().validate().violations.iter().any(
                    |violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(observed)
                            if *observed == transaction.id()
                    )
                ));
            }
        }
    }

    #[test]
    fn partition_rejects_orphan_registered_cleanup_waits() {
        for error in [libc::EINTR, libc::EIO, libc::ECHILD] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::RegisteredCleanup,
                flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
            });
            observer.finish_wait_error(wait, error);
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidRegisteredCleanupWaitOwnership(observed)
                            if *observed == wait.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_registered_cleanup_wait_outside_transaction_interval() {
        for before_start in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
            let outside_wait_before = if before_start {
                let outside_wait = observer.begin_wait(PhysicalWaitContext {
                    generation: Some(generation),
                    task: captured_task(7, 12),
                    producer: PhysicalWaitProducer::RegisteredCleanup,
                    flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
                });
                observer.finish_wait_error(outside_wait, libc::EINTR);
                Some(outside_wait)
            } else {
                None
            };
            let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
            let terminal_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task: captured_task(7, 13),
                producer: PhysicalWaitProducer::RegisteredCleanup,
                flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
            });
            observer.finish_wait_error(terminal_wait, libc::ECHILD);
            prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
            observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
            let outside_wait = match outside_wait_before {
                Some(outside_wait) => outside_wait,
                None => {
                    let outside_wait = observer.begin_wait(PhysicalWaitContext {
                        generation: Some(generation),
                        task: captured_task(7, 14),
                        producer: PhysicalWaitProducer::RegisteredCleanup,
                        flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
                    });
                    observer.finish_wait_error(outside_wait, libc::EINTR);
                    outside_wait
                }
            };
            observer.record_generation_finished(generation);
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidRegisteredCleanupWaitOwnership(observed)
                            if *observed == outside_wait.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_registered_cleanup_undecodable_intermediate() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let intermediate = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
        });
        let status = observer.allocate_status();
        let siginfo = undecodable_wait_siginfo(7);
        observer.record_wait_siginfo(intermediate, siginfo, Some(status));
        observer.finish_wait_undecodable_status(intermediate, status, siginfo, libc::EPROTO);
        observer.link_registered_cleanup_status(transaction, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: Some(libc::SIGKILL),
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 14),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidRegisteredCleanupWaitOwnership(observed)
                        if *observed == intermediate.id()
                ))
        );
    }

    #[test]
    fn partition_rejects_registered_cleanup_eio_intermediate() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let stopped_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
        });
        let status = finish_stopped_wait(&observer, stopped_wait);
        observer.link_registered_cleanup_status(transaction, status);
        observer.publish_registered_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: Some(libc::SIGKILL),
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        let intermediate = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 14),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
        });
        observer.finish_wait_error(intermediate, libc::EIO);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 15),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: production_wait_flags(PhysicalWaitProducer::RegisteredCleanup),
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidRegisteredCleanupWaitOwnership(observed)
                        if *observed == intermediate.id()
                ))
        );
    }

    #[test]
    fn partition_accepts_registered_cleanup_stop_sigkill_handoff() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let cleanup_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, cleanup_wait);
        observer.link_registered_cleanup_status(transaction, status);
        observer.publish_registered_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: Some(libc::SIGKILL),
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 14),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_typed_stopped_resume_of_registered_cleanup_stop() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let cleanup_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, cleanup_wait);
        observer.link_registered_cleanup_status(transaction, status);
        observer.publish_registered_cleanup_stop(generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 14),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidResumeSourceStatus(observed)
                        if *observed == resume.id()
                ))
        );
    }

    #[test]
    fn partition_rejects_cleanup_terminal_action_after_transaction_completion() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let cause_wait = record_fatal_wait_error(&observer, generation, task, libc::EIO);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let terminal_status = finish_exited_wait(&observer, terminal_wait);
        observer.link_registered_cleanup_status(transaction, terminal_status);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.finish_registered_cleanup_terminal_status(generation, terminal_status);
        observer.record_generation_finished(generation);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| {
                    matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidRegisteredCleanupTransaction(observed)
                            if *observed == transaction.id()
                    )
                })
        );
    }

    #[test]
    fn partition_rejects_undecodable_status_publication_and_reservation() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let (status, cause_wait) = record_undecodable_status(&observer, generation, task);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        observer.link_registered_cleanup_status(transaction, status);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_ordinary_reservation_rolled_back(reservation, status);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert_eq!(validation.physical_statuses, 1);
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::UndecodableStatusEscaped(observed)
                if *observed == status
        )));
    }

    #[test]
    fn partition_rejects_raw_eproto_error_without_undecodable_outcome() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = observer.allocate_status();
        observer.record_wait_siginfo(wait, wait_siginfo(7), Some(status));
        observer.finish_wait_error(wait, libc::EPROTO);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert_eq!(validation.physical_statuses, 0);
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::WaitSiginfoStatusMismatch(observed)
                if *observed == wait.id()
        )));
    }

    #[test]
    fn partition_rejects_external_finish_from_non_cleanup_producer() {
        let observer = observer();
        let generation = generation();
        let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        observer.attach_generation(generation);
        observer.link_pre_registration_task(task, generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::__WALL | libc::WNOHANG,
        });
        observer.finish_wait_error(wait, libc::ECHILD);
        observer.finish_unregistered_generation(generation, wait.id());
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationFinishEvidence(id)
                        if *id == wait.id()
                ))
        );
    }

    #[test]
    fn partition_rejects_external_finish_for_wrong_generation() {
        let observer = observer();
        let original = generation();
        let wrong = generation();
        let original_task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        let wrong_task = PhysicalTaskIdentity::direct_child(Pid::from_raw(8));
        observer.attach_generation(original);
        observer.attach_generation(wrong);
        observer.link_pre_registration_task(original_task, original);
        observer.link_pre_registration_task(wrong_task, wrong);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(original),
            task: original_task,
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        });
        observer.finish_wait_error(wait, libc::ECHILD);
        observer.finish_unregistered_generation(wrong, wait.id());
        observer.finish_unregistered_generation(original, wait.id());
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationFinishEvidence(id)
                        if *id == wait.id()
                ))
        );
    }

    #[test]
    fn partition_rejects_dropped_status() {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        let wait = observer.begin_wait(wait_context(generation));
        finish_stopped_wait(&observer, wait);
        observer.close();
        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::StatusNotPublished(_)
        )));
    }

    #[test]
    fn partition_rejects_duplicate_status_identity() {
        let observer = observer();
        let generation = generation();
        observer.link_pre_registration_task(
            PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            generation,
        );
        let first = observer.begin_wait(wait_context(generation));
        let status = finish_stopped_wait(&observer, first);
        let second = observer.begin_wait(wait_context(generation));
        observer.inject_for_test(PhysicalEventRecordKind::WaitResult {
            attempt: second.id(),
            outcome: PhysicalWaitOutcome::Status {
                id: status,
                raw_status: stopped_status(),
                siginfo: None,
            },
        });
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::DuplicatePhysicalStatus(id) if *id == status
                ))
        );
    }

    #[test]
    fn partition_rejects_wrong_generation() {
        let observer = observer();
        let original = generation();
        let wrong = generation();
        observer.link_pre_registration_task(
            PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            original,
        );
        observer.link_pre_registration_task(
            PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            wrong,
        );
        let wait = observer.begin_wait(wait_context(original));
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(wrong, status, PhysicalStatusPublication::RegularFifo);
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::WrongGeneration(id) if *id == status
                ))
        );
    }

    #[test]
    fn partition_rejects_live_reservation() {
        let observer = observer();
        let generation = generation();
        let wait = observer.begin_wait(wait_context(generation));
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::LiveReservation(id) if *id == reservation
                ))
        );
    }

    #[test]
    fn partition_rejects_live_decode() {
        let observer = observer();
        let generation = generation();
        let wait = observer.begin_wait(wait_context(generation));
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
        observer.record_ordinary_reservation_rolled_back(reservation, status);
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::LiveDecode(id) if *id == reservation
                ))
        );
    }

    #[test]
    fn partition_accepts_reservation_drop_rollback_without_decode() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_ordinary_reservation_rolled_back(reservation, status);
        deliver_status(&observer, generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_accepts_exact_notifier_died_consumption() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_ptrace_event_wait(&observer, wait, libc::PTRACE_EVENT_FORK);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let reservation = observer.next_reservation();
        observer.record_reserved(generation, reservation, status);
        observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            reservation,
            status,
            PhysicalDecodeOutcome::DiedConsumed,
            PhysicalDecodeOwner::Notifier,
        );
        observer.finish_status(status, PhysicalStatusDisposition::DecodeDied);
        observer.record_reservation_committed(reservation, status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_malformed_died_consumption_cross_products() {
        #[derive(Clone, Copy)]
        enum Malformation {
            WrongOwner,
            MissingDisposition,
            EarlyDisposition,
            LateDisposition,
            RolledBack,
            TerminalReplayed,
        }

        for malformation in [
            Malformation::WrongOwner,
            Malformation::MissingDisposition,
            Malformation::EarlyDisposition,
            Malformation::LateDisposition,
            Malformation::RolledBack,
            Malformation::TerminalReplayed,
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = finish_ptrace_event_wait(&observer, wait, libc::PTRACE_EVENT_FORK);
            observer.record_status_published(
                generation,
                status,
                PhysicalStatusPublication::RegularFifo,
            );
            let reservation = observer.next_reservation();
            observer.record_reserved(generation, reservation, status);
            let owner = if matches!(malformation, Malformation::WrongOwner) {
                PhysicalDecodeOwner::Synchronous
            } else {
                PhysicalDecodeOwner::Notifier
            };
            observer.record_decode_started(reservation, status, owner);
            if matches!(malformation, Malformation::EarlyDisposition) {
                observer.finish_status(status, PhysicalStatusDisposition::DecodeDied);
            }
            observer.record_decode_finished(
                reservation,
                status,
                PhysicalDecodeOutcome::DiedConsumed,
                owner,
            );
            if !matches!(
                malformation,
                Malformation::MissingDisposition
                    | Malformation::EarlyDisposition
                    | Malformation::LateDisposition
            ) {
                observer.finish_status(status, PhysicalStatusDisposition::DecodeDied);
            }
            match malformation {
                Malformation::RolledBack => {
                    observer.record_ordinary_reservation_rolled_back(reservation, status)
                }
                Malformation::TerminalReplayed => {
                    observer.record_terminal_replayed(reservation, status)
                }
                _ => observer.record_reservation_committed(reservation, status),
            }
            if matches!(malformation, Malformation::LateDisposition) {
                observer.finish_status(status, PhysicalStatusDisposition::DecodeDied);
            }
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidDecodeReservationOutcome(observed)
                            if *observed == reservation
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_died_consumption_for_non_getevent_stops() {
        for exit_stop in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            if !exit_stop {
                observer.record_worker_started(generation);
            }
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: if exit_stop {
                    PhysicalWaitProducer::SynchronousWait
                } else {
                    PhysicalWaitProducer::NotifierWorker
                },
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = if exit_stop {
                finish_ptrace_exit_wait(&observer, wait)
            } else {
                finish_stopped_wait(&observer, wait)
            };
            observer.record_status_published(
                generation,
                status,
                if exit_stop {
                    PhysicalStatusPublication::SynchronousFifo
                } else {
                    PhysicalStatusPublication::RegularFifo
                },
            );
            let reservation = observer.next_reservation();
            observer.record_reserved(generation, reservation, status);
            observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
            observer.record_decode_finished(
                reservation,
                status,
                PhysicalDecodeOutcome::DiedConsumed,
                PhysicalDecodeOwner::Notifier,
            );
            observer.finish_status(status, PhysicalStatusDisposition::DecodeDied);
            observer.record_reservation_committed(reservation, status);
            if exit_stop {
                finish_synchronous_generation_with_echild(
                    &observer,
                    generation,
                    captured_task(7, 12),
                );
            } else {
                finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
            }
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidDecodeReservationOutcome(observed)
                            if *observed == reservation
                    ))
            );
        }
    }

    #[test]
    fn partition_accepts_synchronous_cancellation_success_and_tolerated_esrch() {
        for tolerated_esrch in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            let status = publish_fifo_stop(
                &observer,
                generation,
                task,
                PhysicalWaitProducer::SynchronousWait,
                PhysicalStatusPublication::RegularFifo,
            );
            let reservation = begin_cancelled_decode(
                &observer,
                generation,
                status,
                PhysicalDecodeOwner::Synchronous,
            );
            let resume = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::SynchronousCancellation,
            });
            if tolerated_esrch {
                observer.finish_resume(resume, PhysicalResumeOutcome::Error(libc::ESRCH));
                observer.tolerate_resume_error(resume, libc::ESRCH);
                observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
            } else {
                observer.finish_resume(resume, PhysicalResumeOutcome::Success);
            }
            observer.record_cleanup_reservation_committed(reservation, status);
            finish_synchronous_generation_with_echild(&observer, generation, captured_task(7, 13));
            observer.close();

            assert!(observer.snapshot().validate().is_valid());
        }
    }

    #[test]
    fn partition_rejects_retained_terminal_reclassified_as_cancellation() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::SynchronousWait,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_exited_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RetainedTerminal,
        );
        let reservation = begin_cancelled_decode(
            &observer,
            generation,
            status,
            PhysicalDecodeOwner::Synchronous,
        );
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.record_cleanup_reservation_committed(reservation, status);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::DuplicateStatusDisposition(observed)
                if *observed == status
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidDecodeReservationOutcome(observed)
                if *observed == reservation
        )));
    }

    #[test]
    fn partition_accepts_cancelled_rollback_then_exact_retry() {
        for first_owner in [
            PhysicalDecodeOwner::Synchronous,
            PhysicalDecodeOwner::Cleanup,
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            let synchronous = first_owner == PhysicalDecodeOwner::Synchronous;
            if !synchronous {
                observer.record_worker_started(generation);
            }
            let status = publish_fifo_stop(
                &observer,
                generation,
                task,
                if synchronous {
                    PhysicalWaitProducer::SynchronousWait
                } else {
                    PhysicalWaitProducer::NotifierWorker
                },
                PhysicalStatusPublication::RegularFifo,
            );
            let rollback = begin_cancelled_decode(&observer, generation, status, first_owner);
            if first_owner == PhysicalDecodeOwner::Synchronous {
                let failed = observer.begin_resume(PhysicalResumeContext {
                    generation: Some(generation),
                    task: captured_task(7, 12),
                    source_status: Some(status),
                    operation: PhysicalResumeOperation::Continue,
                    signal: None,
                    owner: PhysicalResumeOwner::SynchronousCancellation,
                });
                observer.finish_resume(failed, PhysicalResumeOutcome::Error(libc::EINVAL));
            }
            observer.record_cleanup_reservation_rolled_back(rollback, status);
            if synchronous {
                let retry = begin_cancelled_decode(
                    &observer,
                    generation,
                    status,
                    PhysicalDecodeOwner::Synchronous,
                );
                let resumed = observer.begin_resume(PhysicalResumeContext {
                    generation: Some(generation),
                    task: captured_task(7, 13),
                    source_status: Some(status),
                    operation: PhysicalResumeOperation::Continue,
                    signal: None,
                    owner: PhysicalResumeOwner::SynchronousCancellation,
                });
                observer.finish_resume(resumed, PhysicalResumeOutcome::Success);
                observer.record_cleanup_reservation_committed(retry, status);
                finish_synchronous_generation_with_echild(
                    &observer,
                    generation,
                    captured_task(7, 14),
                );
            } else {
                let retry = observer.next_reservation();
                observer.record_reserved(generation, retry, status);
                observer.record_decode_started(retry, status, PhysicalDecodeOwner::Notifier);
                observer.record_decode_finished(
                    retry,
                    status,
                    PhysicalDecodeOutcome::Returned,
                    PhysicalDecodeOwner::Notifier,
                );
                observer.record_reservation_committed(retry, status);
                resume_typed_status(&observer, generation, captured_task(7, 13), status);
                finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 14));
            }
            observer.close();

            assert!(observer.snapshot().validate().is_valid());
        }
    }

    #[test]
    fn partition_accepts_notifier_cancelled_handoff_to_cleanup() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let status = publish_fifo_stop(
            &observer,
            generation,
            task,
            PhysicalWaitProducer::NotifierWorker,
            PhysicalStatusPublication::RegularFifo,
        );
        let cancelled =
            begin_cancelled_decode(&observer, generation, status, PhysicalDecodeOwner::Notifier);
        observer.record_cleanup_reservation_rolled_back(cancelled, status);

        let cleanup = observer.next_reservation();
        observer.record_cleanup_reserved(generation, cleanup, status);
        observer.record_decode_started(cleanup, status, PhysicalDecodeOwner::Cleanup);
        observer.record_decode_finished(
            cleanup,
            status,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Cleanup,
        );
        observer.record_cleanup_reservation_committed(cleanup, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_accepts_notifier_cancelled_cleanup_drop_then_drain() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let status = publish_fifo_stop(
            &observer,
            generation,
            task,
            PhysicalWaitProducer::NotifierWorker,
            PhysicalStatusPublication::RegularFifo,
        );
        let cancelled =
            begin_cancelled_decode(&observer, generation, status, PhysicalDecodeOwner::Notifier);
        observer.record_cleanup_reservation_rolled_back(cancelled, status);

        let dropped_cleanup = observer.next_reservation();
        observer.record_cleanup_reserved(generation, dropped_cleanup, status);
        observer.record_cleanup_reservation_rolled_back(dropped_cleanup, status);

        let cleanup = observer.next_reservation();
        observer.record_cleanup_reserved(generation, cleanup, status);
        observer.record_decode_started(cleanup, status, PhysicalDecodeOwner::Cleanup);
        observer.record_decode_finished(
            cleanup,
            status,
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Cleanup,
        );
        observer.record_cleanup_reservation_committed(cleanup, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_accepts_repeated_cancelled_rollback_before_cleanup_drain() {
        for repeated_owner in [
            PhysicalDecodeOwner::Notifier,
            PhysicalDecodeOwner::Synchronous,
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let status = publish_fifo_stop(
                &observer,
                generation,
                task,
                PhysicalWaitProducer::NotifierWorker,
                PhysicalStatusPublication::RegularFifo,
            );
            let first = begin_cancelled_decode(
                &observer,
                generation,
                status,
                PhysicalDecodeOwner::Notifier,
            );
            observer.record_cleanup_reservation_rolled_back(first, status);
            let repeated = begin_cancelled_decode(&observer, generation, status, repeated_owner);
            observer.record_cleanup_reservation_rolled_back(repeated, status);

            let cleanup = observer.next_reservation();
            observer.record_cleanup_reserved(generation, cleanup, status);
            observer.record_decode_started(cleanup, status, PhysicalDecodeOwner::Cleanup);
            observer.record_decode_finished(
                cleanup,
                status,
                PhysicalDecodeOutcome::Returned,
                PhysicalDecodeOwner::Cleanup,
            );
            observer.record_cleanup_reservation_committed(cleanup, status);
            resume_typed_status(&observer, generation, captured_task(7, 12), status);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
            observer.close();

            assert!(observer.snapshot().validate().is_valid());
        }
    }

    #[test]
    fn partition_accepts_consumed_stop_cancelled_after_terminal_evidence() {
        for cleanup_delivery in [false, true] {
            for disposition_after_finish in [false, true] {
                let observer = observer();
                let generation = generation();
                let task = captured_task(7, 11);
                observer.attach_generation(generation);
                observer.bind_identity(generation, task);
                observer.record_worker_started(generation);
                let status = publish_fifo_stop(
                    &observer,
                    generation,
                    task,
                    PhysicalWaitProducer::NotifierWorker,
                    PhysicalStatusPublication::RegularFifo,
                );
                let delivery = observer.next_reservation();
                if cleanup_delivery {
                    observer.record_cleanup_reserved(generation, delivery, status);
                } else {
                    observer.record_reserved(generation, delivery, status);
                }
                let owner = if cleanup_delivery {
                    PhysicalDecodeOwner::Cleanup
                } else {
                    PhysicalDecodeOwner::Notifier
                };
                observer.record_decode_started(delivery, status, owner);
                observer.record_decode_finished(
                    delivery,
                    status,
                    PhysicalDecodeOutcome::Returned,
                    owner,
                );
                if cleanup_delivery {
                    observer.record_cleanup_reservation_committed(delivery, status);
                } else {
                    observer.record_reservation_committed(delivery, status);
                }
                let failed = observer.begin_resume(PhysicalResumeContext {
                    generation: Some(generation),
                    task: captured_task(7, 12),
                    source_status: Some(status),
                    operation: PhysicalResumeOperation::Continue,
                    signal: None,
                    owner: PhysicalResumeOwner::TypedStopped,
                });
                observer.finish_resume(failed, PhysicalResumeOutcome::Error(libc::EIO));
                publish_retained_terminal(&observer, generation, captured_task(7, 13));
                if disposition_after_finish {
                    observer.record_generation_finished(generation);
                    observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
                } else {
                    observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
                    observer.record_generation_finished(generation);
                }
                observer.close();

                let validation = observer.snapshot().validate();
                assert!(
                    validation.is_valid(),
                    "cleanup_delivery={cleanup_delivery} disposition_after_finish={disposition_after_finish}: {validation:#?}",
                );
            }
        }
    }

    #[test]
    fn partition_rejects_terminal_cancellation_without_consumed_stop() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let status = publish_fifo_stop(
            &observer,
            generation,
            task,
            PhysicalWaitProducer::NotifierWorker,
            PhysicalStatusPublication::RegularFifo,
        );
        publish_retained_terminal(&observer, generation, captured_task(7, 12));
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.record_generation_finished(generation);
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidStatusDisposition(observed)
                        if *observed == status
                ))
        );
    }

    #[test]
    fn partition_rejects_notifier_cancelled_rollback_then_returned_retry() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let status = publish_fifo_stop(
            &observer,
            generation,
            task,
            PhysicalWaitProducer::NotifierWorker,
            PhysicalStatusPublication::RegularFifo,
        );
        let cancelled =
            begin_cancelled_decode(&observer, generation, status, PhysicalDecodeOwner::Notifier);
        observer.record_cleanup_reservation_rolled_back(cancelled, status);
        deliver_status(&observer, generation, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidDecodeReservationOutcome(observed)
                        if *observed == cancelled
                ))
        );
    }

    #[test]
    fn partition_rejects_duplicate_sync_cancel_attempts_and_exit_stop_eio_tolerance() {
        for exit_stop_eio in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            let status = if exit_stop_eio {
                let wait = observer.begin_wait(PhysicalWaitContext {
                    generation: Some(generation),
                    task,
                    producer: PhysicalWaitProducer::SynchronousWait,
                    flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
                });
                let status = finish_ptrace_exit_wait(&observer, wait);
                observer.record_status_published(
                    generation,
                    status,
                    PhysicalStatusPublication::SynchronousFifo,
                );
                status
            } else {
                publish_fifo_stop(
                    &observer,
                    generation,
                    task,
                    PhysicalWaitProducer::SynchronousWait,
                    PhysicalStatusPublication::RegularFifo,
                )
            };
            let reservation = begin_cancelled_decode(
                &observer,
                generation,
                status,
                PhysicalDecodeOwner::Synchronous,
            );
            let first = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::SynchronousCancellation,
            });
            if exit_stop_eio {
                observer.finish_resume(first, PhysicalResumeOutcome::Error(libc::EIO));
                observer.tolerate_resume_error(first, libc::EIO);
                observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
            } else {
                observer.finish_resume(first, PhysicalResumeOutcome::Success);
                let duplicate = observer.begin_resume(first.context());
                observer.finish_resume(duplicate, PhysicalResumeOutcome::Success);
            }
            observer.record_cleanup_reservation_committed(reservation, status);
            finish_synchronous_generation_with_echild(&observer, generation, captured_task(7, 13));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidDecodeReservationOutcome(observed)
                            if *observed == reservation
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_sync_cancel_attempts_reusing_completed_reservation() {
        for (late_error, tolerate) in [
            (libc::EINVAL, false),
            (libc::ESRCH, true),
            (libc::EIO, true),
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            let status = publish_fifo_stop(
                &observer,
                generation,
                task,
                PhysicalWaitProducer::SynchronousWait,
                PhysicalStatusPublication::RegularFifo,
            );
            let reservation = begin_cancelled_decode(
                &observer,
                generation,
                status,
                PhysicalDecodeOwner::Synchronous,
            );
            let consumed = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::SynchronousCancellation,
            });
            observer.finish_resume(consumed, PhysicalResumeOutcome::Success);
            observer.record_cleanup_reservation_committed(reservation, status);

            let late = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 13),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::SynchronousCancellation,
            });
            observer.finish_resume(late, PhysicalResumeOutcome::Error(late_error));
            if tolerate {
                observer.tolerate_resume_error(late, late_error);
            }
            finish_synchronous_generation_with_echild(&observer, generation, captured_task(7, 14));
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidResumeSourceStatus(observed)
                    if *observed == late.id()
            )));
            if tolerate {
                assert!(validation.violations.iter().any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidToleratedResumeError(observed)
                        if *observed == late.id()
                )));
            }
        }
    }

    #[test]
    fn partition_rejects_sync_cancellation_without_sync_wait_authority() {
        for notifier_produced in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            if notifier_produced {
                observer.record_worker_started(generation);
            }
            let status = publish_fifo_stop(
                &observer,
                generation,
                task,
                if notifier_produced {
                    PhysicalWaitProducer::NotifierWorker
                } else {
                    PhysicalWaitProducer::SynchronousWait
                },
                PhysicalStatusPublication::RegularFifo,
            );
            if !notifier_produced {
                observer.record_worker_started(generation);
            }
            let reservation = begin_cancelled_decode(
                &observer,
                generation,
                status,
                PhysicalDecodeOwner::Synchronous,
            );
            let resume = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::SynchronousCancellation,
            });
            observer.finish_resume(resume, PhysicalResumeOutcome::Success);
            observer.record_cleanup_reservation_committed(reservation, status);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidDecodeReservationOutcome(observed)
                    if *observed == reservation
            )));
            assert!(validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidResumeSourceStatus(observed)
                    if *observed == resume.id()
            )));
        }
    }

    #[test]
    fn partition_rejects_overlapping_reservations_for_one_nonretained_status() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_ptrace_event_wait(&observer, wait, libc::PTRACE_EVENT_FORK);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let first = observer.next_reservation();
        observer.record_reserved(generation, first, status);
        observer.record_decode_started(first, status, PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            first,
            status,
            PhysicalDecodeOutcome::RetryRolledBack,
            PhysicalDecodeOwner::Notifier,
        );
        let second = observer.next_reservation();
        observer.record_reserved(generation, second, status);
        observer.record_ordinary_reservation_rolled_back(first, status);
        observer.record_decode_started(second, status, PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            second,
            status,
            PhysicalDecodeOutcome::RetryRolledBack,
            PhysicalDecodeOwner::Notifier,
        );
        observer.record_ordinary_reservation_rolled_back(second, status);
        deliver_status(&observer, generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::OverlappingStatusReservations(observed)
                        if *observed == status
                ))
        );
    }

    #[test]
    fn partition_rejects_reservations_for_non_fifo_destinations() {
        for destination in [
            PhysicalStatusPublication::DirectStopped,
            PhysicalStatusPublication::CleanupStopped,
            PhysicalStatusPublication::ExitCapability,
            PhysicalStatusPublication::ExternalCleanup,
            PhysicalStatusPublication::CleanupTerminal,
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = finish_stopped_wait(&observer, wait);
            observer.record_status_published(generation, status, destination);
            let reservation = observer.next_reservation();
            observer.record_reserved(generation, reservation, status);
            observer.record_ordinary_reservation_rolled_back(reservation, status);
            observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidReservationDestination(observed)
                            if *observed == reservation
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_two_consuming_commits_for_one_status() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        for _ in 0..2 {
            let reservation = observer.next_reservation();
            observer.record_reserved(generation, reservation, status);
            observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
            observer.record_decode_finished(
                reservation,
                status,
                PhysicalDecodeOutcome::Returned,
                PhysicalDecodeOwner::Notifier,
            );
            observer.record_reservation_committed(reservation, status);
        }
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::DuplicateStatusConsumption(observed)
                        if *observed == status
                ))
        );
    }

    #[test]
    fn partition_rejects_reserving_later_fifo_status_before_head() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let mut published = Vec::new();
        for pidfd in [12, 13] {
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task: captured_task(7, pidfd),
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = finish_stopped_wait(&observer, wait);
            observer.record_status_published(
                generation,
                status,
                PhysicalStatusPublication::RegularFifo,
            );
            published.push(status);
        }
        for status in [published[1], published[0]] {
            let reservation = observer.next_reservation();
            observer.record_reserved(generation, reservation, status);
            observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
            observer.record_decode_finished(
                reservation,
                status,
                PhysicalDecodeOutcome::Returned,
                PhysicalDecodeOwner::Notifier,
            );
            observer.record_reservation_committed(reservation, status);
            resume_typed_status(&observer, generation, captured_task(7, 14), status);
        }
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 15));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::OutOfOrderStatusReservation(observed)
                        if *observed == published[1]
                ))
        );
    }

    #[test]
    fn partition_accepts_fifo_head_rollback_retry_then_next_status() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let mut published = Vec::new();
        for (index, pidfd) in [12, 13].into_iter().enumerate() {
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task: captured_task(7, pidfd),
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = if index == 0 {
                finish_ptrace_event_wait(&observer, wait, libc::PTRACE_EVENT_FORK)
            } else {
                finish_stopped_wait(&observer, wait)
            };
            observer.record_status_published(
                generation,
                status,
                PhysicalStatusPublication::RegularFifo,
            );
            published.push(status);
        }
        let rollback = observer.next_reservation();
        observer.record_reserved(generation, rollback, published[0]);
        observer.record_decode_started(rollback, published[0], PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            rollback,
            published[0],
            PhysicalDecodeOutcome::RetryRolledBack,
            PhysicalDecodeOwner::Notifier,
        );
        observer.record_ordinary_reservation_rolled_back(rollback, published[0]);
        for status in published.iter().copied() {
            let reservation = observer.next_reservation();
            observer.record_reserved(generation, reservation, status);
            observer.record_decode_started(reservation, status, PhysicalDecodeOwner::Notifier);
            observer.record_decode_finished(
                reservation,
                status,
                PhysicalDecodeOutcome::Returned,
                PhysicalDecodeOwner::Notifier,
            );
            observer.record_reservation_committed(reservation, status);
            resume_typed_status(&observer, generation, captured_task(7, 14), status);
        }
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 15));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_retry_rollback_for_plain_signal_stop() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let status = publish_fifo_stop(
            &observer,
            generation,
            task,
            PhysicalWaitProducer::NotifierWorker,
            PhysicalStatusPublication::RegularFifo,
        );
        let rollback = observer.next_reservation();
        observer.record_reserved(generation, rollback, status);
        observer.record_decode_started(rollback, status, PhysicalDecodeOwner::Notifier);
        observer.record_decode_finished(
            rollback,
            status,
            PhysicalDecodeOutcome::RetryRolledBack,
            PhysicalDecodeOwner::Notifier,
        );
        observer.record_ordinary_reservation_rolled_back(rollback, status);
        deliver_status(&observer, generation, status);
        resume_typed_status(&observer, generation, captured_task(7, 12), status);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidDecodeReservationOutcome(observed)
                        if *observed == rollback
                ))
        );
    }

    #[test]
    fn partition_rejects_duplicate_resume() {
        let observer = observer();
        let generation = generation();
        let status = publish_and_resume(&observer, generation);
        let duplicate = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(duplicate, PhysicalResumeOutcome::Success);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::DuplicateSuccessfulResume(id) if *id == status
                ))
        );
    }

    #[test]
    fn partition_rejects_second_typed_resume_after_first_attempt_failed() {
        for first_error in [libc::EIO, libc::EINVAL] {
            for ordinary_handled in [false, true] {
                let observer = observer();
                let generation = generation();
                let task = captured_task(7, 11);
                observer.attach_generation(generation);
                observer.bind_identity(generation, task);
                observer.record_worker_started(generation);
                let status = publish_fifo_stop(
                    &observer,
                    generation,
                    task,
                    PhysicalWaitProducer::NotifierWorker,
                    PhysicalStatusPublication::RegularFifo,
                );
                let reservation = deliver_status(&observer, generation, status);
                let first = observer.begin_resume(PhysicalResumeContext {
                    generation: Some(generation),
                    task: captured_task(7, 12),
                    source_status: Some(status),
                    operation: PhysicalResumeOperation::Continue,
                    signal: None,
                    owner: PhysicalResumeOwner::TypedStopped,
                });
                observer.finish_resume(first, PhysicalResumeOutcome::Error(first_error));
                let second = observer.begin_resume(PhysicalResumeContext {
                    generation: Some(generation),
                    task: captured_task(7, 13),
                    source_status: Some(status),
                    operation: PhysicalResumeOperation::Continue,
                    signal: None,
                    owner: PhysicalResumeOwner::TypedStopped,
                });
                if ordinary_handled {
                    observer.finish_resume(second, PhysicalResumeOutcome::Error(libc::ESRCH));
                    observer.finish_status(status, PhysicalStatusDisposition::OrdinaryHandled);
                } else {
                    observer.finish_resume(second, PhysicalResumeOutcome::Success);
                }
                finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 14));
                observer.close();

                assert!(observer.snapshot().validate().violations.iter().any(
                    |violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidTypedResumeCardinality(observed)
                            if *observed == reservation
                    )
                ));
            }
        }
    }

    #[test]
    fn partition_accepts_ordinary_handled_after_exact_typed_esrch() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, status);
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::TypedStopped,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Error(libc::ESRCH));
        observer.finish_status(status, PhysicalStatusDisposition::OrdinaryHandled);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn partition_rejects_fabricated_ordinary_disposition() {
        {
            let disposition = PhysicalStatusDisposition::OrdinaryHandled;
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = finish_stopped_wait(&observer, wait);
            observer.record_status_published(
                generation,
                status,
                PhysicalStatusPublication::RegularFifo,
            );
            observer.finish_status(status, disposition);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidStatusDisposition(observed)
                            if *observed == status
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_resume_before_publication_or_without_delivery() {
        for publish_before_attempt in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = finish_stopped_wait(&observer, wait);
            if publish_before_attempt {
                observer.record_status_published(
                    generation,
                    status,
                    PhysicalStatusPublication::RegularFifo,
                );
            }
            let resume = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::TypedStopped,
            });
            if !publish_before_attempt {
                observer.record_status_published(
                    generation,
                    status,
                    PhysicalStatusPublication::RegularFifo,
                );
                deliver_status(&observer, generation, status);
            }
            observer.finish_resume(resume, PhysicalResumeOutcome::Success);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
            observer.close();

            let validation = observer.snapshot().validate();
            if publish_before_attempt {
                assert!(validation.violations.iter().any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidResumeSourceStatus(attempt)
                        if *attempt == resume.id()
                )));
            } else {
                assert!(validation.violations.iter().any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::ResumeBeforeStatusPublication(attempt)
                        if *attempt == resume.id()
                )));
            }
        }
    }

    #[test]
    fn partition_rejects_terminal_and_continued_resume_sources() {
        for continued in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = if continued {
                finish_continued_wait(&observer, wait)
            } else {
                finish_exited_wait(&observer, wait)
            };
            observer.record_status_published(
                generation,
                status,
                if continued {
                    PhysicalStatusPublication::RegularFifo
                } else {
                    PhysicalStatusPublication::RetainedTerminal
                },
            );
            if continued {
                deliver_status(&observer, generation, status);
            }
            let resume = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::TypedStopped,
            });
            observer.finish_resume(resume, PhysicalResumeOutcome::Success);
            if continued {
                finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
            } else {
                observer.record_generation_finished(generation);
            }
            observer.close();

            assert!(
                observer
                    .snapshot()
                    .validate()
                    .violations
                    .iter()
                    .any(|violation| matches!(
                        violation,
                        PhysicalPartitionViolation::InvalidResumeSourceStatus(observed)
                            if *observed == resume.id()
                    ))
            );
        }
    }

    #[test]
    fn partition_rejects_unmatched_cleanup_resume() {
        let observer = observer();
        let resume = observer.begin_resume(PhysicalResumeContext {
            generation: None,
            task: PhysicalTaskIdentity::direct_child(Pid::from_raw(7)),
            source_status: Some(PhysicalStatusId(991)),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(resume, PhysicalResumeOutcome::Success);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::UnknownPhysicalStatus(PhysicalStatusId(991))
                ))
        );
    }

    #[test]
    fn partition_rejects_cleanup_resume_operation_and_signal_mismatches() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        let bad_shapes = [
            (
                PhysicalResumeOwner::TypedStopped,
                PhysicalResumeOperation::Continue,
                Some(999),
            ),
            (
                PhysicalResumeOwner::PreRegistrationCleanup,
                PhysicalResumeOperation::SingleStep,
                None,
            ),
            (
                PhysicalResumeOwner::SynchronousCancellation,
                PhysicalResumeOperation::Detach,
                None,
            ),
            (
                PhysicalResumeOwner::RootCleanup,
                PhysicalResumeOperation::Continue,
                Some(libc::SIGUSR1),
            ),
            (
                PhysicalResumeOwner::DescendantCleanup,
                PhysicalResumeOperation::Syscall,
                None,
            ),
        ];
        let attempts = bad_shapes.map(|(owner, operation, signal)| {
            let attempt = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task,
                source_status: Some(status),
                operation,
                signal,
                owner,
            });
            observer.finish_resume(attempt, PhysicalResumeOutcome::Error(libc::EINVAL));
            attempt.id()
        });
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 12));
        observer.close();

        let validation = observer.snapshot().validate();
        for attempt in attempts {
            assert!(validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidCleanupResumeShape(observed)
                    if *observed == attempt
            )));
        }
    }

    #[test]
    fn partition_rejects_registered_wait_failure_resume_without_sigkill() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let (status, cause_wait) = record_undecodable_status(&observer, generation, task);
        let transaction = observer.begin_registered_cleanup_transaction(cause_wait);
        observer.link_registered_cleanup_status(transaction, status);
        let root = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::RootCleanup,
        });
        observer.finish_resume(root, PhysicalResumeOutcome::Error(libc::EINVAL));
        let synchronous = observer.begin_resume(PhysicalResumeContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            source_status: Some(status),
            operation: PhysicalResumeOperation::Continue,
            signal: None,
            owner: PhysicalResumeOwner::SynchronousCancellation,
        });
        observer.finish_resume(synchronous, PhysicalResumeOutcome::Error(libc::EINVAL));
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        let terminal_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 14),
            producer: PhysicalWaitProducer::RegisteredCleanup,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        observer.finish_wait_error(terminal_wait, libc::ECHILD);
        prove_registered_cleanup_echild(&observer, transaction, terminal_wait);
        observer.finish_registered_cleanup_transaction(transaction, terminal_wait);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        for attempt in [root.id(), synchronous.id()] {
            assert!(validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidCleanupResumeShape(observed)
                    if *observed == attempt
            )));
        }
    }

    #[test]
    fn partition_rejects_live_notifier_worker() {
        let observer = observer();
        let generation = generation();
        observer.attach_generation(generation);
        observer.record_worker_started(generation);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::LiveNotifierWorker(id) if *id == generation
                ))
        );
    }

    #[test]
    fn partition_rejects_duplicate_exit_capability() {
        let observer = observer();
        let generation = generation();
        let wait = observer.begin_wait(wait_context(generation));
        let status = finish_ptrace_exit_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::ExitCapability,
        );
        observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Published);
        observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Published);
        observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Revoked);
        observer.finish_status(status, PhysicalStatusDisposition::CancellationCleanup);
        observer.close();
        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::DuplicateExitCapabilityPublication(id)
                        if *id == status
                ))
        );
    }

    #[test]
    fn partition_rejects_every_second_exit_capability_resume_attempt() {
        for retry_prestarted in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
            });
            let status = finish_ptrace_exit_wait(&observer, wait);
            observer.record_status_published(
                generation,
                status,
                PhysicalStatusPublication::ExitCapability,
            );
            observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Published);
            observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Revoked);
            let context = PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::RootCleanup,
            };
            let failed = observer.begin_resume(context);
            let prestarted = retry_prestarted.then(|| observer.begin_resume(context));
            observer.finish_resume(failed, PhysicalResumeOutcome::Error(libc::ESRCH));
            let retry = prestarted.unwrap_or_else(|| observer.begin_resume(context));
            let final_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
            });
            let final_status = finish_exited_wait(&observer, final_wait);
            observer.record_status_published(
                generation,
                final_status,
                PhysicalStatusPublication::RetainedTerminal,
            );
            observer.resolve_ambiguous_resume_with_final_status(failed, final_status);
            observer.finish_resume(retry, PhysicalResumeOutcome::Success);
            observer.record_generation_finished(generation);
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(validation.violations.iter().any(|violation| matches!(
                violation,
                PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(observed)
                    if *observed == failed.id()
            )));
        }
    }

    #[test]
    fn partition_requires_causal_resolution_despite_generic_disposition() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let (source, failed) =
            begin_ambiguous_exit_resume(&observer, generation, task, libc::ESRCH);
        observer.finish_status(source, PhysicalStatusDisposition::CancellationCleanup);
        finish_notifier_generation_with_echild(&observer, generation, task);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(observed)
                if *observed == failed.id()
        )));
    }

    #[test]
    fn partition_accepts_ambiguous_exit_resolved_by_later_status() {
        for error in [libc::ESRCH, libc::EIO] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let (source, failed) = begin_ambiguous_exit_resume(&observer, generation, task, error);

            let successor_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
            });
            let successor = finish_stopped_wait(&observer, successor_wait);
            observer.record_status_published(
                generation,
                successor,
                PhysicalStatusPublication::RegularFifo,
            );
            observer.resolve_ambiguous_resume_with_later_status(failed, successor);

            let reservation = observer.next_reservation();
            observer.record_reserved(generation, reservation, successor);
            observer.record_decode_started(reservation, successor, PhysicalDecodeOwner::Notifier);
            observer.record_decode_finished(
                reservation,
                successor,
                PhysicalDecodeOutcome::Returned,
                PhysicalDecodeOwner::Notifier,
            );
            observer.record_reservation_committed(reservation, successor);
            let successor_resume = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task,
                source_status: Some(successor),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::TypedStopped,
            });
            observer.finish_resume(successor_resume, PhysicalResumeOutcome::Success);
            finish_notifier_generation_with_echild(&observer, generation, task);
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(validation.is_valid(), "{validation:#?}");
            assert_eq!(validation.successful_resumes, 1);
            assert!(observer.snapshot().records().iter().any(|record| matches!(
                record.kind(),
                PhysicalEventRecordKind::StatusDisposition {
                    status,
                    disposition: PhysicalStatusDisposition::AmbiguousResumeCausallyResolved {
                        attempt,
                        proof: PhysicalAmbiguousResumeProof::LaterStatus(proof),
                    },
                } if status == source && attempt == failed.id() && proof == successor
            )));
        }
    }

    #[test]
    fn partition_accepts_ambiguous_exit_terminal_proofs() {
        for use_echild in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let exit_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
            });
            let source = finish_ptrace_exit_wait(&observer, exit_wait);
            observer.record_status_published(
                generation,
                source,
                PhysicalStatusPublication::ExitCapability,
            );
            observer.record_exit_capability(source, PhysicalExitCapabilityTransition::Published);
            observer.record_exit_capability(source, PhysicalExitCapabilityTransition::Revoked);
            let terminal_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
            });
            let failed = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task,
                source_status: Some(source),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner: PhysicalResumeOwner::RootCleanup,
            });
            observer.finish_resume(failed, PhysicalResumeOutcome::Error(libc::EIO));
            let proof = if use_echild {
                observer.finish_wait_error(terminal_wait, libc::ECHILD);
                observer.record_echild_pidfd_exited(terminal_wait, generation, task, libc::POLLIN);
                observer.record_synthetic_echild(generation, Some(terminal_wait.id()));
                observer.resolve_ambiguous_resume_with_proven_echild(failed, terminal_wait);
                PhysicalAmbiguousResumeProof::ProvenEchild(terminal_wait.id())
            } else {
                let final_status = finish_exited_wait(&observer, terminal_wait);
                observer.record_status_published(
                    generation,
                    final_status,
                    PhysicalStatusPublication::RetainedTerminal,
                );
                observer.resolve_ambiguous_resume_with_final_status(failed, final_status);
                PhysicalAmbiguousResumeProof::FinalStatus(final_status)
            };
            observer.record_generation_finished(generation);
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(validation.is_valid(), "{validation:#?}");
            assert!(observer.snapshot().records().iter().any(|record| matches!(
                record.kind(),
                PhysicalEventRecordKind::StatusDisposition {
                    status,
                    disposition: PhysicalStatusDisposition::AmbiguousResumeCausallyResolved {
                        attempt,
                        proof: observed,
                    },
                } if status == source && attempt == failed.id() && observed == proof
            )));
        }
    }

    #[test]
    fn partition_rejects_ambiguous_exit_resolution_after_generation_finish() {
        for use_echild in [false, true] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let (source, failed) =
                begin_ambiguous_exit_resume(&observer, generation, task, libc::EIO);
            let terminal_wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
            });
            let proof = if use_echild {
                observer.finish_wait_error(terminal_wait, libc::ECHILD);
                observer.record_echild_pidfd_exited(terminal_wait, generation, task, libc::POLLIN);
                observer.record_synthetic_echild(generation, Some(terminal_wait.id()));
                observer.record_generation_finished(generation);
                observer.resolve_ambiguous_resume_with_proven_echild(failed, terminal_wait);
                PhysicalAmbiguousResumeProof::ProvenEchild(terminal_wait.id())
            } else {
                let final_status = finish_exited_wait(&observer, terminal_wait);
                observer.record_status_published(
                    generation,
                    final_status,
                    PhysicalStatusPublication::RetainedTerminal,
                );
                observer.record_generation_finished(generation);
                observer.resolve_ambiguous_resume_with_final_status(failed, final_status);
                PhysicalAmbiguousResumeProof::FinalStatus(final_status)
            };
            observer.close();

            let validation = observer.snapshot().validate();
            assert!(
                validation.violations.contains(
                    &PhysicalPartitionViolation::InvalidGenerationLifecycle(generation),
                ),
                "post-finish {proof:?} resolution was accepted: {validation:#?}",
            );
            assert!(!validation.violations.contains(
                &PhysicalPartitionViolation::InvalidAmbiguousResumeResolution(failed.id()),
            ));
            assert!(observer.snapshot().records().iter().any(|record| matches!(
                record.kind(),
                PhysicalEventRecordKind::StatusDisposition {
                    status,
                    disposition: PhysicalStatusDisposition::AmbiguousResumeCausallyResolved {
                        attempt,
                        proof: observed,
                    },
                } if status == source && attempt == failed.id() && observed == proof
            )));
        }
    }

    #[test]
    fn partition_rejects_duplicate_ambiguous_resolution() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let (source, failed) = begin_ambiguous_exit_resume(&observer, generation, task, libc::EIO);
        let final_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: production_wait_flags(PhysicalWaitProducer::NotifierWorker),
        });
        let final_status = finish_exited_wait(&observer, final_wait);
        observer.record_status_published(
            generation,
            final_status,
            PhysicalStatusPublication::RetainedTerminal,
        );
        observer.resolve_ambiguous_resume_with_final_status(failed, final_status);
        observer.resolve_ambiguous_resume_with_final_status(failed, final_status);
        observer.record_generation_finished(generation);
        observer.close();

        let validation = observer.snapshot().validate();
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::DuplicateAmbiguousResumeResolution(observed)
                if *observed == failed.id()
        )));
        assert!(validation.violations.iter().any(|violation| matches!(
            violation,
            PhysicalPartitionViolation::DuplicateStatusDisposition(observed)
                if *observed == source
        )));
    }

    #[test]
    fn partition_accepts_claimed_revoked_and_transferred_exit_capability_resumes() {
        enum Path {
            TypedClaimed,
            CleanupRevoked,
            CleanupTransferred,
        }

        for path in [
            Path::TypedClaimed,
            Path::CleanupRevoked,
            Path::CleanupTransferred,
        ] {
            let observer = observer();
            let generation = generation();
            let task = captured_task(7, 11);
            observer.attach_generation(generation);
            observer.bind_identity(generation, task);
            observer.record_worker_started(generation);
            let wait = observer.begin_wait(PhysicalWaitContext {
                generation: Some(generation),
                task,
                producer: PhysicalWaitProducer::NotifierWorker,
                flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
            });
            let status = finish_ptrace_exit_wait(&observer, wait);
            observer.record_status_published(
                generation,
                status,
                PhysicalStatusPublication::ExitCapability,
            );
            observer.record_exit_capability(status, PhysicalExitCapabilityTransition::Published);
            let owner = match path {
                Path::TypedClaimed => {
                    observer
                        .record_exit_capability(status, PhysicalExitCapabilityTransition::Claimed);
                    PhysicalResumeOwner::TypedStopped
                }
                Path::CleanupRevoked => {
                    observer
                        .record_exit_capability(status, PhysicalExitCapabilityTransition::Revoked);
                    PhysicalResumeOwner::RootCleanup
                }
                Path::CleanupTransferred => {
                    observer
                        .record_exit_capability(status, PhysicalExitCapabilityTransition::Claimed);
                    observer.record_exit_capability(
                        status,
                        PhysicalExitCapabilityTransition::TransferredToCleanup,
                    );
                    PhysicalResumeOwner::DescendantCleanup
                }
            };
            let resume = observer.begin_resume(PhysicalResumeContext {
                generation: Some(generation),
                task: captured_task(7, 12),
                source_status: Some(status),
                operation: PhysicalResumeOperation::Continue,
                signal: None,
                owner,
            });
            observer.finish_resume(resume, PhysicalResumeOutcome::Success);
            finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
            observer.close();

            assert!(observer.snapshot().validate().is_valid());
        }
    }

    #[test]
    fn identical_raw_statuses_keep_distinct_physical_ids() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let first_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let first = finish_stopped_wait(&observer, first_wait);
        observer.record_status_published(generation, first, PhysicalStatusPublication::RegularFifo);
        deliver_status(&observer, generation, first);
        resume_typed_status(&observer, generation, captured_task(7, 12), first);
        let second_wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 13),
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let second = finish_stopped_wait(&observer, second_wait);
        observer.record_status_published(
            generation,
            second,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, second);
        resume_typed_status(&observer, generation, captured_task(7, 14), second);
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 15));
        observer.close();
        assert_ne!(first, second);
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn observer_sessions_allocate_distinct_statuses_and_reject_foreign_status() {
        let first_observer = observer();
        let second_observer = observer();
        let first_generation = generation();
        let second_generation = generation();
        let first_task = captured_task(7, 11);
        let second_task = captured_task(8, 21);
        first_observer.attach_generation(first_generation);
        first_observer.bind_identity(first_generation, first_task);
        first_observer.record_worker_started(first_generation);
        second_observer.attach_generation(second_generation);
        second_observer.bind_identity(second_generation, second_task);
        second_observer.record_worker_started(second_generation);

        let first_wait = first_observer.begin_wait(PhysicalWaitContext {
            generation: Some(first_generation),
            task: first_task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let first_status = finish_stopped_wait(&first_observer, first_wait);
        let second_wait = second_observer.begin_wait(PhysicalWaitContext {
            generation: Some(second_generation),
            task: second_task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let second_status = finish_stopped_wait(&second_observer, second_wait);
        assert_ne!(first_status, second_status);

        first_observer.record_status_published(
            first_generation,
            first_status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&first_observer, first_generation, first_status);
        resume_typed_status(
            &first_observer,
            first_generation,
            captured_task(7, 12),
            first_status,
        );
        finish_notifier_generation_with_echild(
            &first_observer,
            first_generation,
            captured_task(7, 13),
        );
        first_observer.close();
        assert!(first_observer.snapshot().validate().is_valid());

        second_observer.record_status_published(
            second_generation,
            second_status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&second_observer, second_generation, second_status);
        resume_typed_status(
            &second_observer,
            second_generation,
            captured_task(8, 22),
            second_status,
        );
        second_observer.record_status_published(
            second_generation,
            first_status,
            PhysicalStatusPublication::RegularFifo,
        );
        finish_notifier_generation_with_echild(
            &second_observer,
            second_generation,
            captured_task(8, 23),
        );
        second_observer.close();
        assert!(
            second_observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::UnknownPhysicalStatus(status)
                        if *status == first_status
                ))
        );
    }

    #[test]
    fn external_finish_rejects_foreign_observer_wait_id() {
        let first_observer = observer();
        let second_observer = observer();
        let first_generation = generation();
        let second_generation = generation();
        let first_task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        let second_task = PhysicalTaskIdentity::direct_child(Pid::from_raw(8));
        first_observer.attach_generation(first_generation);
        first_observer.link_pre_registration_task(first_task, first_generation);
        second_observer.attach_generation(second_generation);
        second_observer.link_pre_registration_task(second_task, second_generation);
        let first_wait = first_observer.begin_wait(PhysicalWaitContext {
            generation: Some(first_generation),
            task: first_task,
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        });
        first_observer.finish_wait_error(first_wait, libc::ECHILD);
        first_observer.record_pre_registration_task_gone(first_generation, first_task, libc::ESRCH);
        let second_wait = second_observer.begin_wait(PhysicalWaitContext {
            generation: Some(second_generation),
            task: second_task,
            producer: PhysicalWaitProducer::PreRegistrationCleanup,
            flags: libc::__WALL | libc::WNOHANG,
        });
        second_observer.finish_wait_error(second_wait, libc::ECHILD);
        assert_ne!(first_wait.id(), second_wait.id());

        first_observer.finish_unregistered_generation(first_generation, first_wait.id());
        first_observer.close();
        let first_validation = first_observer.snapshot().validate();
        assert!(first_validation.is_valid(), "{first_validation:#?}");

        second_observer.finish_unregistered_generation(second_generation, first_wait.id());
        second_observer.close();
        assert!(
            second_observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidGenerationFinishEvidence(id)
                        if *id == first_wait.id()
                ))
        );
    }

    #[test]
    fn kernel_superseded_stop_has_one_explicit_disposition() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, status);
        publish_expired_exit_capability(&observer, generation, captured_task(7, 12));
        observer.finish_status(
            status,
            PhysicalStatusDisposition::KernelSupersededByExitStop,
        );
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();
        assert!(observer.snapshot().validate().is_valid());
    }

    #[test]
    fn kernel_superseded_rejects_missing_consuming_delivery() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        publish_expired_exit_capability(&observer, generation, captured_task(7, 12));
        observer.finish_status(
            status,
            PhysicalStatusDisposition::KernelSupersededByExitStop,
        );
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidStatusDisposition(observed)
                        if *observed == status
                ))
        );
    }

    #[test]
    fn kernel_superseded_rejects_exit_stop_created_before_ordinary_stop() {
        let observer = observer();
        let generation = generation();
        let task = captured_task(7, 11);
        observer.attach_generation(generation);
        observer.bind_identity(generation, task);
        observer.record_worker_started(generation);
        publish_expired_exit_capability(&observer, generation, task);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task: captured_task(7, 12),
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let ordinary = finish_stopped_wait(&observer, wait);
        observer.record_status_published(
            generation,
            ordinary,
            PhysicalStatusPublication::RegularFifo,
        );
        deliver_status(&observer, generation, ordinary);
        observer.finish_status(
            ordinary,
            PhysicalStatusDisposition::KernelSupersededByExitStop,
        );
        finish_notifier_generation_with_echild(&observer, generation, captured_task(7, 13));
        observer.close();

        assert!(
            observer
                .snapshot()
                .validate()
                .violations
                .iter()
                .any(|violation| matches!(
                    violation,
                    PhysicalPartitionViolation::InvalidStatusDisposition(observed)
                        if *observed == ordinary
                ))
        );
    }

    #[test]
    fn close_waits_for_writer_registered_before_state_check() {
        let observer = observer();
        observer.pause_next_writer_after_registration_for_test();

        let writer_observer = observer.clone();
        let generation = generation();
        let writer = std::thread::spawn(move || writer_observer.attach_generation(generation));
        while !observer.writer_is_paused_after_registration_for_test() {
            std::thread::yield_now();
        }
        let registered_writers = observer.inner.active_writers.load(Ordering::SeqCst);

        let close_observer = observer.clone();
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            close_observer.close();
            closed_tx.send(()).expect("report observer close");
        });
        while observer.inner.state.load(Ordering::SeqCst) == OBSERVER_OPEN {
            std::thread::yield_now();
        }
        let state_while_writer_paused = observer.inner.state.load(Ordering::SeqCst);
        let close_returned_while_writer_paused = closed_rx.try_recv();
        let snapshot_while_writer_paused = observer.snapshot();

        observer.release_writer_after_registration_for_test();
        writer.join().expect("registered writer completes");
        closer.join().expect("observer close completes");

        assert_eq!(registered_writers, 1);
        assert_eq!(state_while_writer_paused, OBSERVER_CLOSING);
        assert!(matches!(
            close_returned_while_writer_paused,
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(!snapshot_while_writer_paused.is_closed());
        assert!(!snapshot_while_writer_paused.validate().is_valid());

        let snapshot = observer.snapshot();
        assert!(snapshot.is_closed());
        assert_eq!(snapshot.after_close(), 1);
        assert!(!snapshot.validate().is_valid());
        assert!(
            snapshot
                .records()
                .iter()
                .any(|record| matches!(record.kind(), PhysicalEventRecordKind::ObserverClosed))
        );
    }

    #[test]
    fn close_waits_for_foreign_token_check_and_record_transaction() {
        let foreign_observer = observer();
        let foreign_wait = foreign_observer.begin_wait(wait_context(generation()));
        foreign_observer.close();

        let observer = observer();
        observer.pause_next_writer_after_registration_for_test();
        let writer_observer = observer.clone();
        let writer = std::thread::spawn(move || {
            writer_observer.finish_wait_error(foreign_wait, libc::ECHILD);
        });
        while !observer.writer_is_paused_after_registration_for_test() {
            std::thread::yield_now();
        }

        let close_observer = observer.clone();
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            close_observer.close();
            closed_tx.send(()).expect("report observer close");
        });
        while observer.inner.state.load(Ordering::SeqCst) == OBSERVER_OPEN {
            std::thread::yield_now();
        }
        let state_while_check_paused = observer.inner.state.load(Ordering::SeqCst);
        let close_returned_while_check_paused = closed_rx.try_recv();
        let snapshot_while_check_paused = observer.snapshot();

        observer.release_writer_after_registration_for_test();
        writer.join().expect("foreign-token writer completes");
        closer.join().expect("observer close completes");

        assert_eq!(state_while_check_paused, OBSERVER_CLOSING);
        assert!(matches!(
            close_returned_while_check_paused,
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert!(!snapshot_while_check_paused.is_closed());
        assert!(!snapshot_while_check_paused.validate().is_valid());

        let snapshot = observer.snapshot();
        assert!(snapshot.is_closed());
        assert_eq!(snapshot.after_close(), 2);
        assert!(!snapshot.validate().is_valid());
        assert!(
            snapshot
                .records()
                .iter()
                .any(|record| matches!(record.kind(), PhysicalEventRecordKind::ObserverClosed))
        );
    }

    #[test]
    fn kernel_superseded_disposition_survives_ordinary_overflow() {
        let observer = PhysicalEventObserver::new(PhysicalEventObserverConfig::new(4, 3))
            .expect("create superseded-stop overflow observer");
        let generation = generation();
        let task = PhysicalTaskIdentity::direct_child(Pid::from_raw(7));
        observer.attach_generation(generation);
        observer.link_pre_registration_task(task, generation);
        let wait = observer.begin_wait(PhysicalWaitContext {
            generation: Some(generation),
            task,
            producer: PhysicalWaitProducer::NotifierWorker,
            flags: libc::WEXITED | libc::WSTOPPED | libc::__WALL,
        });
        let status = observer.finish_wait_status(wait, stopped_status(), None);
        observer.record_status_published(
            generation,
            status,
            PhysicalStatusPublication::RegularFifo,
        );
        observer.finish_status(
            status,
            PhysicalStatusDisposition::KernelSupersededByExitStop,
        );
        observer.record_generation_finished(generation);
        observer.close();

        let snapshot = observer.snapshot();
        assert!(snapshot.ordinary_lost() > 0);
        assert_eq!(snapshot.cleanup_lost(), 0);
        assert!(snapshot.records().iter().any(|record| matches!(
            record.kind(),
            PhysicalEventRecordKind::StatusDisposition {
                status: observed,
                disposition: PhysicalStatusDisposition::KernelSupersededByExitStop,
            } if observed == status
        )));
    }

    #[test]
    fn cleanup_reservation_transaction_survives_ordinary_overflow() {
        let observer = PhysicalEventObserver::new(PhysicalEventObserverConfig::new(1, 5))
            .expect("create cleanup transaction classification observer");
        let generation = generation();
        observer.attach_generation(generation);
        observer.attach_generation(generation);
        observer.record_cleanup_reserved(
            generation,
            PhysicalReservationId(17),
            PhysicalStatusId(19),
        );
        observer.record_decode_started(
            PhysicalReservationId(17),
            PhysicalStatusId(19),
            PhysicalDecodeOwner::Cleanup,
        );
        observer.record_decode_finished(
            PhysicalReservationId(17),
            PhysicalStatusId(19),
            PhysicalDecodeOutcome::Returned,
            PhysicalDecodeOwner::Cleanup,
        );
        observer
            .record_cleanup_reservation_committed(PhysicalReservationId(17), PhysicalStatusId(19));
        observer.close();
        let snapshot = observer.snapshot();
        assert_eq!(snapshot.ordinary_lost(), 1);
        assert_eq!(snapshot.cleanup_lost(), 0);
        assert!(snapshot.records().iter().any(|record| matches!(
            record.kind(),
            PhysicalEventRecordKind::StatusReserved {
                reservation: PhysicalReservationId(17),
                status: PhysicalStatusId(19),
                generation: observed,
            } if observed == generation
        )));
        assert!(snapshot.records().iter().any(|record| matches!(
            record.kind(),
            PhysicalEventRecordKind::ReservationCommitted {
                reservation: PhysicalReservationId(17),
                status: PhysicalStatusId(19),
            }
        )));
    }

    #[test]
    fn overflow_is_sticky_and_cleanup_capacity_remains_available() {
        let observer = PhysicalEventObserver::new(PhysicalEventObserverConfig::new(1, 2))
            .expect("create tiny physical observer");
        let generation = generation();
        observer.attach_generation(generation);
        observer.attach_generation(generation);
        observer.finish_status(
            PhysicalStatusId(19),
            PhysicalStatusDisposition::CancellationCleanup,
        );
        observer.close();
        let snapshot = observer.snapshot();
        assert!(snapshot.ordinary_lost() > 0);
        assert!(
            snapshot
                .records()
                .iter()
                .any(|record| matches!(record.kind(), PhysicalEventRecordKind::ObserverClosed))
        );
        assert!(!snapshot.validate().is_valid());
    }
}
