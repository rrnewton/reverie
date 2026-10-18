/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The host kernel as the in-guest backend's [`LinuxProvider`].
//!
//! Every syscall a Tool injects through
//! [`Guest::inject`](reverie::Guest::inject) or
//! [`Guest::tail_inject`](reverie::Guest::tail_inject) that the backend
//! forwards to Linux runs here, through [`LinuxProvider::execute`]. The Tool
//! runs on the guest thread itself, so the call is that thread issuing the
//! syscall through the runtime's trusted gate. The provider owns no state:
//! the host kernel holds it all, and the gate token is the execution handle
//! ([`LinuxProvider::Context`]).
//!
//! The backend's filters stay in `InGuest`, before the token exists. They
//! protect what the runtime shares with the guest (its mappings, reserved
//! descriptors and signals, protection keys, staged endings and the private
//! loader), and none of them is Linux semantics. So are the calls the backend
//! answers or carries out itself: a plain fork, native exec, the virtual
//! signal calls and `pkey_alloc`. The provider receives only a call the
//! backend has admitted, and runs it unchanged.
//!
//! The provider produces only [`ProviderOutcome::Return`]. The call is a real
//! `syscall` instruction in the guest's own thread, so Linux restarts it or
//! returns `-EINTR` as for any thread, and both are results of a call that
//! ran: there is no stop before the instruction at which a signal could leave
//! it unrun ([`ProviderOutcome::Interrupted`]). Exits are staged before the
//! forward, nothing blocks in a cancellable way, and the host kernel creates
//! children and replaces images itself, so no other outcome arises.
//!
//! The provider is zero-sized and built at each call, so the call is
//! dispatched statically. `execute`'s future is still boxed, because the
//! trait is an `async_trait`; the box comes from the Tool heap, as every
//! injection's own future does. `execute` never awaits, so the future
//! completes on its first poll.

use std::convert::Infallible;
use std::marker::PhantomData;

use reverie::Tid;
use reverie::provider::LinuxProvider;
use reverie::provider::ProviderOutcome;
use reverie::provider::SyscallRequest;

use super::Error;
use super::Pid;
use super::io;
use super::tool_fatal;

/// The host kernel, reached through a backend gate.
pub(super) struct HostLinux<G>(PhantomData<fn() -> G>);

impl<G> HostLinux<G> {
    pub(super) fn new() -> Self {
        Self(PhantomData)
    }
}

/// How the backend reaches the host kernel for one admitted call.
///
/// A value is the proof that the backend's admission filters have run on the
/// call it is lent for; see each implementation's constructor.
pub(super) trait PhysicalGate: Send {
    /// A gate failure. It is a broken invariant, never a guest errno.
    type Error: Send;
    /// The guest thread on which the gate issues the call.
    fn tid(&self) -> Pid;
    /// Issues `number(args)` and returns Linux's raw result: a nonnegative
    /// value, or `-errno`.
    fn syscall(&mut self, number: i64, args: [u64; 6]) -> Result<i64, Self::Error>;
}

/// A broken backend invariant reported by [`LinuxProvider::execute`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GateError<E> {
    /// `execute` was asked to run a call for a thread other than the gate's.
    WrongTask {
        /// The tid `execute` was given.
        task: Tid,
        /// The gate's own tid.
        gate: Pid,
    },
    /// The gate failed.
    Gate(E),
}

#[reverie::provider]
impl<G: PhysicalGate> LinuxProvider for HostLinux<G> {
    type Context = G;
    /// The host kernel creates children and replaces images itself, so there
    /// is never a plan for the backend.
    type Plan = Infallible;
    type Error = GateError<G::Error>;

    /// Runs `request` through `cx`, the gate, and returns Linux's raw result
    /// unchanged as [`ProviderOutcome::Return`].
    ///
    /// `task` must be the gate's own tid, as the trait requires of a provider
    /// bound to one task; anything else is a broken backend invariant and
    /// returns `Err` without issuing the call.
    async fn execute(
        &mut self,
        task: Tid,
        request: &SyscallRequest,
        cx: &mut G,
    ) -> Result<ProviderOutcome<Infallible>, GateError<G::Error>> {
        if task != cx.tid() {
            return Err(GateError::WrongTask {
                task,
                gate: cx.tid(),
            });
        }
        let (number, args) = parts(request);
        cx.syscall(number, args)
            .map(ProviderOutcome::Return)
            .map_err(GateError::Gate)
    }
}

/// The request for a call of `number` with `args`. The number is carried bit
/// for bit, so [`parts`] gives back every `i64`.
fn request(number: i64, args: [u64; 6]) -> SyscallRequest {
    SyscallRequest::new(number as u64, args)
}

/// The syscall number and arguments of a request built by [`request`].
fn parts(request: &SyscallRequest) -> (i64, [u64; 6]) {
    (request.number() as i64, *request.args())
}

/// The raw result in `outcome`, if it is one `HostLinux` produces: only
/// [`ProviderOutcome::Return`] carries one.
fn raw_result(outcome: ProviderOutcome<Infallible>) -> Option<i64> {
    match outcome {
        ProviderOutcome::Return(raw) => Some(raw),
        _ => None,
    }
}

/// The trusted guest gate (`physical_guest_syscall6` without a protection-key
/// image), on the calling guest thread.
struct GuestGate {
    tid: Pid,
}

impl GuestGate {
    /// # Safety
    ///
    /// The gate is lent to exactly one [`LinuxProvider::execute`] call, on the
    /// guest thread `tid`, in its own syscall turn. Before that call, the
    /// backend's admission filters have run on exactly the request it is
    /// given, so that every obligation of
    /// [`physical_guest_syscall6`](crate::trap::physical_guest_syscall6) with
    /// `pkru` `None` holds for it.
    unsafe fn new(tid: Pid) -> Self {
        Self { tid }
    }
}

impl PhysicalGate for GuestGate {
    type Error = Infallible;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn syscall(&mut self, number: i64, args: [u64; 6]) -> Result<i64, Infallible> {
        // SAFETY: `GuestGate::new`'s caller admitted this call.
        Ok(unsafe { crate::trap::physical_guest_syscall6(number, args, None) }.result)
    }
}

/// Forwards `number(args)` for the guest thread `tid` to the host kernel,
/// through [`HostLinux`] over the trusted guest gate, and returns Linux's raw
/// result.
///
/// A broken invariant (`execute` refusing the call, or an outcome other than
/// `Return`) ends the process with status 125, as the backend's other broken
/// invariants do; it is never a guest errno. Neither can happen: the gate and
/// the request are built here for the same `tid`.
///
/// # Safety
///
/// `tid` is the calling guest thread, in its own syscall turn, and every
/// admission filter of the backend has run on exactly `number` and `args`, as
/// for [`physical_guest_syscall6`](crate::trap::physical_guest_syscall6) with
/// `pkru` `None`.
pub(super) async unsafe fn forward(tid: Pid, number: i64, args: [u64; 6]) -> i64 {
    // SAFETY: the caller admitted `number(args)` on `tid`, and the gate is
    // lent to this one call.
    let mut gate = unsafe { GuestGate::new(tid) };
    let outcome = HostLinux::<GuestGate>::new()
        .execute(tid, &request(number, args), &mut gate)
        .await;
    let broken = match outcome {
        Ok(outcome) => match raw_result(outcome) {
            Some(raw) => return raw,
            None => format!("HostLinux produced {outcome:?} for syscall {number}"),
        },
        Err(GateError::WrongTask { task, gate }) => {
            format!("HostLinux was asked to run syscall {number} for tid {task} on tid {gate}")
        }
        Err(GateError::Gate(never)) => match never {},
    };
    tool_fatal(125, &Error::Tool(io::Error::other(broken).into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_host::drive_ready;

    /// A gate that records every call and answers each with `result`.
    struct MockGate {
        tid: Pid,
        result: Result<i64, &'static str>,
        calls: Vec<(i64, [u64; 6])>,
    }

    impl MockGate {
        fn new(tid: i32, result: Result<i64, &'static str>) -> Self {
            Self {
                tid: Pid::from_raw(tid),
                result,
                calls: Vec::new(),
            }
        }
    }

    impl PhysicalGate for MockGate {
        type Error = &'static str;

        fn tid(&self) -> Pid {
            self.tid
        }

        fn syscall(&mut self, number: i64, args: [u64; 6]) -> Result<i64, &'static str> {
            self.calls.push((number, args));
            self.result
        }
    }

    fn execute(
        task: i32,
        number: i64,
        args: [u64; 6],
        gate: &mut MockGate,
    ) -> Result<ProviderOutcome<Infallible>, GateError<&'static str>> {
        drive_ready(HostLinux::<MockGate>::new().execute(
            Pid::from_raw(task),
            &request(number, args),
            gate,
        ))
    }

    const ARGS: [u64; 6] = [0, 1, u64::MAX, 0x7fff_ffff_f000, 42, 1 << 63];

    /// The provider and the guest gate carry no state of their own.
    #[test]
    fn the_provider_is_zero_sized() {
        assert_eq!(size_of::<HostLinux<GuestGate>>(), 0);
        assert_eq!(size_of::<HostLinux<MockGate>>(), 0);
    }

    /// The gate receives the request's number and all six arguments
    /// unchanged, including arguments with the top bit set and numbers
    /// outside the syscall table, and is called exactly once.
    #[test]
    fn the_request_reaches_the_gate_unchanged() {
        for number in [
            libc::SYS_getppid,
            libc::SYS_read,
            libc::SYS_rt_sigprocmask,
            0,
            -1,
            i64::MAX,
            i64::MIN,
        ] {
            assert_eq!(parts(&request(number, ARGS)), (number, ARGS));
            let mut gate = MockGate::new(7, Ok(0));
            execute(7, number, ARGS, &mut gate).unwrap();
            assert_eq!(gate.calls, [(number, ARGS)]);
        }
    }

    /// Linux's raw result comes back as `Return`, bit for bit: a success, a
    /// success that reads as a large negative number outside the errno
    /// range, and every errno from the smallest to the largest.
    #[test]
    fn the_raw_result_is_returned_unchanged() {
        for raw in [0, 1, -1, -4, -512, -4095, -4096, i64::MAX, i64::MIN] {
            let mut gate = MockGate::new(7, Ok(raw));
            assert_eq!(
                execute(7, libc::SYS_getppid, ARGS, &mut gate),
                Ok(ProviderOutcome::Return(raw))
            );
            assert_eq!(raw_result(ProviderOutcome::Return(raw)), Some(raw));
        }
    }

    /// A call for a thread other than the gate's is refused as a broken
    /// invariant, and the gate is never called.
    #[test]
    fn a_call_for_another_thread_is_refused_without_running() {
        let mut gate = MockGate::new(7, Ok(0));
        assert_eq!(
            execute(8, libc::SYS_getppid, ARGS, &mut gate),
            Err(GateError::WrongTask {
                task: Pid::from_raw(8),
                gate: Pid::from_raw(7),
            })
        );
        assert!(gate.calls.is_empty());
    }

    /// A gate failure is a broken invariant, not a guest errno.
    #[test]
    fn a_gate_failure_is_an_error_not_a_result() {
        let mut gate = MockGate::new(7, Err("permit lost"));
        assert_eq!(
            execute(7, libc::SYS_getppid, ARGS, &mut gate),
            Err(GateError::Gate("permit lost"))
        );
    }

    /// Only `Return` carries a raw result. Every other outcome is one
    /// `HostLinux` never produces, which `forward` treats as a broken
    /// invariant.
    #[test]
    fn only_a_return_outcome_carries_a_raw_result() {
        assert_eq!(raw_result(ProviderOutcome::Interrupted), None);
        assert_eq!(raw_result(ProviderOutcome::Cancelled), None);
        assert_eq!(
            raw_result(ProviderOutcome::ThreadGroupExited {
                status: reverie::ExitStatus::Exited(0),
            }),
            None
        );
        assert_eq!(
            raw_result(ProviderOutcome::SegmentBase {
                result: 0,
                register: reverie::provider::SegmentRegister::Fs,
                base: 0x1000,
            }),
            None
        );
    }

    /// The guest gate, over a real `getppid`, returns what the kernel
    /// returns, through the provider. Outside an installed runtime the
    /// trusted gate is an ordinary `syscall` instruction.
    #[test]
    fn the_guest_gate_runs_the_call_on_the_host() {
        let tid = Pid::from_raw(unsafe { libc::gettid() });
        let raw = drive_ready(unsafe { forward(tid, libc::SYS_getppid, [0; 6]) });
        assert_eq!(raw, i64::from(unsafe { libc::getppid() }));
    }
}
