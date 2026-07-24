use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

fn run_guest(program: &str, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_reverie-liteinst-strace"))
        .env("REVERIE_LITEINST_PRELOAD", preload_path())
        .arg(program)
        .args(arguments)
        .output()
        .unwrap()
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
