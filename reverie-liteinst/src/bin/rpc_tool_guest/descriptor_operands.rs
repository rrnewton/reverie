//! Guest cells for the runtime's own descriptors where a call names them
//! inside memory, by index, or through io_uring
//! (https://github.com/facebookexperimental/reverie/issues/56). After the
//! Tool is installed, FICLONE and F_DUPFD_QUERY naming a reserved descriptor
//! answer EBADF; FICLONERANGE, FIDEDUPERANGE and kcmp run on checked copies
//! with reserved descriptors hidden; and every io_uring call answers ENOSYS.
//!
//! Each cell's call is made at one of two syscall sites, so that it reaches
//! the runtime on the route its run names: `hook`, a site in this
//! executable's text that LiteInst patches at its first call, so that later
//! calls enter its hook; or `trap`, a three-byte site in an anonymous mapping,
//! too short to patch, so that every call traps to the fallback. A call that
//! did not take its route is reported with an `off-route` suffix, which no
//! required value has.
//!
//! The reserved number the cells name is the runtime's performance counter,
//! found after a clock read through `/proc/self/fd` (where reserved
//! descriptors are visible) as the `anon_inode:[perf_event]` descriptor at or
//! above the runtime's floor. Every line printed here starts `operands`,
//! followed by its kind (`cell`, `detail`, `setup`, `no-result`, `refused` or
//! `fail`), the cell's id (`*` for every cell of the run), its route, and the
//! observed text; the test compares each observed value with the cell's
//! required one.

use core::arch::global_asm;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;

use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::Sysno;

#[path = "descriptor_operands/exec_ring.rs"]
mod exec_ring;
#[path = "descriptor_operands/filters.rs"]
mod filters;
#[path = "descriptor_operands/io_uring.rs"]
mod io_uring;
#[path = "descriptor_operands/preactivation_ring.rs"]
mod preactivation_ring;

/// `FICLONE`, `_IOW(0x94, 9, int)`: the source descriptor is argument 2.
const FICLONE: u64 = 0x4004_9409;
/// `FICLONERANGE`, `_IOW(0x94, 13, struct file_clone_range)`.
const FICLONERANGE: u64 = 0x4020_940d;
/// `FIDEDUPERANGE`, `_IOWR(0x94, 54, struct file_dedupe_range)`.
const FIDEDUPERANGE: u64 = 0xc018_9436;
/// `F_DUPFD_QUERY`: whether argument 2 names the same file as the descriptor.
const F_DUPFD_QUERY: u64 = 1027;
const KCMP_FILE: u64 = 0;
const KCMP_EPOLL_TFD: u64 = 7;
/// A descriptor number above every possible table size (fs/file.c:100-101),
/// so always closed. Not -1, which io_uring_register takes as its blind
/// opcode path rather than a descriptor lookup.
const CLOSED: u64 = 0x7FFF_FFF0;
/// The size of the dedupe cells' files and of their requests' range.
const BYTES: usize = 4096;
/// What the supervisor answers for the staged dedupe's second copy: the
/// 24-byte header and one 32-byte entry, the copy a count of 1 asks for.
const STAGED_COPY_BYTES: i64 = 56;
const PERF_EVENT: &str = "anon_inode:[perf_event]";

const CLOCK_NOT_READ: u64 = 0;
const CLOCK_AVAILABLE: u64 = 1;
const CLOCK_UNAVAILABLE: u64 = 2;
/// What the Tool's last clock read gave.
static CLOCK: AtomicU64 = AtomicU64::new(CLOCK_NOT_READ);

/// Reads the runtime's clock on every guest getpid, so that a guest can make
/// the clock read the cells start with, and lets every call run.
#[derive(Default)]
struct OperandTool;

#[reverie::tool]
impl Tool for OperandTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ();

    fn subscriptions(_config: &()) -> Subscription {
        [Sysno::getpid].into_iter().collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let clock = if guest.read_clock().is_ok() {
            CLOCK_AVAILABLE
        } else {
            CLOCK_UNAVAILABLE
        };
        CLOCK.store(clock, Ordering::Release);
        Ok(guest.inject(syscall).await?)
    }
}

/// `struct file_clone_range`.
#[repr(C)]
#[derive(Default)]
struct FileCloneRange {
    src_fd: i64,
    src_offset: u64,
    src_length: u64,
    dest_offset: u64,
}

/// `struct kcmp_epoll_slot`.
#[repr(C)]
struct KcmpEpollSlot {
    efd: u32,
    tfd: u32,
    toff: u32,
}

/// `struct file_dedupe_range` with one `struct file_dedupe_range_info`.
#[repr(C)]
#[derive(Default)]
struct DedupeOne {
    src_offset: u64,
    src_length: u64,
    dest_count: u16,
    reserved1: u16,
    reserved2: u32,
    dest_fd: i64,
    dest_offset: u64,
    bytes_deduped: u64,
    status: i32,
    reserved: u32,
}

const _: () = assert!(core::mem::size_of::<DedupeOne>() == 56);

impl DedupeOne {
    /// {src_offset 0, src_length 4096, dest_count 1, [dest_fd, 0]}.
    fn naming(dest_fd: i64) -> Self {
        Self {
            src_length: BYTES as u64,
            dest_count: 1,
            dest_fd,
            ..Self::default()
        }
    }

    /// The entry's status after a call that returned `result`, or the call's
    /// own error.
    fn status_after(&self, result: i64) -> String {
        if result == 0 {
            format!("status={}", self.status)
        } else {
            errno_or_ret(result)
        }
    }

    /// Every field, as the guest's memory now holds it.
    fn fields(&self) -> String {
        format!(
            "header={},{},{},{},{} entry={},{},{},{},{}",
            self.src_offset,
            self.src_length,
            self.dest_count,
            self.reserved1,
            self.reserved2,
            self.dest_fd,
            self.dest_offset,
            self.bytes_deduped,
            self.status,
            self.reserved
        )
    }
}

global_asm!(
    r#"
    .text
    .p2align 6
    .global descriptor_operands_patched_site
    .hidden descriptor_operands_patched_site
    .type descriptor_operands_patched_site,@function
descriptor_operands_patched_site:
    .cfi_startproc
    syscall
    nop
    nop
    nop
    ret
    .cfi_endproc
    .size descriptor_operands_patched_site, .-descriptor_operands_patched_site

    # descriptor_operands_call(site, number, args): the syscall `number` with
    # the six words at `args`, made at the `syscall` instruction at `site`.
    .p2align 4
    .global descriptor_operands_call
    .hidden descriptor_operands_call
    .type descriptor_operands_call,@function
descriptor_operands_call:
    .cfi_startproc
    sub rsp, 8
    .cfi_adjust_cfa_offset 8
    mov r11, rdi
    mov rax, rsi
    mov rdi, [rdx]
    mov rsi, [rdx + 8]
    mov r10, [rdx + 24]
    mov r8, [rdx + 32]
    mov r9, [rdx + 40]
    mov rdx, [rdx + 16]
    call r11
    add rsp, 8
    .cfi_adjust_cfa_offset -8
    ret
    .cfi_endproc
    .size descriptor_operands_call, .-descriptor_operands_call
"#
);

unsafe extern "C" {
    fn descriptor_operands_call(site: usize, number: i64, args: *const [u64; 6]) -> i64;
    static descriptor_operands_patched_site: u8;
}

/// How a run's calls reach the runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    /// LiteInst's patched hook.
    Hook,
    /// The SIGSYS trap and the fallback continuation.
    Trap,
}

impl Route {
    fn parse(text: &OsStr) -> Self {
        match text.to_str() {
            Some("hook") => Route::Hook,
            Some("trap") => Route::Trap,
            other => panic!("unknown route {other:?}"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Route::Hook => "hook",
            Route::Trap => "trap",
        }
    }
}

/// A syscall site on one route.
struct Site {
    route: Route,
    address: usize,
}

/// One call made at a site, with the site's trap and hook counts' changes.
struct Call {
    result: i64,
    traps: u64,
    hooks: u64,
    route: Route,
}

impl Call {
    /// Whether the call took its site's route: one hook entry and no trap at
    /// the patched site, or one trap and no hook entry at the trap site.
    fn on_route(&self) -> bool {
        match self.route {
            Route::Hook => (self.traps, self.hooks) == (0, 1),
            Route::Trap => (self.traps, self.hooks) == (1, 0),
        }
    }

    /// `value`, marked when the call did not take its route.
    fn observed(&self, value: String) -> String {
        if self.on_route() {
            return value;
        }
        let (traps, hooks) = (self.traps, self.hooks);
        format!("{value} off-route(traps+{traps},hooks+{hooks})")
    }
}

impl Site {
    /// The route's site. Made before the runtime is installed: the trap
    /// route's site is a fresh anonymous mapping.
    fn new(route: Route) -> Self {
        let address = match route {
            Route::Hook => core::ptr::addr_of!(descriptor_operands_patched_site) as usize,
            Route::Trap => trap_site(),
        };
        Self { route, address }
    }

    fn counts(&self) -> (u64, u64) {
        let address = self.address as u64;
        let traps = reverie_liteinst::reverie_liteinst_site_trap_count(address);
        let hooks = reverie_liteinst::reverie_liteinst_site_hook_count(address);
        (traps, hooks)
    }

    /// The syscall `number` with `args`, made at this site as the guest.
    fn call(&self, number: i64, args: [u64; 6]) -> Call {
        let (traps, hooks) = self.counts();
        let result = unsafe { descriptor_operands_call(self.address, number, &args) };
        let (traps_after, hooks_after) = self.counts();
        Call {
            result,
            traps: traps_after.wrapping_sub(traps),
            hooks: hooks_after.wrapping_sub(hooks),
            route: self.route,
        }
    }
}

/// `syscall; ret` at the end of a fresh executable page: three bytes, too
/// short for LiteInst to patch, so every call there traps.
fn trap_site() -> usize {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mapping = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let code: [u8; 3] = [0x0f, 0x05, 0xc3];
    let site = unsafe { mapping.cast::<u8>().add(page - code.len()) };
    unsafe { core::ptr::copy_nonoverlapping(code.as_ptr(), site, code.len()) };
    let executable = libc::PROT_READ | libc::PROT_EXEC;
    assert_eq!(unsafe { libc::mprotect(mapping, page, executable) }, 0);
    site as usize
}

/// A raw syscall result as the cells print it.
fn errno_or_ret(result: i64) -> String {
    if (-4095..0).contains(&result) {
        format!("errno={}", -result)
    } else {
        format!("ret={result}")
    }
}

/// Prints one line for the test: `operands <kind> <id> <route> <text>`.
fn report(kind: &str, id: &str, route: &str, text: &str) {
    println!("operands {kind} {id} {route} {text}");
}

/// A cell whose observed value is the call's result.
fn cell(id: &str, call: &Call) {
    let value = call.observed(errno_or_ret(call.result));
    report("cell", id, call.route.label(), &value);
}

/// An `io_uring_setup(1, params)` cell at `site`. A descriptor, which only a
/// missing refusal gives, is closed and reported as `ret>=0`.
fn ring_setup_cell(id: &str, site: &Site) {
    let mut params = io_uring::Params::default();
    let arguments = [1, (&raw mut params) as u64, 0, 0, 0, 0];
    let call = site.call(io_uring::SYS_IO_URING_SETUP, arguments);
    let value = if call.result >= 0 {
        unsafe { libc::close(call.result as i32) };
        "ret>=0".to_string()
    } else {
        errno_or_ret(call.result)
    };
    report("cell", id, call.route.label(), &call.observed(value));
}

/// Installs the Tool: the activation every cell after it depends on. A
/// failure fails every cell of the run.
fn activate(path: &Path, label: &str) {
    let installed = unsafe { reverie_liteinst::install_tool::<OperandTool>(path) };
    if let Err(error) = installed {
        let reason = format!("install_tool failed: {error}");
        report("fail", "*", label, &reason);
        std::process::exit(1);
    }
}

/// The lowest number the runtime gives its own descriptors in this process:
/// RUNTIME_FD_MIN, or, under a soft RLIMIT_NOFILE that leaves no room there,
/// the bottom of the eight numbers just below that limit (the fallback band
/// of `reverie_inguest::guest::protect::runtime_fd_minimums`).
fn runtime_floor() -> i32 {
    const RESERVE: i32 = 8;
    let minimum = reverie_inguest::guest::protect::RUNTIME_FD_MIN;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0;
    if read && limit.rlim_cur < (minimum + RESERVE) as u64 {
        (limit.rlim_cur as i32 - RESERVE).max(3)
    } else {
        minimum
    }
}

/// Every open descriptor at or above `floor`, with what its `/proc/self/fd`
/// link reads, in increasing order.
fn descriptors_from(floor: i32) -> Vec<(i32, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd").expect("/proc/self/fd") {
        let entry = entry.expect("a /proc/self/fd entry");
        let name = entry.file_name();
        let Some(fd) = name.to_str().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        if fd < floor {
            continue;
        }
        if let Ok(target) = std::fs::read_link(entry.path()) {
            found.push((fd, target.to_string_lossy().into_owned()));
        }
    }
    found.sort();
    found
}

/// The cells' setup on `site`'s route: a clock read through the site (which
/// also patches the hook route's site), so that the runtime's counter is in
/// place, then the reserved number, the lowest `anon_inode:[perf_event]`
/// descriptor at or above the runtime's floor. Finding none fails every cell
/// of the run.
fn reserved_counter(site: &Site) -> Option<i32> {
    let label = site.route.label();
    site.call(libc::SYS_getpid, [0; 6]);
    let clock = match CLOCK.load(Ordering::Acquire) {
        CLOCK_AVAILABLE => "available",
        CLOCK_UNAVAILABLE => "unavailable",
        _ => "not-read",
    };
    let floor = runtime_floor();
    let counters: Vec<i32> = descriptors_from(floor)
        .into_iter()
        .filter(|(_, target)| target == PERF_EVENT)
        .map(|(fd, _)| fd)
        .collect();
    let setup = format!("clock={clock} floor={floor} counters={counters:?}");
    report("setup", "*", label, &setup);
    let reserved = counters.first().copied();
    if reserved.is_none() {
        let reason = format!(
            "no {PERF_EVENT} at or above floor {floor} after the clock read (clock {clock})"
        );
        report("fail", "*", label, &reason);
    }
    reserved
}

/// A new file at `path`, opened for reading and writing, holding `content`.
fn create_file(path: &Path, content: &[u8]) -> File {
    use std::io::Write;
    let mut file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap_or_else(|error| panic!("creating {}: {error}", path.display()));
    file.write_all(content).unwrap();
    file
}

/// Waits for a cell's child, failing the cell if it did not exit with 0.
fn wait_child(id: &str, label: &str, child: libc::pid_t) {
    let mut status = 0;
    let waited = unsafe { libc::waitpid(child, &mut status, 0) };
    if waited != child || !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        let reason = format!("child {child}: wait status {status:#x}, waitpid {waited}");
        report("fail", id, label, &reason);
    }
}

/// Cells E1, E2 and E4-E11 on `route`: the reserved counter named by FICLONE,
/// in FICLONERANGE's operand, by F_DUPFD_QUERY, by kcmp's KCMP_FILE index and
/// in KCMP_EPOLL_TFD's slot; a kcmp of two guest descriptors; the three
/// io_uring calls; and FICLONERANGE with every copy failed by a guest filter.
pub(super) fn run(path: &Path, route: &OsStr) {
    let route = Route::parse(route);
    let label = route.label();
    let directory = path.parent().expect("the coordinator socket's directory");
    let pid = std::process::id();
    // Before the runtime: the route's site; G, a regular file in the test's
    // directory, which the clone ioctls target; and a memfd, a file on
    // another mount, as the filtered clone's source.
    let site = Site::new(route);
    let target = create_file(&directory.join(format!("operands-{label}-{pid}")), &[]);
    let source = unsafe { libc::memfd_create(c"operands-source".as_ptr(), 0) };
    let error = std::io::Error::last_os_error();
    assert!(source >= 0, "memfd_create: {error}");
    activate(path, label);
    let Some(reserved) = reserved_counter(&site) else {
        std::process::exit(1)
    };
    let r = reserved as u64;
    let g = target.as_raw_fd() as u64;
    let pid = u64::from(pid);

    // E1: FICLONE from the reserved counter.
    let ficlone = [g, FICLONE, r, 0, 0, 0];
    cell("E1", &site.call(libc::SYS_ioctl, ficlone));
    // E2: FICLONERANGE whose operand names the reserved counter.
    let range = FileCloneRange {
        src_fd: i64::from(reserved),
        ..FileCloneRange::default()
    };
    let range_address = (&raw const range) as u64;
    let clone_range = [g, FICLONERANGE, range_address, 0, 0, 0];
    cell("E2", &site.call(libc::SYS_ioctl, clone_range));
    // E4: F_DUPFD_QUERY comparing G with the reserved counter.
    let query = [g, F_DUPFD_QUERY, r, 0, 0, 0];
    cell("E4", &site.call(libc::SYS_fcntl, query));
    // E5: KCMP_FILE of the reserved counter and G.
    let reserved_file = [pid, pid, KCMP_FILE, r, g, 0];
    cell("E5", &site.call(libc::SYS_kcmp, reserved_file));
    // E6: KCMP_FILE of G with itself.
    let guest_file = [pid, pid, KCMP_FILE, g, g, 0];
    cell("E6", &site.call(libc::SYS_kcmp, guest_file));
    // E7: KCMP_EPOLL_TFD whose slot names the reserved counter as its epoll.
    let slot = KcmpEpollSlot {
        efd: reserved as u32,
        tfd: g as u32,
        toff: 0,
    };
    let slot_address = (&raw const slot) as u64;
    let epoll = [pid, pid, KCMP_EPOLL_TFD, g, slot_address, 0];
    cell("E7", &site.call(libc::SYS_kcmp, epoll));
    // E8-E10: the three io_uring calls, on a closed descriptor where one is
    // named.
    ring_setup_cell("E8", &site);
    let enter = [CLOSED, 0, 0, 0, 0, 0];
    cell("E9", &site.call(io_uring::SYS_IO_URING_ENTER, enter));
    let register = [CLOSED, 0, 0, 0, 0, 0];
    cell("E10", &site.call(io_uring::SYS_IO_URING_REGISTER, register));
    filtered_copy(&site, g, source);
}

/// E11: in a forked child, a guest filter fails every copy the runtime makes
/// of a guest operand. FICLONERANGE then answers EFAULT, as Linux answers an
/// operand it cannot read, and clones nothing. Its operand names `source`, a
/// file on another mount than `target`, so a call that read the guest's own
/// operand would answer EXDEV instead.
fn filtered_copy(site: &Site, target: u64, source: i32) {
    let label = site.route.label();
    let child = unsafe { libc::fork() };
    let error = std::io::Error::last_os_error();
    assert!(child >= 0, "fork: {error}");
    if child != 0 {
        wait_child("E11", label, child);
        return;
    }
    // The child's clock read binds its own counter before the filter.
    site.call(libc::SYS_getpid, [0; 6]);
    if let Err(reason) = filters::deny_copies(site) {
        report("refused", "E11", label, &reason);
        unsafe { libc::_exit(0) };
    }
    let range = FileCloneRange {
        src_fd: i64::from(source),
        ..FileCloneRange::default()
    };
    let range_address = (&raw const range) as u64;
    let clone_range = [target, FICLONERANGE, range_address, 0, 0, 0];
    cell("E11", &site.call(libc::SYS_ioctl, clone_range));
    unsafe { libc::_exit(0) };
}

/// Cells E3 and E12 on `route`, which need a file system that dedupes
/// (`reflink`, btrfs or XFS) and a second file system (`second`); both
/// directories are the test's own, fresh for this run. E3: FIDEDUPERANGE
/// naming the reserved counter as its destination. E12: a dedupe whose
/// second copy a supervisor fabricates.
pub(super) fn run_reflink(path: &Path, route: &OsStr, reflink: &OsStr, second: &OsStr) {
    let route = Route::parse(route);
    let label = route.label();
    let reflink = Path::new(reflink);
    let second = Path::new(second);
    // Before the runtime: the ioctl's source and the destinations G and G2,
    // equal 4,096-byte files on the reflink file system; W on the second
    // file system; and the route's site.
    let pattern: Vec<u8> = (0..BYTES).map(|index| (index * 7 + 3) as u8).collect();
    let source = create_file(&reflink.join("source"), &pattern);
    let g = create_file(&reflink.join("g"), &pattern);
    let g2 = create_file(&reflink.join("g2"), &pattern);
    let w = create_file(&second.join("w"), &[]);
    let site = Site::new(route);
    activate(path, label);
    let Some(reserved) = reserved_counter(&site) else {
        std::process::exit(1)
    };

    // E3: FIDEDUPERANGE whose one destination is the reserved counter.
    let mut request = DedupeOne::naming(i64::from(reserved));
    let source_fd = source.as_raw_fd() as u64;
    let request_address = (&raw mut request) as u64;
    let dedupe = [source_fd, FIDEDUPERANGE, request_address, 0, 0, 0];
    let call = site.call(libc::SYS_ioctl, dedupe);
    let value = call.observed(request.status_after(call.result));
    report("cell", "E3", label, &value);
    let files = DedupeFiles {
        source: &source,
        g: &g,
        g2: &g2,
        w: &w,
        pattern: &pattern,
    };
    staged_fabricated(&site, &files);
}

/// The files of the staged dedupe: the ioctl's source, G (FC0's destination,
/// equal content), G2 (the destination the guest's own staged request names,
/// equal content), W (on the second file system, which fd 0 is made to
/// name) and the content the equal files hold.
struct DedupeFiles<'a> {
    source: &'a File,
    g: &'a File,
    g2: &'a File,
    w: &'a File,
    pattern: &'a [u8],
}

/// E12, in a forked child. FC0, a real dedupe of the source into G, leaves
/// its request in the runtime's buffer. Then fd 0 names W, on another
/// superblock, and a supervisor lets the runtime's first copy of the next
/// request (its count) run and fabricates the second (the header and entry):
/// success with the full count, nothing copied. The runtime zeroes the
/// buffer before that copy, so the kernel reads a request naming fd 0 with a
/// zero range: W's entry fails with EXDEV, G2, which the guest's own request
/// names, is untouched, and none of FC0's bytes reach the kernel.
fn staged_fabricated(site: &Site, files: &DedupeFiles<'_>) {
    let label = site.route.label();
    let child = unsafe { libc::fork() };
    let error = std::io::Error::last_os_error();
    assert!(child >= 0, "fork: {error}");
    if child != 0 {
        wait_child("E12", label, child);
        return;
    }
    // The child's clock read binds its own counter before the filter.
    site.call(libc::SYS_getpid, [0; 6]);
    let source_fd = files.source.as_raw_fd() as u64;
    // FC0.
    let mut control = DedupeOne::naming(i64::from(files.g.as_raw_fd()));
    let control_address = (&raw mut control) as u64;
    let dedupe = [source_fd, FIDEDUPERANGE, control_address, 0, 0, 0];
    let call = site.call(libc::SYS_ioctl, dedupe);
    let deduped = (control.status, control.bytes_deduped);
    if call.result != 0 || deduped != (0, BYTES as u64) {
        let result = errno_or_ret(call.result);
        let reason = format!(
            "FC0 control: {result}, (status, bytes_deduped) {deduped:?}, not ret=0, (0, {BYTES})"
        );
        report("fail", "E12", label, &reason);
        unsafe { libc::_exit(0) };
    }
    if unsafe { libc::dup2(files.w.as_raw_fd(), 0) } != 0 {
        let reason = format!("dup2 onto 0: {}", std::io::Error::last_os_error());
        report("fail", "E12", label, &reason);
        unsafe { libc::_exit(0) };
    }
    // Before the filter: this getpid reaches the Tool.
    let target = std::process::id();
    let listener = match filters::notify_copies(site) {
        Ok(listener) => listener,
        Err(reason) => {
            report("refused", "E12", label, &reason);
            unsafe { libc::_exit(0) }
        }
    };
    // A raw fork at the site: no libc wrapper, so no signal-mask change,
    // whose runtime copy the new filter would hold for a supervisor that
    // does not exist yet.
    let supervisor = site.call(libc::SYS_fork, [0; 6]).result;
    if supervisor == 0 {
        filters::supervise(site, listener, target, STAGED_COPY_BYTES, label);
    }
    if supervisor < 0 {
        let answer = errno_or_ret(supervisor);
        let reason = format!("the supervisor's fork answered {answer}");
        report("fail", "E12", label, &reason);
        unsafe { libc::_exit(0) };
    }
    // FC3's guest-level form: the guest's own request names G2.
    let mut staged = DedupeOne::naming(i64::from(files.g2.as_raw_fd()));
    let staged_address = (&raw mut staged) as u64;
    let dedupe = [source_fd, FIDEDUPERANGE, staged_address, 0, 0, 0];
    let call = site.call(libc::SYS_ioctl, dedupe);
    let supervisor = supervisor as libc::pid_t;
    unsafe { libc::kill(supervisor, libc::SIGKILL) };
    let mut status = 0;
    unsafe { libc::waitpid(supervisor, &mut status, 0) };
    let mut after = vec![0_u8; BYTES];
    let g2 = match files.g2.read_exact_at(&mut after, 0) {
        Ok(()) if after == files.pattern => "unchanged",
        Ok(()) => "changed",
        Err(_) => "unreadable",
    };
    let value = call.observed(staged.status_after(call.result));
    report("cell", "E12", label, &value);
    let detail = format!("ret={} {} g2={g2}", call.result, staged.fields());
    report("detail", "E12", label, &detail);
    unsafe { libc::_exit(0) };
}

/// The native controls, in a process that never installs the runtime: E13,
/// `io_uring_setup` gives a ring (`ok`), and E14, kcmp compares a descriptor
/// with itself (`works`, CONFIG_KCMP).
pub(super) fn run_native() {
    fn last_errno() -> String {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        format!("errno={errno}")
    }
    let mut params = io_uring::Params::default();
    let entries: libc::c_long = 1;
    let setup = io_uring::SYS_IO_URING_SETUP;
    let ring = unsafe { libc::syscall(setup, entries, &raw mut params) };
    let observed = if ring >= 0 {
        unsafe { libc::close(ring as i32) };
        "ok".to_string()
    } else {
        last_errno()
    };
    report("cell", "E13", "native", &observed);
    let file = unsafe { libc::memfd_create(c"operands-control".as_ptr(), 0) };
    let error = std::io::Error::last_os_error();
    assert!(file >= 0, "memfd_create: {error}");
    let pid = libc::c_long::from(unsafe { libc::getpid() });
    let file = libc::c_long::from(file);
    let kind = KCMP_FILE as libc::c_long;
    let compared = unsafe { libc::syscall(libc::SYS_kcmp, pid, pid, kind, file, file) };
    let observed = match compared {
        0 => "works".to_string(),
        negative if negative < 0 => last_errno(),
        other => format!("ret={other}"),
    };
    report("cell", "E14", "native", &observed);
}

/// Cells E15 and E16, the pre-activation ring (EXCLUDED-DECLARED).
pub(super) fn run_preactivation_ring(path: &Path) {
    preactivation_ring::run(path);
}

/// Cell E17's launcher (EXCLUDED-DECLARED), which never installs the runtime.
pub(super) fn run_exec_ring_launcher(path: &Path) {
    exec_ring::launch(path);
}

/// Cell E17 in the image the launcher executes.
pub(super) fn run_exec_ring_guest(path: &Path, descriptor: &OsStr) {
    exec_ring::guest(path, descriptor);
}
