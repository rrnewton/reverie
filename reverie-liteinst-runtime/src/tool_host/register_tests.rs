use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use super::*;

static CHANGE_STACK: AtomicBool = AtomicBool::new(false);
static ACCEPTED: AtomicBool = AtomicBool::new(false);
static UNCHANGED: AtomicBool = AtomicBool::new(false);
static EVENT_UNCHANGED: AtomicBool = AtomicBool::new(false);
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct RegisterTool;

#[reverie::tool]
impl Tool for RegisterTool {
    type GlobalState = ();
    type ThreadState = ();

    fn subscriptions(_: &()) -> reverie::Subscription {
        reverie::Subscription::none()
            .syscalls([Sysno::getpid])
            .clone()
    }

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        _: reverie::syscalls::Syscall,
    ) -> Result<i64, Error> {
        CALLBACKS.fetch_add(1, Ordering::Relaxed);
        let original = guest.regs().await;
        let mut requested = original;
        requested.r12 = 99;
        if CHANGE_STACK.load(Ordering::Relaxed) {
            requested.rsp -= 16;
            requested.rax = libc::SYS_getuid as u64;
            requested.rdi = 123;
            let mut context: HookContext = unsafe { core::mem::zeroed() };
            context.stack_pointer = original.rsp;
            let original_context = format!("{context:?}");
            let mut event = SyscallEvent {
                owned_binding: None,
                exit: None,
                number: libc::SYS_getpid,
                args: [1, 2, 3, 4, 5, 6],
                instruction_pointer: original.rip,
                result: 42,
                context: (&raw mut context) as usize,
            };
            let refusal = set_guest_registers(&mut event, requested, false);
            EVENT_UNCHANGED.store(
                matches!(refusal, Err(Error::Errno(Errno::EOPNOTSUPP)))
                    && format!("{context:?}") == original_context
                    && event.number == libc::SYS_getpid
                    && event.args == [1, 2, 3, 4, 5, 6]
                    && event.instruction_pointer == original.rip
                    && event.result == 42
                    && event.context == (&raw mut context) as usize
                    && event.owned_binding.is_none()
                    && event.exit.is_none(),
                Ordering::Relaxed,
            );
        }
        let result = guest.set_regs(requested).await;
        ACCEPTED.store(result.is_ok(), Ordering::Relaxed);
        if let Err(error) = result {
            assert!(matches!(error, Error::Errno(Errno::EOPNOTSUPP)));
        }
        let after = guest.regs().await;
        UNCHANGED.store(
            format!("{original:?}") == format!("{after:?}"),
            Ordering::Relaxed,
        );
        Ok(4242)
    }
}

struct Child(std::process::Child);

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Isolated children reset libtest handlers before public installation and exit
/// through the trusted gate after all assertions, without libtest TLS teardown.
/// The spare return address keeps execution safe even if an RSP update succeeds.
#[test]
fn deferred_register_updates_execute_or_refuse_atomically() {
    const SOCKET: &str = "REVERIE_REGISTER_TEST_SOCKET";
    const CASE: &str = "REVERIE_REGISTER_TEST_CASE";
    if let Some(socket) = std::env::var_os(SOCKET) {
        let case = std::env::var(CASE).unwrap();
        let change_stack = case.ends_with("rsp");
        let sud = case.starts_with("sud");
        CHANGE_STACK.store(change_stack, Ordering::Relaxed);
        for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGPIPE, 33] {
            let action = [libc::SIG_DFL as u64, 0, 0, 0];
            assert_eq!(
                unsafe {
                    raw_syscall6(
                        libc::SYS_rt_sigaction,
                        [signal as u64, action.as_ptr() as u64, 0, 8, 0, 0],
                    )
                },
                0
            );
        }
        assert!(!crate::clock_control::requested());
        assert!(!crate::clock_control::active());
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
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
        let site = unsafe { mapping.cast::<u8>().add(page - 3) };
        unsafe { std::ptr::copy_nonoverlapping([0x0f, 0x05, 0xc3].as_ptr(), site, 3) };
        assert_eq!(
            unsafe { libc::mprotect(mapping, page, libc::PROT_READ | libc::PROT_EXEC) },
            0
        );
        let guest_mask = 1_u64 << (libc::SIGUSR1 - 1);
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [
                        libc::SIG_SETMASK as u64,
                        (&raw const guest_mask) as u64,
                        0,
                        8,
                        0,
                        0,
                    ],
                )
            },
            0
        );
        unsafe {
            install_tool_with_mode::<RegisterTool>(
                socket,
                if sud {
                    crate::SyscallMode::UserDispatchWithoutPatching
                } else {
                    crate::SyscallMode::SeccompWithPatching
                },
            )
        }
        .unwrap();
        let original_stack: u64;
        let resumed_stack: u64;
        let resumed_register: u64;
        let result: u64;
        unsafe {
            core::arch::asm!(
                "mov r15, rsp",
                "lea r14, [rip + 2f]",
                "mov [rsp - 24], r14",
                "call r13",
                "2:",
                "mov rdx, rsp",
                "mov rsp, r15",
                "mov rsi, r15",
                inlateout("rax") libc::SYS_getpid as u64 => result,
                inlateout("r12") 7_u64 => resumed_register,
                inlateout("r13") site => _,
                lateout("r14") _,
                lateout("r15") _,
                lateout("rdx") resumed_stack,
                lateout("rsi") original_stack,
                inlateout("rdi") 0_u64 => _,
                lateout("rcx") _,
                lateout("r11") _,
            );
        }
        assert_eq!(CALLBACKS.load(Ordering::Relaxed), 1);
        assert_eq!(result, 4242);
        if sud {
            assert!(crate::syscall_mode_stats().deferred_sud > 0);
        } else {
            assert_eq!(crate::reverie_liteinst_site_hook_count(site as u64), 0);
            assert_eq!(crate::reverie_liteinst_site_trap_count(site as u64), 1);
        }
        assert_eq!(
            unsafe { std::slice::from_raw_parts(site, 3) },
            [0x0f, 0x05, 0xc3]
        );
        let accepted = ACCEPTED.load(Ordering::Relaxed);
        assert_eq!(
            resumed_stack,
            original_stack - if change_stack && accepted { 16 } else { 0 },
            "accepted RSP update was ignored by the executed continuation"
        );
        assert_eq!(accepted, !change_stack);
        assert_eq!(resumed_register, if change_stack { 7 } else { 99 });
        assert_eq!(UNCHANGED.load(Ordering::Relaxed), change_stack);
        if change_stack {
            assert!(EVENT_UNCHANGED.load(Ordering::Relaxed));
        }
        assert!(!crate::clock_control::active());
        let mut resumed_mask = 0_u64;
        assert_eq!(
            unsafe {
                raw_syscall6(
                    libc::SYS_rt_sigprocmask,
                    [0, 0, (&raw mut resumed_mask) as u64, 8, 0, 0],
                )
            },
            0
        );
        assert_eq!(resumed_mask, guest_mask);
        println!("executed continuation checks passed");
        unsafe { raw_syscall6(libc::SYS_exit_group, [0; 6]) };
        unreachable!();
    }

    for case in [
        "sud-supported",
        "sud-rsp",
        "seccomp-supported",
        "seccomp-rsp",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("rpc.sock");
        let server_path = socket.clone();
        let (ready, wait) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let server =
                    reverie_rpc_transport::RpcServer::bind(server_path, Arc::new(()), ()).unwrap();
                ready.send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(20), server.serve_one()).await
            })
        });
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        let stdout = directory.path().join("stdout");
        let stderr = directory.path().join("stderr");
        let mut child = Child(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tool_host::register_tests::deferred_register_updates_execute_or_refuse_atomically", "--nocapture"])
            .env(SOCKET, socket).env(CASE, case)
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(std::fs::File::create(&stderr).unwrap())
            .spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "{case}: continuation child timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(child);
        assert!(
            status.success(),
            "{case}: {status}\n{}\n{}",
            std::fs::read_to_string(&stdout).unwrap(),
            std::fs::read_to_string(stderr).unwrap()
        );
        assert!(
            std::fs::read_to_string(stdout)
                .unwrap()
                .contains("executed continuation checks passed")
        );
        server.join().unwrap().unwrap().unwrap();
    }
}
