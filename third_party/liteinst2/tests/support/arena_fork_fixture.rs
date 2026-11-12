#![deny(warnings)]
//! Ordinary main-thread fixture for fork-related arena ownership controls.

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn main() {
    panic!("arena fork controls require Linux x86-64");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn main() {
    native::run();
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod native {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;
    use std::io::Read;
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::Instant;

    use liteinst2::scanner::InstructionScanner;
    use liteinst2::trampoline::HookContext;
    use liteinst2::trampoline::TrampolineArena;
    use liteinst2::trampoline::TrampolineError;
    use liteinst2::trampoline::TrampolinePlan;

    const PAGE: usize = 4096;
    const LIMIT: Duration = Duration::from_secs(5);
    static TRACK: AtomicBool = AtomicBool::new(false);
    static META: AtomicUsize = AtomicUsize::new(0);
    static META_PROT: AtomicI32 = AtomicI32::new(-1);
    static META_UNMAPS: AtomicUsize = AtomicUsize::new(0);
    static META_ABSENT: AtomicBool = AtomicBool::new(false);
    static CLOSE_ERROR: AtomicBool = AtomicBool::new(false);
    static REPLACEMENT: AtomicI32 = AtomicI32::new(-1);
    static REAL_CLOSE_RESULT: AtomicI32 = AtomicI32::new(-2);
    static ORIGINAL_FD: AtomicI32 = AtomicI32::new(-1);
    static ORIGINAL_CLOSES: AtomicUsize = AtomicUsize::new(0);
    static ORIGINAL_CLOSED: AtomicBool = AtomicBool::new(false);
    static BACKING_DEV: AtomicUsize = AtomicUsize::new(0);
    static BACKING_INO: AtomicUsize = AtomicUsize::new(0);
    static RX_BACKING_MATCHES: AtomicBool = AtomicBool::new(false);
    static RW: AtomicUsize = AtomicUsize::new(0);
    static RX: AtomicUsize = AtomicUsize::new(0);
    static RW_UNMAPS: AtomicUsize = AtomicUsize::new(0);
    static RX_UNMAPS: AtomicUsize = AtomicUsize::new(0);
    static RW_ABSENT: AtomicBool = AtomicBool::new(false);
    static RX_ABSENT: AtomicBool = AtomicBool::new(false);

    // Isolated fixture interposition: forward every mmap to the real kernel.
    // Only record the additional metadata mapping in the constructor control.
    unsafe fn map(
        address: *mut libc::c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut libc::c_void {
        let result = unsafe { libc::syscall(libc::SYS_mmap, address, len, prot, flags, fd, offset) }
            as *mut libc::c_void;
        if TRACK.load(Ordering::Relaxed)
            && result != libc::MAP_FAILED
            && len == PAGE
            && flags == (libc::MAP_SHARED | libc::MAP_ANONYMOUS)
        {
            META.store(result as usize, Ordering::Relaxed);
            META_PROT.store(prot, Ordering::Relaxed);
        }
        if TRACK.load(Ordering::Relaxed)
            && result != libc::MAP_FAILED
            && len == 2 * PAGE
            && flags & libc::MAP_SHARED != 0
            && fd >= 0
        {
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::syscall(libc::SYS_fstat, fd, &raw mut stat) };
            if prot == libc::PROT_READ | libc::PROT_WRITE {
                ORIGINAL_FD.store(fd, Ordering::Relaxed);
                BACKING_DEV.store(stat.st_dev as usize, Ordering::Relaxed);
                BACKING_INO.store(stat.st_ino as usize, Ordering::Relaxed);
                RW.store(result as usize, Ordering::Relaxed);
            } else if prot == libc::PROT_READ | libc::PROT_EXEC {
                RX.store(result as usize, Ordering::Relaxed);
                RX_BACKING_MATCHES.store(
                    rc == 0
                        && fd == ORIGINAL_FD.load(Ordering::Relaxed)
                        && stat.st_dev as usize == BACKING_DEV.load(Ordering::Relaxed)
                        && stat.st_ino as usize == BACKING_INO.load(Ordering::Relaxed),
                    Ordering::Relaxed,
                );
            }
        }
        result
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mmap(
        address: *mut libc::c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut libc::c_void {
        unsafe { map(address, len, prot, flags, fd, offset) }
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn mmap64(
        address: *mut libc::c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut libc::c_void {
        unsafe { map(address, len, prot, flags, fd, offset) }
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn munmap(address: *mut libc::c_void, len: usize) -> i32 {
        let result = unsafe { libc::syscall(libc::SYS_munmap, address, len) } as i32;
        if result == 0
            && TRACK.load(Ordering::Relaxed)
            && META.load(Ordering::Relaxed) == address as usize
        {
            META_UNMAPS.fetch_add(1, Ordering::Relaxed);
            let mut state = 0_u8;
            let rc = unsafe { libc::syscall(libc::SYS_mincore, address, len, &raw mut state) };
            META_ABSENT.store(
                rc == -1 && unsafe { *libc::__errno_location() } == libc::ENOMEM,
                Ordering::Relaxed,
            );
        }
        if result == 0 && TRACK.load(Ordering::Relaxed) && len == 2 * PAGE {
            for (recorded, count, absent) in
                [(&RW, &RW_UNMAPS, &RW_ABSENT), (&RX, &RX_UNMAPS, &RX_ABSENT)]
            {
                if recorded.load(Ordering::Relaxed) == address as usize {
                    count.fetch_add(1, Ordering::Relaxed);
                    let mut all_absent = true;
                    for offset in [0, PAGE] {
                        let mut state = 0_u8;
                        let page = (address as usize + offset) as *mut libc::c_void;
                        let rc =
                            unsafe { libc::syscall(libc::SYS_mincore, page, PAGE, &raw mut state) };
                        all_absent &=
                            rc == -1 && unsafe { *libc::__errno_location() } == libc::ENOMEM;
                    }
                    absent.store(all_absent, Ordering::Relaxed);
                }
            }
        }
        result
    }
    #[unsafe(no_mangle)]
    unsafe extern "C" fn close(fd: i32) -> i32 {
        let result = unsafe { libc::syscall(libc::SYS_close, fd) } as i32;
        if TRACK.load(Ordering::Relaxed) && fd == ORIGINAL_FD.load(Ordering::Relaxed) {
            ORIGINAL_CLOSES.fetch_add(1, Ordering::Relaxed);
            ORIGINAL_CLOSED.store(result == 0, Ordering::Relaxed);
        }
        if META.load(Ordering::Relaxed) != 0 && CLOSE_ERROR.swap(false, Ordering::Relaxed) {
            REAL_CLOSE_RESULT.store(result, Ordering::Relaxed);
            // The actual close already released fd. Reuse its number before
            // returning the simulated late error to the constructor caller.
            let replacement = unsafe {
                libc::syscall(
                    libc::SYS_openat,
                    libc::AT_FDCWD,
                    c"/dev/null".as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC,
                    0,
                )
            } as i32;
            if replacement != fd && replacement >= 0 {
                let duplicated =
                    unsafe { libc::syscall(libc::SYS_dup3, replacement, fd, libc::O_CLOEXEC) }
                        as i32;
                unsafe {
                    libc::syscall(libc::SYS_close, replacement);
                }
                REPLACEMENT.store(duplicated, Ordering::Relaxed);
            } else {
                REPLACEMENT.store(replacement, Ordering::Relaxed);
            }
            unsafe {
                *libc::__errno_location() = libc::EIO;
            }
            return -1;
        }
        result
    }

    unsafe extern "C" fn noop(_context: *mut HookContext) {}

    fn plans() -> Vec<TrampolinePlan> {
        let page = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(page, libc::MAP_FAILED);
        let scanner = InstructionScanner::default();
        let mut plans = Vec::new();
        for index in 0..3 {
            // mov eax, distinct value; ret. The displaced five-byte mov is
            // emitted by the plan and its continuation reaches the real ret.
            let code = [0xb8, 11 + 11 * index as u8, 0, 0, 0, 0xc3];
            let address = unsafe { page.cast::<u8>().add(index * 16) };
            unsafe {
                std::ptr::copy_nonoverlapping(code.as_ptr(), address, code.len());
            }
            let scan = scanner.scan(&code, address as u64).unwrap();
            plans.push(TrampolinePlan::from_scan(&scan, address as u64, noop).unwrap());
        }
        assert_eq!(
            unsafe { libc::mprotect(page, PAGE, libc::PROT_READ | libc::PROT_EXEC) },
            0
        );
        // This original-code mapping, like its trampolines, lasts until exit.
        plans
    }

    fn check_image(plan: &TrampolinePlan, address: u64, expected: u32) {
        let image = plan.emit_at(address).unwrap();
        let actual =
            unsafe { std::slice::from_raw_parts(address as *const u8, image.bytes().len()) };
        assert_eq!(
            actual,
            image.bytes(),
            "a published trampoline image was overwritten"
        );
        let function: unsafe extern "C" fn() -> u32 =
            unsafe { std::mem::transmute(address as usize) };
        assert_eq!(unsafe { function() }, expected, "wrong trampoline result");
    }
    fn allocate(arena: &TrampolineArena, plan: &TrampolinePlan, count: usize) -> Vec<u64> {
        (0..count)
            .map(|_| arena.allocate(plan).unwrap().address())
            .collect()
    }
    fn write_addresses(stream: &mut UnixStream, addresses: &[u64]) {
        for address in addresses {
            stream.write_all(&address.to_le_bytes()).unwrap();
        }
    }
    fn read_addresses(stream: &mut UnixStream, count: usize) -> Vec<u64> {
        (0..count)
            .map(|_| {
                let mut bytes = [0; 8];
                stream.read_exact(&mut bytes).unwrap();
                u64::from_le_bytes(bytes)
            })
            .collect()
    }
    fn check_all(
        arena: &TrampolineArena,
        plans: &[TrampolinePlan],
        existing: u64,
        parent: &[u64],
        child: &[u64],
    ) {
        let addresses: BTreeSet<_> = [existing]
            .into_iter()
            .chain(parent.iter().copied())
            .chain(child.iter().copied())
            .collect();
        assert_eq!(
            addresses.len(),
            1 + parent.len() + child.len(),
            "fork-related processes reused an arena slot"
        );
        let ordered: Vec<_> = addresses.iter().copied().collect();
        for pair in ordered.windows(2) {
            assert!(
                pair[0].checked_add(PAGE as u64).unwrap() <= pair[1],
                "fork-related processes received overlapping full slots"
            );
        }
        check_image(&plans[0], existing, 11);
        for address in parent {
            check_image(&plans[1], *address, 22);
        }
        for address in child {
            check_image(&plans[2], *address, 33);
        }
        for _ in 0..32 {
            assert!(
                matches!(arena.allocate(&plans[0]), Err(TrampolineError::ArenaFull)),
                "all processes must observe the same exhausted slot budget"
            );
        }
        check_image(&plans[0], existing, 11);
        for address in parent {
            check_image(&plans[1], *address, 22);
        }
        for address in child {
            check_image(&plans[2], *address, 33);
        }
    }

    struct Child(i32);
    impl Child {
        fn wait(mut self) {
            let deadline = Instant::now() + LIMIT;
            loop {
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let rc = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        self.0 as u32,
                        &raw mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                if rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                assert_eq!(rc, 0);
                if unsafe { info.si_pid() } != 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "fork child exceeded five seconds"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(self.0, &raw mut status, 0) }, self.0);
            self.0 = -1;
            assert!(libc::WIFEXITED(status), "child wait status {status}");
            assert_eq!(libc::WEXITSTATUS(status), 0, "child wait status {status}");
        }
    }
    impl Drop for Child {
        fn drop(&mut self) {
            if self.0 > 0 {
                unsafe {
                    libc::kill(self.0, libc::SIGKILL);
                }
                loop {
                    let rc = unsafe { libc::waitpid(self.0, std::ptr::null_mut(), 0) };
                    if rc == self.0 {
                        break;
                    }
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::EINTR)
                    );
                }
            }
        }
    }

    fn fork_case(mode: &str) {
        let plans = plans();
        let count = if mode == "concurrent" { 16 } else { 1 };
        let arena =
            TrampolineArena::allocate_near(plans[0].execute_address(), 1 + 2 * count).unwrap();
        let existing = arena.allocate(&plans[0]).unwrap().address();
        check_image(&plans[0], existing, 11);
        let (mut parent_stream, mut child_stream) = UnixStream::pair().unwrap();
        for stream in [&parent_stream, &child_stream] {
            stream.set_read_timeout(Some(LIMIT)).unwrap();
            stream.set_write_timeout(Some(LIMIT)).unwrap();
        }
        assert_single_thread();
        let pid = unsafe { libc::fork() };
        assert!(
            pid >= 0,
            "real fork unavailable: {}",
            std::io::Error::last_os_error()
        );
        if pid == 0 {
            drop(parent_stream);
            let result = std::panic::catch_unwind(move || {
                let (parent, child) = if mode == "parent-first" {
                    let parent = read_addresses(&mut child_stream, count);
                    let child = allocate(&arena, &plans[2], count);
                    write_addresses(&mut child_stream, &child);
                    (parent, child)
                } else if mode == "child-first" {
                    let child = allocate(&arena, &plans[2], count);
                    write_addresses(&mut child_stream, &child);
                    (read_addresses(&mut child_stream, count), child)
                } else {
                    let mut ready = [0];
                    child_stream.read_exact(&mut ready).unwrap();
                    assert_eq!(ready, [1]);
                    let child = allocate(&arena, &plans[2], count);
                    write_addresses(&mut child_stream, &child);
                    (read_addresses(&mut child_stream, count), child)
                };
                check_all(&arena, &plans, existing, &parent, &child);
            });
            unsafe {
                libc::_exit(if result.is_ok() { 0 } else { 101 });
            }
        }
        let child_owner = Child(pid);
        drop(child_stream);
        let (parent, child) = if mode == "parent-first" {
            let parent = allocate(&arena, &plans[1], count);
            write_addresses(&mut parent_stream, &parent);
            (parent, read_addresses(&mut parent_stream, count))
        } else if mode == "child-first" {
            let child = read_addresses(&mut parent_stream, count);
            let parent = allocate(&arena, &plans[1], count);
            write_addresses(&mut parent_stream, &parent);
            (parent, child)
        } else {
            parent_stream.write_all(&[1]).unwrap();
            let parent = allocate(&arena, &plans[1], count);
            write_addresses(&mut parent_stream, &parent);
            (parent, read_addresses(&mut parent_stream, count))
        };
        check_all(&arena, &plans, existing, &parent, &child);
        child_owner.wait();
        check_all(&arena, &plans, existing, &parent, &child);
        println!(
            "arena {mode}: {} distinct slots, exact images and execution, shared exhaustion, child reaped",
            1 + 2 * count
        );
    }
    fn assert_single_thread() {
        assert_eq!(unsafe { libc::getpid() } as i64, unsafe {
            libc::syscall(libc::SYS_gettid)
        });
        assert_eq!(std::fs::read_dir("/proc/self/task").unwrap().count(), 1);
    }
    fn capacity() {
        let plans = plans();
        assert!(matches!(
            TrampolineArena::allocate_near(plans[0].execute_address(), 0),
            Err(TrampolineError::InvalidArenaCapacity)
        ));
        assert!(matches!(
            TrampolineArena::allocate_near(plans[0].execute_address(), usize::MAX),
            Err(TrampolineError::InvalidArenaCapacity)
        ));
        let arena = TrampolineArena::allocate_near(plans[0].execute_address(), 2).unwrap();
        assert!(arena.can_reach(plans[0].execute_address()));
        assert!(!arena.can_reach(u64::MAX));
        let scanner = InstructionScanner::default();
        let unreachable = TrampolinePlan::from_scan(
            &scanner.scan(&[0xb8, 11, 0, 0, 0, 0xc3], 0x1000).unwrap(),
            0x1000,
            noop,
        )
        .unwrap();
        assert!(matches!(
            arena.allocate(&unreachable),
            Err(TrampolineError::NoReachableMapping)
        ));
        let addresses = allocate(&arena, &plans[0], 2);
        assert_ne!(addresses[0], addresses[1]);
        for _ in 0..32 {
            assert!(matches!(
                arena.allocate(&plans[0]),
                Err(TrampolineError::ArenaFull)
            ));
        }
        for address in addresses {
            check_image(&plans[0], address, 11);
        }
        println!(
            "arena capacity: zero and overflow rejected, reach checked, exact two slots preserved after exhaustion"
        );
    }
    fn fd_count() -> usize {
        std::fs::read_dir("/proc/self/fd").unwrap().count()
    }
    fn fd_snapshot() -> BTreeMap<i32, (u64, u64)> {
        let numbers: Vec<i32> = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .parse()
                    .unwrap()
            })
            .collect();
        numbers
            .into_iter()
            .filter_map(|fd| {
                let mut stat: libc::stat = unsafe { std::mem::zeroed() };
                (unsafe { libc::fstat(fd, &raw mut stat) } == 0)
                    .then_some((fd, (stat.st_dev, stat.st_ino)))
            })
            .collect()
    }
    fn aliases() -> Vec<String> {
        std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .filter(|line| line.contains("/memfd:liteinst2-trampoline"))
            .map(str::to_owned)
            .collect()
    }
    fn reject_metadata_mmap() {
        // Only reject this new page-sized, shared anonymous mmap. Existing
        // memfd-backed code aliases and ordinary allocator mappings still work.
        let stmt = |code, k| libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k,
        };
        let jump = |k, jf| libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf,
            k,
        };
        let filter = [
            stmt(0x20, 0),
            jump(libc::SYS_mmap as u32, 5),
            stmt(0x20, 40),
            jump((libc::MAP_SHARED | libc::MAP_ANONYMOUS) as u32, 3),
            stmt(0x20, 24),
            jump(PAGE as u32, 1),
            stmt(0x06, libc::SECCOMP_RET_ERRNO | libc::ENOMEM as u32),
            stmt(0x06, libc::SECCOMP_RET_ALLOW),
        ];
        let program = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_ptr().cast_mut(),
        };
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
            0
        );
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) },
            0
        );
    }
    fn construction_failure(mode: &str) {
        let plans = plans();
        let before_fds = fd_count();
        let before_descriptors = fd_snapshot();
        let before_aliases = aliases();
        if mode == "mmap-error" {
            reject_metadata_mmap();
        }
        TRACK.store(true, Ordering::Relaxed);
        CLOSE_ERROR.store(mode == "close-error", Ordering::Relaxed);
        let result = TrampolineArena::allocate_near(plans[0].execute_address(), 2);
        assert!(
            result.is_err(),
            "requested constructor failure did not occur"
        );
        let error = result.err().unwrap();
        let original_fd = ORIGINAL_FD.load(Ordering::Relaxed);
        assert!(original_fd >= 0);
        assert!(RX_BACKING_MATCHES.load(Ordering::Relaxed));
        assert_ne!(RW.load(Ordering::Relaxed), 0);
        assert_ne!(RX.load(Ordering::Relaxed), 0);
        assert_ne!(RW.load(Ordering::Relaxed), RX.load(Ordering::Relaxed));
        assert_eq!(RW_UNMAPS.load(Ordering::Relaxed), 1);
        assert_eq!(RX_UNMAPS.load(Ordering::Relaxed), 1);
        assert!(RW_ABSENT.load(Ordering::Relaxed));
        assert!(RX_ABSENT.load(Ordering::Relaxed));
        if mode == "mmap-error" {
            assert!(
                matches!(
                    error,
                    TrampolineError::BackingStore {
                        operation: "mmap shared arena reservation",
                        errno: libc::ENOMEM
                    }
                ),
                "wrong constructor failure: {error}"
            );
            assert_eq!(ORIGINAL_CLOSES.load(Ordering::Relaxed), 1);
            assert!(ORIGINAL_CLOSED.load(Ordering::Relaxed));
            assert_eq!(META.load(Ordering::Relaxed), 0);
            assert_eq!(unsafe { libc::fcntl(original_fd, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            assert_eq!(fd_count(), before_fds);
        } else {
            assert!(
                matches!(
                    error,
                    TrampolineError::BackingStore {
                        operation: "close trampoline arena memfd",
                        errno: libc::EIO
                    }
                ),
                "wrong constructor failure: {error}"
            );
            assert_eq!(REAL_CLOSE_RESULT.load(Ordering::Relaxed), 0);
            assert_eq!(
                META_PROT.load(Ordering::Relaxed),
                libc::PROT_READ | libc::PROT_WRITE,
                "arena metadata must remain non-executable"
            );
            let replacement = REPLACEMENT.load(Ordering::Relaxed);
            assert_eq!(replacement, original_fd);
            assert!(replacement >= 0);
            assert!(
                unsafe { libc::fcntl(replacement, libc::F_GETFD) } >= 0,
                "constructor cleanup closed a reused descriptor number"
            );
            assert_eq!(ORIGINAL_CLOSES.load(Ordering::Relaxed), 1);
            assert!(ORIGINAL_CLOSED.load(Ordering::Relaxed));
            assert_eq!(META_UNMAPS.load(Ordering::Relaxed), 1);
            assert!(
                META_ABSENT.load(Ordering::Relaxed),
                "metadata page survived failed construction"
            );
            assert_eq!(fd_count(), before_fds + 1);
            assert_eq!(unsafe { libc::close(replacement) }, 0);
            assert_eq!(fd_count(), before_fds);
        }
        assert_eq!(fd_snapshot(), before_descriptors);
        assert_eq!(
            aliases(),
            before_aliases,
            "failed arena leaked code aliases"
        );
        println!(
            "arena {mode}: exact error, pending aliases and metadata released, descriptor ownership preserved; RW {:#x}..{:#x}, RX {:#x}..{:#x}, metadata {:#x}, fd {}, backing {}:{}",
            RW.load(Ordering::Relaxed),
            RW.load(Ordering::Relaxed) + 2 * PAGE,
            RX.load(Ordering::Relaxed),
            RX.load(Ordering::Relaxed) + 2 * PAGE,
            META.load(Ordering::Relaxed),
            original_fd,
            BACKING_DEV.load(Ordering::Relaxed),
            BACKING_INO.load(Ordering::Relaxed),
        );
    }
    pub(super) fn run() {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
        assert_single_thread();
        assert_eq!(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }, PAGE as i64);
        let mode = std::env::args().nth(1).expect("arena fixture mode");
        match mode.as_str() {
            "parent-first" | "child-first" | "concurrent" => fork_case(&mode),
            "capacity" => capacity(),
            "mmap-error" | "close-error" => construction_failure(&mode),
            _ => panic!("unknown arena fixture mode {mode}"),
        }
        // Every child created by this main has been reaped, rather than merely
        // reaching a logical channel-completion marker.
        assert_eq!(
            unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
