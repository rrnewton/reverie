/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! End-to-end skeleton verification (the task's "Verify" bar):
//!
//! * the cdylib loads via `LD_PRELOAD`,
//! * its constructor installs seccomp + the SIGSYS handler,
//! * a trapped syscall is caught and mediated by the dispatcher.
//!
//! `passthrough` proves syscalls are trapped and correctly forwarded (the guest
//! behaves normally); `spoof-getpid` proves the trap can *mutate* a result; and
//! a `fork` guest proves the filter is inherited by children.

use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use reverie_preload::BuiltinTool;
use reverie_preload::SPOOF_PID;
use reverie_preload::configure_command;

fn preload_path() -> PathBuf {
    let probe = PathBuf::from(env!("CARGO_BIN_EXE_reverie-preload-probe"));
    let target = probe.parent().unwrap();
    [
        target.join("libreverie_preload.so"),
        target.join("deps/libreverie_preload.so"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("cargo did not build the preload cdylib")
}

fn run(program: &str, args: &[&str], tool: BuiltinTool) -> Output {
    let mut command = Command::new(program);
    command.args(args);
    // Point the launcher helper at the freshly built cdylib.
    unsafe {
        std::env::set_var("REVERIE_PRELOAD_LIB", preload_path());
    }
    configure_command(&mut command, tool).unwrap();
    command.output().unwrap()
}

fn wait_bounded(child: &mut std::process::Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("preload signal guest did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn passthrough_traps_and_forwards_echo() {
    let output = run("/bin/echo", &["hello"], BuiltinTool::Passthrough);
    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    // Correct output means every syscall /bin/echo made was trapped via SIGSYS
    // and forwarded through the trusted gate without corruption.
    assert_eq!(output.stdout, b"hello\n");
}

#[test]
fn spoof_getpid_proves_result_mutation() {
    let probe = env!("CARGO_BIN_EXE_reverie-preload-probe");
    let output = run(probe, &[], BuiltinTool::SpoofGetpid);
    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout.trim(),
        format!("getpid={SPOOF_PID}"),
        "the SIGSYS trap did not rewrite the getpid result"
    );
}

#[test]
fn passthrough_probe_reports_real_pid() {
    // Control: under passthrough the same probe must NOT see the spoof value.
    let probe = env!("CARGO_BIN_EXE_reverie-preload-probe");
    let output = run(probe, &[], BuiltinTool::Passthrough);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_ne!(stdout.trim(), format!("getpid={SPOOF_PID}"));
    assert!(stdout.trim().starts_with("getpid="));
}

#[test]
fn libc_signal_handler_returns_through_its_exact_restorer() {
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "trap 'echo signal; exit 0' USR1; echo ready; while :; do :; done",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        std::env::set_var("REVERIE_PRELOAD_LIB", preload_path());
    }
    configure_command(&mut command, BuiltinTool::Passthrough).unwrap();
    let mut child = command.spawn().unwrap();
    let child_pid = child.id();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let stdout_reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut bytes = Vec::new();
        let ready = reader.read_until(b'\n', &mut bytes).map(|_| bytes.clone());
        let _ = ready_tx.send(ready);
        reader.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = BufReader::new(stderr).read_to_end(&mut bytes);
        result.map(|_| bytes)
    });

    let ready = match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(ready)) => ready,
        result => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            panic!("preload signal guest did not become ready: {result:?}");
        }
    };
    if ready != b"ready\n" {
        let _ = child.kill();
        let _ = child.wait();
        let stdout = stdout_reader.join().unwrap().unwrap();
        let stderr = stderr_reader.join().unwrap().unwrap();
        panic!("unexpected readiness {ready:?}: stdout={stdout:?} stderr={stderr:?}");
    }

    if unsafe { libc::kill(child_pid as libc::pid_t, libc::SIGUSR1) } != 0 {
        let error = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        panic!("send external SIGUSR1: {error}");
    }
    let status = wait_bounded(&mut child, Duration::from_secs(5));
    let stdout = stdout_reader.join().unwrap().unwrap();
    let stderr = stderr_reader.join().unwrap().unwrap();
    assert!(
        status.success(),
        "status={status:?}\nstderr={}",
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(stdout, b"ready\nsignal\n");
}

#[test]
fn fork_child_inherits_the_filter() {
    // A shell that forks a child (subshell) and both write output. If the child
    // did not inherit the seccomp filter + handler, its syscalls would either
    // escape instrumentation or crash with default SIGSYS; either way the guest
    // would not produce this exact output under passthrough.
    let output = run(
        "/bin/sh",
        &["-c", "(echo child) ; echo parent"],
        BuiltinTool::Passthrough,
    );
    assert!(
        output.status.success(),
        "status={:?}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"child\nparent\n");
}
