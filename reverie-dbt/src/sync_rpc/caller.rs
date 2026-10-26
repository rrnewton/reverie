// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

//! Strict caller-mode borrowing of the existing native primary connection.

#[cfg(test)]
mod tests;

use reverie::Error;
use reverie::GlobalRPC;
use reverie::tool_host::CallbackPorts;
use reverie::tool_host::InitialRootAdmission;
use reverie::tool_host::TaskIdentity;

use super::*;

#[derive(Clone, Copy)]
pub(crate) struct CallContext {
    pub context: usize,
    pub invoke: SyscallInvoker,
    pub identity: TaskIdentity,
}

pub(crate) struct PrimaryBinding<G: GlobalTool> {
    config: G::Config,
    identity: std::sync::Arc<()>,
    root: TaskIdentity,
}

impl<G: GlobalTool> PrimaryBinding<G> {
    pub(crate) fn connect(call: CallContext) -> Result<Self, Error> {
        if call.identity.pid != call.identity.tid
            || call.identity.pid.as_raw() as u32 != std::process::id()
        {
            return Err(io::Error::other("caller Tool requires the initial physical root").into());
        }
        let path = std::env::var_os(RPC_SOCKET_ENV)
            .ok_or_else(|| io::Error::other("caller Tool requires its primary coordinator"))?;
        let mut slot = GUEST_CLIENT
            .connection
            .try_lock()
            .map_err(|_| io::Error::other("caller primary connection is busy or poisoned"))?;
        if slot.is_some() {
            return Err(io::Error::other(
                "caller primary connection was already prepared by another owner",
            )
            .into());
        }
        let (connection, bytes) = connect_guest_with_config(
            call.context,
            call.invoke,
            Path::new(&path),
            std::process::id(),
        )?;
        let config = match decode_exact(&bytes) {
            Ok(config) => config,
            Err(error) => {
                close_guest(call.context, call.invoke, connection.fd);
                return Err(io::Error::other(error).into());
            }
        };
        let identity = std::sync::Arc::clone(&connection.identity);
        GUEST_CLIENT.primary.store(
            (u64::from(connection.pid) << 32) | u64::from(connection.fd as u32),
            Ordering::Release,
        );
        *slot = Some(connection);
        Ok(Self {
            config,
            identity,
            root: call.identity,
        })
    }
}

pub(crate) struct PrimaryPort<'a, G: GlobalTool> {
    binding: &'a PrimaryBinding<G>,
    call: CallContext,
}
impl<G: GlobalTool> Copy for PrimaryPort<'_, G> {}
impl<G: GlobalTool> Clone for PrimaryPort<'_, G> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<G: GlobalTool> CallbackPorts<G> for PrimaryBinding<G> {
    type Context<'a>
        = CallContext
    where
        Self: 'a;
    type Port<'a>
        = PrimaryPort<'a, G>
    where
        Self: 'a;
    fn config(&self) -> &G::Config {
        &self.config
    }
    fn borrow<'a>(&'a self, call: CallContext) -> Result<PrimaryPort<'a, G>, Error> {
        if call.identity != self.root {
            return Err(
                io::Error::other("caller primary belongs to a different physical task").into(),
            );
        }
        let port = PrimaryPort {
            binding: self,
            call,
        };
        port.exchange(|_| Ok(()))?;
        Ok(port)
    }
}

impl<G: GlobalTool> PrimaryPort<'_, G> {
    fn exchange<V>(
        &self,
        operation: impl FnOnce(
            &mut reverie_rpc_transport::BorrowedBlockingRpcClient<'_, G, GuestIo>,
        ) -> Result<V, RpcError>,
    ) -> Result<V, Error> {
        let slot = GUEST_CLIENT
            .connection
            .try_lock()
            .map_err(|_| io::Error::other("caller primary connection is busy or poisoned"))?;
        let connection = slot
            .as_ref()
            .filter(|connection| {
                connection.usable
                    && connection.pid == std::process::id()
                    && connection.pid == self.call.identity.pid.as_raw() as u32
                    && GUEST_CLIENT.descriptor(connection.pid) == Some(connection.fd)
                    && std::sync::Arc::ptr_eq(&connection.identity, &self.binding.identity)
            })
            .ok_or_else(|| {
                io::Error::other("caller primary connection identity changed or retired")
            })?;
        let mut stream = GuestIo {
            context: self.call.context,
            invoke_syscall: self.call.invoke,
            fd: connection.fd,
        };
        operation(&mut BlockingRpcClient::<G>::from_borrowed_stream(
            &mut stream,
            self.call.identity.tid,
            &self.binding.config,
        ))
        .map_err(|error| io::Error::other(error).into())
    }

    pub(crate) fn declare(admission: InitialRootAdmission<Self>) -> Result<(), Error> {
        let (port, root) = admission.into_port();
        if root != port.call.identity {
            return Err(io::Error::other("caller admission belongs to another task").into());
        }
        port.exchange(|client| client.declare_initial_root_ready())
    }
}

#[reverie::tool]
impl<G: GlobalTool> GlobalRPC<G> for PrimaryPort<'_, G> {
    async fn send_rpc(&self, request: G::Request) -> G::Response {
        self.exchange(|client| client.try_send_rpc(request))
            .expect("caller Tool primary RPC failed")
    }
    fn try_send_rpc_sync(&self, request: G::Request) -> Result<G::Response, G::Request> {
        Ok(self
            .exchange(|client| client.try_send_rpc(request))
            .expect("caller Tool primary RPC failed"))
    }
    fn config(&self) -> &G::Config {
        &self.binding.config
    }
}
