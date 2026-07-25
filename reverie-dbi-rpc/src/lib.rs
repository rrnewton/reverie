/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Cross-process RPC transport for the DynamoRIO (DBI) backend.
//!
//! # Why this crate exists
//!
//! In the ptrace backend the whole Detcore tool — including the single
//! [`GlobalState`] scheduler — runs inside one out-of-guest supervisor process
//! that traces the entire process tree, so its guest→global "RPC" can be a plain
//! in-process method call. The DBI backend instead injects the tool *into each
//! guest process*, so a per-process `GlobalState` fragments across every
//! `fork()`: the scheduler, virtual clock, and counters stop being shared.
//!
//! This crate provides the wire transport that lets the DBI backend follow the
//! ptrace model instead: the `hermit` CLI process acts as the coordinator that
//! owns the one `GlobalState`, and every guest thread — in every process of the
//! tree, across `fork`/`exec` — sends its `GlobalTool` requests over a Unix
//! domain socket to that coordinator and blocks for the response.
//!
//! [`GlobalState`]: (in the `detcore` crate)
//!
//! # Shape
//!
//! * [`codec`]: length-prefixed `bincode` framing. The `u32` big-endian payload
//!   length precedes a `bincode` payload encoded with [`bincode::config::legacy`],
//!   the same configuration `reverie-ptrace` uses to round-trip these exact
//!   request/response types, so the DBI wire format matches byte-for-byte.
//! * [`protocol`]: the [`ClientFrame`]/[`ServerFrame`] envelopes, generic over the
//!   backend's request, response, and config types so the crate stays free of a
//!   `reverie`/`detcore` dependency.
//! * [`transport`]: a blocking [`RpcClient`] (one connection per guest thread; the
//!   blocking read *is* how a non-scheduled thread parks) and a blocking
//!   [`RpcServer`]/[`RpcConnection`] pair for the coordinator accept loop.
//!
//! The transport is intentionally synchronous and `std`-only. A guest thread has
//! at most one outstanding request at a time, so no request-id correlation is
//! needed. The coordinator side is `std`-blocking here; wiring it to the
//! coordinator's async `GlobalState` scheduler is done in `hermit-cli` (a later
//! phase), which can run one accept/serve thread per connection.

#![deny(missing_docs)]

pub mod codec;
pub mod protocol;
pub mod transport;

pub use protocol::ClientFrame;
pub use protocol::ConnectInfo;
pub use protocol::Origin;
pub use protocol::ServerFrame;
pub use transport::RpcClient;
pub use transport::RpcConnection;
pub use transport::RpcServer;
