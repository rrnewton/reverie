/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use core::future::Future;
use core::pin::Pin;
use std::any::Any;
use std::cell::Cell;
use std::cell::Ref;
use std::cell::RefCell;
use std::io;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;

use futures::FutureExt;
use futures::future::poll_fn;
use tokio::runtime::Builder;
use tokio::runtime::Runtime;
use tokio::sync::Notify;

use super::Command;
use super::CommandStart;
use super::CommandStartError;

/// Native parent-setup progress, never child terminal or cleanup evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandStartupPhase {
    /// No attempt has been requested.
    Idle,
    /// The owner retains an attempted startup and its pending continuation.
    Starting,
    /// Parent setup finished; the actual Child is still owned by CommandStart.
    Ready,
    /// The first real startup error and its resources remain owned.
    Refused,
    /// Startup panicked; ownership remains, but no completion is inferred.
    Panicked,
}

impl CommandStartupPhase {
    fn settled(self) -> bool {
        matches!(self, Self::Ready | Self::Refused | Self::Panicked)
    }
}

struct Shared {
    // Some before driving or after a settled observation. While Starting, the
    // owner's pinned future can own the actual value directly instead.
    native: RefCell<Option<CommandStart>>,
    runtime: tokio::runtime::Handle,
    client_active: Cell<bool>,
    phase: Cell<CommandStartupPhase>,
    requested: Notify,
    changed: Notify,
    birth_panicked: Cell<bool>,
    driver_panic: RefCell<Option<Box<dyn Any + Send>>>,
}

/// The client can request one birth and inspect setup results, not take custody.
///
/// This handle contains no extraction, signal, wait, or cleanup operation. Its
/// Rc also prevents moving the request to another OS thread in safe Rust.
#[derive(Clone)]
pub struct CommandStartupClient {
    shared: Rc<Shared>,
}

impl CommandStartupClient {
    /// Runs the ordinary birth synchronously on the owner's entered thread.
    ///
    /// The owner stored the real CommandStart and its driver before this call.
    /// A real error remains in that slot; await setup_phase to inspect it.
    /// Reusing a slot panics before another birth. No new callback bound or
    /// clone/pre_exec/thread behavior is introduced.
    pub fn start(&self, command: &mut Command) {
        assert!(
            self.shared.client_active.get(),
            "birth requires the active owner"
        );
        assert_eq!(
            self.phase(),
            CommandStartupPhase::Idle,
            "one startup per owner",
        );
        self.shared.phase.set(CommandStartupPhase::Starting);
        // Declared before the borrow: an unwind releases RefMut before waking
        // the independent owner driver. It does not destroy the native slot.
        let _wake = WakeOwner(&self.shared);
        let _entered = self.shared.runtime.enter();
        let mut native = self.shared.native.borrow_mut();
        let _retained_error = command.spawn_into(native.as_mut().expect("pre-birth slot"));
    }

    /// Reads scheduling state only, not kernel disposition.
    pub fn phase(&self) -> CommandStartupPhase {
        self.shared.phase.get()
    }

    /// Waits for a setup observation while the outer owner drives startup.
    /// Cancelling this borrowed observation never cancels the native driver.
    pub async fn setup_phase(&self) -> CommandStartupPhase {
        loop {
            let changed = self.shared.changed.notified();
            let phase = self.phase();
            if phase.settled() {
                return phase;
            }
            changed.await;
        }
    }

    /// Borrows the original typed error after the driver returns its owned slot.
    /// Absence during Starting is not success. No lossy error conversion occurs.
    pub fn refusal(&self) -> Option<Ref<'_, CommandStartError>> {
        if self.phase() != CommandStartupPhase::Refused {
            return None;
        }
        Ref::filter_map(self.shared.native.borrow(), |native| {
            native.as_ref()?.refusal()
        })
        .ok()
    }
}

struct WakeOwner<'a>(&'a Shared);

impl Drop for WakeOwner<'_> {
    fn drop(&mut self) {
        self.0.birth_panicked.set(std::thread::panicking());
        self.0.requested.notify_one();
    }
}

struct ActiveClient<'a>(&'a Cell<bool>);

impl<'a> ActiveClient<'a> {
    fn enter(active: &'a Cell<bool>) -> Self {
        assert!(
            !active.replace(true),
            "the owner already has an active client"
        );
        Self(active)
    }
}

impl Drop for ActiveClient<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// A synchronous original-thread owner outside one cancellable client future.
///
/// Construct before client initialization or birth. The caller moves its actual
/// resource value into this owner; R may borrow and need not be Send or static.
/// The runtime itself and native finish future remain here when drive_client
/// returns, including after a client timeout or panic. Keep this owner alive
/// while moving to the next owning postspawn phase.
///
/// This first integration slice intentionally does not expose Child extraction
/// or implement attached-task/notifier/Tool/output/fallback disposition. Ready
/// means parent setup only. Dropping this outer owner is NOT safe cleanup and
/// does not establish terminal completion; a complete synchronous driver scope
/// must still retain it through real disposition or durable unresolved custody.
/// Existing unscoped Command/Tracer/Backend entry points are unchanged.
#[must_use]
pub struct CommandStartupOwner<R> {
    // Field order drops the native driver before this owner's shared state,
    // resource value and runtime. Surviving client handles are
    // observations only once ActiveClient has left; they cannot restart birth.
    driver: Pin<Box<dyn Future<Output = ()>>>,
    driver_finished: bool,
    shared: Rc<Shared>,
    resources: R,
    runtime: Runtime,
}

impl<R> CommandStartupOwner<R> {
    /// Allocates the owner/runtime before any child exists.
    /// This explicit synchronous boundary cannot be nested inside a runtime.
    pub fn new(resources: R) -> io::Result<Self> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "startup owner requires an outer synchronous runtime boundary",
            ));
        }
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let shared = Rc::new(Shared {
            native: RefCell::new(Some(CommandStart::new())),
            runtime: runtime.handle().clone(),
            client_active: Cell::new(false),
            phase: Cell::new(CommandStartupPhase::Idle),
            requested: Notify::new(),
            changed: Notify::new(),
            birth_panicked: Cell::new(false),
            driver_panic: RefCell::new(None),
        });
        let driver_state = Rc::clone(&shared);
        // This owned future has no borrowed Command, caller F, or resource R.
        // Its static storage does not add a static bound to the client or R.
        let driver = Box::pin(async move {
            driver_state.requested.notified().await;
            let phase = if driver_state.birth_panicked.get() {
                CommandStartupPhase::Panicked
            } else {
                let mut native = driver_state
                    .native
                    .borrow_mut()
                    .take()
                    .expect("startup slot");
                let outcome = AssertUnwindSafe(native.finish()).catch_unwind().await;
                let phase = match outcome {
                    Ok(Ok(())) => CommandStartupPhase::Ready,
                    Ok(Err(_)) => CommandStartupPhase::Refused,
                    Err(panic) => {
                        *driver_state.driver_panic.borrow_mut() = Some(panic);
                        CommandStartupPhase::Panicked
                    }
                };
                *driver_state.native.borrow_mut() = Some(native);
                phase
            };
            // The actual native value is back in the slot before publication.
            driver_state.phase.set(phase);
            driver_state.changed.notify_waiters();
        });
        Ok(Self {
            driver,
            driver_finished: false,
            shared,
            resources,
            runtime,
        })
    }

    /// Produces a request/observation handle, without transferring native state.
    pub fn client(&self) -> CommandStartupClient {
        CommandStartupClient {
            shared: Rc::clone(&self.shared),
        }
    }

    /// Polls the owned startup alongside a possibly borrowed, non-Send client.
    ///
    /// Returning the client result drops only that client future. The pending
    /// native future is still pinned in self and can be driven again. A panic
    /// is returned as its original payload at this boundary. This covers unwind,
    /// not process abort or a double panic. An ordinary typed client Result is
    /// preserved unchanged inside the outer thread::Result. No timeout is
    /// renewed: a caller's timeout future can finish while this owner retains
    /// native startup and its actual resource/timeout value R.
    pub fn drive_client<F: Future>(&mut self, client: F) -> std::thread::Result<F::Output> {
        let driver = &mut self.driver;
        let finished = &mut self.driver_finished;
        let _active = ActiveClient::enter(&self.shared.client_active);
        std::panic::catch_unwind(AssertUnwindSafe(|| {
            self.runtime.block_on(async {
                let mut client = std::pin::pin!(client);
                poll_fn(|cx| {
                    if !*finished && driver.as_mut().poll(cx).is_ready() {
                        *finished = true;
                    }
                    client.as_mut().poll(cx)
                })
                .await
            })
        }))
    }

    /// Continues the same startup after a client stopped observing it.
    /// Refusal/panic is sticky; this does not retry native operations or reap.
    pub fn drive_startup(&mut self) -> CommandStartupPhase {
        if self.shared.phase.get() == CommandStartupPhase::Idle {
            return CommandStartupPhase::Idle;
        }
        if !self.driver_finished {
            self.runtime.block_on(self.driver.as_mut());
            self.driver_finished = true;
        }
        self.shared.phase.get()
    }

    /// Association from the actual settled native child, not signal authority.
    ///
    /// A next-phase owner must accept this entire owner, not just this PID.
    /// Pending/refused/panicked startup returns None without advancing or retrying
    /// anything. The native Child, descriptors, runtime and R remain here.
    #[doc(hidden)]
    pub fn ready_child_id(&self) -> Option<super::Pid> {
        if !self.driver_finished || self.shared.phase.get() != CommandStartupPhase::Ready {
            return None;
        }
        self.shared.native.borrow().as_ref()?.child_id()
    }

    /// The actual caller-supplied resource remains owned independently of client.
    pub fn resources(&self) -> &R {
        &self.resources
    }

    /// Takes only a retained driver-panic diagnostic, never its native state.
    /// A birth panic is instead returned by drive_client as the client panic.
    pub fn take_driver_panic(&mut self) -> Option<Box<dyn Any + Send>> {
        self.shared.driver_panic.borrow_mut().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_nonsend_client_and_resource_keep_their_original_lifetime() {
        let value = Cell::new(0);
        let mut owner = CommandStartupOwner::new(&value).unwrap();
        let outcome = owner.drive_client(async {
            value.set(7);
            value.get()
        });
        match outcome {
            Ok(result) => assert_eq!(result, 7),
            Err(_) => panic!("client unexpectedly panicked"),
        }
        assert!(std::ptr::eq(*owner.resources(), &value));
        assert_eq!(owner.client().phase(), CommandStartupPhase::Idle);
        assert!(!owner.driver_finished);
    }

    #[test]
    fn client_panic_does_not_drop_the_outer_resource_owner() {
        struct Resource(Rc<Cell<usize>>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let drops = Rc::new(Cell::new(0));
        {
            let mut owner = CommandStartupOwner::new(Resource(Rc::clone(&drops))).unwrap();
            let outcome = owner.drive_client(async { std::panic::panic_any(71usize) });
            let Err(panic) = outcome else {
                panic!("client panic was lost")
            };
            match panic.downcast::<usize>() {
                Ok(value) => assert_eq!(*value, 71),
                Err(_) => panic!("client panic type was lost"),
            }
            assert_eq!(drops.get(), 0);
            assert_eq!(owner.client().phase(), CommandStartupPhase::Idle);
            assert!(!owner.driver_finished);
        }
        // There was no birth in this data control; this is not guest cleanup.
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn detached_request_is_refused_before_startup_state_or_birth() {
        let owner = CommandStartupOwner::new(()).unwrap();
        let client = owner.client();
        let mut command = Command::new("/not-executed-by-this-control");
        let refused = std::panic::catch_unwind(AssertUnwindSafe(|| {
            client.start(&mut command);
        }));
        assert!(refused.is_err());
        assert_eq!(client.phase(), CommandStartupPhase::Idle);
        assert!(
            owner
                .shared
                .native
                .borrow()
                .as_ref()
                .unwrap()
                .child_id()
                .is_none()
        );
        assert!(!owner.driver_finished);
    }
}
