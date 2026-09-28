//! Ordinary kernel controls for an installed-runtime fixture.
//!
//! The caller owns a fresh, single-threaded process and a ten-second outer
//! deadline, including forced cleanup. The original blocking GETEVENTS call is
//! retained. This module creates no process or user thread and emits no output.
//! It does not designate any descriptor as private or claim transport security.

use std::ffi::CString;
use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

const REGISTER_FILES: u32 = 2;
const UPDATE: u32 = 6;
const UPDATE2: u32 = 14;
const SQPOLL: u32 = 2;
const SINGLE_MMAP: u32 = 1;
const SQPOLL_NONFIXED: u32 = 1 << 7;
const FIXED_FILE: u8 = 1;
const ASYNC: u8 = 1 << 4;
const GETEVENTS: u32 = 1;
const SQ_WAKEUP: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed(String),
    /// The operation was not measured. This is never accepted by all_passed.
    Unavailable(String),
}

#[derive(Debug)]
pub struct Report {
    pub cases: Vec<CaseReport>,
}

impl Report {
    pub fn all_passed(&self) -> bool {
        !self.cases.is_empty() && self.cases.iter().all(|c| c.outcome == Outcome::Passed)
    }
}

#[derive(Debug)]
pub struct CaseReport {
    pub name: &'static str,
    pub outcome: Outcome,
    pub observations: Vec<Observation>,
}

// Every field is retained by Debug for the caller's evidence file. Individual
// fixture binaries may not otherwise destructure these observation variants.
#[allow(dead_code)]
#[derive(Debug)]
pub enum Observation {
    Syscall {
        name: &'static str,
        arguments: [u64; 6],
        /// Kernel convention: errno is negative; successful returns are exact.
        result: i64,
    },
    Setup(Params),
    Bytes {
        name: &'static str,
        address: u64,
        bytes: Vec<u8>,
    },
    Queue {
        phase: &'static str,
        sq_head: u32,
        sq_tail: u32,
        cq_head: u32,
        cq_tail: u32,
        sq_dropped: u32,
        cq_overflow: u32,
    },
    Completion(Cqe),
    Fdinfo {
        phase: &'static str,
        text: String,
    },
    Fact {
        name: &'static str,
        value: String,
    },
}

type Check = Result<(), Outcome>;

impl CaseReport {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            outcome: Outcome::Failed("case did not complete".into()),
            observations: Vec::new(),
        }
    }

    fn fact(&mut self, name: &'static str, value: impl ToString) {
        self.observations.push(Observation::Fact {
            name,
            value: value.to_string(),
        });
    }

    fn bytes(&mut self, name: &'static str, address: u64, bytes: &[u8]) {
        self.observations.push(Observation::Bytes {
            name,
            address,
            bytes: bytes.to_vec(),
        });
    }

    fn syscall(&mut self, name: &'static str, nr: i64, arguments: [u64; 6]) -> i64 {
        // All callers supply initialized, live objects for the syscall ABI.
        let value = unsafe {
            libc::syscall(
                nr,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3],
                arguments[4],
                arguments[5],
            )
        };
        let result = if value == -1 {
            -i64::from(io::Error::last_os_error().raw_os_error().unwrap())
        } else {
            value
        };
        self.observations.push(Observation::Syscall {
            name,
            arguments,
            result,
        });
        result
    }

    fn register(&mut self, fd: i32, opcode: u32, pointer: u64, count: u32) -> i64 {
        self.syscall(
            "io_uring_register",
            libc::SYS_io_uring_register,
            [
                fd as u64,
                u64::from(opcode),
                pointer,
                u64::from(count),
                0,
                0,
            ],
        )
    }

    fn fdinfo(&mut self, fd: i32, phase: &'static str) -> Result<String, Outcome> {
        let path = format!("/proc/self/fdinfo/{fd}");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Outcome::Failed(format!("read {path}: {e}")))?;
        self.observations.push(Observation::Fdinfo {
            phase,
            text: text.clone(),
        });
        Ok(text)
    }
}

fn require(condition: bool, message: impl ToString) -> Check {
    if condition {
        Ok(())
    } else {
        Err(Outcome::Failed(message.to_string()))
    }
}

fn unavailable(operation: &str, result: i64) -> Outcome {
    Outcome::Unavailable(format!(
        "{operation} returned {result}; operation unmeasured"
    ))
}

fn own_fd(result: i64, operation: &str) -> Result<OwnedFd, Outcome> {
    if result < 0 {
        return Err(Outcome::Failed(format!("{operation} returned {result}")));
    }
    // A successful FD-producing syscall transfers this newly created descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(result as i32) })
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct SqOffsets {
    head: u32,
    tail: u32,
    mask: u32,
    entries: u32,
    flags: u32,
    dropped: u32,
    array: u32,
    resv: u32,
    user_addr: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct CqOffsets {
    head: u32,
    tail: u32,
    mask: u32,
    entries: u32,
    overflow: u32,
    cqes: u32,
    flags: u32,
    resv: u32,
    user_addr: u64,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct Params {
    sq_entries: u32,
    cq_entries: u32,
    flags: u32,
    sq_thread_cpu: u32,
    sq_thread_idle: u32,
    features: u32,
    wq_fd: u32,
    resv: [u32; 3],
    sq_off: SqOffsets,
    cq_off: CqOffsets,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Sqe {
    opcode: u8,
    flags: u8,
    ioprio: u16,
    fd: i32,
    off: u64,
    addr: u64,
    len: u32,
    msg_flags: u32,
    user_data: u64,
    buf_index: u16,
    personality: u16,
    file_index: u32,
    addr3: u64,
    pad2: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Cqe {
    pub user_data: u64,
    pub result: i32,
    pub flags: u32,
}

const _: () = assert!(size_of::<Params>() == 120);
const _: () = assert!(size_of::<Sqe>() == 64);
const _: () = assert!(size_of::<Cqe>() == 16);

struct Mapping {
    address: *mut u8,
    length: usize,
}

impl Mapping {
    fn new(fd: i32, offset: i64, length: usize) -> Result<Self, Outcome> {
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                offset,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(Outcome::Failed(format!(
                "mmap offset={offset} length={length}: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(Self {
            address: address.cast(),
            length,
        })
    }

    fn checked<T>(&self, offset: usize, count: usize) -> Result<*mut T, Outcome> {
        let end = size_of::<T>()
            .checked_mul(count)
            .and_then(|bytes| offset.checked_add(bytes));
        require(
            end.is_some_and(|end| end <= self.length)
                && offset.is_multiple_of(std::mem::align_of::<T>()),
            "kernel ring offset or length is outside its mapping",
        )?;
        Ok(unsafe { self.address.add(offset).cast() })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.address.cast(), self.length) };
    }
}

struct Ring {
    fd: OwnedFd,
    params: Params,
    sq: Mapping,
    cq: Option<Mapping>,
    sqes: Mapping,
}

fn setup(case: &mut CaseReport, entries: u32, flags: u32) -> Result<(OwnedFd, Params), Outcome> {
    let mut params = Params {
        flags,
        ..Params::default()
    };
    let result = case.syscall(
        "io_uring_setup",
        libc::SYS_io_uring_setup,
        [
            u64::from(entries),
            ptr::from_mut(&mut params) as u64,
            0,
            0,
            0,
            0,
        ],
    );
    case.observations.push(Observation::Setup(params));
    if result < 0 {
        return Err(unavailable("io_uring_setup", result));
    }
    let fd = own_fd(result, "io_uring_setup")?;
    require(params.flags == flags, "setup changed requested flags")?;
    require(
        params.sq_entries.is_power_of_two() && params.cq_entries.is_power_of_two(),
        "setup did not return power-of-two ring sizes",
    )?;
    Ok((fd, params))
}

impl Ring {
    fn new(case: &mut CaseReport, flags: u32) -> Result<Self, Outcome> {
        let (fd, params) = setup(case, 4, flags)?;
        let sq_size = params.sq_off.array as usize + params.sq_entries as usize * 4;
        let cq_size = params.cq_off.cqes as usize + params.cq_entries as usize * 16;
        let single = params.features & SINGLE_MMAP != 0;
        let sq = Mapping::new(
            fd.as_raw_fd(),
            0,
            if single {
                sq_size.max(cq_size)
            } else {
                sq_size
            },
        )?;
        let cq = if single {
            None
        } else {
            Some(Mapping::new(fd.as_raw_fd(), 0x0800_0000, cq_size)?)
        };
        let sqes = Mapping::new(fd.as_raw_fd(), 0x1000_0000, params.sq_entries as usize * 64)?;
        let ring = Self {
            fd,
            params,
            sq,
            cq,
            sqes,
        };
        for offset in [
            params.sq_off.head,
            params.sq_off.tail,
            params.sq_off.mask,
            params.sq_off.entries,
            params.sq_off.flags,
            params.sq_off.dropped,
        ] {
            ring.sq.checked::<AtomicU32>(offset as usize, 1)?;
        }
        for offset in [
            params.cq_off.head,
            params.cq_off.tail,
            params.cq_off.mask,
            params.cq_off.entries,
            params.cq_off.overflow,
        ] {
            ring.cq_map().checked::<AtomicU32>(offset as usize, 1)?;
        }
        ring.sq
            .checked::<u32>(params.sq_off.array as usize, params.sq_entries as usize)?;
        ring.cq_map()
            .checked::<Cqe>(params.cq_off.cqes as usize, params.cq_entries as usize)?;
        ring.sqes.checked::<Sqe>(0, params.sq_entries as usize)?;
        require(
            ring.sq_load(params.sq_off.mask) == params.sq_entries - 1,
            "SQ mask mismatch",
        )?;
        require(
            ring.cq_load(params.cq_off.mask) == params.cq_entries - 1,
            "CQ mask mismatch",
        )?;
        Ok(ring)
    }

    fn cq_map(&self) -> &Mapping {
        self.cq.as_ref().unwrap_or(&self.sq)
    }

    fn sq_atomic(&self, offset: u32) -> &AtomicU32 {
        // Ring::new checked the alignment and bounds of each atomic field.
        unsafe { &*self.sq.address.add(offset as usize).cast::<AtomicU32>() }
    }

    fn cq_atomic(&self, offset: u32) -> &AtomicU32 {
        unsafe {
            &*self
                .cq_map()
                .address
                .add(offset as usize)
                .cast::<AtomicU32>()
        }
    }

    fn sq_load(&self, offset: u32) -> u32 {
        self.sq_atomic(offset).load(Ordering::Acquire)
    }
    fn cq_load(&self, offset: u32) -> u32 {
        self.cq_atomic(offset).load(Ordering::Acquire)
    }

    fn queue(&self, case: &mut CaseReport, phase: &'static str) -> (u32, u32, u32, u32) {
        let p = &self.params;
        let values = (
            self.sq_load(p.sq_off.head),
            self.sq_load(p.sq_off.tail),
            self.cq_load(p.cq_off.head),
            self.cq_load(p.cq_off.tail),
        );
        case.observations.push(Observation::Queue {
            phase,
            sq_head: values.0,
            sq_tail: values.1,
            cq_head: values.2,
            cq_tail: values.3,
            sq_dropped: self.sq_load(p.sq_off.dropped),
            cq_overflow: self.cq_load(p.cq_off.overflow),
        });
        values
    }

    fn send(
        &self,
        case: &mut CaseReport,
        fd: i32,
        peer: i32,
        flags: u8,
        payload: &'static [u8],
        token: u64,
    ) -> Check {
        // Static payload lifetime also covers an interrupted/failed enter while
        // the kernel still owns a pending SEND. Never free a borrowed buffer.
        let (head, tail, cq_head, cq_tail) = self.queue(case, "before SEND");
        require(
            head == tail && cq_head == cq_tail,
            "SEND did not begin with empty queues",
        )?;
        let index = tail & (self.params.sq_entries - 1);
        let sqe = Sqe {
            opcode: 26,
            flags,
            ioprio: 0,
            fd,
            off: 0,
            addr: payload.as_ptr() as u64,
            len: payload.len() as u32,
            msg_flags: libc::MSG_NOSIGNAL as u32,
            user_data: token,
            buf_index: 0,
            personality: 0,
            file_index: 0,
            addr3: 0,
            pad2: 0,
        };
        let target = self.sqes.checked::<Sqe>(index as usize * 64, 1)?;
        unsafe {
            ptr::write(target, sqe);
            ptr::write(
                self.sq
                    .checked::<u32>(
                        self.params.sq_off.array as usize,
                        self.params.sq_entries as usize,
                    )?
                    .add(index as usize),
                index,
            );
            case.bytes(
                "submitted SQE",
                target as u64,
                std::slice::from_raw_parts(target.cast(), 64),
            );
        }
        case.bytes("original SEND payload", payload.as_ptr() as u64, payload);
        self.sq_atomic(self.params.sq_off.tail)
            .store(tail.wrapping_add(1), Ordering::Release);
        let enter_flags = GETEVENTS
            | if self.params.flags & SQPOLL != 0 {
                SQ_WAKEUP
            } else {
                0
            };
        let result = case.syscall(
            "io_uring_enter",
            libc::SYS_io_uring_enter,
            [
                self.fd.as_raw_fd() as u64,
                1,
                1,
                u64::from(enter_flags),
                0,
                0,
            ],
        );
        require(
            result == 1,
            format!("enter must submit exactly one operation, got {result}"),
        )?;
        let (_, _, c_head, c_tail) = self.queue(case, "after enter");
        require(c_tail.wrapping_sub(c_head) == 1, "expected exactly one CQE")?;
        let cq_index = c_head & (self.params.cq_entries - 1);
        let cqe_pointer = self.cq_map().checked::<Cqe>(
            self.params.cq_off.cqes as usize,
            self.params.cq_entries as usize,
        )?;
        let cqe = unsafe { ptr::read(cqe_pointer.add(cq_index as usize)) };
        case.observations.push(Observation::Completion(cqe));
        unsafe {
            case.bytes(
                "CQE bytes",
                cqe_pointer.add(cq_index as usize) as u64,
                std::slice::from_raw_parts(cqe_pointer.add(cq_index as usize).cast(), 16),
            );
        }
        self.cq_atomic(self.params.cq_off.head)
            .store(c_head.wrapping_add(1), Ordering::Release);
        require(
            cqe.user_data == token && cqe.result == payload.len() as i32 && cqe.flags == 0,
            format!(
                "unexpected CQE {cqe:?}; expected token={token} length={} flags=0",
                payload.len()
            ),
        )?;
        let mut received = [0u8; 128];
        let result = case.syscall(
            "recvfrom",
            libc::SYS_recvfrom,
            [
                peer as u64,
                received.as_mut_ptr() as u64,
                128,
                libc::MSG_DONTWAIT as u64,
                0,
                0,
            ],
        );
        let length = if result < 0 {
            0
        } else {
            (result as usize).min(received.len())
        };
        case.bytes("peer bytes", received.as_ptr() as u64, &received[..length]);
        require(
            result == payload.len() as i64 && &received[..length] == payload,
            "peer payload mismatch",
        )?;
        let duplicate = case.syscall(
            "recvfrom duplicate check",
            libc::SYS_recvfrom,
            [
                peer as u64,
                received.as_mut_ptr() as u64,
                128,
                libc::MSG_DONTWAIT as u64,
                0,
                0,
            ],
        );
        if duplicate > 0 {
            case.bytes(
                "unexpected duplicate bytes",
                received.as_ptr() as u64,
                &received[..(duplicate as usize).min(128)],
            );
        }
        require(
            duplicate == -i64::from(libc::EAGAIN),
            "peer received a duplicate or unexpected error",
        )?;
        let (head, tail, cq_head, cq_tail) = self.queue(case, "after consume");
        require(
            head == tail && cq_head == cq_tail,
            "queues not empty after one operation",
        )?;
        require(
            self.sq_load(self.params.sq_off.dropped) == 0
                && self.cq_load(self.params.cq_off.overflow) == 0,
            "ring dropped an SQE or overflowed the CQ",
        )
    }
}

fn send_case(case: &mut CaseReport, fixed: bool, worker: bool, sqpoll: bool) -> Check {
    let mut fds = [-1i32; 2];
    let result = case.syscall(
        "socketpair",
        libc::SYS_socketpair,
        [
            libc::AF_UNIX as u64,
            (libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC) as u64,
            0,
            fds.as_mut_ptr() as u64,
            0,
            0,
        ],
    );
    require(result == 0, format!("socketpair returned {result}"))?;
    let socket = own_fd(i64::from(fds[0]), "socketpair endpoint")?;
    let peer = own_fd(i64::from(fds[1]), "socketpair peer")?;
    let ring = Ring::new(case, if sqpoll { SQPOLL } else { 0 })?;
    if sqpoll && !fixed && ring.params.features & SQPOLL_NONFIXED == 0 {
        return Err(Outcome::Unavailable(
            "SQPOLL_NONFIXED feature absent; operation unmeasured".into(),
        ));
    }
    if fixed {
        let original = [socket.as_raw_fd()];
        case.bytes(
            "original fixed-file array",
            original.as_ptr() as u64,
            &original[0].to_ne_bytes(),
        );
        let result = case.register(
            ring.fd.as_raw_fd(),
            REGISTER_FILES,
            original.as_ptr() as u64,
            1,
        );
        require(
            result == 0,
            format!("register fixed socket returned {result}"),
        )?;
    }
    let before = case.fdinfo(ring.fd.as_raw_fd(), "before SENDs")?;
    let sq_thread = before
        .lines()
        .find_map(|line| line.strip_prefix("SqThread:\t"))
        .and_then(|value| value.parse::<i32>().ok());
    require(
        if sqpoll {
            sq_thread.is_some_and(|tid| tid > 0)
        } else {
            sq_thread == Some(-1)
        },
        format!("SQPOLL={sqpoll} does not match actual fdinfo SqThread {sq_thread:?}"),
    )?;
    let flags = if fixed { FIXED_FILE } else { 0 } | if worker { ASYNC } else { 0 };
    let fd = if fixed { 0 } else { socket.as_raw_fd() };
    ring.send(
        case,
        fd,
        peer.as_raw_fd(),
        flags,
        b"ordinary-ring-control",
        101,
    )?;
    // Historical F1 payload, now sent to an ordinary owned descriptor. The old
    // protected-descriptor bypass remains a rejected result in its own record.
    ring.send(case, fd, peer.as_raw_fd(), flags, b"forged-log-data\n", 102)?;
    case.fdinfo(ring.fd.as_raw_fd(), "after SENDs")?;
    Ok(())
}

fn memfd(case: &mut CaseReport, name: &str) -> Result<OwnedFd, Outcome> {
    let name = CString::new(name).map_err(|e| Outcome::Failed(e.to_string()))?;
    let result = case.syscall(
        "memfd_create",
        libc::SYS_memfd_create,
        [name.as_ptr() as u64, libc::MFD_CLOEXEC as u64, 0, 0, 0, 0],
    );
    own_fd(result, "memfd_create")
}

fn slots(info: &str) -> Vec<(u32, String)> {
    let mut in_files = false;
    let mut entries = Vec::new();
    for line in info.lines() {
        if line.starts_with("UserFiles:") {
            in_files = true;
            continue;
        }
        if !in_files {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            break;
        }
        if let Some((index, name)) = line.trim_start().split_once(": ")
            && let Ok(index) = index.parse()
        {
            entries.push((index, name.to_owned()));
        }
    }
    entries
}

fn fd_name(fd: i32) -> Result<String, Outcome> {
    std::fs::read_link(format!("/proc/self/fd/{fd}"))
        // fdinfo uses seq_file path escaping, whereas readlink returns the raw
        // pathname. Encode the expected identity; never strip suffixes or use
        // a substring match that could admit an old slot or a different file.
        .map(|p| {
            p.to_string_lossy()
                .chars()
                .map(|c| match c {
                    ' ' => "\\040".into(),
                    '\t' => "\\011".into(),
                    '\n' => "\\012".into(),
                    '\\' => "\\134".into(),
                    _ => c.to_string(),
                })
                .collect()
        })
        .map_err(|e| Outcome::Failed(format!("read descriptor identity: {e}")))
}

fn prefix_case(case: &mut CaseReport, opcode: u32) -> Check {
    let (ring, _) = setup(case, 2, 0)?;
    let old0 = memfd(case, "installed-uring-old0")?;
    let old1 = memfd(case, "installed-uring-old1")?;
    let new0 = memfd(case, "installed-uring-new0")?;
    let initial = [old0.as_raw_fd(), old1.as_raw_fd()];
    let result = case.register(ring.as_raw_fd(), REGISTER_FILES, initial.as_ptr() as u64, 2);
    require(
        result == 0,
        format!("initial registration returned {result}"),
    )?;
    let before = case.fdinfo(ring.as_raw_fd(), "before UPDATE")?;
    require(
        slots(&before)
            == vec![
                (0, fd_name(old0.as_raw_fd())?),
                (1, fd_name(old1.as_raw_fd())?),
            ],
        "initial slots do not match both original files",
    )?;
    let update = [new0.as_raw_fd(), -12345i32];
    let header = [0u64, update.as_ptr() as u64, 0, 2];
    let count = if opcode == UPDATE { 2 } else { 32 };
    // These are the original live header and array, not copied syscall input.
    // Zero offset/reserved fields/tags and UPDATE2 nr=2 match the original.
    unsafe {
        case.bytes(
            "original update array",
            update.as_ptr() as u64,
            std::slice::from_raw_parts(update.as_ptr().cast(), 8),
        );
        case.bytes(
            "original update header",
            header.as_ptr() as u64,
            std::slice::from_raw_parts(header.as_ptr().cast(), 32),
        );
    }
    let result = case.register(ring.as_raw_fd(), opcode, header.as_ptr() as u64, count);
    let after = case.fdinfo(ring.as_raw_fd(), "after UPDATE")?;
    require(
        result == 1,
        format!("UPDATE successful prefix must return 1, got {result}"),
    )?;
    require(
        slots(&after) == vec![(0, fd_name(new0.as_raw_fd())?)],
        "UPDATE must replace slot 0 and remove the failing slot 1",
    )?;
    require(
        update == [new0.as_raw_fd(), -12345] && header == [0, update.as_ptr() as u64, 0, 2],
        "kernel modified original UPDATE input",
    )
}

// Same register-only permission-switch sequence as the preserved diagnostic's
// raw_syscall6_with_pkru. It is an ordinary syscall instruction, not a runtime
// trusted gate. No stack, global or TLS access occurs with key zero disabled.
core::arch::global_asm!(
    r#"
.text
.p2align 4
.global installed_io_uring_pkru_syscall
.hidden installed_io_uring_pkru_syscall
.type installed_io_uring_pkru_syscall,@function
installed_io_uring_pkru_syscall:
    push r12
    push r13
    push r14
    push r15
    mov r12d, edx
    mov r13, rdi
    mov r14, [rsi + 16]
    mov rdi, [rsi]
    mov rdx, [rsi + 8]
    mov r10, [rsi + 24]
    mov r8, [rsi + 32]
    mov r9, [rsi + 40]
    mov rsi, rdx
    xor ecx, ecx
    rdpkru
    mov r15d, eax
    mov eax, r12d
    xor edx, edx
    wrpkru
    lfence
    mov rax, r13
    mov rdx, r14
    syscall
    mov r12, rax
    mov eax, r15d
    xor ecx, ecx
    xor edx, edx
    wrpkru
    lfence
    mov rax, r12
    pop r15
    pop r14
    pop r13
    pop r12
    ret
.size installed_io_uring_pkru_syscall, .-installed_io_uring_pkru_syscall
"#
);

unsafe extern "C" {
    fn installed_io_uring_pkru_syscall(number: i64, args: *const u64, pkru: u32) -> i64;
}

fn read_pkru() -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("rdpkru",in("ecx") 0u32,out("eax") value,out("edx") _,options(nostack,nomem));
    }
    value
}

fn restore_pkru(value: u32) {
    unsafe {
        core::arch::asm!("wrpkru","lfence",in("eax") value,in("ecx") 0u32,in("edx") 0u32,options(nostack));
    }
}

struct RseqPause {
    area: usize,
}

impl RseqPause {
    fn new(case: &mut CaseReport) -> Result<Self, Outcome> {
        let offset = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__rseq_offset".as_ptr()) };
        let size = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"__rseq_size".as_ptr()) };
        require(
            !offset.is_null() && !size.is_null(),
            "glibc rseq symbols unavailable",
        )?;
        let size = unsafe { *size.cast::<u32>() };
        if size == 0 {
            case.fact("rseq", "not registered by libc");
            return Ok(Self { area: 0 });
        }
        require(
            size <= 32,
            "libc rseq area exceeds original diagnostic's 32-byte ABI",
        )?;
        let mut fs = 0usize;
        let result = case.syscall(
            "arch_prctl GET_FS",
            libc::SYS_arch_prctl,
            [0x1003, ptr::from_mut(&mut fs) as u64, 0, 0, 0, 0],
        );
        require(result == 0, format!("GET_FS returned {result}"))?;
        let area = fs.wrapping_add_signed(unsafe { *offset.cast::<isize>() });
        let result = case.syscall(
            "rseq unregister",
            libc::SYS_rseq,
            [area as u64, 32, 1, 0x53053053, 0, 0],
        );
        require(result == 0, format!("rseq unregister returned {result}"))?;
        Ok(Self { area })
    }

    fn restore(&mut self, case: &mut CaseReport) -> Check {
        if self.area == 0 {
            return Ok(());
        }
        let result = case.syscall(
            "rseq restore",
            libc::SYS_rseq,
            [self.area as u64, 32, 0, 0x53053053, 0, 0],
        );
        require(result == 0, format!("rseq restore returned {result}"))?;
        self.area = 0;
        Ok(())
    }
}

impl Drop for RseqPause {
    fn drop(&mut self) {
        if self.area != 0 {
            unsafe { libc::syscall(libc::SYS_rseq, self.area, 32, 0, 0x53053053u32) };
        }
    }
}

fn pkru_case(case: &mut CaseReport, opcode: u32, copied: bool) -> Check {
    let cpuid = core::arch::x86_64::__cpuid_count(7, 0);
    case.fact("cpuid7.ecx", cpuid.ecx);
    if cpuid.ecx & (1 << 4) == 0 {
        return Err(Outcome::Unavailable(
            "OSPKE absent; original-pointer PKRU operation unmeasured".into(),
        ));
    }
    let initial_pkru = read_pkru();
    let (ring, _) = setup(case, 2, 0)?;
    let file = std::fs::File::open("/dev/null").map_err(|e| Outcome::Failed(e.to_string()))?;
    let initial = [file.as_raw_fd(), file.as_raw_fd()];
    let registered = case.register(ring.as_raw_fd(), REGISTER_FILES, initial.as_ptr() as u64, 2);
    require(
        registered == 0,
        format!("PKRU initial registration returned {registered}"),
    )?;
    let before = case.fdinfo(ring.as_raw_fd(), "before PKRU UPDATE")?;
    require(
        slots(&before) == vec![(0, "/dev/null".into()), (1, "/dev/null".into())],
        "PKRU initial file slots mismatch",
    )?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    require(page_size >= 72, "invalid page size")?;
    let data = unsafe {
        libc::mmap(
            ptr::null_mut(),
            page_size as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if data == libc::MAP_FAILED {
        return Err(Outcome::Failed(format!(
            "PKRU mmap: {}",
            io::Error::last_os_error()
        )));
    }
    let mapping = Mapping {
        address: data.cast(),
        length: page_size as usize,
    };
    let key = case.syscall("pkey_alloc", libc::SYS_pkey_alloc, [0; 6]);
    if key <= 0 {
        return Err(unavailable("pkey_alloc", key));
    }
    let result = (|| -> Check {
        let protected = case.syscall(
            "pkey_mprotect",
            libc::SYS_pkey_mprotect,
            [
                data as u64,
                page_size as u64,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                key as u64,
                0,
                0,
            ],
        );
        require(
            protected == 0,
            format!("pkey_mprotect returned {protected}"),
        )?;
        let mut rseq = RseqPause::new(case)?;
        let inner = (|| -> Check {
            let array = unsafe { mapping.address.add(64).cast::<i32>() };
            let header = mapping.address.cast::<[u64; 4]>();
            unsafe {
                array.write(file.as_raw_fd());
                array.add(1).write(-12345);
                header.write([0, array as u64, 0, 2]);
            }
            let copy_array = [file.as_raw_fd(), -12345];
            let copy_header = [0u64, copy_array.as_ptr() as u64, 0, 2];
            let pointer = if copied {
                copy_header.as_ptr() as u64
            } else {
                header as u64
            };
            unsafe {
                case.bytes(
                    "original keyed update array",
                    array as u64,
                    std::slice::from_raw_parts(array.cast(), 8),
                );
                case.bytes(
                    "original keyed update header",
                    header as u64,
                    std::slice::from_raw_parts(header.cast(), 32),
                );
                case.bytes(
                    "deliberate key-zero copy array",
                    copy_array.as_ptr() as u64,
                    std::slice::from_raw_parts(copy_array.as_ptr().cast(), 8),
                );
                case.bytes(
                    "deliberate key-zero copy header",
                    copy_header.as_ptr() as u64,
                    std::slice::from_raw_parts(copy_header.as_ptr().cast(), 32),
                );
            }
            case.fact("pkey", key);
            case.fact("copied diagnostic input", copied);
            case.fact("syscall PKRU", 1);
            let args = [
                ring.as_raw_fd() as u64,
                u64::from(opcode),
                pointer,
                if opcode == UPDATE { 2 } else { 32 },
                0,
                0,
            ];
            let prior = read_pkru();
            let result = unsafe {
                installed_io_uring_pkru_syscall(libc::SYS_io_uring_register, args.as_ptr(), 1)
            };
            let restored = read_pkru();
            case.observations.push(Observation::Syscall {
                name: "io_uring_register with PKRU=1",
                arguments: args,
                result,
            });
            case.fact("PKRU before syscall", prior);
            case.fact("PKRU after syscall", restored);
            let after = case.fdinfo(ring.as_raw_fd(), "after PKRU UPDATE")?;
            let expected = if copied { -i64::from(libc::EFAULT) } else { 1 };
            require(
                result == expected,
                format!("PKRU copied={copied}: expected {expected}, got {result}"),
            )?;
            require(prior == restored, "syscall did not restore caller PKRU")?;
            let expected_slots = if copied {
                vec![(0, "/dev/null".into()), (1, "/dev/null".into())]
            } else {
                vec![(0, "/dev/null".into())]
            };
            require(
                slots(&after) == expected_slots,
                "PKRU UPDATE slots differ from kernel prefix/fault semantics",
            )
        })();
        let restored = rseq.restore(case);
        inner.and(restored)
    })();
    drop(mapping);
    let free = case.syscall(
        "pkey_free",
        libc::SYS_pkey_free,
        [key as u64, 0, 0, 0, 0, 0],
    );
    restore_pkru(initial_pkru);
    result.and(require(free == 0, format!("pkey_free returned {free}")))
}

/// Report each completed case after its syscall, permission restoration and
/// owned resource cleanup. The observer may retain evidence before a later
/// case fails to return. Case order, assertions and outcomes are unchanged.
pub fn run_observed(mut observer: impl FnMut(&CaseReport)) -> Report {
    let mut cases = Vec::new();
    for (name, fixed, worker, sqpoll) in [
        ("send-unfixed", false, false, false),
        ("send-fixed", true, false, false),
        ("send-worker-unfixed", false, true, false),
        ("send-worker-fixed", true, true, false),
        ("send-sqpoll-unfixed", false, false, true),
        ("send-sqpoll-fixed", true, false, true),
    ] {
        let mut case = CaseReport::new(name);
        case.outcome = match send_case(&mut case, fixed, worker, sqpoll) {
            Ok(()) => Outcome::Passed,
            Err(e) => e,
        };
        observer(&case);
        cases.push(case);
    }
    for (name, opcode) in [("update-prefix", UPDATE), ("update2-prefix", UPDATE2)] {
        let mut case = CaseReport::new(name);
        case.outcome = match prefix_case(&mut case, opcode) {
            Ok(()) => Outcome::Passed,
            Err(e) => e,
        };
        observer(&case);
        cases.push(case);
    }
    for (name, opcode, copied) in [
        ("update-original-pointer-pkru", UPDATE, false),
        ("update-copied-pointer-pkru-fault", UPDATE, true),
        ("update2-original-pointer-pkru", UPDATE2, false),
        ("update2-copied-pointer-pkru-fault", UPDATE2, true),
    ] {
        let mut case = CaseReport::new(name);
        case.outcome = match pkru_case(&mut case, opcode, copied) {
            Ok(()) => Outcome::Passed,
            Err(e) => e,
        };
        observer(&case);
        cases.push(case);
    }
    Report { cases }
}
