//! Fixed public/private ordered producers for the explicitly installed mapping set.

use std::io;
use std::mem::ManuallyDrop;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use reverie_preload::trap::raw_syscall6;
use reverie_rpc_transport::guest_log::PublishError;
use reverie_rpc_transport::guest_log::RecordCommit;
use reverie_rpc_transport::guest_log::SharedBuffer;
use reverie_rpc_transport::guest_log::ordered::Buffer;
use reverie_rpc_transport::guest_log::ordered::Writer;

use crate::rpc::SpinMutex;

struct Producer {
    buffer: Arc<Buffer>,
    writer: Option<Writer>,
    failure: Option<PublishError>,
}

impl Producer {
    fn fail(&mut self, error: PublishError) {
        self.failure.get_or_insert(error);
        self.buffer.fail_guest();
        self.writer = None;
    }
}

struct Producers([Producer; 2]);

struct InstalledLogs {
    producers: SpinMutex<Option<Producers>>,
    buffers: [Arc<Buffer>; 2],
    blocked_publication: Duration,
    failed_installation: AtomicBool,
}

static LOGS: OnceLock<InstalledLogs> = OnceLock::new();

/// A fixed public or private complete-record producer. Copying this handle does
/// not change the selected capture and does not expose a backing descriptor.
#[derive(Clone, Copy)]
pub struct MappedLogProducer {
    role: usize,
}

/// Obtain the independently selected public and private producers after setup.
/// A caller formats one complete record before write_record, as for HostProducer.
pub fn mapped_log_producers() -> Option<(MappedLogProducer, MappedLogProducer)> {
    let logs = LOGS.get()?;
    if logs.failed_installation.load(Ordering::Acquire) {
        return None;
    }
    Some((MappedLogProducer { role: 0 }, MappedLogProducer { role: 1 }))
}

impl MappedLogProducer {
    pub fn record_failed(&self) {
        let _runtime = crate::runtime::enter_mapped_runtime_io();
        if let Some(logs) = LOGS.get() {
            // Failure notification never waits for a producer/publication lock.
            logs.buffers[self.role].fail_guest();
        }
    }

    pub fn write_record(&self, bytes: &[u8]) -> Result<RecordCommit, PublishError> {
        let _runtime = crate::runtime::enter_mapped_runtime_io();
        let logs = LOGS.get().ok_or(PublishError::Invalid)?;
        if logs.failed_installation.load(Ordering::Acquire) {
            return Err(PublishError::Stopped);
        }
        let deadline = Instant::now() + logs.blocked_publication;
        let mut slot = logs.producers.lock();
        let producers = slot.as_mut().ok_or(PublishError::Invalid)?;
        let producer = &mut producers.0[self.role];
        if let Some(error) = producer.failure {
            return Err(error);
        }
        let writer = producer.writer.as_mut().ok_or(PublishError::Invalid)?;
        let result = writer.write_record(bytes, |buffer, progress| {
            wait_progress(buffer, progress, deadline)
        });
        if let Err(error) = result {
            producer.fail(error);
        }
        result
    }
}

fn wait_progress(
    buffer: &SharedBuffer,
    progress: u32,
    deadline: Instant,
) -> Result<(), PublishError> {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return Err(PublishError::Full);
    };
    let timeout = libc::timespec {
        tv_sec: remaining.as_secs() as libc::time_t,
        tv_nsec: i64::from(remaining.subsec_nanos()),
    };
    let result = unsafe {
        raw_syscall6(
            libc::SYS_futex,
            [
                buffer.progress_address() as u64,
                libc::FUTEX_WAIT as u64,
                u64::from(progress),
                (&raw const timeout) as u64,
                0,
                0,
            ],
        )
    };
    match result {
        0 => Ok(()),
        value if value == -i64::from(libc::EINTR) || value == -i64::from(libc::EAGAIN) => Ok(()),
        value if value == -i64::from(libc::ETIMEDOUT) => Err(PublishError::Full),
        _ => Err(PublishError::Invalid),
    }
}

fn activate(
    buffers: [Arc<Buffer>; 2],
    indices: [usize; 2],
    pid: i32,
    child: bool,
) -> io::Result<Producers> {
    fn one(buffer: Arc<Buffer>, index: usize, pid: i32, child: bool) -> io::Result<Producer> {
        if index == 0 {
            if !child || !buffer.guest_failed() {
                return Err(io::Error::other("inactive healthy/root capture"));
            }
            return Ok(Producer {
                buffer,
                writer: None,
                failure: Some(PublishError::Stopped),
            });
        }
        // SAFETY: root installation or this exact physical fork exclusively
        // owns the reserved incarnation. Inherited Writers are never reused.
        match unsafe { buffer.activate(index, i64::from(pid)) } {
            Ok(writer) => Ok(Producer {
                buffer,
                writer: Some(writer),
                failure: None,
            }),
            Err(error) if child && buffer.guest_stopped() => {
                // A real reservation remains in the shared record even when a
                // later capture failure prevents activation. Never erase it or
                // claim this failed output supplied FINISH.
                buffer.fail_guest();
                Ok(Producer {
                    buffer,
                    writer: None,
                    failure: Some(error),
                })
            }
            Err(error) => Err(io::Error::other(format!("producer activation: {error:?}"))),
        }
    }
    let [public, private] = buffers;
    Ok(Producers([
        one(public, indices[0], pid, child)?,
        one(private, indices[1], pid, child)?,
    ]))
}

/// Owns only the root installation published by one setup transaction.
/// An error or unwinding configuration callback cannot leave its producers live.
pub(crate) struct Installation {
    logs: &'static InstalledLogs,
    complete: bool,
}

impl Installation {
    pub(crate) fn complete(mut self) {
        self.complete = true;
    }
}

impl Drop for Installation {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        // Notify both captures before acquiring a local producer lock. Failure
        // does not publish FINISH or turn an incomplete capture into completion.
        for buffer in &self.logs.buffers {
            buffer.fail_guest();
        }
        self.logs.failed_installation.store(true, Ordering::Release);
        let producers = self.logs.producers.lock().take();
        // The temporary lock guard above is already released. Writer has no
        // implicit FINISH, and LOGS retains the two mapping owners permanently.
        drop(producers);
    }
}

pub(crate) fn install(
    buffers: [Arc<Buffer>; 2],
    pid: i32,
    blocked_publication: Duration,
) -> io::Result<Installation> {
    if blocked_publication.is_zero() || Instant::now().checked_add(blocked_publication).is_none() {
        return Err(io::Error::other("invalid installed publication deadline"));
    }
    // Refuse before modifying root incarnation state on a duplicate install.
    if LOGS.get().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "mapped logs installed twice",
        ));
    }
    let producers = activate(buffers.clone(), [1, 1], pid, false)?;
    LOGS.set(InstalledLogs {
        producers: SpinMutex::new(Some(producers)),
        buffers,
        blocked_publication,
        failed_installation: AtomicBool::new(false),
    })
    .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "mapped logs installed twice"))?;
    Ok(Installation {
        logs: LOGS.get().expect("root installation was just published"),
        complete: false,
    })
}

pub(crate) struct PreparedLogs {
    inherited: ManuallyDrop<Producers>,
    pub(crate) incarnations: [usize; 2],
}

pub(crate) fn prepare_fork() -> PreparedLogs {
    let logs = LOGS.get().unwrap_or_else(|| fatal());
    let mut producers = logs.producers.lock().take().unwrap_or_else(|| fatal());
    let mut incarnations = [0; 2];
    for (producer, incarnation) in producers.0.iter_mut().zip(&mut incarnations) {
        if let Some(writer) = &producer.writer {
            match producer.buffer.reserve_child(writer.index()) {
                Ok(index) => *incarnation = index,
                Err(error)
                    if error == PublishError::Capacity || producer.buffer.guest_stopped() =>
                {
                    producer.fail(error);
                }
                Err(_) => fatal(),
            }
        } else if !producer.buffer.guest_failed() {
            // Inactivity cannot be chosen for an otherwise healthy capture.
            fatal();
        }
    }
    PreparedLogs {
        inherited: ManuallyDrop::new(producers),
        incarnations,
    }
}

pub(crate) fn restore_parent(prepared: PreparedLogs, result: i64) {
    if result == 0 {
        fatal();
    }
    let producers = ManuallyDrop::into_inner(prepared.inherited);
    for (producer, index) in producers.0.iter().zip(prepared.incarnations) {
        // This is the original actual kernel result, including a real failure.
        if index != 0 {
            producer
                .buffer
                .resolve_fork(index, result)
                .unwrap_or_else(|_| fatal());
        }
    }
    let mut slot = LOGS.get().unwrap_or_else(|| fatal()).producers.lock();
    if slot.is_some() {
        fatal();
    }
    *slot = Some(producers);
}

pub(crate) fn complete_child(
    prepared: PreparedLogs,
    buffers: [Arc<Buffer>; 2],
    pid: i32,
) -> io::Result<()> {
    let producers = activate(buffers, prepared.incarnations, pid, true)?;
    {
        let mut slot = LOGS.get().unwrap_or_else(|| fatal()).producers.lock();
        if slot.is_some() {
            fatal();
        }
        *slot = Some(producers);
    }
    // Ordered Writer has no implicit FINISH/close. Its COW-private Arc metadata
    // and VMAs are released only after both fresh producers are installed.
    drop(ManuallyDrop::into_inner(prepared.inherited));
    Ok(())
}

pub(crate) fn finish() {
    let Some(logs) = LOGS.get() else { return };
    let deadline = Instant::now() + logs.blocked_publication;
    let mut slot = logs.producers.lock();
    let Some(producers) = slot.as_mut() else {
        fatal()
    };
    // Attempt both independent FINISH records under the same deadline. A sink
    // failure is retained in that capture; it does not rename the guest's exit.
    for producer in &mut producers.0 {
        if let Some(writer) = &mut producer.writer
            && let Err(error) =
                writer.finish(|buffer, progress| wait_progress(buffer, progress, deadline))
        {
            producer.fail(error);
        }
    }
}

fn fatal() -> ! {
    unsafe {
        raw_syscall6(libc::SYS_exit_group, [123, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}
