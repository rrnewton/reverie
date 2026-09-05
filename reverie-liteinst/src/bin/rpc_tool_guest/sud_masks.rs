use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;
use std::path::Path;

use reverie::Error;
use reverie::Guest;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie::syscalls::Sysno;
use reverie_liteinst::SyscallMode;
use reverie_preload::trap::raw_syscall6;

static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static GUEST_MASK: AtomicU64 = AtomicU64::new(0);

fn kernel_mask() -> u64 {
    let mut mask = 0u64;
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [0, 0, (&raw mut mask) as u64, 8, 0, 0],
            )
        },
        0
    );
    mask
}

fn assert_runtime_mask() {
    let unmaskable = (1u64 << (libc::SIGKILL - 1)) | (1u64 << (libc::SIGSTOP - 1));
    assert_eq!(kernel_mask(), !(unmaskable | (1u64 << (libc::SIGSYS - 1))));
}

#[derive(Default)]
struct MaskTool;

#[reverie::tool]
impl Tool for MaskTool {
    type GlobalState = super::CounterGlobal;
    type ThreadState = ();

    fn subscriptions(_: &()) -> Subscription {
        [Sysno::rt_sigprocmask, Sysno::getgid, Sysno::getpid]
            .into_iter()
            .collect()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        let count = CALLBACKS.fetch_add(1, Ordering::Relaxed) + 1;
        assert_runtime_mask();
        assert_eq!(
            reverie_preload::user_dispatch::dispatch_mask(),
            Some(GUEST_MASK.load(Ordering::Relaxed))
        );
        let nested = unsafe { libc::syscall(libc::SYS_getuid) };
        assert!(nested >= 0);
        assert_eq!(CALLBACKS.load(Ordering::Relaxed), count);
        assert_runtime_mask();
        let (number, args) = syscall.into_parts();
        if number == Sysno::getgid {
            guest
                .tail_inject(Syscall::from_raw(Sysno::rt_sigprocmask, args))
                .await
        }
        if number == Sysno::getpid {
            return Ok(count as i64);
        }
        let result = guest.inject(Syscall::from_raw(number, args)).await;
        assert_runtime_mask();
        assert_eq!(CALLBACKS.load(Ordering::Relaxed), count);
        Ok(result?)
    }
}

pub(super) fn run(path: &Path) {
    let mask = kernel_mask() | (1u64 << (libc::SIGUSR1 - 1)) | (1u64 << (libc::SIGALRM - 1));
    assert_eq!(
        unsafe {
            raw_syscall6(
                libc::SYS_rt_sigprocmask,
                [
                    libc::SIG_SETMASK as u64,
                    (&raw const mask) as u64,
                    0,
                    8,
                    0,
                    0,
                ],
            )
        },
        0
    );
    GUEST_MASK.store(mask, Ordering::Relaxed);
    unsafe {
        reverie_liteinst::install_tool_with_mode::<MaskTool>(
            path,
            SyscallMode::UserDispatchWithoutPatching,
        )
    }
    .unwrap();
    let mut observed = 0u64;
    for number in [libc::SYS_rt_sigprocmask, libc::SYS_getgid] {
        assert_eq!(
            unsafe { libc::syscall(number, 123i64, 0u64, &raw mut observed, 8u64) },
            0
        );
        assert_eq!(observed, mask);
        assert_eq!(kernel_mask(), mask);
        assert_eq!(
            unsafe { libc::syscall(number, 0u64, 0u64, &raw mut observed, 7u64) },
            -1
        );
        assert_eq!(unsafe { *libc::__errno_location() }, libc::EINVAL);
        assert_eq!(kernel_mask(), mask);
        assert_eq!(unsafe { libc::syscall(number, 0u64, 0u64, 1u64, 8u64) }, -1);
        assert_eq!(unsafe { *libc::__errno_location() }, libc::EFAULT);
        assert_eq!(kernel_mask(), mask);
    }
    assert_eq!(
        unsafe {
            libc::syscall(
                libc::SYS_rt_sigprocmask,
                libc::SIG_SETMASK as u64,
                1u64,
                0u64,
                8u64,
            )
        },
        -1
    );
    assert_eq!(unsafe { *libc::__errno_location() }, libc::EPERM);
    assert_eq!(kernel_mask(), mask);
    assert_eq!(unsafe { libc::syscall(libc::SYS_getpid) }, 7);
    assert_eq!(CALLBACKS.load(Ordering::Relaxed), 7);
    let stats = reverie_liteinst::syscall_mode_stats();
    assert_eq!(
        (
            stats.planning_attempts,
            stats.patch_attempts,
            stats.installed_patches,
            stats.vdso_rewrite_attempts
        ),
        (0, 0, 0, 0)
    );
    println!("sud-masks: inject/tail/query/error/restore callbacks=7 patches=0");
}

unsafe extern "C" fn reject_source(_: i32, _: *mut libc::siginfo_t, _: *mut libc::c_void) -> bool {
    false
}

unsafe extern "C" fn unreachable_body(
    _: i32,
    _: *mut libc::siginfo_t,
    _: *mut libc::c_void,
) -> reverie_preload::clock_boundary::Continuation {
    unsafe { raw_syscall6(libc::SYS_exit_group, [124, 0, 0, 0, 0, 0]) };
    std::process::abort();
}

extern "C" fn ordinary_handler(_: i32) {}

static SOURCES: [reverie_preload::signal::RuntimeSignal; 1] =
    [reverie_preload::signal::RuntimeSignal {
        signal: libc::SIGUSR2,
        disable_descriptor: None,
        validate: reject_source,
        body: unreachable_body,
    }];

pub(super) fn policy_probe(path: &Path, mode: &str) {
    if mode == "sud-policy-blocked" {
        let mask = kernel_mask() | (1u64 << (libc::SIGUSR2 - 1));
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const mask) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        let error =
            unsafe { reverie_preload::signal::configure_runtime_signals(&SOURCES) }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(kernel_mask(), mask);
    } else if mode == "sud-policy-ignored" {
        assert_ne!(
            unsafe { libc::signal(libc::SIGUSR2, libc::SIG_IGN) },
            libc::SIG_ERR
        );
        let mask = kernel_mask();
        let error =
            unsafe { reverie_preload::signal::configure_runtime_signals(&SOURCES) }.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(kernel_mask(), mask);
        assert_eq!(
            unsafe { libc::signal(libc::SIGUSR2, libc::SIG_IGN) },
            libc::SIG_IGN
        );
    } else {
        unsafe { reverie_preload::signal::configure_runtime_signals(&SOURCES) }.unwrap();
        let mask = kernel_mask();
        if mode == "sud-policy-ordinary" {
            assert_ne!(
                unsafe { libc::signal(libc::SIGUSR1, ordinary_handler as *const () as usize) },
                libc::SIG_ERR
            );
            let error = unsafe {
                reverie_liteinst::install_tool_with_mode::<MaskTool>(
                    path,
                    SyscallMode::UserDispatchWithoutPatching,
                )
            }
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
            assert_eq!(kernel_mask(), mask);
            assert_eq!(
                unsafe { libc::signal(libc::SIGUSR1, ordinary_handler as *const () as usize) },
                ordinary_handler as *const () as usize
            );
        } else {
            assert_eq!(mode, "sud-policy-unknown");
            let pid = unsafe { raw_syscall6(libc::SYS_getpid, [0; 6]) } as u64;
            let tid = unsafe { raw_syscall6(libc::SYS_gettid, [0; 6]) } as u64;
            unsafe {
                reverie_liteinst::install_tool_with_mode::<MaskTool>(
                    path,
                    SyscallMode::UserDispatchWithoutPatching,
                )
            }
            .unwrap();
            unsafe { raw_syscall6(libc::SYS_tgkill, [pid, tid, libc::SIGUSR2 as u64, 0, 0, 0]) };
            panic!("unknown source was accepted");
        }
    }
    println!("{mode}: refused, original mask/disposition preserved");
}
