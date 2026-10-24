#[cfg(feature = "liteinst")]
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::File;
#[cfg(feature = "liteinst")]
use std::io::Read;
#[cfg(feature = "liteinst")]
use std::os::fd::AsRawFd;
#[cfg(feature = "liteinst")]
use std::os::fd::FromRawFd;
#[cfg(feature = "liteinst")]
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use reverie_liteinst::BuiltinTool;
#[cfg(feature = "liteinst")]
use reverie_liteinst::COMPAT_EVENT_COOKIE_ENV;
#[cfg(feature = "liteinst")]
use reverie_liteinst::COMPAT_EVENT_FD_ENV;
use reverie_liteinst::PreloadTool;
use reverie_liteinst::SITE_PATCHING_ENV;
use reverie_liteinst::SPOOF_PID;
use reverie_liteinst::STRADDLER_STALENESS_TICKS_ENV;
use reverie_liteinst::configure_command;
use reverie_liteinst::configure_command_builtin;

#[cfg(not(feature = "liteinst"))]
#[path = "support/fulltool_capture.rs"]
mod fulltool_capture;
#[allow(dead_code)]
#[path = "support/liteinst_runtime.rs"]
mod liteinst_runtime;

#[cfg(feature = "liteinst")]
const TEST_EVENT_COOKIE: u64 = 7_915_913_731_959_187_131;
#[cfg(feature = "liteinst")]
const TEST_EVENT_FD_ENV: &str = "REVERIE_LITEINST_TEST_EVENT_FD";
const TEST_STRADDLER_STALENESS_TICKS: &str = "20000";

fn enable_concurrent_patch_testing(command: &mut Command) {
    command.env(
        STRADDLER_STALENESS_TICKS_ENV,
        TEST_STRADDLER_STALENESS_TICKS,
    );
}

#[cfg(feature = "liteinst")]
fn strace_guest_command(program: &str, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-strace"));
    command
        .env("REVERIE_LITEINST_PRELOAD", preload_path())
        .env(
            STRADDLER_STALENESS_TICKS_ENV,
            TEST_STRADDLER_STALENESS_TICKS,
        )
        .arg(program)
        .args(arguments);
    command
}

#[cfg(feature = "liteinst")]
fn run_guest(program: &str, arguments: &[&str]) -> Output {
    strace_guest_command(program, arguments).output().unwrap()
}

/// Run `command` in a process group of its own and collect its output, killing
/// the whole group once the command exits or `limit` passes, whichever is
/// first. A command that has not exited within `limit` fails the calling test
/// instead of stalling the test run. A task it leaves behind in the group is
/// killed with the group, so it cannot hold the output pipes open; that alone
/// does not fail the test.
#[cfg(feature = "liteinst")]
fn output_within(command: &mut Command, limit: Duration) -> Output {
    fn read_to_end_in_background(
        mut pipe: impl Read + Send + 'static,
    ) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).unwrap();
            bytes
        })
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = libc::pid_t::try_from(child.id()).unwrap();
    let stdout = read_to_end_in_background(child.stdout.take().unwrap());
    let stderr = read_to_end_in_background(child.stderr.take().unwrap());
    let deadline = Instant::now() + limit;
    let exited = loop {
        // WNOWAIT leaves the exited command unreaped, so its pid still names
        // the process group when the group is killed below.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                group as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(waited, 0, "waitid: {}", std::io::Error::last_os_error());
        if unsafe { info.si_pid() } != 0 {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    // Kill anything left in the group, such as a task that a forwarded clone3
    // created, so that the pipe readers reach end of file.
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
    let status = child.wait().unwrap();
    let output = Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(
        exited,
        "guest did not exit within {limit:?}; killed its process group: {output:?}"
    );
    output
}

#[cfg(feature = "liteinst")]
fn run_compat_guest(program: &str, arguments: &[&str]) -> Output {
    let mut command = Command::new(program);
    command.args(arguments);
    enable_concurrent_patch_testing(&mut command);
    configure_command(&mut command, PreloadTool::Compatibility).unwrap();
    command.output().unwrap()
}

#[cfg(feature = "liteinst")]
fn compile_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let output = directory.path().join(name.trim_end_matches(".c"));
    let compiler = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let result = Command::new(compiler)
        .args(["-std=gnu11", "-O0", "-fno-pie", "-no-pie"])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "failed to compile {}:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    (directory, output)
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared built-in guest harness.
fn run_builtin_guest(tool: BuiltinTool, program: &str, arguments: &[&str]) -> Output {
    let mut command = Command::new(program);
    command.args(arguments);
    configure_command_builtin(&mut command, tool).unwrap();
    command.output().unwrap()
}

#[cfg(feature = "liteinst")]
fn run_compat_guest_with_event_pipe(program: &str, arguments: &[&str]) -> (Output, Vec<u8>) {
    run_compat_guest_with_event_pipe_within(program, arguments, None)
}

/// As [`run_compat_guest_with_event_pipe`], but with `Some(limit)` the guest
/// runs through [`output_within`].
#[cfg(feature = "liteinst")]
fn run_compat_guest_with_event_pipe_within(
    program: &str,
    arguments: &[&str],
    limit: Option<Duration>,
) -> (Output, Vec<u8>) {
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
    enable_concurrent_patch_testing(&mut command);
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
    let output = match limit {
        None => {
            let child = command.spawn().unwrap();
            drop(write_fd);
            child.wait_with_output().unwrap()
        }
        Some(limit) => {
            // The reader drains the event pipe while the guest runs, so holding
            // this end open until the guest's group is gone cannot block it.
            let output = output_within(&mut command, limit);
            drop(write_fd);
            output
        }
    };
    let events = reader.join().unwrap();
    (output, events)
}

fn preload_path() -> PathBuf {
    liteinst_runtime::required_preload_path().unwrap_or_else(|error| {
        panic!("strace integration requires the qualified standalone producer inputs: {error}")
    })
}

#[test]
#[cfg(feature = "liteinst")]
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

#[cfg(feature = "liteinst")]
#[test]
fn first_sigsys_installs_a_hook_for_later_calls() {
    let output = run_guest(env!("CARGO_BIN_EXE_reverie-liteinst-trap-count-guest"), &[]);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"calls=32 traps=1 hooks=32\n");
}

#[test]
#[cfg(not(feature = "liteinst"))]
fn fulltool_traps_preserve_original_pc_relative_addresses() -> Result<(), String> {
    let native = run_pc_relative_guest(false);
    assert!(native.status.success(), "native oracle: {native:?}");
    assert_eq!(native.stderr, b"");
    assert_eq!(
        native.stdout,
        b"pc-relative native: calls=32 addresses=32 start_addresses=32\n"
    );
    let qualified = liteinst_runtime::required_runtime()?;
    let evidence = liteinst_runtime::evidence_directory("pc-relative-fulltool-traps")?;
    let before = liteinst_runtime::current_source(&evidence, "before")?;
    assert_eq!(before, qualified.source);
    let binary = liteinst_runtime::artifact::FileIdentity::read(std::path::Path::new(env!(
        "CARGO_BIN_EXE_reverie-liteinst-trap-count-guest"
    )))?;
    let socket = evidence.join("coordinator.sock");
    let mut held = liteinst_runtime::held_environment(
        b"same two original assembly/LEA sites, 32 calls each; genuine Fixed FullTool; exact 64 callbacks and fallbacks; unchanged function bytes; zero sites/hooks".to_vec(),
    );
    held.environment.retain(|(key, _)| {
        ![
            "LD_PRELOAD",
            "REVERIE_LITEINST_TOOL",
            "REVERIE_INGUEST_TOOL",
            reverie_liteinst::COORDINATOR_ENV,
            SITE_PATCHING_ENV,
        ]
        .iter()
        .any(|name| key == name)
    });
    held.environment.extend([
        ("LD_PRELOAD".into(), qualified.path.as_os_str().to_owned()),
        (
            reverie_liteinst::COORDINATOR_ENV.into(),
            socket.as_os_str().to_owned(),
        ),
        (SITE_PATCHING_ENV.into(), "0".into()),
    ]);
    let mut command = Command::new(&binary.path);
    command
        .arg("pc-relative-trapped")
        .current_dir(&qualified.source.root);
    let result = (|| -> Result<(), String> {
        let run = match fulltool_capture::run(command, held, socket) {
            Ok(run) => {
                liteinst_runtime::retain_capture(&evidence, "actual-fulltool", &run)?;
                run
            }
            Err(error) => {
                liteinst_runtime::retain_failure(&evidence, "actual-fulltool", &error)?;
                return Err(error.to_string());
            }
        };
        assert_eq!(run.end, liteinst_runtime::contract::CaptureEnd::Complete);
        assert!(!run.output_truncated && run.input.is_none());
        assert!(run.status.is_some_and(|status| status.success()), "{run:?}");
        assert_eq!(run.stdout, b"pc-relative trapped: calls=32 addresses=32 start_addresses=32 fallback=64 callbacks=64 sites=0 hooks=0 bytes=unchanged\n");
        assert!(run.stderr.is_empty(), "{run:?}");
        Ok(())
    })();
    let after = (|| -> Result<(), String> {
        binary.verify()?;
        qualified.receipt.artifact.verify()?;
        assert_eq!(
            liteinst_runtime::current_source(&evidence, "after")?,
            before
        );
        Ok(())
    })();
    match (result, after) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => {
            Err(format!("{error}; evidence {}", evidence.display()))
        }
        (Err(error), Err(after)) => Err(format!(
            "{error}; final guard: {after}; evidence {}",
            evidence.display()
        )),
    }
}

fn run_pc_relative_guest(hooked: bool) -> Output {
    let binary = env!("CARGO_BIN_EXE_reverie-liteinst-trap-count-guest");
    let directory = tempfile::tempdir().unwrap();
    let mut command = if hooked {
        let mut command = Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-strace"));
        command.arg(binary).arg("pc-relative-hooked");
        command.env("REVERIE_LITEINST_PRELOAD", preload_path());
        enable_concurrent_patch_testing(&mut command);
        command
    } else {
        let mut command = Command::new(binary);
        command.arg("pc-relative-native");
        command
    };
    command
        .env_remove("LD_PRELOAD")
        .env_remove("REVERIE_LITEINST_TOOL")
        .env_remove("REVERIE_INGUEST_TOOL")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(File::create(directory.path().join("stdout")).unwrap())
        .stderr(File::create(directory.path().join("stderr")).unwrap());
    // SAFETY: only async-signal-safe setrlimit calls run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            for (resource, value) in [(libc::RLIMIT_CORE, 0), (libc::RLIMIT_FSIZE, 1024 * 1024)] {
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            result => {
                // SAFETY: the live child's dedicated process group is ours.
                unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                let _ = child.wait();
                panic!(
                    "pc-relative guest exceeded its deadline or could not be waited: {result:?}"
                );
            }
        }
    };
    Output {
        status,
        stdout: std::fs::read(directory.path().join("stdout")).unwrap(),
        stderr: std::fs::read(directory.path().join("stderr")).unwrap(),
    }
}

#[test]
#[cfg(feature = "liteinst")]
fn syscall_hook_preserves_displaced_self_relative_address() {
    let native = run_pc_relative_guest(false);
    assert!(native.status.success(), "native oracle: {native:?}");
    assert_eq!(native.stderr, b"");
    assert_eq!(
        native.stdout,
        b"pc-relative native: calls=32 addresses=32 start_addresses=32\n"
    );

    let hooked = run_pc_relative_guest(true);
    assert!(hooked.status.success(), "hooked guest: {hooked:?}");
    if cfg!(feature = "liteinst") {
        assert_eq!(
            hooked.stdout,
            b"pc-relative hooked: calls=32 addresses=32 traps=32 hooks=0 \
              start_addresses=32 start_traps=1 start_hooks=32\n"
        );
    } else {
        assert_eq!(
            hooked.stdout,
            b"pc-relative hooked: calls=32 addresses=32 traps=0 hooks=0 \
              start_addresses=32 start_traps=0 start_hooks=0\n"
        );
        let stderr = String::from_utf8(hooked.stderr).unwrap();
        assert_eq!(
            stderr
                .lines()
                .filter(|line| line.contains("syscall(39,"))
                .count(),
            64,
            "both relative-address sites must reach the Tool on every call: {stderr}"
        );
    }
}

#[test]
#[cfg(feature = "liteinst")]
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
#[cfg(feature = "liteinst")]
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

    let events = String::from_utf8(events).unwrap();
    let prefix = format!("reverie-liteinst: tool=compat cookie={TEST_EVENT_COOKIE} pid=");
    assert!(
        events.lines().all(|line| {
            line.strip_prefix(&prefix)
                .and_then(|record| record.split_once(" syscall="))
                .is_some_and(|(pid, number)| {
                    pid.parse::<u32>().is_ok() && number.parse::<i64>().is_ok()
                })
        }),
        "unexpected events: {events}"
    );
    assert!(!events.contains("999999"), "guest stderr leaked: {events}");
    assert!(events.lines().count() > 1, "missing events: {events}");
}

#[test]
#[cfg(feature = "liteinst")]
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
#[cfg(feature = "liteinst")]
fn compatibility_event_fd_rejects_guest_spoof_write() {
    let forged = format!(
        "reverie-liteinst: tool=compat cookie={TEST_EVENT_COOKIE} pid=999999 syscall=999999"
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
#[cfg(feature = "liteinst")]
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
    enable_concurrent_patch_testing(&mut command);
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
#[cfg(feature = "liteinst")]
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
    enable_concurrent_patch_testing(&mut command);
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
#[cfg(feature = "liteinst")]
fn compatibility_tool_rejects_process_group_escape() {
    let (_directory, guest) = compile_fixture("compat_setsid.c");
    let output = run_compat_guest(guest.to_str().unwrap(), &[]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"setsid-rejected\n");
}

#[test]
#[cfg(feature = "liteinst")]
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
    enable_concurrent_patch_testing(&mut command);
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

/// Other runtimes accept only their build's default selector. In a build
/// without LiteInst, `0` keeps every syscall on the trap path.
#[test]
fn other_runtimes_refuse_site_patching_selector() {
    fn check(mut command: Command, runtime: &str, value: &str) {
        command.env(SITE_PATCHING_ENV, value);
        let output = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if value == if cfg!(feature = "liteinst") { "1" } else { "0" } {
            assert!(output.status.success(), "{runtime} {value}: {output:?}");
            assert!(
                !stderr.contains(SITE_PATCHING_ENV),
                "{runtime} {value}: {stderr}"
            );
            return;
        }
        assert_eq!(
            output.status.code(),
            Some(127),
            "{runtime} {value}: {output:?}"
        );
        if !cfg!(feature = "liteinst") && value == "1" {
            assert!(
                stderr.contains("built without LiteInst"),
                "{runtime} {value}: {stderr}"
            );
        } else {
            assert!(
                stderr.contains(&format!("the {runtime} runtime does not take it")),
                "{runtime} {value}: {stderr}"
            );
        }
    }
    for value in ["1", "0", "2"] {
        #[cfg(feature = "liteinst")]
        for (tool, runtime) in [
            (PreloadTool::Strace, "strace"),
            (PreloadTool::Compatibility, "compat"),
        ] {
            let mut command = Command::new("/bin/true");
            configure_command(&mut command, tool).unwrap();
            check(command, runtime, value);
        }
        let mut command = Command::new("/bin/true");
        configure_command_builtin(&mut command, BuiltinTool::Passthrough).unwrap();
        check(command, "built-in Tool", value);
    }
}

#[test]
#[cfg(not(feature = "liteinst"))]
fn legacy_launcher_api_refuses_with_typed_error_before_command_mutation() {
    for (tool, name) in [
        (PreloadTool::Strace, "strace"),
        (PreloadTool::Compatibility, "compat"),
    ] {
        let mut command = Command::new("/nonexistent-command-must-not-run");
        command
            .arg("literal-argument")
            .env("LITERAL_MARKER", "unchanged")
            .env(
                "REVERIE_LITEINST_PRELOAD",
                "/nonexistent-preload-must-not-resolve",
            );
        let arguments = command
            .get_args()
            .map(|value| value.to_owned())
            .collect::<Vec<_>>();
        let environment = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(|value| value.to_owned())))
            .collect::<Vec<_>>();
        let error = configure_command(&mut command, tool).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            error.to_string(),
            format!(
                "legacy {name} runtime was built without LiteInst; enable the `liteinst` Cargo feature or use the in-guest FullTool installation route"
            )
        );
        assert_eq!(command.get_program(), "/nonexistent-command-must-not-run");
        assert_eq!(
            command
                .get_args()
                .map(|value| value.to_owned())
                .collect::<Vec<_>>(),
            arguments
        );
        assert_eq!(
            command
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(|value| value.to_owned())))
                .collect::<Vec<_>>(),
            environment
        );
    }
}

#[test]
#[cfg(not(feature = "liteinst"))]
fn raw_legacy_constructor_selectors_refuse_before_main() -> Result<(), String> {
    let qualified = liteinst_runtime::required_runtime()?;
    let evidence = liteinst_runtime::evidence_directory("raw-legacy-selector-refusal")?;
    let before = liteinst_runtime::current_source(&evidence, "before")?;
    assert_eq!(before, qualified.source);
    let result = (|| {
        for name in ["strace", "compat"] {
            let mut held = liteinst_runtime::held_environment(
                format!("raw {name} selector; genuine qualified constructor; explicit refusal, no positive parity").into_bytes(),
            );
            held.environment.retain(|(key, _)| {
                ![
                    "LD_PRELOAD",
                    "REVERIE_LITEINST_TOOL",
                    "REVERIE_INGUEST_TOOL",
                    SITE_PATCHING_ENV,
                ]
                .iter()
                .any(|value| key == &OsString::from(value))
            });
            held.environment.extend([
                ("LD_PRELOAD".into(), qualified.path.as_os_str().to_owned()),
                ("REVERIE_LITEINST_TOOL".into(), name.into()),
                (SITE_PATCHING_ENV.into(), "0".into()),
            ]);
            let mut command = Command::new("/bin/echo");
            command.arg("main-must-not-run");
            let run = match liteinst_runtime::contract::run_without_input(command, &held) {
                Ok(run) => {
                    liteinst_runtime::retain_capture(&evidence, name, &run)?;
                    run
                }
                Err(error) => {
                    liteinst_runtime::retain_failure(&evidence, name, &error)?;
                    return Err(error.to_string());
                }
            };
            assert_eq!(run.end, liteinst_runtime::contract::CaptureEnd::Complete);
            assert!(!run.output_truncated && run.input.is_none());
            assert_eq!(run.status.and_then(|status| status.code()), Some(127));
            assert!(run.stdout.is_empty());
            assert_eq!(run.stderr, format!("reverie-liteinst initialization failed: legacy {name} runtime was built without LiteInst; enable the `liteinst` Cargo feature or use the in-guest FullTool installation route\n").as_bytes());
        }
        Ok(())
    })();
    let after = (|| -> Result<(), String> {
        qualified.receipt.artifact.verify()?;
        assert_eq!(
            liteinst_runtime::current_source(&evidence, "after")?,
            before
        );
        Ok(())
    })();
    match (result, after) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => {
            Err(format!("{error}; evidence {}", evidence.display()))
        }
        (Err(error), Err(after)) => Err(format!(
            "{error}; final guard: {after}; evidence {}",
            evidence.display()
        )),
    }
}

#[test]
#[cfg(feature = "liteinst")]
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
#[cfg(feature = "liteinst")]
fn compatibility_fork_reports_clone_only_from_parent() {
    assert_compatibility_fork_event(&[], libc::SYS_clone);
}

#[test]
#[cfg(feature = "liteinst")]
fn compatibility_raw_fork_reports_once_before_child() {
    assert_compatibility_fork_event(&["--raw-fork"], libc::SYS_fork);
}

#[test]
#[cfg(feature = "liteinst")]
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

/// Longest a guest in the vfork/clone3 refusal test may run. The probe takes
/// milliseconds. A regression that forwards one of the probed calls can leave
/// the guest, or a task it created, running forever; the test must then fail
/// rather than hang.
#[cfg(feature = "liteinst")]
const PROCESS_CREATION_PROBE_LIMIT: Duration = Duration::from_secs(30);

// https://github.com/rrnewton/reverie/issues/758: a raw vfork that
// strace/compatibility mode forwards from the dispatcher runs a child through
// the parent's suspended dispatcher frames, and a forwarded clone3 can do the
// same or start a child on a fresh stack with none of them. Both must be
// refused, and traced, before the kernel sees them. The refusal goes by
// syscall number, so it also covers a fork-shaped clone3, whose child would
// get its own copy of the frames.
//
// The guest makes the vfork call and seven clone3 calls through libc's
// syscall() site, which is already hooked when they arrive. It then makes one
// more vfork call and one more clone3 call, each from a site that has never
// run, so those two reach the refusal through the first SIGSYS trap. The trace
// counts separate a refusal in process_syscall from the SIGSYS handler's
// untraced fallback, which returns the same errno.
#[test]
#[cfg(feature = "liteinst")]
fn raw_vfork_and_clone3_are_refused_in_compatibility_and_strace_modes() {
    let guest = env!("CARGO_BIN_EXE_reverie-liteinst-fork-guest");
    let expected = format!(
        "unsafe process creation rejected: vfork={0} clone3={0} clone3-shared={0} \
         clone3-thread={0} clone3-stack={0} clone3-tls={0} clone3-flags={0} clone3-size={0} \
         fresh-site-vfork={0} fresh-site-clone3={0} parent-canaries=unchanged\n",
        libc::ENOTSUP
    );

    let (compatibility, events) = run_compat_guest_with_event_pipe_within(
        guest,
        &["--unsafe-process"],
        Some(PROCESS_CREATION_PROBE_LIMIT),
    );
    assert!(compatibility.status.success(), "{compatibility:?}");
    assert_eq!(
        compatibility.stdout,
        expected.as_bytes(),
        "{compatibility:?}"
    );
    let events = String::from_utf8(events).unwrap();
    let traced = |syscall: i64| {
        let suffix = format!(" syscall={syscall}");
        events
            .lines()
            .filter(|line| line.ends_with(&suffix))
            .count()
    };
    assert_eq!(traced(libc::SYS_vfork), 2, "{events}");
    assert_eq!(traced(libc::SYS_clone3), 8, "{events}");
    let pids: BTreeSet<_> = events
        .lines()
        .filter_map(|line| line.split_once(" pid=")?.1.split_once(" syscall="))
        .map(|(pid, _)| pid)
        .collect();
    assert_eq!(pids.len(), 1, "only the parent may emit events:\n{events}");

    let strace = output_within(
        &mut strace_guest_command(guest, &["--unsafe-process"]),
        PROCESS_CREATION_PROBE_LIMIT,
    );
    assert!(strace.status.success(), "{strace:?}");
    assert_eq!(strace.stdout, expected.as_bytes(), "{strace:?}");
    let stderr = String::from_utf8(strace.stderr).unwrap();
    let refused = |syscall: i64| {
        let prefix = format!("] syscall({syscall}, ");
        let suffix = format!(") = -{}", libc::ENOTSUP);
        stderr
            .lines()
            .filter(|line| line.contains(&prefix) && line.ends_with(&suffix))
            .count()
    };
    assert_eq!(refused(libc::SYS_vfork), 2, "{stderr}");
    assert_eq!(refused(libc::SYS_clone3), 8, "{stderr}");
    let pids: BTreeSet<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("[liteinst strace pid "))
        .filter_map(|line| line.split(']').next())
        .collect();
    assert_eq!(pids.len(), 1, "only the parent may emit records:\n{stderr}");
}

#[cfg(feature = "liteinst")]
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

    let events = String::from_utf8(events).unwrap();
    let fork_suffix = format!(" syscall={syscall}");
    let lines: Vec<_> = events.lines().collect();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.ends_with(&fork_suffix))
            .count(),
        1,
        "successful fork must have one parent-owned compatibility event:\n{events}"
    );
    let clone_position = lines
        .iter()
        .position(|line| line.ends_with(&fork_suffix))
        .unwrap();
    let parent_pid = lines[clone_position]
        .split_once(" pid=")
        .unwrap()
        .1
        .split_once(" syscall=")
        .unwrap()
        .0;
    let child_position = lines
        .iter()
        .position(|line| {
            line.split_once(" pid=")
                .and_then(|(_, record)| record.split_once(" syscall="))
                .is_some_and(|(pid, _)| pid != parent_pid)
        })
        .expect("child instrumentation event is missing");
    assert!(
        clone_position < child_position,
        "clone marker must precede child activity:\n{events}"
    );
    let pids: BTreeSet<_> = lines
        .iter()
        .filter_map(|line| line.split_once(" pid=")?.1.split_once(" syscall="))
        .map(|(pid, _)| pid)
        .collect();
    assert!(
        pids.len() >= 2,
        "child instrumentation events are missing: {pids:?}\n{events}"
    );
}

#[test]
#[cfg(feature = "liteinst")]
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

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared spoof-getpid built-in behavior test.
#[test]
fn spoof_getpid_builtin_mutates_getpid_result() {
    let output = run_builtin_guest(
        BuiltinTool::SpoofGetpid,
        env!("CARGO_BIN_EXE_reverie-liteinst-spoof-guest"),
        &[],
    );
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The shared built-in installs via reverie_inguest::install_builtin and
    // rewrites the trapped getpid result: the LiteInst trap path MUTATED a
    // syscall return value, not merely observed it.
    assert_eq!(output.stdout, format!("getpid={SPOOF_PID}\n").as_bytes());
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared passthrough built-in behavior test.
#[test]
fn passthrough_builtin_preserves_getpid_result() {
    let output = run_builtin_guest(
        BuiltinTool::Passthrough,
        env!("CARGO_BIN_EXE_reverie-liteinst-spoof-guest"),
        &[],
    );
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let pid: i64 = stdout
        .strip_prefix("getpid=")
        .and_then(|value| value.trim_end().parse().ok())
        .unwrap_or_else(|| panic!("unexpected guest output: {stdout:?}"));
    // The passthrough built-in leaves the real result intact; a real PID is a
    // small positive value and never the spoof sentinel.
    assert!(pid > 0, "expected a real pid, got {pid}");
    assert_ne!(pid, SPOOF_PID, "passthrough must not spoof getpid");
}
