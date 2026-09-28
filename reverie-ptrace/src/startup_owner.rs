/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use core::future::Future;
use core::pin::Pin;
use core::task::Context;
use core::task::Poll;
use std::any::Any;
use std::cell::Cell;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

use futures::future::poll_fn;
use reverie::process::CommandStartupOwner;
use safeptrace::Error;
use safeptrace::Event;
use safeptrace::ExitStatus;
use safeptrace::Running;
use safeptrace::RunningWaitError;
use safeptrace::Signal;
use safeptrace::Stopped;
use safeptrace::StoppedTransitionError;
use safeptrace::TerminalCleanup;
use safeptrace::Wait;
use tokio::sync::Notify;

/// Observation of the retained initial-stop continuation, not full initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitialStopPhase {
    /// The actual running capability or its notifier wait remains owned.
    Waiting,
    /// The actual initial SIGSTOP and normal ptrace options are retained.
    Stopped,
    /// The notifier delivered an actual terminal wait, before an initial stop.
    Exited,
    /// The first typed refusal and remaining native/ptrace state stay owned.
    Refused,
    /// Polling panicked; the owner retains remaining state and does not repoll it.
    Panicked,
}

/// An original failure together with any capability the failing API returned.
#[derive(Debug)]
pub enum InitialStopRefusal {
    /// Registration failed; the original Running and native child stay owned.
    Registration(safeptrace::Errno),
    /// A completed failed wait retains its original association and exact error.
    /// It is no longer pending and is not automatically polled or recreated.
    Wait(RunningWaitError),
    /// A spurious-stop resume refused, retaining Stopped or exact-token Zombie.
    Resume(StoppedTransitionError),
    /// Setting options failed; the observed Stopped remains in the owner too.
    Options(Error),
}

struct Published {
    phase: Cell<InitialStopPhase>,
    changed: Notify,
}

/// Observation only; retaining this handle does not keep the outer owner alive.
#[derive(Clone)]
pub struct InitialStopClient(Rc<Published>);

impl InitialStopClient {
    /// Reads the last observation without transferring a native capability.
    pub fn phase(&self) -> InitialStopPhase {
        self.0.phase.get()
    }

    /// A cancellable observation of the independently retained continuation.
    pub async fn observation(&self) -> InitialStopPhase {
        loop {
            let changed = self.0.changed.notified();
            let phase = self.phase();
            if phase != InitialStopPhase::Waiting {
                return phase;
            }
            changed.await;
        }
    }
}

// Owned continuations remain local to the original thread without a Send bound.
type OwnedLocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

struct InitialStop {
    running: Option<Running>,
    waiting: Option<OwnedLocalFuture<Result<Wait, RunningWaitError>>>,
    observed: Option<Wait>,
    cleanup: Option<TerminalCleanup>,
    refusal: Option<InitialStopRefusal>,
    panic: Option<Box<dyn Any + Send>>,
    published: Rc<Published>,
}

impl InitialStop {
    fn publish(&self, phase: InitialStopPhase) {
        self.published.phase.set(phase);
        self.published.changed.notify_waiters();
    }

    fn refuse(&mut self, error: InitialStopRefusal) {
        self.refusal = Some(error);
        self.publish(InitialStopPhase::Refused);
    }

    // All state lives outside this borrowed poll and outside the client future.
    // A completed error is stored before publishing it and is never retried.
    fn poll(&mut self, cx: &mut Context<'_>) {
        if self.published.phase.get() != InitialStopPhase::Waiting {
            return;
        }
        if let Some(running) = &self.running {
            if self.cleanup.is_none() {
                self.cleanup = Some(running.terminal_cleanup());
                if let Some(error) = self.cleanup.as_ref().unwrap().registration_error() {
                    self.refuse(InitialStopRefusal::Registration(error));
                    return;
                }
            }
            let running = self.running.take().unwrap();
            self.waiting = Some(Box::pin(running.next_state_owned()));
        }
        if let Some(waiting) = &mut self.waiting {
            let Poll::Ready(result) = waiting.as_mut().poll(cx) else {
                return;
            };
            self.waiting = None;
            match result {
                Ok(observed) => self.observed = Some(observed),
                Err(error) => {
                    self.refuse(InitialStopRefusal::Wait(error));
                    return;
                }
            }
        }
        match self.observed.as_ref().expect("the wait stored its outcome") {
            Wait::Exited(..) => self.publish(InitialStopPhase::Exited),
            Wait::Stopped(stopped, Event::Signal(Signal::SIGSTOP)) => {
                // Borrow the original stop, so an errno cannot discard it.
                match stopped.setoptions(crate::tracer::initial_ptrace_options()) {
                    Ok(()) => self.publish(InitialStopPhase::Stopped),
                    Err(error) => self.refuse(InitialStopRefusal::Options(error)),
                }
            }
            Wait::Stopped(..) => {
                let Wait::Stopped(stopped, event) = self.observed.take().unwrap() else {
                    unreachable!()
                };
                let signal = match event {
                    Event::Signal(signal) => Some(signal),
                    _ => None,
                };
                match stopped.resume_owned(signal) {
                    Ok(running) => {
                        self.running = Some(running);
                        // One real transition per poll; do not starve the client.
                        cx.waker().wake_by_ref();
                    }
                    Err(error) => self.refuse(InitialStopRefusal::Resume(error)),
                }
            }
        }
    }
}

/// Original-thread successor owning the whole native startup and initial stop.
///
/// The native Child, descriptors, runtime and possibly borrowed/non-Send R move
/// together. The client owns no wait future or stop. This explicit capability
/// stops before tracee_preinit/Tool/task creation; legacy Tracer APIs are not
/// silently rerouted. No Child or numeric-PID cleanup callback is extracted.
///
/// Dropping this OUTER owner is not cleanup. Retain it through a later owning
/// disposition. Refused/unknown is not terminal; a surviving client handle alone
/// does not retain this owner. The startup panic qualification still applies:
/// catching unwind is not rollback of an arbitrary consuming callee's locals.
#[must_use]
pub struct InitialStopOwner<R> {
    state: InitialStop,
    native: CommandStartupOwner<R>,
}

impl<R> InitialStopOwner<R> {
    /// Accepts the entire settled native owner, or returns that same owner.
    ///
    /// An unready/refused native attempt is neither retried nor discarded. The
    /// association comes from its actual successful clone, never a caller PID.
    /// Notifier registration occurs later, with custody already in self.
    pub fn from_startup(native: CommandStartupOwner<R>) -> Result<Self, CommandStartupOwner<R>> {
        let Some(pid) = native.ready_child_id() else {
            return Err(native);
        };
        Ok(Self {
            state: InitialStop {
                running: Some(Running::new(pid)),
                waiting: None,
                observed: None,
                cleanup: None,
                refusal: None,
                panic: None,
                published: Rc::new(Published {
                    phase: Cell::new(InitialStopPhase::Waiting),
                    changed: Notify::new(),
                }),
            },
            native,
        })
    }

    /// Returns an observation handle with no signal/extraction capability.
    pub fn client(&self) -> InitialStopClient {
        InitialStopClient(Rc::clone(&self.state.published))
    }

    /// Drives retained initial-stop state beside a borrowed, non-Send client.
    ///
    /// Client completion/cancellation/unwind does not consume the owned wait.
    /// A driver panic is retained separately and never causes another poll.
    /// There is no fresh timeout, repeated kill loop or numeric-PID rescue.
    pub fn drive_client<F: Future>(&mut self, client: F) -> std::thread::Result<F::Output> {
        let state = &mut self.state;
        let mut client = std::pin::pin!(client);
        self.native.drive_client(poll_fn(|cx| {
            if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(|| state.poll(cx))) {
                state.panic = Some(panic);
                state.publish(InitialStopPhase::Panicked);
            }
            client.as_mut().poll(cx)
        }))
    }

    /// Borrows the actual initial stop only after normal options succeeded.
    pub fn initial_stop(&self) -> Option<&Stopped> {
        if self.state.published.phase.get() != InitialStopPhase::Stopped {
            return None;
        }
        match self.state.observed.as_ref()? {
            Wait::Stopped(stopped, Event::Signal(Signal::SIGSTOP)) => Some(stopped),
            _ => None,
        }
    }

    /// Borrows the original refusal; absence is not a completion predicate.
    pub fn refusal(&self) -> Option<&InitialStopRefusal> {
        self.state.refusal.as_ref()
    }

    /// Returns only an actual terminal wait previously consumed by this owner.
    pub fn terminal_status(&self) -> Option<ExitStatus> {
        match self.state.observed.as_ref()? {
            Wait::Exited(_, status) => Some(*status),
            _ => None,
        }
    }

    /// The same caller resource remains here after native-to-ptrace handoff.
    pub fn resources(&self) -> &R {
        self.native.resources()
    }

    /// Takes a driver diagnostic, not its stopped/running/native custody.
    pub fn take_driver_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        self.state.panic.take()
    }
}

/// Setup observation for a Builder-owned command, before Tool/task creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TracerStartupPhase {
    /// The owned production preparation future has not completed.
    Preparing,
    /// The native owner retains the one requested startup attempt.
    Starting,
    /// Native setup completed; this is not an initial-stop or Tool readiness claim.
    Ready,
    /// Production preparation failed before any native birth request.
    PreparationRefused,
    /// The native owner retains its original typed startup refusal.
    NativeRefused,
    /// Preparation or native advancement panicked; no automatic retry is allowed.
    Panicked,
}

impl TracerStartupPhase {
    /// True only when the whole native owner can transfer to initial-stop custody.
    pub fn is_ready(self) -> bool {
        self == Self::Ready
    }
}

struct TracerPublished {
    phase: Cell<TracerStartupPhase>,
    changed: Notify,
}

/// Observation only; this handle owns neither the prepared payload nor the child.
#[derive(Clone)]
pub struct TracerStartupClient {
    published: Rc<TracerPublished>,
    native: reverie::process::CommandStartupClient,
}

impl TracerStartupClient {
    /// Reads preparation/setup state without advancing or transferring custody.
    pub fn phase(&self) -> TracerStartupPhase {
        self.published.phase.get()
    }

    /// Reads the actual native phase. Idle means no birth has been requested,
    /// including pending or refused preparation; it does not mean setup succeeded.
    pub fn native_phase(&self) -> reverie::process::CommandStartupPhase {
        self.native.phase()
    }

    /// Waits beside the owned continuation; cancelling this wait drops no payload.
    pub async fn setup_phase(&self) -> TracerStartupPhase {
        loop {
            let changed = self.published.changed.notified();
            let phase = self.phase();
            if !matches!(
                phase,
                TracerStartupPhase::Preparing | TracerStartupPhase::Starting
            ) {
                return phase;
            }
            changed.await;
        }
    }
}

struct TracerPreparation<T: reverie::Tool + 'static> {
    future: Option<OwnedLocalFuture<Result<crate::tracer::PreparedCommand<T>, reverie::Error>>>,
    prepared: Option<crate::tracer::PreparedCommand<T>>,
    refusal: Option<reverie::Error>,
    panic: Option<Box<dyn Any + Send>>,
    client: TracerStartupClient,
    native_observation: OwnedLocalFuture<reverie::process::CommandStartupPhase>,
}

impl<T: reverie::Tool + 'static> TracerPreparation<T> {
    fn publish(&self, phase: TracerStartupPhase) {
        self.client.published.phase.set(phase);
        self.client.published.changed.notify_waiters();
    }

    fn poll(&mut self, cx: &mut Context<'_>) {
        if self.client.phase() == TracerStartupPhase::Preparing {
            let Poll::Ready(result) = self
                .future
                .as_mut()
                .expect("owned preparation")
                .as_mut()
                .poll(cx)
            else {
                return;
            };
            // Store the complete payload before any birth or callback in start.
            // A panic after this point cannot drop it as a poll-local temporary.
            match result {
                Ok(prepared) => self.prepared = Some(prepared),
                Err(error) => self.refusal = Some(error),
            }
            self.future = None;
            if self.refusal.is_some() {
                self.publish(TracerStartupPhase::PreparationRefused);
                return;
            }
            self.publish(TracerStartupPhase::Starting);
            self.client
                .native
                .start(&mut self.prepared.as_mut().unwrap().command);
        }
        if self.client.phase() != TracerStartupPhase::Starting {
            return;
        }
        if let Poll::Ready(phase) = self.native_observation.as_mut().poll(cx) {
            use reverie::process::CommandStartupPhase;
            let phase = match phase {
                CommandStartupPhase::Ready => TracerStartupPhase::Ready,
                CommandStartupPhase::Refused => TracerStartupPhase::NativeRefused,
                CommandStartupPhase::Panicked => TracerStartupPhase::Panicked,
                CommandStartupPhase::Idle | CommandStartupPhase::Starting => unreachable!(),
            };
            self.publish(phase);
        }
    }
}

/// Original-thread ownership of real Builder preparation, native startup and R.
///
/// Construct this at a synchronous boundary before entering Tokio. The borrowed
/// client may be non-Send and non-static; its return, cancellation or unwind does
/// not destroy the owned preparation/native continuation or caller resource.
/// The same production preparation is used by ordinary TracerBuilder::spawn.
///
/// This capability ends before TracedTask::new, preinit, GDB serving and Tool
/// execution. Existing async Backend/Hermit callers are not redirected. Dropping
/// this OUTER owner is not a disposal policy: a later owning disposition is still
/// required. Catching a preparation panic preserves remaining state, not locals
/// already destroyed inside an arbitrary Tool initializer's unwind.
#[must_use]
pub struct TracerStartupOwner<T: reverie::Tool + 'static, R> {
    // Allocate the large preparation slot before birth; refusal returns this
    // same allocation together with the original native owner.
    preparation: Box<TracerPreparation<T>>,
    native: CommandStartupOwner<R>,
}

impl<T: reverie::Tool + 'static, R> TracerStartupOwner<T, R> {
    pub(crate) fn new(
        builder: crate::tracer::TracerBuilder<T>,
        resources: R,
    ) -> std::io::Result<Self> {
        let native = CommandStartupOwner::new(resources)?;
        let native_client = native.client();
        let observation = native_client.clone();
        Ok(Self {
            preparation: Box::new(TracerPreparation {
                future: Some(Box::pin(builder.prepare())),
                prepared: None,
                refusal: None,
                panic: None,
                client: TracerStartupClient {
                    published: Rc::new(TracerPublished {
                        phase: Cell::new(TracerStartupPhase::Preparing),
                        changed: Notify::new(),
                    }),
                    native: native_client,
                },
                native_observation: Box::pin(async move { observation.setup_phase().await }),
            }),
            native,
        })
    }

    /// Returns a handle that can observe, but cannot start or dispose of, a child.
    pub fn client(&self) -> TracerStartupClient {
        self.preparation.client.clone()
    }

    /// Drives retained production preparation/native setup beside this client.
    /// Client panics and owned-driver panics remain distinct; neither is success.
    pub fn drive_client<F: Future>(&mut self, client: F) -> std::thread::Result<F::Output> {
        let preparation = &mut self.preparation;
        let mut client = std::pin::pin!(client);
        self.native.drive_client(poll_fn(|cx| {
            if let Err(panic) = std::panic::catch_unwind(AssertUnwindSafe(|| preparation.poll(cx)))
            {
                preparation.panic = Some(panic);
                preparation.publish(TracerStartupPhase::Panicked);
            }
            client.as_mut().poll(cx)
        }))
    }

    /// Borrows the original preparation error, with native phase still Idle.
    pub fn preparation_refusal(&self) -> Option<&reverie::Error> {
        self.preparation.refusal.as_ref()
    }

    /// Borrows a real native refusal without extracting its retained resources.
    pub fn native_refusal(
        &self,
    ) -> Option<std::cell::Ref<'_, reverie::process::CommandStartError>> {
        self.preparation.client.native.refusal()
    }

    /// Borrows the same caller resource retained before any birth.
    pub fn resources(&self) -> &R {
        self.native.resources()
    }

    /// Takes a diagnostic only; this does not resume a panicked continuation.
    pub fn take_driver_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        self.preparation
            .panic
            .take()
            .or_else(|| self.native.take_driver_panic())
    }

    /// Transfers the complete payload/native/R into initial-stop ownership.
    /// A pending, refused or panicked attempt returns this same owner unchanged.
    pub fn into_initial_stop(self) -> Result<TracerInitialStopOwner<T, R>, Self> {
        if !self.preparation.client.phase().is_ready() || self.preparation.prepared.is_none() {
            return Err(self);
        }
        let Self {
            mut preparation,
            native,
        } = self;
        match InitialStopOwner::from_startup(native) {
            Ok(stop) => Ok(TracerInitialStopOwner {
                prepared: preparation.prepared.take().unwrap(),
                stop,
            }),
            Err(native) => Err(Self {
                preparation,
                native,
            }),
        }
    }
}

/// Whole Builder payload and native/runtime/R held through a real initial stop.
///
/// This is not a Tracer, initialized Tool or cleanup guard. It exposes no Child,
/// Stopped extraction, resume or signal operation. GDB/LiteInst settings, config,
/// subscriptions and the actual GlobalTool remain in the private prepared payload
/// for a future owning task-construction transition; that transition is not here.
#[must_use]
pub struct TracerInitialStopOwner<T: reverie::Tool + 'static, R> {
    prepared: crate::tracer::PreparedCommand<T>,
    stop: InitialStopOwner<R>,
}

impl<T: reverie::Tool + 'static, R> TracerInitialStopOwner<T, R> {
    /// Borrows the exact configuration retained with the prepared GlobalTool.
    /// This does not initialize a Tool or transfer any native capability.
    pub fn config(&self) -> &<T::GlobalState as reverie::GlobalTool>::Config {
        &self.prepared.config
    }

    /// Observes the retained wait; the handle itself does not keep custody alive.
    pub fn client(&self) -> InitialStopClient {
        self.stop.client()
    }

    /// Drives the same owning wait beside a possibly borrowed, non-Send client.
    pub fn drive_client<F: Future>(&mut self, client: F) -> std::thread::Result<F::Output> {
        self.stop.drive_client(client)
    }

    /// True only for the real retained SIGSTOP with normal ptrace options set.
    /// This borrows no raw stop and does not claim Tool/task initialization.
    pub fn has_initial_stop(&self) -> bool {
        self.stop.initial_stop().is_some()
    }

    /// Borrows an original refusal without transferring its custody.
    pub fn refusal(&self) -> Option<&InitialStopRefusal> {
        self.stop.refusal()
    }

    /// Reports only an actual terminal wait, never a signal request or timeout.
    pub fn terminal_status(&self) -> Option<ExitStatus> {
        self.stop.terminal_status()
    }

    /// Borrows the same resource retained through the whole-owner transfer.
    pub fn resources(&self) -> &R {
        self.stop.resources()
    }

    /// Takes only a diagnostic; the first-stop continuation stays stopped/refused.
    pub fn take_driver_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        self.stop.take_driver_panic()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use futures::future::Either;
    use reverie::process::Command;
    use reverie::process::CommandStartupPhase;
    use tokio::io::AsyncReadExt;

    use super::*;

    struct Resource<'a> {
        value: &'a Cell<usize>,
        drops: &'a Cell<usize>,
    }

    impl Drop for Resource<'_> {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    fn actual_birth(cancel_by_panic: bool) {
        // One absolute deadline includes birth, cancelled observation, handoff,
        // real initial stop and explicit test disposition. No refreshed grace.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let parent_tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
        let (arrived_parent, arrived_child) = UnixStream::pair().unwrap();
        let (mut release_parent, release_child) = UnixStream::pair().unwrap();
        arrived_parent.set_nonblocking(true).unwrap();
        let arrival_fd = arrived_child.as_raw_fd();
        let release_fd = release_child.as_raw_fd();
        let mut command = Command::new("/bin/true");
        unsafe {
            command.pre_exec(move || {
                // Async-signal-safe post-clone barrier before the unchanged
                // production init_tracee. No allocation/TLS migration in child.
                let pid = libc::getpid().to_ne_bytes();
                if libc::write(arrival_fd, pid.as_ptr().cast(), pid.len()) != pid.len() as isize {
                    return Err(safeptrace::Errno::last());
                }
                let mut byte = 0u8;
                loop {
                    let count = libc::read(release_fd, (&mut byte as *mut u8).cast(), 1);
                    if count == 1 {
                        break;
                    }
                    if count < 0 && safeptrace::Errno::last() == safeptrace::Errno::EINTR {
                        continue;
                    }
                    return Err(safeptrace::Errno::EIO);
                }
                crate::tracer::init_tracee(false)
            });
        }
        let value = Cell::new(41);
        let drops = Cell::new(0);
        let born_pid = Cell::new(0);
        let mut native = CommandStartupOwner::new(Resource {
            value: &value,
            drops: &drops,
        })
        .unwrap();
        let startup = native.client();
        let outcome = native.drive_client(async {
            let mut arrival = tokio::net::UnixStream::from_std(arrived_parent).unwrap();
            let request = async {
                startup.start(&mut command);
                startup.setup_phase().await
            };
            let arrival = async {
                let mut bytes = [0; 4];
                arrival.read_exact(&mut bytes).await.unwrap();
                i32::from_ne_bytes(bytes)
            };
            let request = Box::pin(request);
            let arrival = Box::pin(arrival);
            let selected =
                tokio::time::timeout_at(deadline, futures::future::select(request, arrival))
                    .await
                    .expect("real pre_exec arrival before the absolute deadline");
            let Either::Right((pid, pending_request)) = selected else {
                panic!("native setup completed before the child barrier opened")
            };
            born_pid.set(pid);
            drop(pending_request); // Cancellation of the observing client only.
            assert_eq!(startup.phase(), CommandStartupPhase::Starting);
            value.set(42); // The actual borrowed non-Send caller state is usable.
            if cancel_by_panic {
                std::panic::panic_any(71usize);
            }
        });
        if cancel_by_panic {
            let Err(panic) = outcome else {
                panic!("client panic was lost")
            };
            assert_eq!(*panic.downcast::<usize>().expect("original panic type"), 71);
        } else {
            assert!(outcome.is_ok(), "client unexpectedly panicked");
        }
        assert!(born_pid.get() > 0);
        assert_eq!(drops.get(), 0);
        assert_eq!(value.get(), 42);
        assert_eq!(startup.phase(), CommandStartupPhase::Starting);
        native = match InitialStopOwner::from_startup(native) {
            Err(native) => native,
            Ok(_) => panic!("pending startup was incorrectly admitted"),
        };
        assert!(std::ptr::eq(native.resources().value, &value));
        drop(arrived_child);
        drop(release_child);
        release_parent.write_all(&[1]).unwrap();
        let phase = native
            .drive_client(async { tokio::time::timeout_at(deadline, startup.setup_phase()).await });
        assert!(matches!(phase, Ok(Ok(CommandStartupPhase::Ready))));
        assert_eq!(native.ready_child_id().unwrap().as_raw(), born_pid.get());
        let mut owner = match InitialStopOwner::from_startup(native) {
            Ok(owner) => owner,
            Err(_) => panic!("same native startup did not transfer when Ready"),
        };
        let client = owner.client();
        let phase = owner
            .drive_client(async { tokio::time::timeout_at(deadline, client.observation()).await });
        assert!(matches!(phase, Ok(Ok(InitialStopPhase::Stopped))));
        assert!(owner.refusal().is_none());
        assert!(owner.take_driver_panic().is_none());
        assert_eq!(drops.get(), 0);
        assert!(std::ptr::eq(owner.resources().value, &value));
        let stopped = owner.initial_stop().expect("actual owned initial SIGSTOP");
        assert_eq!(stopped.pid().as_raw(), born_pid.get());
        stopped
            .getregs()
            .expect("actual original-thread ptrace access");
        let cleanup = owner.state.cleanup.as_ref().unwrap();
        assert!(cleanup.same_generation(&stopped.terminal_cleanup()));
        assert!(cleanup.registration_error().is_none());
        let status = std::fs::read_to_string(format!("/proc/{}/status", born_pid.get())).unwrap();
        let tracer = status
            .lines()
            .find_map(|line| line.strip_prefix("TracerPid:"))
            .unwrap();
        assert_eq!(tracer.trim().parse::<i32>().unwrap(), parent_tid);
        assert!(owner.terminal_status().is_none());

        // Boundary assertions above precede disposal. This is explicit test
        // disposition through the held stop, NOT product postspawn/Tool cleanup.
        // The actual owner stores the resulting wait before another client runs.
        let Wait::Stopped(stopped, Event::Signal(Signal::SIGSTOP)) =
            owner.state.observed.take().unwrap()
        else {
            unreachable!()
        };
        match stopped.detach_owned(None) {
            Ok(running) => owner.state.running = Some(running),
            Err(error) => {
                owner.state.refuse(InitialStopRefusal::Resume(error));
                panic!("normal test detach refused: {:?}", owner.refusal());
            }
        }
        owner.state.publish(InitialStopPhase::Waiting);
        let phase = owner
            .drive_client(async { tokio::time::timeout_at(deadline, client.observation()).await });
        assert!(matches!(phase, Ok(Ok(InitialStopPhase::Exited))));
        assert_eq!(owner.terminal_status(), Some(ExitStatus::Exited(0)));
        let cleanup = owner.state.cleanup.as_ref().unwrap();
        assert!(cleanup.wait(deadline.saturating_duration_since(tokio::time::Instant::now())));
        assert!(matches!(
            std::fs::symlink_metadata(format!("/proc/{}", born_pid.get())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        assert!(tokio::time::Instant::now() < deadline);
        assert_eq!(drops.get(), 0);
        drop(owner);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn real_birth_client_cancellation_retains_native_and_initial_stop() {
        actual_birth(false);
    }

    #[test]
    fn real_birth_client_panic_retains_native_and_initial_stop() {
        actual_birth(true);
    }

    // Each owner has its own witness Arc, carried in the real Config. Concurrent
    // libtest invocations share no counters, registry or thread-local attribution.
    #[derive(Default)]
    struct BuilderWitness {
        calls: Vec<(&'static str, i32)>,
        tool_constructors: usize,
        global_drops: usize,
    }

    #[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
    struct BuilderConfig {
        cookie: u64,
        #[serde(skip)]
        witness: std::sync::Arc<std::sync::Mutex<BuilderWitness>>,
    }

    #[derive(Default)]
    struct BuilderGlobal {
        cookie: u64,
        witness: std::sync::Arc<std::sync::Mutex<BuilderWitness>>,
    }

    impl Drop for BuilderGlobal {
        fn drop(&mut self) {
            self.witness.lock().unwrap().global_drops += 1;
        }
    }

    #[reverie::global_tool]
    impl reverie::GlobalTool for BuilderGlobal {
        type Config = BuilderConfig;
        type Request = ();
        type Response = ();

        async fn init_global_state(config: &Self::Config) -> Self {
            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
            config
                .witness
                .lock()
                .unwrap()
                .calls
                .push(("global-start", tid));
            tokio::task::yield_now().await;
            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
            config
                .witness
                .lock()
                .unwrap()
                .calls
                .push(("global-ready", tid));
            Self {
                cookie: config.cookie,
                witness: config.witness.clone(),
            }
        }

        async fn receive_rpc(&self, _from: reverie::Tid, _message: ()) {}
    }

    #[derive(Default)]
    struct BuilderTool;

    #[reverie::tool]
    impl reverie::Tool for BuilderTool {
        type GlobalState = BuilderGlobal;
        type ThreadState = ();

        fn new(_pid: reverie::Pid, config: &BuilderConfig) -> Self {
            config.witness.lock().unwrap().tool_constructors += 1;
            Self
        }

        fn subscriptions(config: &BuilderConfig) -> reverie::Subscription {
            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
            config
                .witness
                .lock()
                .unwrap()
                .calls
                .push(("subscriptions", tid));
            [reverie::syscalls::Sysno::getpgid].into_iter().collect()
        }
    }

    fn builder_child_start_time(pid: i32) -> u64 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        stat.rsplit_once(')')
            .expect("complete process stat")
            .1
            .split_whitespace()
            .nth(19)
            .expect("kernel start time field")
            .parse()
            .unwrap()
    }

    fn builder_actual_birth(cancel_by_panic: bool) {
        // The original three-second absolute envelope includes both observations,
        // the owning handoff, first stop and explicit test disposition.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let parent_tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
        let (arrived_parent, arrived_child) = UnixStream::pair().unwrap();
        let (mut release_parent, release_child) = UnixStream::pair().unwrap();
        arrived_parent.set_nonblocking(true).unwrap();
        let arrival_fd = arrived_child.as_raw_fd();
        let release_fd = release_child.as_raw_fd();
        let witness = std::sync::Arc::new(std::sync::Mutex::new(BuilderWitness::default()));
        let cookie: u64 = if cancel_by_panic { 0xc071 } else { 0xc072 };
        let mut command = Command::new("/bin/true");
        command.arg0("builder-owned-true");
        command.env("BUILDER_OWNER_COOKIE", cookie.to_string());
        command.stdin(reverie::process::Stdio::piped());
        command.stdout(reverie::process::Stdio::piped());
        command.stderr(reverie::process::Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                // Only a causal, async-signal-safe arrival/release barrier here.
                // In particular this does NOT call init_tracee or install a filter:
                // those must come from the real shared Builder preparation.
                let identity = [libc::getpid(), libc::syscall(libc::SYS_gettid) as i32];
                let length = std::mem::size_of_val(&identity);
                if libc::write(arrival_fd, identity.as_ptr().cast(), length) != length as isize {
                    return Err(safeptrace::Errno::EIO);
                }
                let mut byte = 0u8;
                loop {
                    let count = libc::read(release_fd, (&mut byte as *mut u8).cast(), 1);
                    if count == 1 {
                        return Ok(());
                    }
                    if count < 0 && safeptrace::Errno::last() == safeptrace::Errno::EINTR {
                        continue;
                    }
                    return Err(safeptrace::Errno::EIO);
                }
            });
        }
        let value = Cell::new(41);
        let drops = Cell::new(0);
        let born_pid = Cell::new(0);
        let born_start = Cell::new(0);
        // This is the actual Builder entry, outside any Tokio runtime. The
        // owner's resource and observing future both borrow !Send caller state.
        let mut owner = crate::TracerBuilder::<BuilderTool>::new(command)
            .config(BuilderConfig {
                cookie,
                witness: witness.clone(),
            })
            .gdbserver(0u16)
            .sequentialized_guest()
            .injected_syscall_trap(0x7172, 0x7374)
            .backend_stats(reverie::BackendStatsRequest::ENABLED)
            .into_startup_owner(Resource {
                value: &value,
                drops: &drops,
            })
            .unwrap();
        let startup = owner.client();
        assert_eq!(startup.phase(), TracerStartupPhase::Preparing);
        assert_eq!(startup.native_phase(), CommandStartupPhase::Idle);
        assert!(witness.lock().unwrap().calls.is_empty());
        let outcome = owner.drive_client(async {
            let mut arrival = tokio::net::UnixStream::from_std(arrived_parent).unwrap();
            let request = Box::pin(startup.setup_phase());
            let arrival = Box::pin(async {
                let mut bytes = [0; 8];
                arrival.read_exact(&mut bytes).await.unwrap();
                let pid = i32::from_ne_bytes(bytes[..4].try_into().unwrap());
                let tid = i32::from_ne_bytes(bytes[4..].try_into().unwrap());
                assert_eq!(pid, tid, "the real root child is its initial thread");
                pid
            });
            let selected =
                tokio::time::timeout_at(deadline, futures::future::select(request, arrival))
                    .await
                    .expect("real Builder pre_exec arrival before the absolute deadline");
            let Either::Right((pid, pending_request)) = selected else {
                panic!("Builder setup completed before the causal child barrier opened")
            };
            born_pid.set(pid);
            born_start.set(builder_child_start_time(pid));
            drop(pending_request);
            assert_eq!(startup.phase(), TracerStartupPhase::Starting);
            assert_eq!(startup.native_phase(), CommandStartupPhase::Starting);
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
            assert_eq!(
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("TracerPid:"))
                    .unwrap()
                    .trim(),
                "0"
            );
            value.set(42);
            if cancel_by_panic {
                std::panic::panic_any(73usize);
            }
        });
        if cancel_by_panic {
            let Err(panic) = outcome else {
                panic!("client panic was lost")
            };
            assert_eq!(
                *panic.downcast::<usize>().expect("original client panic"),
                73
            );
        } else {
            assert!(outcome.is_ok(), "client unexpectedly panicked");
        }
        assert!(born_pid.get() > 0);
        assert_eq!(drops.get(), 0);
        assert_eq!(value.get(), 42);
        assert!(std::ptr::eq(owner.resources().value, &value));
        assert!(owner.preparation_refusal().is_none());
        assert!(owner.native_refusal().is_none());
        assert!(owner.take_driver_panic().is_none());
        let expected_calls = vec![
            ("global-start", parent_tid),
            ("global-ready", parent_tid),
            ("subscriptions", parent_tid),
        ];
        assert_eq!(witness.lock().unwrap().calls, expected_calls);
        let prepared = owner
            .preparation
            .prepared
            .as_ref()
            .expect("whole prepared payload survived client drop");
        assert_eq!(prepared.config.cookie, cookie);
        assert_eq!(prepared.gref.cookie, cookie);
        assert!(std::sync::Arc::ptr_eq(&prepared.gref.witness, &witness));
        assert_eq!(prepared.command.get_arg0(), "builder-owned-true");
        assert_eq!(
            &*prepared.command.get_env("BUILDER_OWNER_COOKIE").unwrap(),
            std::ffi::OsStr::new(&cookie.to_string())
        );
        assert_eq!(
            &*prepared.command.get_env("LSAN_OPTIONS").unwrap(),
            std::ffi::OsStr::new("detect_leaks=0")
        );
        assert_eq!(
            &*prepared.command.get_env("ASAN_OPTIONS").unwrap(),
            std::ffi::OsStr::new("detect_leaks=0")
        );
        assert_eq!(
            prepared.events,
            [reverie::syscalls::Sysno::getpgid].into_iter().collect()
        );
        assert!(prepared.gdbserver.is_some());
        assert!(prepared.sequentialized_guest);
        assert_eq!(
            prepared.injected_syscall_trap.as_ref().unwrap().marker,
            0x7172
        );
        assert_eq!(prepared.injected_syscall_trap.as_ref().unwrap().rip, 0x7374);
        assert!(prepared.backend_stats.is_some());
        assert!(prepared.liteinst_runtime.is_none());
        assert_eq!(witness.lock().unwrap().tool_constructors, 0);
        assert_eq!(witness.lock().unwrap().global_drops, 0);
        owner = match owner.into_initial_stop() {
            Err(owner) => owner,
            Ok(_) => panic!("pending native startup transferred as ready"),
        };
        drop(arrived_child);
        drop(release_child);
        release_parent.write_all(&[1]).unwrap();
        let phase = owner
            .drive_client(async { tokio::time::timeout_at(deadline, startup.setup_phase()).await });
        assert!(matches!(phase, Ok(Ok(TracerStartupPhase::Ready))));
        assert_eq!(startup.native_phase(), CommandStartupPhase::Ready);
        assert_eq!(
            owner.native.ready_child_id().unwrap().as_raw(),
            born_pid.get()
        );
        let mut owner = match owner.into_initial_stop() {
            Ok(owner) => owner,
            Err(_) => panic!("complete Builder/native payload did not transfer"),
        };
        // Native Ready was handoff readiness, not a fabricated observed SIGSTOP.
        assert!(!owner.has_initial_stop());
        let client = owner.client();
        let phase = owner
            .drive_client(async { tokio::time::timeout_at(deadline, client.observation()).await });
        assert!(matches!(phase, Ok(Ok(InitialStopPhase::Stopped))));
        assert!(owner.has_initial_stop());
        assert!(owner.refusal().is_none());
        assert!(owner.take_driver_panic().is_none());
        assert!(owner.terminal_status().is_none());
        assert_eq!(owner.config().cookie, cookie);
        assert!(std::sync::Arc::ptr_eq(
            &owner.prepared.gref.witness,
            &witness
        ));
        assert_eq!(witness.lock().unwrap().calls, expected_calls);
        assert_eq!(witness.lock().unwrap().tool_constructors, 0);
        assert_eq!(witness.lock().unwrap().global_drops, 0);
        assert_eq!(drops.get(), 0);
        assert!(std::ptr::eq(owner.resources().value, &value));
        let stopped = owner.stop.initial_stop().unwrap();
        assert_eq!(stopped.pid().as_raw(), born_pid.get());
        stopped
            .getregs()
            .expect("actual ptrace access on the birth thread");
        let cleanup = owner.stop.state.cleanup.as_ref().unwrap();
        assert!(cleanup.same_generation(&stopped.terminal_cleanup()));
        assert!(cleanup.registration_error().is_none());
        assert_eq!(builder_child_start_time(born_pid.get()), born_start.get());
        assert_eq!(
            unsafe { libc::syscall(libc::SYS_gettid) } as i32,
            parent_tid
        );
        let status = std::fs::read_to_string(format!("/proc/{}/status", born_pid.get())).unwrap();
        assert_eq!(
            status
                .lines()
                .find_map(|line| line.strip_prefix("TracerPid:"))
                .unwrap()
                .trim()
                .parse::<i32>()
                .unwrap(),
            parent_tid
        );

        // Explicit test disposition AFTER the ownership predicate. No claim of
        // production cleanup, postspawn, Tool callbacks or output completion.
        let Wait::Stopped(stopped, Event::Signal(Signal::SIGSTOP)) =
            owner.stop.state.observed.take().unwrap()
        else {
            unreachable!()
        };
        match stopped.detach_owned(None) {
            Ok(running) => owner.stop.state.running = Some(running),
            Err(error) => {
                owner.stop.state.refuse(InitialStopRefusal::Resume(error));
                panic!("normal test detach refused: {:?}", owner.refusal());
            }
        }
        owner.stop.state.publish(InitialStopPhase::Waiting);
        let phase = owner
            .drive_client(async { tokio::time::timeout_at(deadline, client.observation()).await });
        assert!(matches!(phase, Ok(Ok(InitialStopPhase::Exited))));
        assert_eq!(owner.terminal_status(), Some(ExitStatus::Exited(0)));
        assert!(
            owner
                .stop
                .state
                .cleanup
                .as_ref()
                .unwrap()
                .wait(deadline.saturating_duration_since(tokio::time::Instant::now()))
        );
        assert!(
            matches!(std::fs::symlink_metadata(format!("/proc/{}", born_pid.get())), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(tokio::time::Instant::now() < deadline);
        assert_eq!(drops.get(), 0);
        assert_eq!(witness.lock().unwrap().global_drops, 0);
        drop(owner);
        assert_eq!(drops.get(), 1);
        assert_eq!(witness.lock().unwrap().global_drops, 1);
    }

    #[test]
    fn builder_real_birth_cancellation_retains_prepared_payload() {
        builder_actual_birth(false);
    }

    #[test]
    fn builder_real_birth_panic_retains_prepared_payload() {
        builder_actual_birth(true);
    }
}
