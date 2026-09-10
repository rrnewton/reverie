use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::owned_context::stack::StackAllocation;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StackRecord {
    pub version: u32,
    pub size: u32,
    pub allocation_id: u64,
    pub allocation_low: usize,
    pub allocation_size: usize,
    pub usable_low: usize,
    pub usable_size: usize,
    pub lower_guard_size: usize,
    pub upper_guard_size: usize,
    pub reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StackInfo {
    pub storage: StackRecord,
    pub gnu_stack_low: usize,
    pub gnu_stack_size: usize,
    pub gnu_reported_guard_size: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct RawResult {
    stage: u32,
    domain: u32,
    code: u32,
    reserved: u32,
}

const _: () = {
    use std::mem::offset_of;
    use std::mem::size_of;
    assert!(size_of::<StackRecord>() == 80);
    assert!(offset_of!(StackRecord, version) == 0);
    assert!(offset_of!(StackRecord, size) == 4);
    assert!(offset_of!(StackRecord, allocation_id) == 8);
    assert!(offset_of!(StackRecord, allocation_low) == 16);
    assert!(offset_of!(StackRecord, allocation_size) == 24);
    assert!(offset_of!(StackRecord, usable_low) == 32);
    assert!(offset_of!(StackRecord, usable_size) == 40);
    assert!(offset_of!(StackRecord, lower_guard_size) == 48);
    assert!(offset_of!(StackRecord, upper_guard_size) == 56);
    assert!(offset_of!(StackRecord, reserved) == 64);
    assert!(size_of::<StackInfo>() == 104);
    assert!(offset_of!(StackInfo, storage) == 0);
    assert!(offset_of!(StackInfo, gnu_stack_low) == 80);
    assert!(offset_of!(StackInfo, gnu_stack_size) == 88);
    assert!(offset_of!(StackInfo, gnu_reported_guard_size) == 96);
    assert!(size_of::<RawResult>() == 16);
    assert!(offset_of!(RawResult, stage) == 0);
    assert!(offset_of!(RawResult, domain) == 4);
    assert!(offset_of!(RawResult, code) == 8);
    assert!(offset_of!(RawResult, reserved) == 12);
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Stage {
    Root = 1,
    Domain,
    Adopt,
    RootQuery,
    Prepare,
    Query,
    Rollback,
    Close,
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GnuStatus {
    Ok,
    NotReady,
    WrongOwner,
    Invalid,
    NoMemory,
    Exhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterStatus {
    Ok,
    Phase,
    Owner,
    Invalid,
    Exhausted,
    Mismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Gnu(GnuStatus),
    Adapter(AdapterStatus),
    Unknown {
        stage: u32,
        domain: u32,
        code: u32,
        reserved: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderError {
    pub stage: Stage,
    pub status: Status,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "private GNU provider {:?}: {:?}",
            self.stage, self.status
        )
    }
}

impl std::error::Error for ProviderError {}

fn checked(raw: RawResult, expected: Stage) -> Result<(), ProviderError> {
    let stage = match raw.stage {
        1 => Some(Stage::Root),
        2 => Some(Stage::Domain),
        3 => Some(Stage::Adopt),
        4 => Some(Stage::RootQuery),
        5 => Some(Stage::Prepare),
        6 => Some(Stage::Query),
        7 => Some(Stage::Rollback),
        8 => Some(Stage::Close),
        9 => Some(Stage::Check),
        _ => None,
    };
    let unknown = Status::Unknown {
        stage: raw.stage,
        domain: raw.domain,
        code: raw.code,
        reserved: raw.reserved,
    };
    let status = match (raw.domain, raw.code) {
        (1, 0) => Status::Adapter(AdapterStatus::Ok),
        (1, 1) => Status::Adapter(AdapterStatus::Phase),
        (1, 2) => Status::Adapter(AdapterStatus::Owner),
        (1, 3) => Status::Adapter(AdapterStatus::Invalid),
        (1, 4) => Status::Adapter(AdapterStatus::Exhausted),
        (1, 5) => Status::Adapter(AdapterStatus::Mismatch),
        (2, 0) => Status::Gnu(GnuStatus::Ok),
        (2, 1) => Status::Gnu(GnuStatus::NotReady),
        (2, 2) => Status::Gnu(GnuStatus::WrongOwner),
        (2, 3) => Status::Gnu(GnuStatus::Invalid),
        (2, 4) => Status::Gnu(GnuStatus::NoMemory),
        (2, 5) => Status::Gnu(GnuStatus::Exhausted),
        _ => unknown,
    };
    if raw.reserved != 0 || stage.is_none() {
        return Err(ProviderError {
            stage: expected,
            status: unknown,
        });
    }
    if matches!(
        status,
        Status::Gnu(GnuStatus::Ok) | Status::Adapter(AdapterStatus::Ok)
    ) {
        return if stage == Some(expected) {
            Ok(())
        } else {
            Err(ProviderError {
                stage: expected,
                status: unknown,
            })
        };
    }
    Err(ProviderError {
        stage: stage.unwrap(),
        status,
    })
}

unsafe extern "C" {
    fn pl_gnu_root(context: *const c_void) -> RawResult;
    fn pl_gnu_check(context: *const c_void) -> RawResult;
    fn pl_gnu_close(context: *const c_void) -> RawResult;
    fn pl_gnu_prepare(
        context: *const c_void,
        stack: *mut StackRecord,
        ticket: *mut u64,
    ) -> RawResult;
    fn pl_gnu_query(context: *const c_void, ticket: u64, info: *mut StackInfo) -> RawResult;
    fn pl_gnu_rollback(context: *const c_void, ticket: u64) -> RawResult;
}

#[derive(Debug)]
pub struct Startup {
    context: *const c_void,
    open: bool,
    owner: PhantomData<Rc<()>>,
}

impl Startup {
    /// # Safety
    /// The retained CRT context must be live, immediately after successful
    /// private TLS preparation and before execution-context registration.
    pub unsafe fn prepare(context: *const c_void) -> Result<Self, ProviderError> {
        checked(unsafe { pl_gnu_root(context) }, Stage::Root)?;
        Ok(Self {
            context,
            open: true,
            owner: PhantomData,
        })
    }

    // Retaining the reservation in the error preserves rollback ownership.
    #[expect(clippy::result_large_err)]
    pub fn reserve(&self, bytes: usize) -> Result<Reservation, ReserveError> {
        checked(unsafe { pl_gnu_check(self.context) }, Stage::Check)
            .map_err(ReserveError::Provider)?;
        let allocation = StackAllocation::allocate(bytes).map_err(ReserveError::Allocation)?;
        self.reserve_allocation(allocation)
    }

    // Retaining the reservation in the error preserves rollback ownership.
    #[expect(clippy::result_large_err)]
    fn reserve_allocation(&self, allocation: StackAllocation) -> Result<Reservation, ReserveError> {
        let range = allocation.range();
        let mut reservation = Reservation {
            context: self.context,
            ticket: 0,
            allocation: Some(allocation),
            record: StackRecord {
                version: 1,
                size: 80,
                allocation_low: range.start,
                allocation_size: range.len(),
                usable_low: range.start,
                usable_size: range.len(),
                ..StackRecord::default()
            },
            owner: PhantomData,
        };
        let result = unsafe {
            pl_gnu_prepare(
                self.context,
                &mut reservation.record,
                &mut reservation.ticket,
            )
        };
        if let Err(error) = checked(result, Stage::Prepare) {
            if reservation.ticket == 0
                && error.stage == Stage::Prepare
                && matches!(
                    error.status,
                    Status::Gnu(
                        GnuStatus::NotReady
                            | GnuStatus::WrongOwner
                            | GnuStatus::Invalid
                            | GnuStatus::NoMemory
                            | GnuStatus::Exhausted
                    ) | Status::Adapter(
                        AdapterStatus::Phase
                            | AdapterStatus::Owner
                            | AdapterStatus::Invalid
                            | AdapterStatus::Exhausted
                    )
                )
            {
                drop(reservation.allocation.take());
                return Err(ReserveError::Provider(error));
            }
            return Err(ReserveError::Retained(reservation, error));
        }
        if reservation.ticket == 0 {
            return Err(ReserveError::Retained(
                reservation,
                mismatch(Stage::Prepare),
            ));
        }
        if let Err(error) = reservation.query() {
            return Err(ReserveError::Retained(reservation, error));
        }
        Ok(reservation)
    }

    pub fn close(mut self) -> Result<(), ProviderError> {
        self.open = false;
        checked(unsafe { pl_gnu_close(self.context) }, Stage::Close)
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        if self.open {
            let _ = unsafe { pl_gnu_close(self.context) };
        }
    }
}

#[derive(Debug)]
pub enum ReserveError {
    Allocation(std::io::Error),
    Provider(ProviderError),
    Retained(Reservation, ProviderError),
}

#[derive(Debug)]
pub struct Reservation {
    context: *const c_void,
    ticket: u64,
    record: StackRecord,
    allocation: Option<StackAllocation>,
    owner: PhantomData<Rc<()>>,
}

fn mismatch(stage: Stage) -> ProviderError {
    ProviderError {
        stage,
        status: Status::Adapter(AdapterStatus::Mismatch),
    }
}

impl Reservation {
    pub fn query(&self) -> Result<StackInfo, ProviderError> {
        let mut info = StackInfo::default();
        checked(
            unsafe { pl_gnu_query(self.context, self.ticket, &mut info) },
            Stage::Query,
        )?;
        let range = self.allocation.as_ref().unwrap().range();
        if self.record.version != 1
            || self.record.size != 80
            || self.record.allocation_id == 0
            || self.record.allocation_low != range.start
            || self.record.allocation_size != range.len()
            || self.record.usable_low != range.start
            || self.record.usable_size != range.len()
            || self.record.lower_guard_size != 0
            || self.record.upper_guard_size != 0
            || self.record.reserved != [0; 2]
            || info.storage != self.record
            || info.gnu_stack_low != range.start
            || info.gnu_stack_size != range.len()
            || info.gnu_reported_guard_size != 0
        {
            return Err(mismatch(Stage::Query));
        }
        Ok(info)
    }

    // A failed rollback must return the live reservation to its caller.
    #[expect(clippy::result_large_err)]
    pub fn rollback(mut self) -> Result<StackAllocation, (Self, ProviderError)> {
        if let Err(error) = checked(
            unsafe { pl_gnu_rollback(self.context, self.ticket) },
            Stage::Rollback,
        ) {
            return Err((self, error));
        }
        Ok(self.allocation.take().unwrap())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(allocation) = self.allocation.take() {
            allocation.leak();
        }
    }
}

#[cfg(test)]
mod tests;
