/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Copying a whole KVM run with one host `fork()`.
//!
//! PROTOTYPE. A forked child inherits this process's memory, so every guest
//! address space, every emulated kernel object and every Tool state comes
//! across copy-on-write. Three things do not come across correctly, and this
//! module repairs them in the child:
//!
//! - **VMs and vCPUs.** KVM refuses every VM and vCPU ioctl from a process
//!   whose memory map is not the creator's (`EIO`). The parent reads each
//!   vCPU's state before forking ([`prepare`]); the child builds a new VM and
//!   vCPU over the same guest memory, restores that state, and puts the new
//!   objects behind the OLD descriptor numbers and the old `kvm_run` address
//!   ([`repair_in_child`]), so no backend structure has to learn it was forked.
//! - **The pending exit.** A vCPU parked in a syscall hypercall exit waits for
//!   KVM to complete that exit at the next `KVM_RUN`. A new vCPU has no such
//!   pending exit, so the child runs the new vCPU once from the trapping
//!   instruction, which raises the same exit again and leaves KVM in the same
//!   state as the parent's vCPU. A vCPU parked after an interrupted entry or a
//!   debug exit has nothing pending and needs no replay.
//! - **Guest memory.** Guest arenas are memfd mappings shared with the parent.
//!   The child gives each memfd a private copy behind the same descriptor
//!   numbers and re-maps every shared mapping of it at the same address.
//!
//! What it does not do: re-create the host threads that run the other VMs'
//! vCPUs (the caller parks and re-creates every thread around the fork),
//! re-open host descriptors of guest files (the caller does that), or anything
//! for other backends.

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use kvm_bindings::CpuId;
use kvm_bindings::Msrs;
use kvm_bindings::kvm_debugregs;
use kvm_bindings::kvm_enable_cap;
use kvm_bindings::kvm_msr_entry;
use kvm_bindings::kvm_regs;
use kvm_bindings::kvm_sregs;
use kvm_bindings::kvm_userspace_memory_region;
use kvm_bindings::kvm_vcpu_events;
use kvm_bindings::kvm_xcrs;
use kvm_bindings::kvm_xsave;
use kvm_ioctls::Kvm;
use kvm_ioctls::VcpuExit;
use kvm_ioctls::VcpuFd;
use kvm_ioctls::VmFd;

/// MSRs restored first, in this order: CPUID faulting can be enabled in
/// `MISC_FEATURES_ENABLES` (0x140) only once `PLATFORM_INFO` (0xce) offers it.
const FIRST_MSRS: &[u32] = &[0xce];

/// MSRs this backend sets on every vCPU, read even if the host's save list
/// lacks one: STAR, LSTAR, CSTAR, SFMASK, `PLATFORM_INFO`,
/// `MISC_FEATURES_ENABLES` and `IA32_XSS` (the last may be absent).
const REQUIRED_MSRS: &[u32] = &[
    0xc000_0081,
    0xc000_0082,
    0xc000_0083,
    0xc000_0084,
    0xce,
    0x140,
    0xda0,
];

/// What KVM_GET_CPUID2 may return at most.
const MAX_CPUID_ENTRIES: usize = 256;

/// Bumped by every [`repair_in_child`]: a vCPU that cached a KVM setting
/// (its guest debugging mode) compares this with the value it cached it under.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The number of host forks this process has been repaired after.
pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// One vCPU's state, read by the parent before the fork.
struct SavedVcpu {
    cpuid: CpuId,
    msrs: Vec<kvm_msr_entry>,
    sregs: kvm_sregs,
    regs: kvm_regs,
    xsave: kvm_xsave,
    xcrs: kvm_xcrs,
    debugregs: kvm_debugregs,
    events: kvm_vcpu_events,
    /// The `kvm_run` structure as the last exit left it.
    run_page: Vec<u8>,
    exit_reason: u32,
    hypercall_nr: u64,
}

struct VcpuRecord {
    fd: RawFd,
    id: u64,
    run_address: usize,
    run_size: usize,
    saved: Option<SavedVcpu>,
}

struct VmRecord {
    token: u64,
    kvm_fd: RawFd,
    vm_fd: RawFd,
    regions: Vec<kvm_userspace_memory_region>,
    caps: Vec<kvm_enable_cap>,
    vcpus: Vec<VcpuRecord>,
}

static REGISTRY: Mutex<Vec<VmRecord>> = Mutex::new(Vec::new());

/// The parent's perf event pages, which Linux does not copy into a child
/// (`VM_DONTCOPY`): the child refills each range with a zeroed page, so that
/// nothing else is mapped there before the counter's owner unmaps it. A
/// zeroed page reads as "no hardware counter", which sends a reader to the
/// `read` system call.
static UNCOPIED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn uncopied_ranges() -> Vec<(usize, usize)> {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return Vec::new();
    };
    maps.lines()
        .filter(|line| line.ends_with("anon_inode:[perf_event]"))
        .filter_map(|line| {
            let (start, end) = line.split_whitespace().next()?.split_once('-')?;
            Some((
                usize::from_str_radix(start, 16).ok()?,
                usize::from_str_radix(end, 16).ok()?,
            ))
        })
        .collect()
}

/// Fills, in a child, the ranges [`UNCOPIED`] recorded. Returns how many.
fn refill_uncopied() -> Result<usize, String> {
    let ranges = UNCOPIED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for &(start, end) in &ranges {
        // SAFETY: the range was the parent's perf page, absent from this
        // copy; MAP_FIXED_NOREPLACE refuses to replace anything mapped since.
        let mapped = unsafe {
            libc::mmap(
                start as *mut libc::c_void,
                end - start,
                libc::PROT_READ,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED_NOREPLACE,
                -1,
                0,
            )
        };
        if mapped as usize != start {
            return Err(format!(
                "child: refilling the parent's perf page at {start:#x}: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(ranges.len())
}
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Keeps a VM in the registry while the backend that owns it lives.
#[derive(Debug)]
pub(crate) struct Registration {
    token: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        registry.retain(|record| record.token != self.token);
    }
}

/// Records a VM with one vCPU, as `KvmBackend` builds them.
pub(crate) fn register(
    kvm: &Kvm,
    vm_fd: RawFd,
    region: kvm_userspace_memory_region,
    cap: kvm_enable_cap,
    vcpu: &mut VcpuFd,
    vcpu_id: u64,
) -> Registration {
    let run_address = vcpu.get_kvm_run() as *mut kvm_bindings::kvm_run as usize;
    let run_size = kvm.get_vcpu_mmap_size().unwrap_or(4096);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let record = VmRecord {
        token,
        kvm_fd: kvm.as_raw_fd(),
        vm_fd,
        regions: vec![region],
        caps: vec![cap],
        vcpus: vec![VcpuRecord {
            fd: vcpu.as_raw_fd(),
            id: vcpu_id,
            run_address,
            run_size,
            saved: None,
        }],
    };
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(record);
    Registration { token }
}

fn ioctl_ptr<T>(fd: RawFd, request: libc::c_ulong, arg: *mut T) -> std::io::Result<libc::c_int> {
    // SAFETY: each caller passes the argument type the request names.
    let rc = unsafe { libc::ioctl(fd, request as _, arg) };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

const fn ioc<T>(dir: u64, nr: u64) -> libc::c_ulong {
    ((dir << 30) | ((std::mem::size_of::<T>() as u64) << 16) | (0xAE << 8) | nr) as libc::c_ulong
}

const KVM_GET_REGS: libc::c_ulong = ioc::<kvm_regs>(2, 0x81);
const KVM_GET_SREGS: libc::c_ulong = ioc::<kvm_sregs>(2, 0x83);
const KVM_GET_MSRS: libc::c_ulong = ioc::<kvm_bindings::kvm_msrs>(3, 0x88);
const KVM_GET_CPUID2: libc::c_ulong = ioc::<kvm_bindings::kvm_cpuid2>(3, 0x91);
const KVM_GET_VCPU_EVENTS: libc::c_ulong = ioc::<kvm_vcpu_events>(2, 0x9f);
const KVM_GET_DEBUGREGS: libc::c_ulong = ioc::<kvm_debugregs>(2, 0xa1);
const KVM_GET_XSAVE: libc::c_ulong = ioc::<kvm_xsave>(2, 0xa4);
const KVM_GET_XCRS: libc::c_ulong = ioc::<kvm_xcrs>(2, 0xa6);
const _: () = assert!(KVM_GET_REGS == 0x8090_ae81);
const _: () = assert!(KVM_GET_MSRS == 0xc008_ae88);
const _: () = assert!(KVM_GET_CPUID2 == 0xc008_ae91);

fn read_msr(fd: RawFd, index: u32) -> Option<kvm_msr_entry> {
    let mut one = Msrs::from_entries(&[kvm_msr_entry {
        index,
        ..Default::default()
    }])
    .ok()?;
    match ioctl_ptr(fd, KVM_GET_MSRS, one.as_mut_fam_struct_ptr()) {
        Ok(1) => Some(one.as_slice()[0]),
        _ => None,
    }
}

/// The MSRs to carry across: the host's save list, plus [`REQUIRED_MSRS`].
fn msr_indices(kvm_fd: RawFd) -> Vec<u32> {
    // SAFETY: a live /dev/kvm descriptor, borrowed: ManuallyDrop never closes it.
    let kvm = std::mem::ManuallyDrop::new(unsafe { Kvm::from_raw_fd(kvm_fd) });
    let mut indices: Vec<u32> = kvm
        .get_msr_index_list()
        .map(|list| list.as_slice().to_vec())
        .unwrap_or_default();
    for index in REQUIRED_MSRS {
        if !indices.contains(index) {
            indices.push(*index);
        }
    }
    indices
}

fn read_vcpu(kvm_fd: RawFd, fd: RawFd, run_address: usize) -> Result<SavedVcpu, String> {
    let err = |what: &str, e: std::io::Error| format!("parked vCPU {fd}: {what}: {e}");
    let mut regs = kvm_regs::default();
    ioctl_ptr(fd, KVM_GET_REGS, &mut regs).map_err(|e| err("KVM_GET_REGS", e))?;
    let mut sregs = kvm_sregs::default();
    ioctl_ptr(fd, KVM_GET_SREGS, &mut sregs).map_err(|e| err("KVM_GET_SREGS", e))?;
    let mut xsave = kvm_xsave::default();
    ioctl_ptr(fd, KVM_GET_XSAVE, &mut xsave).map_err(|e| err("KVM_GET_XSAVE", e))?;
    let mut xcrs = kvm_xcrs::default();
    ioctl_ptr(fd, KVM_GET_XCRS, &mut xcrs).map_err(|e| err("KVM_GET_XCRS", e))?;
    let mut debugregs = kvm_debugregs::default();
    ioctl_ptr(fd, KVM_GET_DEBUGREGS, &mut debugregs).map_err(|e| err("KVM_GET_DEBUGREGS", e))?;
    let mut events = kvm_vcpu_events::default();
    ioctl_ptr(fd, KVM_GET_VCPU_EVENTS, &mut events).map_err(|e| err("KVM_GET_VCPU_EVENTS", e))?;
    let mut cpuid = CpuId::new(MAX_CPUID_ENTRIES).map_err(|e| format!("CpuId: {e:?}"))?;
    ioctl_ptr(fd, KVM_GET_CPUID2, cpuid.as_mut_fam_struct_ptr())
        .map_err(|e| err("KVM_GET_CPUID2", e))?;
    let msrs = msr_indices(kvm_fd)
        .into_iter()
        .filter_map(|index| read_msr(fd, index))
        .collect::<Vec<_>>();
    for index in REQUIRED_MSRS {
        if !msrs.iter().any(|entry| entry.index == *index) && *index != 0xda0 {
            return Err(format!(
                "parked vCPU {fd}: MSR {index:#x} could not be read"
            ));
        }
    }

    let run = run_address as *const kvm_bindings::kvm_run;
    let mut run_page = vec![0u8; std::mem::size_of::<kvm_bindings::kvm_run>()];
    // SAFETY: run_address is this vCPU's live kvm_run mapping and the vCPU is
    // parked, so nothing writes it concurrently.
    unsafe {
        std::ptr::copy_nonoverlapping(run.cast::<u8>(), run_page.as_mut_ptr(), run_page.len());
    }
    // SAFETY: as above.
    let exit_reason = unsafe { (*run).exit_reason };
    let hypercall_nr = if exit_reason == kvm_bindings::KVM_EXIT_HYPERCALL {
        // SAFETY: the exit reason names the active union member.
        unsafe { (*run).__bindgen_anon_1.hypercall.nr }
    } else {
        0
    };
    Ok(SavedVcpu {
        cpuid,
        msrs,
        sregs,
        regs,
        xsave,
        xcrs,
        debugregs,
        events,
        run_page,
        exit_reason,
        hypercall_nr,
    })
}

/// What [`prepare`] found.
#[derive(Debug, Default, Clone, Copy)]
pub struct PrepareReport {
    pub vms: usize,
    pub vcpus: usize,
    pub msrs: usize,
    pub exit_reason: u32,
    pub us: u64,
}

/// Reads every registered vCPU's state, in the parent, before the fork.
/// Each vCPU must be parked in a syscall hypercall exit (re-created in the
/// child), after an interrupted entry or a debug exit, or before its first
/// entry (nothing pending).
pub fn prepare() -> Result<PrepareReport, String> {
    let start = std::time::Instant::now();
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if registry.is_empty() {
        return Err("no live KVM VM to copy".to_string());
    }
    *UNCOPIED.lock().unwrap_or_else(|e| e.into_inner()) = uncopied_ranges();
    let mut report = PrepareReport::default();
    for vm in registry.iter_mut() {
        report.vms += 1;
        for vcpu in vm.vcpus.iter_mut() {
            let saved = read_vcpu(vm.kvm_fd, vcpu.fd, vcpu.run_address)?;
            match saved.exit_reason {
                // KVM_EXIT_UNKNOWN is the zeroed kvm_run of a vCPU that has
                // not run yet, and KVM_EXIT_HLT a vCPU parked at its HLT (the
                // instruction completed): nothing is pending, as after an
                // interrupt.
                kvm_bindings::KVM_EXIT_HYPERCALL
                | kvm_bindings::KVM_EXIT_INTR
                | kvm_bindings::KVM_EXIT_DEBUG
                | kvm_bindings::KVM_EXIT_HLT
                | kvm_bindings::KVM_EXIT_UNKNOWN => {}
                other => {
                    return Err(format!(
                        "vCPU {} is parked in exit reason {other}, whose completion a new \
                         vCPU cannot re-create; capture refused",
                        vcpu.fd
                    ));
                }
            }
            report.vcpus += 1;
            report.msrs = saved.msrs.len();
            report.exit_reason = saved.exit_reason;
            vcpu.saved = Some(saved);
        }
    }
    report.us = start.elapsed().as_micros() as u64;
    Ok(report)
}

/// What one child's repair did, with its timings in microseconds.
#[derive(Debug, Default, Clone)]
pub struct RepairReport {
    pub refilled: usize,
    pub memfds: usize,
    pub memfd_bytes: u64,
    pub mappings: usize,
    pub memfd_us: u64,
    /// Guest-memory memfds mapped copy-on-write instead of copied
    /// (`REVERIE_KVM_FORK_PRIVATE_ARENAS`), and the bytes of their data
    /// extents.
    pub private_arenas: usize,
    pub private_arena_bytes: u64,
    pub vms: usize,
    pub msrs_restored: usize,
    pub msrs_refused: usize,
    pub replays: usize,
    pub vm_us: u64,
    /// Time per rebuild step, summed over every VM and vCPU, in step order.
    pub vm_steps_us: Vec<(&'static str, u64)>,
    /// KVM_CREATE_VM time of each VM, in rebuild order.
    pub create_vm_us: Vec<u64>,
}

/// Sums the time of each named step of a VM rebuild.
struct StepClock {
    last: std::time::Instant,
}

impl StepClock {
    fn start() -> Self {
        Self {
            last: std::time::Instant::now(),
        }
    }

    fn mark(&mut self, report: &mut RepairReport, name: &'static str) {
        let now = std::time::Instant::now();
        let us = now.duration_since(self.last).as_micros() as u64;
        self.last = now;
        match report
            .vm_steps_us
            .iter_mut()
            .find(|(step, _)| *step == name)
        {
            Some((_, total)) => *total += us,
            None => report.vm_steps_us.push((name, us)),
        }
    }
}

/// What a copy's guest memory did while the copy ran, for its measurements:
/// the memfds it still maps copy-on-write, and those it gave a memfd of its
/// own because something reached their bytes through a descriptor.
pub fn exit_report() -> String {
    let (left, materialized, bytes) = reverie_kvm_core::host_fork::private_memfd_counts();
    format!("private_memfds_left={left} materialized={materialized} materialized_bytes={bytes}")
}

/// Rebuilds, in a forked child, every registered VM and vCPU and every memfd.
pub fn repair_in_child() -> Result<RepairReport, String> {
    // First, before anything maps memory into the holes fork left.
    let refilled = refill_uncopied()?;
    reverie_kvm_core::host_fork::mark_forked_child();
    let mut report = RepairReport {
        refilled,
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    privatize_memfds(&mut report)?;
    report.memfd_us = t0.elapsed().as_micros() as u64;
    let t1 = std::time::Instant::now();
    let registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    // Every VM is created before any vCPU runs: after the first KVM_RUN in
    // this process, each later KVM_CREATE_VM waited about 12 ms or 24 ms
    // (MEASURED on a loaded host), while all of them before it took 0.2 ms.
    let mut built = Vec::new();
    for vm in registry.iter() {
        built.push(build_vm(vm, &mut report)?);
    }
    for vm in built {
        finish_vm(vm, &mut report)?;
        report.vms += 1;
    }
    report.vm_us = t1.elapsed().as_micros() as u64;
    GENERATION.fetch_add(1, Ordering::Relaxed);
    Ok(report)
}

/// A VM rebuilt in the child with its vCPUs' state set, not yet run.
struct BuiltVm<'a> {
    record: &'a VmRecord,
    vm: VmFd,
    vcpus: Vec<(&'a VcpuRecord, VcpuFd)>,
}

fn build_vm<'a>(vm: &'a VmRecord, report: &mut RepairReport) -> Result<BuiltVm<'a>, String> {
    // SAFETY: a live /dev/kvm descriptor, borrowed: ManuallyDrop never closes it.
    let kvm = std::mem::ManuallyDrop::new(unsafe { Kvm::from_raw_fd(vm.kvm_fd) });
    let mut clock = StepClock::start();
    let created = std::time::Instant::now();
    let new_vm = kvm
        .create_vm()
        .map_err(|e| format!("child: KVM_CREATE_VM: {e}"))?;
    report
        .create_vm_us
        .push(created.elapsed().as_micros() as u64);
    clock.mark(report, "create_vm");
    for cap in &vm.caps {
        new_vm
            .enable_cap(cap)
            .map_err(|e| format!("child: KVM_ENABLE_CAP {}: {e}", cap.cap))?;
    }
    clock.mark(report, "enable_cap");
    for region in &vm.regions {
        // SAFETY: the same host range the parent registered, which the
        // child's copy of the address space keeps mapped.
        unsafe { new_vm.set_user_memory_region(*region) }
            .map_err(|e| format!("child: KVM_SET_USER_MEMORY_REGION: {e}"))?;
    }
    clock.mark(report, "memslot");
    let mut new_vcpus = Vec::new();
    for record in &vm.vcpus {
        let saved = record
            .saved
            .as_ref()
            .ok_or_else(|| "child: a vCPU was not prepared before the fork".to_string())?;
        let vcpu = new_vm
            .create_vcpu(record.id)
            .map_err(|e| format!("child: KVM_CREATE_VCPU: {e}"))?;
        clock.mark(report, "create_vcpu");
        vcpu.set_cpuid2(&saved.cpuid)
            .map_err(|e| format!("child: KVM_SET_CPUID2: {e}"))?;
        clock.mark(report, "cpuid");
        vcpu.set_sregs(&saved.sregs)
            .map_err(|e| format!("child: KVM_SET_SREGS: {e}"))?;
        vcpu.set_xcrs(&saved.xcrs)
            .map_err(|e| format!("child: KVM_SET_XCRS: {e}"))?;
        clock.mark(report, "sregs_xcrs");
        let ordered = FIRST_MSRS
            .iter()
            .filter_map(|first| saved.msrs.iter().find(|entry| entry.index == *first))
            .chain(
                saved
                    .msrs
                    .iter()
                    .filter(|entry| !FIRST_MSRS.contains(&entry.index)),
            );
        for entry in ordered {
            let one = Msrs::from_entries(&[*entry]).map_err(|e| format!("Msrs: {e:?}"))?;
            match vcpu.set_msrs(&one) {
                Ok(1) => report.msrs_restored += 1,
                _ if REQUIRED_MSRS.contains(&entry.index) => {
                    return Err(format!("child: KVM_SET_MSRS {:#x} refused", entry.index));
                }
                _ => report.msrs_refused += 1,
            }
        }
        clock.mark(report, "msrs");
        vcpu.set_regs(&saved.regs)
            .map_err(|e| format!("child: KVM_SET_REGS: {e}"))?;
        // SAFETY: the state came from KVM_GET_XSAVE of a vCPU with the same
        // CPUID, whose setup enables no dynamically sized XSTATE feature.
        unsafe { vcpu.set_xsave(&saved.xsave) }
            .map_err(|e| format!("child: KVM_SET_XSAVE: {e}"))?;
        vcpu.set_debug_regs(&saved.debugregs)
            .map_err(|e| format!("child: KVM_SET_DEBUGREGS: {e}"))?;
        vcpu.set_vcpu_events(&saved.events)
            .map_err(|e| format!("child: KVM_SET_VCPU_EVENTS: {e}"))?;
        clock.mark(report, "regs_xsave_events");
        new_vcpus.push((record, vcpu));
    }
    Ok(BuiltVm {
        record: vm,
        vm: new_vm,
        vcpus: new_vcpus,
    })
}

/// Runs each vCPU's parked hypercall again, gives it the parent's last
/// exit, and puts the new objects behind the old descriptor numbers.
fn finish_vm(built: BuiltVm<'_>, report: &mut RepairReport) -> Result<(), String> {
    let BuiltVm {
        record: vm,
        vm: new_vm,
        vcpus: mut new_vcpus,
    } = built;
    let mut clock = StepClock::start();
    for (record, vcpu) in new_vcpus.iter_mut() {
        let saved = record
            .saved
            .as_ref()
            .ok_or_else(|| "child: a vCPU was not prepared before the fork".to_string())?;
        if saved.exit_reason == kvm_bindings::KVM_EXIT_HYPERCALL {
            replay_hypercall(vcpu, saved)?;
            report.replays += 1;
        }
        clock.mark(report, "replay");
        // The backend reads the parent's view of the last exit: give it that.
        let run = vcpu.get_kvm_run() as *mut kvm_bindings::kvm_run as *mut u8;
        // SAFETY: the new vCPU's own mapping holds a whole kvm_run.
        unsafe {
            std::ptr::copy_nonoverlapping(saved.run_page.as_ptr(), run, saved.run_page.len())
        };
        clock.mark(report, "run_page");
    }
    // Put the new objects behind the old numbers and the old kvm_run address.
    dup_onto(new_vm.as_raw_fd(), vm.vm_fd)?;
    for (record, vcpu) in &new_vcpus {
        dup_onto(vcpu.as_raw_fd(), record.fd)?;
        // SAFETY: the old address held the parent vCPU's kvm_run mapping,
        // which only the backend's VcpuFd refers to; MAP_FIXED replaces it
        // with the new vCPU's, now behind the same number.
        let mapped = unsafe {
            libc::mmap(
                record.run_address as *mut libc::c_void,
                record.run_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_FIXED,
                record.fd,
                0,
            )
        };
        if mapped as usize != record.run_address {
            return Err(format!(
                "child: re-mapping kvm_run at {:#x}: {}",
                record.run_address,
                std::io::Error::last_os_error()
            ));
        }
    }
    // Dropping the new handles closes their own numbers and unmaps their own
    // kvm_run views; the objects live on behind the old numbers.
    clock.mark(report, "dup_remap");
    drop(new_vcpus);
    drop(new_vm);
    clock.mark(report, "close_spare");
    Ok(())
}

/// Runs the new vCPU from the parked VMCALL until it raises the same exit,
/// so that KVM completes it at the backend's next entry, as it would have
/// for the parent's vCPU.
fn replay_hypercall(vcpu: &mut VcpuFd, saved: &SavedVcpu) -> Result<(), String> {
    {
        let run = vcpu.get_kvm_run();
        run.immediate_exit = 0;
        // Registers come from KVM_SET_REGS above, never from a stale sync area.
        run.kvm_dirty_regs = 0;
    }
    let mut attempts = 0;
    loop {
        attempts += 1;
        match vcpu.run() {
            Ok(VcpuExit::Hypercall(exit)) if exit.nr == saved.hypercall_nr => return Ok(()),
            Ok(other) => {
                return Err(format!(
                    "child: replaying the parked hypercall gave another exit: {other:?}"
                ));
            }
            Err(e) if e.errno() == libc::EINTR && attempts < 100 => continue,
            Err(e) => return Err(format!("child: KVM_RUN replay: {e}")),
        }
    }
}

fn dup_onto(from: RawFd, onto: RawFd) -> Result<(), String> {
    // SAFETY: fcntl on a live descriptor of this process.
    let flags = unsafe { libc::fcntl(onto, libc::F_GETFD) };
    let cloexec = if flags >= 0 && flags & libc::FD_CLOEXEC != 0 {
        libc::O_CLOEXEC
    } else {
        0
    };
    // SAFETY: both are descriptors of this process; dup3 replaces `onto`.
    if unsafe { libc::dup3(from, onto, cloexec) } < 0 {
        return Err(format!(
            "child: dup3({from}, {onto}): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// One shared mapping of a memfd, from /proc/self/maps.
struct SharedMapping {
    start: usize,
    end: usize,
    offset: u64,
    prot: libc::c_int,
}

/// One memfd inode: its descriptors and its shared mappings.
#[derive(Default)]
struct MemfdInode {
    name: String,
    fds: Vec<RawFd>,
    mappings: Vec<SharedMapping>,
}

fn memfd_inodes() -> Result<BTreeMap<u64, MemfdInode>, String> {
    let mut inodes: BTreeMap<u64, MemfdInode> = BTreeMap::new();
    let entries =
        std::fs::read_dir("/proc/self/fd").map_err(|e| format!("child: /proc/self/fd: {e}"))?;
    for entry in entries.flatten() {
        let Ok(fd) = entry.file_name().to_string_lossy().parse::<RawFd>() else {
            continue;
        };
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        let Some(name) = target.strip_prefix("/memfd:") else {
            continue;
        };
        // SAFETY: fstat of a descriptor into a zeroed buffer.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::fstat(fd, &mut stat) } < 0 {
            continue;
        }
        let inode = inodes.entry(stat.st_ino).or_default();
        inode.name = name.trim_end_matches(" (deleted)").to_string();
        inode.fds.push(fd);
    }
    let maps = std::fs::read_to_string("/proc/self/maps")
        .map_err(|e| format!("child: reading /proc/self/maps: {e}"))?;
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let (Some(range), Some(perms), Some(offset), Some(_dev), Some(inode)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let path: String = fields.collect::<Vec<_>>().join(" ");
        let Some(name) = path.strip_prefix("/memfd:") else {
            continue;
        };
        let perms = perms.as_bytes();
        if perms.get(3) != Some(&b's') {
            continue;
        }
        let (Some((start, end)), Ok(offset), Ok(inode)) = (
            range.split_once('-'),
            u64::from_str_radix(offset, 16),
            inode.parse::<u64>(),
        ) else {
            continue;
        };
        let (Ok(start), Ok(end)) = (
            usize::from_str_radix(start, 16),
            usize::from_str_radix(end, 16),
        ) else {
            continue;
        };
        let mut prot = 0;
        if perms[0] == b'r' {
            prot |= libc::PROT_READ;
        }
        if perms[1] == b'w' {
            prot |= libc::PROT_WRITE;
        }
        if perms[2] == b'x' {
            prot |= libc::PROT_EXEC;
        }
        let record = inodes.entry(inode).or_default();
        if record.name.is_empty() {
            record.name = name.trim_end_matches(" (deleted)").to_string();
        }
        record.mappings.push(SharedMapping {
            start,
            end,
            offset,
            prot,
        });
    }
    Ok(inodes)
}

fn new_memfd(name: &str) -> Result<RawFd, String> {
    let cname = std::ffi::CString::new(name).unwrap_or_default();
    for flags in [
        libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING | libc::MFD_NOEXEC_SEAL,
        libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
    ] {
        // SAFETY: a new descriptor, owned by the caller.
        let fd = unsafe { libc::memfd_create(cname.as_ptr(), flags) };
        if fd >= 0 {
            return Ok(fd);
        }
    }
    Err(format!(
        "child: memfd_create: {}",
        std::io::Error::last_os_error()
    ))
}

/// The memfd name guest-memory backings are created with.
const GUEST_MEMORY_NAME: &str = "reverie-kvm-guest-memory";

/// The environment variable that selects copy-on-write guest memory for a
/// host-fork copy (PROTOTYPE): `1` maps each eligible guest-memory memfd
/// copy-on-write instead of copying it. A program that passes its own
/// environment to its guest should remove it from the guest's.
pub const PRIVATE_ARENAS_ENV: &str = "REVERIE_KVM_FORK_PRIVATE_ARENAS";

fn private_arenas() -> bool {
    std::env::var_os(PRIVATE_ARENAS_ENV).is_some_and(|value| value == "1")
}

/// Whether no byte of the memfd is mapped twice: then each mapping can become
/// a private copy-on-write mapping without splitting a view that two
/// mappings must share.
fn mappings_disjoint(mappings: &[SharedMapping]) -> bool {
    let mut ranges: Vec<(u64, u64)> = mappings
        .iter()
        .map(|m| (m.offset, m.offset + (m.end - m.start) as u64))
        .collect();
    ranges.sort_unstable();
    ranges.windows(2).all(|pair| pair[0].1 <= pair[1].0)
}

/// Re-maps every mapping of a guest-memory memfd as a private copy-on-write
/// mapping of the same file, which the template no longer writes, and
/// records it ([`reverie_kvm_core::host_fork::register_private_memfd`]):
/// guest memory then reaches its descriptor only after the copy has a memfd
/// of its own with its bytes ([`reverie_kvm_core::host_fork::materialize_private_memfd`]).
/// The descriptors are reopened read-only meanwhile, so that a write through
/// one that bypassed that step fails rather than reaching the template and
/// every other copy.
fn map_copy_on_write(inode: &MemfdInode, report: &mut RepairReport) -> Result<(), String> {
    let source = inode.fds[0];
    // SAFETY: fstat of a descriptor into a zeroed buffer.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    if unsafe { libc::fstat(source, &mut stat) } < 0 {
        return Err(format!(
            "child: fstat {}: {}",
            inode.name,
            std::io::Error::last_os_error()
        ));
    }
    for mapping in &inode.mappings {
        // The file's data extents become private views of the file; its holes,
        // which the template never touched, become private anonymous memory:
        // a copy's first store there then costs one anonymous page, where a
        // private view of a hole would first allocate a page in the shared
        // file and then copy it.
        let length = (mapping.end - mapping.start) as u64;
        let mut pieces: Vec<(u64, u64, bool)> = Vec::new();
        let mut cursor = mapping.offset;
        let end = mapping.offset + length;
        while cursor < end {
            // SAFETY: lseek on a live descriptor.
            let data = unsafe { libc::lseek(source, cursor as libc::off_t, libc::SEEK_DATA) };
            let data = if data < 0 {
                end
            } else {
                (data as u64).min(end)
            };
            if data > cursor {
                pieces.push((cursor, data, false));
            }
            if data >= end {
                break;
            }
            // SAFETY: as above.
            let hole = unsafe { libc::lseek(source, data as libc::off_t, libc::SEEK_HOLE) };
            let hole = if hole < 0 {
                end
            } else {
                (hole as u64).min(end)
            };
            pieces.push((data, hole, true));
            cursor = hole;
        }
        for (from, to, data) in pieces {
            let address = mapping.start + (from - mapping.offset) as usize;
            let size = (to - from) as usize;
            // SAFETY: replaces part of this process's own shared mapping of the
            // memfd, at the same address, with a private view of the same file
            // range or with zero-filled private memory.
            let mapped = unsafe {
                if data {
                    libc::mmap(
                        address as *mut libc::c_void,
                        size,
                        mapping.prot,
                        libc::MAP_PRIVATE | libc::MAP_FIXED | libc::MAP_NORESERVE,
                        source,
                        from as libc::off_t,
                    )
                } else {
                    libc::mmap(
                        address as *mut libc::c_void,
                        size,
                        mapping.prot,
                        libc::MAP_PRIVATE
                            | libc::MAP_ANONYMOUS
                            | libc::MAP_FIXED
                            | libc::MAP_NORESERVE,
                        -1,
                        0,
                    )
                }
            };
            if mapped as usize != address {
                return Err(format!(
                    "child: private re-mapping of {} at {address:#x}: {}",
                    inode.name,
                    std::io::Error::last_os_error()
                ));
            }
            if data {
                report.private_arena_bytes += size as u64;
            } else {
                // The memfd these pages replace got huge pages where the host
                // gives shared memory huge pages; anonymous memory gets them
                // only when asked (MEASURED: without this, a copy that then
                // touched 1 GiB took 908-979 ms against 622-745 ms copied
                // eagerly). Advice only: a refusal changes no byte.
                // SAFETY: advice on the range just mapped.
                unsafe { libc::madvise(address as *mut libc::c_void, size, libc::MADV_HUGEPAGE) };
            }
        }
        report.mappings += 1;
    }
    let path = format!("/proc/self/fd/{source}\0");
    // SAFETY: a NUL-terminated path; a new read-only descriptor.
    let read_only = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if read_only < 0 {
        return Err(format!(
            "child: reopening {} read-only: {}",
            inode.name,
            std::io::Error::last_os_error()
        ));
    }
    for &fd in &inode.fds {
        dup_onto(read_only, fd)?;
    }
    // SAFETY: the duplicates keep the description open.
    unsafe { libc::close(read_only) };
    reverie_kvm_core::host_fork::register_private_memfd(
        stat.st_dev,
        stat.st_ino,
        &inode.name,
        inode
            .mappings
            .iter()
            .map(|m| reverie_kvm_core::host_fork::PrivateMapping {
                start: m.start,
                end: m.end,
                offset: m.offset,
                prot: m.prot,
            })
            .collect(),
    );
    report.private_arenas += 1;
    Ok(())
}

/// Gives every memfd of this process a private copy behind the same
/// descriptor numbers, and re-maps every shared mapping of it at the same
/// address with the same protection. A memfd with no descriptor left is
/// copied from its mappings.
fn privatize_memfds(report: &mut RepairReport) -> Result<(), String> {
    let copy_on_write = private_arenas();
    for (_, inode) in memfd_inodes()? {
        // A copy of a copy inherits the parent copy's private mappings, which
        // fork() already made copy-on-write, and its record of them: copying
        // such a memfd through its descriptor would give it the template's
        // bytes rather than this copy's.
        if inode
            .fds
            .first()
            .is_some_and(|&fd| reverie_kvm_core::host_fork::is_private_memfd(fd))
        {
            continue;
        }
        if copy_on_write
            && inode.name == GUEST_MEMORY_NAME
            && !inode.fds.is_empty()
            && !inode.mappings.is_empty()
            && mappings_disjoint(&inode.mappings)
        {
            map_copy_on_write(&inode, report)?;
            continue;
        }
        let new = new_memfd(&inode.name)?;
        if let Some(&source) = inode.fds.first() {
            // SAFETY: fstat of a descriptor into a zeroed buffer.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: as above.
            if unsafe { libc::fstat(source, &mut stat) } < 0 {
                return Err(format!(
                    "child: fstat memfd: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: the new descriptor.
            if unsafe { libc::ftruncate(new, stat.st_size) } < 0 {
                return Err(format!(
                    "child: ftruncate: {}",
                    std::io::Error::last_os_error()
                ));
            }
            report.memfd_bytes += copy_data_extents(source, new, stat.st_size as u64)?;
        } else {
            let size = inode
                .mappings
                .iter()
                .map(|m| m.offset + (m.end - m.start) as u64)
                .max()
                .unwrap_or(0);
            // SAFETY: the new descriptor.
            if unsafe { libc::ftruncate(new, size as libc::off_t) } < 0 {
                return Err(format!(
                    "child: ftruncate: {}",
                    std::io::Error::last_os_error()
                ));
            }
            for mapping in &inode.mappings {
                if mapping.prot & libc::PROT_READ == 0 {
                    continue;
                }
                let length = mapping.end - mapping.start;
                // SAFETY: the mapping is readable and live in this process.
                let written = unsafe {
                    libc::pwrite(
                        new,
                        mapping.start as *const libc::c_void,
                        length,
                        mapping.offset as libc::off_t,
                    )
                };
                if written != length as isize {
                    return Err(format!(
                        "child: copying an unreferenced memfd mapping: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                report.memfd_bytes += length as u64;
            }
        }
        for mapping in &inode.mappings {
            // SAFETY: replaces this process's own mapping of the memfd with
            // the same range of its private copy.
            let mapped = unsafe {
                libc::mmap(
                    mapping.start as *mut libc::c_void,
                    mapping.end - mapping.start,
                    mapping.prot,
                    libc::MAP_SHARED | libc::MAP_FIXED,
                    new,
                    mapping.offset as libc::off_t,
                )
            };
            if mapped as usize != mapping.start {
                return Err(format!(
                    "child: re-mapping memfd {} at {:#x}: {}",
                    inode.name,
                    mapping.start,
                    std::io::Error::last_os_error()
                ));
            }
            report.mappings += 1;
        }
        // Every descriptor of the old memfd now names the copy. Descriptors
        // that shared one open file description share the copy's; others
        // get their own, with the old offset and status flags.
        let mut done: Vec<RawFd> = Vec::new();
        for &fd in &inode.fds {
            let shared_with = done
                .iter()
                .copied()
                .find(|&other| same_description(fd, other));
            // SAFETY: lseek/fcntl on a live descriptor.
            let offset = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
            // SAFETY: as above.
            let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            match shared_with {
                Some(other) => dup_onto(other, fd)?,
                None if done.is_empty() => {
                    dup_onto(new, fd)?;
                    restore_position(fd, offset, status);
                }
                None => {
                    let path = format!("/proc/self/fd/{new}\0");
                    // SAFETY: a NUL-terminated path; a new descriptor.
                    let reopened =
                        unsafe { libc::open(path.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC) };
                    if reopened < 0 {
                        return Err(format!(
                            "child: reopening memfd copy: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    dup_onto(reopened, fd)?;
                    // SAFETY: the duplicate lives on behind `fd`.
                    unsafe { libc::close(reopened) };
                    restore_position(fd, offset, status);
                }
            }
            done.push(fd);
        }
        // SAFETY: the copy lives on behind the old numbers and mappings.
        unsafe { libc::close(new) };
        report.memfds += 1;
    }
    Ok(())
}

fn restore_position(fd: RawFd, offset: libc::off_t, status: libc::c_int) {
    if offset > 0 {
        // SAFETY: lseek on a live descriptor.
        unsafe { libc::lseek(fd, offset, libc::SEEK_SET) };
    }
    if status >= 0 {
        // SAFETY: fcntl on a live descriptor; F_SETFL changes only the
        // status flags Linux lets it change.
        unsafe { libc::fcntl(fd, libc::F_SETFL, status) };
    }
}

/// Whether two descriptors of this process name one open file description.
pub fn same_description(a: RawFd, b: RawFd) -> bool {
    const KCMP_FILE: libc::c_long = 0;
    // SAFETY: getpid has no preconditions.
    let pid = unsafe { libc::getpid() } as libc::c_long;
    // SAFETY: kcmp compares two descriptors of this process; no memory.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_kcmp,
            pid,
            pid,
            KCMP_FILE,
            a as libc::c_long,
            b as libc::c_long,
        )
    };
    rc == 0
}

/// Copies the data extents of the first `size` bytes of `from` into the
/// zero-filled `to`, skipping holes.
fn copy_data_extents(from: RawFd, to: RawFd, size: u64) -> Result<u64, String> {
    let mut copied = 0u64;
    let mut cursor: libc::off_t = 0;
    let end = size as libc::off_t;
    while cursor < end {
        // SAFETY: lseek on a live descriptor.
        let data = unsafe { libc::lseek(from, cursor, libc::SEEK_DATA) };
        if data < 0 {
            break;
        }
        // SAFETY: as above.
        let hole = unsafe { libc::lseek(from, data, libc::SEEK_HOLE) };
        let hole = if hole < 0 { end } else { hole.min(end) };
        let mut off_in = data;
        let mut off_out = data;
        let mut remaining = (hole - data) as usize;
        while remaining > 0 {
            // SAFETY: both descriptors are live; the offsets are local.
            let n =
                unsafe { libc::copy_file_range(from, &mut off_in, to, &mut off_out, remaining, 0) };
            if n <= 0 {
                return Err(format!(
                    "child: copy_file_range: {}",
                    std::io::Error::last_os_error()
                ));
            }
            remaining -= n as usize;
            copied += n as u64;
        }
        cursor = hole;
    }
    Ok(copied)
}
