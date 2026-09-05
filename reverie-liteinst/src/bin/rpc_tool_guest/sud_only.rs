use core::sync::atomic::AtomicBool;
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::path::Path;

use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::TimerSchedule;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Fork;
use reverie::syscalls::Getpid;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::SyscallMode;
use reverie_liteinst::syscall_mode_stats;
use reverie_preload::trap::raw_syscall6;

static ABI_PROBE: AtomicBool = AtomicBool::new(false);
static ORIGINAL_PID: AtomicU64 = AtomicU64::new(0);
static EXPECTED_SITE: AtomicU64 = AtomicU64::new(0);
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static TIMER_REFUSALS: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct RawTool;

#[reverie::tool]
impl Tool for RawTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = u32;

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::getpid, Sysno::getuid, Sysno::getgid, Sysno::getppid]
            .into_iter()
            .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let previous_callbacks = CALLBACKS.fetch_add(1, Ordering::Relaxed);
        if previous_callbacks == 0 {
            for precise in [false, true] {
                for schedule in [
                    TimerSchedule::Time(std::time::Duration::from_nanos(1)),
                    TimerSchedule::Rcbs(1),
                    TimerSchedule::RcbsAndInstructions(1, 1),
                ] {
                    let result = if precise {
                        guest.set_timer_precise(schedule)
                    } else {
                        guest.set_timer(schedule)
                    };
                    assert!(matches!(result, Err(Error::Errno(Errno::EOPNOTSUPP))));
                    TIMER_REFUSALS.fetch_add(1, Ordering::Relaxed);
                }
            }
            assert_eq!(guest.inject(Fork::new()).await, Err(Errno::EOPNOTSUPP));
        }
        let (number, args) = syscall.into_parts();
        if ABI_PROBE.load(Ordering::Relaxed) {
            assert_eq!(
                [
                    args.arg0, args.arg1, args.arg2, args.arg3, args.arg4, args.arg5
                ],
                [11, 22, 33, 44, 55, 66]
            );
            let registers = guest.regs().await;
            assert_eq!(registers.rip, EXPECTED_SITE.load(Ordering::Relaxed));
            assert_ne!(registers.rsp, 0);
        }
        let before = CALLBACKS.load(Ordering::Relaxed);
        assert_eq!(
            unsafe { libc::getpid() } as u64,
            ORIGINAL_PID.load(Ordering::Relaxed)
        );
        assert_eq!(CALLBACKS.load(Ordering::Relaxed), before);
        let (total, senders) = guest.send_rpc(1).await;
        super::LAST_TOTAL.store(total, Ordering::Relaxed);
        super::LAST_SENDERS.store(senders, Ordering::Relaxed);
        unsafe {
            *libc::__errno_location() = libc::EINVAL;
            core::arch::asm!("pxor xmm0, xmm0", out("xmm0") _, options(nostack, preserves_flags));
        }
        match number {
            Sysno::getpid => Ok(424_242),
            Sysno::getuid => Err(Errno::EPERM.into()),
            Sysno::getgid => guest.tail_inject(Getpid::new()).await,
            Sysno::getppid => {
                *guest.thread_state_mut() += 1;
                if *guest.thread_state() == 1 {
                    Err(Errno::ERESTARTSYS.into())
                } else {
                    Ok(777_777)
                }
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Default)]
struct InstructionTool;

#[reverie::tool]
impl Tool for InstructionTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ();
    fn subscriptions(_: &()) -> Subscription {
        let mut subscriptions = Subscription::default();
        subscriptions.rdtsc();
        subscriptions
    }
}

#[derive(Default)]
struct VdsoTool;

#[reverie::tool]
impl Tool for VdsoTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ();
    fn subscriptions(_: &()) -> Subscription {
        [Sysno::clock_gettime].into_iter().collect()
    }
}

#[unsafe(naked)]
unsafe extern "C" fn text_site() -> i64 {
    core::arch::naked_asm!("syscall", "ret");
}

fn deny_ptrace() {
    let mut instructions = [
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: libc::SYS_ptrace as u32,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_KILL_PROCESS,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let filter = libc::sock_fprog {
        len: instructions.len() as u16,
        filter: instructions.as_mut_ptr(),
    };
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_prctl,
                [libc::PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0],
            )
        },
        0
    );
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_seccomp,
                [
                    libc::SECCOMP_SET_MODE_FILTER as u64,
                    0,
                    (&raw const filter) as u64,
                    0,
                    0,
                    0,
                ],
            )
        },
        0
    );
}

fn mapped_site(offset: usize, pages: usize) -> usize {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            page * pages,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let site = unsafe { mapping.cast::<u8>().add(offset) };
    unsafe { std::ptr::copy_nonoverlapping([0x0f, 0x05, 0xc3].as_ptr(), site, 3) };
    assert_eq!(
        unsafe { libc::mprotect(mapping, page * pages, libc::PROT_READ | libc::PROT_EXEC) },
        0
    );
    site as usize
}

fn snapshot(address: usize, length: usize) -> Vec<u8> {
    unsafe { std::slice::from_raw_parts(address as *const u8, length) }.to_vec()
}

fn vdso_range() -> (usize, usize) {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let line = maps.lines().find(|line| line.ends_with("[vdso]")).unwrap();
    let (start, end) = line
        .split_whitespace()
        .next()
        .unwrap()
        .split_once('-')
        .unwrap();
    let start = usize::from_str_radix(start, 16).unwrap();
    let end = usize::from_str_radix(end, 16).unwrap();
    assert!(end - start <= 65536);
    (start, end - start)
}

fn probe(site: usize, number: i64, expected: i64) {
    ABI_PROBE.store(true, Ordering::Relaxed);
    EXPECTED_SITE.store(site as u64, Ordering::Relaxed);
    let before = syscall_mode_stats();
    assert_eq!(
        unsafe { super::syscall_fallback_guest::fallback_test_call(site, number) },
        expected
    );
    let after = syscall_mode_stats();
    assert_eq!(after.deferred_sud - before.deferred_sud, 1);
    assert!(after.validated_sud > before.validated_sud);
    assert_eq!(unsafe { *libc::__errno_location() }, libc::E2BIG);
}

extern "C" fn unused_handler(_: i32) {}

pub(super) fn run(path: &Path, mode: &str) {
    deny_ptrace();
    if mode == "sud-ptrace-control" {
        unsafe { raw_syscall6(libc::SYS_ptrace, [0; 6]) };
        panic!("ptrace denial failed");
    }
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        assert_ne!(
            unsafe { libc::signal(signal, libc::SIG_DFL) },
            libc::SIG_ERR
        );
    }
    if mode == "sud-clock" {
        let prior = unsafe { reverie_liteinst::__clock_constructor_begin() };
        let result = unsafe {
            reverie_liteinst::install_tool_with_mode::<RawTool>(
                path,
                SyscallMode::UserDispatchWithoutPatching,
            )
        };
        unsafe { reverie_liteinst::__clock_constructor_finish(0, prior) };
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        println!("sud-clock: refused");
        return;
    }
    if mode == "sud-handler" {
        assert_ne!(
            unsafe { libc::signal(libc::SIGUSR1, unused_handler as *const () as usize) },
            libc::SIG_ERR
        );
        let error = unsafe {
            reverie_liteinst::install_tool_with_mode::<RawTool>(
                path,
                SyscallMode::UserDispatchWithoutPatching,
            )
        }
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        println!("sud-handler: refused");
        return;
    }
    if mode == "sud-instructions" || mode == "sud-vdso" {
        let result = if mode == "sud-instructions" {
            unsafe {
                reverie_liteinst::install_tool_with_mode::<InstructionTool>(
                    path,
                    SyscallMode::UserDispatchWithoutPatching,
                )
            }
        } else {
            unsafe {
                reverie_liteinst::install_tool_with_mode::<VdsoTool>(
                    path,
                    SyscallMode::UserDispatchWithoutPatching,
                )
            }
        };
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        println!("{mode}: refused");
        return;
    }
    ORIGINAL_PID.store(unsafe { libc::getpid() } as u64, Ordering::Relaxed);
    let text = text_site as *const () as usize;
    let libc_site = libc::getpid as *const () as usize;
    let (vdso, vdso_size) = vdso_range();
    let text_bytes = snapshot(text, 3);
    let libc_bytes = snapshot(libc_site, 64);
    let vdso_bytes = snapshot(vdso, vdso_size);
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page_end = mapped_site(page - 2, 2);
    let page_bytes = snapshot(page_end - (page - 2), 2 * page);
    unsafe {
        reverie_liteinst::install_tool_with_mode::<RawTool>(
            path,
            SyscallMode::UserDispatchWithoutPatching,
        )
    }
    .unwrap();
    if mode == "sud-x32" {
        unsafe { libc::syscall(0x4000_0000 | libc::SYS_getpid) };
        panic!("x32 was decoded as native");
    }
    if mode == "sud-compat" {
        unsafe {
            core::arch::asm!("int 0x80", inout("eax") 20u32 => _, options(nostack));
        }
        panic!("compat was decoded as native");
    }
    assert_eq!(mode, "sud-only");
    assert_eq!(unsafe { libc::syscall(-1i64) }, -1);
    assert_eq!(unsafe { *libc::__errno_location() }, libc::ENOSYS);
    let anonymous = mapped_site(32, 1);
    let anonymous_bytes = snapshot(anonymous - 32, page);
    unsafe { *libc::__errno_location() = libc::E2BIG };
    for site in [text, page_end, anonymous] {
        for _ in 0..3 {
            probe(site, libc::SYS_getpid, 424_242);
        }
    }
    ABI_PROBE.store(false, Ordering::Relaxed);
    for _ in 0..3 {
        let before = syscall_mode_stats();
        assert_eq!(unsafe { libc::getpid() }, 424_242);
        let after = syscall_mode_stats();
        assert_eq!(after.deferred_sud - before.deferred_sud, 1);
        assert!(after.validated_sud > before.validated_sud);
        assert_eq!(unsafe { *libc::__errno_location() }, libc::E2BIG);
    }
    probe(text, libc::SYS_getuid, -i64::from(libc::EPERM));
    probe(
        text,
        libc::SYS_getgid,
        ORIGINAL_PID.load(Ordering::Relaxed) as i64,
    );
    probe(text, libc::SYS_getppid, 777_777);
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), 16);
    assert_eq!(super::LAST_TOTAL.load(Ordering::Relaxed), 16);
    assert_eq!(super::LAST_SENDERS.load(Ordering::Relaxed), 1);
    assert_eq!(TIMER_REFUSALS.load(Ordering::Relaxed), 6);
    assert_eq!(snapshot(text, 3), text_bytes);
    assert_eq!(snapshot(libc_site, 64), libc_bytes);
    assert_eq!(snapshot(vdso, vdso_size), vdso_bytes);
    assert_eq!(snapshot(page_end - (page - 2), page * 2), page_bytes);
    assert_eq!(snapshot(anonymous - 32, page), anonymous_bytes);
    let stats = syscall_mode_stats();
    assert_eq!(
        (
            stats.planning_attempts,
            stats.patch_attempts,
            stats.installed_patches,
            stats.vdso_rewrite_attempts
        ),
        (0, 0, 0, 0)
    );
    println!(
        "sud-only: guest_probes=15 rpc=16 bytes=unchanged abi=preserved timer_requests_refused=6 injected_fork=refused {stats:?}"
    );
}
