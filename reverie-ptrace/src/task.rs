/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! `TracedTask` and its methods.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::ops::DerefMut;
use std::os::unix::ffi::OsStringExt;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock as StdOnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;

use async_trait::async_trait;
use futures::future;
use futures::future::Either;
use futures::future::Future;
use futures::future::FutureExt;
use futures::future::TryFutureExt;
use nix::sys::mman::ProtFlags;
use nix::sys::signal::Signal;
use reverie::Backtrace;
use reverie::Errno;
use reverie::ExitStatus;
use reverie::Frame;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Never;
use reverie::Pid;
#[cfg(target_arch = "x86_64")]
use reverie::Rdtsc;
use reverie::Subscription;
use reverie::Tid;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Addr;
use reverie::syscalls::AddrMut;
use reverie::syscalls::MemoryAccess;
use reverie::syscalls::Mprotect;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use safeptrace::ChildOp;
use safeptrace::Error as TraceError;
use safeptrace::Event;
use safeptrace::Running;
use safeptrace::Stopped;
use safeptrace::StoppedMemory;
use safeptrace::Wait;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinError;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::LiteinstInstrumentationStats;
use crate::PtraceBackendStatsSource;
use crate::children;
use crate::cp;
use crate::error::Error;
use crate::error::LiteinstActivationFailure;
use crate::error::LiteinstActivationFailureReason;
use crate::error::LiteinstActivationOperation;
use crate::error::LiteinstActivationStage;
use crate::error::TraceResultExt;
use crate::gdbstub::BreakpointType;
use crate::gdbstub::CoreRegs;
use crate::gdbstub::GdbRequest;
use crate::gdbstub::GdbServer;
use crate::gdbstub::ResumeAction;
use crate::gdbstub::ResumeInferior;
use crate::gdbstub::StopEvent;
use crate::gdbstub::StopReason;
use crate::gdbstub::StoppedInferior;
use crate::injected_syscall::InjectedSyscallFrame;
use crate::liteinst_stats::LiteinstPatchOutcome;
use crate::regs::Reg;
use crate::regs::RegAccess;
use crate::stack::GuestStack;
use crate::timer::HandleFailure;
use crate::timer::Timer;
use crate::timer::TimerEventRequest;
use crate::tracer::HeldRootStop;
use crate::tracer::HeldTaskStops;
use crate::tracer::NewbornTracee;
use crate::tracer::RootStopLease;
use crate::tracer::TraceeIdentity;
use crate::vdso;

#[cfg(target_arch = "x86_64")]
mod after_loader_task;

fn validate_liteinst_user_regs_update(
    current: &libc::user_regs_struct,
    requested: &libc::user_regs_struct,
) -> Result<(), Errno> {
    if current.rsp == requested.rsp {
        Ok(())
    } else {
        Err(Errno::ENOTSUPP)
    }
}

#[cfg(target_arch = "x86_64")]
fn liteinst_helper_entry_rflags(flags: u64) -> u64 {
    const RFLAGS_TF: u64 = 1 << 8;
    const RFLAGS_DF: u64 = 1 << 10;
    const RFLAGS_RF: u64 = 1 << 16;
    const RFLAGS_AC: u64 = 1 << 18;
    flags & !(RFLAGS_TF | RFLAGS_DF | RFLAGS_RF | RFLAGS_AC)
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstCpuidPolicy {
    Unsupported,
    UnchangedEnabled,
    RestoreDisabled,
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstTscPolicy {
    Unsupported,
    UnchangedEnabled,
    RestoreFaulting,
}

#[cfg(target_arch = "x86_64")]
struct LiteinstHelperSavedState {
    cpuid_policy: LiteinstCpuidPolicy,
    tsc_policy: LiteinstTscPolicy,
    regs: libc::user_regs_struct,
    xstate: safeptrace::XState,
    stack_address: usize,
    stack_value: u64,
}

#[cfg(target_arch = "x86_64")]
fn is_legacy_vsyscall_ip(ip: Reg) -> bool {
    const VSYSCALL_START: Reg = 0xffff_ffff_ff60_0000;
    const VSYSCALL_END: Reg = VSYSCALL_START + 0x1000;

    (VSYSCALL_START..VSYSCALL_END).contains(&ip)
}

#[derive(Debug)]
struct Suspended {
    waker: Option<mpsc::Sender<Pid>>,
    suspended: Arc<AtomicBool>,
}

/// Expected resume action sent by gdb client, when the task is in a gdb stop.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ExpectedGdbResume {
    /// Expecting a normal gdb resume, either single step, until or continue
    Resume,
    /// Expecting a gdb step over, this happens the underlying task hit a sw
    /// breakpoint, gdb then needs to restore the original instruction --
    /// which implies deleting the breakpoint, single-step, then restore
    /// the breakpoint. This is a special case because we need to serialize
    /// the whole operation, otherwise when there's a different thread in
    /// the same process group which share the same breakpoint, removing
    /// breakpoint can cause the 2nd thread to miss the breakpoint.
    StepOver,
    /// Force single-step, even if Resume(continue) is requested. This
    /// is a workaround when fork/vfork/clone event is reported to gdb,
    /// gdb could then issue an `vCont;p<pid>:-1` to resume all threads in
    /// the thread group, which could cause the main thread to miss events.
    StepOnly,
}

pub struct Child {
    id: Pid,
    /// Task is suspended, either stopped by gdb (client), or received
    /// SIGSTOP sent by other threads in the same process group.
    suspended: Arc<AtomicBool>,
    /// Notify a task reached SIGSTOP.
    wait_all_stop_tx: Option<mpsc::Sender<(Pid, Suspended)>>,
    /// Channel to receive if a child task is becoming a daemon, when
    /// `daemonize()` is called.
    pub(crate) daemonizer_rx: Option<mpsc::Receiver<broadcast::Receiver<()>>>,
    /// Join handle to let child task exit gracefully.
    pub(crate) handle: JoinHandle<ExitStatus>,
}

impl Child {
    /// Child task identifier.
    pub fn id(&self) -> Pid {
        self.id
    }
}

impl fmt::Debug for Child {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Child").field("id", &self.id).finish()
    }
}

impl Future for Child {
    type Output = Result<ExitStatus, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.handle.poll_unpin(cx)
    }
}

pub type Children = children::Children<Child>;

enum HandleSignalResult {
    /// Signal is suppressed with task resumed.
    SignalSuppressed(Wait),
    /// signal needs to be delivered.
    SignalToDeliver(Stopped, Signal),
}

#[cfg(target_arch = "x86_64")]
// Linux can report PTRACE_SINGLESTEP completion from a seccomp syscall skip as
// TRAP_BRKPT without advancing RIP. Distinguish that kernel transition from an
// external or guest breakpoint using the controller's exact pre-step state.
fn is_expected_syscall_skip_breakpoint(
    si_code: i32,
    pre_rip: u64,
    post_rip: u64,
    syscall_opcode: [u8; cp::SYSCALL_INSTR_SIZE],
    post_opcode: u8,
    forced_external_for_test: bool,
) -> bool {
    !forced_external_for_test
        && si_code == libc::TRAP_BRKPT
        && post_rip == pre_rip
        && syscall_opcode == [0x0f, 0x05]
        && post_opcode != 0xcc
}

fn is_expected_syscall_skip_trap(
    task: &Stopped,
    pre_rip: u64,
    forced_external_for_test: bool,
) -> Result<bool, TraceError> {
    if forced_external_for_test {
        return Ok(false);
    }
    let siginfo = task.getsiginfo()?;
    if siginfo.si_code == libc::TRAP_TRACE {
        return Ok(true);
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        return Ok(false);
    }
    #[cfg(target_arch = "x86_64")]
    if siginfo.si_code != libc::TRAP_BRKPT {
        return Ok(false);
    }
    #[cfg(target_arch = "x86_64")]
    {
        let post_rip = task.getregs()?.ip();
        let syscall_site = pre_rip
            .checked_sub(cp::SYSCALL_INSTR_SIZE as u64)
            .ok_or(Errno::EOVERFLOW)? as usize;
        let mut syscall_opcode = [0; cp::SYSCALL_INSTR_SIZE];
        task.read_exact(syscall_site, &mut syscall_opcode)?;
        let mut post_opcode = [0];
        task.read_exact(post_rip as usize, &mut post_opcode)?;
        Ok(is_expected_syscall_skip_breakpoint(
            siginfo.si_code,
            pre_rip,
            post_rip,
            syscall_opcode,
            post_opcode[0],
            forced_external_for_test,
        ))
    }
}

fn is_expected_breakpoint_trap(
    task: &Stopped,
    breakpoint_rip: u64,
    forced_external_for_test: bool,
) -> Result<bool, TraceError> {
    if forced_external_for_test {
        return Ok(false);
    }
    let siginfo = task.getsiginfo()?;
    let observed_rip = task.getregs()?.ip();
    let after_breakpoint = breakpoint_rip.checked_add(1);
    Ok((siginfo.si_code == libc::TRAP_BRKPT
        && (observed_rip == breakpoint_rip || Some(observed_rip) == after_breakpoint))
        || (siginfo.si_code == libc::SI_KERNEL && Some(observed_rip) == after_breakpoint))
}

fn is_expected_private_syscall_trap(
    task: &Stopped,
    expected_rip: u64,
    forced_external_for_test: bool,
) -> Result<bool, TraceError> {
    if forced_external_for_test {
        return Ok(false);
    }
    if task.getregs()?.ip() != expected_rip {
        return Ok(false);
    }
    let siginfo = task.getsiginfo()?;
    if !matches!(siginfo.si_code, libc::TRAP_TRACE | libc::TRAP_BRKPT) {
        return Ok(false);
    }

    // Some x86 kernels report PTRACE_SINGLESTEP completion after `syscall` as
    // TRAP_BRKPT rather than TRAP_TRACE. In either case, the private page is
    // RWX and therefore guest-mutable, so accept the stop only while the exact
    // controller-installed `syscall; ud2` stub remains intact.
    #[cfg(target_arch = "x86_64")]
    let expected_stub = [0x0f, 0x05, 0x0f, 0x0b];
    #[cfg(target_arch = "aarch64")]
    let expected_stub = [
        0x01, 0x00, 0x00, 0xd4, // svc 0
        0xad, 0xde, 0x00, 0x00, // udf 0xdead
    ];
    let mut observed_stub = [0; cp::SYSCALL_INSTR_SIZE * 2];
    task.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut observed_stub)?;
    Ok(observed_stub == expected_stub)
}

enum NestedTrapExpectation {
    None,
    SyscallSkip { pre_rip: u64 },
    Breakpoint(u64),
    PrivateSyscall(u64),
}
#[derive(Clone)]
pub(crate) struct InjectedSyscallTrap {
    pub(crate) marker: u64,
    pub(crate) rip: u64,
    pub(crate) provenance: Option<InjectedSyscallProvenance>,
}

#[derive(Clone)]
pub(crate) struct InjectedSyscallProvenance {
    pub(crate) image: PathBuf,
    pub(crate) image_inode: u64,
    pub(crate) image_entry_address: u64,
    pub(crate) patched_site_addresses: Arc<[u64]>,
}

#[derive(Debug)]
struct GuestMap {
    start: u64,
    end: u64,
    offset: u64,
    device_major: u64,
    device_minor: u64,
    readable: bool,
    writable: bool,
    executable: bool,
    shared: bool,
    inode: u64,
    path: Option<PathBuf>,
}

impl GuestMap {
    fn contains(&self, address: u64) -> bool {
        self.start <= address && address < self.end
    }

    fn contains_range(&self, range: GuestRange) -> bool {
        self.start <= range.start && range.end <= self.end
    }
}

fn guest_maps(pid: Pid) -> Option<Vec<GuestMap>> {
    let maps = std::fs::read(format!("/proc/{pid}/maps")).ok()?;
    Some(
        maps.split(|byte| *byte == b'\n')
            .filter_map(parse_guest_map)
            .collect(),
    )
}

fn next_proc_maps_field<'a>(line: &'a [u8], cursor: &mut usize) -> Option<&'a [u8]> {
    while line.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
    let start = *cursor;
    while line
        .get(*cursor)
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
    (start < *cursor).then(|| &line[start..*cursor])
}

fn parse_guest_map(line: &[u8]) -> Option<GuestMap> {
    let mut cursor = 0;
    let range = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;
    let permissions = next_proc_maps_field(line, &mut cursor)?;
    let offset = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;
    let device = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;
    let inode = std::str::from_utf8(next_proc_maps_field(line, &mut cursor)?).ok()?;

    let (start, end) = range.split_once('-')?;
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    let offset = u64::from_str_radix(offset, 16).ok()?;
    let (device_major, device_minor) = device.split_once(':')?;
    let device_major = u64::from_str_radix(device_major, 16).ok()?;
    let device_minor = u64::from_str_radix(device_minor, 16).ok()?;
    let inode = inode.parse::<u64>().ok()?;

    while line.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    let path = (cursor < line.len()).then(|| decode_proc_maps_path(&line[cursor..]));
    Some(GuestMap {
        start,
        end,
        offset,
        device_major,
        device_minor,
        readable: permissions.first() == Some(&b'r'),
        writable: permissions.get(1) == Some(&b'w'),
        executable: permissions.get(2) == Some(&b'x'),
        shared: permissions.get(3) == Some(&b's'),
        inode,
        path,
    })
}

fn decode_proc_maps_path(bytes: &[u8]) -> PathBuf {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1..index + 4]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            let value = u16::from(bytes[index + 1] - b'0') * 64
                + u16::from(bytes[index + 2] - b'0') * 8
                + u16::from(bytes[index + 3] - b'0');
            if let Ok(value) = u8::try_from(value) {
                decoded.push(value);
                index += 4;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    PathBuf::from(OsString::from_vec(decoded))
}

fn guest_auxv_entry(pid: Pid, key: u64) -> Option<u64> {
    let bytes = std::fs::read(format!("/proc/{pid}/auxv")).ok()?;
    bytes.as_chunks::<16>().0.iter().find_map(|entry| {
        let entry_key = u64::from_ne_bytes(entry[..8].try_into().ok()?);
        let value = u64::from_ne_bytes(entry[8..].try_into().ok()?);
        (entry_key == key).then_some(value)
    })
}

impl InjectedSyscallTrap {
    // TODO-HUMAN-REVIEW(PR-271): Review rewritten-image load-bias and patched-site
    // collision filtering before Tool dispatch.
    fn validates_site_provenance(
        &self,
        pid: Pid,
        trap_rip: u64,
        frame: &InjectedSyscallFrame,
    ) -> bool {
        let Some(provenance) = &self.provenance else {
            return trap_rip == self.rip;
        };
        let Some(maps) = guest_maps(pid) else {
            return false;
        };
        let matches_image = |mapping: &&GuestMap| {
            mapping.inode == provenance.image_inode
                && mapping.path.as_ref() == Some(&provenance.image)
        };
        let Some(load_bias) = guest_auxv_entry(pid, libc::AT_ENTRY)
            .and_then(|entry| entry.checked_sub(provenance.image_entry_address))
        else {
            return false;
        };
        self.rip.checked_add(load_bias) == Some(trap_rip)
            && maps
                .iter()
                .filter(matches_image)
                .any(|mapping| mapping.executable && mapping.contains(trap_rip))
            && maps
                .iter()
                .filter(matches_image)
                .any(|mapping| mapping.executable && mapping.contains(frame.instruction_pointer()))
            && frame
                .instruction_pointer()
                .checked_sub(load_bias)
                .is_some_and(|address| {
                    provenance
                        .patched_site_addresses
                        .binary_search(&address)
                        .is_ok()
                })
    }
}

#[derive(Clone)]
pub(crate) struct LiteinstRuntimeConfig {
    pub(crate) preload: PathBuf,
    #[cfg(target_arch = "x86_64")]
    pub(crate) after_loader: Option<crate::LiteinstAfterLoaderConfig>,
    pub(crate) begin_marker: u64,
    pub(crate) ready_marker: u64,
    pub(crate) helper_return_marker: u64,
    pub(crate) syscall_marker: u64,
    pub(crate) newborn_tracees: Arc<StdMutex<HashMap<Pid, NewbornTracee>>>,
    pub(crate) held_task_stops: HeldTaskStops,
    /// Records a fail-closed LiteInst refusal raised by any task.
    ///
    /// A non-root task's error cannot reach the root's cleanup guard, so
    /// without this the session could finish "successfully" after a child was
    /// released untraced. The root consults it before reporting success.
    pub(crate) session_failure: Arc<StdMutex<Option<String>>>,
    /// Wakes the root task as soon as a non-root refusal records the shared
    /// failure. The root may otherwise remain blocked in a guest wait for the
    /// refused child and never return control to the session cleanup guard.
    pub(crate) session_failure_changed: Arc<Notify>,
    /// Set once the guest has created a second task.
    ///
    /// Hook installation is single-task-only (see `maybe_install_preload_liteinst_site`).
    pub(crate) multi_task: Arc<AtomicBool>,
    /// TID of the session's root tracee, published once the guest is spawned.
    ///
    /// The root-stop lease and its cleanup guard are owned by exactly this
    /// TID. A forked child is its own thread-group leader, so the
    /// `tid == pid` shape cannot distinguish it from the root.
    pub(crate) root_tid: Arc<StdOnceLock<Pid>>,
    pub(crate) instrumentation_stats: Option<Arc<StdMutex<LiteinstInstrumentationStats>>>,
    #[cfg(test)]
    pub(crate) fail_preinit: bool,
    /// Synthesises a fail-closed error at the new-task boundary.
    ///
    /// Production no longer refuses task creation, so the cleanup guard's
    /// whole-group reaping needs an explicit trigger that still produces a
    /// multi-task tree at the moment of failure.
    #[cfg(test)]
    pub(crate) fail_new_task: bool,
    #[cfg(test)]
    pub(crate) pause_new_task: Option<mpsc::UnboundedSender<Pid>>,
    #[cfg(test)]
    pub(crate) pause_after_new_task: bool,
    #[cfg(test)]
    pub(crate) pause_before_new_task: Option<mpsc::UnboundedSender<Pid>>,
    #[cfg(test)]
    pub(crate) fail_discovery_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) fail_after_scan_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_task_scan_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) pause_root_stop: Option<(RootStopPause, mpsc::UnboundedSender<Pid>)>,
    #[cfg(test)]
    pub(crate) pause_preinit_step: Option<(usize, mpsc::UnboundedSender<Pid>)>,
    #[cfg(test)]
    pub(crate) pause_precise_timer_step: Option<mpsc::UnboundedSender<Pid>>,
    #[cfg(test)]
    pub(crate) activate_without_handshake: bool,
    #[cfg(test)]
    pub(crate) queue_pending_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_skip_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_context_none_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_context_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_preinit_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_post_exec_signal_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(crate) force_private_stub_mutation_once: Option<Arc<AtomicBool>>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum RootStopPause {
    Seccomp,
    Signal(Signal),
}

#[derive(Clone)]
struct LiteinstStopArmer {
    task_tid: Pid,
    held_task_stops: HeldTaskStops,
}

impl LiteinstStopArmer {
    fn arm(&self, task: &Stopped, event: &Event) -> Result<(), TraceError> {
        if task.pid() != self.task_tid {
            return Ok(());
        }
        HeldRootStop::arm_empty(&self.held_task_stops, task, event)
    }

    fn ensure(&self, task: &Stopped, event: &Event) -> Result<(), TraceError> {
        if task.pid() != self.task_tid {
            return Ok(());
        }
        HeldRootStop::ensure_current(&self.held_task_stops, task, event)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct LiteinstHandshakeFrame {
    version: u64,
    begin_rip: u64,
    ready_rip: u64,
    install_helper: u64,
    helper_stack_top: u64,
    helper_return: u64,
    helper_return_rip: u64,
    syscall_trap_rip: u64,
    syscall_trap_return_rip: u64,
    install_result: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct LiteinstInstallResult {
    version: u64,
    site_start: u64,
    site_len: u64,
    relocated_tail: u64,
    trampoline_start: u64,
    trampoline_len: u64,
    arena_writable_start: u64,
    arena_writable_len: u64,
    arena_executable_start: u64,
    arena_executable_len: u64,
    instruction_len: u64,
    straddle_prefix: u64,
    complete: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestRange {
    start: u64,
    end: u64,
}

impl GuestRange {
    fn new(start: u64, len: u64) -> Option<Self> {
        let end = start.checked_add(len)?;
        (start < end).then_some(Self { start, end })
    }

    fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    fn contains(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

fn kernel_page_range(start: u64, len: u64, page_size: u64) -> Result<Option<GuestRange>, ()> {
    if page_size == 0 || !page_size.is_power_of_two() {
        return Err(());
    }
    if len == 0 {
        return Ok(None);
    }

    let end = start.checked_add(len).ok_or(())?;
    let page_mask = page_size - 1;
    let start = start & !page_mask;
    let end = end.checked_add(page_mask).ok_or(())? & !page_mask;
    Ok(Some(GuestRange { start, end }))
}

fn host_page_size() -> Result<u64, Errno> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).map_err(|_| Errno::EIO)?;
    page_size
        .is_power_of_two()
        .then_some(page_size)
        .ok_or(Errno::EIO)
}

fn is_liteinst_mapping_syscall(nr: Sysno) -> bool {
    // TODO-HUMAN-REVIEW(PR-270): Review pkey_mprotect mapping-lifecycle classification.
    matches!(
        nr,
        // AUTONOMOUS-BOT-IMPLEMENTED
        Sysno::mmap | Sysno::munmap | Sysno::mremap | Sysno::mprotect | Sysno::pkey_mprotect
    )
}

/// Syscalls whose return lands in two tasks at once.
///
/// The kernel starts the new task at the instruction following the `syscall`,
/// so the site must still decode as the original instruction stream there.
fn is_task_creating_syscall(nr: Sysno) -> bool {
    matches!(
        nr,
        Sysno::clone | Sysno::clone3 | Sysno::fork | Sysno::vfork
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveHookFootprint {
    site: GuestRange,
    trampoline: GuestRange,
    arena_writable: GuestRange,
    arena_executable: GuestRange,
}

impl ActiveHookFootprint {
    fn protected_ranges(&self) -> [(GuestRange, i32); 4] {
        [
            (self.site, libc::PROT_READ | libc::PROT_EXEC),
            (self.trampoline, libc::PROT_READ | libc::PROT_EXEC),
            (self.arena_writable, libc::PROT_READ | libc::PROT_WRITE),
            (self.arena_executable, libc::PROT_READ | libc::PROT_EXEC),
        ]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstRuntimePhase {
    PreExec,
    Waiting,
    Bootstrap,
    Ready,
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiteinstTrapSiteProvenance {
    generation: u64,
    rip: u64,
}

#[cfg(target_arch = "x86_64")]
impl LiteinstTrapSiteProvenance {
    fn validates_retired_runtime_trap(
        self,
        generation: u64,
        observed_marker: u64,
        expected_marker: u64,
        observed_rip: u64,
        si_code: i32,
        trap_opcode: u8,
        runtime_device: u64,
        runtime_inode: u64,
        mapping: &GuestMap,
    ) -> bool {
        let Some(trap_address) = self.rip.checked_sub(1) else {
            return false;
        };
        let Some(trap_range) = GuestRange::new(trap_address, 2) else {
            return false;
        };
        self.generation == generation
            && observed_marker == expected_marker
            && observed_rip == self.rip
            && matches!(si_code, libc::TRAP_BRKPT | libc::SI_KERNEL)
            && trap_opcode == 0xcc
            && mapping.readable
            && !mapping.writable
            && mapping.executable
            && !mapping.shared
            && mapping.contains_range(trap_range)
            && mapping.inode == runtime_inode
            && libc::makedev(
                mapping.device_major as libc::c_uint,
                mapping.device_minor as libc::c_uint,
            ) == runtime_device
    }
}

#[derive(Clone, Debug)]
struct LiteinstRuntimeState {
    phase: LiteinstRuntimePhase,
    frame: Option<LiteinstHandshakeFrame>,
    generation: u64,
    ready_generation: Option<u64>,
    #[cfg(target_arch = "x86_64")]
    after_loader_syscall_trap: Option<LiteinstTrapSiteProvenance>,
    #[cfg(target_arch = "x86_64")]
    after_loader_reference: Option<(u64, crate::target_loader::TargetHostInitializer)>,
    #[cfg(target_arch = "x86_64")]
    after_loader_guest_observed: bool,
    attempted_sites: HashSet<u64>,
    fallback_sites: HashMap<u64, LiteinstPatchOutcome>,
    active_hooks: HashMap<u64, ActiveHookFootprint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiteinstEntryGuard {
    address: u64,
    saved_instruction: u64,
}

impl Default for LiteinstRuntimeState {
    fn default() -> Self {
        Self {
            phase: LiteinstRuntimePhase::PreExec,
            frame: None,
            generation: 0,
            ready_generation: None,
            #[cfg(target_arch = "x86_64")]
            after_loader_syscall_trap: None,
            #[cfg(target_arch = "x86_64")]
            after_loader_reference: None,
            #[cfg(target_arch = "x86_64")]
            after_loader_guest_observed: false,
            attempted_sites: HashSet::new(),
            fallback_sites: HashMap::new(),
            active_hooks: HashMap::new(),
        }
    }
}

impl LiteinstRuntimeState {
    fn after_exec(&self) -> Result<Self, Errno> {
        Ok(Self {
            phase: LiteinstRuntimePhase::Waiting,
            generation: self.generation.checked_add(1).ok_or(Errno::EOVERFLOW)?,
            ..Self::default()
        })
    }

    fn mapping_mutates_active_hook(&self, nr: Sysno, args: SyscallArgs, page_size: u64) -> bool {
        let operation_range = match nr {
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::mmap if args.arg3 as i32 & libc::MAP_FIXED != 0 => {
                kernel_page_range(args.arg0 as u64, args.arg1 as u64, page_size)
            }
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::munmap | Sysno::mprotect | Sysno::pkey_mprotect | Sysno::mremap => {
                kernel_page_range(args.arg0 as u64, args.arg1 as u64, page_size)
            }
            _ => return false,
        };
        let requested_protection = match nr {
            Sysno::mprotect => Some(args.arg2 as i32),
            Sysno::pkey_mprotect if args.arg3 == 0 => Some(args.arg2 as i32),
            _ => None,
        };
        let source_mutates_active_hook = match operation_range {
            Ok(Some(operation_range)) => {
                let active_hook = self.active_hooks.values().any(|hook| {
                    hook.protected_ranges()
                        .into_iter()
                        .any(|(range, protection)| {
                            range.overlaps(operation_range)
                                && requested_protection != Some(protection)
                        })
                });
                active_hook
            }
            Ok(None) => false,
            Err(()) => !self.active_hooks.is_empty(),
        };
        if source_mutates_active_hook {
            return true;
        }
        if nr == Sysno::mremap && args.arg3 as i32 & libc::MREMAP_FIXED != 0 {
            let destination = match kernel_page_range(args.arg4 as u64, args.arg2 as u64, page_size)
            {
                Ok(Some(range)) => range,
                Ok(None) => return false,
                Err(()) => return !self.active_hooks.is_empty(),
            };
            let active_hook = self.active_hooks.values().any(|hook| {
                hook.protected_ranges()
                    .into_iter()
                    .any(|(range, _)| range.overlaps(destination))
            });
            return active_hook;
        }
        false
    }

    fn invalidate_attempted_pages(&mut self, start: u64, len: u64, page_size: u64) {
        let range = match kernel_page_range(start, len, page_size) {
            Ok(Some(range)) => range,
            Ok(None) => return,
            Err(()) => {
                self.attempted_sites.clear();
                self.fallback_sites.clear();
                return;
            }
        };
        if range.start >= range.end {
            self.attempted_sites.clear();
            self.fallback_sites.clear();
            return;
        }
        self.attempted_sites
            .retain(|address| !(*address >= range.start && *address < range.end));
        self.fallback_sites
            .retain(|address, _| !(*address >= range.start && *address < range.end));
    }
}

enum LiteinstTrap {
    HandshakeBegin,
    HandshakeReady,
    Syscall(usize),
    Invalid,
}

/// All the info needed to be able to interact with the global state.
struct GlobalState<G: GlobalTool> {
    /// The tool's static configuration data.
    cfg: G::Config,

    /// Reference to the tool's global state. This is used to send it "rpc" messages.
    gs_ref: Arc<G>,

    /// Events the tool is subscripted (like interception)
    subscriptions: Arc<Subscription>,

    /// guests are sequentialized already (by detcore for example), gdbserver
    /// should avoid sequentialize threads.
    sequentialized_guest: Arc<bool>,

    /// Marker and exact RIP identifying a binary-rewriter syscall trap.
    injected_syscall_trap: Option<InjectedSyscallTrap>,

    /// Optional dynamic LiteInst runtime configuration.
    liteinst_runtime: Option<LiteinstRuntimeConfig>,

    /// Optional collector for general ptrace lifecycle activity.
    backend_stats: Option<PtraceBackendStatsSource>,
}

impl<G: GlobalTool> Clone for GlobalState<G> {
    fn clone(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            gs_ref: self.gs_ref.clone(),
            subscriptions: self.subscriptions.clone(),
            sequentialized_guest: self.sequentialized_guest.clone(),
            injected_syscall_trap: self.injected_syscall_trap.clone(),
            liteinst_runtime: self.liteinst_runtime.clone(),
            backend_stats: self.backend_stats.clone(),
        }
    }
}

/// A raw argument remains exact unless both its type and launch ownership are known.
struct SyscallArgsForLog {
    nr: Sysno,
    args: SyscallArgs,
    command_bootstrap: bool,
}

struct CommandBootstrapAddress(usize);

impl fmt::Debug for CommandBootstrapAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            f.write_str("0")
        } else {
            write!(f, "<hostaddr {:#x}>", self.0)
        }
    }
}

impl fmt::Debug for SyscallArgsForLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.command_bootstrap || self.nr != Sysno::execve {
            return fmt::Debug::fmt(&self.args, f);
        }
        // Command::do_exec passes pathname, argv and envp pointers from the
        // inherited launcher image. The ABI-unused registers are still raw.
        f.debug_struct("SyscallArgs")
            .field("arg0", &CommandBootstrapAddress(self.args.arg0))
            .field("arg1", &CommandBootstrapAddress(self.args.arg1))
            .field("arg2", &CommandBootstrapAddress(self.args.arg2))
            .field("arg3", &self.args.arg3)
            .field("arg4", &self.args.arg4)
            .field("arg5", &self.args.arg5)
            .finish()
    }
}

/// Event configuration supplied when a traced task is created.
pub(crate) struct TracedTaskOptions<'a> {
    pub(crate) command_bootstrap: bool,
    pub(crate) events: &'a Subscription,
    pub(crate) injected_syscall_trap: Option<InjectedSyscallTrap>,
    pub(crate) liteinst_runtime: Option<LiteinstRuntimeConfig>,
    pub(crate) backend_stats: Option<PtraceBackendStatsSource>,
}

/// Our runtime representation of what Reverie knows about a guest thread. Its
/// lifetime matches the lifetime of the thread.
pub struct TracedTask<L: Tool> {
    /// Thread ID.
    tid: Pid,

    /// Process ID.
    pid: Pid,

    /// Parent process ID.
    ppid: Option<Pid>,

    /// State associated with the thread. Unique for each thread.
    thread_state: L::ThreadState,

    /// State associated with the process. This is shared among threads in the
    /// same thread group.
    process_state: Arc<L>,

    /// Global state. This is shared among all threads in a process tree.
    global_state: GlobalState<L::GlobalState>,

    /// True only for TracerBuilder::spawn's Command root until successful exec.
    /// Descendants and spawn_fn never inherit this logging provenance.
    command_bootstrap: bool,

    /// True if we can intercept CPUID, false otherwise.
    has_cpuid_interception: bool,

    /// Set to `Some` if the syscall has not been injected yet. `None` if it has.
    pending_syscall: Option<(Sysno, SyscallArgs)>,

    /// The pending syscall was converted out of its seccomp stop before Tool dispatch.
    pending_syscall_already_skipped: bool,

    /// Address of the writable e9tool register frame for the active event.
    injected_syscall_frame: Option<usize>,

    /// The exact physical stop owned by the Tool callback currently in flight.
    ///
    /// Tool syscall injection may advance the tracee through several kernel
    /// stops.  Keeping the typed capability here lets the injection path
    /// replace it with the final stop instead of reconstructing an unchecked
    /// capability for the old stop and resuming that status twice.
    active_tool_stop: Option<Stopped>,

    /// Per-process dynamic LiteInst handshake and patched-site state.
    liteinst_runtime: Arc<StdMutex<LiteinstRuntimeState>>,

    /// Controller-owned breakpoint preventing the executable entry before Ready.
    liteinst_entry_guard: Option<LiteinstEntryGuard>,
    #[cfg(target_arch = "x86_64")]
    liteinst_after_loader_guard: Option<crate::entry_call::EntryGuard>,
    #[cfg(target_arch = "x86_64")]
    liteinst_after_loader_syscall_permit: Option<after_loader_task::AfterLoaderSyscallPermit>,
    #[cfg(target_arch = "x86_64")]
    liteinst_after_loader_syscall_inflight: Option<after_loader_task::AfterLoaderSyscallPermit>,
    #[cfg(target_arch = "x86_64")]
    liteinst_after_loader_forward_inflight: Option<after_loader_task::AfterLoaderForwardInFlight>,
    #[cfg(target_arch = "x86_64")]
    liteinst_after_loader_private_call: Option<after_loader_task::AfterLoaderPrivateCall>,
    #[cfg(target_arch = "x86_64")]
    liteinst_after_loader_private_state: Option<after_loader_task::AfterLoaderPrivateState>,

    /// Original typed fail-closed error retained while the exit waiter reaps root.
    liteinst_failure: Option<LiteinstActivationFailure>,

    /// pending signal to deliver. This can happen when
    /// syscall got interrupted (by signal)
    pending_signal: Option<Signal>,

    /// A channel to allow short-circuiting the next state to main run loop. This
    /// is useful inside of `inject` or `tail_inject` where we might need to
    /// cancel a future early.
    next_state: mpsc::Sender<Result<Wait, TraceError>>,

    /// The receiving end of the next_state channel.
    next_state_rx: Option<mpsc::Receiver<Result<Wait, TraceError>>>,

    /// The timer tracking this task. Used to trigger RCB-based `timeouts`.
    timer: Timer,

    /// Set when `tail_inject` needs to cancel the current tool handler.
    cancel_handler: Arc<AtomicBool>,

    /// Child processes to wait on. When one of the children exits, it should be
    /// removed from this list.
    child_procs: Arc<Mutex<Children>>,

    /// Child threads to wait on. When one of the child threads exits, it should
    /// be removed from this list.
    child_threads: Arc<Mutex<Children>>,

    /// Channel to send child processes to that are left over by the time this
    /// task exits.
    orphanage: mpsc::Sender<Child>,

    /// broadcast to kill all daemons
    daemon_kill_switch: broadcast::Sender<()>,

    /// Channel to damonize a process
    daemonizer: mpsc::Sender<broadcast::Receiver<()>>,

    /// The rx end of `daemonizer`.
    daemonizer_rx: Option<mpsc::Receiver<broadcast::Receiver<()>>>,

    /// Total number of tasks
    ntasks: Arc<AtomicUsize>,

    /// Total number of daemons
    ndaemons: Arc<AtomicUsize>,

    /// Task is a daemon
    is_a_daemon: bool,

    /// Software breakpoints.
    // NB: For multi-threaded programs, sw breakpoints apply to all threads
    // because they're in the same address space. Hence removing sw
    // breakpoint in one thread also remove it for the rest of the threads
    // in the same process group. *However*, our model is slightly different
    // because we use different tx/rx channels even the threads are in the
    // same process group, hence each threads owns `breakpoints: HashMap`
    // instead of `Arc<Mutex<..>>`.
    breakpoints: HashMap<u64, u64>,

    /// Notify gdbserver start accepting incoming packets.
    gdbserver_start_tx: Option<oneshot::Sender<()>>,

    /// task is suspended (received SIGSTOP)
    suspended: Arc<AtomicBool>,

    /// Notify gdbserver there's a new stop event.
    gdb_stop_tx: Option<mpsc::Sender<StoppedInferior>>,

    /// Task is attached by gdb.
    // NB: gdb doesn't always attach everything, when fork/clone is called.
    // gdb also allows detach from a task, and re-attach again.
    attached_by_gdb: bool,

    /// Task is resumed by gdb.
    // NB: gdb doesn't always attach everything, when fork/clone is called.
    // gdb also allows detach from a task, and re-attach again.
    resumed_by_gdb: Option<ResumeAction>,

    /// GDB resume request, gdbstub is the sender
    gdb_resume_tx: Option<mpsc::Sender<ResumeInferior>>,

    /// GDB resume request, reverie is the receiver
    gdb_resume_rx: Option<mpsc::Receiver<ResumeInferior>>,

    /// Request sent by gdb. the tx channel is used by gdb instead of
    /// `TracedTask`.
    gdb_request_tx: Option<mpsc::Sender<GdbRequest>>,

    /// Receiver to receive gdb request.
    gdb_request_rx: Option<mpsc::Receiver<GdbRequest>>,

    /// Wait to be resumed when in sigstop due to all stop mode.
    exit_suspend_tx: Option<mpsc::Sender<Pid>>,

    /// Wait to be resumed when in sigstop due to all stop mode.
    exit_suspend_rx: Option<mpsc::Receiver<Pid>>,

    /// Suspended task when hitting swbp. This is used to implement gdb's
    /// all stop mode.
    suspended_tasks: BTreeMap<Pid, Suspended>,

    /// Task needs (single) step over the swbp instruciton when a swbp is
    /// hit. unless this is done, if is not safe for other threads running
    /// in parallel to report breakpoint, otherwise there're could be an
    /// interleaved step-over, which might remove the breakpoint, hence
    /// causing others to miss the breakpoint.
    needs_step_over: Arc<Mutex<()>>,

    /// Whether or not the tool is currently holding a handle on the guest Stack (and thus
    /// potentially using actual stack memory within the guest).
    stack_checked_out: Arc<AtomicBool>,
}

impl<L: Tool> fmt::Debug for TracedTask<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TracedTask")
            .field("tid", &self.tid)
            .field("pid", &self.pid)
            .field("ppid", &self.ppid)
            .finish()
    }
}

impl<L: Tool> TracedTask<L> {
    /// Create a new TracedTask.
    pub(crate) fn new(
        tid: Pid,
        cfg: <L::GlobalState as GlobalTool>::Config,
        gs_ref: Arc<L::GlobalState>,
        options: TracedTaskOptions<'_>,
        orphanage: mpsc::Sender<Child>,
        daemon_kill_switch: broadcast::Sender<()>,
        mut gdbserver: Option<GdbServer>,
    ) -> Self {
        let process_state = Arc::new(L::new(tid, &cfg));
        let global_state = GlobalState {
            gs_ref,
            cfg,
            subscriptions: Arc::new(options.events.clone()),
            sequentialized_guest: Arc::new(
                gdbserver
                    .as_ref()
                    .map(|s| s.sequentialized_guest)
                    .unwrap_or(false),
            ),
            injected_syscall_trap: options.injected_syscall_trap.clone(),
            liteinst_runtime: options.liteinst_runtime,
            backend_stats: options.backend_stats,
        };
        let thread_state = process_state.init_thread_state(tid, None);
        let (next_state, next_state_rx) = mpsc::channel(1);
        let (daemonizer, daemonizer_rx) = mpsc::channel(1);
        let (gdb_resume_tx, gdb_resume_rx) = mpsc::channel(1);
        let (gdb_request_tx, gdb_request_rx) = mpsc::channel(1);
        let (exit_suspend_tx, exit_suspend_rx) = mpsc::channel(16);
        Self {
            tid,
            pid: tid,
            ppid: None,
            thread_state,
            process_state,
            global_state,
            command_bootstrap: options.command_bootstrap,
            has_cpuid_interception: false,
            pending_syscall: None,
            pending_syscall_already_skipped: false,
            injected_syscall_frame: None,
            active_tool_stop: None,
            liteinst_runtime: Arc::new(StdMutex::new(LiteinstRuntimeState::default())),
            liteinst_entry_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_syscall_permit: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_syscall_inflight: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_forward_inflight: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_private_call: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_private_state: None,
            liteinst_failure: None,
            next_state,
            next_state_rx: Some(next_state_rx),
            timer: Timer::new(tid, tid),
            cancel_handler: Arc::new(AtomicBool::new(false)),
            pending_signal: None,
            child_procs: Arc::new(Mutex::new(Children::new())),
            child_threads: Arc::new(Mutex::new(Children::new())),
            orphanage,
            daemon_kill_switch,
            daemonizer,
            daemonizer_rx: Some(daemonizer_rx),
            ntasks: Arc::new(AtomicUsize::new(1)),
            ndaemons: Arc::new(AtomicUsize::new(0)),
            is_a_daemon: false,
            gdbserver_start_tx: gdbserver.as_mut().and_then(|s| s.server_tx.take()),
            gdb_stop_tx: gdbserver
                .as_mut()
                .and_then(|s| s.inferior_attached_tx.take()),
            attached_by_gdb: false,
            resumed_by_gdb: None,
            gdb_resume_tx: Some(gdb_resume_tx),
            gdb_resume_rx: Some(gdb_resume_rx),
            breakpoints: HashMap::new(),
            suspended: Arc::new(AtomicBool::new(false)),
            gdb_request_tx: Some(gdb_request_tx),
            gdb_request_rx: Some(gdb_request_rx),
            exit_suspend_tx: Some(exit_suspend_tx),
            exit_suspend_rx: Some(exit_suspend_rx),
            needs_step_over: Arc::new(Mutex::new(())),
            suspended_tasks: BTreeMap::new(),
            stack_checked_out: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a child TracedTask corresponding to a clone()
    fn cloned(&self, child: Pid) -> Self {
        let global_state = self.global_state.clone();
        let process_state = self.process_state.clone();
        let thread_state =
            process_state.init_thread_state(child, Some((self.tid, &self.thread_state)));
        let (next_state, next_state_rx) = mpsc::channel(1);
        let (daemonizer, daemonizer_rx) = mpsc::channel(1);
        let (gdb_resume_tx, gdb_resume_rx) = mpsc::channel(1);
        let (gdb_request_tx, gdb_request_rx) = mpsc::channel(1);
        let (exit_suspend_tx, exit_suspend_rx) = mpsc::channel(16);
        self.ntasks.fetch_add(1, Ordering::SeqCst);
        Self {
            tid: child,
            pid: self.pid,
            ppid: self.ppid,
            thread_state,
            process_state,
            global_state,
            command_bootstrap: false,
            has_cpuid_interception: self.has_cpuid_interception,
            pending_syscall: None,
            pending_syscall_already_skipped: false,
            injected_syscall_frame: None,
            active_tool_stop: None,
            liteinst_runtime: self.liteinst_runtime.clone(),
            liteinst_entry_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_syscall_permit: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_syscall_inflight: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_forward_inflight: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_private_call: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_private_state: None,
            liteinst_failure: None,
            next_state,
            next_state_rx: Some(next_state_rx),
            timer: Timer::new(self.pid, child),
            cancel_handler: Arc::new(AtomicBool::new(false)),
            pending_signal: None,
            child_procs: self.child_procs.clone(),
            child_threads: self.child_threads.clone(),
            orphanage: self.orphanage.clone(),
            daemon_kill_switch: self.daemon_kill_switch.clone(),
            daemonizer,
            daemonizer_rx: Some(daemonizer_rx),
            ntasks: self.ntasks.clone(),
            ndaemons: self.ndaemons.clone(),
            is_a_daemon: self.is_a_daemon,
            gdbserver_start_tx: None,
            gdb_stop_tx: None,
            attached_by_gdb: self.attached_by_gdb,
            resumed_by_gdb: self.resumed_by_gdb,
            gdb_resume_tx: Some(gdb_resume_tx),
            gdb_resume_rx: Some(gdb_resume_rx),
            breakpoints: self.breakpoints.clone(),
            suspended: Arc::new(AtomicBool::new(false)),
            gdb_request_tx: Some(gdb_request_tx),
            gdb_request_rx: Some(gdb_request_rx),
            exit_suspend_tx: Some(exit_suspend_tx),
            exit_suspend_rx: Some(exit_suspend_rx),
            needs_step_over: self.needs_step_over.clone(),
            suspended_tasks: BTreeMap::new(),
            stack_checked_out: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a child TracedTask corresponding to a fork()
    fn forked(&self, child: Pid) -> Self {
        let process_state = Arc::new(L::new(child, &self.global_state.cfg));
        let thread_state =
            process_state.init_thread_state(child, Some((self.tid, &self.thread_state)));
        let (next_state, next_state_rx) = mpsc::channel(1);
        let (daemonizer, daemonizer_rx) = mpsc::channel(1);
        let (gdb_resume_tx, gdb_resume_rx) = mpsc::channel(1);
        let (gdb_request_tx, gdb_request_rx) = mpsc::channel(1);
        let (exit_suspend_tx, exit_suspend_rx) = mpsc::channel(16);
        self.ntasks.fetch_add(1, Ordering::SeqCst);
        Self {
            tid: child,
            pid: child,
            ppid: Some(self.pid),
            thread_state,
            process_state,
            global_state: self.global_state.clone(),
            command_bootstrap: false,
            has_cpuid_interception: self.has_cpuid_interception,
            pending_syscall: None,
            pending_syscall_already_skipped: false,
            injected_syscall_frame: None,
            active_tool_stop: None,
            liteinst_runtime: Arc::new(StdMutex::new(
                self.liteinst_runtime.lock().unwrap().clone(),
            )),
            liteinst_entry_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_syscall_permit: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_syscall_inflight: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_forward_inflight: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_private_call: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_after_loader_private_state: None,
            liteinst_failure: None,
            next_state,
            next_state_rx: Some(next_state_rx),
            timer: Timer::new(child, child),
            cancel_handler: Arc::new(AtomicBool::new(false)),
            pending_signal: None,
            child_procs: Arc::new(Mutex::new(Children::new())),
            child_threads: Arc::new(Mutex::new(Children::new())),
            orphanage: self.orphanage.clone(),
            daemon_kill_switch: self.daemon_kill_switch.clone(),
            daemonizer,
            daemonizer_rx: Some(daemonizer_rx),
            ntasks: self.ntasks.clone(),
            ndaemons: self.ndaemons.clone(),
            // NB: if daemon forks, then its child's parent pid is no longer 1.
            is_a_daemon: false,
            gdbserver_start_tx: None,
            gdb_stop_tx: None,
            attached_by_gdb: self.attached_by_gdb,
            resumed_by_gdb: None,
            gdb_resume_tx: Some(gdb_resume_tx),
            gdb_resume_rx: Some(gdb_resume_rx),
            breakpoints: self.breakpoints.clone(),
            suspended: Arc::new(AtomicBool::new(false)),
            gdb_request_tx: Some(gdb_request_tx),
            gdb_request_rx: Some(gdb_request_rx),
            exit_suspend_tx: Some(exit_suspend_tx),
            exit_suspend_rx: Some(exit_suspend_rx),
            needs_step_over: Arc::new(Mutex::new(())),
            suspended_tasks: BTreeMap::new(),
            stack_checked_out: Arc::new(AtomicBool::new(false)),
        }
    }

    fn read_injected_syscall_frame(
        &self,
        task: &Stopped,
        address: usize,
    ) -> Result<InjectedSyscallFrame, TraceError> {
        let address = Addr::from_raw(address).ok_or(Errno::EFAULT)?;
        Ok(task.read_value(address)?)
    }

    fn write_injected_syscall_frame(
        &self,
        task: &Stopped,
        address: usize,
        frame: &InjectedSyscallFrame,
    ) -> Result<(), TraceError> {
        let address = AddrMut::from_raw(address).ok_or(Errno::EFAULT)?;
        let mut memory = task.memory();
        Ok(memory.write_value(address, frame)?)
    }

    fn write_injected_syscall_result(
        &self,
        task: &Stopped,
        result: Result<i64, Errno>,
    ) -> Result<(), TraceError> {
        let address = self.injected_syscall_frame.ok_or(Errno::EIO)?;
        let mut frame = self.read_injected_syscall_frame(task, address)?;
        let result = result.unwrap_or_else(|errno| -(errno.into_raw() as i64));
        frame.set_result(result);
        self.write_injected_syscall_frame(task, address, &frame)
    }

    fn read_guest_registers(&self, task: &Stopped) -> Result<libc::user_regs_struct, TraceError> {
        let mut regs = task.getregs()?;
        if let Some(address) = self.injected_syscall_frame {
            let frame = self.read_injected_syscall_frame(task, address)?;
            frame.copy_to_user_regs(&mut regs);
        }
        Ok(regs)
    }

    fn write_guest_registers(
        &self,
        task: &Stopped,
        regs: &libc::user_regs_struct,
    ) -> Result<(), TraceError> {
        if let Some(address) = self.injected_syscall_frame {
            let mut frame = self.read_injected_syscall_frame(task, address)?;
            let current = self.read_guest_registers(task)?;
            InjectedSyscallFrame::validate_user_regs_update(&current, regs)?;
            if self.global_state.liteinst_runtime.is_some() {
                validate_liteinst_user_regs_update(&current, regs)?;
            }
            frame.copy_from_user_regs(regs);
            self.write_injected_syscall_frame(task, address, &frame)
        } else {
            task.setregs(regs)
        }
    }

    fn get_syscall(&self, task: &Stopped) -> Result<Syscall, TraceError> {
        let regs = task.getregs()?;
        let nr = Sysno::from(regs.orig_syscall() as i32);

        let args = regs.args();

        Ok(Syscall::from_raw(
            nr,
            SyscallArgs::new(
                args.0 as usize,
                args.1 as usize,
                args.2 as usize,
                args.3 as usize,
                args.4 as usize,
                args.5 as usize,
            ),
        ))
    }
}

fn set_ret(task: &Stopped, ret: Reg) -> Result<Reg, TraceError> {
    let mut regs = task.getregs()?;
    let old = regs.ret();
    *regs.ret_mut() = ret;
    task.setregs(&regs)?;
    Ok(old)
}

/// Canonical marker emitted when a guest-thread task dies of a panic.
///
/// The token is what a harness greps for, in the same spirit as
/// `HERMIT_SKID_OVERSHOOT`; keep it stable. It exists because an exit code
/// alone cannot say *why* a run ended, and this failure mode was expensive
/// precisely because it was unreadable: the run hung, so a panic was
/// indistinguishable from a slow run to every harness that judges by wall time.
const TASK_PANIC_MARKER: &str = "HERMIT_TASK_PANIC";

/// Exit status used when a guest-thread task panics.
///
/// This is rustc's conventional panic status inside the tracer process. An
/// embedding executable may normalize it at an outer process boundary, so the
/// marker above remains the authoritative machine-readable diagnosis.
const TASK_PANIC_EXIT_CODE: i32 = 101;

/// Renders the one-line panic marker. Separate from the exit so it can be
/// tested without ending the test process.
fn format_task_panic_marker(tid: Pid, payload: &(dyn std::any::Any + Send)) -> String {
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&'static str>().copied())
        .unwrap_or("<non-string panic payload>");
    // Always exactly one line: a marker that can wrap is a marker a harness
    // cannot grep. The panic's own (multi-line) output has already reached
    // stderr through the default hook; this line exists to be machine-read.
    let message: String = message
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    format!(
        "{} tid={} exit={} message={}",
        TASK_PANIC_MARKER,
        tid,
        TASK_PANIC_EXIT_CODE,
        message.trim()
    )
}

/// A guest-thread task died of a panic. End the run, loudly.
///
/// WHY THE PROCESS EXITS RATHER THAN PROPAGATING AN ERROR. Tokio's task harness
/// catches a panic in a `spawn_local` task and parks it in the `JoinHandle`,
/// which nothing polls until `tool_exit`. The run cannot reach `tool_exit`,
/// because by then the tool's scheduler is parked waiting for a turn request
/// this task will never post, and detcore's `Ivar` has no way to report that
/// its writer is gone -- so every other guest thread waits forever and the run
/// hangs until an external timeout kills it. There is no live party left to
/// hand an error to. detcore reached the same conclusion about its own
/// scheduler task and built `immediate_fatal_exit` for exactly this.
///
/// Exiting does not leak the guest: `postspawn` sets `PTRACE_O_EXITKILL`, so
/// the tracees die with the tracer. The terminal-deadlock path already relies
/// on that.
fn guest_task_panic_is_fatal(tid: Pid, payload: Box<dyn std::any::Any + Send>) -> ! {
    // `eprintln!` rather than `tracing::error!` on purpose, matching detcore's
    // terminal-deadlock report: the tracing writer prefixes a real wall-clock
    // timestamp, and a marker meant to be compared across runs must not carry
    // one.
    eprintln!("{}", format_task_panic_marker(tid, payload.as_ref()));
    let _ = std::io::stderr().flush();
    let _ = std::io::stdout().flush();
    std::process::exit(TASK_PANIC_EXIT_CODE)
}

fn log_guest_exit(tid: Pid, pid: Pid, exit_status: ExitStatus) {
    if let ExitStatus::Signaled(signal, core_dumped) = exit_status {
        tracing::error!(
            target: "reverie_ptrace::lifecycle",
            %tid,
            %pid,
            %signal,
            core_dumped,
            "guest terminated by signal"
        );
    }
}

/// Handles a potentially internal error, converting it to an exit status.
async fn handle_internal_error(err: Error) -> Result<ExitStatus, reverie::Error> {
    match err {
        Error::Internal(TraceError::Died(zombie))
        | Error::Tracee {
            source: TraceError::Died(zombie),
            ..
        } => zombie
            .reap()
            .await
            .map_err(|error| anyhow::anyhow!("failed to reap dead tracee: {error}").into()),
        Error::Internal(TraceError::Errno(errno)) => Err(errno.into()),
        Error::Tracee {
            operation,
            pid,
            source: TraceError::Errno(errno),
        } => Err(anyhow::anyhow!("{operation} failed for tracee {pid}: {errno}").into()),
        Error::Runtime {
            operation,
            pid,
            message,
        } => Err(anyhow::anyhow!("{operation} failed for tracee {pid}: {message}").into()),
        Error::External(err) => Err(err),
    }
}

/// Helper for canceling handlers.
async fn cancellable<F>(cancel_handler: Arc<AtomicBool>, f: F) -> Option<F::Output>
where
    F: Future,
{
    futures::pin_mut!(f);
    future::poll_fn(|cx| {
        let result = f.as_mut().poll(cx);

        // `tail_inject` sets this while polling `f`, then remains pending. We
        // can cancel the handler in the same poll instead of waking the Tokio
        // task solely to make this future observe its own notification.
        if cancel_handler.swap(false, Ordering::SeqCst) {
            Poll::Ready(None)
        } else {
            result.map(Some)
        }
    })
    .await
}

#[cfg(target_arch = "x86_64")]
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum SegfaultTrapInfo {
    Cpuid,
    Rdtscs(Rdtsc),
}

#[cfg(target_arch = "x86_64")]
impl SegfaultTrapInfo {
    /// Check if segfault is called by cpuid/rdtsc trap
    pub fn decode_segfault(insn_at_rip: u64) -> Option<SegfaultTrapInfo> {
        if insn_at_rip & 0xffffu64 == 0xa20fu64 {
            Some(SegfaultTrapInfo::Cpuid)
        } else if insn_at_rip & 0xffffu64 == 0x310fu64 {
            Some(SegfaultTrapInfo::Rdtscs(Rdtsc::Tsc))
        } else if insn_at_rip & 0xffffffu64 == 0xf9010fu64 {
            Some(SegfaultTrapInfo::Rdtscs(Rdtsc::Tscp))
        } else {
            None
        }
    }
}

// restore syscall context when it returns. This is needed because we might
// have injected a different syscall (or arguments) in handle_seccomp.
fn restore_context(
    task: &Stopped,
    context: libc::user_regs_struct,
    retval: Option<Reg>,
    restore_stack: bool,
) -> Result<(), TraceError> {
    let mut regs = task.getregs()?;

    if let Some(ret) = retval {
        *regs.ret_mut() = ret;
    }
    // TODO-HUMAN-REVIEW(PR-103): Review injected parent-stack restoration.
    if restore_stack {
        *regs.stack_ptr_mut() = context.stack_ptr();
    }

    // Restore instruction pointer.
    *regs.ip_mut() = context.ip();

    // Restore syscall arguments.
    regs.set_args(context.args());

    // This is needed when syscall is interrupted by a signal (ERESTARTSYS)
    // we need restore the original syscall number as well because it is
    // possible syscall is reinjected as a different variant, like vfork ->
    // clone, which accepts different arguments.
    *regs.orig_syscall_mut() = context.orig_syscall();

    // The `syscall` instruction clobbers %rcx/%r11. When we injected a syscall
    // (or a different syscall variant) from the private trampoline page, %rcx
    // and %r11 now hold the *trampoline's* return RIP / RFLAGS rather than the
    // guest's. Although the ABI leaves these "undefined" after a syscall, an
    // injection should be transparent, and leaving Reverie's private trampoline
    // address in %rcx would leak a tracer-internal (and potentially
    // nondeterministic) pointer to the guest. Restore them from the guest's own
    // pre-syscall snapshot. (No-op on aarch64.)
    regs.restore_syscall_clobbers(&context);

    task.setregs(&regs)
}

impl<L: Tool + 'static> TracedTask<L> {
    #[cfg(target_arch = "x86_64")]
    async fn cpuid_state(&mut self) -> Result<i64, Errno> {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.inject_with_retry(ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_GET_CPUID(None)))
            .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn intercept_cpuid(&mut self) -> Result<(), Errno> {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.inject_with_retry(ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_SET_CPUID(0)))
            .await
            .map(|_| ())
    }

    /// Perform the very first setup of a fresh tracee process:
    ///
    /// (1) Set up the special reverie/guest shared page in the tracee.
    ///
    /// (2) Also disables vdso within the guest
    ///
    /// Warning: this function MUTATES guest code to accomplish the modifications, even though this
    /// mutation is undone before it returns.  As a result, it  has an extra precondition.
    ///
    /// Precondition: all threads in the guest process are stopped. Otherwise a guest state may be
    /// executing the instructions that are mutated and may crash (due to problems with incoherent
    /// instruction fetch resulting in non-atomic writes to instructions that straddle cache line
    /// boundaries).
    ///
    /// Precondition: the caller is entitled to execute (blocking, destructive) waitpids against the
    /// target tracee.  This must not race with concurrent asynchronous tasks operating on the same
    /// TID.
    ///
    /// Postcondition: the guest registers and code memory are restored to their original state,
    /// including RIP, but the vdso page and special shared page are modified accordingly.
    #[tracing::instrument(
        target = "reverie_ptrace::lifecycle",
        name = "tracee.initialize",
        level = "debug",
        skip_all,
        fields(pid = %task.pid())
    )]
    pub async fn tracee_preinit(&mut self, task: Stopped) -> Result<Stopped, TraceError> {
        #[cfg(target_arch = "x86_64")]
        let bootstrap_before_exec = self.after_loader_config().is_some() && self.command_bootstrap;
        #[cfg(not(target_arch = "x86_64"))]
        let bootstrap_before_exec = false;

        let held_task_stops = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.held_task_stops));
        let reject_activation_signals = self.global_state.liteinst_runtime.is_some();
        let unexpected_preinit_signal = Arc::new(StdMutex::new(None));
        #[cfg(test)]
        let pause_preinit_step = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_preinit_step.clone());
        #[cfg(test)]
        let force_preinit_signal_once = (self.liteinst_runtime.lock().unwrap().phase
            == LiteinstRuntimePhase::Waiting)
            .then(|| {
                self.global_state
                    .liteinst_runtime
                    .as_ref()
                    .and_then(|runtime| runtime.force_preinit_signal_once.clone())
            })
            .flatten();

        fn arm_preinit_stop(
            held_task_stops: &Option<HeldTaskStops>,
            task: &Stopped,
            event: &Event,
        ) {
            if let Some(slot) = held_task_stops {
                HeldRootStop::arm_empty(slot, task, event)
                    .expect("rearmed an undisarmed preinit task stop lease");
            }
        }

        #[cfg(test)]
        async fn pause_preinit(
            pause: &Option<(usize, mpsc::UnboundedSender<Pid>)>,
            step: usize,
            task: &Stopped,
        ) {
            if let Some((target, sender)) = pause
                && *target == step
            {
                let _ = sender.send(task.pid());
                future::pending::<()>().await;
            }
        }

        #[cfg(test)]
        if self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.fail_preinit)
        {
            return Err(Errno::EPERM.into());
        }

        type SavedInstructions = [u8; 8];

        /// Helper function for tracee_preinit that does the core work.
        async fn setup_special_mmap_page<L: Tool + 'static>(
            owner: &mut TracedTask<L>,
            task: Stopped,
            saved_regs: &libc::user_regs_struct,
            held_task_stops: &Option<HeldTaskStops>,
            reject_activation_signals: bool,
            unexpected_signal: &Arc<StdMutex<Option<Signal>>>,
            #[cfg(test)] pause_preinit_step: &Option<(usize, mpsc::UnboundedSender<Pid>)>,
            #[cfg(test)] force_preinit_signal_once: &Option<Arc<AtomicBool>>,
        ) -> Result<Stopped, TraceError> {
            // NOTE: This point in the code assumes that a specific instruction
            // sequence "SYSCALL; INT3", has been patched into the guest, and
            // that RIP points to the syscall.
            let mut regs = *saved_regs;

            let page_addr = cp::PRIVATE_PAGE_OFFSET;
            let mmap_args = [
                page_addr as u64,
                cp::PRIVATE_PAGE_SIZE as u64,
                (libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) as u64,
                (libc::MAP_PRIVATE | libc::MAP_FIXED | libc::MAP_ANONYMOUS) as u64,
                u64::MAX,
                0,
            ];

            *regs.syscall_mut() = Sysno::mmap as Reg;
            *regs.orig_syscall_mut() = regs.syscall();
            regs.set_args((
                mmap_args[0] as Reg,
                mmap_args[1] as Reg,
                mmap_args[2] as Reg,
                mmap_args[3] as Reg,
                mmap_args[4] as Reg,
                mmap_args[5] as Reg,
            ));

            #[cfg(target_arch = "x86_64")]
            if owner.after_loader_config().is_some() && !owner.command_bootstrap {
                owner.arm_after_loader_syscall_permit_with_instruction(
                    &task,
                    after_loader_task::AfterLoaderSyscallPurpose::TraceePreinit,
                    Sysno::mmap as i64,
                    mmap_args,
                    saved_regs.ip(),
                    [0x0f, 0x05, 0xcc, 0xcc],
                )?;
            }
            task.setregs(&regs)?;
            // Execute the injected mmap call.
            let mut running = RootStopLease::new(task, held_task_stops.clone()).step(None)?;

            // loop until second breakpoint hit after injected syscall.
            #[cfg(test)]
            let mut step = 0;
            let task = loop {
                let (task, event) = running.next_state().await?.assume_stopped();
                arm_preinit_stop(held_task_stops, &task, &event);
                #[cfg(test)]
                let forced_external_sigtrap = event == Event::Signal(Signal::SIGTRAP)
                    && force_preinit_signal_once
                        .as_ref()
                        .is_some_and(|force_once| force_once.load(Ordering::SeqCst));
                #[cfg(not(test))]
                let forced_external_sigtrap = false;
                #[cfg(test)]
                if let Some((target, sender)) = pause_preinit_step
                    && *target == step
                {
                    let _ = sender.send(task.pid());
                    future::pending::<()>().await;
                }
                #[cfg(test)]
                {
                    step += 1;
                }
                match event {
                    Event::Signal(Signal::SIGTRAP) => {
                        let expected_rip = saved_regs
                            .ip()
                            .checked_add(cp::SYSCALL_INSTR_SIZE as u64)
                            .ok_or(Errno::EOVERFLOW)?;
                        if reject_activation_signals
                            && !is_expected_breakpoint_trap(
                                &task,
                                expected_rip,
                                forced_external_sigtrap,
                            )?
                        {
                            #[cfg(test)]
                            if forced_external_sigtrap
                                && let Some(force_once) = force_preinit_signal_once.as_ref()
                            {
                                force_once.store(false, Ordering::SeqCst);
                            }
                            *unexpected_signal.lock().unwrap() = Some(Signal::SIGTRAP);
                            return Err(Errno::EPROTO.into());
                        }
                        break task;
                    }
                    Event::Signal(sig) => {
                        if reject_activation_signals {
                            *unexpected_signal.lock().unwrap() = Some(sig);
                            return Err(Errno::EPROTO.into());
                        }
                        // We can catch spurious signals here, such as SIGWINCH.
                        // All we can do is skip over them.
                        tracing::debug!(
                            "[{}] Skipping {:?} during initialization",
                            task.pid(),
                            event
                        );
                        running = RootStopLease::new(task, held_task_stops.clone()).resume(sig)?;
                    }
                    Event::Seccomp => {
                        #[cfg(target_arch = "x86_64")]
                        if owner.after_loader_config().is_some() && !owner.command_bootstrap {
                            let permit = owner
                                .consume_after_loader_syscall_permit(&task)?
                                .ok_or(Errno::EPROTO)?;
                            if permit.purpose
                                != after_loader_task::AfterLoaderSyscallPurpose::TraceePreinit
                                || permit.number != Sysno::mmap as i64
                                || permit.args != mmap_args
                            {
                                return Err(Errno::EPROTO.into());
                            }
                            let completion = RootStopLease::new(task, held_task_stops.clone())
                                .syscall(None)?
                                .next_state()
                                .await?;
                            let (completed, event) = completion.assume_stopped();
                            arm_preinit_stop(held_task_stops, &completed, &event);
                            if event != Event::Syscall {
                                return Err(Errno::EPROTO.into());
                            }
                            let observed = completed.getregs()?;
                            if observed.orig_syscall() as i64 != Sysno::mmap as i64
                                || observed.args()
                                    != (
                                        mmap_args[0],
                                        mmap_args[1],
                                        mmap_args[2],
                                        mmap_args[3],
                                        mmap_args[4],
                                        mmap_args[5],
                                    )
                            {
                                return Err(Errno::EPROTO.into());
                            }
                            let completed_permit = owner
                                .complete_after_loader_syscall_inflight(&completed)?
                                .ok_or(Errno::EPROTO)?;
                            if completed_permit != permit {
                                return Err(Errno::EPROTO.into());
                            }
                            running = RootStopLease::new(completed, held_task_stops.clone())
                                .resume(None)?;
                            continue;
                        }
                        // The ordinary filter reports this setup mmap only
                        // when the Tool subscribed to it.
                        running = RootStopLease::new(task, held_task_stops.clone()).resume(None)?;
                    }
                    unknown => {
                        panic!("task {} returned unknown event {:?}", task.pid(), unknown);
                    }
                }
            };

            // Make sure we got our desired address.
            assert_eq!(
                Errno::from_ret(task.getregs()?.ret() as usize)?,
                page_addr,
                "Could not mmap address {}",
                page_addr
            );

            cp::populate_mmap_page(task.pid().into(), page_addr)?;

            // Restore our saved registers, including our instruction pointer.
            task.setregs(saved_regs)?;
            Ok(task)
        }

        /// Put the guest into the weird state where it has an
        /// "INT3;SYSCALL;INT3" patched into the code wherever RIP happens to be
        /// pointing. It leaves RIP pointing at the syscall instruction. This
        /// allows forcible injection of syscalls into the guest.
        async fn establish_injection_state(
            mut task: Stopped,
        ) -> Result<(Stopped, libc::user_regs_struct, SavedInstructions), TraceError> {
            #[cfg(target_arch = "x86_64")]
            const SYSCALL_BP: SavedInstructions = [
                0x0f, 0x05, // syscall
                0xcc, // int3
                0xcc, 0xcc, 0xcc, 0xcc, 0xcc, // padding
            ];

            #[cfg(target_arch = "aarch64")]
            const SYSCALL_BP: SavedInstructions = [
                0x01, 0x00, 0x00, 0xd4, // svc 0
                0x20, 0x00, 0x20, 0xd4, // brk 1
            ];

            // Save the original registers so we can restore them later.
            let regs = task.getregs()?;

            // Saved instruction memory
            let ip = AddrMut::from_raw(regs.ip() as usize).ok_or(Errno::EFAULT)?;
            let saved: SavedInstructions = task.read_value(ip)?;

            // Patch the tracee at the current instruction pointer.
            //
            // NOTE: `process_vm_writev` cannot write to write-protected pages,
            // but `PTRACE_POKEDATA` can! Thus, we need to make sure we only
            // write one word-sized chunk at a time. Luckily, the instructions
            // we want to inject fit inside of just one 64-bit word.
            task.write_value(ip.cast(), &SYSCALL_BP)?;

            Ok((task, regs, saved))
        }

        /// Undo the effects of `establish_injection_state` and put the program
        /// code memory and instruction pointer back to normal.
        fn remove_injection_state(
            task: &mut Stopped,
            regs: libc::user_regs_struct,
            saved: SavedInstructions,
        ) -> Result<(), TraceError> {
            // NOTE: Again, because `process_vm_writev` cannot write to
            // write-protected pages, we must write in word-sized chunks with
            // PTRACE_POKEDATA.
            let ip = AddrMut::from_raw(regs.ip() as usize).ok_or(Errno::EFAULT)?;
            task.write_value(ip, &saved)?;
            task.setregs(&regs)?;
            Ok(())
        }

        let (task, regs, prev_state) = establish_injection_state(task).await?;
        let task = setup_special_mmap_page(
            self,
            task,
            &regs,
            &held_task_stops,
            reject_activation_signals,
            &unexpected_preinit_signal,
            #[cfg(test)]
            &pause_preinit_step,
            #[cfg(test)]
            &force_preinit_signal_once,
        )
        .await;
        if let Some(sig) = unexpected_preinit_signal.lock().unwrap().take() {
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::UnexpectedPreinitSignal,
                Error::runtime(
                    self.tid(),
                    "reject unexpected LiteInst activation signal",
                    format!(
                        "received {sig} before the required preload handshake completed: tracee pre-initialization observed an unexpected nested signal"
                    ),
                ),
            );
        }
        let mut task = task?;
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 1, &task).await;

        // Restore registers after adding our temporary injection state.
        remove_injection_state(&mut task, regs, prev_state)?;

        // The command launcher still needs the private page before the
        // thread-start Tool callback.  Exec discards this address space, so
        // vDSO and architectural setup are performed only for the real image
        // at its post-exec stop, where the strict all-syscall filter is active.
        if bootstrap_before_exec {
            return Ok(task);
        }

        #[cfg(target_arch = "x86_64")]
        let defer_vdso_until_after_loader = self.after_loader_config().is_some();
        #[cfg(not(target_arch = "x86_64"))]
        let defer_vdso_until_after_loader = false;
        if !defer_vdso_until_after_loader
            && vdso::is_patch_required(&self.global_state.subscriptions)
        {
            let subscriptions = self.global_state.subscriptions.clone();
            self.begin_tool_callback(task)?;
            let patch_result = vdso::vdso_patch(self, &subscriptions).await;
            task = self.take_tool_callback_stop()?;
            patch_result.expect("unable to patch vdso");
        }
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 2, &task).await;

        // Protect our trampoline page from being written to. We won't need to
        // change this again for the lifetime of the guest process.
        self.begin_tool_callback(task)?;
        let protect_result = self
            .inject_with_retry(
                Mprotect::new()
                    .with_addr(AddrMut::from_raw(cp::TRAMPOLINE_BASE))
                    .with_len(cp::TRAMPOLINE_SIZE)
                    .with_protection(ProtFlags::PROT_READ | ProtFlags::PROT_EXEC),
            )
            .await;
        task = self.take_tool_callback_stop()?;
        protect_result?;
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 3, &task).await;

        // Try to intercept cpuid instructions on x86_64
        #[cfg(target_arch = "x86_64")]
        if self.global_state.subscriptions.has_cpuid() {
            self.begin_tool_callback(task)?;
            self.has_cpuid_interception = match self.cpuid_state().await {
                Ok(initial_state @ (0 | 1)) => match self.intercept_cpuid().await {
                    Ok(()) => match self.cpuid_state().await {
                        Ok(0) => true,
                        Ok(state) => {
                            tracing::error!(
                                state,
                                "ARCH_SET_CPUID succeeded but ARCH_GET_CPUID did not report the disabled state; continuing without CPUID interception"
                            );
                            false
                        }
                        Err(err) => {
                            tracing::error!(
                                "Unable to verify ARCH_SET_CPUID with ARCH_GET_CPUID: {}; continuing without CPUID interception",
                                err
                            );
                            false
                        }
                    },
                    Err(Errno::ENODEV) => {
                        tracing::error!(
                            initial_state,
                            "ARCH_GET_CPUID reported a valid state, but ARCH_SET_CPUID returned ENODEV. The kernel exposes CPUID state without hardware faulting support. On AMD hosts, use Linux 6.17+ upstream or a kernel with CPUID faulting backported; continuing without CPUID interception"
                        );
                        false
                    }
                    Err(err) => {
                        tracing::error!(
                            "Unable to disable CPUID after ARCH_GET_CPUID reported a valid state: {}; continuing without CPUID interception",
                            err
                        );
                        false
                    }
                },
                Ok(state) => {
                    tracing::error!(
                        state,
                        "ARCH_GET_CPUID returned an unexpected state; continuing without CPUID interception"
                    );
                    false
                }
                Err(Errno::ENODEV) => {
                    tracing::error!(
                        "CPUID faulting is unavailable: arch_prctl(ARCH_GET_CPUID) returned ENODEV. On AMD hosts, use Linux 6.17+ upstream or a kernel with CPUID faulting backported; continuing without CPUID interception"
                    );
                    false
                }
                Err(err) => {
                    tracing::error!(
                        "Unable to query CPUID faulting with arch_prctl(ARCH_GET_CPUID): {}; continuing without CPUID interception",
                        err
                    );
                    false
                }
            };
            task = self.take_tool_callback_stop()?;
        }
        #[cfg(test)]
        pause_preinit(&pause_preinit_step, 4, &task).await;

        // Restore registers again after we've injected syscalls so that we
        // don't leave the return value register (%rax) in a dirty state.
        task.setregs(&regs)?;

        Ok(task)
    }

    #[cfg(target_arch = "x86_64")]
    async fn handle_cpuid(
        &mut self,
        mut regs: libc::user_regs_struct,
    ) -> Result<libc::user_regs_struct, TraceError> {
        let eax = regs.rax as u32;
        let ecx = regs.rcx as u32;
        #[cfg(target_arch = "x86_64")]
        self.observe_after_loader_tool_callback("Tool::handle_cpuid_event")
            .map_err(|_| Errno::EOVERFLOW)?;
        let cpuid = cancellable(self.cancel_handler.clone(), async {
            self.process_state
                .clone()
                .handle_cpuid_event(self, eax, ecx)
                .await
        })
        .await
        .ok_or(Errno::EPROTO)??;
        regs.rax = cpuid.eax as u64;
        regs.rbx = cpuid.ebx as u64;
        regs.rcx = cpuid.ecx as u64;
        regs.rdx = cpuid.edx as u64;
        regs.rip += 2;
        self.timer.finalize_requests();
        Ok(regs)
    }

    #[cfg(target_arch = "x86_64")]
    async fn handle_rdtscs(
        &mut self,
        mut regs: libc::user_regs_struct,
        request: Rdtsc,
    ) -> Result<libc::user_regs_struct, TraceError> {
        #[cfg(target_arch = "x86_64")]
        self.observe_after_loader_tool_callback(match request {
            Rdtsc::Tsc => "Tool::handle_rdtsc_event(RDTSC)",
            Rdtsc::Tscp => "Tool::handle_rdtsc_event(RDTSCP)",
        })
        .map_err(|_| Errno::EOVERFLOW)?;
        let retval = cancellable(self.cancel_handler.clone(), async {
            self.process_state
                .clone()
                .handle_rdtsc_event(self, request)
                .await
        })
        .await
        .ok_or(Errno::EPROTO)??;
        regs.rax = retval.tsc & 0xffff_ffffu64;
        regs.rdx = retval.tsc >> 32;
        match request {
            Rdtsc::Tsc => {
                regs.rip += 2;
            }
            Rdtsc::Tscp => {
                regs.rip += 3;
                regs.rcx = retval.aux.unwrap_or(0) as u64;
            }
        }
        self.timer.finalize_requests();
        Ok(regs)
    }

    /// Returns `true` if the signal was actually meant for the timer, and
    /// therefore should not be forwarded to the tool / guest.
    async fn handle_timer(&mut self, task: Stopped) -> Result<(bool, Stopped), TraceError> {
        let armer = self.liteinst_stop_armer(&task);
        let held_task_stops = armer
            .as_ref()
            .map(|armer| Arc::clone(&armer.held_task_stops));
        let mut step = move |task| RootStopLease::new(task, held_task_stops.clone()).step(None);
        let mut observe = |wait: &Wait| {
            if let (Some(armer), Wait::Stopped(task, event)) = (armer.as_ref(), wait) {
                armer.arm(task, event)?;
            }
            Ok(())
        };
        let task = match self
            .timer
            .handle_signal(task, &mut step, &mut observe)
            .await
        {
            Err(HandleFailure::ImproperSignal(task)) => return Ok((false, task)),
            Err(HandleFailure::Cancelled(task)) => return Ok((true, task)),
            Err(HandleFailure::TraceError(e)) => return Err(e),
            Err(HandleFailure::Event(wait)) => self.abort(Ok(wait)).await,
            Ok(task) => task,
        };
        #[cfg(test)]
        if let Some(sender) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_precise_timer_step.as_ref())
        {
            let _ = sender.send(task.pid());
            future::pending::<()>().await;
        }
        self.begin_tool_callback(task)?;
        #[cfg(target_arch = "x86_64")]
        self.observe_after_loader_tool_callback("Tool::handle_timer_event")
            .map_err(|_| Errno::EOVERFLOW)?;
        let _ = cancellable(self.cancel_handler.clone(), async {
            self.process_state.clone().handle_timer_event(self).await
        })
        .await;
        let task = self.take_tool_callback_stop()?;
        self.timer.finalize_requests();
        Ok((true, task))
    }

    /// Handle a state change in the guest, and leave it in a stopped state.
    /// Return the signal that the process would be resumed with, if any.
    ///
    /// Preconditions:
    ///  * running on the ptracer pthread
    ///
    /// Postconditions:
    ///  * guest thread may or may not be stopped, depending on value of GuestNext
    async fn handle_stop_event(&mut self, stopped: Stopped, event: Event) -> Result<Wait, Error> {
        #[cfg(target_arch = "x86_64")]
        let stopped = if self.liteinst_after_loader_forward_inflight.is_some() {
            match self
                .route_after_loader_forward_inflight(stopped, &event)
                .await?
            {
                after_loader_task::AfterLoaderForwardRoute::Continue(stopped) => stopped,
                after_loader_task::AfterLoaderForwardRoute::Completed(wait) => return Ok(wait),
            }
        } else {
            stopped
        };
        #[cfg(target_arch = "x86_64")]
        if let (Some(config), Event::NewChild(_, child)) = (self.after_loader_config(), &event) {
            let observer = stopped.physical_event_observer().ok_or_else(|| {
                Error::runtime(
                    self.tid(),
                    "attach LiteInst child physical observer",
                    "parent stopped capability has no physical observer",
                )
            })?;
            if observer.id() != config.physical_observer.id() {
                return Err(Error::runtime(
                    self.tid(),
                    "attach LiteInst child physical observer",
                    "parent observer differs from the after-loader session observer",
                ));
            }
            child
                .attach_physical_event_observer(&observer)
                .map_err(|error| {
                    Error::runtime(
                        child.pid(),
                        "attach LiteInst child physical observer",
                        format!("{error:?}"),
                    )
                })?;
            config
                .diagnostics
                .record(
                    "physical event observer attached to child",
                    self.timer.diagnostic_clock(),
                    format!(
                        "parent={} child={} observer={:?} generation={:?}",
                        stopped.pid(),
                        child.pid(),
                        observer.id(),
                        child.physical_event_generation(),
                    ),
                )
                .map_err(|error| {
                    Error::runtime(
                        child.pid(),
                        "record LiteInst child physical observer attachment",
                        error.to_string(),
                    )
                })?;
        }
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some()
            && matches!(event, Event::Signal(Signal::SIGTRAP))
            && self.liteinst_entry_guard.is_some_and(|guard| {
                stopped
                    .getregs()
                    .is_ok_and(|registers| guard.address.checked_add(1) == Some(registers.ip()))
            })
        {
            // This controller-installed breakpoint exists only to hold AT_ENTRY
            // while the runtime is loaded. It was absent from the ordinary
            // execution stream, so it must reach the caller without advancing
            // Timer or consuming the first restored guest event.
            return self
                .handle_signal(stopped, Signal::SIGTRAP)
                .await
                .tracee_context(self.tid(), "handle after-loader entry guard");
        }
        #[cfg(target_arch = "x86_64")]
        if matches!(event, Event::Seccomp)
            && let Some(operation) = self.classify_after_loader_trace_only_syscall(&stopped)?
        {
            // The after-loader filter reports every syscall. Operations that
            // the ordinary subscription never selected must retain the old
            // no-callback/no-timer behavior. Admission and the single kernel
            // completion happen before the common Tool event boundary.
            return self
                .forward_after_loader_trace_only_syscall(stopped, operation)
                .await;
        }
        #[cfg(target_arch = "x86_64")]
        self.observe_after_loader_stopped_event(&stopped, &event)?;
        self.timer.observe_event();
        let tid = self.tid();

        #[cfg(test)]
        if let Some((pause, sender)) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_root_stop.as_ref())
        {
            let selected = match &event {
                Event::Seccomp => match pause {
                    RootStopPause::Seccomp => true,
                    RootStopPause::Signal(_) => false,
                },
                Event::Signal(actual) => match pause {
                    RootStopPause::Seccomp => false,
                    RootStopPause::Signal(expected) => expected == actual,
                },
                Event::NewChild(..)
                | Event::Exec(_)
                | Event::VforkDone
                | Event::Exit
                | Event::Stop
                | Event::Syscall => false,
            };
            if selected {
                let _ = sender.send(stopped.pid());
                future::pending::<()>().await;
            }
        }

        match event {
            Event::Signal(sig) => self
                .handle_signal(stopped, sig)
                .await
                .tracee_context(tid, "handle signal-delivery stop"),
            Event::Exec(former_tid) => self
                .handle_exec_event(stopped, former_tid)
                .await
                .tracee_context(tid, "handle exec stop"),
            Event::Seccomp => self.handle_seccomp(stopped).await,
            Event::NewChild(op, child) => self
                .dispatch_new_task(op, stopped, child, None, None)
                .await
                .tracee_context(tid, "handle new tracee stop"),
            Event::VforkDone => self
                .handle_vfork_done_event(stopped)
                .await
                .tracee_context(tid, "handle vfork completion stop"),
            task_state => panic!("unknown task state for tracee {}: {:?}", tid, task_state),
        }
    }

    async fn get_stop_tx(&self) -> Option<(Arc<AtomicBool>, mpsc::Sender<(Pid, Suspended)>)> {
        for child in self.child_threads.lock().await.deref_mut().into_iter() {
            if child.id() == self.tid() {
                return Some((child.suspended.clone(), child.wait_all_stop_tx.take()?));
            }
        }
        None
    }

    // TODO-HUMAN-REVIEW(PR-103): Review rewritten rt_sigreturn tail execution.
    async fn resume_injected_rt_sigreturn(
        &mut self,
        task: Stopped,
        frame: &InjectedSyscallFrame,
    ) -> Result<Wait, TraceError> {
        let mut regs = task.getregs()?;
        frame.copy_to_user_regs(&mut regs);
        *regs.syscall_mut() = Sysno::rt_sigreturn as Reg;
        *regs.orig_syscall_mut() = Sysno::rt_sigreturn as Reg;

        // rt_sigreturn consumes the signal frame at the original guest stack
        // pointer and does not return to its caller. The after-loader all-Trace
        // filter first authenticates this exact guest-frame operation; only
        // then may the kernel consume the frame and follow the restored state.
        *regs.ip_mut() = cp::PRIVATE_PAGE_OFFSET as Reg;
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() && !self.command_bootstrap {
            self.arm_after_loader_syscall_permit(
                &task,
                after_loader_task::AfterLoaderSyscallPurpose::GuestRtSigreturn,
                Sysno::rt_sigreturn as i64,
                [regs.rdi, regs.rsi, regs.rdx, regs.r10, regs.r8, regs.r9],
                cp::PRIVATE_PAGE_OFFSET as u64,
            )?;
        }
        task.setregs(&regs)?;
        let wait = self.resume_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() && !self.command_bootstrap {
            let stopped = match wait {
                Wait::Stopped(stopped, Event::Seccomp) => stopped,
                _ => return Err(Errno::EPROTO.into()),
            };
            let permit = self
                .consume_after_loader_syscall_permit(&stopped)?
                .ok_or(Errno::EPROTO)?;
            let wait = self.resume_stopped(stopped, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            let successor = match &wait {
                Wait::Stopped(stopped, _) => self
                    .complete_after_loader_syscall_successor(stopped)?
                    .ok_or(Errno::EPROTO)?,
                Wait::Exited(_, _) => return Err(Errno::EPROTO.into()),
            };
            if successor != permit {
                return Err(Errno::EPROTO.into());
            }
            return Ok(wait);
        }
        Ok(wait)
    }

    // TODO-HUMAN-REVIEW(PR-102): Review rewritten-syscall dispatch and result handling.
    async fn handle_injected_syscall(
        &mut self,
        mut task: Stopped,
        frame_address: usize,
        trap_rflags: u64,
    ) -> Result<Wait, TraceError> {
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            return Err(Errno::EPROTO.into());
        }
        let mut frame = self.read_injected_syscall_frame(&task, frame_address)?;
        let raw_number = frame.raw_syscall_number();
        let known_number = u32::try_from(raw_number)
            .ok()
            .and_then(|raw| Sysno::new(raw as usize));
        if known_number == Some(Sysno::rt_sigreturn) {
            return self.resume_injected_rt_sigreturn(task, &frame).await;
        }
        let syscall = frame.syscall();
        let (nr, args) = syscall.into_parts();

        frame.emulate_syscall_entry(trap_rflags);
        self.write_injected_syscall_frame(&task, frame_address, &frame)?;

        if !self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == nr)
        {
            self.injected_syscall_frame = Some(frame_address);
            let (task, result) = self.untraced_syscall(task, nr, args).await?;
            self.write_injected_syscall_result(&task, result)?;
            self.injected_syscall_frame = None;
            let signal = self.take_pending_signal_for_resume(
                LiteinstActivationOperation::ResumeInjectedSyscall,
            )?;
            return self.resume_stopped(task, signal)?.next_state().await;
        }

        let span = tracing::trace_span!(
            target: "reverie_ptrace::syscall",
            "syscall.intercept",
            tid = %self.tid(),
            syscall = %nr,
            args = ?args,
            source = "injected-trap",
        );

        async {
            self.injected_syscall_frame = Some(frame_address);
            self.pending_syscall = Some((nr, args));
            self.pending_syscall_already_skipped = false;

            self.begin_tool_callback(task)?;
            #[cfg(target_arch = "x86_64")]
            self.observe_after_loader_tool_callback("Tool::handle_syscall_event(installed trap)")
                .map_err(|_| Errno::EOVERFLOW)?;
            let retval = cancellable(self.cancel_handler.clone(), async {
                self.process_state
                    .clone()
                    .handle_syscall_event(self, syscall)
                    .await
            })
            .await;
            task = self.take_tool_callback_stop()?;

            self.timer.finalize_requests();

            if let Some(retval) = retval {
                let result = match retval {
                    Ok(value) => value,
                    Err(error) => -(error.into_errno().unwrap_or(Errno::EIO).into_raw() as i64),
                };
                self.write_injected_syscall_result(&task, Ok(result))?;
            }

            self.pending_syscall = None;
            self.pending_syscall_already_skipped = false;
            self.injected_syscall_frame = None;
            let signal = self.take_pending_signal_for_resume(
                LiteinstActivationOperation::ResumeInterceptedInjectedSyscall,
            )?;
            let wait = self.resume_stopped(task, signal)?.next_state().await?;
            tracing::trace!(
                target: "reverie_ptrace::syscall",
                "completed injected syscall interception"
            );
            Ok(wait)
        }
        .instrument(span)
        .await
    }

    fn validate_liteinst_handshake(
        &self,
        task: &Stopped,
        frame_address: usize,
        trap_rip: u64,
        ready: bool,
    ) -> Option<LiteinstHandshakeFrame> {
        let config = self.global_state.liteinst_runtime.as_ref()?;
        let address = Addr::from_raw(frame_address)?;
        let frame: LiteinstHandshakeFrame = task.read_value(address).ok()?;
        if frame.version != 4
            || frame.helper_stack_top < 8
            || frame.helper_stack_top & 0xf != 0
            || trap_rip
                != if ready {
                    frame.ready_rip
                } else {
                    frame.begin_rip
                }
        {
            return None;
        }
        let maps = guest_maps(task.pid())?;
        let preload_code = |address| {
            maps.iter().any(|mapping| {
                #[cfg(target_arch = "x86_64")]
                if let Some(caller) = &config.after_loader {
                    return mapping.readable
                        && mapping.executable
                        && !mapping.writable
                        && !mapping.shared
                        && after_loader_task::image_mapping_matches(
                            &caller.sealed_runtime.image,
                            mapping,
                        )
                        && mapping.contains(address);
                }
                mapping.executable
                    && mapping.path.as_ref() == Some(&config.preload)
                    && mapping.contains(address)
            })
        };
        if ![
            frame.begin_rip,
            frame.ready_rip,
            frame.install_helper,
            frame.helper_return,
            frame.helper_return_rip,
            frame.syscall_trap_rip,
            frame.syscall_trap_return_rip,
        ]
        .into_iter()
        .all(preload_code)
        {
            return None;
        }
        let frame_readable = maps
            .iter()
            .any(|mapping| mapping.readable && mapping.contains(frame_address as u64));
        let helper_stack_map = maps.iter().find(|mapping| {
            mapping.writable && mapping.contains(frame.helper_stack_top.saturating_sub(8))
        });
        let install_result = GuestRange::new(
            frame.install_result,
            core::mem::size_of::<LiteinstInstallResult>() as u64,
        )?;
        let install_result_writable = maps.iter().any(|mapping| {
            Some((mapping.start, mapping.end))
                == helper_stack_map.map(|stack| (stack.start, stack.end))
                && mapping.readable
                && mapping.writable
                && mapping.contains_range(install_result)
        });
        (frame_readable && helper_stack_map.is_some() && install_result_writable).then_some(frame)
    }

    fn install_liteinst_entry_guard(&mut self, task: &mut Stopped) -> Result<(), TraceError> {
        if self.global_state.liteinst_runtime.is_none() {
            return Ok(());
        }
        if self.liteinst_entry_guard.is_some() {
            return Err(Errno::EALREADY.into());
        }
        let address = guest_auxv_entry(task.pid(), libc::AT_ENTRY).ok_or(Errno::ENOEXEC)?;
        let range =
            GuestRange::new(address, core::mem::size_of::<u64>() as u64).ok_or(Errno::ENOEXEC)?;
        if !guest_maps(task.pid()).is_some_and(|maps| {
            maps.iter().any(|mapping| {
                mapping.readable && mapping.executable && mapping.contains_range(range)
            })
        }) {
            return Err(Errno::ENOEXEC.into());
        }
        let read_address = Addr::<u64>::from_raw(address as usize).ok_or(Errno::EFAULT)?;
        let guard_address = AddrMut::<u64>::from_raw(address as usize).ok_or(Errno::EFAULT)?;
        let saved_instruction: u64 = task.read_value(read_address)?;
        if saved_instruction as u8 == 0xcc {
            return Err(Errno::EPROTO.into());
        }
        let guarded_instruction = (saved_instruction & !0xff) | 0xcc;
        task.write_value(guard_address, &guarded_instruction)?;
        let observed: u64 = task.read_value(read_address)?;
        if observed != guarded_instruction {
            let _ = task.write_value(guard_address, &saved_instruction);
            return Err(Errno::EIO.into());
        }
        self.liteinst_entry_guard = Some(LiteinstEntryGuard {
            address,
            saved_instruction,
        });
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            self.liteinst_after_loader_guard = Some(
                crate::entry_call::EntryGuard::prepare(
                    self.after_loader_identity(task)
                        .map_err(|_| Errno::EPROTO)?,
                    saved_instruction.to_ne_bytes(),
                )
                .map_err(|_| Errno::EPROTO)?,
            );
        }
        Ok(())
    }

    fn restore_liteinst_entry_guard(&mut self, task: &mut Stopped) -> Result<(), TraceError> {
        let guard = self.liteinst_entry_guard.ok_or(Errno::EPROTO)?;
        let read_address = Addr::<u64>::from_raw(guard.address as usize).ok_or(Errno::EFAULT)?;
        let address = AddrMut::<u64>::from_raw(guard.address as usize).ok_or(Errno::EFAULT)?;
        let guarded_instruction = (guard.saved_instruction & !0xff) | 0xcc;
        let observed: u64 = task.read_value(read_address)?;
        if observed != guarded_instruction {
            return Err(Errno::EPROTO.into());
        }
        task.write_value(address, &guard.saved_instruction)?;
        let restored: u64 = task.read_value(read_address)?;
        if restored != guard.saved_instruction {
            return Err(Errno::EIO.into());
        }
        self.liteinst_entry_guard = None;
        Ok(())
    }

    fn classify_liteinst_trap(
        &mut self,
        task: &Stopped,
        regs: &libc::user_regs_struct,
    ) -> Option<LiteinstTrap> {
        let config = self.global_state.liteinst_runtime.as_ref()?;
        if regs.rax == config.begin_marker {
            let frame =
                self.validate_liteinst_handshake(task, regs.rdi as usize, regs.ip(), false)?;
            #[cfg(target_arch = "x86_64")]
            let after_loader = self.after_loader_config().is_some();
            let mut state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Waiting {
                return None;
            }
            state.phase = LiteinstRuntimePhase::Bootstrap;
            state.frame = Some(frame);
            #[cfg(target_arch = "x86_64")]
            if after_loader {
                state.after_loader_syscall_trap = Some(LiteinstTrapSiteProvenance {
                    generation: state.generation,
                    rip: frame.syscall_trap_rip,
                });
            }
            return Some(LiteinstTrap::HandshakeBegin);
        }
        if regs.rax == config.ready_marker {
            let frame =
                self.validate_liteinst_handshake(task, regs.rdi as usize, regs.ip(), true)?;
            let state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Bootstrap || state.frame != Some(frame) {
                return None;
            }
            return Some(LiteinstTrap::HandshakeReady);
        }
        if regs.rax != config.syscall_marker {
            return None;
        }
        #[cfg(target_arch = "x86_64")]
        if let Some(after_loader) = self.after_loader_config() {
            let (phase, generation, trap) = {
                let state = self.liteinst_runtime.lock().unwrap();
                (
                    state.phase,
                    state.generation,
                    state.after_loader_syscall_trap,
                )
            };
            if phase == LiteinstRuntimePhase::Ready {
                // After-loader mode never installs the runtime syscall hook and
                // clears the live handshake frame before Ready. A marker value
                // at any ordinary guest breakpoint is therefore just guest
                // register state. Refuse only the authenticated runtime trap
                // instruction retained from that cleared frame.
                let trap = trap?;
                let trap_address = trap.rip.checked_sub(1)?;
                let trap_range = GuestRange::new(trap_address, 2)?;
                let maps = guest_maps(task.pid())?;
                let mapping = maps
                    .iter()
                    .find(|mapping| mapping.contains_range(trap_range))?;
                let address = Addr::from_raw(trap_address as usize)?;
                let trap_opcode: u8 = task.read_value(address).ok()?;
                let si_code = task.getsiginfo().ok()?.si_code;
                return trap
                    .validates_retired_runtime_trap(
                        generation,
                        regs.rax,
                        config.syscall_marker,
                        regs.ip(),
                        si_code,
                        trap_opcode,
                        after_loader.sealed_runtime.image.device,
                        after_loader.sealed_runtime.image.inode,
                        mapping,
                    )
                    .then_some(LiteinstTrap::Invalid);
            }
        }
        let handshake = self.liteinst_runtime.lock().unwrap().frame?;
        if regs.ip() != handshake.syscall_trap_rip {
            return None;
        }
        let stack_address = usize::try_from(regs.rsp).ok()?;
        let frame_address = usize::try_from(regs.rdi).ok()?;
        let maps = guest_maps(task.pid())?;
        let controller_stack = maps.iter().find(|mapping| {
            mapping.readable
                && mapping.writable
                && mapping.contains(regs.rsp)
                && mapping.contains(
                    regs.rsp
                        .saturating_add(core::mem::size_of::<u64>() as u64 - 1),
                )
                && mapping.contains(regs.rdi)
                && mapping.contains(
                    regs.rdi
                        .saturating_add(core::mem::size_of::<InjectedSyscallFrame>() as u64 - 1),
                )
        });
        if controller_stack.is_none() || regs.rsp.abs_diff(regs.rdi) > 128 * 1024 {
            return None;
        }
        let return_address: u64 = task.read_value(Addr::from_raw(stack_address)?).ok()?;
        if return_address != handshake.syscall_trap_return_rip {
            // A same-process caller can find the raw trap entry, but only the
            // hidden runtime wrapper produces this exact inner return site.
            return None;
        }
        let frame = match self.read_injected_syscall_frame(task, frame_address) {
            Ok(frame) => frame,
            Err(_) => return Some(LiteinstTrap::Invalid),
        };
        let state = self.liteinst_runtime.lock().unwrap();
        let active_provenance = state
            .active_hooks
            .contains_key(&frame.instruction_pointer());
        if state.phase != LiteinstRuntimePhase::Ready
            || state.ready_generation != Some(state.generation)
            || !active_provenance
        {
            return Some(LiteinstTrap::Invalid);
        }
        Some(LiteinstTrap::Syscall(frame_address))
    }

    async fn handle_sigtrap(
        &mut self,
        mut task: Stopped,
    ) -> Result<HandleSignalResult, TraceError> {
        let resumed_by_gdb_step = self
            .resumed_by_gdb
            .is_some_and(|action| matches!(action, ResumeAction::Step(_)));
        let mut regs = task.getregs()?;
        if let Some(guard) = self.liteinst_entry_guard
            && regs.ip() == guard.address.saturating_add(1)
        {
            #[cfg(target_arch = "x86_64")]
            if self.after_loader_config().is_some() {
                match self.run_after_loader(task).await {
                    Ok(task) => {
                        return Ok(HandleSignalResult::SignalSuppressed(
                            self.resume_stopped(task, None)?.next_state().await?,
                        ));
                    }
                    Err(error) => {
                        self.record_liteinst_failure(
                            LiteinstActivationFailureReason::AfterLoaderCall,
                            error,
                        );
                        return Err(Errno::EPROTO.into());
                    }
                }
            }
            let address = Addr::from_raw(guard.address as usize).ok_or(Errno::EFAULT)?;
            let observed: u64 = task.read_value(address)?;
            let guarded_instruction = (guard.saved_instruction & !0xff) | 0xcc;
            if observed != guarded_instruction {
                return Err(Errno::EPROTO.into());
            }
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::ExecutableEntryBeforeHandshake,
                Error::runtime(
                    self.tid(),
                    "verify LiteInst runtime before executable entry",
                    format!(
                        "tracee reached guarded executable entry {:#x} before the required preload handshake completed",
                        guard.address
                    ),
                ),
            );
            return Err(Errno::EPROTO.into());
        }
        match self.classify_liteinst_trap(&task, &regs) {
            Some(LiteinstTrap::HandshakeBegin) => {
                return Ok(HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                ));
            }
            Some(LiteinstTrap::HandshakeReady) => {
                if let Err(error) = self.restore_liteinst_entry_guard(&mut task) {
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::RestoreExecutableEntryGuard,
                        Error::runtime(
                            self.tid(),
                            "restore LiteInst executable-entry guard",
                            error.to_string(),
                        ),
                    );
                    return Err(error);
                }
                {
                    let mut state = self.liteinst_runtime.lock().unwrap();
                    if state.phase != LiteinstRuntimePhase::Bootstrap {
                        return Err(Errno::EPROTO.into());
                    }
                    state.phase = LiteinstRuntimePhase::Ready;
                    state.ready_generation = Some(state.generation);
                }
                return Ok(HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                ));
            }
            Some(LiteinstTrap::Syscall(frame_address)) => {
                if let Some(stats) = self
                    .global_state
                    .liteinst_runtime
                    .as_ref()
                    .and_then(|config| config.instrumentation_stats.as_ref())
                {
                    stats.lock().unwrap().record_direct_hook();
                }
                let next_state = self
                    .handle_injected_syscall(task, frame_address, regs.eflags)
                    .await?;
                return Ok(HandleSignalResult::SignalSuppressed(next_state));
            }
            Some(LiteinstTrap::Invalid) => return Err(Errno::EPROTO.into()),
            None => {}
        }
        let phase = self.liteinst_runtime.lock().unwrap().phase;
        if self.global_state.liteinst_runtime.is_some() && phase != LiteinstRuntimePhase::Ready {
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::UnexpectedActivationTrap,
                Error::runtime(
                    self.tid(),
                    "reject unexpected LiteInst activation trap",
                    format!(
                        "received SIGTRAP at RIP {:#x} with RAX {:#x} that matched neither the entry guard nor a validated runtime handshake (phase {phase:?})",
                        regs.ip(), regs.rax
                    ),
                ),
            );
            return Err(Errno::EPROTO.into());
        }
        // TODO-HUMAN-REVIEW(PR-103): Review rewritten-trap provenance validation.
        if let Some(trap) = self.global_state.injected_syscall_trap.as_ref()
            && regs.rax == trap.marker
        {
            if let Ok(frame) = self.read_injected_syscall_frame(&task, regs.rdi as usize)
                && trap.validates_site_provenance(task.pid(), regs.ip(), &frame)
            {
                let next_state = self
                    .handle_injected_syscall(task, regs.rdi as usize, regs.eflags)
                    .await?;
                return Ok(HandleSignalResult::SignalSuppressed(next_state));
            }
            return Ok(HandleSignalResult::SignalToDeliver(task, Signal::SIGTRAP));
        }

        let rip_minus_one = regs.ip() - 1;

        Ok(if self.breakpoints.contains_key(&rip_minus_one) {
            *regs.ip_mut() = rip_minus_one;
            let next_state = self.resume_from_swbreak(task, regs).await?;
            HandleSignalResult::SignalSuppressed(next_state)
        } else if resumed_by_gdb_step {
            self.notify_gdb_stop(StopReason::stopped(
                task.pid(),
                self.pid(),
                StopEvent::Signal(Signal::SIGTRAP),
                regs.into(),
            ))
            .await?;
            let running = self
                .await_gdb_resume(task, ExpectedGdbResume::Resume)
                .await?;
            HandleSignalResult::SignalSuppressed(running.next_state().await?)
        } else {
            let running = self.resume_stopped(task, None)?;
            HandleSignalResult::SignalSuppressed(running.next_state().await?)
        })
    }

    async fn handle_sigstop(&mut self, task: Stopped) -> Result<HandleSignalResult, TraceError> {
        let resumed_by_gdb_step = self
            .resumed_by_gdb
            .is_some_and(|action| matches!(action, ResumeAction::Step(_)));
        debug_assert!(!resumed_by_gdb_step);
        if let Some((suspended_flag, stop_tx)) = self.get_stop_tx().await {
            let notify_stop_tx = stop_tx
                .send((
                    task.pid(),
                    Suspended {
                        waker: self.exit_suspend_tx.clone(),
                        suspended: suspended_flag,
                    },
                ))
                .await;
            drop(stop_tx);
            if notify_stop_tx.is_ok()
                && let Some(rx) = self.exit_suspend_rx.as_mut()
                && rx.recv().await.is_none()
            {
                tracing::warn!(
                    tid = %self.tid(),
                    "tracee suspension channel closed before resume"
                );
            }
        }
        Ok(HandleSignalResult::SignalSuppressed(
            self.resume_stopped(task, None)?.next_state().await?,
        ))
    }

    #[cfg(target_arch = "x86_64")]
    async fn handle_sigsegv(&mut self, task: Stopped) -> Result<HandleSignalResult, TraceError> {
        let regs = task.getregs()?;
        let trap_info = Addr::from_raw(regs.rip as usize)
            .and_then(|addr| task.read_value(addr).ok())
            .and_then(SegfaultTrapInfo::decode_segfault);
        Ok(match trap_info {
            Some(SegfaultTrapInfo::Cpuid)
                if self.global_state.subscriptions.has_cpuid() && self.has_cpuid_interception =>
            {
                self.begin_tool_callback(task)?;
                let register_result = self.handle_cpuid(regs).await;
                let task = self.take_tool_callback_stop()?;
                let regs = register_result?;
                task.setregs(&regs)?;
                HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                )
            }
            Some(SegfaultTrapInfo::Rdtscs(req)) if self.global_state.subscriptions.has_rdtsc() => {
                self.begin_tool_callback(task)?;
                let register_result = self.handle_rdtscs(regs, req).await;
                let task = self.take_tool_callback_stop()?;
                let regs = register_result?;
                task.setregs(&regs)?;
                HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                )
            }
            _ => HandleSignalResult::SignalToDeliver(task, Signal::SIGSEGV),
        })
    }

    #[cfg(not(target_arch = "x86_64"))]
    async fn handle_sigsegv(&mut self, task: Stopped) -> Result<HandleSignalResult, TraceError> {
        Ok(HandleSignalResult::SignalToDeliver(task, Signal::SIGSEGV))
    }

    fn liteinst_activation_in_progress(&self) -> bool {
        #[cfg(test)]
        let test_activation_bypass = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.activate_without_handshake);
        #[cfg(not(test))]
        let test_activation_bypass = false;

        self.global_state.liteinst_runtime.is_some()
            && self.liteinst_runtime.lock().unwrap().phase != LiteinstRuntimePhase::Ready
            && !test_activation_bypass
    }

    fn record_liteinst_failure(&mut self, reason: LiteinstActivationFailureReason, error: Error) {
        let stage = match self.liteinst_runtime.lock().unwrap().phase {
            LiteinstRuntimePhase::Ready => LiteinstActivationStage::PostReady,
            LiteinstRuntimePhase::PreExec
            | LiteinstRuntimePhase::Waiting
            | LiteinstRuntimePhase::Bootstrap => LiteinstActivationStage::PreReady,
        };
        let failure = LiteinstActivationFailure::new(stage, reason, error);
        if let Some(runtime) = self.global_state.liteinst_runtime.clone() {
            let mut slot = runtime.session_failure.lock().unwrap();
            if slot.is_none() {
                *slot = Some(format!("tracee {}: {failure}", self.tid()));
                drop(slot);
                runtime.session_failure_changed.notify_waiters();
            }
        }
        self.liteinst_failure = Some(failure);
    }

    fn reject_liteinst_activation_signal(
        &mut self,
        sig: Signal,
        reason: LiteinstActivationFailureReason,
        detail: impl Into<String>,
    ) -> TraceError {
        self.record_liteinst_failure(
            reason,
            Error::runtime(
                self.tid(),
                "reject unexpected LiteInst activation signal",
                format!(
                    "received {sig} before the required preload handshake completed: {}",
                    detail.into()
                ),
            ),
        );
        Errno::EPROTO.into()
    }

    fn take_pending_signal_for_resume(
        &mut self,
        operation: LiteinstActivationOperation,
    ) -> Result<Option<Signal>, TraceError> {
        let signal = self.pending_signal.take();
        if self.liteinst_activation_in_progress()
            && let Some(sig) = signal
        {
            return Err(self.reject_liteinst_activation_signal(
                sig,
                LiteinstActivationFailureReason::SignalBeforeHandshake(operation),
                format!(
                    "{} attempted to deliver a queued signal",
                    operation.as_str()
                ),
            ));
        }
        Ok(signal)
    }

    fn validate_nested_liteinst_activation_signal(
        &mut self,
        task: &Stopped,
        sig: Signal,
        operation: LiteinstActivationOperation,
        expected_trap: NestedTrapExpectation,
        forced_external_for_test: bool,
    ) -> Result<(), TraceError> {
        if !self.liteinst_activation_in_progress() {
            return Ok(());
        }
        let expected = sig == Signal::SIGTRAP
            && match expected_trap {
                NestedTrapExpectation::None => false,
                NestedTrapExpectation::SyscallSkip { pre_rip } => {
                    is_expected_syscall_skip_trap(task, pre_rip, forced_external_for_test)?
                }
                NestedTrapExpectation::Breakpoint(expected_rip) => {
                    is_expected_breakpoint_trap(task, expected_rip, forced_external_for_test)?
                }
                NestedTrapExpectation::PrivateSyscall(expected_rip) => {
                    is_expected_private_syscall_trap(task, expected_rip, forced_external_for_test)?
                }
            };
        if expected {
            return Ok(());
        }
        Err(self.reject_liteinst_activation_signal(
            sig,
            LiteinstActivationFailureReason::UnexpectedControllerProvenance(operation),
            format!(
                "{} observed a nested signal without the expected controller provenance",
                operation.as_str()
            ),
        ))
    }

    // handle ptrace signal delivery stop
    async fn handle_signal(&mut self, task: Stopped, sig: Signal) -> Result<Wait, TraceError> {
        tracing::debug!("[{}] handle_signal: received signal {}", task.pid(), sig);
        if self.liteinst_activation_in_progress() {
            match sig {
                Signal::SIGTRAP => {}
                Signal::SIGSEGV => {
                    return match self.handle_sigsegv(task).await? {
                        HandleSignalResult::SignalSuppressed(wait) => Ok(wait),
                        HandleSignalResult::SignalToDeliver(_, _) => {
                            Err(self.reject_liteinst_activation_signal(
                                sig,
                                LiteinstActivationFailureReason::UnexpectedActivationSignal,
                                "the fault was not a subscribed, controller-intercepted CPUID or RDTSC instruction",
                            ))
                        }
                    };
                }
                sig if sig == Timer::signal_type() => {
                    let (was_timer, task) = self.handle_timer(task).await?;
                    if !was_timer {
                        return Err(self.reject_liteinst_activation_signal(
                            sig,
                            LiteinstActivationFailureReason::UnexpectedActivationSignal,
                            "the signal was not generated by this tracee's controller timer",
                        ));
                    }
                    return self.resume_stopped(task, None)?.next_state().await;
                }
                sig => {
                    return Err(self.reject_liteinst_activation_signal(
                        sig,
                        LiteinstActivationFailureReason::UnexpectedActivationSignal,
                        "the signal is outside the activation allowlist",
                    ));
                }
            }
        }
        let result = match sig {
            Signal::SIGSEGV => self.handle_sigsegv(task).await?,
            Signal::SIGSTOP => self.handle_sigstop(task).await?,
            Signal::SIGTRAP => self.handle_sigtrap(task).await?,
            sig if sig == Timer::signal_type() => {
                let (was_timer, task) = self.handle_timer(task).await?;
                if was_timer {
                    HandleSignalResult::SignalSuppressed(
                        self.resume_stopped(task, None)?.next_state().await?,
                    )
                } else {
                    HandleSignalResult::SignalToDeliver(task, sig)
                }
            }
            sig => HandleSignalResult::SignalToDeliver(task, sig),
        };

        match result {
            HandleSignalResult::SignalSuppressed(wait) => Ok(wait),
            HandleSignalResult::SignalToDeliver(task, sig) => {
                self.begin_tool_callback(task)?;
                #[cfg(target_arch = "x86_64")]
                self.observe_after_loader_tool_callback("Tool::handle_signal_event")
                    .map_err(|_| Errno::EOVERFLOW)?;
                let signal_result = cancellable(self.cancel_handler.clone(), async {
                    self.process_state
                        .clone()
                        .handle_signal_event(self, sig)
                        .await
                })
                .await;
                let task = self.take_tool_callback_stop()?;
                let sig = match signal_result {
                    Some(result) => result?,
                    None => Some(sig),
                };
                self.timer.finalize_requests();
                Ok(self.resume_stopped(task, sig)?.next_state().await?)
            }
        }
    }

    fn reject_liteinst_nonleader_exec(&mut self, former_tid: Pid) -> TraceError {
        self.record_liteinst_failure(
            LiteinstActivationFailureReason::PostStartExec,
            Error::runtime(
                self.tid(),
                "reject LiteInst post-start exec",
                format!(
                    "exec requires the original thread-group leader (former tid {former_tid}, event tid {}, pid {})",
                    self.tid(), self.pid()
                ),
            ),
        );
        Errno::ENOTSUPP.into()
    }

    // PTRACE_GETEVENTMSG reports the caller's former TID. A nonleader exec
    // already has the leader's TID at this stop, so is_main_thread alone cannot
    // establish which thread replaced the image.
    async fn handle_exec_event(
        &mut self,
        task: Stopped,
        former_tid: Pid,
    ) -> Result<Wait, TraceError> {
        // PTRACE_EVENT_EXEC proves replacement succeeded. Clear before any
        // post-exec Tool callback; failed exec attempts retain launch provenance.
        self.command_bootstrap = false;
        if self.global_state.liteinst_runtime.is_some() {
            if former_tid != self.tid() {
                return Err(self.reject_liteinst_nonleader_exec(former_tid));
            }
            let state = self.liteinst_runtime.lock().unwrap();
            #[cfg(target_arch = "x86_64")]
            if self.after_loader_config().is_some() && state.generation != 0 {
                drop(state);
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::AfterLoaderCall,
                    Error::runtime(
                        self.tid(),
                        "LiteInst after-loader call",
                        "second exec is outside the fixed fixture",
                    ),
                );
                return Err(Errno::ENOTSUPP.into());
            }
            if state.phase != LiteinstRuntimePhase::PreExec
                && !(state.phase == LiteinstRuntimePhase::Ready && self.is_main_thread())
            {
                let phase = state.phase;
                drop(state);
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::PostStartExec,
                    Error::runtime(
                        self.tid(),
                        "reject LiteInst post-start exec",
                        format!(
                            "exec requires an activated thread-group leader (phase {phase:?}, tid {}, pid {})",
                            self.tid(), self.pid()
                        ),
                    ),
                );
                return Err(Errno::ENOTSUPP.into());
            }
            let next = state.after_exec();
            drop(state);
            let next = match next {
                Ok(next) => next,
                Err(error) => {
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::PostStartExec,
                        Error::runtime(
                            self.tid(),
                            "advance LiteInst execution generation",
                            error.to_string(),
                        ),
                    );
                    return Err(error.into());
                }
            };
            #[cfg(target_arch = "x86_64")]
            if self.after_loader_config().is_some() {
                let observer = task.physical_event_observer().ok_or(Errno::EPROTO)?;
                observer.bind_exec_generation(task.physical_event_generation(), next.generation);
            }
            // The kernel has replaced this address space. Other holders of the
            // old image's state must not observe this reset, and no saved code
            // or controller-stack address may be reused by the new image.
            self.liteinst_runtime = Arc::new(StdMutex::new(next));
            self.liteinst_entry_guard = None;
            #[cfg(target_arch = "x86_64")]
            {
                if self.liteinst_after_loader_syscall_permit.is_some()
                    || self.liteinst_after_loader_syscall_inflight.is_some()
                    || self.liteinst_after_loader_forward_inflight.is_some()
                    || self.liteinst_after_loader_private_call.is_some()
                    || self.liteinst_after_loader_private_state.is_some()
                {
                    return Err(Errno::EPROTO.into());
                }
                self.liteinst_after_loader_guard = None;
            }
            self.injected_syscall_frame = None;
            self.pending_syscall_already_skipped = false;
        }
        // execve/execveat are tail injected, however, after exec, the new
        // program start as a clean slate, hence it is actually ok to do either
        // inject or tail inject after execve succeeded.
        self.pending_syscall = None;

        // TODO: Update PID? Need to write a test checking this.

        // Step the tracee to get the SIGTRAP that immediately follows the
        // PTRACE_EVENT_EXEC. We can't call `tracee_preinit` until after this
        // because when it tries to step the tracee, it'll get this SIGTRAP
        // signal instead.
        let task = if self.global_state.liteinst_runtime.is_some() {
            let expected_post_exec_rip = task.getregs()?.ip();
            let wait = self.step_stopped(task, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)) => {
                    #[cfg(test)]
                    let forced_external_sigtrap = self
                        .global_state
                        .liteinst_runtime
                        .as_ref()
                        .and_then(|runtime| runtime.force_post_exec_signal_once.as_ref())
                        .is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst));
                    #[cfg(not(test))]
                    let forced_external_sigtrap = false;
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        Signal::SIGTRAP,
                        LiteinstActivationOperation::WaitForPostExecTrap,
                        NestedTrapExpectation::Breakpoint(expected_post_exec_rip),
                        forced_external_sigtrap,
                    )?;
                    task
                }
                Wait::Stopped(task, Event::Signal(sig)) => {
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        sig,
                        LiteinstActivationOperation::WaitForPostExecTrap,
                        NestedTrapExpectation::None,
                        false,
                    )?;
                    unreachable!("activation validation must reject a non-SIGTRAP signal")
                }
                Wait::Stopped(_, event) => {
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::UnexpectedPostExecEvent,
                        Error::runtime(
                            self.tid(),
                            "validate LiteInst post-exec trap",
                            format!(
                                "received unexpected {event:?} before tracee pre-initialization"
                            ),
                        ),
                    );
                    return Err(Errno::EPROTO.into());
                }
                Wait::Exited(pid, exit_status) => {
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::ExitedBeforePostExecTrap,
                        Error::runtime(
                            pid,
                            "validate LiteInst post-exec trap",
                            format!(
                                "tracee exited with {exit_status:?} before the required post-exec SIGTRAP"
                            ),
                        ),
                    );
                    return Err(Errno::EPROTO.into());
                }
            }
        } else {
            let (task, event) = self
                .step_stopped(task, None)?
                .wait_for_signal(Signal::SIGTRAP)
                .await?
                .assume_stopped();
            assert_eq!(event, Event::Signal(Signal::SIGTRAP));
            self.arm_liteinst_stop(&task, &event);
            task
        };
        let mut task = self.tracee_preinit(task).await?;
        if let Err(error) = self.install_liteinst_entry_guard(&mut task) {
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::InstallExecutableEntryGuard,
                Error::runtime(
                    self.tid(),
                    "install LiteInst executable-entry guard",
                    error.to_string(),
                ),
            );
            return Err(error);
        }

        #[cfg(test)]
        if self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.activate_without_handshake)
        {
            if let Err(error) = self.restore_liteinst_entry_guard(&mut task) {
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::RestoreExecutableEntryGuard,
                    Error::runtime(
                        self.tid(),
                        "restore test LiteInst executable-entry guard",
                        error.to_string(),
                    ),
                );
                return Err(error);
            }
            {
                let mut state = self.liteinst_runtime.lock().unwrap();
                state.phase = LiteinstRuntimePhase::Ready;
                state.ready_generation = Some(state.generation);
            }
        }

        self.begin_tool_callback(task)?;
        #[cfg(target_arch = "x86_64")]
        self.observe_after_loader_tool_callback("Tool::handle_post_exec")
            .map_err(|_| Errno::EOVERFLOW)?;
        let post_exec = cancellable(self.cancel_handler.clone(), async {
            self.process_state.clone().handle_post_exec(self).await
        })
        .await;
        task = self.take_tool_callback_stop()?;
        if let Some(post_exec) = post_exec {
            post_exec?;
        }
        self.timer.finalize_requests();

        if self.attached_by_gdb {
            let request_tx = self.gdb_request_tx.clone();
            let resume_tx = self.gdb_resume_tx.clone();

            let proc_exe = format!("/proc/{}/exe", task.pid());
            let exe = std::fs::read_link(&proc_exe).unwrap_or_else(|err| {
                tracing::warn!(
                    tid = %self.tid(),
                    path = %proc_exe,
                    error = %err,
                    "failed to resolve executable after exec; reporting procfs path to GDB"
                );
                proc_exe.clone().into()
            });

            let stopped = StoppedInferior {
                reason: StopReason::stopped(
                    task.pid(),
                    self.pid(),
                    StopEvent::Exec(exe),
                    task.getregs()?.into(),
                ),
                request_tx: request_tx.ok_or(Errno::EIO)?,
                resume_tx: resume_tx.ok_or(Errno::EIO)?,
            };

            // NB: notify initial gdb stop, this is the first time we can
            // tell gdb tracee is ready, because a new memory map has been
            // loaded (due to execve). Otherwise gdb may try to manipulate
            // old process' address space.
            if let Some(attach_tx) = self.gdb_stop_tx.as_ref()
                && attach_tx.send(stopped).await.is_err()
            {
                tracing::warn!(
                    tid = %self.tid(),
                    "GDB stop channel closed while reporting exec"
                );
                self.attached_by_gdb = false;
                return self.step_stopped(task, None)?.next_state().await;
            }
            let running = self
                .await_gdb_resume(task, ExpectedGdbResume::Resume)
                .await?;
            Ok(running.next_state().await?)
        } else {
            let running = if self.global_state.liteinst_runtime.is_some() {
                self.resume_stopped(task, None)?
            } else {
                self.step_stopped(task, None)?
            };
            Ok(running.next_state().await?)
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_arch_prctl<S: SyscallInfo>(
        &mut self,
        task: Stopped,
        syscall: S,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        let (nr, args) = syscall.into_parts();
        self.untraced_syscall(task, nr, args).await
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_prctl(
        &mut self,
        task: Stopped,
        option: libc::c_int,
        arg2: usize,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        self.untraced_syscall(
            task,
            Sysno::prctl,
            SyscallArgs::new(option as usize, arg2, 0, 0, 0, 0),
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_get_tsc_state(
        &mut self,
        task: Stopped,
        scratch_address: usize,
    ) -> Result<(Stopped, Result<Result<libc::c_int, Errno>, String>), TraceError> {
        // PR_GET_TSC writes a c_int through a tracee pointer. Reuse the
        // already-validated helper return slot: its original eight bytes are
        // saved before this call, the helper return address replaces them
        // before execution, and every exit path restores them.
        let (task, result) = self
            .liteinst_prctl(task, libc::PR_GET_TSC, scratch_address)
            .await?;
        let result = match result {
            Ok(0) => match Addr::<libc::c_int>::from_raw(scratch_address) {
                Some(address) => task
                    .read_value(address)
                    .map(Ok)
                    .map_err(|error| format!("read PR_GET_TSC state: {error}")),
                None => Err("PR_GET_TSC scratch address is null".to_owned()),
            },
            Ok(result) => Err(format!("PR_GET_TSC returned unexpected value {result}")),
            Err(error) => Ok(Err(error)),
        };
        Ok((task, result))
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_set_tsc_state(
        &mut self,
        task: Stopped,
        state: libc::c_int,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        self.liteinst_prctl(task, libc::PR_SET_TSC, state as usize)
            .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_get_cpuid_state(
        &mut self,
        task: Stopped,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.liteinst_arch_prctl(
            task,
            ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_GET_CPUID(None)),
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn liteinst_set_cpuid_state(
        &mut self,
        task: Stopped,
        state: u64,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        use reverie::syscalls::ArchPrctl;
        use reverie::syscalls::ArchPrctlCmd;

        self.liteinst_arch_prctl(
            task,
            ArchPrctl::new().with_cmd(ArchPrctlCmd::ARCH_SET_CPUID(state)),
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn set_and_verify_liteinst_cpuid_state(
        &mut self,
        task: Stopped,
        state: u64,
    ) -> Result<(Stopped, Vec<String>), TraceError> {
        let (task, set_result) = self.liteinst_set_cpuid_state(task, state).await?;
        let mut failures = Vec::new();
        match set_result {
            Ok(0) => {}
            Ok(result) => failures.push(format!(
                "ARCH_SET_CPUID({state}) returned unexpected value {result}"
            )),
            Err(error) => failures.push(format!("ARCH_SET_CPUID({state}): {error}")),
        }
        let (task, verify_failures) = self.verify_liteinst_cpuid_state(task, state).await?;
        failures.extend(verify_failures);
        Ok((task, failures))
    }

    #[cfg(target_arch = "x86_64")]
    async fn verify_liteinst_cpuid_state(
        &mut self,
        task: Stopped,
        state: u64,
    ) -> Result<(Stopped, Vec<String>), TraceError> {
        let mut failures = Vec::new();
        let (task, get_result) = self.liteinst_get_cpuid_state(task).await?;
        match get_result {
            Ok(observed) if observed == state as i64 => {}
            Ok(observed) => failures.push(format!(
                "ARCH_GET_CPUID returned {observed} after setting {state}"
            )),
            Err(error) => failures.push(format!("verify ARCH_GET_CPUID({state}): {error}")),
        }
        Ok((task, failures))
    }

    #[cfg(target_arch = "x86_64")]
    async fn prepare_liteinst_helper_cpuid(
        &mut self,
        task: Stopped,
    ) -> Result<(Stopped, Result<LiteinstCpuidPolicy, String>), TraceError> {
        let (task, result) = self.liteinst_get_cpuid_state(task).await?;
        match result {
            Ok(1) => Ok((task, Ok(LiteinstCpuidPolicy::UnchangedEnabled))),
            Ok(0) => {
                let (task, enable_failures) =
                    self.set_and_verify_liteinst_cpuid_state(task, 1).await?;
                if enable_failures.is_empty() {
                    Ok((task, Ok(LiteinstCpuidPolicy::RestoreDisabled)))
                } else {
                    let (task, restore_failures) =
                        self.set_and_verify_liteinst_cpuid_state(task, 0).await?;
                    let mut message = format!(
                        "enable native CPUID for patch helper: {}",
                        enable_failures.join("; ")
                    );
                    if !restore_failures.is_empty() {
                        message.push_str(&format!(
                            "; restore original CPUID policy after enable failure: {}",
                            restore_failures.join("; ")
                        ));
                    }
                    Ok((task, Err(message)))
                }
            }
            Ok(state) => Ok((
                task,
                Err(format!("ARCH_GET_CPUID returned unexpected value {state}")),
            )),
            Err(Errno::ENODEV) => Ok((task, Ok(LiteinstCpuidPolicy::Unsupported))),
            Err(error) => Ok((task, Err(format!("ARCH_GET_CPUID: {error}")))),
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn set_and_verify_liteinst_tsc_state(
        &mut self,
        task: Stopped,
        scratch_address: usize,
        state: libc::c_int,
    ) -> Result<(Stopped, Vec<String>), TraceError> {
        let (task, set_result) = self.liteinst_set_tsc_state(task, state).await?;
        let mut failures = Vec::new();
        match set_result {
            Ok(0) => {}
            Ok(result) => {
                failures.push(format!(
                    "PR_SET_TSC({state}) returned unexpected value {result}"
                ));
            }
            Err(error) => failures.push(format!("PR_SET_TSC({state}): {error}")),
        }
        let (task, verify_failures) = self
            .verify_liteinst_tsc_state(task, scratch_address, state)
            .await?;
        failures.extend(verify_failures);
        Ok((task, failures))
    }

    #[cfg(target_arch = "x86_64")]
    async fn verify_liteinst_tsc_state(
        &mut self,
        task: Stopped,
        scratch_address: usize,
        state: libc::c_int,
    ) -> Result<(Stopped, Vec<String>), TraceError> {
        let mut failures = Vec::new();
        let (task, get_result) = self.liteinst_get_tsc_state(task, scratch_address).await?;
        match get_result {
            Ok(Ok(observed)) if observed == state => {}
            Ok(Ok(observed)) => failures.push(format!(
                "PR_GET_TSC returned {observed} after setting {state}"
            )),
            Ok(Err(error)) => failures.push(format!("verify PR_GET_TSC({state}): {error}")),
            Err(error) => failures.push(format!("verify PR_GET_TSC({state}): {error}")),
        }
        Ok((task, failures))
    }

    #[cfg(target_arch = "x86_64")]
    async fn prepare_liteinst_helper_tsc(
        &mut self,
        task: Stopped,
        scratch_address: usize,
    ) -> Result<(Stopped, Result<LiteinstTscPolicy, String>), TraceError> {
        let (task, result) = self.liteinst_get_tsc_state(task, scratch_address).await?;
        match result {
            Ok(Ok(libc::PR_TSC_ENABLE)) => Ok((task, Ok(LiteinstTscPolicy::UnchangedEnabled))),
            Ok(Ok(libc::PR_TSC_SIGSEGV)) => {
                let (task, enable_failures) = self
                    .set_and_verify_liteinst_tsc_state(task, scratch_address, libc::PR_TSC_ENABLE)
                    .await?;
                if enable_failures.is_empty() {
                    Ok((task, Ok(LiteinstTscPolicy::RestoreFaulting)))
                } else {
                    let (task, restore_failures) = self
                        .set_and_verify_liteinst_tsc_state(
                            task,
                            scratch_address,
                            libc::PR_TSC_SIGSEGV,
                        )
                        .await?;
                    let mut message = format!(
                        "enable native TSC for patch helper: {}",
                        enable_failures.join("; ")
                    );
                    if !restore_failures.is_empty() {
                        message.push_str(&format!(
                            "; restore original TSC policy after enable failure: {}",
                            restore_failures.join("; ")
                        ));
                    }
                    Ok((task, Err(message)))
                }
            }
            Ok(Ok(state)) => Ok((
                task,
                Err(format!("PR_GET_TSC returned unexpected state {state}")),
            )),
            // EINVAL is the documented prctl response when this option is not
            // supported by the running kernel/architecture.
            Ok(Err(Errno::EINVAL)) => Ok((task, Ok(LiteinstTscPolicy::Unsupported))),
            Ok(Err(error)) => Ok((task, Err(format!("PR_GET_TSC: {error}")))),
            Err(error) => Ok((task, Err(error))),
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn restore_liteinst_helper_state(
        &mut self,
        task: Stopped,
        saved: &LiteinstHelperSavedState,
    ) -> Result<(Stopped, Vec<String>), TraceError> {
        let (task, mut failures) = match saved.tsc_policy {
            LiteinstTscPolicy::Unsupported => (task, Vec::new()),
            LiteinstTscPolicy::RestoreFaulting => {
                let (task, failures) = self
                    .set_and_verify_liteinst_tsc_state(
                        task,
                        saved.stack_address,
                        libc::PR_TSC_SIGSEGV,
                    )
                    .await?;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("TSC policy: {failure}"))
                        .collect(),
                )
            }
            LiteinstTscPolicy::UnchangedEnabled => {
                let (task, failures) = self
                    .verify_liteinst_tsc_state(task, saved.stack_address, libc::PR_TSC_ENABLE)
                    .await?;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("TSC policy: {failure}"))
                        .collect(),
                )
            }
        };
        let (mut task, cpuid_failures) = match saved.cpuid_policy {
            LiteinstCpuidPolicy::Unsupported => (task, Vec::new()),
            LiteinstCpuidPolicy::RestoreDisabled => {
                let (task, failures) = self.set_and_verify_liteinst_cpuid_state(task, 0).await?;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("CPUID policy: {failure}"))
                        .collect(),
                )
            }
            LiteinstCpuidPolicy::UnchangedEnabled => {
                let (task, failures) = self.verify_liteinst_cpuid_state(task, 1).await?;
                (
                    task,
                    failures
                        .into_iter()
                        .map(|failure| format!("CPUID policy: {failure}"))
                        .collect(),
                )
            }
        };
        failures.extend(cpuid_failures);
        match AddrMut::from_raw(saved.stack_address) {
            Some(address) => {
                if let Err(error) = task.write_value(address, &saved.stack_value) {
                    failures.push(format!("helper stack: {error}"));
                }
            }
            None => failures.push("helper stack: invalid restore address".to_owned()),
        }
        if let Err(error) = task.setxstate(&saved.xstate) {
            failures.push(format!("XSTATE: {error}"));
        }
        if let Err(error) = task.setregs(&saved.regs) {
            failures.push(format!("general registers: {error}"));
        }
        Ok((task, failures))
    }

    #[cfg(target_arch = "x86_64")]
    async fn rollback_liteinst_helper_error(
        &mut self,
        task: Stopped,
        saved: &LiteinstHelperSavedState,
        original: Error,
    ) -> Error {
        match self.restore_liteinst_helper_state(task, saved).await {
            Ok((_, rollback_failures)) => self.liteinst_helper_failure(original, rollback_failures),
            Err(error) => Error::runtime(
                self.tid(),
                "restore LiteInst patch-helper state",
                format!(
                    "original failure: {original}; restoration lost the physical stopped capability: {error}"
                ),
            ),
        }
    }

    fn liteinst_helper_failure(&self, original: Error, rollback_failures: Vec<String>) -> Error {
        if rollback_failures.is_empty() {
            original
        } else {
            Error::runtime(
                self.tid(),
                "restore LiteInst patch-helper state",
                format!(
                    "original failure: {original}; rollback failures: {}",
                    rollback_failures.join("; ")
                ),
            )
        }
    }

    fn record_liteinst_fallback_stats(
        &self,
        task: &Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
    ) {
        let stats = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|config| config.instrumentation_stats.as_ref());
        crate::liteinst_stats::with_liteinst_stats(stats, |stats| {
            let shape = Addr::from_raw(frame.install_result as usize)
                .and_then(|address| {
                    let result: LiteinstInstallResult = task.read_value(address).ok()?;
                    Some(result)
                })
                .and_then(|result| {
                    let instruction_len = usize::try_from(result.instruction_len).ok()?;
                    let straddle_prefix = usize::try_from(result.straddle_prefix).ok()?;
                    (result.version == 2
                        && result.complete == 0
                        && result.site_start == site
                        && result.site_len == 8
                        && (1..=15).contains(&instruction_len)
                        && straddle_prefix < instruction_len.min(5))
                    .then_some((
                        instruction_len,
                        (straddle_prefix != 0).then_some(straddle_prefix),
                    ))
                });
            let outcome = if shape.as_ref().is_some_and(|(_, prefix)| prefix.is_some()) {
                LiteinstPatchOutcome::PtraceStraddlerBail
            } else {
                LiteinstPatchOutcome::PtraceOtherFallback
            };
            let process_identity =
                u64::try_from(self.pid.as_raw()).expect("tracee PID must be positive");
            let execution_generation = {
                let mut runtime = self.liteinst_runtime.lock().unwrap();
                runtime.fallback_sites.insert(site, outcome);
                runtime.generation
            };
            stats.record_process_site(process_identity, execution_generation, site, outcome, shape);
            match outcome {
                LiteinstPatchOutcome::PtraceStraddlerBail => {
                    stats.record_cacheline_straddler_fallback();
                }
                LiteinstPatchOutcome::PtraceOtherFallback => {
                    stats.record_unpatchable_or_other_fallback();
                }
                LiteinstPatchOutcome::DirectPunPatched | LiteinstPatchOutcome::RelocatedPatched => {
                    unreachable!("fallback accounting received a patched outcome")
                }
            }
        });
    }

    fn record_retained_liteinst_fallback_hit(&self, task: &Stopped) {
        let Some(stats) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|config| config.instrumentation_stats.as_ref())
        else {
            return;
        };
        let Some(site) = task
            .getregs()
            .ok()
            .and_then(|regs| regs.ip().checked_sub(2))
        else {
            return;
        };
        let outcome = self
            .liteinst_runtime
            .lock()
            .unwrap()
            .fallback_sites
            .get(&site)
            .copied();
        crate::liteinst_stats::with_liteinst_stats(Some(stats), |stats| match outcome {
            Some(LiteinstPatchOutcome::PtraceStraddlerBail) => {
                stats.record_cacheline_straddler_fallback();
            }
            Some(LiteinstPatchOutcome::PtraceOtherFallback) => {
                stats.record_unpatchable_or_other_fallback();
            }
            Some(
                LiteinstPatchOutcome::DirectPunPatched | LiteinstPatchOutcome::RelocatedPatched,
            )
            | None => {}
        });
    }

    fn validate_liteinst_install_result(
        &self,
        task: &Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
    ) -> Option<(u64, ActiveHookFootprint)> {
        let address = Addr::from_raw(frame.install_result as usize)?;
        let result: LiteinstInstallResult = task.read_value(address).ok()?;
        let instruction_len = usize::try_from(result.instruction_len).ok()?;
        let straddle_prefix = usize::try_from(result.straddle_prefix).ok()?;
        if result.version != 2
            || result.complete != 1
            || result.site_start != site
            || result.site_len != 8
            || !(1..=15).contains(&instruction_len)
            || straddle_prefix >= instruction_len.min(5)
        {
            return None;
        }
        let site = GuestRange::new(result.site_start, result.site_len)?;
        let trampoline = GuestRange::new(result.trampoline_start, result.trampoline_len)?;
        let arena_writable =
            GuestRange::new(result.arena_writable_start, result.arena_writable_len)?;
        let arena_executable =
            GuestRange::new(result.arena_executable_start, result.arena_executable_len)?;
        if !arena_executable.contains(trampoline)
            || !trampoline.contains(GuestRange::new(result.relocated_tail, 1)?)
        {
            return None;
        }
        let maps = guest_maps(task.pid())?;
        let site_map = maps.iter().find(|mapping| {
            mapping.readable
                && !mapping.writable
                && mapping.executable
                && mapping.contains_range(site)
        })?;
        let writable_map = maps.iter().find(|mapping| {
            mapping.start == arena_writable.start
                && mapping.end == arena_writable.end
                && mapping.offset == 0
                && mapping.inode != 0
                && mapping.shared
                && mapping.readable
                && mapping.writable
                && !mapping.executable
        })?;
        let executable_map = maps.iter().find(|mapping| {
            mapping.start == arena_executable.start
                && mapping.end == arena_executable.end
                && mapping.offset == 0
                && mapping.inode != 0
                && mapping.shared
                && mapping.readable
                && !mapping.writable
                && mapping.executable
        })?;
        if writable_map.device_major != executable_map.device_major
            || writable_map.device_minor != executable_map.device_minor
            || writable_map.inode != executable_map.inode
            || writable_map.end - writable_map.start != executable_map.end - executable_map.start
            || site_map.start == writable_map.start
            || site_map.start == executable_map.start
        {
            return None;
        }
        if let Some(stats) = self
            .global_state
            .liteinst_runtime
            .as_ref()?
            .instrumentation_stats
            .as_ref()
        {
            let mut stats = stats.lock().unwrap();
            let process_identity =
                u64::try_from(self.pid.as_raw()).expect("tracee PID must be positive");
            let execution_generation = self.liteinst_runtime.lock().unwrap().generation;
            stats.record_process_site(
                process_identity,
                execution_generation,
                result.site_start,
                LiteinstPatchOutcome::RelocatedPatched,
                Some((
                    instruction_len,
                    (straddle_prefix != 0).then_some(straddle_prefix),
                )),
            );
            stats.record_ptrace_installation();
        }
        Some((
            result.relocated_tail,
            ActiveHookFootprint {
                site,
                trampoline,
                arena_writable,
                arena_executable,
            },
        ))
    }

    #[cfg(target_arch = "x86_64")]
    async fn call_liteinst_install_helper(
        &mut self,
        task: Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
    ) -> Result<(Stopped, Option<(u64, ActiveHookFootprint)>), Error> {
        let helper_return_marker = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .ok_or(Errno::EIO)?
            .helper_return_marker;
        let saved_regs = task.getregs()?;
        let saved_xstate = task.getxstate()?;
        let stack_address = frame.helper_stack_top.saturating_sub(8) as usize;
        let stack_read_address = Addr::from_raw(stack_address).ok_or(Errno::EFAULT)?;
        let stack_write_address = AddrMut::from_raw(stack_address).ok_or(Errno::EFAULT)?;
        let saved_stack: u64 = task.read_value(stack_read_address)?;
        let mut saved = LiteinstHelperSavedState {
            cpuid_policy: LiteinstCpuidPolicy::Unsupported,
            tsc_policy: LiteinstTscPolicy::Unsupported,
            regs: saved_regs,
            xstate: saved_xstate,
            stack_address,
            stack_value: saved_stack,
        };
        let (task, cpuid_policy) = self
            .prepare_liteinst_helper_cpuid(task)
            .await
            .map_err(Error::Internal)?;
        saved.cpuid_policy = match cpuid_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = Error::runtime(
                    self.tid(),
                    "prepare LiteInst patch-helper CPUID policy",
                    message,
                );
                return Err(self
                    .rollback_liteinst_helper_error(task, &saved, original)
                    .await);
            }
        };
        let (task, tsc_policy) = self
            .prepare_liteinst_helper_tsc(task, stack_address)
            .await
            .map_err(Error::Internal)?;
        saved.tsc_policy = match tsc_policy {
            Ok(policy) => policy,
            Err(message) => {
                let original = Error::runtime(
                    self.tid(),
                    "prepare LiteInst patch-helper TSC policy",
                    message,
                );
                return Err(self
                    .rollback_liteinst_helper_error(task, &saved, original)
                    .await);
            }
        };
        let mut task = task;
        if let Err(error) = task.write_value(stack_write_address, &frame.helper_return) {
            let original = Error::from(error);
            return Err(self
                .rollback_liteinst_helper_error(task, &saved, original)
                .await);
        }

        let mut helper_regs = saved.regs;
        *helper_regs.ip_mut() = frame.install_helper;
        *helper_regs.stack_ptr_mut() = frame.helper_stack_top - 8;
        helper_regs.rdi = site;
        *helper_regs.orig_syscall_mut() = -1_i64 as u64;
        helper_regs.eflags = liteinst_helper_entry_rflags(saved.regs.eflags);
        if let Err(error) = task.setregs(&helper_regs) {
            let original = Error::Internal(error);
            return Err(self
                .rollback_liteinst_helper_error(task, &saved, original)
                .await);
        }

        let running = match self.resume_stopped(task, None) {
            Ok(running) => running,
            Err(error) => {
                return Err(Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    format!(
                        "resume failed before a replacement stopped capability was observed; helper state cannot be restored safely: {error}"
                    ),
                ));
            }
        };
        let mut wait = match running.next_state().await {
            Ok(wait) => wait,
            Err(error) => {
                return Err(Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    format!(
                        "wait failed before a replacement stopped capability was observed; helper state cannot be restored safely: {error}"
                    ),
                ));
            }
        };
        self.arm_liteinst_wait(&wait);
        loop {
            match wait {
                Wait::Stopped(stopped, Event::Seccomp) => {
                    // Arena preparation is complete before Ready. The fixed
                    // post-Ready install helper has no admitted syscall. A
                    // seccomp stop here is therefore an unreviewed runtime
                    // effect, not permission to execute it behind the Tool.
                    let original = Error::runtime(
                        self.tid(),
                        "run LiteInst patch helper",
                        "the post-Ready install helper attempted an unadmitted syscall",
                    );
                    return Err(self
                        .rollback_liteinst_helper_error(stopped, &saved, original)
                        .await);
                }
                Wait::Stopped(stopped, Event::Signal(Signal::SIGTRAP)) => {
                    let regs = match stopped.getregs() {
                        Ok(regs) => regs,
                        Err(error) => {
                            let original = Error::Internal(error);
                            return Err(self
                                .rollback_liteinst_helper_error(stopped, &saved, original)
                                .await);
                        }
                    };
                    if regs.r10 != helper_return_marker || regs.ip() != frame.helper_return_rip {
                        let original = Error::runtime(
                            self.tid(),
                            "validate LiteInst patch-helper return",
                            "unexpected helper return marker or instruction pointer",
                        );
                        return Err(self
                            .rollback_liteinst_helper_error(stopped, &saved, original)
                            .await);
                    }
                    let result = regs.rax as i64;
                    let install = if u64::try_from(result).is_ok() {
                        match self.validate_liteinst_install_result(&stopped, frame, site) {
                            Some(install) => Some(install),
                            None => {
                                let original = Error::runtime(
                                    self.tid(),
                                    "validate LiteInst patch-helper result",
                                    "successful helper returned invalid active-hook metadata",
                                );
                                return Err(self
                                    .rollback_liteinst_helper_error(stopped, &saved, original)
                                    .await);
                            }
                        }
                    } else {
                        self.record_liteinst_fallback_stats(&stopped, frame, site);
                        None
                    };
                    let (stopped, rollback) = self
                        .restore_liteinst_helper_state(stopped, &saved)
                        .await
                        .map_err(Error::Internal)?;
                    if rollback.is_empty() {
                        return Ok((stopped, install));
                    }
                    let original = Error::runtime(
                        self.tid(),
                        "restore LiteInst patch-helper state",
                        "patch helper completed successfully",
                    );
                    return Err(self.liteinst_helper_failure(original, rollback));
                }
                Wait::Stopped(stopped, event) => {
                    let original = Error::runtime(
                        self.tid(),
                        "run LiteInst patch helper",
                        format!("unexpected stopped event: {event:?}"),
                    );
                    return Err(self
                        .rollback_liteinst_helper_error(stopped, &saved, original)
                        .await);
                }
                Wait::Exited(_, exit_status) => self.exit(exit_status).await,
            }
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    async fn call_liteinst_install_helper(
        &mut self,
        _task: Stopped,
        _frame: LiteinstHandshakeFrame,
        _site: u64,
    ) -> Result<(Stopped, Option<(u64, ActiveHookFootprint)>), Error> {
        Err(Error::runtime(
            self.tid(),
            "run LiteInst patch helper",
            "the dynamic LiteInst hybrid requires x86-64 XSTATE support",
        ))
    }

    async fn maybe_install_preload_liteinst_site(
        &mut self,
        task: Stopped,
        nr: Sysno,
    ) -> Result<(Stopped, bool, Option<u64>), Error> {
        if self.global_state.liteinst_runtime.is_none() {
            return Ok((task, false, None));
        }
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            return Err(Error::runtime(
                self.tid(),
                "install preload-mode LiteInst syscall site",
                "after-loader execution must stay on the raw seccomp path",
            ));
        }
        // A task-creating syscall must not be patched. Patching overwrites the
        // instruction bytes AT the site, and the new task is resumed with the
        // register context captured before the injection -- i.e. with `rip`
        // pointing just past the original two-byte `syscall`. Once the site
        // holds a longer relocating jump, that address is no longer an
        // instruction boundary and the child executes rubbish. Leaving these
        // sites unpatched costs nothing: they are entered once per task.
        if is_task_creating_syscall(nr) {
            return Ok((task, false, None));
        }
        if self
            .global_state
            .liteinst_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.multi_task.load(Ordering::Acquire))
        {
            return Ok((task, false, None));
        }
        let regs = task.getregs()?;
        let Some(site) = regs.ip().checked_sub(2) else {
            return Ok((task, false, None));
        };
        let site_address = Addr::from_raw(site as usize).ok_or(Errno::EFAULT)?;
        let instruction: u16 = task.read_value(site_address)?;
        if instruction != 0x050f {
            return Ok((task, false, None));
        }
        let frame = {
            let mut state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Ready
                || state.ready_generation != Some(state.generation)
                || !state.attempted_sites.insert(site)
            {
                return Ok((task, false, None));
            }
            state.frame.ok_or(Errno::EIO)?
        };

        if let Some(stats) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|config| config.instrumentation_stats.as_ref())
        {
            stats.lock().unwrap().record_first_site_seccomp();
        }

        // Convert the active seccomp stop into an ordinary stopped state before
        // calling arbitrary tracee code. The original event is still serviced
        // exactly once by the host Tool below.
        let task = self.skip_seccomp_syscall(task).await?;
        let (task, install) = self.call_liteinst_install_helper(task, frame, site).await?;
        let relocated_tail = install.as_ref().map(|(address, _)| *address);
        if let Some((_, footprint)) = install {
            self.liteinst_runtime
                .lock()
                .unwrap()
                .active_hooks
                .insert(site, footprint);
        }
        Ok((task, true, relocated_tail))
    }

    fn validate_liteinst_mapping_execution(
        &self,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<(), Errno> {
        let page_size = host_page_size()?;
        if self
            .liteinst_runtime
            .lock()
            .unwrap()
            .mapping_mutates_active_hook(nr, args, page_size)
        {
            Err(Errno::ENOTSUPP)
        } else {
            Ok(())
        }
    }

    fn observe_liteinst_mapping_result(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
        result: Result<i64, Errno>,
    ) {
        if self.global_state.liteinst_runtime.is_none() {
            return;
        }
        let Ok(result) = result else {
            return;
        };
        let mut state = self.liteinst_runtime.lock().unwrap();
        let Ok(page_size) = host_page_size() else {
            state.attempted_sites.clear();
            state.fallback_sites.clear();
            return;
        };
        match nr {
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::mmap => {
                if let Ok(start) = u64::try_from(result) {
                    state.invalidate_attempted_pages(start, args.arg1 as u64, page_size);
                }
            }
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::munmap | Sysno::mprotect | Sysno::pkey_mprotect => {
                state.invalidate_attempted_pages(args.arg0 as u64, args.arg1 as u64, page_size);
            }
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::mremap => {
                state.invalidate_attempted_pages(args.arg0 as u64, args.arg1 as u64, page_size);
                if let Ok(start) = u64::try_from(result) {
                    state.invalidate_attempted_pages(start, args.arg2 as u64, page_size);
                }
            }
            _ => {}
        }
    }

    async fn handle_liteinst_mapping_syscall(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<Wait, Error> {
        let tid = self.tid();
        if self.validate_liteinst_mapping_execution(nr, args).is_err() {
            return Err(Error::runtime(
                tid,
                "validate LiteInst mapping mutation",
                format!("{nr} overlaps an active LiteInst hook footprint"),
            ));
        }
        let wait = self
            .syscall_stopped(task, None)
            .tracee_context(tid, "resume controller-observed mapping syscall")?
            .next_state()
            .await
            .tracee_context(tid, "wait for controller-observed mapping syscall")?;
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(stopped, Event::Syscall) => {
                let regs = stopped
                    .getregs()
                    .tracee_context(tid, "read controller-observed mapping result")?;
                let result = Errno::from_ret(regs.ret() as usize).map(|value| value as i64);
                self.observe_liteinst_mapping_result(nr, args, result);
                self.resume_stopped(stopped, None)
                    .tracee_context(tid, "resume after controller-observed mapping syscall")?
                    .next_state()
                    .await
                    .tracee_context(tid, "wait after controller-observed mapping syscall")
            }
            Wait::Stopped(_, event) => Err(Error::runtime(
                tid,
                "observe LiteInst mapping syscall",
                format!("unexpected stopped event: {event:?}"),
            )),
            Wait::Exited(_, exit_status) => self.exit(exit_status).await,
        }
    }

    async fn handle_seccomp(&mut self, mut task: Stopped) -> Result<Wait, Error> {
        let tid = self.tid();
        let syscall = self
            .get_syscall(&task)
            .tracee_context(tid, "read registers at seccomp stop")?;
        let (nr, args) = syscall.into_parts();
        let tool_subscribed = self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == nr);
        if is_liteinst_mapping_syscall(nr) && !tool_subscribed {
            return self.handle_liteinst_mapping_syscall(task, nr, args).await;
        }
        #[cfg(target_arch = "x86_64")]
        let after_loader_raw = self.after_loader_config().is_some();
        #[cfg(not(target_arch = "x86_64"))]
        let after_loader_raw = false;
        let (syscall_already_skipped, liteinst_resume_rip) = if after_loader_raw {
            // Keep the guest's original instruction and register image intact.
            // Linux then owns ERESTART, signal-frame and rt_sigreturn semantics.
            (false, None)
        } else {
            self.record_retained_liteinst_fallback_hit(&task);
            let (installed_task, skipped, resume_rip) =
                self.maybe_install_preload_liteinst_site(task, nr).await?;
            task = installed_task;
            (skipped, resume_rip)
        };
        #[cfg(target_arch = "x86_64")]
        let is_legacy_vsyscall = !syscall_already_skipped
            && is_legacy_vsyscall_ip(
                task.getregs()
                    .tracee_context(tid, "identify legacy vsyscall stop")?
                    .ip(),
            );
        #[cfg(not(target_arch = "x86_64"))]
        let is_legacy_vsyscall = false;
        let span = tracing::trace_span!(
            target: "reverie_ptrace::syscall",
            "syscall.intercept",
            tid = %tid,
            syscall = %nr,
            args = ?SyscallArgsForLog {
                nr,
                args,
                command_bootstrap: self.command_bootstrap,
            },
        );

        async {
            tracing::trace!(
                target: "reverie_ptrace::syscall",
                "intercepting guest syscall"
            );
            self.pending_syscall = Some((nr, args));
            self.pending_syscall_already_skipped = syscall_already_skipped;

            self.begin_tool_callback(task)?;
            #[cfg(target_arch = "x86_64")]
            self.observe_after_loader_tool_callback("Tool::handle_syscall_event(seccomp)")?;
            let retval = cancellable(self.cancel_handler.clone(), async {
                self.process_state
                    .clone()
                    .handle_syscall_event(self, syscall)
                    .await
            })
            .await;
            task = self.take_tool_callback_stop()?;

            let emulate_legacy_vsyscall = is_legacy_vsyscall && self.pending_syscall.is_some();
            if emulate_legacy_vsyscall {
                // The kernel owns the synthetic `ret` from the fixed
                // vsyscall page. Leave the task at its seccomp stop and mark
                // the syscall skipped below; resuming then lets the kernel
                // return directly to the caller without single-stepping the
                // caller's first instruction.
                self.pending_syscall = None;
            } else if self.pending_syscall.is_some() && !syscall_already_skipped {
                task = self
                    .skip_seccomp_syscall(task)
                    .await
                    .tracee_context(tid, "skip intercepted syscall")?;
            }

            self.timer.finalize_requests();

            if let Some(retval) = retval {
                let ret = match retval {
                    Ok(x) => x as u64,
                    Err(err) => (-(err.into_errno()?.into_raw() as i64)) as u64,
                };

                #[cfg(target_arch = "x86_64")]
                if emulate_legacy_vsyscall {
                    let mut regs = task
                        .getregs()
                        .tracee_context(tid, "read legacy-vsyscall registers")?;
                    *regs.orig_syscall_mut() = -1i64 as u64;
                    *regs.ret_mut() = ret;
                    task.setregs(&regs)
                        .tracee_context(tid, "set legacy-vsyscall result")?;
                } else {
                    set_ret(&task, ret).tracee_context(tid, "set intercepted syscall result")?;
                }

                #[cfg(not(target_arch = "x86_64"))]
                set_ret(&task, ret).tracee_context(tid, "set intercepted syscall result")?;
            }

            self.pending_syscall_already_skipped = false;

            if let Some(resume_rip) = liteinst_resume_rip {
                let mut regs = task
                    .getregs()
                    .tracee_context(tid, "read registers before LiteInst tail resume")?;
                *regs.ip_mut() = resume_rip;
                task.setregs(&regs)
                    .tracee_context(tid, "resume after displaced LiteInst window")?;
            }

            #[cfg(test)]
            if self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Waiting
                && let Some(queue_once) = self
                    .global_state
                    .liteinst_runtime
                    .as_ref()
                    .and_then(|runtime| runtime.queue_pending_signal_once.as_ref())
                && queue_once.swap(false, Ordering::SeqCst)
            {
                self.pending_signal = Some(Signal::SIGUSR1);
            }
            let sig = self.take_pending_signal_for_resume(
                LiteinstActivationOperation::ResumeAfterSeccompStop,
            )?;
            let running = self
                .resume_stopped(task, sig)
                .tracee_context(tid, "resume after seccomp stop")?;
            let wait = running
                .next_state()
                .await
                .tracee_context(tid, "wait after seccomp resume")?;
            tracing::trace!(
                target: "reverie_ptrace::syscall",
                "completed guest syscall interception"
            );
            Ok(wait)
        }
        .instrument(span)
        .await
    }

    /// The LiteInst config, but only when this task is the session root.
    ///
    /// The root-stop lease and the fail-closed cleanup guard are owned by the
    /// single spawned root TID. `tid == pid` is NOT that predicate: a forked
    /// child is its own thread-group leader and would otherwise take the
    /// root's shared lease, whose `root_tid` check then rejects every
    /// transition with `EINVAL`.
    fn liteinst_root_runtime(&self, task: &Stopped) -> Option<&LiteinstRuntimeConfig> {
        let runtime = self.liteinst_root_config()?;
        (Some(&task.pid()) == runtime.root_tid.get()).then_some(runtime)
    }

    /// The LiteInst config, but only when *this task* is the session root.
    fn liteinst_root_config(&self) -> Option<&LiteinstRuntimeConfig> {
        let runtime = self.global_state.liteinst_runtime.as_ref()?;
        (Some(&self.tid()) == runtime.root_tid.get()).then_some(runtime)
    }

    fn liteinst_stop_slot(&self, task: &Stopped) -> Option<HeldTaskStops> {
        let runtime = self.global_state.liteinst_runtime.as_ref()?;
        (task.pid() == self.tid()).then(|| Arc::clone(&runtime.held_task_stops))
    }

    fn liteinst_stop_armer(&self, task: &Stopped) -> Option<LiteinstStopArmer> {
        let runtime = self.global_state.liteinst_runtime.as_ref()?;
        Some(LiteinstStopArmer {
            task_tid: task.pid(),
            held_task_stops: Arc::clone(&runtime.held_task_stops),
        })
    }

    pub(crate) fn arm_liteinst_stop(&self, task: &Stopped, event: &Event) {
        let Some(armer) = self.liteinst_stop_armer(task) else {
            return;
        };
        armer
            .arm(task, event)
            .expect("rearmed an undisarmed or mismatched LiteInst task stop lease");
    }

    /// Gives the cleanup guard ownership of a newborn tracee's wait statuses.
    ///
    /// This is deliberately NOT root-scoped, unlike the root-stop lease: the
    /// guard has to be able to reap the whole descendant tree, and
    /// `handle_new_task` requires the entry to exist for every child it sees.
    /// A grandchild is reported to its own non-root parent, so scoping this to
    /// the root leaves it unregistered.
    fn register_liteinst_newborn(&self, task: &Stopped, event: &Event) {
        let Some(runtime) = self.global_state.liteinst_runtime.as_ref() else {
            return;
        };
        if let Event::NewChild(op, child) = event {
            runtime
                .newborn_tracees
                .lock()
                .unwrap()
                .entry(child.pid())
                .or_insert_with(|| NewbornTracee::from_event(task.pid(), *op, child));
        }
    }

    fn arm_liteinst_wait(&self, wait: &Wait) {
        if let Wait::Stopped(task, event) = wait {
            self.register_liteinst_newborn(task, event);
            self.arm_liteinst_stop(task, event);
        }
    }

    fn ensure_liteinst_wait(&self, wait: &Wait) {
        if let Wait::Stopped(task, event) = wait {
            self.register_liteinst_newborn(task, event);
            let Some(armer) = self.liteinst_stop_armer(task) else {
                return;
            };
            armer
                .ensure(task, event)
                .expect("run-loop stop mismatched its armed task lease");
        }
    }

    fn lease_liteinst_stop(&self, task: Stopped) -> RootStopLease {
        let slot = self.liteinst_stop_slot(&task);
        RootStopLease::new(task, slot)
    }

    fn resume_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<Running, TraceError> {
        #[cfg(target_arch = "x86_64")]
        if self.liteinst_after_loader_forward_inflight.is_some() {
            return self.lease_liteinst_stop(task).syscall(signal);
        }
        self.lease_liteinst_stop(task).resume(signal)
    }

    fn step_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.lease_liteinst_stop(task).step(signal)
    }

    fn syscall_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<Running, TraceError> {
        self.lease_liteinst_stop(task).syscall(signal)
    }

    async fn dispatch_new_task(
        &mut self,
        op: ChildOp,
        parent: Stopped,
        child: Running,
        context: Option<libc::user_regs_struct>,
        child_context: Option<libc::user_regs_struct>,
    ) -> Result<Wait, TraceError> {
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            // The event consumer registered this exact newborn before reaching
            // dispatch. Preserve it for the existing whole-session cleanup.
            self.record_liteinst_failure(
                LiteinstActivationFailureReason::AfterLoaderCall,
                Error::runtime(
                    self.tid(),
                    "LiteInst after-loader call",
                    format!("unexpected {op:?} child {} in fixed fixture", child.pid()),
                ),
            );
            return Err(Errno::ENOTSUPP.into());
        }
        #[cfg(test)]
        if let Some(sender) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|runtime| runtime.pause_before_new_task.as_ref())
        {
            let _ = sender.send(child.pid());
            future::pending::<()>().await;
        }
        self.handle_new_task(op, parent, child, context, child_context)
            .await
    }

    async fn handle_new_task(
        &mut self,
        op: ChildOp,
        parent: Stopped,
        child: Running,
        context: Option<libc::user_regs_struct>,
        child_context: Option<libc::user_regs_struct>,
    ) -> Result<Wait, TraceError> {
        if let Some(runtime) = self.global_state.liteinst_runtime.clone() {
            runtime.multi_task.store(true, Ordering::Release);
            let newborn_tracees = Arc::clone(&runtime.newborn_tracees);
            let child_pid = child.pid();
            let registration_error = {
                let newborns = newborn_tracees.lock().unwrap();
                let Some(newborn) = newborns.get(&child_pid) else {
                    drop(newborns);
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::NewbornRegistration,
                        Error::runtime(
                            self.tid(),
                            "register LiteInst newborn tracee",
                            format!("newborn {child_pid} event ownership is absent"),
                        ),
                    );
                    return Err(Errno::ESRCH.into());
                };
                newborn.registration_error()
            };
            if let Some(error) = registration_error {
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::NewbornRegistration,
                    Error::runtime(
                        self.tid(),
                        "register LiteInst newborn tracee",
                        format!("newborn {child_pid} registration failed: {error}"),
                    ),
                );
                return Err(error.into());
            }
            let child_identity =
                match TraceeIdentity::capture_event_child(child_pid, parent.pid(), op) {
                    Ok(identity) => identity,
                    Err(error) => {
                        self.record_liteinst_failure(
                            LiteinstActivationFailureReason::NewbornIdentity,
                            Error::runtime(
                                self.tid(),
                                "capture LiteInst newborn identity",
                                format!("newborn {child_pid} identity capture failed: {error}"),
                            ),
                        );
                        return Err(error.into());
                    }
                };
            if op == ChildOp::Vfork {
                // A vfork child borrows the parent's memory and suspends it
                // until the child execs or exits. Bind and terminate this exact
                // child generation before returning the refusal; otherwise the
                // parent remains kernel-frozen and orderly task cleanup cannot
                // reach the session-level guard.
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::VforkUnsupported,
                    Error::runtime(
                        self.tid(),
                        "refuse vfork under the LiteInst hybrid",
                        format!(
                            "vfork child of {} refused: exec cannot preserve the preload runtime",
                            parent.pid()
                        ),
                    ),
                );
            }
            {
                let mut newborns = newborn_tracees.lock().unwrap();
                let Some(newborn) = newborns.get_mut(&child_pid) else {
                    drop(newborns);
                    self.record_liteinst_failure(
                        LiteinstActivationFailureReason::NewbornRegistration,
                        Error::runtime(
                            self.tid(),
                            "store LiteInst newborn identity",
                            format!(
                                "newborn {child_pid} event ownership disappeared before identity storage"
                            ),
                        ),
                    );
                    return Err(Errno::ESRCH.into());
                };
                newborn.set_identity(child_identity);
            }
            #[cfg(test)]
            if let Some(sender) = self
                .global_state
                .liteinst_runtime
                .as_ref()
                .and_then(|runtime| runtime.pause_new_task.as_ref())
            {
                let _ = sender.send(child.pid());
                if self
                    .global_state
                    .liteinst_runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.pause_after_new_task)
                {
                    future::pending::<()>().await;
                }
            }
            #[cfg(test)]
            if runtime.fail_new_task {
                return Err(Errno::ENOTSUPP.into());
            }
            if op == ChildOp::Vfork {
                let termination = newborn_tracees
                    .lock()
                    .unwrap()
                    .get(&child_pid)
                    .ok_or(Errno::ESRCH)?
                    .terminate_vfork_child();
                termination?;
                return Err(Errno::ENOTSUPP.into());
            }
            // Any other new task proceeds under the ordinary ptrace lifecycle.
            // Root cleanup still owns every process child and CLONE_THREAD TID
            // if a later LiteInst failure does fail closed: it signals only
            // group-leader pidfds and drains every bound notifier generation on
            // the ptracer thread.
        }
        tracing::debug!(
            "[scheduler] handling fork from parent {} to child {}: {:?}",
            parent.pid(),
            child.pid(),
            op
        );

        let mut child_task = match op {
            ChildOp::Clone => self.cloned(child.pid()),
            ChildOp::Fork => self.forked(child.pid()),
            ChildOp::Vfork => self.forked(child.pid()),
        };

        let (child_stop_tx, child_stop_rx) = mpsc::channel(1);
        child_task.gdb_stop_tx = Some(child_stop_tx);

        let daemonizer_rx = child_task.daemonizer_rx.take();
        let child_resume_tx = child_task.gdb_resume_tx.clone();
        let child_request_tx = child_task.gdb_request_tx.clone();
        let suspended = child_task.suspended.clone();

        // TODO-HUMAN-REVIEW(PR-103): Review rewritten clone parent/child restoration.
        if let Some(context) = context {
            restore_context(
                &parent,
                context,
                Some(child.pid().as_raw() as u64),
                child_context.is_some(),
            )?;
        }
        let child_restore_context = child_context.or(context);

        let id = child.pid();
        // Under the LiteInst runtime the cleanup guard registers every newborn
        // with the notifier the moment its parent reports `Event::NewChild`, so
        // the "notifier is not yet aware of this PID" precondition for the raw
        // `wait` below no longer holds: the notifier worker would consume the
        // initial stop and the raw `wait` would block forever. Take the initial
        // stop from the notifier instead, which is the same state by a
        // registered route.
        let notifier_owns_initial_stop = self.global_state.liteinst_runtime.is_some();

        // A panic anywhere in this body would otherwise be caught by tokio's
        // task harness and silently wedge the whole run; see
        // `guest_task_panic_is_fatal`. The body is built as its own future so
        // the catch sits at the task boundary and covers all of it.
        let panic_tid = id;
        let body = async move {
            // The child could potentially exit here. In most cases the first
            // event we get here should be `Event::Signal(Signal::SIGSTOP)`, but
            // we can also receive `Event::Exit` if a thread is created via
            // `clone`, but immediately killed via an `exit_group`. We have to
            // handle that rare case here.
            //
            // NOTE: It is okay to call `wait` instead of the async `next_state`
            // here because the notifier is not yet aware of the new process.
            let initial_stop = if notifier_owns_initial_stop {
                child.next_state().await
            } else {
                child.wait()
            };
            let (child, event) = match initial_stop {
                Ok(wait) => wait.assume_stopped(),
                Err(TraceError::Died(zombie)) => {
                    let exit_status = match zombie.reap().await {
                        Ok(exit_status) => exit_status,
                        Err(error) => {
                            tracing::error!(
                                target: "reverie_ptrace::lifecycle",
                                tid = %id,
                                %error,
                                "failed to reap new tracee after its initial-stop race"
                            );
                            return ExitStatus::Exited(1);
                        }
                    };
                    tracing::error!(
                        target: "reverie_ptrace::lifecycle",
                        tid = %id,
                        ?exit_status,
                        "new tracee exited before its initial stop"
                    );
                    return exit_status;
                }
                Err(TraceError::Errno(errno)) => {
                    tracing::error!(
                        target: "reverie_ptrace::lifecycle",
                        tid = %id,
                        %errno,
                        "failed waiting for new tracee initial stop"
                    );
                    return ExitStatus::Exited(1);
                }
            };

            child_task.arm_liteinst_stop(&child, &event);
            assert!(
                event == Event::Signal(Signal::SIGSTOP) || event == Event::Exit,
                "Got unexpected event {:?}",
                event
            );

            if let Some(context) = child_restore_context {
                // Restore context, but only if the child hasn't arrived at
                // `Event::Exit`.
                if event == Event::Signal(Signal::SIGSTOP)
                    && let Err(err) = restore_context(&child, context, None, false)
                {
                    tracing::error!(
                        tid = %child.pid(),
                        error = %err,
                        "failed to restore new tracee register context"
                    );
                    return ExitStatus::Exited(1);
                }
            }

            if child_task.is_a_daemon {
                child_task.ndaemons.fetch_add(1, Ordering::SeqCst);
            }

            let tid = child.pid();
            let detach_held_task_stops = child_task
                .global_state
                .liteinst_runtime
                .as_ref()
                .map(|runtime| Arc::clone(&runtime.held_task_stops));
            let liteinst_fail_closed = child_task.global_state.liteinst_runtime.is_some();
            match child_task.run(child).await {
                Err(err) => {
                    tracing::error!("Error in tracee tid {}: {}", tid, err);

                    if liteinst_fail_closed {
                        // Every LiteInst failure returns to the session cleanup
                        // guard. It owns the exact pidfds and notifier
                        // generations for this tree; reconstructing a Stopped
                        // capability here would lose the physical status that
                        // authorized the transition, and detaching would let a
                        // guest parent consume the child before cleanup sees
                        // its terminal status.
                        return ExitStatus::Exited(1);
                    }

                    // We assume the tracee is stopped since this error likely
                    // originated from the tool itself when the tracee is
                    // already stopped. If the tracee is not in a stopped state,
                    // that's fine too and ignore the detach error.
                    let detach_span = tracing::debug_span!(
                        target: "reverie_ptrace::lifecycle",
                        "tracee.detach",
                        %tid,
                        reason = "handler error"
                    );
                    let detach_guard = detach_span.enter();
                    let running = match RootStopLease::new(
                        Stopped::new_unchecked(tid),
                        detach_held_task_stops,
                    )
                    .detach(None)
                    {
                        Err(err) => {
                            // If we get an error here, the child process may
                            // not be in a ptrace stop.
                            tracing::error!("Failed to detach from {}: {}", tid, err);
                            return ExitStatus::Exited(1);
                        }
                        Ok(running) => running,
                    };
                    drop(detach_guard);

                    match running.next_state().await {
                        Ok(wait) => wait.assume_exited().1,
                        Err(TraceError::Died(zombie)) => match zombie.reap().await {
                            Ok(exit_status) => exit_status,
                            Err(error) => {
                                tracing::error!(
                                    %tid,
                                    %error,
                                    "failed to reap detached tracee"
                                );
                                ExitStatus::Exited(1)
                            }
                        },
                        Err(TraceError::Errno(errno)) => {
                            tracing::error!(
                                %tid,
                                %errno,
                                "failed waiting for detached tracee exit"
                            );
                            ExitStatus::Exited(1)
                        }
                    }
                }
                Ok(exit_status) => exit_status,
            }
        };
        let task = tokio::task::spawn_local(async move {
            match AssertUnwindSafe(body).catch_unwind().await {
                Ok(exit_status) => exit_status,
                Err(payload) => guest_task_panic_is_fatal(panic_tid, payload),
            }
        });

        if op == ChildOp::Clone {
            let mut child_threads = self.child_threads.lock().await;
            child_threads.push(Child {
                id,
                suspended,
                wait_all_stop_tx: None,
                daemonizer_rx,
                handle: task,
            });
        } else {
            let mut child_procs = self.child_procs.lock().await;
            child_procs.push(Child {
                id,
                suspended,
                wait_all_stop_tx: None,
                daemonizer_rx,
                handle: task,
            });
        }

        let parent_regs = parent.getregs()?;
        if self.attached_by_gdb {
            // NB: We report T05;create event (for clone). However gdbserver
            // from binutils-gdb doesn't report it, even after toggling
            // QThreadEvents, as mentioned in https://sourceware.org/gdb/onlinedocs/gdb/General-Query-Packets.html#QThreadEvents
            // We report `create` event anyway.
            self.notify_gdb_stop(StopReason::new_task(
                self.tid(),
                self.pid(),
                id,
                parent_regs.into(),
                op,
                child_request_tx,
                child_resume_tx,
                Some(child_stop_rx),
            ))
            .await?;
            // We just reported a new event, wait for gdb resume.
            let running = self
                .await_gdb_resume(parent, ExpectedGdbResume::StepOnly)
                .await?;
            // NB: We could potentially hit a breakpoint after above resume,
            // make sure we don't miss the breakpoint and await for gdb
            // resume (once again). This is possible because result of
            // handle_new_task in status_to_result is ignored, while it could be
            // a valid state like SIGTRAP, which could be a breakpoint is hit.
            running
                .next_state()
                .and_then(|wait| self.check_swbreak(wait))
                .await
        } else {
            // This nested parent step consumes the root-stop lease, so the
            // resulting stop has to re-arm it before returning to a caller
            // that will transition the root again. Every other nested handler
            // does the same; this one only looks new because the whole
            // new-task path used to be unreachable under LiteInst.
            let wait = self.step_stopped(parent, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            Ok(wait)
        }
    }

    async fn handle_vfork_done_event(&mut self, stopped: Stopped) -> Result<Wait, TraceError> {
        self.resume_stopped(stopped, None)?.next_state().await
    }

    async fn wait_after_exit_event(
        task: Stopped,
        held_task_stops: Option<HeldTaskStops>,
    ) -> Result<Wait, TraceError> {
        // de_thread can replace the leader after its PTRACE_EVENT_EXIT, so
        // this wait may report Exec with the caller's former TID, not death.
        if let Some(slot) = held_task_stops.as_ref() {
            HeldRootStop::supersede_with_exit(slot, &task)?;
        }
        RootStopLease::new(task, held_task_stops)
            .resume(None)?
            .next_state()
            .await
    }

    #[cfg(test)]
    pub(crate) async fn handle_exit_event(
        task: Stopped,
        held_task_stops: Option<HeldTaskStops>,
    ) -> Result<ExitStatus, TraceError> {
        let wait = Self::wait_after_exit_event(task, held_task_stops).await?;
        let (_pid, exit_status) = wait.assume_exited();
        Ok(exit_status)
    }

    /// Aborts the current handler. This just sends a result through a channel to
    /// the `run_loop`, which should cause the current future to be dropped and
    /// canceled. Thus, this function will never return so that execution of the
    /// current future doesn't proceed any further.
    async fn abort(&mut self, result: Result<Wait, TraceError>) -> ! {
        if self.next_state.send(result).await.is_err() {
            panic!(
                "failed to abort tracee {}: run-loop next-state channel is closed",
                self.tid()
            );
        }

        // Wait on a future that will never complete. This pending future will
        // be dropped when the channel receives the event just sent.
        future::pending().await
    }

    /// Marks the current task as exited via a channel. The receiver end of the
    /// channel should cause the current future to be dropped and canceled. Thus,
    /// this function will never return so that execution doesn't proceed any
    /// further.
    async fn exit(&mut self, exit_status: ExitStatus) -> ! {
        self.abort(Ok(Wait::Exited(self.tid(), exit_status))).await
    }

    /// Marks the current task as having successfully called `execve` and so it
    /// should never return.
    async fn execve(&mut self, next_state: Wait) -> ! {
        self.abort(Ok(next_state)).await
    }

    /// Triggers the tool exit callbacks.
    async fn tool_exit(self, exit_status: ExitStatus) -> Result<(), reverie::Error> {
        #[cfg(target_arch = "x86_64")]
        let after_loader_exit_callbacks = self.after_loader_tool_callback_context();
        if self.is_main_thread() {
            // Wait for all child threads to fully exit. This *must* happen before
            // the main thread can exit.
            // TODO: Use FuturesUnordered instead of `join_all` for better
            // performance.
            {
                let children = self.child_threads.lock().await.take_inner();
                future::join_all(children).await;
            }

            // Check if there are any children who's futures are still pending. If
            // this is the case, then they shall be considered "orphans" and are
            // "adopted" by the tracer process who shall then wait for them to exit
            // and get their final exit code. Normally, when not running under
            // ptrace, orphans are adopted by the init process who should
            // automatically reap them by waiting for the final exit status.
            let orphans = if self.global_state.liteinst_runtime.is_some() {
                // A LiteInst session follows process children as part of one
                // fail-closed instrumentation domain. Do not let root exit end
                // the LocalSet while a child can still publish a session
                // failure: join those exact followed tasks first.
                let children = self.child_procs.lock().await.take_inner();
                future::join_all(children).await;
                Children::new()
            } else {
                let (orphans, _) = {
                    let mut child_procs = self.child_procs.lock().await;
                    child_procs.deref_mut().await
                };
                orphans
            };

            for orphan in orphans.into_inner() {
                // Bon voyage.
                if let Err(err) = self.orphanage.send(orphan).await {
                    let orphan = err.0;
                    tracing::warn!(
                        pid = %orphan.id(),
                        "orphan reaper closed; waiting for child inline"
                    );
                    let _ = orphan.await;
                }
            }

            let _ = self
                .notify_gdb_stop(StopReason::Exited(self.pid(), exit_status))
                .await;

            let wrapped = WrappedFrom(self.tid, &self.global_state);

            // Thread exit
            #[cfg(target_arch = "x86_64")]
            if let Some(context) = &after_loader_exit_callbacks {
                context
                    .record("Tool::on_exit_thread")
                    .map_err(|error| reverie::Error::from(anyhow::Error::new(error)))?;
            }
            self.process_state
                .on_exit_thread(self.tid, &wrapped, self.thread_state, exit_status)
                .await?;

            // The try_unwrap and subsequent unwrap are safe to do. ptrace
            // guarantees that all threads in the thread group have exited
            // before the main thread.
            let process_state = Arc::try_unwrap(self.process_state).unwrap_or_else(|_| {
                // If you end up seeing this panic, make sure that all clones of
                // `process_state` are dropped before reaching this point.
                panic!("Reverie internal invariant broken. try_unwrap on process state failed")
            });
            let wrapped = WrappedFrom(self.tid, &self.global_state);
            #[cfg(target_arch = "x86_64")]
            if let Some(context) = &after_loader_exit_callbacks {
                context
                    .record("Tool::on_exit_process")
                    .map_err(|error| reverie::Error::from(anyhow::Error::new(error)))?;
            }
            process_state
                .on_exit_process(self.tid, &wrapped, exit_status)
                .await?;

            let ntasks_remaining = self.ntasks.fetch_sub(1, Ordering::SeqCst);
            let ndaemons = self.ndaemons.load(Ordering::SeqCst);

            if self.is_a_daemon {
                self.ndaemons.fetch_sub(1, Ordering::SeqCst);
            }

            if ntasks_remaining == 1 + ndaemons {
                // daemonize() might not get called, this is not an error.
                let _ = self.daemon_kill_switch.send(());
            }
        } else {
            let _ = self
                .notify_gdb_stop(StopReason::ThreadExited(
                    self.tid(),
                    self.pid(),
                    exit_status,
                ))
                .await;
            let wrapped = WrappedFrom(self.tid, &self.global_state);

            self.child_threads
                .lock()
                .await
                .retain(|child| child.id() != self.tid);

            // Thread exit
            #[cfg(target_arch = "x86_64")]
            if let Some(context) = &after_loader_exit_callbacks {
                context
                    .record("Tool::on_exit_thread")
                    .map_err(|error| reverie::Error::from(anyhow::Error::new(error)))?;
            }
            self.process_state
                .on_exit_thread(self.tid, &wrapped, self.thread_state, exit_status)
                .await?;

            self.ntasks.fetch_sub(1, Ordering::SeqCst);
            if self.is_a_daemon {
                self.ndaemons.fetch_sub(1, Ordering::SeqCst);
            }
        }

        Ok(())
    }

    async fn run_loop(&mut self, task: Stopped) -> Result<ExitStatus, reverie::Error> {
        match self.run_loop_internal(task).await {
            Ok(exit_status) => Ok(exit_status),
            Err(err) => {
                if let Some(runtime) = self.global_state.liteinst_runtime.as_ref() {
                    let mut failure = runtime.session_failure.lock().unwrap();
                    if failure.is_none() {
                        *failure = Some(format!(
                            "tracee {} failed while owning its exact notifier generation: {err}",
                            self.tid()
                        ));
                        drop(failure);
                        runtime.session_failure_changed.notify_waiters();
                    }
                    // Return immediately to the outer LiteInst cleanup guard.
                    // It owns the original root pidfd and every generation-
                    // bound notifier handle; this task must not reopen or
                    // numerically signal the root PID.
                    return Err(anyhow::Error::new(err).into());
                }
                // Note: Calling handle_internal_error cannot happen in the
                // `select!()` of the `run` function because then the exit
                // events that get generated in here cannot be caught by the
                // `select!()`.
                handle_internal_error(err).await
            }
        }
    }

    async fn run_loop_internal(&mut self, mut task: Stopped) -> Result<ExitStatus, Error> {
        // This is the beginning of the life of the guest. Allow the tool to
        // inject syscalls as soon as the thread starts.
        self.begin_tool_callback(task)
            .tracee_context(self.tid(), "bind thread-start physical stop")?;
        #[cfg(target_arch = "x86_64")]
        self.observe_after_loader_tool_callback("Tool::handle_thread_start")?;
        let callback = cancellable(self.cancel_handler.clone(), async {
            self.process_state.clone().handle_thread_start(self).await
        })
        .await;
        task = self
            .take_tool_callback_stop()
            .tracee_context(self.tid(), "recover thread-start physical stop")?;
        if let Some(Err(err)) = callback {
            // Propagate user errors. Don't care about the result of syscall injections.
            err.into_errno()?;
        }
        self.timer.finalize_requests();

        // Resume the guest for the first time. Note that the root task and
        // child tasks start out in a stopped state for different reasons: The
        // root task is stopped because of the SIGSTOP raised inside of `fork()`
        // after calling `traceme`. Child tasks start out in a running state,
        // but we wait for them to stop in `Event::NewChild`.
        //
        // NB: await_gdb_resume == resume if not attached_by_gdb.
        let running = self
            .await_gdb_resume(task, ExpectedGdbResume::Resume)
            .await
            .tracee_context(self.tid(), "initial tracee resume")?;

        // Notify gdb server (if any) that tracee is ready.
        if let Some(server_tx) = self.gdbserver_start_tx.take() {
            self.attached_by_gdb = true;
            if server_tx.send(()).is_err() {
                tracing::warn!(tid = %self.tid(), "GDB server closed before tracee attach");
                self.attached_by_gdb = false;
            }
        }

        let mut task_state = running
            .next_state()
            .await
            .tracee_context(self.tid(), "wait after initial tracee resume")?;
        let mut next_state_rx = self.next_state_rx.take().ok_or_else(|| {
            Error::runtime(
                self.tid(),
                "initialize run loop",
                "next-state receiver was already taken",
            )
        })?;

        loop {
            // Bind every decoded stop before statistics, Tool dispatch, or any
            // other common handler can inspect it.
            self.ensure_liteinst_wait(&task_state);
            if let Some(stats) = &self.global_state.backend_stats {
                stats.record_wait(&task_state);
            }
            // A nested handler may forward a stop it already armed before
            // inspecting the status. Accept only that exact generation/status;
            // every ordinary returned transition still requires an empty slot.
            match task_state {
                Wait::Stopped(stopped, event) => {
                    // Allow short-circuiting of the event stream. This makes it
                    // easier to send exit and execve events directly to the run
                    // loop from within `inject` or `tail_inject`.
                    let tid = self.tid();
                    let fut1 = next_state_rx.recv().fuse();
                    let fut2 = self.handle_stop_event(stopped, event).fuse();

                    futures::pin_mut!(fut1, fut2);

                    task_state = futures::select_biased! {
                        next_state = fut1 => {
                            if let Some(next_state) = next_state {
                                next_state.map_err(Error::Internal)
                            } else {
                                Err(Error::runtime(
                                    tid,
                                    "receive injected tracee state",
                                    "next-state channel closed unexpectedly",
                                ))
                            }
                        }
                        next_state = fut2 => next_state,
                    }?;
                }
                Wait::Exited(pid, exit_status) => {
                    #[cfg(target_arch = "x86_64")]
                    self.observe_after_loader_terminal_event(pid, exit_status)?;
                    self.notify_gdb_stop(StopReason::Exited(pid, exit_status))
                        .await?;
                    break Ok(exit_status);
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn observe_after_loader_exit_stop(
        &self,
        generation: safeptrace::PhysicalEventGenerationId,
        status: Option<safeptrace::PhysicalStatusId>,
    ) -> Result<(), Error> {
        let Some(config) = self.after_loader_config() else {
            return Ok(());
        };
        let mut runtime = self.liteinst_runtime.lock().unwrap();
        if runtime.phase != LiteinstRuntimePhase::Ready
            || runtime.after_loader_guest_observed
            || runtime.after_loader_reference.is_none()
        {
            return Ok(());
        }
        config
            .diagnostics
            .record(
                "first ordinary guest event after restoration",
                self.timer.diagnostic_clock(),
                format!(
                    "tid={} generation={} physical_generation={generation:?} physical_status={status:?} event=Exit registers=unavailable",
                    self.tid(),
                    runtime.generation,
                ),
            )
            .map_err(|error| {
                Error::runtime(
                    self.tid(),
                    "record first ordinary guest event after restoration",
                    error.to_string(),
                )
            })?;
        runtime.after_loader_guest_observed = true;
        Ok(())
    }

    /// Drive a single guest thread to completion. Returns the final exit code
    /// when that guest thread exits.
    pub async fn run(mut self, child: Stopped) -> Result<ExitStatus, reverie::Error> {
        let exit_held_task_stops = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .map(|runtime| Arc::clone(&runtime.held_task_stops));
        let root_session_failure = self.liteinst_root_config().map(|runtime| {
            (
                Arc::clone(&runtime.session_failure),
                Arc::clone(&runtime.session_failure_changed),
            )
        });
        let completion = {
            let exit_event = child.exit_event().fuse();
            let run_loop = self.run_loop(child).fuse();
            let session_failure = async move {
                let Some((failure, changed)) = root_session_failure else {
                    return future::pending::<String>().await;
                };
                loop {
                    let notified = changed.notified();
                    if let Some(message) = failure.lock().unwrap().clone() {
                        return message;
                    }
                    notified.await;
                }
            }
            .fuse();
            futures::pin_mut!(exit_event, run_loop, session_failure);

            futures::select_biased! {
                task = exit_event => match task {
                    Ok(task) => {
                        let observation = (
                            task.physical_event_generation(),
                            task.physical_status_id(),
                        );
                        Either::Left((
                            Some(observation),
                            Self::wait_after_exit_event(task, exit_held_task_stops).await,
                        ))
                    }
                    Err(err) => Either::Left((None, Err(err))),
                },
                message = session_failure => Either::Right(Err(anyhow::anyhow!(
                    "LiteInst session failed closed in a non-root task: {message}"
                ).into())),
                exit_status = run_loop => Either::Right(exit_status),
            }
        };
        #[cfg(target_arch = "x86_64")]
        if let Either::Left((observation, result)) = &completion {
            if let Some((generation, status)) = observation {
                self.observe_after_loader_exit_stop(*generation, *status)
                    .map_err(|error| reverie::Error::from(anyhow::Error::new(error)))?;
            }
            if let Ok(Wait::Stopped(stopped, event)) = result {
                self.observe_after_loader_stopped_event(stopped, event)
                    .map_err(|source| {
                        reverie::Error::from(anyhow::Error::new(Error::Tracee {
                            operation: "observe first ordinary guest stop after restoration",
                            pid: stopped.pid(),
                            source,
                        }))
                    })?;
            }
        }
        // Drop the old run-loop future before mutating its owner. The stopped
        // event carries the existing notifier generation; re-arm that exact
        // stop before publishing failure so session cleanup can consume it.
        let outcome = match completion {
            Either::Left((_, Ok(wait @ Wait::Stopped(_, Event::Exec(former_tid)))))
                if self.global_state.liteinst_runtime.is_some() && former_tid != self.tid() =>
            {
                self.arm_liteinst_wait(&wait);
                let (stopped, _) = wait.assume_stopped();
                // SAFETY: this branch owns the continuation of the single
                // ExitFuture-minted Stopped. wait_after_exit_event consumed
                // that capability through resume, and the old run-loop future
                // was dropped above. Retire its claimed exit permission before
                // handing the actual Exec stop to cancellation cleanup.
                match unsafe { stopped.terminal_cleanup().revoke_owned_exit_stop() } {
                    Ok(()) => {
                        let error = self.reject_liteinst_nonleader_exec(former_tid);
                        handle_internal_error(error.into()).await
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Either::Left((_, Ok(wait))) => {
                let (pid, exit_status) = wait.assume_exited();
                #[cfg(target_arch = "x86_64")]
                self.observe_after_loader_terminal_event(pid, exit_status)
                    .map_err(|error| reverie::Error::from(anyhow::Error::new(error)))?;
                Ok(exit_status)
            }
            Either::Left((_, Err(error))) => handle_internal_error(error.into()).await,
            Either::Right(outcome) => outcome,
        };
        if outcome.is_ok() && self.global_state.liteinst_runtime.is_some() {
            let phase = self.liteinst_runtime.lock().unwrap().phase;
            if phase != LiteinstRuntimePhase::Ready {
                self.record_liteinst_failure(
                    LiteinstActivationFailureReason::TerminatedBeforeHandshake,
                    Error::runtime(
                        self.tid(),
                        "verify LiteInst runtime activation",
                        format!(
                            "tracee terminated before the required preload handshake completed (phase {phase:?})"
                        ),
                    ),
                );
            }
        }
        let local_failure_reason = self
            .liteinst_failure
            .as_ref()
            .map(LiteinstActivationFailure::reason);
        let (exit_status, failure) = match (outcome, self.liteinst_failure.take()) {
            (_, Some(original)) => (
                None,
                Some(reverie::Error::from(anyhow::Error::new(original))),
            ),
            (Ok(exit_status), None) => (Some(exit_status), None),
            (Err(error), _) => (None, Some(error)),
        };
        if let Some(failure) = failure {
            let vfork_failure =
                local_failure_reason == Some(LiteinstActivationFailureReason::VforkUnsupported);
            if self.global_state.liteinst_runtime.is_some()
                && self.liteinst_root_config().is_none()
                && !vfork_failure
            {
                let tid = self.tid();
                if let Err(error) = self.tool_exit(ExitStatus::Exited(1)).await {
                    tracing::warn!(
                        %tid,
                        %error,
                        "tool exit hook failed while releasing a failed LiteInst task"
                    );
                }
            }
            // A vfork parent returns directly to the session-level cleanup
            // guard because orderly per-task exit cannot advance while the
            // kernel has it frozen behind that child. Other non-root failures
            // complete the existing tool-exit bookkeeping. The root failure
            // notification allows cleanup to proceed independently if that
            // bookkeeping blocks on a tracee which has not exited yet.
            return Err(failure);
        }
        let exit_status = exit_status.expect("a task without a failure has an exit status");
        let root_session_failure = self.liteinst_root_config().and_then(|_| {
            self.global_state.liteinst_runtime.as_ref().map(|runtime| {
                (
                    Arc::clone(&runtime.session_failure),
                    Arc::clone(&runtime.session_failure_changed),
                )
            })
        });

        // A fail-closed refusal raised by a non-root task cannot reach the
        // root's cleanup guard, and that task's tracee was released so the rest
        // of the guest could finish. Refuse to report success over it.
        if let Some(message) = root_session_failure
            .as_ref()
            .and_then(|(slot, _)| slot.lock().unwrap().clone())
        {
            return Err(anyhow::anyhow!(
                "LiteInst session failed closed in a non-root task: {message}"
            )
            .into());
        }

        if let Some(stats) = &self.global_state.backend_stats {
            stats.record_tracee_exit();
        }
        log_guest_exit(self.tid(), self.pid(), exit_status);

        let tool_exit = self.tool_exit(exit_status).fuse();
        if let Some((failure, changed)) = root_session_failure.as_ref() {
            let session_failure = async {
                loop {
                    let notified = changed.notified();
                    if let Some(message) = failure.lock().unwrap().clone() {
                        return message;
                    }
                    notified.await;
                }
            }
            .fuse();
            futures::pin_mut!(tool_exit, session_failure);
            futures::select_biased! {
                message = session_failure => return Err(anyhow::anyhow!(
                    "LiteInst session failed closed in a non-root task: {message}"
                ).into()),
                result = tool_exit => result?,
            }
        } else {
            tool_exit.await?;
        }

        // A child can fail while the root is joining it in `tool_exit`, after
        // the fast-path check above.  The join is the final ordering boundary:
        // re-read the shared slot before allowing the root's success to escape.
        if let Some(message) = root_session_failure
            .as_ref()
            .and_then(|(slot, _)| slot.lock().unwrap().clone())
        {
            return Err(anyhow::anyhow!(
                "LiteInst session failed closed in a non-root task: {message}"
            )
            .into());
        }

        Ok(exit_status)
    }

    /// Skip the syscall which is about to happen in the tracee, switching the tracee
    /// from Seccomp() state to Stopped(SIGTRAP) state.
    ///
    /// This uses the convention that setting the syscall number to -1 causes the
    /// kernel to skip it. This function takes as argument the current register state
    /// and restores it after stepping over the skipped syscall instruction.
    ///
    /// Preconditions:
    ///  Ptrace tracee is in a (seccomp) stopped state.
    ///  The tracee was stopped with the RIP pointing just after a syscall instruction (+2).
    ///
    /// Postconditions:
    ///  Set tracee state to Stopped/SIGTRP.
    ///  Restore the registers to the state specified by the regs arg.
    async fn skip_seccomp_syscall(&mut self, task: Stopped) -> Result<Stopped, TraceError> {
        // So here we are, at ptrace seccomp stop, if we simply resume, the kernel
        // would do the syscall, without our patch. we change to syscall number to
        // -1, so that kernel would simply skip the syscall, so that we can jump to
        // our patched syscall on the first run. Please note after calling this
        // function, the task state will no longer be in ptrace event seccomp.
        let regs = task.getregs()?;
        let pre_rip = regs.ip();

        #[cfg(target_arch = "x86_64")]
        {
            let mut new_regs = regs;
            *new_regs.orig_syscall_mut() = -1i64 as u64;
            task.setregs(&new_regs)?;
        }

        #[cfg(target_arch = "aarch64")]
        task.set_syscall(-1)?;

        let mut running = self.step_stopped(task, None)?;

        // After the step, wait for the next transition. Note that this can return
        // an exited state if there is a group exit while some thread is blocked on
        // a syscall.
        loop {
            let wait = running.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match wait {
                Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)) => {
                    #[cfg(test)]
                    let forced_external_sigtrap = self.liteinst_runtime.lock().unwrap().phase
                        == LiteinstRuntimePhase::Waiting
                        && self
                            .global_state
                            .liteinst_runtime
                            .as_ref()
                            .and_then(|runtime| runtime.force_skip_signal_once.as_ref())
                            .is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst));
                    #[cfg(not(test))]
                    let forced_external_sigtrap = false;
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        Signal::SIGTRAP,
                        LiteinstActivationOperation::SkipInterceptedSyscall,
                        NestedTrapExpectation::SyscallSkip { pre_rip },
                        forced_external_sigtrap,
                    )?;
                    #[cfg(target_arch = "x86_64")]
                    task.setregs(&regs)?;
                    break Ok(task);
                }
                Wait::Stopped(task, Event::Signal(sig)) => {
                    self.validate_nested_liteinst_activation_signal(
                        &task,
                        sig,
                        LiteinstActivationOperation::SkipInterceptedSyscall,
                        NestedTrapExpectation::SyscallSkip { pre_rip },
                        false,
                    )?;
                    // We can get a spurious signal here, such as SIGWINCH. Skip
                    // past them until the tracee eventually arrives at SIGTRAP.
                    running = self.step_stopped(task, sig)?;
                }
                Wait::Stopped(task, event) => {
                    panic!(
                        "skip_seccomp_syscall: PID {} got unexpected event: {:?}",
                        task.pid(),
                        event
                    );
                }
                Wait::Exited(_pid, exit_status) => {
                    #[allow(unreachable_code)]
                    break self.exit(exit_status).await;
                }
            }
        }
    }

    /// inject syscall for given tracee
    ///
    /// NB: limitations:
    /// - tracee must be in stopped state.
    /// - the tracee must have returned from PTRACE_EXEC_EVENT
    /// - must be called on the ptracer thread
    ///
    /// Side effects:
    /// - mutates contexts
    async fn untraced_syscall(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        self.validate_liteinst_mapping_execution(nr, args)?;
        tracing::trace!(
            "[scheduler/tool] (pid = {}) untraced syscall: {:?}",
            task.pid(),
            nr
        );
        // TODO-HUMAN-REVIEW(PR-103): Review original-frame syscall injection.
        let oldregs = task.getregs()?;
        let mut regs = if self.injected_syscall_frame.is_some() {
            self.read_guest_registers(&task)?
        } else {
            oldregs
        };

        *regs.syscall_mut() = nr as Reg;
        *regs.orig_syscall_mut() = nr as Reg;
        regs.set_args((
            args.arg0 as Reg,
            args.arg1 as Reg,
            args.arg2 as Reg,
            args.arg3 as Reg,
            args.arg4 as Reg,
            args.arg5 as Reg,
        ));
        let child_context = self.injected_syscall_frame.is_some().then_some(regs);

        // Jump to our private page to run the syscall instruction there. See
        // `populate_mmap_page` for details.
        *regs.ip_mut() = cp::PRIVATE_PAGE_OFFSET as Reg;

        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            let purpose =
                if self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Ready {
                    after_loader_task::AfterLoaderSyscallPurpose::ToolInjection
                } else {
                    after_loader_task::AfterLoaderSyscallPurpose::TraceePreinit
                };
            self.arm_after_loader_syscall_permit(
                &task,
                purpose,
                nr as i64,
                [
                    args.arg0 as u64,
                    args.arg1 as u64,
                    args.arg2 as u64,
                    args.arg3 as u64,
                    args.arg4 as u64,
                    args.arg5 as u64,
                ],
                cp::PRIVATE_PAGE_OFFSET as u64,
            )?;
        }

        task.setregs(&regs)?;

        // Step to run the syscall instruction.
        let mut wait = self.step_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait);
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            let stopped = match wait {
                Wait::Stopped(stopped, Event::Seccomp) => stopped,
                _ => return Err(Errno::EPROTO.into()),
            };
            let permit = self
                .consume_after_loader_syscall_permit(&stopped)?
                .ok_or(Errno::EPROTO)?;
            wait = self.syscall_stopped(stopped, None)?.next_state().await?;
            self.arm_liteinst_wait(&wait);
            match &wait {
                Wait::Stopped(stopped, Event::Syscall) => {
                    let completed = self
                        .complete_after_loader_syscall_inflight(stopped)?
                        .ok_or(Errno::EPROTO)?;
                    if completed != permit {
                        return Err(Errno::EPROTO.into());
                    }
                }
                _ => return Err(Errno::EPROTO.into()),
            }
        }

        // Get the result of the syscall to return to the caller.
        let (task, result) = self
            .status_to_result(wait, Some(oldregs), child_context)
            .await?;
        self.observe_liteinst_mapping_result(nr, args, result);
        Ok((task, result))
    }

    // Helper function
    async fn private_inject(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        let task = self.skip_seccomp_syscall(task).await?;

        self.untraced_syscall(task, nr, args).await
    }

    async fn status_to_result(
        &mut self,
        wait_status: Wait,
        context: Option<libc::user_regs_struct>,
        child_context: Option<libc::user_regs_struct>,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        #[cfg(test)]
        let forced_external_sigtrap = matches!(&wait_status, Wait::Stopped(_, _))
            && self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Waiting
            && self
                .global_state
                .liteinst_runtime
                .as_ref()
                .is_some_and(|runtime| {
                    let force_once = if context.is_none() {
                        runtime.force_context_none_signal_once.as_ref()
                    } else {
                        runtime.force_context_signal_once.as_ref()
                    };
                    force_once.is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst))
                });
        #[cfg(not(test))]
        let forced_external_sigtrap = false;
        #[cfg(test)]
        let wait_status = if forced_external_sigtrap {
            match wait_status {
                Wait::Stopped(task, _) => Wait::Stopped(task, Event::Signal(Signal::SIGTRAP)),
                other => other,
            }
        } else {
            wait_status
        };
        #[cfg(test)]
        if context.is_some()
            && self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Waiting
            && let Wait::Stopped(stopped, _) = &wait_status
            && self
                .global_state
                .liteinst_runtime
                .as_ref()
                .and_then(|runtime| runtime.force_private_stub_mutation_once.as_ref())
                .is_some_and(|force_once| force_once.swap(false, Ordering::SeqCst))
        {
            let mut mutated_stub = [0; cp::SYSCALL_INSTR_SIZE * 2];
            stopped.read_exact(cp::PRIVATE_PAGE_OFFSET, &mut mutated_stub)?;
            mutated_stub[0] ^= 0xff;
            let mut stopped_writer = stopped.memory();
            let address = AddrMut::from_raw(cp::PRIVATE_PAGE_OFFSET).ok_or(Errno::EFAULT)?;
            stopped_writer.write_value(address, &mutated_stub)?;
        }
        match wait_status {
            Wait::Stopped(stopped, event) => match event {
                Event::Signal(sig) if context.is_none() => {
                    self.validate_nested_liteinst_activation_signal(
                        &stopped,
                        sig,
                        LiteinstActivationOperation::FinishReinjectedSyscall,
                        NestedTrapExpectation::None,
                        forced_external_sigtrap,
                    )?;
                    let regs = stopped.getregs()?;
                    Ok((stopped, Ok(regs.ret() as i64)))
                }
                Event::Signal(sig) => {
                    self.validate_nested_liteinst_activation_signal(
                        &stopped,
                        sig,
                        LiteinstActivationOperation::FinishInjectedSyscall,
                        NestedTrapExpectation::PrivateSyscall(
                            (cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE) as u64,
                        ),
                        forced_external_sigtrap,
                    )?;
                    let mut regs = stopped.getregs()?;
                    // NB: it is possible to get interrupted by signal (such as
                    // SIGCHLD) before single step finishes, while RIP still
                    // points at the private page.
                    debug_assert!(
                        regs.ip() as usize == cp::PRIVATE_PAGE_OFFSET + cp::SYSCALL_INSTR_SIZE
                            || regs.ip() as usize == cp::PRIVATE_PAGE_OFFSET
                    );
                    // interrupted by signal, return -ERESTARTSYS so that tracee can do a
                    // restart_syscall.
                    if sig != Signal::SIGTRAP {
                        *regs.ret_mut() = (-(Errno::ERESTARTSYS.into_raw()) as i64) as u64;
                        self.pending_signal = Some(sig);
                    }
                    let result = Errno::from_ret(regs.ret() as usize).map(|x| x as i64);
                    if let Some(context) = context {
                        if child_context.is_some() {
                            // An injected-frame event temporarily replaces the
                            // controller's live trap registers with the logical
                            // guest frame. Restore every controller register;
                            // leaving even a callee-saved register (notably R12,
                            // used by LiteInst as its HookContext base) would
                            // corrupt the callback that resumes after injection.
                            stopped.setregs(&context)?;
                        } else {
                            // Restore syscall args to original values. This is
                            // needed when we convert syscalls like SYS_open ->
                            // SYS_openat, syscall args are modified need to restore
                            // it back.
                            restore_context(&stopped, context, None, false)?;
                        }
                    }
                    Ok((stopped, result))
                }
                Event::NewChild(op, child) => {
                    let ret = child.pid().as_raw() as i64;
                    let wait = self
                        .dispatch_new_task(op, stopped, child, context, child_context)
                        .await?;
                    match wait {
                        Wait::Stopped(stopped, _) => Ok((stopped, Ok(ret))),
                        Wait::Exited(_, exit_status) => self.exit(exit_status).await,
                    }
                }
                Event::Exec(former_tid) => {
                    // This should never return.
                    let next_state = self.handle_exec_event(stopped, former_tid).await?;
                    self.execve(next_state).await
                }
                Event::Syscall => {
                    let regs = stopped.getregs()?;
                    let result = Errno::from_ret(regs.ret() as usize).map(|x| x as i64);
                    if let Some(context) = context {
                        if child_context.is_some() {
                            stopped.setregs(&context)?;
                        } else {
                            restore_context(&stopped, context, None, false)?;
                        }
                    }
                    Ok((stopped, result))
                }
                st => panic!("untraced_syscall returned unknown state: {:?}", st),
            },
            Wait::Exited(_pid, exit_status) => self.exit(exit_status).await,
        }
    }

    async fn do_inject(&mut self, nr: Sysno, args: SyscallArgs) -> Result<i64, Errno> {
        match self.inner_inject(nr, args).await {
            Ok(ret) => ret,
            Err(err) => self.abort(Err(err)).await,
        }
    }

    async fn inner_inject(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<Result<i64, Errno>, TraceError> {
        let task = self.take_tool_callback_stop()?;

        tracing::debug!(
            "[tool] (tid {}) beginning inject of syscall: {}, args {:?}",
            self.tid(),
            nr,
            args,
        );

        let outcome =
            if self.injected_syscall_frame.is_some() || self.pending_syscall_already_skipped {
                self.pending_syscall = None;
                self.untraced_syscall(task, nr, args).await
            } else if self.pending_syscall.take() == Some((nr, args)) {
                // If we're reinjecting the same syscall with the same arguments,
                // then we can just let the tracee continue and stop at sysexit.
                self.validate_liteinst_mapping_execution(nr, args)?;
                let wait = self.syscall_stopped(task, None)?.next_state().await?;
                self.arm_liteinst_wait(&wait);
                let (task, result) = self.status_to_result(wait, None, None).await?;
                self.observe_liteinst_mapping_result(nr, args, result);
                Ok((task, result))
            } else {
                self.private_inject(task, nr, args).await
            };
        match outcome {
            Ok((task, result)) => {
                self.finish_tool_injection(task)?;
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }

    async fn do_tail_inject(&mut self, nr: Sysno, args: SyscallArgs) -> ! {
        match self.inner_tail_inject(nr, args).await {
            Ok(_) => {
                // Drop the handle_syscall_event future.
                self.cancel_handler.store(true, Ordering::SeqCst);
                future::pending().await
            }
            Err(err) => self.abort(Err(err)).await,
        }
    }

    async fn inner_tail_inject(
        &mut self,
        nr: Sysno,
        args: SyscallArgs,
    ) -> Result<Result<i64, Errno>, TraceError> {
        let tid = self.tid();

        tracing::info!(
            "[tool] (tid {}) beginning tail_inject of syscall: {}",
            &tid,
            nr,
        );

        let task = self.take_tool_callback_stop()?;

        if self.injected_syscall_frame.is_some() {
            self.pending_syscall = None;
            let (task, result) = self.untraced_syscall(task, nr, args).await?;
            self.write_injected_syscall_result(&task, result)?;
            self.finish_tool_injection(task)?;
            return Ok(result);
        }

        if self.pending_syscall_already_skipped {
            self.pending_syscall = None;
            let (task, result) = self.untraced_syscall(task, nr, args).await?;
            set_ret(
                &task,
                result.unwrap_or_else(|errno| -(errno.into_raw() as i64)) as u64,
            )?;
            self.finish_tool_injection(task)?;
            return Ok(result);
        }

        if is_liteinst_mapping_syscall(nr) && self.pending_syscall == Some((nr, args)) {
            self.pending_syscall = None;
            let (task, result) = self.private_inject(task, nr, args).await?;
            set_ret(
                &task,
                result.unwrap_or_else(|errno| -(errno.into_raw() as i64)) as u64,
            )?;
            self.finish_tool_injection(task)?;
            return Ok(result);
        }

        if self.pending_syscall.take() == Some((nr, args)) {
            // We're reinjecting the same syscall with the same arguments.
            // Nothing to actually do but let the tracee resume.

            // The return value here doesn't matter.
            self.finish_tool_injection(task)?;
            Ok(Ok(0))
        } else {
            // Syscall has already been injected. Can't do the optimization.
            let (task, result) = self.private_inject(task, nr, args).await?;
            self.finish_tool_injection(task)?;
            Ok(result)
        }
    }

    fn active_stopped(&self) -> Result<&Stopped, TraceError> {
        self.active_tool_stop
            .as_ref()
            .ok_or_else(|| Errno::EPROTO.into())
    }

    fn begin_tool_callback(&mut self, task: Stopped) -> Result<(), TraceError> {
        if self.active_tool_stop.is_some() {
            return Err(Errno::EALREADY.into());
        }
        self.active_tool_stop = Some(task);
        Ok(())
    }

    fn take_tool_callback_stop(&mut self) -> Result<Stopped, TraceError> {
        self.active_tool_stop
            .take()
            .ok_or_else(|| Errno::EPROTO.into())
    }

    fn finish_tool_injection(&mut self, task: Stopped) -> Result<(), TraceError> {
        if self.active_tool_stop.is_some() {
            return Err(Errno::EALREADY.into());
        }
        self.active_tool_stop = Some(task);
        Ok(())
    }

    async fn notify_gdb_stop(&self, reason: StopReason) -> Result<(), TraceError> {
        if !self.attached_by_gdb {
            return Ok(());
        }

        if let Some(stop_tx) = self.gdb_stop_tx.as_ref() {
            let request_tx = self.gdb_request_tx.clone();
            let resume_tx = self.gdb_resume_tx.clone();
            let stop = StoppedInferior {
                reason,
                request_tx: request_tx.ok_or(Errno::EIO)?,
                resume_tx: resume_tx.ok_or(Errno::EIO)?,
            };
            if stop_tx.send(stop).await.is_err() {
                tracing::warn!(
                    tid = %self.tid(),
                    "GDB stop channel closed while reporting tracee stop"
                );
            }
        }
        Ok(())
    }

    async fn handle_gdb_request(&mut self, request: Option<GdbRequest>) {
        if let Some(request) = request {
            match request {
                GdbRequest::SetBreakpoint(bkpt, reply_tx) => {
                    if bkpt.ty == BreakpointType::Software {
                        let result = self.add_breakpoint(bkpt.addr).await;
                        let _ = reply_tx.send(result);
                    }
                }
                GdbRequest::RemoveBreakpoint(bkpt, reply_tx) => {
                    if bkpt.ty == BreakpointType::Software {
                        let result = self.remove_breakpoint(bkpt.addr).await;
                        let _ = reply_tx.send(result);
                    }
                }
                GdbRequest::ReadInferiorMemory(addr, length, reply_tx) => {
                    let result = self.read_inferior_memory(addr, length);
                    let _ = reply_tx.send(result);
                }
                GdbRequest::WriteInferiorMemory(addr, length, data, reply_tx) => {
                    let result = self.write_inferior_memory(addr, length, data);
                    let _ = reply_tx.send(result);
                }
                GdbRequest::ReadRegisters(reply_tx) => {
                    let result = self.read_registers();
                    let _ = reply_tx.send(result);
                }
                GdbRequest::WriteRegisters(core_regs, reply_tx) => {
                    let result = self.write_registers(core_regs);
                    let _ = reply_tx.send(result);
                }
            }
        }
    }

    async fn handle_gdb_resume(
        &mut self,
        resume: Option<ResumeInferior>,
        task: Stopped,
        resume_action: ExpectedGdbResume,
    ) -> Result<(Running, Option<ResumeInferior>), TraceError> {
        match resume {
            None => Ok((self.resume_stopped(task, None)?, None)),
            Some(resume) => {
                let is_resume = resume_action == ExpectedGdbResume::Resume || resume.detach;
                let is_step_only = resume_action == ExpectedGdbResume::StepOnly;
                // During a step-over, gdb normally single-steps over the
                // breakpoint installed at the current PC. But if gdb has already
                // removed that breakpoint it issues a plain continue instead of a
                // single-step. This happens, for example, after `finish`: gdb
                // implements it with a temporary breakpoint at the return address
                // which it deletes as soon as it is hit, so when the user then
                // resumes there is no breakpoint left to step over. No step-over
                // is required in that case, so resume normally rather than
                // treating the continue as an unexpected action (which used to
                // panic here).
                let is_step_over = resume_action == ExpectedGdbResume::StepOver;
                let running = match resume.action {
                    ResumeAction::Step(sig) => self.step_stopped(task, sig)?,
                    ResumeAction::Continue(sig) if is_resume => self.resume_stopped(task, sig)?,
                    ResumeAction::Continue(sig) if is_step_only => self.step_stopped(task, sig)?,
                    ResumeAction::Continue(sig) if is_step_over => {
                        self.resume_stopped(task, sig)?
                    }
                    action => panic!(
                        "[pid = {}] unexpected resume action {:?}, expecting: {:?}",
                        task.pid(),
                        action,
                        resume_action,
                    ),
                };
                Ok((running, Some(resume)))
            }
        }
    }

    async fn await_gdb_resume(
        &mut self,
        task: Stopped,
        resume_action: ExpectedGdbResume,
    ) -> Result<Running, TraceError> {
        if !self.attached_by_gdb {
            return self.resume_stopped(task, None);
        }

        self.begin_tool_callback(task)?;
        let mut resume_rx = self.gdb_resume_rx.take().ok_or(Errno::EIO)?;
        let mut gdb_request_rx = self.gdb_request_rx.take().ok_or(Errno::EIO)?;

        let mut resume_future = Box::pin(resume_rx.recv());

        let (running, resumed) = loop {
            let request_future = Box::pin(gdb_request_rx.recv());

            match future::select(request_future, resume_future).await {
                Either::Left((gdb_request, pending_resume_future)) => {
                    self.handle_gdb_request(gdb_request).await;
                    resume_future = pending_resume_future;
                }
                Either::Right((resume_request, _)) => {
                    let task = self.take_tool_callback_stop()?;
                    break self
                        .handle_gdb_resume(resume_request, task, resume_action)
                        .await?;
                }
            }
        };

        self.gdb_request_rx = Some(gdb_request_rx);
        self.gdb_resume_rx = Some(resume_rx);

        if let Some(resumed) = resumed {
            if resumed.detach {
                tracing::debug!(
                    target: "reverie_ptrace::lifecycle",
                    parent: &tracing::debug_span!(
                        target: "reverie_ptrace::lifecycle",
                        "tracee.detach",
                        tid = %self.tid(),
                        reason = "GDB detach"
                    ),
                    "GDB detached from tracee"
                );
                // no longer report stop event to gdb
                // self.gdb_stop_tx = None;
                self.attached_by_gdb = false;
            }

            self.resumed_by_gdb = Some(resumed.action);
        }

        Ok(running)
    }

    /// Resume from a software breakpoint set by gdb. The resume action is
    /// initiated from gdb (client).
    // NB: caller to %rip accordingly prior to hitting breakpoint.
    async fn resume_from_swbreak(
        &mut self,
        task: Stopped,
        regs: libc::user_regs_struct,
    ) -> Result<Wait, TraceError> {
        task.setregs(&regs)?;

        // Task could be hitting a breakpoint, after previously suspended by
        // a different task, need to notify this task is fully stopped.
        self.suspended.store(true, Ordering::SeqCst);
        if let Some((suspended_flag, stop_tx)) = self.get_stop_tx().await
            && stop_tx
                .send((
                    self.tid(),
                    Suspended {
                        waker: None,
                        suspended: suspended_flag,
                    },
                ))
                .await
                .is_err()
        {
            tracing::warn!(
                    tid = %self.tid(),
                    "tracee freeze channel closed during GDB breakpoint handling"
            );
        }

        // When resuming from breakpoint, gdb (client) needs to remove the
        // breakpoint (implying restore the original instruction), do a
        // single-step (step-over), and re-insert the breakpoint.
        // Because removing (sw) breakpoint modifies the instructions, other
        // thread might miss the breakpoint after the breakpoint is removed
        // and before the breakpoint is (re-)inserted. Hence we must make
        // serialize this sequence.
        let needs_step_over = self.needs_step_over.clone();
        let _guard = needs_step_over.lock().await;

        self.notify_gdb_stop(StopReason::stopped(
            task.pid(),
            self.pid(),
            StopEvent::SwBreak,
            regs.into(),
        ))
        .await?;

        self.freeze_all().await?;

        let running = self
            .await_gdb_resume(task, ExpectedGdbResume::StepOver)
            .await?;

        // If gdb removed the breakpoint at the current PC and issued a plain
        // continue instead of the usual step-over single-step (e.g. after a
        // `finish` temporary breakpoint was hit and deleted, and the user then
        // continues), there is no intermediate single-step stop to report back
        // to gdb. Just run to the next event and return it directly. The task
        // may run all the way to exit in this case, so we must not assume it
        // stops again.
        if !matches!(self.resumed_by_gdb, Some(ResumeAction::Step(_))) {
            // Release the siblings frozen above *before* waiting for this
            // task's next event. gdb has resumed everything, and this task may
            // now block on a sibling -- a join, a futex, a pipe read -- which a
            // frozen sibling can never satisfy. Waiting first deadlocks the
            // guest.
            self.thaw_all().await?;
            let wait = running.next_state().await?;
            self.arm_liteinst_wait(&wait);
            return Ok(wait);
        }

        let wait = running.next_state().await?.assume_stopped();
        let mut task = wait.0;
        let mut event = wait.1;
        self.arm_liteinst_stop(&task, &event);

        // Detached by client.
        if !self.attached_by_gdb {
            self.thaw_all().await?;
            return Ok(Wait::Stopped(task, event));
        }

        task = loop {
            match event {
                Event::Signal(Signal::SIGTRAP) => break task,
                Event::Signal(Signal::SIGSTOP) => {
                    let running = self.step_stopped(task, None)?;
                    let wait = running.next_state().await?.assume_stopped();
                    task = wait.0;
                    event = wait.1;
                    self.arm_liteinst_stop(&task, &event);
                }
                // TODO: combine with handle_signal!
                Event::Signal(Signal::SIGCHLD) => {
                    let running = self.step_stopped(task, Signal::SIGCHLD)?;
                    let wait = running.next_state().await?.assume_stopped();
                    task = wait.0;
                    event = wait.1;
                    self.arm_liteinst_stop(&task, &event);
                }
                unknown => panic!("[pid = {}] got unexpected event {:?}", self.tid(), unknown),
            }
        };
        self.notify_gdb_stop(StopReason::stopped(
            task.pid(),
            self.pid(),
            StopEvent::Signal(Signal::SIGTRAP),
            task.getregs()?.into(),
        ))
        .await?;

        let running = self
            .await_gdb_resume(task, ExpectedGdbResume::Resume)
            .await?;
        // Same ordering requirement as the plain-continue path above: the
        // step-over is finished and the breakpoint is back in place, so the
        // siblings must be released before this task's next event is awaited.
        self.thaw_all().await?;
        let wait = running.next_state().await?;
        self.arm_liteinst_wait(&wait);
        Ok(wait)
    }

    /// check if the stop is caused by sw breakpoint.
    async fn check_swbreak(&mut self, wait: Wait) -> Result<Wait, TraceError> {
        self.arm_liteinst_wait(&wait);
        match wait {
            Wait::Stopped(task, event) if event == Event::Signal(Signal::SIGTRAP) => {
                let mut regs = task.getregs()?;
                let rip_minus_one = regs.ip() - 1;
                if self.breakpoints.contains_key(&rip_minus_one) {
                    *regs.ip_mut() = rip_minus_one;
                    self.resume_from_swbreak(task, regs).await
                } else {
                    Ok(Wait::Stopped(task, event))
                }
            }
            other => Ok(other),
        }
    }

    async fn add_breakpoint(&mut self, addr: u64) -> Result<(), TraceError> {
        if let Some(bkpt_addr) = AddrMut::from_raw(addr as usize) {
            let task = self.active_stopped()?;
            let mut memory = task.memory();
            let saved_insn: u64 = memory.read_value(bkpt_addr)?;
            let insn = (saved_insn & !0xffu64) | 0xccu64;
            memory.write_value(bkpt_addr, &insn)?;
            self.breakpoints.insert(addr, saved_insn);
        }
        Ok(())
    }

    /// thaw all threads.
    async fn thaw_all(&mut self) -> Result<(), TraceError> {
        for (_pid, suspended_task) in core::mem::take(&mut self.suspended_tasks) {
            if let Some(tx) = suspended_task.waker.as_ref() {
                suspended_task.suspended.store(false, Ordering::SeqCst);
                let _sent = tx.try_send(self.tid());
            }
        }
        Ok(())
    }

    /// freeze all threads, except the caller.
    async fn freeze_all(&mut self) -> Result<(), TraceError> {
        // The tool have chosen to sequentialize thread execution, gdbserver
        // should avoid doing its own thread serialization, otherwise this
        // could lead to deadlock.
        if *self.global_state.sequentialized_guest {
            return Ok(());
        }
        let (stop_tx, mut stop_rx) = mpsc::channel(1);
        for child in self.child_threads.lock().await.deref_mut().into_iter() {
            if child.id() != self.tid() && !child.suspended.load(Ordering::SeqCst) {
                let killed = Errno::result(unsafe {
                    libc::syscall(libc::SYS_tgkill, self.pid(), child.id(), Signal::SIGSTOP)
                });
                if killed.is_ok() {
                    child.suspended.store(true, Ordering::SeqCst);
                    child.wait_all_stop_tx = Some(stop_tx.clone());
                }
            }
        }
        drop(stop_tx);
        while let Some((pid, suspended_task)) = stop_rx.recv().await {
            self.suspended_tasks.insert(pid, suspended_task);
        }
        Ok(())
    }

    async fn remove_breakpoint(&mut self, addr: u64) -> Result<(), TraceError> {
        let mut memory = self.active_stopped()?.memory();
        let insn = self.breakpoints.remove(&addr).ok_or(Errno::ENOENT)?;
        if let Some(bkpt_addr) = AddrMut::from_raw(addr as usize) {
            memory.write_value(bkpt_addr, &insn)?;
        }
        Ok(())
    }

    fn read_inferior_memory(&self, addr: u64, mut size: usize) -> Result<Vec<u8>, TraceError> {
        let task = self.active_stopped()?;
        let memory = task.memory();

        // NB: dont' trust size to be sane blindly.
        if size > 0x8000 {
            size = 0x8000;
        }

        let mut res = vec![0; size];
        if let Some(addr) = Addr::from_raw(addr as usize) {
            let nb = memory.read(addr, &mut res)?;
            res.resize(nb, 0);
        }

        // There could be a software breakpoint within the address requested,
        // we should return the orignal contents without the breakpoint insn.
        // This is *not* documented in gdb remote protocol, however, both
        // gdbserver and rr does this. see:
        // rr: https://github.com/rr-debugger/rr/blob/master/src/GdbServer.cc#L561
        // gdbserver: https://github.com/bminor/binutils-gdb/blob/master/gdbserver/mem-break.cc#L1914
        for (bkpt, saved_insn) in self.breakpoints.iter() {
            if (addr..addr + res.len() as u64).contains(bkpt) {
                // This abuses bkpt insn 0xcc is single byte.
                res[*bkpt as usize - addr as usize] = *saved_insn as u8;
            }
        }

        Ok(res)
    }

    fn write_inferior_memory(
        &self,
        addr: u64,
        size: usize,
        data: Vec<u8>,
    ) -> Result<(), TraceError> {
        let mut memory = self.active_stopped()?.memory();
        let size = std::cmp::min(size, data.len());
        let addr = AddrMut::from_raw(addr as usize).ok_or(Errno::EFAULT)?;
        memory.write(addr, &data[..size])?;
        Ok(())
    }

    fn read_registers(&self) -> Result<CoreRegs, TraceError> {
        let task = self.active_stopped()?;
        let regs = task.getregs()?;
        let fpregs = task.getfpregs()?;
        let core_regs = CoreRegs::from_parts(regs, fpregs);
        Ok(core_regs)
    }

    fn write_registers(&self, core_regs: CoreRegs) -> Result<(), TraceError> {
        let task = self.active_stopped()?;
        let (regs, fpregs) = core_regs.into_parts();
        task.setregs(&regs)?;
        task.setfpregs(&fpregs)?;
        Ok(())
    }
}

#[async_trait]
impl<L: Tool + 'static> Guest<L> for TracedTask<L> {
    type Memory = StoppedMemory;
    type Stack = GuestStack;

    #[inline]
    fn tid(&self) -> Pid {
        self.tid
    }

    #[inline]
    fn pid(&self) -> Pid {
        self.pid
    }

    #[inline]
    fn ppid(&self) -> Option<Pid> {
        self.ppid
    }

    fn is_command_bootstrap(&self) -> bool {
        self.command_bootstrap
    }

    fn memory(&self) -> Self::Memory {
        self.active_stopped()
            .expect("tool memory access requires its observed stopped capability")
            .memory()
    }

    async fn regs(&mut self) -> libc::user_regs_struct {
        let task = match self.active_stopped() {
            Ok(task) => task,
            Err(err) => self.abort(Err(err)).await,
        };

        match self.read_guest_registers(task) {
            Ok(ret) => ret,
            Err(err) => self.abort(Err(err)).await,
        }
    }

    async fn set_regs(&mut self, regs: libc::user_regs_struct) -> Result<(), reverie::Error> {
        let task = match self.active_stopped() {
            Ok(task) => task,
            Err(err) => self.abort(Err(err)).await,
        };

        if let Err(err) = self.write_guest_registers(task, &regs) {
            // Mirror `regs()`: a ptrace register access failure aborts the task.
            self.abort(Err(err)).await;
        }
        Ok(())
    }

    async fn stack(&mut self) -> Self::Stack {
        let checkout = self.stack_checked_out.clone();
        let stack = match self.active_stopped() {
            Ok(task) => GuestStack::new(task, checkout),
            Err(err) => Err(err),
        };
        match stack {
            Ok(ret) => ret,
            Err(err) => self.abort(Err(err)).await,
        }
    }

    fn thread_state_mut(&mut self) -> &mut L::ThreadState {
        &mut self.thread_state
    }

    fn thread_state(&self) -> &L::ThreadState {
        &self.thread_state
    }

    async fn daemonize(&mut self) {
        let pid = self.pid();
        self.ndaemons.fetch_add(1, Ordering::SeqCst);
        self.is_a_daemon = true;

        tracing::info!("[reverie] daemonizing pid {} ..", pid);
        if self
            .daemonizer
            .send(self.daemon_kill_switch.subscribe())
            .await
            .is_err()
        {
            tracing::error!(%pid, "failed to notify orphan reaper while daemonizing tracee");
            self.ndaemons.fetch_sub(1, Ordering::SeqCst);
            self.is_a_daemon = false;
            return;
        }

        if self.ndaemons.load(Ordering::SeqCst) == self.ntasks.load(Ordering::SeqCst) {
            let _ = self.daemon_kill_switch.send(());
        }
    }

    async fn inject<S: SyscallInfo>(&mut self, syscall: S) -> Result<i64, Errno> {
        // Call a non-templatized function to reduce code bloat.
        let (nr, args) = syscall.into_parts();
        self.do_inject(nr, args).await
    }

    #[allow(unreachable_code)]
    async fn tail_inject<S: SyscallInfo>(&mut self, syscall: S) -> Never {
        // Call a non-templatized function to reduce code bloat.
        let (nr, args) = syscall.into_parts();
        self.do_tail_inject(nr, args).await
    }

    fn set_timer(&mut self, sched: TimerSchedule) -> Result<(), reverie::Error> {
        let rcbs = match sched {
            TimerSchedule::Rcbs(r) => r,
            TimerSchedule::Time(dur) => Timer::as_ticks(dur),
            //if timer is imprecise there is no really a point in trying to single step any further than r
            TimerSchedule::RcbsAndInstructions(r, _) => r,
        };
        self.timer
            .request_event(TimerEventRequest::Imprecise(rcbs))?;
        Ok(())
    }

    fn set_timer_precise(&mut self, sched: TimerSchedule) -> Result<(), reverie::Error> {
        match sched {
            TimerSchedule::Rcbs(r) => self.timer.request_event(TimerEventRequest::Precise(r))?,
            TimerSchedule::Time(dur) => self
                .timer
                .request_event(TimerEventRequest::Precise(Timer::as_ticks(dur)))?,
            TimerSchedule::RcbsAndInstructions(r, i) => self
                .timer
                .request_event(TimerEventRequest::PreciseInstruction(r, i))?,
        };
        Ok(())
    }

    fn read_clock(&mut self) -> Result<u64, reverie::Error> {
        Ok(self.timer.read_clock())
    }

    fn backtrace(&mut self) -> Option<Backtrace> {
        use unwind::Accessors;
        use unwind::AddressSpace;
        use unwind::Byteorder;
        use unwind::Cursor;
        use unwind::PTraceState;
        use unwind::RegNum;

        let mut frames = Vec::new();

        let space = AddressSpace::new(Accessors::ptrace(), Byteorder::DEFAULT).ok()?;
        let state = PTraceState::new(self.tid.as_raw() as u32).ok()?;
        let mut cursor = Cursor::remote(&space, &state).ok()?;

        loop {
            let ip = cursor.register(RegNum::IP).ok()?;
            let is_signal = cursor.is_signal_frame().ok()?;

            frames.push(Frame { ip, is_signal });

            if !cursor.step().ok()? {
                break;
            }
        }

        // TODO: Take a snapshot of `/proc/self/maps` so the backtrace can be
        // processed offline?

        Some(Backtrace::new(self.tid(), frames))
    }

    fn has_cpuid_interception(&self) -> bool {
        self.has_cpuid_interception
    }
}

#[async_trait]
impl<L: Tool + 'static> GlobalRPC<L::GlobalState> for TracedTask<L> {
    async fn send_rpc<'a>(
        &'a self,
        args: <L::GlobalState as GlobalTool>::Request,
    ) -> <L::GlobalState as GlobalTool>::Response {
        let wrapped = WrappedFrom(self.tid(), &self.global_state);
        wrapped.send_rpc(args).await
    }

    fn config(&self) -> &<L::GlobalState as GlobalTool>::Config {
        &self.global_state.cfg
    }
}

/// Wrap a GlobalState with a Tid from which the messages originate.  This enables the
/// GlobalRPC instance below.
struct WrappedFrom<'a, G: GlobalTool>(Tid, &'a GlobalState<G>);

#[async_trait]
impl<'a, G: GlobalTool> GlobalRPC<G> for WrappedFrom<'a, G> {
    async fn send_rpc(&self, args: G::Request) -> G::Response {
        // In debugging mode we round-trip through a serialized representation
        // to make sure it works.
        let deserial = if cfg!(debug_assertions) {
            let serial = bincode::serde::encode_to_vec(&args, bincode::config::legacy())
                .expect("GlobalRPC request must serialize in debug validation mode");
            bincode::serde::decode_from_slice(&serial, bincode::config::legacy())
                .expect("serialized GlobalRPC request must deserialize in debug validation mode")
                .0
        } else {
            args
        };
        self.1.gs_ref.receive_rpc(self.0, deserial).await
    }
    fn config(&self) -> &G::Config {
        &self.1.cfg
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_bootstrap_arguments_preserve_types_and_raw_tail() {
        let args = super::SyscallArgs::new(0x1000, 0x2000, 0, 41, 0x3000, 43);
        let render = |nr, command_bootstrap| {
            format!(
                "{:?}",
                super::SyscallArgsForLog {
                    nr,
                    args,
                    command_bootstrap,
                }
            )
        };
        assert_eq!(
            render(super::Sysno::execve, true),
            "SyscallArgs { arg0: <hostaddr 0x1000>, arg1: <hostaddr 0x2000>, arg2: 0, arg3: 41, arg4: 12288, arg5: 43 }"
        );
        for nr in [
            super::Sysno::execve,
            super::Sysno::write,
            super::Sysno::execveat,
        ] {
            assert_eq!(render(nr, false), format!("{args:?}"));
        }
        assert_eq!(render(super::Sysno::write, true), format!("{args:?}"));
        assert_eq!(render(super::Sysno::execveat, true), format!("{args:?}"));
        let aliased = super::SyscallArgsForLog {
            nr: super::Sysno::execve,
            args: super::SyscallArgs::new(0x1000, 0x1000, 0, 41, 0x3000, 43),
            command_bootstrap: true,
        };
        assert_ne!(format!("{aliased:?}"), render(super::Sysno::execve, true));
    }

    use super::*;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn syscall_skip_breakpoint_requires_exact_captured_provenance() {
        let exec_rip = 0x7f00_1234_530b;
        let arch_prctl_rip = 0x7f00_1234_cb19;
        let syscall_opcode = [0x0f, 0x05];
        assert!(is_expected_syscall_skip_breakpoint(
            libc::TRAP_BRKPT,
            exec_rip,
            exec_rip,
            syscall_opcode,
            0x48,
            false,
        ));
        assert!(is_expected_syscall_skip_breakpoint(
            libc::TRAP_BRKPT,
            arch_prctl_rip,
            arch_prctl_rip,
            syscall_opcode,
            0x48,
            false,
        ));

        for rejected in [
            is_expected_syscall_skip_breakpoint(
                libc::SI_USER,
                exec_rip,
                exec_rip,
                syscall_opcode,
                0x48,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip + 1,
                syscall_opcode,
                0x48,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip,
                [0xcc, 0x05],
                0x48,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip,
                syscall_opcode,
                0xcc,
                false,
            ),
            is_expected_syscall_skip_breakpoint(
                libc::TRAP_BRKPT,
                exec_rip,
                exec_rip,
                syscall_opcode,
                0x48,
                true,
            ),
        ] {
            assert!(!rejected);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn retired_after_loader_trap_requires_exact_runtime_origin() {
        let marker = 0x7265_766c_6900_0004;
        let rip = 0x7f00_1234;
        let generation = 7;
        let runtime_inode = 42;
        let runtime_device = libc::makedev(8, 2);
        let provenance = LiteinstTrapSiteProvenance { generation, rip };
        let mapping = |writable, inode| GuestMap {
            start: 0x7f00_0000,
            end: 0x7f00_2000,
            offset: 0,
            device_major: 8,
            device_minor: 2,
            readable: true,
            writable,
            executable: true,
            shared: false,
            inode,
            path: Some(PathBuf::from("/sealed/liteinst-runtime.so")),
        };

        for si_code in [libc::TRAP_BRKPT, libc::SI_KERNEL] {
            assert!(provenance.validates_retired_runtime_trap(
                generation,
                marker,
                marker,
                rip,
                si_code,
                0xcc,
                runtime_device,
                runtime_inode,
                &mapping(false, runtime_inode),
            ));
        }

        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip + 0x20,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_device,
            runtime_inode,
            &mapping(false, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker ^ 1,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_device,
            runtime_inode,
            &mapping(false, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::SI_USER,
            0xcc,
            runtime_device,
            runtime_inode,
            &mapping(false, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0x90,
            runtime_device,
            runtime_inode,
            &mapping(false, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_device,
            runtime_inode,
            &mapping(true, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_device,
            runtime_inode,
            &mapping(false, runtime_inode + 1),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation + 1,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_device,
            runtime_inode,
            &mapping(false, runtime_inode),
        ));
    }

    fn active_state() -> LiteinstRuntimeState {
        let mut state = LiteinstRuntimeState::default();
        state.active_hooks.insert(
            0x401005,
            ActiveHookFootprint {
                site: GuestRange::new(0x401005, 8).unwrap(),
                trampoline: GuestRange::new(0x7000_1000, 0x1000).unwrap(),
                arena_writable: GuestRange::new(0x7100_0000, 0x80_000).unwrap(),
                arena_executable: GuestRange::new(0x7000_0000, 0x80_000).unwrap(),
            },
        );
        state
    }

    #[test]
    fn exec_generation_replaces_image_state_without_changing_old_holders() {
        let mut old = active_state();
        old.phase = LiteinstRuntimePhase::Ready;
        old.generation = 41;
        old.ready_generation = Some(41);
        #[cfg(target_arch = "x86_64")]
        {
            old.after_loader_syscall_trap = Some(LiteinstTrapSiteProvenance {
                generation: 41,
                rip: 0x7000_1070,
            });
        }
        old.frame = Some(LiteinstHandshakeFrame {
            begin_rip: 0x7000_1000,
            ..Default::default()
        });
        old.attempted_sites.insert(0x401005);
        old.fallback_sites
            .insert(0x401005, LiteinstPatchOutcome::PtraceOtherFallback);
        let old = Arc::new(StdMutex::new(old));
        let holder = Arc::clone(&old);
        let next = Arc::new(StdMutex::new(old.lock().unwrap().after_exec().unwrap()));
        assert!(!Arc::ptr_eq(&holder, &next));
        let next = next.lock().unwrap();
        assert_eq!(next.phase, LiteinstRuntimePhase::Waiting);
        assert_eq!(next.generation, 42);
        assert!(next.ready_generation.is_none());
        #[cfg(target_arch = "x86_64")]
        assert!(next.after_loader_syscall_trap.is_none());
        assert!(next.frame.is_none());
        assert!(next.attempted_sites.is_empty());
        assert!(next.fallback_sites.is_empty());
        assert!(next.active_hooks.is_empty());
        let mut old = holder.lock().unwrap();
        assert_eq!(old.phase, LiteinstRuntimePhase::Ready);
        assert_eq!(old.ready_generation, Some(41));
        #[cfg(target_arch = "x86_64")]
        assert_eq!(
            old.after_loader_syscall_trap,
            Some(LiteinstTrapSiteProvenance {
                generation: 41,
                rip: 0x7000_1070,
            })
        );
        assert_eq!(old.frame.unwrap().begin_rip, 0x7000_1000);
        assert!(old.attempted_sites.contains(&0x401005));
        assert_eq!(
            old.fallback_sites.get(&0x401005),
            Some(&LiteinstPatchOutcome::PtraceOtherFallback)
        );
        assert_eq!(old.active_hooks.len(), 1);
        old.generation = u64::MAX;
        assert_eq!(old.after_exec().unwrap_err(), Errno::EOVERFLOW);
        assert_eq!(old.generation, u64::MAX);
        assert_eq!(old.phase, LiteinstRuntimePhase::Ready);
    }

    #[test]
    fn kernel_page_ranges_floor_ceil_and_reject_overflow() {
        assert_eq!(
            kernel_page_range(0x401005, 1, 4096),
            Ok(Some(GuestRange {
                start: 0x401000,
                end: 0x402000,
            }))
        );
        assert_eq!(kernel_page_range(0x401000, 0, 4096), Ok(None));
        assert_eq!(kernel_page_range(u64::MAX - 1, 4, 4096), Err(()));
        assert_eq!(kernel_page_range(0x401000, 1, 3000), Err(()));
    }

    #[test]
    fn short_successful_mapping_invalidates_the_whole_attempted_page() {
        let mut state = LiteinstRuntimeState::default();
        state.attempted_sites.extend([0x401005, 0x401fff, 0x402005]);

        state.invalidate_attempted_pages(0x401000, 1, 4096);

        assert_eq!(state.attempted_sites, HashSet::from([0x402005]));
    }

    #[test]
    fn proc_maps_paths_preserve_literal_whitespace_and_decode_octal_escapes() {
        let mapping = parse_guest_map(
            br"00400000-00401000 r-xp 00000000 08:02 123 /tmp/a  double	tab\040space\011escaped\134slash",
        )
        .unwrap();
        assert_eq!(
            mapping.path.unwrap(),
            PathBuf::from("/tmp/a  double\ttab space\tescaped\\slash")
        );
    }

    #[test]
    fn cancellable_returns_a_completed_result() {
        let cancel_handler = Arc::new(AtomicBool::new(false));

        assert_eq!(
            futures::executor::block_on(cancellable(cancel_handler, async { 42 })),
            Some(42)
        );
    }

    #[test]
    fn cancellable_observes_cancellation_in_the_same_poll() {
        let cancel_handler = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel_handler);
        let pending = future::poll_fn(move |_| {
            signal.store(true, Ordering::SeqCst);
            Poll::<()>::Pending
        });

        assert_eq!(
            futures::executor::block_on(cancellable(Arc::clone(&cancel_handler), pending)),
            None
        );
        assert!(!cancel_handler.load(Ordering::SeqCst));
    }

    #[test]
    fn active_hook_footprint_rejects_destructive_mapping_overlap() {
        let state = active_state();
        assert!(state.mapping_mutates_active_hook(
            Sysno::mprotect,
            SyscallArgs::new(0x401000, 0x1000, libc::PROT_NONE as usize, 0, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mremap,
            SyscallArgs::new(0x7000_1000, 0x1000, 0x2000, 0, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::munmap,
            SyscallArgs::new(0x7100_0000, 0x1000, 0, 0, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mmap,
            SyscallArgs::new(
                0x7000_0000,
                0x1000,
                libc::PROT_READ as usize,
                (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as usize,
                usize::MAX,
                0,
            ),
            4096,
        ));
    }

    #[test]
    fn short_mapping_lengths_cover_the_whole_active_page() {
        let state = active_state();
        for nr in [Sysno::mprotect, Sysno::pkey_mprotect, Sysno::munmap] {
            assert!(state.mapping_mutates_active_hook(
                nr,
                SyscallArgs::new(0x401000, 1, libc::PROT_NONE as usize, 0, 0, 0),
                4096,
            ));
        }
        assert!(state.mapping_mutates_active_hook(
            Sysno::mmap,
            SyscallArgs::new(0x401000, 1, 0, libc::MAP_FIXED as usize, 0, 0),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mremap,
            SyscallArgs::new(0x5000_0000, 1, 1, libc::MREMAP_FIXED as usize, 0x401000, 0,),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mremap,
            SyscallArgs::new(0x5000_0000, 0, 1, libc::MREMAP_FIXED as usize, 0x401000, 0,),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mprotect,
            SyscallArgs::new(u64::MAX as usize - 1, 4, libc::PROT_NONE as usize, 0, 0, 0),
            4096,
        ));
    }

    #[test]
    fn pkey_mprotect_is_a_controller_mapping_syscall() {
        assert!(is_liteinst_mapping_syscall(Sysno::pkey_mprotect));
    }

    #[test]
    fn active_hook_noop_protection_retains_provenance() {
        let mut state = active_state();
        assert!(!state.mapping_mutates_active_hook(
            Sysno::mprotect,
            SyscallArgs::new(
                0x401000,
                0x1000,
                (libc::PROT_READ | libc::PROT_EXEC) as usize,
                0,
                0,
                0,
            ),
            4096,
        ));
        state.invalidate_attempted_pages(0x401000, 1, 4096);
        assert_eq!(state.active_hooks.len(), 1);
    }

    #[test]
    fn liteinst_rejects_stack_pointer_updates_without_weakening_shared_frame() {
        let current = libc::user_regs_struct {
            rsp: 0x7fff_1000,
            ..unsafe { core::mem::zeroed() }
        };
        let requested = libc::user_regs_struct {
            rsp: current.rsp + 8,
            ..current
        };
        assert_eq!(
            validate_liteinst_user_regs_update(&current, &requested),
            Err(Errno::ENOTSUPP)
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn liteinst_helper_clears_only_abi_sensitive_transient_flags() {
        let transient = (1 << 8) | (1 << 10) | (1 << 16) | (1 << 18);
        let preserved = (1 << 0) | (1 << 2) | (1 << 6) | (1 << 9) | (1 << 11);
        assert_eq!(
            liteinst_helper_entry_rflags(transient | preserved),
            preserved
        );
    }

    #[test]
    fn task_panic_marker_has_canonical_shape() {
        // The token and the field order are what a harness greps for; keep
        // them stable.
        let line = format_task_panic_marker(
            Pid::from_raw(4242),
            &"Clock perf counter exceeds target value" as &(dyn std::any::Any + Send),
        );
        assert_eq!(
            line,
            "HERMIT_TASK_PANIC tid=4242 exit=101 \
             message=Clock perf counter exceeds target value"
        );
        assert!(line.starts_with(TASK_PANIC_MARKER));
        assert_eq!(TASK_PANIC_MARKER, "HERMIT_TASK_PANIC");
        assert_eq!(TASK_PANIC_EXIT_CODE, 101);
    }

    #[test]
    fn task_panic_marker_is_always_one_greppable_line() {
        // A `panic!` with a formatted message arrives as `String`, and a
        // multi-line message would otherwise split the marker across lines and
        // make it unmatchable.
        let payload = String::from("first line\nsecond line\r\nthird");
        let line =
            format_task_panic_marker(Pid::from_raw(7), &payload as &(dyn std::any::Any + Send));
        assert_eq!(line.lines().count(), 1);
        assert_eq!(
            line,
            "HERMIT_TASK_PANIC tid=7 exit=101 message=first line second line  third"
        );
    }

    #[test]
    fn task_panic_marker_survives_a_non_string_payload() {
        // `panic_any(42)` carries no string. The marker must still be emitted:
        // an unreadable reason is not a reason to go back to hanging.
        let line =
            format_task_panic_marker(Pid::from_raw(9), &42u32 as &(dyn std::any::Any + Send));
        assert_eq!(
            line,
            "HERMIT_TASK_PANIC tid=9 exit=101 message=<non-string panic payload>"
        );
    }
}
