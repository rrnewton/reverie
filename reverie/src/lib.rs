/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Reverie is a user space system-call interception framework for Linux. It can
//! be used to intercept, modify, or elide a syscall before the kernel executes
//! it.
//!
//! Reverie consists of a family of crates:
//!  - `reverie` (this one): Primarily provides the [`Tool`] trait interface
//!    that Reverie tools must implement to intercept syscalls. It also defines
//!    the [`Backend`] trait, which is the contract a *backend* implementation
//!    must satisfy in order to run an arbitrary tool.
//!  - `reverie-ptrace`: The backend that uses ptrace to intercept syscalls.
//!    This is currently the only non-experimental backend and is the reference
//!    implementation of the [`Backend`] contract. In the future, we may have a
//!    backend that uses binary rewriting to intercept syscalls within the guest
//!    process.
//!  - `reverie-syscalls`: Provides typed syscalls, which provide safer and more
//!    ergonomic access to the arguments of a syscall. Also provides pretty
//!    printing of syscalls and their arguments.
//!
//! The rest of the `reverie-*` crates are used in service to the above crates.
//!
//! # Tools and backends
//!
//! There are two sides to every Reverie program:
//!  - A [`Tool`] decides *what* to do when the guest hits a trappable event.
//!    This is what most users write; see the [`Tool`] trait for the full
//!    handler API.
//!  - A [`Backend`] decides *how* those events are trapped and how the tool is
//!    run against a live guest process tree (spawning, syscall interception,
//!    hosting global state, teardown). `reverie-ptrace` is the reference
//!    backend; the [`Backend`] trait spells out exactly what any alternative
//!    backend must provide.
//!
//! For examples of usage, please see the [`reverie-examples`][] folder.
//!
//! See also [`README.md`][] for a high-level overview of Reverie.
//!
//! [`reverie-examples`]: https://github.com/facebookexperimental/reverie/tree/main/reverie-examples
//! [`README.md`]: https://github.com/facebookexperimental/reverie

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
// Without `std` the crate builds with `core` and `alloc` only, for the Narf
// kernel target: it still defines the whole `Tool`, `GlobalTool` and `Guest`
// contract, but not the backends, backtrace symbolization or anything else
// that reads host files or starts processes.
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg(any(target_os = "linux", not(feature = "std")))]

// The `std`-free libc stand-in reverie-syscalls provides is x86_64 Linux's.
#[cfg(all(not(feature = "std"), not(target_arch = "x86_64")))]
compile_error!("reverie without `std` is only defined for x86_64");

extern crate alloc;

// The libc items the contract names (`user_regs_struct`, `AT_*`, signal
// numbers). With `std` this is the `libc` crate; without it, the checked
// x86_64 Linux copy reverie-syscalls provides at the same path.
#[cfg(feature = "std")]
use ::libc;
#[cfg(not(feature = "std"))]
use reverie_syscalls::libc;

mod auxv;
#[cfg(feature = "std")]
mod backend;
pub mod backend_stats;
mod backtrace;
mod error;
mod guest;
#[cfg(target_arch = "x86_64")]
pub mod pmu;
mod process_signal_control;
#[cfg(target_arch = "x86_64")]
mod rdtsc;
mod regs;
mod signal;
mod signal_observation;
mod stack;
mod subscription;
mod timer;
mod tool;

pub use auxv::*;
#[cfg(feature = "std")]
pub use backend::*;
pub use backend_stats::*;
pub use backtrace::*;
pub use error::*;
pub use guest::*;
pub use process::ExitStatus;
pub use process::Pid;
#[cfg(target_arch = "x86_64")]
pub use rdtsc::*;
pub use regs::RegDisplay;
pub use regs::RegDisplayOptions;
pub use reverie_process as process;
pub use signal::*;
pub use signal_observation::*;
pub use stack::*;
pub use subscription::*;
pub use timer::*;
pub use tool::*;

/// The identifier for a specific thread, corresponding to the output of gettid.
/// In many cases, Linux blurs the Pid/Tid distinction, but Reverie should
/// consistently use TIDs when referring to threads, and Pids when referring to
/// shared address spaces that (typically) correspond to processes.
///
/// This type is currently equivalent to [`Pid`], but relying on that equivalence
/// is deprecated. `Tid` may be a distinct newtype in the future.
pub type Tid = Pid;

/// Required for `impl Tool for MyTool` blocks.
///
/// NOTE: This is just an alias for `async_trait` for now, but may be extended in
/// the future to do more things (like derive syscall subscriptions).
pub use async_trait::async_trait as tool;
/// Required for `impl GlobalTool for MyGlobalTool` blocks.
///
/// NOTE: This is just an alias for `async_trait` for now, but may be extended in
/// the future to do more things (like deriving Request/Response types from
/// method names).
pub use async_trait::async_trait as global_tool;
/// Required for `impl Backend for MyBackend` blocks.
///
/// NOTE: This is just an alias for `async_trait` for now, but may be extended in
/// the future.
#[cfg(feature = "std")]
pub use async_trait::async_trait as backend;
/// CPUID result.
pub use raw_cpuid::CpuIdResult;
// The signal type: nix's `Signal` with `std`, and reverie-process's
// same-shaped stand-in without it.
pub use reverie_process::Signal;
/// typed syscalls.
pub use reverie_syscalls as syscalls;

/// `Never` type is a stopgap for the unstable `!` type (i.e., the never type).
pub type Never = never_say_never::Never;

// Run-owned process signal control; no borrowed Guest is retained.
pub use process_signal_control::*;
