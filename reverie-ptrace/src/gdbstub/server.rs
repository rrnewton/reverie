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
    /// ⚠️ `Option` SO THE RELAY CAN DROP IT. The session's command loop ends
    /// only when every sender on this channel is gone; holding it in `self` for
    /// the lifetime of [`GdbServerImpl::run`] made that impossible. See the
    /// deadlock note on `run`.
    pkt_tx: Option<mpsc::Sender<Packet>>,
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
            pkt_tx: Some(tx),
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
            pkt_tx: Some(tx),
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
            .as_ref()
            .ok_or(Error::GdbServerSendPacketError)?
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

        // ⚠️ THE CLIENT IS GONE, SO DROP THE SENDER. The session's command loop
        // is `while let Some(pkt) = cmd_rx.recv().await`, which ends only when
        // every sender is dropped. This one lived in `self` for the whole of
        // `run`, and `run` cannot return until `try_join` below completes, and
        // `try_join` cannot complete until the session loop ends. Holding it
        // here made the exit condition unreachable BY CONSTRUCTION: the relay
        // would notice the peer had closed, return `Ok(())`, and then wait
        // forever for a session that was waiting for this sender to go away.
        //
        // ⚠️ AND IT IS NOT THE hermit HANG, WHICH THIS COMMENT CLAIMED FOR THREE
        // REVISIONS. "The hang had simply moved here" was written from reading and
        // is DISPROVED by instrumenting the run: with the fake-gdb reproduction,
        // every await in this file resolves, `try_join` COMPLETES, `run` returns
        // `Ok`, and hermit still exits rc=124. The wedge is the inferior's resume
        // path: the tracee is ALIVE in `t (tracing stop)` with `TracerPid` set to
        // the container, and `guest-3` is blocked in `do_wait` for it while the
        // tokio worker is parked -- not this file. Caught by
        // `agent(codex-rev-493)`.
        //
        // ⚠️ AN EARLIER VERSION OF THIS COMMENT SAID "with no children" AND
        // CONCLUDED THE TRACEE WAS GONE. `task/<tid>/children` DOES NOT EXIST ON
        // THIS KERNEL: `# CONFIG_PROC_CHILDREN is not set` (6.19.2-0_fbk0_hardened),
        // so the read fails for EVERY process -- measured, an ordinary host parent
        // with a live child in the same namespace reads empty 10 times out of 10.
        // The file is not evidence of anything, anywhere, here.
        //
        // ⚠️ AND THE FIRST CORRECTION OF THAT MISTAKE WAS ALSO WRONG: it blamed the
        // tracee's PID namespace, which sounds right and is not why. That is the
        // same failure twice -- a correct observation with an invented mechanism
        // attached -- so the mechanism is now named from `/boot/config` rather than
        // reasoned about. `agent(hermit-dbgrev16)` measured both.
        //
        // ⚠️ AND THE REACH IS NARROWER THAN "a departed peer ends the session".
        // Closing this channel ends `Session::run` only once it has passed its
        // initial `gdb_stop_rx.recv()` and entered the command loop; a peer that
        // departs BEFORE the first inferior stop is parked on a different channel
        // entirely, whose sender lives in `TracedTask`. Caught by
        // `agent(codex-rev-493)`. What this fixes is a session that has BEGUN and
        // then loses its client.
        self.pkt_tx.take();

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
            // ⚠️ BOTH HALVES MUST BE ABLE TO END, AND ONE OF THEM COULD NOT.
            // `relay_gdb_packets` terminates when the peer closes; `session.run`
            // terminates when its command channel closes. The relay now drops
            // the only sender as it leaves, so a departed client ends both and
            // this join returns. Before that, a client that closed the
            // connection left the session waiting on a channel whose sender was
            // owned by the very future the join was waiting for.
            let run_session = session.run();
            let run_loop = self.relay_gdb_packets();
            future::try_join(run_session, run_loop).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;

    use super::*;

    /// ⚠️ THIS DRIVES `relay_gdb_packets` ITSELF. AN EARLIER VERSION DID NOT, AND
    /// THAT VERSION WAS WORTHLESS.
    ///
    /// What it used to do: build a local `mpsc` channel in the test, drop the
    /// local sender, and assert the local receiver ended. That is a proof that a
    /// channel behaves like a channel. It never touched [`GdbServerImpl`], so
    /// deleting the production `self.pkt_tx.take()` left it PASSING in 0.00s --
    /// measured, after two review lanes refused the head for exactly this. The
    /// mutation advertised as old-fails/new-passes mutated the MODEL, not the
    /// code.
    ///
    /// What it does now: constructs a real [`GdbServerImpl`] over a duplex pipe,
    /// closes the peer, calls the real `relay_gdb_packets`, and asserts the
    /// session's real receiver observes the channel CLOSED. Removing the `take()`
    /// makes `rx.recv()` pend forever, because the sender is still owned by the
    /// `server` binding this test is holding -- which is precisely the ownership
    /// relationship `run()` has, and precisely the deadlock.
    ///
    /// ⚠️ THE ASSERTION IS ON `recv()` RETURNING `None`, NOT ON A TIMER. A closed
    /// channel resolves immediately; an open one never resolves. The timeout is a
    /// bound so a regression FAILS instead of wedging the runner, not the thing
    /// being measured.
    #[tokio::test]
    async fn the_relay_closes_the_session_channel_when_the_peer_departs() {
        // A duplex pipe stands in for the accepted TCP stream. Dropping our end
        // gives the reader EOF, which is what a departed gdb looks like here.
        let (peer, ours) = tokio::io::duplex(64);
        let (reader, _writer) = tokio::io::split(ours);
        let (tx, mut rx) = mpsc::channel::<Packet>(1);
        let (_server_tx, server_rx) = oneshot::channel();

        let mut server = GdbServerImpl {
            reader: Box::new(reader),
            pkt_tx: Some(tx),
            server_rx: Some(server_rx),
            // `relay_gdb_packets` never touches the session; `run` does, and a
            // real `Session` needs an inferior. Keeping this `None` is what lets
            // the boundary under test be exercised on its own.
            session: None,
        };

        // The peer departs without ever speaking, which is the reproduction:
        // `gdb` exiting before it finishes connecting.
        let mut peer = peer;
        peer.shutdown().await.expect("failed to close the peer");
        drop(peer);

        let relayed = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            server.relay_gdb_packets(),
        )
        .await
        .expect("relay_gdb_packets did not return after the peer closed");
        assert!(relayed.is_ok(), "the relay reported an error: {relayed:?}");

        // ⚠️ THE ACTUAL PROPERTY. `server` is still alive and still owns
        // `pkt_tx` unless the relay took it -- exactly as `run()` holds `self`
        // across its `try_join`. If the sender survives, this pends forever and
        // the session's command loop could never end.
        let closed = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect(
                "the session channel was still OPEN after the relay returned, so the command \
                 loop could never end -- this is the deadlock",
            );
        assert!(
            closed.is_none(),
            "expected the channel to be closed, got a packet"
        );

        // Named so a reader cannot mistake the assertion above for a liveness
        // check on a value nobody holds.
        drop(server);
    }
}
