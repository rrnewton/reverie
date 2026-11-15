//! Raw io_uring, as the cells need it and without liburing: the three system
//! call numbers, the setup parameters, a submission-polling ring mapped into
//! this process, and a count of this process's polling threads.

use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

/// `io_uring_setup`, `io_uring_enter` and `io_uring_register` (425-427 on
/// x86-64; this guest body is built for x86-64 only).
pub(super) const SYS_IO_URING_SETUP: i64 = 425;
pub(super) const SYS_IO_URING_ENTER: i64 = 426;
pub(super) const SYS_IO_URING_REGISTER: i64 = 427;

/// `IORING_SETUP_SQPOLL`: a kernel thread polls the submission queue, so a
/// request needs no system call to be submitted.
const IORING_SETUP_SQPOLL: u32 = 1 << 1;
const IORING_OFF_SQ_RING: libc::off_t = 0;
const IORING_OFF_CQ_RING: libc::off_t = 0x0800_0000;
const IORING_OFF_SQES: libc::off_t = 0x1000_0000;
/// The submission ring's flag the polling thread sets when it goes to sleep;
/// only `io_uring_enter` wakes it.
pub(super) const IORING_SQ_NEED_WAKEUP: u32 = 1;
pub(super) const IORING_OP_STATX: u8 = 21;

/// `struct io_sqring_offsets`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SubmissionOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    flags: u32,
    dropped: u32,
    array: u32,
    resv1: u32,
    user_addr: u64,
}

/// `struct io_cqring_offsets`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CompletionOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    overflow: u32,
    cqes: u32,
    flags: u32,
    resv1: u32,
    user_addr: u64,
}

/// `struct io_uring_params`, which `io_uring_setup` reads and fills.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct Params {
    sq_entries: u32,
    cq_entries: u32,
    flags: u32,
    sq_thread_cpu: u32,
    sq_thread_idle: u32,
    features: u32,
    wq_fd: u32,
    resv: [u32; 3],
    sq_off: SubmissionOffsets,
    cq_off: CompletionOffsets,
}

const _: () = assert!(core::mem::size_of::<Params>() == 120);

/// `struct io_uring_sqe`, with the fields IORING_OP_STATX uses named for it:
/// `fd` is the directory descriptor, `addr` the path, `len` the statx mask,
/// `off` the statx buffer and `op_flags` the statx flags.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct Sqe {
    pub(super) opcode: u8,
    pub(super) flags: u8,
    pub(super) ioprio: u16,
    pub(super) fd: i32,
    pub(super) off: u64,
    pub(super) addr: u64,
    pub(super) len: u32,
    pub(super) op_flags: u32,
    pub(super) user_data: u64,
    pub(super) buf_index: u16,
    pub(super) personality: u16,
    pub(super) file_index: u32,
    pub(super) addr3: u64,
    pub(super) pad: u64,
}

const _: () = assert!(core::mem::size_of::<Sqe>() == 64);

/// `struct io_uring_cqe` (no 32-byte completions are requested).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct Cqe {
    pub(super) user_data: u64,
    pub(super) res: i32,
    pub(super) flags: u32,
}

const _: () = assert!(core::mem::size_of::<Cqe>() == 16);

/// A submission-polling ring made in this process with `io_uring_setup`,
/// called natively (never through a runtime), with its submission ring,
/// completion ring and entries mapped. The mappings and the descriptor live
/// as long as the process.
pub(super) struct PollingRing {
    pub(super) fd: i32,
    params: Params,
    submission: *mut u8,
    completion: *mut u8,
    entries: *mut Sqe,
}

/// `io_uring_setup(entries, params)` with IORING_SETUP_SQPOLL and an idle
/// time of `idle_ms` before the polling thread sleeps, made natively. Returns
/// the ring's descriptor and the parameters Linux filled, or the errno.
fn polling_setup(entries: u32, idle_ms: u32) -> Result<(i32, Params), i32> {
    let mut params = Params {
        flags: IORING_SETUP_SQPOLL,
        sq_thread_idle: idle_ms,
        ..Params::default()
    };
    let entries = libc::c_long::from(entries);
    let fd = unsafe { libc::syscall(SYS_IO_URING_SETUP, entries, &raw mut params) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
    }
    Ok((fd as i32, params))
}

/// A submission-polling ring's descriptor alone, made natively, for a caller
/// that never submits on it. Err holds the errno of the setup.
pub(super) fn polling_descriptor(idle_ms: u32) -> Result<i32, i32> {
    polling_setup(1, idle_ms).map(|(fd, _)| fd)
}

/// Maps `length` bytes of the ring `fd` at `offset`, natively.
fn map_ring(fd: i32, length: usize, offset: libc::off_t) -> Result<*mut u8, String> {
    let mapping = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_POPULATE,
            fd,
            offset,
        )
    };
    if mapping == libc::MAP_FAILED {
        let error = std::io::Error::last_os_error();
        return Err(format!(
            "mapping {length} bytes of the ring at offset {offset:#x} failed: {error}"
        ));
    }
    Ok(mapping.cast())
}

impl PollingRing {
    /// A submission-polling ring with `entries` submission entries and an
    /// idle time of `idle_ms`, made and mapped natively. Err explains a setup
    /// or mapping failure, which leaves the host without native submission
    /// polling for these cells.
    pub(super) fn create(entries: u32, idle_ms: u32) -> Result<Self, String> {
        let (fd, params) = match polling_setup(entries, idle_ms) {
            Ok(ring) => ring,
            Err(errno) => {
                let setup = "native io_uring_setup with IORING_SETUP_SQPOLL";
                return Err(format!("{setup} answered errno={errno}"));
            }
        };
        let submission_length =
            params.sq_off.array as usize + params.sq_entries as usize * core::mem::size_of::<u32>();
        let completion_length =
            params.cq_off.cqes as usize + params.cq_entries as usize * core::mem::size_of::<Cqe>();
        let entries_length = params.sq_entries as usize * core::mem::size_of::<Sqe>();
        let submission = map_ring(fd, submission_length, IORING_OFF_SQ_RING)?;
        let completion = map_ring(fd, completion_length, IORING_OFF_CQ_RING)?;
        let entries = map_ring(fd, entries_length, IORING_OFF_SQES)?;
        Ok(Self {
            fd,
            params,
            submission,
            completion,
            entries: entries.cast(),
        })
    }

    fn submission_word(&self, offset: u32) -> &AtomicU32 {
        // SAFETY: Linux's offsets name aligned 32-bit words inside the
        // submission ring mapping, which lives as long as the process.
        unsafe { AtomicU32::from_ptr(self.submission.add(offset as usize).cast()) }
    }

    fn completion_word(&self, offset: u32) -> &AtomicU32 {
        // SAFETY: as for submission_word, in the completion ring mapping.
        unsafe { AtomicU32::from_ptr(self.completion.add(offset as usize).cast()) }
    }

    /// The submission ring's flags, read after a full fence, so that they are
    /// the flags as they stood once a tail stored before this call was
    /// visible.
    pub(super) fn submission_flags(&self) -> u32 {
        core::sync::atomic::fence(Ordering::SeqCst);
        let flags = self.submission_word(self.params.sq_off.flags);
        flags.load(Ordering::Acquire)
    }

    /// Publishes `entry` with no system call: the entry and its array slot
    /// are stored first, then the submission tail with release ordering, so
    /// that the polling thread sees the entry once it sees the tail.
    pub(super) fn publish(&self, entry: Sqe) {
        let offsets = self.params.sq_off;
        let mask = self.submission_word(offsets.ring_mask);
        let tail = self.submission_word(offsets.tail);
        // This thread is the ring's only producer.
        let current = tail.load(Ordering::Relaxed);
        let index = current & mask.load(Ordering::Relaxed);
        unsafe {
            self.entries.add(index as usize).write_volatile(entry);
            let array = self.submission.add(offsets.array as usize).cast::<u32>();
            array.add(index as usize).write_volatile(index);
        }
        tail.store(current.wrapping_add(1), Ordering::Release);
    }

    /// The next completion, if one is posted: the completion tail is read
    /// with acquire ordering before the entry, and the head then advanced.
    pub(super) fn completion(&self) -> Option<Cqe> {
        let offsets = self.params.cq_off;
        let head = self.completion_word(offsets.head);
        let tail = self.completion_word(offsets.tail);
        let mask = self.completion_word(offsets.ring_mask);
        let end = tail.load(Ordering::Acquire);
        let current = head.load(Ordering::Relaxed);
        if current == end {
            return None;
        }
        let slot = (current & mask.load(Ordering::Relaxed)) as usize;
        let entry = unsafe {
            let entries = self.completion.add(offsets.cqes as usize).cast::<Cqe>();
            entries.add(slot).read_volatile()
        };
        head.store(current.wrapping_add(1), Ordering::Release);
        Some(entry)
    }
}

/// How many threads of this process have a name starting `iou-sqp-`, the name
/// Linux gives a ring's submission-polling thread.
pub(super) fn polling_threads() -> usize {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    tasks
        .filter_map(Result::ok)
        .filter_map(thread_name)
        .filter(|name| name.starts_with("iou-sqp-"))
        .count()
}

/// A thread's name, from its `/proc/self/task` entry.
fn thread_name(task: std::fs::DirEntry) -> Option<String> {
    std::fs::read_to_string(task.path().join("comm")).ok()
}

/// [`polling_threads`], waiting up to `limit` for at least one: the polling
/// thread names itself when it first runs, a moment after `io_uring_setup`
/// returns.
pub(super) fn polling_threads_within(limit: Duration) -> usize {
    let deadline = Instant::now() + limit;
    loop {
        let count = polling_threads();
        if count > 0 || Instant::now() >= deadline {
            return count;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
