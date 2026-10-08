/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The in-guest host for a Reverie Tool: it owns the Tool, its per-thread
//! state and its coordinator connection, runs each guest syscall and
//! instruction event through the Tool's handlers, follows forks, and
//! implements [`Guest`] for the Tool's callbacks. The backend that owns the
//! trap and hook paths implements [`HostRuntime`] for the services the host
//! calls back into.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io;

use reverie::Error;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
use reverie::Rdtsc;
use reverie::Stack;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::Errno;
use reverie::syscalls::LocalMemory;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;

use super::context::RegisterContext;
use super::event::InstructionEventKind;
use super::event::SyscallDispatch;
use super::event::SyscallEvent;
use super::rpc::CoordinatorRpc;
use crate::sync::SpinMutex;
use crate::tool_host::DrivenSyscall;
use crate::tool_host::TailResult;
use crate::tool_host::drive_ready;
use crate::tool_host::drive_tool_syscall;
use crate::trap::raw_syscall6;

/// The services a [`ToolHost`] needs from the backend that owns the trap and
/// hook paths.
pub trait HostRuntime: Send + Sync + 'static {
    /// In a forked child, before anything else: rebind per-process trap-path
    /// state (such as the fallback continuation) to the child.
    fn fork_child_rebind(&self);
    /// Reports a lifecycle stage on the backend's diagnostic stream, if any.
    fn emit_stage(&self, stage: &[u8]);
    /// In a forked child, after the Tool and thread state are replaced: reset
    /// the inherited observability and statistics, and account the child's
    /// first event (`event`) to the path that entered it.
    fn fork_child_reset(&self, event: &SyscallEvent);
    /// Whether CPUID is delivered to the Tool in this process.
    fn cpuid_interception_enabled(&self) -> bool;
    /// At process exit, after the Tool's exit callbacks: submit the process's
    /// statistics, if the backend collects them. An error ends the process
    /// with status 125.
    fn exit_process_stats(&self, tid: Pid) -> io::Result<()>;
    /// Reads the guest thread's retired-conditional-branch clock.
    fn read_clock(&self) -> io::Result<u64>;
    /// Whether a guest `rt_sigaction`-family call may be forwarded.
    fn signal_action_supported(&self, number: i64, args: [u64; 6]) -> bool;
    /// The signals the runtime keeps for itself; stripped from any set the
    /// guest blocks.
    fn reserved_signal_mask(&self) -> u64;
}

/// Decide a guest signal call through the runtime's virtual SIGALRM state;
/// `Some` is its result and it must not be forwarded.
fn virtual_signal_call(
    runtime: &impl HostRuntime,
    guest_pkru: Option<u32>,
    original_args: [u64; 6],
    number: i64,
    args: [u64; 6],
) -> Option<i64> {
    let rights = super::sigalrm::GuestRights {
        pkru: guest_pkru,
        original_args,
    };
    if super::sigalrm::original_input_denied(rights, number, args) {
        return Some(-i64::from(libc::EFAULT));
    }
    let policy = super::sigalrm::Policy {
        reserved: runtime.reserved_signal_mask(),
        rights,
    };
    // SAFETY: called only while forwarding the guest's own call, in its turn.
    unsafe { super::sigalrm::intercept(&policy, number, args) }
}

const STACK_CAPACITY: usize = 4096;

static COMMITTED_STACKS: SpinMutex<Vec<Box<[u8]>>> = SpinMutex::new(Vec::new());

struct DispatchScratchScope {
    _allocation_scope: super::alloc::DispatchAllocationScope,
}

impl DispatchScratchScope {
    fn enter() -> Self {
        COMMITTED_STACKS.lock().clear();
        Self {
            _allocation_scope: super::alloc::enter_dispatch(),
        }
    }
}

impl Drop for DispatchScratchScope {
    fn drop(&mut self) {
        COMMITTED_STACKS.lock().clear();
    }
}

/// Hosts one Reverie Tool inside the guest process: owns the Tool, its
/// per-thread state and its coordinator connection, and runs each guest
/// syscall or instruction event through the Tool's handlers. The backend that
/// owns the trap and hook paths supplies `R`, the runtime services the host
/// calls back into.
pub struct ToolHost<T: Tool, R: HostRuntime> {
    tool: SpinMutex<Option<T>>,
    rpc: CoordinatorRpc<T::GlobalState>,
    root_pid: Pid,
    subscriptions: HashSet<Sysno>,
    cpuid_interception: bool,
    states: SpinMutex<HashMap<i32, T::ThreadState>>,
    runtime: R,
}

impl<T: Tool, R: HostRuntime> ToolHost<T, R> {
    /// This process's coordinator connection.
    pub fn rpc(&self) -> &CoordinatorRpc<T::GlobalState> {
        &self.rpc
    }
}

impl<T, R> ToolHost<T, R>
where
    T: Tool + 'static,
    R: HostRuntime,
{
    /// Creates the host for the root process's Tool. `subscriptions` are the
    /// Tool's syscall subscriptions; `cpuid_interception` says whether CPUID
    /// is delivered to the Tool.
    pub fn new(
        tool: T,
        rpc: CoordinatorRpc<T::GlobalState>,
        root_pid: Pid,
        subscriptions: HashSet<Sysno>,
        cpuid_interception: bool,
        runtime: R,
    ) -> Self {
        COMMITTED_STACKS.lock().clear();
        Self {
            tool: SpinMutex::new(Some(tool)),
            rpc,
            root_pid,
            subscriptions,
            cpuid_interception,
            states: SpinMutex::new(HashMap::new()),
            runtime,
        }
    }

    /// Runs one guest syscall through the Tool and sets `event.result`.
    ///
    /// # Safety
    ///
    /// `event.context` must be 0 or the address of a [`RegisterContext`]
    /// that is valid for reads and writes, and accessed by nothing else, for
    /// the whole call: the Tool reads and writes the guest's registers
    /// through it. The caller must be on the guest thread's syscall path that
    /// produced the event (a trap, a hook or the fallback continuation), so
    /// that forwarding and injecting run as that thread.
    ///
    /// The Tool runs in this process, and a Tool callback can inject
    /// syscalls that read or write any memory of the process (a `read` into
    /// an arbitrary address, for instance). The caller vouches for the Tool:
    /// its injections must not break Rust's memory invariants for any memory
    /// they touch.
    pub unsafe fn dispatch(&self, event: &mut SyscallEvent) {
        let _scratch_scope = DispatchScratchScope::enter();
        let tid = raw_pid(libc::SYS_gettid);
        let pid = raw_pid(libc::SYS_getpid);
        let ppid = (pid != self.root_pid).then(|| raw_pid(libc::SYS_getppid));

        // These process-wide locks are valid only while thread creation stays
        // fail-closed. A scheduler RPC may block until a sibling runs, so MT
        // support requires per-thread RPC/state ownership before relaxing the
        // clone guard below.
        let mut tool_slot = self.tool.lock();
        let tool = tool_slot.as_ref().unwrap_or_else(|| fatal(126));
        let mut states = self.states.lock();
        let is_new = !states.contains_key(&tid.as_raw());
        let state = states
            .entry(tid.as_raw())
            .or_insert_with(|| tool.init_thread_state(tid, None));
        let tail = TailResult::default();
        let mut guest = InGuest::<T, R> {
            event,
            tid,
            pid,
            ppid,
            state,
            rpc: &self.rpc,
            tail: &tail,
            cpuid_interception: self.cpuid_interception,
            fork_parent_state: None,
            runtime: &self.runtime,
        };

        if is_new && let Err(error) = drive_ready(tool.handle_thread_start(&mut guest)) {
            tool_fatal(124, &error);
        }

        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-liteinst-post-exec): The kernel exec'd the guest
        // image before this LD_PRELOAD backend attached, so — unlike the ptrace
        // backend — the `Tool::handle_post_exec` lifecycle callback never fired.
        // A Tool such as Detcore relies on it to determinize the auxv AT_RANDOM
        // vector and to advance per-thread state (e.g. its seeded PRNG) exactly
        // as the ptrace backend does; without it the guest-visible getrandom(2)
        // stream is offset relative to ptrace and cross-backend parity fails.
        // The guest genuinely did execve into this image, so emitting the event
        // once for the root process's main thread restores contract parity. It
        // is intentionally not emitted for child threads (there is none in the
        // current single-process/thread tool mode) nor re-emitted per dispatch.
        if is_new
            && tid.as_raw() == self.root_pid.as_raw()
            && let Err(error) = drive_ready(tool.handle_post_exec(&mut guest))
        {
            // handle_post_exec returns Errno; tool_fatal expects reverie::Error.
            tool_fatal(124, &Error::from(error));
        }

        let Some(number) = usize::try_from(guest.event.number)
            .ok()
            .and_then(Sysno::new)
        else {
            guest.event.result = -i64::from(libc::ENOSYS);
            return;
        };
        if !self.subscriptions.contains(&number) {
            let number = guest.event.number;
            let args = guest.event.args;
            let guest_pkru = guest.event.guest_pkru;
            if is_plain_fork(number, args) {
                guest.prepare_fork_parent_state();
                let result = forward_plain_fork(number, args, Some(&mut guest.event.guest_pkru));
                if result == 0 {
                    let parent_state = guest.take_fork_parent_state();
                    drop(guest);
                    finish_fork_child(
                        &mut tool_slot,
                        &mut states,
                        &self.rpc,
                        &self.runtime,
                        event,
                        parent_state,
                        ForkChildContext {
                            parent_tid: tid,
                            parent_pid: pid,
                            child_tid: raw_pid(libc::SYS_gettid),
                            child_pid: raw_pid(libc::SYS_getpid),
                        },
                    );
                } else {
                    event.result = result;
                }
                return;
            } else if is_exit_syscall(number) {
                finish_tool_exit(
                    &mut tool_slot,
                    &mut states,
                    &self.rpc,
                    &self.runtime,
                    ToolExitContext {
                        tid,
                        pid,
                        number,
                        args,
                    },
                );
            } else if let Some(error) = injected_syscall_guard(&self.runtime, number, args) {
                event.result = -i64::from(error.into_raw());
                return;
            } else if let Some(result) =
                unsafe { super::protect::protect_forwarded_descriptor_change(number, args) }
            {
                // A close or close_range the runtime left for dispatch, of a
                // Tool that does not subscribe to it: protected here instead.
                event.result = result;
                return;
            }
            if let Some(result) = virtual_signal_call(&self.runtime, guest_pkru, args, number, args)
            {
                event.result = result;
                return;
            }
            // This is the original unsubscribed guest operation. Private
            // inject/tail_inject below deliberately keep caller rights.
            match stripped_signal_mask(&self.runtime, number, args) {
                Err(error) => event.result = -i64::from(error.into_raw()),
                Ok(None) => event.result = unsafe { event.forward() },
                Ok(Some(mask)) => {
                    // The stripped copy is runtime-private, so the call runs
                    // through the ordinary raw gate, as on inject and
                    // tail_inject, with the thread's current keys rather than
                    // the guest's saved PKRU (see stripped_signal_mask).
                    let mut stripped_args = args;
                    stripped_args[1] = (&raw const mask) as u64;
                    event.result = unsafe { raw_syscall6(number, stripped_args) };
                }
            }
            return;
        }
        let args = guest.event.args.map(|arg| arg as usize);
        let syscall = Syscall::from_raw(
            number,
            SyscallArgs::new(args[0], args[1], args[2], args[3], args[4], args[5]),
        );

        // Drive the Tool handler to a terminal outcome. The shared driver owns
        // the ERESTARTSYS restart protocol (Reverie #362) so it cannot
        // drift between the in-guest backends; this host maps each terminal
        // outcome onto its own per-thread lifecycle (exit/fork-child) state.
        match drive_tool_syscall(tool, &mut guest, syscall, &tail) {
            DrivenSyscall::Result(value) => {
                guest.event.result = value;
            }
            DrivenSyscall::Exit { number, args } => {
                finish_tool_exit(
                    &mut tool_slot,
                    &mut states,
                    &self.rpc,
                    &self.runtime,
                    ToolExitContext {
                        tid,
                        pid,
                        number,
                        args,
                    },
                );
                event.result = unsafe { raw_syscall6(number, args) };
            }
            DrivenSyscall::ForkChild {
                parent_tid,
                parent_pid,
                child_tid,
                child_pid,
            } => {
                let parent_state = guest.take_fork_parent_state();
                drop(guest);
                finish_fork_child(
                    &mut tool_slot,
                    &mut states,
                    &self.rpc,
                    &self.runtime,
                    event,
                    parent_state,
                    ForkChildContext {
                        parent_tid,
                        parent_pid,
                        child_tid,
                        child_pid,
                    },
                );
            }
            DrivenSyscall::Fatal(error) => tool_fatal(125, &error),
        }
    }

    /// Runs one CPUID, RDTSC or RDTSCP event through the Tool and writes its
    /// result into `context`.
    ///
    /// # Safety
    ///
    /// `context` must be the registers of the guest instruction being
    /// handled, on the guest thread's path that intercepted it.
    ///
    /// The Tool runs in this process, and a Tool callback can inject
    /// syscalls that read or write any memory of the process (a `read` into
    /// an arbitrary address, for instance). The caller vouches for the Tool:
    /// its injections must not break Rust's memory invariants for any memory
    /// they touch.
    pub unsafe fn dispatch_instruction(
        &self,
        kind: InstructionEventKind,
        context: &mut RegisterContext,
    ) {
        let _scratch_scope = DispatchScratchScope::enter();
        let tid = raw_pid(libc::SYS_gettid);
        let pid = raw_pid(libc::SYS_getpid);
        let ppid = (pid != self.root_pid).then(|| raw_pid(libc::SYS_getppid));
        let tool_slot = self.tool.lock();
        let tool = tool_slot.as_ref().unwrap_or_else(|| fatal(126));
        let mut states = self.states.lock();
        let is_new = !states.contains_key(&tid.as_raw());
        let state = states
            .entry(tid.as_raw())
            .or_insert_with(|| tool.init_thread_state(tid, None));
        let tail = TailResult::default();
        let mut event = SyscallEvent {
            number: -1,
            args: [0; 6],
            instruction_pointer: context.instruction_pointer,
            result: 0,
            context: context as *mut RegisterContext as usize,
            dispatch: SyscallDispatch::InstalledHook,
            guest_pkru: None,
        };
        let mut guest = InGuest::<T, R> {
            event: &mut event,
            tid,
            pid,
            ppid,
            state,
            rpc: &self.rpc,
            tail: &tail,
            cpuid_interception: self.cpuid_interception,
            fork_parent_state: None,
            runtime: &self.runtime,
        };
        if is_new && let Err(error) = drive_ready(tool.handle_thread_start(&mut guest)) {
            tool_fatal(124, &error);
        }
        if is_new
            && tid.as_raw() == self.root_pid.as_raw()
            && let Err(error) = drive_ready(tool.handle_post_exec(&mut guest))
        {
            tool_fatal(124, &Error::from(error));
        }

        match kind {
            InstructionEventKind::Cpuid => {
                let result = drive_ready(tool.handle_cpuid_event(
                    &mut guest,
                    context.rax as u32,
                    context.rcx as u32,
                ))
                .unwrap_or_else(|error| tool_fatal(125, &Error::from(error)));
                context.rax = u64::from(result.eax);
                context.rbx = u64::from(result.ebx);
                context.rcx = u64::from(result.ecx);
                context.rdx = u64::from(result.edx);
            }
            InstructionEventKind::Rdtsc | InstructionEventKind::Rdtscp => {
                let request = if kind == InstructionEventKind::Rdtscp {
                    Rdtsc::Tscp
                } else {
                    Rdtsc::Tsc
                };
                let result = drive_ready(tool.handle_rdtsc_event(&mut guest, request))
                    .unwrap_or_else(|error| tool_fatal(125, &Error::from(error)));
                context.rax = result.tsc as u32 as u64;
                context.rdx = result.tsc.checked_shr(32).unwrap_or(0);
                if let Some(aux) = result.aux {
                    context.rcx = u64::from(aux);
                }
            }
        }
    }
}

fn finish_fork_child<T: Tool, R: HostRuntime>(
    tool_slot: &mut Option<T>,
    states: &mut HashMap<i32, T::ThreadState>,
    rpc: &CoordinatorRpc<T::GlobalState>,
    runtime: &R,
    event: &mut SyscallEvent,
    parent_snapshot: T::ThreadState,
    context: ForkChildContext,
) {
    let ForkChildContext {
        parent_tid,
        parent_pid,
        child_tid,
        child_pid,
    } = context;
    runtime.fork_child_rebind();
    runtime.emit_stage(b"fork-child-thread-start-begin");
    // This child inherited the parent's coordinator connection. Flag it before
    // any child-side callback can issue an RPC (`handle_thread_start` below is
    // the first such opportunity) so the next `send_rpc` reconnects under the
    // child's own identity. Doing it here rather than from a `pthread_atfork`
    // hook also covers forks that never enter libc, such as a raw `SYS_fork` or
    // a raw plain `SYS_clone`.
    super::rpc::note_fork_in_child();
    let inherited_parent_state = states
        .remove(&parent_tid.as_raw())
        .unwrap_or_else(|| fatal(126));
    drop(inherited_parent_state);
    let child_tool = T::new(child_pid, rpc.config());
    let child_state = child_tool.init_thread_state(child_tid, Some((parent_tid, &parent_snapshot)));
    states.clear();
    states.insert(child_tid.as_raw(), child_state);
    *tool_slot = Some(child_tool);
    // Both installed hooks and deferred fallback carry a register context.
    // Attribute the child's first event to the path that actually entered it.
    runtime.fork_child_reset(event);

    let tool = tool_slot.as_ref().unwrap_or_else(|| fatal(126));
    let state = states
        .get_mut(&child_tid.as_raw())
        .unwrap_or_else(|| fatal(126));
    let child_tail = TailResult::default();
    let mut child_guest = InGuest::<T, R> {
        event,
        tid: child_tid,
        pid: child_pid,
        ppid: Some(parent_pid),
        state,
        rpc,
        tail: &child_tail,
        cpuid_interception: runtime.cpuid_interception_enabled(),
        fork_parent_state: None,
        runtime,
    };
    if let Err(error) = drive_ready(tool.handle_thread_start(&mut child_guest)) {
        tool_fatal(124, &error);
    }
    runtime.emit_stage(b"fork-child-thread-start-complete");
    child_guest.event.result = 0;
}

struct ForkChildContext {
    parent_tid: Pid,
    parent_pid: Pid,
    child_tid: Pid,
    child_pid: Pid,
}

// TODO-HUMAN-REVIEW(PR-143): Review single-process Tool exit lifecycle.
fn finish_tool_exit<T: Tool, R: HostRuntime>(
    tool_slot: &mut Option<T>,
    states: &mut HashMap<i32, T::ThreadState>,
    rpc: &CoordinatorRpc<T::GlobalState>,
    runtime: &R,
    context: ToolExitContext,
) {
    let ToolExitContext {
        tid,
        pid,
        number,
        args,
    } = context;
    let state = states
        .remove(&tid.as_raw())
        .expect("LiteInst thread state disappeared before exit");
    let status = reverie::ExitStatus::Exited((args[0] & 0xff) as i32);
    let tool = tool_slot.as_ref().unwrap_or_else(|| fatal(126));
    if let Err(error) = drive_ready(tool.on_exit_thread(tid, rpc, state, status)) {
        tool_fatal(125, &error);
    }
    if is_process_exit(number, tid, pid) {
        let tool = tool_slot.take().unwrap_or_else(|| fatal(126));
        if let Err(error) = drive_ready(tool.on_exit_process(pid, rpc, status)) {
            tool_fatal(125, &error);
        }
        if let Err(error) = runtime.exit_process_stats(tid) {
            tool_fatal(125, &Error::from(error));
        }
    }
}

struct ToolExitContext {
    tid: Pid,
    pid: Pid,
    number: i64,
    args: [u64; 6],
}

// TODO-HUMAN-REVIEW(PR-143): Review exit syscall lifecycle classification.
fn is_exit_syscall(number: i64) -> bool {
    // AUTONOMOUS-BOT-IMPLEMENTED
    matches!(number, libc::SYS_exit | libc::SYS_exit_group)
}

// TODO-HUMAN-REVIEW(PR-143): Review single-process exit classification.
fn is_process_exit(number: i64, tid: Pid, pid: Pid) -> bool {
    // AUTONOMOUS-BOT-IMPLEMENTED
    number == libc::SYS_exit_group || tid == pid
}

fn raw_pid(number: i64) -> Pid {
    let value = unsafe { raw_syscall6(number, [0; 6]) };
    if value <= 0 {
        fatal(126);
    }
    Pid::from_raw(value as i32)
}

/// The [`Guest`] a Tool callback receives: the calling thread's identity and
/// state, the event being handled, and the coordinator connection.
struct InGuest<'a, T: Tool, R: HostRuntime> {
    event: &'a mut SyscallEvent,
    tid: Pid,
    pid: Pid,
    ppid: Option<Pid>,
    state: &'a mut T::ThreadState,
    rpc: &'a CoordinatorRpc<T::GlobalState>,
    tail: &'a TailResult,
    cpuid_interception: bool,
    fork_parent_state: Option<T::ThreadState>,
    runtime: &'a R,
}

impl<T: Tool, R: HostRuntime> InGuest<'_, T, R> {
    /// Materialize the parent view while every synchronization owner still
    /// exists. A raw process fork can otherwise copy a locked Tool-state mutex
    /// into the child after its owning thread disappeared. Round-tripping via
    /// the existing ThreadState migration contract gives the child private,
    /// unlocked synchronization primitives without a backend-specific Tool API.
    fn prepare_fork_parent_state(&mut self) {
        let encoded = bincode::serde::encode_to_vec(&*self.state, bincode::config::standard())
            .unwrap_or_else(|_| fatal(126));
        let (snapshot, consumed) = bincode::serde::decode_from_slice::<T::ThreadState, _>(
            &encoded,
            bincode::config::standard(),
        )
        .unwrap_or_else(|_| fatal(126));
        if consumed != encoded.len() {
            fatal(126);
        }
        self.fork_parent_state = Some(snapshot);
    }

    fn take_fork_parent_state(&mut self) -> T::ThreadState {
        self.fork_parent_state.take().unwrap_or_else(|| fatal(126))
    }
}

#[reverie::tool]
impl<T: Tool, R: HostRuntime> GlobalRPC<T::GlobalState> for InGuest<'_, T, R> {
    async fn send_rpc(
        &self,
        message: <T::GlobalState as GlobalTool>::Request,
    ) -> <T::GlobalState as GlobalTool>::Response {
        self.rpc.send_rpc(message).await
    }

    fn config(&self) -> &<T::GlobalState as GlobalTool>::Config {
        self.rpc.config()
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-326): Review the plain-fork injection boundary.
fn is_plain_fork(number: i64, args: [u64; 6]) -> bool {
    if number == libc::SYS_fork {
        return true;
    }
    if number == libc::SYS_vfork {
        return true;
    }
    if number == libc::SYS_clone3 {
        return clone3_is_plain_fork(args[0], args[1]);
    }
    if number != libc::SYS_clone {
        return false;
    }
    const SIGNAL_MASK: u64 = 0xff;
    let allowed_flags =
        (libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::CLONE_PARENT_SETTID) as u64;
    args[1] == 0
        && args[0] & SIGNAL_MASK == libc::SIGCHLD as u64
        && args[0] & !(SIGNAL_MASK | allowed_flags) == 0
}

fn clone3_is_plain_fork(address: u64, size: u64) -> bool {
    const CLONE_ARGS_SIZE_VER0: usize = 64;
    const CLONE_ARGS_SIZE_VER2: usize = 88;
    if address == 0 || size < CLONE_ARGS_SIZE_VER0 as u64 {
        return false;
    }
    let mut fields = [0_u64; CLONE_ARGS_SIZE_VER2 / core::mem::size_of::<u64>()];
    let read_len = usize::try_from(size)
        .unwrap_or(usize::MAX)
        .min(core::mem::size_of_val(&fields));
    let local = libc::iovec {
        iov_base: fields.as_mut_ptr().cast(),
        iov_len: read_len,
    };
    let remote = libc::iovec {
        iov_base: address as usize as *mut libc::c_void,
        iov_len: read_len,
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let read = unsafe {
        raw_syscall6(
            libc::SYS_process_vm_readv,
            [
                pid as u64,
                (&raw const local) as u64,
                1,
                (&raw const remote) as u64,
                1,
                0,
            ],
        )
    };
    if read != read_len as i64 {
        return false;
    }

    clone_args_are_plain_fork(&fields)
}

/// Whether `struct clone_args` fields (at least its first 88 bytes, as
/// eleven words) describe the plain-fork subset the runtime accepts: only
/// the TID-output flags, exit signal SIGCHLD, no stack, and nothing in the
/// later fields (set_tid, set_tid_size, cgroup).
fn clone_args_are_plain_fork(fields: &[u64]) -> bool {
    let allowed_flags =
        (libc::CLONE_CHILD_CLEARTID | libc::CLONE_CHILD_SETTID | libc::CLONE_PARENT_SETTID) as u64;
    let flags = fields[0];
    flags & !allowed_flags == 0
        && fields[4] == libc::SIGCHLD as u64
        && fields[5] == 0
        && fields[6] == 0
        && fields[8..11].iter().all(|field| *field == 0)
}

/// One process creation the guest asked for, as the runtime performed or
/// refused it; reported to the [`PhysicalCreationHook`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalCreation {
    /// The child exists. Its birth identity is the pidfs inode number of the
    /// pidfd the kernel created atomically with it (`CLONE_PIDFD`): one inode
    /// per process, never reused while the kernel runs, so it names the child
    /// even after the child has died, been reaped and had its pid reused.
    Created {
        /// The child's pid, which the guest receives.
        pid: i32,
        /// The child's pidfs inode number.
        birth_identity: u64,
    },
    /// The guest's own call failed with this errno, which the guest
    /// receives. No child exists.
    Failed {
        /// The errno the guest receives.
        errno: i32,
    },
    /// The runtime could not perform the creation exactly as the guest's own
    /// call would have run. Whatever the hook returns, the creating process
    /// then ends with status 125.
    Refused {
        /// Why.
        refusal: CreationRefusal,
        /// A child that was created anyway.
        child: Option<RefusedChild>,
    },
}

/// A child created for a refused creation (see [`PhysicalCreation::Refused`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefusedChild {
    /// The child's pid.
    pub pid: i32,
    /// Whether Linux accepted the runtime's SIGKILL, sent through the
    /// child's pidfd before it was closed.
    pub killed: bool,
}

/// Why the runtime refused a creation; see [`PhysicalCreation::Refused`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreationRefusal {
    /// The guest's protection keys deny access to key 0, which the runtime's
    /// own clone3 record and pidfd output carry.
    ProtectionKeyDenied,
    /// The runtime cannot tell what Linux's own reading of the guest's
    /// clone_args would return: it cannot copy a page Linux can read (a
    /// write-only page), or its readability probe of a page gave a result
    /// that proves neither a read nor a fault.
    ArgumentsUnreadable,
    /// The physical clone3 failed with an errno other than those the guest's
    /// own call returns for the same reason (EAGAIN or ENOSPC, and for a
    /// guest clone3 also E2BIG or EINVAL from its own record). Errors such as
    /// EMFILE, ENFILE or ENOMEM, which allocating the runtime's pidfd can
    /// return, cannot be attributed to the guest's call.
    RuntimeError(i32),
    /// The guest's parent TID output (`CLONE_PARENT_SETTID`) overlaps the
    /// runtime's own pidfd output: Linux refuses an equal pair with EINVAL
    /// for the substituted call, and an overlapping one would corrupt the
    /// descriptor number, while the guest's own call would succeed.
    OutputAliased,
    /// The guest's clone_args changed between the runtime's classification of
    /// the call and its copy (another process sharing that memory), and the
    /// copy is no longer a plain fork. Nothing was created.
    ArgumentsChanged,
    /// The child exists, but its pidfd is not a pidfs inode or cannot be
    /// read, so it has no birth identity.
    IdentityUnavailable,
    /// The child exists, but the runtime's pidfd was not closed: close failed
    /// with this errno, or (0) close reported success or EINTR while the
    /// descriptor stayed open.
    CloseFailed(i32),
}

/// Called by the runtime in the creating process only, exactly once for each
/// process creation the guest asks for (fork, the copying vfork, and the
/// plain-fork forms of clone and clone3), whether it succeeded, failed or was
/// refused. The call happens synchronously after the physical call returns,
/// and before the copying vfork's wait or the return to the Tool. For
/// `Created` the transient pidfd is closed, and its closure confirmed, before
/// the call. For `Refused` it may still be open (a child that could not be
/// killed, or a failed close); the process then ends right after the call,
/// which releases it. The hook must not wait for guest progress or take
/// the dispatch locks. An error, like a refusal, ends the creating process
/// with status 125; the Tool reports its own failure before returning one.
pub type PhysicalCreationHook = fn(PhysicalCreation) -> io::Result<()>;

static CREATION_HOOK: std::sync::OnceLock<PhysicalCreationHook> = std::sync::OnceLock::new();

/// Why [`set_physical_creation_hook`] failed.
#[derive(Debug)]
pub enum CreationHookError {
    /// A hook is already registered.
    AlreadyRegistered,
    /// The kernel's pidfds are not pidfs inodes (Linux before 6.9), so a
    /// pidfd's inode number does not identify one process.
    PidfsUnavailable,
    /// The runtime's own seccomp filter is not installed yet, so a guest
    /// syscall made before it could attach a filter without the runtime
    /// seeing it. Register the hook after the runtime is installed.
    InterceptionNotInstalled,
    /// The runtime's private page for the pidfd output could not be mapped
    /// (this errno).
    StorageUnavailable(i32),
}

/// Registers the process's [`PhysicalCreationHook`].
///
/// Prerequisite, which the runtime cannot check: no seccomp filter other than
/// the runtime's own may be attached to the process. Another filter would see
/// the runtime's own calls (its readability probes, clone3, close,
/// pidfd_send_signal, exit_group) and could fail them or fake their results,
/// and it could fake any evidence of itself the process might look for
/// (PR_GET_SECCOMP, /proc/self/status). So the registering side establishes
/// it from outside the process: Hermit's coordinator reads the process's
/// `Seccomp_filters` count through the `/proc/<pid>` directory it holds for
/// every admitted process. The runtime guarantees the rest: registration
/// requires the runtime's filter to be installed already, so every later
/// guest syscall is intercepted, and once a hook is registered a guest call
/// that installs a filter is refused with EOPNOTSUPP.
///
/// Threat model: like every in-guest runtime structure, this mechanism
/// assumes the guest does not deliberately corrupt the runtime's memory. The
/// guest shares the runtime's address space and could overwrite the runtime's
/// private page, or replace its mapping, with ordinary stores or mapping
/// calls; that is outside these guarantees. What is guaranteed is that no
/// value the runtime reads after a kernel write (the pidfd and its
/// fstatfs/fstat results) sits in memory a well-behaved guest's own outputs or
/// mappings can reach.
///
/// Scope: the in-guest Tool host's dispatch (a full Tool, with single-threaded
/// guest processes). Calls a built-in tool forwards (`install_builtin`), and
/// syscalls a Tool makes itself through its trusted path, do not pass the
/// creation report or the filter refusal; a Tool that registers a hook must
/// not install a filter or create a process through those paths, and a
/// built-in-tool process must not register one.
///
/// With a hook registered,
/// the runtime performs every process creation as `clone3` with a
/// runtime-owned `CLONE_PIDFD` output, to report the child's birth identity;
/// without one, creations run exactly as before. Fails if a hook is already
/// registered, or if the kernel's pidfds are not pidfs inodes.
pub fn set_physical_creation_hook(hook: PhysicalCreationHook) -> Result<(), CreationHookError> {
    if !crate::seccomp::runtime_filter_installed() {
        return Err(CreationHookError::InterceptionNotInstalled);
    }
    creation_storage().map_err(CreationHookError::StorageUnavailable)?;
    pidfs_available()?;
    CREATION_HOOK
        .set(hook)
        .map_err(|_| CreationHookError::AlreadyRegistered)
}

/// The filesystem magic of pidfs (`PIDFS_MAGIC`), where Linux 6.9 and later
/// give each process one pidfd inode.
const PIDFS_MAGIC: i64 = 0x5049_4446;

/// The address of the four-byte word the kernel writes the runtime's pidfd
/// into: the start of a page the runtime maps privately and anonymously at
/// registration. Nothing else can alias it. A guest-controlled stack could be
/// a shared mapping whose bytes the guest also maps elsewhere and names as a
/// TID output; a private anonymous page has no other mapping of its bytes,
/// and after a fork the child's TID stores reach only its own copy.
/// Inherited by fork children.
fn creation_storage() -> Result<u64, i32> {
    static STORAGE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    if let Some(address) = STORAGE.get() {
        return Ok(*address);
    }
    let mapped = unsafe {
        raw_syscall6(
            libc::SYS_mmap,
            [
                0,
                4096,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                u64::MAX,
                0,
            ],
        )
    };
    if (-4095..0).contains(&mapped) {
        return Err((-mapped) as i32);
    }
    Ok(*STORAGE.get_or_init(|| mapped as u64))
}

/// Whether a pidfd for this process is a pidfs inode. With no filter but the
/// runtime's (the prerequisite), closing the probe cannot fail but for a
/// kernel fault; if it does, the process ends rather than continue with the
/// probe's descriptor open.
fn pidfs_available() -> Result<(), CreationHookError> {
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let pidfd = unsafe { raw_syscall6(libc::SYS_pidfd_open, [pid as u64, 0, 0, 0, 0, 0]) };
    if pidfd < 0 {
        return Err(CreationHookError::PidfsUnavailable);
    }
    let identity = pidfs_identity(pidfd as libc::c_int);
    if close_runtime_descriptor(pidfd as libc::c_int).is_err() {
        end_process();
    }
    identity
        .map(|_| ())
        .ok_or(CreationHookError::PidfsUnavailable)
}

/// Ends the process with status 125.
fn end_process() -> ! {
    unsafe {
        let _ = raw_syscall6(libc::SYS_exit_group, [125, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Closes a descriptor the runtime opened, and confirms that Linux released
/// it: a seccomp filter can report success or EINTR without running close.
/// Linux releases the descriptor even when close reports EINTR. Fails with
/// close's errno, or 0 if the descriptor is still open.
fn close_runtime_descriptor(fd: libc::c_int) -> Result<(), i32> {
    let closed = unsafe { raw_syscall6(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) };
    if closed < 0 && closed != -i64::from(libc::EINTR) {
        return Err((-closed) as i32);
    }
    let open = unsafe {
        raw_syscall6(
            libc::SYS_fcntl,
            [fd as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
        )
    };
    if open == -i64::from(libc::EBADF) {
        Ok(())
    } else {
        Err(if closed < 0 { (-closed) as i32 } else { 0 })
    }
}

/// Whether a guest call installs a seccomp filter, with its arguments read at
/// the kernel's widths: seccomp's operation is an unsigned int and prctl's
/// option an int; prctl's mode is an unsigned long.
fn installs_seccomp_filter(number: i64, args: [u64; 6]) -> bool {
    (number == libc::SYS_seccomp && args[0] as u32 == libc::SECCOMP_SET_MODE_FILTER)
        || (number == libc::SYS_prctl
            && args[0] as u32 as i32 == libc::PR_SET_SECCOMP
            && args[1] == u64::from(libc::SECCOMP_MODE_FILTER))
}

/// Whether a guest call would give memory a protection key other than 0: an
/// execute-only mapping (protection exactly `PROT_EXEC` once Linux has
/// dropped the `PROT_GROWSDOWN`/`PROT_GROWSUP` modifiers), which Linux backs
/// with an implicit key, or `pkey_mprotect` naming a key other than 0 or -1
/// (-1 is plain `mprotect`).
fn assigns_protection_key(number: i64, args: [u64; 6]) -> bool {
    let prot = match number {
        libc::SYS_mmap | libc::SYS_mprotect => args[2],
        libc::SYS_pkey_mprotect => {
            if !matches!(args[3] as i32, 0 | -1) {
                return true;
            }
            args[2]
        }
        _ => return false,
    };
    prot as i32 & !(libc::PROT_GROWSDOWN | libc::PROT_GROWSUP) == libc::PROT_EXEC
}

/// A guest call that installs a seccomp filter is refused once a creation
/// hook is registered (see [`set_physical_creation_hook`]), and in a process
/// that admits guest SIGALRM handlers: a guest filter could make the
/// runtime's own signal calls report success without running
/// ([`super::sigalrm::record_filter_baseline`]).
fn guest_seccomp_filter_policy(number: i64, args: [u64; 6]) -> Option<Errno> {
    (installs_seccomp_filter(number, args)
        && (CREATION_HOOK.get().is_some() || super::sigalrm::admitted()))
    .then_some(Errno::EOPNOTSUPP)
}

/// The birth identity of `pidfd`: its inode number, if it is a pidfs inode.
/// Before pidfs every pidfd shares one anonymous inode, which names no
/// process.
fn pidfs_identity(pidfd: libc::c_int) -> Option<u64> {
    // The kernel's results go to the runtime's private page when it exists
    // (see `creation_storage`), past the pidfd word, rather than to a stack
    // the guest's own mappings might alias.
    const STATFS_OFFSET: usize = 64;
    const STAT_OFFSET: usize = 512;
    let mut local_statfs = core::mem::MaybeUninit::<libc::statfs>::zeroed();
    let mut local_stat = core::mem::MaybeUninit::<libc::stat>::zeroed();
    let (filesystem, metadata) = match creation_storage() {
        Ok(page) => (
            (page as usize + STATFS_OFFSET) as *mut libc::statfs,
            (page as usize + STAT_OFFSET) as *mut libc::stat,
        ),
        Err(_) => (local_statfs.as_mut_ptr(), local_stat.as_mut_ptr()),
    };
    let statfs = unsafe {
        raw_syscall6(
            libc::SYS_fstatfs,
            [pidfd as u64, filesystem as u64, 0, 0, 0, 0],
        )
    };
    if statfs != 0 {
        return None;
    }
    // `f_type`'s width differs between targets.
    #[allow(clippy::unnecessary_cast)]
    let magic =
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*filesystem).f_type)) } as i64;
    if magic != PIDFS_MAGIC {
        return None;
    }
    let stat =
        unsafe { raw_syscall6(libc::SYS_fstat, [pidfd as u64, metadata as u64, 0, 0, 0, 0]) };
    (stat == 0)
        .then(|| unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*metadata).st_ino)) })
}

/// The end of user memory on x86-64 with four-level paging (TASK_SIZE_MAX).
const FOUR_LEVEL_USER_LIMIT: usize = (1 << 47) - 4096;
/// The same with five-level paging.
const FIVE_LEVEL_USER_LIMIT: usize = (1 << 56) - 4096;

/// Linux's limit on a clone3 `size` (PAGE_SIZE); a larger one is E2BIG.
const CLONE_ARGS_SIZE_LIMIT: usize = 4096;
/// The `struct clone_args` size Linux knows (CLONE_ARGS_SIZE_VER2). It copies
/// this much first, then requires any longer record's tail to be zero.
const CLONE_ARGS_SIZE_KNOWN: usize = 88;

/// A creation the runtime did not perform.
enum Unperformed {
    /// The guest's own call would fail with this errno.
    Failed(i32),
    Refused(CreationRefusal),
}

/// Whether the guest's protection keys deny the runtime's key-0 memory,
/// which the kernel must read and write under them.
fn guest_keys_deny_runtime_memory(permissions: Option<u32>) -> bool {
    // PKRU holds an access-disable and a write-disable bit per key; key 0's
    // are bits 0 and 1.
    permissions.is_some_and(|pkru| pkru & 0b11 != 0)
}

/// Fills `record` with the `clone3` arguments that perform the plain-fork
/// creation `number`/`args` with a `CLONE_PIDFD` output at `pidfd`, and
/// returns their size. Callers have already accepted the call with
/// [`is_plain_fork`].
fn clone3_record_for(
    number: i64,
    args: [u64; 6],
    pidfd: u64,
    permissions: Option<u32>,
    record: &mut [u64; CLONE_ARGS_SIZE_LIMIT / 8],
) -> Result<usize, Unperformed> {
    const PIDFD: u64 = libc::CLONE_PIDFD as u64;
    const SIGNAL_MASK: u64 = 0xff;
    const SIZE_VER0: usize = 64;
    let fields: [u64; 8] = match number {
        // The copying vfork stays a COW fork with the parent waiting: no
        // CLONE_VM or CLONE_VFORK is applied physically.
        libc::SYS_fork | libc::SYS_vfork => [PIDFD, pidfd, 0, 0, libc::SIGCHLD as u64, 0, 0, 0],
        // Legacy clone(flags, stack, parent_tid, child_tid, tls): the CSIGNAL
        // byte becomes exit_signal, and each TID output keeps its own
        // destination, separate from the pidfd output. The plain-fork subset
        // has no stack and no CLONE_SETTLS.
        libc::SYS_clone => {
            let flags = args[0];
            let child_tid_flags = (libc::CLONE_CHILD_SETTID | libc::CLONE_CHILD_CLEARTID) as u64;
            let child_tid = if flags & child_tid_flags != 0 {
                args[3]
            } else {
                0
            };
            let parent_tid = if flags & libc::CLONE_PARENT_SETTID as u64 != 0 {
                args[2]
            } else {
                0
            };
            [
                (flags & !SIGNAL_MASK) | PIDFD,
                pidfd,
                child_tid,
                parent_tid,
                flags & SIGNAL_MASK,
                0,
                0,
                0,
            ]
        }
        // The guest's whole record at its own size, so Linux checks the size
        // and the tail on the same bytes, plus the runtime's CLONE_PIDFD
        // output.
        libc::SYS_clone3 => {
            let size = copy_guest_clone_args(args[0], args[1], permissions, record)?;
            // The copy is what runs, and the guest's memory may have changed
            // since the call was classified (a peer sharing it): check the
            // copy itself before adding the runtime's output.
            if !clone_args_are_plain_fork(&record[..]) {
                return Err(Unperformed::Refused(CreationRefusal::ArgumentsChanged));
            }
            record[0] |= PIDFD;
            record[1] = pidfd;
            return Ok(size);
        }
        _ => unreachable!("is_plain_fork accepts only fork, vfork, clone and clone3"),
    };
    record[..fields.len()].copy_from_slice(&fields);
    Ok(SIZE_VER0)
}

/// Copies the guest's `clone_args` (`size` bytes at `address`) into `record`,
/// failing exactly where Linux's own reading (`copy_struct_from_user`) fails,
/// or refusing where the runtime cannot tell what Linux would return.
///
/// Linux reads the record under the guest's protection keys and native page
/// permissions. For a record longer than the 88 bytes it knows, it first
/// checks that the tail is zero, in ascending order: a nonzero byte is E2BIG,
/// and a fault before any nonzero byte is EFAULT. Only then does it copy the
/// first 88 bytes, where a fault is EFAULT.
fn copy_guest_clone_args(
    address: u64,
    size: u64,
    permissions: Option<u32>,
    record: &mut [u64; CLONE_ARGS_SIZE_LIMIT / 8],
) -> Result<usize, Unperformed> {
    const PAGE: usize = 4096;
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    if size > CLONE_ARGS_SIZE_LIMIT {
        return Err(Unperformed::Failed(libc::E2BIG));
    }
    let start = address as usize;
    let Some(end) = start.checked_add(size) else {
        return Err(Unperformed::Failed(libc::EFAULT));
    };
    let unknown = || Unperformed::Refused(CreationRefusal::ArgumentsUnreadable);
    // Linux first checks that the whole range is user memory (access_ok, on
    // the tail and then the head), before reading any of it. The limit is
    // the paging mode's highest user address.
    if end > FIVE_LEVEL_USER_LIMIT {
        return Err(Unperformed::Failed(libc::EFAULT));
    }
    if end > FOUR_LEVEL_USER_LIMIT {
        // EFAULT with four-level paging, readable with five: which one is
        // not known here.
        return Err(unknown());
    }
    let bytes = unsafe { core::slice::from_raw_parts_mut(record.as_mut_ptr().cast::<u8>(), size) };
    // Checks [from, to) page by page, copying each readable page's bytes.
    // Stops at the first page Linux cannot read, or (with `zero_tail`) at the
    // first page holding a nonzero byte.
    let mut walk = |from: usize, to: usize, zero_tail: bool| -> Result<(), Unperformed> {
        if from >= to {
            return Ok(());
        }
        let mut page = from & !(PAGE - 1);
        while page < to {
            let low = page.max(from);
            let high = (page + PAGE).min(to);
            match native_readability(low & !3, permissions) {
                Readability::Readable => {}
                Readability::Unreadable => return Err(Unperformed::Failed(libc::EFAULT)),
                Readability::Unknown => return Err(unknown()),
            }
            if !read_guest(low, &mut bytes[low - start..high - start]) {
                return Err(unknown());
            }
            if zero_tail
                && bytes[low - start..high - start]
                    .iter()
                    .any(|byte| *byte != 0)
            {
                return Err(Unperformed::Failed(libc::E2BIG));
            }
            page += PAGE;
        }
        Ok(())
    };
    let head_end = start + size.min(CLONE_ARGS_SIZE_KNOWN);
    walk(head_end, end, true)?;
    walk(start, head_end, false)?;
    Ok(size)
}

/// Copies guest memory at `address` into `buffer` without the guest's
/// protection keys (process_vm_readv); false unless every byte was copied.
fn read_guest(address: usize, buffer: &mut [u8]) -> bool {
    let local = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast(),
        iov_len: buffer.len(),
    };
    let remote = libc::iovec {
        iov_base: address as *mut libc::c_void,
        iov_len: buffer.len(),
    };
    let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) };
    let read = unsafe {
        raw_syscall6(
            libc::SYS_process_vm_readv,
            [
                pid as u64,
                (&raw const local) as u64,
                1,
                (&raw const remote) as u64,
                1,
                0,
            ],
        )
    };
    read == buffer.len() as i64
}

/// What Linux's own read of a page would do, under the guest's rights.
enum Readability {
    Readable,
    Unreadable,
    /// The probes proved neither a read nor a fault.
    Unknown,
}

/// Whether Linux can read the aligned word at `address` under the guest's
/// protection keys and native page permissions. Two `FUTEX_WAIT`s with a
/// zero timeout read it without any effect: one expecting the word's actual
/// value (copied without the keys) must time out, and one expecting another
/// value must return EAGAIN; each faults with EFAULT where Linux cannot
/// read. A result that a fixed errno could fake is not accepted as either.
/// When the runtime cannot copy the word, both probes faulting still proves
/// that Linux cannot read it; any other result is unknown.
fn native_readability(address: usize, permissions: Option<u32>) -> Readability {
    let mut word = [0_u8; 4];
    let copied = read_guest(address, &mut word);
    let actual = u32::from_ne_bytes(word);
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let wait = |expected: u32| unsafe {
        crate::trap::raw_syscall6_with_result(
            libc::SYS_futex,
            [
                address as u64,
                (libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG) as u64,
                u64::from(expected),
                (&raw const timeout) as u64,
                0,
                0,
            ],
            permissions,
        )
    }
    .result;
    let fault = -i64::from(libc::EFAULT);
    match (wait(actual), wait(!actual)) {
        (equal, different)
            if copied
                && equal == -i64::from(libc::ETIMEDOUT)
                && different == -i64::from(libc::EAGAIN) =>
        {
            Readability::Readable
        }
        (equal, different) if equal == fault && different == fault => Readability::Unreadable,
        _ => Readability::Unknown,
    }
}

/// Whether a clone3 record's parent TID output (`CLONE_PARENT_SETTID`, field
/// 3) overlaps its pidfd output (field 1); both are four bytes. Linux refuses
/// an equal pair with EINVAL, and for an overlapping one writes the pidfd
/// first and then the child's pid over part of it, so the runtime would read
/// a different descriptor number than the one it owns.
fn parent_tid_aliases_pidfd(record: &[u64]) -> bool {
    let overlaps = |a: u64, b: u64| a < b.saturating_add(4) && b < a.saturating_add(4);
    record[0] & libc::CLONE_PARENT_SETTID as u64 != 0 && overlaps(record[3], record[1])
}

/// Whether a physical clone3 errno is one the guest's own call returns for
/// the same reason; any other is refused (see
/// [`CreationRefusal::RuntimeError`]).
fn errno_is_the_guests(number: i64, errno: i32) -> bool {
    matches!(errno, libc::EAGAIN | libc::ENOSPC)
        || (number == libc::SYS_clone3 && matches!(errno, libc::E2BIG | libc::EINVAL))
}

/// Reports `creation` to `hook`; ends the process if the hook fails or the
/// creation was refused.
fn report_creation(hook: PhysicalCreationHook, creation: PhysicalCreation) {
    let refused = matches!(creation, PhysicalCreation::Refused { .. });
    if hook(creation).is_err() || refused {
        end_process();
    }
}

/// Performs a plain-fork creation as clone3 with the runtime's CLONE_PIDFD
/// output, and reports it to `hook`. Never runs the guest's call another way.
fn reported_creation(
    hook: PhysicalCreationHook,
    number: i64,
    args: [u64; 6],
    guest_pkru: Option<&mut Option<u32>>,
) -> i64 {
    let permissions = guest_pkru.as_ref().and_then(|value| **value);
    let refuse = |refusal, child| {
        report_creation(hook, PhysicalCreation::Refused { refusal, child });
        unreachable!("a refused creation ends the process");
    };
    // The record (on this stack) and the pidfd output (the runtime's private
    // page) carry key 0, and the kernel reads and writes them under the
    // guest's protection keys.
    if guest_keys_deny_runtime_memory(permissions) {
        refuse(CreationRefusal::ProtectionKeyDenied, None);
    }
    // Mapped at registration, so this does not fail here.
    let Ok(pidfd_output) = creation_storage() else {
        refuse(CreationRefusal::RuntimeError(libc::ENOMEM), None);
        unreachable!();
    };
    let pidfd_word = pidfd_output as *mut libc::c_int;
    unsafe { pidfd_word.write_volatile(-1) };
    let mut record = [0_u64; CLONE_ARGS_SIZE_LIMIT / 8];
    let size = match clone3_record_for(number, args, pidfd_output, permissions, &mut record) {
        Ok(size) if parent_tid_aliases_pidfd(&record) => {
            let _ = size;
            refuse(CreationRefusal::OutputAliased, None);
            unreachable!();
        }
        Ok(size) => size,
        Err(Unperformed::Failed(errno)) => {
            report_creation(hook, PhysicalCreation::Failed { errno });
            return -i64::from(errno);
        }
        Err(Unperformed::Refused(refusal)) => {
            refuse(refusal, None);
            unreachable!();
        }
    };
    let physical = unsafe {
        crate::trap::raw_syscall6_with_result(
            libc::SYS_clone3,
            [record.as_ptr() as u64, size as u64, 0, 0, 0, 0],
            permissions,
        )
    };
    if let Some(output) = guest_pkru {
        *output = physical.pkru;
    }
    let result = physical.result;
    if result == 0 {
        // The child, which holds no copy of the pidfd: the kernel installs
        // it in the parent's table only.
        return 0;
    }
    if result < 0 {
        let errno = (-result) as i32;
        if !errno_is_the_guests(number, errno) {
            refuse(CreationRefusal::RuntimeError(errno), None);
        }
        report_creation(hook, PhysicalCreation::Failed { errno });
        return result;
    }
    let pid = result as i32;
    let pidfd = unsafe { pidfd_word.read_volatile() };
    let refused_child = || {
        let sent = unsafe {
            raw_syscall6(
                libc::SYS_pidfd_send_signal,
                [pidfd as u64, libc::SIGKILL as u64, 0, 0, 0, 0],
            )
        };
        Some(RefusedChild {
            pid,
            killed: sent == 0,
        })
    };
    let Some(birth_identity) = pidfs_identity(pidfd) else {
        let child = refused_child();
        // A child that could not be killed keeps its handle until the
        // process ends.
        if child.is_some_and(|child| child.killed) {
            let _ = close_runtime_descriptor(pidfd);
        }
        refuse(CreationRefusal::IdentityUnavailable, child);
        unreachable!();
    };
    if let Err(errno) = close_runtime_descriptor(pidfd) {
        // The descriptor may still be open, so the kill can still use it.
        refuse(CreationRefusal::CloseFailed(errno), refused_child());
    }
    report_creation(
        hook,
        PhysicalCreation::Created {
            pid,
            birth_identity,
        },
    );
    result
}

fn forward_plain_fork(number: i64, args: [u64; 6], guest_pkru: Option<&mut Option<u32>>) -> i64 {
    let result = if let Some(hook) = CREATION_HOOK.get().copied() {
        reported_creation(hook, number, args, guest_pkru)
    } else {
        let permissions = guest_pkru.as_ref().and_then(|value| **value);
        let physical = if number == libc::SYS_vfork {
            // A real vfork child would run the instrumentation callback on the
            // parent's shared stack. Use a COW fork and preserve vfork's parent
            // suspension until the child exits. Exec remains fail-closed, so exit
            // is the only supported vfork completion boundary for now.
            unsafe { crate::trap::raw_syscall6_with_result(libc::SYS_fork, [0; 6], permissions) }
        } else {
            unsafe { crate::trap::raw_syscall6_with_result(number, args, permissions) }
        };
        if let Some(output) = guest_pkru {
            *output = physical.pkru;
        }
        physical.result
    };
    if number == libc::SYS_vfork && result > 0 {
        let mut info = core::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        loop {
            let waited = unsafe {
                raw_syscall6(
                    libc::SYS_waitid,
                    [
                        libc::P_PID as u64,
                        result as u64,
                        info.as_mut_ptr() as u64,
                        (libc::WEXITED | libc::WNOWAIT) as u64,
                        0,
                        0,
                    ],
                )
            };
            if waited == -i64::from(libc::EINTR) {
                continue;
            }
            if waited < 0 {
                return waited;
            }
            break;
        }
    }
    result
}

// TODO-HUMAN-REVIEW(PR-127): Review injected process/signal safety policy.
fn injected_syscall_guard(
    runtime: &impl HostRuntime,
    number: i64,
    args: [u64; 6],
) -> Option<Errno> {
    // Every forwarded guest call passes here, so this is where a guest's own
    // seccomp filter is noticed or refused.
    if let Some(error) = guest_seccomp_filter_policy(number, args) {
        return Some(error);
    }
    // Signal phase 1: the runtime's virtual signal calls copy guest memory
    // with every protection key open, so a process that admits SIGALRM
    // handlers allocates none: Linux's answer when no key is left.
    if number == libc::SYS_pkey_alloc && super::sigalrm::admitted() {
        return Some(Errno::ENOSPC);
    }
    // Nor gives memory any key but 0: Linux backs an execute-only mapping
    // with an implicit key of its own.
    if super::sigalrm::admitted() && assigns_protection_key(number, args) {
        return Some(Errno::EPERM);
    }
    // Signal phase 1: an accepted SIGALRM restorer's page stays mapped,
    // unchanged, for the rest of the process's life.
    if super::restorer::mapping_change_refused(number, args) {
        return Some(Errno::EPERM);
    }
    let unsupported_process =
        // AUTONOMOUS-BOT-IMPLEMENTED
        (matches!(number, libc::SYS_clone | libc::SYS_clone3 | libc::SYS_vfork)
            && !is_plain_fork(number, args))
        // AUTONOMOUS-BOT-IMPLEMENTED
        || matches!(number, libc::SYS_execve | libc::SYS_execveat);
    let protected_signal =
        // AUTONOMOUS-BOT-IMPLEMENTED
        // TODO-HUMAN-REVIEW(PR-133): Review fail-closed guest signal-handler policy.
        (!runtime.signal_action_supported(number, args)
            && !super::sigalrm::decides_action(number, args))
        // AUTONOMOUS-BOT-IMPLEMENTED
        || (number == libc::SYS_sigaltstack && args[0] != 0);

    if unsupported_process {
        Some(Errno::EOPNOTSUPP)
    } else if protected_signal {
        Some(Errno::EPERM)
    } else {
        None
    }
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-913): Review stripping reserved signals from
// rt_sigprocmask sets instead of refusing them.
/// For an `rt_sigprocmask` that installs a set, returns a copy of that set
/// without [`HostRuntime::reserved_signal_mask`], to forward in its place.
///
/// Linux silently drops SIGKILL and SIGSTOP from such a set; the reserved
/// signals are dropped the same way. A guest, or a Tool such as Detcore that
/// blocks every signal around a blocking `wait4`, can then block everything
/// else and keep running, instead of failing with EPERM. A later read of the
/// mask shows the reserved signals unblocked, which Linux would not. A reserved
/// signal that the guest asked to block also stays deliverable: one sent with
/// `kill`, `tgkill` or `sigqueue` reaches the runtime's handler, which ends the
/// process with status 126 for SIGSYS and applies the default action to
/// SIGSEGV, where Linux would hold it pending for `rt_sigpending`,
/// `rt_sigtimedwait` or `signalfd` to observe. Both deviations are tracked in
/// <https://github.com/rrnewton/reverie/issues/915>.
///
/// `Ok(None)` forwards the call unchanged: it installs no set, or its size is
/// not 8, which Linux rejects with EINVAL before reading the set. A set that
/// cannot be read returns `Err(EFAULT)`, as Linux would, and nothing is
/// forwarded, so an unstripped set never reaches Linux.
///
/// The set is copied by `process_vm_writev` from this process to itself: the
/// kernel reads the source with the same user copy `rt_sigprocmask` uses, so
/// any set Linux could read is read here, including one on a write-only page,
/// and an unreadable one fails without a fault in the runtime.
///
/// The copy, and the stripped call that writes the old set, run with the
/// calling thread's current protection-key rights; neither switches PKRU. At an
/// installed hook those are the guest's rights, so a set or old set in memory
/// whose key the guest's PKRU denies fails with EFAULT, as on Linux. On the
/// SIGSYS fallback path the runtime has opened every key, so the same call is
/// accepted there and the old set is written.
///
/// A seccomp filter the guest added sees calls Linux would not make. If it
/// refuses `gettid` or `process_vm_writev` with an error, that error is returned
/// instead of EFAULT; if it kills or traps on either, that happens to a call
/// Linux would have accepted. A filter that inspects `rt_sigprocmask`'s
/// arguments sees the runtime's set pointer in place of the guest's, so one
/// that allows only the guest's pointer refuses the stripped call. Each case
/// fails closed: no unstripped set is installed.
fn stripped_signal_mask(
    runtime: &impl HostRuntime,
    number: i64,
    args: [u64; 6],
) -> Result<Option<u64>, Errno> {
    const SIGSET_SIZE: u64 = core::mem::size_of::<u64>() as u64;
    if number != libc::SYS_rt_sigprocmask || args[1] == 0 || args[3] != SIGSET_SIZE {
        return Ok(None);
    }
    let mut requested = 0_u64;
    // The guest's set is the local source; this runtime's copy is the remote
    // destination, written into the same address space.
    let source = libc::iovec {
        iov_base: args[1] as usize as *mut libc::c_void,
        iov_len: SIGSET_SIZE as usize,
    };
    let destination = libc::iovec {
        iov_base: (&raw mut requested).cast(),
        iov_len: SIGSET_SIZE as usize,
    };
    // A thread id names this address space even after the thread-group leader
    // has exited, when its process id no longer does.
    let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) };
    if tid < 0 {
        return Err(Errno::new(-tid as i32));
    }
    let copied = unsafe {
        raw_syscall6(
            libc::SYS_process_vm_writev,
            [
                tid as u64,
                (&raw const source) as u64,
                1,
                (&raw const destination) as u64,
                1,
                0,
            ],
        )
    };
    if copied == SIGSET_SIZE as i64 {
        Ok(Some(requested & !runtime.reserved_signal_mask()))
    } else if copied >= 0 || copied == -i64::from(libc::EFAULT) {
        // Part of the set, or none of it, could be read.
        Err(Errno::EFAULT)
    } else {
        Err(Errno::new(-copied as i32))
    }
}

#[reverie::tool]
impl<T: Tool, R: HostRuntime> Guest<T> for InGuest<'_, T, R> {
    type Memory = LocalMemory;
    type Stack = LocalStack;

    fn tid(&self) -> Pid {
        self.tid
    }

    fn pid(&self) -> Pid {
        self.pid
    }

    fn ppid(&self) -> Option<Pid> {
        self.ppid
    }

    // This in-process host has no begin/ready runtime window: install_runtime
    // prepares instrumentation before reverie_inguest::install arms the
    // seccomp filter through which the Tool receives syscalls, so the
    // runtime's own preparation is never delivered to it.
    fn is_backend_runtime_bootstrap(&self) -> bool {
        false
    }

    fn memory(&self) -> Self::Memory {
        LocalMemory::new()
    }

    fn thread_state_mut(&mut self) -> &mut T::ThreadState {
        self.state
    }

    fn thread_state(&self) -> &T::ThreadState {
        self.state
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        let mut regs = unsafe { core::mem::zeroed::<libc::user_regs_struct>() };
        if self.event.context != 0 {
            let context = unsafe { &*(self.event.context as *const RegisterContext) };
            regs.r15 = context.r15;
            regs.r14 = context.r14;
            regs.r13 = context.r13;
            regs.r12 = context.r12;
            regs.rbp = context.rbp;
            regs.rbx = context.rbx;
            regs.r11 = context.r11;
            regs.r10 = context.r10;
            regs.r9 = context.r9;
            regs.r8 = context.r8;
            regs.rax = context.rax;
            regs.rcx = context.rcx;
            regs.rdx = context.rdx;
            regs.rsi = context.rsi;
            regs.rdi = context.rdi;
            regs.orig_rax = context.rax;
            regs.rip = context.instruction_pointer;
            regs.rsp = context.stack_pointer;
            regs.eflags = context.rflags;
            return regs;
        }
        regs.rax = self.event.number as u64;
        regs.orig_rax = self.event.number as u64;
        regs.rdi = self.event.args[0];
        regs.rsi = self.event.args[1];
        regs.rdx = self.event.args[2];
        regs.r10 = self.event.args[3];
        regs.r8 = self.event.args[4];
        regs.r9 = self.event.args[5];
        regs.rip = self.event.instruction_pointer;
        regs
    }
    async fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), Error> {
        self.event.number = regs.rax as i64;
        self.event.args = [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9];
        if self.event.context != 0 {
            let context = unsafe { &mut *(self.event.context as *mut RegisterContext) };
            context.r15 = regs.r15;
            context.r14 = regs.r14;
            context.r13 = regs.r13;
            context.r12 = regs.r12;
            context.rbp = regs.rbp;
            context.rbx = regs.rbx;
            context.r11 = regs.r11;
            context.r10 = regs.r10;
            context.r9 = regs.r9;
            context.r8 = regs.r8;
            context.rax = regs.rax;
            context.rcx = regs.rcx;
            context.rdx = regs.rdx;
            context.rsi = regs.rsi;
            context.rdi = regs.rdi;
            context.stack_pointer = regs.rsp;
            context.rflags = regs.eflags;
        }
        Ok(())
    }
    async fn stack(&mut self) -> Self::Stack {
        LocalStack::new()
    }

    async fn daemonize(&mut self) {}

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        let (number, args) = syscall.into_parts();
        let number = number.id() as i64;
        let mut raw_args = [
            args.arg0 as u64,
            args.arg1 as u64,
            args.arg2 as u64,
            args.arg3 as u64,
            args.arg4 as u64,
            args.arg5 as u64,
        ];

        if is_plain_fork(number, raw_args) {
            let parent_tid = self.tid;
            let parent_pid = self.pid;
            self.prepare_fork_parent_state();
            let result = forward_plain_fork(number, raw_args, None);
            if result == 0 {
                let child_tid = raw_pid(libc::SYS_gettid);
                let child_pid = raw_pid(libc::SYS_getpid);
                self.tail
                    .set_fork_child(parent_tid, parent_pid, child_tid, child_pid);
                return std::future::pending().await;
            }
            return Errno::from_ret(result as usize).map(|value| value as i64);
        }

        // AUTONOMOUS-BOT-IMPLEMENTED
        if matches!(number, libc::SYS_clone | libc::SYS_clone3 | libc::SYS_vfork) {
            const MESSAGE: &[u8] = b"reverie-liteinst: clone injection requires ptrace fallback\n";
            unsafe {
                let _ = raw_syscall6(
                    libc::SYS_write,
                    [
                        libc::STDERR_FILENO as u64,
                        MESSAGE.as_ptr() as u64,
                        MESSAGE.len() as u64,
                        0,
                        0,
                        0,
                    ],
                );
            }
            return Err(Errno::EOPNOTSUPP);
        }
        if let Some(error) = injected_syscall_guard(self.runtime, number, raw_args) {
            return Err(error);
        }
        if let Some(result) =
            unsafe { super::protect::protect_forwarded_descriptor_change(number, raw_args) }
        {
            return Errno::from_ret(result as usize).map(|value| value as i64);
        }
        if is_exit_syscall(number) {
            self.tail.set_exit(number, raw_args);
            return std::future::pending().await;
        }
        if let Some(result) = virtual_signal_call(
            self.runtime,
            self.event.guest_pkru,
            self.event.args,
            number,
            raw_args,
        ) {
            return Errno::from_ret(result as usize).map(|value| value as i64);
        }
        let kernel_signal_mask = stripped_signal_mask(self.runtime, number, raw_args)?;
        if let Some(mask) = kernel_signal_mask.as_ref() {
            raw_args[1] = mask as *const u64 as u64;
        }

        let result = unsafe { raw_syscall6(number, raw_args) };
        Errno::from_ret(result as usize).map(|value| value as i64)
    }

    /// Signal phase 1, step I4: deliver a SIGALRM the Tool has committed (the
    /// ledger entry is already taken) as this fallback syscall completes. Only
    /// a guest SIGALRM handler kept virtual by this runtime can be the target;
    /// everything else keeps the default refusal.
    async fn defer_signal_delivery(&mut self, event: reverie::SignalEvent) -> Result<(), Error> {
        if event.signal() != libc::SIGALRM || !super::sigalrm::handled() {
            return Err(Errno::ENOSYS.into());
        }
        // SAFETY: inside the guest call's own turn, in its dispatch.
        unsafe { super::sigalrm::prepare_delivery(self.runtime.reserved_signal_mask()) }
            .map_err(|errno| Error::Errno(Errno::new(errno)))
    }

    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        let (number, syscall_args) = syscall.into_parts();
        let args = [
            syscall_args.arg0 as u64,
            syscall_args.arg1 as u64,
            syscall_args.arg2 as u64,
            syscall_args.arg3 as u64,
            syscall_args.arg4 as u64,
            syscall_args.arg5 as u64,
        ];
        let number = number.id() as i64;
        if is_plain_fork(number, args) {
            let parent_tid = self.tid;
            let parent_pid = self.pid;
            self.prepare_fork_parent_state();
            let result = forward_plain_fork(number, args, None);
            if result == 0 {
                self.tail.set_fork_child(
                    parent_tid,
                    parent_pid,
                    raw_pid(libc::SYS_gettid),
                    raw_pid(libc::SYS_getpid),
                );
            } else {
                self.tail.set_result(result);
            }
        } else if let Some(error) = injected_syscall_guard(self.runtime, number, args) {
            self.tail.set_result(-i64::from(error.into_raw()));
        } else if let Some(result) =
            unsafe { super::protect::protect_forwarded_descriptor_change(number, args) }
        {
            self.tail.set_result(result);
        } else if is_exit_syscall(number) {
            self.tail.set_exit(number, args);
        } else if let Some(result) = virtual_signal_call(
            self.runtime,
            self.event.guest_pkru,
            self.event.args,
            number,
            args,
        ) {
            self.tail.set_result(result);
        } else {
            let value = match stripped_signal_mask(self.runtime, number, args) {
                Err(error) => -i64::from(error.into_raw()),
                Ok(None) => unsafe { raw_syscall6(number, args) },
                Ok(Some(mask)) => {
                    let mut args = args;
                    args[1] = (&raw const mask) as u64;
                    unsafe { raw_syscall6(number, args) }
                }
            };
            self.tail.set_result(value);
        }
        std::future::pending().await
    }

    // TODO-HUMAN-REVIEW(PR-326): Review the coarse
    // syscall-boundary clock until the minimal ptrace supervisor wires PMU delivery.
    fn set_timer(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        // Nothing in the guest arms an RCB threshold or dispatches
        // `Tool::handle_timer_event`. Accepting the request would let a Tool
        // believe a CPU-bound thread is bounded while it runs unpreempted to
        // its next syscall, so refuse, as reverie-dbt does, and let a Tool
        // that needs preemption fail closed. Clock reads still work.
        Err(Errno::ENOSYS.into())
    }

    fn set_timer_precise(&mut self, _sched: TimerSchedule) -> Result<(), Error> {
        // Same refusal as set_timer; never synthesize host time.
        Err(Errno::ENOSYS.into())
    }

    fn read_clock(&mut self) -> Result<u64, Error> {
        self.runtime.read_clock().map_err(Error::from)
    }

    fn has_cpuid_interception(&self) -> bool {
        self.cpuid_interception
    }
}

pub struct LocalStack {
    arena: Box<[u8]>,
    offset: usize,
}

impl LocalStack {
    fn new() -> Self {
        Self {
            arena: vec![0; STACK_CAPACITY].into_boxed_slice(),
            offset: 0,
        }
    }

    /// Claims the next `size_of::<V>()` bytes aligned for `V` and returns
    /// their address. Nothing is written to them.
    fn claim<V>(&mut self) -> *mut V {
        let align = core::mem::align_of::<V>();
        let base = self.arena.as_ptr() as usize;
        let start = (base + self.offset + align - 1) & !(align - 1);
        let offset = start - base;
        let end = offset + core::mem::size_of::<V>();
        assert!(end <= self.arena.len(), "LiteInst guest stack overflow");
        let pointer = unsafe { self.arena.as_mut_ptr().add(offset).cast::<V>() };
        self.offset = end;
        pointer
    }

    fn allocate<'stack, V>(&mut self, value: V) -> AddrMut<'stack, V> {
        let pointer = self.claim::<V>();
        unsafe { pointer.write(value) };
        AddrMut::from_raw(pointer as usize).expect("LiteInst stack produced a null address")
    }
}

pub struct LocalStackGuard {
    arena: Option<Box<[u8]>>,
}

impl Drop for LocalStackGuard {
    fn drop(&mut self) {
        if let Some(arena) = self.arena.take() {
            COMMITTED_STACKS.lock().push(arena);
        }
    }
}

impl Stack for LocalStack {
    type StackGuard = LocalStackGuard;

    fn size(&self) -> usize {
        self.offset
    }

    fn capacity(&self) -> usize {
        self.arena.len()
    }

    fn push<'stack, V>(&mut self, value: V) -> Addr<'stack, V> {
        self.allocate(value).into()
    }

    fn reserve<'stack, V>(&mut self) -> AddrMut<'stack, V> {
        // Zero the bytes without making a `V`: all zeros is not a valid value
        // of every type (a reference or a NonZeroU64, for instance).
        let pointer = self.claim::<V>();
        unsafe {
            pointer
                .cast::<u8>()
                .write_bytes(0, core::mem::size_of::<V>())
        };
        AddrMut::from_raw(pointer as usize).expect("LiteInst stack produced a null address")
    }

    fn commit(self) -> Result<Self::StackGuard, Errno> {
        Ok(LocalStackGuard {
            arena: Some(self.arena),
        })
    }
}

fn tool_fatal(status: i32, error: &Error) -> ! {
    let message = format!("reverie-liteinst tool error: {error:?}\n");
    unsafe {
        let _ = raw_syscall6(
            libc::SYS_write,
            [
                libc::STDERR_FILENO as u64,
                message.as_ptr() as u64,
                message.len() as u64,
                0,
                0,
                0,
            ],
        );
    }
    fatal(status)
}

fn fatal(status: i32) -> ! {
    unsafe {
        let _ = raw_syscall6(libc::SYS_exit_group, [status as u64, 0, 0, 0, 0, 0]);
    }
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_key_zero_memory_is_admitted_for_signal_phase_one() {
        let exec = libc::PROT_EXEC as u64;
        let grows = (libc::PROT_EXEC | libc::PROT_GROWSDOWN) as u64;
        let grows_up = (libc::PROT_EXEC | libc::PROT_GROWSUP) as u64;
        let read_exec = (libc::PROT_READ | libc::PROT_EXEC) as u64;
        for number in [libc::SYS_mmap, libc::SYS_mprotect] {
            assert!(assigns_protection_key(number, [0, 4096, exec, 0, 0, 0]));
            assert!(assigns_protection_key(number, [0, 4096, grows, 0, 0, 0]));
            assert!(assigns_protection_key(number, [0, 4096, grows_up, 0, 0, 0]));
            assert!(!assigns_protection_key(
                number,
                [0, 4096, read_exec, 0, 0, 0]
            ));
            assert!(!assigns_protection_key(number, [0, 4096, 0, 0, 0, 0]));
        }
        let pkey = libc::SYS_pkey_mprotect;
        assert!(assigns_protection_key(pkey, [0, 4096, read_exec, 1, 0, 0]));
        assert!(!assigns_protection_key(pkey, [0, 4096, read_exec, 0, 0, 0]));
        assert!(!assigns_protection_key(
            pkey,
            [0, 4096, read_exec, u32::MAX as u64, 0, 0]
        ));
        assert!(assigns_protection_key(
            pkey,
            [0, 4096, exec, u32::MAX as u64, 0, 0]
        ));
        assert!(!assigns_protection_key(
            libc::SYS_munmap,
            [0, 4096, exec, 0, 0, 0]
        ));
    }

    #[test]
    fn reserve_zeroes_aligned_storage_for_types_without_a_zero_value() {
        let mut stack = LocalStack::new();
        let _ = stack.push(1_u8);
        let reference = stack.reserve::<&'static u64>().as_raw();
        let nonzero = stack.reserve::<core::num::NonZeroU64>().as_raw();
        for address in [reference, nonzero] {
            assert_eq!(address % core::mem::align_of::<u64>(), 0);
            // SAFETY: the stack owns these eight bytes; they are read as bytes.
            let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, 8) };
            assert_eq!(bytes, [0; 8]);
        }
        assert_eq!(stack.size(), 24);
    }

    /// A pidfd's inode is a birth identity only on pidfs. An anonymous inode,
    /// which every pidfd shared before pidfs, is rejected.
    #[test]
    fn only_a_pidfs_inode_is_a_birth_identity() {
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) } as i32;
        assert!(pidfd >= 0, "{}", std::io::Error::last_os_error());
        let mut metadata: libc::stat = unsafe { core::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(pidfd, &mut metadata) }, 0);
        // pidfs arrived in Linux 6.9; before it, pidfds have no identity.
        let mut name: libc::utsname = unsafe { core::mem::zeroed() };
        assert_eq!(unsafe { libc::uname(&mut name) }, 0);
        let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let mut numbers = release
            .split(|c: char| !c.is_ascii_digit())
            .map(|part| part.parse::<u32>().unwrap_or(0));
        let version = (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0));
        if version >= (6, 9) {
            assert_eq!(pidfs_identity(pidfd), Some(metadata.st_ino), "{release}");
            assert!(pidfs_available().is_ok(), "{release}");
        } else {
            assert_eq!(pidfs_identity(pidfd), None, "{release}");
            assert!(
                matches!(pidfs_available(), Err(CreationHookError::PidfsUnavailable)),
                "{release}"
            );
        }
        let anonymous = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(anonymous >= 0);
        assert_eq!(pidfs_identity(anonymous), None);
        unsafe {
            libc::close(pidfd);
            libc::close(anonymous);
        }
    }

    /// Only a PKRU that denies key 0's access or writes refuses the creation.
    #[test]
    fn guest_keys_deny_runtime_memory_only_when_key_0_is_restricted() {
        assert!(!guest_keys_deny_runtime_memory(None));
        assert!(!guest_keys_deny_runtime_memory(Some(0)));
        assert!(!guest_keys_deny_runtime_memory(Some(0x5555_5554)));
        assert!(guest_keys_deny_runtime_memory(Some(0b01)));
        assert!(guest_keys_deny_runtime_memory(Some(0b10)));
    }

    /// Only errors the guest's own call returns for the same reason reach
    /// the guest; ENOMEM, which the runtime's pidfd allocation can return,
    /// does not.
    #[test]
    fn only_the_guests_own_errors_reach_the_guest() {
        for number in [libc::SYS_fork, libc::SYS_clone, libc::SYS_clone3] {
            assert!(errno_is_the_guests(number, libc::EAGAIN));
            assert!(errno_is_the_guests(number, libc::ENOSPC));
            for errno in [
                libc::ENOMEM,
                libc::EMFILE,
                libc::ENFILE,
                libc::EFAULT,
                libc::EPERM,
            ] {
                assert!(!errno_is_the_guests(number, errno), "{number} {errno}");
            }
        }
        assert!(errno_is_the_guests(libc::SYS_clone3, libc::E2BIG));
        assert!(!errno_is_the_guests(libc::SYS_fork, libc::E2BIG));
        assert!(!errno_is_the_guests(libc::SYS_fork, libc::ENOSYS));
    }

    /// Linux range-checks a whole clone3 record before reading any of it: a
    /// record ending past the highest user address is EFAULT, even with a
    /// nonzero byte in a readable part of its tail. Past the four-level limit
    /// but within the five-level one, the paging mode decides, so it is
    /// refused. Neither case reads memory.
    #[test]
    fn a_clone3_record_past_user_memory_is_range_checked_first() {
        let mut record = [0_u64; CLONE_ARGS_SIZE_LIMIT / 8];
        let failed =
            copy_guest_clone_args((FIVE_LEVEL_USER_LIMIT - 200) as u64, 300, None, &mut record);
        assert!(matches!(failed, Err(Unperformed::Failed(libc::EFAULT))));
        let refused =
            copy_guest_clone_args((FOUR_LEVEL_USER_LIMIT - 200) as u64, 300, None, &mut record);
        assert!(matches!(
            refused,
            Err(Unperformed::Refused(CreationRefusal::ArgumentsUnreadable))
        ));
    }

    /// A parent TID output overlapping the runtime's four-byte pidfd output
    /// at any offset is refused before the call; an adjacent one is not, and
    /// addresses near the top of the range do not wrap.
    #[test]
    fn a_parent_tid_overlapping_the_pidfd_output_is_refused() {
        let pidfd = 0x7000_u64;
        let set = libc::CLONE_PARENT_SETTID as u64 | libc::CLONE_PIDFD as u64;
        for offset in -3_i64..=3 {
            let parent_tid = pidfd.wrapping_add_signed(offset);
            assert!(
                parent_tid_aliases_pidfd(&[set, pidfd, 0, parent_tid]),
                "{offset}"
            );
        }
        for offset in [-4_i64, 4, 8, -8] {
            let parent_tid = pidfd.wrapping_add_signed(offset);
            assert!(
                !parent_tid_aliases_pidfd(&[set, pidfd, 0, parent_tid]),
                "{offset}"
            );
        }
        let unset = libc::CLONE_PIDFD as u64;
        assert!(!parent_tid_aliases_pidfd(&[unset, pidfd, 0, pidfd]));
        assert!(parent_tid_aliases_pidfd(&[
            set,
            u64::MAX - 1,
            0,
            u64::MAX - 2
        ]));
        assert!(!parent_tid_aliases_pidfd(&[set, u64::MAX - 1, 0, 0]));
    }

    /// The pidfd output lives in a private anonymous page of its own, which
    /// no other mapping can alias, and the same page is used every time.
    #[test]
    fn the_pidfd_output_is_a_private_anonymous_page() {
        let address = creation_storage().unwrap();
        assert_eq!(creation_storage().unwrap(), address);
        assert_eq!(address % 4096, 0);
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        let line = maps
            .lines()
            .find(|line| {
                let (range, _) = line.split_once(' ').unwrap();
                let (start, end) = range.split_once('-').unwrap();
                let start = u64::from_str_radix(start, 16).unwrap();
                let end = u64::from_str_radix(end, 16).unwrap();
                (start..end).contains(&address)
            })
            .unwrap();
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields[1], "rw-p", "{line}");
        assert_eq!(fields[4], "0", "anonymous, no inode: {line}");
        assert!(fields.len() == 5 || fields[5].starts_with('['), "{line}");
    }

    /// The copied clone3 record is checked as a plain fork itself: a copy that
    /// asks for CLONE_PIDFD, CLONE_FILES, a stack, another exit signal or a
    /// set_tid is not, whatever the earlier classification saw.
    #[test]
    fn a_copied_clone3_record_must_still_be_a_plain_fork() {
        let mut plain = [0_u64; 11];
        plain[4] = libc::SIGCHLD as u64;
        assert!(clone_args_are_plain_fork(&plain));
        plain[0] = libc::CLONE_PARENT_SETTID as u64 | libc::CLONE_CHILD_SETTID as u64;
        assert!(clone_args_are_plain_fork(&plain));
        for (field, value) in [
            (0, libc::CLONE_PIDFD as u64),
            (0, libc::CLONE_FILES as u64),
            (0, libc::CLONE_VM as u64),
            (4, libc::SIGUSR1 as u64),
            (5, 0x1000),
            (6, 0x1000),
            (8, 0x1000),
            (9, 1),
            (10, 3),
        ] {
            let mut changed = plain;
            if field == 0 {
                changed[0] |= value;
            } else {
                changed[field] = value;
            }
            assert!(!clone_args_are_plain_fork(&changed), "{field} {value:#x}");
        }
    }

    /// A hook cannot be registered before the runtime's own filter is
    /// installed (this test process has none), and nothing is registered.
    #[test]
    fn a_hook_is_refused_before_interception_is_installed() {
        fn hook(_: PhysicalCreation) -> io::Result<()> {
            Ok(())
        }
        assert!(matches!(
            set_physical_creation_hook(hook),
            Err(CreationHookError::InterceptionNotInstalled)
        ));
        assert!(CREATION_HOOK.get().is_none());
    }

    /// seccomp's operation and prctl's option are read at the kernel's
    /// widths, so high bits cannot hide a filter installation.
    #[test]
    fn filter_installation_is_recognised_at_kernel_widths() {
        let high = 1_u64 << 32;
        let seccomp = u64::from(libc::SECCOMP_SET_MODE_FILTER);
        assert!(installs_seccomp_filter(
            libc::SYS_seccomp,
            [seccomp, 0, 0, 0, 0, 0]
        ));
        assert!(installs_seccomp_filter(
            libc::SYS_seccomp,
            [high | seccomp, 0, 0, 0, 0, 0]
        ));
        let prctl = libc::PR_SET_SECCOMP as u64;
        let mode = u64::from(libc::SECCOMP_MODE_FILTER);
        assert!(installs_seccomp_filter(
            libc::SYS_prctl,
            [high | prctl, mode, 0, 0, 0, 0]
        ));
        assert!(!installs_seccomp_filter(
            libc::SYS_prctl,
            [prctl, high | mode, 0, 0, 0, 0]
        ));
        assert!(!installs_seccomp_filter(
            libc::SYS_seccomp,
            [u64::from(libc::SECCOMP_GET_ACTION_AVAIL), 0, 0, 0, 0, 0]
        ));
    }
}
