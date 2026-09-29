use super::*;

#[derive(Default)]
struct CapturedRightsLog {
    sends: AtomicU64,
    receives: AtomicU64,
}

#[reverie::global_tool]
impl GlobalTool for CapturedRightsLog {
    type Request = bool;
    type Response = ();
    type Config = ();
    async fn receive_rpc(&self, _: Pid, sending: bool) {
        if sending {
            self.sends.fetch_add(1, Ordering::SeqCst);
        } else {
            self.receives.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Clone, Default)]
struct CapturedRightsTool;

#[reverie::tool]
impl Tool for CapturedRightsTool {
    type GlobalState = CapturedRightsLog;
    type ThreadState = ();
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        match syscall {
            Syscall::Sendmsg(_) => guest.send_rpc(true).await,
            Syscall::Recvmsg(_) => guest.send_rpc(false).await,
            _ => (),
        }
        guest.tail_inject(syscall).await
    }
}

#[test]
fn private_captured_rights_survive_sender_exit_peek_and_forwarding() {
    const TEST: &str =
        "captured_rights::private_captured_rights_survive_sender_exit_peek_and_forwarding";
    if !leader_self_exec_bounded(TEST) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "captured-rights",
        include_str!("../fixtures/captured_rights.c"),
    );
    for with_tool in [false, true] {
        let program = executable.to_str().unwrap();
        let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
        backend
            .install_static_elf_file_with_context(
                std::fs::File::open(&executable).unwrap(),
                &[program],
                &[],
                &directory.0,
            )
            .unwrap();
        let (code, stdout, stderr) = if with_tool {
            let (log, code, stdout, stderr) = futures::executor::block_on(
                backend.run_static_elf_with_tool::<CapturedRightsTool>((), true),
            )
            .unwrap();
            assert_eq!(
                log.sends.load(Ordering::SeqCst),
                2,
                "actual sendmsg callbacks"
            );
            assert_eq!(
                log.receives.load(Ordering::SeqCst),
                4,
                "actual recvmsg callbacks"
            );
            (code, stdout, stderr)
        } else {
            backend.run_static_elf_captured().unwrap()
        };
        assert_eq!(
            code, 0,
            "with_tool={with_tool} stdout={stdout:?} stderr={stderr:?}"
        );
        assert_eq!(
            stdout, b"PQRVWZcapture-rights-ok\n",
            "with_tool={with_tool}"
        );
        assert_eq!(stderr, b"E", "with_tool={with_tool}");
    }
}
