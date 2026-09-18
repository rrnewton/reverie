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
use std::path::Path;
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

const X32_SYSCALL_BIT: u64 = 0x4000_0000;
const UFFD_IOCTL_TYPE: usize = 0xaa;
const PR_SET_MM: usize = 35;
const PR_SET_MM_START_BRK: usize = 6;
const PR_SET_MM_BRK: usize = 7;
const PR_SET_MM_MAP: usize = 14;
use crate::stack::GuestStack;
use crate::timer::HandleFailure;
use crate::timer::PrivateExecutionTimerSuspension;
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

#[cfg(target_arch = "x86_64")]
fn validate_liteinst_installed_user_regs_update(
    current: &libc::user_regs_struct,
    requested: &libc::user_regs_struct,
) -> Result<(), Errno> {
    // The stopped tracee's native register file can represent a Tool-directed
    // control-flow change, unlike the compact injected-syscall frame. Keep all
    // of that frame's other native-syscall identity restrictions exact.
    let mut identity_view = *requested;
    identity_view.rip = current.rip;
    InjectedSyscallFrame::validate_user_regs_update(current, &identity_view)
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstToolProgramCounterAction {
    RestoreGenerated,
    Deopt,
}

#[cfg(target_arch = "x86_64")]
fn liteinst_tool_program_counter_action(
    observed_rip: u64,
    expected_logical_rip: u64,
) -> LiteinstToolProgramCounterAction {
    if observed_rip == expected_logical_rip {
        LiteinstToolProgramCounterAction::RestoreGenerated
    } else {
        LiteinstToolProgramCounterAction::Deopt
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnmatchedSigtrapDisposition {
    SoftwareBreakpoint(u64),
    GdbStep,
    Deliver,
}

fn unmatched_sigtrap_disposition(
    software_breakpoint: Option<u64>,
    resumed_by_gdb_step: bool,
) -> UnmatchedSigtrapDisposition {
    if let Some(address) = software_breakpoint {
        UnmatchedSigtrapDisposition::SoftwareBreakpoint(address)
    } else if resumed_by_gdb_step {
        UnmatchedSigtrapDisposition::GdbStep
    } else {
        UnmatchedSigtrapDisposition::Deliver
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
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

/// Device and inode in the identity domain reported by `/proc/<pid>/maps`.
///
/// This is deliberately not comparable with `stat.st_dev`/`stat.st_ino`.
/// Linux filesystems such as Btrfs may report different device numbers for
/// those two interfaces even when they describe the same mapped file.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MappingIdentity {
    device_major: u64,
    device_minor: u64,
    inode: u64,
}

impl MappingIdentity {
    fn from_target_loader(identity: (u64, u64, u64)) -> Self {
        Self {
            device_major: identity.0,
            device_minor: identity.1,
            inode: identity.2,
        }
    }

    fn as_target_loader(self) -> (u64, u64, u64) {
        (self.device_major, self.device_minor, self.inode)
    }
}

impl GuestMap {
    fn contains(&self, address: u64) -> bool {
        self.start <= address && address < self.end
    }

    fn contains_range(&self, range: GuestRange) -> bool {
        self.start <= range.start && range.end <= self.end
    }

    fn mapping_identity(&self) -> MappingIdentity {
        MappingIdentity {
            device_major: self.device_major,
            device_minor: self.device_minor,
            inode: self.inode,
        }
    }
}

const MAX_GUEST_MAPS_BYTES: usize = 2 * 1024 * 1024;
const MAX_GUEST_SMAPS_BYTES: usize = 64 * 1024 * 1024;

fn read_proc_bounded(path: impl AsRef<Path>, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let amount = file.read(&mut chunk).ok()?;
        if amount == 0 {
            return Some(bytes);
        }
        let required = bytes.len().checked_add(amount)?;
        if required > limit {
            return None;
        }
        if required > bytes.capacity() {
            let target = bytes
                .capacity()
                .max(chunk.len())
                .checked_mul(2)?
                .max(required)
                .min(limit);
            bytes.try_reserve_exact(target - bytes.len()).ok()?;
        }
        bytes.extend_from_slice(&chunk[..amount]);
    }
}

fn guest_maps(pid: Pid) -> Option<Vec<GuestMap>> {
    let maps = read_proc_bounded(format!("/proc/{pid}/maps"), MAX_GUEST_MAPS_BYTES)?;
    parse_guest_maps_snapshot(&maps)
}

fn parse_guest_maps_snapshot(maps: &[u8]) -> Option<Vec<GuestMap>> {
    maps.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(parse_guest_map)
        .collect()
}

fn guest_start_program_break(pid: Pid) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(44)?
        .parse::<u64>()
        .ok()
        .filter(|address| *address != 0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GuestHookMappingAttributes {
    fork_safe: bool,
    protection_key: u64,
}

fn guest_hook_mapping_attributes(
    pid: Pid,
    target: &GuestMap,
) -> Option<GuestHookMappingAttributes> {
    let smaps = read_proc_bounded(format!("/proc/{pid}/smaps"), MAX_GUEST_SMAPS_BYTES)?;
    parse_guest_hook_mapping_attributes(&smaps, target)
}

fn parse_guest_hook_mapping_attributes(
    smaps: &[u8],
    target: &GuestMap,
) -> Option<GuestHookMappingAttributes> {
    let mut selected = false;
    let mut target_seen = false;
    let mut protection_key = None;
    let mut result = None;
    for line in smaps.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(mapping) = parse_guest_map(line) {
            selected = mapping.start == target.start && mapping.end == target.end;
            if selected {
                if target_seen {
                    return None;
                }
                target_seen = true;
            }
            protection_key = None;
            continue;
        }
        let mut fields = line.split(|byte| byte.is_ascii_whitespace());
        let first = fields.find(|field| !field.is_empty());
        let second = fields.find(|field| !field.is_empty());
        let permissions = second.is_some_and(|field| {
            field.len() == 4
                && matches!(field[0], b'r' | b'-')
                && matches!(field[1], b'w' | b'-')
                && matches!(field[2], b'x' | b'-')
                && matches!(field[3], b'p' | b's')
        });
        if !first.is_some_and(|field| field.ends_with(b":")) || permissions {
            return None;
        }
        if selected && line.starts_with(b"ProtectionKey:") {
            if protection_key.is_some() || result.is_some() {
                return None;
            }
            protection_key = Some(
                std::str::from_utf8(&line[b"ProtectionKey:".len()..])
                    .ok()?
                    .trim()
                    .parse()
                    .ok()?,
            );
            continue;
        }
        if selected && line.starts_with(b"VmFlags:") {
            if result.is_some() {
                return None;
            }
            // `ht` mappings need VMA-specific huge-page geometry; neither the
            // signal observer nor the controller's base-page guard may patch
            // them until that geometry is bound explicitly.
            let fork_safe = !line[8..]
                .split(u8::is_ascii_whitespace)
                .any(|flag| matches!(flag, b"dc" | b"wf" | b"ht"));
            result = Some(GuestHookMappingAttributes {
                fork_safe,
                protection_key: protection_key?,
            });
        }
    }
    result
}

fn guest_mapping_allows_forked_hook(pid: Pid, target: &GuestMap) -> bool {
    guest_hook_mapping_attributes(pid, target)
        .is_some_and(|attributes| attributes.fork_safe && attributes.protection_key == 0)
}

fn bind_prepared_liteinst_arenas(
    baseline: &[GuestMap],
    current: &[GuestMap],
) -> Option<(Vec<PreparedArenaFootprint>, Vec<GuestRange>)> {
    let arena_path = PathBuf::from("/memfd:liteinst2-trampoline (deleted)");
    let new_maps = current
        .iter()
        .filter(|mapping| !baseline.contains(mapping))
        .collect::<Vec<_>>();
    let candidates = new_maps
        .iter()
        .copied()
        .filter(|mapping| mapping.path.as_ref() == Some(&arena_path))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    let mut identities = BTreeMap::<MappingIdentity, Vec<&GuestMap>>::new();
    for mapping in candidates {
        if mapping.end.checked_sub(mapping.start) != Some(LITEINST_ARENA_BYTES)
            || mapping.offset != 0
            || mapping.inode == 0
            || !mapping.readable
            || !mapping.shared
            || mapping.writable == mapping.executable
        {
            return None;
        }
        identities
            .entry(mapping.mapping_identity())
            .or_default()
            .push(mapping);
    }

    let mut occupied = Vec::new();
    let mut arenas = Vec::with_capacity(identities.len());
    for mappings in identities.into_values() {
        if mappings.len() != 2 {
            return None;
        }
        let writable = mappings
            .iter()
            .find(|mapping| mapping.writable && !mapping.executable)?;
        let executable = mappings
            .iter()
            .find(|mapping| !mapping.writable && mapping.executable)?;
        let writable = GuestRange::new(writable.start, LITEINST_ARENA_BYTES)?;
        let executable = GuestRange::new(executable.start, LITEINST_ARENA_BYTES)?;
        if writable.overlaps(executable)
            || occupied
                .iter()
                .any(|range: &GuestRange| range.overlaps(writable) || range.overlaps(executable))
        {
            return None;
        }
        occupied.extend([writable, executable]);
        arenas.push(PreparedArenaFootprint {
            writable,
            executable,
        });
    }
    let page_size = host_page_size().ok()?;
    let reservations = new_maps
        .iter()
        .copied()
        .filter(|mapping| {
            mapping.end.checked_sub(mapping.start) == Some(page_size)
                && mapping.offset == 0
                && mapping.device_major == 0
                && mapping.device_minor == 1
                && mapping.inode != 0
                && mapping.readable
                && mapping.writable
                && !mapping.executable
                && mapping.shared
                && mapping.path.as_deref() == Some(std::path::Path::new("/dev/zero (deleted)"))
        })
        .map(|mapping| GuestRange::new(mapping.start, page_size))
        .collect::<Option<Vec<_>>>()?;
    if reservations.len() != arenas.len()
        || new_maps.len() != arenas.len().checked_mul(3)?
        || reservations.iter().enumerate().any(|(index, reservation)| {
            reservations[..index]
                .iter()
                .any(|prior| prior.overlaps(*reservation))
                || occupied.iter().any(|range| range.overlaps(*reservation))
        })
    {
        return None;
    }
    Some((arenas, reservations))
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
    if end.contains('-') {
        return None;
    }
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    if start >= end {
        return None;
    }
    let offset = u64::from_str_radix(offset, 16).ok()?;
    let (device_major, device_minor) = device.split_once(':')?;
    if device_minor.contains(':') {
        return None;
    }
    let device_major = u64::from_str_radix(device_major, 16).ok()?;
    let device_minor = u64::from_str_radix(device_minor, 16).ok()?;
    let inode = inode.parse::<u64>().ok()?;
    if permissions.len() != 4
        || !matches!(permissions[0], b'r' | b'-')
        || !matches!(permissions[1], b'w' | b'-')
        || !matches!(permissions[2], b'x' | b'-')
        || !matches!(permissions[3], b'p' | b's')
    {
        return None;
    }

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

    fn arm_with(
        &self,
        task: &Stopped,
        event: &Event,
        on_committed: impl FnOnce(),
    ) -> Result<(), TraceError> {
        if task.pid() != self.task_tid {
            return Err(Errno::EINVAL.into());
        }
        HeldRootStop::arm_empty_with(&self.held_task_stops, task, event, on_committed)
    }

    fn ensure_with(
        &self,
        task: &Stopped,
        event: &Event,
        on_committed: impl FnOnce(),
    ) -> Result<(), TraceError> {
        if task.pid() != self.task_tid {
            return Err(Errno::EINVAL.into());
        }
        HeldRootStop::ensure_current_with(&self.held_task_stops, task, event, on_committed)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct LiteinstHandshakeFrame {
    version: u64,
    begin_rip: u64,
    ready_rip: u64,
    install_helper: u64,
    install_helper_rip: u64,
    install_helper_page_start: u64,
    install_helper_page_len: u64,
    helper_stack_top: u64,
    helper_return: u64,
    helper_return_rip: u64,
    syscall_trap_rip: u64,
    syscall_trap_return_rip: u64,
    install_request: u64,
    install_result: u64,
    start_program_break: u64,
    initial_program_break: u64,
}

fn same_liteinst_handshake_protocol(
    left: LiteinstHandshakeFrame,
    right: LiteinstHandshakeFrame,
) -> bool {
    LiteinstHandshakeFrame {
        initial_program_break: 0,
        ..left
    } == LiteinstHandshakeFrame {
        initial_program_break: 0,
        ..right
    }
}

const LITEINST_INSTALL_REQUEST_VERSION: u64 = 1;
const LITEINST_INSTALL_SOURCE_BYTES: usize = 64;
const LITEINST_PATCH_WORD_BYTES: u64 = 8;
const LITEINST_INSTALL_PC_MAPPINGS: usize = 16;
const LITEINST_MAX_TRAMPOLINE_CODE_BYTES: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
struct LiteinstInstallRequest {
    version: u64,
    site_start: u64,
    mapping_end: u64,
    source_len: u64,
    source: [u8; LITEINST_INSTALL_SOURCE_BYTES],
}

impl Default for LiteinstInstallRequest {
    fn default() -> Self {
        Self {
            version: 0,
            site_start: 0,
            mapping_end: 0,
            source_len: 0,
            source: [0; LITEINST_INSTALL_SOURCE_BYTES],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct LiteinstProgramCounterMapping {
    generated_start: u64,
    generated_end: u64,
    logical_address: u64,
}

impl LiteinstProgramCounterMapping {
    fn translate(self, program_counter: u64) -> Option<u64> {
        (self.generated_start <= program_counter && program_counter < self.generated_end)
            .then_some(self.logical_address)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
struct LiteinstInstallResult {
    version: u64,
    site_start: u64,
    site_len: u64,
    ptrace_entry_stop_rip: u64,
    ptrace_completion_stop_rip: u64,
    relocated_tail: u64,
    trampoline_start: u64,
    trampoline_len: u64,
    trampoline_code_len: u64,
    arena_writable_start: u64,
    arena_writable_len: u64,
    arena_executable_start: u64,
    arena_executable_len: u64,
    instruction_len: u64,
    straddle_prefix: u64,
    program_counter_count: u64,
    program_counters: [LiteinstProgramCounterMapping; LITEINST_INSTALL_PC_MAPPINGS],
    complete: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiteinstPatchWordSnapshot {
    site: GuestRange,
    bytes: [u8; LITEINST_PATCH_WORD_BYTES as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiteinstDeoptPatchWord {
    site: GuestRange,
    patched: [u8; LITEINST_PATCH_WORD_BYTES as usize],
    original: [u8; LITEINST_PATCH_WORD_BYTES as usize],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstDeoptPatchTransition {
    RestoreOriginal,
    RepatchIfRestored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstDeoptPatchFailure<E> {
    Forward(E),
    Rollback,
}

fn rollback_liteinst_deopt_patch_words<E>(
    words: &[LiteinstDeoptPatchWord],
    transition: &mut impl FnMut(
        &LiteinstDeoptPatchWord,
        LiteinstDeoptPatchTransition,
    ) -> Result<(), E>,
) -> bool {
    let mut succeeded = true;
    for word in words.iter().rev() {
        if transition(word, LiteinstDeoptPatchTransition::RepatchIfRestored).is_err() {
            succeeded = false;
        }
    }
    succeeded
}

fn transition_liteinst_deopt_patch_words<E>(
    words: &[LiteinstDeoptPatchWord],
    mut transition: impl FnMut(
        &LiteinstDeoptPatchWord,
        LiteinstDeoptPatchTransition,
    ) -> Result<(), E>,
) -> Result<(), LiteinstDeoptPatchFailure<E>> {
    for word in words {
        if let Err(error) = transition(word, LiteinstDeoptPatchTransition::RestoreOriginal) {
            return if rollback_liteinst_deopt_patch_words(words, &mut transition) {
                Err(LiteinstDeoptPatchFailure::Forward(error))
            } else {
                Err(LiteinstDeoptPatchFailure::Rollback)
            };
        }
    }
    Ok(())
}

fn resolve_liteinst_deopt_patch_result<E: From<Errno>>(
    result: Result<(), LiteinstDeoptPatchFailure<E>>,
) -> Result<(), E> {
    match result {
        Ok(()) => Ok(()),
        Err(LiteinstDeoptPatchFailure::Forward(error)) => Err(error),
        Err(LiteinstDeoptPatchFailure::Rollback) => Err(Errno::EIO.into()),
    }
}

fn commit_liteinst_deopt_state(
    state: &mut LiteinstRuntimeState,
    generation: u64,
    active_hooks: &HashMap<u64, ActiveHookFootprint>,
    hooks: &[ActiveHookFootprint],
) -> Result<(), ()> {
    if state.generation != generation || state.active_hooks != *active_hooks {
        return Err(());
    }
    state.active_hooks.clear();
    for hook in hooks {
        state.attempted_sites.insert(hook.site.start);
        state
            .fallback_sites
            .insert(hook.site.start, LiteinstRetainedFallback::Deoptimized);
    }
    Ok(())
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct LiteinstInstallHelperArm {
    tid: Pid,
    generation: u64,
    origin_status: u64,
    entry: u64,
    entry_rip: u64,
    site: u64,
    stack_pointer: u64,
    return_address: u64,
    request_address: u64,
    request: LiteinstInstallRequest,
    code_mapping: GuestMap,
    helper_code: LiteinstHelperCode,
    entry_bytes: [u8; 16],
}

fn physical_status_advanced(origin: u64, observed: Option<u64>) -> bool {
    observed.is_some_and(|observed| observed > origin)
}

#[cfg(target_arch = "x86_64")]
fn liteinst_helper_entry_scalars_match(
    arm: &LiteinstInstallHelperArm,
    tid: Pid,
    generation: u64,
    observed_status: Option<u64>,
    regs: &libc::user_regs_struct,
    si_code: i32,
    entry_opcode: u8,
    request: LiteinstInstallRequest,
) -> bool {
    arm.tid == tid
        && arm.generation == generation
        && physical_status_advanced(arm.origin_status, observed_status)
        && regs.rip == arm.entry_rip
        && regs.rdi == arm.site
        && regs.rsp == arm.stack_pointer
        && regs.orig_rax == u64::MAX
        && matches!(si_code, libc::TRAP_BRKPT | libc::SI_KERNEL)
        && entry_opcode == 0xcc
        && request == arm.request
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

fn kernel_mapping_effect_range(
    start: u64,
    len: u64,
    page_size: u64,
) -> Result<Option<GuestRange>, ()> {
    if page_size == 0 || !page_size.is_power_of_two() {
        return Err(());
    }
    if !start.is_multiple_of(page_size) {
        return Err(());
    }
    if len == 0 {
        return Ok(None);
    }

    let end = start.checked_add(len).ok_or(())?;
    let page_mask = page_size - 1;
    let end = end.checked_add(page_mask).ok_or(())? & !page_mask;
    Ok(Some(GuestRange { start, end }))
}

fn remap_file_pages_effect_range(
    args: SyscallArgs,
    page_size: u64,
) -> Result<Option<GuestRange>, ()> {
    if page_size == 0 || !page_size.is_power_of_two() || args.arg2 != 0 || args.arg4 != 0 {
        return Err(());
    }
    let start = args.arg0 as u64 & !(page_size - 1);
    let len = args.arg1 as u64 & !(page_size - 1);
    let end = start
        .checked_add(len)
        .filter(|end| *end > start)
        .ok_or(())?;
    let pages = len / page_size;
    (args.arg3 as u64).checked_add(pages).ok_or(())?;
    Ok(Some(GuestRange { start, end }))
}

fn page_ceil_u64(value: u64, page_size: u64) -> Result<u64, ()> {
    value
        .checked_add(page_size.checked_sub(1).ok_or(())?)
        .map(|value| value & !(page_size - 1))
        .ok_or(())
}

fn brk_shrink_effect_range(
    start_break: u64,
    current_break: u64,
    requested_break: u64,
    page_size: u64,
) -> Result<Option<GuestRange>, ()> {
    if page_size == 0
        || !page_size.is_power_of_two()
        || start_break == 0
        || current_break < start_break
    {
        return Err(());
    }
    if requested_break == 0 || requested_break < start_break || requested_break >= current_break {
        return Ok(None);
    }
    let start = page_ceil_u64(requested_break, page_size)?;
    let end = page_ceil_u64(current_break, page_size)?;
    Ok((start < end).then_some(GuestRange { start, end }))
}

fn madvise_preserves_liteinst_generation(advice: usize) -> bool {
    matches!(
        advice as u32,
        0 | 1 | 2 | 3 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 19 | 20 | 21 | 22 | 23 | 25
    )
}

fn kernel_page_covering_range(
    start: u64,
    len: u64,
    page_size: u64,
) -> Result<Option<GuestRange>, ()> {
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

fn snapshot_liteinst_install_request(
    task: &Stopped,
    site: u64,
    page_size: u64,
) -> Option<(LiteinstInstallRequest, GuestRange, GuestMap)> {
    let patch = GuestRange::new(site, LITEINST_PATCH_WORD_BYTES)?;
    let maps = guest_maps(task.pid())?;
    let mapping = maps.iter().find(|mapping| {
        mapping.readable
            && !mapping.writable
            && mapping.executable
            && !mapping.shared
            && mapping.contains_range(patch)
    })?;
    let attributes = guest_hook_mapping_attributes(task.pid(), mapping)?;
    if !attributes.fork_safe || attributes.protection_key != 0 {
        return None;
    }
    let available = usize::try_from(mapping.end.checked_sub(site)?)
        .ok()?
        .min(LITEINST_INSTALL_SOURCE_BYTES);
    if available < LITEINST_PATCH_WORD_BYTES as usize {
        return None;
    }
    let mut source = [0_u8; LITEINST_INSTALL_SOURCE_BYTES];
    task.read_exact(site as usize, &mut source[..available])
        .ok()?;
    if source[..2] != [0x0f, 0x05] {
        return None;
    }
    let protection =
        kernel_page_covering_range(site, LITEINST_PATCH_WORD_BYTES, page_size).ok()??;
    Some((
        LiteinstInstallRequest {
            version: LITEINST_INSTALL_REQUEST_VERSION,
            site_start: site,
            mapping_end: mapping.end,
            source_len: available as u64,
            source,
        },
        protection,
        mapping.clone(),
    ))
}

fn liteinst_temporary_source_is_rwx(
    task: &Stopped,
    original: &GuestMap,
    protection: GuestRange,
) -> bool {
    guest_maps(task.pid()).is_some_and(|maps| {
        maps.iter().any(|mapping| {
            let expected_offset = original
                .offset
                .checked_add(protection.start.saturating_sub(original.start));
            let actual_offset = mapping
                .offset
                .checked_add(protection.start.saturating_sub(mapping.start));
            mapping.readable
                && mapping.writable
                && mapping.executable
                && !mapping.shared
                && mapping.contains_range(protection)
                && mapping.device_major == original.device_major
                && mapping.device_minor == original.device_minor
                && mapping.inode == original.inode
                && mapping.path == original.path
                && actual_offset == expected_offset
                && guest_hook_mapping_attributes(task.pid(), mapping).is_some_and(|attributes| {
                    attributes.fork_safe && attributes.protection_key == 0
                })
        })
    })
}

fn liteinst_source_is_exactly_restored(task: &Stopped, original: &GuestMap) -> bool {
    guest_maps(task.pid()).is_some_and(|maps| {
        maps.iter().any(|mapping| mapping == original)
            && guest_hook_mapping_attributes(task.pid(), original)
                .is_some_and(|attributes| attributes.fork_safe && attributes.protection_key == 0)
    })
}

fn bind_liteinst_helper_code(
    task: &Stopped,
    frame: LiteinstHandshakeFrame,
) -> Option<LiteinstHelperCode> {
    let range = GuestRange::new(
        frame.install_helper_page_start,
        frame.install_helper_page_len,
    )?;
    let maps = guest_maps(task.pid())?;
    let original_mapping = maps.into_iter().find(|mapping| {
        mapping.readable
            && !mapping.writable
            && mapping.executable
            && !mapping.shared
            && mapping.contains_range(range)
    })?;
    guest_hook_mapping_attributes(task.pid(), &original_mapping)
        .filter(|attributes| attributes.fork_safe && attributes.protection_key == 0)?;
    let length = usize::try_from(range.end.checked_sub(range.start)?).ok()?;
    let address = usize::try_from(range.start).ok()?;
    let mut bytes = vec![0; length];
    read_stopped_ptrace_words(task, address, &mut bytes).then_some(())?;
    Some(LiteinstHelperCode {
        range,
        original_mapping,
        bytes,
    })
}

fn read_stopped_ptrace_words(task: &Stopped, address: usize, bytes: &mut [u8]) -> bool {
    if !bytes.len().is_multiple_of(core::mem::size_of::<u64>()) {
        return false;
    }
    for (index, chunk) in bytes
        .chunks_exact_mut(core::mem::size_of::<u64>())
        .enumerate()
    {
        let Some(address) = address
            .checked_add(index * core::mem::size_of::<u64>())
            .and_then(Addr::<u64>::from_raw)
        else {
            return false;
        };
        let Ok(word): Result<u64, _> = task.read_value(address) else {
            return false;
        };
        chunk.copy_from_slice(&word.to_ne_bytes());
    }
    true
}

fn liteinst_helper_code_bytes_match(task: &Stopped, helper: &LiteinstHelperCode) -> bool {
    let Ok(address) = usize::try_from(helper.range.start) else {
        return false;
    };
    let mut bytes = vec![0; helper.bytes.len()];
    read_stopped_ptrace_words(task, address, &mut bytes) && bytes == helper.bytes
}

fn liteinst_helper_code_has_protection(
    task: &Stopped,
    helper: &LiteinstHelperCode,
    expected_protection: i32,
) -> bool {
    guest_maps(task.pid()).is_some_and(|maps| {
        maps.iter().any(|mapping| {
            let Some(original_delta) = helper
                .range
                .start
                .checked_sub(helper.original_mapping.start)
            else {
                return false;
            };
            let Some(actual_delta) = helper.range.start.checked_sub(mapping.start) else {
                return false;
            };
            let expected_offset = helper.original_mapping.offset.checked_add(original_delta);
            let actual_offset = mapping.offset.checked_add(actual_delta);
            (if expected_protection == libc::PROT_NONE {
                mapping.start == helper.range.start && mapping.end == helper.range.end
            } else {
                mapping.contains_range(helper.range)
            }) && mapping.readable == (expected_protection & libc::PROT_READ != 0)
                && mapping.writable == (expected_protection & libc::PROT_WRITE != 0)
                && mapping.executable == (expected_protection & libc::PROT_EXEC != 0)
                && !mapping.shared
                && mapping.device_major == helper.original_mapping.device_major
                && mapping.device_minor == helper.original_mapping.device_minor
                && mapping.inode == helper.original_mapping.inode
                && mapping.path == helper.original_mapping.path
                && actual_offset == expected_offset
                && guest_hook_mapping_attributes(task.pid(), mapping).is_some_and(|attributes| {
                    attributes.fork_safe && attributes.protection_key == 0
                })
        })
    })
}

fn liteinst_arena_alias_has_protection(
    task: &Stopped,
    range: GuestRange,
    expected_protection: i32,
) -> bool {
    guest_maps(task.pid()).is_some_and(|maps| {
        maps.iter().any(|mapping| {
            mapping.start == range.start
                && mapping.end == range.end
                && mapping.offset == 0
                && mapping.inode != 0
                && mapping.shared
                && mapping.readable == (expected_protection & libc::PROT_READ != 0)
                && mapping.writable == (expected_protection & libc::PROT_WRITE != 0)
                && mapping.executable == (expected_protection & libc::PROT_EXEC != 0)
        })
    })
}

fn liteinst_trampoline_code_bytes_match(task: &Stopped, hook: &ActiveHookFootprint) -> bool {
    let Ok(length) = usize::try_from(hook.trampoline_code.end - hook.trampoline_code.start) else {
        return false;
    };
    if length != hook.trampoline_code_bytes.len() {
        return false;
    }
    let mut observed = vec![0_u8; length];
    task.read_exact(hook.trampoline_code.start as usize, &mut observed)
        .is_ok()
        && observed == hook.trampoline_code_bytes
}

fn liteinst_active_trampoline_bytes_match(
    task: &Stopped,
    hooks: &HashMap<u64, ActiveHookFootprint>,
) -> bool {
    hooks
        .values()
        .all(|hook| liteinst_trampoline_code_bytes_match(task, hook))
}

fn expected_liteinst_patch_word(
    request: &LiteinstInstallRequest,
    result: &LiteinstInstallResult,
) -> Option<[u8; LITEINST_PATCH_WORD_BYTES as usize]> {
    if request.source_len < LITEINST_PATCH_WORD_BYTES
        || result.site_start != request.site_start
        || result.site_len != LITEINST_PATCH_WORD_BYTES
    {
        return None;
    }
    let next = request.site_start.checked_add(5)?;
    let displacement = i32::try_from(result.trampoline_start as i128 - next as i128).ok()?;
    let mut expected = [0_u8; LITEINST_PATCH_WORD_BYTES as usize];
    expected[0] = 0xe9;
    expected[1..5].copy_from_slice(&displacement.to_le_bytes());
    expected[5..].copy_from_slice(&request.source[5..LITEINST_PATCH_WORD_BYTES as usize]);
    Some(expected)
}

#[cfg(target_arch = "x86_64")]
fn x86_near_jump_target(instruction_pointer: u64, instruction: [u8; 5]) -> Option<u64> {
    if instruction[0] != 0xe9 {
        return None;
    }
    let displacement = i32::from_le_bytes(instruction[1..].try_into().ok()?);
    let next = instruction_pointer.checked_add(instruction.len() as u64)?;
    u64::try_from(next as i128 + displacement as i128).ok()
}

fn liteinst_install_result_matches_return(raw_result: i64, result: &LiteinstInstallResult) -> bool {
    u64::try_from(raw_result) == Ok(result.relocated_tail)
}

fn plan_liteinst_active_patch_words(
    state: &LiteinstRuntimeState,
    new_site: GuestRange,
    protection: GuestRange,
) -> Option<Vec<LiteinstPatchWordSnapshot>> {
    if !protection.contains(new_site)
        || state
            .prepared_reservations
            .iter()
            .any(|range| range.overlaps(protection))
        || state.prepared_arenas.iter().any(|arena| {
            arena
                .protected_ranges()
                .into_iter()
                .any(|(range, _)| range.overlaps(protection))
        })
    {
        return None;
    }

    let mut snapshots = Vec::new();
    for hook in state.active_hooks.values() {
        if hook.site.overlaps(new_site) {
            return None;
        }
        let mut site_overlaps_page = false;
        for (range, _) in hook.protected_ranges() {
            if !range.overlaps(protection) {
                continue;
            }
            if range != hook.site
                || range.end.checked_sub(range.start) != Some(LITEINST_PATCH_WORD_BYTES)
            {
                return None;
            }
            site_overlaps_page = true;
        }
        if site_overlaps_page {
            snapshots.push(LiteinstPatchWordSnapshot {
                site: hook.site,
                bytes: hook.expected_site_word,
            });
        }
    }
    snapshots.sort_unstable_by_key(|snapshot| snapshot.site.start);
    if snapshots
        .windows(2)
        .any(|pair| pair[0].site.overlaps(pair[1].site))
    {
        return None;
    }
    Some(snapshots)
}

fn snapshot_liteinst_active_patch_words(
    task: &Stopped,
    state: &LiteinstRuntimeState,
    new_site: GuestRange,
    protection: GuestRange,
) -> Option<Vec<LiteinstPatchWordSnapshot>> {
    let snapshots = plan_liteinst_active_patch_words(state, new_site, protection)?;
    liteinst_patch_words_match(task, &snapshots).then_some(snapshots)
}

fn liteinst_patch_words_match(task: &Stopped, snapshots: &[LiteinstPatchWordSnapshot]) -> bool {
    snapshots.iter().all(|snapshot| {
        let mut bytes = [0_u8; LITEINST_PATCH_WORD_BYTES as usize];
        usize::try_from(snapshot.site.start)
            .is_ok_and(|address| task.read_exact(address, &mut bytes).is_ok())
            && bytes == snapshot.bytes
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MremapEffectRanges {
    source: GuestRange,
    destination: Option<GuestRange>,
    clones_shared_mapping: bool,
}

fn mremap_effect_ranges(args: SyscallArgs, page_size: u64) -> Result<MremapEffectRanges, ()> {
    let flags = args.arg3;
    let may_move = flags & libc::MREMAP_MAYMOVE as usize != 0;
    let fixed = flags & libc::MREMAP_FIXED as usize != 0;
    let dont_unmap = flags & libc::MREMAP_DONTUNMAP as usize != 0;
    let allowed_flags =
        (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED | libc::MREMAP_DONTUNMAP) as usize;
    if flags & !allowed_flags != 0 || (fixed && !may_move) || (dont_unmap && !may_move) {
        return Err(());
    }
    let new_at_source =
        kernel_mapping_effect_range(args.arg0 as u64, args.arg2 as u64, page_size)?.ok_or(())?;
    let clones_shared_mapping = args.arg1 == 0;
    if clones_shared_mapping && (!may_move || dont_unmap) {
        return Err(());
    }
    let source = if clones_shared_mapping {
        new_at_source
    } else {
        kernel_mapping_effect_range(args.arg0 as u64, args.arg1 as u64, page_size)?.ok_or(())?
    };
    if dont_unmap && source.end - source.start != new_at_source.end - new_at_source.start {
        return Err(());
    }
    let requested_destination = if fixed || dont_unmap {
        Some(kernel_mapping_effect_range(args.arg4 as u64, args.arg2 as u64, page_size)?.ok_or(())?)
    } else {
        None
    };
    if !clones_shared_mapping && requested_destination.is_some_and(|range| range.overlaps(source)) {
        return Err(());
    }
    let destination = fixed.then_some(requested_destination).flatten();
    Ok(MremapEffectRanges {
        source,
        destination,
        clones_shared_mapping,
    })
}

fn host_page_size() -> Result<u64, Errno> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).map_err(|_| Errno::EIO)?;
    page_size
        .is_power_of_two()
        .then_some(page_size)
        .ok_or(Errno::EIO)
}

fn process_has_exactly_one_task(pid: Pid) -> bool {
    let expected = std::ffi::OsString::from(pid.as_raw().to_string());
    for _ in 0..2 {
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            return false;
        };
        let mut seen = false;
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            if seen || entry.file_name() != expected {
                return false;
            }
            seen = true;
        }
        if !seen {
            return false;
        }
    }
    true
}

fn is_liteinst_mapping_syscall(nr: Sysno, args: SyscallArgs) -> bool {
    // TODO-HUMAN-REVIEW(PR-270): Review pkey_mprotect mapping-lifecycle classification.
    matches!(
        nr,
        // AUTONOMOUS-BOT-IMPLEMENTED
        Sysno::mmap
            | Sysno::munmap
            | Sysno::mremap
            | Sysno::mprotect
            | Sysno::pkey_mprotect
            | Sysno::madvise
            | Sysno::remap_file_pages
            | Sysno::process_madvise
            | Sysno::shmat
            | Sysno::shmdt
            | Sysno::brk
            | Sysno::io_uring_setup
            | Sysno::io_uring_enter
            | Sysno::io_uring_register
            | Sysno::userfaultfd
    ) || (nr == Sysno::ioctl && args.arg1 >> 8 & 0xff == UFFD_IOCTL_TYPE)
        || (nr == Sysno::prctl
            && args.arg0 as u32 as usize == PR_SET_MM
            && matches!(
                args.arg1 as u32 as usize,
                PR_SET_MM_START_BRK | PR_SET_MM_BRK | PR_SET_MM_MAP
            ))
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
    original_site_word: [u8; LITEINST_PATCH_WORD_BYTES as usize],
    expected_site_word: [u8; LITEINST_PATCH_WORD_BYTES as usize],
    trampoline: GuestRange,
    trampoline_code: GuestRange,
    trampoline_code_bytes: Vec<u8>,
    ptrace_entry_stop_rip: u64,
    ptrace_completion_stop_rip: u64,
    relocated_tail: u64,
    program_counters: Vec<LiteinstProgramCounterMapping>,
    arena_writable: GuestRange,
    arena_executable: GuestRange,
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstInstalledPrivatePhase {
    AwaitingRuntimeTrap,
    AwaitingCompletion { runtime_status: u64 },
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct LiteinstStopResolutionSnapshot {
    delivery_logical_stop: safeptrace::LogicalStopId,
    delivery_status: u64,
    registers: libc::user_regs_struct,
    xstate: safeptrace::XState,
    private_sigmask: safeptrace::PtraceSigmask,
}

#[cfg(target_arch = "x86_64")]
#[derive(Debug)]
enum LiteinstInstalledEventPhase {
    AwaitingRuntimeTrap,
    AwaitingCompletion { runtime_status: u64 },
    AwaitStopResolution {
        prior: LiteinstInstalledPrivatePhase,
        snapshot: LiteinstStopResolutionSnapshot,
    },
}

#[cfg(target_arch = "x86_64")]
impl LiteinstInstalledEventPhase {
    fn private(&self) -> Option<LiteinstInstalledPrivatePhase> {
        match self {
            Self::AwaitingRuntimeTrap => Some(LiteinstInstalledPrivatePhase::AwaitingRuntimeTrap),
            Self::AwaitingCompletion { runtime_status } => {
                Some(LiteinstInstalledPrivatePhase::AwaitingCompletion {
                    runtime_status: *runtime_status,
                })
            }
            Self::AwaitStopResolution { .. } => None,
        }
    }

    fn restore(private: LiteinstInstalledPrivatePhase) -> Self {
        match private {
            LiteinstInstalledPrivatePhase::AwaitingRuntimeTrap => Self::AwaitingRuntimeTrap,
            LiteinstInstalledPrivatePhase::AwaitingCompletion { runtime_status } => {
                Self::AwaitingCompletion { runtime_status }
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
struct LiteinstInstalledEvent {
    generation: u64,
    physical_generation: safeptrace::PhysicalEventGenerationId,
    entry_status: u64,
    status_floor: u64,
    logical_stop_floor: safeptrace::LogicalStopId,
    footprint: ActiveHookFootprint,
    entry_regs: libc::user_regs_struct,
    entry_xstate: safeptrace::XState,
    original_sigmask: safeptrace::PtraceSigmask,
    private_sigmask: safeptrace::PtraceSigmask,
    timer_suspension: Option<PrivateExecutionTimerSuspension>,
    phase: LiteinstInstalledEventPhase,
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct LiteinstEphemeralPcTranslation {
    generation: u64,
    physical_generation: safeptrace::PhysicalEventGenerationId,
    logical_rip: u64,
    generated_rip: u64,
}

const LITEINST_ARENA_BYTES: u64 = 128 * 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
struct PreparedArenaFootprint {
    writable: GuestRange,
    executable: GuestRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LiteinstHelperCode {
    range: GuestRange,
    original_mapping: GuestMap,
    bytes: Vec<u8>,
}

impl PreparedArenaFootprint {
    fn protected_ranges(&self) -> [(GuestRange, i32); 2] {
        [
            (self.writable, libc::PROT_NONE),
            (self.executable, libc::PROT_READ | libc::PROT_EXEC),
        ]
    }
}

impl ActiveHookFootprint {
    fn protected_ranges(&self) -> [(GuestRange, i32); 4] {
        [
            (self.site, libc::PROT_READ | libc::PROT_EXEC),
            (self.trampoline, libc::PROT_READ | libc::PROT_EXEC),
            (self.arena_writable, libc::PROT_NONE),
            (self.arena_executable, libc::PROT_READ | libc::PROT_EXEC),
        ]
    }

    #[cfg(target_arch = "x86_64")]
    fn validates_ptrace_stop(&self, task: &Stopped, rip: u64) -> bool {
        let Some(opcode_address) = rip.checked_sub(1) else {
            return false;
        };
        let Some(opcode_range) = GuestRange::new(opcode_address, 1) else {
            return false;
        };
        if !self.trampoline_code.contains(opcode_range)
            || !self.trampoline.contains(self.trampoline_code)
            || !self.arena_executable.contains(self.trampoline)
        {
            return false;
        }
        let Some(opcode_address) = Addr::from_raw(opcode_address as usize) else {
            return false;
        };
        let Ok(opcode): Result<u8, _> = task.read_value(opcode_address) else {
            return false;
        };
        let mut site_word = [0_u8; LITEINST_PATCH_WORD_BYTES as usize];
        if opcode != 0xcc
            || task
                .read_exact(self.site.start as usize, &mut site_word)
                .is_err()
            || site_word != self.expected_site_word
            || !liteinst_trampoline_code_bytes_match(task, self)
            || !liteinst_arena_alias_has_protection(task, self.arena_writable, libc::PROT_NONE)
        {
            return false;
        }
        guest_maps(task.pid()).is_some_and(|maps| {
            maps.iter().any(|mapping| {
                mapping.start == self.arena_executable.start
                    && mapping.end == self.arena_executable.end
                    && mapping.readable
                    && !mapping.writable
                    && mapping.executable
                    && mapping.shared
                    && mapping.inode != 0
                    && mapping.contains_range(self.trampoline)
            })
        })
    }

    #[cfg(target_arch = "x86_64")]
    fn translate_program_counter(&self, program_counter: u64) -> Option<u64> {
        self.program_counters
            .iter()
            .find_map(|mapping| mapping.translate(program_counter))
    }
}

#[cfg(target_arch = "x86_64")]
fn liteinst_logical_program_counter(
    hooks: &[ActiveHookFootprint],
    generated_rip: u64,
) -> Result<Option<u64>, ()> {
    let mut logical_rip = None;
    for hook in hooks {
        if let Some(translated) = hook.translate_program_counter(generated_rip) {
            if logical_rip.replace(translated).is_some() {
                return Err(());
            }
        } else if hook.trampoline_code.start <= generated_rip
            && generated_rip < hook.trampoline_code.end
        {
            return Err(());
        }
    }
    Ok(logical_rip)
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstDeoptProgramCounter {
    TranslateGenerated,
    PreserveObserved,
}

#[cfg(target_arch = "x86_64")]
fn liteinst_deopt_program_counter(
    hooks: &[ActiveHookFootprint],
    observed_rip: u64,
    mode: LiteinstDeoptProgramCounter,
) -> Result<Option<u64>, ()> {
    match mode {
        LiteinstDeoptProgramCounter::TranslateGenerated => {
            liteinst_logical_program_counter(hooks, observed_rip)
        }
        LiteinstDeoptProgramCounter::PreserveObserved => Ok(None),
    }
}

#[cfg(target_arch = "x86_64")]
fn liteinst_private_sigmask(original: safeptrace::PtraceSigmask) -> safeptrace::PtraceSigmask {
    let mut bytes = original.into_bytes();
    for signal in 1..=64_usize {
        let index = signal - 1;
        bytes[index / u8::BITS as usize] |= 1 << (index % u8::BITS as usize);
    }
    for signal in [libc::SIGKILL as usize, libc::SIGSTOP as usize] {
        let index = signal - 1;
        bytes[index / u8::BITS as usize] &= !(1 << (index % u8::BITS as usize));
    }
    safeptrace::PtraceSigmask::from_bytes(bytes)
}

#[cfg(target_arch = "x86_64")]
fn liteinst_raw_syscall_number_is_x32(raw_number: u64) -> bool {
    raw_number as i64 >= 0 && raw_number & X32_SYSCALL_BIT != 0
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstInstalledSyscallClass {
    X32,
    Unknown,
    RtSigreturn,
    Known(Sysno),
}

#[cfg(target_arch = "x86_64")]
fn classify_liteinst_installed_syscall(raw_number: u64) -> LiteinstInstalledSyscallClass {
    if liteinst_raw_syscall_number_is_x32(raw_number) {
        return LiteinstInstalledSyscallClass::X32;
    }
    match usize::try_from(raw_number).ok().and_then(Sysno::new) {
        Some(Sysno::rt_sigreturn) => LiteinstInstalledSyscallClass::RtSigreturn,
        Some(number) => LiteinstInstalledSyscallClass::Known(number),
        None => LiteinstInstalledSyscallClass::Unknown,
    }
}

#[cfg(target_arch = "x86_64")]
fn liteinst_installed_tool_event_number(raw_number: u64) -> Option<Sysno> {
    match classify_liteinst_installed_syscall(raw_number) {
        LiteinstInstalledSyscallClass::Known(number) => Some(number),
        LiteinstInstalledSyscallClass::X32
        | LiteinstInstalledSyscallClass::Unknown
        | LiteinstInstalledSyscallClass::RtSigreturn => None,
    }
}

#[cfg(target_arch = "x86_64")]
fn liteinst_register_words(registers: &libc::user_regs_struct) -> [u64; 27] {
    [
        registers.r15,
        registers.r14,
        registers.r13,
        registers.r12,
        registers.rbp,
        registers.rbx,
        registers.r11,
        registers.r10,
        registers.r9,
        registers.r8,
        registers.rax,
        registers.rcx,
        registers.rdx,
        registers.rsi,
        registers.rdi,
        registers.orig_rax,
        registers.rip,
        registers.cs,
        registers.eflags,
        registers.rsp,
        registers.ss,
        registers.fs_base,
        registers.gs_base,
        registers.ds,
        registers.es,
        registers.fs,
        registers.gs,
    ]
}

#[cfg(target_arch = "x86_64")]
fn liteinst_completion_registers_match(
    entry: &libc::user_regs_struct,
    completion: &libc::user_regs_struct,
    completion_rip: u64,
) -> bool {
    let mut expected = *entry;
    expected.rip = completion_rip;
    liteinst_register_words(&expected) == liteinst_register_words(completion)
}

#[cfg(target_arch = "x86_64")]
fn liteinst_logical_syscall_entry_registers(
    entry: &libc::user_regs_struct,
    continuation: u64,
) -> libc::user_regs_struct {
    let raw_number = entry.rax;
    let mut logical = *entry;
    logical.rax = (-(Errno::ENOSYS.into_raw() as i64)) as u64;
    logical.orig_rax = raw_number;
    logical.rip = continuation;
    logical.rcx = continuation;
    logical.r11 = entry.eflags;
    logical
}

#[cfg(target_arch = "x86_64")]
fn liteinst_runtime_frame_matches_entry(
    frame: &InjectedSyscallFrame,
    entry: &libc::user_regs_struct,
    site: u64,
) -> bool {
    let Some(continuation) = site.checked_add(cp::SYSCALL_INSTR_SIZE as u64) else {
        return false;
    };
    let view = frame.user_regs(entry.eflags);
    frame.instruction_pointer() == site
        && frame.raw_syscall_number() == entry.rax
        && frame.raw_args()
            == [
                entry.rdi, entry.rsi, entry.rdx, entry.r10, entry.r8, entry.r9,
            ]
        && view.r15 == entry.r15
        && view.r14 == entry.r14
        && view.r13 == entry.r13
        && view.r12 == entry.r12
        && view.rbp == entry.rbp
        && view.rbx == entry.rbx
        && view.r11 == entry.r11
        && view.r10 == entry.r10
        && view.r9 == entry.r9
        && view.r8 == entry.r8
        && view.rax == entry.rax
        && view.rcx == entry.rcx
        && view.rdx == entry.rdx
        && view.rsi == entry.rsi
        && view.rdi == entry.rdi
        && view.orig_rax == entry.rax
        && view.rip == continuation
        && view.eflags == entry.eflags
        && view.rsp == entry.rsp
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
        runtime_mapping: MappingIdentity,
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
            && mapping.mapping_identity() == runtime_mapping
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    fallback_sites: HashMap<u64, LiteinstRetainedFallback>,
    active_hooks: HashMap<u64, ActiveHookFootprint>,
    arena_baseline_maps: Vec<GuestMap>,
    prepared_arenas: Vec<PreparedArenaFootprint>,
    prepared_reservations: Vec<GuestRange>,
    helper_code: Option<LiteinstHelperCode>,
    start_break: Option<u64>,
    current_break: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiteinstRetainedFallback {
    CachelineStraddler,
    UnpatchableOrOther,
    Deoptimized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiteinstEntryGuard {
    address: u64,
    saved_instruction: u64,
}

#[cfg(target_arch = "x86_64")]
#[derive(Debug)]
struct RestoredLiteinstEntryGuard {
    guard: LiteinstEntryGuard,
    authenticated: crate::entry_call::AuthenticatedEntryWord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryWordIo {
    Read,
    Write(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryWordIoStage {
    InitialRead,
    Write,
    Readback,
}

#[derive(Debug, Eq, PartialEq)]
enum EntryWordTransitionError<E> {
    Access { stage: EntryWordIoStage, error: E },
    Expected { expected: u64, observed: u64 },
    Readback { expected: u64, observed: u64 },
}

fn replace_exact_entry_word<E>(
    expected: u64,
    replacement: u64,
    mut access: impl FnMut(EntryWordIo) -> Result<u64, E>,
) -> Result<(), EntryWordTransitionError<E>> {
    let observed = access(EntryWordIo::Read).map_err(|error| EntryWordTransitionError::Access {
        stage: EntryWordIoStage::InitialRead,
        error,
    })?;
    if observed != expected {
        return Err(EntryWordTransitionError::Expected { expected, observed });
    }
    access(EntryWordIo::Write(replacement)).map_err(|error| EntryWordTransitionError::Access {
        stage: EntryWordIoStage::Write,
        error,
    })?;
    let observed = access(EntryWordIo::Read).map_err(|error| EntryWordTransitionError::Access {
        stage: EntryWordIoStage::Readback,
        error,
    })?;
    if observed != replacement {
        return Err(EntryWordTransitionError::Readback {
            expected: replacement,
            observed,
        });
    }
    Ok(())
}

fn publish_exact_entry_word<E>(
    replacement: u64,
    mut access: impl FnMut(EntryWordIo) -> Result<u64, E>,
) -> Result<(), EntryWordTransitionError<E>> {
    access(EntryWordIo::Write(replacement)).map_err(|error| EntryWordTransitionError::Access {
        stage: EntryWordIoStage::Write,
        error,
    })?;
    let observed = access(EntryWordIo::Read).map_err(|error| EntryWordTransitionError::Access {
        stage: EntryWordIoStage::Readback,
        error,
    })?;
    if observed != replacement {
        return Err(EntryWordTransitionError::Readback {
            expected: replacement,
            observed,
        });
    }
    Ok(())
}

fn liteinst_entry_word_access(
    task: &mut Stopped,
    address: u64,
    operation: EntryWordIo,
) -> Result<u64, TraceError> {
    match operation {
        EntryWordIo::Read => {
            let address = Addr::<u64>::from_raw(address as usize).ok_or(Errno::EFAULT)?;
            Ok(task.read_value(address)?)
        }
        EntryWordIo::Write(value) => {
            let address = AddrMut::<u64>::from_raw(address as usize).ok_or(Errno::EFAULT)?;
            task.write_value(address, &value)?;
            Ok(value)
        }
    }
}

fn entry_word_transition_trace_error(error: EntryWordTransitionError<TraceError>) -> TraceError {
    match error {
        EntryWordTransitionError::Access { error, .. } => error,
        EntryWordTransitionError::Expected { .. } => Errno::EPROTO.into(),
        EntryWordTransitionError::Readback { .. } => Errno::EIO.into(),
    }
}

fn replace_liteinst_entry_word(
    task: &mut Stopped,
    address: u64,
    expected: u64,
    replacement: u64,
) -> Result<(), TraceError> {
    replace_exact_entry_word(expected, replacement, |operation| {
        liteinst_entry_word_access(task, address, operation)
    })
    .map_err(entry_word_transition_trace_error)
}

fn publish_liteinst_entry_word(
    task: &mut Stopped,
    address: u64,
    replacement: u64,
) -> Result<(), TraceError> {
    publish_exact_entry_word(replacement, |operation| {
        liteinst_entry_word_access(task, address, operation)
    })
    .map_err(entry_word_transition_trace_error)
}

fn entry_guard_inspection_blocks_resume(has_restored_guard: bool, uncertain: bool) -> bool {
    has_restored_guard || uncertain
}

fn entry_word_recovery_error<E>(original: E, recovery: Result<(), E>) -> E {
    match recovery {
        Ok(()) => original,
        Err(recovery_error) => recovery_error,
    }
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
            arena_baseline_maps: Vec::new(),
            prepared_arenas: Vec::new(),
            prepared_reservations: Vec::new(),
            helper_code: None,
            start_break: None,
            current_break: None,
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
        let controls_exist = !self.active_hooks.is_empty()
            || !self.prepared_arenas.is_empty()
            || !self.prepared_reservations.is_empty()
            || self.helper_code.is_some();
        // A hugetlbfs source can make Linux round a raw one-byte fixed
        // replacement to a huge page even when the syscall flags do not expose
        // that geometry. Preserve MAP_FIXED_NOREPLACE, which never replaces an
        // existing control, but refuse true fixed replacements while controls
        // exist until both observers have VMA-aware huge-page geometry.
        if controls_exist
            && ((nr == Sysno::mmap && args.arg3 & libc::MAP_FIXED as usize != 0)
                || (nr == Sysno::mremap && args.arg3 & libc::MREMAP_FIXED as usize != 0))
        {
            return true;
        }
        // process_madvise ranges live behind a guest pointer, io_uring can
        // submit IORING_OP_MADVISE without another syscall boundary, and
        // SHM_REMAP omits the segment length. The controller has no allocation-
        // free exact range proof for them, so keep these fail-closed even before
        // the first hook publishes its arena aliases.
        if matches!(
            nr,
            Sysno::process_madvise
                | Sysno::io_uring_setup
                | Sysno::io_uring_enter
                | Sysno::io_uring_register
                | Sysno::userfaultfd
        ) || (nr == Sysno::shmat && args.arg2 as i32 & libc::SHM_REMAP != 0)
            || (nr == Sysno::ioctl && args.arg1 >> 8 & 0xff == UFFD_IOCTL_TYPE)
            || (nr == Sysno::prctl
                && args.arg0 as u32 as usize == PR_SET_MM
                && matches!(
                    args.arg1 as u32 as usize,
                    PR_SET_MM_START_BRK | PR_SET_MM_BRK | PR_SET_MM_MAP
                ))
        {
            return true;
        }
        if nr == Sysno::brk {
            let Some((start_break, current_break)) = self.start_break.zip(self.current_break)
            else {
                return !self.active_hooks.is_empty();
            };
            return match brk_shrink_effect_range(
                start_break,
                current_break,
                args.arg0 as u64,
                page_size,
            ) {
                Ok(Some(changed)) => self
                    .active_hooks
                    .values()
                    .any(|hook| hook.site.overlaps(changed)),
                Ok(None) => false,
                Err(()) => !self.active_hooks.is_empty(),
            };
        }
        if nr == Sysno::mremap {
            let Ok(effect) = mremap_effect_ranges(args, page_size) else {
                return false;
            };
            let source_mutates_active_hook = self.active_hooks.values().any(|hook| {
                if effect.clones_shared_mapping {
                    [hook.trampoline, hook.arena_writable, hook.arena_executable]
                        .into_iter()
                        .any(|range| range.overlaps(effect.source))
                } else {
                    hook.protected_ranges()
                        .into_iter()
                        .any(|(range, _)| range.overlaps(effect.source))
                }
            });
            let source_mutates_prepared_arena = self.prepared_arenas.iter().any(|arena| {
                arena
                    .protected_ranges()
                    .into_iter()
                    .any(|(range, _)| range.overlaps(effect.source))
            });
            let source_mutates_private_control = !effect.clones_shared_mapping
                && (self
                    .prepared_reservations
                    .iter()
                    .any(|range| range.overlaps(effect.source))
                    || self
                        .helper_code
                        .as_ref()
                        .is_some_and(|helper| helper.range.overlaps(effect.source)));
            if source_mutates_active_hook
                || source_mutates_prepared_arena
                || source_mutates_private_control
            {
                return true;
            }
            let Some(destination) = effect.destination else {
                return false;
            };
            return self.active_hooks.values().any(|hook| {
                hook.protected_ranges()
                    .into_iter()
                    .any(|(range, _)| range.overlaps(destination))
            }) || self.prepared_arenas.iter().any(|arena| {
                arena
                    .protected_ranges()
                    .into_iter()
                    .any(|(range, _)| range.overlaps(destination))
            }) || self
                .prepared_reservations
                .iter()
                .any(|range| range.overlaps(destination))
                || self
                    .helper_code
                    .as_ref()
                    .is_some_and(|helper| helper.range.overlaps(destination));
        }
        let operation_range = match nr {
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::mmap if args.arg3 as i32 & libc::MAP_FIXED != 0 => {
                kernel_mapping_effect_range(args.arg0 as u64, args.arg1 as u64, page_size)
            }
            // AUTONOMOUS-BOT-IMPLEMENTED
            Sysno::munmap | Sysno::mprotect | Sysno::pkey_mprotect => {
                kernel_mapping_effect_range(args.arg0 as u64, args.arg1 as u64, page_size)
            }
            // The kernel receives behavior as a C int. Byte- and fork-
            // preserving advice retains controls, including when raw high
            // register bits are nonzero.
            Sysno::madvise if madvise_preserves_liteinst_generation(args.arg2) => return false,
            // DONTFORK/WIPEONFORK can remove or zero inherited hooks; destructive
            // and unknown future advice must not become an unreviewed bypass.
            Sysno::madvise => {
                kernel_mapping_effect_range(args.arg0 as u64, args.arg1 as u64, page_size)
            }
            Sysno::remap_file_pages => remap_file_pages_effect_range(args, page_size),
            _ => return false,
        };
        let requested_protection = match nr {
            Sysno::mprotect => Some(args.arg2 as i32),
            Sysno::pkey_mprotect if args.arg3 == 0 => Some(args.arg2 as i32),
            _ => None,
        };
        let source_mutates_active_hook = match operation_range {
            Ok(Some(operation_range)) => {
                let prepared_arena = self.prepared_arenas.iter().any(|arena| {
                    arena
                        .protected_ranges()
                        .into_iter()
                        .any(|(range, protection)| {
                            range.overlaps(operation_range)
                                && requested_protection != Some(protection)
                        })
                });
                let prepared_reservation = self.prepared_reservations.iter().any(|range| {
                    range.overlaps(operation_range)
                        && requested_protection != Some(libc::PROT_READ | libc::PROT_WRITE)
                });
                let active_hook = self.active_hooks.values().any(|hook| {
                    hook.protected_ranges()
                        .into_iter()
                        .any(|(range, protection)| {
                            range.overlaps(operation_range)
                                && requested_protection != Some(protection)
                        })
                });
                let helper_code = self.helper_code.as_ref().is_some_and(|helper| {
                    helper.range.overlaps(operation_range)
                        && requested_protection != Some(libc::PROT_NONE)
                });
                prepared_arena || prepared_reservation || active_hook || helper_code
            }
            Ok(None) => false,
            // Invalid guest geometry cannot mutate a mapping and must reach the
            // kernel's native errno rather than becoming a LiteInst refusal.
            Err(()) => false,
        };
        if source_mutates_active_hook {
            return true;
        }
        false
    }

    fn invalidate_attempted_pages(&mut self, start: u64, len: u64, page_size: u64) {
        let range = match kernel_page_covering_range(start, len, page_size) {
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
        self.attempted_sites.retain(|address| {
            address
                .checked_add(8)
                .is_some_and(|end| !(range.start < end && *address < range.end))
        });
        self.fallback_sites.retain(|address, _| {
            address
                .checked_add(8)
                .is_some_and(|end| !(range.start < end && *address < range.end))
        });
    }
}

#[cfg(target_arch = "x86_64")]
fn after_loader_liteinst_ready_is_publishable(
    state: &LiteinstRuntimeState,
    generation: u64,
    frame: LiteinstHandshakeFrame,
    prepared_arenas: &[PreparedArenaFootprint],
    prepared_reservations: &[GuestRange],
    helper_code: &LiteinstHelperCode,
    initializer: &crate::target_loader::TargetHostInitializer,
) -> bool {
    let helper_range = GuestRange::new(
        frame.install_helper_page_start,
        frame.install_helper_page_len,
    );
    let helper_identity = helper_code.original_mapping.mapping_identity();
    let initializer_identity = MappingIdentity::from_target_loader(initializer.mapping_identity);
    let control_ranges = prepared_arenas
        .iter()
        .flat_map(|arena| arena.protected_ranges().map(|(range, _)| range))
        .chain(prepared_reservations.iter().copied())
        .collect::<Vec<_>>();
    let controls_are_disjoint = control_ranges.iter().enumerate().all(|(index, range)| {
        !range.overlaps(helper_code.range)
            && control_ranges[..index]
                .iter()
                .all(|prior| !prior.overlaps(*range))
    });
    state.phase == LiteinstRuntimePhase::Bootstrap
        && state.generation == generation
        && state.ready_generation.is_none()
        && state.frame == Some(frame)
        && state.start_break == Some(frame.start_program_break)
        && state.current_break == Some(frame.initial_program_break)
        && !state.arena_baseline_maps.is_empty()
        && state.attempted_sites.is_empty()
        && state.fallback_sites.is_empty()
        && state.active_hooks.is_empty()
        && state.prepared_arenas.is_empty()
        && state.prepared_reservations.is_empty()
        && state.helper_code.is_none()
        && state.after_loader_reference.is_none()
        && !state.after_loader_guest_observed
        && state.after_loader_syscall_trap
            == Some(LiteinstTrapSiteProvenance {
                generation,
                rip: frame.syscall_trap_rip,
            })
        && !prepared_arenas.is_empty()
        && prepared_arenas.len() == prepared_reservations.len()
        && helper_range == Some(helper_code.range)
        && helper_code
            .original_mapping
            .contains_range(helper_code.range)
        && helper_identity == initializer_identity
        && controls_are_disjoint
}

#[cfg(all(target_arch = "x86_64", test))]
fn publish_after_loader_liteinst_ready(
    state: &mut LiteinstRuntimeState,
    generation: u64,
    frame: LiteinstHandshakeFrame,
    prepared_arenas: Vec<PreparedArenaFootprint>,
    prepared_reservations: Vec<GuestRange>,
    helper_code: LiteinstHelperCode,
    handle: u64,
    initializer: crate::target_loader::TargetHostInitializer,
) -> bool {
    if !after_loader_liteinst_ready_is_publishable(
        state,
        generation,
        frame,
        &prepared_arenas,
        &prepared_reservations,
        &helper_code,
        &initializer,
    ) {
        return false;
    }
    commit_after_loader_liteinst_ready(
        state,
        generation,
        prepared_arenas,
        prepared_reservations,
        helper_code,
        handle,
        initializer,
    );
    true
}

#[cfg(target_arch = "x86_64")]
fn commit_after_loader_liteinst_ready(
    state: &mut LiteinstRuntimeState,
    generation: u64,
    prepared_arenas: Vec<PreparedArenaFootprint>,
    prepared_reservations: Vec<GuestRange>,
    helper_code: LiteinstHelperCode,
    handle: u64,
    initializer: crate::target_loader::TargetHostInitializer,
) {
    drop(std::mem::take(&mut state.arena_baseline_maps));
    state.prepared_arenas = prepared_arenas;
    state.prepared_reservations = prepared_reservations;
    state.helper_code = Some(helper_code);
    state.after_loader_reference = Some((handle, initializer));
    state.ready_generation = Some(generation);
    state.phase = LiteinstRuntimePhase::Ready;
}

fn observe_liteinst_mapping_result_in_state(
    state: &mut LiteinstRuntimeState,
    nr: Sysno,
    args: SyscallArgs,
    result: Result<i64, Errno>,
    page_size: u64,
) {
    let Ok(result) = result else {
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
            // The signal-runtime observer cannot recover a hugetlb source's
            // page size without violating its allocation/syscall boundary, so
            // any successful nonfixed mremap retires the whole LiteInst
            // generation. Keep active footprints only as conservative mapping
            // guards; revoking ready_generation disables new installs and
            // hidden callback authentication.
            state.attempted_sites.clear();
            state.fallback_sites.clear();
            state.ready_generation = None;
        }
        Sysno::madvise if !madvise_preserves_liteinst_generation(args.arg2) => {
            state.invalidate_attempted_pages(args.arg0 as u64, args.arg1 as u64, page_size);
        }
        Sysno::remap_file_pages => match remap_file_pages_effect_range(args, page_size) {
            Ok(Some(range)) => {
                state.invalidate_attempted_pages(range.start, range.end - range.start, page_size);
            }
            Ok(None) => {}
            Err(()) => {
                state.attempted_sites.clear();
                state.fallback_sites.clear();
            }
        },
        Sysno::brk => {
            let Ok(returned) = u64::try_from(result) else {
                state.current_break = None;
                return;
            };
            let Some((start_break, old_break)) = state.start_break.zip(state.current_break) else {
                state.attempted_sites.clear();
                state.fallback_sites.clear();
                return;
            };
            if returned < start_break || (returned != old_break && returned != args.arg0 as u64) {
                state.current_break = None;
                state.attempted_sites.clear();
                state.fallback_sites.clear();
                return;
            }
            if returned != old_break {
                let pages = page_ceil_u64(old_break, page_size)
                    .and_then(|old| page_ceil_u64(returned, page_size).map(|new| (old, new)));
                match pages {
                    Ok((old, new)) if old < new => {
                        state.invalidate_attempted_pages(old, new - old, page_size);
                    }
                    Ok((old, new)) if new < old => {
                        state.invalidate_attempted_pages(new, old - new, page_size);
                    }
                    Ok(_) => {}
                    Err(()) => {
                        state.attempted_sites.clear();
                        state.fallback_sites.clear();
                    }
                }
                state.current_break = Some(returned);
            }
        }
        _ => {}
    }
}

fn observe_liteinst_mapping_result_without_page_size(state: &mut LiteinstRuntimeState, nr: Sysno) {
    state.attempted_sites.clear();
    state.fallback_sites.clear();
    if nr == Sysno::mremap {
        state.ready_generation = None;
    }
}

enum LiteinstTrap {
    HandshakeBegin,
    HandshakeReady,
    #[cfg(target_arch = "x86_64")]
    InstalledEntry {
        generation: u64,
        footprint: ActiveHookFootprint,
    },
    #[cfg(target_arch = "x86_64")]
    InstalledCompletion,
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

    /// Exact post-Ready direct-hook transaction spanning generated entry,
    /// retained runtime trap, and guest-visible completion stops.
    #[cfg(target_arch = "x86_64")]
    liteinst_installed_event: Option<LiteinstInstalledEvent>,
    #[cfg(target_arch = "x86_64")]
    liteinst_installed_resume_rip: Option<u64>,
    #[cfg(target_arch = "x86_64")]
    liteinst_active_pc_footprint: Option<ActiveHookFootprint>,

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
    liteinst_entry_guard_inspection: Option<RestoredLiteinstEntryGuard>,
    #[cfg(target_arch = "x86_64")]
    liteinst_entry_guard_uncertain: bool,
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
            #[cfg(target_arch = "x86_64")]
            liteinst_installed_event: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_installed_resume_rip: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_active_pc_footprint: None,
            active_tool_stop: None,
            liteinst_runtime: Arc::new(StdMutex::new(LiteinstRuntimeState::default())),
            liteinst_entry_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_entry_guard_inspection: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_entry_guard_uncertain: false,
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
            #[cfg(target_arch = "x86_64")]
            liteinst_installed_event: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_installed_resume_rip: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_active_pc_footprint: None,
            active_tool_stop: None,
            liteinst_runtime: self.liteinst_runtime.clone(),
            liteinst_entry_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_entry_guard_inspection: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_entry_guard_uncertain: false,
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
            #[cfg(target_arch = "x86_64")]
            liteinst_installed_event: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_installed_resume_rip: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_active_pc_footprint: None,
            active_tool_stop: None,
            liteinst_runtime: Arc::new(StdMutex::new(
                self.liteinst_runtime.lock().unwrap().clone(),
            )),
            liteinst_entry_guard: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_entry_guard_inspection: None,
            #[cfg(target_arch = "x86_64")]
            liteinst_entry_guard_uncertain: false,
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
            frame.copy_from_user_regs(regs);
            self.write_injected_syscall_frame(task, address, &frame)
        } else {
            #[cfg(target_arch = "x86_64")]
            if self.liteinst_installed_resume_rip.is_some() {
                let current = task.getregs()?;
                validate_liteinst_installed_user_regs_update(&current, regs)?;
            }
            task.setregs(regs)
        }
    }

    fn get_syscall(&self, task: &Stopped) -> Result<Syscall, TraceError> {
        let regs = task.getregs()?;
        let raw = regs.orig_syscall();
        let nr = usize::try_from(raw)
            .ok()
            .and_then(Sysno::new)
            .ok_or(Errno::ENOSYS)?;

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
        ) -> Result<(), TraceError> {
            if let Some(slot) = held_task_stops {
                HeldRootStop::arm_empty(slot, task, event)?;
            }
            Ok(())
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
                arm_preinit_stop(held_task_stops, &task, &event)?;
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
                            arm_preinit_stop(held_task_stops, &completed, &event)?;
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
        #[cfg(target_arch = "x86_64")]
        let pc_translation = self.virtualize_liteinst_timer_program_counter(&task)?;
        #[cfg(target_arch = "x86_64")]
        if let Some(translation) = pc_translation.as_ref() {
            self.liteinst_installed_resume_rip = Some(translation.generated_rip);
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
        #[cfg(target_arch = "x86_64")]
        {
            self.liteinst_installed_resume_rip = None;
            if let Some(translation) = pc_translation.as_ref() {
                match liteinst_tool_program_counter_action(
                    task.getregs()?.ip(),
                    translation.logical_rip,
                ) {
                    LiteinstToolProgramCounterAction::RestoreGenerated => {
                        self.restore_liteinst_timer_program_counter(&task, translation)?;
                    }
                    LiteinstToolProgramCounterAction::Deopt => {
                        self.deopt_liteinst_hooks_quiescent(
                            &task,
                            LiteinstDeoptProgramCounter::PreserveObserved,
                        )?;
                    }
                }
            }
        }
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
        if matches!(event, Event::Signal(Signal::SIGTRAP))
            && self.is_liteinst_installed_entry_candidate(&stopped)
        {
            // Classification decides whether the raw number is a subscribed
            // logical event before advancing Timer state. Unknown and
            // unsubscribed numbers at a shared patched syscall site retain
            // Linux's no-Tool-event behavior.
            return self
                .handle_signal(stopped, Signal::SIGTRAP)
                .await
                .tracee_context(self.tid(), "handle installed-hook entry stop");
        }
        #[cfg(target_arch = "x86_64")]
        if self.liteinst_installed_event.is_some() {
            // Entry was the single Tool-visible event and suspended the Timer.
            // Runtime and completion stops are private transaction boundaries:
            // they must not consume another logical event or call any Timer API
            // before completion restores the exact suspension token.
            return match event {
                Event::Signal(Signal::SIGTRAP) => self
                    .handle_signal(stopped, Signal::SIGTRAP)
                    .await
                    .tracee_context(self.tid(), "handle installed-hook private stop"),
                Event::Signal(Signal::SIGSTOP) => self
                    .resolve_liteinst_installed_sigstop(stopped)
                    .await
                    .tracee_context(self.tid(), "resolve installed-hook SIGSTOP delivery"),
                private => Err(Error::runtime(
                    self.tid(),
                    "advance installed-hook transaction",
                    format!("unexpected private stopped event: {private:?}"),
                )),
            };
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
        self.arm_liteinst_wait(&wait)?;
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
            self.arm_liteinst_wait(&wait)?;
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
        if known_number.is_none() {
            return Err(Errno::ENOSYS.into());
        }
        if known_number == Some(Sysno::rt_sigreturn) {
            return self.resume_injected_rt_sigreturn(task, &frame).await;
        }
        let syscall = frame.syscall();
        let (nr, args) = syscall.into_parts();
        let mapping_syscall = is_liteinst_mapping_syscall(nr, args);
        if mapping_syscall {
            self.validate_liteinst_mapping_execution(nr, args)?;
        }

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
            if mapping_syscall {
                self.observe_liteinst_mapping_result(nr, args, result);
            }
            self.write_injected_syscall_result(&task, result)?;
            self.injected_syscall_frame = None;
            let signal = self.take_pending_signal_for_resume(
                &task,
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
                &task,
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
        let page_size = host_page_size().ok()?;
        let helper_code = GuestRange::new(
            frame.install_helper_page_start,
            frame.install_helper_page_len,
        )?;
        let helper_entry = GuestRange::new(frame.install_helper, 16)?;
        if frame.version != 8
            || frame.start_program_break == 0
            || frame.initial_program_break == 0
            || frame.initial_program_break < frame.start_program_break
            || frame.helper_stack_top < 8
            || frame.helper_stack_top & 0xf != 0
            || frame.install_helper.checked_add(1) != Some(frame.install_helper_rip)
            || !frame.install_helper_page_start.is_multiple_of(page_size)
            || frame.install_helper_page_len != page_size
            || helper_code.overlaps(helper_entry)
            || trap_rip
                != if ready {
                    frame.ready_rip
                } else {
                    frame.begin_rip
                }
        {
            return None;
        }
        #[cfg(target_arch = "x86_64")]
        if config.after_loader.is_some()
            && (guest_start_program_break(task.pid()) != Some(frame.start_program_break)
                || self
                    .liteinst_after_loader_private_state
                    .as_ref()
                    .and_then(|state| state.current_program_break())
                    != Some(frame.initial_program_break))
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
                        && self
                            .liteinst_after_loader_private_state
                            .as_ref()
                            .and_then(|state| {
                                state.image_mapping_identity(&caller.sealed_runtime.image)
                            })
                            .is_some_and(|identity| identity == mapping.mapping_identity())
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
            frame.install_helper_rip,
            frame.install_helper_page_start,
            helper_code.end - 1,
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
        let helper_code_mapping = maps.iter().find(|mapping| {
            mapping.readable
                && !mapping.writable
                && mapping.executable
                && !mapping.shared
                && mapping.contains_range(helper_code)
        })?;
        if !preload_code(helper_code_mapping.start) {
            return None;
        }
        let mut entry_bytes = [0_u8; 6];
        task.read_exact(frame.install_helper as usize, &mut entry_bytes)
            .ok()?;
        let jump: [u8; 5] = entry_bytes[1..].try_into().ok()?;
        if entry_bytes[0] != 0xcc
            || !x86_near_jump_target(frame.install_helper_rip, jump)
                .is_some_and(|target| helper_code.start <= target && target < helper_code.end)
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
        let install_request = GuestRange::new(
            frame.install_request,
            core::mem::size_of::<LiteinstInstallRequest>() as u64,
        )?;
        let install_result_writable = maps.iter().any(|mapping| {
            Some((mapping.start, mapping.end))
                == helper_stack_map.map(|stack| (stack.start, stack.end))
                && mapping.readable
                && mapping.writable
                && mapping.contains_range(install_result)
                && mapping.contains_range(install_request)
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
        #[cfg(target_arch = "x86_64")]
        if entry_guard_inspection_blocks_resume(
            self.liteinst_entry_guard_inspection.is_some(),
            self.liteinst_entry_guard_uncertain,
        ) {
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
        #[cfg(target_arch = "x86_64")]
        if entry_guard_inspection_blocks_resume(
            self.liteinst_entry_guard_inspection.is_some(),
            self.liteinst_entry_guard_uncertain,
        ) {
            return Err(Errno::EPROTO.into());
        }
        let guard = self.liteinst_entry_guard.ok_or(Errno::EPROTO)?;
        let guarded_instruction = (guard.saved_instruction & !0xff) | 0xcc;
        if let Err(error) = replace_liteinst_entry_word(
            task,
            guard.address,
            guarded_instruction,
            guard.saved_instruction,
        ) {
            let recovery = publish_liteinst_entry_word(task, guard.address, guarded_instruction);
            #[cfg(target_arch = "x86_64")]
            if recovery.is_err() {
                self.liteinst_entry_guard_uncertain = true;
            }
            return Err(entry_word_recovery_error(error, recovery));
        }
        self.liteinst_entry_guard = None;
        Ok(())
    }

    /// Run one synchronous, stopped-task inspection against pristine entry
    /// bytes, then restore the exact authenticated breakpoint before returning.
    /// The closure cannot suspend; no tracee resume belongs in this window.
    #[cfg(target_arch = "x86_64")]
    fn with_restored_liteinst_entry_guard<T>(
        &mut self,
        task: &mut Stopped,
        authenticated: crate::entry_call::AuthenticatedEntryWord,
        inspect: impl FnOnce(&mut Self, &Stopped) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.restore_liteinst_entry_guard_for_inspection(task, authenticated)?;
        let result = inspect(self, task);
        // A re-arm failure takes precedence: the stopped task must never leave
        // this method with an apparently armed guard that is not in memory.
        self.rearm_liteinst_entry_guard_after_inspection(task)?;
        result
    }

    #[cfg(target_arch = "x86_64")]
    fn restore_liteinst_entry_guard_for_inspection(
        &mut self,
        task: &mut Stopped,
        authenticated: crate::entry_call::AuthenticatedEntryWord,
    ) -> Result<(), TraceError> {
        let guard = self.liteinst_entry_guard.ok_or(Errno::EPROTO)?;
        let original = u64::from_ne_bytes(authenticated.original());
        let guarded = u64::from_ne_bytes(authenticated.guarded());
        let identity = authenticated.identity();
        if self.liteinst_entry_guard_inspection.is_some()
            || self.liteinst_entry_guard_uncertain
            || self.liteinst_after_loader_guard.is_some()
            || identity.tid != task.pid().as_raw()
            || identity.at_entry != guard.address
            || original != guard.saved_instruction
            || guarded != ((guard.saved_instruction & !0xff) | 0xcc)
            || self
                .after_loader_identity(task)
                .map_err(|_| Errno::EPROTO)?
                != identity
        {
            return Err(Errno::EPROTO.into());
        }
        self.liteinst_entry_guard = None;
        let restored = RestoredLiteinstEntryGuard {
            guard,
            authenticated,
        };
        if let Err(error) = replace_liteinst_entry_word(task, guard.address, guarded, original) {
            return Err(self.recover_or_retain_liteinst_entry_guard(task, restored, error));
        }
        self.liteinst_entry_guard_inspection = Some(restored);
        self.liteinst_entry_guard_uncertain = false;
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn rearm_liteinst_entry_guard_after_inspection(
        &mut self,
        task: &mut Stopped,
    ) -> Result<(), TraceError> {
        if self.liteinst_entry_guard.is_some() {
            return Err(Errno::EALREADY.into());
        }
        if self.liteinst_entry_guard_uncertain {
            return Err(
                self.recover_current_liteinst_entry_guard_inspection(task, Errno::EPROTO.into())
            );
        }
        let restored = self
            .liteinst_entry_guard_inspection
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        let original = u64::from_ne_bytes(restored.authenticated.original());
        let guarded = u64::from_ne_bytes(restored.authenticated.guarded());
        if let Err(error) =
            replace_liteinst_entry_word(task, restored.guard.address, original, guarded)
        {
            return Err(self.recover_current_liteinst_entry_guard_inspection(task, error));
        }
        let restored = self
            .liteinst_entry_guard_inspection
            .take()
            .ok_or(Errno::EPROTO)?;
        self.liteinst_entry_guard = Some(restored.guard);
        self.liteinst_entry_guard_uncertain = false;
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn recover_current_liteinst_entry_guard_inspection(
        &mut self,
        task: &mut Stopped,
        original_error: TraceError,
    ) -> TraceError {
        let Some(restored) = self.liteinst_entry_guard_inspection.take() else {
            self.liteinst_entry_guard_uncertain = true;
            return original_error;
        };
        self.recover_or_retain_liteinst_entry_guard(task, restored, original_error)
    }

    #[cfg(target_arch = "x86_64")]
    fn recover_or_retain_liteinst_entry_guard(
        &mut self,
        task: &mut Stopped,
        restored: RestoredLiteinstEntryGuard,
        original_error: TraceError,
    ) -> TraceError {
        let guarded = u64::from_ne_bytes(restored.authenticated.guarded());
        let recovery = publish_liteinst_entry_word(task, restored.guard.address, guarded);
        if recovery.is_ok() {
            self.liteinst_entry_guard = Some(restored.guard);
            self.liteinst_entry_guard_inspection = None;
            self.liteinst_entry_guard_uncertain = false;
        } else {
            self.liteinst_entry_guard = None;
            self.liteinst_entry_guard_inspection = Some(restored);
            self.liteinst_entry_guard_uncertain = true;
        }
        entry_word_recovery_error(original_error, recovery)
    }

    #[cfg(target_arch = "x86_64")]
    fn is_liteinst_installed_entry_candidate(&self, task: &Stopped) -> bool {
        if self.after_loader_config().is_none() || self.liteinst_installed_event.is_some() {
            return false;
        }
        let Ok(registers) = task.getregs() else {
            return false;
        };
        let state = self.liteinst_runtime.lock().unwrap();
        state.phase == LiteinstRuntimePhase::Ready
            && state.ready_generation == Some(state.generation)
            && state
                .active_hooks
                .values()
                .any(|hook| hook.ptrace_entry_stop_rip == registers.ip())
    }

    #[cfg(target_arch = "x86_64")]
    async fn begin_liteinst_installed_event(
        &mut self,
        task: Stopped,
        generation: u64,
        footprint: ActiveHookFootprint,
    ) -> Result<Wait, TraceError> {
        if !self.liteinst_installed_root_is_quiescent(&task)
            || self.liteinst_installed_event.is_some()
            || self.injected_syscall_frame.is_some()
            || self.active_tool_stop.is_some()
            || self.pending_syscall.is_some()
            || self.pending_syscall_already_skipped
            || self.pending_signal.is_some()
            || self.liteinst_after_loader_syscall_permit.is_some()
            || self.liteinst_after_loader_syscall_inflight.is_some()
            || self.liteinst_after_loader_forward_inflight.is_some()
            || self.liteinst_after_loader_private_call.is_some()
            || self.liteinst_after_loader_private_state.is_some()
        {
            return Err(Errno::EPROTO.into());
        }
        let (phase, current_generation, ready_generation) = {
            let state = self.liteinst_runtime.lock().unwrap();
            (state.phase, state.generation, state.ready_generation)
        };
        if phase != LiteinstRuntimePhase::Ready
            || generation != current_generation
            || ready_generation != Some(generation)
            || task.pid() != self.tid()
        {
            return Err(Errno::EPROTO.into());
        }
        let entry_regs = task.getregs()?;
        if entry_regs.ip() != footprint.ptrace_entry_stop_rip
            || !footprint.validates_ptrace_stop(&task, entry_regs.ip())
        {
            return Err(Errno::EPROTO.into());
        }
        self.liteinst_active_pc_footprint = None;
        self.observe_after_loader_stopped_event(&task, &Event::Signal(Signal::SIGTRAP))?;
        let tool_subscribed = liteinst_installed_tool_event_number(entry_regs.rax).is_some_and(
            |number| {
                self.global_state
                    .subscriptions
                    .iter_syscalls()
                    .any(|candidate| candidate == number)
            },
        );
        if tool_subscribed {
            self.timer.observe_event();
        }
        let entry_xstate = task.getxstate()?;
        let entry_status = task
            .physical_status_id()
            .ok_or(Errno::EPROTO)?
            .get();
        let entry_logical_stop = task.logical_stop_id();
        let original_sigmask = task.getsigmask()?;
        let private_sigmask = liteinst_private_sigmask(original_sigmask);
        task.setsigmask(&private_sigmask)?;
        let installed_private_sigmask = task.getsigmask();
        if !matches!(installed_private_sigmask, Ok(mask) if mask == private_sigmask) {
            let _ = task.setsigmask(&original_sigmask);
            return match installed_private_sigmask {
                Ok(_) => Err(Errno::EIO.into()),
                Err(error) => Err(error),
            };
        }
        let timer_suspension = match self.timer.begin_suspend_for_private_execution() {
            Ok(suspension) => suspension,
            Err(error) => {
                task.setsigmask(&original_sigmask)?;
                if task.getsigmask()? != original_sigmask {
                    return Err(Errno::EIO.into());
                }
                tracing::error!(
                    tid = %self.tid(),
                    %error,
                    "failed to suspend deterministic timer for installed hook"
                );
                return Err(Errno::EPROTO.into());
            }
        };
        // From this point onward the exact timer token always has a durable
        // owner. Any later error leaves the transaction available for the
        // terminal-only retirement path rather than orphaning a suspension.
        self.liteinst_installed_event = Some(LiteinstInstalledEvent {
            generation,
            physical_generation: task.physical_event_generation(),
            entry_status,
            status_floor: entry_status,
            logical_stop_floor: entry_logical_stop,
            footprint,
            entry_regs,
            entry_xstate,
            original_sigmask,
            private_sigmask,
            timer_suspension: Some(timer_suspension),
            phase: LiteinstInstalledEventPhase::AwaitingRuntimeTrap,
        });
        let timer_suspension = self
            .liteinst_installed_event
            .as_ref()
            .and_then(|transaction| transaction.timer_suspension.as_ref())
            .ok_or(Errno::EPROTO)?;
        if let Err(error) = self
            .timer
            .complete_suspend_for_private_execution(timer_suspension)
        {
            tracing::error!(
                tid = %self.tid(),
                %error,
                "failed to complete deterministic timer suspension for installed hook"
            );
            return Err(Errno::EPROTO.into());
        }
        let wait = self.resume_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        Ok(wait)
    }

    #[cfg(target_arch = "x86_64")]
    async fn advance_liteinst_installed_runtime_trap(
        &mut self,
        task: Stopped,
        frame_address: usize,
    ) -> Result<Wait, TraceError> {
        let (
            generation,
            physical_generation,
            entry_status,
            status_floor,
            logical_stop_floor,
            site,
            entry_regs,
            phase,
        ) = {
            let transaction = self
                .liteinst_installed_event
                .as_ref()
                .ok_or(Errno::EPROTO)?;
            (
                transaction.generation,
                transaction.physical_generation,
                transaction.entry_status,
                transaction.status_floor,
                transaction.logical_stop_floor,
                transaction.footprint.site.start,
                transaction.entry_regs,
                transaction.phase.private(),
            )
        };
        let runtime_status = task
            .physical_status_id()
            .ok_or(Errno::EPROTO)?
            .get();
        let frame = self.read_injected_syscall_frame(&task, frame_address)?;
        let current_generation = self.liteinst_runtime.lock().unwrap().generation;
        if phase != Some(LiteinstInstalledPrivatePhase::AwaitingRuntimeTrap)
            || generation != current_generation
            || task.physical_event_generation() != physical_generation
            || runtime_status <= entry_status
            || runtime_status <= status_floor
            || !task
                .logical_stop_id()
                .is_strictly_after(logical_stop_floor)
            || !liteinst_runtime_frame_matches_entry(&frame, &entry_regs, site)
            || task.getsigmask()?
                != self
                    .liteinst_installed_event
                    .as_ref()
                    .ok_or(Errno::EPROTO)?
                    .private_sigmask
        {
            return Err(Errno::EPROTO.into());
        }
        let transaction = self
            .liteinst_installed_event
            .as_mut()
            .ok_or(Errno::EPROTO)?;
        transaction.status_floor = runtime_status;
        transaction.logical_stop_floor = task.logical_stop_id();
        transaction.phase = LiteinstInstalledEventPhase::AwaitingCompletion { runtime_status };
        let wait = self.resume_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        Ok(wait)
    }

    #[cfg(target_arch = "x86_64")]
    async fn complete_liteinst_installed_event(
        &mut self,
        task: Stopped,
    ) -> Result<Wait, TraceError> {
        let completion_status = task
            .physical_status_id()
            .ok_or(Errno::EPROTO)?
            .get();
        let completion_regs = task.getregs()?;
        let siginfo = task.getsiginfo()?;
        let current_generation = self.liteinst_runtime.lock().unwrap().generation;
        let transaction = self
            .liteinst_installed_event
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        let runtime_status = match transaction.phase {
            LiteinstInstalledEventPhase::AwaitingCompletion { runtime_status } => runtime_status,
            LiteinstInstalledEventPhase::AwaitingRuntimeTrap
            | LiteinstInstalledEventPhase::AwaitStopResolution { .. } => {
                return Err(Errno::EPROTO.into());
            }
        };
        if transaction.generation != current_generation
            || task.physical_event_generation() != transaction.physical_generation
            || completion_status <= runtime_status
            || completion_status <= transaction.status_floor
            || !task
                .logical_stop_id()
                .is_strictly_after(transaction.logical_stop_floor)
            || completion_regs.ip() != transaction.footprint.ptrace_completion_stop_rip
            || transaction.footprint.ptrace_completion_stop_rip
                != transaction.footprint.relocated_tail
            || !matches!(siginfo.si_code, libc::TRAP_BRKPT | libc::SI_KERNEL)
            || !transaction
                .footprint
                .validates_ptrace_stop(&task, completion_regs.ip())
            || !liteinst_completion_registers_match(
                &transaction.entry_regs,
                &completion_regs,
                transaction.footprint.ptrace_completion_stop_rip,
            )
            || task.getxstate()? != transaction.entry_xstate
            || task.getsigmask()? != transaction.private_sigmask
        {
            return Err(Errno::EPROTO.into());
        }
        let original_sigmask = transaction.original_sigmask;
        let timer_suspension = transaction
            .timer_suspension
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        let frozen_clock = timer_suspension.frozen_clock();
        if self.timer.diagnostic_clock() != Some(frozen_clock) {
            return Err(Errno::EPROTO.into());
        }
        let raw_number = transaction.entry_regs.rax;
        let args = SyscallArgs::new(
            transaction.entry_regs.rdi as usize,
            transaction.entry_regs.rsi as usize,
            transaction.entry_regs.rdx as usize,
            transaction.entry_regs.r10 as usize,
            transaction.entry_regs.r8 as usize,
            transaction.entry_regs.r9 as usize,
        );
        let continuation = transaction
            .footprint
            .site
            .start
            .checked_add(cp::SYSCALL_INSTR_SIZE as u64)
            .ok_or(Errno::EOVERFLOW)?;
        let logical_regs =
            liteinst_logical_syscall_entry_registers(&transaction.entry_regs, continuation);
        let relocated_tail = transaction.footprint.relocated_tail;
        let completed_footprint = transaction.footprint.clone();

        task.setsigmask(&original_sigmask)?;
        if task.getsigmask()? != original_sigmask {
            return Err(Errno::EIO.into());
        }
        task.setregs(&logical_regs)?;
        if let Err(error) = self
            .timer
            .restore_after_private_execution(timer_suspension)
        {
            tracing::error!(
                tid = %self.tid(),
                %error,
                "failed to restore deterministic timer after installed hook"
            );
            return Err(Errno::EPROTO.into());
        }
        // The timer and signal mask are now fully restored. Only now retire
        // the transaction and its one-use token; every prior error retained
        // both for exact terminal cleanup.
        let mut transaction = self
            .liteinst_installed_event
            .take()
            .ok_or(Errno::EPROTO)?;
        drop(
            transaction
                .timer_suspension
                .take()
                .ok_or(Errno::EPROTO)?,
        );
        if self.timer.diagnostic_clock() != Some(frozen_clock) {
            return Err(Errno::EPROTO.into());
        }

        if let Some(stats) = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .and_then(|config| config.instrumentation_stats.as_ref())
        {
            stats.lock().unwrap().record_direct_hook();
        }
        self.liteinst_active_pc_footprint = Some(completed_footprint);
        self.dispatch_liteinst_installed_syscall(
            task,
            raw_number,
            args,
            relocated_tail,
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn resume_liteinst_installed_tail(
        &mut self,
        task: Stopped,
        relocated_tail: u64,
    ) -> Result<Wait, TraceError> {
        let signal = self.take_pending_signal_for_resume(
            &task,
            LiteinstActivationOperation::ResumeInterceptedInjectedSyscall,
        )?;
        let mut registers = task.getregs()?;
        if signal.is_none() {
            let default_logical_rip = self
                .liteinst_active_pc_footprint
                .as_ref()
                .and_then(|footprint| footprint.translate_program_counter(relocated_tail))
                .ok_or(Errno::EPROTO)?;
            match liteinst_tool_program_counter_action(registers.ip(), default_logical_rip) {
                LiteinstToolProgramCounterAction::RestoreGenerated => {
                    registers.rip = relocated_tail;
                    task.setregs(&registers)?;
                }
                LiteinstToolProgramCounterAction::Deopt => {
                    // A Tool-directed control-flow change is legal. Retire the
                    // patched words before honoring it so an address selected
                    // in any displaced instruction range remains real guest code.
                    self.deopt_liteinst_hooks_quiescent(
                        &task,
                        LiteinstDeoptProgramCounter::PreserveObserved,
                    )?;
                }
            }
        }
        self.pending_syscall = None;
        self.pending_syscall_already_skipped = false;
        self.liteinst_installed_resume_rip = None;
        let wait = self.resume_stopped(task, signal)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        Ok(wait)
    }

    #[cfg(target_arch = "x86_64")]
    fn restore_liteinst_stop_resolution(
        &mut self,
        prior: LiteinstInstalledPrivatePhase,
        snapshot: &LiteinstStopResolutionSnapshot,
        status_floor: u64,
        logical_stop_floor: safeptrace::LogicalStopId,
    ) -> Result<(), TraceError> {
        let transaction = self
            .liteinst_installed_event
            .as_mut()
            .ok_or(Errno::EPROTO)?;
        match &transaction.phase {
            LiteinstInstalledEventPhase::AwaitStopResolution {
                prior: retained_prior,
                snapshot: retained_snapshot,
            } if *retained_prior == prior && retained_snapshot == snapshot => {}
            _ => return Err(Errno::EPROTO.into()),
        }
        if status_floor < snapshot.delivery_status
            || (!logical_stop_floor.is_strictly_after(snapshot.delivery_logical_stop)
                && logical_stop_floor != snapshot.delivery_logical_stop)
        {
            return Err(Errno::EPROTO.into());
        }
        transaction.status_floor = status_floor;
        transaction.logical_stop_floor = logical_stop_floor;
        transaction.phase = LiteinstInstalledEventPhase::restore(prior);
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn retire_liteinst_installed_event_on_terminal(&mut self) -> Result<(), TraceError> {
        if self.liteinst_installed_event.is_none() {
            return self.retire_after_loader_private_timer_on_terminal();
        }
        let transaction = self
            .liteinst_installed_event
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        let suspension = transaction
            .timer_suspension
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        if let Err(error) = self
            .timer
            .retire_private_execution_on_terminal(suspension)
        {
            tracing::error!(
                tid = %self.tid(),
                %error,
                "failed to retire deterministic timer after terminal installed event"
            );
            return Err(Errno::EPROTO.into());
        }
        let mut transaction = self
            .liteinst_installed_event
            .take()
            .ok_or(Errno::EPROTO)?;
        drop(
            transaction
                .timer_suspension
                .take()
                .ok_or(Errno::EPROTO)?,
        );
        self.liteinst_installed_resume_rip = None;
        self.liteinst_active_pc_footprint = None;
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn validate_cancelled_liteinst_installed_stop(
        &self,
        task: &Stopped,
        event: &Event,
        prior: LiteinstInstalledPrivatePhase,
        delivery: &LiteinstStopResolutionSnapshot,
    ) -> Result<(), TraceError> {
        if *event != Event::Signal(Signal::SIGTRAP) {
            return Err(Errno::EPROTO.into());
        }
        let status = task
            .physical_status_id()
            .ok_or(Errno::EPROTO)?
            .get();
        let logical_stop = task.logical_stop_id();
        let transaction = self
            .liteinst_installed_event
            .as_ref()
            .ok_or(Errno::EPROTO)?;
        let retained_matches = matches!(
            &transaction.phase,
            LiteinstInstalledEventPhase::AwaitStopResolution {
                prior: retained_prior,
                snapshot,
            } if *retained_prior == prior && snapshot == delivery
        );
        if !retained_matches
            || task.physical_event_generation() != transaction.physical_generation
            || status <= delivery.delivery_status
            || !logical_stop.is_strictly_after(delivery.delivery_logical_stop)
            || task.getsigmask()? != transaction.private_sigmask
        {
            return Err(Errno::EPROTO.into());
        }

        match prior {
            LiteinstInstalledPrivatePhase::AwaitingRuntimeTrap => {
                let registers = task.getregs()?;
                let frame_address = self
                    .installed_runtime_frame_address_for_phase(
                        task,
                        &registers,
                        prior,
                        true,
                    )
                    .ok_or(Errno::EPROTO)?;
                let frame = self.read_injected_syscall_frame(task, frame_address)?;
                if !liteinst_runtime_frame_matches_entry(
                    &frame,
                    &transaction.entry_regs,
                    transaction.footprint.site.start,
                ) {
                    return Err(Errno::EPROTO.into());
                }
            }
            LiteinstInstalledPrivatePhase::AwaitingCompletion { runtime_status } => {
                let registers = task.getregs()?;
                let siginfo = task.getsiginfo()?;
                let state = self.liteinst_runtime.lock().unwrap();
                if state.phase != LiteinstRuntimePhase::Ready
                    || state.generation != transaction.generation
                    || state.ready_generation != Some(transaction.generation)
                    || status <= runtime_status
                    || registers.ip() != transaction.footprint.ptrace_completion_stop_rip
                    || transaction.footprint.ptrace_completion_stop_rip
                        != transaction.footprint.relocated_tail
                    || siginfo.si_signo != libc::SIGTRAP
                    || !matches!(siginfo.si_code, libc::TRAP_BRKPT | libc::SI_KERNEL)
                    || !transaction
                        .footprint
                        .validates_ptrace_stop(task, registers.ip())
                    || !liteinst_completion_registers_match(
                        &transaction.entry_regs,
                        &registers,
                        transaction.footprint.ptrace_completion_stop_rip,
                    )
                    || task.getxstate()? != transaction.entry_xstate
                {
                    return Err(Errno::EPROTO.into());
                }
            }
        }
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn validate_liteinst_terminal_stop_resolution(
        outcome: safeptrace::StopResolutionOutcome,
    ) -> Result<(), TraceError> {
        match outcome {
            safeptrace::StopResolutionOutcome::Terminal { .. }
            | safeptrace::StopResolutionOutcome::ExitStop { .. }
            | safeptrace::StopResolutionOutcome::GenerationEnded {
                error: Errno::ECHILD | Errno::ESRCH | Errno::EIO,
            } => Ok(()),
            safeptrace::StopResolutionOutcome::Continued { .. }
            | safeptrace::StopResolutionOutcome::Stopped { .. }
            | safeptrace::StopResolutionOutcome::GenerationEnded { .. } => {
                Err(Errno::EPROTO.into())
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn resolve_liteinst_installed_sigstop(
        &mut self,
        task: Stopped,
    ) -> Result<Wait, TraceError> {
        if !self.liteinst_installed_root_is_quiescent(&task) {
            return Err(Errno::EPROTO.into());
        }
        let delivery_info = match task.getsiginfo() {
            Ok(info) if info.si_signo == libc::SIGSTOP => info,
            Ok(_) | Err(TraceError::Errno(Errno::EINVAL)) => {
                return Err(Errno::EPROTO.into());
            }
            Err(error) => return Err(error),
        };
        let _ = delivery_info;
        let delivery_status = task
            .physical_status_id()
            .ok_or(Errno::EPROTO)?
            .get();
        let delivery_logical_stop = task.logical_stop_id();
        let (
            prior,
            physical_generation,
            prior_status_floor,
            prior_logical_stop_floor,
            private_sigmask,
        ) = {
            let transaction = self
                .liteinst_installed_event
                .as_ref()
                .ok_or(Errno::EPROTO)?;
            (
                transaction.phase.private().ok_or(Errno::EPROTO)?,
                transaction.physical_generation,
                transaction.status_floor,
                transaction.logical_stop_floor,
                transaction.private_sigmask,
            )
        };
        if task.physical_event_generation() != physical_generation
            || delivery_status <= prior_status_floor
            || !delivery_logical_stop.is_strictly_after(prior_logical_stop_floor)
            || task.getsigmask()? != private_sigmask
        {
            return Err(Errno::EPROTO.into());
        }
        let snapshot = LiteinstStopResolutionSnapshot {
            delivery_logical_stop,
            delivery_status,
            registers: task.getregs()?,
            xstate: task.getxstate()?,
            private_sigmask,
        };
        let mut watcher = task.watch_stop_resolution()?;
        if watcher.delivery_logical_stop_id() != delivery_logical_stop
            || watcher.delivery_physical_status_id() != task.physical_status_id()
        {
            return Err(Errno::EPROTO.into());
        }
        {
            let transaction = self
                .liteinst_installed_event
                .as_mut()
                .ok_or(Errno::EPROTO)?;
            transaction.status_floor = delivery_status;
            transaction.logical_stop_floor = delivery_logical_stop;
            transaction.phase = LiteinstInstalledEventPhase::AwaitStopResolution {
                prior,
                snapshot: snapshot.clone(),
            };
        }

        let running = match self.resume_stopped(task, Signal::SIGSTOP) {
            Ok(running) => running,
            Err(error) => {
                if matches!(
                    &error,
                    TraceError::Died(_)
                        | TraceError::Errno(Errno::ESRCH | Errno::EIO)
                ) {
                    let boundary = watcher.terminal_before_group_stop().await?;
                    Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                    self.retire_liteinst_installed_event_on_terminal()?;
                    return future::pending::<Result<Wait, TraceError>>().await;
                }
                self.restore_liteinst_stop_resolution(
                    prior,
                    &snapshot,
                    delivery_status,
                    delivery_logical_stop,
                )?;
                return Err(error);
            }
        };
        let next = match running.next_state().await {
            Ok(next) => next,
            Err(error) => {
                if matches!(
                    &error,
                    TraceError::Died(_)
                        | TraceError::Errno(Errno::ESRCH | Errno::EIO)
                ) {
                    let boundary = watcher.terminal_before_group_stop().await?;
                    Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                    self.retire_liteinst_installed_event_on_terminal()?;
                    return future::pending::<Result<Wait, TraceError>>().await;
                }
                self.restore_liteinst_stop_resolution(
                    prior,
                    &snapshot,
                    delivery_status,
                    delivery_logical_stop,
                )?;
                return Err(error);
            }
        };
        self.arm_liteinst_wait(&next)?;
        let (next_task, next_event) = match next {
            Wait::Stopped(next_task, next_event) => (next_task, next_event),
            exited @ Wait::Exited(_, _) => {
                let boundary = watcher.terminal_before_group_stop().await?;
                Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                self.retire_liteinst_installed_event_on_terminal()?;
                return Ok(exited);
            }
        };
        let next_status = next_task
            .physical_status_id()
            .ok_or(Errno::EPROTO)?
            .get();
        let next_logical_stop = next_task.logical_stop_id();
        if next_task.physical_event_generation() != physical_generation
            || next_status <= delivery_status
            || !next_logical_stop.is_strictly_after(delivery_logical_stop)
        {
            return Err(Errno::EPROTO.into());
        }

        let group_stop = match next_task.getsiginfo() {
            Ok(info) if info.si_signo == libc::SIGTRAP => false,
            Ok(_) => return Err(Errno::EPROTO.into()),
            Err(TraceError::Errno(Errno::EINVAL)) => true,
            Err(error)
                if matches!(
                    &error,
                    TraceError::Died(_)
                        | TraceError::Errno(Errno::ESRCH | Errno::EIO)
                ) =>
            {
                let boundary = watcher
                    .terminal_before_group_acknowledgement(&next_task)
                    .await?;
                Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                self.retire_liteinst_installed_event_on_terminal()?;
                return future::pending::<Result<Wait, TraceError>>().await;
            }
            Err(error) => return Err(error),
        };
        if !group_stop {
            // Authenticate the exact private successor while the transaction
            // still retains AwaitStopResolution. Only after this pure check is
            // it safe to disarm the watcher and expose the same T once to the
            // ordinary runtime/completion handler.
            self.validate_cancelled_liteinst_installed_stop(
                &next_task,
                &next_event,
                prior,
                &snapshot,
            )?;
            watcher.complete_cancelled_delivery(&next_task)?;
            self.restore_liteinst_stop_resolution(
                prior,
                &snapshot,
                delivery_status,
                delivery_logical_stop,
            )?;
            return Ok(Wait::Stopped(next_task, next_event));
        }

        let retained_group_state = (|| -> Result<bool, TraceError> {
            Ok(next_event == Event::Signal(Signal::SIGSTOP)
                && next_task.getregs()? == snapshot.registers
                && next_task.getxstate()? == snapshot.xstate
                && next_task.getsigmask()? == snapshot.private_sigmask)
        })();
        match retained_group_state {
            Ok(true) => {}
            Ok(false) => return Err(Errno::EPROTO.into()),
            Err(error)
                if matches!(
                    &error,
                    TraceError::Died(_)
                        | TraceError::Errno(Errno::ESRCH | Errno::EIO)
                ) =>
            {
                let boundary = watcher
                    .terminal_before_group_acknowledgement(&next_task)
                    .await?;
                Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                self.retire_liteinst_installed_event_on_terminal()?;
                return future::pending::<Result<Wait, TraceError>>().await;
            }
            Err(error) => return Err(error),
        }
        let held_group_stop = self
            .liteinst_stop_slot(&next_task)
            .ok_or(Errno::EPROTO)?;
        let group_stop_pid = next_task.pid();
        watcher.acknowledge_group_stop(&next_task)?;
        match watcher.following_outcome().await? {
            safeptrace::StopResolutionOutcome::Continued { physical_status } => {
                let continued_status = physical_status.ok_or(Errno::EPROTO)?.get();
                if continued_status <= next_status {
                    return Err(Errno::EPROTO.into());
                }
                let retained_group_state = (|| -> Result<bool, TraceError> {
                    Ok(next_task.getregs()? == snapshot.registers
                        && next_task.getxstate()? == snapshot.xstate
                        && next_task.getsigmask()? == snapshot.private_sigmask)
                })();
                match retained_group_state {
                    Ok(true) => {}
                    Ok(false) => return Err(Errno::EPROTO.into()),
                    Err(error)
                        if matches!(
                            &error,
                            TraceError::Died(_)
                                | TraceError::Errno(Errno::ESRCH | Errno::EIO)
                        ) =>
                    {
                        let error = match error {
                            TraceError::Died(_) => Errno::ESRCH,
                            TraceError::Errno(error) => error,
                        };
                        let boundary = watcher.resolve_continued_probe_error(error).await?;
                        Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                        self.retire_liteinst_installed_event_on_terminal()?;
                        return future::pending::<Result<Wait, TraceError>>().await;
                    }
                    Err(error) => return Err(error),
                }
                self.restore_liteinst_stop_resolution(
                    prior,
                    &snapshot,
                    continued_status,
                    next_logical_stop,
                )?;
                if let Some(boundary) = watcher.begin_continued_resume()? {
                    Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                    self.retire_liteinst_installed_event_on_terminal()?;
                    return future::pending::<Result<Wait, TraceError>>().await;
                }
                let running = match self.resume_liteinst_group_stop_with_attempt(next_task) {
                    Ok((running, _attempt)) => {
                        if let Some(boundary) = watcher.complete_continued_resume()? {
                            Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                            self.retire_liteinst_installed_event_on_terminal()?;
                            return future::pending::<Result<Wait, TraceError>>().await;
                        }
                        running
                    }
                    Err((_, Some(attempt), Some(error @ (Errno::ESRCH | Errno::EIO)))) => {
                        match watcher
                            .resolve_continued_resume_error(error, attempt)
                            .await?
                        {
                            safeptrace::StopResolutionResumeErrorOutcome::Boundary(boundary) => {
                                Self::validate_liteinst_terminal_stop_resolution(boundary)?;
                                self.retire_liteinst_installed_event_on_terminal()?;
                                return future::pending::<Result<Wait, TraceError>>().await;
                            }
                            safeptrace::StopResolutionResumeErrorOutcome::LaterStatus(
                                resolution,
                            ) => {
                                let running = HeldRootStop::retire_causally_resolved_stop(
                                    &held_group_stop,
                                    group_stop_pid,
                                    resolution,
                                )?;
                                let wait = running.next_state().await?;
                                self.arm_liteinst_wait(&wait)?;
                                return Ok(wait);
                            }
                        }
                    }
                    Err((error, _, _)) => return Err(error),
                };
                let wait = running.next_state().await?;
                self.arm_liteinst_wait(&wait)?;
                Ok(wait)
            }
            outcome @ safeptrace::StopResolutionOutcome::GenerationEnded { .. } => {
                Self::validate_liteinst_terminal_stop_resolution(outcome)?;
                self.retire_liteinst_installed_event_on_terminal()?;
                future::pending::<Result<Wait, TraceError>>().await
            }
            safeptrace::StopResolutionOutcome::Terminal { .. }
            | safeptrace::StopResolutionOutcome::ExitStop { .. } => {
                self.retire_liteinst_installed_event_on_terminal()?;
                future::pending::<Result<Wait, TraceError>>().await
            }
            safeptrace::StopResolutionOutcome::Stopped { .. } => {
                self.restore_liteinst_stop_resolution(
                    prior,
                    &snapshot,
                    next_status,
                    next_logical_stop,
                )?;
                Err(Errno::EPROTO.into())
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    async fn dispatch_liteinst_installed_syscall(
        &mut self,
        mut task: Stopped,
        raw_number: u64,
        args: SyscallArgs,
        relocated_tail: u64,
    ) -> Result<Wait, TraceError> {
        self.liteinst_installed_resume_rip = Some(relocated_tail);
        let nr = match classify_liteinst_installed_syscall(raw_number) {
            LiteinstInstalledSyscallClass::X32 => return Err(Errno::EPROTO.into()),
            LiteinstInstalledSyscallClass::Unknown => {
                set_ret(&task, (-(Errno::ENOSYS.into_raw() as i64)) as u64)?;
                return self
                    .resume_liteinst_installed_tail(task, relocated_tail)
                    .await;
            }
            LiteinstInstalledSyscallClass::RtSigreturn => {
                // The kernel may restore an arbitrary user-selected RIP and has no
                // required post-syscall stop. Make every displaced instruction
                // real again before allowing that context transfer.
                self.deopt_liteinst_hooks_quiescent(
                    &task,
                    LiteinstDeoptProgramCounter::TranslateGenerated,
                )?;
                self.liteinst_installed_resume_rip = None;
                return self.resume_live_installed_rt_sigreturn(task, args).await;
            }
            LiteinstInstalledSyscallClass::Known(number) => number,
        };
        let mapping_syscall = is_liteinst_mapping_syscall(nr, args);
        if mapping_syscall {
            self.validate_liteinst_mapping_execution(nr, args)?;
        }
        let subscribed = self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|candidate| candidate == nr);
        if !subscribed {
            let (stopped, result) = self.untraced_syscall(task, nr, args).await?;
            task = stopped;
            set_ret(
                &task,
                result.unwrap_or_else(|errno| -(errno.into_raw() as i64)) as u64,
            )?;
            return self
                .resume_liteinst_installed_tail(task, relocated_tail)
                .await;
        }

        let syscall = Syscall::from_raw(nr, args);
        self.pending_syscall = Some((nr, args));
        self.pending_syscall_already_skipped = true;
        self.begin_tool_callback(task)?;
        self.observe_after_loader_tool_callback("Tool::handle_syscall_event(installed completion)")
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
            set_ret(&task, result as u64)?;
        }
        self.resume_liteinst_installed_tail(task, relocated_tail)
            .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn resume_live_installed_rt_sigreturn(
        &mut self,
        task: Stopped,
        args: SyscallArgs,
    ) -> Result<Wait, TraceError> {
        let mut registers = task.getregs()?;
        registers.rax = Sysno::rt_sigreturn as u64;
        registers.orig_rax = Sysno::rt_sigreturn as u64;
        registers.rip = cp::PRIVATE_PAGE_OFFSET as u64;
        self.arm_after_loader_syscall_permit(
            &task,
            after_loader_task::AfterLoaderSyscallPurpose::GuestRtSigreturn,
            Sysno::rt_sigreturn as i64,
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
        task.setregs(&registers)?;
        let wait = self.resume_stopped(task, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        let stopped = match wait {
            Wait::Stopped(stopped, Event::Seccomp) => stopped,
            _ => return Err(Errno::EPROTO.into()),
        };
        let permit = self
            .consume_after_loader_syscall_permit(&stopped)?
            .ok_or(Errno::EPROTO)?;
        let wait = self.resume_stopped(stopped, None)?.next_state().await?;
        self.arm_liteinst_wait(&wait)?;
        let successor = match &wait {
            Wait::Stopped(stopped, _) => self
                .complete_after_loader_syscall_successor(stopped)?
                .ok_or(Errno::EPROTO)?,
            Wait::Exited(_, _) => return Err(Errno::EPROTO.into()),
        };
        if successor != permit {
            return Err(Errno::EPROTO.into());
        }
        Ok(wait)
    }

    #[cfg(target_arch = "x86_64")]
    fn installed_runtime_frame_address(
        &self,
        task: &Stopped,
        registers: &libc::user_regs_struct,
    ) -> Option<usize> {
        self.installed_runtime_frame_address_for_phase(
            task,
            registers,
            LiteinstInstalledPrivatePhase::AwaitingRuntimeTrap,
            false,
        )
    }

    #[cfg(target_arch = "x86_64")]
    fn installed_runtime_frame_address_for_phase(
        &self,
        task: &Stopped,
        registers: &libc::user_regs_struct,
        expected: LiteinstInstalledPrivatePhase,
        resolving_delivery: bool,
    ) -> Option<usize> {
        let transaction = self.liteinst_installed_event.as_ref()?;
        let phase_matches = if resolving_delivery {
            matches!(
                &transaction.phase,
                LiteinstInstalledEventPhase::AwaitStopResolution { prior, .. }
                    if *prior == expected
            )
        } else {
            transaction.phase.private() == Some(expected)
        };
        if !phase_matches {
            return None;
        }
        let config = self.global_state.liteinst_runtime.as_ref()?;
        let (handshake, trap, runtime_mapping, generation, ready_generation) = {
            let state = self.liteinst_runtime.lock().ok()?;
            (
                state.frame?,
                state.after_loader_syscall_trap?,
                MappingIdentity::from_target_loader(
                    state.after_loader_reference.as_ref()?.1.mapping_identity,
                ),
                state.generation,
                state.ready_generation,
            )
        };
        if registers.ip() != handshake.syscall_trap_rip {
            return None;
        }
        let stack_address = usize::try_from(registers.rsp).ok()?;
        let frame_address = usize::try_from(registers.rdi).ok()?;
        let maps = guest_maps(task.pid())?;
        let trap_address = trap.rip.checked_sub(1)?;
        let trap_range = GuestRange::new(trap_address, 2)?;
        let trap_mapping = maps
            .iter()
            .find(|mapping| mapping.contains_range(trap_range))?;
        let trap_opcode: u8 = task
            .read_value(Addr::from_raw(trap_address as usize)?)
            .ok()?;
        if !trap.validates_retired_runtime_trap(
            generation,
            registers.rax,
            config.syscall_marker,
            registers.ip(),
            task.getsiginfo().ok()?.si_code,
            trap_opcode,
            runtime_mapping,
            trap_mapping,
        ) || ready_generation != Some(generation)
            || generation != transaction.generation
        {
            return None;
        }
        maps.iter().find(|mapping| {
            mapping.readable
                && mapping.writable
                && mapping.contains(registers.rsp)
                && mapping.contains(
                    registers
                        .rsp
                        .saturating_add(core::mem::size_of::<u64>() as u64 - 1),
                )
                && mapping.contains(registers.rdi)
                && mapping.contains(
                    registers
                        .rdi
                        .saturating_add(core::mem::size_of::<InjectedSyscallFrame>() as u64 - 1),
                )
        })?;
        if registers.rsp.abs_diff(registers.rdi) > 128 * 1024 {
            return None;
        }
        let return_address: u64 = task.read_value(Addr::from_raw(stack_address)?).ok()?;
        if return_address != handshake.syscall_trap_return_rip {
            return None;
        }
        let frame = self.read_injected_syscall_frame(task, frame_address).ok()?;
        let state = self.liteinst_runtime.lock().ok()?;
        (state.phase == LiteinstRuntimePhase::Ready
            && state.ready_generation == Some(transaction.generation)
            && state.generation == transaction.generation
            && state
                .active_hooks
                .get(&frame.instruction_pointer())
                .is_some_and(|footprint| footprint == &transaction.footprint))
        .then_some(frame_address)
    }

    fn classify_liteinst_trap(
        &mut self,
        task: &Stopped,
        regs: &libc::user_regs_struct,
    ) -> Option<LiteinstTrap> {
        let config = self.global_state.liteinst_runtime.as_ref()?;
        if self
            .liteinst_runtime
            .lock()
            .ok()?
            .frame
            .is_some_and(|frame| regs.ip() == frame.install_helper_rip)
        {
            // The synchronous helper loop consumes its armed entry trap
            // directly. Reaching the same permanent INT3 through the ordinary
            // signal path has no controller-owned arm and must never advance
            // into the temporarily executable helper body.
            return Some(LiteinstTrap::Invalid);
        }
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            if let Some(transaction) = self.liteinst_installed_event.as_ref() {
                if regs.ip() == transaction.footprint.ptrace_completion_stop_rip {
                    return matches!(
                        transaction.phase.private(),
                        Some(LiteinstInstalledPrivatePhase::AwaitingCompletion { .. })
                    )
                    .then_some(LiteinstTrap::InstalledCompletion)
                    .or(Some(LiteinstTrap::Invalid));
                }
            } else {
                let candidate = {
                    let state = self.liteinst_runtime.lock().ok()?;
                    if state.phase != LiteinstRuntimePhase::Ready
                        || state.ready_generation != Some(state.generation)
                        || state.frame.is_none()
                    {
                        None
                    } else {
                        state
                            .active_hooks
                            .values()
                            .find(|hook| hook.ptrace_entry_stop_rip == regs.ip())
                            .cloned()
                            .map(|footprint| (state.generation, footprint))
                    }
                };
                if let Some((generation, footprint)) = candidate {
                    let siginfo = task.getsiginfo().ok()?;
                    return (matches!(siginfo.si_code, libc::TRAP_BRKPT | libc::SI_KERNEL)
                        && footprint.validates_ptrace_stop(task, regs.ip()))
                    .then_some(LiteinstTrap::InstalledEntry {
                        generation,
                        footprint,
                    })
                    .or(Some(LiteinstTrap::Invalid));
                }
            }
        }
        if regs.rax == config.begin_marker {
            let frame =
                self.validate_liteinst_handshake(task, regs.rdi as usize, regs.ip(), false)?;
            let arena_baseline_maps = guest_maps(task.pid())?;
            #[cfg(target_arch = "x86_64")]
            let after_loader = self.after_loader_config().is_some();
            let mut state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Waiting {
                return None;
            }
            state.phase = LiteinstRuntimePhase::Bootstrap;
            state.frame = Some(frame);
            state.arena_baseline_maps = arena_baseline_maps;
            state.start_break = Some(frame.start_program_break);
            state.current_break = Some(frame.initial_program_break);
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
            let mut state = self.liteinst_runtime.lock().unwrap();
            if state.phase != LiteinstRuntimePhase::Bootstrap
                || !state
                    .frame
                    .is_some_and(|begin| same_liteinst_handshake_protocol(begin, frame))
            {
                return None;
            }
            state.frame = Some(frame);
            state.current_break = Some(frame.initial_program_break);
            return Some(LiteinstTrap::HandshakeReady);
        }
        if regs.rax != config.syscall_marker {
            #[cfg(target_arch = "x86_64")]
            if self.liteinst_installed_event.is_some() {
                return Some(LiteinstTrap::Invalid);
            }
            return None;
        }
        #[cfg(target_arch = "x86_64")]
        if self.after_loader_config().is_some() {
            let (phase, generation, trap, runtime_mapping, frame_bound) = {
                let state = self.liteinst_runtime.lock().unwrap();
                (
                    state.phase,
                    state.generation,
                    state.after_loader_syscall_trap,
                    state
                        .after_loader_reference
                        .as_ref()
                        .map(|(_, initializer)| {
                            MappingIdentity::from_target_loader(initializer.mapping_identity)
                        }),
                    state.frame.is_some(),
                )
            };
            if phase == LiteinstRuntimePhase::Ready && !frame_bound {
                // A retired after-loader runtime can no longer authenticate a
                // hook callback frame. Refuse only its exact retained trap;
                // an ordinary guest breakpoint carrying the marker remains
                // guest register state.
                let trap = trap?;
                let runtime_mapping = runtime_mapping?;
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
                        runtime_mapping,
                        mapping,
                    )
                    .then_some(LiteinstTrap::Invalid);
            }
            if self.liteinst_installed_event.is_some() {
                return self
                    .installed_runtime_frame_address(task, regs)
                    .map(LiteinstTrap::Syscall)
                    .or(Some(LiteinstTrap::Invalid));
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
                let current_maps = guest_maps(task.pid()).ok_or(Errno::EPROTO)?;
                let (arena_baseline_maps, frame) = {
                    let mut state = self.liteinst_runtime.lock().unwrap();
                    (
                        std::mem::take(&mut state.arena_baseline_maps),
                        state.frame.ok_or(Errno::EPROTO)?,
                    )
                };
                let (prepared_arenas, prepared_reservations) =
                    if let Some(state) = self.liteinst_after_loader_private_state.as_ref() {
                        state.prepared_liteinst_controls()
                    } else {
                        bind_prepared_liteinst_arenas(&arena_baseline_maps, &current_maps)
                    }
                    .ok_or(Errno::EPROTO)?;
                let helper_code = bind_liteinst_helper_code(&task, frame).ok_or(Errno::EPROTO)?;
                let (next, protection_result) = self
                    .set_liteinst_internal_protection(task, helper_code.range, libc::PROT_NONE)
                    .await?;
                task = next;
                if protection_result != Ok(0)
                    || !liteinst_helper_code_has_protection(&task, &helper_code, libc::PROT_NONE)
                    || !liteinst_helper_code_bytes_match(&task, &helper_code)
                {
                    return Err(Errno::EPROTO.into());
                }
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
                    state.prepared_arenas = prepared_arenas;
                    state.prepared_reservations = prepared_reservations;
                    state.helper_code = Some(helper_code);
                }
                return Ok(HandleSignalResult::SignalSuppressed(
                    self.resume_stopped(task, None)?.next_state().await?,
                ));
            }
            #[cfg(target_arch = "x86_64")]
            Some(LiteinstTrap::InstalledEntry {
                generation,
                footprint,
            }) => {
                let next = self
                    .begin_liteinst_installed_event(task, generation, footprint)
                    .await?;
                return Ok(HandleSignalResult::SignalSuppressed(next));
            }
            #[cfg(target_arch = "x86_64")]
            Some(LiteinstTrap::InstalledCompletion) => {
                let next = self.complete_liteinst_installed_event(task).await?;
                return Ok(HandleSignalResult::SignalSuppressed(next));
            }
            Some(LiteinstTrap::Syscall(frame_address)) => {
                #[cfg(target_arch = "x86_64")]
                let next_state = if self.liteinst_installed_event.is_some() {
                    self.advance_liteinst_installed_runtime_trap(task, frame_address)
                        .await?
                } else {
                    self.handle_injected_syscall(task, frame_address, regs.eflags)
                        .await?
                };
                #[cfg(not(target_arch = "x86_64"))]
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

        let software_breakpoint = regs
            .ip()
            .checked_sub(1)
            .filter(|address| self.breakpoints.contains_key(address));

        Ok(match unmatched_sigtrap_disposition(software_breakpoint, resumed_by_gdb_step) {
            UnmatchedSigtrapDisposition::SoftwareBreakpoint(address) => {
                *regs.ip_mut() = address;
                let next_state = self.resume_from_swbreak(task, regs).await?;
                HandleSignalResult::SignalSuppressed(next_state)
            }
            UnmatchedSigtrapDisposition::GdbStep => {
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
            }
            UnmatchedSigtrapDisposition::Deliver => {
                HandleSignalResult::SignalToDeliver(task, Signal::SIGTRAP)
            }
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
        if self
            .liteinst_runtime
            .lock()
            .unwrap()
            .helper_code
            .as_ref()
            .is_some_and(|helper| helper.range.start <= regs.rip && regs.rip < helper.range.end)
        {
            return Err(Errno::EPROTO.into());
        }
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
        task: &Stopped,
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
        #[cfg(target_arch = "x86_64")]
        if signal.is_some() {
            self.deopt_liteinst_hooks_quiescent(
                task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
        }
        Ok(signal)
    }

    #[cfg(target_arch = "x86_64")]
    fn virtualize_liteinst_timer_program_counter(
        &mut self,
        task: &Stopped,
    ) -> Result<Option<LiteinstEphemeralPcTranslation>, TraceError> {
        let Some(footprint) = self.liteinst_active_pc_footprint.as_ref() else {
            return Ok(None);
        };
        let mut registers = task.getregs()?;
        let generated_rip = registers.ip();
        let Some(logical_rip) = liteinst_logical_program_counter(
            core::slice::from_ref(footprint),
            generated_rip,
        )
        .map_err(|()| Errno::EPROTO)?
        else {
            self.liteinst_active_pc_footprint = None;
            return Ok(None);
        };
        let generation = self.liteinst_runtime.lock().unwrap().generation;
        registers.rip = logical_rip;
        task.setregs(&registers)?;
        Ok(Some(LiteinstEphemeralPcTranslation {
            generation,
            physical_generation: task.physical_event_generation(),
            logical_rip,
            generated_rip,
        }))
    }

    #[cfg(target_arch = "x86_64")]
    fn restore_liteinst_timer_program_counter(
        &self,
        task: &Stopped,
        translation: &LiteinstEphemeralPcTranslation,
    ) -> Result<(), TraceError> {
        let mut registers = task.getregs()?;
        if registers.ip() != translation.logical_rip
            || task.physical_event_generation() != translation.physical_generation
            || self.liteinst_runtime.lock().unwrap().generation != translation.generation
        {
            return Err(Errno::EPROTO.into());
        }
        registers.rip = translation.generated_rip;
        task.setregs(&registers)
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
        #[cfg(target_arch = "x86_64")]
        if sig == Signal::SIGSEGV {
            // CPUID/RDTSC immediately after the displaced syscall can fault in
            // the relocated tail. Retire the patch and translate RIP before
            // the instruction handler decodes bytes or exposes registers.
            self.deopt_liteinst_hooks_quiescent(
                &task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
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
                #[cfg(target_arch = "x86_64")]
                self.deopt_liteinst_hooks_quiescent(
                    &task,
                    LiteinstDeoptProgramCounter::TranslateGenerated,
                )?;
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
                #[cfg(target_arch = "x86_64")]
                {
                    self.liteinst_installed_resume_rip = None;
                }
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
            #[cfg(target_arch = "x86_64")]
            if self.liteinst_entry_guard_inspection.is_some() || self.liteinst_entry_guard_uncertain
            {
                return Err(Errno::EPROTO.into());
            }
            self.liteinst_runtime = Arc::new(StdMutex::new(next));
            self.liteinst_entry_guard = None;
            #[cfg(target_arch = "x86_64")]
            {
                self.liteinst_entry_guard_inspection = None;
                self.liteinst_entry_guard_uncertain = false;
                if self.liteinst_after_loader_syscall_permit.is_some()
                    || self.liteinst_after_loader_syscall_inflight.is_some()
                    || self.liteinst_after_loader_forward_inflight.is_some()
                    || self.liteinst_after_loader_private_call.is_some()
                    || self.liteinst_after_loader_private_state.is_some()
                    || self.liteinst_installed_event.is_some()
                    || self.liteinst_installed_resume_rip.is_some()
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
            self.arm_liteinst_wait(&wait)?;
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
            self.arm_liteinst_stop(&task, &event)?;
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

    #[cfg(target_arch = "x86_64")]
    fn liteinst_install_helper_is_quiescent(&self, task: &Stopped) -> bool {
        let Some(runtime) = self.global_state.liteinst_runtime.as_ref() else {
            return false;
        };
        runtime.root_tid.get() == Some(&task.pid())
            && task.pid() == self.tid()
            && !runtime.multi_task.load(Ordering::Acquire)
            && runtime.newborn_tracees.lock().unwrap().is_empty()
            && self.pending_signal.is_none()
            && process_has_exactly_one_task(task.pid())
    }

    #[cfg(target_arch = "x86_64")]
    fn liteinst_installed_root_is_quiescent(&self, task: &Stopped) -> bool {
        let Some(runtime) = self.global_state.liteinst_runtime.as_ref() else {
            return false;
        };
        runtime.after_loader.is_some()
            && runtime.root_tid.get() == Some(&task.pid())
            && task.pid() == self.tid()
            && task.continued_status_authority_is_live()
            && !runtime.multi_task.load(Ordering::Acquire)
            && runtime.newborn_tracees.lock().unwrap().is_empty()
            && process_has_exactly_one_task(task.pid())
            && process_has_exactly_one_task(task.pid())
    }

    #[cfg(target_arch = "x86_64")]
    fn liteinst_hook_deopt_is_quiescent(&self, task: &Stopped) -> bool {
        let Some(runtime) = self.global_state.liteinst_runtime.as_ref() else {
            return false;
        };
        runtime.root_tid.get() == Some(&task.pid())
            && task.pid() == self.tid()
            && !runtime.multi_task.load(Ordering::Acquire)
            && runtime.newborn_tracees.lock().unwrap().is_empty()
            && process_has_exactly_one_task(task.pid())
    }

    #[cfg(target_arch = "x86_64")]
    fn transition_liteinst_patch_word(
        task: &Stopped,
        site: GuestRange,
        expected: [u8; LITEINST_PATCH_WORD_BYTES as usize],
        replacement: [u8; LITEINST_PATCH_WORD_BYTES as usize],
    ) -> Result<(), TraceError> {
        let mut observed = [0_u8; LITEINST_PATCH_WORD_BYTES as usize];
        task.read_exact(site.start as usize, &mut observed)?;
        if observed != expected {
            return Err(Errno::EPROTO.into());
        }
        let address = AddrMut::<u64>::from_raw(site.start as usize).ok_or(Errno::EFAULT)?;
        let mut memory = task.memory();
        memory.write_value(address, &u64::from_ne_bytes(replacement))?;
        task.read_exact(site.start as usize, &mut observed)?;
        if observed != replacement {
            return Err(Errno::EIO.into());
        }
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn transition_liteinst_deopt_patch_word(
        task: &Stopped,
        word: &LiteinstDeoptPatchWord,
        transition: LiteinstDeoptPatchTransition,
    ) -> Result<(), TraceError> {
        match transition {
            LiteinstDeoptPatchTransition::RestoreOriginal => {
                Self::transition_liteinst_patch_word(task, word.site, word.patched, word.original)
            }
            LiteinstDeoptPatchTransition::RepatchIfRestored => {
                let mut observed = [0_u8; LITEINST_PATCH_WORD_BYTES as usize];
                task.read_exact(word.site.start as usize, &mut observed)?;
                if observed == word.patched {
                    Ok(())
                } else if observed == word.original {
                    Self::transition_liteinst_patch_word(
                        task,
                        word.site,
                        word.original,
                        word.patched,
                    )
                } else {
                    Err(Errno::EIO.into())
                }
            }
        }
    }

    /// Retire every direct hook while the exact process consists only of this
    /// stopped root task. This is the non-resuming boundary needed before a
    /// guest signal frame can expose a logical PC, or before clone can create
    /// another task that might concurrently fetch a patched site.
    #[cfg(target_arch = "x86_64")]
    fn deopt_liteinst_hooks_quiescent(
        &mut self,
        task: &Stopped,
        program_counter: LiteinstDeoptProgramCounter,
    ) -> Result<(), TraceError> {
        let (generation, active_hooks, mut hooks) = {
            let state = self.liteinst_runtime.lock().unwrap();
            if state.active_hooks.is_empty() {
                self.liteinst_active_pc_footprint = None;
                return Ok(());
            }
            if state.phase != LiteinstRuntimePhase::Ready
                || state.ready_generation != Some(state.generation)
            {
                return Err(Errno::EPROTO.into());
            }
            (
                state.generation,
                state.active_hooks.clone(),
                state.active_hooks.values().cloned().collect::<Vec<_>>(),
            )
        };
        if !self.liteinst_hook_deopt_is_quiescent(task) {
            return Err(Errno::EPROTO.into());
        }
        hooks.sort_unstable_by_key(|hook| hook.site.start);
        let patch_words = hooks
            .iter()
            .map(|hook| LiteinstDeoptPatchWord {
                site: hook.site,
                patched: hook.expected_site_word,
                original: hook.original_site_word,
            })
            .collect::<Vec<_>>();
        let maps = guest_maps(task.pid()).ok_or(Errno::EFAULT)?;
        let registers = task.getregs()?;
        let generated_rip = registers.ip();
        for hook in &hooks {
            if hook.original_site_word == hook.expected_site_word
                || !liteinst_trampoline_code_bytes_match(task, hook)
                || !liteinst_arena_alias_has_protection(
                    task,
                    hook.arena_writable,
                    libc::PROT_NONE,
                )
                || !liteinst_arena_alias_has_protection(
                    task,
                    hook.arena_executable,
                    libc::PROT_READ | libc::PROT_EXEC,
                )
                || !maps.iter().any(|mapping| {
                    mapping.readable
                        && !mapping.writable
                        && mapping.executable
                        && !mapping.shared
                        && mapping.contains_range(hook.site)
                })
            {
                return Err(Errno::EPROTO.into());
            }
        }
        let logical_rip = liteinst_deopt_program_counter(&hooks, generated_rip, program_counter)
            .map_err(|()| Errno::EPROTO)?;
        if self
            .liteinst_active_pc_footprint
            .as_ref()
            .is_some_and(|footprint| active_hooks.get(&footprint.site.start) != Some(footprint))
        {
            return Err(Errno::EPROTO.into());
        }

        resolve_liteinst_deopt_patch_result(transition_liteinst_deopt_patch_words(
            &patch_words,
            |word, transition| {
                Self::transition_liteinst_deopt_patch_word(task, word, transition)
            },
        ))?;
        {
            let mut state = self.liteinst_runtime.lock().unwrap();
            if commit_liteinst_deopt_state(&mut state, generation, &active_hooks, &hooks).is_err() {
                let rollback_succeeded = rollback_liteinst_deopt_patch_words(
                    &patch_words,
                    &mut |word, transition| {
                        Self::transition_liteinst_deopt_patch_word(task, word, transition)
                    },
                );
                return if rollback_succeeded {
                    Err(Errno::EPROTO.into())
                } else {
                    Err(Errno::EIO.into())
                };
            }
        }
        self.liteinst_active_pc_footprint = None;
        if let Some(logical_rip) = logical_rip {
            let mut logical = registers;
            logical.rip = logical_rip;
            task.setregs(&logical)?;
            if liteinst_register_words(&task.getregs()?) != liteinst_register_words(&logical) {
                return Err(Errno::EIO.into());
            }
        }
        Ok(())
    }

    #[cfg(target_arch = "x86_64")]
    fn arm_liteinst_install_helper(
        &self,
        task: &Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
        request: LiteinstInstallRequest,
    ) -> Option<LiteinstInstallHelperArm> {
        if !self.liteinst_install_helper_is_quiescent(task) {
            return None;
        }
        let entry_range = GuestRange::new(frame.install_helper, 16)?;
        let maps = guest_maps(task.pid())?;
        let code_mapping = maps.into_iter().find(|mapping| {
            mapping.readable
                && !mapping.writable
                && mapping.executable
                && !mapping.shared
                && mapping.contains_range(entry_range)
                && mapping.contains(frame.install_helper_rip)
        })?;
        let mut entry_bytes = [0_u8; 16];
        task.read_exact(frame.install_helper as usize, &mut entry_bytes)
            .ok()?;
        if entry_bytes[0] != 0xcc {
            return None;
        }
        let state = self.liteinst_runtime.lock().ok()?;
        if state.phase != LiteinstRuntimePhase::Ready
            || state.ready_generation != Some(state.generation)
            || state.frame != Some(frame)
            || state.prepared_arenas.is_empty()
            || !state.prepared_arenas.iter().all(|arena| {
                liteinst_arena_alias_has_protection(
                    task,
                    arena.writable,
                    libc::PROT_READ | libc::PROT_WRITE,
                ) && liteinst_arena_alias_has_protection(
                    task,
                    arena.executable,
                    libc::PROT_READ | libc::PROT_EXEC,
                )
            })
            || !liteinst_active_trampoline_bytes_match(task, &state.active_hooks)
        {
            return None;
        }
        let helper_code = state.helper_code.clone()?;
        if !liteinst_helper_code_has_protection(
            task,
            &helper_code,
            libc::PROT_READ | libc::PROT_EXEC,
        ) || !liteinst_helper_code_bytes_match(task, &helper_code)
        {
            return None;
        }
        Some(LiteinstInstallHelperArm {
            tid: task.pid(),
            generation: state.generation,
            origin_status: task.physical_status_id()?.get(),
            entry: frame.install_helper,
            entry_rip: frame.install_helper_rip,
            site,
            stack_pointer: frame.helper_stack_top.checked_sub(8)?,
            return_address: frame.helper_return,
            request_address: frame.install_request,
            request,
            code_mapping,
            helper_code,
            entry_bytes,
        })
    }

    #[cfg(target_arch = "x86_64")]
    fn consume_liteinst_install_helper_arm(
        &self,
        task: &Stopped,
        arm: LiteinstInstallHelperArm,
    ) -> bool {
        if !self.liteinst_install_helper_is_quiescent(task) {
            return false;
        }
        let Ok(regs) = task.getregs() else {
            return false;
        };
        let Ok(siginfo) = task.getsiginfo() else {
            return false;
        };
        let Some(entry_address) = Addr::from_raw(arm.entry as usize) else {
            return false;
        };
        let Ok(entry_opcode) = task.read_value(entry_address) else {
            return false;
        };
        let Some(request_address) = Addr::from_raw(arm.request_address as usize) else {
            return false;
        };
        let Ok(request) = task.read_value(request_address) else {
            return false;
        };
        let Some(stack_address) = Addr::<u64>::from_raw(arm.stack_pointer as usize) else {
            return false;
        };
        let Ok(return_address) = task.read_value(stack_address) else {
            return false;
        };
        let mut entry_bytes = [0_u8; 16];
        if task
            .read_exact(arm.entry as usize, &mut entry_bytes)
            .is_err()
        {
            return false;
        }
        let state = self.liteinst_runtime.lock().unwrap();
        let generation = state.generation;
        let ready = state.phase == LiteinstRuntimePhase::Ready
            && state.ready_generation == Some(generation)
            && !state.prepared_arenas.is_empty()
            && state.prepared_arenas.iter().all(|arena| {
                liteinst_arena_alias_has_protection(
                    task,
                    arena.writable,
                    libc::PROT_READ | libc::PROT_WRITE,
                ) && liteinst_arena_alias_has_protection(
                    task,
                    arena.executable,
                    libc::PROT_READ | libc::PROT_EXEC,
                )
            })
            && liteinst_active_trampoline_bytes_match(task, &state.active_hooks);
        drop(state);
        ready
            && return_address == arm.return_address
            && entry_bytes == arm.entry_bytes
            && guest_maps(task.pid()).is_some_and(|maps| maps.contains(&arm.code_mapping))
            && liteinst_helper_code_has_protection(
                task,
                &arm.helper_code,
                libc::PROT_READ | libc::PROT_EXEC,
            )
            && liteinst_helper_code_bytes_match(task, &arm.helper_code)
            && liteinst_helper_entry_scalars_match(
                &arm,
                task.pid(),
                generation,
                task.physical_status_id().map(|status| status.get()),
                &regs,
                siginfo.si_code,
                entry_opcode,
                request,
            )
    }

    #[cfg(target_arch = "x86_64")]
    fn clear_liteinst_install_request(
        &self,
        task: &mut Stopped,
        frame: LiteinstHandshakeFrame,
    ) -> Result<(), TraceError> {
        let write = AddrMut::from_raw(frame.install_request as usize).ok_or(Errno::EFAULT)?;
        let read = Addr::from_raw(frame.install_request as usize).ok_or(Errno::EFAULT)?;
        task.write_value(write, &LiteinstInstallRequest::default())?;
        let cleared: LiteinstInstallRequest = task.read_value(read)?;
        if cleared != LiteinstInstallRequest::default() {
            return Err(Errno::EIO.into());
        }
        Ok(())
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
                    (result.version == 4
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
                let retained = match outcome {
                    LiteinstPatchOutcome::PtraceStraddlerBail => {
                        LiteinstRetainedFallback::CachelineStraddler
                    }
                    LiteinstPatchOutcome::PtraceOtherFallback => {
                        LiteinstRetainedFallback::UnpatchableOrOther
                    }
                    LiteinstPatchOutcome::DirectPunPatched
                    | LiteinstPatchOutcome::RelocatedPatched => {
                        unreachable!("fallback accounting received a patched outcome")
                    }
                };
                runtime.fallback_sites.insert(site, retained);
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
            Some(LiteinstRetainedFallback::CachelineStraddler) => {
                stats.record_cacheline_straddler_fallback();
            }
            Some(LiteinstRetainedFallback::UnpatchableOrOther) => {
                stats.record_unpatchable_or_other_fallback();
            }
            Some(LiteinstRetainedFallback::Deoptimized) => {
                stats.record_deoptimized_fallback();
            }
            None => {}
        });
    }

    fn validate_liteinst_install_result(
        &self,
        task: &Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
        original_site_word: [u8; LITEINST_PATCH_WORD_BYTES as usize],
        expected_site_word: [u8; LITEINST_PATCH_WORD_BYTES as usize],
    ) -> Option<(u64, ActiveHookFootprint)> {
        let address = Addr::from_raw(frame.install_result as usize)?;
        let result: LiteinstInstallResult = task.read_value(address).ok()?;
        let instruction_len = usize::try_from(result.instruction_len).ok()?;
        let straddle_prefix = usize::try_from(result.straddle_prefix).ok()?;
        let program_counter_count = usize::try_from(result.program_counter_count).ok()?;
        if result.version != 4
            || result.complete != 1
            || result.site_start != site
            || result.site_len != 8
            || !(1..=15).contains(&instruction_len)
            || straddle_prefix >= instruction_len.min(5)
            || !(1..=LITEINST_INSTALL_PC_MAPPINGS).contains(&program_counter_count)
            || result.trampoline_code_len > LITEINST_MAX_TRAMPOLINE_CODE_BYTES
            || result.program_counters[program_counter_count..]
                .iter()
                .any(|mapping| *mapping != LiteinstProgramCounterMapping::default())
        {
            return None;
        }
        let site = GuestRange::new(result.site_start, result.site_len)?;
        let trampoline = GuestRange::new(result.trampoline_start, result.trampoline_len)?;
        let trampoline_code = GuestRange::new(result.trampoline_start, result.trampoline_code_len)?;
        let entry_stop_opcode = GuestRange::new(result.ptrace_entry_stop_rip.checked_sub(1)?, 1)?;
        let completion_stop_opcode =
            GuestRange::new(result.ptrace_completion_stop_rip.checked_sub(1)?, 1)?;
        let arena_writable =
            GuestRange::new(result.arena_writable_start, result.arena_writable_len)?;
        let arena_executable =
            GuestRange::new(result.arena_executable_start, result.arena_executable_len)?;
        if !arena_executable.contains(trampoline)
            || !trampoline.contains(trampoline_code)
            || !trampoline_code.contains(entry_stop_opcode)
            || !trampoline_code.contains(completion_stop_opcode)
            || !trampoline_code.contains(GuestRange::new(result.relocated_tail, 1)?)
            || result.ptrace_entry_stop_rip != result.trampoline_start.checked_add(1)?
            || result.ptrace_completion_stop_rip != result.relocated_tail
            || result.ptrace_entry_stop_rip >= result.ptrace_completion_stop_rip
        {
            return None;
        }
        let program_counters = &result.program_counters[..program_counter_count];
        let mut previous_end = None;
        for mapping in program_counters {
            let generated = GuestRange::new(
                mapping.generated_start,
                mapping.generated_end.checked_sub(mapping.generated_start)?,
            )?;
            if !trampoline_code.contains(generated)
                || mapping.logical_address < site.start
                || mapping.logical_address > site.end
                || previous_end.is_some_and(|end| mapping.generated_start < end)
            {
                return None;
            }
            previous_end = Some(mapping.generated_end);
        }
        let logical_tail = result
            .site_start
            .checked_add(cp::SYSCALL_INSTR_SIZE as u64)?;
        if !program_counters
            .iter()
            .any(|mapping| mapping.translate(result.relocated_tail) == Some(logical_tail))
        {
            return None;
        }
        let entry_stop_address = Addr::from_raw(entry_stop_opcode.start as usize)?;
        let completion_stop_address = Addr::from_raw(completion_stop_opcode.start as usize)?;
        let entry_opcode: u8 = task.read_value(entry_stop_address).ok()?;
        let completion_opcode: u8 = task.read_value(completion_stop_address).ok()?;
        if entry_opcode != 0xcc || completion_opcode != 0xcc {
            return None;
        }
        let maps = guest_maps(task.pid())?;
        let site_map = maps.iter().find(|mapping| {
            mapping.readable
                && !mapping.writable
                && mapping.executable
                && !mapping.shared
                && mapping.contains_range(site)
        })?;
        if !guest_mapping_allows_forked_hook(task.pid(), site_map) {
            return None;
        }
        let writable_map = maps.iter().find(|mapping| {
            mapping.start == arena_writable.start
                && mapping.end == arena_writable.end
                && mapping.offset == 0
                && mapping.inode != 0
                && mapping.shared
                && !mapping.readable
                && !mapping.writable
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
        let mut trampoline_code_bytes =
            vec![0_u8; usize::try_from(result.trampoline_code_len).ok()?];
        task.read_exact(
            result.trampoline_start as usize,
            &mut trampoline_code_bytes,
        )
        .ok()?;
        if !self
            .liteinst_runtime
            .lock()
            .ok()?
            .prepared_arenas
            .iter()
            .any(|prepared| {
                prepared.writable == arena_writable && prepared.executable == arena_executable
            })
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
                original_site_word,
                expected_site_word,
                trampoline,
                trampoline_code,
                trampoline_code_bytes,
                ptrace_entry_stop_rip: result.ptrace_entry_stop_rip,
                ptrace_completion_stop_rip: result.ptrace_completion_stop_rip,
                relocated_tail: result.relocated_tail,
                program_counters: program_counters.to_vec(),
                arena_writable,
                arena_executable,
            },
        ))
    }

    #[cfg(target_arch = "x86_64")]
    async fn set_liteinst_internal_protection(
        &mut self,
        task: Stopped,
        range: GuestRange,
        value: i32,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        let start = usize::try_from(range.start).map_err(|_| Errno::EOVERFLOW)?;
        let length = usize::try_from(range.end - range.start).map_err(|_| Errno::EOVERFLOW)?;
        self.untraced_syscall_with_mapping_observation(
            task,
            Sysno::mprotect,
            SyscallArgs::new(start, length, value as usize, 0, 0, 0),
            false,
            true,
        )
        .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn set_liteinst_arena_writable_protection(
        &mut self,
        mut task: Stopped,
        arenas: &[PreparedArenaFootprint],
        protection: i32,
    ) -> Result<(Stopped, bool), TraceError> {
        let mut exact = !arenas.is_empty();
        for arena in arenas {
            let (next, result) = self
                .set_liteinst_internal_protection(task, arena.writable, protection)
                .await?;
            task = next;
            exact &= result == Ok(0)
                && liteinst_arena_alias_has_protection(&task, arena.writable, protection)
                && liteinst_arena_alias_has_protection(
                    &task,
                    arena.executable,
                    libc::PROT_READ | libc::PROT_EXEC,
                );
        }
        Ok((task, exact))
    }

    #[cfg(target_arch = "x86_64")]
    async fn set_liteinst_patch_source_protection(
        &mut self,
        task: Stopped,
        protection: GuestRange,
        value: i32,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        self.set_liteinst_internal_protection(task, protection, value)
            .await
    }

    #[cfg(target_arch = "x86_64")]
    async fn restore_liteinst_helper_and_source(
        &mut self,
        task: Stopped,
        saved: &LiteinstHelperSavedState,
        frame: LiteinstHandshakeFrame,
        protection: GuestRange,
        original_mapping: &GuestMap,
        helper_code: &LiteinstHelperCode,
        prior_words: &[LiteinstPatchWordSnapshot],
        helper_was_executable: bool,
    ) -> Result<(Stopped, Vec<String>), Error> {
        let (prepared_arenas, active_hooks) = {
            let state = self.liteinst_runtime.lock().unwrap();
            (state.prepared_arenas.clone(), state.active_hooks.clone())
        };
        let (task, aliases_isolated) = self
            .set_liteinst_arena_writable_protection(
                task,
                &prepared_arenas,
                libc::PROT_NONE,
            )
            .await
            .map_err(Error::Internal)?;
        if !aliases_isolated {
            return Err(Error::runtime(
                self.tid(),
                "isolate LiteInst trampoline writable aliases",
                "PROT_NONE transition or exact alias readback differed",
            ));
        }
        let (task, result) = self
            .set_liteinst_patch_source_protection(
                task,
                protection,
                libc::PROT_READ | libc::PROT_EXEC,
            )
            .await
            .map_err(Error::Internal)?;
        if result != Ok(0) || !liteinst_source_is_exactly_restored(&task, original_mapping) {
            return Err(Error::runtime(
                self.tid(),
                "restore LiteInst patch-source protection",
                format!("mprotect result {result:?} or exact source-map readback differed"),
            ));
        }
        let mut task = task;
        if helper_was_executable {
            let (next, result) = self
                .set_liteinst_internal_protection(task, helper_code.range, libc::PROT_NONE)
                .await
                .map_err(Error::Internal)?;
            task = next;
            if result != Ok(0)
                || !liteinst_helper_code_has_protection(&task, helper_code, libc::PROT_NONE)
                || !liteinst_helper_code_bytes_match(&task, helper_code)
            {
                return Err(Error::runtime(
                    self.tid(),
                    "restore LiteInst patch-helper code protection",
                    format!("mprotect result {result:?} or exact helper-map readback differed"),
                ));
            }
        } else if !liteinst_helper_code_has_protection(&task, helper_code, libc::PROT_NONE)
            || !liteinst_helper_code_bytes_match(&task, helper_code)
        {
            return Err(Error::runtime(
                self.tid(),
                "verify LiteInst patch-helper code protection",
                "the unarmed helper body was not PROT_NONE",
            ));
        }

        let words_match = liteinst_patch_words_match(&task, prior_words);
        let trampoline_bytes_match =
            liteinst_active_trampoline_bytes_match(&task, &active_hooks);
        let clear_result = self.clear_liteinst_install_request(&mut task, frame);
        let (task, rollback_failures) = self
            .restore_liteinst_helper_state(task, saved)
            .await
            .map_err(Error::Internal)?;
        if !words_match || !trampoline_bytes_match || clear_result.is_err() {
            let original = Error::runtime(
                self.tid(),
                "authenticate LiteInst helper cleanup",
                format!(
                    "prior patch words unchanged={words_match}, prior trampoline bytes unchanged={trampoline_bytes_match}, request clear/readback={clear_result:?}"
                ),
            );
            return Err(self.liteinst_helper_failure(original, rollback_failures));
        }
        Ok((task, rollback_failures))
    }

    #[cfg(target_arch = "x86_64")]
    async fn call_liteinst_install_helper(
        &mut self,
        task: Stopped,
        frame: LiteinstHandshakeFrame,
        site: u64,
        request: LiteinstInstallRequest,
        protection: GuestRange,
        original_mapping: GuestMap,
    ) -> Result<(Stopped, Option<(u64, ActiveHookFootprint)>), Error> {
        let helper_return_marker = self
            .global_state
            .liteinst_runtime
            .as_ref()
            .ok_or(Errno::EIO)?
            .helper_return_marker;
        if !self.liteinst_install_helper_is_quiescent(&task) {
            self.record_liteinst_fallback_stats(&task, frame, site);
            return Ok((task, None));
        }
        let new_site = GuestRange::new(site, LITEINST_PATCH_WORD_BYTES).ok_or(Errno::EOVERFLOW)?;
        let (helper_code, prior_words, prepared_arenas) = {
            let state = self.liteinst_runtime.lock().unwrap();
            let Some(helper_code) = state.helper_code.clone() else {
                drop(state);
                self.record_liteinst_fallback_stats(&task, frame, site);
                return Ok((task, None));
            };
            if helper_code.range.overlaps(protection)
                || !liteinst_helper_code_has_protection(&task, &helper_code, libc::PROT_NONE)
                || !liteinst_helper_code_bytes_match(&task, &helper_code)
                || state.prepared_arenas.is_empty()
                || !state.prepared_arenas.iter().all(|arena| {
                    liteinst_arena_alias_has_protection(
                        &task,
                        arena.writable,
                        libc::PROT_NONE,
                    ) && liteinst_arena_alias_has_protection(
                        &task,
                        arena.executable,
                        libc::PROT_READ | libc::PROT_EXEC,
                    )
                })
                || !liteinst_active_trampoline_bytes_match(&task, &state.active_hooks)
            {
                drop(state);
                self.record_liteinst_fallback_stats(&task, frame, site);
                return Ok((task, None));
            }
            let Some(prior_words) =
                snapshot_liteinst_active_patch_words(&task, &state, new_site, protection)
            else {
                drop(state);
                self.record_liteinst_fallback_stats(&task, frame, site);
                return Ok((task, None));
            };
            (helper_code, prior_words, state.prepared_arenas.clone())
        };
        let saved_regs = task.getregs()?;
        let saved_xstate = task.getxstate()?;
        let stack_address = frame.helper_stack_top.saturating_sub(8) as usize;
        let stack_read_address = Addr::from_raw(stack_address).ok_or(Errno::EFAULT)?;
        let stack_write_address = AddrMut::from_raw(stack_address).ok_or(Errno::EFAULT)?;
        let request_write =
            AddrMut::from_raw(frame.install_request as usize).ok_or(Errno::EFAULT)?;
        let request_read = Addr::from_raw(frame.install_request as usize).ok_or(Errno::EFAULT)?;
        let result_write = AddrMut::from_raw(frame.install_result as usize).ok_or(Errno::EFAULT)?;
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
        let (next, protection_result) = self
            .set_liteinst_patch_source_protection(
                task,
                protection,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            )
            .await
            .map_err(Error::Internal)?;
        task = next;
        if protection_result != Ok(0) {
            let controls_intact = liteinst_source_is_exactly_restored(&task, &original_mapping)
                && liteinst_helper_code_has_protection(&task, &helper_code, libc::PROT_NONE)
                && liteinst_helper_code_bytes_match(&task, &helper_code)
                && liteinst_patch_words_match(&task, &prior_words);
            let (stopped, rollback) = self
                .restore_liteinst_helper_state(task, &saved)
                .await
                .map_err(Error::Internal)?;
            if !controls_intact || !rollback.is_empty() {
                let original = Error::runtime(
                    self.tid(),
                    "prepare LiteInst patch-source protection",
                    format!(
                        "mprotect refused with {protection_result:?}; unchanged controls={controls_intact}"
                    ),
                );
                return Err(self.liteinst_helper_failure(original, rollback));
            }
            self.record_liteinst_fallback_stats(&stopped, frame, site);
            return Ok((stopped, None));
        }
        if !liteinst_temporary_source_is_rwx(&task, &original_mapping, protection)
            || !liteinst_patch_words_match(&task, &prior_words)
        {
            let original = Error::runtime(
                self.tid(),
                "prepare LiteInst patch-source protection",
                "temporary RWX map or an authenticated prior patch word differed",
            );
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    false,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }
        let (next, helper_protection_result) = self
            .set_liteinst_internal_protection(
                task,
                helper_code.range,
                libc::PROT_READ | libc::PROT_EXEC,
            )
            .await
            .map_err(Error::Internal)?;
        task = next;
        if helper_protection_result != Ok(0)
            || !liteinst_helper_code_has_protection(
                &task,
                &helper_code,
                libc::PROT_READ | libc::PROT_EXEC,
            )
            || !liteinst_helper_code_bytes_match(&task, &helper_code)
            || !liteinst_patch_words_match(&task, &prior_words)
        {
            let original = Error::runtime(
                self.tid(),
                "arm LiteInst patch-helper code",
                format!(
                    "mprotect result {helper_protection_result:?}, exact helper/source controls differed"
                ),
            );
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }
        if let Err(error) = task.write_value(request_write, &request) {
            let original = Error::from(error);
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }
        let request_readback: LiteinstInstallRequest = match task.read_value(request_read) {
            Ok(request) => request,
            Err(error) => {
                let original = Error::from(error);
                return match self
                    .restore_liteinst_helper_and_source(
                        task,
                        &saved,
                        frame,
                        protection,
                        &original_mapping,
                        &helper_code,
                        &prior_words,
                        true,
                    )
                    .await
                {
                    Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                    Err(error) => Err(error),
                };
            }
        };
        if request_readback != request {
            let original = Error::runtime(
                self.tid(),
                "bind LiteInst patch-helper request",
                "request write/readback differed",
            );
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }
        if let Err(error) = task.write_value(result_write, &LiteinstInstallResult::default()) {
            let original = Error::from(error);
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }
        if let Err(error) = task.write_value(stack_write_address, &frame.helper_return) {
            let original = Error::from(error);
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }

        let (next, aliases_writable) = self
            .set_liteinst_arena_writable_protection(
                task,
                &prepared_arenas,
                libc::PROT_READ | libc::PROT_WRITE,
            )
            .await
            .map_err(Error::Internal)?;
        task = next;
        if !aliases_writable {
            let original = Error::runtime(
                self.tid(),
                "open LiteInst trampoline writable aliases",
                "RW transition or exact alias readback differed",
            );
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }

        let mut helper_regs = saved.regs;
        *helper_regs.ip_mut() = frame.install_helper;
        *helper_regs.stack_ptr_mut() = frame.helper_stack_top - 8;
        helper_regs.rdi = site;
        *helper_regs.orig_syscall_mut() = -1_i64 as u64;
        helper_regs.eflags = liteinst_helper_entry_rflags(saved.regs.eflags);
        if let Err(error) = task.setregs(&helper_regs) {
            let original = Error::Internal(error);
            return match self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await
            {
                Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                Err(error) => Err(error),
            };
        }

        let Some(arm) = self.arm_liteinst_install_helper(&task, frame, site, request) else {
            let (stopped, rollback) = self
                .restore_liteinst_helper_and_source(
                    task,
                    &saved,
                    frame,
                    protection,
                    &original_mapping,
                    &helper_code,
                    &prior_words,
                    true,
                )
                .await?;
            if !rollback.is_empty() {
                let original = Error::runtime(
                    self.tid(),
                    "arm LiteInst patch helper",
                    "the sole-task helper authority changed before its entry trap",
                );
                return Err(self.liteinst_helper_failure(original, rollback));
            }
            self.record_liteinst_fallback_stats(&stopped, frame, site);
            return Ok((stopped, None));
        };

        let running = match self.resume_stopped(task, None) {
            Ok(running) => running,
            Err(error) => {
                return Err(Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    format!(
                        "entry resume failed before a replacement stopped capability was observed; source/helper protections remain armed until terminal teardown: {error}"
                    ),
                ));
            }
        };
        let wait = match running.next_state().await {
            Ok(wait) => wait,
            Err(error) => {
                return Err(Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    format!(
                        "entry wait failed before a replacement stopped capability was observed; source/helper protections remain armed until terminal teardown: {error}"
                    ),
                ));
            }
        };
        self.arm_liteinst_wait(&wait)?;
        let entry_task = match wait {
            Wait::Stopped(stopped, Event::Signal(Signal::SIGTRAP)) => {
                if !self.consume_liteinst_install_helper_arm(&stopped, arm) {
                    let original = Error::runtime(
                        self.tid(),
                        "consume LiteInst patch-helper entry authority",
                        "entry trap identity, request, code, mapping, or sole-task state differed",
                    );
                    return match self
                        .restore_liteinst_helper_and_source(
                            stopped,
                            &saved,
                            frame,
                            protection,
                            &original_mapping,
                            &helper_code,
                            &prior_words,
                            true,
                        )
                        .await
                    {
                        Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                        Err(error) => Err(error),
                    };
                }
                stopped
            }
            Wait::Stopped(stopped, Event::Seccomp) => {
                let stopped = self.skip_seccomp_syscall(stopped).await?;
                let original = Error::runtime(
                    self.tid(),
                    "consume LiteInst patch-helper entry authority",
                    "the guarded entry attempted a syscall before its INT3",
                );
                return match self
                    .restore_liteinst_helper_and_source(
                        stopped,
                        &saved,
                        frame,
                        protection,
                        &original_mapping,
                        &helper_code,
                        &prior_words,
                        true,
                    )
                    .await
                {
                    Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                    Err(error) => Err(error),
                };
            }
            Wait::Stopped(stopped, event) => {
                let original = Error::runtime(
                    self.tid(),
                    "consume LiteInst patch-helper entry authority",
                    format!("unexpected guarded-entry event: {event:?}"),
                );
                return match self
                    .restore_liteinst_helper_and_source(
                        stopped,
                        &saved,
                        frame,
                        protection,
                        &original_mapping,
                        &helper_code,
                        &prior_words,
                        true,
                    )
                    .await
                {
                    Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                    Err(error) => Err(error),
                };
            }
            Wait::Exited(_, exit_status) => self.exit(exit_status).await,
        };
        let entry_status = entry_task
            .physical_status_id()
            .expect("authenticated helper entry has a physical status")
            .get();
        let running = match self.resume_stopped(entry_task, None) {
            Ok(running) => running,
            Err(error) => {
                return Err(Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    format!(
                        "body resume failed before a replacement stopped capability was observed; source/helper protections remain armed until terminal teardown: {error}"
                    ),
                ));
            }
        };
        let wait = match running.next_state().await {
            Ok(wait) => wait,
            Err(error) => {
                return Err(Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    format!(
                        "body wait failed before a replacement stopped capability was observed; source/helper protections remain armed until terminal teardown: {error}"
                    ),
                ));
            }
        };
        self.arm_liteinst_wait(&wait)?;
        match wait {
            Wait::Stopped(stopped, Event::Seccomp) => {
                // Do not execute an unadmitted helper syscall. First convert
                // the seccomp stop into an ordinary stop, then restore both the
                // helper machine state and the temporarily writable source.
                let stopped = self.skip_seccomp_syscall(stopped).await?;
                let original = Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    "the post-Ready install helper attempted an unadmitted syscall",
                );
                match self
                    .restore_liteinst_helper_and_source(
                        stopped,
                        &saved,
                        frame,
                        protection,
                        &original_mapping,
                        &helper_code,
                        &prior_words,
                        true,
                    )
                    .await
                {
                    Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                    Err(error) => Err(error),
                }
            }
            Wait::Stopped(stopped, Event::Signal(Signal::SIGTRAP)) => {
                let regs = match stopped.getregs() {
                    Ok(regs) => regs,
                    Err(error) => {
                        let original = Error::Internal(error);
                        return match self
                            .restore_liteinst_helper_and_source(
                                stopped,
                                &saved,
                                frame,
                                protection,
                                &original_mapping,
                                &helper_code,
                                &prior_words,
                                true,
                            )
                            .await
                        {
                            Ok((_, rollback)) => {
                                Err(self.liteinst_helper_failure(original, rollback))
                            }
                            Err(error) => Err(error),
                        };
                    }
                };
                let return_opcode: Option<u8> = frame
                    .helper_return_rip
                    .checked_sub(1)
                    .and_then(|address| Addr::<u8>::from_raw(address as usize))
                    .and_then(|address| stopped.read_value(address).ok());
                let si_code = stopped.getsiginfo().ok().map(|info| info.si_code);
                let consumed_request: Option<LiteinstInstallRequest> =
                    stopped.read_value(request_read).ok();
                let request_consumed = consumed_request == Some(LiteinstInstallRequest::default());
                if regs.r10 != helper_return_marker
                    || regs.ip() != frame.helper_return_rip
                    || regs.rsp != frame.helper_stack_top
                    || return_opcode != Some(0xcc)
                    || !si_code
                        .is_some_and(|code| matches!(code, libc::TRAP_BRKPT | libc::SI_KERNEL))
                    || !physical_status_advanced(
                        entry_status,
                        stopped.physical_status_id().map(|status| status.get()),
                    )
                    || !request_consumed
                    || !liteinst_temporary_source_is_rwx(&stopped, &original_mapping, protection)
                    || !liteinst_helper_code_has_protection(
                        &stopped,
                        &helper_code,
                        libc::PROT_READ | libc::PROT_EXEC,
                    )
                    || !liteinst_helper_code_bytes_match(&stopped, &helper_code)
                    || !liteinst_patch_words_match(&stopped, &prior_words)
                {
                    let original = Error::runtime(
                        self.tid(),
                        "validate LiteInst patch-helper return",
                        "return trap identity, physical status, consumed request, mappings, or prior patch words differed",
                    );
                    return match self
                        .restore_liteinst_helper_and_source(
                            stopped,
                            &saved,
                            frame,
                            protection,
                            &original_mapping,
                            &helper_code,
                            &prior_words,
                            true,
                        )
                        .await
                    {
                        Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                        Err(error) => Err(error),
                    };
                }
                let raw_result = regs.rax as i64;
                let (stopped, rollback) = self
                    .restore_liteinst_helper_and_source(
                        stopped,
                        &saved,
                        frame,
                        protection,
                        &original_mapping,
                        &helper_code,
                        &prior_words,
                        true,
                    )
                    .await?;
                if !rollback.is_empty() {
                    let original = Error::runtime(
                        self.tid(),
                        "restore LiteInst patch-helper state",
                        "patch helper reached its authenticated return trap",
                    );
                    return Err(self.liteinst_helper_failure(original, rollback));
                }
                let mut actual_word = [0_u8; LITEINST_PATCH_WORD_BYTES as usize];
                stopped.read_exact(site as usize, &mut actual_word)?;
                let install = if raw_result >= 0 {
                    let result_address =
                        Addr::from_raw(frame.install_result as usize).ok_or(Errno::EFAULT)?;
                    let result: LiteinstInstallResult = stopped.read_value(result_address)?;
                    if !liteinst_install_result_matches_return(raw_result, &result) {
                        return Err(Error::runtime(
                            self.tid(),
                            "validate LiteInst patch-helper return",
                            format!(
                                "raw helper result {raw_result} differs from relocated tail {:#x}",
                                result.relocated_tail
                            ),
                        ));
                    }
                    let expected =
                        expected_liteinst_patch_word(&request, &result).ok_or_else(|| {
                            Error::runtime(
                                self.tid(),
                                "validate LiteInst patch-helper publication",
                                "successful result cannot encode the exact near-jump word",
                            )
                        })?;
                    if actual_word != expected {
                        return Err(Error::runtime(
                            self.tid(),
                            "validate LiteInst patch-helper publication",
                            "published site bytes differ from the exact E9/rel32/tail word",
                        ));
                    }
                    Some(
                        self.validate_liteinst_install_result(
                            &stopped,
                            frame,
                            site,
                            request.source[..LITEINST_PATCH_WORD_BYTES as usize]
                                .try_into()
                                .expect("validated install request contains one patch word"),
                            expected,
                        )
                            .ok_or_else(|| {
                                Error::runtime(
                                    self.tid(),
                                    "validate LiteInst patch-helper result",
                                    "successful helper returned invalid active-hook metadata",
                                )
                            })?,
                    )
                } else {
                    if !matches!(raw_result, value if value == -i64::from(libc::EOPNOTSUPP) || value == -i64::from(libc::ENOSPC))
                    {
                        return Err(Error::runtime(
                            self.tid(),
                            "validate LiteInst patch-helper result",
                            format!("helper returned unadmitted failure {raw_result}"),
                        ));
                    }
                    if actual_word != request.source[..LITEINST_PATCH_WORD_BYTES as usize] {
                        return Err(Error::runtime(
                            self.tid(),
                            "validate LiteInst patch-helper fallback",
                            "fallback changed the authenticated source word",
                        ));
                    }
                    self.record_liteinst_fallback_stats(&stopped, frame, site);
                    None
                };
                Ok((stopped, install))
            }
            Wait::Stopped(stopped, event) => {
                let original = Error::runtime(
                    self.tid(),
                    "run LiteInst patch helper",
                    format!("unexpected stopped event: {event:?}"),
                );
                match self
                    .restore_liteinst_helper_and_source(
                        stopped,
                        &saved,
                        frame,
                        protection,
                        &original_mapping,
                        &helper_code,
                        &prior_words,
                        true,
                    )
                    .await
                {
                    Ok((_, rollback)) => Err(self.liteinst_helper_failure(original, rollback)),
                    Err(error) => Err(error),
                }
            }
            Wait::Exited(_, exit_status) => self.exit(exit_status).await,
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    async fn call_liteinst_install_helper(
        &mut self,
        _task: Stopped,
        _frame: LiteinstHandshakeFrame,
        _site: u64,
        _request: LiteinstInstallRequest,
        _protection: GuestRange,
        _original_mapping: GuestMap,
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
        // A task-creating syscall must not be patched. Patching overwrites the
        // instruction bytes AT the site, and the new task is resumed with the
        // register context captured before the injection -- i.e. with `rip`
        // pointing just past the original two-byte `syscall`. Once the site
        // holds a longer relocating jump, that address is no longer an
        // instruction boundary and the child executes rubbish. Leaving these
        // sites unpatched costs nothing: they are entered once per task.
        if is_task_creating_syscall(nr) {
            #[cfg(target_arch = "x86_64")]
            self.deopt_liteinst_hooks_quiescent(
                &task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
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

        let page_size = host_page_size()?;
        let Some((request, protection, original_mapping)) =
            snapshot_liteinst_install_request(&task, site, page_size)
        else {
            self.record_liteinst_fallback_stats(&task, frame, site);
            return Ok((task, false, None));
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
        let (task, install) = self
            .call_liteinst_install_helper(task, frame, site, request, protection, original_mapping)
            .await?;
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
        if result.is_err() {
            return;
        }
        let mut state = self.liteinst_runtime.lock().unwrap();
        let Ok(page_size) = host_page_size() else {
            observe_liteinst_mapping_result_without_page_size(&mut state, nr);
            return;
        };
        observe_liteinst_mapping_result_in_state(&mut state, nr, args, result, page_size);
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
        self.arm_liteinst_wait(&wait)?;
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
        let raw_number = task
            .getregs()
            .tracee_context(tid, "read raw syscall number at seccomp stop")?
            .orig_syscall();
        if raw_number & X32_SYSCALL_BIT != 0 {
            return Err(Error::runtime(
                tid,
                "validate LiteInst syscall ABI",
                format!("x32 syscall number {raw_number:#x} is not supported"),
            ));
        }
        let syscall = self
            .get_syscall(&task)
            .tracee_context(tid, "read registers at seccomp stop")?;
        let (nr, args) = syscall.into_parts();
        let tool_subscribed = self
            .global_state
            .subscriptions
            .iter_syscalls()
            .any(|subscribed| subscribed == nr);
        if is_liteinst_mapping_syscall(nr, args) && !tool_subscribed {
            return self.handle_liteinst_mapping_syscall(task, nr, args).await;
        }
        if is_liteinst_mapping_syscall(nr, args)
            && self.validate_liteinst_mapping_execution(nr, args).is_err()
        {
            return Err(Error::runtime(
                tid,
                "validate subscribed LiteInst mapping mutation",
                format!("{nr} overlaps or can indirectly mutate LiteInst control state"),
            ));
        }
        self.record_retained_liteinst_fallback_hit(&task);
        let (installed_task, syscall_already_skipped, liteinst_resume_rip) =
            self.maybe_install_preload_liteinst_site(task, nr).await?;
        task = installed_task;
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
                &task,
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

    pub(crate) fn arm_liteinst_stop(
        &self,
        task: &Stopped,
        event: &Event,
    ) -> Result<(), TraceError> {
        let Some(armer) = self.liteinst_stop_armer(task) else {
            return Ok(());
        };
        armer.arm(task, event)
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

    fn arm_liteinst_wait(&self, wait: &Wait) -> Result<(), TraceError> {
        if let Wait::Stopped(task, event) = wait {
            let Some(armer) = self.liteinst_stop_armer(task) else {
                self.register_liteinst_newborn(task, event);
                return Ok(());
            };
            armer.arm_with(task, event, || self.register_liteinst_newborn(task, event))?;
        }
        Ok(())
    }

    fn ensure_liteinst_wait(&self, wait: &Wait) -> Result<(), TraceError> {
        if let Wait::Stopped(task, event) = wait {
            let Some(armer) = self.liteinst_stop_armer(task) else {
                self.register_liteinst_newborn(task, event);
                return Ok(());
            };
            armer.ensure_with(task, event, || self.register_liteinst_newborn(task, event))?;
        }
        Ok(())
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
        if entry_guard_inspection_blocks_resume(
            self.liteinst_entry_guard_inspection.is_some(),
            self.liteinst_entry_guard_uncertain,
        ) {
            return Err(Errno::EPROTO.into());
        }
        #[cfg(target_arch = "x86_64")]
        if self.liteinst_after_loader_forward_inflight.is_some() {
            return self.lease_liteinst_stop(task).syscall(signal);
        }
        self.lease_liteinst_stop(task).resume(signal)
    }

    #[cfg(target_arch = "x86_64")]
    fn resume_liteinst_group_stop_with_attempt(
        &self,
        task: Stopped,
    ) -> Result<
        (Running, safeptrace::PhysicalResumeAttempt),
        (TraceError, Option<safeptrace::PhysicalResumeAttempt>, Option<Errno>),
    > {
        if self.liteinst_entry_guard_inspection.is_some()
            || self.liteinst_entry_guard_uncertain
            || self.liteinst_after_loader_forward_inflight.is_some()
        {
            return Err((Errno::EPROTO.into(), None, None));
        }
        self.lease_liteinst_stop(task)
            .resume_with_physical_attempt(None)
    }

    fn step_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<Running, TraceError> {
        #[cfg(target_arch = "x86_64")]
        if entry_guard_inspection_blocks_resume(
            self.liteinst_entry_guard_inspection.is_some(),
            self.liteinst_entry_guard_uncertain,
        ) {
            return Err(Errno::EPROTO.into());
        }
        self.lease_liteinst_stop(task).step(signal)
    }

    fn syscall_stopped<T: Into<Option<Signal>>>(
        &self,
        task: Stopped,
        signal: T,
    ) -> Result<Running, TraceError> {
        #[cfg(target_arch = "x86_64")]
        if entry_guard_inspection_blocks_resume(
            self.liteinst_entry_guard_inspection.is_some(),
            self.liteinst_entry_guard_uncertain,
        ) {
            return Err(Errno::EPROTO.into());
        }
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

            if let Err(error) = child_task.arm_liteinst_stop(&child, &event) {
                tracing::error!(
                    tid = %child.pid(),
                    %error,
                    "failed to bind new tracee's exact LiteInst stop"
                );
                child_task.record_liteinst_failure(
                    LiteinstActivationFailureReason::NewbornRegistration,
                    Error::Internal(error),
                );
                return ExitStatus::Exited(1);
            }
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
            self.arm_liteinst_wait(&wait)?;
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
            self.ensure_liteinst_wait(&task_state)?;
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
                    if self.liteinst_installed_event.is_some()
                        || self.after_loader_private_timer_is_owned()
                    {
                        // This branch carries the retained final status for
                        // this exact generation. Retire private execution
                        // before diagnostics or Tool callbacks inspect it.
                        self.retire_liteinst_installed_event_on_terminal()?;
                    }
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
        if matches!(completion, Either::Left((Some(_), _)))
            && (self.liteinst_installed_event.is_some()
                || self.after_loader_private_timer_is_owned())
        {
            // `Some` exists only when ExitFuture yielded this generation's
            // typed PTRACE_EVENT_EXIT stop. Arbitrary ExitFuture errors and
            // special stopped results carry no terminal proof and must not
            // retire private state.
            self.retire_liteinst_installed_event_on_terminal()
                .map_err(|error| reverie::Error::from(anyhow::Error::new(error)))?;
        }
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
                // `wait_after_exit_event` already retired the old claimed exit
                // token atomically with its typed continuation. This `wait` is
                // a distinct ordinary Exec stop and remains armed for session
                // cleanup without transferring the consumed exit claim again.
                let error = match self.arm_liteinst_wait(&wait) {
                    Ok(()) => self.reject_liteinst_nonleader_exec(former_tid),
                    Err(error) => error,
                };
                handle_internal_error(error.into()).await
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
            self.arm_liteinst_wait(&wait)?;
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
        self.untraced_syscall_with_mapping_observation(task, nr, args, true, false)
            .await
    }

    async fn untraced_syscall_with_mapping_observation(
        &mut self,
        task: Stopped,
        nr: Sysno,
        args: SyscallArgs,
        observe_mapping: bool,
        patch_protection: bool,
    ) -> Result<(Stopped, Result<i64, Errno>), TraceError> {
        #[cfg(target_arch = "x86_64")]
        if is_task_creating_syscall(nr) {
            self.deopt_liteinst_hooks_quiescent(
                &task,
                LiteinstDeoptProgramCounter::TranslateGenerated,
            )?;
        }
        if !patch_protection {
            self.validate_liteinst_mapping_execution(nr, args)?;
        } else if nr != Sysno::mprotect {
            return Err(Errno::EPROTO.into());
        }
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
            let purpose = if patch_protection {
                after_loader_task::AfterLoaderSyscallPurpose::PatchProtection
            } else if self.liteinst_runtime.lock().unwrap().phase == LiteinstRuntimePhase::Ready {
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
        self.arm_liteinst_wait(&wait)?;
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
            self.arm_liteinst_wait(&wait)?;
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
        if observe_mapping {
            self.observe_liteinst_mapping_result(nr, args, result);
        }
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
        let mapping_syscall = is_liteinst_mapping_syscall(nr, args);
        if mapping_syscall {
            self.validate_liteinst_mapping_execution(nr, args)?;
        }

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
                let wait = self.syscall_stopped(task, None)?.next_state().await?;
                self.arm_liteinst_wait(&wait)?;
                let (task, result) = self.status_to_result(wait, None, None).await?;
                Ok((task, result))
            } else {
                self.private_inject(task, nr, args).await
            };
        match outcome {
            Ok((task, result)) => {
                if mapping_syscall {
                    self.observe_liteinst_mapping_result(nr, args, result);
                }
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
        let mapping_syscall = is_liteinst_mapping_syscall(nr, args);
        if mapping_syscall {
            self.validate_liteinst_mapping_execution(nr, args)?;
        }

        if self.injected_syscall_frame.is_some() {
            self.pending_syscall = None;
            let (task, result) = self.untraced_syscall(task, nr, args).await?;
            if mapping_syscall {
                self.observe_liteinst_mapping_result(nr, args, result);
            }
            self.write_injected_syscall_result(&task, result)?;
            self.finish_tool_injection(task)?;
            return Ok(result);
        }

        if self.pending_syscall_already_skipped {
            self.pending_syscall = None;
            let (task, result) = self.untraced_syscall(task, nr, args).await?;
            if mapping_syscall {
                self.observe_liteinst_mapping_result(nr, args, result);
            }
            set_ret(
                &task,
                result.unwrap_or_else(|errno| -(errno.into_raw() as i64)) as u64,
            )?;
            self.finish_tool_injection(task)?;
            return Ok(result);
        }

        if mapping_syscall && self.pending_syscall == Some((nr, args)) {
            self.pending_syscall = None;
            let (task, result) = self.private_inject(task, nr, args).await?;
            self.observe_liteinst_mapping_result(nr, args, result);
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
            if mapping_syscall {
                self.observe_liteinst_mapping_result(nr, args, result);
            }
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
            self.arm_liteinst_wait(&wait)?;
            return Ok(wait);
        }

        let wait = running.next_state().await?.assume_stopped();
        let mut task = wait.0;
        let mut event = wait.1;
        self.arm_liteinst_stop(&task, &event)?;

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
                    self.arm_liteinst_stop(&task, &event)?;
                }
                // TODO: combine with handle_signal!
                Event::Signal(Signal::SIGCHLD) => {
                    let running = self.step_stopped(task, Signal::SIGCHLD)?;
                    let wait = running.next_state().await?.assume_stopped();
                    task = wait.0;
                    event = wait.1;
                    self.arm_liteinst_stop(&task, &event)?;
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
        self.arm_liteinst_wait(&wait)?;
        Ok(wait)
    }

    /// check if the stop is caused by sw breakpoint.
    async fn check_swbreak(&mut self, wait: Wait) -> Result<Wait, TraceError> {
        self.arm_liteinst_wait(&wait)?;
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

    #[test]
    fn liteinst_stop_armer_pid_mismatch_never_commits_callback() {
        let held_task_stops = Arc::new(StdMutex::new(HashMap::new()));
        let armer = LiteinstStopArmer {
            task_tid: Pid::from_raw(i32::MAX - 30),
            held_task_stops: Arc::clone(&held_task_stops),
        };
        let task = Stopped::new_unchecked(Pid::from_raw(i32::MAX - 31));
        let callbacks = AtomicUsize::new(0);

        assert!(matches!(
            armer.arm_with(&task, &Event::Signal(Signal::SIGSTOP), || {
                callbacks.fetch_add(1, Ordering::SeqCst);
            }),
            Err(TraceError::Errno(Errno::EINVAL))
        ));
        assert!(matches!(
            armer.ensure_with(&task, &Event::Signal(Signal::SIGSTOP), || {
                callbacks.fetch_add(1, Ordering::SeqCst);
            }),
            Err(TraceError::Errno(Errno::EINVAL))
        ));
        assert_eq!(callbacks.load(Ordering::SeqCst), 0);
        assert!(held_task_stops.lock().unwrap().is_empty());
    }

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
        let runtime_mapping = MappingIdentity {
            device_major: 8,
            device_minor: 2,
            inode: runtime_inode,
        };
        let provenance = LiteinstTrapSiteProvenance { generation, rip };
        let mapping = |writable, device_major, device_minor, inode| GuestMap {
            start: 0x7f00_0000,
            end: 0x7f00_2000,
            offset: 0,
            device_major,
            device_minor,
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
                runtime_mapping,
                &mapping(false, 8, 2, runtime_inode),
            ));
        }

        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip + 0x20,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_mapping,
            &mapping(false, 8, 2, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker ^ 1,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_mapping,
            &mapping(false, 8, 2, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::SI_USER,
            0xcc,
            runtime_mapping,
            &mapping(false, 8, 2, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0x90,
            runtime_mapping,
            &mapping(false, 8, 2, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_mapping,
            &mapping(true, 8, 2, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_mapping,
            &mapping(false, 8, 2, runtime_inode + 1),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_mapping,
            &mapping(false, 9, 2, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_mapping,
            &mapping(false, 8, 3, runtime_inode),
        ));
        assert!(!provenance.validates_retired_runtime_trap(
            generation + 1,
            marker,
            marker,
            rip,
            libc::TRAP_BRKPT,
            0xcc,
            runtime_mapping,
            &mapping(false, 8, 2, runtime_inode),
        ));
    }

    fn active_state() -> LiteinstRuntimeState {
        let mut state = LiteinstRuntimeState::default();
        state.active_hooks.insert(
            0x401005,
            ActiveHookFootprint {
                site: GuestRange::new(0x401005, 8).unwrap(),
                original_site_word: [0x0f, 0x05, 2, 3, 4, 5, 6, 7],
                expected_site_word: [0xe9, 1, 2, 3, 4, 5, 6, 7],
                trampoline: GuestRange::new(0x7000_1000, 0x1000).unwrap(),
                trampoline_code: GuestRange::new(0x7000_1000, 0x200).unwrap(),
                trampoline_code_bytes: vec![0xcc; 0x200],
                ptrace_entry_stop_rip: 0x7000_1001,
                ptrace_completion_stop_rip: 0x7000_1100,
                relocated_tail: 0x7000_1100,
                program_counters: vec![LiteinstProgramCounterMapping {
                    generated_start: 0x7000_1100,
                    generated_end: 0x7000_1104,
                    logical_address: 0x401007,
                }],
                arena_writable: GuestRange::new(0x7100_0000, 0x80_000).unwrap(),
                arena_executable: GuestRange::new(0x7000_0000, 0x80_000).unwrap(),
            },
        );
        state
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn relocated_program_counter_selection_is_unique_and_total_inside_code() {
        let state = active_state();
        let hook = state.active_hooks.get(&0x401005).unwrap().clone();
        assert_eq!(
            liteinst_logical_program_counter(core::slice::from_ref(&hook), hook.relocated_tail),
            Ok(Some(0x401007))
        );
        assert_eq!(
            liteinst_deopt_program_counter(
                core::slice::from_ref(&hook),
                hook.relocated_tail,
                LiteinstDeoptProgramCounter::PreserveObserved,
            ),
            Ok(None),
            "Tool-selected generated RIP was translated instead of preserved"
        );
        assert_eq!(
            liteinst_logical_program_counter(core::slice::from_ref(&hook), 0x7000_1050),
            Err(()),
            "an unmapped generated PC inside trampoline code was accepted"
        );
        assert_eq!(
            liteinst_logical_program_counter(core::slice::from_ref(&hook), 0x6000_0000),
            Ok(None)
        );
        assert_eq!(
            liteinst_logical_program_counter(&[hook.clone(), hook], 0x7000_1100),
            Err(()),
            "two active hooks claimed the same generated PC"
        );
    }

    #[test]
    fn partial_multi_hook_deopt_rolls_back_exactly_or_returns_eio() {
        let mut state = active_state();
        let template = state.active_hooks.values().next().unwrap().clone();
        state.active_hooks.clear();
        for index in 0..3_u8 {
            let mut hook = template.clone();
            let site = 0x401000 + u64::from(index) * 0x1000;
            hook.site = GuestRange::new(site, LITEINST_PATCH_WORD_BYTES).unwrap();
            hook.original_site_word = [0x10 + index; LITEINST_PATCH_WORD_BYTES as usize];
            hook.expected_site_word = [0x20 + index; LITEINST_PATCH_WORD_BYTES as usize];
            state.active_hooks.insert(site, hook);
        }
        let before = state.active_hooks.clone();
        let mut words = state
            .active_hooks
            .values()
            .map(|hook| LiteinstDeoptPatchWord {
                site: hook.site,
                patched: hook.expected_site_word,
                original: hook.original_site_word,
            })
            .collect::<Vec<_>>();
        words.sort_unstable_by_key(|word| word.site.start);

        let run = |corrupt_rollback: bool| {
            let mut memory = words
                .iter()
                .map(|word| (word.site.start, word.patched))
                .collect::<BTreeMap<_, _>>();
            let mut calls = Vec::new();
            let result = transition_liteinst_deopt_patch_words(&words, |word, transition| {
                calls.push((word.site.start, transition));
                if corrupt_rollback
                    && transition == LiteinstDeoptPatchTransition::RepatchIfRestored
                    && word.site.start == words[0].site.start
                {
                    memory.insert(word.site.start, [0x55; LITEINST_PATCH_WORD_BYTES as usize]);
                }
                let observed = memory.get_mut(&word.site.start).unwrap();
                match transition {
                    LiteinstDeoptPatchTransition::RestoreOriginal
                        if *observed == word.patched =>
                    {
                        *observed = word.original;
                        if word.site.start == words[1].site.start {
                            Err(Errno::EPERM)
                        } else {
                            Ok(())
                        }
                    }
                    LiteinstDeoptPatchTransition::RepatchIfRestored
                        if *observed == word.patched =>
                    {
                        Ok(())
                    }
                    LiteinstDeoptPatchTransition::RepatchIfRestored
                        if *observed == word.original =>
                    {
                        *observed = word.patched;
                        Ok(())
                    }
                    _ => Err(Errno::EIO),
                }
            });
            (resolve_liteinst_deopt_patch_result(result), memory, calls)
        };

        let (result, memory, calls) = run(false);
        assert_eq!(result, Err(Errno::EPERM));
        for word in &words {
            assert_eq!(memory.get(&word.site.start), Some(&word.patched));
        }
        assert_eq!(
            calls,
            [
                (words[0].site.start, LiteinstDeoptPatchTransition::RestoreOriginal),
                (words[1].site.start, LiteinstDeoptPatchTransition::RestoreOriginal),
                (words[2].site.start, LiteinstDeoptPatchTransition::RepatchIfRestored),
                (words[1].site.start, LiteinstDeoptPatchTransition::RepatchIfRestored),
                (words[0].site.start, LiteinstDeoptPatchTransition::RepatchIfRestored),
            ]
        );
        assert_eq!(state.active_hooks, before);

        let (result, memory, _) = run(true);
        assert_eq!(result, Err(Errno::EIO));
        assert_eq!(
            memory.get(&words[0].site.start),
            Some(&[0x55; LITEINST_PATCH_WORD_BYTES as usize])
        );
        assert_eq!(memory.get(&words[1].site.start), Some(&words[1].patched));
        assert_eq!(memory.get(&words[2].site.start), Some(&words[2].patched));
        assert_eq!(state.active_hooks, before);

        let generation = state.generation;
        let mut mismatched = state.clone();
        mismatched.generation = generation + 1;
        let mismatched_before = mismatched.clone();
        let hooks = before.values().cloned().collect::<Vec<_>>();
        assert_eq!(
            commit_liteinst_deopt_state(&mut mismatched, generation, &before, &hooks),
            Err(())
        );
        assert_eq!(mismatched, mismatched_before);

        let mut restored_memory = words
            .iter()
            .map(|word| (word.site.start, word.original))
            .collect::<BTreeMap<_, _>>();
        assert!(rollback_liteinst_deopt_patch_words(
            &words,
            &mut |word, transition| {
                assert_eq!(
                    transition,
                    LiteinstDeoptPatchTransition::RepatchIfRestored
                );
                let observed = restored_memory.get_mut(&word.site.start).unwrap();
                if *observed != word.original {
                    return Err(Errno::EIO);
                }
                *observed = word.patched;
                Ok(())
            },
        ));
        for word in &words {
            assert_eq!(restored_memory.get(&word.site.start), Some(&word.patched));
        }
        assert_eq!(mismatched, mismatched_before);

        let mut committed = state.clone();
        commit_liteinst_deopt_state(&mut committed, generation, &before, &hooks).unwrap();
        assert!(committed.active_hooks.is_empty());
        for word in &words {
            assert!(committed.attempted_sites.contains(&word.site.start));
            assert_eq!(
                committed.fallback_sites.get(&word.site.start),
                Some(&LiteinstRetainedFallback::Deoptimized)
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn after_loader_ready_publication_fixture() -> (
        LiteinstRuntimeState,
        u64,
        LiteinstHandshakeFrame,
        Vec<PreparedArenaFootprint>,
        Vec<GuestRange>,
        LiteinstHelperCode,
        crate::target_loader::TargetHostInitializer,
    ) {
        let generation = 9;
        let frame = LiteinstHandshakeFrame {
            version: 8,
            install_helper_page_start: 0x9000,
            install_helper_page_len: 0x1000,
            syscall_trap_rip: 0x7100,
            start_program_break: 0x50_0000,
            initial_program_break: 0x50_1000,
            ..LiteinstHandshakeFrame::default()
        };
        let helper_mapping = GuestMap {
            start: 0x8000,
            end: 0xb000,
            offset: 0x2000,
            device_major: 8,
            device_minor: 1,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            inode: 7,
            path: Some(PathBuf::from("/sealed/runtime.so")),
        };
        let helper = LiteinstHelperCode {
            range: GuestRange::new(0x9000, 0x1000).unwrap(),
            original_mapping: helper_mapping.clone(),
            bytes: vec![0x5a; 0x1000],
        };
        let arenas = vec![PreparedArenaFootprint {
            writable: GuestRange::new(0x20_0000, 0x1000).unwrap(),
            executable: GuestRange::new(0x30_0000, 0x1000).unwrap(),
        }];
        let reservations = vec![GuestRange::new(0x40_0000, 0x1000).unwrap()];
        let initializer = crate::target_loader::TargetHostInitializer {
            tid: 17,
            start_ticks: 23,
            executable_phdr: 0x400040,
            link_map: 0x600000,
            load_bias: 0x700000,
            address: 0x710000,
            mapping_identity: (8, 1, 7),
        };
        let mut state = LiteinstRuntimeState {
            phase: LiteinstRuntimePhase::Bootstrap,
            generation,
            frame: Some(frame),
            start_break: Some(frame.start_program_break),
            current_break: Some(frame.initial_program_break),
            arena_baseline_maps: vec![helper_mapping],
            after_loader_syscall_trap: Some(LiteinstTrapSiteProvenance {
                generation,
                rip: frame.syscall_trap_rip,
            }),
            ..LiteinstRuntimeState::default()
        };
        state.arena_baseline_maps.reserve_exact(8);
        (
            state,
            generation,
            frame,
            arenas,
            reservations,
            helper,
            initializer,
        )
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn after_loader_ready_publication_is_atomic_and_releases_baseline() {
        let (base, generation, frame, arenas, reservations, helper, initializer) =
            after_loader_ready_publication_fixture();
        assert!(after_loader_liteinst_ready_is_publishable(
            &base,
            generation,
            frame,
            &arenas,
            &reservations,
            &helper,
            &initializer,
        ));

        let mut published = base.clone();
        assert!(publish_after_loader_liteinst_ready(
            &mut published,
            generation,
            frame,
            arenas.clone(),
            reservations.clone(),
            helper.clone(),
            0x1234,
            initializer.clone(),
        ));
        assert_eq!(published.phase, LiteinstRuntimePhase::Ready);
        assert_eq!(published.ready_generation, Some(generation));
        assert!(published.arena_baseline_maps.is_empty());
        assert_eq!(published.arena_baseline_maps.capacity(), 0);
        assert_eq!(published.prepared_arenas, arenas);
        assert_eq!(published.prepared_reservations, reservations);
        assert_eq!(published.helper_code.as_ref(), Some(&helper));
        assert_eq!(
            published.after_loader_reference,
            Some((0x1234, initializer.clone()))
        );

        let assert_refused =
            |mut state: LiteinstRuntimeState,
             candidate_generation: u64,
             candidate_frame: LiteinstHandshakeFrame,
             candidate_arenas: Vec<PreparedArenaFootprint>,
             candidate_reservations: Vec<GuestRange>,
             candidate_helper: LiteinstHelperCode,
             candidate_initializer: crate::target_loader::TargetHostInitializer| {
                let before = state.clone();
                assert!(!publish_after_loader_liteinst_ready(
                    &mut state,
                    candidate_generation,
                    candidate_frame,
                    candidate_arenas,
                    candidate_reservations,
                    candidate_helper,
                    0x1234,
                    candidate_initializer,
                ));
                assert_eq!(state, before);
            };

        assert_refused(
            base.clone(),
            generation + 1,
            frame,
            arenas.clone(),
            reservations.clone(),
            helper.clone(),
            initializer.clone(),
        );
        let mut wrong_frame = frame;
        wrong_frame.syscall_trap_rip += 1;
        assert_refused(
            base.clone(),
            generation,
            wrong_frame,
            arenas.clone(),
            reservations.clone(),
            helper.clone(),
            initializer.clone(),
        );
        let mut wrong_helper = helper.clone();
        wrong_helper.original_mapping.inode += 1;
        assert_refused(
            base.clone(),
            generation,
            frame,
            arenas.clone(),
            reservations.clone(),
            wrong_helper,
            initializer.clone(),
        );
        assert_refused(
            base.clone(),
            generation,
            frame,
            arenas.clone(),
            vec![helper.range],
            helper.clone(),
            initializer.clone(),
        );
        assert_refused(
            base.clone(),
            generation,
            frame,
            Vec::new(),
            Vec::new(),
            helper.clone(),
            initializer.clone(),
        );
        let mut dirty = base;
        dirty.attempted_sites.insert(0x401000);
        assert_refused(
            dirty,
            generation,
            frame,
            arenas,
            reservations,
            helper,
            initializer,
        );
    }

    #[test]
    fn same_page_patch_plan_requires_disjoint_exact_prior_words() {
        let state = active_state();
        let page = GuestRange::new(0x401000, 0x1000).unwrap();
        let existing = state.active_hooks.get(&0x401005).unwrap();
        let adjacent = GuestRange::new(existing.site.end, LITEINST_PATCH_WORD_BYTES).unwrap();
        let planned = plan_liteinst_active_patch_words(&state, adjacent, page).unwrap();
        assert_eq!(
            planned,
            [LiteinstPatchWordSnapshot {
                site: existing.site,
                bytes: existing.expected_site_word,
            }]
        );

        assert!(plan_liteinst_active_patch_words(&state, existing.site, page).is_none());
        for shift in 1..LITEINST_PATCH_WORD_BYTES {
            let overlap =
                GuestRange::new(existing.site.start + shift, LITEINST_PATCH_WORD_BYTES).unwrap();
            assert!(
                plan_liteinst_active_patch_words(&state, overlap, page).is_none(),
                "accepted an overlap shifted by {shift} bytes"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn helper_entry_scalar_authority_is_single_status_and_exact() {
        let code_mapping = GuestMap {
            start: 0x7000,
            end: 0x8000,
            offset: 0,
            device_major: 8,
            device_minor: 1,
            readable: true,
            writable: false,
            executable: true,
            shared: false,
            inode: 7,
            path: Some(PathBuf::from("/runtime")),
        };
        let helper_code = LiteinstHelperCode {
            range: GuestRange::new(0x9000, 0x1000).unwrap(),
            original_mapping: GuestMap {
                start: 0x9000,
                end: 0xa000,
                ..code_mapping.clone()
            },
            bytes: vec![0; 0x1000],
        };
        let request = liteinst_install_request_for_test();
        let tid = Pid::from_raw(77);
        let arm = LiteinstInstallHelperArm {
            tid,
            generation: 4,
            origin_status: 10,
            entry: 0x7100,
            entry_rip: 0x7101,
            site: request.site_start,
            stack_pointer: 0xbff8,
            return_address: 0x7200,
            request_address: 0xc000,
            request,
            code_mapping,
            helper_code,
            entry_bytes: [0; 16],
        };
        let regs = libc::user_regs_struct {
            rip: arm.entry_rip,
            rdi: arm.site,
            rsp: arm.stack_pointer,
            orig_rax: u64::MAX,
            ..unsafe { core::mem::zeroed() }
        };
        assert!(liteinst_helper_entry_scalars_match(
            &arm,
            tid,
            arm.generation,
            Some(11),
            &regs,
            libc::TRAP_BRKPT,
            0xcc,
            request,
        ));
        for rejected in [
            liteinst_helper_entry_scalars_match(
                &arm,
                tid,
                arm.generation,
                None,
                &regs,
                libc::TRAP_BRKPT,
                0xcc,
                request,
            ),
            liteinst_helper_entry_scalars_match(
                &arm,
                tid,
                arm.generation,
                Some(9),
                &regs,
                libc::TRAP_BRKPT,
                0xcc,
                request,
            ),
            liteinst_helper_entry_scalars_match(
                &arm,
                tid,
                arm.generation,
                Some(10),
                &regs,
                libc::TRAP_BRKPT,
                0xcc,
                request,
            ),
            liteinst_helper_entry_scalars_match(
                &arm,
                tid,
                arm.generation + 1,
                Some(11),
                &regs,
                libc::TRAP_BRKPT,
                0xcc,
                request,
            ),
            liteinst_helper_entry_scalars_match(
                &arm,
                tid,
                arm.generation,
                Some(11),
                &regs,
                libc::SI_USER,
                0xcc,
                request,
            ),
            liteinst_helper_entry_scalars_match(
                &arm,
                tid,
                arm.generation,
                Some(11),
                &regs,
                libc::TRAP_BRKPT,
                0x90,
                request,
            ),
        ] {
            assert!(!rejected);
        }
        assert!(physical_status_advanced(10, Some(11)));
        assert!(!physical_status_advanced(10, None));
        assert!(!physical_status_advanced(10, Some(10)));
        assert!(!physical_status_advanced(10, Some(9)));
    }

    #[test]
    fn unmatched_sigtrap_is_delivered_unless_owned_by_breakpoint_or_gdb_step() {
        assert_eq!(
            unmatched_sigtrap_disposition(None, false),
            UnmatchedSigtrapDisposition::Deliver
        );
        assert_eq!(
            unmatched_sigtrap_disposition(None, true),
            UnmatchedSigtrapDisposition::GdbStep
        );
        assert_eq!(
            unmatched_sigtrap_disposition(Some(0x4000), false),
            UnmatchedSigtrapDisposition::SoftwareBreakpoint(0x4000)
        );
        assert_eq!(
            unmatched_sigtrap_disposition(Some(0x4000), true),
            UnmatchedSigtrapDisposition::SoftwareBreakpoint(0x4000)
        );
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
            .insert(0x401005, LiteinstRetainedFallback::UnpatchableOrOther);
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
            Some(&LiteinstRetainedFallback::UnpatchableOrOther)
        );
        assert_eq!(old.active_hooks.len(), 1);
        old.generation = u64::MAX;
        assert_eq!(old.after_exec().unwrap_err(), Errno::EOVERFLOW);
        assert_eq!(old.generation, u64::MAX);
        assert_eq!(old.phase, LiteinstRuntimePhase::Ready);
    }

    #[test]
    fn kernel_mapping_effect_ranges_require_alignment_and_round_the_tail() {
        assert_eq!(
            kernel_mapping_effect_range(0x401000, 1, 4096),
            Ok(Some(GuestRange {
                start: 0x401000,
                end: 0x402000,
            }))
        );
        assert_eq!(kernel_mapping_effect_range(0x401001, 1, 4096), Err(()));
        assert_eq!(kernel_mapping_effect_range(0x401000, 0, 4096), Ok(None));
        assert_eq!(kernel_mapping_effect_range(u64::MAX - 1, 4, 4096), Err(()));
        assert_eq!(kernel_mapping_effect_range(0x401000, 1, 3000), Err(()));
        assert_eq!(
            kernel_page_covering_range(0x401005, 1, 4096),
            Ok(Some(GuestRange {
                start: 0x401000,
                end: 0x402000,
            }))
        );
    }

    #[test]
    fn short_successful_mapping_invalidates_the_whole_attempted_page() {
        let mut state = LiteinstRuntimeState::default();
        state.attempted_sites.extend([0x401005, 0x401fff, 0x402005]);

        state.invalidate_attempted_pages(0x401000, 1, 4096);

        assert_eq!(state.attempted_sites, HashSet::from([0x402005]));
    }

    #[test]
    fn controller_successful_mremap_retires_generation_and_failed_call_is_noop() {
        let page = 4096_usize;
        let source = 0x20_0000_usize;
        let destination = 0x40_0000_usize;
        let other_destination = 0x50_0000_usize;
        let unrelated = 0x60_0008_u64;
        let candidates = HashSet::from([
            source as u64 + 8,
            destination as u64 + 8,
            other_destination as u64 + 8,
            unrelated,
        ]);
        let seeded_state = || {
            let mut state = active_state();
            state.phase = LiteinstRuntimePhase::Ready;
            state.generation = 17;
            state.ready_generation = Some(17);
            state.attempted_sites = candidates.clone();
            for address in &candidates {
                state
                    .fallback_sites
                    .insert(*address, LiteinstRetainedFallback::UnpatchableOrOther);
            }
            state
        };

        let impossible_successes = [
            (
                SyscallArgs::new(source, page, page, 1 << 8, 0, 0),
                destination as i64,
            ),
            (
                SyscallArgs::new(
                    source,
                    page,
                    page,
                    libc::MREMAP_FIXED as usize,
                    destination,
                    0,
                ),
                destination as i64,
            ),
            (
                SyscallArgs::new(
                    source,
                    page,
                    page,
                    (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as usize,
                    destination,
                    0,
                ),
                other_destination as i64,
            ),
            (
                SyscallArgs::new(
                    source,
                    page,
                    2 * page,
                    (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as usize,
                    0,
                    0,
                ),
                destination as i64,
            ),
            (
                SyscallArgs::new(source, 2 * page, page, libc::MREMAP_MAYMOVE as usize, 0, 0),
                (source + page) as i64,
            ),
            (
                SyscallArgs::new(source, 2 * page, page, libc::MREMAP_MAYMOVE as usize, 0, 0),
                destination as i64,
            ),
            (
                SyscallArgs::new(source, page, page, libc::MREMAP_MAYMOVE as usize, 0, 0),
                destination as i64,
            ),
            (
                SyscallArgs::new(source, 0, page, libc::MREMAP_MAYMOVE as usize, 0, 0),
                source as i64,
            ),
            (
                SyscallArgs::new(source, page, page, 0, 0, 0),
                destination as i64,
            ),
        ];
        for (args, result) in impossible_successes {
            let mut state = seeded_state();
            observe_liteinst_mapping_result_in_state(
                &mut state,
                Sysno::mremap,
                args,
                Ok(result),
                page as u64,
            );
            assert!(state.attempted_sites.is_empty());
            assert!(state.fallback_sites.is_empty());
            assert_eq!(state.ready_generation, None);
            assert_eq!(state.active_hooks.len(), 1);
        }

        for (args, result) in [
            (
                SyscallArgs::new(source, 0, page, libc::MREMAP_MAYMOVE as usize, 0, 0),
                destination,
            ),
            (
                SyscallArgs::new(
                    source,
                    page,
                    page,
                    (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as usize,
                    0,
                    0,
                ),
                destination,
            ),
            (
                SyscallArgs::new(source, page, 2 * page, libc::MREMAP_MAYMOVE as usize, 0, 0),
                destination,
            ),
        ] {
            let mut state = seeded_state();
            observe_liteinst_mapping_result_in_state(
                &mut state,
                Sysno::mremap,
                args,
                Ok(result as i64),
                page as u64,
            );
            assert!(state.attempted_sites.is_empty());
            assert!(state.fallback_sites.is_empty());
            assert_eq!(state.ready_generation, None);
            assert_eq!(state.active_hooks.len(), 1);
        }

        let mut failed = seeded_state();
        observe_liteinst_mapping_result_in_state(
            &mut failed,
            Sysno::mremap,
            SyscallArgs::new(source, page, page, libc::MREMAP_MAYMOVE as usize, 0, 0),
            Err(Errno::ENOMEM),
            page as u64,
        );
        assert_eq!(failed.attempted_sites, candidates);
        assert_eq!(
            failed
                .fallback_sites
                .keys()
                .copied()
                .collect::<HashSet<_>>(),
            candidates
        );
        assert_eq!(failed.ready_generation, Some(17));
        assert_eq!(failed.active_hooks.len(), 1);

        let mut missing_page_size = seeded_state();
        observe_liteinst_mapping_result_without_page_size(&mut missing_page_size, Sysno::mremap);
        assert!(missing_page_size.attempted_sites.is_empty());
        assert!(missing_page_size.fallback_sites.is_empty());
        assert_eq!(missing_page_size.ready_generation, None);
        assert_eq!(missing_page_size.active_hooks.len(), 1);

        let mut non_remap_page_size_failure = seeded_state();
        observe_liteinst_mapping_result_without_page_size(
            &mut non_remap_page_size_failure,
            Sysno::munmap,
        );
        assert_eq!(non_remap_page_size_failure.ready_generation, Some(17));
    }

    #[test]
    fn remap_file_pages_controller_floors_geometry_and_observes_only_success() {
        let page = 4096_usize;
        let page_start = 0x401000_usize;
        let args = SyscallArgs::new(page_start + 37, page + 511, 0, 7, 0, 0);
        assert_eq!(
            remap_file_pages_effect_range(args, page as u64),
            Ok(Some(GuestRange {
                start: page_start as u64,
                end: (page_start + page) as u64,
            }))
        );
        let active = active_state();
        assert!(active.mapping_mutates_active_hook(Sysno::remap_file_pages, args, page as u64,));
        let invalid_flags = SyscallArgs::new(page_start, page, 0, 0, 1, 0);
        assert_eq!(
            remap_file_pages_effect_range(invalid_flags, page as u64),
            Err(())
        );
        assert!(
            !active.mapping_mutates_active_hook(
                Sysno::remap_file_pages,
                invalid_flags,
                page as u64,
            ),
            "overlapping nonzero flags must reach the kernel's native EINVAL"
        );
        for invalid in [
            SyscallArgs::new(page_start, page - 1, 0, 0, 0, 0),
            SyscallArgs::new(page_start, page, 1, 0, 0, 0),
            SyscallArgs::new(page_start, page, 0, usize::MAX, 0, 0),
            SyscallArgs::new(usize::MAX - (page - 1), page, 0, 0, 0, 0),
        ] {
            assert_eq!(remap_file_pages_effect_range(invalid, page as u64), Err(()));
            assert!(!active.mapping_mutates_active_hook(
                Sysno::remap_file_pages,
                invalid,
                page as u64,
            ));
        }
        assert_eq!(
            remap_file_pages_effect_range(args, page as u64 - 1),
            Err(())
        );

        let before = HashSet::from([
            (page_start - 8) as u64,
            (page_start + 5) as u64,
            (page_start + page) as u64,
        ]);
        let mut successful = LiteinstRuntimeState::default();
        successful.attempted_sites = before.clone();
        for address in &before {
            successful
                .fallback_sites
                .insert(*address, LiteinstRetainedFallback::UnpatchableOrOther);
        }
        observe_liteinst_mapping_result_in_state(
            &mut successful,
            Sysno::remap_file_pages,
            args,
            Ok(0),
            page as u64,
        );
        let retained = HashSet::from([(page_start - 8) as u64, (page_start + page) as u64]);
        assert_eq!(successful.attempted_sites, retained);
        assert_eq!(
            successful
                .fallback_sites
                .keys()
                .copied()
                .collect::<HashSet<_>>(),
            retained
        );

        let mut failed = LiteinstRuntimeState::default();
        failed.attempted_sites = before.clone();
        observe_liteinst_mapping_result_in_state(
            &mut failed,
            Sysno::remap_file_pages,
            args,
            Err(Errno::EINVAL),
            page as u64,
        );
        assert_eq!(failed.attempted_sites, before);

        observe_liteinst_mapping_result_in_state(
            &mut failed,
            Sysno::remap_file_pages,
            SyscallArgs::new(page_start, page - 1, 0, 0, 0, 0),
            Ok(0),
            page as u64,
        );
        assert!(failed.attempted_sites.is_empty());
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
    fn controller_madvise_guard_uses_kernel_c_int_classification() {
        const PRESERVING: &[usize] = &[
            0, 1, 2, 3, 11, 12, 13, 14, 15, 16, 17, 19, 20, 21, 22, 23, 25,
        ];
        const DESTRUCTIVE_OR_UNKNOWN: &[usize] = &[
            4,
            8,
            9,
            10,
            18,
            24,
            26,
            100,
            101,
            102,
            103,
            u32::MAX as usize,
        ];
        let state = active_state();
        for advice in PRESERVING {
            for raw in [*advice, *advice | 0xfeed_beef_0000_0000_usize] {
                assert!(madvise_preserves_liteinst_generation(raw));
                assert!(!state.mapping_mutates_active_hook(
                    Sysno::madvise,
                    SyscallArgs::new(0x401000, 1, raw, 0, 0, 0),
                    4096,
                ));
            }
        }
        for advice in DESTRUCTIVE_OR_UNKNOWN {
            for raw in [*advice, *advice | 0xfeed_beef_0000_0000_usize] {
                assert!(!madvise_preserves_liteinst_generation(raw));
                assert!(state.mapping_mutates_active_hook(
                    Sysno::madvise,
                    SyscallArgs::new(0x401000, 1, raw, 0, 0, 0),
                    4096,
                ));
            }
        }
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
            Sysno::mremap,
            SyscallArgs::new(
                0x5000_0000,
                1,
                1,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as usize,
                0x401000,
                0,
            ),
            4096,
        ));
        assert!(state.mapping_mutates_active_hook(
            Sysno::mmap,
            SyscallArgs::new(
                0x5000_0000,
                1,
                libc::PROT_READ as usize,
                libc::MAP_FIXED as usize,
                usize::MAX,
                0,
            ),
            4096,
        ));
        assert!(!state.mapping_mutates_active_hook(
            Sysno::mmap,
            SyscallArgs::new(
                0x5000_0000,
                1,
                libc::PROT_READ as usize,
                libc::MAP_FIXED_NOREPLACE as usize,
                usize::MAX,
                0,
            ),
            4096,
        ));
        assert!(!state.mapping_mutates_active_hook(
            Sysno::mprotect,
            SyscallArgs::new(u64::MAX as usize - 1, 4, libc::PROT_NONE as usize, 0, 0, 0),
            4096,
        ));
    }

    #[test]
    fn invalid_mprotect_geometry_reaches_the_native_einval_path() {
        let address = usize::MAX - 1;
        let result =
            unsafe { libc::syscall(libc::SYS_mprotect, address, 4_usize, libc::PROT_NONE) };
        assert_eq!(result, -1);
        assert_eq!(Errno::last(), Errno::EINVAL);
    }

    #[test]
    fn mremap_guard_preserves_nonfixed_invalid_errno_and_protects_shareable_arenas() {
        let state = active_state();
        let clone_site = SyscallArgs::new(0x401000, 0, 1, libc::MREMAP_MAYMOVE as usize, 0, 0);
        assert_eq!(
            mremap_effect_ranges(clone_site, 4096),
            Ok(MremapEffectRanges {
                source: GuestRange::new(0x401000, 4096).unwrap(),
                destination: None,
                clones_shared_mapping: true,
            })
        );
        assert!(!state.mapping_mutates_active_hook(Sysno::mremap, clone_site, 4096));

        let clone_arena = SyscallArgs::new(0x7000_0000, 0, 1, libc::MREMAP_MAYMOVE as usize, 0, 0);
        assert!(state.mapping_mutates_active_hook(Sysno::mremap, clone_arena, 4096));

        let moved_site = SyscallArgs::new(0x401000, 1, 1, libc::MREMAP_MAYMOVE as usize, 0, 0);
        assert!(state.mapping_mutates_active_hook(Sysno::mremap, moved_site, 4096));

        for invalid in [
            SyscallArgs::new(0x401000, 0, 1, 0, 0, 0),
            SyscallArgs::new(
                0x401000,
                0,
                1,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as usize,
                0,
                0,
            ),
            SyscallArgs::new(0x401000, 1, 0, 0, 0, 0),
            SyscallArgs::new(0x401000, 1, 1, 1 << 8, 0, 0),
            SyscallArgs::new(
                0x401000,
                1,
                8192,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as usize,
                0,
                0,
            ),
            SyscallArgs::new(
                0x401000,
                1,
                1,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as usize,
                0x401001,
                0,
            ),
            SyscallArgs::new(
                0x401000,
                1,
                1,
                (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as usize,
                0x401000,
                0,
            ),
        ] {
            assert_eq!(mremap_effect_ranges(invalid, 4096), Err(()));
            assert!(!state.mapping_mutates_active_hook(Sysno::mremap, invalid, 4096));
        }
        let invalid_fixed =
            SyscallArgs::new(0x401000, 1, 1, libc::MREMAP_FIXED as usize, 0x500000, 0);
        assert_eq!(mremap_effect_ranges(invalid_fixed, 4096), Err(()));
        assert!(
            state.mapping_mutates_active_hook(Sysno::mremap, invalid_fixed, 4096),
            "the hugetlb support boundary refuses every true fixed replacement while controls exist"
        );

        let dont_unmap_hinting_arena = SyscallArgs::new(
            0x5000_0000,
            4096,
            4096,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_DONTUNMAP) as usize,
            0x7000_0000,
            0,
        );
        assert!(mremap_effect_ranges(dont_unmap_hinting_arena, 4096).is_ok());
        assert!(!state.mapping_mutates_active_hook(Sysno::mremap, dont_unmap_hinting_arena, 4096,));

        let fixed_overlapping_clone = SyscallArgs::new(
            0x7000_0000,
            0,
            8192,
            (libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED) as usize,
            0x7000_1000,
            0,
        );
        assert!(mremap_effect_ranges(fixed_overlapping_clone, 4096).is_ok());
        assert!(state.mapping_mutates_active_hook(Sysno::mremap, fixed_overlapping_clone, 4096,));
        assert!(!state.mapping_mutates_active_hook(
            Sysno::mremap,
            SyscallArgs::new(0x5000_0000, 0, 1, libc::MREMAP_MAYMOVE as usize, 0, 0,),
            4096,
        ));
    }

    #[test]
    fn native_mremap_zero_old_size_requires_a_shareable_mapping() {
        let page = host_page_size().unwrap() as usize;
        unsafe {
            let private = libc::mmap(
                core::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(private, libc::MAP_FAILED);
            let private_clone = libc::syscall(
                libc::SYS_mremap,
                private,
                0_usize,
                page,
                libc::MREMAP_MAYMOVE,
            );
            assert_eq!(private_clone, -1);
            assert_eq!(Errno::last(), Errno::EINVAL);
            assert_eq!(libc::munmap(private, page), 0);

            let shared = libc::mmap(
                core::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert_ne!(shared, libc::MAP_FAILED);
            shared.cast::<u64>().write(0x1234_5678_9abc_def0);
            let shared_clone = libc::syscall(
                libc::SYS_mremap,
                shared,
                0_usize,
                page,
                libc::MREMAP_MAYMOVE,
            );
            assert_ne!(shared_clone, -1, "shared clone: {}", Errno::last());
            let shared_clone = shared_clone as *mut libc::c_void;
            assert_eq!(shared_clone.cast::<u64>().read(), 0x1234_5678_9abc_def0);

            for (new_size, flags) in [(0_usize, 0_usize), (page, 1_usize << 8)] {
                let result = libc::syscall(libc::SYS_mremap, shared, page, new_size, flags);
                assert_eq!(result, -1);
                assert_eq!(Errno::last(), Errno::EINVAL);
            }
            assert_eq!(libc::munmap(shared_clone, page), 0);
            assert_eq!(libc::munmap(shared, page), 0);
        }
    }

    #[test]
    fn pkey_mprotect_is_a_controller_mapping_syscall() {
        assert!(is_liteinst_mapping_syscall(
            Sysno::pkey_mprotect,
            SyscallArgs::new(0, 0, 0, 0, 0, 0)
        ));
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

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn installed_native_register_update_allows_rip_and_rsp_but_preserves_syscall_identity() {
        let current = libc::user_regs_struct {
            rip: 0x401002,
            rsp: 0x7fff_1000,
            orig_rax: libc::SYS_getpid as u64,
            eflags: 0x202,
            cs: 0x33,
            ss: 0x2b,
            ..unsafe { core::mem::zeroed() }
        };
        let mut allowed = current;
        allowed.rip = 0x7000_1100;
        allowed.rsp += 8;
        allowed.rax = 17;
        allowed.rdi = 23;
        allowed.eflags ^= 1;
        assert_eq!(
            validate_liteinst_installed_user_regs_update(&current, &allowed),
            Ok(())
        );

        let rejected = [
            {
                let mut regs = allowed;
                regs.orig_rax += 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.cs ^= 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.ss ^= 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.ds ^= 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.es ^= 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.fs ^= 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.gs ^= 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.fs_base += 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.gs_base += 1;
                regs
            },
            {
                let mut regs = allowed;
                regs.eflags ^= 1 << 9;
                regs
            },
        ];
        for (index, requested) in rejected.iter().enumerate() {
            assert_eq!(
                validate_liteinst_installed_user_regs_update(&current, requested),
                Err(Errno::ENOTSUPP),
                "forbidden installed-register mutation {index} was accepted"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn installed_tool_program_counter_change_selects_deopt() {
        assert_eq!(
            liteinst_tool_program_counter_action(0x401002, 0x401002),
            LiteinstToolProgramCounterAction::RestoreGenerated
        );
        assert_eq!(
            liteinst_tool_program_counter_action(0x402000, 0x401002),
            LiteinstToolProgramCounterAction::Deopt
        );
        assert_eq!(
            liteinst_tool_program_counter_action(0x7000_1100, 0x401002),
            LiteinstToolProgramCounterAction::Deopt,
            "a Tool-selected trampoline RIP did not select preserving deopt"
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
    fn guest_maps_snapshot_rejects_a_malformed_middle_record() {
        let first = b"1000-2000 r-xp 00000000 08:01 7 /first";
        let second = b"2000-3000 rw-p 00001000 08:01 7 /second";
        let mut valid = Vec::new();
        valid.extend_from_slice(first);
        valid.push(b'\n');
        valid.extend_from_slice(second);
        valid.push(b'\n');
        let parsed = parse_guest_maps_snapshot(&valid).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], parse_guest_map(first).unwrap());
        assert_eq!(parsed[1], parse_guest_map(second).unwrap());

        let mut malformed = Vec::new();
        malformed.extend_from_slice(first);
        malformed.extend_from_slice(b"\nnot-a-map r-xp 00000000 08:01 8 /bad\n");
        malformed.extend_from_slice(second);
        malformed.push(b'\n');
        assert!(parse_guest_maps_snapshot(&malformed).is_none());
    }

    #[test]
    fn guest_smaps_attributes_cannot_cross_a_malformed_header() {
        let target = parse_guest_map(b"1000-2000 r-xp 00000000 08:01 7 /target").unwrap();
        let valid = b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         0\n\
VmFlags: rd ex mr mw me\n";
        assert_eq!(
            parse_guest_hook_mapping_attributes(valid, &target),
            Some(GuestHookMappingAttributes {
                fork_safe: true,
                protection_key: 0,
            })
        );

        let malformed_middle = b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         0\n\
not-a-range r-xp 00000000 08:01 8 /other\n\
VmFlags: rd ex mr mw me\n";
        assert!(parse_guest_hook_mapping_attributes(malformed_middle, &target).is_none());

        let doubly_malformed_middle = b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         0\n\
1000-z000 badp 00000000 08:01 8 /other\n\
VmFlags: rd ex mr mw me\n";
        assert!(parse_guest_hook_mapping_attributes(doubly_malformed_middle, &target).is_none());

        let valid_next = b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         0\n\
2000-3000 r-xp 00001000 08:01 8 /other\n\
ProtectionKey:         0\n\
VmFlags: rd ex mr mw me\n";
        assert!(parse_guest_hook_mapping_attributes(valid_next, &target).is_none());

        let admitted = |smaps: &[u8]| {
            parse_guest_hook_mapping_attributes(smaps, &target)
                .is_some_and(|attributes| attributes.fork_safe && attributes.protection_key == 0)
        };
        assert!(admitted(valid));
        let four_digit_metric = b"1000-2000 r-xp 00000000 08:01 7 /target\n\
Anonymous:          4096 kB\n\
ProtectionKey:         0\n\
VmFlags: rd ex mr mw me\n";
        assert!(admitted(four_digit_metric));
        for refused in [
            b"1000-2000 r-xp 00000000 08:01 7 /target\n\
VmFlags: rd ex mr mw me\n"
                .as_slice(),
            b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         1\n\
VmFlags: rd ex mr mw me\n"
                .as_slice(),
            b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         not-decimal\n\
VmFlags: rd ex mr mw me\n"
                .as_slice(),
            b"1000-2000 r-xp 00000000 08:01 7 /target\n\
VmFlags: rd ex mr mw me\n\
ProtectionKey:         0\n"
                .as_slice(),
            b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         0\n\
VmFlags: rd ex mr mw me\n\
ProtectionKey:         1\n"
                .as_slice(),
            b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         not-decimal\n\
ProtectionKey:         0\n\
VmFlags: rd ex mr mw me\n"
                .as_slice(),
            b"1000-2000 r-xp 00000000 08:01 7 /target\n\
ProtectionKey:         0\n\
VmFlags: rd ex ht mr mw me\n"
                .as_slice(),
        ] {
            assert!(!admitted(refused));
        }
    }

    fn liteinst_install_request_for_test() -> LiteinstInstallRequest {
        let mut source = [0_u8; LITEINST_INSTALL_SOURCE_BYTES];
        source[..8].copy_from_slice(&[0x0f, 0x05, 2, 3, 4, 5, 6, 7]);
        LiteinstInstallRequest {
            version: LITEINST_INSTALL_REQUEST_VERSION,
            site_start: 0x4000,
            mapping_end: 0x5000,
            source_len: 8,
            source,
        }
    }

    #[test]
    fn exact_liteinst_patch_word_binds_jump_and_original_tail() {
        let request = liteinst_install_request_for_test();
        let result = LiteinstInstallResult {
            site_start: request.site_start,
            site_len: LITEINST_PATCH_WORD_BYTES,
            trampoline_start: 0x4800,
            ..LiteinstInstallResult::default()
        };
        let displacement = i32::try_from(result.trampoline_start - (request.site_start + 5))
            .unwrap()
            .to_le_bytes();
        let mut expected = [0_u8; LITEINST_PATCH_WORD_BYTES as usize];
        expected[0] = 0xe9;
        expected[1..5].copy_from_slice(&displacement);
        expected[5..].copy_from_slice(&request.source[5..8]);
        assert_eq!(
            expected_liteinst_patch_word(&request, &result),
            Some(expected)
        );

        let mut invalid = request;
        invalid.source_len = LITEINST_PATCH_WORD_BYTES - 1;
        assert_eq!(expected_liteinst_patch_word(&invalid, &result), None);
        let mut invalid = result;
        invalid.site_start += 1;
        assert_eq!(expected_liteinst_patch_word(&request, &invalid), None);
        let mut invalid = result;
        invalid.site_len -= 1;
        assert_eq!(expected_liteinst_patch_word(&request, &invalid), None);
        let mut invalid = result;
        invalid.trampoline_start = u64::MAX;
        assert_eq!(expected_liteinst_patch_word(&request, &invalid), None);
    }

    #[test]
    fn liteinst_helper_return_must_equal_the_reported_relocated_tail() {
        let result = LiteinstInstallResult {
            relocated_tail: 0x1234,
            ..LiteinstInstallResult::default()
        };
        assert!(liteinst_install_result_matches_return(0x1234, &result));
        for raw in [0, 0x1233, 0x1235, -1] {
            assert!(!liteinst_install_result_matches_return(raw, &result));
        }
        let unrepresentable = LiteinstInstallResult {
            relocated_tail: i64::MAX as u64 + 1,
            ..result
        };
        assert!(!liteinst_install_result_matches_return(
            i64::MAX,
            &unrepresentable
        ));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn ptrace_word_reader_reads_an_exact_prot_none_page() {
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0);
        let page_size = page_size as usize;
        assert!(page_size.is_multiple_of(core::mem::size_of::<u64>()));
        let mapping = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                page_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let mapping = mapping.cast::<u8>();
        for index in 0..page_size {
            unsafe { mapping.add(index).write((index as u8).wrapping_mul(37)) };
        }
        let expected = unsafe { core::slice::from_raw_parts(mapping, page_size) }.to_vec();

        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let traced = unsafe {
                libc::ptrace(
                    libc::PTRACE_TRACEME,
                    0,
                    core::ptr::null_mut::<libc::c_void>(),
                    core::ptr::null_mut::<libc::c_void>(),
                )
            };
            if traced != 0
                || unsafe { libc::mprotect(mapping.cast(), page_size, libc::PROT_NONE) } != 0
                || unsafe { libc::raise(libc::SIGSTOP) } != 0
            {
                unsafe { libc::_exit(2) };
            }
            unsafe { libc::_exit(0) };
        }

        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFSTOPPED(status));
        let task = Stopped::new_unchecked(Pid::from_raw(child));
        let mut observed = vec![0_u8; page_size];
        let read = read_stopped_ptrace_words(&task, mapping as usize, &mut observed);
        drop(task);
        assert_eq!(
            unsafe {
                libc::ptrace(
                    libc::PTRACE_CONT,
                    child,
                    core::ptr::null_mut::<libc::c_void>(),
                    core::ptr::null_mut::<libc::c_void>(),
                )
            },
            0
        );
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        assert_eq!(unsafe { libc::munmap(mapping.cast(), page_size) }, 0);
        assert!(read);
        assert_eq!(observed, expected);
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

    #[test]
    fn exact_entry_word_transition_refuses_every_io_failure_and_mismatch() {
        let original = 0x8877_6655_4433_2211_u64;
        let guarded = (original & !0xff) | 0xcc;
        let mut word = guarded;
        let mut operations = Vec::new();
        replace_exact_entry_word(guarded, original, |operation| {
            operations.push(operation);
            match operation {
                EntryWordIo::Read => Ok::<_, u8>(word),
                EntryWordIo::Write(value) => {
                    word = value;
                    Ok(value)
                }
            }
        })
        .unwrap();
        assert_eq!(word, original);
        assert_eq!(
            operations,
            [
                EntryWordIo::Read,
                EntryWordIo::Write(original),
                EntryWordIo::Read,
            ]
        );

        assert_eq!(
            replace_exact_entry_word(guarded, original, |_| Err::<u64, _>(1_u8)),
            Err(EntryWordTransitionError::Access {
                stage: EntryWordIoStage::InitialRead,
                error: 1,
            })
        );
        assert_eq!(
            replace_exact_entry_word(guarded, original, |_| Ok::<_, u8>(guarded ^ 1)),
            Err(EntryWordTransitionError::Expected {
                expected: guarded,
                observed: guarded ^ 1,
            })
        );

        let mut step = 0;
        assert_eq!(
            replace_exact_entry_word(guarded, original, |_| {
                step += 1;
                if step == 1 { Ok(guarded) } else { Err(2_u8) }
            }),
            Err(EntryWordTransitionError::Access {
                stage: EntryWordIoStage::Write,
                error: 2,
            })
        );

        let mut step = 0;
        assert_eq!(
            replace_exact_entry_word(guarded, original, |_| {
                step += 1;
                match step {
                    1 => Ok(guarded),
                    2 => Ok(original),
                    _ => Err(3_u8),
                }
            }),
            Err(EntryWordTransitionError::Access {
                stage: EntryWordIoStage::Readback,
                error: 3,
            })
        );

        let mut step = 0;
        assert_eq!(
            replace_exact_entry_word(guarded, original, |_| {
                step += 1;
                Ok::<_, u8>(match step {
                    1 => guarded,
                    2 => original,
                    _ => original ^ 1,
                })
            }),
            Err(EntryWordTransitionError::Readback {
                expected: original,
                observed: original ^ 1,
            })
        );
    }

    #[test]
    fn exact_entry_word_recovery_and_resume_barrier_are_fail_closed() {
        let guarded = 0x8877_6655_4433_22cc_u64;
        let mut word = 0_u64;
        publish_exact_entry_word(guarded, |operation| match operation {
            EntryWordIo::Read => Ok::<_, u8>(word),
            EntryWordIo::Write(value) => {
                word = value;
                Ok(value)
            }
        })
        .unwrap();
        assert_eq!(word, guarded);

        assert_eq!(
            publish_exact_entry_word(guarded, |_| Err::<u64, _>(4_u8)),
            Err(EntryWordTransitionError::Access {
                stage: EntryWordIoStage::Write,
                error: 4,
            })
        );
        let mut step = 0;
        assert_eq!(
            publish_exact_entry_word(guarded, |_| {
                step += 1;
                if step == 1 { Ok(guarded) } else { Err(5_u8) }
            }),
            Err(EntryWordTransitionError::Access {
                stage: EntryWordIoStage::Readback,
                error: 5,
            })
        );
        let mut step = 0;
        assert_eq!(
            publish_exact_entry_word(guarded, |_| {
                step += 1;
                Ok::<_, u8>(if step == 1 { guarded } else { guarded ^ 1 })
            }),
            Err(EntryWordTransitionError::Readback {
                expected: guarded,
                observed: guarded ^ 1,
            })
        );

        assert!(!entry_guard_inspection_blocks_resume(false, false));
        assert!(entry_guard_inspection_blocks_resume(true, false));
        assert!(entry_guard_inspection_blocks_resume(false, true));
        assert!(entry_guard_inspection_blocks_resume(true, true));
        assert_eq!(entry_word_recovery_error(7_u8, Ok(())), 7);
        assert_eq!(entry_word_recovery_error(7_u8, Err(9_u8)), 9);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn installed_event_private_mask_blocks_every_maskable_kernel_signal() {
        let private = liteinst_private_sigmask(safeptrace::PtraceSigmask::from_bytes([0; 8]));
        let mut expected = [0xff; 8];
        expected[(libc::SIGKILL as usize - 1) / 8] &= !(1 << ((libc::SIGKILL as usize - 1) % 8));
        expected[(libc::SIGSTOP as usize - 1) / 8] &= !(1 << ((libc::SIGSTOP as usize - 1) % 8));
        assert_eq!(private.into_bytes(), expected);

        let normalized = liteinst_private_sigmask(safeptrace::PtraceSigmask::from_bytes([0xff; 8]));
        assert_eq!(normalized.into_bytes(), expected);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn installed_rt_sigreturn_is_never_a_tool_or_timer_event() {
        assert_eq!(
            liteinst_installed_tool_event_number(Sysno::rt_sigreturn as u64),
            None
        );
        assert_eq!(
            classify_liteinst_installed_syscall(Sysno::rt_sigreturn as u64),
            LiteinstInstalledSyscallClass::RtSigreturn
        );
        assert_eq!(
            liteinst_installed_tool_event_number(X32_SYSCALL_BIT | Sysno::getpid as u64),
            None
        );
        assert_eq!(
            classify_liteinst_installed_syscall(X32_SYSCALL_BIT | Sysno::getpid as u64),
            LiteinstInstalledSyscallClass::X32
        );
        assert!(liteinst_raw_syscall_number_is_x32(
            X32_SYSCALL_BIT | Sysno::getpid as u64
        ));
        for raw in [u64::MAX, (-4095_i64) as u64] {
            assert!(!liteinst_raw_syscall_number_is_x32(raw));
            assert_eq!(liteinst_installed_tool_event_number(raw), None);
            assert!(usize::try_from(raw).ok().and_then(Sysno::new).is_none());
            assert_eq!(
                classify_liteinst_installed_syscall(raw),
                LiteinstInstalledSyscallClass::Unknown
            );
        }
        assert_eq!(
            liteinst_installed_tool_event_number(Sysno::getpid as u64),
            Some(Sysno::getpid)
        );
        assert_eq!(
            classify_liteinst_installed_syscall(Sysno::getpid as u64),
            LiteinstInstalledSyscallClass::Known(Sysno::getpid)
        );
    }

    #[test]
    fn prepared_and_active_arena_writers_are_prot_none_at_rest() {
        let state = active_state();
        let hook = state.active_hooks.get(&0x401005).unwrap();
        assert!(hook
            .protected_ranges()
            .contains(&(hook.arena_writable, libc::PROT_NONE)));
        let prepared = PreparedArenaFootprint {
            writable: hook.arena_writable,
            executable: hook.arena_executable,
        };
        assert_eq!(
            prepared.protected_ranges(),
            [
                (hook.arena_writable, libc::PROT_NONE),
                (
                    hook.arena_executable,
                    libc::PROT_READ | libc::PROT_EXEC
                ),
            ]
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn installed_completion_requires_exact_restored_registers_except_boundary_rip() {
        let mut entry = unsafe { core::mem::zeroed::<libc::user_regs_struct>() };
        entry.r15 = 1;
        entry.rax = 39;
        entry.rcx = 0x1234;
        entry.r11 = 0x246;
        entry.orig_rax = u64::MAX;
        entry.rsp = 0x7fff_0000;
        entry.rip = 0x7000_1001;
        entry.eflags = 0x246;
        let completion_rip = 0x7000_1100;
        let mut completion = entry;
        completion.rip = completion_rip;
        assert!(liteinst_completion_registers_match(
            &entry,
            &completion,
            completion_rip
        ));

        for mutate in [
            |registers: &mut libc::user_regs_struct| registers.r15 ^= 1,
            |registers: &mut libc::user_regs_struct| registers.rax ^= 1,
            |registers: &mut libc::user_regs_struct| registers.rsp ^= 8,
            |registers: &mut libc::user_regs_struct| registers.eflags ^= 1,
            |registers: &mut libc::user_regs_struct| registers.orig_rax ^= 1,
        ] {
            let mut forged = completion;
            mutate(&mut forged);
            assert!(!liteinst_completion_registers_match(
                &entry,
                &forged,
                completion_rip
            ));
        }
        assert!(!liteinst_completion_registers_match(
            &entry,
            &completion,
            completion_rip + 1
        ));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn installed_logical_entry_matches_native_seccomp_register_shape() {
        let mut entry = unsafe { core::mem::zeroed::<libc::user_regs_struct>() };
        entry.rax = Sysno::getpid as u64;
        entry.orig_rax = u64::MAX;
        entry.rip = 0x7000_0101;
        entry.rcx = 0xaaaa;
        entry.r11 = 0xbbbb;
        entry.eflags = 0x246;
        let continuation = 0x401007;
        let logical = liteinst_logical_syscall_entry_registers(&entry, continuation);
        assert_eq!(logical.rax as i64, -(libc::ENOSYS as i64));
        assert_eq!(logical.orig_rax, Sysno::getpid as u64);
        assert_eq!(logical.rip, continuation);
        assert_eq!(logical.rcx, continuation);
        assert_eq!(logical.r11, entry.eflags);
        let mut expected = entry;
        expected.rax = (-(libc::ENOSYS as i64)) as u64;
        expected.orig_rax = Sysno::getpid as u64;
        expected.rip = continuation;
        expected.rcx = continuation;
        expected.r11 = entry.eflags;
        assert_eq!(
            liteinst_register_words(&logical),
            liteinst_register_words(&expected)
        );
    }
}
