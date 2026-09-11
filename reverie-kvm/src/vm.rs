/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use kvm_bindings::CpuId;
use kvm_bindings::KVM_MAX_CPUID_ENTRIES;
use kvm_bindings::kvm_enable_cap;
use kvm_bindings::kvm_regs;
use kvm_bindings::kvm_userspace_memory_region;
use kvm_bindings::kvm_xsave;
use kvm_ioctls::Cap;
use kvm_ioctls::Kvm;
use kvm_ioctls::VcpuExit;
use kvm_ioctls::VcpuFd;
use kvm_ioctls::VmFd;
use reverie::BackendChildWaitEvent;
use reverie::BackendChildWaitState;
use reverie::BackendStatsRequest;
use reverie::BackendStatsSource;
use reverie::ExitStatus;
use reverie::GlobalTool;
use reverie::Pid;
use reverie::ThreadOwnership;
use reverie::Tool;

use crate::CpuidPolicy;
use crate::Error;
use crate::GuestMemory;
use crate::Result;
use crate::Syscall;
use crate::SyscallRequest;
use crate::bootstrap::BOOT_RESERVED_END;
use crate::bootstrap::MAX_GUEST_THREADS;
use crate::bootstrap::SYSCALL_FRAME_ADDRESS;
use crate::bootstrap::SYSCALL_TRAMPOLINE_ADDRESS;
use crate::bootstrap::SegmentBase;
use crate::bootstrap::THREAD_SYSCALL_AREA_START;
use crate::bootstrap::THREAD_SYSCALL_AREA_STRIDE;
use crate::bootstrap::THREAD_TOOL_STACK_AREA_START;
use crate::bootstrap::TOOL_STACK_SIZE;
use crate::bootstrap::TOOL_STACK_TOP;
use crate::bootstrap::configure_long_mode;
use crate::bootstrap::configure_long_mode_with_syscall_area;
use crate::bootstrap::configure_process_syscall_return;
use crate::bootstrap::configure_user_segments;
use crate::bootstrap::exception_from_halt;
use crate::bootstrap::exception_pushes_error_code;
use crate::bootstrap::process_syscall_return_registers;
use crate::bootstrap::set_syscall_return_park;
use crate::bootstrap::set_user_segment_base;
use crate::bootstrap::stage_process_syscall_return;
use crate::bootstrap::syscall_hypercall_address;
use crate::bootstrap::thread_tool_stack_top;
use crate::elf::LoadedStaticElf;
use crate::elf::TaskLifecycleTable;
use crate::elf::initial_thread_name;
use crate::elf::load_static_elf;
use crate::elf::load_static_elf_file;
use crate::executor::ChildCompletion;
use crate::executor::ChildStartCommand;
use crate::executor::ChildStartGate;
use crate::executor::ElfExecutor;
use crate::executor::ProcessAction;
use crate::executor::SignalDisposition;
use crate::executor::conventional_exit_code;
use crate::runtime::PendingChildCancellation;
use crate::runtime::PendingChildKind;
use crate::runtime::PendingChildStart;
use crate::runtime::SharedChildStarts;
use crate::runtime::SyscallExecutor;
use crate::runtime::ToolContext;
use crate::signal::LEGACY_FPSTATE_SIZE;
use crate::signal::RT_SIGFRAME_SIZE;
use crate::signal::RT_SIGRETURN_SIZE;
use crate::signal::RtSigframe;
use crate::signal::SA_RESTORER;
use crate::signal::SIGNAL_UCONTEXT_FLAGS;
use crate::signal::Sigcontext;
use crate::signal::SignalFrameLayout;
use crate::signal::Ucontext;
use crate::signal::XsaveImage;
use crate::stats::KvmBackendStats;
use crate::stats::KvmExitCollector;
use crate::syscall::FRAME_SIZE;

/// KVM currently permits userspace exits for this standardized hypercall.
/// The prototype uses it as a transport opcode and places the syscall frame
/// address in the first hypercall argument.
pub const VMCALL_SYSCALL_TRANSPORT: u64 = 12;

const SYSCALL_FRAME_STRIDE: u64 = 4096;
const PAGE_SIZE: u64 = 4096;
const VMCALL: [u8; 3] = [0x0f, 0x01, 0xc1];
const VMMCALL: [u8; 3] = [0x0f, 0x01, 0xd9];
const HLT: u8 = 0xf4;
const VMWARE_BACKDOOR_MAGIC: u64 = 0x564d_5868;
const VMWARE_BACKDOOR_PORT: u64 = 0x5658;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StaticElfException {
    vector: u8,
    instruction_pointer: u64,
    stack_pointer: u64,
    rflags: u64,
}

#[derive(Clone)]
pub(crate) struct PageZeroFault {
    pub(crate) registers: kvm_regs,
    pub(crate) event: reverie::SignalEvent,
    code_segment: u16,
    stack_segment: u16,
    error_code: u64,
    address: u64,
    halted_registers: kvm_regs,
    special_registers: kvm_bindings::kvm_sregs,
    xsave: Arc<kvm_xsave>,
    transport: [u8; FRAME_SIZE],
    transport_address: u64,
}

impl PageZeroFault {
    pub(crate) fn pending(&self) -> crate::executor::PendingSignal {
        crate::executor::PendingSignal {
            event: self.event,
            domain: crate::executor::PendingSignalDomain::Thread,
        }
    }

    pub(crate) fn resume_user(&self, backend: &mut KvmBackend, registers: kvm_regs) -> Result<()> {
        configure_user_segments(&backend.vcpu)?;
        let mut special = backend.vcpu.get_sregs()?;
        special.cs.selector = self.code_segment;
        special.ss.selector = self.stack_segment;
        backend.vcpu.set_sregs(&special)?;
        backend.vcpu.set_regs(&registers)?;
        Ok(())
    }

    pub(crate) fn user_registers(&self) -> libc::user_regs_struct {
        let mut registers = crate::runtime::kvm_registers(self.registers, u64::MAX);
        registers.cs = self.code_segment.into();
        registers.ss = self.stack_segment.into();
        registers.fs_base = self.special_registers.fs.base;
        registers.gs_base = self.special_registers.gs.base;
        registers.ds = self.special_registers.ds.selector.into();
        registers.es = self.special_registers.es.selector.into();
        registers.fs = self.special_registers.fs.selector.into();
        registers.gs = self.special_registers.gs.selector.into();
        registers
    }

    fn restore_boundary(&self, backend: &mut KvmBackend) -> Result<()> {
        backend
            .memory
            .write_raw(self.transport_address, &self.transport)?;
        backend.vcpu.set_sregs(&self.special_registers)?;
        backend.vcpu.set_regs(&self.halted_registers)?;
        unsafe {
            backend.vcpu.set_xsave(self.xsave.as_ref())?;
        }
        Ok(())
    }

    fn configure_child(&self, backend: &mut KvmBackend, stack: Option<u64>) -> Result<()> {
        let mut registers = self.registers;
        registers.rax = 0;
        if let Some(stack) = stack {
            registers.rsp = stack;
        }
        self.resume_user(backend, registers)?;
        unsafe {
            backend.vcpu.set_xsave(self.xsave.as_ref())?;
        }
        Ok(())
    }
}

extern "C" fn interrupt_guest_worker(_signal: libc::c_int) {}

fn worker_interrupt_signal() -> libc::c_int {
    libc::SIGURG
}

fn install_worker_interrupt_handler() -> Result<()> {
    static INSTALL_ERRNO: OnceLock<libc::c_int> = OnceLock::new();
    let errno = *INSTALL_ERRNO.get_or_init(|| {
        // SAFETY: action is initialized before sigaction reads it. The handler
        // performs no operations and exists only to make blocking syscalls
        // return EINTR during KVM thread-group teardown.
        unsafe {
            let mut action = std::mem::zeroed::<libc::sigaction>();
            action.sa_sigaction = interrupt_guest_worker as *const () as usize;
            action.sa_flags = 0;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(worker_interrupt_signal(), &action, std::ptr::null_mut()) == 0 {
                0
            } else {
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO)
            }
        }
    });
    if errno == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(errno).into())
    }
}

fn set_guest_interrupt_signal_mask(how: libc::c_int) -> Result<bool> {
    // SAFETY: set and previous are initialized before libc reads or writes them.
    unsafe {
        let mut set = std::mem::zeroed::<libc::sigset_t>();
        let mut previous = std::mem::zeroed::<libc::sigset_t>();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, worker_interrupt_signal());
        let error = libc::pthread_sigmask(how, &set, &mut previous);
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error).into());
        }
        Ok(libc::sigismember(&previous, worker_interrupt_signal()) == 1)
    }
}
type GuestWorkerResult = Result<(ExitStatus, Vec<u8>, Vec<u8>)>;

struct GuestWorkerHandle {
    tid: i32,
    start: Option<ChildStartGate>,
    handle: std::thread::JoinHandle<GuestWorkerResult>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessActionOutcome {
    pub(crate) image_replaced: bool,
    pub(crate) syscall_result: i64,
}

impl ProcessActionOutcome {
    fn returned(syscall_result: i64) -> Self {
        Self {
            image_replaced: false,
            syscall_result,
        }
    }

    pub(crate) fn replaced() -> Self {
        Self {
            image_replaced: true,
            syscall_result: 0,
        }
    }
}

#[derive(Default)]
// TODO-HUMAN-REVIEW(PR-172): Review process-wide KVM worker cancellation state.
struct GuestThreadGroup {
    cancelled: AtomicBool,
    // AUTONOMOUS-BOT-IMPLEMENTED: Propagate worker exit_group to the root vCPU.
    // TODO-HUMAN-REVIEW(PR-177): Review KVM thread-group exit ordering.
    exit_status: Mutex<Option<ExitStatus>>,
    root: Mutex<Option<libc::pthread_t>>,
    workers: Mutex<Vec<libc::pthread_t>>,
    // AUTONOMOUS-BOT-IMPLEMENTED: Join cancelled KVM workers before root teardown returns.
    // TODO-HUMAN-REVIEW(PR-178): Review KVM worker join ordering.
    worker_handles: Mutex<Vec<GuestWorkerHandle>>,
    transport_slots: Mutex<Vec<bool>>,
}

impl GuestThreadGroup {
    fn exit_status(&self) -> Option<ExitStatus> {
        *self
            .exit_status
            .lock()
            .expect("KVM exit-group lock poisoned")
    }

    fn request_exit_group(&self, status: ExitStatus) {
        self.exit_status
            .lock()
            .expect("KVM exit-group lock poisoned")
            .get_or_insert(status);
        self.cancelled.store(true, Ordering::Release);

        if let Some(root) = *self.root.lock().expect("KVM guest root lock poisoned") {
            // SAFETY: root is registered for the lifetime of its run loop.
            unsafe {
                libc::pthread_kill(root, worker_interrupt_signal());
            }
        }
        let workers = self.workers.lock().expect("KVM guest worker lock poisoned");
        for &worker in workers.iter() {
            // SAFETY: the registry lock keeps each pthread ID live for this call.
            unsafe {
                libc::pthread_kill(worker, worker_interrupt_signal());
            }
        }
    }

    fn add_worker_handle(&self, tid: i32, handle: std::thread::JoinHandle<GuestWorkerResult>) {
        self.add_worker_handle_with_gate(tid, None, handle);
    }

    fn add_unstarted_worker(
        &self,
        tid: i32,
        start: ChildStartGate,
        handle: std::thread::JoinHandle<GuestWorkerResult>,
    ) {
        self.add_worker_handle_with_gate(tid, Some(start), handle);
    }

    fn add_worker_handle_with_gate(
        &self,
        tid: i32,
        start: Option<ChildStartGate>,
        handle: std::thread::JoinHandle<GuestWorkerResult>,
    ) {
        let mut handles = self
            .worker_handles
            .lock()
            .expect("KVM guest worker-handle lock poisoned");
        assert!(
            !handles.iter().any(|worker| worker.tid == tid),
            "duplicate KVM guest worker tid {tid}"
        );
        handles.push(GuestWorkerHandle { tid, start, handle });
    }

    fn discard_unstarted_worker(&self, tid: i32) -> Result<bool> {
        let handle = {
            let mut handles = self
                .worker_handles
                .lock()
                .expect("KVM guest worker-handle lock poisoned");
            handles
                .iter()
                .position(|worker| {
                    worker.tid == tid
                        && worker
                            .start
                            .as_ref()
                            .is_some_and(ChildStartGate::is_cancelled)
                })
                .map(|index| handles.swap_remove(index).handle)
        };
        let Some(handle) = handle else {
            return Ok(false);
        };
        let _ = handle.join().map_err(|_| {
            Error::UnexpectedVcpuExit(format!("unstarted KVM guest thread {tid} panicked"))
        })??;
        Ok(true)
    }

    fn cancel_pending_worker_gates(handles: &[GuestWorkerHandle]) {
        for gate in handles.iter().filter_map(|worker| worker.start.as_ref()) {
            if matches!(
                gate.cancel(),
                crate::executor::ChildStartCancellation::NewlyCancelled {
                    delivery_failed: true
                }
            ) {
                eprintln!("reverie-kvm unstarted guest thread lost its cancellation gate");
            }
        }
    }

    fn join_workers(&self) {
        // A worker may register a nested clone while an earlier batch is joining.
        loop {
            let handles = std::mem::take(
                &mut *self
                    .worker_handles
                    .lock()
                    .expect("KVM guest worker-handle lock poisoned"),
            );
            if handles.is_empty() {
                return;
            }
            Self::cancel_pending_worker_gates(&handles);
            for worker in handles {
                if worker.handle.join().is_err() {
                    eprintln!("reverie-kvm guest thread panicked during teardown");
                }
            }
        }
    }

    fn cancel_workers(&self) {
        self.cancelled.store(true, Ordering::Release);
        Self::cancel_pending_worker_gates(
            &self
                .worker_handles
                .lock()
                .expect("KVM guest worker-handle lock poisoned"),
        );
        let workers = self.workers.lock().expect("KVM guest worker lock poisoned");
        for &worker in workers.iter() {
            // SAFETY: the registry lock keeps each pthread ID live for this call.
            unsafe {
                libc::pthread_kill(worker, worker_interrupt_signal());
            }
        }
    }

    // TODO-HUMAN-REVIEW(PR-211): Review KVM exec sibling cancellation ordering.
    fn rearm_after_exec(&self) {
        *self
            .exit_status
            .lock()
            .expect("KVM exit-group lock poisoned") = None;
        self.cancelled.store(false, Ordering::Release);
    }

    // AUTONOMOUS-BOT-IMPLEMENTED: Reuse syscall transports after guest threads exit.
    // TODO-HUMAN-REVIEW(PR-176): Review KVM transport slot lifecycle.
    fn reserve_transport_slot(&self, child_tid: i32) -> Result<usize> {
        let mut slots = self
            .transport_slots
            .lock()
            .expect("KVM transport-slot lock poisoned");
        if slots.is_empty() {
            slots.resize(MAX_GUEST_THREADS as usize, false);
        }
        let slot = slots
            .iter()
            .position(|in_use| !*in_use)
            .ok_or(Error::GuestThreadLimitExceeded(child_tid))?;
        slots[slot] = true;
        Ok(slot)
    }

    fn release_transport_slot(&self, slot: usize) {
        let mut slots = self
            .transport_slots
            .lock()
            .expect("KVM transport-slot lock poisoned");
        if let Some(in_use) = slots.get_mut(slot) {
            *in_use = false;
        }
    }
}

pub(crate) struct GuestThreadRegistration {
    group: Arc<GuestThreadGroup>,
    pthread: libc::pthread_t,
    root: bool,
    restore_blocked_signal: bool,
}

impl Drop for GuestThreadRegistration {
    fn drop(&mut self) {
        if self.root {
            let mut root = self
                .group
                .root
                .lock()
                .expect("KVM guest root lock poisoned");
            if *root == Some(self.pthread) {
                *root = None;
            }
        } else {
            self.group
                .workers
                .lock()
                .expect("KVM guest worker lock poisoned")
                .retain(|worker| *worker != self.pthread);
        }
        if self.restore_blocked_signal {
            let _ = set_guest_interrupt_signal_mask(libc::SIG_BLOCK);
        }
    }
}

fn duplicate_stdin() -> Result<Option<File>> {
    // Duplicate before opening /dev/kvm so internal descriptors can never alias
    // a logically open guest stdin.
    let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if fd >= 0 {
        // SAFETY: F_DUPFD_CLOEXEC returned a new owned descriptor.
        return Ok(Some(unsafe { File::from_raw_fd(fd) }));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EBADF) {
        Ok(None)
    } else {
        Err(error.into())
    }
}

fn validate_root_pid(pid: i32) -> Result<i32> {
    if pid > 0 {
        Ok(pid)
    } else {
        Err(Error::InvalidGuestPid(pid))
    }
}

/// The PID-namespace init process seen by the deterministic container. The
/// ptrace backend runs the guest inside a real PID namespace whose `init` is
/// PID 1, so the conventional root guest (PID 3, see detcore `ROOT_DETPID`) has
/// `getppid() == 1`. KVM synthesizes the guest identity rather than using a real
/// namespace, so it must reproduce the same parent value for parity.
const CONTAINER_INIT_PID: i32 = 1;

/// Deterministic parent PID for the container's root guest, matching the ptrace
/// backend. A guest that is itself the namespace init (PID 1) has no parent and
/// reports `getppid() == 0`, exactly as Linux `init` does; any other root guest
/// is parented to the namespace init (PID 1).
fn root_parent_pid(root_pid: i32) -> i32 {
    if root_pid == CONTAINER_INIT_PID {
        0
    } else {
        CONTAINER_INIT_PID
    }
}

/// Normalizes either KVM-observable RIP form to the post-hypercall boundary
/// that can be restored after a returning injected process action.
fn normalize_completed_syscall_boundary_registers(
    memory: &GuestMemory,
    mut registers: kvm_regs,
    hypercall_instruction: [u8; 3],
    hypercall_address: u64,
) -> Result<kvm_regs> {
    let return_address = hypercall_address
        .checked_add(hypercall_instruction.len() as u64)
        .ok_or_else(|| Error::UnexpectedVcpuExit("hypercall RIP overflow".to_owned()))?;
    if registers.rip != hypercall_address && registers.rip != return_address {
        return Err(Error::UnexpectedVcpuExit(format!(
            "syscall boundary RIP {:#x} is neither hypercall {hypercall_address:#x} nor return {return_address:#x}",
            registers.rip,
        )));
    }
    let mut observed = [0; 3];
    memory.read_raw(hypercall_address, &mut observed)?;
    if observed != hypercall_instruction {
        return Err(Error::UnexpectedVcpuExit(format!(
            "syscall boundary at {hypercall_address:#x} does not contain the configured hypercall"
        )));
    }
    if registers.rip == hypercall_address {
        registers.rip = return_address;
    }
    // The static Tool loop publishes zero to KVM_EXIT_HYPERCALL.ret before
    // any process action can consume the exit. Both admitted RIP forms model
    // that same completed-hypercall state.
    registers.rax = 0;
    Ok(registers)
}

/// Exact reusable state for a syscall transport whose KVM hypercall has
/// already been acknowledged but whose ring-zero return path has not run.
///
/// A returning process action temporarily consumes that transport to park and
/// snapshot the parent. Restoring only the logical user registers is not
/// sufficient: a later signal delivery still stages its frame through the
/// stopped ring-zero trampoline. Keep the complete transport frame plus both
/// KVM register sets together so every caller restores the same boundary.
#[derive(Clone)]
pub(crate) struct CompletedSyscallBoundary {
    frame_address: u64,
    frame: [u8; FRAME_SIZE],
    registers: kvm_regs,
    special_registers: kvm_bindings::kvm_sregs,
    action_return_registers: Option<kvm_regs>,
    stage_action_outcome: bool,
}

impl CompletedSyscallBoundary {
    pub(crate) fn capture(
        backend: &KvmBackend,
        frame_address: u64,
        action_return_registers: Option<kvm_regs>,
    ) -> Result<Self> {
        let mut frame = [0; FRAME_SIZE];
        backend.memory.read_raw(frame_address, &mut frame)?;
        let (registers, special_registers) = backend.completed_syscall_boundary_registers()?;
        Ok(Self {
            frame_address,
            frame,
            registers,
            special_registers,
            action_return_registers,
            stage_action_outcome: false,
        })
    }

    pub(crate) fn capture_for_action(
        backend: &KvmBackend,
        frame_address: u64,
        action_return_registers: Option<kvm_regs>,
        action: &ProcessAction,
    ) -> Result<ProcessActionContinuation> {
        match action {
            ProcessAction::Fork { .. } | ProcessAction::Thread { .. } => {
                let mut boundary = Self::capture(backend, frame_address, action_return_registers)?;
                boundary.stage_action_outcome = true;
                Ok(ProcessActionContinuation::Restore(Box::new(boundary)))
            }
            ProcessAction::Exec { .. } => {
                let mut boundary = Self::capture(backend, frame_address, action_return_registers)?;
                boundary.stage_action_outcome = true;
                Ok(ProcessActionContinuation::Exec(Box::new(boundary)))
            }
        }
    }

    pub(crate) fn stage_action_result(&self, backend: &KvmBackend, result: i64) -> Result<()> {
        let mut memory = backend.memory.clone();
        if let Some(mut registers) = self.action_return_registers {
            registers.rax = result as u64;
            stage_process_syscall_return(&mut memory, &backend.vcpu, self.frame_address, registers)
        } else {
            SyscallRequest::write_result(&mut memory, self.frame_address, result)
        }
    }

    fn restore(&self, backend: &mut KvmBackend) -> Result<()> {
        let memory = backend.memory.clone();
        memory.write_raw(self.frame_address, &self.frame)?;
        backend.vcpu.set_sregs(&self.special_registers)?;
        backend.vcpu.set_regs(&self.registers)?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            frame_address: 0x1000,
            frame: [0; FRAME_SIZE],
            // SAFETY: these plain KVM ABI register structures admit all-zero
            // values, and policy-only tests never submit them to KVM.
            registers: unsafe { std::mem::zeroed() },
            special_registers: unsafe { std::mem::zeroed() },
            action_return_registers: None,
            stage_action_outcome: false,
        }
    }
}

pub(crate) enum ProcessActionContinuation {
    Restore(Box<CompletedSyscallBoundary>),
    Exec(Box<CompletedSyscallBoundary>),
}

impl ProcessActionContinuation {
    pub(crate) fn from_captured(
        action: &ProcessAction,
        boundary: CompletedSyscallBoundary,
    ) -> Self {
        match action {
            ProcessAction::Fork { .. } | ProcessAction::Thread { .. } => {
                Self::Restore(Box::new(boundary))
            }
            ProcessAction::Exec { .. } => Self::Exec(Box::new(boundary)),
        }
    }

    fn finish(
        self,
        backend: &mut KvmBackend,
        action_result: Result<ProcessActionOutcome>,
    ) -> Result<ProcessActionOutcome> {
        // Never restore after a failed action: its partial process state must
        // remain observable to the caller's fatal cleanup path.
        let outcome = action_result?;
        match (self, outcome.image_replaced) {
            (Self::Restore(boundary), _) | (Self::Exec(boundary), false) => {
                boundary.restore(backend)?;
                if boundary.stage_action_outcome {
                    boundary.stage_action_result(backend, outcome.syscall_result)?;
                }
            }
            (Self::Exec(_), true) => {}
        }
        Ok(outcome)
    }
}

/// A single-vCPU KVM backend used to exercise the syscall transport.
pub struct KvmBackend {
    // Field order ensures the vCPU and VM are dropped before registered memory.
    pub(crate) vcpu: VcpuFd,
    vm: VmFd,
    pub(crate) memory: GuestMemory,
    _kvm: Kvm,
    cpuid_policy: CpuidPolicy,
    hypercall_instruction: [u8; 3],
    syscall_trampoline_address: u64,
    pub(crate) syscall_frame_address: u64,
    thread_group: Arc<GuestThreadGroup>,
    thread_slot: Option<usize>,
    is_guest_thread: bool,
    // Who owns this backend's guest threads. The single value drives BOTH the
    // CLONE_THREAD worker dispatch path (`run_process_action_with_tool`) and
    // `futex`/CLEARTID ownership (`is_backend_owned_syscall`), so the two can
    // never disagree. Propagated to every child backend. This is the *effective*
    // ownership; when running a tool it is resolved from `thread_ownership_override`
    // (if set) else the tool's `Tool::thread_ownership` at run entry.
    pub(crate) thread_ownership: ThreadOwnership,
    // Explicit caller override for `thread_ownership`. `None` means "follow the
    // tool" — resolve from `Tool::thread_ownership` at run entry (the safe,
    // Tool-owned "follow children" default). `Some(_)` forces that ownership
    // regardless of the tool (set via `set_thread_ownership` /
    // `unmonitored_threads`), and survives run-entry resolution.
    thread_ownership_override: Option<ThreadOwnership>,
    pub(crate) static_elf: Option<LoadedStaticElf>,
    stdin: Option<File>,
    pub(crate) root_pid: i32,
    // One optional collector is shared by every fork and thread backend in the
    // guest tree. `None` is the allocation-free, update-free default.
    pub(crate) exit_collector: Option<Arc<KvmExitCollector>>,
}

struct KvmProcessSnapshot {
    memory: GuestMemory,
    registers: kvm_regs,
    xsave: kvm_xsave,
    stdin: Option<File>,
    cpuid_policy: CpuidPolicy,
}

struct ForkedProcess {
    pid: i32,
    backend: KvmBackend,
    executor: ElfExecutor,
}

// A process snapshot can be taken while another Tool-owned thread has its own
// scratch page exposed in the shared user-access map. The fork child must not
// inherit any of those temporary mappings; normalize only the copied map and
// leave the parent's live handlers unchanged.
fn hide_tool_scratch_pages(memory: &GuestMemory) -> Result<()> {
    memory.unmap_user_range(TOOL_STACK_TOP - TOOL_STACK_SIZE, TOOL_STACK_SIZE)?;
    memory.unmap_user_range(
        THREAD_TOOL_STACK_AREA_START,
        BOOT_RESERVED_END - THREAD_TOOL_STACK_AREA_START,
    )?;
    Ok(())
}

impl KvmBackend {
    pub(crate) fn guest_thread_is_cancelled(&self) -> bool {
        self.is_guest_thread && self.thread_group.cancelled.load(Ordering::Acquire)
    }

    /// Creates a VM with one vCPU and a memory slot starting at GPA zero.
    pub fn new(memory_size: usize) -> Result<Self> {
        Self::new_with_cpuid_policy(memory_size, CpuidPolicy::default())
    }

    /// Creates a VM with an explicitly reserved supervisor standard input.
    ///
    /// Callers that initialize async runtimes before KVM should reserve stdin
    /// first so an originally closed descriptor cannot be reused internally.
    pub fn new_with_stdin(memory_size: usize, stdin: Option<File>) -> Result<Self> {
        Self::new_with_cpuid_policy_and_stdin(memory_size, CpuidPolicy::default(), stdin)
    }

    /// Creates a VM with a caller-selected CPUID feature policy.
    pub fn new_with_cpuid_policy(memory_size: usize, cpuid_policy: CpuidPolicy) -> Result<Self> {
        let stdin = duplicate_stdin()?;
        Self::new_with_cpuid_policy_and_stdin(memory_size, cpuid_policy, stdin)
    }

    fn new_with_cpuid_policy_and_stdin(
        memory_size: usize,
        cpuid_policy: CpuidPolicy,
        stdin: Option<File>,
    ) -> Result<Self> {
        let memory = GuestMemory::new(0, memory_size)?;
        Self::new_with_memory_and_cpuid_policy(memory, cpuid_policy, stdin)
    }

    fn new_with_memory_and_cpuid_policy(
        memory: GuestMemory,
        cpuid_policy: CpuidPolicy,
        stdin: Option<File>,
    ) -> Result<Self> {
        install_worker_interrupt_handler()?;
        let kvm = Kvm::new()?;
        let vm = kvm.create_vm()?;
        if !vm.check_extension(Cap::ExitHypercall) {
            return Err(Error::HypercallExitUnsupported);
        }

        let mut cpuid = kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?;
        // TODO-HUMAN-REVIEW(PR-129): Review host-selected private hypercall transport.
        let hypercall_instruction = supported_hypercall_instruction(&cpuid)?;
        cpuid_policy.apply(&mut cpuid)?;
        let cap = kvm_enable_cap {
            cap: Cap::ExitHypercall as u32,
            args: [1_u64 << VMCALL_SYSCALL_TRANSPORT, 0, 0, 0],
            ..Default::default()
        };
        vm.enable_cap(&cap)?;

        let region = kvm_userspace_memory_region {
            slot: 0,
            guest_phys_addr: memory.guest_base(),
            memory_size: memory.len() as u64,
            userspace_addr: memory.host_address(),
            flags: 0,
        };
        // SAFETY: memory owns a page-aligned mapping that remains live until
        // after vcpu and vm are dropped, and slot 0 is registered only once.
        unsafe {
            vm.set_user_memory_region(region)?;
        }

        let vcpu = vm.create_vcpu(0)?;
        vcpu.set_cpuid2(&cpuid)?;
        Ok(Self {
            vcpu,
            vm,
            memory,
            _kvm: kvm,
            cpuid_policy,
            hypercall_instruction,
            syscall_trampoline_address: SYSCALL_TRAMPOLINE_ADDRESS,
            syscall_frame_address: SYSCALL_FRAME_ADDRESS,
            thread_group: Arc::new(GuestThreadGroup::default()),
            thread_slot: None,
            is_guest_thread: false,
            // Effective ownership before any tool run resolves it. The direct
            // (non-tool) personality never dispatches threads through a Tool loop,
            // so Host is the correct effective value there; when a tool runs,
            // `run_static_elf_with_tool` resolves this from the override or the
            // tool's `Tool::thread_ownership` (the Tool-owned "follow children"
            // default).
            thread_ownership: ThreadOwnership::Host,
            // No explicit caller override: follow the tool at run entry.
            thread_ownership_override: None,
            static_elf: None,
            stdin,
            root_pid: 1,
            exit_collector: None,
        })
    }

    /// Enables or disables completed-process-tree KVM exit statistics.
    ///
    /// Call this once before entering the guest. Enabling creates the collector
    /// that every later fork and `CLONE_THREAD` child inherits; disabling drops
    /// it, so unmeasured runs allocate and update no statistics state.
    pub fn set_backend_stats_request(&mut self, request: BackendStatsRequest) {
        self.exit_collector = request
            .is_enabled()
            .then(|| Arc::new(KvmExitCollector::default()));
    }

    /// Returns whether this process tree is collecting KVM exit statistics.
    pub fn backend_stats_request(&self) -> BackendStatsRequest {
        BackendStatsRequest::new(self.exit_collector.is_some())
    }

    /// Records one vCPU exit in the shared process-tree collector, when enabled.
    ///
    /// This is an associated function over a disjoint field so callers can use
    /// it while the live [`VcpuExit`] still borrows the vCPU's `KVM_RUN` mapping.
    pub(crate) fn record_exit(collector: Option<&KvmExitCollector>, exit: &VcpuExit<'_>) {
        if let Some(collector) = collector {
            collector.record(exit);
        }
    }

    /// Forces who owns this backend's guest threads (see [`ThreadOwnership`]),
    /// overriding the tool's own [`reverie::Tool::thread_ownership`].
    ///
    /// The single value drives both the CLONE_THREAD worker dispatch path and
    /// `futex`/CLEARTID ownership, so execution and synchronization can never
    /// disagree. Normally you do **not** call this: when running a tool the
    /// ownership is resolved from the tool's `Tool::thread_ownership`, whose
    /// default is the safe Tool-owned "follow children" model. Call this only to
    /// force a specific ownership regardless of the tool; the override is sticky
    /// and survives run-entry resolution. Call it before running.
    pub fn set_thread_ownership(&mut self, thread_ownership: ThreadOwnership) {
        self.thread_ownership_override = Some(thread_ownership);
        self.thread_ownership = thread_ownership;
    }

    /// Opt this backend's guest threads *out* of tool monitoring: run every
    /// child thread uninstrumented on the direct host personality with
    /// host-backed `futex`/CLEARTID synchronization ([`ThreadOwnership::Host`]).
    ///
    /// This is the deliberately-named "scary" opt-out. It is **not** `unsafe`
    /// (it cannot cause undefined behavior), but it is a determinism/coverage
    /// hazard, so weigh it carefully:
    ///
    /// * The tool never sees the opted-out threads' syscalls, so it cannot
    ///   sanitize, record, or schedule them — determinism is **not** guaranteed
    ///   for those threads or anything ordered against them.
    /// * A tool that expects to schedule the whole thread group (e.g. Detcore)
    ///   has its model broken by unmonitored siblings, and mixing unmonitored
    ///   threads with tool-owned joins can deadlock a `pthread_join`.
    ///
    /// Prefer leaving threads tool-owned (the default). Use this only when a
    /// backend genuinely cannot or must not drive a thread through the tool.
    pub fn unmonitored_threads(&mut self) -> &mut Self {
        self.set_thread_ownership(ThreadOwnership::Host);
        self
    }

    /// Resolves the effective [`ThreadOwnership`] for a tool run: an explicit
    /// caller override ([`Self::set_thread_ownership`] /
    /// [`Self::unmonitored_threads`]) wins, otherwise follow the tool's
    /// [`reverie::Tool::thread_ownership`] (default: Tool-owned "follow
    /// children"). Called once at run entry, before any thread is created.
    pub(crate) fn resolve_thread_ownership(&mut self, ownership: ThreadOwnership) {
        self.thread_ownership = self.thread_ownership_override.unwrap_or(ownership);
    }

    /// Panic-not-hang tripwire enforced at thread creation: the thread's
    /// *execution* owner (which dispatch arm `run_process_action_with_tool`
    /// takes) and its *futex/CLEARTID* owner (how `is_backend_owned_syscall`
    /// classifies `futex`) must agree. They are both derived from the single
    /// [`ThreadOwnership`] value, so this can only fail if a future change
    /// reintroduces a second, independent source of truth — exactly the
    /// split-brain that historically deadlocked `pthread_join` (a Tool-executed
    /// worker whose `futex` was host-owned: the joiner's host `FUTEX_WAIT` was
    /// never woken by the exiting worker's logical `CLEARTID`, observed as a
    /// silent hang / exit=124). Asserting here converts that regression into an
    /// immediate, clearly-labelled panic instead of a hang.
    fn debug_assert_thread_ownership_consistent(&self) {
        debug_assert_eq!(
            self.thread_ownership.executes_on_tool(),
            !crate::runtime::is_backend_owned_syscall(
                libc::SYS_futex as u64,
                self.thread_ownership,
            ),
            "thread execution owner and futex owner disagree for {:?}: a \
             Tool-executed worker with a host-owned futex (or a host-executed \
             worker with a Tool-owned futex) deadlocks pthread_join and must be \
             unrepresentable now that one ThreadOwnership drives both decisions",
            self.thread_ownership,
        );
    }

    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-238): Review configurable KVM root process identity.
    pub fn set_root_pid(&mut self, pid: i32) -> Result<()> {
        let pid = validate_root_pid(pid)?;
        self.root_pid = pid;
        if let Some(loaded) = self.static_elf.as_mut() {
            loaded.pid = pid;
            loaded.pgid = pid;
            loaded.tid = pid;
            loaded.ppid = root_parent_pid(pid);
            loaded.task_lifecycle = Arc::new(Mutex::new(TaskLifecycleTable::with_root(
                pid,
                pid,
                pid,
                loaded.dumpable,
            )));
        }
        Ok(())
    }

    /// Installs an arbitrary real-mode program and selects it as the vCPU entry point.
    pub fn install_real_mode_program(&mut self, entry_point: u64, code: &[u8]) -> Result<()> {
        self.memory.write(entry_point, code)?;
        self.static_elf = None;

        let mut sregs = self.vcpu.get_sregs()?;
        sregs.cs.base = 0;
        sregs.cs.selector = 0;
        sregs.ds.base = 0;
        sregs.ds.selector = 0;
        self.vcpu.set_sregs(&sregs)?;

        let mut regs = self.vcpu.get_regs()?;
        regs.rip = entry_point;
        regs.rflags = 2;
        self.vcpu.set_regs(&regs)?;
        Ok(())
    }

    /// Returns the Tool scratch-page top for this backend. Process leaders use
    /// the fixed root page; guest threads use the page paired with their
    /// transport slot.
    pub(crate) fn tool_stack_top(&self) -> u64 {
        self.thread_slot
            .map_or(TOOL_STACK_TOP, thread_tool_stack_top)
    }

    /// Releases a guest thread's transport and Tool scratch-page slot.
    ///
    /// Normal exit paths call this before notifying the scheduler or clearing
    /// the child TID, so the next guest thread's slot does not depend on when
    /// the host worker object is destroyed. `Drop` remains the error-path
    /// fallback.
    pub(crate) fn release_thread_slot(&mut self) {
        if let Some(slot) = self.thread_slot.take() {
            self.thread_group.release_transport_slot(slot);
        }
    }

    /// Returns the VM's guest memory.
    pub fn memory(&self) -> &GuestMemory {
        &self.memory
    }

    /// Returns mutable access to the VM's guest memory.
    pub fn memory_mut(&mut self) -> &mut GuestMemory {
        &mut self.memory
    }

    /// Loads a static ELF executable and prepares the vCPU to enter it in long mode.
    ///
    /// The initial process personality supports x86-64 `ET_EXEC` images without a
    /// `PT_INTERP` segment. Dynamic executables require a userspace dynamic linker
    /// and are deliberately rejected.
    pub fn install_static_elf(&mut self, image: &[u8], argv0: &str) -> Result<()> {
        self.install_static_elf_with_args(image, &[argv0], &[])
    }

    /// Loads a static ELF with an explicit `argv` and `envp` and prepares the
    /// vCPU to enter it in long mode.
    ///
    /// `argv` must be non-empty; `argv[0]` becomes the program name reported to
    /// the guest (initial stack and `AT_EXECFN`/`readlink("/proc/self/exe")`).
    /// The guest observes a standard System V initial stack: `argc`, the `argv`
    /// pointer array, a NULL terminator, the `envp` pointer array, a NULL
    /// terminator, and the auxiliary vector.
    pub fn install_static_elf_with_args(
        &mut self,
        image: &[u8],
        argv: &[&str],
        envp: &[&str],
    ) -> Result<()> {
        let cwd = std::env::current_dir()?;
        self.install_static_elf_with_context(image, argv, envp, &cwd)
    }

    /// Loads independently supplied ELF bytes, arguments, environment, and working directory.
    ///
    /// This entry does not establish a backing file from `argv[0]`. Executable
    /// following-stat and retained self-exec require a known backing file and
    /// return ENOENT without one. The legacy nominal executable readlink remains.
    pub fn install_static_elf_with_context(
        &mut self,
        image: &[u8],
        argv: &[&str],
        envp: &[&str],
        cwd: &Path,
    ) -> Result<()> {
        let loaded = load_static_elf(&mut self.memory, image, argv, envp, cwd)?;
        self.install_loaded_static_elf(loaded)
    }

    fn install_loaded_static_elf(&mut self, mut loaded: LoadedStaticElf) -> Result<()> {
        loaded.pid = self.root_pid;
        loaded.pgid = self.root_pid;
        loaded.tid = self.root_pid;
        loaded.ppid = root_parent_pid(self.root_pid);
        loaded.task_lifecycle = Arc::new(Mutex::new(TaskLifecycleTable::with_root(
            self.root_pid,
            self.root_pid,
            self.root_pid,
            loaded.dumpable,
        )));
        loaded.stdin = self.stdin.as_ref().map(File::try_clone).transpose()?;
        configure_long_mode(
            &mut self.memory,
            &self.vcpu,
            loaded.entry_point,
            loaded.stack_pointer,
            self.hypercall_instruction,
        )?;
        self.memory.enable_user_access();
        self.static_elf = Some(loaded);
        Ok(())
    }

    /// Loads image bytes from the supplied open file, retaining the final executable.
    ///
    /// Positional reads start at zero without changing the file's shared offset.
    /// `argv[0]` remains independent of the file identity. The retained object's
    /// path supplies the initial executable name; later exec uses its invoked name.
    /// For scripts, each interpreter is read from its retained open file, and the
    /// final interpreter's file and bytes become the executable identity. The
    /// original script still supplies the invoked name and script argument.
    /// This does not add initial execution authorization or snapshot concurrent
    /// host file writes. The caller must provide a stable, readable executable.
    pub fn install_static_elf_file_with_context(
        &mut self,
        file: File,
        argv: &[&str],
        envp: &[&str],
        cwd: &Path,
    ) -> Result<()> {
        let loaded = load_static_elf_file(&mut self.memory, file, argv, envp, cwd)?;
        self.install_loaded_static_elf(loaded)
    }

    // AUTONOMOUS-BOT-IMPLEMENTED
    // TODO-HUMAN-REVIEW(PR-228): Review the KVM random-seed configuration API.
    /// Configure the deterministic seed used by virtual random devices.
    pub fn set_random_seed(&mut self, seed: u64) -> Result<()> {
        let loaded = self
            .static_elf
            .as_mut()
            .ok_or(Error::StaticElfNotInstalled)?;
        loaded.random_seed = seed;
        Ok(())
    }

    /// Returns the stable ring-zero continuation for a consumed syscall
    /// hypercall. KVM reports RIP at the VMCALL/VMMCALL instruction; after
    /// userspace publishes `exit.ret`, the next KVM_RUN advances past it.
    /// Process-action parking consumes that exit, so a reusable boundary must
    /// model the post-hypercall instruction rather than replaying the syscall.
    pub(crate) fn completed_syscall_boundary_registers(
        &self,
    ) -> Result<(kvm_regs, kvm_bindings::kvm_sregs)> {
        let hypercall_address = syscall_hypercall_address(
            self.hypercall_instruction,
            self.syscall_trampoline_address,
            self.syscall_frame_address,
        );
        let registers = normalize_completed_syscall_boundary_registers(
            &self.memory,
            self.vcpu.get_regs()?,
            self.hypercall_instruction,
            hypercall_address,
        )?;
        Ok((registers, self.vcpu.get_sregs()?))
    }

    fn snapshot_process(&self) -> Result<KvmProcessSnapshot> {
        let memory = self.memory.snapshot()?;
        hide_tool_scratch_pages(&memory)?;
        Ok(KvmProcessSnapshot {
            memory,
            registers: self.vcpu.get_regs()?,
            xsave: self.vcpu.get_xsave()?,
            stdin: self.stdin.as_ref().map(File::try_clone).transpose()?,
            cpuid_policy: self.cpuid_policy,
        })
    }

    fn from_process_snapshot(snapshot: KvmProcessSnapshot) -> Result<Self> {
        let mut child = Self::new_with_memory_and_cpuid_policy(
            snapshot.memory,
            snapshot.cpuid_policy,
            snapshot.stdin,
        )?;
        configure_long_mode(
            &mut child.memory,
            &child.vcpu,
            0,
            snapshot.registers.rsp,
            child.hypercall_instruction,
        )?;
        child.vcpu.set_regs(&snapshot.registers)?;
        // SAFETY: this guest setup does not enable dynamically sized XSTATE features.
        unsafe { child.vcpu.set_xsave(&snapshot.xsave)? };
        Ok(child)
    }

    // TODO-HUMAN-REVIEW(PR-172): Review independent vCPU creation from clone3 state.
    fn from_thread_state(
        memory: GuestMemory,
        registers: kvm_regs,
        xsave: kvm_xsave,
        stdin: Option<File>,
        cpuid_policy: CpuidPolicy,
        child_tid: i32,
        thread_group: Arc<GuestThreadGroup>,
    ) -> Result<Self> {
        let mut child = Self::new_with_memory_and_cpuid_policy(memory, cpuid_policy, stdin)?;
        child.thread_group = thread_group;
        child.is_guest_thread = true;
        let slot = child.thread_group.reserve_transport_slot(child_tid)?;
        child.thread_slot = Some(slot);
        let syscall_trampoline_address =
            THREAD_SYSCALL_AREA_START + slot as u64 * THREAD_SYSCALL_AREA_STRIDE;
        let syscall_frame_address = syscall_trampoline_address + PAGE_SIZE;
        child.syscall_trampoline_address = syscall_trampoline_address;
        child.syscall_frame_address = syscall_frame_address;
        configure_long_mode_with_syscall_area(
            &mut child.memory,
            &child.vcpu,
            0,
            registers.rsp,
            child.hypercall_instruction,
            syscall_trampoline_address,
            syscall_frame_address,
            false,
        )?;
        child.vcpu.set_regs(&registers)?;
        // SAFETY: this guest setup does not enable dynamically sized XSTATE features.
        unsafe { child.vcpu.set_xsave(&xsave)? };
        Ok(child)
    }

    // TODO-HUMAN-REVIEW(PR-156): Review lifecycle-hook exec image replacement API.
    pub(crate) fn exec_process(
        &mut self,
        executor: &mut ElfExecutor,
        executable: (&Path, Option<Arc<File>>),
        image: &[u8],
        argv: &[String],
        envp: &[String],
    ) -> Result<()> {
        if self.is_guest_thread {
            return Err(Error::GuestThreadExecUnsupported);
        }
        // ElfExecutor preflights the image before scheduling this action. From
        // this reset onward, any exceptional loader or KVM failure is fatal to
        // the backend and is never reported back to the old guest image. This
        // is the same point-of-no-return behavior as the prior zero_raw reset.
        let user_length = usize::try_from(self.memory.guest_end() - BOOT_RESERVED_END)
            .expect("guest memory length must fit usize");
        self.memory.discard_pages(BOOT_RESERVED_END, user_length)?;

        let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
        let envp = envp.iter().map(String::as_str).collect::<Vec<_>>();
        let mut loaded = load_static_elf(&mut self.memory, image, &argv, &envp, executor.cwd())?;
        let (executable_path, executable_file) = executable;
        loaded.executable_path = executable_file
            .as_ref()
            .and_then(|file| std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok())
            .unwrap_or_else(|| executable_path.to_owned());
        loaded.executable_file = executable_file;
        let thread_name = initial_thread_name(executable_path);
        loaded.thread_name = thread_name;
        *loaded
            .thread_group_leader_name
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = thread_name;
        loaded.stdin = self.stdin.as_ref().map(File::try_clone).transpose()?;
        configure_long_mode(
            &mut self.memory,
            &self.vcpu,
            loaded.entry_point,
            loaded.stack_pointer,
            self.hypercall_instruction,
        )?;
        self.memory.enable_user_access();
        executor.replace_after_exec(loaded);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_forked_process(
        &mut self,
        executor: &ElfExecutor,
        child_pid: i32,
        child_stack: Option<u64>,
        parent_tid: Option<u64>,
        child_tid: Option<u64>,
        clear_child_tid: Option<u64>,
        clear_sighand: bool,
        share_address_space: bool,
        park_syscall_return: bool,
        fault: Option<&PageZeroFault>,
    ) -> Result<ForkedProcess> {
        let mut child_executor =
            executor.fork_child(child_pid, clear_sighand, share_address_space)?;
        child_executor.set_clear_child_tid(clear_child_tid);
        if park_syscall_return {
            set_syscall_return_park(
                &mut self.memory,
                self.hypercall_instruction,
                self.syscall_trampoline_address,
                self.syscall_frame_address,
                true,
            )?;
            let vcpu_exit = self.vcpu.run()?;
            Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
            let parked = match vcpu_exit {
                VcpuExit::Hlt => Ok(()),
                exit => Err(Error::UnexpectedVcpuExit(format!(
                    "parent did not park at fork: {exit:?}"
                ))),
            };
            set_syscall_return_park(
                &mut self.memory,
                self.hypercall_instruction,
                self.syscall_trampoline_address,
                self.syscall_frame_address,
                false,
            )?;
            parked?;
        }
        let child_snapshot = self.snapshot_process()?;
        write_tid_best_effort(&mut self.memory, parent_tid, child_pid);

        let mut child = Self::from_process_snapshot(child_snapshot)?;
        // Forked children inherit the parent's thread ownership so execution and
        // `is_backend_owned_syscall`'s futex classification stay consistent.
        child.thread_ownership = self.thread_ownership;
        child.exit_collector = self.exit_collector.clone();
        write_tid_best_effort(&mut child.memory, child_tid, child_pid);
        let (fs_base, gs_base) = child_executor.segment_bases();
        set_user_segment_base(&child.vcpu, SegmentBase::Fs, fs_base)?;
        set_user_segment_base(&child.vcpu, SegmentBase::Gs, gs_base)?;
        configure_process_syscall_return(
            &child.memory,
            &child.vcpu,
            child.syscall_frame_address,
            0,
            child_stack,
        )?;
        if let Some(fault) = fault {
            fault.configure_child(&mut child, child_stack)?;
        }
        Ok(ForkedProcess {
            pid: child_pid,
            backend: child,
            executor: child_executor,
        })
    }

    fn finish_forked_process(
        &mut self,
        executor: &mut ElfExecutor,
        mut child: ForkedProcess,
        status: ExitStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    ) -> Result<()> {
        // A process clone has a private snapshot, so no surviving task can
        // observe this clear; preserve the child-side ABI.
        write_tid_best_effort(
            &mut child.backend.memory,
            child.executor.take_clear_child_tid(),
            0,
        );
        let completion = executor.child_completion(status);
        executor.record_child_completion(child.pid, completion)?;
        executor.append_output(stdout, stderr);
        configure_process_syscall_return(
            &self.memory,
            &self.vcpu,
            self.syscall_frame_address,
            i64::from(child.pid),
            None,
        )
    }

    // TODO-HUMAN-REVIEW(PR-156): Review process actions completed during Tool injection.
    fn run_process_action_inner(
        &mut self,
        executor: &mut ElfExecutor,
        action: ProcessAction,
        park_syscall_return: bool,
        fault: Option<&PageZeroFault>,
    ) -> Result<ProcessActionOutcome> {
        let outcome = match &action {
            ProcessAction::Fork { child_pid, .. } => {
                ProcessActionOutcome::returned(i64::from(*child_pid))
            }
            ProcessAction::Thread { child_tid, .. } => {
                ProcessActionOutcome::returned(i64::from(*child_tid))
            }
            ProcessAction::Exec { .. } => ProcessActionOutcome::replaced(),
        };
        match action {
            ProcessAction::Fork {
                child_pid,
                child_stack,
                parent_tid,
                child_tid,
                clear_child_tid,
                clear_sighand,
                share_address_space,
            } => {
                let mut child = self.prepare_forked_process(
                    executor,
                    child_pid,
                    child_stack,
                    parent_tid,
                    child_tid,
                    clear_child_tid,
                    clear_sighand,
                    share_address_space,
                    park_syscall_return,
                    fault,
                )?;
                let (code, stdout, stderr) =
                    child.backend.run_static_elf_process(&mut child.executor)?;
                self.finish_forked_process(executor, child, code, stdout, stderr)?;
            }
            // TODO-HUMAN-REVIEW(PR-172): Review concurrent CLONE_THREAD lifecycle semantics.
            ProcessAction::Thread {
                child_tid,
                child_stack,
                parent_tid,
                child_tid_address,
                clear_child_tid,
                tls,
            } => {
                let parent_registers = self.vcpu.get_regs()?;
                let parent_xsave = self.vcpu.get_xsave()?;
                let (parent_fs, parent_gs) = executor.segment_bases();
                let mut parent_syscall_frame = vec![0; FRAME_SIZE];
                self.memory
                    .read_raw(self.syscall_frame_address, &mut parent_syscall_frame)?;

                if park_syscall_return {
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        true,
                    )?;
                    let vcpu_exit = self.vcpu.run()?;
                    Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
                    let parked = match vcpu_exit {
                        VcpuExit::Hlt => Ok(()),
                        exit => Err(Error::UnexpectedVcpuExit(format!(
                            "parent did not park at thread clone: {exit:?}"
                        ))),
                    };
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        false,
                    )?;
                    parked?;
                }
                let child_registers = self.vcpu.get_regs()?;

                write_tid_best_effort(&mut self.memory, parent_tid, child_tid);
                write_tid_best_effort(&mut self.memory, child_tid_address, child_tid);
                let child_fs = tls.unwrap_or(parent_fs);
                let mut child_executor = executor.thread_child(child_tid)?;
                child_executor.set_thread_context(child_tid, child_fs, parent_gs);
                child_executor.set_clear_child_tid(clear_child_tid);
                let child_stdin = self.stdin.as_ref().map(File::try_clone).transpose()?;
                let mut child = Self::from_thread_state(
                    self.memory.clone(),
                    child_registers,
                    parent_xsave,
                    child_stdin,
                    self.cpuid_policy,
                    child_tid,
                    self.thread_group.clone(),
                )?;
                // Thread children inherit the parent's thread ownership so
                // execution and futex classification stay consistent.
                self.debug_assert_thread_ownership_consistent();
                child.thread_ownership = self.thread_ownership;
                child.exit_collector = self.exit_collector.clone();
                child
                    .memory
                    .write_raw(child.syscall_frame_address, &parent_syscall_frame)?;
                set_user_segment_base(&child.vcpu, SegmentBase::Fs, child_fs)?;
                set_user_segment_base(&child.vcpu, SegmentBase::Gs, parent_gs)?;
                configure_process_syscall_return(
                    &child.memory,
                    &child.vcpu,
                    child.syscall_frame_address,
                    0,
                    Some(child_stack),
                )?;

                if let Some(fault) = fault {
                    fault.configure_child(&mut child, Some(child_stack))?;
                }

                self.vcpu.set_regs(&parent_registers)?;
                configure_process_syscall_return(
                    &self.memory,
                    &self.vcpu,
                    self.syscall_frame_address,
                    i64::from(child_tid),
                    None,
                )?;

                let handle = std::thread::Builder::new()
                    .name(format!("reverie-kvm-guest-{child_tid}"))
                    .spawn(move || {
                        let result = child.run_static_elf_process(&mut child_executor);
                        child.release_thread_slot();
                        clear_tid_and_wake(
                            &mut child.memory,
                            child_executor.take_clear_child_tid(),
                        );
                        let cancelled = child.thread_group.cancelled.load(Ordering::Acquire);
                        if let Err(error) = &result
                            && !cancelled
                        {
                            eprintln!("reverie-kvm guest thread {child_tid} failed: {error}");
                        }
                        result
                    })?;
                self.thread_group.add_worker_handle(child_tid, handle);
            }
            ProcessAction::Exec {
                executable_path,
                executable_file,
                image,
                argv,
                envp,
            } => {
                if self.is_guest_thread {
                    return Err(Error::GuestThreadExecUnsupported);
                }
                if park_syscall_return {
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        true,
                    )?;
                    let vcpu_exit = self.vcpu.run()?;
                    Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
                    let parked = match vcpu_exit {
                        VcpuExit::Hlt => Ok(()),
                        exit => Err(Error::UnexpectedVcpuExit(format!(
                            "process did not park before exec: {exit:?}"
                        ))),
                    };
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        false,
                    )?;
                    parked?;
                }
                // A successful exec terminates every sibling thread before the
                // new address space becomes visible. Leaving a sibling vCPU
                // alive lets it execute stale instructions in the replacement
                // image and can turn an otherwise successful exec into a fault.
                self.cancel_guest_threads();
                let result = self.exec_process(
                    executor,
                    (&executable_path, executable_file),
                    &image,
                    &argv,
                    &envp,
                );
                self.thread_group.rearm_after_exec();
                result?;
            }
        }
        Ok(outcome)
    }

    #[cfg(test)]
    fn run_process_action(
        &mut self,
        executor: &mut ElfExecutor,
        action: ProcessAction,
        park_syscall_return: bool,
    ) -> Result<()> {
        self.run_process_action_inner(executor, action, park_syscall_return, None)
            .map(|_| ())
    }

    /// Cancels and joins children created by a Tool action whose parent
    /// boundary could not be committed.
    pub(crate) fn discard_unstarted_tool_children(
        &mut self,
        executor: &mut ElfExecutor,
        starts: &SharedChildStarts,
    ) -> Result<()> {
        let children = starts
            .lock()
            .expect("KVM child-start lock poisoned")
            .drain(..)
            .map(PendingChildStart::cancel)
            .collect::<Vec<_>>();
        let mut first_error = None;
        for child in children {
            let PendingChildCancellation::NewlyCancelled {
                child,
                delivery_failed,
            } = child
            else {
                continue;
            };
            if delivery_failed {
                first_error.get_or_insert_with(|| {
                    Error::UnexpectedVcpuExit(
                        "unstarted KVM child lost its parent cancellation gate".to_owned(),
                    )
                });
            }
            let result = match child {
                PendingChildKind::ForkProcess(pid) => executor
                    .discard_unstarted_child_process(pid)
                    .and_then(|found| {
                        found.then_some(()).ok_or_else(|| {
                            Error::UnexpectedVcpuExit(format!(
                                "unstarted KVM child process {pid} was not registered"
                            ))
                        })
                    }),
                PendingChildKind::ToolThread(tid) => self
                    .thread_group
                    .discard_unstarted_worker(tid)
                    .and_then(|found| {
                        found.then_some(()).ok_or_else(|| {
                            Error::UnexpectedVcpuExit(format!(
                                "unstarted KVM guest thread {tid} was not registered"
                            ))
                        })
                    }),
            };
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn finish_tool_process_action_at_boundary(
        &mut self,
        executor: &mut ElfExecutor,
        starts: &SharedChildStarts,
        continuation: ProcessActionContinuation,
        action_result: Result<ProcessActionOutcome>,
    ) -> Result<ProcessActionOutcome> {
        match continuation.finish(self, action_result) {
            Ok(outcome) => Ok(outcome),
            Err(action_error) => Err(self.cleanup_unstarted_tool_children_after_error(
                executor,
                starts,
                action_error,
            )),
        }
    }

    pub(crate) fn start_pending_tool_children(
        &mut self,
        executor: &mut ElfExecutor,
        starts: &SharedChildStarts,
    ) -> Result<()> {
        if let Err(error) = crate::runtime::start_pending_children(starts) {
            return Err(self.cleanup_unstarted_tool_children_after_error(executor, starts, error));
        }
        Ok(())
    }

    /// Preserves the action/handler failure as the primary diagnostic while
    /// still reporting a secondary failure to cancel or join an unstarted
    /// child created during the same callback.
    pub(crate) fn cleanup_unstarted_tool_children_after_error(
        &mut self,
        executor: &mut ElfExecutor,
        starts: &SharedChildStarts,
        primary: Error,
    ) -> Error {
        match self.discard_unstarted_tool_children(executor, starts) {
            Ok(()) => primary,
            Err(cleanup) => Error::UnexpectedVcpuExit(format!(
                "KVM Tool callback failed: {primary}; unstarted-child cleanup also failed: {cleanup}"
            )),
        }
    }

    /// Runs one process action and restores the completed syscall transport
    /// exactly when the action returns to the original image.
    pub(crate) fn run_process_action_at_boundary(
        &mut self,
        executor: &mut ElfExecutor,
        action: ProcessAction,
        continuation: ProcessActionContinuation,
    ) -> Result<ProcessActionOutcome> {
        let result = self.run_process_action_inner(executor, action, true, None);
        continuation.finish(self, result)
    }

    // TODO-HUMAN-REVIEW(PR-192): Review tool lifecycle for KVM fork children.
    // TODO-HUMAN-REVIEW(PR-235): Review concurrent fork-child Tool execution.
    async fn run_process_action_with_tool_inner<T>(
        &mut self,
        executor: &mut ElfExecutor,
        action: ProcessAction,
        park_syscall_return: bool,
        context: ToolContext<'_, T>,
        fault: Option<&PageZeroFault>,
    ) -> Result<ProcessActionOutcome>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        match action {
            ProcessAction::Fork {
                child_pid,
                child_stack,
                parent_tid,
                child_tid,
                clear_child_tid,
                clear_sighand,
                share_address_space,
            } => {
                let mut child = self.prepare_forked_process(
                    executor,
                    child_pid,
                    child_stack,
                    parent_tid,
                    child_tid,
                    clear_child_tid,
                    clear_sighand,
                    share_address_space,
                    park_syscall_return,
                    fault,
                )?;

                let child_pid = Pid::from_raw(child.pid);
                let child_tool = T::new(child_pid, &context.config);
                let child_thread_state = child_tool
                    .init_thread_state(child_pid, Some((context.tid, context.thread_state)));
                let global_state = context.global_state.ok_or_else(|| {
                    Error::UnexpectedVcpuExit(
                        "forked KVM Tool process requires shared global state".to_owned(),
                    )
                })?;
                let config = context.config;
                let subscriptions = context.subscriptions;
                let pending_child_starts = context.pending_child_starts;
                let raw_child_pid = child.pid;
                let parent_pid = context.pid;
                let lifecycle_state = global_state.clone();
                let auto_reap = executor.child_exit_policy();
                let completion_notifier = executor.child_completion_notifier();
                let completion = Arc::new(Mutex::new(None));
                let child_completion = completion.clone();
                let (start_sender, start_receiver) = std::sync::mpsc::channel();
                let start_gate = ChildStartGate::new(start_sender);
                let handle = std::thread::Builder::new()
                    .name(format!("reverie-kvm-process-{raw_child_pid}"))
                    .spawn(move || {
                        match start_receiver.recv() {
                            Ok(ChildStartCommand::Start) => {}
                            Ok(ChildStartCommand::Cancel) => return Ok(()),
                            Err(_) => {
                                return Err(Error::UnexpectedVcpuExit(format!(
                                    "KVM child process {raw_child_pid} lost its parent start gate"
                                )));
                            }
                        }
                        let result = futures::executor::block_on(
                            child.backend.run_static_elf_process_with_tool(
                                &mut child.executor,
                                child_pid,
                                // A forked process child is its own leader (tid == pid).
                                child_pid,
                                child_tool,
                                child_thread_state,
                                global_state,
                                &config,
                                &subscriptions,
                                false,
                            ),
                        );
                        match result {
                            Ok((status, _, _)) => {
                                write_tid_best_effort(
                                    &mut child.backend.memory,
                                    child.executor.take_clear_child_tid(),
                                    0,
                                );
                                let waitable = !auto_reap.load(Ordering::SeqCst);
                                let completion =
                                    ChildCompletion::from_waitability(status, waitable);
                                *child_completion
                                    .lock()
                                    .expect("KVM child completion lock poisoned") =
                                    Some(completion);
                                let _ = completion_notifier.send(raw_child_pid);
                                futures::executor::block_on(
                                    lifecycle_state.on_backend_child_wait_event(
                                        BackendChildWaitEvent {
                                            parent: parent_pid,
                                            child: child_pid,
                                            state: BackendChildWaitState::Exited {
                                                status,
                                                waitable,
                                            },
                                        },
                                    ),
                                )
                                .map_err(Error::Reverie)?;
                                Ok(())
                            }
                            Err(error) => {
                                *child_completion
                                    .lock()
                                    .expect("KVM child completion lock poisoned") =
                                    Some(ChildCompletion::Failed);
                                let _ = completion_notifier.send(raw_child_pid);
                                Err(error)
                            }
                        }
                    })?;
                pending_child_starts
                    .lock()
                    .expect("KVM child-start lock poisoned")
                    .push(PendingChildStart::fork_process(
                        raw_child_pid,
                        start_gate.clone(),
                    ));
                executor.register_child_process_with_gate(
                    raw_child_pid,
                    start_gate,
                    completion,
                    handle,
                );
                configure_process_syscall_return(
                    &self.memory,
                    &self.vcpu,
                    self.syscall_frame_address,
                    i64::from(raw_child_pid),
                    None,
                )
                .map(|()| ProcessActionOutcome::returned(i64::from(raw_child_pid)))
            }
            // `ThreadOwnership::Host`: CLONE_THREAD workers run uninstrumented on
            // the direct backend personality with host-backed synchronization,
            // exactly as `run_process_action`'s Thread branch does. Execution and
            // `futex` ownership are both Host (a single `ThreadOwnership`), so the
            // joiner's real host `FUTEX_WAIT` and the exiting worker's real host
            // `CLONE_CHILD_CLEARTID` wake meet on the same futex word — no
            // deadlock. (The split-brain that deadlocks a join — a Tool-executed
            // worker whose `futex` is host-owned, so its logical CLEARTID wake
            // never reaches the host waiter — is unrepresentable now that one
            // enum drives both decisions.)
            ProcessAction::Thread { .. } if self.thread_ownership.executes_on_host() => {
                self.run_process_action_inner(executor, action, park_syscall_return, fault)
            }
            // `ThreadOwnership::Tool`: a CLONE_THREAD worker runs its own vCPU on
            // a fresh OS thread but shares the guest address space, file table,
            // and process Tool identity with its creator. It is driven through
            // the same Tool loop as the process leader so Detcore sees its
            // syscalls, shares its fd model, and schedules it; `futex` routes to
            // Detcore (runtime.rs) so a join's logical `FUTEX_WAIT` is woken by
            // the worker's logical `CLEARTID`. This mirrors the physical setup in
            // `run_process_action`'s Thread branch, but spawns the Tool loop
            // instead of the direct backend personality.
            ProcessAction::Thread {
                child_tid,
                child_stack,
                parent_tid,
                child_tid_address,
                clear_child_tid,
                tls,
            } => {
                let parent_registers = self.vcpu.get_regs()?;
                let parent_xsave = self.vcpu.get_xsave()?;
                let (parent_fs, parent_gs) = executor.segment_bases();
                let mut parent_syscall_frame = vec![0; FRAME_SIZE];
                self.memory
                    .read_raw(self.syscall_frame_address, &mut parent_syscall_frame)?;

                if park_syscall_return {
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        true,
                    )?;
                    let vcpu_exit = self.vcpu.run()?;
                    Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
                    let parked = match vcpu_exit {
                        VcpuExit::Hlt => Ok(()),
                        exit => Err(Error::UnexpectedVcpuExit(format!(
                            "parent did not park at thread clone: {exit:?}"
                        ))),
                    };
                    set_syscall_return_park(
                        &mut self.memory,
                        self.hypercall_instruction,
                        self.syscall_trampoline_address,
                        self.syscall_frame_address,
                        false,
                    )?;
                    parked?;
                }
                let child_registers = self.vcpu.get_regs()?;

                write_tid_best_effort(&mut self.memory, parent_tid, child_tid);
                write_tid_best_effort(&mut self.memory, child_tid_address, child_tid);
                let child_fs = tls.unwrap_or(parent_fs);
                let mut child_executor = executor.thread_child(child_tid)?;
                child_executor.set_thread_context(child_tid, child_fs, parent_gs);
                child_executor.set_clear_child_tid(clear_child_tid);
                let child_stdin = self.stdin.as_ref().map(File::try_clone).transpose()?;
                let mut child = Self::from_thread_state(
                    self.memory.clone(),
                    child_registers,
                    parent_xsave,
                    child_stdin,
                    self.cpuid_policy,
                    child_tid,
                    self.thread_group.clone(),
                )?;
                // Thread children inherit the parent's thread ownership so
                // execution and futex classification stay consistent.
                self.debug_assert_thread_ownership_consistent();
                child.thread_ownership = self.thread_ownership;
                child.exit_collector = self.exit_collector.clone();
                child
                    .memory
                    .write_raw(child.syscall_frame_address, &parent_syscall_frame)?;
                set_user_segment_base(&child.vcpu, SegmentBase::Fs, child_fs)?;
                set_user_segment_base(&child.vcpu, SegmentBase::Gs, parent_gs)?;
                configure_process_syscall_return(
                    &child.memory,
                    &child.vcpu,
                    child.syscall_frame_address,
                    0,
                    Some(child_stack),
                )?;

                if let Some(fault) = fault {
                    fault.configure_child(&mut child, Some(child_stack))?;
                }

                // CLONE_THREAD shares the process (thread-group) identity, so the
                // worker's Tool carries the creator's pid (tgid) as its detpid,
                // while the thread state is keyed on the new child tid. This
                // mirrors reverie-ptrace's `cloned()`, where the child shares the
                // process Tool identity and receives fresh per-thread state
                // linked to the parent thread.
                let tgid = context.pid;
                let child_tid_pid = Pid::from_raw(child_tid);
                let child_tool = T::new(tgid, &context.config);
                let child_thread_state = child_tool
                    .init_thread_state(child_tid_pid, Some((context.tid, context.thread_state)));
                let global_state = context.global_state.ok_or_else(|| {
                    Error::UnexpectedVcpuExit(
                        "KVM CLONE_THREAD worker requires shared global state".to_owned(),
                    )
                })?;
                let config = context.config;
                let subscriptions = context.subscriptions;

                self.vcpu.set_regs(&parent_registers)?;
                configure_process_syscall_return(
                    &self.memory,
                    &self.vcpu,
                    self.syscall_frame_address,
                    i64::from(child_tid),
                    None,
                )?;

                let pending_child_starts = context.pending_child_starts;
                let (start_sender, start_receiver) = std::sync::mpsc::channel();
                let start_gate = ChildStartGate::new(start_sender);
                let handle = std::thread::Builder::new()
                    .name(format!("reverie-kvm-guest-{child_tid}"))
                    .spawn(move || {
                        match start_receiver.recv() {
                            Ok(ChildStartCommand::Start) => {}
                            Ok(ChildStartCommand::Cancel) => {
                                return Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()));
                            }
                            Err(_) => {
                                panic!("KVM guest thread {child_tid} lost its parent start gate")
                            }
                        }
                        let result =
                            futures::executor::block_on(child.run_static_elf_process_with_tool(
                                &mut child_executor,
                                tgid,
                                child_tid_pid,
                                child_tool,
                                child_thread_state,
                                global_state,
                                &config,
                                &subscriptions,
                                false,
                            ));
                        child.release_thread_slot();
                        clear_tid_and_wake(
                            &mut child.memory,
                            child_executor.take_clear_child_tid(),
                        );
                        let cancelled = child.thread_group.cancelled.load(Ordering::Acquire);
                        if let Err(error) = &result
                            && !cancelled
                        {
                            eprintln!(
                                "reverie-kvm guest thread {child_tid} tool loop failed: {error}"
                            );
                        }
                        result
                    })?;
                self.thread_group
                    .add_unstarted_worker(child_tid, start_gate.clone(), handle);
                pending_child_starts
                    .lock()
                    .expect("KVM child-start lock poisoned")
                    .push(PendingChildStart::tool_thread(child_tid, start_gate));
                Ok(ProcessActionOutcome::returned(i64::from(child_tid)))
            }
            other => self.run_process_action_inner(executor, other, park_syscall_return, fault),
        }
    }

    /// Runs one Tool-owned process action and restores the completed syscall
    /// transport exactly when the action returns to the original image.
    pub(crate) async fn run_process_action_with_tool_at_boundary<T>(
        &mut self,
        executor: &mut ElfExecutor,
        action: ProcessAction,
        context: ToolContext<'_, T>,
        continuation: ProcessActionContinuation,
    ) -> Result<ProcessActionOutcome>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        let pending_child_starts = context.pending_child_starts.clone();
        let action_result = self
            .run_process_action_with_tool_inner(executor, action, true, context, None)
            .await;
        self.finish_tool_process_action_at_boundary(
            executor,
            &pending_child_starts,
            continuation,
            action_result,
        )
    }

    pub(crate) async fn run_process_action_with_tool_from_fault<T>(
        &mut self,
        executor: &mut ElfExecutor,
        action: ProcessAction,
        context: ToolContext<'_, T>,
        fault: &PageZeroFault,
    ) -> Result<ProcessActionOutcome>
    where
        T: Tool + 'static,
        T::ThreadState: 'static,
        T::GlobalState: 'static,
        <T::GlobalState as GlobalTool>::Config: 'static,
    {
        stage_process_syscall_return(
            &mut self.memory,
            &self.vcpu,
            fault.transport_address,
            fault.registers,
        )?;
        let result = self
            .run_process_action_with_tool_inner(executor, action, false, context, Some(fault))
            .await;
        let restored = fault.restore_boundary(self);
        match result {
            Err(error) => Err(error),
            Ok(outcome) => {
                restored?;
                Ok(outcome)
            }
        }
    }

    fn static_elf_exception(&self) -> Result<Option<StaticElfException>> {
        let registers = self.vcpu.get_regs()?;
        let Some(vector) = exception_from_halt(registers.rip) else {
            return Ok(None);
        };
        let first_frame_word = usize::from(exception_pushes_error_code(vector));
        let read_frame_word = |word: usize| -> Result<u64> {
            let mut bytes = [0; std::mem::size_of::<u64>()];
            self.memory.read_raw(
                registers.rsp + ((first_frame_word + word) * bytes.len()) as u64,
                &mut bytes,
            )?;
            Ok(u64::from_le_bytes(bytes))
        };
        Ok(Some(StaticElfException {
            vector,
            instruction_pointer: read_frame_word(0)?,
            rflags: read_frame_word(2)?,
            stack_pointer: read_frame_word(3)?,
        }))
    }

    pub(crate) fn capture_page_zero_fault(
        &self,
        executor: &ElfExecutor,
    ) -> Result<Option<PageZeroFault>> {
        let halted_registers = self.vcpu.get_regs()?;
        if exception_from_halt(halted_registers.rip) != Some(14) {
            return Ok(None);
        }
        let special_registers = self.vcpu.get_sregs()?;
        let address = special_registers.cr2;
        if address >= PAGE_SIZE {
            return Ok(None);
        }
        let mut frame = [0; 6 * 8];
        self.memory.read_raw(halted_registers.rsp, &mut frame)?;
        let word = |index: usize| {
            u64::from_le_bytes(
                frame[index * 8..index * 8 + 8]
                    .try_into()
                    .expect("exception frame word"),
            )
        };
        let error_code = word(0);
        let code_segment = word(2);
        let stack_segment = word(5);
        if ![4, 6, 20].contains(&error_code)
            || code_segment != u64::from(crate::signal::USER_CODE_SELECTOR)
            || ![
                u64::from(crate::signal::USER_DATA_SELECTOR),
                u64::from(crate::signal::USER_DATA_SELECTOR & !3),
            ]
            .contains(&stack_segment)
        {
            return Ok(None);
        }
        let mut registers = halted_registers;
        registers.rip = word(1);
        registers.rflags = word(3);
        registers.rsp = word(4);
        let mut transport = [0; FRAME_SIZE];
        self.memory
            .read_raw(self.syscall_frame_address, &mut transport)?;
        Ok(Some(PageZeroFault {
            registers,
            event: executor.page_zero_fault_event(address),
            code_segment: code_segment as u16,
            stack_segment: stack_segment as u16,
            error_code,
            address,
            halted_registers,
            special_registers,
            xsave: Arc::new(self.vcpu.get_xsave()?),
            transport,
            transport_address: self.syscall_frame_address,
        }))
    }

    // TODO-HUMAN-REVIEW(PR-202): Review the narrowly matched VMware backdoor probe emulation.
    pub(crate) fn try_resume_vmware_backdoor_probe(&mut self) -> Result<bool> {
        let Some(exception) = self.static_elf_exception()? else {
            return Ok(false);
        };
        if exception.vector != 13 {
            return Ok(false);
        }

        let registers = self.vcpu.get_regs()?;
        let mut instruction = [0];
        if self
            .memory
            .read_raw(exception.instruction_pointer, &mut instruction)
            .is_err()
            || instruction != [0xed]
            || registers.rbx & u64::from(u32::MAX) != VMWARE_BACKDOOR_MAGIC
            || registers.rcx & u64::from(u32::MAX) != VMWARE_BACKDOOR_PORT
        {
            return Ok(false);
        }

        let mut registers = registers;
        registers.rbx = 0;
        registers.rip = exception.instruction_pointer + 1;
        registers.rsp = exception.stack_pointer;
        registers.rflags = exception.rflags;
        configure_user_segments(&self.vcpu)?;
        self.vcpu.set_regs(&registers)?;
        Ok(true)
    }

    pub(crate) fn static_elf_halt_error(&self) -> Result<Error> {
        if let Some(exception) = self.static_elf_exception()? {
            return Ok(Error::GuestException {
                vector: exception.vector,
                instruction_pointer: exception.instruction_pointer,
                fault_address: self.vcpu.get_sregs()?.cr2,
            });
        }

        Ok(Error::UnexpectedVcpuExit(
            "static ELF halted without exiting".to_string(),
        ))
    }

    pub(crate) fn deliver_pending_signal_at_syscall_boundary(
        &mut self,
        executor: &mut ElfExecutor,
        syscall_frame_address: u64,
        result: i64,
    ) -> Result<bool> {
        let interrupted = process_syscall_return_registers(
            &self.memory,
            self.vcpu.get_regs()?,
            syscall_frame_address,
            result,
            None,
        )?;
        self.deliver_pending_signal_from_registers(executor, syscall_frame_address, interrupted)
    }

    pub(crate) fn deliver_pending_signal_from_registers(
        &mut self,
        executor: &mut ElfExecutor,
        syscall_frame_address: u64,
        interrupted: kvm_bindings::kvm_regs,
    ) -> Result<bool> {
        loop {
            let Some(pending) = executor
                .take_pending_signal_for_delivery()
                .map_err(|errno| Error::Reverie(errno.into()))?
            else {
                return Ok(false);
            };

            if executor.signal_disposition(pending.event.signal()) == SignalDisposition::Ignore {
                continue;
            }
            return self.deliver_selected_signal_from_registers(
                executor,
                syscall_frame_address,
                interrupted,
                pending,
            );
        }
    }

    pub(crate) fn deliver_selected_signal_from_registers(
        &mut self,
        executor: &mut ElfExecutor,
        syscall_frame_address: u64,
        interrupted: kvm_bindings::kvm_regs,
        pending: crate::executor::PendingSignal,
    ) -> Result<bool> {
        self.deliver_selected_signal_at_boundary(
            executor,
            syscall_frame_address,
            interrupted,
            pending,
            None,
        )
    }

    pub(crate) fn deliver_page_zero_fault(
        &mut self,
        executor: &mut ElfExecutor,
        fault: &PageZeroFault,
        pending: crate::executor::PendingSignal,
    ) -> Result<bool> {
        self.deliver_selected_signal_at_boundary(
            executor,
            self.syscall_frame_address,
            fault.registers,
            pending,
            Some(fault),
        )
    }

    fn deliver_selected_signal_at_boundary(
        &mut self,
        executor: &mut ElfExecutor,
        syscall_frame_address: u64,
        interrupted: kvm_regs,
        pending: crate::executor::PendingSignal,
        fault: Option<&PageZeroFault>,
    ) -> Result<bool> {
        let signal = pending.event.signal();
        match executor.signal_disposition(signal) {
            SignalDisposition::Ignore => return Ok(false),
            SignalDisposition::Terminate => {
                executor.force_signal_exit(signal);
                return Ok(false);
            }
            SignalDisposition::Stop => {
                return Err(Error::UnexpectedVcpuExit(format!(
                    "stopped-state delivery for signal {signal} is unsupported"
                )));
            }
            SignalDisposition::Handled => {}
        }

        let action = executor.signal_action(signal);
        if action.flags & SA_RESTORER == 0
            || action.handler == 0
            || action.handler >= (1_u64 << 47)
            || action.restorer >= (1_u64 << 47)
        {
            executor.force_signal_exit(libc::SIGSEGV);
            return Ok(false);
        }
        let altstack = executor.signal_altstack(interrupted.rsp);
        let (stack_top, lower_bound, reserve_red_zone, autodisarm) =
            match executor.signal_stack_top(action, interrupted.rsp) {
                Ok(stack) => stack,
                Err(_) => {
                    executor.force_signal_exit(libc::SIGSEGV);
                    return Ok(false);
                }
            };
        let layout = match SignalFrameLayout::below(stack_top, reserve_red_zone) {
            Ok(layout) => layout,
            Err(_) => {
                executor.force_signal_exit(libc::SIGSEGV);
                return Ok(false);
            }
        };
        if lower_bound.is_some_and(|lower_bound| !layout.fits_above(lower_bound)) {
            executor.force_signal_exit(libc::SIGSEGV);
            return Ok(false);
        }
        let xsave = if let Some(fault) = fault {
            XsaveImage::from_kvm(&fault.xsave)
        } else {
            XsaveImage::from_kvm(&self.vcpu.get_xsave()?)
        };
        let mut mcontext =
            Sigcontext::from_kvm(interrupted, layout.xsave_address, executor.signal_mask());
        if let Some(fault) = fault {
            mcontext.cs = fault.code_segment;
            mcontext.ss = fault.stack_segment;
            mcontext.err = fault.error_code;
            mcontext.trapno = 14;
            mcontext.cr2 = fault.address;
        }
        let frame = RtSigframe {
            pretcode: action.restorer,
            ucontext: Ucontext {
                flags: SIGNAL_UCONTEXT_FLAGS,
                link: 0,
                stack: altstack,
                mcontext,
                sigmask: executor.signal_mask(),
            },
            siginfo: pending.event.siginfo(),
        };
        if self
            .memory
            .copy_to_user(layout.frame_address, &frame.encode())
            .is_err()
            || self
                .memory
                .copy_to_user(layout.xsave_address, xsave.bytes())
                .is_err()
        {
            executor.force_signal_exit(libc::SIGSEGV);
            return Ok(false);
        }

        let mut handler = interrupted;
        handler.rax = 0;
        handler.rdi = signal as u64;
        handler.rsi = layout.frame_address + 312;
        handler.rdx = layout.frame_address + 8;
        handler.rip = action.handler;
        handler.rsp = layout.frame_address;
        handler.rflags &= !((1 << 8) | (1 << 10) | (1 << 16));
        if let Some(fault) = fault {
            fault.resume_user(self, handler)?;
        } else {
            stage_process_syscall_return(
                &mut self.memory,
                &self.vcpu,
                syscall_frame_address,
                handler,
            )?;
        }
        executor.enter_signal_handler(pending, action, autodisarm);
        Ok(true)
    }

    pub(crate) fn discard_process_clear_tid_at_signal_return(&self, executor: &mut ElfExecutor) {
        if !self.is_guest_thread {
            let _ = executor.take_clear_child_tid();
        }
    }

    pub(crate) fn clear_registered_worker_tid_before_exit(&mut self, executor: &mut ElfExecutor) {
        self.release_thread_slot();
        if !self.is_guest_thread {
            return;
        }
        let Some(address) = executor.take_clear_child_tid() else {
            return;
        };
        debug_assert!(self.thread_slot.is_none());
        let _ = self.memory.put_user_i32(address, 0);
        if self.memory.user_accessible_prefix(address, 4).ok() != Some(4) {
            return;
        }
        let Some(offset) = address.checked_sub(self.memory.guest_base()) else {
            return;
        };
        let Some(host_address) = self.memory.host_address().checked_add(offset) else {
            return;
        };
        unsafe {
            libc::syscall(libc::SYS_futex, host_address, libc::FUTEX_WAKE, 1, 0, 0, 0);
        }
    }

    pub(crate) fn restore_rt_sigreturn(
        &mut self,
        executor: &mut ElfExecutor,
        syscall_frame_address: u64,
    ) -> Result<Option<kvm_bindings::kvm_regs>> {
        let current = process_syscall_return_registers(
            &self.memory,
            self.vcpu.get_regs()?,
            syscall_frame_address,
            0,
            None,
        )?;
        let Some(frame_address) = current.rsp.checked_sub(8) else {
            executor.force_signal_exit(libc::SIGSEGV);
            return Ok(None);
        };
        let mut frame_bytes = [0; RT_SIGFRAME_SIZE];
        if self
            .memory
            .read(frame_address, &mut frame_bytes[..RT_SIGRETURN_SIZE])
            .is_err()
        {
            executor.force_signal_exit(libc::SIGSEGV);
            return Ok(None);
        }
        let frame = RtSigframe::decode(frame_bytes);
        if frame.ucontext.mcontext.validate_for_restore().is_err() {
            executor.force_signal_exit(libc::SIGSEGV);
            return Ok(None);
        }
        let fpstate = frame.ucontext.mcontext.fpstate;
        let xsave = if fpstate == 0 {
            XsaveImage::initialized_kvm()
        } else {
            let mut prefix = [0; LEGACY_FPSTATE_SIZE];
            if self.memory.read(fpstate, &mut prefix).is_err() {
                executor.force_signal_exit(libc::SIGSEGV);
                return Ok(None);
            }
            let size = match XsaveImage::restore_size(&prefix) {
                Ok(size) => size,
                Err(_) => {
                    executor.force_signal_exit(libc::SIGSEGV);
                    return Ok(None);
                }
            };
            let required_alignment = if size == LEGACY_FPSTATE_SIZE {
                16
            } else {
                crate::signal::XSAVE_ALIGNMENT
            };
            if !fpstate.is_multiple_of(required_alignment) {
                executor.force_signal_exit(libc::SIGSEGV);
                return Ok(None);
            }
            let mut bytes = vec![0; size];
            bytes[..LEGACY_FPSTATE_SIZE].copy_from_slice(&prefix);
            if size > LEGACY_FPSTATE_SIZE
                && self
                    .memory
                    .read(
                        fpstate + LEGACY_FPSTATE_SIZE as u64,
                        &mut bytes[LEGACY_FPSTATE_SIZE..],
                    )
                    .is_err()
            {
                executor.force_signal_exit(libc::SIGSEGV);
                return Ok(None);
            }
            match XsaveImage::restore_from_signal(&bytes) {
                Ok(xsave) => xsave,
                Err(_) => {
                    executor.force_signal_exit(libc::SIGSEGV);
                    return Ok(None);
                }
            }
        };
        executor.restore_signal_thread_state(
            frame.ucontext.sigmask,
            frame.ucontext.stack,
            current.rsp,
        );
        let mut restored = current;
        frame.ucontext.mcontext.restore_kvm(&mut restored);
        // SAFETY: the fixed KVM CPUID policy exposes exactly the feature subset
        // accepted by XsaveImage::restore_from_signal above.
        unsafe { self.vcpu.set_xsave(&xsave)? };
        Ok(Some(restored))
    }

    /// Runs the installed static ELF and its forked children until the root exits.
    pub fn run_static_elf(&mut self) -> Result<i32> {
        let loaded = self.static_elf.take().ok_or(Error::StaticElfNotInstalled)?;
        let mut executor = ElfExecutor::new(loaded, false);
        let (status, _, _) = self.run_static_elf_process(&mut executor)?;
        Ok(conventional_exit_code(status))
    }

    /// Runs the installed ELF process tree and captures its standard output streams.
    pub fn run_static_elf_captured(&mut self) -> Result<(i32, Vec<u8>, Vec<u8>)> {
        let loaded = self.static_elf.take().ok_or(Error::StaticElfNotInstalled)?;
        let mut executor = ElfExecutor::new(loaded, true);
        let (status, stdout, stderr) = self.run_static_elf_process(&mut executor)?;
        Ok((conventional_exit_code(status), stdout, stderr))
    }

    fn run_static_elf_process(
        &mut self,
        executor: &mut ElfExecutor,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        let _registration = self.register_guest_thread()?;
        loop {
            if let Some(status) = self.guest_thread_group_exit_status() {
                if !self.is_guest_thread {
                    self.cancel_guest_threads();
                }
                let (stdout, stderr) = executor.take_output();
                return Ok((status, stdout, stderr));
            }
            if self.is_guest_thread && self.thread_group.cancelled.load(Ordering::Acquire) {
                return Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()));
            }
            let vcpu_exit = match self.vcpu.run() {
                Ok(exit) => exit,
                Err(error) if error.errno() == libc::EINTR => continue,
                Err(error) => return Err(error.into()),
            };
            Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
            let (segment_update, process_action, mut signal_boundary) = match vcpu_exit {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    let frame_address = exit.args[0];
                    if frame_address != self.syscall_frame_address {
                        return Err(Error::UnexpectedVcpuExit(format!(
                            "syscall frame is at unexpected address {frame_address:#x}",
                        )));
                    }
                    let return_slot = std::ptr::from_mut(exit.ret) as usize;
                    let request = SyscallRequest::read_from(&self.memory, frame_address)?;
                    // SAFETY: return_slot points into this stopped vCPU's stable KVM_RUN mapping.
                    unsafe {
                        (return_slot as *mut u64).write(0);
                    }
                    if request.number() == libc::SYS_rt_sigreturn as u64 {
                        if let Some(restored) =
                            self.restore_rt_sigreturn(executor, frame_address)?
                        {
                            let delivered = self.deliver_pending_signal_from_registers(
                                executor,
                                frame_address,
                                restored,
                            )?;
                            if !delivered {
                                stage_process_syscall_return(
                                    &mut self.memory,
                                    &self.vcpu,
                                    frame_address,
                                    restored,
                                )?;
                            }
                        }
                        if executor.has_pending_exit() {
                            self.discard_process_clear_tid_at_signal_return(executor);
                            self.clear_registered_worker_tid_before_exit(executor);
                        }
                        (None, None, None)
                    } else {
                        let userspace = process_syscall_return_registers(
                            &self.memory,
                            self.vcpu.get_regs()?,
                            frame_address,
                            0,
                            None,
                        )?;
                        executor.set_current_user_stack_pointer(userspace.rsp);
                        let result = executor.execute(&request, &self.memory);
                        SyscallRequest::write_result(&mut self.memory, frame_address, result)?;
                        (
                            executor.take_segment(),
                            executor.take_process_action(),
                            Some((frame_address, result)),
                        )
                    }
                }
                VcpuExit::Hlt => {
                    if self.try_resume_vmware_backdoor_probe()? {
                        continue;
                    }
                    let Some(fault) = self.capture_page_zero_fault(executor)? else {
                        return Err(self.static_elf_halt_error()?);
                    };
                    executor.prepare_captured_page_zero_fault();
                    self.deliver_page_zero_fault(executor, &fault, fault.pending())?;
                    if executor.has_pending_exit() {
                        self.discard_process_clear_tid_at_signal_return(executor);
                        self.clear_registered_worker_tid_before_exit(executor);
                    }
                    (None, None, None)
                }
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            };

            if let Some((segment, address)) = segment_update {
                set_user_segment_base(&self.vcpu, segment, address)?;
            }

            let mut returns_to_original_image = process_action
                .as_ref()
                .is_none_or(ProcessAction::returns_to_original_image);
            if !returns_to_original_image && executor.has_eligible_pending_signal() {
                return Err(Error::UnexpectedVcpuExit(
                    "KVM exec with an eligible deferred signal is unsupported; \
                     delivery requires a syscall return frame"
                        .to_owned(),
                ));
            }
            let continuation = process_action
                .as_ref()
                .map(|action| {
                    let (frame_address, _) = signal_boundary
                        .expect("a process action can only originate at a syscall boundary");
                    CompletedSyscallBoundary::capture_for_action(self, frame_address, None, action)
                })
                .transpose()?;
            if let Some(action) = process_action {
                let outcome = self.run_process_action_at_boundary(
                    executor,
                    action,
                    continuation.expect("a process action has a continuation policy"),
                )?;
                returns_to_original_image = !outcome.image_replaced;
                if let Some((_, result)) = signal_boundary.as_mut() {
                    *result = outcome.syscall_result;
                }
            }

            // exit/exit_group never return to userspace, so pending delivery
            // cannot replace an exit already selected by the syscall.
            let mut pending_exit = executor.take_exit();
            if returns_to_original_image
                && pending_exit.is_none()
                && let Some((frame_address, result)) = signal_boundary
            {
                self.deliver_pending_signal_at_syscall_boundary(executor, frame_address, result)?;
            }

            pending_exit = pending_exit.or_else(|| executor.take_exit());
            if let Some(exit) = pending_exit {
                if exit.group {
                    self.request_guest_thread_group_exit(exit.status);
                }
                if !self.is_guest_thread {
                    self.cancel_guest_threads();
                }
                let (stdout, stderr) = executor.take_output();
                return Ok((exit.status, stdout, stderr));
            }
        }
    }

    pub(crate) fn register_guest_thread(&self) -> Result<GuestThreadRegistration> {
        let restore_blocked_signal = set_guest_interrupt_signal_mask(libc::SIG_UNBLOCK)?;
        // SAFETY: pthread_self returns the live calling thread's identifier.
        let pthread = unsafe { libc::pthread_self() };
        if self.is_guest_thread {
            self.thread_group
                .workers
                .lock()
                .expect("KVM guest worker lock poisoned")
                .push(pthread);
        } else {
            let previous = self
                .thread_group
                .root
                .lock()
                .expect("KVM guest root lock poisoned")
                .replace(pthread);
            assert!(previous.is_none(), "KVM guest root already registered");
        }
        Ok(GuestThreadRegistration {
            group: self.thread_group.clone(),
            pthread,
            root: !self.is_guest_thread,
            restore_blocked_signal,
        })
    }

    pub(crate) fn guest_thread_group_exit_status(&self) -> Option<ExitStatus> {
        self.thread_group.exit_status()
    }

    pub(crate) fn request_guest_thread_group_exit(&self, status: ExitStatus) {
        self.thread_group.request_exit_group(status);
    }

    // TODO-HUMAN-REVIEW(PR-172): Review signal-driven KVM worker cancellation.
    pub(crate) fn cancel_guest_threads(&self) {
        self.thread_group.cancel_workers();
        if !self.is_guest_thread {
            self.thread_group.join_workers();
        }
    }

    /// Installs one syscall frame and a `vmcall`/`vmmcall; hlt` guest program.
    pub fn install_syscall(
        &mut self,
        entry_point: u64,
        frame_address: u64,
        request: SyscallRequest,
    ) -> Result<()> {
        self.install_syscalls(entry_point, frame_address, &[request])
    }

    /// Installs a guest program that issues each syscall through a userspace hypercall.
    ///
    /// Frames occupy consecutive guest pages because KVM validates this transport
    /// using the `KVM_HC_MAP_GPA_RANGE` argument shape before exiting to userspace.
    pub fn install_syscalls(
        &mut self,
        entry_point: u64,
        frame_address: u64,
        requests: &[SyscallRequest],
    ) -> Result<()> {
        if !frame_address.is_multiple_of(SYSCALL_FRAME_STRIDE) {
            return Err(Error::InvalidSyscallFrameAddress(frame_address));
        }

        let mut code = Vec::with_capacity(requests.len().saturating_mul(15).saturating_add(1));
        for (index, request) in requests.iter().copied().enumerate() {
            let address = SYSCALL_FRAME_STRIDE
                .checked_mul(index as u64)
                .and_then(|offset| frame_address.checked_add(offset))
                .ok_or(Error::InvalidSyscallFrameAddress(frame_address))?;
            let address =
                u32::try_from(address).map_err(|_| Error::InvalidSyscallFrameAddress(address))?;

            request.write_to(&mut self.memory, u64::from(address))?;

            // Real mode defaults to 16-bit operands. The 0x66 prefix loads the
            // complete 32-bit hypercall number and guest-physical frame address.
            code.extend_from_slice(&[0x66, 0xb8]);
            code.extend_from_slice(&(VMCALL_SYSCALL_TRANSPORT as u32).to_le_bytes());
            code.extend_from_slice(&[0x66, 0xbb]);
            code.extend_from_slice(&address.to_le_bytes());
            code.extend_from_slice(&self.hypercall_instruction);
        }
        code.push(HLT);
        // Writes the program and installs the real-mode segment/rip/rflags state.
        self.install_real_mode_program(entry_point, &code)?;

        let mut regs = self.vcpu.get_regs()?;
        // The guest program loads the transport number and frame address into
        // rax/rbx itself, so only the MAP_GPA_RANGE argument shape is set here:
        // KVM validates it before forwarding the enabled hypercall to userspace.
        regs.rcx = 1;
        regs.rdx = 0;
        self.vcpu.set_regs(&regs)?;
        Ok(())
    }

    /// Runs until the guest halts, invoking `handler` for each syscall vmcall.
    pub fn run<F>(&mut self, mut handler: F) -> Result<()>
    where
        F: FnMut(Syscall, &GuestMemory) -> i64,
    {
        loop {
            let vcpu_exit = self.vcpu.run()?;
            Self::record_exit(self.exit_collector.as_deref(), &vcpu_exit);
            match vcpu_exit {
                VcpuExit::Hypercall(exit) => {
                    if exit.nr != VMCALL_SYSCALL_TRANSPORT {
                        return Err(Error::UnexpectedHypercall(exit.nr));
                    }
                    let syscall =
                        SyscallRequest::read_from(&self.memory, exit.args[0])?.into_syscall()?;
                    *exit.ret = handler(syscall, &self.memory) as u64;
                }
                VcpuExit::Hlt => return Ok(()),
                exit => return Err(Error::UnexpectedVcpuExit(format!("{exit:?}"))),
            }
        }
    }

    /// Exposes the VM fd for future backend setup without transferring ownership.
    pub fn vm_fd(&self) -> &VmFd {
        &self.vm
    }
}

impl BackendStatsSource for KvmBackend {
    type Snapshot = KvmBackendStats;

    /// Snapshots exits from the root and every inherited fork/thread collector.
    /// End-of-run callers observe a complete tree because the KVM run paths join
    /// process workers and guest threads before returning to the root caller.
    fn backend_stats(&self) -> Self::Snapshot {
        self.exit_collector
            .as_deref()
            .map_or_else(KvmBackendStats::default, KvmExitCollector::snapshot)
    }
}

impl Drop for KvmBackend {
    fn drop(&mut self) {
        self.release_thread_slot();
        if !self.is_guest_thread {
            self.cancel_guest_threads();
        }
    }
}

fn write_tid_best_effort(memory: &mut GuestMemory, address: Option<u64>, tid: i32) {
    if let Some(address) = address {
        // Linux creates the child even if a clone TID store faults.
        let _ = memory.write(address, &tid.to_le_bytes());
    }
}

// TODO-HUMAN-REVIEW(PR-172): Review CHILD_CLEARTID store and shared futex wake ordering.
pub(crate) fn clear_tid_and_wake(memory: &mut GuestMemory, address: Option<u64>) {
    let Some(address) = address else {
        return;
    };
    // Linux treats a failed CHILD_CLEARTID store as best-effort and skips the
    // wake when the user address is invalid.
    if memory.write(address, &0_i32.to_le_bytes()).is_err() {
        return;
    }
    let Some(offset) = address.checked_sub(memory.guest_base()) else {
        return;
    };
    let Some(host_address) = memory.host_address().checked_add(offset) else {
        return;
    };
    // SAFETY: the successful write above validates the complete futex word,
    // and GuestMemory keeps its shared host mapping alive for this call.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            host_address,
            1, // FUTEX_WAKE; the kernel's CHILD_CLEARTID wake is not private.
            1,
            0,
            0,
            0,
        );
    }
}

fn supported_hypercall_instruction(cpuid: &CpuId) -> Result<[u8; 3]> {
    let supports_vmcall = cpuid
        .as_slice()
        .iter()
        .find(|entry| entry.function == 1)
        .is_some_and(|entry| entry.ecx & (1 << 5) != 0);
    if supports_vmcall {
        return Ok(VMCALL);
    }

    let supports_vmmcall = cpuid
        .as_slice()
        .iter()
        .find(|entry| entry.function == 0x8000_0001)
        .is_some_and(|entry| entry.ecx & (1 << 2) != 0);
    if supports_vmmcall {
        return Ok(VMMCALL);
    }
    Err(Error::HypercallInstructionUnsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct SlotReleaseLog {
        group: Arc<GuestThreadGroup>,
        reused: Mutex<Option<usize>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for SlotReleaseLog {
        type Request = ();
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, (): ()) {
            let slot = self
                .group
                .reserve_transport_slot(10_000)
                .expect("exiting worker slot was not released before on_exit_thread");
            self.group.release_transport_slot(slot);
            *self.reused.lock().expect("slot result lock poisoned") = Some(slot);
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct SlotReleaseTool;

    #[reverie::tool]
    impl Tool for SlotReleaseTool {
        type GlobalState = SlotReleaseLog;
        type ThreadState = ();

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: Pid,
            global: &G,
            _thread_state: Self::ThreadState,
            _status: ExitStatus,
        ) -> std::result::Result<(), reverie::Error> {
            global.send_rpc(()).await;
            Ok(())
        }
    }

    #[derive(Default)]
    struct CancellationLifecycleLog {
        events: Mutex<Vec<u8>>,
        clear_tid: Mutex<Option<(GuestMemory, u64)>>,
        clear_tid_value_at_thread_exit: Mutex<Option<i32>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for CancellationLifecycleLog {
        type Request = u8;
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, event: u8) {
            if event == 2 {
                let mut bytes = [0; std::mem::size_of::<i32>()];
                let clear_tid = self.clear_tid.lock().unwrap();
                let (memory, address) = clear_tid
                    .as_ref()
                    .expect("thread-exit observation must have a CHILD_CLEARTID address");
                memory.read(*address, &mut bytes).unwrap();
                *self.clear_tid_value_at_thread_exit.lock().unwrap() =
                    Some(i32::from_le_bytes(bytes));
            }
            self.events.lock().unwrap().push(event);
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct CancellationLifecycleTool;

    #[reverie::tool]
    impl Tool for CancellationLifecycleTool {
        type GlobalState = CancellationLifecycleLog;
        type ThreadState = ();

        async fn handle_thread_start<G: reverie::Guest<Self>>(
            &self,
            guest: &mut G,
        ) -> std::result::Result<(), reverie::Error> {
            guest.send_rpc(1).await;
            Ok(())
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: Pid,
            global: &G,
            _state: Self::ThreadState,
            _status: ExitStatus,
        ) -> std::result::Result<(), reverie::Error> {
            global.send_rpc(2).await;
            Ok(())
        }

        async fn on_exit_process<G: reverie::GlobalRPC<Self::GlobalState>>(
            self,
            _pid: Pid,
            global: &G,
            _status: ExitStatus,
        ) -> std::result::Result<(), reverie::Error> {
            global.send_rpc(3).await;
            Ok(())
        }
    }

    #[test]
    fn completed_syscall_boundary_accepts_only_the_two_transport_rip_forms() {
        const HYPERCALL_ADDRESS: u64 = 0x100;
        let memory = GuestMemory::new(0, 0x1000).unwrap();
        memory.write_raw(HYPERCALL_ADDRESS, &VMCALL).unwrap();

        let mut at_hypercall: kvm_regs = unsafe { std::mem::zeroed() };
        at_hypercall.rip = HYPERCALL_ADDRESS;
        at_hypercall.rax = 0xfeed;
        let normalized = normalize_completed_syscall_boundary_registers(
            &memory,
            at_hypercall,
            VMCALL,
            HYPERCALL_ADDRESS,
        )
        .unwrap();
        assert_eq!(normalized.rip, HYPERCALL_ADDRESS + VMCALL.len() as u64);
        assert_eq!(normalized.rax, 0);

        let mut after_hypercall = at_hypercall;
        after_hypercall.rip = HYPERCALL_ADDRESS + VMCALL.len() as u64;
        let normalized = normalize_completed_syscall_boundary_registers(
            &memory,
            after_hypercall,
            VMCALL,
            HYPERCALL_ADDRESS,
        )
        .unwrap();
        assert_eq!(normalized.rip, after_hypercall.rip);
        assert_eq!(normalized.rax, 0);

        let mut wrong_rip = at_hypercall;
        wrong_rip.rip += 1;
        assert!(
            normalize_completed_syscall_boundary_registers(
                &memory,
                wrong_rip,
                VMCALL,
                HYPERCALL_ADDRESS,
            )
            .is_err()
        );
        memory.write_raw(HYPERCALL_ADDRESS, &[0x90; 3]).unwrap();
        assert!(
            normalize_completed_syscall_boundary_registers(
                &memory,
                at_hypercall,
                VMCALL,
                HYPERCALL_ADDRESS,
            )
            .is_err()
        );
    }

    #[test]
    fn process_action_continuation_policy_is_exhaustive() {
        let fork = ProcessAction::Fork {
            child_pid: 2,
            child_stack: None,
            parent_tid: None,
            child_tid: None,
            clear_child_tid: None,
            clear_sighand: false,
            share_address_space: false,
        };
        let thread = ProcessAction::Thread {
            child_tid: 3,
            child_stack: 0x8000,
            parent_tid: None,
            child_tid_address: None,
            clear_child_tid: None,
            tls: None,
        };
        let exec = ProcessAction::Exec {
            executable_path: std::path::PathBuf::new(),
            executable_file: None,
            image: Vec::new(),
            argv: Vec::new(),
            envp: Vec::new(),
        };

        assert!(matches!(
            ProcessActionContinuation::from_captured(&fork, CompletedSyscallBoundary::for_test()),
            ProcessActionContinuation::Restore(_)
        ));
        assert!(matches!(
            ProcessActionContinuation::from_captured(&thread, CompletedSyscallBoundary::for_test()),
            ProcessActionContinuation::Restore(_)
        ));
        assert!(matches!(
            ProcessActionContinuation::from_captured(&exec, CompletedSyscallBoundary::for_test()),
            ProcessActionContinuation::Exec(_)
        ));
        assert!(fork.returns_to_original_image());
        assert!(thread.returns_to_original_image());
        assert!(!exec.returns_to_original_image());
    }

    #[test]
    fn completed_boundary_restores_exact_real_kvm_transport_state() {
        const ENTRY: u64 = 0x1000;
        const FRAME: u64 = 0x2000;
        let mut backend = match KvmBackend::new(0x10_000) {
            Ok(backend) => backend,
            Err(error) => {
                if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                    panic!("KVM is required: {error}");
                }
                eprintln!("skipping completed-boundary KVM test: {error}");
                return;
            }
        };
        backend
            .install_syscall(
                ENTRY,
                FRAME,
                SyscallRequest::new(libc::SYS_getpid as u64, [0; 6]),
            )
            .unwrap();

        let trampoline_hypercall = syscall_hypercall_address(
            backend.hypercall_instruction,
            backend.syscall_trampoline_address,
            backend.syscall_frame_address,
        );
        let trampoline_offset = trampoline_hypercall - backend.syscall_trampoline_address;
        backend.syscall_trampoline_address = ENTRY + 12 - trampoline_offset;

        let frame_address = match backend.vcpu.run().unwrap() {
            VcpuExit::Hypercall(exit) => {
                assert_eq!(exit.nr, VMCALL_SYSCALL_TRANSPORT);
                *exit.ret = 0;
                exit.args[0]
            }
            exit => panic!("expected syscall hypercall, got {exit:?}"),
        };
        let boundary = CompletedSyscallBoundary::capture(&backend, frame_address, None).unwrap();
        let expected = boundary.clone();

        backend
            .memory
            .write_raw(frame_address, &[0xa5; FRAME_SIZE])
            .unwrap();
        let mut poisoned_registers = backend.vcpu.get_regs().unwrap();
        poisoned_registers.r12 ^= 0x1234_5678;
        poisoned_registers.r13 ^= 0x8765_4321;
        backend.vcpu.set_regs(&poisoned_registers).unwrap();
        let mut poisoned_special = backend.vcpu.get_sregs().unwrap();
        poisoned_special.cr2 ^= 0x4000;
        backend.vcpu.set_sregs(&poisoned_special).unwrap();

        boundary.restore(&mut backend).unwrap();
        let mut restored_frame = [0; FRAME_SIZE];
        backend
            .memory
            .read_raw(frame_address, &mut restored_frame)
            .unwrap();
        assert_eq!(restored_frame, expected.frame);
        assert_eq!(backend.vcpu.get_regs().unwrap(), expected.registers);
        assert_eq!(
            backend.vcpu.get_sregs().unwrap(),
            expected.special_registers
        );
    }
    fn test_exec_action() -> ProcessAction {
        ProcessAction::Exec {
            executable_path: std::path::PathBuf::new(),
            executable_file: None,
            image: Vec::new(),
            argv: Vec::new(),
            envp: Vec::new(),
        }
    }

    #[test]
    fn synthetic_returning_exec_outcome_restores_boundary_and_stages_actual_result() {
        let Some((mut backend, _executor, boundary)) = backend_at_completed_tool_boundary() else {
            return;
        };
        let expected_registers = boundary.registers;
        let expected_special_registers = boundary.special_registers;
        let mut expected_frame = boundary.frame;
        let action = test_exec_action();
        let continuation = CompletedSyscallBoundary::capture_for_action(
            &backend,
            boundary.frame_address,
            None,
            &action,
        )
        .unwrap();
        let expected_result = -i64::from(libc::ENOTSUP);
        let result_offset = crate::syscall::RESULT_WORD * std::mem::size_of::<u64>();
        expected_frame[result_offset..result_offset + std::mem::size_of::<u64>()]
            .copy_from_slice(&(expected_result as u64).to_le_bytes());

        poison_test_boundary(&mut backend, boundary.frame_address);
        let result = continuation.finish(
            &mut backend,
            Ok(ProcessActionOutcome::returned(expected_result)),
        );
        let outcome = result.unwrap();

        assert_eq!(
            outcome,
            ProcessActionOutcome {
                image_replaced: false,
                syscall_result: expected_result,
            }
        );
        let mut restored_frame = [0; FRAME_SIZE];
        backend
            .memory
            .read_raw(backend.syscall_frame_address, &mut restored_frame)
            .unwrap();
        assert_eq!(restored_frame, expected_frame);
        assert_eq!(backend.vcpu.get_regs().unwrap(), expected_registers);

        assert_eq!(
            backend.vcpu.get_sregs().unwrap(),
            expected_special_registers
        );
    }

    #[test]
    fn synthetic_tool_injected_refused_exec_restores_outer_result_before_handler_return() {
        let Some((mut backend, _executor, boundary)) = backend_at_completed_tool_boundary() else {
            return;
        };
        let expected = boundary.clone();
        let action = test_exec_action();
        let continuation = ProcessActionContinuation::from_captured(&action, boundary);
        let expected_exec_result = -i64::from(libc::ENOTSUP);

        poison_test_boundary(&mut backend, expected.frame_address);
        let result = continuation.finish(
            &mut backend,
            Ok(ProcessActionOutcome::returned(expected_exec_result)),
        );
        let outcome = result.unwrap();
        assert_eq!(
            outcome,
            ProcessActionOutcome {
                image_replaced: false,
                syscall_result: expected_exec_result,
            },
            "the injected Exec result belongs to the Tool"
        );

        let mut restored_frame = [0; FRAME_SIZE];
        backend
            .memory
            .read_raw(expected.frame_address, &mut restored_frame)
            .unwrap();
        assert_eq!(restored_frame, expected.frame);
        assert_eq!(backend.vcpu.get_regs().unwrap(), expected.registers);
        assert_eq!(
            backend.vcpu.get_sregs().unwrap(),
            expected.special_registers
        );

        let handler_result = 73;
        expected
            .stage_action_result(&backend, handler_result)
            .unwrap();
        let mut returned_frame = [0; FRAME_SIZE];
        backend
            .memory
            .read_raw(expected.frame_address, &mut returned_frame)
            .unwrap();
        let result_offset = crate::syscall::RESULT_WORD * std::mem::size_of::<u64>();
        let mut expected_returned_frame = expected.frame;
        expected_returned_frame[result_offset..result_offset + std::mem::size_of::<u64>()]
            .copy_from_slice(&(handler_result as u64).to_le_bytes());
        assert_eq!(returned_frame, expected_returned_frame);
        assert_eq!(
            &returned_frame[result_offset..result_offset + std::mem::size_of::<u64>()],
            &(handler_result as u64).to_le_bytes(),
            "a later enclosing-return stage must overwrite the private injected Exec result"
        );
    }

    fn poison_test_boundary(backend: &mut KvmBackend, frame_address: u64) {
        backend
            .memory
            .write_raw(frame_address, &[0xa5; FRAME_SIZE])
            .unwrap();
        let mut registers = backend.vcpu.get_regs().unwrap();
        registers.rax ^= 0x7654;
        registers.rbx ^= 0x4321;
        backend.vcpu.set_regs(&registers).unwrap();
        let mut special = backend.vcpu.get_sregs().unwrap();
        special.fs.base ^= 0x1000;
        backend.vcpu.set_sregs(&special).unwrap();
    }

    #[test]
    fn synthetic_exec_replacement_and_error_do_not_restore_boundary() {
        for failed in [false, true] {
            let Some((mut backend, _executor, boundary)) = backend_at_completed_tool_boundary()
            else {
                return;
            };
            let action = test_exec_action();
            let frame_address = boundary.frame_address;
            let continuation = ProcessActionContinuation::from_captured(&action, boundary);
            poison_test_boundary(&mut backend, frame_address);
            let registers = backend.vcpu.get_regs().unwrap();
            let special = backend.vcpu.get_sregs().unwrap();
            let result = continuation.finish(
                &mut backend,
                if failed {
                    Err(Error::UnexpectedVcpuExit(
                        "synthetic action failure".to_owned(),
                    ))
                } else {
                    Ok(ProcessActionOutcome::replaced())
                },
            );
            if failed {
                assert!(
                    matches!(result, Err(Error::UnexpectedVcpuExit(message)) if message == "synthetic action failure")
                );
            } else {
                assert_eq!(result.unwrap(), ProcessActionOutcome::replaced());
            }
            let mut frame = [0; FRAME_SIZE];
            backend.memory.read_raw(frame_address, &mut frame).unwrap();
            assert_eq!(frame, [0xa5; FRAME_SIZE]);
            assert_eq!(backend.vcpu.get_regs().unwrap(), registers);
            assert_eq!(backend.vcpu.get_sregs().unwrap(), special);
        }
    }

    #[test]
    fn real_fork_and_thread_wrappers_restore_both_capture_modes() {
        for thread in [false, true] {
            for direct in [false, true] {
                for explicit_registers in [false, true] {
                    let Some((mut backend, mut executor, boundary)) =
                        backend_at_completed_tool_boundary()
                    else {
                        return;
                    };
                    backend.thread_ownership = ThreadOwnership::Tool;
                    let user = process_syscall_return_registers(
                        &backend.memory,
                        backend.vcpu.get_regs().unwrap(),
                        boundary.frame_address,
                        0,
                        None,
                    )
                    .unwrap();
                    let action = if thread {
                        ProcessAction::Thread {
                            child_tid: 2,
                            child_stack: user.rsp,
                            parent_tid: None,
                            child_tid_address: None,
                            clear_child_tid: None,
                            tls: None,
                        }
                    } else {
                        ProcessAction::Fork {
                            child_pid: 2,
                            child_stack: None,
                            parent_tid: None,
                            child_tid: None,
                            clear_child_tid: None,
                            clear_sighand: false,
                            share_address_space: false,
                        }
                    };
                    let returned_registers = explicit_registers.then_some(user);
                    let captured = CompletedSyscallBoundary::capture(
                        &backend,
                        boundary.frame_address,
                        returned_registers,
                    )
                    .unwrap();
                    let continuation = if direct {
                        CompletedSyscallBoundary::capture_for_action(
                            &backend,
                            boundary.frame_address,
                            returned_registers,
                            &action,
                        )
                        .unwrap()
                    } else {
                        ProcessActionContinuation::from_captured(&action, captured.clone())
                    };
                    let mut expected_frame = captured.frame;
                    let mut expected_registers = captured.registers;
                    if direct {
                        if explicit_registers {
                            for (word, value) in [
                                (1, user.rdi),
                                (2, user.rsi),
                                (3, user.rdx),
                                (4, user.r10),
                                (5, user.r8),
                                (6, user.r9),
                                (7, 2),
                                (8, user.rip),
                                (9, user.rflags),
                                (10, user.rbx),
                            ] {
                                expected_frame[word * 8..word * 8 + 8]
                                    .copy_from_slice(&value.to_le_bytes());
                            }
                            expected_registers.rbp = user.rbp;
                            expected_registers.rsp = user.rsp;
                            expected_registers.r12 = user.r12;
                            expected_registers.r13 = user.r13;
                            expected_registers.r14 = user.r14;
                            expected_registers.r15 = user.r15;
                        } else {
                            expected_frame[56..64].copy_from_slice(&2_u64.to_le_bytes());
                        }
                    }
                    let starts = Arc::new(Mutex::new(Vec::new()));
                    let context = ToolContext::<crate::StraceTool> {
                        pid: Pid::from_raw(1),
                        tid: Pid::from_raw(1),
                        thread_state: &(),
                        global_state: Some(Arc::new(crate::StraceLog::default())),
                        config: (),
                        subscriptions: reverie::Subscription::none(),
                        pending_child_starts: starts.clone(),
                    };
                    let result = futures::executor::block_on(
                        backend.run_process_action_with_tool_at_boundary(
                            &mut executor,
                            action,
                            context,
                            continuation,
                        ),
                    );
                    let start_count = starts.lock().unwrap().len();
                    let child_present = if thread {
                        backend
                            .thread_group
                            .worker_handles
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|worker| worker.tid == 2)
                    } else {
                        executor.has_pending_child_process(2)
                    };
                    let cleanup = backend.discard_unstarted_tool_children(&mut executor, &starts);
                    assert_eq!(result.unwrap(), ProcessActionOutcome::returned(2));
                    cleanup.unwrap();
                    assert_eq!(start_count, 1);
                    assert!(child_present);
                    assert!(starts.lock().unwrap().is_empty());
                    assert!(!executor.has_pending_child_process(2));
                    assert!(
                        backend
                            .thread_group
                            .worker_handles
                            .lock()
                            .unwrap()
                            .is_empty()
                    );
                    let mut frame = [0; FRAME_SIZE];
                    backend
                        .memory
                        .read_raw(captured.frame_address, &mut frame)
                        .unwrap();
                    assert_eq!(frame, expected_frame);
                    assert_eq!(backend.vcpu.get_regs().unwrap(), expected_registers);
                    assert_eq!(
                        backend.vcpu.get_sregs().unwrap(),
                        captured.special_registers
                    );
                    if !direct {
                        captured.stage_action_result(&backend, 73).unwrap();
                        if explicit_registers {
                            for (word, value) in [
                                (1, user.rdi),
                                (2, user.rsi),
                                (3, user.rdx),
                                (4, user.r10),
                                (5, user.r8),
                                (6, user.r9),
                                (7, 73),
                                (8, user.rip),
                                (9, user.rflags),
                                (10, user.rbx),
                            ] {
                                expected_frame[word * 8..word * 8 + 8]
                                    .copy_from_slice(&value.to_le_bytes());
                            }
                        } else {
                            expected_frame[56..64].copy_from_slice(&73_u64.to_le_bytes());
                        }
                        backend
                            .memory
                            .read_raw(captured.frame_address, &mut frame)
                            .unwrap();
                        assert_eq!(frame, expected_frame);
                    }
                }
            }
        }
    }

    fn test_executor() -> ElfExecutor {
        ElfExecutor::new(
            crate::executor::test_loaded_state_for_vm(
                &std::env::current_dir().expect("test current directory"),
            ),
            false,
        )
    }

    #[derive(Default)]
    struct FatalProcessMemoryLog {
        memory: Mutex<Option<GuestMemory>>,
        observations: Mutex<Vec<(i32, Vec<u8>)>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for FatalProcessMemoryLog {
        type Request = i32;
        type Response = ();
        type Config = ();

        async fn receive_rpc(&self, _from: Pid, status: i32) {
            let mut bytes = vec![0; 4096];
            self.memory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .read(0x80_0000, &mut bytes)
                .unwrap();
            self.observations.lock().unwrap().push((status, bytes));
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct FatalProcessMemoryTool;

    #[reverie::tool]
    impl Tool for FatalProcessMemoryTool {
        type GlobalState = FatalProcessMemoryLog;
        type ThreadState = ();

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: Pid,
            global: &G,
            _state: (),
            status: ExitStatus,
        ) -> std::result::Result<(), reverie::Error> {
            global.send_rpc(status.into_raw()).await;
            Ok(())
        }
    }

    fn check_fatal_page_zero_registration(with_tool: bool, writable: bool) {
        let mut parent = match KvmBackend::new(16 * 1024 * 1024) {
            Ok(backend) => backend,
            Err(error) => {
                assert!(
                    std::env::var_os("REVERIE_REQUIRE_KVM").is_none(),
                    "KVM is required: {error}"
                );
                eprintln!("skipping fatal-process KVM test: {error}");
                return;
            }
        };
        let mut code = vec![0xb8];
        code.extend_from_slice(&(libc::SYS_getpid as u32).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x31, 0xc0, 0x48, 0x8b, 0x00, 0x0f, 0x0b]);
        parent
            .install_static_elf(&minimal_test_elf(&code), "/bin/fatal-process-memory")
            .unwrap();
        let mut executor = ElfExecutor::new(parent.static_elf.take().unwrap(), true);
        match parent.vcpu.run().unwrap() {
            VcpuExit::Hypercall(exit) => {
                assert_eq!(exit.nr, VMCALL_SYSCALL_TRANSPORT);
                *exit.ret = 0;
            }
            other => panic!("unexpected initial boundary: {other:?}"),
        }
        parent
            .memory
            .map_user_permissions(0x80_0000, 4096, true, writable)
            .unwrap();
        parent.memory.write(0x80_0000, &[0x5a; 4096]).unwrap();
        let mut child = parent
            .prepare_forked_process(
                &executor, 2, None, None, None, None, false, false, true, None,
            )
            .unwrap();
        assert_eq!(
            child.executor.execute(
                &SyscallRequest::new(libc::SYS_set_tid_address as u64, [0x80_0000, 0, 0, 0, 0, 0]),
                &child.backend.memory
            ),
            2
        );
        let memory = child.backend.memory.clone();
        let log = Arc::new(FatalProcessMemoryLog::default());
        *log.memory.lock().unwrap() = Some(memory.clone());
        let (status, stdout, stderr) = if with_tool {
            futures::executor::block_on(child.backend.run_static_elf_process_with_tool(
                &mut child.executor,
                Pid::from_raw(2),
                Pid::from_raw(2),
                FatalProcessMemoryTool,
                (),
                log.clone(),
                &(),
                &reverie::Subscription::none(),
                false,
            ))
            .unwrap()
        } else {
            child
                .backend
                .run_static_elf_process(&mut child.executor)
                .unwrap()
        };
        let mut before_wrapper = [0; 4096];
        memory.read(0x80_0000, &mut before_wrapper).unwrap();
        parent
            .finish_forked_process(&mut executor, child, status, stdout.clone(), stderr.clone())
            .unwrap();
        let mut after_wrapper = [0; 4096];
        memory.read(0x80_0000, &mut after_wrapper).unwrap();
        assert_eq!(status.into_raw(), libc::SIGSEGV | 0x80);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        if with_tool {
            assert_eq!(
                *log.observations.lock().unwrap(),
                vec![(libc::SIGSEGV | 0x80, vec![0x5a; 4096])]
            );
        } else {
            assert!(log.observations.lock().unwrap().is_empty());
        }
        assert_eq!(before_wrapper, [0x5a; 4096]);
        assert_eq!(after_wrapper, [0x5a; 4096]);
        parent
            .memory
            .map_user_permissions(0x80_0000, 4096, true, true)
            .unwrap();
        let request = SyscallRequest::new(
            libc::SYS_wait4 as u64,
            [2, 0x80_0000, libc::WNOHANG as u64, 0, 0, 0],
        );
        assert_eq!(executor.execute(&request, &parent.memory), 2);
        let mut wait_status = [0; 4];
        parent.memory.read(0x80_0000, &mut wait_status).unwrap();
        assert_eq!(i32::from_ne_bytes(wait_status), libc::SIGSEGV | 0x80);
    }

    #[test]
    fn fatal_page_zero_registration_direct_ro() {
        check_fatal_page_zero_registration(false, false);
    }

    #[test]
    fn fatal_page_zero_registration_direct_rw() {
        check_fatal_page_zero_registration(false, true);
    }

    #[test]
    fn fatal_page_zero_registration_tool_ro() {
        check_fatal_page_zero_registration(true, false);
    }

    #[test]
    fn fatal_page_zero_registration_tool_rw() {
        check_fatal_page_zero_registration(true, true);
    }

    #[test]
    fn page_fault_action_restores_complete_stopped_context() {
        for thread in [false, true] {
            for missing_global in [false, true] {
                let mut backend = match KvmBackend::new(16 * 1024 * 1024) {
                    Ok(backend) => backend,
                    Err(error) => {
                        assert!(
                            std::env::var_os("REVERIE_REQUIRE_KVM").is_none(),
                            "KVM is required: {error}"
                        );
                        eprintln!("skipping page-fault boundary KVM test: {error}");
                        return;
                    }
                };
                backend
                    .install_static_elf(
                        &minimal_test_elf(&[0x31, 0xc0, 0x48, 0x8b, 0x00]),
                        "/page-fault-boundary",
                    )
                    .unwrap();
                let mut executor = ElfExecutor::new(backend.static_elf.take().unwrap(), false);
                backend.thread_ownership = ThreadOwnership::Tool;
                let mut registers = backend.vcpu.get_regs().unwrap();
                registers.rcx = 0x7777;
                registers.r11 = 0x8888;
                backend.vcpu.set_regs(&registers).unwrap();
                assert!(matches!(backend.vcpu.run().unwrap(), VcpuExit::Hlt));
                let fault = backend.capture_page_zero_fault(&executor).unwrap().unwrap();
                let mut frame_before = [0; PAGE_SIZE as usize];
                let mut exception_before = [0; PAGE_SIZE as usize];
                backend
                    .memory
                    .read_raw(backend.syscall_frame_address, &mut frame_before)
                    .unwrap();
                let exception_page = fault.halted_registers.rsp & !(PAGE_SIZE - 1);
                backend
                    .memory
                    .read_raw(exception_page, &mut exception_before)
                    .unwrap();
                let action = if thread {
                    ProcessAction::Thread {
                        child_tid: 2,
                        child_stack: fault.registers.rsp - 4096,
                        parent_tid: None,
                        child_tid_address: None,
                        clear_child_tid: None,
                        tls: None,
                    }
                } else {
                    ProcessAction::Fork {
                        child_pid: 2,
                        child_stack: None,
                        parent_tid: None,
                        child_tid: None,
                        clear_child_tid: None,
                        clear_sighand: false,
                        share_address_space: false,
                    }
                };
                let starts = Arc::new(Mutex::new(Vec::new()));
                let context = ToolContext::<crate::StraceTool> {
                    pid: Pid::from_raw(1),
                    tid: Pid::from_raw(1),
                    thread_state: &(),
                    global_state: (!missing_global).then(|| Arc::new(crate::StraceLog::default())),
                    config: (),
                    subscriptions: reverie::Subscription::none(),
                    pending_child_starts: starts.clone(),
                };
                let result =
                    futures::executor::block_on(backend.run_process_action_with_tool_from_fault(
                        &mut executor,
                        action,
                        context,
                        &fault,
                    ));
                let child_count = starts.lock().unwrap().len();
                let cleanup = backend.discard_unstarted_tool_children(&mut executor, &starts);
                if missing_global {
                    assert!(result.is_err());
                    assert_eq!(child_count, 0);
                } else {
                    assert_eq!(result.unwrap(), ProcessActionOutcome::returned(2));
                    assert_eq!(child_count, 1);
                }
                cleanup.unwrap();
                assert!(starts.lock().unwrap().is_empty());
                assert!(!executor.has_pending_child_process(2));
                assert!(
                    backend
                        .thread_group
                        .worker_handles
                        .lock()
                        .unwrap()
                        .is_empty()
                );
                let mut frame_after = [0; PAGE_SIZE as usize];
                let mut exception_after = [0; PAGE_SIZE as usize];
                backend
                    .memory
                    .read_raw(backend.syscall_frame_address, &mut frame_after)
                    .unwrap();
                backend
                    .memory
                    .read_raw(exception_page, &mut exception_after)
                    .unwrap();
                assert_eq!(frame_after, frame_before);
                assert_eq!(exception_after, exception_before);
                assert_eq!(backend.vcpu.get_regs().unwrap(), fault.halted_registers);
                assert_eq!(backend.vcpu.get_sregs().unwrap(), fault.special_registers);
                assert_eq!(backend.vcpu.get_xsave().unwrap().region, fault.xsave.region);
            }
        }
    }

    fn check_fatal_process_registration_preserves_memory(with_tool: bool, writable: bool) {
        let mut parent = match KvmBackend::new(16 * 1024 * 1024) {
            Ok(backend) => backend,
            Err(error) => {
                assert!(
                    std::env::var_os("REVERIE_REQUIRE_KVM").is_none(),
                    "KVM is required: {error}"
                );
                eprintln!("skipping fatal-process KVM test: {error}");
                return;
            }
        };
        let mut code = vec![0xb8];
        code.extend_from_slice(&(libc::SYS_getpid as u32).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x31, 0xe4, 0xb8]);
        code.extend_from_slice(&(libc::SYS_rt_sigreturn as u32).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x0f, 0x0b]);
        parent
            .install_static_elf(&minimal_test_elf(&code), "/bin/fatal-process-memory")
            .unwrap();
        let mut executor = ElfExecutor::new(parent.static_elf.take().unwrap(), true);
        match parent.vcpu.run().unwrap() {
            VcpuExit::Hypercall(exit) => {
                assert_eq!(exit.nr, VMCALL_SYSCALL_TRANSPORT);
                *exit.ret = 0;
            }
            other => panic!("unexpected initial boundary: {other:?}"),
        }
        parent
            .memory
            .map_user_permissions(0x80_0000, 4096, true, writable)
            .unwrap();
        parent.memory.write(0x80_0000, &[0x5a; 4096]).unwrap();
        let mut child = parent
            .prepare_forked_process(
                &executor, 2, None, None, None, None, false, false, true, None,
            )
            .unwrap();
        assert_eq!(
            child.executor.execute(
                &SyscallRequest::new(libc::SYS_set_tid_address as u64, [0x80_0000, 0, 0, 0, 0, 0]),
                &child.backend.memory
            ),
            2
        );
        let memory = child.backend.memory.clone();
        let log = Arc::new(FatalProcessMemoryLog::default());
        *log.memory.lock().unwrap() = Some(memory.clone());
        let (status, stdout, stderr) = if with_tool {
            futures::executor::block_on(child.backend.run_static_elf_process_with_tool(
                &mut child.executor,
                Pid::from_raw(2),
                Pid::from_raw(2),
                FatalProcessMemoryTool,
                (),
                log.clone(),
                &(),
                &reverie::Subscription::none(),
                false,
            ))
            .unwrap()
        } else {
            child
                .backend
                .run_static_elf_process(&mut child.executor)
                .unwrap()
        };
        let mut before_wrapper = [0; 4096];
        memory.read(0x80_0000, &mut before_wrapper).unwrap();
        parent
            .finish_forked_process(&mut executor, child, status, stdout.clone(), stderr.clone())
            .unwrap();
        let mut after_wrapper = [0; 4096];
        memory.read(0x80_0000, &mut after_wrapper).unwrap();
        assert_eq!(status.into_raw(), libc::SIGSEGV | 0x80);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        if with_tool {
            assert_eq!(
                *log.observations.lock().unwrap(),
                vec![(libc::SIGSEGV | 0x80, vec![0x5a; 4096])]
            );
        } else {
            assert!(log.observations.lock().unwrap().is_empty());
        }
        assert_eq!(before_wrapper, [0x5a; 4096]);
        assert_eq!(after_wrapper, [0x5a; 4096]);
        parent
            .memory
            .map_user_permissions(0x80_0000, 4096, true, true)
            .unwrap();
        let request = SyscallRequest::new(
            libc::SYS_wait4 as u64,
            [2, 0x80_0000, libc::WNOHANG as u64, 0, 0, 0],
        );
        assert_eq!(executor.execute(&request, &parent.memory), 2);
        let mut wait_status = [0; 4];
        parent.memory.read(0x80_0000, &mut wait_status).unwrap();
        assert_eq!(i32::from_ne_bytes(wait_status), libc::SIGSEGV | 0x80);
    }

    #[test]
    fn fatal_process_registration_preserves_rw_memory_with_tool() {
        check_fatal_process_registration_preserves_memory(true, true);
    }

    #[test]
    fn fatal_process_registration_preserves_ro_memory_with_tool() {
        check_fatal_process_registration_preserves_memory(true, false);
    }

    #[test]
    fn fatal_process_registration_preserves_rw_memory_direct() {
        check_fatal_process_registration_preserves_memory(false, true);
    }

    #[test]
    fn fatal_process_registration_preserves_ro_memory_direct() {
        check_fatal_process_registration_preserves_memory(false, false);
    }

    #[derive(Default)]
    struct FatalWorkerMemoryLog {
        memory: Mutex<Option<GuestMemory>>,
        group: Mutex<Option<Arc<GuestThreadGroup>>>,
        address: Mutex<u64>,
        observations: Mutex<Vec<(i32, usize, Vec<u8>)>>,
        start_ready: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        start_release: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        wake_observed: Mutex<Option<std::sync::mpsc::Receiver<Option<usize>>>>,
        wake_slots: Mutex<Vec<Option<usize>>>,
    }

    #[reverie::global_tool]
    impl GlobalTool for FatalWorkerMemoryLog {
        type Request = i32;
        type Response = ();
        type Config = bool;

        async fn receive_rpc(&self, _from: Pid, status: i32) {
            if status == -1 {
                if let Some(sender) = self.start_ready.lock().unwrap().take() {
                    sender.send(()).unwrap();
                    self.start_release
                        .lock()
                        .unwrap()
                        .take()
                        .unwrap()
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap();
                }
                return;
            }
            if let Some(receiver) = self.wake_observed.lock().unwrap().take() {
                self.wake_slots.lock().unwrap().push(
                    receiver
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap_or(None),
                );
            }
            let group = self.group.lock().unwrap().as_ref().unwrap().clone();
            let slot = group.reserve_transport_slot(3).unwrap();
            group.release_transport_slot(slot);
            let mut memory = self.memory.lock().unwrap();
            let memory = memory.as_mut().unwrap();
            let mut bytes = vec![0; 8192];
            memory.read_raw(0x80_0000, &mut bytes).unwrap();
            self.observations
                .lock()
                .unwrap()
                .push((status, slot, bytes));
            memory
                .write_raw(
                    *self.address.lock().unwrap(),
                    &0x6b6b_6b6b_i32.to_ne_bytes(),
                )
                .unwrap();
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct FatalWorkerMemoryTool {
        fail_callback: bool,
    }

    #[reverie::tool]
    impl Tool for FatalWorkerMemoryTool {
        type GlobalState = FatalWorkerMemoryLog;
        type ThreadState = ();

        fn new(_pid: Pid, fail_callback: &bool) -> Self {
            Self {
                fail_callback: *fail_callback,
            }
        }

        async fn handle_thread_start<G: reverie::Guest<Self>>(
            &self,
            guest: &mut G,
        ) -> std::result::Result<(), reverie::Error> {
            guest.send_rpc(-1).await;
            Ok(())
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            _tid: Pid,
            global: &G,
            _state: (),
            status: ExitStatus,
        ) -> std::result::Result<(), reverie::Error> {
            global.send_rpc(status.into_raw()).await;
            if self.fail_callback {
                return Err(reverie::syscalls::Errno::EIO.into());
            }
            Ok(())
        }
    }

    fn check_fatal_worker_memory(mode: u8, fail_callback: bool, cancel_before_start: bool) {
        let Some((mut parent, mut executor, boundary)) = backend_at_completed_tool_boundary()
        else {
            return;
        };
        let mut code = vec![0x31, 0xe4, 0xb8];
        code.extend_from_slice(&(libc::SYS_rt_sigreturn as u32).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, 0x0f, 0x0b]);
        let registers = process_syscall_return_registers(
            &parent.memory,
            parent.vcpu.get_regs().unwrap(),
            boundary.frame_address,
            0,
            None,
        )
        .unwrap();
        parent.memory.write_raw(registers.rip, &code).unwrap();
        parent
            .memory
            .map_user_permissions(0x80_0000, 8192, true, true)
            .unwrap();
        parent.memory.write(0x80_0000, &[0x5a; 8192]).unwrap();
        let offset = if mode == 3 { 4094 } else { 64 };
        let address = 0x80_0000 + offset;
        match mode {
            0 => {}
            1 => parent
                .memory
                .map_user_permissions(0x80_0000, 4096, true, false)
                .unwrap(),
            2 => parent
                .memory
                .map_user_permissions(0x80_0000, 4096, false, false)
                .unwrap(),
            3 => parent
                .memory
                .map_user_permissions(0x80_1000, 4096, true, false)
                .unwrap(),
            _ => unreachable!(),
        }
        let log = Arc::new(FatalWorkerMemoryLog::default());
        *log.memory.lock().unwrap() = Some(parent.memory.clone());
        *log.group.lock().unwrap() = Some(parent.thread_group.clone());
        *log.address.lock().unwrap() = address;
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        if cancel_before_start {
            *log.start_ready.lock().unwrap() = Some(started_sender);
            *log.start_release.lock().unwrap() = Some(release_receiver);
        }
        let starts = Arc::new(Mutex::new(Vec::new()));
        parent.thread_ownership = ThreadOwnership::Tool;
        let context = ToolContext::<FatalWorkerMemoryTool> {
            pid: Pid::from_raw(1),
            tid: Pid::from_raw(1),
            thread_state: &(),
            global_state: Some(log.clone()),
            config: fail_callback,
            subscriptions: reverie::Subscription::none(),
            pending_child_starts: starts.clone(),
        };
        let action = ProcessAction::Thread {
            child_tid: 2,
            child_stack: registers.rsp,
            parent_tid: None,
            child_tid_address: None,
            clear_child_tid: Some(address),
            tls: None,
        };
        let outcome = futures::executor::block_on(parent.run_process_action_with_tool_at_boundary(
            &mut executor,
            action,
            context,
            ProcessActionContinuation::Restore(Box::new(boundary)),
        ))
        .unwrap();
        assert_eq!(outcome, ProcessActionOutcome::returned(2));
        let waiter = if mode < 2 {
            let memory = parent.memory.clone();
            let group = parent.thread_group.clone();
            let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
            let (wake_sender, wake_receiver) = std::sync::mpsc::channel();
            *log.wake_observed.lock().unwrap() = Some(wake_receiver);
            let waiter = std::thread::spawn(move || {
                let host = memory.host_address() + address - memory.guest_base();
                let bound = libc::timespec {
                    tv_sec: 1,
                    tv_nsec: 0,
                };
                ready_sender.send(()).unwrap();
                let result = unsafe {
                    libc::syscall(
                        libc::SYS_futex,
                        host,
                        libc::FUTEX_WAIT,
                        0x5a5a_5a5a_i32,
                        &bound,
                        0,
                        0,
                    )
                };
                let error = if result < 0 {
                    std::io::Error::last_os_error().raw_os_error()
                } else {
                    None
                };
                let slot = if result == 0 {
                    let slot = group.reserve_transport_slot(4).unwrap();
                    group.release_transport_slot(slot);
                    Some(slot)
                } else {
                    None
                };
                wake_sender.send(slot).unwrap();
                (result, error)
            });
            ready_receiver
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            Some(waiter)
        } else {
            None
        };
        crate::runtime::start_pending_children(&starts).unwrap();
        if cancel_before_start {
            started_receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            parent.thread_group.cancel_workers();
            release_sender.send(()).unwrap();
        }
        parent.thread_group.join_workers();
        let wake = waiter.map(|waiter| waiter.join().unwrap());
        let observations = log.observations.lock().unwrap().clone();
        let mut expected = vec![0x5a; 8192];
        if mode == 0 {
            expected[offset as usize..offset as usize + 4].fill(0);
        }
        let expected_status = if cancel_before_start {
            0
        } else {
            libc::SIGSEGV | 0x80
        };
        assert_eq!(observations, vec![(expected_status, 0, expected.clone())]);
        expected[offset as usize..offset as usize + 4].fill(0x6b);
        let mut actual = vec![0; 8192];
        parent.memory.read_raw(0x80_0000, &mut actual).unwrap();
        assert_eq!(actual, expected);
        if mode < 2 {
            assert_eq!(wake, Some((0, None)));
            assert_eq!(*log.wake_slots.lock().unwrap(), vec![Some(0)]);
        }
    }

    #[test]
    fn fatal_worker_rw_store_wake_and_slot_release() {
        check_fatal_worker_memory(0, false, false);
    }
    #[test]
    fn fatal_worker_ro_failed_store_still_wakes() {
        check_fatal_worker_memory(1, false, false);
    }
    #[test]
    fn fatal_worker_none_preserves_full_memory() {
        check_fatal_worker_memory(2, false, false);
    }
    #[test]
    fn fatal_worker_crossing_scalar_preserves_full_memory() {
        check_fatal_worker_memory(3, false, false);
    }
    #[test]
    fn fatal_worker_callback_error_prevents_second_store() {
        check_fatal_worker_memory(0, true, false);
    }
    #[test]
    fn fatal_worker_cancelled_start_preserves_clear_order() {
        check_fatal_worker_memory(0, false, true);
    }

    #[test]
    fn failed_boundary_finalization_cancels_and_joins_each_typed_child() {
        let mut backend = match KvmBackend::new(0x10_000) {
            Ok(backend) => backend,
            Err(error) => {
                if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                    panic!("KVM is required: {error}");
                }
                eprintln!("skipping boundary-cleanup KVM test: {error}");
                return;
            }
        };
        let mut executor = test_executor();
        let starts = Arc::new(Mutex::new(Vec::new()));

        let fork_cancelled = Arc::new(AtomicBool::new(false));
        let fork_cancelled_in_child = fork_cancelled.clone();
        let (fork_sender, fork_receiver) = std::sync::mpsc::channel();
        let fork_gate = ChildStartGate::new(fork_sender);
        let fork_completion = Arc::new(Mutex::new(None));
        let fork_handle = std::thread::spawn(move || match fork_receiver.recv() {
            Ok(ChildStartCommand::Cancel) => {
                fork_cancelled_in_child.store(true, Ordering::Release);
                Ok(())
            }
            other => Err(Error::UnexpectedVcpuExit(format!(
                "unstarted test process received {other:?}"
            ))),
        });
        executor.register_child_process_with_gate(
            41,
            fork_gate.clone(),
            fork_completion,
            fork_handle,
        );
        starts
            .lock()
            .unwrap()
            .push(PendingChildStart::fork_process(41, fork_gate));

        let thread_cancelled = Arc::new(AtomicBool::new(false));
        let thread_cancelled_in_child = thread_cancelled.clone();
        let (thread_sender, thread_receiver) = std::sync::mpsc::channel();
        let thread_gate = ChildStartGate::new(thread_sender);
        let thread_handle = std::thread::spawn(move || {
            match thread_receiver.recv() {
                Ok(ChildStartCommand::Cancel) => {
                    thread_cancelled_in_child.store(true, Ordering::Release);
                }
                other => panic!("unstarted test thread received {other:?}"),
            }
            Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
        });
        backend
            .thread_group
            .add_unstarted_worker(42, thread_gate.clone(), thread_handle);
        starts
            .lock()
            .unwrap()
            .push(PendingChildStart::tool_thread(42, thread_gate));

        let mut boundary = CompletedSyscallBoundary::for_test();
        boundary.frame_address = backend.memory.guest_end();
        let error = backend
            .finish_tool_process_action_at_boundary(
                &mut executor,
                &starts,
                ProcessActionContinuation::Restore(Box::new(boundary)),
                Ok(ProcessActionOutcome::returned(42)),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidGuestAddress { address, length, .. }
                if address == backend.memory.guest_end() && length == FRAME_SIZE
        ));
        assert!(fork_cancelled.load(Ordering::Acquire));
        assert!(thread_cancelled.load(Ordering::Acquire));
        assert!(starts.lock().unwrap().is_empty());
        assert!(!executor.discard_unstarted_child_process(41).unwrap());
        assert!(!backend.thread_group.discard_unstarted_worker(42).unwrap());
    }

    fn minimal_test_elf(code: &[u8]) -> Vec<u8> {
        const LOAD_ADDRESS: u64 = 0x20_0000;
        const CODE_OFFSET: usize = 0x1000;
        let mut image = vec![0; CODE_OFFSET + code.len()];
        let put_u16 = |image: &mut [u8], offset: usize, value: u16| {
            image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        };
        let put_u32 = |image: &mut [u8], offset: usize, value: u32| {
            image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        let put_u64 = |image: &mut [u8], offset: usize, value: u64| {
            image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        };

        image[..4].copy_from_slice(b"\x7fELF");
        image[4] = 2;
        image[5] = 1;
        image[6] = 1;
        put_u16(&mut image, 16, 2);
        put_u16(&mut image, 18, 62);
        put_u32(&mut image, 20, 1);
        put_u64(&mut image, 24, LOAD_ADDRESS);
        put_u64(&mut image, 32, 64);
        put_u16(&mut image, 52, 64);
        put_u16(&mut image, 54, 56);
        put_u16(&mut image, 56, 1);
        put_u32(&mut image, 64, 1);
        put_u32(&mut image, 68, 5);
        put_u64(&mut image, 72, CODE_OFFSET as u64);
        put_u64(&mut image, 80, LOAD_ADDRESS);
        put_u64(&mut image, 88, LOAD_ADDRESS);
        put_u64(&mut image, 96, code.len() as u64);
        put_u64(&mut image, 104, 0x2000);
        put_u64(&mut image, 112, 0x1000);
        image[CODE_OFFSET..].copy_from_slice(code);
        image
    }

    fn backend_at_completed_tool_boundary()
    -> Option<(KvmBackend, ElfExecutor, CompletedSyscallBoundary)> {
        const MEMORY_SIZE: usize = 16 * 1024 * 1024;
        let mut backend = match KvmBackend::new(MEMORY_SIZE) {
            Ok(backend) => backend,
            Err(error) => {
                if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                    panic!("KVM is required: {error}");
                }
                eprintln!("skipping production-boundary cleanup KVM test: {error}");
                return None;
            }
        };
        let mut code = vec![0xb8]; // mov eax, SYS_getpid
        code.extend_from_slice(&(libc::SYS_getpid as u32).to_le_bytes());
        code.extend_from_slice(&[0x0f, 0x05, HLT]); // syscall; hlt
        backend
            .install_static_elf(&minimal_test_elf(&code), "/bin/boundary-cleanup-test")
            .unwrap();
        let mut executor = ElfExecutor::new(backend.static_elf.take().unwrap(), false);
        let frame_address = match backend.vcpu.run().unwrap() {
            VcpuExit::Hypercall(exit) => {
                assert_eq!(exit.nr, VMCALL_SYSCALL_TRANSPORT);
                *exit.ret = 0;
                exit.args[0]
            }
            exit => panic!("expected syscall hypercall, got {exit:?}"),
        };
        let boundary = CompletedSyscallBoundary::capture(&backend, frame_address, None).unwrap();
        // Keep the executor mutable in the return type to emphasize that the
        // wrapper owns both action registration and its rollback.
        executor.set_current_user_stack_pointer(boundary.registers.rsp);
        Some((backend, executor, boundary))
    }

    #[test]
    fn page_zero_translation_preserves_neighbor_and_upper_pages() {
        let Some((backend, _executor, _boundary)) = backend_at_completed_tool_boundary() else {
            return;
        };
        for address in (PAGE_SIZE..backend.memory.guest_end()).step_by(PAGE_SIZE as usize) {
            let translated = backend.vcpu.translate_gva(address).unwrap();
            assert_eq!(translated.valid, 1, "address {address:#x}");
            assert_eq!(translated.physical_address, address);
        }
        for address in [0, 1, PAGE_SIZE - 1] {
            assert_eq!(
                backend.vcpu.translate_gva(address).unwrap().valid,
                0,
                "address {address:#x}"
            );
        }
    }

    fn check_page_zero_table_lifetime(mode: &str) {
        let Some((mut parent, mut executor, _boundary)) = backend_at_completed_tool_boundary()
        else {
            return;
        };
        let mut before = [0_u8; PAGE_SIZE as usize];
        parent.memory.read_raw(0, &mut before).unwrap();
        match mode {
            "fork" => {
                let child =
                    KvmBackend::from_process_snapshot(parent.snapshot_process().unwrap()).unwrap();
                let mut after = [0_u8; PAGE_SIZE as usize];
                parent.memory.read_raw(0, &mut after).unwrap();
                assert_eq!(after, before);
                assert_eq!(child.vcpu.translate_gva(0).unwrap().valid, 0);
                assert_eq!(
                    child
                        .vcpu
                        .translate_gva(PAGE_SIZE)
                        .unwrap()
                        .physical_address,
                    PAGE_SIZE
                );
            }
            "thread" => {
                let child = KvmBackend::from_thread_state(
                    parent.memory.clone(),
                    parent.vcpu.get_regs().unwrap(),
                    parent.vcpu.get_xsave().unwrap(),
                    None,
                    parent.cpuid_policy,
                    2001,
                    parent.thread_group.clone(),
                )
                .unwrap();
                let mut after = [0_u8; PAGE_SIZE as usize];
                child.memory.read_raw(0, &mut after).unwrap();
                assert_eq!(after, before);
                assert_eq!(child.vcpu.translate_gva(0).unwrap().valid, 0);
                assert_eq!(
                    child
                        .vcpu
                        .translate_gva(child.syscall_frame_address)
                        .unwrap()
                        .physical_address,
                    child.syscall_frame_address
                );
            }
            "exec" => {
                parent
                    .exec_process(
                        &mut executor,
                        (Path::new("/page-zero-exec"), None),
                        &minimal_test_elf(&[HLT]),
                        &["/page-zero-exec".to_owned()],
                        &[],
                    )
                    .unwrap();
                assert_eq!(parent.vcpu.translate_gva(0).unwrap().valid, 0);
                assert_eq!(
                    parent
                        .vcpu
                        .translate_gva(parent.tool_stack_top() - PAGE_SIZE)
                        .unwrap()
                        .physical_address,
                    parent.tool_stack_top() - PAGE_SIZE
                );
            }
            _ => panic!("unknown page-zero lifetime mode"),
        }
    }

    #[test]
    fn page_zero_table_fork_lifetime() {
        check_page_zero_table_lifetime("fork");
    }

    #[test]
    fn page_zero_table_thread_lifetime() {
        check_page_zero_table_lifetime("thread");
    }

    #[test]
    fn page_zero_table_exec_lifetime() {
        check_page_zero_table_lifetime("exec");
    }

    #[test]
    fn started_tool_worker_observes_cancellation_after_start_lifecycle() {
        let Some((mut backend, root_executor, boundary)) = backend_at_completed_tool_boundary()
        else {
            return;
        };
        backend.is_guest_thread = true;
        let mut executor = root_executor.thread_child(2).unwrap();

        backend.thread_ownership = ThreadOwnership::Tool;
        let thread_group = backend.thread_group.clone();
        let global_state = Arc::new(CancellationLifecycleLog::default());
        let observed_state = global_state.clone();
        let clear_tid = boundary.registers.rsp - 16;
        backend
            .memory
            .write(clear_tid, &123_i32.to_le_bytes())
            .unwrap();
        executor.set_clear_child_tid(Some(clear_tid));
        *global_state.clear_tid.lock().unwrap() = Some((backend.memory.clone(), clear_tid));
        let registers = backend.vcpu.get_regs().unwrap();
        let (start_sender, start_receiver) = std::sync::mpsc::channel();
        let start_gate = ChildStartGate::new(start_sender);
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (continue_sender, continue_receiver) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            assert_eq!(start_receiver.recv().unwrap(), ChildStartCommand::Start);
            started_sender.send(()).unwrap();
            continue_receiver.recv().unwrap();
            let config = ();
            let subscriptions = reverie::Subscription::none();
            let (status, stdout, stderr) =
                futures::executor::block_on(backend.run_static_elf_process_with_tool(
                    &mut executor,
                    Pid::from_raw(1),
                    Pid::from_raw(2),
                    CancellationLifecycleTool,
                    (),
                    global_state,
                    &config,
                    &subscriptions,
                    false,
                ))
                .unwrap();
            assert_eq!(status, ExitStatus::SUCCESS);
            assert!(stdout.is_empty());
            assert!(stderr.is_empty());
            assert_eq!(backend.vcpu.get_regs().unwrap(), registers);
            Ok((status, stdout, stderr))
        });
        thread_group.add_unstarted_worker(2, start_gate.clone(), handle);

        assert_eq!(start_gate.start(), Ok(true));
        started_receiver.recv().unwrap();
        // The gate is Started but the worker has not registered its pthread.
        // Cancellation must be observed after handle_thread_start and before
        // the first vCPU entry even though the pthread scan cannot see it yet.
        thread_group.cancel_workers();
        continue_sender.send(()).unwrap();
        thread_group.join_workers();

        assert_eq!(
            *observed_state.events.lock().unwrap(),
            vec![1, 2],
            "handle_thread_start must precede thread exit lifecycle",
        );
        assert_eq!(
            *observed_state
                .clear_tid_value_at_thread_exit
                .lock()
                .unwrap(),
            Some(0),
            "CHILD_CLEARTID must be zero before on_exit_thread",
        );
    }

    fn assert_production_wrapper_cleans_failed_action(
        action: ProcessAction,
        child: PendingChildKind,
    ) {
        let Some((mut backend, mut executor, mut boundary)) = backend_at_completed_tool_boundary()
        else {
            return;
        };
        if matches!(action, ProcessAction::Thread { .. }) {
            backend.thread_ownership = ThreadOwnership::Tool;
        }
        boundary.frame_address = backend.memory.guest_end();
        let starts = Arc::new(Mutex::new(Vec::new()));
        let thread_state = ();
        let global_state = Arc::new(crate::StraceLog::default());
        let context = ToolContext::<crate::StraceTool> {
            pid: Pid::from_raw(1),
            tid: Pid::from_raw(1),
            thread_state: &thread_state,
            global_state: Some(global_state),
            config: (),
            subscriptions: reverie::Subscription::none(),
            pending_child_starts: starts.clone(),
        };
        let error = futures::executor::block_on(backend.run_process_action_with_tool_at_boundary(
            &mut executor,
            action,
            context,
            ProcessActionContinuation::Restore(Box::new(boundary)),
        ))
        .unwrap_err();
        assert!(matches!(error, Error::InvalidGuestAddress { .. }));

        let starts_were_empty = starts.lock().unwrap().is_empty();
        let child_was_absent = match child {
            PendingChildKind::ForkProcess(pid) => {
                let absent = !executor.has_pending_child_process(pid);
                if !absent {
                    // Keep a failing mutation bounded even if it wrongly started
                    // the child instead of cancelling and removing it.
                    let _ = executor.join_all_child_processes();
                }
                absent
            }
            PendingChildKind::ToolThread(tid) => {
                let present = backend
                    .thread_group
                    .worker_handles
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|worker| worker.tid == tid);
                if present {
                    let gates = backend
                        .thread_group
                        .worker_handles
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|worker| worker.tid == tid)
                        .filter_map(|worker| worker.start.clone())
                        .collect::<Vec<_>>();
                    for gate in gates {
                        let _ = gate.cancel();
                    }
                    let _ = backend.thread_group.discard_unstarted_worker(tid);
                }
                !present
            }
        };
        assert!(
            starts_were_empty,
            "wrapper left an unresolved child-start entry"
        );
        assert!(
            child_was_absent,
            "wrapper left the failed action registered"
        );
    }

    #[test]
    fn production_wrapper_cleans_fork_after_boundary_restore_failure() {
        assert_production_wrapper_cleans_failed_action(
            ProcessAction::Fork {
                child_pid: 41,
                child_stack: None,
                parent_tid: None,
                child_tid: None,
                clear_child_tid: None,
                clear_sighand: false,
                share_address_space: false,
            },
            PendingChildKind::ForkProcess(41),
        );
    }

    #[test]
    fn production_wrapper_cleans_tool_thread_after_boundary_restore_failure() {
        assert_production_wrapper_cleans_failed_action(
            ProcessAction::Thread {
                child_tid: 42,
                child_stack: 0x80_0000,
                parent_tid: None,
                child_tid_address: None,
                clear_child_tid: None,
                tls: None,
            },
            PendingChildKind::ToolThread(42),
        );
    }

    #[test]
    fn callback_local_start_does_not_release_an_unrelated_shared_fork() {
        let mut backend = match KvmBackend::new(0x10_000) {
            Ok(backend) => backend,
            Err(error) => {
                if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                    panic!("KVM is required: {error}");
                }
                eprintln!("skipping callback-local child-start test: {error}");
                return;
            }
        };
        let mut leader = test_executor();
        let mut sibling = leader.thread_child(2).unwrap();

        let (first_sender, first_receiver) = std::sync::mpsc::channel();
        let first_gate = ChildStartGate::new(first_sender);
        leader.register_child_process_with_gate(
            41,
            first_gate.clone(),
            Arc::new(Mutex::new(Some(ChildCompletion::Waitable(
                ExitStatus::SUCCESS,
            )))),
            std::thread::spawn(|| Ok(())),
        );

        let (second_sender, second_receiver) = std::sync::mpsc::channel();
        let second_gate = ChildStartGate::new(second_sender);
        sibling.register_child_process_with_gate(
            42,
            second_gate,
            Arc::new(Mutex::new(None)),
            std::thread::spawn(|| Ok(())),
        );

        let starts = Arc::new(Mutex::new(vec![PendingChildStart::fork_process(
            41, first_gate,
        )]));
        backend
            .start_pending_tool_children(&mut leader, &starts)
            .unwrap();
        assert!(starts.lock().unwrap().is_empty());
        assert_eq!(first_receiver.recv().unwrap(), ChildStartCommand::Start);
        assert_eq!(
            second_receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "completing one callback must not start another callback's shared child",
        );
        assert!(sibling.discard_unstarted_child_process(42).unwrap());
        assert_eq!(second_receiver.recv().unwrap(), ChildStartCommand::Cancel);
        leader.join_all_child_processes().unwrap();
    }

    #[test]
    fn partial_start_failure_keeps_later_child_reachable_for_cleanup() {
        let mut backend = match KvmBackend::new(0x10_000) {
            Ok(backend) => backend,
            Err(error) => {
                if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                    panic!("KVM is required: {error}");
                }
                eprintln!("skipping child-start cleanup KVM test: {error}");
                return;
            }
        };
        let mut executor = test_executor();
        let starts = Arc::new(Mutex::new(Vec::new()));

        let (first_sender, first_receiver) = std::sync::mpsc::channel();
        let first_gate = ChildStartGate::new(first_sender);
        let (first_started_sender, first_started_receiver) = std::sync::mpsc::channel();
        let (first_release_sender, first_release_receiver) = std::sync::mpsc::channel();
        let first_handle = std::thread::spawn(move || {
            assert_eq!(first_receiver.recv().unwrap(), ChildStartCommand::Start);
            first_started_sender.send(()).unwrap();
            first_release_receiver.recv().unwrap();
            Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
        });
        backend
            .thread_group
            .add_unstarted_worker(51, first_gate.clone(), first_handle);
        starts
            .lock()
            .unwrap()
            .push(PendingChildStart::tool_thread(51, first_gate));

        let (lost_sender, lost_receiver) = std::sync::mpsc::channel();
        drop(lost_receiver);
        let lost_gate = ChildStartGate::new(lost_sender);
        let lost_handle = std::thread::spawn(|| Ok(()));
        executor.register_child_process_with_gate(
            52,
            lost_gate.clone(),
            Arc::new(Mutex::new(None)),
            lost_handle,
        );
        starts
            .lock()
            .unwrap()
            .push(PendingChildStart::fork_process(52, lost_gate));

        let later_cancelled = Arc::new(AtomicBool::new(false));
        let later_cancelled_in_child = later_cancelled.clone();
        let (later_sender, later_receiver) = std::sync::mpsc::channel();
        let later_gate = ChildStartGate::new(later_sender);
        let later_handle = std::thread::spawn(move || {
            assert_eq!(later_receiver.recv().unwrap(), ChildStartCommand::Cancel);
            later_cancelled_in_child.store(true, Ordering::Release);
            Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
        });
        backend
            .thread_group
            .add_unstarted_worker(53, later_gate.clone(), later_handle);
        starts
            .lock()
            .unwrap()
            .push(PendingChildStart::tool_thread(53, later_gate));

        let error = backend
            .start_pending_tool_children(&mut executor, &starts)
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("registered KVM child lost its parent start gate"));
        assert!(message.contains("cleanup also failed"));
        assert!(later_cancelled.load(Ordering::Acquire));
        assert!(starts.lock().unwrap().is_empty());
        assert!(!executor.discard_unstarted_child_process(52).unwrap());
        assert!(!backend.thread_group.discard_unstarted_worker(53).unwrap());
        first_started_receiver.recv().unwrap();
        assert_eq!(backend.thread_group.worker_handles.lock().unwrap().len(), 1);
        assert!(!executor.has_pending_child_process(52));
        first_release_sender.send(()).unwrap();
        backend.thread_group.join_workers();
    }

    #[test]
    fn root_guest_pid_must_be_positive() {
        assert_eq!(validate_root_pid(1).unwrap(), 1);
        assert_eq!(validate_root_pid(3).unwrap(), 3);
        assert!(matches!(
            validate_root_pid(0),
            Err(Error::InvalidGuestPid(0))
        ));
        assert!(matches!(
            validate_root_pid(-1),
            Err(Error::InvalidGuestPid(-1))
        ));
    }

    #[test]
    fn root_parent_pid_matches_ptrace_namespace_convention() {
        // Conventional root guest (detcore ROOT_DETPID == 3) is parented to the
        // namespace init, so getppid() == 1 exactly as under the ptrace backend.
        assert_eq!(root_parent_pid(3), 1);
        // Any non-init root guest is likewise parented to init.
        assert_eq!(root_parent_pid(2), 1);
        assert_eq!(root_parent_pid(42), 1);
        // A guest that is itself the namespace init has no parent (getppid == 0).
        assert_eq!(root_parent_pid(CONTAINER_INIT_PID), 0);
    }

    #[test]
    fn guest_thread_transport_slots_are_bounded_and_reusable() {
        let group = GuestThreadGroup::default();
        let mut slots = Vec::new();
        for tid in 2..2 + MAX_GUEST_THREADS as i32 {
            slots.push(group.reserve_transport_slot(tid).unwrap());
        }
        assert_eq!(slots, (0..MAX_GUEST_THREADS as usize).collect::<Vec<_>>());
        assert!(matches!(
            group.reserve_transport_slot(10_000),
            Err(Error::GuestThreadLimitExceeded(10_000))
        ));

        let released = slots[slots.len() / 2];
        group.release_transport_slot(released);
        assert_eq!(group.reserve_transport_slot(10_001).unwrap(), released);
    }

    #[test]
    fn tool_exit_releases_transport_slot_before_exit_callback() {
        match Kvm::new() {
            Ok(_) => {}
            Err(error) if matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM) => {
                eprintln!("skipping KVM slot-release test: cannot open /dev/kvm: {error}");
                return;
            }
            Err(error) => panic!("failed to probe /dev/kvm: {error}"),
        }

        let group = Arc::new(GuestThreadGroup::default());
        for tid in 2..MAX_GUEST_THREADS as i32 + 1 {
            group.reserve_transport_slot(tid).unwrap();
        }
        let exiting_slot = group.reserve_transport_slot(10_000).unwrap();
        assert_eq!(exiting_slot, MAX_GUEST_THREADS as usize - 1);

        let mut backend = KvmBackend::new(16 * 1024 * 1024).unwrap();
        backend.thread_group = group.clone();
        backend.thread_slot = Some(exiting_slot);
        backend.is_guest_thread = true;
        let log = SlotReleaseLog {
            group,
            reused: Mutex::new(None),
        };

        futures::executor::block_on(backend.notify_tool_exit(
            SlotReleaseTool,
            (Pid::from_raw(3), Pid::from_raw(10_000)),
            &log,
            &(),
            (),
            ExitStatus::SUCCESS,
        ))
        .unwrap();

        assert_eq!(backend.thread_slot, None);
        assert_eq!(*log.reused.lock().unwrap(), Some(exiting_slot));
    }

    #[test]
    fn process_snapshot_hides_internal_tool_scratch_pages() {
        let memory = GuestMemory::new(0, (BOOT_RESERVED_END + PAGE_SIZE) as usize).unwrap();
        let root_bottom = TOOL_STACK_TOP - TOOL_STACK_SIZE;
        let first_worker_bottom = thread_tool_stack_top(0) - TOOL_STACK_SIZE;
        let last_worker_bottom =
            thread_tool_stack_top(MAX_GUEST_THREADS as usize - 1) - TOOL_STACK_SIZE;
        let ordinary_page = BOOT_RESERVED_END;

        memory
            .map_user_range(root_bottom, TOOL_STACK_SIZE, false)
            .unwrap();
        memory
            .map_user_range(first_worker_bottom, TOOL_STACK_SIZE, false)
            .unwrap();
        memory
            .map_user_range(last_worker_bottom, TOOL_STACK_SIZE, false)
            .unwrap();
        memory
            .map_user_range(ordinary_page, PAGE_SIZE, false)
            .unwrap();
        let snapshot = memory.snapshot().unwrap();

        hide_tool_scratch_pages(&snapshot).unwrap();

        assert!(memory.user_range_is_mapped(root_bottom, TOOL_STACK_SIZE));
        assert!(!snapshot.user_range_is_mapped(root_bottom, TOOL_STACK_SIZE));
        assert!(!snapshot.user_range_is_mapped(first_worker_bottom, TOOL_STACK_SIZE));
        assert!(!snapshot.user_range_is_mapped(last_worker_bottom, TOOL_STACK_SIZE));
        assert!(snapshot.user_range_is_mapped(ordinary_page, PAGE_SIZE));
    }

    #[test]
    fn guest_thread_group_joins_registered_workers() {
        let group = Arc::new(GuestThreadGroup::default());
        let outer_finished = Arc::new(AtomicBool::new(false));
        let nested_finished = Arc::new(AtomicBool::new(false));
        let worker_group = group.clone();
        let worker_finished = outer_finished.clone();
        let child_finished = nested_finished.clone();
        group.add_worker_handle(
            2,
            std::thread::spawn(move || {
                worker_group.add_worker_handle(
                    3,
                    std::thread::spawn(move || {
                        child_finished.store(true, Ordering::Release);
                        Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
                    }),
                );
                worker_finished.store(true, Ordering::Release);
                Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
            }),
        );

        group.join_workers();

        assert!(outer_finished.load(Ordering::Acquire));
        assert!(nested_finished.load(Ordering::Acquire));
        assert!(group.worker_handles.lock().unwrap().is_empty());
    }

    #[test]
    fn worker_exec_action_preserves_cancelled_group_without_parking() {
        check_worker_exec_action_preserves_cancelled_group(false);
    }

    #[test]
    fn worker_exec_action_preserves_cancelled_group_before_parking() {
        check_worker_exec_action_preserves_cancelled_group(true);
    }

    fn check_worker_exec_action_preserves_cancelled_group(park_syscall_return: bool) {
        match Kvm::new() {
            Ok(_) => {}
            Err(error) if matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM) => {
                assert!(
                    std::env::var_os("REVERIE_REQUIRE_KVM").is_none(),
                    "worker exec action test requires /dev/kvm: {error}"
                );
                eprintln!("skipping KVM worker exec action test: cannot open /dev/kvm: {error}");
                return;
            }
            Err(error) => panic!("failed to probe /dev/kvm: {error}"),
        }

        let mut image = vec![0; 0x1001];
        image[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        for (offset, value) in [(16, 2_u16), (18, 62), (52, 64), (54, 56), (56, 1)] {
            image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [(20, 1_u32), (64, 1), (68, 5)] {
            image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [
            (24, 0x20_0000_u64),
            (32, 64),
            (72, 0x1000),
            (80, 0x20_0000),
            (88, 0x20_0000),
            (96, 1),
            (104, 0x1000),
            (112, 0x1000),
        ] {
            image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        image[0x1000] = HLT;
        let mut parent = KvmBackend::new(16 * 1024 * 1024).unwrap();
        parent
            .install_static_elf(&image, "/worker-exec-guard")
            .unwrap();
        let leader = ElfExecutor::new(parent.static_elf.take().unwrap(), false);
        let mut executor = leader.thread_child(7).unwrap();
        let group = parent.thread_group.clone();
        let mut worker = KvmBackend::from_thread_state(
            parent.memory.clone(),
            parent.vcpu.get_regs().unwrap(),
            parent.vcpu.get_xsave().unwrap(),
            None,
            parent.cpuid_policy,
            7,
            group.clone(),
        )
        .unwrap();
        assert!(worker.is_guest_thread);
        assert!(group.root.lock().unwrap().is_none());
        assert!(group.workers.lock().unwrap().is_empty());
        assert!(group.worker_handles.lock().unwrap().is_empty());
        group.cancelled.store(true, Ordering::Release);
        *group.exit_status.lock().unwrap() = Some(ExitStatus::Exited(127));
        let slots = group.transport_slots.lock().unwrap().clone();
        let thread_slot = worker.thread_slot;
        worker.set_backend_stats_request(BackendStatsRequest::ENABLED);
        let stats = worker.backend_stats();
        let registers = worker.vcpu.get_regs().unwrap();
        let special_registers = worker.vcpu.get_sregs().unwrap();
        let xsave = worker.vcpu.get_xsave().unwrap();
        let mut before = vec![0; worker.memory.len()];
        worker.memory.read_raw(0, &mut before).unwrap();

        let result = worker.run_process_action(
            &mut executor,
            ProcessAction::Exec {
                executable_path: Path::new("/worker-exec-guard").to_owned(),
                executable_file: None,
                image,
                argv: vec!["/worker-exec-guard".to_owned()],
                envp: Vec::new(),
            },
            park_syscall_return,
        );

        let mut after = vec![0; worker.memory.len()];
        worker.memory.read_raw(0, &mut after).unwrap();
        eprintln!(
            "park={park_syscall_return} result={result:?} cancelled={} exit_status={:?} registers_unchanged={} memory_unchanged={}",
            group.cancelled.load(Ordering::Acquire),
            group.exit_status(),
            worker.vcpu.get_regs().unwrap() == registers,
            after == before,
        );
        assert!(matches!(result, Err(Error::GuestThreadExecUnsupported)));
        assert!(group.cancelled.load(Ordering::Acquire));
        assert_eq!(group.exit_status(), Some(ExitStatus::Exited(127)));
        assert!(Arc::ptr_eq(&worker.thread_group, &group));
        assert_eq!(worker.thread_slot, thread_slot);
        assert_eq!(*group.transport_slots.lock().unwrap(), slots);
        assert!(group.root.lock().unwrap().is_none());
        assert!(group.workers.lock().unwrap().is_empty());
        assert!(group.worker_handles.lock().unwrap().is_empty());
        assert_eq!(worker.vcpu.get_regs().unwrap(), registers);
        assert_eq!(worker.vcpu.get_sregs().unwrap(), special_registers);
        assert_eq!(worker.vcpu.get_xsave().unwrap().region, xsave.region);
        assert_eq!(worker.backend_stats(), stats);
        assert!(
            after == before,
            "worker exec action altered shared guest memory"
        );
        for (number, expected) in [(libc::SYS_getpid, 1), (libc::SYS_gettid, 7)] {
            assert_eq!(
                executor.execute(&SyscallRequest::new(number as u64, [0; 6]), &worker.memory),
                expected,
            );
        }
        assert!(executor.take_process_action().is_none());
    }

    #[test]
    fn exec_cancellation_joins_and_rearms_the_thread_group() {
        let group = Arc::new(GuestThreadGroup::default());
        let worker_group = group.clone();
        let worker_finished = Arc::new(AtomicBool::new(false));
        let finished = worker_finished.clone();
        group.add_worker_handle(
            2,
            std::thread::spawn(move || {
                while !worker_group.cancelled.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                finished.store(true, Ordering::Release);
                Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
            }),
        );
        *group.exit_status.lock().unwrap() = Some(ExitStatus::Exited(127));

        group.cancel_workers();
        group.join_workers();
        group.rearm_after_exec();

        assert!(worker_finished.load(Ordering::Acquire));
        assert!(group.worker_handles.lock().unwrap().is_empty());
        assert_eq!(group.exit_status(), None);
        assert!(!group.cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn post_preflight_interpreter_failure_is_fatal_after_image_reset() {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::ffi::OsStringExt;

        const MEMORY_SIZE: usize = 64 * 1024 * 1024;
        let mut backend = match KvmBackend::new(MEMORY_SIZE) {
            Ok(backend) => backend,
            Err(Error::Kvm(error))
                if matches!(error.errno(), libc::ENOENT | libc::EACCES | libc::EPERM) =>
            {
                if std::env::var_os("REVERIE_REQUIRE_KVM").is_some() {
                    panic!("post-preflight exec test requires usable /dev/kvm: {error}");
                }
                return;
            }
            Err(error) => panic!("failed to create KVM backend: {error}"),
        };

        let mut image = std::fs::read("/usr/bin/true").unwrap();
        backend
            .install_static_elf_with_args(&image, &["true"], &[])
            .unwrap();
        let elf = goblin::elf::Elf::parse(&image).unwrap();
        let interpreter = elf.interpreter.unwrap().to_owned();
        let header = elf
            .program_headers
            .iter()
            .find(|header| header.p_type == goblin::elf::program_header::PT_INTERP)
            .unwrap();
        let interpreter_offset = usize::try_from(header.p_offset).unwrap();
        let interpreter_capacity = usize::try_from(header.p_filesz).unwrap();
        drop(elf);

        let suffix = std::process::id();
        let mut interpreter_path = format!("/tmp/kvmld-{suffix}").into_bytes();
        assert!(interpreter_path.len() < interpreter_capacity);
        interpreter_path.resize(interpreter_capacity - 1, b'x');
        let interpreter_copy =
            std::path::PathBuf::from(std::ffi::OsString::from_vec(interpreter_path));
        let executable = std::env::temp_dir().join(format!("kvmexec-{suffix}"));
        struct Cleanup(Vec<std::path::PathBuf>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let mut cleanup = Cleanup(Vec::new());
        let mut interpreter_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&interpreter_copy)
            .unwrap();
        cleanup.0.push(interpreter_copy.clone());
        interpreter_file
            .write_all(&std::fs::read(interpreter).unwrap())
            .unwrap();
        drop(interpreter_file);
        let interpreter_path = interpreter_copy.as_os_str().as_bytes();
        assert_eq!(interpreter_path.len(), interpreter_capacity - 1);
        image[interpreter_offset..interpreter_offset + interpreter_capacity].fill(0);
        image[interpreter_offset..interpreter_offset + interpreter_path.len()]
            .copy_from_slice(interpreter_path);
        let mut executable_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&executable)
            .unwrap();
        cleanup.0.push(executable.clone());
        executable_file.write_all(&image).unwrap();
        drop(executable_file);

        let stack_bottom = backend.memory.guest_end() - crate::elf::STACK_LIMIT;
        let path_address = stack_bottom;
        let path = executable.as_os_str().as_bytes();
        backend.memory.write(path_address, path).unwrap();
        backend
            .memory
            .write(path_address + path.len() as u64, &[0])
            .unwrap();
        let argv_address = stack_bottom + 0x100;
        backend
            .memory
            .write(argv_address, &path_address.to_ne_bytes())
            .unwrap();
        backend
            .memory
            .write(argv_address + 8, &0_u64.to_ne_bytes())
            .unwrap();
        let envp_address = argv_address + 16;
        backend
            .memory
            .write(envp_address, &0_u64.to_ne_bytes())
            .unwrap();

        let loaded = backend.static_elf.take().unwrap();
        let mut executor = ElfExecutor::new(loaded, false);
        let request = SyscallRequest::new(
            libc::SYS_execve as u64,
            [path_address, argv_address, envp_address, 0, 0, 0],
        );
        assert_eq!(executor.execute(&request, &backend.memory), 0);
        let action = executor
            .take_process_action()
            .expect("successful preflight must schedule exec");

        // Removing the interpreter after the executor's successful preflight
        // forces a rare failure beyond exec's point of no return. The backend
        // must fail instead of resuming the now-reset old image, matching Linux
        // semantics when exec fails after tearing down the old address space.
        std::fs::remove_file(&interpreter_copy).unwrap();
        let sentinel_address = stack_bottom + PAGE_SIZE;
        const SENTINEL: [u8; 16] = *b"old-image-state!";
        backend.memory.write(sentinel_address, &SENTINEL).unwrap();
        let retained = backend.memory.clone();
        let error = backend
            .run_process_action(&mut executor, action, false)
            .unwrap_err();
        assert!(
            matches!(
                &error,
                Error::UnsupportedElf(message) if message.contains("interpreter")
            ),
            "unexpected exec error: {error:?}"
        );
        let mut observed = [0; SENTINEL.len()];
        assert!(
            !backend
                .memory
                .user_range_is_mapped(sentinel_address, SENTINEL.len() as u64)
        );
        retained.read_raw(sentinel_address, &mut observed).unwrap();
        assert_eq!(observed, [0; SENTINEL.len()]);
    }

    #[test]

    fn cancelling_named_tool_thread_joins_only_that_unstarted_worker() {
        let group = Arc::new(GuestThreadGroup::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_in_worker = cancelled.clone();
        let (cancel_sender, cancel_receiver) = std::sync::mpsc::channel();
        let cancel_gate = ChildStartGate::new(cancel_sender);
        group.add_unstarted_worker(
            2,
            cancel_gate.clone(),
            std::thread::spawn(move || {
                match cancel_receiver.recv() {
                    Ok(ChildStartCommand::Cancel) => {
                        cancelled_in_worker.store(true, Ordering::Release)
                    }
                    other => panic!("unstarted worker received {other:?}"),
                }
                Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
            }),
        );

        let sibling_finished = Arc::new(AtomicBool::new(false));
        let sibling_finished_in_worker = sibling_finished.clone();
        let (sibling_start_sender, sibling_start_receiver) = std::sync::mpsc::channel();
        let (sibling_release_sender, sibling_release_receiver) = std::sync::mpsc::channel();
        group.add_worker_handle(
            3,
            std::thread::spawn(move || {
                assert_eq!(
                    sibling_start_receiver.recv().unwrap(),
                    ChildStartCommand::Start
                );
                sibling_release_receiver.recv().unwrap();
                sibling_finished_in_worker.store(true, Ordering::Release);
                Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
            }),
        );
        sibling_start_sender.send(ChildStartCommand::Start).unwrap();

        let pending = PendingChildStart::tool_thread(2, cancel_gate);
        assert!(matches!(
            pending.cancel(),
            PendingChildCancellation::NewlyCancelled {
                child: PendingChildKind::ToolThread(2),
                delivery_failed: false
            }
        ));
        assert!(group.discard_unstarted_worker(2).unwrap());
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(group.worker_handles.lock().unwrap().len(), 1);
        assert_eq!(group.worker_handles.lock().unwrap()[0].tid, 3);
        assert!(!group.discard_unstarted_worker(99).unwrap());
        assert!(!sibling_finished.load(Ordering::Acquire));

        sibling_release_sender.send(()).unwrap();
        group.join_workers();
        assert!(sibling_finished.load(Ordering::Acquire));
    }

    #[test]
    fn pending_gated_worker_is_cancelled_before_teardown_join() {
        let group = Arc::new(GuestThreadGroup::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_in_worker = cancelled.clone();
        let (start_sender, start_receiver) = std::sync::mpsc::channel();
        let start_gate = ChildStartGate::new(start_sender);
        group.add_unstarted_worker(
            2,
            start_gate,
            std::thread::spawn(move || {
                assert_eq!(start_receiver.recv().unwrap(), ChildStartCommand::Cancel);
                cancelled_in_worker.store(true, Ordering::Release);
                Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
            }),
        );

        group.cancel_workers();
        group.join_workers();

        assert!(cancelled.load(Ordering::Acquire));
        assert!(group.worker_handles.lock().unwrap().is_empty());
    }
    #[test]
    fn clone_tid_stores_and_clear_are_best_effort() {
        const TID_ADDRESS: u64 = 0x100;

        let mut memory = GuestMemory::new(0, 4096).unwrap();
        write_tid_best_effort(&mut memory, Some(TID_ADDRESS), 7);
        let mut bytes = [0; std::mem::size_of::<i32>()];
        memory.read(TID_ADDRESS, &mut bytes).unwrap();
        assert_eq!(i32::from_le_bytes(bytes), 7);

        write_tid_best_effort(&mut memory, Some(TID_ADDRESS), 0);
        memory.read(TID_ADDRESS, &mut bytes).unwrap();
        assert_eq!(i32::from_le_bytes(bytes), 0);

        write_tid_best_effort(&mut memory, Some(4095), 9);
        write_tid_best_effort(&mut memory, None, 9);
    }

    #[test]
    fn nested_pending_worker_is_cancelled_in_the_next_join_batch() {
        let group = Arc::new(GuestThreadGroup::default());
        let nested_cancelled = Arc::new(AtomicBool::new(false));
        let nested_cancelled_in_worker = nested_cancelled.clone();
        let parent_group = group.clone();
        group.add_worker_handle(
            1,
            std::thread::spawn(move || {
                let (start_sender, start_receiver) = std::sync::mpsc::channel();
                let start_gate = ChildStartGate::new(start_sender);
                parent_group.add_unstarted_worker(
                    2,
                    start_gate,
                    std::thread::spawn(move || {
                        assert_eq!(start_receiver.recv().unwrap(), ChildStartCommand::Cancel);
                        nested_cancelled_in_worker.store(true, Ordering::Release);
                        Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
                    }),
                );
                Ok((ExitStatus::SUCCESS, Vec::new(), Vec::new()))
            }),
        );

        group.join_workers();

        assert!(nested_cancelled.load(Ordering::Acquire));
        assert!(group.worker_handles.lock().unwrap().is_empty());
    }
}
