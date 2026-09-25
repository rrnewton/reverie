/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Linux ptrace host for the shared canonical-trace Tool.
//!
//! `narf_canonical_ptrace GUEST [ARGS...]` runs `GUEST` under reverie-ptrace
//! with `reverie_narf_tools::canonical::CanonicalTrace` and writes the Tool's
//! canonical records to stderr, one line each. The guest's own stdout and
//! stderr are inherited. This is the Linux reference cell for comparing the
//! Narf kernel backend against reverie-ptrace: the Tool source is the same;
//! only the line sink differs.

use std::io::Write;

use reverie::Error;
use reverie::process::Command;
use reverie_narf_tools::LineSink;
use reverie_narf_tools::canonical::CanonicalTrace;

/// Writes each canonical record, newline-terminated, to stderr in one call.
struct Stderr;

impl LineSink for Stderr {
    fn emit(line: &str) {
        let mut record = String::with_capacity(line.len() + 1);
        record.push_str(line);
        record.push('\n');
        let _ = std::io::stderr().lock().write_all(record.as_bytes());
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = std::env::args_os().skip(1);
    let Some(program) = args.next() else {
        eprintln!("usage: narf_canonical_ptrace GUEST [ARGS...]");
        std::process::exit(2);
    };
    let mut command = Command::new(program);
    command.args(args);
    let tracer = reverie_ptrace::TracerBuilder::<CanonicalTrace<Stderr>>::new(command)
        .spawn()
        .await?;
    let (status, ()) = tracer.wait().await?;
    status.raise_or_exit()
}
