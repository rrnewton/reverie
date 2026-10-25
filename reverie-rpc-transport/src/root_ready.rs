/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! One initial-root declaration, separate from configuration and application RPC.

use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::process::Child;
use std::sync::Arc;
use std::sync::Mutex;

use reverie::Tid;

use crate::RpcError;

pub(crate) const CONTROL_LEN: usize = 12;
const FAMILY: &[u8; 6] = b"\xff\xff\xff\xffRI";

pub(crate) fn invalid(reason: impl std::fmt::Display) -> RpcError {
    io::Error::other(format!("initial-root readiness: {reason}")).into()
}

pub(crate) fn request(sender: Tid) -> Result<[u8; CONTROL_LEN], RpcError> {
    if sender.as_raw() <= 0 {
        return Err(invalid("sender must be positive"));
    }
    let mut bytes = [0; CONTROL_LEN];
    bytes[..6].copy_from_slice(FAMILY);
    bytes[6] = 1;
    bytes[8..].copy_from_slice(&sender.as_raw().to_le_bytes());
    Ok(bytes)
}

/// Recognize the family before validating its version, opcode or body.
pub(crate) fn decode_request(bytes: &[u8]) -> Result<Option<Tid>, RpcError> {
    if bytes.len() < FAMILY.len() {
        return if !bytes.is_empty() && FAMILY.starts_with(bytes) {
            Err(invalid("truncated reserved control prefix"))
        } else {
            Ok(None)
        };
    }
    if !bytes.starts_with(FAMILY) {
        return Ok(None);
    }
    if bytes.len() != CONTROL_LEN || bytes[6] != 1 || bytes[7] != 0 {
        return Err(invalid("invalid control length, version or opcode"));
    }
    let sender = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if sender <= 0 {
        return Err(invalid("sender must be positive"));
    }
    Ok(Some(Tid::from_raw(sender)))
}

pub(crate) fn ack(sender: Tid) -> Result<[u8; CONTROL_LEN], RpcError> {
    let mut bytes = request(sender)?;
    bytes[7] = 1;
    Ok(bytes)
}

pub(crate) fn check_ack(sender: Tid, bytes: &[u8]) -> Result<(), RpcError> {
    if bytes != ack(sender)? {
        return Err(invalid("acknowledgement differs"));
    }
    Ok(())
}

fn live(pidfd: &OwnedFd) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result != 0 || descriptor.revents != 0 {
        return Err(io::Error::other("process is exited or pidfd is unusable"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Phase {
    Unseen,
    AckPending(u64),
    Declared,
    Failed(&'static str),
}

struct State {
    attached: bool,
    next_token: u64,
    phase: Phase,
}

struct Inner {
    pid: i32,
    pidfd: OwnedFd,
    state: Mutex<State>,
}

/// Observes one declaration intended for a launch root's initial image.
///
/// This is a transport fact, not proof of Tool installation, activation or an
/// image identity. Configuration and application requests never declare it.
/// A launcher must still wait, drain connections and check their failures and
/// the actual exit status. An ACK observed by a client does not imply this
/// monitor's final result: a later duplicate or cancellation can fail it.
///
/// There is one declaration over the bound process lifetime. Exec/reconnection
/// does not reset it. A later installer must identify initial installation from
/// its own lifecycle facts and must not declare again from post-exec hooks.
#[derive(Clone)]
pub struct InitialRootReadyMonitor(Arc<Inner>);

impl InitialRootReadyMonitor {
    /// Bind the launcher's actual, never-reaped child without consuming status.
    ///
    /// The caller owns all waiting and must exclude external waiters, including
    /// SIGCHLD auto-reaping, and keep the child unreaped until its PID/PGID-based
    /// cleanup finishes. A cached `Child` whose PID was reused after a previous
    /// reap is not an identity capability; this API cannot detect that misuse.
    /// Server, child and declaring client must share a PID namespace.
    ///
    /// An exited child refuses with its kernel wait status still available.
    /// This method never calls a reaping wait, kills the child, or adopts its
    /// cleanup responsibility. The returned monitor retains its own pidfd.
    pub fn for_child(root: &mut Child) -> Result<Self, RpcError> {
        let pid = i32::try_from(root.id()).map_err(|_| invalid("invalid root PID"))?;
        if pid <= 0 {
            return Err(invalid("invalid root PID"));
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                root.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(invalid(format_args!(
                "root wait observation failed: {}",
                io::Error::last_os_error()
            )));
        }
        if unsafe { info.si_pid() } != 0 {
            return Err(invalid("root already exited"));
        }
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(invalid(format_args!(
                "root pidfd unavailable: {}",
                io::Error::last_os_error()
            )));
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
        live(&pidfd).map_err(|_| invalid("root is not live"))?;
        Ok(Self(Arc::new(Inner {
            pid,
            pidfd,
            state: Mutex::new(State {
                attached: false,
                next_token: 0,
                phase: Phase::Unseen,
            }),
        })))
    }

    /// Require a completed declaration and no later readiness failure.
    ///
    /// Call after the launcher's existing connection drain. This does not
    /// require the root still alive and does not replace other run outcomes.
    pub fn check_declared(&self) -> Result<(), RpcError> {
        let state = self.0.state.lock().map_err(|_| invalid("state poisoned"))?;
        match state.phase {
            Phase::Declared => Ok(()),
            Phase::Failed(reason) => Err(invalid(reason)),
            Phase::Unseen => Err(invalid("declaration missing")),
            Phase::AckPending(_) => Err(invalid("acknowledgement incomplete")),
        }
    }

    pub(crate) fn attach(&self) -> Result<(), RpcError> {
        let mut state = self.0.state.lock().map_err(|_| invalid("state poisoned"))?;
        if state.attached {
            return Err(invalid("monitor already attached"));
        }
        state.attached = true;
        Ok(())
    }

    pub(crate) fn fail(&self, reason: &'static str) -> RpcError {
        // Drop may reach this while unwinding. Poison never triggers a second
        // panic, and check_declared still refuses a poisoned lock.
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !matches!(state.phase, Phase::Failed(_)) {
            state.phase = Phase::Failed(reason);
        }
        invalid(reason)
    }

    pub(crate) fn capture(&self, stream: &impl AsRawFd) -> InitialRootConnection {
        InitialRootConnection {
            monitor: self.clone(),
            peer: PhysicalPeer::capture(stream),
        }
    }
}

struct PhysicalPeer {
    pid: i32,
    pidfd: OwnedFd,
}

impl PhysicalPeer {
    fn capture(stream: &impl AsRawFd) -> io::Result<Self> {
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&credentials) as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if len as usize != std::mem::size_of_val(&credentials) || credentials.pid <= 0 {
            return Err(io::Error::other("peer PID unavailable"));
        }
        Ok(Self {
            pid: credentials.pid,
            // The socket's original connector, never pidfd_open(cached_pid).
            pidfd: crate::server::peer_pidfd(stream)?,
        })
    }
}

/// Opaque actual-socket facts; callers can wrap I/O but cannot invent identity.
pub(crate) struct InitialRootConnection {
    monitor: InitialRootReadyMonitor,
    peer: io::Result<PhysicalPeer>,
}

impl InitialRootConnection {
    pub(crate) fn malformed(&self) -> RpcError {
        self.monitor.fail("malformed readiness control")
    }

    pub(crate) fn begin(&self, sender: Tid) -> Result<PendingAck, RpcError> {
        let peer = self
            .peer
            .as_ref()
            .map_err(|_| self.monitor.fail("connecting peer unavailable"))?;
        live(&peer.pidfd).map_err(|_| self.monitor.fail("connecting peer is not live"))?;
        live(&self.monitor.0.pidfd).map_err(|_| self.monitor.fail("root is not live"))?;
        if peer.pid != self.monitor.0.pid {
            return Err(self.monitor.fail("connecting peer differs from root"));
        }
        if sender.as_raw() != self.monitor.0.pid {
            return Err(self.monitor.fail("encoded sender differs from root"));
        }
        let mut state = self
            .monitor
            .0
            .state
            .lock()
            .map_err(|_| invalid("state poisoned"))?;
        if !state.attached {
            return Err(invalid("monitor is not attached"));
        }
        if !matches!(state.phase, Phase::Unseen) {
            if !matches!(state.phase, Phase::Failed(_)) {
                state.phase = Phase::Failed("duplicate declaration");
            }
            return Err(invalid("duplicate or failed declaration"));
        }
        let Some(token) = state.next_token.checked_add(1) else {
            state.phase = Phase::Failed("declaration identity exhausted");
            return Err(invalid("declaration identity exhausted"));
        };
        state.next_token = token;
        state.phase = Phase::AckPending(token);
        Ok(PendingAck {
            monitor: self.monitor.clone(),
            token,
            completed: false,
        })
    }
}

pub(crate) struct PendingAck {
    monitor: InitialRootReadyMonitor,
    token: u64,
    completed: bool,
}

impl PendingAck {
    pub(crate) fn commit(mut self) -> Result<(), RpcError> {
        let mut state = self
            .monitor
            .0
            .state
            .lock()
            .map_err(|_| invalid("state poisoned"))?;
        if !matches!(state.phase, Phase::AckPending(token) if token == self.token) {
            return Err(invalid("pending declaration was invalidated"));
        }
        state.phase = Phase::Declared;
        self.completed = true;
        Ok(())
    }
}

impl Drop for PendingAck {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.monitor.fail("acknowledgement did not complete");
        }
    }
}
