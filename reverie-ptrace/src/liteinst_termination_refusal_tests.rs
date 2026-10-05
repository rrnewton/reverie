/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Exit 102 (`HERMIT_TASK_TERMINATION_FAILED`) on the dynamic LiteInst route.
//!
//! The ordinary route keeps a refused group cleanup pending and resumable, so
//! it never reaches exit 102. The only remaining 102 site is the dynamic
//! LiteInst task's failed-run join, `wait_for_failed_run_terminal`: after a
//! published backend failure it must terminate its exact bound task before
//! waiting for that task's EXIT, and no session owner is left to retry a
//! refused kill there. This native subprocess fixture forces that refusal.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::mpsc;

use reverie::BackendFailure;
use reverie::Guest;
use reverie::syscalls::Syscall;
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::Notify;

use super::*;

const NAME: &str = "tracer::liteinst_termination_refusal_tests::liteinst_failed_run_kill_refusal_keeps_tracees_in_exitkill_domain";
const TRACER_ENV: &str = "REVERIE_LITEINST_TERMINATION_REFUSAL_CHANNEL";
const PRIMARY: &str = "LiteInst fixture child started then forced pidfd signal refusal";

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct Config {
    channel: i32,
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
    /// The creator of a native child, recorded at its thread-state boundary.
    type ThreadState = Option<Pid>;

    fn new(_pid: Pid, config: &Config) -> Self {
        Self { config: *config }
    }

    fn subscriptions(_config: &Config) -> Subscription {
        let mut subscriptions = Subscription::none();
        subscriptions.syscall(Sysno::fork);
        subscriptions.syscall(Sysno::vfork);
        subscriptions.syscall(Sysno::clone);
        subscriptions.syscall(Sysno::clone3);
        subscriptions
    }

    fn init_thread_state(&self, _child: Pid, parent: Option<(Pid, &Option<Pid>)>) -> Option<Pid> {
        parent.map(|(parent, _)| parent)
    }

    async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
        if let Some(creator) = *guest.thread_state() {
            // Both tasks are held in ptrace stops by this tracer: the creator
            // parks in handle_syscall_event and this child has not started.
            // Neither ID can be recycled while the descriptions are bound.
            send_original_tracees(
                self.config.channel,
                [creator.as_raw(), guest.tid().as_raw()],
            );
            refuse_pidfd_send_signal_on_this_tracer_thread();
            // Publish the failure through the real backend contract, then
            // suspend. The LiteInst run's biased failure branch, not this
            // callback, must be the caller that actually encounters EPERM.
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
        // Keep the creator stopped. Neither guest executes the sentinel _exit.
        std::future::pending().await
    }

    fn on_backend_thread_terminal(
        &self,
        _tid: Pid,
        _global: &Failure,
        _state: &mut Option<Pid>,
        _status: ExitStatus,
    ) {
        panic!("UNEXPECTED_NATIVE_TERMINAL before fatal cleanup refusal");
    }

    async fn on_exit_thread<G: reverie::GlobalRPC<Failure>>(
        &self,
        _tid: Pid,
        _global: &G,
        _state: Option<Pid>,
        _status: ExitStatus,
    ) -> Result<(), Error> {
        panic!("UNEXPECTED_TOOL_EXIT after unconfirmed termination");
    }
}

fn refuse_pidfd_send_signal_on_this_tracer_thread() {
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
            k: libc::SYS_pidfd_send_signal as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x0005_0000 | libc::EPERM as u32,
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

const PIDFD_THREAD: libc::c_long = libc::O_EXCL as libc::c_long;

fn send_original_tracees(channel: i32, pids: [i32; 2]) {
    assert!(pids[0] > 0 && pids[1] > 0 && pids[0] != pids[1]);
    let files = pids.map(|pid| {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, PIDFD_THREAD) } as i32;
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
        .set_read_timeout(Some(Duration::from_secs(5)))
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
    tracer: std::process::Child,
    status: Option<std::process::ExitStatus>,
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

/// The guest program: one native fork (the shell's `clone` for an external
/// command), then a sentinel exit that neither the creator nor the child may
/// reach after the failed run. The shell is single-threaded, so the two
/// tracees are the whole guest.
fn guest_command() -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "/bin/true; exit 97"]);
    command
}

fn run_failed_tracer(channel: i32) -> ! {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(async move {
        let tracer = TracerBuilder::<Refusal>::new(guest_command())
            .config(Config { channel })
            .liteinst_runtime(PathBuf::from("/not/used.so"), 1, 2, 3, 4, 5)
            .activate_liteinst_without_handshake_for_test()
            .spawn()
            .await?;
        tracer.wait().await
    });
    panic!(
        "failed LiteInst tracer returned instead of preserving fatal ownership: {}",
        match &result {
            Ok(_) => "Ok".to_owned(),
            Err(error) => format!("Err({error:?})"),
        }
    );
}

#[test]
fn liteinst_failed_run_kill_refusal_keeps_tracees_in_exitkill_domain() {
    if let Ok(channel) = std::env::var(TRACER_ENV) {
        run_failed_tracer(channel.parse().unwrap());
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
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        NAME,
        "--quiet",
        "--nocapture",
        "--test-threads=1",
    ]);
    command.env(TRACER_ENV, channel.to_string());
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
        let read = stderr.by_ref().take(1_048_577).read_to_end(&mut bytes);
        let _ = done.send((read, bytes));
    });
    owner.tracees = Some(receive_original_tracees(&receiver));
    while owner.status.is_none() && Instant::now() < deadline {
        owner.status = owner.tracer.try_wait().unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    let status = owner.status.and_then(|status| status.code());
    let tracees = owner.tracees.as_mut().unwrap();
    for index in 0..2 {
        let info = loop {
            assert!(
                Instant::now() < deadline,
                "original tracee {} did not terminate (tracer status {status:?})",
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
        bytes.len() <= 1_048_576,
        "inner stderr exceeded retained bound"
    );
    reader.join().unwrap();
    let stderr = String::from_utf8_lossy(&bytes).into_owned();
    eprint!("{stderr}");
    assert_eq!(status, Some(102), "exact fatal refusal status");
    let primary: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("TEST_PRIMARY_BACKEND_FAILURE "))
        .collect();
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
        "failed-liteinst-tracer exit=102 original-tracees=2 reaped=2 ECHILD=true elapsed={:?}",
        started.elapsed()
    );
}
