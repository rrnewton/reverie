//! Test-only owner for a fresh, audited host with no independent child waiters.
//! This does not enforce that contract on arbitrary callbacks or library users.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::process::{Child, ChildStdin, Command};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
pub struct WaitRecord {
    pub sequence: u64,
    pub pid: i32,
    pub raw_status: i32,
    pub root: bool,
}

#[derive(Clone)]
pub struct SignalRecord {
    pub sequence: u64,
    pub pid: i32,
    pub result: i64,
    pub errno: Option<i32>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub root_pid: Option<i32>,
    pub root_status: Option<i32>,
    pub spawn_errno: Option<i32>,
    pub spawn_in_progress: bool,
    pub admission_closed: bool,
    pub echild: bool,
    pub ownership_known: bool,
    pub stdout_pipe: bool,
    pub stderr_pipe: bool,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub waits: Vec<WaitRecord>,
    pub signals: Vec<SignalRecord>,
    pub issues: Vec<String>,
    pub complete: bool,
}

impl std::fmt::Debug for WaitRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitRecord")
            .field("sequence", &self.sequence)
            .field("pid", &self.pid)
            .field("raw_status", &self.raw_status)
            .field("root", &self.root)
            .finish()
    }
}

impl std::fmt::Debug for SignalRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalRecord")
            .field("sequence", &self.sequence)
            .field("pid", &self.pid)
            .field("result", &self.result)
            .field("errno", &self.errno)
            .finish()
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("root_pid", &self.root_pid)
            .field("root_status", &self.root_status)
            .field("spawn_errno", &self.spawn_errno)
            .field("spawn_in_progress", &self.spawn_in_progress)
            .field("admission_closed", &self.admission_closed)
            .field("echild", &self.echild)
            .field("ownership_known", &self.ownership_known)
            .field("stdout_pipe", &self.stdout_pipe)
            .field("stderr_pipe", &self.stderr_pipe)
            .field("stdout_eof", &self.stdout_eof)
            .field("stderr_eof", &self.stderr_eof)
            .field("stdout", &self.stdout)
            .field("stderr", &self.stderr)
            .field("waits", &self.waits)
            .field("signals", &self.signals)
            .field("issues", &self.issues)
            .field("complete", &self.complete)
            .finish()
    }
}

#[derive(Default)]
struct Output {
    file: Option<File>,
    configured: bool,
    eof: bool,
    bytes: Vec<u8>,
}

impl Output {
    fn attach(&mut self, raw: i32) -> io::Result<()> {
        let file = unsafe { File::from_raw_fd(raw) };
        self.configured = true;
        self.file = Some(file);
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn poll(&mut self) -> io::Result<()> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };
        for _ in 0..16 {
            let remaining = OUTPUT_LIMIT - self.bytes.len();
            if remaining == 0 {
                return Err(io::Error::other(
                    "output ceiling reached; unread fd retained",
                ));
            }
            let mut buffer = [0; 4096];
            let capacity = remaining.min(buffer.len());
            match file.read(&mut buffer[..capacity]) {
                Ok(0) => {
                    self.eof = true;
                    self.file = None;
                    return Ok(());
                }
                Ok(length) => self.bytes.extend_from_slice(&buffer[..length]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

pub struct ControlledHost {
    pid: i32,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    root_pid: Option<i32>,
    root_status: Option<i32>,
    spawn_errno: Option<i32>,
    spawn_attempted: bool,
    spawning: bool,
    admission: bool,
    echild: bool,
    known: bool,
    cancelling: bool,
    deadline: Option<Instant>,
    sequence: u64,
    stdout: Output,
    stderr: Output,
    waits: Vec<WaitRecord>,
    signals: Vec<SignalRecord>,
    issues: Vec<String>,
}

fn children() -> io::Result<BTreeSet<i32>> {
    let mut children = BTreeSet::new();
    // This kernel does not expose task/<tid>/children. Enumerate only numeric
    // process entries, without recursion. This census is for cancellation;
    // only the owner's process-wide wait through ECHILD proves quiescence.
    let owner = unsafe { libc::getpid() };
    for candidate in std::fs::read_dir("/proc")? {
        let candidate = candidate?;
        let Some(pid) = candidate
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read(candidate.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let end = stat
            .windows(2)
            .rposition(|pair| pair == b") ")
            .ok_or_else(|| io::Error::other("malformed process stat"))?;
        let fields = std::str::from_utf8(&stat[end + 2..]).map_err(io::Error::other)?;
        let parent: i32 = fields
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| io::Error::other("missing process parent"))?
            .parse()
            .map_err(io::Error::other)?;
        if parent == owner {
            children.insert(pid);
        }
    }
    Ok(children)
}

impl ControlledHost {
    /// Call only at the start of a separately exec-created fixture, before any
    /// private resource, worker or child is created. The actual fixture source
    /// maintains exclusive spawning/waiting and credential/dumpability policy.
    pub fn enter() -> io::Result<Self> {
        let pid = unsafe { libc::getpid() };
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
        if pid != tid || std::fs::read_dir("/proc/self/task")?.count() != 1 {
            return Err(io::Error::other("host is not its single initial thread"));
        }
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err(io::Error::other("host has a competing SIGCHLD policy"));
        }
        if !children()?.is_empty() {
            return Err(io::Error::other("host already has children"));
        }
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0
            || unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut subreaper: libc::c_int = 0;
        if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut subreaper, 0, 0, 0) } != 0
            || subreaper != 1
            || unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0
        {
            return Err(io::Error::other("host process policy was not installed"));
        }
        Ok(Self {
            pid,
            child: None,
            stdin: None,
            root_pid: None,
            root_status: None,
            spawn_errno: None,
            spawn_attempted: false,
            spawning: false,
            admission: true,
            echild: false,
            known: true,
            cancelling: false,
            deadline: None,
            sequence: 0,
            stdout: Output::default(),
            stderr: Output::default(),
            waits: Vec::new(),
            signals: Vec::new(),
            issues: Vec::new(),
        })
    }

    pub fn spawn_root(&mut self, command: &mut Command) -> io::Result<i32> {
        assert_eq!(unsafe { libc::getpid() }, self.pid);
        if !self.admission || self.spawn_attempted || self.spawning {
            return Err(io::Error::other("root launch admission is closed or used"));
        }
        self.spawn_attempted = true;
        self.spawning = true;
        // Exclusive &mut ownership, synchronous spawn and no reaper thread:
        // std owns its child until spawn returns, including pre_exec failure.
        let result = command.spawn();
        self.spawning = false;
        let child = match result {
            Ok(child) => child,
            Err(error) => {
                self.spawn_errno = error.raw_os_error();
                return Err(error);
            }
        };
        let pid = child.id() as i32;
        self.root_pid = Some(pid);
        self.child = Some(child);
        let child = self.child.as_mut().unwrap();
        self.stdin = child.stdin.take();
        if let Some(stdout) = child.stdout.take() {
            self.stdout.attach(stdout.into_raw_fd())?;
        }
        if let Some(stderr) = child.stderr.take() {
            self.stderr.attach(stderr.into_raw_fd())?;
        }
        Ok(pid)
    }

    pub fn close_root_stdin(&mut self) {
        drop(self.stdin.take());
    }

    pub fn close_admission(&mut self) {
        self.admission = false;
    }

    pub fn cancel(&mut self) {
        self.close_admission();
        self.cancelling = true;
    }

    fn stop_direct_children(&mut self) -> io::Result<()> {
        if !self.known {
            return Err(io::Error::other(
                "cannot signal after wait ownership was lost",
            ));
        }
        for pid in children()? {
            let mut observed: libc::siginfo_t = unsafe { std::mem::zeroed() };
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut observed,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT | libc::__WALL,
                )
            } != 0
            {
                self.known = false;
                return Err(io::Error::last_os_error());
            }
            // No other waiter may reap this child between the ownership check,
            // pidfd_open and signal. Signals use only this retained kernel handle.
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            let errno = (result < 0).then(|| io::Error::last_os_error().raw_os_error().unwrap());
            self.sequence += 1;
            self.signals.push(SignalRecord {
                sequence: self.sequence,
                pid,
                result,
                errno,
            });
            if result < 0 && errno != Some(libc::ESRCH) {
                return Err(io::Error::from_raw_os_error(errno.unwrap()));
            }
        }
        Ok(())
    }

    fn reap_available(&mut self) -> io::Result<()> {
        for _ in 0..256 {
            let mut observed: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_ALL,
                    0,
                    &mut observed,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT | libc::__WALL,
                )
            };
            if result != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.echild = true;
                    if self.root_pid.is_some() && self.root_status.is_none() {
                        self.known = false;
                        return Err(io::Error::other("ECHILD without the owned root status"));
                    }
                    return Ok(());
                }
                self.known = false;
                return Err(error);
            }
            self.echild = false;
            let pid = unsafe { observed.si_pid() };
            if pid == 0 {
                return Ok(());
            }
            let mut status = 0;
            let reaped = unsafe { libc::waitpid(pid, &mut status, libc::__WALL | libc::WNOHANG) };
            if reaped != pid {
                self.known = false;
                return Err(io::Error::other(format!(
                    "owned wait for {pid} returned {reaped}"
                )));
            }
            let root = self.root_pid == Some(pid);
            if root {
                if self.root_status.replace(status).is_some() {
                    self.known = false;
                    return Err(io::Error::other("root status was consumed twice"));
                }
                // Child's Drop does not wait or signal. We never call its wait
                // methods after the successful std spawn ownership handoff.
                drop(self.child.take());
            }
            self.sequence += 1;
            self.waits.push(WaitRecord {
                sequence: self.sequence,
                pid,
                raw_status: status,
                root,
            });
        }
        Ok(())
    }

    pub fn poll(&mut self, deadline: Instant) -> io::Result<()> {
        assert_eq!(unsafe { libc::getpid() }, self.pid);
        assert!(!self.spawning, "reaper entered an in-progress spawn");
        // The first caller deadline is retained across polling, cancellation
        // and draining. A subsequent call may tighten it, never extend it.
        let deadline = *self
            .deadline
            .insert(self.deadline.map_or(deadline, |old| old.min(deadline)));
        let result = (|| {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "original owner deadline expired",
                ));
            }
            if self.cancelling {
                self.stop_direct_children()?;
            }
            self.reap_available()?;
            self.stdout.poll()?;
            self.stderr.poll()
        })();
        if let Err(error) = &result {
            self.issues.push(error.to_string());
        }
        result
    }

    pub fn snapshot(&self) -> Snapshot {
        let complete = !self.admission
            && !self.spawning
            && self.echild
            && self.known
            && (self.root_pid.is_none() || self.root_status.is_some())
            && self.stdout.file.is_none()
            && self.stderr.file.is_none()
            && self.issues.is_empty();
        Snapshot {
            root_pid: self.root_pid,
            root_status: self.root_status,
            spawn_errno: self.spawn_errno,
            spawn_in_progress: self.spawning,
            admission_closed: !self.admission,
            echild: self.echild,
            ownership_known: self.known,
            stdout_pipe: self.stdout.configured,
            stderr_pipe: self.stderr.configured,
            stdout_eof: self.stdout.eof,
            stderr_eof: self.stderr.eof,
            stdout: self.stdout.bytes.clone(),
            stderr: self.stderr.bytes.clone(),
            waits: self.waits.clone(),
            signals: self.signals.clone(),
            issues: self.issues.clone(),
            complete,
        }
    }

    /// On error this borrowed owner, its captured bytes and unreaped authority
    /// remain with the caller. No new cleanup deadline, detach or Drop signal.
    pub fn drain_until(&mut self, deadline: Instant) -> io::Result<Snapshot> {
        loop {
            self.poll(deadline)?;
            let snapshot = self.snapshot();
            if snapshot.complete {
                return Ok(snapshot);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
