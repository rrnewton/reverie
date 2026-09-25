/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! This crate wraps raw `u64` syscall arguments in stronger Rust types. This
//! has a number of useful side effects:
//! 1. Syscalls and their arguments can be easily displayed for debugging
//!    purposes.
//! 2. When intercepting syscalls, the Rust type can be accessed more safely.
//! 3. When injecting syscalls, it is easier and clearer to set the arguments
//!    using the `with_*` builder methods.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
// Without `std` the crate builds with `core` and `alloc` only, for the Narf
// kernel target. The libc and nix items it names then come from
// `libc_shim.rs` and `nix_shim.rs`, which are checked against the real crates
// by the host tests.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg(any(target_os = "linux", not(feature = "std")))]

// The `std`-free libc and nix stand-ins are x86_64 Linux's.
#[cfg(all(not(feature = "std"), not(target_arch = "x86_64")))]
compile_error!("reverie-syscalls without `std` is only defined for x86_64");

extern crate alloc;

#[macro_use]
mod macros;

mod args;
mod display;
mod raw;
mod syscalls;

/// The `libc` items the syscall types are built from.
///
/// With `std` this is the `libc` crate. Without `std` (where `libc` is empty)
/// it is a copy of the x86_64 Linux definitions of just those items.
#[cfg(feature = "std")]
pub use ::libc;
#[cfg(not(feature = "std"))]
#[path = "libc_shim.rs"]
pub mod libc;
// Compiled into the host tests too, which compare it with `libc`. There it
// is private, so items only the no-std build uses look unused.
#[cfg(all(test, feature = "std"))]
#[allow(dead_code, unused_imports)]
mod libc_shim;

// The `nix` flags types and `Pid` the syscall types use, under nix's paths.
#[cfg(feature = "std")]
use ::nix;
#[cfg(not(feature = "std"))]
#[path = "nix_shim.rs"]
mod nix;
// Compiled into the host tests too, which compare it with `nix`.
#[cfg(all(test, feature = "std"))]
#[allow(dead_code)]
mod nix_shim;

// Re-export the only things that might be needed from the syscalls crate
pub use ::reverie_memory::*;
pub use ::syscalls::Errno;
pub use ::syscalls::SyscallArgs;
pub use ::syscalls::Sysno;

pub use crate::args::*;
pub use crate::display::*;
pub use crate::raw::*;
pub use crate::syscalls::*;
