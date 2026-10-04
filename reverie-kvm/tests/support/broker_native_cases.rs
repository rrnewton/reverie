//! PRIVATE UNARMED test-body proposal. Included by the existing early launcher,
//! before libtest or any guest resources. This file does not bootstrap a broker.
//! The caller retains BrokerOwner and uses its existing shutdown/actual-wait path.
//! Failure returns all pending native/file owners; do not format-and-drop it.
use std::fs::File;
use std::io::Read;
use std::net::TcpListener;
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use reverie_kvm::native_exit_broker::BrokerClient;
use reverie_kvm::native_exit_broker::BrokerOwner;
use reverie_kvm::native_exit_broker::CoreError;
use reverie_kvm::native_exit_broker::JobProgress;
use reverie_kvm::native_exit_broker::JobStage;
use reverie_kvm::native_exit_broker::NativeExitJob;
use reverie_kvm::native_exit_broker::NativeExitReceipt;
use reverie_kvm::native_exit_broker::ReservationProgress;
use reverie_kvm::native_exit_broker::RetryReason;
use reverie_kvm::native_exit_broker::SocketReference;

const OBSERVATION: Duration = Duration::from_secs(5);
const MAX_QUEUED: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub struct Resources {
    job: Option<NativeExitJob>,
    files: Vec<SocketReference>,
    tcp: Option<QueuedTcp>,
    unix_peer: Option<UnixStream>,
    foreign: Option<Arc<File>>,
    worker_limit: Option<WorkerLimit>,
}
struct WorkerLimit {
    pid: libc::pid_t,
    start_ticks: u64,
    _pidfd: std::os::fd::OwnedFd,
    original: libc::rlimit,
    applied: bool,
}
#[must_use = "retain protocol/files on an unconfirmed cleanup failure"]
pub struct CaseFailure {
    pub message: String,
    pub resources: Resources,
}
#[derive(Debug)]
pub struct CaseReceipt {
    pub case: String,
    pub waits: Vec<NativeExitReceipt>,
    pub sent: usize,
    pub outq: i32,
    pub notsent: i32,
    pub first_chunk_acknowledged: bool,
    pub fault: Option<FaultRecord>,
}

#[derive(Debug)]
pub struct FaultRecord {
    pub limited_worker_pid: i32,
    pub limited_worker_start_ticks: u64,
    pub private_fds: Vec<i32>,
    pub applied_soft: u64,
    pub original_soft: u64,
    pub original_hard: u64,
}
fn reserved_worker_identity(job: &NativeExitJob) -> Result<(i32, u64, Vec<i32>), String> {
    // READY-bound PID from the actual private protocol, never guessed by name.
    let pid = job
        .native_worker_pid()
        .ok_or("READY lacks authenticated worker PID")?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(io)?;
    let rest = stat.rsplit_once(") ").ok_or("malformed worker stat")?.1;
    let fields: Vec<_> = rest.split_whitespace().collect();
    let start = fields
        .get(19)
        .ok_or("worker start identity absent")?
        .parse::<u64>()
        .map_err(|e| e.to_string())?;
    let mut fds: Vec<i32> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map_err(io)?
        .map(|entry| {
            entry.map_err(io).and_then(|e| {
                e.file_name()
                    .to_string_lossy()
                    .parse::<i32>()
                    .map_err(|e| e.to_string())
            })
        })
        .collect::<Result<_, _>>()?;
    fds.sort_unstable();
    require(
        !fds.is_empty() && fds.iter().all(|fd| *fd >= 0),
        "owned worker descriptor census",
    )?;
    Ok((pid, start, fds))
}
fn require(ok: bool, why: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(why.to_owned()) }
}
fn io(error: std::io::Error) -> String {
    error.to_string()
}
fn core(error: CoreError) -> String {
    format!("{} errno={}", error.operation, error.errno)
}
fn socket_option(fd: RawFd, option: i32, value: i32) -> Result<(), String> {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            option,
            (&value as *const i32).cast(),
            std::mem::size_of::<i32>() as _,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io(std::io::Error::last_os_error()))
    }
}
fn linger(fd: RawFd) -> Result<(i32, i32), String> {
    let mut value = libc::linger {
        l_onoff: 0,
        l_linger: 0,
    };
    let mut size = std::mem::size_of_val(&value) as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&mut value as *mut libc::linger).cast(),
            &mut size,
        )
    };
    require(
        rc == 0 && size as usize == std::mem::size_of_val(&value),
        "exact SO_LINGER query",
    )?;
    Ok((value.l_onoff, value.l_linger))
}
fn wait_io(fd: RawFd, events: i16, deadline: Instant) -> Result<(), String> {
    let left = deadline.saturating_duration_since(Instant::now());
    require(
        !left.is_zero(),
        "fixed observation expired; no native completion inferred",
    )?;
    let mut p = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let rc = unsafe { libc::poll(&mut p, 1, left.as_millis().min(10).max(1) as i32) };
    if rc >= 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
        Ok(())
    } else {
        Err(io(std::io::Error::last_os_error()))
    }
}
fn wait_job(job: &NativeExitJob, deadline: Instant) -> Result<(), String> {
    let left = deadline.saturating_duration_since(Instant::now());
    require(!left.is_zero(), "fixed native-job observation expired")?;
    let mut interests = job.poll_interests();
    let millis = job
        .retry_after()
        .unwrap_or(Duration::from_millis(10))
        .min(left)
        .as_millis()
        .clamp(1, 10) as i32;
    let rc = unsafe { libc::poll(interests.as_mut_ptr(), interests.len() as _, millis) };
    if rc >= 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
        Ok(())
    } else {
        Err(io(std::io::Error::last_os_error()))
    }
}
fn reserve(r: &mut Resources, client: &BrokerClient, deadline: Instant) -> Result<(), String> {
    r.job = Some(client.reserve_worker().map_err(core)?);
    let job = r.job.as_mut().unwrap();
    loop {
        match job.advance_reservation().map_err(core)? {
            ReservationProgress::Ready => return Ok(()),
            ReservationProgress::Pending => wait_job(job, deadline)?,
        }
    }
}
fn complete(r: &mut Resources, deadline: Instant) -> Result<NativeExitReceipt, String> {
    let job = r.job.as_mut().unwrap();
    loop {
        match job.advance(&mut r.files).map_err(core)? {
            JobProgress::Complete(receipt) => {
                require(
                    receipt.native_pid > 0 && receipt.job > 0 && receipt.raw_wait_status == 0,
                    "exact successful native wait receipt",
                )?;
                require(
                    job.parent_references_retired() && r.files.is_empty(),
                    "all supplied references retired",
                )?;
                return Ok(receipt);
            }
            JobProgress::Pending => wait_job(job, deadline)?,
        }
    }
}

struct QueuedTcp {
    peer: TcpStream,
    expected: Vec<u8>,
    outq: i32,
    notsent: i32,
}
impl QueuedTcp {
    // Creates the owner and stores it in Resources BEFORE any queued writes, so
    // all later errors return it rather than running a lingering local Drop.
    fn setup(r: &mut Resources) -> Result<(), String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(io)?;
        socket_option(listener.as_raw_fd(), libc::SO_RCVBUF, 4096)?;
        let sender = TcpStream::connect(listener.local_addr().map_err(io)?).map_err(io)?;
        let (peer, _) = listener.accept().map_err(io)?;
        socket_option(sender.as_raw_fd(), libc::SO_SNDBUF, 4096)?;
        sender.set_nonblocking(true).map_err(io)?;
        peer.set_nonblocking(true).map_err(io)?;
        r.files.push(SocketReference::Owned(unsafe {
            File::from_raw_fd(sender.into_raw_fd())
        }));
        r.tcp = Some(Self {
            peer,
            expected: Vec::new(),
            outq: 0,
            notsent: 0,
        });
        let fd = r.files[0].as_raw_fd();
        let option = libc::linger {
            l_onoff: 1,
            l_linger: 600,
        };
        require(
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    (&option as *const libc::linger).cast(),
                    std::mem::size_of_val(&option) as _,
                )
            } == 0,
            "enable exact native SO_LINGER",
        )?;
        let deadline = Instant::now() + OBSERVATION;
        let tcp = r.tcp.as_mut().unwrap();
        loop {
            require(
                Instant::now() < deadline && tcp.expected.len() < MAX_QUEUED,
                "bounded native TCP setup",
            )?;
            let bytes: Vec<u8> = (0..4096)
                .map(|n| ((tcp.expected.len() + n) % 251) as u8)
                .collect();
            let count =
                unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL) };
            if count > 0 {
                tcp.expected.extend_from_slice(&bytes[..count as usize]);
                continue;
            }
            let errno = std::io::Error::last_os_error().raw_os_error();
            if errno == Some(libc::EINTR) {
                continue;
            }
            require(
                count < 0 && errno == Some(libc::EAGAIN),
                "TCP fill stops only at EAGAIN",
            )?;
            break;
        }
        require(
            unsafe { libc::ioctl(fd, 0x5411, &mut tcp.outq) } == 0,
            "native SIOCOUTQ",
        )?;
        require(
            unsafe { libc::ioctl(fd, 0x894b, &mut tcp.notsent) } == 0,
            "native SIOCOUTQNSD",
        )?;
        require(
            !tcp.expected.is_empty() && tcp.outq > 0 && tcp.notsent > 0,
            "positive queued and not-sent bytes",
        )?;
        require(
            linger(fd)? == (1, 600),
            "SO_LINGER unchanged before transfer",
        )
    }
    fn append_foreign_byte(&mut self, fd: RawFd) -> Result<(), String> {
        // Call only after drain_payload: EAGAIN here is a real failure, not retried.
        let byte = [193u8];
        require(
            unsafe { libc::send(fd, byte.as_ptr().cast(), 1, libc::MSG_NOSIGNAL) } == 1,
            "surviving foreign Arc remains writable",
        )?;
        self.expected.push(193);
        Ok(())
    }
    fn drain_payload(&mut self, received: &mut Vec<u8>, deadline: Instant) -> Result<(), String> {
        while received.len() < self.expected.len() {
            let mut buffer = [0u8; 8192];
            match self.peer.read(&mut buffer) {
                Ok(0) => return Err("premature EOF before complete exact stream".into()),
                Ok(n) => received.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_io(self.peer.as_raw_fd(), libc::POLLIN, deadline)?
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io(e)), // ECONNRESET is never accepted.
            }
        }
        require(
            *received == self.expected,
            "whole stream exact, no drop/corruption",
        )
    }
    fn eof(&mut self, deadline: Instant) -> Result<(), String> {
        loop {
            let mut byte = [0u8];
            match self.peer.read(&mut byte) {
                Ok(0) => return Ok(()),
                Ok(_) => return Err("unexpected trailing stream byte".into()),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_io(self.peer.as_raw_fd(), libc::POLLIN, deadline)?
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io(e)),
            }
        }
    }
}

fn queue_right(socket: RawFd, right: RawFd) -> Result<(), String> {
    let mut payload = [b'R'];
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as _) } as _;
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    require(!header.is_null(), "SCM_RIGHTS header fits")?;
    unsafe {
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as _) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), right);
    }
    require(
        unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL) } == 1,
        "queue one real native SCM_RIGHTS owner",
    )
}

/// Called only by the isolated, already-bootstrapped launcher. The caller keeps
/// any returned failure resources alive while its existing guard settles them.
pub fn run_queued_case(case: &str, client: &BrokerClient) -> Result<CaseReceipt, CaseFailure> {
    let mut resources = Resources::default();
    let result = (|| -> Result<CaseReceipt, String> {
        QueuedTcp::setup(&mut resources)?;
        let tcp = resources.tcp.as_ref().unwrap();
        let mut receipt = CaseReceipt {
            case: case.into(),
            waits: Vec::new(),
            sent: tcp.expected.len(),
            outq: tcp.outq,
            notsent: tcp.notsent,
            first_chunk_acknowledged: false,
            fault: None,
        };
        match case {
            "single" => {}
            "shared-arc" => {
                let SocketReference::Owned(file) = resources.files.remove(0) else {
                    unreachable!()
                };
                let file = Arc::new(file);
                resources.files.extend([
                    SocketReference::Shared(file.clone()),
                    SocketReference::Shared(file.clone()),
                ]);
                resources.foreign = Some(file);
            }
            "nested-scm" => {
                let (sender, receiver) = UnixStream::pair().map_err(io)?;
                queue_right(sender.as_raw_fd(), resources.files[0].as_raw_fd())?;
                // The queued SCM copy already owns the TCP file, so this is not
                // its last close. The test does not receive the queued right.
                resources.files.clear();
                drop(sender);
                resources.files.push(SocketReference::Owned(unsafe {
                    File::from_raw_fd(receiver.into_raw_fd())
                }));
            }
            _ => return Err("unknown exact native queued case".into()),
        }
        reserve(&mut resources, client, Instant::now() + OBSERVATION)?;
        let expected_refs = if case == "shared-arc" { 2 } else { 1 };
        let native = complete(&mut resources, Instant::now() + OBSERVATION)?;
        require(
            native.socket_references == expected_refs && native.transferred_descriptors == 1,
            "exact Arc owner count and distinct kernel-right count",
        )?;
        receipt.waits.push(native);
        // Successful wait was observed BEFORE permitting any peer drain.
        let mut received = Vec::new();
        resources
            .tcp
            .as_mut()
            .unwrap()
            .drain_payload(&mut received, Instant::now() + OBSERVATION)?;
        if case == "shared-arc" {
            let fd = resources.foreign.as_ref().unwrap().as_raw_fd();
            require(linger(fd)? == (1, 600), "foreign owner SO_LINGER unchanged")?;
            resources.tcp.as_mut().unwrap().append_foreign_byte(fd)?;
            resources
                .files
                .push(SocketReference::Shared(resources.foreign.take().unwrap()));
            reserve(&mut resources, client, Instant::now() + OBSERVATION)?;
            receipt
                .waits
                .push(complete(&mut resources, Instant::now() + OBSERVATION)?);
            resources
                .tcp
                .as_mut()
                .unwrap()
                .drain_payload(&mut received, Instant::now() + OBSERVATION)?;
        }
        resources
            .tcp
            .as_mut()
            .unwrap()
            .eof(Instant::now() + OBSERVATION)?;
        Ok(receipt)
    })();
    result.map_err(|message| CaseFailure { message, resources })
}

// Chunk/fault body is deliberately distinct from queued-TCP causal cases.
// Resource pressure is confined to the READY-bound owned worker. The broker,
// launcher and foreign process limits are never changed.
pub fn run_chunk_abort_case(owner: &BrokerOwner) -> Result<CaseReceipt, CaseFailure> {
    let mut r = Resources::default();
    let result = (|| -> Result<CaseReceipt, String> {
        let (socket, peer) = UnixStream::pair().map_err(io)?;
        peer.set_nonblocking(true).map_err(io)?;
        r.unix_peer = Some(peer);
        let first = unsafe { File::from_raw_fd(socket.into_raw_fd()) };
        for _ in 0..253 {
            r.files
                .push(SocketReference::Owned(first.try_clone().map_err(io)?));
        }
        r.files.push(SocketReference::Owned(first));
        let identities: Vec<_> = r.files.iter().map(AsRawFd::as_raw_fd).collect();
        reserve(&mut r, &owner.client(), Instant::now() + OBSERVATION)?;
        let (limited_pid, limited_start, private_fds) =
            reserved_worker_identity(r.job.as_ref().unwrap())?;
        // Keep a kernel reference to this exact generation throughout mutation.
        let pidfd_raw = unsafe { libc::syscall(libc::SYS_pidfd_open, limited_pid, 0) };
        require(pidfd_raw >= 0, "open exact READY-worker pidfd")?;
        let pidfd = unsafe { std::os::fd::OwnedFd::from_raw_fd(pidfd_raw as i32) };
        require(
            reserved_worker_identity(r.job.as_ref().unwrap())?
                == (limited_pid, limited_start, private_fds.clone()),
            "worker generation and held fdset stable before limit mutation",
        )?;
        let mut baseline: libc::rlimit = unsafe { std::mem::zeroed() };
        require(
            unsafe {
                libc::prlimit(
                    limited_pid,
                    libc::RLIMIT_NOFILE,
                    std::ptr::null(),
                    &mut baseline,
                )
            } == 0,
            "read only owned worker NOFILE",
        )?;
        let soft = private_fds.len() as u64 + 253;
        require(
            baseline.rlim_cur > soft && private_fds.iter().all(|fd| (*fd as u64) < soft),
            "fd budget fits exactly one253-right chunk",
        )?;
        let limited = libc::rlimit {
            rlim_cur: soft,
            rlim_max: baseline.rlim_max,
        };
        r.worker_limit = Some(WorkerLimit {
            pid: limited_pid,
            start_ticks: limited_start,
            _pidfd: pidfd,
            original: baseline,
            applied: false,
        });
        require(
            unsafe {
                libc::prlimit(
                    limited_pid,
                    libc::RLIMIT_NOFILE,
                    &limited,
                    std::ptr::null_mut(),
                )
            } == 0,
            "limit only authenticated READY worker",
        )?;
        r.worker_limit.as_mut().unwrap().applied = true;
        let deadline = Instant::now() + OBSERVATION;
        let mut first_ack = false;
        // Do not advance ABORT until the exact limit has been restored. Errors
        // also restore while the protocol still owns this live generation.
        let induce = (|| -> Result<(), String> {
            loop {
                let job = r.job.as_mut().unwrap();
                require(
                    job.attempts() == 1 && !job.parent_references_retired(),
                    "first attempt originals intact",
                )?;
                let progress = job.advance(&mut r.files).map_err(core)?;
                require(
                    matches!(progress, JobProgress::Pending),
                    "fault cannot complete first attempt",
                )?;
                if job.acknowledged_prefix() == 253 {
                    first_ack = true;
                }
                require(
                    r.files
                        .iter()
                        .map(AsRawFd::as_raw_fd)
                        .eq(identities.iter().copied()),
                    "all254 original owners retained in order",
                )?;
                for fd in &identities {
                    require(
                        unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0,
                        "every original fd still open",
                    )?;
                }
                if job.stage() == JobStage::Abort {
                    require(
                        first_ack && job.acknowledged_prefix() == 253,
                        "real first253 ACK precedes second provisioning failure",
                    )?;
                    require(
                        matches!(job.retry_reason(), Some(RetryReason::Resource(e))
                        if e.errno == libc::EMFILE && e.operation == "pre-ACK native worker"),
                        "exact measured second-chunk provisioning EMFILE",
                    )?;
                    return Ok(());
                }
                wait_job(job, deadline)?;
            }
        })();
        // Never issue ABORT or another transfer before restoration readback.
        let stat = std::fs::read_to_string(format!("/proc/{limited_pid}/stat")).map_err(io)?;
        let fields: Vec<_> = stat
            .rsplit_once(") ")
            .ok_or("worker stat")?
            .1
            .split_whitespace()
            .collect();
        require(
            fields[19].parse::<u64>().map_err(|e| e.to_string())? == limited_start,
            "same live worker before exact limit restoration",
        )?;
        let restore_rc = unsafe {
            libc::prlimit(
                limited_pid,
                libc::RLIMIT_NOFILE,
                &baseline,
                std::ptr::null_mut(),
            )
        };
        require(
            restore_rc == 0,
            "restore exact owned worker NOFILE before abort",
        )?;
        let mut readback: libc::rlimit = unsafe { std::mem::zeroed() };
        require(
            unsafe {
                libc::prlimit(
                    limited_pid,
                    libc::RLIMIT_NOFILE,
                    std::ptr::null(),
                    &mut readback,
                )
            } == 0
                && readback.rlim_cur == baseline.rlim_cur
                && readback.rlim_max == baseline.rlim_max,
            "exact limit restoration readback",
        )?;
        r.worker_limit.as_mut().unwrap().applied = false;
        r.worker_limit = None;
        induce?;
        let mut abort = None;
        let native = loop {
            let job = r.job.as_mut().unwrap();
            let progress = job.advance(&mut r.files).map_err(core)?;
            if !job.parent_references_retired() {
                require(
                    r.files
                        .iter()
                        .map(AsRawFd::as_raw_fd)
                        .eq(identities.iter().copied()),
                    "retry preserves original owner vector",
                )?;
            }
            if job.attempts() == 2 && abort.is_none() {
                let actual = job
                    .last_native_status()
                    .ok_or("retry lacks actual previous worker wait")?;
                require(
                    actual.raw_wait_status == 0 && actual.native_pid == limited_pid,
                    "actual exact old worker abort wait before new attempt",
                )?;
                abort = Some(actual);
            }
            require(job.attempts() <= 2, "exact one induced retry")?;
            if let JobProgress::Complete(receipt) = progress {
                break receipt;
            }
            wait_job(job, deadline)?;
        };
        let aborted = abort.ok_or("induced resource abort was not observed")?;
        require(
            native.job != aborted.job
                && native.raw_wait_status == 0
                && native.socket_references == 254
                && native.transferred_descriptors == 254
                && r.files.is_empty(),
            "new job and exact complete254 receipt",
        )?;
        let eof_deadline = Instant::now() + OBSERVATION;
        loop {
            let mut byte = [0u8];
            match r.unix_peer.as_mut().unwrap().read(&mut byte) {
                Ok(0) => break,
                Ok(_) => return Err("unexpected chunk-test payload".into()),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => wait_io(
                    r.unix_peer.as_ref().unwrap().as_raw_fd(),
                    libc::POLLIN,
                    eof_deadline,
                )?,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io(e)),
            }
        }
        Ok(CaseReceipt {
            case: "chunk-resource-abort".into(),
            waits: vec![aborted, native],
            sent: 0,
            outq: 0,
            notsent: 0,
            first_chunk_acknowledged: first_ack,
            fault: Some(FaultRecord {
                limited_worker_pid: limited_pid,
                limited_worker_start_ticks: limited_start,
                private_fds,
                applied_soft: soft,
                original_soft: baseline.rlim_cur,
                original_hard: baseline.rlim_max,
            }),
        })
    })();
    result.map_err(|message| CaseFailure {
        message,
        resources: r,
    })
}
