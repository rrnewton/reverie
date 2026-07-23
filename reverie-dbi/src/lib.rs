/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! DynamoRIO backend prototype for in-process Reverie tools.
//!
//! The native client performs hot-path instruction rewriting while this crate
//! adapts DynamoRIO events to Reverie's shared [`reverie::Tool`] and
//! [`reverie::Guest`] contracts.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod launcher;
mod tools;

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::c_void;
use std::future::Future;
use std::path::Path;
use std::pin::pin;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::AtomicU16;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

pub use launcher::DbiRunner;
use reverie::Error;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
use reverie::Stack;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::PathPtr;
use reverie::syscalls::ReadAddr;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_memory::LocalMemory;
use reverie_memory::MemoryAccess;
use serde::Deserialize;
use serde::Serialize;

/// Native callback used to issue a syscall with DynamoRIO bookkeeping.
pub type SyscallInvoker = unsafe extern "C" fn(usize, i64, *const u64) -> i64;

/// Native callback used to translate DynamoRIO's machine context.
pub type RegisterReader = unsafe extern "C" fn(usize, *mut libc::user_regs_struct) -> i32;

/// In-process guest state passed to a Reverie tool handler.
pub struct DbiGuest<'a, T>
where
    T: Tool,
{
    context: usize,
    tid: Pid,
    pid: Pid,
    ppid: Option<Pid>,
    branch_count: u64,
    thread_state: &'a mut T::ThreadState,
    global_state: &'a T::GlobalState,
    config: &'a <T::GlobalState as GlobalTool>::Config,
    invoke_syscall: SyscallInvoker,
    read_registers: RegisterReader,
}

impl<'a, T> DbiGuest<'a, T>
where
    T: Tool,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        context: usize,
        tid: Pid,
        pid: Pid,
        ppid: Option<Pid>,
        branch_count: u64,
        thread_state: &'a mut T::ThreadState,
        global_state: &'a T::GlobalState,
        config: &'a <T::GlobalState as GlobalTool>::Config,
        invoke_syscall: SyscallInvoker,
        read_registers: RegisterReader,
    ) -> Self {
        Self {
            context,
            tid,
            pid,
            ppid,
            branch_count,
            thread_state,
            global_state,
            config,
            invoke_syscall,
            read_registers,
        }
    }
}

#[reverie::tool]
impl<T> GlobalRPC<T::GlobalState> for DbiGuest<'_, T>
where
    T: Tool,
{
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        self.global_state.receive_rpc(self.tid, message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.config
    }
}

#[reverie::tool]
impl<T> Guest<T> for DbiGuest<'_, T>
where
    T: Tool,
{
    type Memory = LocalMemory;
    type Stack = DbiStack;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn pid(&self) -> Pid {
        self.pid
    }

    fn ppid(&self) -> Option<Pid> {
        // The DynamoRIO backend currently instruments a single process, whose
        // main thread is the root of the traced tree, so `None` is correct for
        // it (see `is_root_process`). Reporting a real in-tree parent for
        // forked children requires the native client to track the process tree
        // across `clone`/`fork`, which it does not yet surface.
        // TODO-STUB(#31): track the process tree so children report their
        // in-tree parent instead of always `None`.
        self.ppid
    }

    fn memory(&self) -> Self::Memory {
        LocalMemory::new()
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.thread_state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.thread_state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        let mut regs = unsafe { std::mem::zeroed() };
        let read = unsafe { (self.read_registers)(self.context, &mut regs) };
        assert_ne!(read, 0, "DynamoRIO failed to translate the guest registers");
        regs
    }

    async fn stack(&mut self) -> Self::Stack {
        DbiStack::new()
    }

    async fn daemonize(&mut self) {}

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        let (number, args) = syscall.into_parts();
        let args = [
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ];
        let result =
            unsafe { (self.invoke_syscall)(self.context, number.id() as i64, args.as_ptr()) };
        Errno::from_ret(result as usize).map(|value| value as i64)
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        // Perform the syscall in-process now, record its result, then suspend
        // forever. The driver polls each handler exactly once; on `Poll::Pending`
        // it installs the recorded result and drops this future. This mirrors the
        // ptrace backend, which tail-injects by cancelling the handler and
        // awaiting `future::pending()`.
        //
        // We must NOT diverge by panicking: unwinding a panic out of a handler
        // aborts the process under the DynamoRIO client (its callback runs on a
        // C frame the Rust unwinder cannot cross, so `catch_unwind` in the driver
        // does not contain it). Suspend-and-drop avoids unwinding entirely.
        let value = match self.inject(syscall).await {
            Ok(value) => value,
            Err(errno) => -(errno.into_raw() as i64),
        };
        TAIL_INJECT_RESULT.with(|slot| slot.set(Some(value)));
        std::future::pending().await
    }

    fn set_timer(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        // A working timer needs a retired-conditional-branch threshold trap
        // installed in the native DynamoRIO client; the branch counter is
        // currently only sampled at syscall boundaries, never armed.
        // TODO-STUB(#31): arm an RCB threshold in the native client and
        // dispatch `Tool::handle_timer_event`.
        Err(Errno::ENOSYS.into())
    }

    fn set_timer_precise(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        // Precise timers additionally require single-stepping to the exact
        // instruction, which the native client does not implement.
        // TODO-STUB(#31): add RCB + single-step delivery in the native client.
        Err(Errno::ENOSYS.into())
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        // Coarse: this is the retired-conditional-branch count sampled by the
        // native client at the most recent syscall entry, not a continuously
        // updated clock. Adequate for ordering at syscall boundaries only.
        // TODO-STUB(#31): expose a continuously updated RCB read from the
        // native client for sub-syscall resolution.
        Ok(self.branch_count)
    }
}

/// Capacity, in bytes, of the in-process scratch arena handed out by
/// [`DbiGuest::stack`]. The ptrace backend uses a comparably small window on
/// the tracee's real stack; a page is generous for argument marshalling.
const DBI_STACK_CAPACITY: usize = 4096;

/// In-process guest scratch stack for the DynamoRIO backend.
///
/// The ptrace backend's `GuestStack` allocates on the tracee's real stack and
/// defers writes until `commit`, because the tracer lives in a different
/// address space. The DynamoRIO backend instead shares the guest's address
/// space, so allocations are written immediately into a heap-backed arena and
/// their addresses are valid for injected syscalls as soon as they are made.
/// `commit` transfers ownership of the arena to the returned guard, which keeps
/// the memory alive until it is dropped (mirroring how a `StackGuard` bounds the
/// lifetime of the allocations).
pub struct DbiStack {
    arena: Box<[u8]>,
    offset: usize,
}

impl DbiStack {
    fn new() -> Self {
        Self {
            arena: vec![0u8; DBI_STACK_CAPACITY].into_boxed_slice(),
            offset: 0,
        }
    }

    fn allocate<'stack, T>(&mut self, value: T) -> AddrMut<'stack, T> {
        let size = core::mem::size_of::<T>();
        let align = core::mem::align_of::<T>();
        let base = self.arena.as_ptr() as usize;
        // Align the absolute address (not just the offset) so `T` reads/writes
        // are always aligned regardless of the arena's base alignment.
        let start = align_up(base + self.offset, align) - base;
        let end = start + size;
        assert!(
            end <= self.arena.len(),
            "DBI guest stack overflow: need {end} bytes, capacity {}",
            self.arena.len()
        );
        // In-process: write directly. The pointer is stable across the later
        // move of `arena` into the guard, so the returned address stays valid.
        let ptr = unsafe { self.arena.as_mut_ptr().add(start) } as *mut T;
        unsafe { ptr.write(value) };
        self.offset = end;
        AddrMut::from_raw(ptr as usize).expect("DBI guest stack produced a null scratch pointer")
    }
}

/// Guard that keeps a committed [`DbiStack`] arena alive. Dropping it releases
/// the scratch memory, at which point any addresses handed out become invalid.
pub struct DbiStackGuard {
    _arena: Box<[u8]>,
}

impl Drop for DbiStackGuard {
    fn drop(&mut self) {}
}

impl Stack for DbiStack {
    type StackGuard = DbiStackGuard;

    fn size(&self) -> usize {
        self.offset
    }

    fn capacity(&self) -> usize {
        self.arena.len()
    }

    fn push<'stack, T>(&mut self, value: T) -> Addr<'stack, T> {
        self.allocate(value).into()
    }

    fn reserve<'stack, T>(&mut self) -> AddrMut<'stack, T> {
        let value: T = unsafe { core::mem::MaybeUninit::zeroed().assume_init() };
        self.allocate(value)
    }

    fn commit(self) -> Result<Self::StackGuard, Errno> {
        Ok(DbiStackGuard { _arena: self.arena })
    }
}

/// Rounds `value` up to the next multiple of `align` (a power of two).
fn align_up(value: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

thread_local! {
    /// Result recorded by [`Guest::tail_inject`] on this thread before it
    /// suspends. The driver takes it when a handler poll returns `Poll::Pending`
    /// and installs it as the syscall's result. See [`Guest::tail_inject`].
    static TAIL_INJECT_RESULT: std::cell::Cell<Option<i64>> =
        const { std::cell::Cell::new(None) };
}

/// Per-thread state used by the prototype tool.
#[repr(C)]
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PrototypeCounters {
    /// Branches observed by inline DynamoRIO instrumentation.
    pub branches: u64,
    /// Syscall entry events observed by DynamoRIO.
    pub observed_syscalls: u64,
    /// Syscalls executed through [`Guest::inject`] and suppressed at entry.
    pub rewritten_syscalls: u64,
}

/// Tool used by the standalone prototype client.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrototypeTool;

#[reverie::tool]
impl Tool for PrototypeTool {
    type GlobalState = ();
    type ThreadState = PrototypeCounters;

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        guest.thread_state_mut().rewritten_syscalls += 1;
        match syscall {
            Syscall::Uname(call) => {
                let result = guest.inject(call).await?;
                if let Some(buffer) = call.buf() {
                    let mut value = guest.memory().read_value(buffer)?;
                    set_c_string(&mut value.release, b"5.2.0");
                    guest.memory().write_value(buffer, &value)?;
                }
                Ok(result)
            }
            Syscall::Bind(call) => {
                rewrite_bind_port(guest, call)?;
                Ok(guest.inject(call).await?)
            }
            Syscall::Open(call) => handle_open(guest, call.path(), call).await,
            Syscall::Openat(call) => handle_open(guest, call.path(), call).await,
            Syscall::Read(call) if is_random_fd(call.fd()) => {
                deterministic_random_read(guest, call)
            }
            Syscall::Close(call) => {
                let result = guest.inject(call).await?;
                if result == 0 {
                    random_fds().remove(&call.fd());
                }
                Ok(result)
            }
            Syscall::Getrandom(call) => deterministic_getrandom(guest, call),
            Syscall::Getrusage(call) => {
                let result = guest.inject(call).await?;
                if result == 0
                    && let Some(usage) = call.usage()
                {
                    guest
                        .memory()
                        .write_value(usage, &unsafe { std::mem::zeroed::<libc::rusage>() })?;
                }
                Ok(result)
            }
            Syscall::Sysinfo(call) => deterministic_sysinfo(guest, call),
            syscall => Ok(guest.inject(syscall).await?),
        }
    }
}

const RNG_SEED_ENV: &str = "HERMIT_DBI_RNG_SEED";

static RANDOM_FDS: LazyLock<Mutex<HashSet<i32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static RANDOM_INVOCATIONS: LazyLock<Mutex<HashMap<usize, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn random_fds() -> std::sync::MutexGuard<'static, HashSet<i32>> {
    RANDOM_FDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn is_random_fd(fd: i32) -> bool {
    random_fds().contains(&fd)
}

fn deterministic_proc_content(path: &Path) -> Option<Vec<u8>> {
    match path.to_str()? {
        "/proc/cpuinfo" => Some(b"processor\t: 0\ncpu MHz\t\t: 0.000\n".to_vec()),
        "/proc/self/maps" => {
            Some(b"00400000-00401000 r--p 00000000 00:00 0 [reverie-dbi]\n".to_vec())
        }
        "/proc/self/stat" => {
            let mut contents = String::from("1 (hermit-dbi) R");
            for _ in 0..49 {
                contents.push_str(" 0");
            }
            contents.push('\n');
            Some(contents.into_bytes())
        }
        "/proc/self/status" => Some(
            b"Name:\thermit-dbi\nPid:\t1\nvoluntary_ctxt_switches:\t0\nnonvoluntary_ctxt_switches:\t0\n"
                .to_vec(),
        ),
        _ => None,
    }
}

fn create_memfd(contents: &[u8]) -> Result<i64, Error> {
    let name = b"reverie-dbi-proc\0";
    let fd = Errno::result(unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            name.as_ptr() as *const libc::c_char,
            libc::MFD_CLOEXEC,
        )
    })? as i32;
    let written = Errno::result(unsafe {
        libc::write(fd, contents.as_ptr() as *const c_void, contents.len())
    });
    if written != Ok(contents.len() as isize)
        || Errno::result(unsafe { libc::lseek(fd, 0, libc::SEEK_SET) }).is_err()
    {
        unsafe { libc::close(fd) };
        return Err(Errno::EIO.into());
    }
    Ok(fd as i64)
}

async fn handle_open<'a, G, S>(
    guest: &mut G,
    path: Option<PathPtr<'a>>,
    call: S,
) -> Result<i64, Error>
where
    G: Guest<PrototypeTool>,
    S: SyscallInfo,
{
    let Some(path) = path else {
        return Ok(guest.inject(call).await?);
    };
    let path = path.read(&guest.memory())?;
    if let Some(contents) = deterministic_proc_content(&path) {
        return create_memfd(&contents);
    }

    let result = guest.inject(call).await?;
    if result >= 0 && matches!(path.to_str(), Some("/dev/random" | "/dev/urandom")) {
        random_fds().insert(result as i32);
    }
    Ok(result)
}

fn configured_rng_seed() -> u64 {
    std::env::var(RNG_SEED_ENV)
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or(0)
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn deterministic_random_bytes(seed: u64, address: usize, invocation: u64, bytes: &mut [u8]) {
    for (index, chunk) in bytes.chunks_mut(8).enumerate() {
        let value = splitmix64(
            seed ^ (address as u64).rotate_left(17) ^ invocation.rotate_left(39) ^ index as u64,
        );
        chunk.copy_from_slice(&value.to_ne_bytes()[..chunk.len()]);
    }
}

fn next_random_invocation(address: usize) -> u64 {
    let mut invocations = RANDOM_INVOCATIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let invocation = invocations.entry(address).or_default();
    let current = *invocation;
    *invocation = invocation.wrapping_add(1);
    current
}

fn write_deterministic_random<G: Guest<PrototypeTool>>(
    guest: &mut G,
    buffer: Option<AddrMut<'_, u8>>,
    length: usize,
) -> Result<i64, Error> {
    if length == 0 {
        return Ok(0);
    }
    let buffer = buffer.ok_or(Errno::EFAULT)?;
    let mut bytes = vec![0; length];
    deterministic_random_bytes(
        configured_rng_seed(),
        buffer.as_raw(),
        next_random_invocation(buffer.as_raw()),
        &mut bytes,
    );
    guest.memory().write_exact(buffer, &bytes)?;
    Ok(length as i64)
}

fn deterministic_random_read<G: Guest<PrototypeTool>>(
    guest: &mut G,
    call: reverie::syscalls::Read,
) -> Result<i64, Error> {
    write_deterministic_random(guest, call.buf(), call.len())
}

fn deterministic_getrandom<G: Guest<PrototypeTool>>(
    guest: &mut G,
    call: reverie::syscalls::Getrandom,
) -> Result<i64, Error> {
    if call.flags() & !((libc::GRND_NONBLOCK | libc::GRND_RANDOM) as usize) != 0 {
        return Err(Errno::EINVAL.into());
    }
    write_deterministic_random(guest, call.buf(), call.buflen())
}

fn deterministic_sysinfo<G: Guest<PrototypeTool>>(
    guest: &mut G,
    call: reverie::syscalls::Sysinfo,
) -> Result<i64, Error> {
    let destination = call.info().ok_or(Errno::EFAULT)?;
    let mut info = unsafe { std::mem::zeroed::<libc::sysinfo>() };
    info.uptime = 1;
    info.loads = [0; 3];
    info.totalram = 1024 * 1024 * 1024;
    info.freeram = 512 * 1024 * 1024;
    info.procs = 1;
    info.mem_unit = 1;
    guest.memory().write_value(destination, &info)?;
    Ok(0)
}

fn should_rewrite_syscall(sysnum: i64) -> bool {
    [
        libc::SYS_write,
        libc::SYS_uname,
        libc::SYS_bind,
        libc::SYS_open,
        libc::SYS_openat,
        libc::SYS_read,
        libc::SYS_close,
        libc::SYS_getrandom,
        libc::SYS_getrusage,
        libc::SYS_sysinfo,
    ]
    .contains(&sysnum)
}

fn set_c_string(destination: &mut [libc::c_char], value: &[u8]) {
    destination.fill(0);
    for (destination, source) in destination.iter_mut().zip(value) {
        *destination = *source as libc::c_char;
    }
}

static NEXT_PORT: AtomicU16 = AtomicU16::new(32768);

fn deterministic_port(next: &AtomicU16, requested: u16) -> u16 {
    if requested == 0 {
        next.fetch_add(1, Ordering::SeqCst)
    } else {
        next.fetch_max(requested.saturating_add(1), Ordering::SeqCst);
        requested
    }
}

fn rewrite_bind_port<G: Guest<PrototypeTool>>(
    guest: &mut G,
    call: reverie::syscalls::Bind,
) -> Result<(), Error> {
    let Some(address) = call.umyaddr() else {
        return Ok(());
    };
    let family = guest.memory().read_value(address.cast::<u16>())?;
    match family as i32 {
        libc::AF_INET => {
            let address = address.cast::<libc::sockaddr_in>();
            let mut value = guest.memory().read_value(address)?;
            let port = deterministic_port(&NEXT_PORT, u16::from_be(value.sin_port));
            value.sin_port = port.to_be();
            guest.memory().write_value(address, &value)?;
        }
        libc::AF_INET6 => {
            let address = address.cast::<libc::sockaddr_in6>();
            let mut value = guest.memory().read_value(address)?;
            let port = deterministic_port(&NEXT_PORT, u16::from_be(value.sin6_port));
            value.sin6_port = port.to_be();
            guest.memory().write_value(address, &value)?;
        }
        _ => {}
    }
    Ok(())
}

/// Polls a handler future exactly once. Returns `Some` if it resolved
/// immediately (the common case: `inject`/`memory`/`regs` are synchronous), or
/// `None` if it suspended. The only expected suspension is [`Guest::tail_inject`],
/// which records its result in [`TAIL_INJECT_RESULT`] before suspending; the
/// driver handles `None` by installing that result and dropping the future. We
/// return `None` rather than panicking because a panic aborts under the
/// DynamoRIO client (see [`Guest::tail_inject`]).
fn run_ready<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

static PROTOTYPE_TOOL: PrototypeTool = PrototypeTool;
static GLOBAL_STATE: () = ();
static CONFIG: () = ();
static TOTAL_BRANCHES: AtomicU64 = AtomicU64::new(0);
static TOTAL_SYSCALLS: AtomicU64 = AtomicU64::new(0);
static TOTAL_REWRITTEN: AtomicU64 = AtomicU64::new(0);

/// Initializes the prototype state for the current application thread.
///
/// # Safety
///
/// `counters` must point to aligned, writable storage for one counter value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn reverie_dbi_runtime_thread_init(counters: *mut PrototypeCounters) {
    unsafe { counters.write(PrototypeCounters::default()) };
}

/// Handles a DynamoRIO pre-syscall event.
///
/// Returning one asks the native client to suppress the original syscall and
/// install `result`; returning zero leaves the syscall unchanged.
///
/// # Safety
///
/// The context and callback pointers must remain valid for the call. `counters`
/// and `result` must be writable, and `args` must address six syscall arguments.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn reverie_dbi_runtime_pre_syscall(
    context: *mut c_void,
    counters: *mut PrototypeCounters,
    tid: i32,
    pid: i32,
    sysnum: i64,
    args: *const u64,
    branches: u64,
    result: *mut i64,
    invoke_syscall: SyscallInvoker,
    read_registers: RegisterReader,
    emit: tools::Emitter,
) -> i32 {
    // NB: `catch_unwind` cannot actually contain a panic here — unwinding out of
    // a handler aborts under the DynamoRIO client (see `Guest::tail_inject`). It
    // is retained only so the same code path is exercised off-DR (unit tests).
    // Handlers therefore must not panic; divergence (tail_inject) suspends
    // instead, which this driver handles explicitly below.
    let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let counters = unsafe { &mut *counters };
        counters.branches = branches;
        counters.observed_syscalls += 1;
        TOTAL_BRANCHES.store(branches, Ordering::Relaxed);
        TOTAL_SYSCALLS.fetch_add(1, Ordering::Relaxed);
        tools::set_emitter(emit);

        let raw_args = unsafe { std::slice::from_raw_parts(args, 6) };

        // If an env-selected observation tool (syscall counter / strace) is
        // active, it handles every syscall via the standard `Tool` trait and
        // supersedes the built-in determinism policy.
        if let Some(value) = tools::run_active_tool(
            context as usize,
            tid,
            pid,
            sysnum,
            raw_args,
            branches,
            invoke_syscall,
            read_registers,
        ) {
            unsafe { result.write(value) };
            TOTAL_REWRITTEN.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        if !should_rewrite_syscall(sysnum) {
            return false;
        }

        let syscall = Syscall::from_raw(
            Sysno::from(sysnum as i32),
            SyscallArgs::new(
                raw_args[0] as usize,
                raw_args[1] as usize,
                raw_args[2] as usize,
                raw_args[3] as usize,
                raw_args[4] as usize,
                raw_args[5] as usize,
            ),
        );
        let mut guest = DbiGuest::new(
            context as usize,
            Pid::from_raw(tid),
            Pid::from_raw(pid),
            None,
            branches,
            counters,
            &GLOBAL_STATE,
            &CONFIG,
            invoke_syscall,
            read_registers,
        );
        // Clear any stale tail-inject result before polling; `tail_inject`
        // records a fresh one just before it suspends.
        TAIL_INJECT_RESULT.with(|slot| slot.set(None));
        let value = match run_ready(PROTOTYPE_TOOL.handle_syscall_event(&mut guest, syscall)) {
            Some(Ok(value)) => value,
            Some(Err(Error::Errno(errno))) => -(errno.into_raw() as i64),
            Some(Err(_)) => -(Errno::EIO.into_raw() as i64),
            None => match TAIL_INJECT_RESULT.with(|slot| slot.take()) {
                // The handler suspended after `tail_inject`: install its result
                // and let the suspended future drop when this scope ends.
                Some(value) => value,
                // Unexpected suspension (a tool awaited something other than
                // `tail_inject`). The single-poll driver cannot resume it, so
                // leave the syscall unchanged rather than installing a bogus
                // result.
                None => return false,
            },
        };
        unsafe { result.write(value) };
        TOTAL_REWRITTEN.fetch_add(1, Ordering::Relaxed);
        true
    }));

    match handled {
        Ok(true) => 1,
        // `catch_unwind` cannot actually catch under DR; a genuine panic aborts
        // before reaching here. `Ok(false)` means "leave the syscall unchanged".
        Ok(false) | Err(_) => 0,
    }
}

/// Returns process-wide prototype counters accumulated at syscall boundaries.
///
/// # Safety
///
/// Each output pointer must be aligned and writable for one `u64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn reverie_dbi_runtime_totals(
    branches: *mut u64,
    syscalls: *mut u64,
    rewritten: *mut u64,
) {
    unsafe {
        branches.write(TOTAL_BRANCHES.load(Ordering::Relaxed));
        syscalls.write(TOTAL_SYSCALLS.load(Ordering::Relaxed));
        rewritten.write(TOTAL_REWRITTEN.load(Ordering::Relaxed));
    }
    // NB: the syscall-counter tool prints its histogram from the guest's own
    // exit_group syscall (see `tools`), not here — this callback runs on the
    // DynamoRIO client's tiny stack, which overflows while formatting.
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn invoke(_context: usize, sysnum: i64, args: *const u64) -> i64 {
        assert_eq!(sysnum, libc::SYS_write);
        unsafe { *args.add(2) as i64 }
    }

    unsafe extern "C" fn invoke_uname(_context: usize, sysnum: i64, args: *const u64) -> i64 {
        assert_eq!(sysnum, libc::SYS_uname);
        unsafe { libc::uname(*args as *mut libc::utsname) as i64 }
    }

    unsafe extern "C" fn read_regs(_context: usize, regs: *mut libc::user_regs_struct) -> i32 {
        unsafe { (*regs).rip = 0x1234 };
        1
    }

    #[test]
    fn prototype_tool_uses_shared_guest_contract() {
        let mut counters = PrototypeCounters::default();
        let syscall = Syscall::from_raw(Sysno::write, SyscallArgs::new(1, 0x1000, 7, 0, 0, 0));
        let mut guest = DbiGuest::new(
            0,
            Pid::from_raw(10),
            Pid::from_raw(10),
            None,
            99,
            &mut counters,
            &GLOBAL_STATE,
            &CONFIG,
            invoke,
            read_regs,
        );

        assert_eq!(
            run_ready(PROTOTYPE_TOOL.handle_syscall_event(&mut guest, syscall))
                .unwrap()
                .unwrap(),
            7
        );
        assert_eq!(guest.thread_state().rewritten_syscalls, 1);
        assert_eq!(guest.read_clock().unwrap(), 99);
        assert_eq!(run_ready(guest.regs()).unwrap().rip, 0x1234);
    }

    #[test]
    fn prototype_tool_virtualizes_uname_release() {
        let mut counters = PrototypeCounters::default();
        let mut utsname = unsafe { std::mem::zeroed::<libc::utsname>() };
        let syscall = Syscall::from_raw(
            Sysno::uname,
            SyscallArgs::new((&mut utsname as *mut libc::utsname) as usize, 0, 0, 0, 0, 0),
        );
        let mut guest = DbiGuest::new(
            0,
            Pid::from_raw(10),
            Pid::from_raw(10),
            None,
            99,
            &mut counters,
            &GLOBAL_STATE,
            &CONFIG,
            invoke_uname,
            read_regs,
        );

        assert_eq!(
            run_ready(PROTOTYPE_TOOL.handle_syscall_event(&mut guest, syscall))
                .unwrap()
                .unwrap(),
            0
        );
        let release = unsafe { std::ffi::CStr::from_ptr(utsname.release.as_ptr()) };
        assert_eq!(release.to_bytes(), b"5.2.0");
    }

    #[test]
    fn procfs_snapshots_cover_volatile_files() {
        for path in [
            "/proc/cpuinfo",
            "/proc/self/maps",
            "/proc/self/stat",
            "/proc/self/status",
        ] {
            assert!(
                !deterministic_proc_content(Path::new(path))
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(deterministic_proc_content(Path::new("/proc/self/cmdline")).is_none());
    }

    #[test]
    fn deterministic_random_streams_are_reproducible_and_distinct() {
        let mut expected = [0; 24];
        deterministic_random_bytes(17, 0x1000, 0, &mut expected);

        let mut repeated = [0; 24];
        deterministic_random_bytes(17, 0x1000, 0, &mut repeated);
        assert_eq!(repeated, expected);

        for (seed, address, invocation) in [(18, 0x1000, 0), (17, 0x2000, 0), (17, 0x1000, 1)] {
            let mut changed = [0; 24];
            deterministic_random_bytes(seed, address, invocation, &mut changed);
            assert_ne!(changed, expected);
        }
    }

    #[test]
    fn rewrite_filter_covers_deterministic_policies() {
        for syscall in [
            libc::SYS_open,
            libc::SYS_openat,
            libc::SYS_read,
            libc::SYS_close,
            libc::SYS_getrandom,
            libc::SYS_getrusage,
            libc::SYS_sysinfo,
        ] {
            assert!(should_rewrite_syscall(syscall));
        }
        assert!(!should_rewrite_syscall(libc::SYS_prlimit64));
    }

    #[test]
    fn dbi_stack_allocations_are_valid_in_process_and_aligned() {
        let mut stack = DbiStack::new();
        assert_eq!(stack.size(), 0);
        assert_eq!(stack.capacity(), DBI_STACK_CAPACITY);

        // A one-byte allocation followed by a u64 must not misalign the u64.
        let flag = stack.push(0x99u8);
        let word = stack.push(0x1122_3344_5566_7788u64);
        assert_eq!(word.as_raw() % core::mem::align_of::<u64>(), 0);

        // `reserve` hands out zeroed, writable scratch.
        let slot = stack.reserve::<u32>();
        assert_eq!(unsafe { (slot.as_raw() as *const u32).read() }, 0);
        unsafe { (slot.as_raw() as *mut u32).write(0xdead_beef) };

        assert!(stack.size() >= 1 + 8 + 4);

        // Addresses stay valid after `commit` moves the arena into the guard.
        let guard = stack.commit().unwrap();
        unsafe {
            assert_eq!((flag.as_raw() as *const u8).read(), 0x99);
            assert_eq!((word.as_raw() as *const u64).read(), 0x1122_3344_5566_7788);
            assert_eq!((slot.as_raw() as *const u32).read(), 0xdead_beef);
        }
        drop(guard);
    }

    #[test]
    fn tail_inject_records_result_and_suspends() {
        TAIL_INJECT_RESULT.with(|slot| slot.set(None));

        let mut counters = PrototypeCounters::default();
        let syscall = Syscall::from_raw(Sysno::write, SyscallArgs::new(1, 0x1000, 7, 0, 0, 0));
        let mut guest: DbiGuest<'_, PrototypeTool> = DbiGuest::new(
            0,
            Pid::from_raw(10),
            Pid::from_raw(10),
            None,
            0,
            &mut counters,
            &GLOBAL_STATE,
            &CONFIG,
            invoke,
            read_regs,
        );

        // `tail_inject` performs the syscall, records its result, then suspends
        // forever, so a single poll returns `Poll::Pending` (`None` here).
        let polled = run_ready(guest.tail_inject(syscall));
        assert!(polled.is_none(), "tail_inject must suspend, not resolve");
        // The injected `write` returns its length argument (see `invoke`).
        assert_eq!(TAIL_INJECT_RESULT.with(|slot| slot.take()), Some(7));
    }

    #[test]
    fn align_up_rounds_to_power_of_two() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(13, 1), 13);
    }

    #[test]
    fn deterministic_ports_advance_past_explicit_bindings() {
        let next = AtomicU16::new(32768);

        assert_eq!(deterministic_port(&next, 0), 32768);
        assert_eq!(deterministic_port(&next, 32769), 32769);
        assert_eq!(deterministic_port(&next, 0), 32770);
        assert_eq!(deterministic_port(&next, 1200), 1200);
        assert_eq!(deterministic_port(&next, 0), 32771);
    }
}
