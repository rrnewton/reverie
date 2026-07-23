/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Milestone 1: run a program under the DynamoRIO backend with a strace-style
//! Reverie tool.
//!
//! This proves the [`reverie::Tool`] / [`reverie::Guest`] interface works end
//! to end through DynamoRIO: [`DbiRunner`] launches the guest under the native
//! client, which drives `StraceTool::handle_syscall_event` over `DbiGuest` for
//! each intercepted syscall. Strace lines are printed to stderr; the guest's
//! own output is unchanged.
//!
//! Usage:
//!
//! ```text
//! cargo run --example dbi_strace -- echo hello
//! cargo run --example dbi_strace            # defaults to `/bin/echo hello`
//! ```

use std::env;
use std::ffi::OsString;
use std::process::Command;
use std::process::ExitCode;

use reverie_dbi::DbiRunner;

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let (program, guest_args): (OsString, Vec<OsString>) = match args.next() {
        Some(program) => (program, args.collect()),
        None => (OsString::from("/bin/echo"), vec![OsString::from("hello")]),
    };

    let runner = match DbiRunner::from_env().or_else(|_| DbiRunner::from_build()) {
        Ok(runner) => runner,
        Err(error) => {
            eprintln!(
                "dbi_strace: could not locate DynamoRIO or the native client: {error}\n\
                 Build the client first: reverie-dbi/scripts/build-client.sh"
            );
            return ExitCode::FAILURE;
        }
    };

    let mut guest = Command::new(&program);
    guest.args(&guest_args);
    // Ask the compiled-in client to run its strace tool instead of the default
    // determinization tool.
    guest.env("REVERIE_DBI_STRACE", "1");

    match runner.status(&guest) {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            ExitCode::from(code as u8)
        }
        Err(error) => {
            eprintln!("dbi_strace: failed to run guest under DynamoRIO: {error}");
            ExitCode::FAILURE
        }
    }
}
