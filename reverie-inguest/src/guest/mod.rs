/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The half of the runtime that runs inside the guest process, shared by the
//! in-guest backends: the syscall event every trap and hook path builds and
//! the register context they save, the allocator that keeps Tool callbacks
//! off libc's heap, the synchronous coordinator RPC handle an in-guest Tool
//! sends its global requests through, and the Tool host that runs the Tool.
//! The trap path and launcher move here in the following steps of the
//! in-guest plan (<https://github.com/rrnewton/hermit/issues/3520>, step C3).

pub mod alloc;
pub mod context;
pub mod event;
#[cfg(feature = "coordinator-rpc")]
pub mod host;
#[cfg(feature = "coordinator-rpc")]
pub mod rpc;
