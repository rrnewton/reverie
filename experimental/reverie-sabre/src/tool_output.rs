/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! A descriptor the coordinator hands the in-guest tool for its own output,
//! such as Hermit's ordered DETLOG record socket.
//!
//! The coordinator passes the descriptor number in [`TOOL_OUTPUT_ENV`]. The
//! plugin takes the variable out of the guest's environment (it is restored
//! for a loader-mediated execve with the other private settings) and protects
//! the descriptor from the guest in this process and in every process forked
//! from it: the guest cannot close, duplicate onto, or use it.

use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::sync::OnceLock;

use crate::paths;
use crate::protected_files;

/// The private variable holding the tool-output descriptor number.
pub const TOOL_OUTPUT_ENV: &str = "REVERIE_SABRE_TOOL_OUTPUT_FD";

static TOOL_OUTPUT: OnceLock<Option<RawFd>> = OnceLock::new();

pub(crate) fn init_tool_output() {
    TOOL_OUTPUT.get_or_init(|| {
        std::env::var_os(TOOL_OUTPUT_ENV)?;
        paths::cache_tool_env();
        // SAFETY: plugin initialization runs before guest threads exist.
        let value = unsafe { paths::take_private_env(TOOL_OUTPUT_ENV) }?;
        let fd = std::str::from_utf8(value.as_os_str().as_bytes())
            .ok()?
            .parse::<RawFd>()
            .ok()?;
        // SAFETY: F_GETFD only reads the descriptor's flags.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return None;
        }
        protected_files::protect_inherited_raw_fd(fd);
        Some(fd)
    });
}

/// The tool-output descriptor the coordinator passed, protected from the
/// guest, if it passed one.
pub fn tool_output_fd() -> Option<RawFd> {
    TOOL_OUTPUT.get().copied().flatten()
}
