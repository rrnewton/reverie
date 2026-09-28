/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * Licensed under the BSD-style license in the repository root LICENSE.
 */
//! Exact task-wide perf effects through the real supervisor and fallback route.
//! Native rows measure the ordinary event. Mediated rows additionally require
//! the Tool's actual private clock; missing clocks and active errors fail.
use core::arch::global_asm;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tid;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;
use reverie_ptrace::DisabledRcbEvent;

const BRANCHES: u64 = 4096;
const DISABLE: u64 = 31;
const ENABLE: u64 = 32;
const IOC_ENABLE: u64 = 0x2400;
const IOC_DISABLE: u64 = 0x2401;
const IOC_RESET: u64 = 0x2403;
const IOC_ID: u64 = 0x8008_2407;
const IORING_SETUP_SQPOLL: u32 = 1 << 1;
const IORING_SETUP_NO_MMAP: u32 = 1 << 14;
const IORING_SETUP_REGISTERED_FD_ONLY: u32 = 1 << 15;
const IORING_SETUP_NO_SQARRAY: u32 = 1 << 16;
const IORING_FEAT_SINGLE_MMAP: u32 = 1;
const IORING_FEAT_SQPOLL_NONFIXED: u32 = 1 << 7;
const IORING_ENTER_GETEVENTS: u32 = 1;
const IORING_ENTER_SQ_WAKEUP: u32 = 1 << 1;
const IORING_ENTER_REGISTERED_RING: u32 = 1 << 4;
const IORING_SQ_NEED_WAKEUP: u32 = 1;
const IORING_REGISTER_PROBE: u32 = 8;
const IORING_REGISTER_USE_REGISTERED_RING: u32 = 1 << 31;
const IO_RINGFD_REG_MAX: u32 = 16;
const IORING_OP_CLOSE: u8 = 19;
const IORING_OFF_SQ_RING: u64 = 0;
const IORING_OFF_CQ_RING: u64 = 0x0800_0000;
const IORING_OFF_SQES: u64 = 0x1000_0000;
const RING_USER_DATA: u64 = 0x5243_422d_434c_4f53;
static WARMUP: AtomicBool = AtomicBool::new(true);
static RUNNING: AtomicBool = AtomicBool::new(false);
static ACTIVE_RECORD: AtomicUsize = AtomicUsize::new(0);
static SAMPLES: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static TRANSPORT_MODE: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
#[repr(C)]
struct Record([u64; 20]);
const _: () = assert!(std::mem::size_of::<Record>() == 160);

struct Ordinary {
    fd: OwnedFd,
    metadata: NonNull<u8>,
    page: usize,
    event_id: u64,
}
impl Ordinary {
    fn new() -> Self {
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        assert!(tid > 0);
        // Intentionally created by this guest thread. Unlike the imported
        // runtime event, it must remain affected by task-wide prctl.
        let event = DisabledRcbEvent::for_thread(Tid::from_raw(tid as i32)).unwrap();
        let (fd, description) = event.into_parts();
        assert_ne!(description.event_id, 0);
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page >= 4096);
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page as usize,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        Self {
            fd,
            metadata: NonNull::new(mapping.cast()).unwrap(),
            page: page as usize,
            event_id: description.event_id,
        }
    }
    fn opposite_state(&self, selector: u64) {
        let control = if selector as u32 == DISABLE as u32 {
            IOC_ENABLE
        } else {
            IOC_DISABLE
        };
        let result = unsafe {
            raw(
                libc::SYS_ioctl,
                [self.fd.as_raw_fd() as u64, control, 0, 0, 0, 0],
            )
        };
        assert_eq!(result, 0);
    }
    fn check(&self, record: &Record, expected: u64) -> u64 {
        let r = &record.0;
        assert_eq!(r[4] as i64, 0, "actual prctl result: {r:?}");
        assert_eq!(r[5] & 1, 0, "metadata was being changed: {r:?}");
        assert_eq!(r[5], r[10], "metadata sequence changed: {r:?}");
        assert_eq!(r[6], r[11], "event was rescheduled: {r:?}");
        assert_eq!(r[7], r[12], "counter width changed: {r:?}");
        assert_eq!(r[9], r[14], "counter offset changed: {r:?}");
        assert_eq!(r[16], r[17], "counter capabilities changed: {r:?}");
        assert_ne!(r[16] & 4, 0, "actual user RDPMC capability is required");
        assert!((1..=64).contains(&r[7]), "actual width: {r:?}");
        let mask = u64::MAX >> (64 - r[7]);
        let delta = r[13].wrapping_sub(r[8]) & mask;
        if expected == 0 {
            assert_eq!(r[6], 0, "ordinary event must actually be inactive");
            // Inactive metadata supplies the actual count through its offset;
            // compare that value with a real complete kernel read as well.
            let mut count = 0_u64;
            let result = unsafe {
                raw(
                    libc::SYS_read,
                    [
                        self.fd.as_raw_fd() as u64,
                        (&raw mut count) as u64,
                        8,
                        0,
                        0,
                        0,
                    ],
                )
            };
            assert_eq!(result, 8, "disabled actual perf read");
            assert_eq!(count, r[14], "actual read disagrees with inactive metadata");
        } else {
            assert_ne!(r[6], 0, "ordinary event must actually be scheduled");
        }
        assert_eq!(delta, expected, "ordinary exact interval: {r:?}");
        delta
    }
}
impl Drop for Ordinary {
    fn drop(&mut self) {
        assert_eq!(
            unsafe { libc::munmap(self.metadata.as_ptr().cast(), self.page) },
            0
        );
    }
}

// The runtime retains installed sites until process exit. Keep this private
// fixture mapping live for that entire lifetime instead of unmapping a live hook.
struct Site {
    address: usize,
    installed: bool,
}
impl Site {
    fn new(installed: bool) -> Self {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page >= 4096);
        let page = page as usize;
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(mapping, libc::MAP_FAILED);
        let bytes: &[u8] = if installed {
            &[0x0f, 0x05, 0x90, 0x90, 0x90, 0xc3]
        } else {
            &[0x0f, 0x05, 0xc3]
        };
        let offset = if installed { 64 } else { page - bytes.len() };
        let site = unsafe { mapping.cast::<u8>().add(offset) };
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), site, bytes.len()) };
        assert_eq!(
            unsafe { libc::mprotect(mapping, page, libc::PROT_READ | libc::PROT_EXEC) },
            0
        );
        Self {
            address: site as usize,
            installed,
        }
    }
    fn check_counts(&self, hooks: u64, traps: u64) {
        assert_eq!(
            reverie_liteinst::reverie_liteinst_site_hook_count(self.address as u64),
            hooks
        );
        assert_eq!(
            reverie_liteinst::reverie_liteinst_site_trap_count(self.address as u64),
            traps
        );
        if self.installed {
            assert_ne!(
                unsafe { std::slice::from_raw_parts(self.address as *const u8, 5) },
                [0x0f, 0x05, 0x90, 0x90, 0x90]
            );
        } else {
            assert_eq!(
                unsafe { std::slice::from_raw_parts(self.address as *const u8, 3) },
                [0x0f, 0x05, 0xc3]
            );
        }
    }
}

fn pin_current_thread() -> usize {
    let mut permitted: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe {
            libc::sched_getaffinity(0, std::mem::size_of_val(&permitted), &raw mut permitted)
        },
        0
    );
    let selected = (0..libc::CPU_SETSIZE as usize)
        .find(|&cpu| unsafe { libc::CPU_ISSET(cpu, &permitted) })
        .expect("at least one permitted CPU");
    let mut chosen: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_SET(selected, &mut chosen) };
    assert_eq!(
        unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&chosen), &chosen) },
        0
    );
    assert_eq!(unsafe { libc::sched_getcpu() }, selected as i32);
    selected
}

fn record_installed_boundary(site: &Site, pid: i32) {
    fn code(address: u64, count: usize) -> String {
        use std::fmt::Write;
        assert!((1..=16384).contains(&count));
        let end = address.checked_add(count as u64).unwrap();
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        assert!(
            maps.lines().any(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                let (start, last) = fields[0].split_once('-').unwrap();
                let start = u64::from_str_radix(start, 16).unwrap();
                let last = u64::from_str_radix(last, 16).unwrap();
                start <= address
                    && end <= last
                    && fields[1].starts_with('r')
                    && fields[1].as_bytes()[2] == b'x'
            }),
            "complete live readable executable range"
        );
        // These addresses come from loaded assembly symbols or the owned live
        // fixture site's published trampoline; the process has one Tool thread.
        let bytes = unsafe { std::slice::from_raw_parts(address as *const u8, count) };
        let mut hex = String::new();
        for byte in bytes {
            write!(hex, "{byte:02x}").unwrap();
        }
        hex
    }
    let (table, entries) = reverie_liteinst::installed_callback_table();
    assert!(!entries.is_empty());
    println!(
        "rcb callback-table pid={pid} address={table:#x} count={}",
        entries.len()
    );
    for [entry, body, bytes, mode] in entries {
        let hex = code(entry, bytes.try_into().unwrap());
        println!(
            "rcb callback pid={pid} entry={entry:#x} body={body:#x} bytes={bytes} mode={mode} code={hex}"
        );
    }
    let (address, bytes) = reverie_preload::trap::rcb_callback_boundary();
    let hex = code(address, bytes);
    println!("rcb callback-boundary pid={pid} address={address:#x} bytes={bytes} code={hex}");
    // Native ownership of this mapping spans the whole fixture; no other Tool
    // thread can replace it. The fallback must have no generated trampoline.
    let layout = unsafe { reverie_liteinst::installed_trampoline_layout(site.address as u64) };
    if site.installed {
        let [
            address,
            bytes,
            instrumentation,
            restore,
            relocated,
            return_bytes,
        ] = layout.unwrap();
        assert_eq!(bytes, instrumentation + restore + relocated + return_bytes);
        let hex = code(address, bytes.try_into().unwrap());
        println!(
            "rcb trampoline pid={pid} site={:#x} address={address:#x} bytes={bytes} instrumentation={instrumentation} restore={restore} relocated={relocated} return={return_bytes} code={hex}",
            site.address
        );
    } else {
        assert!(layout.is_none());
        println!(
            "rcb trampoline pid={pid} site={:#x} absent=true",
            site.address
        );
    }
}

fn record_restorer() {
    // The kernel x86-64 action layout, not libc's differently ordered wrapper.
    #[repr(C)]
    #[derive(Default)]
    struct Action {
        handler: u64,
        flags: u64,
        restorer: u64,
        mask: u64,
    }
    const _: () = assert!(std::mem::size_of::<Action>() == 32);
    let mut action = Action::default();
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_rt_sigaction,
                [libc::SIGSYS as u64, 0, (&raw mut action) as u64, 8, 0, 0],
            )
        },
        0
    );
    assert_ne!(action.flags & 0x0400_0000, 0, "actual SA_RESTORER");
    assert_ne!(action.restorer, 0);
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let (mapping, fields, start) = maps
        .lines()
        .find_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let (first, last) = fields[0].split_once('-').unwrap();
            let start = u64::from_str_radix(first, 16).unwrap();
            let end = u64::from_str_radix(last, 16).unwrap();
            (start <= action.restorer && action.restorer.checked_add(16).is_some_and(|p| p <= end))
                .then_some((line, fields, start))
        })
        .expect("complete readable executable restorer range");
    assert!(fields[1].starts_with('r') && fields[1].as_bytes()[2] == b'x');
    let path = fields[5..].join(" ");
    assert!(path.starts_with('/') && !path.ends_with(" (deleted)"));
    let file = std::fs::File::open(&path).unwrap();
    let stat = file.metadata().unwrap();
    assert_eq!(stat.ino(), fields[4].parse::<u64>().unwrap());
    let (major, minor) = fields[3].split_once(':').unwrap();
    assert_eq!(
        libc::major(stat.dev()),
        u32::from_str_radix(major, 16).unwrap()
    );
    assert_eq!(
        libc::minor(stat.dev()),
        u32::from_str_radix(minor, 16).unwrap()
    );
    let offset = u64::from_str_radix(fields[2], 16).unwrap() + action.restorer - start;
    let mut backing = [0_u8; 16];
    file.read_exact_at(&mut backing, offset).unwrap();
    let actual = unsafe { std::slice::from_raw_parts(action.restorer as *const u8, 16) };
    assert_eq!(
        actual, backing,
        "restorer memory must match its actual backing object"
    );
    // Both admitted spellings have only an optional ENDBR64, one immediate
    // load of SYS_rt_sigreturn and SYSCALL. No conditional branch is admitted.
    let code = if actual.starts_with(&[0xf3, 0x0f, 0x1e, 0xfa]) {
        &actual[4..]
    } else {
        actual
    };
    let length = if code.starts_with(&[0x48, 0xc7, 0xc0, 15, 0, 0, 0, 0x0f, 0x05]) {
        9
    } else {
        assert!(
            code.starts_with(&[0xb8, 15, 0, 0, 0, 0x0f, 0x05]),
            "unknown actual restorer: {actual:02x?}"
        );
        7
    };
    println!(
        "rcb restorer: address={:#x} code={:02x?} mapping={mapping}",
        action.restorer,
        &code[..length]
    );
}

unsafe fn raw(number: i64, args: [u64; 6]) -> i64 {
    let result;
    unsafe {
        core::arch::asm!("syscall", inlateout("rax") number => result,
            in("rdi") args[0], in("rsi") args[1], in("rdx") args[2],
            in("r10") args[3], in("r8") args[4], in("r9") args[5],
            lateout("rcx") _, lateout("r11") _, options(nostack));
    }
    result
}
unsafe fn call_site(site: usize) -> i64 {
    let result;
    unsafe {
        core::arch::asm!("call {site}", site = in(reg) site,
            inlateout("rax") libc::SYS_getpid => result,
            clobber_abi("C"));
    }
    result
}

#[derive(Default)]
struct CounterTool;
#[reverie::tool]
impl Tool for CounterTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ();
    fn subscriptions(_: &()) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let callback = CALLBACKS.fetch_add(1, Ordering::AcqRel);
        let before = guest.read_clock()?;
        if WARMUP.load(Ordering::Acquire) {
            assert_eq!(callback, 0);
            let (total, senders) = guest.send_rpc(1).await;
            assert_eq!((total, senders), (1, 1), "actual post-acquisition RPC");
            if !TRANSPORT_MODE.load(Ordering::Acquire) {
                println!("rcb warmup: clock={before} rpc={total} senders={senders}");
            }
        } else if RUNNING.load(Ordering::Acquire) {
            assert!(callback < 2);
            SAMPLES[callback].store(before, Ordering::Release);
        } else {
            assert_eq!(callback, 0);
            let record = ACTIVE_RECORD.load(Ordering::Acquire) as *mut Record;
            assert!(!record.is_null());
            // This real ordinary Tool callback is inside the held fallback
            // pause. Its nested prctl must affect only guest-owned events.
            unsafe { rcb_acquisition_operation(record) };
            let after = guest.read_clock()?;
            SAMPLES[0].store(before, Ordering::Release);
            SAMPLES[1].store(after, Ordering::Release);
        }
        Ok(guest.inject(syscall).await?)
    }
}

pub(super) fn run(path: &Path, installed: bool) {
    let cpu = pin_current_thread();
    assert!(
        DisabledRcbEvent::cpu_supported(),
        "required counter host has unsupported CPU"
    );
    let ordinary = Ordinary::new();
    let site = Site::new(installed);
    let native_pid = unsafe { libc::getpid() };
    assert!(native_pid > 0);
    let mut native = [[0_u64; 4]; 2];
    for (variant, high_bits) in [0, 0x1234_5678_0000_0000].into_iter().enumerate() {
        for (index, selector) in [DISABLE, ENABLE, DISABLE, ENABLE].into_iter().enumerate() {
            let selector = high_bits | selector;
            ordinary.opposite_state(selector);
            let mut record = Record::default();
            record.0[0] = site.address as u64;
            record.0[1] = ordinary.metadata.as_ptr() as u64;
            record.0[2] = selector;
            record.0[3] = rcb_acquisition_no_sample as *const () as u64;
            unsafe { rcb_acquisition_operation(&raw mut record) };
            native[variant][index] =
                ordinary.check(&record, if index % 2 == 0 { 0 } else { BRANCHES });
            println!(
                "rcb native variant={variant} row={index} pid={native_pid} ordinary-id={} raw={:?}",
                ordinary.event_id, record.0
            );
        }
        assert_eq!(native[variant], [0, BRANCHES, 0, BRANCHES]);
    }
    unsafe { reverie_liteinst::with_tool_root!({
        unsafe { reverie_liteinst::install_tool::<CounterTool>(path) }.unwrap();
    }); }
    record_restorer();
    assert_eq!(unsafe { call_site(site.address) }, i64::from(native_pid));
    assert_eq!(CALLBACKS.load(Ordering::Acquire), 1);
    site.check_counts(u64::from(installed), 1);
    record_installed_boundary(&site, native_pid);
    WARMUP.store(false, Ordering::Release);
    for (variant, high_bits) in [0, 0x1234_5678_0000_0000].into_iter().enumerate() {
        let mut private = [0_u64; 4];
        let mut actual = [0_u64; 4];
        for (index, selector) in [DISABLE, ENABLE, DISABLE, ENABLE].into_iter().enumerate() {
            let selector = high_bits | selector;
            let running = index >= 2;
            ordinary.opposite_state(selector);
            CALLBACKS.store(0, Ordering::Release);
            RUNNING.store(running, Ordering::Release);
            let mut record = Record::default();
            record.0[0] = site.address as u64;
            record.0[1] = ordinary.metadata.as_ptr() as u64;
            record.0[2] = selector;
            record.0[3] = if running {
                rcb_acquisition_sample as *const () as u64
            } else {
                rcb_acquisition_no_sample as *const () as u64
            };
            ACTIVE_RECORD.store((&raw mut record) as usize, Ordering::Release);
            if running {
                unsafe { rcb_acquisition_operation(&raw mut record) };
                assert_eq!(record.0[18], native_pid as u64);
                assert_eq!(record.0[19], native_pid as u64);
            } else {
                assert_eq!(unsafe { call_site(site.address) }, i64::from(native_pid));
            }
            ACTIVE_RECORD.store(0, Ordering::Release);
            assert_eq!(
                CALLBACKS.load(Ordering::Acquire),
                if running { 2 } else { 1 }
            );
            let before = SAMPLES[0].load(Ordering::Acquire);
            let after = SAMPLES[1].load(Ordering::Acquire);
            private[index] = after.checked_sub(before).unwrap();
            assert_eq!(private[index], if running { BRANCHES } else { 0 });
            actual[index] = ordinary.check(&record, native[variant][index]);
            println!(
                "rcb mediated variant={variant} row={index} pid={native_pid} ordinary-id={} private-before={before} private-after={after} raw={:?}",
                ordinary.event_id, record.0
            );
        }
        assert_eq!(private, [0, 0, BRANCHES, BRANCHES]);
        assert_eq!(actual, [0, BRANCHES, 0, BRANCHES]);
        println!("rcb vector variant={variant} private={private:?} ordinary={actual:?}");
    }
    // Warmup: one original entry. Paused rows add four original getpid
    // entries and four nested prctl entries. Running rows add twelve entries.
    // The fallback's four nested prctl signals bypass its per-site trap count;
    // installed hooks count those four real nested calls too.
    let hooks = if installed { 21 } else { 0 };
    let traps = if installed { 1 } else { 17 };
    site.check_counts(hooks, traps);
    let kind = if installed { "installed" } else { "fallback" };
    println!(
        "rcb matrix: path={kind} native=8 mediated=8 actual-pid={native_pid} ordinary-id={} hooks={hooks} traps={traps} callback-rpc=1 cpu={cpu}",
        ordinary.event_id
    );
}

/// Qualification-only corrupt-supervisor control. The complete Result and all
/// formatting/drop work stay inside the assembly root. A correctly refused
/// profile leaves setup incomplete, so the root terminates with status 126.
pub(super) fn run_profile_refusal(path: &Path) -> ! {
    unsafe { reverie_liteinst::with_tool_root!({
            let fault = std::env::var_os("REVERIE_LITEINST_TEST_RCB_PROFILE_FAULT");
            let expected = match fault.as_deref() {
                Some(value) if value == std::ffi::OsStr::new("unsupported") => {
                    "supervisor falsely reported unsupported native PMU"
                }
                Some(value) if value == std::ffi::OsStr::new("wrong-event") => {
                    "supervisor EVENT differs from the captured native PMU profile"
                }
                _ => "invalid profile-fault control",
            };
            let result = unsafe { reverie_liteinst::install_tool::<CounterTool>(path) };
            let marker: &[u8] = match result {
                Err(error) if error.to_string() == expected => b"root-profile-refusal=exact\n",
                _ => b"root-profile-refusal=wrong\n",
            };
            let _ = unsafe {
                raw(
                    libc::SYS_write,
                    [
                        libc::STDOUT_FILENO as u64,
                        marker.as_ptr() as u64,
                        marker.len() as u64,
                        0,
                        0,
                        0,
                    ],
                )
            };
    }); }
    unsafe { libc::_exit(119) }
}

#[derive(Clone, Copy)]
pub(super) enum PrivateTransport {
    ScmRights,
    PidfdGetfd,
    IoUring,
}

#[repr(C, align(8))]
struct Ancillary([u8; 24]);

const _: () = assert!(size_of::<libc::cmsghdr>() == 16);
const _: () = assert!(size_of::<Ancillary>() == 24);

fn socket_pair() -> [i32; 2] {
    socket_pair_with(raw)
}

fn socket_pair_with(gate: RingGate) -> [i32; 2] {
    let mut pair = [-1_i32; 2];
    assert_eq!(
        unsafe {
            gate(
                libc::SYS_socketpair,
                [
                    libc::AF_UNIX as u64,
                    (libc::SOCK_DGRAM | libc::SOCK_CLOEXEC) as u64,
                    0,
                    (&raw mut pair) as u64,
                    0,
                    0,
                ],
            )
        },
        0
    );
    assert!(pair.into_iter().all(|fd| fd >= 0));
    pair
}

fn pipe_pair() -> [i32; 2] {
    let mut pair = [-1_i32; 2];
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_pipe2,
                [(&raw mut pair) as u64, libc::O_CLOEXEC as u64, 0, 0, 0, 0],
            )
        },
        0
    );
    assert!(pair.into_iter().all(|fd| fd >= 0));
    pair
}

unsafe fn send_descriptor(number: i64, socket: i32, descriptor: i32) -> i64 {
    unsafe { send_descriptor_with(raw, number, socket, descriptor) }
}

unsafe fn send_descriptor_with(
    gate: RingGate,
    number: i64,
    socket: i32,
    descriptor: i32,
) -> i64 {
    let mut byte = 0x5a_u8;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut ancillary = Ancillary([0; 24]);
    let header = ancillary.0.as_mut_ptr().cast::<libc::cmsghdr>();
    unsafe {
        (*header).cmsg_len = size_of::<libc::cmsghdr>() + size_of::<i32>();
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        ancillary
            .0
            .as_mut_ptr()
            .add(size_of::<libc::cmsghdr>())
            .cast::<i32>()
            .write_unaligned(descriptor);
    }
    let mut message = libc::msghdr {
        msg_name: core::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &raw mut vector,
        msg_iovlen: 1,
        msg_control: ancillary.0.as_mut_ptr().cast(),
        msg_controllen: ancillary.0.len(),
        msg_flags: 0,
    };
    if number == libc::SYS_sendmsg {
        unsafe { gate(number, [socket as u64, (&raw mut message) as u64, 0, 0, 0, 0]) }
    } else {
        assert_eq!(number, libc::SYS_sendmmsg);
        let mut packet = libc::mmsghdr {
            msg_hdr: message,
            msg_len: 0,
        };
        unsafe { gate(number, [socket as u64, (&raw mut packet) as u64, 1, 0, 0, 0]) }
    }
}

unsafe fn receive_descriptor(number: i64, socket: i32) -> (i64, Option<i32>) {
    unsafe { receive_descriptor_with(raw, number, socket) }
}

unsafe fn receive_descriptor_with(
    gate: RingGate,
    number: i64,
    socket: i32,
) -> (i64, Option<i32>) {
    let mut byte = 0_u8;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut ancillary = Ancillary([0; 24]);
    let mut message = libc::msghdr {
        msg_name: core::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &raw mut vector,
        msg_iovlen: 1,
        msg_control: ancillary.0.as_mut_ptr().cast(),
        msg_controllen: ancillary.0.len(),
        msg_flags: 0,
    };
    let result = if number == libc::SYS_recvmsg {
        unsafe { gate(number, [socket as u64, (&raw mut message) as u64, 0, 0, 0, 0]) }
    } else {
        assert_eq!(number, libc::SYS_recvmmsg);
        let mut packet = libc::mmsghdr {
            msg_hdr: message,
            msg_len: 0,
        };
        unsafe { gate(number, [socket as u64, (&raw mut packet) as u64, 1, 0, 0, 0]) }
    };
    if result <= 0 {
        return (result, None);
    }
    let header = ancillary.0.as_ptr().cast::<libc::cmsghdr>();
    unsafe {
        assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        assert_eq!(
            (*header).cmsg_len,
            size_of::<libc::cmsghdr>() + size_of::<i32>()
        );
        (
            result,
            Some(
                ancillary
                    .0
                    .as_ptr()
                    .add(size_of::<libc::cmsghdr>())
                    .cast::<i32>()
                    .read_unaligned(),
            ),
        )
    }
}

fn descriptor_identity(fd: i32) -> (libc::dev_t, libc::ino_t) {
    let mut status: libc::stat = unsafe { core::mem::zeroed() };
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_fstat,
                [fd as u64, (&raw mut status) as u64, 0, 0, 0, 0],
            )
        },
        0
    );
    (status.st_dev, status.st_ino)
}

fn close_descriptor(fd: i32) {
    assert_eq!(unsafe { raw(libc::SYS_close, [fd as u64, 0, 0, 0, 0, 0]) }, 0);
}

fn private_snapshot() -> [u64; 4] {
    let snapshot = reverie_liteinst::private_rcb_snapshot_for_test().unwrap();
    assert!(snapshot[0] <= i32::MAX as u64, "private descriptor number");
    assert_ne!(snapshot[1], 0, "authenticated supervisor event ID");
    assert_eq!(
        snapshot[2] as i64,
        unsafe { raw(libc::SYS_gettid, [0; 6]) },
        "private event owner"
    );
    snapshot
}

fn prepare_transport_clock(site: &Site) {
    TRANSPORT_MODE.store(true, Ordering::Release);
    CALLBACKS.store(0, Ordering::Release);
    assert!(unsafe { call_site(site.address) } > 0);
    assert_eq!(CALLBACKS.load(Ordering::Acquire), 1, "clock warmup callback");
    WARMUP.store(false, Ordering::Release);
}

fn assert_transport_clock_after_refusal(site: &Site, before: [u64; 4]) -> [u64; 4] {
    CALLBACKS.store(0, Ordering::Release);
    SAMPLES[0].store(0, Ordering::Release);
    SAMPLES[1].store(0, Ordering::Release);
    RUNNING.store(true, Ordering::Release);
    unsafe { rcb_transport_clock_probe(site.address) };
    RUNNING.store(false, Ordering::Release);
    assert_eq!(CALLBACKS.load(Ordering::Acquire), 2, "two exact clock samples");
    let first = SAMPLES[0].load(Ordering::Acquire);
    let last = SAMPLES[1].load(Ordering::Acquire);
    assert_eq!(last.checked_sub(first), Some(BRANCHES));
    let after = private_snapshot();
    assert_eq!(&after[..3], &before[..3], "private descriptor/event/owner changed");
    assert!(first >= before[3]);
    assert!(after[3] >= last);
    after
}

fn scm_rights_native_control(sockets: [i32; 2], target: i32) {
    assert_eq!(
        unsafe { send_descriptor(libc::SYS_sendmsg, sockets[0], target) },
        1
    );
    let (received, alias) = unsafe { receive_descriptor(libc::SYS_recvmsg, sockets[1]) };
    assert_eq!(received, 1);
    let alias = alias.expect("SCM_RIGHTS native alias");
    assert_ne!(alias, target);
    assert_eq!(descriptor_identity(alias), descriptor_identity(target));
    close_descriptor(alias);
}

fn pidfd_native_control(target: i32) {
    let pid = unsafe { raw(libc::SYS_getpid, [0; 6]) };
    assert!(pid > 0);
    let pidfd = unsafe { raw(libc::SYS_pidfd_open, [pid as u64, 0, 0, 0, 0, 0]) };
    assert!(pidfd >= 0, "pidfd_open native control: {pidfd}");
    let alias = unsafe {
        raw(
            libc::SYS_pidfd_getfd,
            [pidfd as u64, target as u64, 0, 0, 0, 0],
        )
    };
    assert!(alias >= 0, "pidfd_getfd native control: {alias}");
    assert_eq!(descriptor_identity(alias as i32), descriptor_identity(target));
    close_descriptor(alias as i32);
    close_descriptor(pidfd as i32);
}

const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
const SECCOMP_SET_MODE_FILTER: u64 = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: u64 = 1 << 3;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_ADDFD_FLAG_SETFD: u32 = 1;
const SECCOMP_IOCTL_NOTIF_RECV: u64 = 0xc050_2100;
const SECCOMP_IOCTL_NOTIF_SEND: u64 = 0xc018_2101;
const SECCOMP_IOCTL_NOTIF_ADDFD: u64 = 0x4018_2103;
const NOTIFICATION_ALIAS_FD: u32 = 198;
const NOTIFICATION_NATIVE_RESULT: i64 = 0x5243_42;

#[derive(Default)]
#[repr(C)]
struct SeccompData {
    number: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

#[derive(Default)]
#[repr(C)]
struct SeccompNotif {
    id: u64,
    pid: u32,
    flags: u32,
    data: SeccompData,
}

#[derive(Default)]
#[repr(C)]
struct SeccompNotifResponse {
    id: u64,
    value: i64,
    error: i32,
    flags: u32,
}

#[derive(Default)]
#[repr(C)]
struct SeccompNotifAddfd {
    id: u64,
    flags: u32,
    source_fd: u32,
    new_fd: u32,
    new_fd_flags: u32,
}

const _: () = assert!(size_of::<SeccompData>() == 64);
const _: () = assert!(size_of::<SeccompNotif>() == 80);
const _: () = assert!(size_of::<SeccompNotifResponse>() == 24);
const _: () = assert!(size_of::<SeccompNotifAddfd>() == 24);

const fn bpf_statement(code: u16, value: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k: value,
    }
}

const fn bpf_jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: yes,
        jf: no,
        k: value,
    }
}

fn install_getppid_user_notification() -> i32 {
    let mut instructions = [
        bpf_statement(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
        bpf_jump(BPF_JMP_JEQ_K, AUDIT_ARCH_X86_64, 1, 0),
        bpf_statement(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        bpf_statement(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
        bpf_jump(BPF_JMP_JEQ_K, libc::SYS_getppid as u32, 0, 1),
        bpf_statement(BPF_RET_K, SECCOMP_RET_USER_NOTIF),
        bpf_statement(BPF_RET_K, SECCOMP_RET_ALLOW),
    ];
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_prctl,
                [libc::PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0],
            )
        },
        0
    );
    let program = libc::sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_mut_ptr(),
    };
    let listener = unsafe {
        raw(
            libc::SYS_seccomp,
            [
                SECCOMP_SET_MODE_FILTER,
                SECCOMP_FILTER_FLAG_NEW_LISTENER,
                (&raw const program) as u64,
                0,
                0,
                0,
            ],
        )
    };
    assert!(listener >= 0, "seccomp user-notification listener: {listener}");
    listener as i32
}

fn receive_notification(listener: i32, expected_child: i32) -> SeccompNotif {
    let mut notification = SeccompNotif::default();
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_ioctl,
                [
                    listener as u64,
                    SECCOMP_IOCTL_NOTIF_RECV,
                    (&raw mut notification) as u64,
                    0,
                    0,
                    0,
                ],
            )
        },
        0,
        "receive real seccomp notification"
    );
    assert_ne!(notification.id, 0);
    assert_eq!(notification.pid, expected_child as u32);
    assert_eq!(notification.flags, 0);
    assert_eq!(notification.data.number, libc::SYS_getppid as i32);
    assert_eq!(notification.data.arch, AUDIT_ARCH_X86_64);
    notification
}

fn send_notification_response(listener: i32, response: &mut SeccompNotifResponse) {
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_ioctl,
                [
                    listener as u64,
                    SECCOMP_IOCTL_NOTIF_SEND,
                    (response as *mut SeccompNotifResponse) as u64,
                    0,
                    0,
                    0,
                ],
            )
        },
        0,
        "send real seccomp notification response"
    );
}

struct SeccompAddfdControl {
    listener: i32,
    child: i32,
    ordinary: i32,
}

impl SeccompAddfdControl {
    fn start(ordinary: i32) -> Self {
        let sockets = socket_pair();
        let ordinary_identity = descriptor_identity(ordinary);
        let child = unsafe { raw(libc::SYS_fork, [0; 6]) };
        assert!(child >= 0, "seccomp-notification child: {child}");
        if child == 0 {
            close_descriptor(sockets[0]);
            close_descriptor(ordinary);
            let _ = unsafe {
                raw(
                    libc::SYS_close,
                    [NOTIFICATION_ALIAS_FD as u64, 0, 0, 0, 0, 0],
                )
            };
            let listener = install_getppid_user_notification();
            assert_eq!(
                unsafe { send_descriptor(libc::SYS_sendmsg, sockets[1], listener) },
                1
            );
            close_descriptor(listener);
            close_descriptor(sockets[1]);
            assert_eq!(
                unsafe { raw(libc::SYS_getppid, [0; 6]) },
                NOTIFICATION_NATIVE_RESULT
            );
            assert_eq!(
                descriptor_identity(NOTIFICATION_ALIAS_FD as i32),
                ordinary_identity,
                "preactivation addfd installed the exact ordinary file"
            );
            close_descriptor(NOTIFICATION_ALIAS_FD as i32);
            assert_eq!(
                unsafe { raw(libc::SYS_getppid, [0; 6]) },
                -i64::from(libc::EPERM)
            );
            assert_eq!(
                unsafe {
                    raw(
                        libc::SYS_fcntl,
                        [
                            NOTIFICATION_ALIAS_FD as u64,
                            libc::F_GETFD as u64,
                            0,
                            0,
                            0,
                            0,
                        ],
                    )
                },
                -i64::from(libc::EBADF),
                "postactivation refusal created no alias"
            );
            assert_eq!(
                unsafe { raw(libc::SYS_getppid, [0; 6]) },
                NOTIFICATION_NATIVE_RESULT + 1
            );
            assert_eq!(
                descriptor_identity(NOTIFICATION_ALIAS_FD as i32),
                ordinary_identity,
                "trusted postactivation addfd installed the ordinary file"
            );
            close_descriptor(NOTIFICATION_ALIAS_FD as i32);
            unsafe { raw(libc::SYS_exit_group, [0; 6]) };
            unreachable!();
        }
        close_descriptor(sockets[1]);
        let (received, listener) = unsafe { receive_descriptor(libc::SYS_recvmsg, sockets[0]) };
        assert_eq!(received, 1);
        let listener = listener.expect("real seccomp listener transfer");
        close_descriptor(sockets[0]);
        let notification = receive_notification(listener, child as i32);
        let mut addfd = SeccompNotifAddfd {
            id: notification.id,
            flags: SECCOMP_ADDFD_FLAG_SETFD,
            source_fd: ordinary as u32,
            new_fd: NOTIFICATION_ALIAS_FD,
            new_fd_flags: libc::O_CLOEXEC as u32,
        };
        assert_eq!(
            unsafe {
                raw(
                    libc::SYS_ioctl,
                    [
                        listener as u64,
                        SECCOMP_IOCTL_NOTIF_ADDFD,
                        (&raw mut addfd) as u64,
                        0,
                        0,
                        0,
                    ],
                )
            },
            i64::from(NOTIFICATION_ALIAS_FD),
            "preactivation ordinary-FD addfd"
        );
        send_notification_response(
            listener,
            &mut SeccompNotifResponse {
                id: notification.id,
                value: NOTIFICATION_NATIVE_RESULT,
                error: 0,
                flags: 0,
            },
        );
        Self {
            listener,
            child: child as i32,
            ordinary,
        }
    }

    fn refuse_private_addfd(self, snapshot: [u64; 4]) {
        let private_identity = descriptor_identity(snapshot[0] as i32);
        let notification = receive_notification(self.listener, self.child);
        let mut addfd = SeccompNotifAddfd {
            id: notification.id,
            flags: SECCOMP_ADDFD_FLAG_SETFD,
            source_fd: snapshot[0] as u32,
            new_fd: NOTIFICATION_ALIAS_FD,
            new_fd_flags: libc::O_CLOEXEC as u32,
        };
        assert_eq!(
            unsafe {
                raw(
                    libc::SYS_ioctl,
                    [
                        self.listener as u64,
                        SECCOMP_IOCTL_NOTIF_ADDFD,
                        (&raw mut addfd) as u64,
                        0,
                        0,
                        0,
                    ],
                )
            },
            -i64::from(libc::ENOTSUP),
            "private seccomp addfd refused before alias"
        );
        assert_eq!(
            descriptor_identity(snapshot[0] as i32),
            private_identity,
            "private file identity changed across refused addfd"
        );
        send_notification_response(
            self.listener,
            &mut SeccompNotifResponse {
                id: notification.id,
                value: 0,
                error: -libc::EPERM,
                flags: 0,
            },
        );
        let trusted_notification = receive_notification(self.listener, self.child);
        let mut trusted_addfd = SeccompNotifAddfd {
            id: trusted_notification.id,
            flags: SECCOMP_ADDFD_FLAG_SETFD,
            source_fd: self.ordinary as u32,
            new_fd: NOTIFICATION_ALIAS_FD,
            new_fd_flags: libc::O_CLOEXEC as u32,
        };
        assert_eq!(
            unsafe {
                trusted_raw(
                    libc::SYS_ioctl,
                    [
                        self.listener as u64,
                        SECCOMP_IOCTL_NOTIF_ADDFD,
                        (&raw mut trusted_addfd) as u64,
                        0,
                        0,
                        0,
                    ],
                )
            },
            i64::from(NOTIFICATION_ALIAS_FD),
            "trusted raw addfd remains functional after activation"
        );
        send_notification_response(
            self.listener,
            &mut SeccompNotifResponse {
                id: trusted_notification.id,
                value: NOTIFICATION_NATIVE_RESULT + 1,
                error: 0,
                flags: 0,
            },
        );
        let mut status = -1_i32;
        assert_eq!(
            unsafe {
                raw(
                    libc::SYS_wait4,
                    [
                        self.child as u64,
                        (&raw mut status) as u64,
                        0,
                        0,
                        0,
                        0,
                    ],
                )
            },
            i64::from(self.child)
        );
        assert_eq!(status, 0, "seccomp addfd child status");
        close_descriptor(self.listener);
    }
}

#[derive(Default)]
#[repr(C)]
struct IoSqringOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    flags: u32,
    dropped: u32,
    array: u32,
    reserved: u32,
    user_addr: u64,
}

#[derive(Default)]
#[repr(C)]
struct IoCqringOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    overflow: u32,
    cqes: u32,
    flags: u32,
    reserved: u32,
    user_addr: u64,
}

#[derive(Default)]
#[repr(C)]
struct IoUringParams {
    sq_entries: u32,
    cq_entries: u32,
    flags: u32,
    sq_thread_cpu: u32,
    sq_thread_idle: u32,
    features: u32,
    wq_fd: u32,
    reserved: [u32; 3],
    sq_offsets: IoSqringOffsets,
    cq_offsets: IoCqringOffsets,
}

const _: () = assert!(size_of::<IoUringParams>() == 120);
const _: () = assert!(core::mem::align_of::<IoUringParams>() == 8);

#[derive(Default)]
#[repr(C)]
struct IoUringProbeOp {
    op: u8,
    reserved: u8,
    flags: u16,
    reserved2: u32,
}

#[derive(Default)]
#[repr(C)]
struct IoUringProbe {
    last_op: u8,
    ops_len: u8,
    reserved: u16,
    reserved2: [u32; 3],
    op: IoUringProbeOp,
}

#[repr(C)]
struct IoUringCqe {
    user_data: u64,
    result: i32,
    flags: u32,
}

const _: () = assert!(size_of::<IoUringProbe>() == 24);
const _: () = assert!(size_of::<IoUringCqe>() == 16);

type RingGate = unsafe fn(i64, [u64; 6]) -> i64;

unsafe fn trusted_raw(number: i64, args: [u64; 6]) -> i64 {
    unsafe { reverie_preload::trap::raw_syscall6(number, args) }
}

fn ring_result(value: i64, context: &str) -> i64 {
    assert!(value >= 0, "{context}: {value}");
    value
}

fn map_ring(gate: RingGate, bytes: usize, fd: i32, offset: u64) -> *mut u8 {
    let result = unsafe {
        gate(
            libc::SYS_mmap,
            [
                0,
                bytes as u64,
                (libc::PROT_READ | libc::PROT_WRITE) as u64,
                (libc::MAP_SHARED | libc::MAP_POPULATE) as u64,
                fd as u64,
                offset,
            ],
        )
    };
    assert!(result > 0, "io_uring mmap offset={offset:#x}: {result}");
    result as *mut u8
}

unsafe fn field<T>(base: *mut u8, offset: u32) -> *mut T {
    unsafe { base.add(offset as usize).cast() }
}

struct NumericRing {
    fd: i32,
    sq: *mut u8,
    cq: *mut u8,
    sqes: *mut u8,
    sq_bytes: usize,
    cq_bytes: usize,
    sqe_bytes: usize,
    single_mapping: bool,
    parameters: IoUringParams,
}

impl NumericRing {
    fn new(gate: RingGate) -> Self {
        let mut parameters = IoUringParams::default();
        let fd = ring_result(
            unsafe {
                gate(
                    libc::SYS_io_uring_setup,
                    [2, (&raw mut parameters) as u64, 0, 0, 0, 0],
                )
            },
            "io_uring_setup native control",
        ) as i32;
        let sq_bytes = parameters.sq_offsets.array as usize
            + parameters.sq_entries as usize * size_of::<u32>();
        let cq_bytes = parameters.cq_offsets.cqes as usize
            + parameters.cq_entries as usize * size_of::<IoUringCqe>();
        let single_mapping = parameters.features & IORING_FEAT_SINGLE_MMAP != 0;
        let sq = map_ring(
            gate,
            if single_mapping {
                sq_bytes.max(cq_bytes)
            } else {
                sq_bytes
            },
            fd,
            IORING_OFF_SQ_RING,
        );
        let cq = if single_mapping {
            sq
        } else {
            map_ring(gate, cq_bytes, fd, IORING_OFF_CQ_RING)
        };
        let sqe_bytes = parameters.sq_entries as usize * 64;
        let sqes = map_ring(gate, sqe_bytes, fd, IORING_OFF_SQES);
        Self {
            fd,
            sq,
            cq,
            sqes,
            sq_bytes: if single_mapping {
                sq_bytes.max(cq_bytes)
            } else {
                sq_bytes
            },
            cq_bytes,
            sqe_bytes,
            single_mapping,
            parameters,
        }
    }

    fn close_operation(&mut self, gate: RingGate, target: i32) {
        unsafe {
            let head = field::<u32>(self.sq, self.parameters.sq_offsets.head).read_volatile();
            let tail = field::<u32>(self.sq, self.parameters.sq_offsets.tail).read_volatile();
            let mask = field::<u32>(self.sq, self.parameters.sq_offsets.ring_mask).read_volatile();
            let entries =
                field::<u32>(self.sq, self.parameters.sq_offsets.ring_entries).read_volatile();
            assert!(tail.wrapping_sub(head) < entries);
            let index = tail & mask;
            let sqe = self.sqes.add(index as usize * 64);
            core::ptr::write_bytes(sqe, 0, 64);
            sqe.write(IORING_OP_CLOSE);
            sqe.add(4).cast::<i32>().write_unaligned(target);
            sqe.add(32).cast::<u64>().write_unaligned(RING_USER_DATA);
            field::<u32>(self.sq, self.parameters.sq_offsets.array)
                .add(index as usize)
                .write_volatile(index);
            core::sync::atomic::fence(Ordering::Release);
            field::<u32>(self.sq, self.parameters.sq_offsets.tail)
                .write_volatile(tail.wrapping_add(1));
            core::sync::atomic::fence(Ordering::SeqCst);
            ring_result(
                gate(
                    libc::SYS_io_uring_enter,
                    [self.fd as u64, 1, 1, IORING_ENTER_GETEVENTS as u64, 0, 0],
                ),
                "io_uring_enter close",
            );
            let cq_head =
                field::<u32>(self.cq, self.parameters.cq_offsets.head).read_volatile();
            let cq_tail =
                field::<u32>(self.cq, self.parameters.cq_offsets.tail).read_volatile();
            assert_eq!(cq_tail.wrapping_sub(cq_head), 1, "one close completion");
            let cq_mask =
                field::<u32>(self.cq, self.parameters.cq_offsets.ring_mask).read_volatile();
            let cqe = field::<IoUringCqe>(self.cq, self.parameters.cq_offsets.cqes)
                .add((cq_head & cq_mask) as usize)
                .read();
            assert_eq!(cqe.user_data, RING_USER_DATA);
            assert_eq!(cqe.result, 0, "IORING_OP_CLOSE result");
            assert_eq!(cqe.flags, 0);
            field::<u32>(self.cq, self.parameters.cq_offsets.head)
                .write_volatile(cq_head.wrapping_add(1));
        }
    }

    fn destroy(self, gate: RingGate) {
        let unmap = |address: *mut u8, bytes: usize| {
            assert_eq!(
                unsafe { gate(libc::SYS_munmap, [address as u64, bytes as u64, 0, 0, 0, 0]) },
                0
            );
        };
        unmap(self.sqes, self.sqe_bytes);
        if !self.single_mapping {
            unmap(self.cq, self.cq_bytes);
        }
        unmap(self.sq, self.sq_bytes);
        assert_eq!(unsafe { gate(libc::SYS_close, [self.fd as u64, 0, 0, 0, 0, 0]) }, 0);
    }
}

struct RegisteredSqpollRing {
    index: u32,
    memory: *mut u8,
    bytes: usize,
    sqes: *mut u8,
    rings: *mut u8,
    parameters: IoUringParams,
}

impl RegisteredSqpollRing {
    fn new() -> Self {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page >= 4096 && (page & (page - 1)) == 0);
        let page = page as usize;
        let bytes = page.checked_mul(2).unwrap();
        let memory = unsafe {
            raw(
                libc::SYS_mmap,
                [
                    0,
                    bytes as u64,
                    (libc::PROT_READ | libc::PROT_WRITE) as u64,
                    (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS) as u64,
                    u64::MAX,
                    0,
                ],
            )
        };
        assert!(memory > 0, "supplied io_uring memory: {memory}");
        let memory = memory as *mut u8;
        let rings = unsafe { memory.add(page) };
        let mut parameters = IoUringParams::default();
        parameters.flags = IORING_SETUP_SQPOLL
            | IORING_SETUP_NO_MMAP
            | IORING_SETUP_REGISTERED_FD_ONLY
            | IORING_SETUP_NO_SQARRAY;
        parameters.sq_thread_idle = 1000;
        parameters.sq_offsets.user_addr = memory as u64;
        parameters.cq_offsets.user_addr = rings as u64;
        let index = unsafe {
            raw(
                libc::SYS_io_uring_setup,
                [2, (&raw mut parameters) as u64, 0, 0, 0, 0],
            )
        };
        assert!(
            (0..i64::from(IO_RINGFD_REG_MAX)).contains(&index),
            "supplied-memory registered-only SQPOLL setup: {index}"
        );
        assert_eq!(parameters.sq_entries, 2);
        assert!((1..=64).contains(&parameters.cq_entries));
        assert_ne!(
            parameters.features & IORING_FEAT_SQPOLL_NONFIXED,
            0,
            "SQPOLL must consume the ordinary close descriptor"
        );
        for offset in [
            parameters.sq_offsets.head,
            parameters.sq_offsets.tail,
            parameters.sq_offsets.ring_mask,
            parameters.sq_offsets.ring_entries,
            parameters.sq_offsets.flags,
        ] {
            assert!(offset as usize + size_of::<u32>() <= page);
        }
        for offset in [
            parameters.cq_offsets.head,
            parameters.cq_offsets.tail,
            parameters.cq_offsets.ring_mask,
            parameters.cq_offsets.ring_entries,
        ] {
            assert!(offset as usize + size_of::<u32>() <= page);
        }
        assert!(
            parameters.cq_offsets.cqes as usize
                + parameters.cq_entries as usize * size_of::<IoUringCqe>()
                <= page
        );
        Self {
            index: index as u32,
            memory,
            bytes,
            sqes: memory,
            rings,
            parameters,
        }
    }

    fn close_operation(&mut self, target: i32) {
        unsafe {
            let head = field::<u32>(self.rings, self.parameters.sq_offsets.head).read_volatile();
            let tail = field::<u32>(self.rings, self.parameters.sq_offsets.tail).read_volatile();
            let mask =
                field::<u32>(self.rings, self.parameters.sq_offsets.ring_mask).read_volatile();
            let entries =
                field::<u32>(self.rings, self.parameters.sq_offsets.ring_entries).read_volatile();
            assert!(tail.wrapping_sub(head) < entries);
            let sqe = self.sqes.add((tail & mask) as usize * 64);
            core::ptr::write_bytes(sqe, 0, 64);
            sqe.write(IORING_OP_CLOSE);
            sqe.add(4).cast::<i32>().write_unaligned(target);
            sqe.add(32).cast::<u64>().write_unaligned(RING_USER_DATA);
            core::sync::atomic::fence(Ordering::Release);
            field::<u32>(self.rings, self.parameters.sq_offsets.tail)
                .write_volatile(tail.wrapping_add(1));
            core::sync::atomic::fence(Ordering::SeqCst);
            let sq_flags =
                field::<u32>(self.rings, self.parameters.sq_offsets.flags).read_volatile();
            if sq_flags & IORING_SQ_NEED_WAKEUP != 0 {
                ring_result(
                    raw(
                        libc::SYS_io_uring_enter,
                        [
                            self.index as u64,
                            0,
                            0,
                            (IORING_ENTER_SQ_WAKEUP | IORING_ENTER_REGISTERED_RING) as u64,
                            0,
                            0,
                        ],
                    ),
                    "registered SQPOLL wakeup",
                );
            }
            ring_result(
                raw(
                    libc::SYS_io_uring_enter,
                    [
                        self.index as u64,
                        0,
                        1,
                        (IORING_ENTER_GETEVENTS | IORING_ENTER_REGISTERED_RING) as u64,
                        0,
                        0,
                    ],
                ),
                "registered SQPOLL close completion",
            );
            let cq_head =
                field::<u32>(self.rings, self.parameters.cq_offsets.head).read_volatile();
            let cq_tail =
                field::<u32>(self.rings, self.parameters.cq_offsets.tail).read_volatile();
            assert_eq!(cq_tail.wrapping_sub(cq_head), 1);
            let cq_mask =
                field::<u32>(self.rings, self.parameters.cq_offsets.ring_mask).read_volatile();
            let cqe = field::<IoUringCqe>(self.rings, self.parameters.cq_offsets.cqes)
                .add((cq_head & cq_mask) as usize)
                .read();
            assert_eq!(cqe.user_data, RING_USER_DATA);
            assert_eq!(cqe.result, 0, "registered SQPOLL IORING_OP_CLOSE");
            assert_eq!(cqe.flags, 0);
            field::<u32>(self.rings, self.parameters.cq_offsets.head)
                .write_volatile(cq_head.wrapping_add(1));
        }
    }
}

fn io_uring_setup(flags: u32) -> i64 {
    let mut parameters: IoUringParams = unsafe { core::mem::zeroed() };
    parameters.flags = flags;
    parameters.sq_thread_idle = 1000;
    unsafe {
        raw(
            libc::SYS_io_uring_setup,
            [2, (&raw mut parameters) as u64, 0, 0, 0, 0],
        )
    }
}

fn io_uring_native_control() {
    let pipe = pipe_pair();
    let mut ring = NumericRing::new(raw);
    ring.close_operation(raw, pipe[0]);
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_fcntl,
                [pipe[0] as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
            )
        },
        -i64::from(libc::EBADF)
    );
    close_descriptor(pipe[1]);
    ring.destroy(raw);
}

pub(super) fn run_private_transport(path: &Path, transport: PrivateTransport) {
    let sockets = socket_pair();
    let pipes = pipe_pair();
    let site = Site::new(true);
    let addfd = match transport {
        PrivateTransport::ScmRights => Some(SeccompAddfdControl::start(pipes[0])),
        PrivateTransport::PidfdGetfd | PrivateTransport::IoUring => None,
    };
    match transport {
        PrivateTransport::ScmRights => scm_rights_native_control(sockets, pipes[0]),
        PrivateTransport::PidfdGetfd => pidfd_native_control(pipes[0]),
        PrivateTransport::IoUring => io_uring_native_control(),
    }
    unsafe { reverie_liteinst::with_tool_root!({
        unsafe { reverie_liteinst::install_tool::<CounterTool>(path) }.unwrap();
    }); }
    let before = private_snapshot();
    let private = before[0] as i32;
    prepare_transport_clock(&site);
    let refusal_before = private_snapshot();
    assert_eq!(
        &refusal_before[..3],
        &before[..3],
        "private event identity changed before the guarded attempt"
    );
    let unsupported = -i64::from(libc::ENOTSUP);
    let name = match transport {
        PrivateTransport::ScmRights => {
            for number in [libc::SYS_sendmsg, libc::SYS_sendmmsg] {
                assert_eq!(unsafe { send_descriptor(number, sockets[0], private) }, unsupported);
                assert_eq!(unsafe { send_descriptor(number, sockets[0], pipes[0]) }, unsupported);
            }
            for number in [libc::SYS_recvmsg, libc::SYS_recvmmsg] {
                assert_eq!(unsafe { receive_descriptor(number, sockets[1]) }.0, unsupported);
            }
            reverie_liteinst::arm_prepared_fork_probe_for_test(sockets[0], sockets[1]).unwrap();
            let child = unsafe { libc::fork() };
            assert!(child >= 0);
            if child == 0 {
                let child_snapshot = private_snapshot();
                assert_ne!(child_snapshot[1], before[1], "child owns a distinct event");
                let child_private = child_snapshot[0] as i32;
                let result = unsafe {
                    send_descriptor(libc::SYS_sendmsg, sockets[0], child_private)
                };
                super::emit_hardware_counter_result("rcb-private-scm-rights-child");
                unsafe { libc::_exit(if result == unsupported { 0 } else { 1 }) };
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert!(libc::WIFEXITED(status));
            assert_eq!(libc::WEXITSTATUS(status), 0);
            let prepared = reverie_liteinst::prepared_fork_probe_for_test().unwrap();
            assert_eq!(prepared[0], unsupported);
            assert!(prepared[1] > 1, "distinct child generation");
            addfd.unwrap().refuse_private_addfd(before);
            assert_eq!(prepared[1], 2, "first child generation");
            "scm-rights prepared-generation=2"
        }
        PrivateTransport::PidfdGetfd => {
            let pid = unsafe { raw(libc::SYS_getpid, [0; 6]) };
            let pidfd = unsafe { raw(libc::SYS_pidfd_open, [pid as u64, 0, 0, 0, 0, 0]) };
            assert!(pidfd >= 0);
            for target in [private, pipes[0]] {
                assert_eq!(
                    unsafe {
                        raw(
                            libc::SYS_pidfd_getfd,
                            [pidfd as u64, target as u64, 0, 0, 0, 0],
                        )
                    },
                    unsupported
                );
            }
            close_descriptor(pidfd as i32);
            "pidfd_getfd"
        }
        PrivateTransport::IoUring => {
            assert_eq!(io_uring_setup(0), unsupported);
            for target in [private, pipes[0]] {
                assert_eq!(
                    unsafe {
                        raw(
                            libc::SYS_io_uring_register,
                            [target as u64, 0, 0, 0, 0, 0],
                        )
                    },
                    unsupported
                );
                assert_eq!(
                    unsafe {
                        raw(
                            libc::SYS_io_uring_enter,
                            [target as u64, 0, 0, 0, 0, 0],
                        )
                    },
                    unsupported
                );
            }
            "io_uring"
        }
    };
    let refusal_after = private_snapshot();
    assert_eq!(
        &refusal_after[..3],
        &refusal_before[..3],
        "guarded attempt changed private descriptor, event ID, or owner"
    );
    assert!(
        refusal_after[3] >= refusal_before[3],
        "guarded attempt regressed the guest clock"
    );
    let after = assert_transport_clock_after_refusal(&site, refusal_after);
    println!(
        "private transport={name} native-control=ok guarded=ENOTSUP event-id={} owner={} clock-before={} clock-after={} clock-delta={BRANCHES}",
        after[1], after[2], refusal_after[3], after[3]
    );
    for fd in sockets.into_iter().chain(pipes) {
        close_descriptor(fd);
    }
}

fn destructive_alias_control(alias: i32, snapshot: [u64; 4]) {
    let mut event_id = 0_u64;
    assert_eq!(
        unsafe {
            trusted_raw(
                libc::SYS_ioctl,
                [alias as u64, IOC_ID, (&raw mut event_id) as u64, 0, 0, 0],
            )
        },
        0
    );
    assert_eq!(event_id, snapshot[1], "alias names the supervisor event");
    assert_eq!(
        unsafe { trusted_raw(libc::SYS_ioctl, [alias as u64, IOC_DISABLE, 0, 0, 0, 0]) },
        0
    );
    assert_eq!(
        unsafe { trusted_raw(libc::SYS_ioctl, [alias as u64, IOC_RESET, 0, 0, 0, 0]) },
        0
    );
    let mut count = u64::MAX;
    assert_eq!(
        unsafe {
            trusted_raw(
                libc::SYS_read,
                [alias as u64, (&raw mut count) as u64, 8, 0, 0, 0],
            )
        },
        8
    );
    assert_eq!(count, 0, "unguarded alias disabled and reset the private event");
}

fn write_sacrificial_result_and_exit(name: &str, snapshot: [u64; 4]) -> ! {
    let line = format!(
        "private sacrificial={name} event-id={} owner={} destructive-effect=confirmed\n",
        snapshot[1], snapshot[2]
    );
    assert_eq!(
        unsafe {
            trusted_raw(
                libc::SYS_write,
                [
                    libc::STDOUT_FILENO as u64,
                    line.as_ptr() as u64,
                    line.len() as u64,
                    0,
                    0,
                    0,
                ],
            )
        },
        line.len() as i64
    );
    unsafe { trusted_raw(libc::SYS_exit_group, [0; 6]) };
    unreachable!()
}

pub(super) fn run_private_transport_sacrificial(path: &Path, transport: PrivateTransport) -> ! {
    let sockets = matches!(transport, PrivateTransport::ScmRights).then(socket_pair);
    TRANSPORT_MODE.store(true, Ordering::Release);
    unsafe { reverie_liteinst::with_tool_root!({
        unsafe { reverie_liteinst::install_tool::<CounterTool>(path) }.unwrap();
    }); }
    let snapshot = private_snapshot();
    let private = snapshot[0] as i32;
    let name = match transport {
        PrivateTransport::ScmRights => {
            let sockets = sockets.unwrap();
            assert_eq!(
                unsafe {
                    send_descriptor_with(trusted_raw, libc::SYS_sendmsg, sockets[0], private)
                },
                1
            );
            let (received, alias) = unsafe {
                receive_descriptor_with(trusted_raw, libc::SYS_recvmsg, sockets[1])
            };
            assert_eq!(received, 1);
            let alias = alias.expect("unguarded SCM_RIGHTS private alias");
            assert_ne!(alias, private);
            assert_eq!(descriptor_identity(alias), descriptor_identity(private));
            destructive_alias_control(alias, snapshot);
            "scm-rights"
        }
        PrivateTransport::PidfdGetfd => {
            let pid = unsafe { trusted_raw(libc::SYS_getpid, [0; 6]) };
            assert!(pid > 0);
            let pidfd = unsafe {
                trusted_raw(libc::SYS_pidfd_open, [pid as u64, 0, 0, 0, 0, 0])
            };
            assert!(pidfd >= 0, "sacrificial pidfd_open: {pidfd}");
            let alias = unsafe {
                trusted_raw(
                    libc::SYS_pidfd_getfd,
                    [pidfd as u64, private as u64, 0, 0, 0, 0],
                )
            };
            assert!(alias >= 0, "unguarded pidfd_getfd private alias: {alias}");
            destructive_alias_control(alias as i32, snapshot);
            "pidfd_getfd"
        }
        PrivateTransport::IoUring => {
            let mut ring = NumericRing::new(trusted_raw);
            ring.close_operation(trusted_raw, private);
            assert_eq!(
                unsafe {
                    trusted_raw(
                        libc::SYS_fcntl,
                        [private as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
                    )
                },
                -i64::from(libc::EBADF),
                "unguarded IORING_OP_CLOSE destroyed the private slot"
            );
            "io_uring-close"
        }
    };
    write_sacrificial_result_and_exit(name, snapshot)
}

pub(super) fn run_inherited_sqpoll_refusal(path: &Path) {
    let mut probe = IoUringProbe::default();
    for index in 0..IO_RINGFD_REG_MAX {
        assert_eq!(
            unsafe {
                raw(
                    libc::SYS_io_uring_register,
                    [
                        index as u64,
                        (IORING_REGISTER_PROBE | IORING_REGISTER_USE_REGISTERED_RING) as u64,
                        (&raw mut probe) as u64,
                        1,
                        0,
                        0,
                    ],
                )
            },
            -i64::from(libc::EBADF),
            "initial registered-ring slot {index}"
        );
    }
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_io_uring_register,
                [
                    IO_RINGFD_REG_MAX as u64,
                    (IORING_REGISTER_PROBE | IORING_REGISTER_USE_REGISTERED_RING) as u64,
                    (&raw mut probe) as u64,
                    1,
                    0,
                    0,
                ],
            )
        },
        -i64::from(libc::EINVAL),
        "index outside the complete registered-ring table"
    );

    let ordinary = NumericRing::new(raw);
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_io_uring_register,
                [
                    ordinary.fd as u64,
                    IORING_REGISTER_PROBE as u64,
                    (&raw mut probe) as u64,
                    1,
                    0,
                    0,
                ],
            )
        },
        0,
        "ordinary numeric ring probe"
    );
    ordinary.destroy(raw);

    let pipe = pipe_pair();
    let mut ring = RegisteredSqpollRing::new();
    ring.close_operation(pipe[0]);
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_fcntl,
                [pipe[0] as u64, libc::F_GETFD as u64, 0, 0, 0, 0],
            )
        },
        -i64::from(libc::EBADF)
    );
    close_descriptor(pipe[1]);
    probe = IoUringProbe::default();
    assert_eq!(
        unsafe {
            raw(
                libc::SYS_io_uring_register,
                [
                    ring.index as u64,
                    (IORING_REGISTER_PROBE | IORING_REGISTER_USE_REGISTERED_RING) as u64,
                    (&raw mut probe) as u64,
                    1,
                    0,
                    0,
                ],
            )
        },
        0,
        "live registered-only ring probe"
    );
    assert!(ring.bytes >= 8192 && !ring.memory.is_null());
    unsafe { reverie_liteinst::with_tool_root!({
        let error = unsafe { reverie_liteinst::install_tool::<CounterTool>(path) }.unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTSUP));
    }); }
    panic!("a refused root activation returned to guest execution");
}

unsafe extern "C" {
    fn rcb_acquisition_operation(record: *mut Record);
    fn rcb_acquisition_sample();
    fn rcb_acquisition_no_sample();
    fn rcb_transport_clock_probe(site: usize);
}
global_asm!(include_str!("rcb_acquisition.S"), options(att_syntax));
