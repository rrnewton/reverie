use super::*;

thread_local! {
    static PAUSED_VALUE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PAUSED_RESULT: std::cell::Cell<i64> = const { std::cell::Cell::new(8) };
    static PAUSED_READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static CHANGE_SEQUENCE: std::cell::Cell<*mut u32> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

unsafe fn paused_read_gate(number: i64, arguments: [u64; 6]) -> i64 {
    assert_eq!(number, libc::SYS_read);
    assert_eq!(arguments[0], 99);
    assert_eq!(arguments[2], 8);
    PAUSED_READS.set(PAUSED_READS.get() + 1);
    if PAUSED_RESULT.get() == 8 {
        unsafe { *(arguments[1] as *mut u64) = PAUSED_VALUE.get() };
    }
    if !CHANGE_SEQUENCE.get().is_null() {
        unsafe { *CHANGE_SEQUENCE.get() = 4 };
    }
    PAUSED_RESULT.get()
}

fn paused_fixture() -> (
    Box<perf::perf_event_mmap_page>,
    std::mem::ManuallyDrop<PerfCounter>,
) {
    PAUSED_VALUE.set(0);
    PAUSED_RESULT.set(8);
    PAUSED_READS.set(0);
    CHANGE_SEQUENCE.set(std::ptr::null_mut());
    let mut page = Box::<perf::perf_event_mmap_page>::default();
    page.lock = 2;
    let counter = std::mem::ManuallyDrop::new(PerfCounter {
        fd: 99,
        mmap: Some(NonNull::from(page.as_mut())),
        raw_syscall: Some(paused_read_gate),
    });
    (page, counter)
}

#[test]
fn paused_read_preserves_exact_counts_without_retry() {
    let (_page, counter) = paused_fixture();
    for value in [0, 1, 2, (1 << 53) + 1, u64::MAX] {
        PAUSED_VALUE.set(value);
        let previous = PAUSED_READS.get();
        assert_eq!(counter.ctr_value_paused_once(), Ok(value));
        assert_eq!(PAUSED_READS.get(), previous + 1);
    }
}

#[test]
fn paused_read_errors_are_explicit_and_not_retried() {
    let (_page, counter) = paused_fixture();
    for (result, error) in [
        (0, Errno::ENODEV),
        (7, Errno::EIO),
        (9, Errno::EIO),
        (-(libc::EINTR as i64), Errno::EINTR),
        (-(libc::EBADF as i64), Errno::EBADF),
    ] {
        PAUSED_RESULT.set(result);
        let previous = PAUSED_READS.get();
        assert_eq!(counter.ctr_value_paused_once(), Err(error));
        assert_eq!(PAUSED_READS.get(), previous + 1);
    }
}

#[test]
fn paused_read_rejects_active_lost_or_unavailable_state_before_syscall() {
    let (mut page, mut counter) = paused_fixture();
    page.lock = 3;
    assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EAGAIN));
    page.lock = 2;
    page.index = 1;
    assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EBUSY));
    page.index = 0;
    page.time_enabled = 1;
    assert_eq!(counter.ctr_value_paused_once(), Err(Errno::ENODEV));
    page.time_enabled = 0;
    counter.raw_syscall = None;
    assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EOPNOTSUPP));
    counter.raw_syscall = Some(paused_read_gate);
    counter.mmap = None;
    assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EOPNOTSUPP));
    assert_eq!(PAUSED_READS.get(), 0);
}

#[test]
fn paused_read_rejects_changed_metadata_after_one_read() {
    let (mut page, counter) = paused_fixture();
    CHANGE_SEQUENCE.set(&raw mut page.lock);
    assert_eq!(counter.ctr_value_paused_once(), Err(Errno::EAGAIN));
    assert_eq!(PAUSED_READS.get(), 1);
    CHANGE_SEQUENCE.set(std::ptr::null_mut());
}
