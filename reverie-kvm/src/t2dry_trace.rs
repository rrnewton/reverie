/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! SCRATCH instrumentation for the kvm lane's T2 dry run; never for review.
//!
//! When `REVERIE_KVM_TIMER_TRACE_DIR` names a directory, each process appends
//! one tab-separated line per preemption-timer notification, delivery and
//! cancelled stepping window to `<dir>/kvmtimer-<pid>-<monotonic ns>.tsv`, after a PROC line naming the
//! process's arguments and working directory, with one unbuffered
//! `write` per line. Without it, every hook is one `OnceLock` read. Nothing it
//! records reaches the guest, the Tool or any log Hermit compares.

use std::io::Write;
use std::sync::Mutex;
use std::sync::OnceLock;

static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

fn sink() -> &'static Option<Mutex<std::fs::File>> {
    SINK.get_or_init(|| {
        let dir = std::env::var_os("REVERIE_KVM_TIMER_TRACE_DIR")?;
        // The process is often PID 1 of its own namespace, so the name also
        // carries the host's monotonic clock at the first line.
        let path = std::path::Path::new(&dir).join(format!(
            "kvmtimer-{}-{}.tsv",
            std::process::id(),
            crate::preemption_timer::monotonic_ns()
        ));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        let args: Vec<String> = std::env::args().collect();
        let cwd = std::env::current_dir().unwrap_or_default();
        let _ = writeln!(
            file,
            "PROC\t{}\t{}\t{}",
            std::process::id(),
            cwd.display(),
            args.join("\u{1f}")
        );
        Some(Mutex::new(file))
    })
}

/// Whether tracing is on for this process.
pub(crate) fn enabled() -> bool {
    sink().is_some()
}

/// Appends one line (a trailing newline is added).
pub(crate) fn line(args: std::fmt::Arguments<'_>) {
    if let Some(file) = sink() {
        let mut text = format!("{args}");
        text.push('\n');
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(text.as_bytes());
        }
    }
}

/// The calling host thread's id.
pub(crate) fn tid() -> i64 {
    // SAFETY: gettid has no arguments and cannot fail.
    unsafe { libc::syscall(libc::SYS_gettid) }
}
