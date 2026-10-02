// Actual FunctionGuest fork-return signal controls. No forced stop or siginfo.
#[cfg(target_arch = "x86_64")]
mod parent_step_native_tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicI32;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;

    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    enum Observation {
        Start(i32, bool),
        Trap(i32, bool, i32, i32),
        Exit(i32, bool, ExitStatus),
    }

    #[derive(Default)]
    struct Log(Mutex<Vec<Observation>>);

    #[reverie::global_tool]
    impl GlobalTool for Log {
        type Config = ();
        type Request = Observation;
        type Response = ();

        async fn receive_rpc(&self, _: Pid, event: Observation) {
            self.0.lock().unwrap().push(event);
        }
    }

    #[derive(Default)]
    struct SignalTool;

    #[reverie::tool]
    impl Tool for SignalTool {
        type GlobalState = Log;
        type ThreadState = bool;

        fn subscriptions(_: &()) -> Subscription {
            // This is the legacy FunctionGuest path, not the Command observer.
            Subscription::none()
        }

        fn init_thread_state(&self, _: Pid, parent: Option<(Pid, &bool)>) -> bool {
            parent.is_some()
        }

        async fn handle_thread_start<G: Guest<Self>>(&self, guest: &mut G) -> Result<(), Error> {
            guest
                .send_rpc(Observation::Start(
                    guest.tid().as_raw(),
                    *guest.thread_state(),
                ))
                .await;
            Ok(())
        }

        async fn handle_signal_event<G: Guest<Self>>(
            &self,
            guest: &mut G,
            signal: Signal,
        ) -> Result<Option<Signal>, Errno> {
            if signal == Signal::SIGTRAP {
                let info = nix::sys::ptrace::getsiginfo(guest.tid().into())
                    .map_err(|error| Errno::new(error as i32))?;
                guest
                    .send_rpc(Observation::Trap(
                        guest.tid().as_raw(),
                        *guest.thread_state(),
                        info.si_code,
                        unsafe { info.si_pid() },
                    ))
                    .await;
            }
            // Never suppress the signal to make the control pass.
            Ok(Some(signal))
        }

        async fn on_exit_thread<G: reverie::GlobalRPC<Self::GlobalState>>(
            &self,
            tid: Pid,
            global: &G,
            child: bool,
            status: ExitStatus,
        ) -> Result<(), Error> {
            global
                .send_rpc(Observation::Exit(tid.as_raw(), child, status))
                .await;
            Ok(())
        }
    }

    // Each fork has private COW copies. The handler uses only lock-free atomics.
    static HANDLED: AtomicUsize = AtomicUsize::new(0);
    static LAST_CODE: AtomicI32 = AtomicI32::new(0);
    static LAST_SENDER: AtomicI32 = AtomicI32::new(0);
    static BAD_FRAME: AtomicBool = AtomicBool::new(false);

    extern "C" fn trap_handler(
        signal: libc::c_int,
        info: *mut libc::siginfo_t,
        context: *mut libc::c_void,
    ) {
        if signal != libc::SIGTRAP || info.is_null() || context.is_null() {
            BAD_FRAME.store(true, Ordering::SeqCst);
            return;
        }
        let (code, sender) = unsafe { ((*info).si_code, (*info).si_pid()) };
        if code == libc::SI_KERNEL {
            let rip = unsafe {
                (*(context.cast::<libc::ucontext_t>())).uc_mcontext.gregs[libc::REG_RIP as usize]
                    as usize
            };
            // An actual INT3 leaves the saved PC after its one-byte opcode.
            if rip == 0 || unsafe { ((rip - 1) as *const u8).read() } != 0xcc {
                BAD_FRAME.store(true, Ordering::SeqCst);
            }
        }
        LAST_CODE.store(code, Ordering::SeqCst);
        LAST_SENDER.store(sender, Ordering::SeqCst);
        HANDLED.fetch_add(1, Ordering::SeqCst);
    }

    fn install_guest_handlers() {
        HANDLED.store(0, Ordering::SeqCst);
        LAST_CODE.store(0, Ordering::SeqCst);
        LAST_SENDER.store(0, Ordering::SeqCst);
        BAD_FRAME.store(false, Ordering::SeqCst);
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = trap_handler as *const () as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
            assert_eq!(
                libc::sigaction(libc::SIGTRAP, &action, std::ptr::null_mut()),
                0
            );
            // Only the isolated guest's dispositions and mask are changed.
            action.sa_sigaction = libc::SIG_DFL;
            action.sa_flags = 0;
            assert_eq!(
                libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()),
                0
            );
            let mut set: libc::sigset_t = std::mem::zeroed();
            assert_eq!(libc::sigemptyset(&mut set), 0);
            assert_eq!(libc::sigaddset(&mut set, libc::SIGTRAP), 0);
            assert_eq!(
                libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()),
                0
            );
        }
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Plain,
        NextInt3,
        ChildTgkill,
    }

    const MAGIC: i32 = 0x5053_5450;
    const REPORT_BYTES: usize = 8 * std::mem::size_of::<i32>();

    fn emit_report(child: bool, wait_status: i32) {
        let words = [
            MAGIC,
            i32::from(child),
            unsafe { libc::getpid() },
            i32::try_from(HANDLED.load(Ordering::SeqCst)).unwrap(),
            LAST_CODE.load(Ordering::SeqCst),
            LAST_SENDER.load(Ordering::SeqCst),
            i32::from(BAD_FRAME.load(Ordering::SeqCst)),
            wait_status,
        ];
        let mut bytes = [0u8; REPORT_BYTES];
        let (slots, remainder) = bytes.as_chunks_mut::<4>();
        assert!(remainder.is_empty());
        for (slot, word) in slots.iter_mut().zip(words) {
            slot.copy_from_slice(&word.to_ne_bytes());
        }
        // One short write per role, below PIPE_BUF, with no handler I/O.
        assert_eq!(
            unsafe { libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len()) },
            bytes.len() as isize,
        );
    }

    fn guest_body(mode: Mode) {
        install_guest_handlers();
        let parent = unsafe { libc::getpid() };
        let child: i64;
        if matches!(mode, Mode::NextInt3) {
            // No compiler instruction can occur between fork and this INT3.
            // Both parent and child execute the same next user instruction.
            unsafe {
                std::arch::asm!(
                    "syscall",
                    "int3",
                    inlateout("rax") libc::SYS_fork => child,
                    lateout("rcx") _,
                    lateout("r11") _,
                    options(nostack),
                );
            }
        } else {
            child = unsafe { libc::syscall(libc::SYS_fork) };
        }
        assert!(child >= 0, "actual fork setup failed");
        if child == 0 {
            if matches!(mode, Mode::ChildTgkill) {
                assert_eq!(
                    unsafe { libc::syscall(libc::SYS_tgkill, parent, parent, libc::SIGTRAP) },
                    0,
                );
            }
            emit_report(true, -1);
            unsafe { libc::_exit(0) };
        }
        let child = libc::pid_t::try_from(child).unwrap();
        let mut status = -1;
        loop {
            let waited = unsafe { libc::waitpid(child, &mut status, 0) };
            if waited == child {
                break;
            }
            assert_eq!(waited, -1);
            assert_eq!(unsafe { *libc::__errno_location() }, libc::EINTR);
        }
        // Retain the actual status in the report, rather than hiding a child failure.
        emit_report(false, status);
    }

    fn run(mode: Mode) {
        let (output, log) = crate::testing::test_fn::<SignalTool, _>(move || guest_body(mode))
            .expect("original FunctionGuest owner must complete");
        // All semantic assertions below follow original wait_with_output completion.
        assert_eq!(output.status, ExitStatus::Exited(0));
        assert!(
            output.stderr.is_empty(),
            "guest stderr: {:?}",
            output.stderr
        );
        assert_eq!(output.stdout.len(), 2 * REPORT_BYTES);
        let mut reports = [None, None];
        let (report_bytes, remainder) = output.stdout.as_chunks::<REPORT_BYTES>();
        assert!(remainder.is_empty());
        for bytes in report_bytes {
            let mut words = [0i32; 8];
            let (encoded_words, remainder) = bytes.as_chunks::<4>();
            assert!(remainder.is_empty());
            for (word, bytes) in words.iter_mut().zip(encoded_words) {
                *word = i32::from_ne_bytes(*bytes);
            }
            assert_eq!(words[0], MAGIC);
            let role = usize::try_from(words[1]).unwrap();
            assert!(role < 2);
            assert!(
                reports[role].replace(words).is_none(),
                "duplicate role report"
            );
        }
        let reports = reports.map(|row| row.expect("both original roles must report"));
        assert!(reports.iter().all(|row| row[2] > 0 && row[6] == 0));
        assert_ne!(reports[0][2], reports[1][2]);
        assert_eq!(reports[0][7], 0, "original child wait must report exit0");
        assert_eq!(reports[1][7], -1);

        let events = log.0.into_inner().unwrap();
        for role in 0..2 {
            let child = role == 1;
            let pid = reports[role][2];
            assert_eq!(
                events
                    .iter()
                    .filter(|event| {
                        matches!(event, Observation::Start(tid, c) if *tid == pid && *c == child)
                    })
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| {
                        matches!(event, Observation::Exit(tid, c, ExitStatus::Exited(0))
                    if *tid == pid && *c == child)
                    })
                    .count(),
                1
            );
            let expected = match mode {
                Mode::Plain => 0,
                Mode::NextInt3 => 1,
                Mode::ChildTgkill => usize::from(!child),
            };
            let traps: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    Observation::Trap(tid, c, code, sender) if *tid == pid && *c == child => {
                        Some((*code, *sender))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                traps.len(),
                expected,
                "Tool must see only genuine guest traps"
            );
            assert_eq!(reports[role][3], expected as i32, "actual handler count");
            if expected == 0 {
                assert_eq!(reports[role][4], 0);
                assert_eq!(reports[role][5], 0);
            } else {
                let expected_code = match mode {
                    Mode::NextInt3 => libc::SI_KERNEL,
                    Mode::ChildTgkill => libc::SI_TKILL,
                    Mode::Plain => unreachable!(),
                };
                assert_eq!(traps[0].0, expected_code);
                assert_eq!(reports[role][4], expected_code);
                if matches!(mode, Mode::ChildTgkill) {
                    assert_eq!(traps[0].1, reports[1][2]);
                    assert_eq!(reports[role][5], reports[1][2]);
                }
            }
        }
        let expected_traps = match mode {
            Mode::Plain => 0,
            Mode::NextInt3 => 2,
            Mode::ChildTgkill => 1,
        };
        assert_eq!(
            events.len(),
            4 + expected_traps,
            "no extra or foreign owner event"
        );
    }

    #[test]
    fn ordinary_fork_has_no_guest_or_tool_administrative_sigtrap() {
        run(Mode::Plain);
    }

    #[test]
    fn next_ip_int3_after_fork_reaches_both_original_guest_handlers_once() {
        run(Mode::NextInt3);
    }

    #[test]
    fn child_tgkill_after_fork_reaches_original_parent_once() {
        run(Mode::ChildTgkill);
    }
}
