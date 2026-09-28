/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Owner-bound SIGSYS counter boundaries. This module does not create counters.
//!
//! The runtime registers an owned perf event in ordinary context. The common
//! assembly entry stops it before Rust and the final assembly epilogue resumes
//! it after Rust. Installed callbacks use the same physical boundary discipline.

mod callback;

use core::arch::global_asm;
use core::mem::align_of;
use core::mem::offset_of;
use core::mem::size_of;
#[cfg(feature = "rcb-qualification")]
use core::sync::atomic::AtomicI32;
#[cfg(feature = "rcb-qualification")]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::Ordering;

const UNAVAILABLE: u32 = 0;
const RUNNING: u32 = 1;
const PAUSED: u32 = 2;
const BUILDING: u32 = 3;
const BROKEN: u32 = 4;
const CPU_OFFSET: usize = 84;

#[cfg(feature = "rcb-qualification")]
static ASYNC_SIGSYS_PROBE: AtomicI32 = AtomicI32::new(0);
#[cfg(feature = "rcb-qualification")]
static ASYNC_SIGSYS_ENTRIES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "rcb-qualification")]
static ASYNC_SIGSYS_MODE: AtomicUsize = AtomicUsize::new(usize::MAX);

#[cfg(feature = "rcb-qualification")]
fn arm_async_sigsys_probe() -> Result<(), i32> {
    ASYNC_SIGSYS_PROBE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| libc::EALREADY)
}

#[cfg(feature = "rcb-qualification")]
pub(super) fn consume_async_sigsys_probe(signal: i32, code: i32) -> bool {
    if signal != libc::SIGSYS || code == 1 || ASYNC_SIGSYS_PROBE.swap(0, Ordering::AcqRel) != 1 {
        return false;
    }
    let state = unsafe { &*record() };
    ASYNC_SIGSYS_MODE.store(state.mode as usize, Ordering::Release);
    ASYNC_SIGSYS_ENTRIES.fetch_add(1, Ordering::AcqRel);
    true
}

#[repr(C)]
struct Record {
    fd: i32,
    owner: i32,
    generation: u64,
    mode: u32,
    entry_owned: u32,
    held: u32,
    release: u32,
    error: i64,
    pause_serial: u64,
    disables: u64,
    enables: u64,
    owner_queries: u64,
    callback_top: usize,
    callback_depth: u32,
    cpu: i32,
}

const _: () = {
    assert!(size_of::<Record>() == 88);
    assert!(align_of::<Record>() == 8);
    assert!(offset_of!(Record, fd) == 0);
    assert!(offset_of!(Record, owner) == 4);
    assert!(offset_of!(Record, generation) == 8);
    assert!(offset_of!(Record, mode) == 16);
    assert!(offset_of!(Record, entry_owned) == 20);
    assert!(offset_of!(Record, held) == 24);
    assert!(offset_of!(Record, release) == 28);
    assert!(offset_of!(Record, error) == 32);
    assert!(offset_of!(Record, pause_serial) == 40);
    assert!(offset_of!(Record, disables) == 48);
    assert!(offset_of!(Record, enables) == 56);
    assert!(offset_of!(Record, owner_queries) == 64);
    assert!(offset_of!(Record, callback_top) == 72);
    assert!(offset_of!(Record, callback_depth) == 80);
    assert!(offset_of!(Record, cpu) == CPU_OFFSET);
};

/// Permission to release one counter pause, not a descriptor owner.
#[derive(Debug, Eq, PartialEq)]
struct PauseToken {
    owner: i32,
    generation: u64,
    pause: u64,
}

unsafe extern "C" {
    fn reverie_preload_rcb_record() -> *mut Record;
    fn reverie_preload_rcb_select_reader(running: usize, paused: usize, invalid: usize) -> usize;
}

fn record() -> *mut Record {
    // Initial-exec TLS, with no __tls_get_addr or lazy Rust TLS initializer.
    unsafe { reverie_preload_rcb_record() }
}

fn owner() -> Result<i32, i32> {
    let value = unsafe { super::raw_syscall6(libc::SYS_gettid, [0; 6]) };
    unsafe { (*record()).owner_queries = (*record()).owner_queries.wrapping_add(1) };
    if (-4095..0).contains(&value) {
        return Err((-value) as i32);
    }
    i32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(libc::EIO)
}

fn validate_cpu(state: &Record) -> Result<(), i32> {
    if state.cpu < 0 {
        return Err(libc::ESTALE);
    }
    let mut cpu = u32::MAX;
    let result = unsafe {
        super::raw_syscall6(
            libc::SYS_getcpu,
            [(&raw mut cpu) as u64, 0, 0, 0, 0, 0],
        )
    };
    if result == 0 && cpu == state.cpu as u32 {
        Ok(())
    } else if result < 0 {
        Err(i32::try_from(-result).unwrap_or(libc::EIO))
    } else {
        Err(libc::ESTALE)
    }
}

/// Remove permission to control an inherited or incompletely prepared event.
/// Run in ordinary context before any child RPC/allocation can reenter SIGSYS.
/// The runtime still owns cleanup of the old descriptor and counter object.
fn begin_setup() -> Result<(), i32> {
    let tid = owner()?;
    let state = unsafe { &mut *record() };
    begin_setup_with(state, tid)
}

fn begin_setup_with(state: &mut Record, tid: i32) -> Result<(), i32> {
    if tid <= 0 {
        return Err(libc::EINVAL);
    }
    if state.mode == BROKEN {
        return Err(error_from(state));
    }
    if state.owner == tid && matches!(state.mode, RUNNING | PAUSED) {
        return Err(libc::EALREADY);
    }
    state.mode = BUILDING;
    state.fd = -1;
    state.owner = tid;
    state.entry_owned = 0;
    state.held = 0;
    state.release = 0;
    state.cpu = -1;
    state.error = 0;
    let generation = state.generation;
    let pause = state.pause_serial;
    callback::rebind(state, generation, pause)?;
    Ok(())
}

/// Complete genuinely unavailable optional setup, without fabricating a clock.
fn setup_unavailable() -> Result<(), i32> {
    let tid = owner()?;
    let state = unsafe { &mut *record() };
    if state.mode != BUILDING || state.owner != tid {
        return Err(libc::EINVAL);
    }
    state.fd = -1;
    state.mode = UNAVAILABLE;
    Ok(())
}

/// Preserve a real acquisition failure as a terminal clock error.
/// No optional-availability transition is permitted after this failure.
fn setup_failed(errno: i32) {
    let state = unsafe { &mut *record() };
    if state.mode == BROKEN {
        return;
    }
    state.fd = -1;
    state.error = -i64::from(if errno > 0 { errno } else { libc::EIO });
    state.held = 0;
    state.release = 0;
    state.mode = BROKEN;
}

/// Preserve the first monotonic-accounting failure as a terminal boundary.
/// The event descriptor stays owned for process cleanup, but every permission
/// capable of returning or physically enabling it is revoked.
fn accounting_failed(errno: i32) {
    accounting_failed_with(unsafe { &mut *record() }, errno);
}

fn accounting_failed_with(state: &mut Record, errno: i32) {
    if state.mode == BROKEN {
        return;
    }
    state.error = -i64::from(if errno > 0 { errno } else { libc::EIO });
    state.entry_owned = 0;
    state.held = 0;
    state.release = 0;
    state.mode = BROKEN;
}

fn error_from(state: &Record) -> i32 {
    state
        .error
        .checked_neg()
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(libc::EIO)
}

/// True only for a retained control/acquisition error, not absent hardware.
fn is_broken() -> bool {
    unsafe { (*record()).mode == BROKEN }
}

/// Register a live event owned by this thread. Publish mode last.
///
/// # Safety
/// The caller owns fd for every boundary use, protects it from admitted guest
/// descriptor operations, and exclusively controls this event. `paused` must
/// describe its actual kernel state. A paused registration belongs to the
/// current ordinary child continuation and returns its new release token.
unsafe fn register(fd: i32, paused: bool, cpu: u32) -> Result<Option<PauseToken>, i32> {
    let tid = owner()?;
    let state = unsafe { &mut *record() };
    if fd < 0
        || cpu > i32::MAX as u32
        || state.mode != BUILDING
        || state.owner != tid
        || (state.callback_top != 0 && !paused)
    {
        return Err(libc::EINVAL);
    }
    let generation = state.generation.checked_add(1).ok_or(libc::EOVERFLOW)?;
    let pause = if paused {
        state.pause_serial.checked_add(1).ok_or(libc::EOVERFLOW)?
    } else {
        state.pause_serial
    };
    callback::rebind(state, generation, pause)?;
    state.fd = fd;
    state.generation = generation;
    state.pause_serial = pause;
    state.entry_owned = 0;
    state.held = u32::from(paused);
    state.release = 0;
    state.error = 0;
    state.cpu = cpu as i32;
    state.mode = if paused { PAUSED } else { RUNNING };
    Ok(paused.then_some(PauseToken {
        owner: tid,
        generation,
        pause,
    }))
}

/// Validate the initial root pause only after runtime setup and the real root
/// lifecycle have completed. This grants an assembly action without changing
/// the logical state. The assembly boundary must first enable the event and
/// only then publish `held=0` and `mode=RUNNING`, while signals remain blocked.
/// A root cannot use a signal's or installed frame's pause.
fn validate_root_release(token: Option<&PauseToken>) -> Result<i64, i32> {
    let tid = owner()?;
    let state = unsafe { &*record() };
    let action = validate_root_release_with(state, token, tid)?;
    if action > 0 {
        validate_cpu(state)?;
    }
    Ok(action)
}

fn validate_root_release_with(
    state: &Record,
    token: Option<&PauseToken>,
    tid: i32,
) -> Result<i64, i32> {
    if tid <= 0 || state.owner != tid || state.callback_top != 0 || state.callback_depth != 0
        || state.entry_owned != 0 || state.release != 0
    {
        return Err(libc::ESTALE);
    }
    match token {
        None if state.mode == UNAVAILABLE && state.fd == -1 && state.held == 0 => Ok(0),
        Some(token) if state.mode == PAUSED && state.fd >= 0 && state.held == 1
            && token.owner == tid && token.generation == state.generation
            && token.pause == state.pause_serial =>
        {
            Ok(i64::from(state.fd) + 1)
        }
        _ => Err(libc::ESTALE),
    }
}

#[cfg(test)]
fn assert_root_release_ownership() {
    fn state() -> Record {
        let mut value: Record = unsafe { core::mem::zeroed() };
        value.fd = 47;
        value.owner = 100;
        value.generation = 9;
        value.pause_serial = 3;
        value.mode = PAUSED;
        value.held = 1;
        value
    }
    fn token() -> PauseToken { PauseToken { owner: 100, generation: 9, pause: 3 } }
    fn fields(value: &Record) -> (i32, i32, u64, u32, u32, u32, u32, u64, usize, u32) {
        (value.fd, value.owner, value.generation, value.mode, value.entry_owned,
            value.held, value.release, value.pause_serial, value.callback_top, value.callback_depth)
    }
    for case in 0..13 {
        let mut state = state();
        let mut permission = token();
        let mut owner = 100;
        match case {
            0 => permission.owner = 101,
            1 => permission.generation = 8,
            2 => permission.pause = 2,
            3 => state.owner = 101,
            4 => state.mode = BUILDING,
            5 => state.mode = BROKEN,
            6 => state.callback_top = 1,
            7 => state.callback_depth = 1,
            8 => state.entry_owned = 1,
            9 => state.release = 1,
            10 => state.held = 0,
            11 => state.fd = -1,
            12 => owner = 0,
            _ => unreachable!(),
        }
        let before = fields(&state);
        assert_eq!(
            validate_root_release_with(&state, Some(&permission), owner),
            Err(libc::ESTALE)
        );
        assert_eq!(fields(&state), before, "refused root permission changed state: {case}");
    }
    let mut running = state();
    let permission = token();
    let before = fields(&running);
    assert_eq!(
        validate_root_release_with(&running, Some(&permission), 100),
        Ok(48)
    );
    assert_eq!(fields(&running), before, "validation published release early");
    running.mode = RUNNING;
    running.held = 0;
    assert_eq!(
        validate_root_release_with(&running, Some(&permission), 100),
        Err(libc::ESTALE)
    );
    let mut replacement = state();
    replacement.generation += 1;
    assert_eq!(
        validate_root_release_with(&replacement, Some(&token()), 100),
        Err(libc::ESTALE)
    );
    let mut unavailable = state();
    unavailable.mode = UNAVAILABLE;
    unavailable.fd = -1;
    unavailable.held = 0;
    let before = fields(&unavailable);
    assert_eq!(validate_root_release_with(&unavailable, None, 100), Ok(0));
    assert_eq!(fields(&unavailable), before, "unavailable validation changed state");
    unavailable.mode = BROKEN;
    assert_eq!(
        validate_root_release_with(&unavailable, None, 100),
        Err(libc::ESTALE)
    );
}

/// Transfer this signal's owned pause to its ordinary continuation.
fn hold_signal_pause() -> Result<Option<PauseToken>, i32> {
    let tid = owner()?;
    let state = unsafe { &mut *record() };
    match state.mode {
        UNAVAILABLE => Ok(None),
        PAUSED if state.owner == tid && state.entry_owned == 1 && state.held == 0 => {
            state.held = 1;
            Ok(Some(PauseToken {
                owner: tid,
                generation: state.generation,
                pause: state.pause_serial,
            }))
        }
        _ => Err(libc::EINVAL),
    }
}

/// Consume the exact continuation's token after its genuine-frame copy.
fn release_signal_pause(token: Option<PauseToken>) -> Result<(), i32> {
    let tid = owner()?;
    let state = unsafe { &mut *record() };
    match token {
        None if state.mode == UNAVAILABLE && state.owner == tid => Ok(()),
        Some(token)
            if state.mode == PAUSED
                && state.held == 1
                && state.owner == tid
                && token.owner == tid
                && token.generation == state.generation
                && token.pause == state.pause_serial =>
        {
            state.held = 0;
            state.release = 1;
            Ok(())
        }
        _ => Err(libc::EINVAL),
    }
}

/// Select an ordinary counter reader without adding a conditional branch
/// before the existing installed-handler sample.
///
/// # Safety
/// All three addresses must be callable with the exact same Rust function
/// type. The caller must already have validated current counter ownership.
unsafe fn select_reader(running: usize, paused: usize, invalid: usize) -> usize {
    unsafe { reverie_preload_rcb_select_reader(running, paused, invalid) }
}

/// Preserve a failed active boundary as an error in ordinary clock reads.
fn active_error() -> i32 {
    error_from(unsafe { &*record() })
}

#[cfg(feature = "rcb-qualification")]
pub(super) fn callback_boundary() -> (u64, usize) {
    callback::boundary()
}

#[repr(C)]
pub(crate) struct ControlRequest {
    operation: u64,
    authority: usize,
    values: [u64; 15],
}

const _: () = {
    assert!(size_of::<ControlRequest>() == 136);
    assert!(align_of::<ControlRequest>() == 8);
    assert!(offset_of!(ControlRequest, operation) == 0);
    assert!(offset_of!(ControlRequest, authority) == 8);
    assert!(offset_of!(ControlRequest, values) == 16);
};

const CONTROL_BEGIN_SETUP: u64 = 0;
const CONTROL_SETUP_UNAVAILABLE: u64 = 1;
const CONTROL_SETUP_FAILED: u64 = 2;
const CONTROL_ACCOUNTING_FAILED: u64 = 3;
const CONTROL_IS_BROKEN: u64 = 4;
const CONTROL_REGISTER: u64 = 5;
const CONTROL_VALIDATE_ROOT_RELEASE: u64 = 6;
const CONTROL_HOLD_SIGNAL_PAUSE: u64 = 7;
const CONTROL_RELEASE_SIGNAL_PAUSE: u64 = 8;
const CONTROL_SELECT_READER: u64 = 9;
const CONTROL_ACTIVE_ERROR: u64 = 10;
const CONTROL_CALLBACK_ACTIVE: u64 = 11;
const CONTROL_CALLBACK_REBIND_UNAVAILABLE: u64 = 12;
const CONTROL_CALLBACK_ADOPT_PAUSE: u64 = 13;
#[cfg(feature = "rcb-qualification")]
const CONTROL_ASSERT_PRISTINE: u64 = 14;
const CONTROL_BIND_AUTHORITY: u64 = 15;
#[cfg(feature = "rcb-qualification")]
const CONTROL_ARM_ASYNC_SIGSYS_PROBE: u64 = 17;
#[cfg(feature = "rcb-qualification")]
const CONTROL_ASYNC_SIGSYS_SNAPSHOT: u64 = 18;
static CONTROL_AUTHORITY: AtomicUsize = AtomicUsize::new(0);

fn control_token(values: &[u64; 15]) -> Result<Option<PauseToken>, i32> {
    match values[0] {
        0 => Ok(None),
        1 => Ok(Some(PauseToken {
            owner: i32::try_from(values[1] as i64).map_err(|_| libc::EINVAL)?,
            generation: values[2],
            pause: values[3],
        })),
        _ => Err(libc::EINVAL),
    }
}

fn write_control_token(values: &mut [u64; 15], token: Option<PauseToken>) {
    values.fill(0);
    if let Some(token) = token {
        values[0] = 1;
        values[1] = token.owner as i64 as u64;
        values[2] = token.generation;
        values[3] = token.pause;
    }
}

/// Hidden fixed-layout integration ABI. The entire safe state/control surface
/// remains private to this crate; reverie-liteinst wraps this symbol in its own
/// private module. It is not a Rust API or dynamic symbol.
#[unsafe(export_name = "reverie_preload_rcb_control")]
pub(crate) unsafe extern "C" fn control(request: *mut ControlRequest) -> i32 {
    let Some(request) = (unsafe { request.as_mut() }) else {
        return libc::EINVAL;
    };
    if request.operation == CONTROL_BIND_AUTHORITY {
        if request.authority == 0 {
            return libc::EINVAL;
        }
        return match CONTROL_AUTHORITY.compare_exchange(
            0,
            request.authority,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => 0,
            Err(authority) if authority == request.authority => 0,
            Err(_) => libc::EPERM,
        };
    }
    if request.authority == 0
        || CONTROL_AUTHORITY.load(Ordering::Acquire) != request.authority
    {
        return libc::EPERM;
    }
    let result = match request.operation {
        CONTROL_BEGIN_SETUP => begin_setup(),
        CONTROL_SETUP_UNAVAILABLE => setup_unavailable(),
        CONTROL_SETUP_FAILED => {
            setup_failed(i32::try_from(request.values[0] as i64).unwrap_or(libc::EIO));
            Ok(())
        }
        CONTROL_ACCOUNTING_FAILED => {
            accounting_failed(i32::try_from(request.values[0] as i64).unwrap_or(libc::EIO));
            Ok(())
        }
        CONTROL_IS_BROKEN => {
            request.values.fill(0);
            request.values[0] = u64::from(is_broken());
            Ok(())
        }
        CONTROL_REGISTER => {
            let fd = i32::try_from(request.values[0]).map_err(|_| libc::EINVAL);
            let cpu = u32::try_from(request.values[2]).map_err(|_| libc::EINVAL);
            let paused = match request.values[1] {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(libc::EINVAL),
            };
            fd.and_then(|fd| paused.and_then(|paused| cpu.map(|cpu| (fd, paused, cpu))))
                .and_then(|(fd, paused, cpu)| unsafe { register(fd, paused, cpu) })
                .map(|token| write_control_token(&mut request.values, token))
        }
        CONTROL_VALIDATE_ROOT_RELEASE => control_token(&request.values)
            .and_then(|token| validate_root_release(token.as_ref()))
            .map(|action| {
                request.values.fill(0);
                request.values[0] = action as u64;
            }),
        CONTROL_HOLD_SIGNAL_PAUSE => {
            hold_signal_pause().map(|token| write_control_token(&mut request.values, token))
        }
        CONTROL_RELEASE_SIGNAL_PAUSE => {
            control_token(&request.values).and_then(release_signal_pause)
        }
        CONTROL_SELECT_READER => {
            let selected = unsafe {
                select_reader(
                    request.values[0] as usize,
                    request.values[1] as usize,
                    request.values[2] as usize,
                )
            };
            request.values.fill(0);
            request.values[0] = selected as u64;
            Ok(())
        }
        CONTROL_ACTIVE_ERROR => {
            request.values.fill(0);
            request.values[0] = active_error() as u64;
            Ok(())
        }
        CONTROL_CALLBACK_ACTIVE => {
            request.values.fill(0);
            request.values[0] = u64::from(callback::active());
            Ok(())
        }
        CONTROL_CALLBACK_REBIND_UNAVAILABLE => {
            callback::rebind_unavailable_fork_child();
            Ok(())
        }
        CONTROL_CALLBACK_ADOPT_PAUSE => control_token(&request.values).and_then(|token| {
            token
                .ok_or(libc::EINVAL)
                .and_then(callback::adopt_pause)
        }),
        #[cfg(feature = "rcb-qualification")]
        CONTROL_ASSERT_PRISTINE => {
            let state = unsafe { &*record() };
            if state.fd == -1
                && state.owner == 0
                && state.generation == 0
                && state.mode == UNAVAILABLE
                && state.entry_owned == 0
                && state.held == 0
                && state.release == 0
                && state.error == 0
                && state.pause_serial == 0
                && state.disables == 0
                && state.enables == 0
                && state.owner_queries == 0
                && state.callback_top == 0
                && state.callback_depth == 0
                && state.cpu == 0
            {
                Ok(())
            } else {
                Err(libc::ESTALE)
            }
        }
        #[cfg(feature = "rcb-qualification")]
        CONTROL_ARM_ASYNC_SIGSYS_PROBE => arm_async_sigsys_probe(),
        #[cfg(feature = "rcb-qualification")]
        CONTROL_ASYNC_SIGSYS_SNAPSHOT => {
            request.values.fill(0);
            request.values[0] = ASYNC_SIGSYS_ENTRIES.load(Ordering::Acquire) as u64;
            request.values[1] = ASYNC_SIGSYS_MODE.load(Ordering::Acquire) as u64;
            Ok(())
        }
        _ => Err(libc::EINVAL),
    };
    match result {
        Ok(()) => 0,
        Err(errno) if errno > 0 => errno,
        Err(_) => libc::EIO,
    }
}

/// Finish while disabled; the returned positive value is fd+1, zero means no
/// enable. Only the assembly epilogue consumes this action.
pub(super) fn finish_signal() -> i64 {
    // Other preload backends register no counter. Do not add a gettid kernel
    // operation to their ordinary return path just to confirm absence.
    let mode = unsafe { (*record()).mode };
    if mode == UNAVAILABLE || mode == BUILDING {
        return 0;
    }
    let tid = match owner() {
        Ok(tid) => tid,
        Err(errno) => return -i64::from(errno),
    };
    let state = unsafe { &mut *record() };
    match state.mode {
        PAUSED if state.owner == tid => {
            if state.held != 0 {
                return 0;
            }
            if state.release == 0 && state.entry_owned == 0 {
                return 0;
            }
            if state.fd < 0 {
                return -i64::from(libc::EBADF);
            }
            state.release = 0;
            state.entry_owned = 0;
            if let Err(errno) = validate_cpu(state) {
                return -i64::from(errno);
            }
            // Assembly keeps all signals masked, physically enables the event,
            // and only then publishes RUNNING before restoring this entry mask.
            i64::from(state.fd) + 1
        }
        BROKEN => state.error,
        _ => -i64::from(libc::ESTALE),
    }
}

unsafe extern "C" fn failed(raw_error: i64) -> ! {
    let state = unsafe { &mut *record() };
    state.mode = BROKEN;
    state.error = if raw_error < 0 {
        raw_error
    } else {
        -i64::from(libc::EIO)
    };
    // Fixed stack storage, no formatting allocation, lock, retry or TLS errno.
    let mut message = *b"reverie-preload: RCB boundary error raw=0x0000000000000000\n";
    let start = message.len() - 17;
    let value = state.error as u64;
    for index in 0..16 {
        let digit = ((value >> ((15 - index) * 4)) & 15) as u8;
        message[start + index] = if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        };
    }
    unsafe {
        super::raw_syscall6(
            libc::SYS_write,
            [2, message.as_ptr() as u64, message.len() as u64, 0, 0, 0],
        );
        super::raw_syscall6(libc::SYS_exit_group, [125, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop()
    }
}

// The final DSO must bind this initial-
// exec relocation, every selected path and the actual restorer instructions.
global_asm!(r#"
    .hidden reverie_preload_rcb_control
    .section .tdata,"awT",@progbits
    .p2align 3
    .global reverie_preload_rcb_tls
    .hidden reverie_preload_rcb_tls
    .type reverie_preload_rcb_tls,@tls_object
reverie_preload_rcb_tls:
    .long -1
    .long 0
    .zero 80
    .size reverie_preload_rcb_tls,88

    .text
    .p2align 4
    .global reverie_preload_rcb_record
    .hidden reverie_preload_rcb_record
    .type reverie_preload_rcb_record,@function
reverie_preload_rcb_record:
    mov rax, qword ptr [rip + reverie_preload_rcb_tls@gottpoff]
    add rax, qword ptr fs:[0]
    ret
    .size reverie_preload_rcb_record,.-reverie_preload_rcb_record

    .p2align 4
    .global reverie_preload_rcb_select_reader
    .hidden reverie_preload_rcb_select_reader
    .type reverie_preload_rcb_select_reader,@function
reverie_preload_rcb_select_reader:
    mov r10,qword ptr [rip + reverie_preload_rcb_tls@gottpoff]
    add r10,qword ptr fs:[0]
    mov rax,rdx
    cmp dword ptr [r10+16],{running}
    cmove rax,rdi
    cmp dword ptr [r10+16],{paused}
    cmove rax,rsi
    ret
    .size reverie_preload_rcb_select_reader,.-reverie_preload_rcb_select_reader

    .p2align 4
    .global reverie_preload_sigsys_entry
    .hidden reverie_preload_sigsys_entry
    .type reverie_preload_sigsys_entry,@function
reverie_preload_sigsys_entry:
    // Kernel entry has RSP = 8 mod 16; keep the normal restorer return address.
    push r12
    push r13
    push r14
    push r15
    sub rsp,72
    mov qword ptr [rsp],0
    mov [rsp+8],rdi
    mov [rsp+16],rsi
    mov [rsp+24],rdx
    // Kernel-installed sa_mask already excludes unrelated asynchronous
    // handlers. Reach initial-exec TLS and issue DISABLE before the first
    // retired conditional branch in this entry. A failed RUNNING disable exits
    // through straight-line/CMOV dispatch and can never return a result.
    call reverie_preload_rcb_record
    mov r12,rax
    mov edi,{ioctl}
    movsxd rsi,dword ptr [r12]
    mov edx,{disable}
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov r15,rax
    cmp dword ptr [r12+16],{running}
    sete al
    test r15,r15
    setne dl
    and al,dl
    lea r13,[rip+.Lrcb_block_signals]
    lea r14,[rip+.Lrcb_early_disable_fatal]
    test al,al
    cmovne r13,r14
    jmp r13
.Lrcb_early_disable_fatal:
    mov edi,{exit_group}
    mov esi,125
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    ud2
    .global reverie_preload_rcb_sigsys_block_signals
    .hidden reverie_preload_rcb_sigsys_block_signals
reverie_preload_rcb_sigsys_block_signals:
.Lrcb_block_signals:
    mov qword ptr [rsp+48],-1
    mov edi,{sigprocmask}
    mov esi,{block}
    lea rdx,[rsp+48]
    lea rcx,[rsp+40]
    mov r8d,8
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lrcb_initial_mask_error]
    lea r14,[rip+.Lrcb_signals_blocked]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_preload_rcb_sigsys_initial_mask_error
    .hidden reverie_preload_rcb_sigsys_initial_mask_error
reverie_preload_rcb_sigsys_initial_mask_error:
.Lrcb_initial_mask_error:
    mov edi,{exit_group}
    mov esi,125
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    ud2
    .global reverie_preload_rcb_sigsys_signals_blocked
    .hidden reverie_preload_rcb_sigsys_signals_blocked
reverie_preload_rcb_sigsys_signals_blocked:
.Lrcb_signals_blocked:
    cld
    mov dword ptr [r12+20],0
    lea r13,[rip+.Lrcb_call_handler]
    lea r14,[rip+.Lrcb_check_owner]
    cmp dword ptr [r12+16],{running}
    cmove r13,r14
    jmp r13
.Lrcb_check_owner:
    mov edi,{gettid}
    xor esi,esi
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    add qword ptr [r12+64],1
    lea r13,[rip+.Lrcb_owner_error]
    lea r14,[rip+.Lrcb_disable]
    cmp eax,dword ptr [r12+4]
    cmove r13,r14
    jmp r13
.Lrcb_owner_error:
    mov rdi,-{stale}
    // Preserve a failed owner-query errno rather than relabelling it ESTALE.
    test rax,rax
    cmovs rdi,rax
    call {failed}
    ud2
    .global reverie_preload_rcb_sigsys_disable
    .hidden reverie_preload_rcb_sigsys_disable
reverie_preload_rcb_sigsys_disable:
.Lrcb_disable:
    // The entry fence already performed the one physical DISABLE. Reject a
    // migrated target before dispatcher code can produce a side effect;
    // finish validates the same CPU again immediately before ENABLE.
    mov dword ptr [rsp+56],-1
    mov edi,{getcpu}
    lea rsi,[rsp+56]
    xor edx,edx
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,-{stale}
    test rax,rax
    cmovs rdi,rax
    lea r13,[rip+.Lrcb_control_error]
    lea r14,[rip+.Lrcb_cpu]
    test rax,rax
    cmovz r13,r14
    jmp r13
.Lrcb_cpu:
    mov eax,dword ptr [rsp+56]
    lea r13,[rip+.Lrcb_control_error]
    lea r14,[rip+.Lrcb_disabled]
    cmp eax,dword ptr [r12+{cpu_offset}]
    cmove r13,r14
    jmp r13
    .global reverie_preload_rcb_sigsys_control_error
    .hidden reverie_preload_rcb_sigsys_control_error
reverie_preload_rcb_sigsys_control_error:
.Lrcb_control_error:
    call {failed}
    ud2
    .global reverie_preload_rcb_sigsys_disabled
    .hidden reverie_preload_rcb_sigsys_disabled
reverie_preload_rcb_sigsys_disabled:
.Lrcb_disabled:
    mov dword ptr [r12+16],{paused}
    mov dword ptr [r12+20],1
    add qword ptr [r12+48],1
    add qword ptr [r12+40],1
    lea r13,[rip+.Lrcb_call_handler]
    lea r14,[rip+.Lrcb_serial_error]
    cmp qword ptr [r12+40],0
    cmove r13,r14
    jmp r13
.Lrcb_serial_error:
    mov rdi,-{overflow}
    call {failed}
    ud2
    .global reverie_preload_rcb_sigsys_handler
    .hidden reverie_preload_rcb_sigsys_handler
reverie_preload_rcb_sigsys_handler:
.Lrcb_call_handler:
    mov rdi,[rsp+8]
    mov rsi,[rsp+16]
    mov rdx,[rsp+24]
    call {handler}
    // Rust returns only an already checked action. No Jcc after ENABLE.
    lea r13,[rip+.Lrcb_restore_mask]
    lea r14,[rip+.Lrcb_enable]
    test rax,rax
    cmovg r13,r14
    lea r14,[rip+.Lrcb_action_error]
    cmovs r13,r14
    jmp r13
    .global reverie_preload_rcb_sigsys_action_error
    .hidden reverie_preload_rcb_sigsys_action_error
reverie_preload_rcb_sigsys_action_error:
.Lrcb_action_error:
    mov rdi,rax
    call {failed}
    ud2
    .global reverie_preload_rcb_sigsys_enable
    .hidden reverie_preload_rcb_sigsys_enable
reverie_preload_rcb_sigsys_enable:
.Lrcb_enable:
    lea rsi,[rax-1]
    mov edi,{ioctl}
    mov edx,{enable}
    xor ecx,ecx
    xor r8d,r8d
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lrcb_control_error]
    lea r14,[rip+.Lrcb_enabled]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_preload_rcb_sigsys_enabled
    .hidden reverie_preload_rcb_sigsys_enabled
reverie_preload_rcb_sigsys_enabled:
.Lrcb_enabled:
    mov dword ptr [r12+16],{running}
    add qword ptr [r12+56],1
    .global reverie_preload_rcb_sigsys_published
    .hidden reverie_preload_rcb_sigsys_published
reverie_preload_rcb_sigsys_published:
    .global reverie_preload_rcb_sigsys_restore_mask
    .hidden reverie_preload_rcb_sigsys_restore_mask
reverie_preload_rcb_sigsys_restore_mask:
.Lrcb_restore_mask:
    mov edi,{sigprocmask}
    mov esi,{setmask}
    lea rdx,[rsp+40]
    xor ecx,ecx
    mov r8d,8
    xor r9d,r9d
    call reverie_preload_trusted_syscall
    mov rdi,rax
    lea r13,[rip+.Lrcb_mask_error]
    lea r14,[rip+.Lrcb_mask_restored]
    test rax,rax
    cmovz r13,r14
    jmp r13
    .global reverie_preload_rcb_sigsys_mask_error
    .hidden reverie_preload_rcb_sigsys_mask_error
reverie_preload_rcb_sigsys_mask_error:
.Lrcb_mask_error:
    call {failed}
    ud2
    .global reverie_preload_rcb_sigsys_mask_restored
    .hidden reverie_preload_rcb_sigsys_mask_restored
reverie_preload_rcb_sigsys_mask_restored:
.Lrcb_mask_restored:
    .global reverie_preload_rcb_sigsys_return
    .hidden reverie_preload_rcb_sigsys_return
reverie_preload_rcb_sigsys_return:
.Lrcb_return:
    add rsp,72
    pop r15
    pop r14
    pop r13
    pop r12
    ret
    .global reverie_preload_rcb_sigsys_end
    .hidden reverie_preload_rcb_sigsys_end
reverie_preload_rcb_sigsys_end:
    .size reverie_preload_sigsys_entry,.-reverie_preload_sigsys_entry
"#,
    running = const RUNNING,
    paused = const PAUSED,
    gettid = const libc::SYS_gettid,
    getcpu = const libc::SYS_getcpu,
    ioctl = const libc::SYS_ioctl,
    // Linux perf_event ioctls: _IO('$',0/1). No group flag is passed.
    enable = const 0x2400u32,
    disable = const 0x2401u32,
    sigprocmask = const libc::SYS_rt_sigprocmask,
    block = const libc::SIG_BLOCK,
    setmask = const libc::SIG_SETMASK,
    exit_group = const libc::SYS_exit_group,
    stale = const libc::ESTALE,
    cpu_offset = const CPU_OFFSET,
    overflow = const libc::EOVERFLOW,
    failed = sym failed,
    handler = sym super::sigsys_handler,
);
