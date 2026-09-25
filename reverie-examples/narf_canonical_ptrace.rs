/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Linux ptrace host for the shared canonical-trace Tool.
//!
//! `narf_canonical_ptrace [--with-bootstrap] [--wait-status FILE] GUEST [ARGS...]`
//! runs `GUEST` under reverie-ptrace with
//! `reverie_narf_tools::canonical::CanonicalTrace` and writes the Tool's
//! canonical records to stderr, one line each. The guest's own stdout and
//! stderr are inherited. `--wait-status FILE` also writes the status the tracer
//! reaped the guest with, as the raw wait status in the form
//! `exit wstatus=0x0000`, before this host exits with that same status. This is the Linux reference
//! cell for comparing the Narf kernel backend against reverie-ptrace: the Tool
//! source is the same; only the line sink differs.
//!
//! # The launch boundary
//!
//! reverie-ptrace starts tracing the spawned `Command` before its `execve`, so
//! the Tool is handed the launcher's `execve` of `GUEST` as a syscall of the
//! root thread ([`reverie::Guest::is_command_bootstrap`] is true for it). On
//! Narf the kernel loads the guest image itself and the root task's first
//! syscall is the guest's own, so no such event exists there. Hermit's Detcore
//! draws the same line (`guest_past_first_execve`) and does not log the launch
//! `execve` as a guest syscall.
//!
//! By default this host therefore runs the Tool only on the guest image's
//! syscalls: while the root is still the command bootstrap, a syscall is
//! executed unchanged and not shown to the Tool. `--with-bootstrap` hands every
//! syscall to the Tool, including the launch `execve`, so the difference can be
//! observed directly.

use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::Guest;
use reverie::Tool;
use reverie::process::Command;
use reverie::syscalls::Syscall;
use reverie_narf_tools::LineSink;
use reverie_narf_tools::canonical::CanonicalTrace;
use serde::Deserialize;
use serde::Serialize;

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

type Inner = CanonicalTrace<Stderr>;

/// Set by `--with-bootstrap`: hand the launch `execve` to the Tool as well.
static WITH_BOOTSTRAP: AtomicBool = AtomicBool::new(false);

/// The inner Tool's thread state, unchanged; the wrapper keeps none of its own.
#[derive(Default, Serialize, Deserialize)]
struct State(<Inner as Tool>::ThreadState);

impl AsRef<<Inner as Tool>::ThreadState> for State {
    fn as_ref(&self) -> &<Inner as Tool>::ThreadState {
        &self.0
    }
}

impl AsMut<<Inner as Tool>::ThreadState> for State {
    fn as_mut(&mut self) -> &mut <Inner as Tool>::ThreadState {
        &mut self.0
    }
}

/// Runs [`CanonicalTrace`] on every syscall except, by default, those the
/// root makes while it is still the command bootstrap.
#[derive(Default)]
struct GuestImageOnly {
    inner: Inner,
}

impl AsMut<Inner> for GuestImageOnly {
    fn as_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }
}

#[reverie::tool]
impl Tool for GuestImageOnly {
    type GlobalState = ();
    type ThreadState = State;

    async fn handle_syscall_event<T: Guest<Self>>(
        &self,
        guest: &mut T,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        if guest.is_command_bootstrap() && !WITH_BOOTSTRAP.load(Ordering::Relaxed) {
            guest.tail_inject(syscall).await
        } else {
            self.inner
                .handle_syscall_event(&mut guest.into_guest(), syscall)
                .await
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    const USAGE: &str =
        "usage: narf_canonical_ptrace [--with-bootstrap] [--wait-status FILE] GUEST [ARGS...]";
    let mut args = std::env::args_os().skip(1).peekable();
    let mut wait_status = None;
    loop {
        if args.peek().is_some_and(|arg| arg == "--with-bootstrap") {
            WITH_BOOTSTRAP.store(true, Ordering::Relaxed);
            args.next();
        } else if args.peek().is_some_and(|arg| arg == "--wait-status") {
            args.next();
            let Some(path) = args.next() else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            wait_status = Some(path);
        } else {
            break;
        }
    }
    let Some(program) = args.next() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let mut command = Command::new(program);
    command.args(args);
    let tracer = reverie_ptrace::TracerBuilder::<GuestImageOnly>::new(command)
        .spawn()
        .await?;
    let (status, ()) = tracer.wait().await?;
    if let Some(path) = wait_status {
        let record = format!("exit wstatus={:#06x}\n", status.into_raw());
        if let Err(error) = std::fs::write(&path, record) {
            eprintln!(
                "narf_canonical_ptrace: cannot write {}: {error}",
                path.to_string_lossy()
            );
            std::process::exit(2);
        }
    }
    status.raise_or_exit()
}
