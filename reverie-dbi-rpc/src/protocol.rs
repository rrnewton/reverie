/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Wire envelopes exchanged between guest clients and the coordinator.
//!
//! The envelopes are generic over the backend's request (`Req`), response
//! (`Resp`), and config (`Cfg`) types so this crate does not depend on `reverie`
//! or `detcore`. For the Detcore backend these instantiate to the
//! `GlobalTool::Request`, `GlobalTool::Response`, and `GlobalTool::Config`
//! associated types, all of which already implement `Serialize`/`Deserialize`.
//!
//! Thread and process ids travel as raw `i32` kernel ids; the reverie-dbi side
//! converts to and from `reverie::Pid`/`Tid`.

use serde::Deserialize;
use serde::Serialize;

/// How a newly connected client thread came to exist, so the coordinator can
/// register it into the single scheduler with the right lifecycle transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// The root guest process's main thread (first process in the tree).
    ProcessStart,
    /// A new thread (`clone` with `CLONE_THREAD`) in an existing guest process.
    Thread,
    /// A child process from `fork`/`clone` without `CLONE_THREAD`.
    ForkChild,
    /// A child process from `vfork`.
    VforkChild,
    /// The same process re-establishing its connection after a successful
    /// `execve` (its pid is preserved).
    PostExec,
}

/// The first frame a client sends on a fresh connection, identifying the guest
/// thread that owns it. The coordinator stores this per connection, so
/// subsequent [`ClientFrame::Rpc`] frames do not repeat the thread id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectInfo {
    /// Kernel pid (thread-group id) of the guest process.
    pub pid: i32,
    /// Kernel tid of the guest thread owning this connection.
    pub tid: i32,
    /// In-tree parent pid, when known (`None` for the root process).
    pub ppid: Option<i32>,
    /// How this thread/process came to exist.
    pub origin: Origin,
    /// DynamoRIO application image generation, used to detect stale clients
    /// across `execve` image restarts.
    pub image_gen: u64,
}

/// Client → coordinator frames.
///
/// One connection is used per guest thread, and a thread has at most one
/// outstanding [`ClientFrame::Rpc`] at a time: it blocks on the matching
/// [`ServerFrame::Rpc`] before continuing. That blocking read is exactly how a
/// thread that the scheduler has not yet selected stays parked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Req: Serialize",
    deserialize = "Req: serde::de::DeserializeOwned"
))]
pub enum ClientFrame<Req> {
    /// Associate this connection with a guest thread. Must be the first frame.
    Connect(ConnectInfo),
    /// One `GlobalTool` request from this connection's thread.
    Rpc(Req),
    /// The thread (or process) is exiting; carries its exit code so the
    /// coordinator can run thread-exit bookkeeping and deregister it.
    Disconnect {
        /// Guest thread/process exit code.
        exit_code: i32,
    },
}

/// Coordinator → client frames.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "Resp: Serialize, Cfg: Serialize",
    deserialize = "Resp: serde::de::DeserializeOwned, Cfg: serde::de::DeserializeOwned"
))]
pub enum ServerFrame<Resp, Cfg> {
    /// Acknowledges [`ClientFrame::Connect`] and ships the authoritative config
    /// so every guest process runs its local tool with byte-identical settings.
    Connected {
        /// Backend configuration to cache in the client.
        config: Cfg,
    },
    /// Response to the immediately preceding [`ClientFrame::Rpc`].
    Rpc(Resp),
    /// The coordinator is shutting down; the client should stop its loop.
    Shutdown,
}
