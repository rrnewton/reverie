/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! One default Cargo case runs the complete native C aggregate, without KVM or
//! Rust FFI. The embedded inputs cannot drift after Cargo builds this test.
//! Current qualification requires readable securityfs with `capability,bpf,ima`
//! and the measured non-PIE x86-64 `read@plt` layout; unknown provider profiles
//! and unsupported dispatch layouts fail. Independent ELF association remains
//! an external qualification check; this case requires the native assertions.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::fs::DirBuilder;
use std::fs::OpenOptions;
use std::fs::{self};
use std::io::Write;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const HELPER: &[u8] = include_bytes!("../src/terminal_read.c");
const HEADER: &[u8] = include_bytes!("../src/terminal_read.h");
const PROTOCOL: &[u8] = include_bytes!("terminal_read_protocol.c");
const STREAM_CAP: usize = 2 * 1024 * 1024;
const WALL_LIMIT: Duration = Duration::from_secs(30);
// The payload is killed at its deadline. Retirement has its own finite grace;
// failure to prove retirement fails the test and retains its private directory.
const RETIRE_LIMIT: Duration = Duration::from_secs(5);
const C_FLAGS: &[&str] = &[
    "-DRVK_READ_TEST",
    "-std=c11",
    "-pthread",
    "-fexceptions",
    "-Wall",
    "-Wextra",
    "-Werror",
    "-UNDEBUG",
];
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct PrivateTree {
    path: PathBuf,
    retain: Arc<AtomicBool>,
}

impl PrivateTree {
    fn new() -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for _ in 0..32 {
            let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "reverie-terminal-read-{}-{stamp}-{serial}",
                std::process::id()
            ));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        retain: Arc::new(AtomicBool::new(false)),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(
            "could not exclusively create protocol temporary directory",
        ))
    }

    fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path.join(name))?;
        file.write_all(bytes)
    }

    fn remove(&mut self) -> io::Result<()> {
        if self.retain.load(Ordering::Relaxed) {
            return Err(io::Error::other(format!(
                "owned group retirement unproved; retained {}",
                self.path.display()
            )));
        }
        if !self.path.as_os_str().is_empty() {
            fs::remove_dir_all(&self.path)?;
            self.path.clear();
        }
        Ok(())
    }
}

impl Drop for PrivateTree {
    fn drop(&mut self) {
        if let Err(error) = self.remove() {
            let _ = writeln!(
                io::stderr(),
                "protocol temporary-directory cleanup: {error}"
            );
        }
    }
}

// This integration-test executable contains exactly one test. Subreaping is
// confined to it and restored on every return/unwind; we never wait for an
// unrelated child. It makes the deliberately forked native children observable
// even when their immediate parent dies during a wrapper failure.
struct Subreaper {
    saved: libc::c_int,
    restored: bool,
}

impl Subreaper {
    fn new() -> io::Result<Self> {
        let mut old = 0;
        // SAFETY: old is writable; these prctl operations take the documented
        // scalar/pointer arguments and affect only this test process.
        unsafe {
            if libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut old) != 0
                || libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            saved: old,
            restored: false,
        })
    }

    fn restore(&mut self) -> io::Result<()> {
        if !self.restored {
            // SAFETY: restore this process's saved scalar prctl setting.
            if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.saved) } != 0 {
                return Err(io::Error::last_os_error());
            }
            self.restored = true;
        }
        Ok(())
    }
}

impl Drop for Subreaper {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            let _ = writeln!(
                io::stderr(),
                "restore child subreaper during unwind: {error}"
            );
        }
    }
}

struct Capture {
    fd: Option<OwnedFd>,
    nonblocking: bool,
    bytes: Vec<u8>,
    overflow: bool,
}

impl Capture {
    fn new(fd: Option<OwnedFd>) -> Self {
        Self {
            fd,
            nonblocking: false,
            bytes: Vec::new(),
            overflow: false,
        }
    }

    fn configure(&mut self) -> io::Result<()> {
        let fd = self
            .fd
            .as_ref()
            .ok_or_else(|| io::Error::other("missing child pipe"))?
            .as_raw_fd();
        // SAFETY: fd remains owned by this capture. Preserve its existing flags.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        self.nonblocking = true;
        Ok(())
    }

    fn drain(&mut self) -> io::Result<()> {
        if !self.nonblocking {
            return Ok(());
        }
        let Some(fd) = self.fd.as_ref().map(AsRawFd::as_raw_fd) else {
            return Ok(());
        };
        // Limit work per stream per iteration so a writer cannot starve the
        // other pipe, wait observation, or deadline check.
        for _ in 0..16 {
            let mut buffer = [0_u8; 8192];
            // SAFETY: buffer is writable and fd is an owned nonblocking pipe.
            let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
            if count == 0 {
                self.fd.take();
                break;
            }
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let count = count as usize;
            let retained = count.min(STREAM_CAP - self.bytes.len());
            self.bytes.extend_from_slice(&buffer[..retained]);
            if retained != count && !self.overflow {
                self.overflow = true;
                return Err(io::Error::other(
                    "child stream exceeded 2 MiB; transcript is incomplete",
                ));
            }
        }
        Ok(())
    }
}

struct StageOwner {
    child: Option<Child>,
    pid: libc::pid_t,
    stdout: Capture,
    stderr: Capture,
    retain: Arc<AtomicBool>,
    status: Option<ExitStatus>,
    descendants: Vec<(libc::pid_t, u64, libc::c_int)>,
    retired: bool,
    signaling_allowed: bool,
    cleanup_returned: bool,
}

fn process_start(pid: libc::pid_t) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("malformed process stat"))?
        .1;
    let start: u64 = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| io::Error::other("missing process start time"))?
        .parse()
        .map_err(io::Error::other)?;
    if start == 0 {
        return Err(io::Error::other("zero process start time"));
    }
    Ok(start)
}

impl StageOwner {
    fn spawn(command: &mut Command, tree: &PrivateTree) -> io::Result<Self> {
        command
            .current_dir(&tree.path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .env("TMPDIR", tree.path.join("tmp"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: the post-fork closure uses only async-signal-safe syscalls and
        // constructs an errno error on failure. No allocation/locks are used.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                for (resource, limit) in [
                    (libc::RLIMIT_CPU, 30),
                    (libc::RLIMIT_AS, 1024 * 1024 * 1024),
                    (libc::RLIMIT_CORE, 0),
                ] {
                    let limits = libc::rlimit {
                        rlim_cur: limit,
                        rlim_max: limit,
                    };
                    if libc::setrlimit(resource, &limits) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let pid = child.id() as libc::pid_t;
        let stdout = Capture::new(child.stdout.take().map(Into::into));
        let stderr = Capture::new(child.stderr.take().map(Into::into));
        Ok(Self {
            child: Some(child),
            pid,
            stdout,
            stderr,
            retain: tree.retain.clone(),
            status: None,
            descendants: Vec::new(),
            retired: false,
            signaling_allowed: true,
            cleanup_returned: false,
        })
    }

    fn exited_without_reaping(&self) -> io::Result<bool> {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: info is initialized/writable. WNOWAIT is essential: retaining
        // this exact child prevents PID/PGID reuse until all signaling ends.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: waitid succeeded and info was zero initialized for WNOHANG.
        Ok(unsafe { info.assume_init().si_pid() } != 0)
    }

    fn drain(&mut self) -> io::Result<()> {
        let left = self.stdout.drain();
        let right = self.stderr.drain();
        left.and(right)
    }

    fn poll_pipes(&self) -> io::Result<()> {
        let mut descriptors = [libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        }; 2];
        for (descriptor, capture) in descriptors.iter_mut().zip([&self.stdout, &self.stderr]) {
            if let Some(fd) = &capture.fd {
                descriptor.fd = fd.as_raw_fd();
            }
        }
        // One nonblocking poll loop drains both streams; no reader thread can
        // remain stuck on a pipe held open by a grandchild.
        // SAFETY: descriptors is a writable array of the stated size.
        if unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 10) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }

    fn retire(&mut self) -> Vec<String> {
        if self.retired {
            return Vec::new();
        }
        let mut errors = Vec::new();
        if !self.signaling_allowed {
            self.retain.store(true, Ordering::Relaxed);
            self.cleanup_returned = true;
            return vec![
                "leader ownership was relinquished; refusing numeric PID/PGID signaling".into(),
            ];
        }
        let deadline = Instant::now() + RETIRE_LIMIT;
        let authenticated = match self.exited_without_reaping() {
            Ok(_) => true,
            Err(error) => {
                errors.push(format!(
                    "authenticate unreaped leader {}: {error}",
                    self.pid
                ));
                false
            }
        };
        if authenticated {
            // SAFETY: the unreaped leader still reserves this PGID. This is the
            // one group signal; irrevocably disarm before consuming ANY status.
            let result = unsafe { libc::kill(-self.pid, libc::SIGKILL) };
            self.signaling_allowed = false;
            if result != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                errors.push(format!(
                    "kill owned group {}: {}",
                    self.pid,
                    io::Error::last_os_error()
                ));
            }
        } else {
            self.signaling_allowed = false;
        }
        // /proc/TID/children is optional (CONFIG_PROC_CHILDREN). Kernel waits
        // include adopted children across our threads; __WALL also includes
        // non-SIGCHLD clone children. No task scan or waitpid(-1) is needed.
        // The leader may be reaped first now that all signaling is disarmed.
        // With trusted group-preserving descendants, every remaining subtree
        // has a direct child here: reparenting precedes exit notification.
        // Thus only ECHILD, after the leader is consumed, proves retirement.
        // Stages are sequential; unrelated external PGID reuse is not waitable.
        while authenticated {
            if Instant::now() >= deadline {
                errors.push("owned group retirement exceeded 5s grace".into());
                break;
            }
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            // SAFETY: initialized output; WNOWAIT pins each returned child for
            // generation observation before its exact-PID consuming wait.
            let result = unsafe {
                libc::waitid(
                    libc::P_PGID,
                    self.pid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT | libc::__WALL,
                )
            };
            if result != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) && self.status.is_some() {
                    self.retired = true;
                } else {
                    errors.push(format!(
                        "wait owned group {} (leader consumed={}): {error}",
                        self.pid,
                        self.status.is_some()
                    ));
                }
                break;
            }
            // SAFETY: successful waitid initialized this zeroed siginfo.
            let pid = unsafe { info.assume_init().si_pid() };
            if pid == self.pid {
                if self.status.is_some() {
                    errors.push("owned leader was reported twice".into());
                    break;
                }
                match self.child.as_mut().unwrap().wait() {
                    Ok(status) => self.status = Some(status),
                    Err(error) => {
                        errors.push(format!("reap owned leader: {error}"));
                        break;
                    }
                }
            } else if pid != 0 {
                // SAFETY: query the still-unreaped child returned by P_PGID.
                if unsafe { libc::getpgid(pid) } != self.pid {
                    errors.push(format!(
                        "waitable child {pid} no longer identifies owned group {}",
                        self.pid
                    ));
                    break;
                }
                let start = match process_start(pid) {
                    Ok(start) => start,
                    Err(error) => {
                        errors.push(format!("identify owned descendant {pid}: {error}"));
                        break;
                    }
                };
                let mut status = 0;
                // SAFETY: consume only this exact waitable owned child, using
                // the same clone-child accounting as the observing group wait.
                let reaped =
                    unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::__WALL) };
                if reaped != pid {
                    errors.push(format!(
                        "reap owned descendant {pid}: result={reaped} error={}",
                        io::Error::last_os_error()
                    ));
                    break;
                }
                self.descendants.push((pid, start, status));
            }
            if let Err(error) = self.drain() {
                errors.push(format!("retirement stream read: {error}"));
            }
            if Instant::now() >= deadline {
                errors.push("owned group retirement exceeded 5s grace".into());
                break;
            }
            if let Err(error) = self.poll_pipes() {
                errors.push(format!("retirement poll: {error}"));
                break;
            }
        }
        if self.retired {
            // A failed nonblocking setup already failed the stage. Closing an
            // unconfigured read end is safe now that every writer has retired.
            for capture in [&mut self.stdout, &mut self.stderr] {
                if !capture.nonblocking {
                    capture.fd.take();
                }
            }
            while self.stdout.fd.is_some() || self.stderr.fd.is_some() {
                if let Err(error) = self.drain() {
                    errors.push(format!("final stream read: {error}"));
                }
                if Instant::now() >= deadline {
                    errors.push("pipe EOF missing after owned group retirement".into());
                    break;
                }
                if let Err(error) = self.poll_pipes() {
                    errors.push(format!("final stream poll: {error}"));
                    break;
                }
            }
        }
        if Instant::now() >= deadline
            && !errors
                .iter()
                .any(|error| error == "owned group retirement exceeded 5s grace")
        {
            errors.push("owned group retirement exceeded 5s grace".into());
        }
        if !self.retired || !errors.is_empty() {
            // Preserve evidence when ownership, pipe EOF, or cleanup cannot be
            // proved. The primary monitor failure remains in StageReport.
            self.retain.store(true, Ordering::Relaxed);
        }
        self.cleanup_returned = true;
        errors
    }
}

impl Drop for StageOwner {
    fn drop(&mut self) {
        // An explicit failed retirement already used its bounded grace and
        // retained the evidence. Unexpected unwind still gets one attempt.
        if !self.retired && !self.cleanup_returned {
            for error in self.retire() {
                let _ = writeln!(io::stderr(), "protocol stage unwind cleanup: {error}");
            }
        }
    }
}

#[derive(Clone)]
struct StageReport {
    name: &'static str,
    leader: libc::pid_t,
    invocation: String,
    status: Option<ExitStatus>,
    primary: Option<String>,
    cleanup: Vec<String>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    retired: bool,
    descendants: usize,
    reaped_descendants: Vec<(libc::pid_t, u64, libc::c_int)>,
    timed_out: bool,
    monitor_failed: bool,
    stdout_overflow: bool,
    stderr_overflow: bool,
}

impl StageReport {
    fn diagnostic(&self) -> String {
        format!(
            "stage={} invocation={} status={:?} primary={:?} cleanup={:?} retired={} descendants={} monitor_failed={} overflow={}/{}\nstdout:\n{}\nstderr:\n{}",
            self.name,
            self.invocation,
            self.status,
            self.primary,
            self.cleanup,
            self.retired,
            self.descendants,
            self.monitor_failed,
            self.stdout_overflow,
            self.stderr_overflow,
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }

    fn require_success(&self) -> Result<(), String> {
        if self.primary.is_none()
            && self.cleanup.is_empty()
            && self.retired
            && self.status.is_some_and(|status| status.success())
        {
            Ok(())
        } else {
            Err(self.diagnostic())
        }
    }
}

fn run_stage(
    tree: &PrivateTree,
    name: &'static str,
    command: &mut Command,
    timeout_control: bool,
) -> StageReport {
    let started = Instant::now();
    let mut report = StageReport {
        name,
        leader: 0,
        invocation: format!("{command:?}"),
        status: None,
        primary: None,
        cleanup: Vec::new(),
        stdout: Vec::new(),
        stderr: Vec::new(),
        retired: false,
        descendants: 0,
        reaped_descendants: Vec::new(),
        timed_out: false,
        monitor_failed: false,
        stdout_overflow: false,
        stderr_overflow: false,
    };
    let mut owner = match StageOwner::spawn(command, tree) {
        Ok(owner) => owner,
        Err(error) => {
            report.primary = Some(format!("spawn: {error}"));
            report.monitor_failed = true;
            return report;
        }
    };
    report.leader = owner.pid;
    let mut deadline = started
        + if timeout_control {
            Duration::from_secs(5)
        } else {
            WALL_LIMIT
        };
    let mut armed = false;
    if let Err(error) = owner
        .stdout
        .configure()
        .and_then(|()| owner.stderr.configure())
    {
        report.primary = Some(format!("configure pipes: {error}"));
    }
    let start = if report.primary.is_none() {
        match process_start(owner.pid) {
            Ok(start) => Some(start),
            Err(error) => {
                report.primary = Some(format!("identify unreaped leader: {error}"));
                None
            }
        }
    } else {
        None
    };
    while report.primary.is_none() {
        if let Err(error) = owner.drain() {
            report.primary = Some(format!("capture: {error}"));
            break;
        }
        if timeout_control && !armed {
            let text = String::from_utf8_lossy(&owner.stdout.bytes);
            if text
                .lines()
                .any(|line| line.starts_with("WRAPPER_TIMEOUT_PARENT "))
                && text
                    .lines()
                    .any(|line| line.starts_with("WRAPPER_TIMEOUT_CHILD "))
            {
                let observed = Instant::now();
                if observed >= deadline {
                    report.primary = Some(
                        "timeout control did not publish both live processes within 5s".into(),
                    );
                    break;
                }
                armed = true;
                deadline = observed + Duration::from_millis(100);
            }
        }
        let exited = match owner.exited_without_reaping() {
            Ok(exited) => exited,
            Err(error) => {
                report.primary = Some(format!("waitid WNOWAIT: {error}"));
                break;
            }
        };
        // Completion observed after the deadline does not excuse the wall
        // bound, even if scheduling delayed this monitor's next observation.
        if Instant::now() >= deadline {
            report.timed_out = !timeout_control || armed;
            report.primary = Some(if timeout_control && !armed {
                "timeout control did not publish both live processes within 5s".into()
            } else {
                "payload wall deadline exceeded".into()
            });
            break;
        }
        if exited {
            break;
        }
        if let Err(error) = owner.poll_pipes() {
            report.primary = Some(format!("poll pipes: {error}"));
        }
    }
    report.monitor_failed = report.primary.is_some();
    report.cleanup = owner.retire();
    report.status = owner.status;
    report.retired = owner.retired;
    report.descendants = owner.descendants.len();
    report.reaped_descendants = std::mem::take(&mut owner.descendants);
    report.stdout_overflow = owner.stdout.overflow;
    report.stderr_overflow = owner.stderr.overflow;
    report.stdout = std::mem::take(&mut owner.stdout.bytes);
    report.stderr = std::mem::take(&mut owner.stderr.bytes);
    if report.primary.is_none() && !report.cleanup.is_empty() {
        report.primary = report.cleanup.first().cloned();
    }
    if report.primary.is_none() && !report.status.is_some_and(|status| status.success()) {
        report.primary = Some(format!(
            "child did not exit successfully: {:?}",
            report.status
        ));
    }
    // Keep test-runner output outside retirement; a blocked output consumer
    // must not stall cleanup while the process-group leader is still owned.
    for (pid, start, status) in &report.reaped_descendants {
        println!(
            "C_PROTOCOL_REAPED pgid={} pid={pid} start={start} raw_status={status}",
            owner.pid
        );
    }
    println!(
        "C_PROTOCOL_STAGE name={name} pid={} pgid={} start={start:?} code={:?} signal={:?} retired={} descendants={} timed_out={} monitor_failed={} stdout_overflow={} stderr_overflow={} stdout_bytes={} stderr_bytes={} cleanup_errors={} wall_ms={} invocation={}",
        owner.pid,
        owner.pid,
        report.status.and_then(|status| status.code()),
        report.status.and_then(|status| status.signal()),
        report.retired,
        report.descendants,
        report.timed_out,
        report.monitor_failed,
        report.stdout_overflow,
        report.stderr_overflow,
        report.stdout.len(),
        report.stderr.len(),
        report.cleanup.len(),
        started.elapsed().as_millis(),
        report.invocation
    );
    report
}

const PASS_BEFORE_CONTEXT: &[&str] = &[
    "PASS terminal-before-start: no pthread/read/join",
    "PASS terminal-during-create: latched through handle publication",
    "PASS terminal-before-handle-publication: queued public cancellation",
    "PASS early-completion: no send window; one physical join",
    "PASS normal-zero: outcome is not retirement; one join before release",
    "PASS pre-public-read: sticky cancellation; no fabricated result",
    "PASS inside-kernel: exact staging args, unchanged flags, alias reuse",
    "PASS public-return-before-disable: real EAGAIN retained on late cancel",
    "PASS delayed-sender: drain before join; fresh sends refused through retirement",
    "PASS queued-inotify: actual EINVAL and retained event",
    "PASS create-error: typed no-thread failure, no fallback read",
    "PASS injected-cancel-error: real event completion; first error retained",
    "PASS wake-before-wait: no lost wake or invented terminal outcome",
];
const CONTEXT_PASS: &str = "PASS inherited-context-v2: matching credentials/namespaces/mask; new thread has disabled altstack; attribute=unavailable-label; read/join/ownership/restoration verified";
const JOIN_PASS: &str =
    "PASS injected-join-error: first failure and ownership retained until process exit";
const AGGREGATE_PASS: &str =
    "PASS all C protocol controls; injected retirement failure contained by process exit";

fn complete_protocol(stdout: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(stdout)
        .map_err(|error| format!("non-UTF8 protocol transcript: {error}"))?;
    let actual: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("PASS"))
        .collect();
    let expected: Vec<_> = PASS_BEFORE_CONTEXT
        .iter()
        .copied()
        .chain([CONTEXT_PASS, JOIN_PASS, AGGREGATE_PASS])
        .collect();
    if actual != expected {
        return Err(format!(
            "incomplete, reordered or unexpected internal PASS records: {actual:?}"
        ));
    }
    context_completion(text, "live", false)
}

fn exact_record(text: &str, expected: &str) -> Result<(), String> {
    if text.lines().filter(|line| *line == expected).count() != 1 {
        return Err(format!(
            "missing/duplicated context qualification: {expected}"
        ));
    }
    Ok(())
}

fn one_record<'a>(text: &'a str, prefix: &str) -> Result<&'a str, String> {
    let mut records = text.lines().filter(|line| line.starts_with(prefix));
    match (records.next(), records.next()) {
        (Some(record), None) => Ok(record),
        _ => Err(format!("missing/duplicated context observation: {prefix}")),
    }
}

fn record_field<'a>(record: &'a str, key: &str) -> Result<&'a str, String> {
    let mut values = record.split_whitespace().filter_map(|field| {
        field
            .split_once('=')
            .filter(|(name, _)| *name == key)
            .map(|(_, value)| value)
    });
    match (values.next(), values.next()) {
        (Some(value), None) => Ok(value),
        _ => Err(format!("missing/duplicated field {key}: {record}")),
    }
}

fn field_is(record: &str, key: &str, expected: &str) -> Result<(), String> {
    if record_field(record, key)? != expected {
        return Err(format!("expected {key}={expected}: {record}"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum AttributeExpected<'a> {
    Value(&'a [u8]),
    ReadError(i32, bool), // errno; explicitly synthetic open=100
    Missing,
    Truncated,
}

fn attribute_record(
    record: &str,
    identity: (libc::pid_t, u64),
    expected: AttributeExpected<'_>,
) -> Result<(), String> {
    field_is(record, "tid", &identity.0.to_string())?;
    field_is(record, "start", &identity.1.to_string())?;
    if !matches!(expected, AttributeExpected::Missing) {
        let open: i32 = record_field(record, "open")?
            .parse()
            .map_err(|_| format!("invalid open result: {record}"))?;
        if open < 0 {
            return Err(format!("unexpected fixture open failure: {record}"));
        }
        field_is(record, "close", "0")?;
    }
    // errno is checked only for failed calls. Successful open/read/close may
    // leave any recorded errno; EOF/kind/length/all bytes remain exact.
    let (kind, reads, last_read, eof, bytes) = match expected {
        AttributeExpected::Value(bytes) => ("0", "2", "0", "1", bytes.to_vec()),
        AttributeExpected::ReadError(errno, synthetic) => {
            if synthetic {
                field_is(record, "open", "100")?;
            }
            field_is(record, "read_errno", &errno.to_string())?;
            ("2", "1", "-1", "0", Vec::new())
        }
        AttributeExpected::Missing => {
            field_is(record, "path", "/proc/self/task/0/attr/current")?;
            field_is(record, "open", "-1")?;
            field_is(record, "open_errno", "2")?;
            field_is(record, "close", "-2")?;
            ("1", "0", "-2", "0", Vec::new())
        }
        AttributeExpected::Truncated => ("4", "1", "4095", "0", vec![b'x'; 4095]),
    };
    for (key, value) in [
        ("kind", kind),
        ("reads", reads),
        ("last_read", last_read),
        ("eof", eof),
    ] {
        field_is(record, key, value)?;
    }
    field_is(record, "length", &bytes.len().to_string())?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    field_is(record, "bytes_hex", &hex)
}

fn live_observations(text: &str) -> Result<[(libc::pid_t, u64); 2], String> {
    let identity = one_record(text, "CONTEXT_IDENTITY ")?;
    let mut identities = [(0, 0); 2];
    for (index, role) in ["creator", "helper"].iter().enumerate() {
        let (pid, start) = record_field(identity, role)?
            .split_once('/')
            .ok_or_else(|| "invalid task identity".to_string())?;
        identities[index] = (
            pid.parse().map_err(|_| "invalid task pid")?,
            start.parse().map_err(|_| "invalid task generation")?,
        );
        if identities[index].0 <= 0 || identities[index].1 == 0 {
            return Err("invalid task identity".into());
        }
    }
    if identities[0].0 == identities[1].0 {
        return Err("creator/helper identity aliased".into());
    }
    let actual: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("CONTEXT_ATTRIBUTE origin=actual-live-query "))
        .collect();
    if actual.len() != 2 {
        return Err("both actual task attribute records are required".into());
    }
    for (record, identity) in actual.into_iter().zip(identities) {
        attribute_record(
            record,
            identity,
            AttributeExpected::ReadError(libc::EINVAL, false),
        )?;
        field_is(
            record,
            "path",
            &format!("/proc/self/task/{}/attr/current", identity.0),
        )?;
    }
    for origin in ["actual-provider-before", "actual-provider-after"] {
        let record = one_record(text, &format!("CONTEXT_ATTRIBUTE origin={origin} "))?;
        attribute_record(
            record,
            identities[0],
            AttributeExpected::Value(b"capability,bpf,ima"),
        )?;
        field_is(record, "path", "/sys/kernel/security/lsm")?;
    }
    Ok(identities)
}

fn context_completion(text: &str, mode: &str, context_only: bool) -> Result<(), String> {
    live_observations(text)?;
    exact_record(
        text,
        &format!(
            "CONTEXT_CONTROL mode={mode} context_only={}",
            u8::from(context_only)
        ),
    )?;
    exact_record(
        text,
        &format!(
            "CONTEXT_PROVIDER_DECISION origin=actual-live-inventory profile=observed-unavailable reason=recognized mode={mode}"
        ),
    )?;
    exact_record(
        text,
        &format!(
            "CONTEXT_ATTRIBUTE_DECISION origin=actual-live-query decision=unavailable-label mode={mode}"
        ),
    )?;
    for exact in [
        "CONTEXT_READ_DISPATCH_STABLE callable=1 plt_bytes=1 slot=1 target=1 public_symbol=1 dso=1",
        "CONTEXT_RETIREMENT physical_joins=1 cancel_sends=0 destroyed=1 owner_closed_fd=1",
    ] {
        exact_record(text, exact)?;
    }
    for prefix in [
        "CONTEXT_ATTRIBUTE origin=actual-provider-before ",
        "CONTEXT_ATTRIBUTE origin=actual-provider-after ",
        "CONTEXT_IDENTITY ",
        "CONTEXT_ALTSTACK ",
        "CONTEXT_READ_BINDING ",
        "CONTEXT_READ_PLT phase=before-read ",
        "CONTEXT_READ_PLT phase=after-join ",
        "CONTEXT_READ_GOT phase=before-read ",
        "CONTEXT_READ_GOT phase=after-join ",
        "CONTEXT_READ_DISPATCH phase=before-read ",
        "CONTEXT_READ_DISPATCH phase=after-join ",
        "CONTEXT_ENDPOINT ",
        "CONTEXT_RESTORATION mask_exact=1 altstack_exact=1 ",
    ] {
        one_record(text, prefix)?;
    }
    let outcomes: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("CONTEXT_READ_OUTCOME "))
        .collect();
    if outcomes.len() != 1
        || !outcomes[0].starts_with("CONTEXT_READ_OUTCOME outcome=1 result=0 read_errno=")
        || !outcomes[0].ends_with(" terminal=0 error_phase=0 error_number=0")
    {
        return Err("missing exact normal read/control outcome".into());
    }
    if text
        .lines()
        .filter(|line| line.starts_with("CONTEXT_ATTRIBUTE origin=actual-live-query "))
        .count()
        != 2
    {
        return Err("both actual task attribute records are required".into());
    }
    if text.lines().any(|line| {
        line.starts_with("UNEXPECTED_ACCEPTANCE")
            || line.starts_with("CONTEXT_ERROR")
            || line.starts_with("CONTEXT_REJECT")
            || line.contains(" decision=rejected ")
    }) {
        return Err("protocol transcript contains a rejected observation".into());
    }
    Ok(())
}

const SYNTHETIC_LABEL_PASS: &str =
    "PASS synthetic-label-classifier: equal length and all bytes including NUL suffix";
const CONTEXT_MODES: &[(&str, &str)] = &[
    ("context-equal-labels", "equal-labels"),
    ("context-mask-mismatch", "mask-mismatch"),
    ("context-query-asymmetry", "query-asymmetry"),
    ("context-query-errors", "query-errors"),
    ("context-missing-task", "missing-task"),
    ("context-truncated-label", "truncated-label"),
    ("context-label-mismatch", "label-mismatch"),
    ("context-label-length", "label-length"),
    ("context-unqualified-provider", "unqualified-provider"),
    ("context-query-eperm", "query-eperm"),
    ("context-inventory-malformed", "inventory-malformed"),
    ("context-inventory-unknown", "inventory-unknown"),
    ("context-inventory-changing", "inventory-changing"),
    ("context-inventory-missing", "inventory-missing"),
    ("context-inventory-truncated", "inventory-truncated"),
];

fn fixture_observations(
    text: &str,
    mode: &str,
    identities: [(libc::pid_t, u64); 2],
) -> Result<(), String> {
    use AttributeExpected::*;
    let (left, right) = match mode {
        "equal-labels" => (Value(b"a\0x"), Value(b"a\0x")),
        "label-mismatch" => (Value(b"a\0x"), Value(b"a\0y")),
        "label-length" => (Value(b"a\0x"), Value(b"a\0x\0")),
        "query-asymmetry" => (Value(b"a\0x"), ReadError(libc::EINVAL, true)),
        "query-errors" => (ReadError(libc::EINVAL, true), ReadError(libc::EACCES, true)),
        "query-eperm" => (ReadError(libc::EPERM, true), ReadError(libc::EPERM, true)),
        "unqualified-provider" => (ReadError(libc::EINVAL, true), ReadError(libc::EINVAL, true)),
        "missing-task" => (ReadError(libc::EINVAL, false), Missing),
        "truncated-label" => (Value(b"a\0x"), Truncated),
        "inventory-malformed" => (Value(b"capability,,bpf,ima"), Value(b"capability,,bpf,ima")),
        "inventory-unknown" => (
            Value(b"capability,bpf,ima,fixture_unknown"),
            Value(b"capability,bpf,ima,fixture_unknown"),
        ),
        "inventory-changing" => (Value(b"capability,bpf,ima"), Value(b"capability,ima,bpf")),
        "inventory-missing" => (Value(b"capability,bpf,ima"), Missing),
        "inventory-truncated" => (Value(b"capability,bpf,ima"), Truncated),
        _ => return Err(format!("unknown fixture mode {mode}")),
    };
    let inventory = mode.starts_with("inventory-");
    let origins = if inventory {
        ["inventory-fixture-before", "inventory-fixture-after"]
    } else {
        ["classifier-fixture-creator", "classifier-fixture-helper"]
    };
    for (index, (origin, expected)) in origins.into_iter().zip([left, right]).enumerate() {
        let identity = if matches!(expected, Missing) {
            (0, 0)
        } else {
            identities[if inventory { 0 } else { index }]
        };
        attribute_record(
            one_record(text, &format!("CONTEXT_ATTRIBUTE origin={origin} "))?,
            identity,
            expected,
        )?;
    }
    Ok(())
}

fn require_context_mode(report: &StageReport, mode: &str) -> Result<(), String> {
    let text = std::str::from_utf8(&report.stdout).map_err(|error| error.to_string())?;
    let stderr = std::str::from_utf8(&report.stderr).map_err(|error| error.to_string())?;
    let identities = live_observations(text)?;
    if identities[0].0 != report.leader {
        return Err("actual creator is not this owned stage leader".into());
    }
    exact_record(text, &format!("CONTEXT_CONTROL mode={mode} context_only=1"))?;
    if text
        .lines()
        .chain(stderr.lines())
        .any(|line| line.starts_with("UNEXPECTED_ACCEPTANCE"))
    {
        return Err(format!("unexpected classifier acceptance in {mode}"));
    }
    let origin = if mode == "mask-mismatch" {
        "actual-helper-SIGUSR1-unblock"
    } else if mode.starts_with("inventory-") {
        "inventory-fixture"
    } else {
        "classifier-fixture"
    };
    let fault = format!("CONTEXT_TEST_FAULT mode={mode} origin={origin}");
    if one_record(text, "CONTEXT_TEST_FAULT ")? != fault {
        return Err(format!("wrong fault origin for {mode}"));
    }
    let passes: Vec<_> = text
        .lines()
        .chain(stderr.lines())
        .filter(|line| line.starts_with("PASS"))
        .collect();
    if mode == "equal-labels" {
        report.require_success()?;
        if !stderr.is_empty() || passes != [SYNTHETIC_LABEL_PASS, CONTEXT_PASS] {
            return Err(report.diagnostic());
        }
        fixture_observations(text, mode, identities)?;
        exact_record(
            text,
            "CONTEXT_ATTRIBUTE_DECISION origin=classifier-fixture decision=exact-label-bytes mode=equal-labels",
        )?;
        // The synthetic fixture never relabels the real unavailable observation.
        return context_completion(text, mode, true);
    }
    if report.monitor_failed
        || !report.retired
        || !report.cleanup.is_empty()
        || report.status.and_then(|status| status.signal()) != Some(libc::SIGABRT)
        || !passes.is_empty()
    {
        return Err(report.diagnostic());
    }
    if text.lines().any(|line| {
        [
            "CONTEXT_READ_",
            "CONTEXT_RETIREMENT ",
            "CONTEXT_RESTORATION ",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
    }) {
        return Err(format!(
            "negative {mode} claimed read/completion/retirement"
        ));
    }
    if mode == "mask-mismatch" {
        exact_record(
            text,
            "CONTEXT_REJECT reason=signal-mask signal=10 creator=1 helper=0",
        )?;
        let assertion = "Assertion `sigismember(&test_mask, signal) == sigismember(&child_context.mask, signal)' failed.";
        if stderr.lines().count() != 1
            || !stderr.starts_with("terminal-read-protocol: ")
            || !stderr.contains(": inherited_context: Assertion ")
            || !stderr.trim_end_matches('\n').ends_with(assertion)
            || stderr.contains("CONTEXT_ERROR")
        {
            return Err(report.diagnostic());
        }
        return Ok(());
    }
    fixture_observations(text, mode, identities)?;
    exact_record(
        text,
        &format!(
            "CONTEXT_PROVIDER_DECISION origin=actual-live-inventory profile=observed-unavailable reason=recognized mode={mode}"
        ),
    )?;
    exact_record(
        text,
        &format!(
            "CONTEXT_ATTRIBUTE_DECISION origin=actual-live-query decision=unavailable-label mode={mode}"
        ),
    )?;
    let operation = if mode.starts_with("inventory-") {
        let reason = match mode {
            "inventory-malformed" => "malformed",
            "inventory-unknown" => "unknown",
            "inventory-changing" => "changing",
            "inventory-missing" | "inventory-truncated" => "query-error",
            _ => return Err(format!("unknown inventory mode {mode}")),
        };
        exact_record(
            text,
            &format!(
                "CONTEXT_PROVIDER_DECISION origin=inventory-fixture profile=unclassified reason={reason} mode={mode}"
            ),
        )?;
        "provider-inventory-oracle"
    } else {
        exact_record(
            text,
            &format!(
                "CONTEXT_ATTRIBUTE_DECISION origin=classifier-fixture decision=rejected mode={mode}"
            ),
        )?;
        "attribute-oracle"
    };
    if stderr
        != format!("CONTEXT_ERROR operation={operation} path={mode} errno=71 (Protocol error)\n")
    {
        return Err(report.diagnostic());
    }
    Ok(())
}

fn context_modes(tree: &PrivateTree) -> Result<(), String> {
    for &(stage, mode) in CONTEXT_MODES {
        let report = run_stage(
            tree,
            stage,
            Command::new(tree.path.join("terminal-read-protocol"))
                .arg("--context-mode")
                .arg(mode),
            false,
        );
        println!("C_PROTOCOL_CONTEXT_STDOUT_BEGIN mode={mode}");
        print!("{}", String::from_utf8_lossy(&report.stdout));
        println!("C_PROTOCOL_CONTEXT_STDOUT_END mode={mode}");
        println!("C_PROTOCOL_CONTEXT_STDERR_BEGIN mode={mode}");
        print!("{}", String::from_utf8_lossy(&report.stderr));
        println!("C_PROTOCOL_CONTEXT_STDERR_END mode={mode}");
        require_context_mode(&report, mode)
            .map_err(|error| format!("{error}\n{}", report.diagnostic()))?;
        if mode == "mask-mismatch" {
            let mut malformed = report.clone();
            malformed.stderr = [b"PASS:".as_slice(), report.stderr.as_slice()].concat();
            if require_context_mode(&malformed, mode).is_ok() {
                return Err("mask assertion with a stderr PASS prefix was accepted".into());
            }
        }
    }
    println!("C_PROTOCOL_CONTEXT_CONTROLS positive=1 rejected=14 stderr_pass_prefix=rejected");
    Ok(())
}

const WRAPPER_FIXTURE: &[u8] = br#"
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
static unsigned long long start_time(pid_t pid) {
  char path[64], data[4096];
  int n = snprintf(path, sizeof(path), "/proc/%ld/stat", (long)pid);
  assert(n > 0 && (size_t)n < sizeof(path));
  FILE *file = fopen(path, "r");
  assert(file != NULL);
  assert(fgets(data, sizeof(data), file) != NULL);
  assert(feof(file) || fgetc(file) == EOF);
  assert(!ferror(file));
  assert(fclose(file) == 0);
  char *cursor = strrchr(data, ')');
  assert(cursor != NULL && cursor[1] == ' ');
  cursor += 2;
  for (int field = 3; field < 22; ++field) {
    cursor = strchr(cursor, ' ');
    assert(cursor != NULL);
    ++cursor;
  }
  char *end;
  errno = 0;
  unsigned long long start = strtoull(cursor, &end, 10);
  assert(errno == 0 && end != cursor && *end == ' ' && start > 0);
  return start;
}
int main(int argc, char **argv) {
  assert(argc == 2);
  if (strcmp(argv[1], "incomplete") == 0) {
    puts("PASS terminal-before-start: no pthread/read/join");
    return 0;
  }
  if (strcmp(argv[1], "abort") == 0) { abort(); }
  if (strcmp(argv[1], "overflow") == 0) {
    char bytes[8192];
    memset(bytes, 'x', sizeof(bytes));
    for (;;) {
      ssize_t n = write(STDOUT_FILENO, bytes, sizeof(bytes));
      assert(n > 0);
    }
  }
  if (strcmp(argv[1], "retirement-order") == 0) {
    pid_t exited = fork();
    assert(exited >= 0);
    if (exited == 0) { _exit(7); }
    siginfo_t info = {0};
    assert(waitid(P_PID, (id_t)exited, &info, WEXITED | WNOWAIT) == 0);
    assert(info.si_pid == exited && info.si_code == CLD_EXITED && info.si_status == 7);
    unsigned long long exited_start = start_time(exited);
    int ready[2];
    assert(pipe(ready) == 0);
    pid_t live = fork();
    assert(live >= 0);
    if (live == 0) {
      assert(close(ready[0]) == 0);
      assert(write(ready[1], "r", 1) == 1);
      assert(close(ready[1]) == 0);
      for (;;) { pause(); }
    }
    assert(close(ready[1]) == 0);
    char byte;
    assert(read(ready[0], &byte, 1) == 1 && byte == 'r');
    assert(close(ready[0]) == 0);
    unsigned long long live_start = start_time(live);
    printf("WRAPPER_RETIREMENT_READY leader=%ld exited=%ld/%llu live=%ld/%llu exited_code=7\n",
           (long)getpid(), (long)exited, exited_start, (long)live, live_start);
    assert(fflush(stdout) == 0);
    return 0;
  }
  assert(strcmp(argv[1], "timeout") == 0);
  pid_t child = fork();
  assert(child >= 0);
  if (child == 0) {
    printf("WRAPPER_TIMEOUT_CHILD pid=%ld parent=%ld\n", (long)getpid(), (long)getppid());
  } else {
    printf("WRAPPER_TIMEOUT_PARENT pid=%ld child=%ld\n", (long)getpid(), (long)child);
  }
  assert(fflush(stdout) == 0);
  for (;;) { pause(); }
}
"#;

fn require_retirement_order(report: &StageReport) -> Result<(), String> {
    report.require_success()?;
    let text = std::str::from_utf8(&report.stdout).map_err(|error| error.to_string())?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() != 5
        || fields[0] != "WRAPPER_RETIREMENT_READY"
        || fields[1] != format!("leader={}", report.leader)
        || fields[4] != "exited_code=7"
        || !report.stderr.is_empty()
        || report.descendants != 2
    {
        return Err(report.diagnostic());
    }
    let identity = |field: &str, prefix: &str| -> Result<(libc::pid_t, u64), String> {
        let (pid, start) = field
            .strip_prefix(prefix)
            .and_then(|value| value.split_once('/'))
            .ok_or_else(|| report.diagnostic())?;
        let pid = pid
            .parse::<libc::pid_t>()
            .map_err(|_| report.diagnostic())?;
        let start = start.parse::<u64>().map_err(|_| report.diagnostic())?;
        if pid <= 0 || start == 0 || pid == report.leader {
            return Err(report.diagnostic());
        }
        Ok((pid, start))
    };
    let exited = identity(fields[2], "exited=")?;
    let live = identity(fields[3], "live=")?;
    if exited.0 == live.0 {
        return Err(report.diagnostic());
    }
    let mut expected = vec![
        (exited.0, exited.1, 7 << 8),
        (live.0, live.1, libc::SIGKILL),
    ];
    let mut actual = report.reaped_descendants.clone();
    expected.sort_unstable();
    actual.sort_unstable();
    if actual != expected {
        return Err(report.diagnostic());
    }
    Ok(())
}

fn compile_command(tree: &PrivateTree, sources: &[&str], output: &str) -> Command {
    let mut command = Command::new("/usr/bin/cc");
    command.args(C_FLAGS);
    for source in sources {
        command.arg(tree.path.join(source));
    }
    command.arg("-ldl").arg("-o").arg(tree.path.join(output));
    command
}

fn exercise(tree: &PrivateTree) -> Result<(), String> {
    for directory in ["src", "tests", "tmp"] {
        DirBuilder::new()
            .mode(0o700)
            .create(tree.path.join(directory))
            .map_err(|error| error.to_string())?;
    }
    for (path, bytes) in [
        ("src/terminal_read.c", HELPER),
        ("src/terminal_read.h", HEADER),
        ("tests/terminal_read_protocol.c", PROTOCOL),
        ("wrapper.c", WRAPPER_FIXTURE),
        (
            "compiler-error.c",
            b"#error RVK_EXPECTED_COMPILER_FAILURE\n".as_slice(),
        ),
    ] {
        tree.write(path, bytes).map_err(|error| error.to_string())?;
    }
    println!(
        "C_PROTOCOL_EMBEDDED helper_bytes={} header_bytes={} protocol_bytes={}",
        HELPER.len(),
        HEADER.len(),
        PROTOCOL.len()
    );
    run_stage(
        tree,
        "compile-protocol",
        &mut compile_command(
            tree,
            &["src/terminal_read.c", "tests/terminal_read_protocol.c"],
            "terminal-read-protocol",
        ),
        false,
    )
    .require_success()?;
    let protocol = run_stage(
        tree,
        "complete-protocol",
        &mut Command::new(tree.path.join("terminal-read-protocol")),
        false,
    );
    protocol.require_success()?;
    if !protocol.stderr.is_empty() {
        return Err(protocol.diagnostic());
    }
    complete_protocol(&protocol.stdout)
        .map_err(|error| format!("{error}\n{}", protocol.diagnostic()))?;
    println!("C_PROTOCOL_NATIVE_TRANSCRIPT_BEGIN");
    print!("{}", String::from_utf8_lossy(&protocol.stdout));
    println!("C_PROTOCOL_NATIVE_TRANSCRIPT_END");
    context_modes(tree)?;

    // These controls exercise this very subprocess owner and parser without
    // modifying or suppressing any source-under-test assertion.
    run_stage(
        tree,
        "compile-wrapper-controls",
        &mut compile_command(tree, &["wrapper.c"], "wrapper"),
        false,
    )
    .require_success()?;
    let compiler_error = run_stage(
        tree,
        "expected-compiler-error",
        &mut compile_command(tree, &["compiler-error.c"], "must-not-build"),
        false,
    );
    if compiler_error.status.and_then(|status| status.code()) != Some(1)
        || compiler_error.monitor_failed
        || !compiler_error.retired
        || !compiler_error.cleanup.is_empty()
        || !String::from_utf8_lossy(&compiler_error.stderr)
            .contains("RVK_EXPECTED_COMPILER_FAILURE")
    {
        return Err(compiler_error.diagnostic());
    }
    let incomplete = run_stage(
        tree,
        "expected-incomplete-aggregate",
        Command::new(tree.path.join("wrapper")).arg("incomplete"),
        false,
    );
    incomplete.require_success()?;
    if incomplete.stdout != format!("{}\n", PASS_BEFORE_CONTEXT[0]).as_bytes()
        || !incomplete.stderr.is_empty()
    {
        return Err(incomplete.diagnostic());
    }
    if complete_protocol(&incomplete.stdout).is_ok() {
        return Err("incomplete child transcript was accepted".into());
    }
    let aborted = run_stage(
        tree,
        "expected-aborted-aggregate",
        Command::new(tree.path.join("wrapper")).arg("abort"),
        false,
    );
    if aborted.status.and_then(|status| status.signal()) != Some(libc::SIGABRT)
        || aborted.monitor_failed
        || !aborted.retired
        || !aborted.cleanup.is_empty()
    {
        return Err(aborted.diagnostic());
    }
    let timeout = run_stage(
        tree,
        "expected-timeout-descendant",
        Command::new(tree.path.join("wrapper")).arg("timeout"),
        true,
    );
    if !timeout.timed_out
        || !timeout.retired
        || !timeout.cleanup.is_empty()
        || timeout.descendants != 1
        || timeout.status.and_then(|status| status.signal()) != Some(libc::SIGKILL)
    {
        return Err(timeout.diagnostic());
    }
    let overflow = run_stage(
        tree,
        "expected-output-overflow",
        Command::new(tree.path.join("wrapper")).arg("overflow"),
        false,
    );
    if overflow.primary.as_deref()
        != Some("capture: child stream exceeded 2 MiB; transcript is incomplete")
        || !overflow.monitor_failed
        || !overflow.stdout_overflow
        || overflow.stderr_overflow
        || overflow.stdout.len() != STREAM_CAP
        || !overflow.stderr.is_empty()
        || !overflow.retired
        || !overflow.cleanup.is_empty()
        || overflow.status.and_then(|status| status.signal()) != Some(libc::SIGKILL)
    {
        return Err(overflow.diagnostic());
    }
    let retirement = run_stage(
        tree,
        "expected-disarmed-group-retirement",
        Command::new(tree.path.join("wrapper")).arg("retirement-order"),
        false,
    );
    require_retirement_order(&retirement)?;
    print!("{}", String::from_utf8_lossy(&retirement.stdout));
    let transcript = std::str::from_utf8(&protocol.stdout).unwrap();
    let reordered = transcript
        .replacen(PASS_BEFORE_CONTEXT[0], "WRAPPER_SWAP", 1)
        .replacen(PASS_BEFORE_CONTEXT[1], PASS_BEFORE_CONTEXT[0], 1)
        .replacen("WRAPPER_SWAP", PASS_BEFORE_CONTEXT[1], 1);
    for (name, altered) in [
        ("unexpected", format!("{transcript}\nPASS unexpected\n")),
        (
            "duplicate",
            format!("{transcript}\n{}\n", PASS_BEFORE_CONTEXT[0]),
        ),
        ("reordered", reordered),
        ("missing-context", transcript.replacen(CONTEXT_PASS, "", 1)),
        ("pass-colon", format!("{transcript}\nPASS:unexpected\n")),
        ("pass-tab", format!("{transcript}\nPASS\tunexpected\n")),
        ("pass-bare", format!("{transcript}\nPASS\n")),
    ] {
        if complete_protocol(altered.as_bytes()).is_ok() {
            return Err(format!("{name} PASS record control was accepted"));
        }
    }
    println!(
        "C_PROTOCOL_WRAPPER_CONTROLS incomplete=rejected abort=rejected compiler_error=rejected timeout=rejected descendant_retired=1 output_overflow=rejected unexpected=rejected duplicate=rejected reordered=rejected missing_context=rejected pass_colon=rejected pass_tab=rejected pass_bare=rejected disarmed_group_retired=2"
    );
    Ok(())
}

#[test]
fn c_terminal_read_protocol_preserves_lifetime_and_context() {
    let mut subreaper = Subreaper::new().expect("enable isolated test-process child subreaper");
    let mut tree = PrivateTree::new().expect("create exclusive native protocol directory");
    let mut errors = Vec::new();
    if let Err(primary) = exercise(&tree) {
        errors.push(primary);
    }
    if let Err(error) = tree.remove() {
        errors.push(format!("temporary-directory cleanup: {error}"));
    }
    if let Err(error) = subreaper.restore() {
        errors.push(format!("restore child subreaper: {error}"));
    }
    if !errors.is_empty() {
        panic!("{}", errors.join("\nadditional cleanup: "));
    }
}
