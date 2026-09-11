use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use super::*;

#[derive(Default)]
struct Mock {
    code: u32,
    ticket_on_failure: bool,
    query_error: bool,
    query_corrupt: bool,
    rollback_error: bool,
    zero_success: bool,
    closed: bool,
    record: StackRecord,
}

thread_local! { static MOCK: RefCell<Mock> = RefCell::new(Mock::default()); }

fn result(stage: Stage, domain: u32, code: u32) -> RawResult {
    RawResult {
        stage: stage as u32,
        domain,
        code,
        reserved: 0,
    }
}

#[unsafe(no_mangle)]
extern "C" fn pl_gnu_root(_: *const c_void) -> RawResult {
    result(Stage::Root, 1, 0)
}

#[unsafe(no_mangle)]
extern "C" fn pl_gnu_check(_: *const c_void) -> RawResult {
    MOCK.with(|mock| result(Stage::Check, 1, u32::from(mock.borrow().closed)))
}

#[unsafe(no_mangle)]
extern "C" fn pl_gnu_close(_: *const c_void) -> RawResult {
    MOCK.with(|mock| mock.borrow_mut().closed = true);
    result(Stage::Close, 1, 0)
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pl_gnu_prepare(
    _: *const c_void,
    stack: *mut StackRecord,
    ticket: *mut u64,
) -> RawResult {
    MOCK.with(|mock| {
        let mut mock = mock.borrow_mut();
        if !mock.zero_success && (mock.code == 0 || mock.ticket_on_failure) {
            unsafe {
                (*stack).allocation_id = 2;
                *ticket = 7;
                mock.record = *stack;
            }
        }
        result(Stage::Prepare, 2, mock.code)
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pl_gnu_query(_: *const c_void, ticket: u64, out: *mut StackInfo) -> RawResult {
    assert_eq!(ticket, 7);
    MOCK.with(|mock| {
        let mock = mock.borrow();
        if mock.query_error {
            return result(Stage::Query, 2, 3);
        }
        unsafe {
            *out = StackInfo {
                storage: mock.record,
                gnu_stack_low: mock.record.usable_low,
                gnu_stack_size: mock.record.usable_size + usize::from(mock.query_corrupt),
                gnu_reported_guard_size: 0,
            };
        }
        result(Stage::Query, 2, 0)
    })
}

#[unsafe(no_mangle)]
extern "C" fn pl_gnu_rollback(_: *const c_void, ticket: u64) -> RawResult {
    assert_eq!(ticket, 7);
    MOCK.with(|mock| {
        result(
            Stage::Rollback,
            2,
            if mock.borrow().rollback_error { 3 } else { 0 },
        )
    })
}

fn startup() -> Startup {
    MOCK.with(|mock| *mock.borrow_mut() = Mock::default());
    unsafe { Startup::prepare(std::ptr::null()) }.unwrap()
}

fn allocation() -> (StackAllocation, Arc<AtomicUsize>) {
    let witness = Arc::new(AtomicUsize::new(0));
    let mut allocation = StackAllocation::allocate(65536).unwrap();
    allocation.witness(witness.clone());
    (allocation, witness)
}

#[test]
fn status_domains_stages_and_unknown_are_not_errno() {
    let statuses = [
        GnuStatus::Ok,
        GnuStatus::NotReady,
        GnuStatus::WrongOwner,
        GnuStatus::Invalid,
        GnuStatus::NoMemory,
        GnuStatus::Exhausted,
    ];
    for (code, status) in statuses.into_iter().enumerate() {
        let decoded = checked(result(Stage::Prepare, 2, code as u32), Stage::Prepare);
        if code == 0 {
            assert!(decoded.is_ok());
        } else {
            let error = decoded.unwrap_err();
            assert_eq!(
                error,
                ProviderError {
                    stage: Stage::Prepare,
                    status: Status::Gnu(status)
                }
            );
            let error = std::io::Error::other(error);
            assert_eq!(error.raw_os_error(), None);
        }
    }
    let adapters = [
        AdapterStatus::Ok,
        AdapterStatus::Phase,
        AdapterStatus::Owner,
        AdapterStatus::Invalid,
        AdapterStatus::Exhausted,
        AdapterStatus::Mismatch,
    ];
    for (code, status) in adapters.into_iter().enumerate().skip(1) {
        assert_eq!(
            checked(result(Stage::Check, 1, code as u32), Stage::Check)
                .unwrap_err()
                .status,
            Status::Adapter(status)
        );
    }
    for raw in [
        result(Stage::Prepare, 2, 6),
        result(Stage::Prepare, 0, 0),
        result(Stage::Query, 2, 0),
        RawResult {
            stage: 99,
            domain: 2,
            code: 0,
            reserved: 0,
        },
        RawResult {
            stage: 5,
            domain: 2,
            code: 0,
            reserved: 1,
        },
    ] {
        assert!(matches!(
            checked(raw, Stage::Prepare).unwrap_err().status,
            Status::Unknown { .. }
        ));
    }
    assert_eq!(
        checked(result(Stage::Adopt, 2, 3), Stage::Root)
            .unwrap_err()
            .stage,
        Stage::Adopt
    );
}

#[test]
fn successful_query_and_rollback_release_only_returned_owner() {
    let startup = startup();
    let (allocation, witness) = allocation();
    let expected = allocation.range();
    let reservation = startup.reserve_allocation(allocation).unwrap();
    let info = reservation.query().unwrap();
    assert_eq!(info.storage.allocation_low, expected.start);
    assert_eq!(info.storage.allocation_size, expected.len());
    assert_eq!(info.storage.allocation_id, 2);
    assert_eq!(info.storage.lower_guard_size, 0);
    assert_eq!(info.storage.upper_guard_size, 0);
    let allocation = reservation.rollback().unwrap();
    assert_eq!(allocation.range(), expected);
    assert_eq!(witness.load(Ordering::SeqCst), 0);
    drop(allocation);
    assert_eq!(witness.load(Ordering::SeqCst), 1);
}

#[test]
fn known_zero_ticket_failure_releases_unpublished_allocation() {
    let startup = startup();
    for code in 1..=5 {
        MOCK.with(|mock| mock.borrow_mut().code = code);
        let (allocation, witness) = allocation();
        assert!(matches!(
            startup.reserve_allocation(allocation),
            Err(ReserveError::Provider(_))
        ));
        assert_eq!(witness.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn nonzero_ticket_failure_and_query_failures_retain_storage() {
    let startup = startup();
    for mode in 0..3 {
        MOCK.with(|mock| {
            *mock.borrow_mut() = Mock {
                code: if mode == 0 { 4 } else { 0 },
                ticket_on_failure: mode == 0,
                query_error: mode == 1,
                query_corrupt: mode == 2,
                ..Mock::default()
            }
        });
        let (allocation, witness) = allocation();
        let error = startup.reserve_allocation(allocation).unwrap_err();
        assert!(
            matches!(&error, ReserveError::Retained(reservation, _) if reservation.ticket == 7)
        );
        drop(error);
        assert_eq!(witness.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn unknown_and_zero_success_leave_unresolved_storage_retained() {
    let startup = startup();
    for code in [0, 99] {
        MOCK.with(|mock| {
            *mock.borrow_mut() = Mock {
                code,
                zero_success: true,
                ..Mock::default()
            }
        });
        let (allocation, witness) = allocation();
        let error = startup.reserve_allocation(allocation).unwrap_err();
        assert!(
            matches!(&error, ReserveError::Retained(reservation, _) if reservation.ticket == 0)
        );
        drop(error);
        assert_eq!(witness.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn refused_rollback_returns_intact_reservation() {
    let startup = startup();
    let (allocation, witness) = allocation();
    let range = allocation.range();
    let reservation = startup.reserve_allocation(allocation).unwrap();
    MOCK.with(|mock| mock.borrow_mut().rollback_error = true);
    let (reservation, error) = reservation.rollback().unwrap_err();
    assert_eq!(error.status, Status::Gnu(GnuStatus::Invalid));
    assert_eq!(reservation.ticket, 7);
    assert_eq!(reservation.allocation.as_ref().unwrap().range(), range);
    drop(reservation);
    assert_eq!(witness.load(Ordering::SeqCst), 0);
}

#[test]
fn reservation_drop_and_unwind_do_not_release_borrowed_storage() {
    for unwind in [false, true] {
        let startup = startup();
        let (allocation, witness) = allocation();
        let reservation = startup.reserve_allocation(allocation).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reservation = reservation;
            if unwind {
                panic!("test borrowed storage unwind");
            }
        }));
        assert_eq!(result.is_err(), unwind);
        assert_eq!(witness.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn callback_style_explicit_leak_and_closed_phase() {
    let startup = startup();
    let (allocation, witness) = allocation();
    let expected = allocation.range();
    assert_eq!(allocation.leak(), expected);
    assert_eq!(witness.load(Ordering::SeqCst), 0);
    MOCK.with(|mock| mock.borrow_mut().closed = true);
    assert!(
        matches!(startup.reserve(usize::MAX), Err(ReserveError::Provider(error))
        if error.stage == Stage::Check && error.status == Status::Adapter(AdapterStatus::Phase))
    );
    startup.close().unwrap();
}
