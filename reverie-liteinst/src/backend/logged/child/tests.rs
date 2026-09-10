use std::collections::VecDeque;
use std::os::linux::process::ChildExt as _;
use std::os::linux::process::CommandExt as _;

use reverie_rpc_transport::guest_log::Options;
use reverie_rpc_transport::guest_log::retained_log;

use super::*;

#[derive(Default)]
struct Scripted {
    wait_errors: VecDeque<i32>,
    signal_errors: VecDeque<i32>,
    waits: usize,
    signals: usize,
}

impl Operations for Scripted {
    fn wait(&mut self, pidfd: &PidFd, blocking: bool) -> io::Result<Option<ExitStatus>> {
        self.waits += 1;
        match self.wait_errors.pop_front() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Kernel.wait(pidfd, blocking),
        }
    }

    fn signal(&mut self, pidfd: &PidFd) -> io::Result<()> {
        self.signals += 1;
        match self.signal_errors.pop_front() {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => Kernel.signal(pidfd),
        }
    }
}

struct Launch;

#[test]
fn pidfd_acquisition_failure_refuses_before_guest_execution() {
    const SELECTOR: &str = "REVERIE_PIDFD_ACQUISITION_FAILURE";
    if std::env::var_os(SELECTOR).is_none() {
        for operation in ["pidfd", "receive"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "backend::logged::child::tests::pidfd_acquisition_failure_refuses_before_guest_execution",
                    "--exact",
                    "--test-threads=1",
                ])
                .env(SELECTOR, operation)
                .output()
                .unwrap();
            assert!(output.status.success(), "{operation}: {output:?}");
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("executed");
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "printf executed > \"$1\"", "pidfd-control"]);
    command.arg(&marker);
    let observer = RunObserver::new(StdioMode::Inherited);
    let (_sink, handle) = retained_log(Options::bounded(1024));
    let operation = match std::env::var(SELECTOR).unwrap().as_str() {
        "pidfd" => libc::SYS_pidfd_open,
        "receive" => libc::SYS_recvmsg,
        _ => panic!("unknown pidfd acquisition control"),
    };
    let mut filter = [
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: operation as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        0
    );
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) },
        0
    );
    let mut launch = Launch;
    let result = Child::spawn(command, &mut launch, observer.clone(), handle, Kernel);
    let error = match result {
        Ok(child) => {
            let pid = child.native.id() as libc::pid_t;
            drop(child);
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            None
        }
        Err(error) => Some(error),
    };
    assert_eq!(
        error.and_then(|error| error.raw_os_error()),
        Some(libc::EPERM)
    );
    assert!(!marker.exists(), "guest executed without pidfd ownership");
    assert!(observer.try_snapshot().unwrap().pid.is_none());
}

impl owned::LaunchLifetime for Launch {
    fn before_spawn(&mut self, _: &mut std::process::Command) -> Result<(), Error> {
        Ok(())
    }
    fn spawning(&mut self) {}
    fn spawn_failed(&mut self) {}
    fn reaped(&mut self) {}
}

fn spawn(operations: Scripted) -> (Child<'static, Scripted>, RunObserver, LogHandle) {
    let observer = RunObserver::new(StdioMode::Inherited);
    let (_sink, handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/true");
    unsafe {
        command.pre_exec(|| Ok(()));
    }
    let launch = Box::leak(Box::new(Launch));
    let child = Child::spawn(
        command,
        launch,
        observer.clone(),
        handle.clone(),
        operations,
    )
    .unwrap();
    (child, observer, handle)
}

#[test]
fn external_reap_before_error_cleanup_never_signals_or_invents_status() {
    let (mut child, observer, handle) = spawn(Scripted::default());
    assert!(
        Kernel
            .wait(child.pidfd().unwrap(), true)
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(
        child.poll_status(false).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    child.start_kill().unwrap();
    assert_eq!(child.operations.signals, 0);
    assert_eq!(
        child.poll_status(true).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert_eq!(child.operations.waits, 1);
    drop(child);
    let state = observer.try_snapshot().unwrap();
    assert!(state.reaped);
    assert!(state.wait_status.is_none());
    assert!(state.wait_error.is_some());
    assert!(handle.snapshot().root_reaped);
}

#[test]
fn external_reap_between_wait_and_signal_uses_only_native_pidfd() {
    let (mut child, observer, handle) = spawn(Scripted {
        wait_errors: VecDeque::from([libc::EIO]),
        ..Scripted::default()
    });
    assert_eq!(
        child.poll_status(false).unwrap_err().raw_os_error(),
        Some(libc::EIO)
    );
    assert!(
        Kernel
            .wait(child.pidfd().unwrap(), true)
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(
        child.start_kill().unwrap_err().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(
        child.poll_status(true).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert_eq!(child.operations.signals, 1);
    drop(child);
    assert!(observer.try_snapshot().unwrap().wait_status.is_none());
    assert!(handle.snapshot().root_reaped);
}

#[test]
fn interrupted_wait_retries_and_reaped_status_is_cached_without_signaling() {
    let (mut child, observer, _) = spawn(Scripted {
        wait_errors: VecDeque::from([libc::EINTR, libc::EINTR]),
        ..Scripted::default()
    });
    let status = child.poll_status(true).unwrap().unwrap();
    assert!(status.success());
    assert_eq!(child.operations.waits, 3);
    assert_eq!(child.poll_status(false).unwrap(), Some(status));
    child.start_kill().unwrap();
    assert_eq!(child.operations.signals, 0);
    drop(child);
    assert_eq!(observer.try_snapshot().unwrap().wait_status, Some(status));
}

#[test]
fn signal_errors_do_not_skip_wait_or_claim_reaping() {
    for errno in [libc::EPERM, libc::ESRCH] {
        let (mut child, observer, handle) = spawn(Scripted {
            signal_errors: VecDeque::from([libc::EINTR, errno]),
            ..Scripted::default()
        });
        assert_eq!(child.start_kill().unwrap_err().raw_os_error(), Some(errno));
        assert_eq!(child.operations.signals, 2);
        assert!(!observer.try_snapshot().unwrap().reaped);
        assert!(child.poll_status(true).unwrap().unwrap().success());
        drop(child);
        assert!(handle.snapshot().root_reaped);
    }
}

#[test]
fn destructor_records_signal_failure_but_still_reaps() {
    let (child, observer, handle) = spawn(Scripted {
        signal_errors: VecDeque::from([libc::EPERM]),
        ..Scripted::default()
    });
    drop(child);
    assert!(
        observer
            .try_snapshot()
            .unwrap()
            .wait_status
            .unwrap()
            .success()
    );
    assert!(handle.snapshot().root_reaped);
    assert!(
        handle
            .snapshot()
            .issues
            .iter()
            .any(|issue| issue.kind == IssueKind::Cleanup)
    );
}

#[test]
fn unrecoverable_wait_error_retains_unconfirmed_state() {
    let (child, observer, handle) = spawn(Scripted {
        wait_errors: VecDeque::from([libc::EIO]),
        ..Scripted::default()
    });
    let identity = child.pidfd().unwrap().as_fd().try_clone_to_owned().unwrap();
    drop(child);
    assert!(!observer.try_snapshot().unwrap().reaped);
    assert!(!handle.snapshot().root_reaped);
    assert!(
        handle
            .snapshot()
            .issues
            .iter()
            .any(|issue| issue.kind == IssueKind::Cleanup && issue.message.contains("unconfirmed"))
    );
    assert!(Kernel.wait(&PidFd::from(identity), true).unwrap().is_some());
}

#[test]
fn missing_native_pidfd_never_uses_numeric_signal_or_wait_fallback() {
    let observer = RunObserver::new(StdioMode::Inherited);
    let (_sink, handle) = retained_log(Options::bounded(1024));
    let mut command = std::process::Command::new("/bin/true");
    command.create_pidfd(false);
    unsafe {
        command.pre_exec(|| Ok(()));
    }
    let native = command.spawn().unwrap();
    assert!(native.pidfd().is_err());
    let pid = native.id() as libc::pid_t;
    observer.spawned(Some(native.id()));
    let mut child = Child {
        native,
        pidfd: None,
        launch: None,
        observer: observer.clone(),
        handle: handle.clone(),
        state: State::Pending,
        readiness: None,
        stdout: None,
        stderr: None,
        operations: Scripted::default(),
    };
    assert!(child.adapt().is_err());
    assert!(child.start_kill().is_err());
    assert!(child.poll_status(true).is_err());
    assert_eq!(child.operations.signals, 0);
    assert_eq!(child.operations.waits, 0);
    drop(child);
    assert!(!observer.try_snapshot().unwrap().reaped);
    assert!(!handle.snapshot().root_reaped);
    assert!(
        handle
            .snapshot()
            .issues
            .iter()
            .any(|issue| issue.kind == IssueKind::Cleanup && issue.message.contains("unconfirmed"))
    );
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(ExitStatus::from_raw(status).success());
}
