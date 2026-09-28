/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! An already-failed tracer must not return and detach actors after kill refusal.
//! This is a native subprocess fixture, not a pure control or a successful run.

#![cfg(target_arch = "x86_64")]

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use std::time::Instant;

use reverie::BackendFailure;
use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::InjectedSyscallEvent;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use reverie_ptrace::testing::test_fn_with_config;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::Notify;

const CHILD_ENV: &str = "REVERIE_TEST_TERMINATION_REFUSAL_CHANNEL";
const PRIMARY: &str = "native fixture retained child then forced pidfd signal refusal";

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
enum Case {
    #[default]
    TerminationRefusal,
    RegistrationRefusal,
}

impl Case {
    fn selector(self) -> &'static str {
        match self {
            Self::TerminationRefusal => "failed_run_kill_refusal_keeps_tracees_in_exitkill_domain",
            Self::RegistrationRefusal => {
                "newborn_registration_failure_keeps_original_tracees_in_exitkill_domain"
            }
        }
    }

    fn exit_code(self) -> i32 {
        match self {
            Self::TerminationRefusal => 102,
            Self::RegistrationRefusal => 103,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct Config {
    channel: i32,
    case: Case,
}

#[derive(Default)]
struct Failure {
    failed: AtomicBool,
    changed: Notify,
}

#[reverie::global_tool]
impl GlobalTool for Failure {
    type Request = ();
    type Response = ();
    type Config = Config;

    async fn receive_rpc(&self, from: Pid, _request: ()) {
        self.report_backend_failure(BackendFailure {
            pid: from,
            tid: from,
            phase: PRIMARY,
        });
    }

    fn report_backend_failure(&self, event: BackendFailure) {
        if !self.failed.swap(true, Ordering::SeqCst) {
            eprintln!(
                "TEST_PRIMARY_BACKEND_FAILURE pid={} tid={} phase={}",
                event.pid, event.tid, event.phase
            );
            self.changed.notify_waiters();
        }
    }

    async fn wait_for_backend_failure(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.failed.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Refusal {
    config: Config,
}

#[reverie::tool]
impl Tool for Refusal {
    type GlobalState = Failure;
    type ThreadState = bool;

    fn new(_pid: Pid, config: &Config) -> Self {
        Self { config: *config }
    }

    fn subscriptions(_config: &Config) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::fork);
        subscriptions
    }

    fn observe_injected_syscalls(_config: &Config) -> bool {
        true
    }

    fn init_thread_state(&self, _child: Pid, parent: Option<(Pid, &bool)>) -> bool {
        parent.is_some()
    }

    fn on_injected_syscall_observed(
        &self,
        tid: Pid,
        _global: &Failure,
        _state: &mut bool,
        nr: Sysno,
        _args: SyscallArgs,
        event: InjectedSyscallEvent,
    ) {
        let InjectedSyscallEvent::ChildCreated(child) = event else {
            panic!("unexpected native result before the forced failure: {event:?}");
        };
        assert_eq!(nr, Sysno::fork);
        // This synchronous hook owns the original NewChild stop. Neither the
        // stopped creator nor its not-yet-resumed child can recycle these IDs.
        // Transfer actual PIDFD_THREAD descriptions to the outside test owner.
        send_original_tracees(self.config.channel, [tid.as_raw(), child.as_raw()]);
        if self.config.case == Case::RegistrationRefusal {
            // Event decoding has already captured the newborn token. Refuse
            // the actual notifier registration's next pidfd_open, after the
            // outside owner retained both original tasks. No guest is resumed.
            refuse_syscall_on_this_tracer_thread(libc::SYS_pidfd_open, libc::EMFILE);
        }
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        if *guest.thread_state() {
            assert_eq!(
                self.config.case,
                Case::TerminationRefusal,
                "registration refusal must precede child Tool startup"
            );
            refuse_syscall_on_this_tracer_thread(libc::SYS_pidfd_send_signal, libc::EPERM);
            // Send via the real backend failure contract, then suspend this
            // continuation. The biased run() failure branch must be the caller
            // that actually encounters EPERM; this is not an injected return.
            guest.send_rpc(()).await;
            return std::future::pending().await;
        }
        Ok(())
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let result = guest.inject(syscall).await;
        assert!(result.is_ok(), "native fork failed: {result:?}");
        // Keep the creator stopped even if its injection completes before the
        // child startup callback. Neither guest executes the sentinel _exit.
        std::future::pending().await
    }

    fn on_backend_thread_terminal(
        &self,
        _tid: Pid,
        _global: &Failure,
        _state: &mut bool,
        _status: reverie::ExitStatus,
    ) {
        panic!("UNEXPECTED_NATIVE_TERMINAL before fatal cleanup refusal");
    }

    async fn on_exit_thread<G: reverie::GlobalRPC<Failure>>(
        &self,
        _tid: Pid,
        _global: &G,
        _state: bool,
        _status: reverie::ExitStatus,
    ) -> Result<(), Error> {
        panic!("UNEXPECTED_TOOL_EXIT after unconfirmed termination");
    }
}

fn refuse_syscall_on_this_tracer_thread(nr: libc::c_long, errno: i32) {
    let mut filter = [
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        }, // LD W ABS syscall nr
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: nr as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x0005_0000 | errno as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x7fff_0000,
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
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &program) }, 0);
}

fn send_original_tracees(channel: i32, pids: [i32; 2]) {
    assert!(pids[0] > 0 && pids[1] > 0 && pids[0] != pids[1]);
    let files = pids.map(|pid| {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 128) } as i32;
        assert!(
            fd >= 0,
            "bind stopped task: {}",
            std::io::Error::last_os_error()
        );
        unsafe { OwnedFd::from_raw_fd(fd) }
    });
    let mut words = [1i32, pids[0], pids[1]];
    let mut iov = libc::iovec {
        iov_base: words.as_mut_ptr().cast(),
        iov_len: 12,
    };
    let mut control = [0usize; 4];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(8) } as usize;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(8) as usize;
        std::ptr::copy_nonoverlapping(
            [files[0].as_raw_fd(), files[1].as_raw_fd()].as_ptr(),
            libc::CMSG_DATA(header).cast::<i32>(),
            2,
        );
        assert_eq!(libc::sendmsg(channel, &message, libc::MSG_NOSIGNAL), 12);
    }
}

struct Tracees {
    pids: [i32; 2],
    files: [OwnedFd; 2],
    reaped: [bool; 2],
}

fn receive_original_tracees(channel: &UnixDatagram) -> Tracees {
    channel
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut words = [0i32; 3];
    let mut iov = libc::iovec {
        iov_base: words.as_mut_ptr().cast(),
        iov_len: 12,
    };
    let mut control = [0usize; 4];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(8) } as usize;
    let count = unsafe { libc::recvmsg(channel.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    assert_eq!(
        count,
        12,
        "receive original pidfds: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC), 0);
    assert_eq!(words[0], 1);
    let files = unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        assert!(!header.is_null());
        assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        assert_eq!((*header).cmsg_len, libc::CMSG_LEN(8) as usize);
        assert!(libc::CMSG_NXTHDR(&message, header).is_null());
        let descriptors = libc::CMSG_DATA(header).cast::<i32>();
        [
            OwnedFd::from_raw_fd(*descriptors),
            OwnedFd::from_raw_fd(*descriptors.add(1)),
        ]
    };
    assert!(words[1] > 0 && words[2] > 0 && words[1] != words[2]);
    Tracees {
        pids: [words[1], words[2]],
        files,
        reaped: [false; 2],
    }
}

fn terminal(file: &OwnedFd, reap: bool) -> std::io::Result<Option<libc::siginfo_t>> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | if reap { 0 } else { libc::WNOWAIT };
    let result = unsafe { libc::waitid(libc::P_PIDFD, file.as_raw_fd() as u32, &mut info, flags) };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((unsafe { info.si_pid() } != 0).then_some(info))
}

struct Owner {
    tracer: Child,
    status: Option<ExitStatus>,
    tracees: Option<Tracees>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        // Failure cleanup is separate from the positive oracle. It never
        // upgrades a timeout, missing receipt, or wrong terminal status.
        let deadline = Instant::now() + Duration::from_secs(5);
        if self.status.is_none() {
            let _ = self.tracer.kill(); // unreaped direct child, not a reopened PID
        }
        if let Some(tracees) = self.tracees.as_ref() {
            for (file, reaped) in tracees.files.iter().zip(tracees.reaped) {
                if !reaped {
                    unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            file.as_raw_fd(),
                            libc::SIGKILL,
                            std::ptr::null::<libc::siginfo_t>(),
                            0,
                        );
                    }
                }
            }
        }
        while Instant::now() < deadline {
            if self.status.is_none() {
                self.status = self.tracer.try_wait().ok().flatten();
            }
            if let Some(tracees) = self.tracees.as_mut() {
                for index in 0..2 {
                    if !tracees.reaped[index] {
                        tracees.reaped[index] = terminal(&tracees.files[index], true)
                            .ok()
                            .flatten()
                            .is_some();
                    }
                }
            }
            if self.status.is_some() && self.tracees.as_ref().is_some_and(|t| t.reaped == [true; 2])
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        eprintln!(
            "TEST_CLEANUP_UNCONFIRMED tracer={:?} tracees={:?}",
            self.status,
            self.tracees.as_ref().map(|t| t.reaped)
        );
    }
}

struct Subreaper(i32);
impl Drop for Subreaper {
    fn drop(&mut self) {
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.0, 0, 0, 0) },
            0
        );
    }
}

#[test]
fn failed_run_kill_refusal_keeps_tracees_in_exitkill_domain() {
    run_case(Case::TerminationRefusal);
}

#[test]
fn newborn_registration_failure_keeps_original_tracees_in_exitkill_domain() {
    run_case(Case::RegistrationRefusal);
}

fn run_case(case: Case) {
    if let Ok(channel) = std::env::var(CHILD_ENV) {
        let channel: i32 = channel.parse().unwrap();
        let result = test_fn_with_config::<Refusal, _>(
            || unsafe {
                libc::syscall(libc::SYS_fork);
                libc::_exit(97); // Neither guest may execute after the failed run.
            },
            Config { channel, case },
            true,
        );
        panic!(
            "failed tracer returned instead of preserving fatal ownership: {}",
            if result.is_ok() { "Ok" } else { "Err" }
        );
    }
    static OUTSIDE_OWNER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // PR_SET_CHILD_SUBREAPER and the final P_ALL census are process-wide.
    // Own the complete adoption/reap/restore lifetime, not one operation.
    let _outside_owner = OUTSIDE_OWNER.lock().unwrap();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(10);
    let mut previous = 0;
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous, 0, 0, 0) },
        0
    );
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let _subreaper = Subreaper(previous);
    let (receiver, sender) = UnixDatagram::pair().unwrap();
    let channel = sender.as_raw_fd();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        case.selector(),
        "--quiet",
        "--nocapture",
        "--test-threads=1",
    ]);
    command.env(CHILD_ENV, channel.to_string());
    command.stdout(Stdio::inherit()).stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(channel, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut owner = Owner {
        tracer: command.spawn().unwrap(),
        status: None,
        tracees: None,
    };
    drop(sender);
    let mut stderr = owner.tracer.stderr.take().unwrap();
    let (done, output) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stderr.by_ref().take(65_537).read_to_end(&mut bytes);
        let _ = done.send((read, bytes));
    });
    owner.tracees = Some(receive_original_tracees(&receiver));
    while owner.status.is_none() && Instant::now() < deadline {
        owner.status = owner.tracer.try_wait().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        owner.status.and_then(|status| status.code()),
        Some(case.exit_code()),
        "exact fatal refusal status for this case"
    );
    let tracees = owner.tracees.as_mut().unwrap();
    for index in 0..2 {
        let info = loop {
            assert!(
                Instant::now() < deadline,
                "original tracee {} did not terminate",
                tracees.pids[index]
            );
            match terminal(&tracees.files[index], false) {
                Ok(Some(info)) => break info,
                Ok(None) => {}
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {} // pending reparenting
                Err(error) => panic!("original pidfd wait: {error}"),
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(unsafe { info.si_pid() }, tracees.pids[index]);
        assert_eq!(info.si_code, libc::CLD_KILLED);
        assert_eq!(unsafe { info.si_status() }, libc::SIGKILL);
        let reaped = terminal(&tracees.files[index], true).unwrap().unwrap();
        assert_eq!(unsafe { reaped.si_pid() }, tracees.pids[index]);
        tracees.reaped[index] = true;
        println!(
            "original-tracee pid={} pidfd-terminal=SIGKILL reaped=true",
            tracees.pids[index]
        );
    }
    let (read, bytes) = output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap();
    read.unwrap();
    assert!(
        bytes.len() <= 65_536,
        "inner stderr exceeded retained bound"
    );
    reader.join().unwrap();
    let stderr = String::from_utf8(bytes).unwrap();
    eprint!("{stderr}");
    let primary: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("TEST_PRIMARY_BACKEND_FAILURE "))
        .collect();
    match case {
        Case::TerminationRefusal => {
            assert_eq!(
                primary,
                vec![format!(
                    "TEST_PRIMARY_BACKEND_FAILURE pid={} tid={} phase={PRIMARY}",
                    tracees.pids[1], tracees.pids[1],
                )]
            );
            let markers: Vec<_> = stderr
                .lines()
                .filter(|line| line.starts_with("HERMIT_TASK_TERMINATION_FAILED "))
                .collect();
            assert_eq!(markers.len(), 1);
            assert!(tracees.pids.iter().any(|pid| markers[0] == format!("HERMIT_TASK_TERMINATION_FAILED tid={pid} exit=102 errno=1 backend_failure=acknowledged cleanup=unconfirmed")));
            assert!(!stderr.contains("HERMIT_CHILD_CUSTODY_FAILED"));
        }
        Case::RegistrationRefusal => {
            assert_eq!(
                primary,
                vec![format!(
                    "TEST_PRIMARY_BACKEND_FAILURE pid={} tid={} phase=register_newborn_wait",
                    tracees.pids[0], tracees.pids[0],
                )]
            );
            let markers: Vec<_> = stderr
                .lines()
                .filter(|line| line.starts_with("HERMIT_CHILD_CUSTODY_FAILED "))
                .collect();
            assert_eq!(
                markers,
                vec![format!(
                    "HERMIT_CHILD_CUSTODY_FAILED creator={} child={} exit=103 phase=register_newborn_wait error=errno=24 cleanup=unconfirmed",
                    tracees.pids[0], tracees.pids[1],
                )]
            );
            assert!(!stderr.contains("HERMIT_TASK_TERMINATION_FAILED"));
        }
    }
    assert!(!stderr.contains("UNEXPECTED_NATIVE_TERMINAL"));
    assert!(!stderr.contains("UNEXPECTED_TOOL_EXIT"));
    let mut remaining: libc::siginfo_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            libc::waitid(
                libc::P_ALL,
                0,
                &mut remaining,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert!(
        Instant::now() < deadline,
        "terminal proof exceeded the fixed fixture deadline"
    );
    println!(
        "failed-tracer exit={} original-tracees=2 reaped=2 ECHILD=true elapsed={:?}",
        case.exit_code(),
        started.elapsed()
    );
}
