use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use reverie_liteinst::COMPAT_EVENT_COOKIE_ENV;
use reverie_liteinst::COMPAT_EVENT_FD_ENV;
use reverie_liteinst::PreloadTool;
use reverie_liteinst::configure_command;

const TEST_EVENT_COOKIE: u64 = 7_915_913_731_959_187_131;
const TEST_EVENT_FD_ENV: &str = "REVERIE_LITEINST_TEST_EVENT_FD";

fn run_guest(program: &str, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-strace"))
        .env("REVERIE_LITEINST_PRELOAD", preload_path())
        .arg(program)
        .args(arguments)
        .output()
        .unwrap()
}

fn run_compat_guest(program: &str, arguments: &[&str]) -> Output {
    let mut command = Command::new(program);
    command.args(arguments);
    configure_command(&mut command, PreloadTool::Compatibility).unwrap();
    command.output().unwrap()
}

fn run_compat_guest_with_event_pipe(program: &str, arguments: &[&str]) -> (Output, Vec<u8>) {
    let mut descriptors = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read_fd = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    let inherited_write_fd = write_fd.as_raw_fd();

    let mut command = Command::new(program);
    command
        .args(arguments)
        .env(COMPAT_EVENT_FD_ENV, inherited_write_fd.to_string())
        .env(COMPAT_EVENT_COOKIE_ENV, TEST_EVENT_COOKIE.to_string())
        .env(TEST_EVENT_FD_ENV, inherited_write_fd.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_command(&mut command, PreloadTool::Compatibility).unwrap();
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(inherited_write_fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let reader = thread::spawn(move || {
        let mut events = Vec::new();
        File::from(read_fd).read_to_end(&mut events).unwrap();
        events
    });
    let child = command.spawn().unwrap();
    drop(write_fd);
    let output = child.wait_with_output().unwrap();
    let events = reader.join().unwrap();
    (output, events)
}

fn preload_path() -> PathBuf {
    let launcher = PathBuf::from(env!("CARGO_BIN_EXE_reverie-liteinst-strace"));
    let target = launcher.parent().unwrap();
    [
        target.join("libreverie_liteinst.so"),
        target.join("deps/libreverie_liteinst.so"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("cargo did not build the preload cdylib")
}

fn parse_compatibility_events(events: &[u8]) -> Vec<(u32, u32, i64, u64)> {
    let events = std::str::from_utf8(events).unwrap();
    let prefix = format!("reverie-liteinst: tool=compat cookie={TEST_EVENT_COOKIE} pid=");
    events
        .lines()
        .map(|line| {
            let record = line
                .strip_prefix(&prefix)
                .unwrap_or_else(|| panic!("unexpected event record: {line}"));
            let (pid, record) = record
                .split_once(" tid=")
                .unwrap_or_else(|| panic!("event record is missing TID: {line}"));
            let (tid, record) = record
                .split_once(" syscall=")
                .unwrap_or_else(|| panic!("event record is missing syscall: {line}"));
            let (syscall, arg1) = record
                .split_once(" arg1=")
                .unwrap_or_else(|| panic!("event record is missing arg1: {line}"));
            (
                pid.parse().unwrap(),
                tid.parse().unwrap(),
                syscall.parse().unwrap(),
                arg1.parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn strace_tool_observes_echo_syscalls() {
    let output = run_guest("/bin/echo", &["hello"]);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"hello\n");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("[liteinst strace pid "));
    assert!(stderr.contains("syscall(1,"));
}

#[test]
fn compatibility_tool_emits_stable_events() {
    let first = run_compat_guest("/bin/echo", &["hello"]);
    let second = run_compat_guest("/bin/echo", &["hello"]);
    assert!(first.status.success(), "first status={:?}", first.status);
    assert!(second.status.success(), "second status={:?}", second.status);
    assert_eq!(first.stdout, b"hello\n");
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.stderr, second.stderr);

    let events = String::from_utf8(first.stderr).unwrap();
    assert!(
        events.lines().all(|line| line
            .strip_prefix("reverie-liteinst: tool=compat syscall=")
            .and_then(|record| record.split_once(" arg1="))
            .is_some_and(|(number, arg1)| {
                number.parse::<i64>().is_ok() && arg1.parse::<u64>().is_ok()
            })),
        "unexpected events: {events}"
    );
    assert!(events.lines().count() > 1, "missing events: {events}");
}

#[test]
fn compatibility_event_fd_separates_guest_stderr() {
    let spoof = "reverie-liteinst: tool=compat syscall=999999\n";
    let (output, events) = run_compat_guest_with_event_pipe(
        "/bin/sh",
        &[
            "-c",
            "test -z \"$REVERIE_LITEINST_EVENT_FD\"; test -z \"$REVERIE_LITEINST_EVENT_COOKIE\"; printf 'reverie-liteinst: tool=compat syscall=999999\\n' >&2",
        ],
    );
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, spoof.as_bytes());

    let records = parse_compatibility_events(&events);
    assert!(records.len() > 1, "missing events");
    assert!(
        records.iter().all(|(_, _, syscall, _)| *syscall != 999999),
        "guest stderr leaked into the event channel"
    );
}

#[test]
fn compatibility_event_fd_survives_guest_close() {
    let (output, events) = run_compat_guest_with_event_pipe(
        "/bin/sh",
        &[
            "-c",
            "eval \"exec ${REVERIE_LITEINST_TEST_EVENT_FD}>&-\"; printf 'channel-survived\\n'",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"channel-survived\n");
    let events = String::from_utf8(events).unwrap();
    assert!(
        events.contains(&format!(
            "reverie-liteinst: tool=compat cookie={TEST_EVENT_COOKIE} pid="
        )),
        "missing dedicated events: {events}"
    );
}

#[test]
fn compatibility_event_fd_rejects_guest_spoof_write() {
    let forged = format!(
        "reverie-liteinst: tool=compat cookie={TEST_EVENT_COOKIE} pid=999999 tid=999999 syscall=999999"
    );
    let script = format!(
        "eval \"printf '{forged}\\n' >&${{REVERIE_LITEINST_TEST_EVENT_FD}}\" 2>/dev/null; result=$?; test $result -ne 0; printf 'spoof-rejected\\n'"
    );
    let (output, events) = run_compat_guest_with_event_pipe("/bin/sh", &["-c", &script]);
    assert!(
        output.status.success(),
        "{output:?}; events={}",
        String::from_utf8_lossy(&events)
    );
    assert_eq!(output.stdout, b"spoof-rejected\n");
    let events = String::from_utf8(events).unwrap();
    assert!(
        !events.contains(&forged),
        "forged event was accepted: {events}"
    );
}

#[test]
fn compatibility_event_fd_backpressure_fails_without_hanging() {
    let mut descriptors = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read_fd = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    let inherited_write_fd = write_fd.as_raw_fd();

    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "i=0; while [ \"$i\" -lt 100000 ]; do : > /dev/null; i=$((i + 1)); done",
        ])
        .env(COMPAT_EVENT_FD_ENV, inherited_write_fd.to_string())
        .env(COMPAT_EVENT_COOKIE_ENV, TEST_EVENT_COOKIE.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_command(&mut command, PreloadTool::Compatibility).unwrap();
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(inherited_write_fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn().unwrap();
    drop(write_fd);
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("dedicated event channel blocked on a full pipe");
        }
        thread::sleep(Duration::from_millis(10));
    };
    drop(read_fd);
    assert_eq!(status.code(), Some(121), "{status:?}");
}

#[test]
fn compatibility_event_fd_recovers_when_delayed_reader_drains() {
    let mut descriptors = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read_fd = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    let inherited_write_fd = write_fd.as_raw_fd();
    assert_eq!(
        unsafe { libc::fcntl(inherited_write_fd, libc::F_SETPIPE_SZ, 4096) },
        4096
    );

    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "i=0; while [ \"$i\" -lt 1000 ]; do : > /dev/null; i=$((i + 1)); done",
        ])
        .env(COMPAT_EVENT_FD_ENV, inherited_write_fd.to_string())
        .env(COMPAT_EVENT_COOKIE_ENV, TEST_EVENT_COOKIE.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_command(&mut command, PreloadTool::Compatibility).unwrap();
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(inherited_write_fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn().unwrap();
    drop(write_fd);
    thread::sleep(Duration::from_millis(250));
    let reader = thread::spawn(move || {
        let mut events = Vec::new();
        File::from(read_fd).read_to_end(&mut events).unwrap();
        events
    });
    let status = child.wait().unwrap();
    let events = reader.join().unwrap();
    assert!(status.success(), "{status:?}");
    assert!(
        events.len() > 4096,
        "delayed reader did not drain a full pipe"
    );
}

#[test]
fn compatibility_tool_rejects_process_group_escape() {
    let output = run_compat_guest(
        "/usr/bin/python3",
        &[
            "-c",
            "import os\ntry:\n os.setsid()\nexcept PermissionError:\n print('setsid-rejected')\nelse:\n raise SystemExit('setsid unexpectedly succeeded')",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"setsid-rejected\n");
}

#[test]
fn compatibility_threads_emit_distinct_tids() {
    let (output, events) = run_compat_guest_with_event_pipe(
        env!("CARGO_BIN_EXE_reverie-liteinst-advanced-guest"),
        &["threads"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"threads-ok\n");

    let records = parse_compatibility_events(&events);
    let pids: BTreeSet<_> = records.iter().map(|(pid, _, _, _)| *pid).collect();
    let tids: BTreeSet<_> = records.iter().map(|(_, tid, _, _)| *tid).collect();
    assert_eq!(pids.len(), 1, "thread workers changed process: {pids:?}");
    assert!(
        tids.len() >= 5,
        "expected the main thread and four workers, got {tids:?}"
    );
}

#[test]
fn compatibility_raw_thread_does_not_require_loader_tls() {
    let (output, events) = run_compat_guest_with_event_pipe(
        env!("CARGO_BIN_EXE_reverie-liteinst-advanced-guest"),
        &["raw-thread"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"raw-thread-ok\n");

    let records = parse_compatibility_events(&events);
    let pids: BTreeSet<_> = records.iter().map(|(pid, _, _, _)| *pid).collect();
    let tids: BTreeSet<_> = records.iter().map(|(_, tid, _, _)| *tid).collect();
    assert_eq!(pids.len(), 1, "raw thread changed process: {pids:?}");
    assert!(
        tids.len() >= 2,
        "expected main and raw clone thread, got {tids:?}"
    );
}

#[test]
fn compatibility_direct_tid_table_survives_thread_churn() {
    let (output, events) = run_compat_guest_with_event_pipe(
        env!("CARGO_BIN_EXE_reverie-liteinst-advanced-guest"),
        &["thread-churn"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"thread-churn-ok\n");
    assert!(!parse_compatibility_events(&events).is_empty());
}

#[test]
fn compatibility_fork_child_uses_process_private_tid_table() {
    let (output, events) = run_compat_guest_with_event_pipe(
        env!("CARGO_BIN_EXE_reverie-liteinst-advanced-guest"),
        &["fork-churn"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"fork-churn-ok\n");
    assert!(!parse_compatibility_events(&events).is_empty());
}

#[test]
fn compatibility_signal_storm_survives_handler_syscalls() {
    let (output, events) = run_compat_guest_with_event_pipe(
        env!("CARGO_BIN_EXE_reverie-liteinst-advanced-guest"),
        &["signals"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"signals-ok\n");
    assert!(!parse_compatibility_events(&events).is_empty());
}

#[test]
fn asynchronous_sigsys_is_rejected_before_event_dispatch() {
    let output = run_compat_guest("/bin/sh", &["-c", "kill -SYS $$"]);
    assert_eq!(output.status.code(), Some(126), "{output:?}");
}

#[test]
fn compatibility_fork_stress_instruments_every_process() {
    let (output, events) = run_compat_guest_with_event_pipe(
        env!("CARGO_BIN_EXE_reverie-liteinst-advanced-guest"),
        &["fork"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"fork-ok\n");

    let records = parse_compatibility_events(&events);
    let pids: BTreeSet<_> = records.iter().map(|(pid, _, _, _)| *pid).collect();
    assert!(
        pids.len() >= 17,
        "expected parent and sixteen children, got {pids:?}"
    );
}

#[test]
fn compatibility_seeded_chaos_combines_threads_signals_and_forks() {
    for seed in 0..4 {
        let seed = seed.to_string();
        let (output, events) = run_compat_guest_with_event_pipe(
            env!("CARGO_BIN_EXE_reverie-liteinst-advanced-guest"),
            &["chaos", &seed],
        );
        assert!(output.status.success(), "seed={seed}: {output:?}");
        assert_eq!(output.stdout, b"chaos-ok\n", "seed={seed}");

        let records = parse_compatibility_events(&events);
        let pids: BTreeSet<_> = records.iter().map(|(pid, _, _, _)| *pid).collect();
        let tids: BTreeSet<_> = records.iter().map(|(_, tid, _, _)| *tid).collect();
        assert!(pids.len() >= 18, "seed={seed}: pids={pids:?}");
        assert!(tids.len() >= 22, "seed={seed}: tids={tids:?}");
    }
}

#[test]
fn compatibility_event_fd_rejects_read_only_descriptor() {
    let mut descriptors = [0; 2];
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
        0
    );
    let read_fd = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let _write_fd = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    let inherited_read_fd = read_fd.as_raw_fd();

    let mut command = Command::new("/bin/true");
    command
        .env(COMPAT_EVENT_FD_ENV, inherited_read_fd.to_string())
        .env(COMPAT_EVENT_COOKIE_ENV, TEST_EVENT_COOKIE.to_string());
    configure_command(&mut command, PreloadTool::Compatibility).unwrap();
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(inherited_read_fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();

    assert_eq!(output.status.code(), Some(127), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("must name a writable descriptor"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fork_child_inherits_preload_instrumentation() {
    let output = run_guest(env!("CARGO_BIN_EXE_reverie-liteinst-fork-guest"), &[]);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("fork child reached guest code"));
    assert!(stdout.contains("fork parent observed child"));

    let stderr = String::from_utf8(output.stderr).unwrap();
    let pids: BTreeSet<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[liteinst strace pid "))
        .filter_map(|line| line.split(']').next())
        .collect();
    assert!(
        pids.len() >= 2,
        "expected trace records from parent and child, got {pids:?}:\n{stderr}"
    );
}

#[test]
fn compatibility_fork_reports_clone_only_from_parent() {
    assert_compatibility_fork_event(&[], libc::SYS_clone);
}

#[test]
fn compatibility_raw_fork_reports_once_before_child() {
    assert_compatibility_fork_event(&["--raw-fork"], libc::SYS_fork);
}

#[test]
fn unsafe_clone_is_rejected_in_compatibility_and_strace_modes() {
    let guest = env!("CARGO_BIN_EXE_reverie-liteinst-fork-guest");
    let compatibility = run_compat_guest(guest, &["--unsafe-clone"]);
    assert!(compatibility.status.success(), "{compatibility:?}");
    assert_eq!(
        compatibility.stdout,
        format!("unsafe clone rejected: {}\n", libc::EPERM).as_bytes()
    );

    let strace = run_guest(guest, &["--unsafe-clone"]);
    assert!(strace.status.success(), "{strace:?}");
    assert_eq!(
        strace.stdout,
        format!("unsafe clone rejected: {}\n", libc::ENOTSUP).as_bytes()
    );
}

fn assert_compatibility_fork_event(arguments: &[&str], syscall: i64) {
    let (output, events) = run_compat_guest_with_event_pipe(
        env!("CARGO_BIN_EXE_reverie-liteinst-fork-guest"),
        arguments,
    );
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let records = parse_compatibility_events(&events);
    assert_eq!(
        records
            .iter()
            .filter(|(_, _, number, _)| *number == syscall)
            .count(),
        1,
        "successful fork must have one parent-owned compatibility event"
    );
    let clone_position = records
        .iter()
        .position(|(_, _, number, _)| *number == syscall)
        .unwrap();
    let parent_pid = records[clone_position].0;
    let child_position = records
        .iter()
        .position(|(pid, _, _, _)| *pid != parent_pid)
        .expect("child instrumentation event is missing");
    assert!(
        clone_position < child_position,
        "clone marker must precede child activity"
    );
    let pids: BTreeSet<_> = records.iter().map(|(pid, _, _, _)| *pid).collect();
    assert!(
        pids.len() >= 2,
        "child instrumentation events are missing: {pids:?}"
    );
}

#[test]
fn exec_fails_closed_before_runtime_is_replaced() {
    let output = run_guest(env!("CARGO_BIN_EXE_reverie-liteinst-exec-guest"), &[]);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"exec rejected with ENOTSUP\n");

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("syscall(59,"));
    assert!(stderr.contains("= -95"));
}
