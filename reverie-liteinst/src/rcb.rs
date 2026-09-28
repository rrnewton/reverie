//! Private cross-crate RCB control bridge.
//!
//! `reverie-preload` owns the signal-entry TLS and counter state. Its Rust
//! module is crate-private even when the LiteInst integration is compiled. This
//! module is the only Rust caller of one hidden fixed-layout control symbol;
//! Tool authors and ordinary dependencies receive no state or mutator API.

#[repr(C)]
struct Request {
    operation: u64,
    authority: usize,
    values: [u64; 15],
}

const _: () = {
    assert!(core::mem::size_of::<Request>() == 136);
    assert!(core::mem::align_of::<Request>() == 8);
    assert!(core::mem::offset_of!(Request, operation) == 0);
    assert!(core::mem::offset_of!(Request, authority) == 8);
    assert!(core::mem::offset_of!(Request, values) == 16);
};

// Address identity is the capability. This private, address-taken allocation
// has no Rust or dynamic-symbol API and root entry binds it before invoking an
// external setup body. A supported fork inherits the same process image. This
// boundary protects ordinary safe Rust consumers and separately linked DSOs;
// it does not claim to isolate hostile native code already linked into this DSO.
#[used]
static AUTHORITY: u8 = 0xa7;

const BEGIN_SETUP: u64 = 0;
const SETUP_UNAVAILABLE: u64 = 1;
const SETUP_FAILED: u64 = 2;
const ACCOUNTING_FAILED: u64 = 3;
const IS_BROKEN: u64 = 4;
const REGISTER: u64 = 5;
const VALIDATE_ROOT_RELEASE: u64 = 6;
const HOLD_SIGNAL_PAUSE: u64 = 7;
const RELEASE_SIGNAL_PAUSE: u64 = 8;
const SELECT_READER: u64 = 9;
const ACTIVE_ERROR: u64 = 10;
const CALLBACK_ACTIVE: u64 = 11;
const CALLBACK_REBIND_UNAVAILABLE: u64 = 12;
const CALLBACK_ADOPT_PAUSE: u64 = 13;
#[cfg(feature = "rcb-qualification")]
const ASSERT_PRISTINE: u64 = 14;
const BIND_AUTHORITY: u64 = 15;
#[cfg(feature = "rcb-qualification")]
const ARM_ASYNC_SIGSYS_PROBE: u64 = 17;
#[cfg(feature = "rcb-qualification")]
const ASYNC_SIGSYS_SNAPSHOT: u64 = 18;

pub(crate) const MODE_OFFSET: usize = 16;
pub(crate) const HELD_OFFSET: usize = 24;
pub(crate) const ENABLES_OFFSET: usize = 56;
pub(crate) const CPU_OFFSET: usize = 84;
pub(crate) const RUNNING_MODE: u32 = 1;

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PauseToken {
    owner: i32,
    generation: u64,
    pause: u64,
}

unsafe extern "C" {
    fn reverie_preload_rcb_control(request: *mut Request) -> i32;
}

fn request(operation: u64, values: [u64; 15]) -> Result<[u64; 15], i32> {
    request_with_authority(operation, core::ptr::addr_of!(AUTHORITY) as usize, values)
}

fn request_with_authority(
    operation: u64,
    authority: usize,
    values: [u64; 15],
) -> Result<[u64; 15], i32> {
    let mut request = Request {
        operation,
        authority,
        values,
    };
    let status = unsafe { reverie_preload_rcb_control(&raw mut request) };
    if status == 0 {
        Ok(request.values)
    } else {
        Err(if status > 0 { status } else { libc::EIO })
    }
}

pub(crate) fn bind() {
    infallible(empty(BIND_AUTHORITY));
}

fn empty(operation: u64) -> Result<[u64; 15], i32> {
    request(operation, [0; 15])
}

fn bridge_failed(errno: i32) -> ! {
    const MESSAGE: &[u8] = b"reverie-liteinst: private RCB bridge failed\n";
    unsafe {
        let _ = reverie_preload::trap::raw_syscall6(
            libc::SYS_write,
            [2, MESSAGE.as_ptr() as u64, MESSAGE.len() as u64, 0, 0, 0],
        );
        let _ = reverie_preload::trap::raw_syscall6(
            libc::SYS_exit_group,
            [u64::from(errno.unsigned_abs().min(255)), 0, 0, 0, 0, 0],
        );
    }
    loop {
        core::hint::spin_loop();
    }
}

fn infallible(result: Result<[u64; 15], i32>) -> [u64; 15] {
    result.unwrap_or_else(bridge_failed)
}

fn encode_token(token: Option<&PauseToken>) -> [u64; 15] {
    let mut values = [0; 15];
    if let Some(token) = token {
        values[0] = 1;
        values[1] = token.owner as i64 as u64;
        values[2] = token.generation;
        values[3] = token.pause;
    }
    values
}

fn decode_token(values: [u64; 15]) -> Result<Option<PauseToken>, i32> {
    match values[0] {
        0 => Ok(None),
        1 => Ok(Some(PauseToken {
            owner: i32::try_from(values[1] as i64).map_err(|_| libc::EIO)?,
            generation: values[2],
            pause: values[3],
        })),
        _ => Err(libc::EIO),
    }
}

pub(crate) fn begin_setup() -> Result<(), i32> {
    empty(BEGIN_SETUP).map(|_| ())
}

pub(crate) fn setup_unavailable() -> Result<(), i32> {
    empty(SETUP_UNAVAILABLE).map(|_| ())
}

pub(crate) fn setup_failed(errno: i32) {
    let mut values = [0; 15];
    values[0] = errno as i64 as u64;
    infallible(request(SETUP_FAILED, values));
}

pub(crate) fn accounting_failed(errno: i32) {
    let mut values = [0; 15];
    values[0] = errno as i64 as u64;
    infallible(request(ACCOUNTING_FAILED, values));
}

pub(crate) fn is_broken() -> bool {
    infallible(empty(IS_BROKEN))[0] == 1
}

pub(crate) unsafe fn register(
    fd: i32,
    paused: bool,
    cpu: u32,
) -> Result<Option<PauseToken>, i32> {
    let mut values = [0; 15];
    values[0] = fd as i64 as u64;
    values[1] = u64::from(paused);
    values[2] = u64::from(cpu);
    decode_token(request(REGISTER, values)?)
}

pub(crate) fn validate_root_release(token: Option<&PauseToken>) -> Result<i64, i32> {
    let values = request(VALIDATE_ROOT_RELEASE, encode_token(token))?;
    Ok(values[0] as i64)
}

pub(crate) fn hold_signal_pause() -> Result<Option<PauseToken>, i32> {
    decode_token(empty(HOLD_SIGNAL_PAUSE)?)
}

pub(crate) fn release_signal_pause(token: Option<PauseToken>) -> Result<(), i32> {
    request(RELEASE_SIGNAL_PAUSE, encode_token(token.as_ref())).map(|_| ())
}

pub(crate) unsafe fn select_reader(running: usize, paused: usize, invalid: usize) -> usize {
    let mut values = [0; 15];
    values[..3].copy_from_slice(&[running as u64, paused as u64, invalid as u64]);
    infallible(request(SELECT_READER, values))[0] as usize
}

pub(crate) fn active_error() -> i32 {
    i32::try_from(infallible(empty(ACTIVE_ERROR))[0])
        .ok()
        .filter(|errno| *errno > 0)
        .unwrap_or(libc::EIO)
}

pub(crate) mod callback {
    use super::*;

    pub(crate) fn active() -> bool {
        infallible(empty(CALLBACK_ACTIVE))[0] == 1
    }

    pub(crate) fn rebind_unavailable_fork_child() {
        infallible(empty(CALLBACK_REBIND_UNAVAILABLE));
    }

    pub(crate) fn adopt_pause(token: PauseToken) -> Result<(), i32> {
        request(CALLBACK_ADOPT_PAUSE, encode_token(Some(&token))).map(|_| ())
    }
}

#[cfg(feature = "rcb-qualification")]
pub(crate) fn arm_async_sigsys_probe() -> Result<(), i32> {
    empty(ARM_ASYNC_SIGSYS_PROBE).map(|_| ())
}

#[cfg(feature = "rcb-qualification")]
pub(crate) fn async_sigsys_snapshot() -> [u64; 2] {
    let values = infallible(empty(ASYNC_SIGSYS_SNAPSHOT));
    [values[0], values[1]]
}

#[cfg(test)]
#[cfg(feature = "rcb-qualification")]
pub(crate) fn assert_pristine() {
    infallible(empty(ASSERT_PRISTINE));
}

#[cfg(test)]
#[cfg(feature = "rcb-qualification")]
pub(crate) fn assert_forgery_refused() {
    #[used]
    static FORGED_AUTHORITY: u8 = 0x5c;
    assert_ne!(
        core::ptr::addr_of!(FORGED_AUTHORITY),
        core::ptr::addr_of!(AUTHORITY)
    );
    assert_eq!(
        request_with_authority(
            IS_BROKEN,
            core::ptr::addr_of!(FORGED_AUTHORITY) as usize,
            [0; 15],
        ),
        Err(libc::EPERM)
    );
    assert!(infallible(empty(IS_BROKEN))[0] <= 1);
}
