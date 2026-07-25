/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Blocking Unix-domain-socket transport.
//!
//! [`RpcClient`] is the guest side: one instance per guest thread, typed on the
//! backend's request/response/config so calls need no turbofish. [`RpcServer`]
//! and [`RpcConnection`] are the coordinator side; the coordinator runs one
//! serve loop per accepted connection.

use std::io;
use std::marker::PhantomData;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::codec::read_frame;
use crate::codec::write_frame;
use crate::protocol::ClientFrame;
use crate::protocol::ConnectInfo;
use crate::protocol::ServerFrame;

/// Guest-side client for one thread's connection to the coordinator.
///
/// `Req`/`Resp`/`Cfg` are the backend's `GlobalTool` request, response, and
/// config types.
pub struct RpcClient<Req, Resp, Cfg> {
    stream: UnixStream,
    _marker: PhantomData<fn(Req) -> (Resp, Cfg)>,
}

impl<Req, Resp, Cfg> RpcClient<Req, Resp, Cfg>
where
    Req: Serialize,
    Resp: DeserializeOwned,
    Cfg: DeserializeOwned,
{
    /// Connect to the coordinator's socket at `path`.
    pub fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(path)?,
            _marker: PhantomData,
        })
    }

    /// Wrap an already-connected stream (e.g. one inherited or dup'd by native
    /// client code).
    pub fn from_stream(stream: UnixStream) -> Self {
        Self {
            stream,
            _marker: PhantomData,
        }
    }

    /// Send [`ClientFrame::Connect`] and await the coordinator's reply, which is
    /// expected to be [`ServerFrame::Connected`].
    pub fn handshake(&mut self, info: ConnectInfo) -> io::Result<ServerFrame<Resp, Cfg>> {
        write_frame(&mut self.stream, &ClientFrame::<Req>::Connect(info))?;
        read_frame(&mut self.stream)
    }

    /// Perform one synchronous request/response round-trip. Blocks until the
    /// coordinator replies, which for scheduler requests is when this thread is
    /// selected to run.
    pub fn rpc(&mut self, request: Req) -> io::Result<ServerFrame<Resp, Cfg>> {
        write_frame(&mut self.stream, &ClientFrame::Rpc(request))?;
        read_frame(&mut self.stream)
    }

    /// Tell the coordinator this thread is exiting. No response is expected; the
    /// connection is then dropped.
    pub fn disconnect(&mut self, exit_code: i32) -> io::Result<()> {
        write_frame(&mut self.stream, &ClientFrame::<Req>::Disconnect { exit_code })
    }
}

/// Coordinator-side listener. Each accepted connection belongs to one guest
/// thread.
pub struct RpcServer {
    listener: UnixListener,
}

impl RpcServer {
    /// Bind a listening socket at `path`. The caller is responsible for choosing
    /// a private directory and for unlinking the path on shutdown.
    pub fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            listener: UnixListener::bind(path)?,
        })
    }

    /// Accept the next guest-thread connection.
    pub fn accept<Req, Resp, Cfg>(&self) -> io::Result<RpcConnection<Req, Resp, Cfg>> {
        let (stream, _addr) = self.listener.accept()?;
        Ok(RpcConnection {
            stream,
            _marker: PhantomData,
        })
    }

    /// Borrow the underlying listener (e.g. to set it non-blocking or hand it to
    /// an async reactor).
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

/// One coordinator-side connection to a single guest thread.
pub struct RpcConnection<Req, Resp, Cfg> {
    stream: UnixStream,
    _marker: PhantomData<fn() -> (Req, Resp, Cfg)>,
}

impl<Req, Resp, Cfg> RpcConnection<Req, Resp, Cfg>
where
    Req: DeserializeOwned,
    Resp: Serialize,
    Cfg: Serialize,
{
    /// Read the next client frame. A clean disconnect surfaces as
    /// [`io::ErrorKind::UnexpectedEof`]; the coordinator must then deregister the
    /// thread so the scheduler does not wait on it forever.
    pub fn recv(&mut self) -> io::Result<ClientFrame<Req>> {
        read_frame(&mut self.stream)
    }

    /// Send a frame back to the client.
    pub fn send(&mut self, frame: &ServerFrame<Resp, Cfg>) -> io::Result<()> {
        write_frame(&mut self.stream, frame)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use serde::Deserialize;
    use serde::Serialize;

    use super::*;

    // Mimics the Detcore shapes: request carries a tagged payload, response is a
    // tuple like `(Option<LogicalTime>, GlobalResponse)`, config is a struct.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Req {
        Add(i64, i64),
        Ping,
    }
    type Resp = (Option<u64>, i64);
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Cfg {
        strict: bool,
        epoch: u64,
    }

    #[test]
    fn client_server_round_trip_over_uds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpc.sock");

        let server = RpcServer::bind(&path).unwrap();
        let cfg = Cfg {
            strict: true,
            epoch: 12345,
        };
        let server_cfg = cfg.clone();

        let server_thread = thread::spawn(move || {
            let mut conn = server.accept::<Req, Resp, Cfg>().unwrap();

            // First frame must be Connect; ack with the authoritative config.
            match conn.recv().unwrap() {
                ClientFrame::Connect(info) => {
                    assert_eq!(info.tid, 4242);
                    assert_eq!(info.origin, crate::protocol::Origin::ProcessStart);
                }
                other => panic!("expected Connect, got {other:?}"),
            }
            conn.send(&ServerFrame::Connected {
                config: server_cfg.clone(),
            })
            .unwrap();

            // Serve RPCs until the client disconnects.
            loop {
                match conn.recv() {
                    Ok(ClientFrame::Rpc(Req::Add(a, b))) => {
                        conn.send(&ServerFrame::Rpc((Some(1), a + b))).unwrap();
                    }
                    Ok(ClientFrame::Rpc(Req::Ping)) => {
                        conn.send(&ServerFrame::Rpc((None, 0))).unwrap();
                    }
                    Ok(ClientFrame::Disconnect { exit_code }) => {
                        return exit_code;
                    }
                    Ok(ClientFrame::Connect(_)) => panic!("unexpected second Connect"),
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return -1,
                    Err(e) => panic!("server recv error: {e}"),
                }
            }
        });

        let mut client = RpcClient::<Req, Resp, Cfg>::connect(&path).unwrap();
        let connected = client
            .handshake(ConnectInfo {
                pid: 4242,
                tid: 4242,
                ppid: None,
                origin: crate::protocol::Origin::ProcessStart,
                image_gen: 1,
            })
            .unwrap();
        match connected {
            ServerFrame::Connected { config } => assert_eq!(config, cfg),
            other => panic!("expected Connected, got {other:?}"),
        }

        match client.rpc(Req::Add(2, 40)).unwrap() {
            ServerFrame::Rpc((tick, sum)) => {
                assert_eq!(tick, Some(1));
                assert_eq!(sum, 42);
            }
            other => panic!("expected Rpc, got {other:?}"),
        }
        match client.rpc(Req::Ping).unwrap() {
            ServerFrame::Rpc((tick, v)) => {
                assert_eq!(tick, None);
                assert_eq!(v, 0);
            }
            other => panic!("expected Rpc, got {other:?}"),
        }

        client.disconnect(7).unwrap();
        assert_eq!(server_thread.join().unwrap(), 7);
    }

    #[test]
    fn server_sees_eof_when_client_drops_without_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpc.sock");
        let server = RpcServer::bind(&path).unwrap();

        let server_thread = thread::spawn(move || {
            let mut conn = server.accept::<Req, Resp, Cfg>().unwrap();
            // Connect, then the client vanishes (simulating a guest crash).
            assert!(matches!(conn.recv().unwrap(), ClientFrame::Connect(_)));
            conn.recv().unwrap_err().kind()
        });

        let mut client = RpcClient::<Req, Resp, Cfg>::connect(&path).unwrap();
        client
            .handshake(ConnectInfo {
                pid: 1,
                tid: 1,
                ppid: None,
                origin: crate::protocol::Origin::ProcessStart,
                image_gen: 1,
            })
            .unwrap();
        drop(client);

        assert_eq!(server_thread.join().unwrap(), io::ErrorKind::UnexpectedEof);
    }
}
