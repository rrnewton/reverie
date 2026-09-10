/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Retained LiteInst selection surface for the Reverie example tools.
//!
//! The generic `Backend` launch refuses until it can express the required
//! caller-owned `PreparedCommand` lifecycle.

#![forbid(unsafe_op_in_unsafe_fn)]
// The reused production tool sources each declare the same test-only KVM helper.
#![allow(clippy::duplicate_mod)]

use std::io;

use reverie::Backend;
use reverie::process::Command;
use reverie_liteinst::LiteinstBackend;

#[allow(dead_code)]
#[path = "../chaos.rs"]
mod chaos;

// TODO-HUMAN-REVIEW(PR-157): Review the narrow chaos config re-export.
pub(crate) use chaos::ChaosOpts;

#[allow(dead_code)]
#[path = "../chrome-trace/main.rs"]
mod chrome_trace;

#[allow(dead_code)]
#[path = "../chunky_print.rs"]
mod chunky_print;
#[allow(dead_code)]
#[path = "../counter1_tool.rs"]
mod counter1;

#[allow(dead_code)]
#[path = "../counter2_tool.rs"]
mod counter2;
#[allow(dead_code)]
#[path = "../debug.rs"]
mod debug;
#[allow(dead_code)]
#[path = "../noop.rs"]
mod noop;
#[allow(dead_code)]
#[path = "../strace/main.rs"]
pub(crate) mod strace;
#[allow(dead_code)]
#[path = "../strace_minimal.rs"]
mod strace_minimal;

pub(crate) use strace::config;
pub(crate) use strace::filter;
pub(crate) use strace::global_state;

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-139): Review the LiteInst example-tool selector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
/// Example tool selection retained while generic LiteInst launch is unsupported.
pub(crate) enum ToolKind {
    /// Introduce short reads and optional interrupted reads.
    // TODO-HUMAN-REVIEW(PR-157): Review the chaos LiteInst selector extension.
    Chaos,
    /// Capture process lifecycle and syscall events as a Chrome trace.
    // TODO-HUMAN-REVIEW(PR-159): Review the ChromeTrace LiteInst selector extension.
    ChromeTrace,
    /// Count every intercepted syscall through the shared global state.
    Counter1,
    /// Aggregate per-thread and per-process syscall counts in global state.
    // TODO-HUMAN-REVIEW(PR-146): Review the counter2 LiteInst selector extension.
    Counter2,
    /// Buffer standard output and error writes by logical epochs.
    // TODO-HUMAN-REVIEW(PR-152): Review the chunky_print LiteInst selector extension.
    ChunkyPrint,
    /// Select the debug example; generic LiteInst launch refuses this selection.
    Debug,
    /// Decode and print subscribed syscalls.
    Strace,
    /// Print every syscall before injecting it.
    // TODO-HUMAN-REVIEW(PR-193): Review the minimal-strace LiteInst selector.
    StraceMinimal,
    /// Preserve guest behavior without subscribing to events.
    Noop,
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-139): Review the LiteInst example-tool launch boundary.
/// Validates the selection and calls the generic LiteInst backend, which refuses
/// before starting a guest.
///
/// `filters` accepts strace syscall filters and must be empty for other tools.
// TODO-HUMAN-REVIEW(PR-157): Review the chaos config extension to the host API.
pub(crate) async fn run(
    kind: ToolKind,
    command: Command,
    filters: Vec<String>,
    chaos_options: ChaosOpts,
) -> Result<(), reverie::Error> {
    if kind != ToolKind::Chaos && chaos_options != ChaosOpts::default() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "chaos options require the chaos tool",
        )
        .into());
    }
    if kind != ToolKind::Strace && !filters.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "strace filters require the strace tool",
        )
        .into());
    }
    let _ = kind;
    let _ = <LiteinstBackend as Backend>::run_with_output::<noop::NoopTool>(command, ()).await?;
    unreachable!("LiteInst Backend execution always refuses before launch")
}
