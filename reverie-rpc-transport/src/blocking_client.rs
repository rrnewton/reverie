/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Blocking guest client for synchronous in-process instrumentation callbacks.

use std::io;
use std::io::Read;
use std::io::Write;
use std::marker::PhantomData;
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Tid;

use crate::codec::DEFAULT_MAX_FRAME_LEN;
use crate::codec::decode;
use crate::codec::encode;
use crate::envelope::RequestEnvelope;
use crate::error::RpcError;

/// A per-thread blocking connection to a coordinator serving `G`.
///
/// Some in-guest instrumentation runtimes, including SaBRe, invoke the tool
/// through a synchronous callback and poll its async handler exactly once.
/// Tokio socket operations normally return `Pending` on that first poll. This
/// client deliberately performs the request/response exchange synchronously
/// inside `GlobalRPC::send_rpc`, so the enclosing tool future remains
/// immediately ready after the coordinator responds.
///
/// Keep one client per guest thread. A Detcore scheduler response can be
/// delayed until another thread releases resources, so sharing one connection
/// across threads could otherwise deadlock behind the single in-flight request.
// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-128): Review the blocking transport used by synchronous in-guest backends.
pub struct BlockingRpcClient<G: GlobalTool, S = UnixStream> {
    tid: Tid,
    config: G::Config,
    stream: Mutex<S>,
    _phantom: PhantomData<fn() -> G>,
}

// TODO-HUMAN-REVIEW(PR-212): Review raw descriptor exposure for in-guest
// runtimes that must hide coordinator transport descriptors from the guest.
impl<G: GlobalTool, S: AsRawFd> AsRawFd for BlockingRpcClient<G, S> {
    fn as_raw_fd(&self) -> RawFd {
        self.stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_raw_fd()
    }
}

impl<G> BlockingRpcClient<G>
where
    G: GlobalTool,
{
    /// Connect and synchronously receive the coordinator's config handshake.
    pub fn connect(path: impl AsRef<Path>, tid: Tid) -> Result<Self, RpcError> {
        Self::from_connected_stream(UnixStream::connect(path)?, tid)
    }
}

impl<G, S> BlockingRpcClient<G, S>
where
    G: GlobalTool,
    S: Read + Write + Send,
{
    /// Own a connected stream and synchronously receive the config handshake.
    pub fn from_connected_stream(mut stream: S, tid: Tid) -> Result<Self, RpcError> {
        let config_bytes = read_message(&mut stream, DEFAULT_MAX_FRAME_LEN)?;
        let config = decode(&config_bytes)?;
        Ok(Self {
            tid,
            config,
            stream: Mutex::new(stream),
            _phantom: PhantomData,
        })
    }

    /// The tid attached to requests on this connection.
    pub fn tid(&self) -> Tid {
        self.tid
    }

    /// Send one request and block until the coordinator returns its response.
    pub fn try_send_rpc(&self, message: G::Request) -> Result<G::Response, RpcError> {
        let request_bytes = encode(&RequestEnvelope {
            from: self.tid,
            request: message,
        })?;
        let mut stream = self.stream.lock().map_err(|_| {
            RpcError::Io(io::Error::other(
                "reverie-rpc-transport: blocking client mutex poisoned",
            ))
        })?;
        write_message(&mut *stream, &request_bytes)?;
        let response_bytes = read_message(&mut *stream, DEFAULT_MAX_FRAME_LEN)?;
        decode(&response_bytes)
    }
}

#[async_trait]
impl<G, S> GlobalRPC<G> for BlockingRpcClient<G, S>
where
    G: GlobalTool,
    S: Read + Write + Send,
{
    async fn send_rpc(&self, message: G::Request) -> G::Response {
        self.try_send_rpc(message)
            .expect("reverie-rpc-transport: blocking RPC to coordinator failed")
    }

    fn config(&self) -> &G::Config {
        &self.config
    }
}

fn write_message(stream: &mut impl Write, payload: &[u8]) -> Result<(), RpcError> {
    let len = u32::try_from(payload.len()).map_err(|_| RpcError::FrameTooLarge {
        len: payload.len(),
        max: u32::MAX as usize,
    })?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

fn read_message(stream: &mut impl Read, max_len: usize) -> Result<Vec<u8>, RpcError> {
    // Read the 4-byte length prefix in one `read` on the common path. The loop
    // only re-enters the kernel on a short read or `EINTR`, collapsing the
    // former 1-byte probe + 3-byte remainder into a single syscall per hop.
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match stream.read(&mut header[filled..]) {
            // A clean EOF exactly at a frame boundary is a graceful close;
            // preserve the previous 1-byte-probe semantics.
            Ok(0) if filled == 0 => return Err(RpcError::Closed),
            // EOF partway through the header is a truncated frame.
            Ok(0) => return Err(RpcError::Io(io::Error::from(io::ErrorKind::UnexpectedEof))),
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(RpcError::Io(error)),
        }
    }

    let len = u32::from_be_bytes(header) as usize;
    if len > max_len {
        return Err(RpcError::FrameTooLarge { len, max: max_len });
    }

    let mut payload = vec![0; len];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::future::Future;
    use std::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::task::Context;
    use std::task::Poll;
    use std::task::Waker;
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct Counter;

    #[async_trait]
    impl GlobalTool for Counter {
        type Request = u64;
        type Response = u64;
        type Config = String;

        async fn receive_rpc(&self, _from: Tid, request: u64) -> u64 {
            request + 1
        }
    }

    struct OwnedStream {
        stream: UnixStream,
        drops: Arc<AtomicUsize>,
        interrupt: Cell<bool>,
        panic_on_read: bool,
        reading: Option<mpsc::Sender<()>>,
    }

    impl OwnedStream {
        fn new(stream: UnixStream, drops: Arc<AtomicUsize>) -> Self {
            Self {
                stream,
                drops,
                interrupt: Cell::new(true),
                panic_on_read: false,
                reading: None,
            }
        }
    }

    impl Read for OwnedStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            assert!(!self.panic_on_read, "controlled owning-stream unwind");
            if self.interrupt.replace(false) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if let Some(reading) = self.reading.take() {
                reading.send(()).unwrap();
            }
            let length = buffer.len().min(1);
            self.stream.read(&mut buffer[..length])
        }
    }

    impl Write for OwnedStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.stream.write(&buffer[..buffer.len().min(1)])
        }

        fn flush(&mut self) -> io::Result<()> {
            self.stream.flush()
        }
    }

    impl AsRawFd for OwnedStream {
        fn as_raw_fd(&self) -> RawFd {
            self.stream.as_raw_fd()
        }
    }

    impl Drop for OwnedStream {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn send_config(peer: &mut UnixStream) {
        write_message(peer, &encode(&"owned-config".to_owned()).unwrap()).unwrap();
    }

    #[test]
    fn owning_stream_preserves_config_tid_framing_and_first_poll() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let descriptor = stream.as_raw_fd();
        let drops = Arc::new(AtomicUsize::new(0));
        let server = std::thread::spawn(move || {
            send_config(&mut peer);
            let bytes = read_message(&mut peer, DEFAULT_MAX_FRAME_LEN).unwrap();
            let request: RequestEnvelope<u64> = decode(&bytes).unwrap();
            assert_eq!(request.from, Tid::from_raw(71));
            assert_eq!(request.request, 8);
            write_message(&mut peer, &encode(&9u64).unwrap()).unwrap();
            assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
        });
        let client = BlockingRpcClient::<Counter, _>::from_connected_stream(
            OwnedStream::new(stream, drops.clone()),
            Tid::from_raw(71),
        )
        .unwrap();
        assert_eq!(client.config(), "owned-config");
        assert_eq!(client.tid(), Tid::from_raw(71));
        assert_eq!(client.as_raw_fd(), descriptor);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        {
            let mut future = pin!(client.send_rpc(8));
            let mut context = Context::from_waker(Waker::noop());
            assert_eq!(future.as_mut().poll(&mut context), Poll::Ready(9));
        }
        drop(client);
        server.join().unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn owning_stream_handshake_errors_retain_classification_and_close_once() {
        let cases = [
            (Vec::new(), "closed"),
            (vec![0, 0], "truncated"),
            (vec![0, 0, 0, 2, 1], "truncated"),
            (u32::MAX.to_be_bytes().to_vec(), "oversized"),
            (vec![0, 0, 0, 0], "decode"),
        ];
        for (bytes, expected) in cases {
            let (stream, mut peer) = UnixStream::pair().unwrap();
            let drops = Arc::new(AtomicUsize::new(0));
            peer.write_all(&bytes).unwrap();
            drop(peer);
            let error = BlockingRpcClient::<Counter, _>::from_connected_stream(
                OwnedStream::new(stream, drops.clone()),
                Tid::from_raw(72),
            )
            .err()
            .expect("invalid handshake must fail");
            match expected {
                "closed" => assert!(matches!(error, RpcError::Closed)),
                "truncated" => assert!(matches!(error, RpcError::Io(ref error)
                    if error.kind() == io::ErrorKind::UnexpectedEof)),
                "oversized" => assert!(matches!(error, RpcError::FrameTooLarge { len, max }
                    if len == u32::MAX as usize && max == DEFAULT_MAX_FRAME_LEN)),
                "decode" => assert!(matches!(error, RpcError::Decode(_))),
                _ => unreachable!(),
            }
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn owning_stream_handshake_unwind_closes_once() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let mut stream = OwnedStream::new(stream, drops.clone());
        stream.panic_on_read = true;
        assert!(
            catch_unwind(AssertUnwindSafe(move || {
                let _ = BlockingRpcClient::<Counter, _>::from_connected_stream(
                    stream,
                    Tid::from_raw(73),
                );
            }))
            .is_err()
        );
        assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn owning_stream_client_unwind_closes_once() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        send_config(&mut peer);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                let _client = BlockingRpcClient::<Counter, _>::from_connected_stream(
                    OwnedStream::new(stream, drops.clone()),
                    Tid::from_raw(74),
                )
                .unwrap();
                panic!("controlled client unwind");
            }))
            .is_err()
        );
        assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn owning_stream_delayed_config_retains_owner_without_blocking_other_client() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let (reading_tx, reading_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let mut stream = OwnedStream::new(stream, drops.clone());
        stream.reading = Some(reading_tx);
        let waiting = std::thread::spawn(move || {
            let client =
                BlockingRpcClient::<Counter, _>::from_connected_stream(stream, Tid::from_raw(75))
                    .unwrap();
            assert_eq!(client.config(), "owned-config");
            drop(client);
            finished_tx.send(()).unwrap();
        });
        reading_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(matches!(
            finished_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let (other, mut other_peer) = UnixStream::pair().unwrap();
        send_config(&mut other_peer);
        let other =
            BlockingRpcClient::<Counter>::from_connected_stream(other, Tid::from_raw(76)).unwrap();
        assert_eq!(other.config(), "owned-config");
        drop(other);
        send_config(&mut peer);
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        waiting.join().unwrap();
        assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn owning_stream_rpc_unwind_preserves_poison_error() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        send_config(&mut peer);
        let client = BlockingRpcClient::<Counter, _>::from_connected_stream(
            OwnedStream::new(stream, drops.clone()),
            Tid::from_raw(77),
        )
        .unwrap();
        client.stream.lock().unwrap().panic_on_read = true;
        assert!(catch_unwind(AssertUnwindSafe(|| client.try_send_rpc(8))).is_err());
        let request = read_message(&mut peer, DEFAULT_MAX_FRAME_LEN).unwrap();
        assert_eq!(decode::<RequestEnvelope<u64>>(&request).unwrap().request, 8);
        assert!(matches!(client.try_send_rpc(9), Err(RpcError::Io(error))
            if error.to_string() == "reverie-rpc-transport: blocking client mutex poisoned"));
        assert!(client.as_raw_fd() >= 0);
        drop(client);
        assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
