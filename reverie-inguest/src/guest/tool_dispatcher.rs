/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in LICENSE.
 */

//! Sync Tool hosting adapter (W5 slice 2).
//!
//! [`ToolDispatcher`] is the consumer shape for in-process hosts
//! (literoot preload/W9): it publishes a [`ToolHost`] over a
//! config-only coordinator connection (zero-RPC replicated state)
//! and delegates trapped syscalls to [`InGuestDispatcher`] with a
//! no-patch seam, so every trap runs the Tool through the fallback
//! continuation in ordinary context. Registration is the existing
//! path only: `reverie_inguest::install(Box::new(dispatcher), …)`.

use std::collections::HashSet;
use std::io;
use std::sync::Arc;

use reverie::Pid;
use reverie::Tool;

use super::event::SyscallEvent;
use super::host::HostRuntime;
use super::host::ToolHost;
use super::installed_tool::publish_tool;
use super::rpc::CoordinatorRpc;
use super::trap_dispatch::InGuestDispatcher;
use super::trap_dispatch::TrapSeam;
use crate::dispatch::SyscallDispatcher;
use crate::dispatch::SyscallEvent as TrapEvent;

/// Trap seam that patches nothing: every trap runs the fallback
/// continuation (trap-only strategies; S5b promotion is separate).
struct NoPatchSeam;

// SAFETY: every method uses the TrapSeam defaults; the unsafe
// site methods are never reached with behavior beyond those
// defaults (syscall_site is the default resume-2/… computation
// inherited unchanged — see note in trap_dispatch).
unsafe impl TrapSeam for NoPatchSeam {}

/// Minimal in-process host runtime: no coordinator services beyond
/// the config-only connection. Fork hooks rebind the coordinator
/// fork flag through the host's own handling; statistics and the
/// RCB clock are unsupported by name, never faked.
struct InProcessRuntime;

impl HostRuntime for InProcessRuntime {
    fn fork_child_rebind(&self) {}
    fn emit_stage(&self, _stage: &[u8]) {}
    fn fork_child_reset(&self, _event: &SyscallEvent) {}
    fn cpuid_interception_enabled(&self) -> bool {
        false
    }
    fn exit_process_stats(&self, _tid: Pid) -> io::Result<()> {
        Ok(())
    }
    fn read_clock(&self) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "in-process runtime has no RCB clock",
        ))
    }
    fn signal_action_supported(&self, number: i64, args: [u64; 6]) -> bool {
        super::signal::signal_action_supported(number, args)
    }
    fn reserved_signal_mask(&self) -> u64 {
        1_u64 << (libc::SIGSYS - 1)
    }
}

/// A [`SyscallDispatcher`] hosting one Reverie [`Tool`] in-process.
///
/// Construction publishes the process's one installed Tool
/// (second construction fails with `AlreadyExists` from
/// `publish_tool` — surfaced at first dispatch-independent
/// `install` call site by the embedder retaining the error from
/// [`ToolDispatcher::publish`]; `new` itself cannot fail, so
/// publication is attempted in `new` and any error is retained
/// and returned by [`ToolDispatcher::publish_error`]).
pub struct ToolDispatcher<T: Tool> {
    inner: InGuestDispatcher<NoPatchSeam>,
    publish_error: Option<io::Error>,
    _tool: std::marker::PhantomData<T>,
}

impl<T: Tool + 'static> ToolDispatcher<T> {
    /// Host `tool` with replicated `config` (zero-RPC): builds the
    /// config-only coordinator connection, selects subscriptions
    /// from it, publishes the ToolHost. Call exactly once per
    /// process, before untrusted threads start (see `publish_tool`).
    pub fn new(tool: T, config: <T::GlobalState as reverie::GlobalTool>::Config) -> Self {
        let rpc = CoordinatorRpc::from_native_local_configuration(Arc::new(config));
        let configuration = reverie::ToolConfiguration::<T>::select(rpc.shared_config());
        let subscriptions: HashSet<reverie::syscalls::Sysno> =
            configuration.subscriptions().iter_syscalls().collect();
        let pid = Pid::from_raw(unsafe { libc::getpid() });
        let cpuid_interception = configuration.subscriptions().has_cpuid();
        let _ = &configuration;
        let host = ToolHost::<T, InProcessRuntime>::new(
            tool,
            rpc,
            pid,
            subscriptions,
            cpuid_interception,
            InProcessRuntime,
        );
        // SAFETY: constructor contract above (exactly once, before
        // untrusted threads; genuine config for this guest). The
        // embedder's install() call site upholds the same premises
        // publish_tool requires.
        let publish_error = unsafe { publish_tool(host) }.err();
        // SAFETY: default seam relies on nothing at trap addresses
        // (TrapSeam defaults read/change nothing there).
        let inner = unsafe { InGuestDispatcher::new(NoPatchSeam) };
        Self {
            inner,
            publish_error,
            _tool: std::marker::PhantomData,
        }
    }

    /// Publication failure retained from construction, if any.
    /// Embedders must check this before arming (a duplicate install
    /// retains the first host; dispatching would serve the wrong
    /// Tool).
    pub fn publish_error(&self) -> Option<&io::Error> {
        self.publish_error.as_ref()
    }
}

impl<T: Tool> SyscallDispatcher for ToolDispatcher<T> {
    fn dispatch(&self, event: &mut TrapEvent) {
        self.inner.dispatch(event);
    }

    fn dispatch_private_signal(&self, frame: &mut crate::trap::frame::SignalFrame<'_>) -> bool {
        self.inner.dispatch_private_signal(frame)
    }

    fn dispatch_signal(
        &self,
        event: &mut TrapEvent,
        frame: &mut crate::trap::frame::SignalFrame<'_>,
    ) {
        self.inner.dispatch_signal(event, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reverie::GlobalTool;
    use reverie::Guest;
    use reverie::syscalls::Syscall;
    use reverie::syscalls::Sysno;

    #[derive(Default)]
    struct G;
    #[reverie::tool]
    impl GlobalTool for G {
        type Request = u64;
        type Response = u64;
        type Config = u64;
        async fn receive_rpc(&self, _from: Pid, message: u64) -> u64 {
            message
        }
    }
    #[derive(Default)]
    struct Counting {
        seen: std::sync::Mutex<Vec<Sysno>>,
    }
    #[reverie::tool]
    impl Tool for Counting {
        type GlobalState = G;
        type ThreadState = ();
        fn init_thread_state(&self, _tid: Pid, _p: Option<(Pid, &())>) {}
        async fn handle_syscall_event<GuestT: Guest<Self>>(
            &self,
            guest: &mut GuestT,
            call: Syscall,
        ) -> Result<i64, reverie::Error> {
            {
                use reverie::syscalls::SyscallInfo;
                self.seen.lock().unwrap().push(call.number());
            }
            guest.inject(call).await.map_err(reverie::Error::from)
        }
    }

    #[test]
    fn tool_dispatcher_constructs_and_publishes_once() {
        // publish_tool is process-global: fork-isolate.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            let status = if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let dispatcher = ToolDispatcher::<Counting>::new(Counting::default(), 7_u64);
                assert!(dispatcher.publish_error().is_none());
                // Without an armed trap runtime/continuation the
                // shared dispatcher refuses by name (EOPNOTSUPP)
                // instead of running the Tool unsafely; with the
                // runtime armed (real preload) the same dispatch
                // path reaches the Tool through the continuation.
                let mut event = TrapEvent::direct(libc::SYS_getpid, [0; 6], 0x1000);
                dispatcher.dispatch(&mut event);
                assert_eq!(
                    event.result(),
                    Some(-95),
                    "unarmed refusal: {:?}",
                    event.result()
                );
            }))
            .is_ok()
            {
                0
            } else {
                125
            };
            unsafe { libc::_exit(status) }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status), "status {status:#x}");
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }
}
