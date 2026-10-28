/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * This source code is licensed under the BSD-style license in LICENSE.
 */

//! Actual native entry into the same ToolHost and concrete InGuest. These
//! methods do not install native services, own a scheduler or admit an image.

use super::native_state::DeferredNative;
use super::native_state::NativeCallbackLease;
use super::*;
use crate::tool_host::NativeCallbackOutcome;
use crate::tool_host::drive_native_callback;
use crate::tool_host::drive_native_ready;

/// A completed Tool callback, or its original still-active native continuation.
/// The latter retains the sole thread-state and scratch owner until the actual
/// native post-event or consuming exit owner completes it.
pub enum NativeCallResult<'owner, T: Tool, V> {
    /// All Guest borrows/futures ended and the original state was published.
    Return(V),
    /// The native owner must apply and complete this exact deferred operation.
    Deferred(DeferredNative<'owner, T>),
}

/// The actual instruction request supplied by the native entry owner.
pub enum NativeInstruction {
    /// The original EAX and ECX input registers.
    Cpuid { leaf: u32, subleaf: u32 },
    /// The actual RDTSC or RDTSCP instruction.
    Rdtsc(Rdtsc),
}

/// A Tool instruction answer. The original native context owner applies these
/// fields and preserves its remaining integer/FP/SIMD state.
pub enum NativeInstructionResult {
    /// EAX, EBX, ECX and EDX from the real Tool.
    Cpuid(reverie::CpuIdResult),
    /// The Tool's timestamp and optional auxiliary value.
    Rdtsc(reverie::RdtscResult),
}

/// The signal decision before the native owner commits the pending delivery.
pub enum NativeSignalResult {
    /// Deliver the same signal with either its authentic or replaced siginfo.
    Deliver {
        signal: reverie::Signal,
        info: [u8; reverie::SIGNAL_INFO_SIZE],
    },
    /// The Tool suppressed this signal.
    Suppress,
}

fn native_callback_error(reason: &'static str) -> Error {
    io::Error::other(reason).into()
}

impl<T: Tool, R: HostRuntime> ToolHost<T, R>
where
    T::GlobalState: 'static,
{
    /// Construct the one common Guest from an actual native lease and drive
    /// owned callback work. No registry/Tool-slot guard spans callbacks or I/O.
    /// The callback must return an owned result, never a Guest borrow/future.
    ///
    /// # Safety
    /// The actual native entry owner must have established its current image,
    /// task, service table, allocator backing, FS/TLS and native continuation
    /// contract. `services` belongs to this exact lease and remains live until
    /// every native use ends. A copied identity/handle or serialized phase flag
    /// is not that authority. No local or private-image dispatch may run here.
    /// The native post-event owner must retain and complete a Deferred result;
    /// an ordinary Rust return alone does not complete that operation.
    pub unsafe fn with_native_guest<'owner, V>(
        &self,
        mut callback: NativeCallbackLease<'owner, T>,
        services: &NativeServices<'_>,
        native_rpc: &NativeRpc<T::GlobalState>,
        event: &mut SyscallEvent,
        work: impl FnOnce(
            &T,
            &mut InGuest<'_, T, R>,
            &TailResult,
        ) -> NativeCallbackOutcome<Result<V, Error>>,
    ) -> Result<NativeCallResult<'owner, T, V>, Error> {
        let owner = self
            .native_threads
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))?;
        let invocation = services.invocation();
        invocation.ensure_current()?;
        let identity = callback.identity();
        if !callback.owned_by(owner)
            || identity != invocation.identity()
            || !Arc::ptr_eq(native_rpc.shared_config(), &self.rpc.shared_config())
        {
            return Err(io::Error::from_raw_os_error(libc::ESTALE).into());
        }

        // Selection of existing engine TLS/allocator scopes follows, rather
        // than substitutes for, the actual native owner's unsafe entry proof.
        // In particular, no process-global COMMITTED_STACKS reset is used.
        let outcome = {
            let _allocation = super::super::alloc::enter_dispatch();
            let _active = super::super::support::ToolCallbackGuard::enter();
            native_rpc.prepare(invocation)?;
            let (tool, state, lifecycle, scratch) = callback.dispatch_parts();
            let tail = TailResult::default();
            // Keep this route as a separate stable stack borrow: the Guest is
            // mutably borrowed by Tool handlers while sync global RPC borrows
            // this identical selected route, never the Guest itself.
            let application = native_rpc.application(invocation);
            let _route = super::super::native_rpc::NativeRpcScope::enter(&application, &tail);
            let mut guest = InGuest {
                event,
                tid: identity.tid(),
                pid: identity.pid(),
                ppid: services.facts().parent,
                state,
                rpc: &self.rpc,
                native: Some(NativeGuestView {
                    services,
                    rpc: native_rpc.application(invocation),
                    scratch,
                    signal_info_replacement: None,
                }),
                tail: &tail,
                cpuid_interception: services.facts().cpuid_interception,
                fork_parent_state: None,
                runtime: &self.runtime,
                staged_signals: Vec::new(),
                staged_signal_delivery: false,
                in_syscall_handler: false,
                #[cfg(feature = "exec-native-injection")]
                exec: None,
                #[cfg(feature = "exec-native-injection")]
                mapping_driver: None,
                unsupported_refusal: None,
                injected_syscalls: 0,
            };
            match enter_tool_callbacks(tool, &mut guest, lifecycle) {
                Ok(()) => work(
                    tool.as_ref().expect("pinned live Tool"),
                    &mut guest,
                    &tail,
                ),
                Err(EntryFailure::Tool(error)) => return Err(error),
                Err(EntryFailure::Ending(_)) => {
                    return Err(native_callback_error(
                        "a local signal ending reached native entry",
                    ));
                }
            }
        };
        // Every Tool future, Guest, global-route borrow and allocation scope
        // above has ended before any continuation/state owner is returned.
        invocation.ensure_current()?;
        match outcome {
            NativeCallbackOutcome::Ready(Err(Error::Errno(errno))) => {
                // The maintained native owner returns Linux errno to the
                // guest. That completed handler does not abandon its state or
                // enter the local ERESTARTSYS retry loop. Tool/Io failures in
                // the following arm still poison the abandoned callback.
                unsafe { callback.complete(identity)? };
                Err(Error::Errno(errno))
            }
            NativeCallbackOutcome::Ready(value) => {
                let value = value?;
                unsafe { callback.complete(identity)? };
                Ok(NativeCallResult::Return(value))
            }
            NativeCallbackOutcome::Deferred { number, args } => Ok(NativeCallResult::Deferred(
                DeferredNative::new(callback, number, args),
            )),
            NativeCallbackOutcome::Unsupported(reason) => Err(native_callback_error(reason)),
        }
    }

    /// Enter the actual subscribed syscall handler once. Native error/restart
    /// behavior is retained: this does not use the local ERESTARTSYS retry loop.
    ///
    /// # Safety
    /// All with_native_guest entry and deferred-completion contracts apply.
    /// The native owner must supply the original event and apply the returned
    /// result/deferred operation through its existing native bookkeeping.
    pub unsafe fn dispatch_native_syscall<'owner>(
        &self,
        callback: NativeCallbackLease<'owner, T>,
        services: &NativeServices<'_>,
        native_rpc: &NativeRpc<T::GlobalState>,
        event: &mut SyscallEvent,
    ) -> Result<NativeCallResult<'owner, T, i64>, Error> {
        unsafe {
            self.with_native_guest(
                callback,
                services,
                native_rpc,
                event,
                |tool, guest, tail| {
                    let invocation = services.invocation();
                    if let Some(result) = native_rpc.protect_operation(
                        invocation,
                        guest.event.number,
                        &guest.event.args,
                        true,
                    ) {
                        return NativeCallbackOutcome::Ready(Ok(result));
                    }
                    // Preserve the maintained native callback's conversion and
                    // invalid-number refusal. A malformed native event is not a
                    // guest syscall result and must not synthesize -ENOSYS.
                    let number = Sysno::from(guest.event.number as i32);
                    let args = guest.event.args;
                    let syscall = Syscall::from_raw(
                        number,
                        SyscallArgs::new(
                            args[0] as usize,
                            args[1] as usize,
                            args[2] as usize,
                            args[3] as usize,
                            args[4] as usize,
                            args[5] as usize,
                        ),
                    );
                    // Native interception is selected by its actual C owner.
                    // Like the maintained native driver, every event reaching
                    // this entry invokes Tool, independent of local hooks'
                    // subscription table. Only Tool may request deferral here.
                    guest.in_syscall_handler = true;
                    let outcome =
                        drive_native_callback(tool.handle_syscall_event(guest, syscall), tail);
                    guest.in_syscall_handler = false;
                    match outcome {
                        NativeCallbackOutcome::Deferred { number, args } => {
                            // Close/close_range still reached the Tool for accounting;
                            // the actual forwarded/deferred operation spares the same
                            // reserved primary descriptor, as before this factoring.
                            match native_rpc.protect_operation(invocation, number, &args, false) {
                                Some(result) => NativeCallbackOutcome::Ready(Ok(result)),
                                None => NativeCallbackOutcome::Deferred { number, args },
                            }
                        }
                        other => other,
                    }
                },
            )
        }
    }

    /// Drive the real instruction handler with this same concrete Guest. The
    /// native context owner retains its original output/commit protocol.
    ///
    /// # Safety
    /// All with_native_guest contracts apply, including real instruction
    /// delivery and caller-owned register/FP/SIMD state.
    pub unsafe fn dispatch_native_instruction<'owner>(
        &self,
        callback: NativeCallbackLease<'owner, T>,
        services: &NativeServices<'_>,
        native_rpc: &NativeRpc<T::GlobalState>,
        event: &mut SyscallEvent,
        instruction: NativeInstruction,
    ) -> Result<NativeCallResult<'owner, T, NativeInstructionResult>, Error> {
        unsafe {
            self.with_native_guest(
                callback,
                services,
                native_rpc,
                event,
                |tool, guest, _tail| {
                    // The maintained instruction owner polls with a no-op waker and
                    // yield_now, without reading a tail result. Do not substitute the
                    // application callback's wake/park or local syscall-restart loop.
                    let result = match instruction {
                        NativeInstruction::Cpuid { leaf, subleaf } => {
                            drive_native_ready(tool.handle_cpuid_event(guest, leaf, subleaf))
                                .map(NativeInstructionResult::Cpuid)
                                .map_err(Error::from)
                        }
                        NativeInstruction::Rdtsc(request) => {
                            drive_native_ready(tool.handle_rdtsc_event(guest, request))
                                .map(NativeInstructionResult::Rdtsc)
                                .map_err(Error::from)
                        }
                    };
                    NativeCallbackOutcome::Ready(result)
                },
            )
        }
    }

    /// Drive an authentic native signal continuation. Signal masks, queued
    /// delivery and physical interrupted context stay with the native owner.
    /// Changing the signal number is still refused, not silently ignored.
    ///
    /// # Safety
    /// All with_native_guest contracts apply. The owned siginfo and signal
    /// number must be from this same actual native pending delivery.
    pub unsafe fn dispatch_native_signal<'owner>(
        &self,
        callback: NativeCallbackLease<'owner, T>,
        services: &NativeServices<'_>,
        native_rpc: &NativeRpc<T::GlobalState>,
        event: &mut SyscallEvent,
        signal: i32,
    ) -> Result<NativeCallResult<'owner, T, NativeSignalResult>, Error> {
        let shown = reverie::Signal::try_from(signal)?;
        let original = services.facts().signal_info.ok_or(Errno::EINVAL)?;
        if i32::from_ne_bytes(original[..4].try_into().expect("siginfo signal field")) != signal {
            return Err(Errno::EINVAL.into());
        }
        unsafe {
            self.with_native_guest(
                callback,
                services,
                native_rpc,
                event,
                |tool, guest, tail| {
                    match drive_native_callback(tool.handle_signal_event(guest, shown), tail) {
                        NativeCallbackOutcome::Ready(Ok(Some(chosen))) if chosen == shown => {
                            let info = guest
                                .native
                                .as_ref()
                                .expect("native entry")
                                .signal_info_replacement
                                .unwrap_or(original);
                            NativeCallbackOutcome::Ready(Ok(NativeSignalResult::Deliver {
                                signal: shown,
                                info,
                            }))
                        }
                        NativeCallbackOutcome::Ready(Ok(Some(_))) => {
                            NativeCallbackOutcome::Ready(Err(native_callback_error(
                                "native signal-number transformation is unsupported",
                            )))
                        }
                        NativeCallbackOutcome::Ready(Ok(None)) => {
                            NativeCallbackOutcome::Ready(Ok(NativeSignalResult::Suppress))
                        }
                        NativeCallbackOutcome::Ready(Err(error)) => {
                            NativeCallbackOutcome::Ready(Err(error.into()))
                        }
                        NativeCallbackOutcome::Deferred { number, args } => {
                            // The existing signal continuation accepts the exact tail
                            // operation; native signal ownership decides its effects.
                            NativeCallbackOutcome::Deferred { number, args }
                        }
                        NativeCallbackOutcome::Unsupported(reason) => {
                            NativeCallbackOutcome::Unsupported(reason)
                        }
                    }
                },
            )
        }
    }
}
