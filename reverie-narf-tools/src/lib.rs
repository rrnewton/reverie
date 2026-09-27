/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Backend-neutral `reverie::Tool`s shared by the Narf kernel backend and the
//! Linux hosts it is compared against.
//!
//! Every Tool here names only `core` and `alloc`, so the same source builds
//! into the Narf kernel (through `reverie-narf-core`) and into a `std` host
//! (through `reverie-ptrace`). A Tool that has to print takes a [`LineSink`]
//! type parameter: the Tool's code, and therefore the bytes it formats, is the
//! same in every host; only where the finished line goes differs.
//!
//! A Tool compiled here unmodified from `reverie-examples` names no
//! `LineSink`: it prints with `eprintln!`. For those Tools `eprintln!` is this
//! crate's macro, which hands each line to the one function the backend sets
//! with [`set_eprintln_sink`].

#![no_std]

extern crate alloc;
// strace's test-only counter, and filter's tests, need `std` and its `vec!`.
#[cfg(test)]
#[macro_use]
extern crate std;

use core::sync::atomic::AtomicPtr;
use core::sync::atomic::Ordering;

pub mod canonical;
pub mod passthrough;
pub mod probe;

/// counter1, compiled unmodified from `reverie-examples`.
#[allow(dead_code)]
#[path = "../../reverie-examples/counter1_tool.rs"]
pub mod counter1;

/// counter2, compiled unmodified from `reverie-examples`. Without `std` it
/// prints its thread-exit line only through a reporter the backend sets.
#[allow(dead_code)]
#[path = "../../reverie-examples/counter2_tool.rs"]
pub mod counter2;

/// Formats one `eprintln!` line of a Tool compiled from `reverie-examples`
/// and delivers it to the sink set with [`set_eprintln_sink`].
///
/// Textually scoped, so it reaches only the modules declared after it.
macro_rules! eprintln {
    ($($arg:tt)*) => {
        $crate::eprintln_line(::core::format_args!($($arg)*))
    };
}

/// strace, compiled from the source files `reverie-examples` builds its `std`
/// strace binary from: it prints every syscall, signal and exit with
/// `eprintln!`, here the macro above.
#[allow(dead_code)]
#[path = "../../reverie-examples/strace"]
pub mod strace {
    pub mod config;
    pub mod filter;
    pub mod global_state;
    pub mod tool;

    pub use config::Config;
    pub use filter::Filter;
    pub use tool::Strace;
}

// strace's modules name each other from the crate root, where
// reverie-examples also re-exports them.
pub(crate) use strace::config;
pub(crate) use strace::filter;
pub(crate) use strace::global_state;

/// Where a printing Tool delivers each finished line.
///
/// `emit` receives one complete line without its terminating newline. The
/// sink appends the newline and must deliver the line atomically with respect
/// to other lines, so concurrent threads cannot interleave within a line.
pub trait LineSink: 'static {
    /// Delivers one line.
    fn emit(line: &str);
}

/// The `fn(&str)` set by [`set_eprintln_sink`], or null.
static EPRINTLN_SINK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Sets where `eprintln!` in the Tools compiled here from `reverie-examples`
/// delivers each line, for every Tool and thread from now on. `sink` has the
/// [`LineSink::emit`] contract.
pub fn set_eprintln_sink(sink: fn(&str)) {
    EPRINTLN_SINK.store(sink as *mut (), Ordering::Release);
}

/// The body of this crate's `eprintln!`.
///
/// # Panics
///
/// If no sink is set: a Tool's line is its output, and dropping it would
/// make a run look complete when it is not.
pub(crate) fn eprintln_line(args: core::fmt::Arguments<'_>) {
    let sink = EPRINTLN_SINK.load(Ordering::Acquire);
    assert!(
        !sink.is_null(),
        "a Tool printed with eprintln! before set_eprintln_sink"
    );
    // SAFETY: the only non-null value `EPRINTLN_SINK` ever holds is a
    // `fn(&str)`, stored by `set_eprintln_sink`.
    let sink = unsafe { core::mem::transmute::<*mut (), fn(&str)>(sink) };
    sink(&alloc::fmt::format(args));
}
