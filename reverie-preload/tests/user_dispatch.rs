use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

fn probe(case: &str) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_user_dispatch_probe"))
        .arg(case)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("SUD probe {case} timed out: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn ordinary_libc_signal_returns_under_both_controllers() {
    for case in ["ordinary-signal-seccomp", "ordinary-signal-sud"] {
        let output = probe(case);
        assert!(output.status.success(), "{case}: {output:?}");
        assert_eq!(output.stdout, b"sud-probe-ok\n", "{case}: {output:?}");
    }
}

#[test]
fn intercepted_signal_delivery_returns_under_both_controllers() {
    for case in ["mediated-signal-seccomp", "mediated-signal-sud"] {
        let output = probe(case);
        assert!(output.status.success(), "{case}: {output:?}");
        assert_eq!(output.stdout, b"sud-probe-ok\n", "{case}: {output:?}");
    }
}

#[test]
fn sud_signal_handler_can_make_nested_guest_syscalls() {
    let output = probe("nested-signal-sud");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"sud-probe-ok\n", "{output:?}");
}

#[test]
fn sud_forwarded_blocking_read_remains_interruptible() {
    let output = probe("interrupted-read");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"sud-probe-ok\n", "{output:?}");
}

#[test]
fn kernel_backed_user_dispatch() {
    for case in [
        "traps",
        "six-args",
        "failed-exec",
        "fork-rearm",
        "thread-rearm",
        "disable-rearm",
        "blocked-mask",
        "unavailable",
        "reconfigure-refused",
        "prctl-forwarded",
        "mask-change-refused",
        "rearm-before-install",
        "exec-refused",
    ] {
        let output = probe(case);
        assert!(output.status.success(), "{case}: {output:?}");
        assert_eq!(output.stdout, b"sud-probe-ok\n", "{case}: {output:?}");
    }
}

#[test]
fn invalid_selector_and_foreign_sigsys_fail_closed() {
    for (case, signal, code) in [
        ("bad-selector", Some(libc::SIGSYS), None),
        ("unreadable-selector", Some(libc::SIGSEGV), None),
        ("foreign-sigsys", None, Some(126)),
        ("ptrace-denied", Some(libc::SIGSYS), None),
    ] {
        let output = probe(case);
        assert_eq!(output.status.signal(), signal, "{case}: {output:?}");
        assert_eq!(output.status.code(), code, "{case}: {output:?}");
    }
}
