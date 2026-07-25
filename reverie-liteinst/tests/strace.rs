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

use reverie_liteinst::COMPAT_EVENT_FD_ENV;
use reverie_liteinst::PreloadTool;
use reverie_liteinst::configure_command;

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
            .is_some_and(|number| number.parse::<i64>().is_ok())),
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
            "printf 'reverie-liteinst: tool=compat syscall=999999\\n' >&2",
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

    let events = String::from_utf8(events).unwrap();
    assert!(
        events.lines().all(|line| line
            .strip_prefix("reverie-liteinst: tool=compat syscall=")
            .is_some_and(|number| number.parse::<i64>().is_ok())),
        "unexpected events: {events}"
    );
    assert!(!events.contains("999999"), "guest stderr leaked: {events}");
    assert!(events.lines().count() > 1, "missing events: {events}");
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
    command.env(COMPAT_EVENT_FD_ENV, inherited_read_fd.to_string());
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
