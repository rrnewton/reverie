use std::cell::Cell;
use std::cell::UnsafeCell;
use std::io;

use reverie::Errno;
use reverie::Error;
use reverie::TimerSchedule;
use reverie_preload::trap::raw_syscall6;
use reverie_ptrace::InGuestRcbTimer;

pub(crate) struct TimerOwner {
    timer: UnsafeCell<InGuestRcbTimer>,
    owner: i64,
    borrowed: Cell<bool>,
}

struct TimerBorrow<'owner> {
    borrowed: &'owner Cell<bool>,
    revision: u64,
}

impl Drop for TimerBorrow<'_> {
    fn drop(&mut self) {
        crate::clock_control::finish_notification_update(self.revision);
        self.borrowed.set(false);
    }
}

impl TimerOwner {
    pub(crate) fn descriptor(&self) -> i32 {
        unsafe { &*self.timer.get() }.raw_fd()
    }

    pub(crate) fn with_timer<Output>(
        &self,
        operation: impl FnOnce(&mut InGuestRcbTimer) -> Result<Output, Errno>,
    ) -> Result<Output, Errno> {
        if unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } != self.owner {
            return Err(Errno::EXDEV);
        }
        if !crate::clock_control::paused() || self.borrowed.replace(true) {
            return Err(Errno::EBUSY);
        }
        let Some(revision) = crate::clock_control::notification_revision().checked_add(1) else {
            self.borrowed.set(false);
            return Err(Errno::EOVERFLOW);
        };
        let _borrow = TimerBorrow {
            borrowed: &self.borrowed,
            revision,
        };
        operation(unsafe { &mut *self.timer.get() })
    }

    fn cancel(&self) -> Result<(), Errno> {
        self.with_timer(|timer| {
            crate::clock_control::cancel_notification_resume();
            timer.disarm()
        })
    }
}

pub(crate) fn initialize() -> io::Result<()> {
    if !crate::clock_control::active() {
        return Ok(());
    }
    if !crate::clock_control::notification_owner().is_null() {
        return Err(io::Error::other("notification timer already installed"));
    }
    let owner = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if owner <= 0 {
        return Err(io::Error::other("notification timer thread unavailable"));
    }
    let timer = unsafe {
        InGuestRcbTimer::current_thread_with_syscall_gate(raw_syscall6, reverie::PERF_EVENT_SIGNAL)
    }
    .map_err(io::Error::other)?;
    crate::clock_control::publish_notification(Box::new(TimerOwner {
        timer: UnsafeCell::new(timer),
        owner,
        borrowed: Cell::new(false),
    }))
}

pub(crate) fn request(_schedule: TimerSchedule, precise: bool) -> Result<(), Error> {
    let owner = crate::clock_control::notification_owner();
    if !owner.is_null() {
        unsafe { &*owner }.cancel()?;
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        if precise {
            "LiteInst precise timer delivery requires an exact position witness and safe Tool entry"
        } else {
            "LiteInst timer delivery requires a safe Tool entry"
        },
    )
    .into())
}
