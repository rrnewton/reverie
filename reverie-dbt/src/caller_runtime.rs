/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * Licensed under the BSD-style license in the repository LICENSE file. */

//! Caller-owned ABI facade over the common in-guest Tool owner.
//!
//! Only the initial root image is admitted. Native callbacks supply physical
//! interception facts; Tool state, startup, restart and retirement belong to
//! `reverie::tool_host`. Helper-ready is not Tool installation.

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Waker;

use reverie::Error;
use reverie::GlobalRPC;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie::tool_host::*;

use crate::sync_rpc::caller::CallContext;
use crate::sync_rpc::caller::PrimaryBinding;
use crate::sync_rpc::caller::PrimaryPort;
use crate::*;

struct NativeDriver(Waker);
impl NativeDriver {
    fn new() -> Self {
        Self(Waker::from(Arc::new(ThreadUnparkWaker(
            std::thread::current(),
        ))))
    }
}
impl PendingDriver for NativeDriver {
    fn waker(&self) -> &Waker {
        &self.0
    }
    fn on_pending(&self) {
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }
}

#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct Callback {
    pub context: usize,
    pub identity: TaskIdentity,
    pub branches: u64,
    pub invoke: SyscallInvoker,
    pub read_registers: RegisterReader,
    pub write_registers: RegisterWriter,
    pub read_memory: Option<MemoryReader>,
}
#[doc(hidden)]
impl Callback {
    pub fn new(
        context: usize,
        identity: TaskIdentity,
        branches: u64,
        invoke: SyscallInvoker,
        read_registers: RegisterReader,
        write_registers: RegisterWriter,
        read_memory: Option<MemoryReader>,
    ) -> Self {
        Self {
            context,
            identity,
            branches,
            invoke,
            read_registers,
            write_registers,
            read_memory,
        }
    }

    fn port(self) -> CallContext {
        CallContext {
            context: self.context,
            invoke: self.invoke,
            identity: self.identity,
        }
    }
}

struct Factory<T: Tool> {
    callback: Callback,
    tool: std::marker::PhantomData<T>,
}
impl<T: Tool> Factory<T> {
    fn new(callback: Callback) -> Self {
        Self {
            callback,
            tool: std::marker::PhantomData,
        }
    }
}
impl<'port, T: Tool> TaskGuestFactory<T, PrimaryPort<'port, T::GlobalState>> for Factory<T> {
    type Guest<'lease>
        = DbtGuest<'lease, T, PrimaryPort<'port, T::GlobalState>>
    where
        Self: 'lease,
        PrimaryPort<'port, T::GlobalState>: 'lease;
    type Ending = std::convert::Infallible;
    fn guest<'lease>(
        &'lease mut self,
        parts: GuestParts<'lease, T, PrimaryPort<'port, T::GlobalState>>,
        tail: &'lease TailResult,
    ) -> Self::Guest<'lease> {
        DbtGuest {
            context: self.callback.context,
            tid: parts.identity.tid,
            pid: parts.identity.pid,
            ppid: None,
            branch_count: self.callback.branches,
            thread_state: parts.state,
            rpc: *parts.rpc,
            config: parts.rpc.config(),
            invoke_syscall: self.callback.invoke,
            read_registers: self.callback.read_registers,
            write_registers: self.callback.write_registers,
            read_memory: self.callback.read_memory,
            runtime_private_memory: RUNTIME_PRIVATE_MEMORY.get().copied(),
            tail_inject_result: Arc::new(TailInjectResult::default()),
            common_tail: Some(tail),
            signal_info: None,
            signal_info_replacement: None,
        }
    }
    fn after_callback<'lease>(_tool: &T, _guest: &mut Self::Guest<'lease>) -> Option<Self::Ending>
    where
        Self: 'lease,
        PrimaryPort<'port, T::GlobalState>: 'lease,
    {
        None
    }
}

/// Initial-root ABI adapter. Its only Tool/task owner is the shared core owner.
#[doc(hidden)]
pub struct Runtime<T: Tool + 'static> {
    owner: OnceLock<ToolOwner<T, PrimaryBinding<T::GlobalState>>>,
    emit: OnceLock<RuntimeEmitter>,
    generation: AtomicU64,
    ready: AtomicU64,
    retired: AtomicBool,
    branches: AtomicU64,
    syscalls: AtomicU64,
    rewritten: AtomicU64,
}
impl<T: Tool + 'static> Default for Runtime<T> {
    fn default() -> Self {
        Self::new()
    }
}
#[doc(hidden)]
impl<T: Tool + 'static> Runtime<T> {
    pub const fn new() -> Self {
        Self {
            owner: OnceLock::new(),
            emit: OnceLock::new(),
            generation: AtomicU64::new(0),
            ready: AtomicU64::new(0),
            retired: AtomicBool::new(false),
            branches: AtomicU64::new(0),
            syscalls: AtomicU64::new(0),
            rewritten: AtomicU64::new(0),
        }
    }
    pub fn image_init(&self) -> u64 {
        self.ready.store(0, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }
    /// # Safety
    /// `pointer` must reference the ABI-15 callback table for this loaded client;
    /// every callback in it must remain valid for the lifetime of this runtime.
    pub unsafe fn background_init(&self, pointer: *const DbtRuntimeCallbacks, mode: u32) -> i32 {
        if pointer.is_null() {
            return -1;
        }
        let callbacks = unsafe { &*pointer };
        let _ = self.emit.set(callbacks.emit);
        if !matches!(
            DbtStartupMode::try_from(mode),
            Ok(DbtStartupMode::Coordinated)
        ) {
            return self.failure("caller Tool requires coordinated native startup");
        }
        install_guest_compare_exchange(callbacks.compare_exchange_u32);
        install_controller_capture_control(callbacks.controller_capture_control);
        install_runtime_private_memory(callbacks.runtime_private_memory);
        install_branch_fact_query(callbacks.branch_fact);
        install_mapping_capture(callbacks.mapping_capture);
        self.ready
            .store(self.generation.load(Ordering::Acquire), Ordering::Release);
        0
    }
    pub fn ready(&self, generation: u64) -> i32 {
        i32::from(generation != 0 && self.ready.load(Ordering::Acquire) == generation)
    }
    pub fn failure(&self, reason: impl std::fmt::Display) -> i32 {
        if let Some(emit) = self.emit.get() {
            let text = format!("reverie-dbt: caller Tool refused: {reason}\n");
            unsafe { emit(text.as_ptr(), text.len()) };
        }
        -1
    }
    /// # Safety
    /// The physical context and callbacks must belong to the current native
    /// application callback and remain valid until this call returns. No
    /// context from another task, helper, or completed callback is permitted.
    pub unsafe fn thread_init(&self, callback: Callback, defer: i32, parent: i32) -> i32 {
        if defer != 0 {
            return 0;
        }
        if parent > 0 || callback.identity.tid != callback.identity.pid {
            return self.failure("initial-root Tool domain refuses another task");
        }
        let result = (|| -> Result<(), Error> {
            if self.owner.get().is_some() {
                return Err(io::Error::other("caller Tool root was admitted twice").into());
            }
            let ports = PrimaryBinding::<T::GlobalState>::connect(callback.port())?;
            let tool = T::new(callback.identity.pid, ports.config());
            let owner = ToolOwner::new(
                tool,
                ports,
                callback.identity,
                AdmissionDomain::InitialRootOnly,
            );
            self.owner
                .set(owner)
                .map_err(|_| io::Error::other("caller Tool owner was installed twice"))?;
            crate::pidfd_self::initialize();
            let owner = self.owner.get().expect("published owner");
            let mut lease = owner.try_borrow(callback.port())?;
            lease.admit(callback.identity)?;
            let tail = TailResult::default();
            let mut factory = Factory::new(callback);
            lease
                .startup(&mut factory, &tail, &NativeDriver::new())
                .map_err(startup_error)?;
            PrimaryPort::declare(lease.finish_initial_root()?)?;
            acknowledge_tool_admission(callback.context);
            Ok(())
        })();
        match result {
            Ok(()) => 0,
            Err(error) => self.failure(error),
        }
    }
    /// # Safety
    /// The physical context and callbacks must belong to the current native
    /// application callback and remain valid until this call returns. No
    /// context from another task, helper, or completed callback is permitted.
    pub unsafe fn syscall(
        &self,
        callback: Callback,
        syscall: Syscall,
    ) -> Result<NativeSyscallOutcome, Error> {
        let owner = self
            .owner
            .get()
            .ok_or_else(|| io::Error::other("caller Tool owner is not installed"))?;
        let (number, args) = syscall_parts(syscall);
        if let PhysicalOperation::Refused { reason, .. } =
            physical_operation(AdmissionDomain::InitialRootOnly, number, args, None)
        {
            return Err(io::Error::other(reason).into());
        }
        self.branches.store(callback.branches, Ordering::Relaxed);
        self.syscalls.fetch_add(1, Ordering::Relaxed);
        let mut lease = owner.try_borrow(callback.port())?;
        lease.admit(callback.identity)?;
        let tail = TailResult::default();
        let mut factory = Factory::new(callback);
        let outcome = if owner
            .subscriptions()
            .iter_syscalls()
            .any(|candidate| candidate == syscall.number())
        {
            let (tool, mut guest) = lease.guest(&mut factory, &tail)?;
            drive_tool_syscall_with(tool, &mut guest, syscall, &tail, &NativeDriver::new())
        } else {
            NativeSyscallOutcome::ExecuteOriginal(syscall)
        };
        let terminal = match &outcome {
            NativeSyscallOutcome::ExecuteOriginal(syscall) => {
                let (number, args) = syscall_parts(*syscall);
                matches!(number, libc::SYS_exit | libc::SYS_exit_group)
                    .then_some(TerminalEvent::Syscall { number, args })
            }
            NativeSyscallOutcome::Complete(DrivenSyscall::Exit { number, args }) => {
                Some(TerminalEvent::Syscall {
                    number: *number,
                    args: *args,
                })
            }
            _ => None,
        };
        if let Some(terminal) = terminal {
            lease.finish_exit(terminal, &NativeDriver::new())?;
            self.retired.store(true, Ordering::Release);
        }
        Ok(outcome)
    }
    /// # Safety
    /// The physical context and callbacks must belong to the current native
    /// application callback and remain valid until this call returns. No
    /// context from another task, helper, or completed callback is permitted.
    pub unsafe fn rdtsc(
        &self,
        callback: Callback,
        request: reverie::Rdtsc,
    ) -> Result<reverie::RdtscResult, Error> {
        let owner = self
            .owner
            .get()
            .ok_or_else(|| io::Error::other("caller Tool owner is not installed"))?;
        if !owner.subscriptions().has_rdtsc() {
            return Ok(reverie::RdtscResult::new(request));
        }
        let mut lease = owner.try_borrow(callback.port())?;
        lease.admit(callback.identity)?;
        let tail = TailResult::default();
        let mut factory = Factory::new(callback);
        let (tool, mut guest) = lease.guest(&mut factory, &tail)?;
        callback_result(drive_callback_with(
            tool.handle_rdtsc_event(&mut guest, request),
            &tail,
            &NativeDriver::new(),
        ))
    }
    /// # Safety
    /// The physical context and callbacks must belong to the current native
    /// application callback and remain valid until this call returns. No
    /// context from another task, helper, or completed callback is permitted.
    pub unsafe fn cpuid(
        &self,
        callback: Callback,
        leaf: u32,
        subleaf: u32,
    ) -> Result<[u32; 4], Error> {
        let owner = self
            .owner
            .get()
            .ok_or_else(|| io::Error::other("caller Tool owner is not installed"))?;
        if !owner.subscriptions().has_cpuid() {
            let value = std::arch::x86_64::__cpuid_count(leaf, subleaf);
            return Ok([value.eax, value.ebx, value.ecx, value.edx]);
        }
        let mut lease = owner.try_borrow(callback.port())?;
        lease.admit(callback.identity)?;
        let tail = TailResult::default();
        let mut factory = Factory::new(callback);
        let (tool, mut guest) = lease.guest(&mut factory, &tail)?;
        let value = callback_result(drive_callback_with(
            tool.handle_cpuid_event(&mut guest, leaf, subleaf),
            &tail,
            &NativeDriver::new(),
        ))?;
        Ok([value.eax, value.ebx, value.ecx, value.edx])
    }
    pub fn thread_exit(&self) {
        // ABI-15 has no fatal return from this notification. Keep the evidence
        // explicit; the launcher owns non-exit root status failure and reaping.
        if !self.retired.load(Ordering::Acquire) {
            self.failure("root not retired before native thread-exit notification");
        }
    }
    pub fn totals(&self) -> [u64; 4] {
        [
            self.branches.load(Ordering::Relaxed),
            self.syscalls.load(Ordering::Relaxed),
            self.rewritten.load(Ordering::Relaxed),
            0,
        ]
    }
    /// # Safety
    /// The native callback context must be current; argument/result pointers
    /// must address the ABI's six readable arguments and writable outputs.
    pub unsafe fn pre_syscall(
        &self,
        callback: Callback,
        number: i64,
        args: *const u64,
        result: *mut i64,
        deferred_number: *mut i64,
        deferred_args: *mut u64,
    ) -> i32 {
        let args: [u64; 6] = unsafe { std::slice::from_raw_parts(args, 6) }
            .try_into()
            .expect("six ABI arguments");
        let Some(number) = Sysno::new(number as usize) else {
            return self.failure("unrecognized physical syscall number");
        };
        let args = args.map(|value| value as usize);
        let syscall = Syscall::from_raw(
            number,
            SyscallArgs::new(args[0], args[1], args[2], args[3], args[4], args[5]),
        );
        match unsafe { self.syscall(callback, syscall) } {
            Ok(NativeSyscallOutcome::Complete(DrivenSyscall::Result(value))) => {
                unsafe { result.write(value) };
                self.rewritten.fetch_add(1, Ordering::Relaxed);
                1
            }
            Ok(NativeSyscallOutcome::ExecuteOriginal(syscall)) => unsafe {
                defer(syscall, deferred_number, deferred_args)
            },
            Ok(NativeSyscallOutcome::Complete(DrivenSyscall::Exit { number, args })) => {
                unsafe {
                    deferred_number.write(number);
                    std::ptr::copy_nonoverlapping(args.as_ptr(), deferred_args, 6);
                }
                2
            }
            Ok(NativeSyscallOutcome::Complete(DrivenSyscall::Fatal(error))) | Err(error) => {
                self.failure(error)
            }
            Ok(NativeSyscallOutcome::Complete(DrivenSyscall::ForkChild { .. })) => {
                self.failure("initial-root Tool received an impossible fork child")
            }
        }
    }
}
fn syscall_parts(syscall: Syscall) -> (i64, [u64; 6]) {
    let (number, args) = syscall.into_parts();
    (
        number.id() as i64,
        [
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ],
    )
}
unsafe fn defer(syscall: Syscall, number: *mut i64, args: *mut u64) -> i32 {
    let (value, values) = syscall_parts(syscall);
    unsafe {
        number.write(value);
        std::ptr::copy_nonoverlapping(values.as_ptr(), args, 6);
    }
    2
}
fn startup_error(error: StartupFailure<std::convert::Infallible>) -> Error {
    match error {
        StartupFailure::Tool(error) => error,
        StartupFailure::Unsupported(reason) => io::Error::other(reason).into(),
        StartupFailure::Exit { .. } => {
            io::Error::other("caller Tool exited before initial-root installation completed").into()
        }
        StartupFailure::Physical(never) => match never {},
    }
}
fn callback_result<V>(
    outcome: CallbackOutcome<Result<V, reverie::syscalls::Errno>>,
) -> Result<V, Error> {
    match outcome {
        CallbackOutcome::Ready(value) => value.map_err(Error::from),
        CallbackOutcome::Unsupported(reason) => Err(io::Error::other(reason).into()),
        CallbackOutcome::Exit { .. } => {
            Err(io::Error::other("caller instruction hook requested terminal injection").into())
        }
    }
}

/// Export the existing ABI-15 symbols for one caller-selected Tool.
///
/// Invoke exactly once in a constructor-free cdylib. Build reverie-dbt with
/// `default-features = false, features = ["caller-runtime"]`. No runtime Tool
/// registry or local GlobalTool is installed; the launcher owns GlobalState.
#[macro_export]
macro_rules! export_caller_tool {
    ($tool:ty) => {
        #[doc(hidden)]
        mod __reverie_caller_runtime {
            use $crate::caller_runtime::Callback;
            use $crate::caller_runtime::Runtime;
            use $crate::*;
            static RUNTIME: Runtime<$tool> = Runtime::new();
            #[unsafe(no_mangle)]
            pub extern "C" fn reverie_dbt_runtime_abi_version() -> u32 {
                DBT_RUNTIME_ABI_VERSION
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn reverie_dbt_runtime_callbacks_size() -> usize {
                std::mem::size_of::<DbtRuntimeCallbacks>()
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn reverie_dbt_runtime_image_init() -> u64 {
                RUNTIME.image_init()
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_background_init_v3(
                argument: *const DbtRuntimeCallbacks,
                mode: u32,
            ) -> i32 {
                unsafe { RUNTIME.background_init(argument, mode) }
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn reverie_dbt_runtime_ready(generation: u64) -> i32 {
                RUNTIME.ready(generation)
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_thread_init(
                _counters: *mut PrototypeCounters,
                context: usize,
                tid: i32,
                pid: i32,
                parent: i32,
                branches: u64,
                defer: i32,
                invoke: SyscallInvoker,
                read: RegisterReader,
                write: RegisterWriter,
            ) -> i32 {
                unsafe {
                    RUNTIME.thread_init(
                        Callback::new(
                            context,
                            ::reverie::tool_host::TaskIdentity {
                                tid: ::reverie::Tid::from_raw(tid),
                                pid: ::reverie::Pid::from_raw(pid),
                            },
                            branches,
                            invoke,
                            read,
                            write,
                            None,
                        ),
                        defer,
                        parent,
                    )
                }
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_pre_syscall(
                context: usize,
                _counters: *mut PrototypeCounters,
                tid: i32,
                pid: i32,
                _generation: u64,
                number: i64,
                args: *const u64,
                branches: u64,
                result: *mut i64,
                deferred_number: *mut i64,
                deferred_args: *mut u64,
                invoke: SyscallInvoker,
                read: RegisterReader,
                write: RegisterWriter,
                memory: MemoryReader,
                _memory_write: MemoryWriter,
                _emit: RuntimeEmitter,
            ) -> i32 {
                unsafe {
                    RUNTIME.pre_syscall(
                        Callback::new(
                            context,
                            ::reverie::tool_host::TaskIdentity {
                                tid: ::reverie::Tid::from_raw(tid),
                                pid: ::reverie::Pid::from_raw(pid),
                            },
                            branches,
                            invoke,
                            read,
                            write,
                            Some(memory),
                        ),
                        number,
                        args,
                        result,
                        deferred_number,
                        deferred_args,
                    )
                }
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_thread_exit(
                _counters: *mut PrototypeCounters,
                _context: usize,
                _tid: i32,
                _invoke: SyscallInvoker,
            ) {
                RUNTIME.thread_exit();
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn reverie_dbt_runtime_process_exit() {
                RUNTIME.thread_exit();
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_thread_created_v2(
                _counters: *mut PrototypeCounters,
                _context: usize,
                _parent_tid: i32,
                _pid: i32,
                _branches: u64,
                _child_tid: i32,
                _child_runtime_tid: i32,
                _child_tid_addr: u64,
                _flags: u64,
                _invoke: SyscallInvoker,
                _read: RegisterReader,
                _write: RegisterWriter,
            ) -> i32 {
                RUNTIME.failure("initial-root Tool domain refuses native thread creation")
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_process_clone_result(
                _counters: *mut PrototypeCounters,
                _context: usize,
                _parent_tid: i32,
                _pid: i32,
                _branches: u64,
                _number: i64,
                _result: i64,
                _child_tid: i32,
                _child_tid_addr: u64,
                _flags: u64,
                _exit_signal: i32,
                _invoke: SyscallInvoker,
                _read: RegisterReader,
                _write: RegisterWriter,
            ) -> i32 {
                RUNTIME.failure("initial-root Tool domain refuses native process creation")
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_exec_failed(
                _counters: *mut PrototypeCounters,
                _pid: i32,
            ) {
                RUNTIME.failure("initial-root Tool domain refuses exec continuation");
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_copied_syscall(
                _number: i64,
                _args: *const u64,
            ) -> i32 {
                RUNTIME.failure("initial-root Tool domain refuses copied runtime")
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_signal_event(
                _context: usize,
                _counters: *mut PrototypeCounters,
                _tid: i32,
                _pid: i32,
                _generation: u64,
                _branches: u64,
                _event: *mut DbtSignalEvent,
                _invoke: SyscallInvoker,
                _read: RegisterReader,
                _write: RegisterWriter,
                _memory: MemoryReader,
                _memory_write: MemoryWriter,
                _emit: RuntimeEmitter,
            ) -> i32 {
                RUNTIME.failure("initial-root Tool domain refuses delivered signal")
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_rdtsc(
                context: usize,
                _counters: *mut PrototypeCounters,
                tid: i32,
                pid: i32,
                branches: u64,
                with_aux: i32,
                invoke: SyscallInvoker,
                read: RegisterReader,
                write: RegisterWriter,
                _emit: RuntimeEmitter,
                tsc: *mut u64,
                aux: *mut u32,
            ) -> i32 {
                let request = if with_aux == 0 {
                    ::reverie::Rdtsc::Tsc
                } else {
                    ::reverie::Rdtsc::Tscp
                };
                match unsafe {
                    RUNTIME.rdtsc(
                        Callback::new(
                            context,
                            ::reverie::tool_host::TaskIdentity {
                                tid: ::reverie::Tid::from_raw(tid),
                                pid: ::reverie::Pid::from_raw(pid),
                            },
                            branches,
                            invoke,
                            read,
                            write,
                            None,
                        ),
                        request,
                    )
                } {
                    Ok(value) => {
                        unsafe {
                            tsc.write(value.tsc);
                            aux.write(value.aux.unwrap_or(0));
                        }
                        1
                    }
                    Err(error) => RUNTIME.failure(error),
                }
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_cpuid(
                context: usize,
                _counters: *mut PrototypeCounters,
                tid: i32,
                pid: i32,
                branches: u64,
                leaf: u32,
                subleaf: u32,
                invoke: SyscallInvoker,
                read: RegisterReader,
                write: RegisterWriter,
                _emit: RuntimeEmitter,
                registers: *mut u32,
            ) -> i32 {
                match unsafe {
                    RUNTIME.cpuid(
                        Callback::new(
                            context,
                            ::reverie::tool_host::TaskIdentity {
                                tid: ::reverie::Tid::from_raw(tid),
                                pid: ::reverie::Pid::from_raw(pid),
                            },
                            branches,
                            invoke,
                            read,
                            write,
                            None,
                        ),
                        leaf,
                        subleaf,
                    )
                } {
                    Ok(values) => {
                        unsafe {
                            std::ptr::copy_nonoverlapping(values.as_ptr(), registers, 4);
                        }
                        1
                    }
                    Err(error) => RUNTIME.failure(error),
                }
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn reverie_dbt_runtime_name() -> *const std::ffi::c_char {
                c"caller-owned Tool".as_ptr()
            }
            #[unsafe(no_mangle)]
            pub extern "C" fn reverie_dbt_runtime_kind_code() -> u8 {
                255
            }
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn reverie_dbt_runtime_totals(
                branches: *mut u64,
                syscalls: *mut u64,
                rewritten: *mut u64,
                memory_hash: *mut u64,
            ) {
                let values = RUNTIME.totals();
                unsafe {
                    branches.write(values[0]);
                    syscalls.write(values[1]);
                    rewritten.write(values[2]);
                    memory_hash.write(values[3]);
                }
            }
        }
    };
}
