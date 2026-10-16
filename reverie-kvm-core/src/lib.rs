/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The KVM guest-memory core of Reverie's KVM backend: the memfd-backed guest
//! arena ([`GuestMemory`], [`memory::UserMemory`]) with its user-memory
//! translation and shared-file views, the entry gate that admits every guest
//! memory operation and carries entry failures ([`entry`]), the run-failure
//! machinery that owns those failures ([`failure`]), the backend's error type
//! ([`Error`]), the fixed guest address map ([`layout`]), and the arena's
//! implementations of the provider memory traits ([`provider_memory`]).
//!
//! Step U6b of the provider seam
//! (<https://github.com/rrnewton/hermit/issues/4485>, epic
//! <https://github.com/rrnewton/hermit/issues/4492>) moved these out of
//! reverie-kvm unchanged so that the Linux syscall emulator can live in its
//! own crate above them without a dependency cycle. It is an internal
//! building block of reverie-kvm, which re-exports what its users need; it is
//! not part of Reverie's `Tool`, `Guest` or backend contract.

#![cfg(target_arch = "x86_64")]

pub mod entry;
pub mod error;
pub mod failure;
pub mod layout;
pub mod memory;
pub mod provider_memory;

/// Shared alias-failure scenarios for reverie-kvm's tests (feature
/// `test-support`).
#[cfg(any(test, feature = "test-support"))]
pub mod alias_failure;

pub use error::Error;
pub use memory::GuestMemory;

/// Result type used by the KVM backend.
pub type Result<T> = std::result::Result<T, Error>;
