/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::io;
use std::net::SocketAddr;
use std::path::Path;

use bytes::BytesMut;
use futures::future;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use super::error::Error;
use super::inferior::StoppedInferior;
use super::packet::Packet;
use super::session::Session;

/// GdbServer controller
pub struct GdbServer {
    /// Signal gdbserver to start.
    pub server_tx: Option<oneshot::Sender<()>>,
    /// Signal gdbserver the very first tracee is ready.
    pub inferior_attached_tx: Option<mpsc::Sender<StoppedInferior>>,
    /// FIXME: the tracees are serialized already, tell gdbserver not to
    /// serialize by its own.
    pub sequentialized_guest: bool,
}

impl GdbServer {
    /// Creates a GDB server and binds to the given address.
    ///
    /// NOTE: The canonical GDB server port is `1234`.
    pub async fn from_addr(addr: SocketAddr) -> Result<Self, Error> {
        let (inferior_attached_tx, inferior_attached_rx) = mpsc::channel(1);
        let (server_tx, server_rx) = oneshot::channel();

        let server = GdbServerImpl::from_addr(addr, server_rx, inferior_attached_rx).await?;
        tokio::task::spawn(async move {
            if let Err(err) = server.run().await {
                tracing::error!("Failed to run gdbserver: {:?}", err);
            }
        });
        Ok(Self {
            server_tx: Some(server_tx),
            inferior_attached_tx: Some(inferior_attached_tx),
            sequentialized_guest: false,
        })
    }

    /// Creates a GDB server from the given unix domain socket. This is useful
    /// when we know there will only be one client and want to avoid binding to a
    /// port.
    pub async fn from_path(path: &Path) -> Result<Self, Error> {
        let (inferior_attached_tx, inferior_attached_rx) = mpsc::channel(1);
        let (server_tx, server_rx) = oneshot::channel();

        let server = GdbServerImpl::from_path(path, server_rx, inferior_attached_rx).await?;
        tokio::task::spawn(async move {
            if let Err(err) = server.run().await {
                tracing::error!("Failed to run gdbserver: {:?}", err);
            }
        });
        Ok(Self {
            server_tx: Some(server_tx),
            inferior_attached_tx: Some(inferior_attached_tx),
            sequentialized_guest: false,
        })
    }

    pub fn sequentialized_guest(&mut self) -> &mut Self {
        self.sequentialized_guest = true;
        self
    }

    #[allow(unused)]
    pub async fn notify_start(&mut self) -> Result<(), Error> {
        if let Some(tx) = self.server_tx.take() {
            tx.send(()).map_err(|_| Error::GdbServerNotStarted)
        } else {
            Ok(())
        }
    }

    #[allow(unused)]
    pub async fn notify_gdb_stop(&mut self, stopped: StoppedInferior) -> Result<(), Error> {
        if let Some(tx) = self.inferior_attached_tx.take() {
            tx.send(stopped)
                .await
                .map_err(|_| Error::GdbServerSendPacketError)
        } else {
            Ok(())
        }
    }
}

struct GdbServerImpl {
    reader: Box<dyn AsyncRead + Send + Unpin>,
    pkt_tx: mpsc::Sender<Packet>,
    server_rx: Option<oneshot::Receiver<()>>,
    session: Option<Session>,
}

/// Binds to the given address and waits for an incoming connection.
async fn wait_for_tcp_connection(addr: SocketAddr) -> io::Result<TcpStream> {
    // NOTE: `tokio::net::TcpListener::bind` is not used here on purpose. It
    // spawns an additional tokio worker thread. We want to avoid an extra
    // thread here since it could perturb the deterministic allocation of PIDs.
    // Using `std::net::TcpListener::bind` appears to avoid spawning an extra
    // tokio worker thread.
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(listener)?;

    let (stream, client_addr) = listener.accept().await?;

    tracing::info!("Accepting client connection: {:?}", client_addr);

    Ok(stream)
}

/// Binds to the given socket path and waits for an incoming connection.
async fn wait_for_unix_connection(path: &Path) -> io::Result<UnixStream> {
    let listener = UnixListener::bind(path)?;

    let (stream, client_addr) = listener.accept().await?;

    tracing::info!("Accepting client connection: {:?}", client_addr);

    Ok(stream)
}

// NB: during handshake, gdb may send packet prefixed with `+' (Ack), or send
// `+' then the actual packet (send two times). Since Ack is also a valid packet
// This may cause confusion to Packet::try_from(), since it tries to decode one
// packet at a time.
enum PacketWithAck {
    // Just a packet, note `+' only is considered to be `JustPacket'.
    JustPacket(Packet),
    // `+' (Ack) followed by a packet, such as `+StartNoAckMode'.
    WithAck(Packet),
}

const PACKET_BUFFER_CAPACITY: usize = 0x8000;

impl GdbServerImpl {
    /// Creates a new gdbserver, by accepting remote connection at `addr`.
    async fn from_addr(
        addr: SocketAddr,
        server_rx: oneshot::Receiver<()>,
        inferior_attached_rx: mpsc::Receiver<StoppedInferior>,
    ) -> Result<Self, Error> {
        let stream = wait_for_tcp_connection(addr)
            .await
            .map_err(|source| Error::WaitForGdbConnect { source })?;
        let (reader, writer) = stream.into_split();

        let (tx, rx) = mpsc::channel(1);
        // create a gdb session.
        let session = Session::new(Box::new(writer), rx, inferior_attached_rx);

        Ok(GdbServerImpl {
            reader: Box::new(reader),
            pkt_tx: tx,
            server_rx: Some(server_rx),
            session: Some(session),
        })
    }

    /// Creates a GDB server and listens on the given unix domain socket.
    async fn from_path(
        path: &Path,
        server_rx: oneshot::Receiver<()>,
        inferior_attached_rx: mpsc::Receiver<StoppedInferior>,
    ) -> Result<Self, Error> {
        let stream = wait_for_unix_connection(path)
            .await
            .map_err(|source| Error::WaitForGdbConnect { source })?;

        let (reader, writer) = stream.into_split();
        let (tx, rx) = mpsc::channel(1);

        // Create a gdb session.
        let session = Session::new(Box::new(writer), rx, inferior_attached_rx);

        Ok(GdbServerImpl {
            reader: Box::new(reader),
            pkt_tx: tx,
            server_rx: Some(server_rx),
            session: Some(session),
        })
    }

    async fn recv_packet(&mut self) -> Result<PacketWithAck, Error> {
        let mut rx_buf = BytesMut::with_capacity(PACKET_BUFFER_CAPACITY);
        self.reader
            .read_buf(&mut rx_buf)
            .await
            .map_err(|_| Error::ConnReset)?;

        // packet to follow, such as `+StartNoAckMode`.
        Ok(if rx_buf.starts_with(b"+") && rx_buf.len() > 1 {
            PacketWithAck::WithAck(Packet::new(rx_buf.split_off(1))?)
        } else {
            PacketWithAck::JustPacket(Packet::new(rx_buf.split())?)
        })
    }

    async fn send_packet(&mut self, packet: Packet) -> Result<(), Error> {
        self.pkt_tx
            .send(packet)
            .await
            .map_err(|_| Error::GdbServerSendPacketError)
    }

    async fn relay_gdb_packets(&mut self) -> Result<(), Error> {
        while let Ok(pkt) = self.recv_packet().await {
            match pkt {
                PacketWithAck::JustPacket(pkt) => {
                    self.send_packet(Packet::Ack).await?;
                    self.send_packet(pkt).await?;
                }
                PacketWithAck::WithAck(pkt) => self.send_packet(pkt).await?,
            }
        }

        // remote client closed connection.
        Ok(())
    }

    /// Run gdbserver.
    ///
    /// The gdbserver can run in a separate tokio thread pool.
    ///
    /// ```no_compile
    /// let gdbserver = GdbServer::new(..).await?;
    /// let handle = tokio::task::spawn(gdbserver.run());
    /// // snip
    /// handle.await??
    /// ```
    async fn run(mut self) -> Result<(), Error> {
        // NB: waiting for initial request to start gdb server. This is
        // required because if gdbserver is started too soon, gdb (client)
        // could get timeout. Some requests such as `g' needs IPC with a
        // gdb session, which only becomes ready later.
        if let Some(server_rx) = self.server_rx.take() {
            server_rx.await.map_err(|_| Error::GdbServerNotStarted)?;
            let mut session = self.session.take().ok_or(Error::SessionNotStarted)?;
            let run_session = session.run();
            let run_loop = self.relay_gdb_packets();
            future::try_join(run_session, run_loop).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `wait_for_tcp_connection` must leave NOTHING LISTENING once it has
    /// accepted its client.
    ///
    /// ⚠️ THIS IS A CROSS-REPOSITORY CONTRACT AND IT IS LOAD-BEARING IN HERMIT.
    /// hermit's gdb-client watcher (`hermit-cli/src/bin/hermit/gdb_client.rs`)
    /// decides whether a container was stranded waiting for a debugger that will
    /// never come by CONNECTING TO THIS PORT after it observes its spawned `gdb`
    /// exit; a successful connect is its evidence that an accept was still
    /// pending. That inference is sound ONLY because this function drops its
    /// listener the moment it returns.
    ///
    /// If this is ever changed to keep the listener bound -- to support
    /// reconnect, say -- then every HEALTHY gdb session that finishes before its
    /// container does gets reported as "the gdb client hermit spawned exited
    /// before it finished connecting". That is hermit's defect 2 restored, and
    /// NOTHING ON HERMIT'S SIDE CAN DETECT IT: to the watcher, a bound port plus
    /// a dead client is indistinguishable from a stranded accept. The check has
    /// to live here, next to the fact it depends on.
    #[tokio::test(flavor = "current_thread")]
    async fn the_listener_is_closed_once_the_client_is_accepted() {
        // A port that is free right now. Bind-and-drop is the only portable way
        // to learn one, because `wait_for_tcp_connection` insists on binding the
        // address itself and will not take a listener we made.
        let addr: SocketAddr = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("failed to bind a probe listener to find a free port");
            probe
                .local_addr()
                .expect("probe listener has no local addr")
        };

        let server = tokio::spawn(wait_for_tcp_connection(addr));

        // Connect as the real client would. Bounded, because a wedged test costs
        // the whole run's budget while a red one names itself in a line.
        let mut client = None;
        for _ in 0..300 {
            if let Ok(s) = std::net::TcpStream::connect(addr) {
                client = Some(s);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let client = client.expect("could not connect to the gdbserver port within 3s");

        let accepted = server
            .await
            .expect("the accept task panicked")
            .expect("wait_for_tcp_connection failed");

        // ⚠️ THE ASSERTION. The client is accepted and its stream is still open,
        // so anything still answering on this port is a LISTENER, not this
        // session. A second connection must therefore be refused.
        let second = std::net::TcpStream::connect(addr);
        assert!(
            second.is_err(),
            "the gdbserver port is STILL LISTENING after the client was accepted; \
             hermit's watcher reads a successful connect here as proof that a container \
             is stranded, so a healthy finished session would be reported as a client \
             that exited before connecting"
        );

        drop(accepted);
        drop(client);
    }
}
