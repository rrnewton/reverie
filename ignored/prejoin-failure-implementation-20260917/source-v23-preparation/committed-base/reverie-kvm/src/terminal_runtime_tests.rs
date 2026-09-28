use super::*;

#[derive(Default)]
struct LowerTool;
#[reverie::tool]
impl Tool for LowerTool {
    type GlobalState = ();
    type ThreadState = ();
}
#[derive(Default)]
struct AdapterTool(LowerTool);
impl AsMut<LowerTool> for AdapterTool {
    fn as_mut(&mut self) -> &mut LowerTool {
        &mut self.0
    }
}
#[reverie::tool]
impl Tool for AdapterTool {
    type GlobalState = ();
    type ThreadState = Box<()>;
}
struct RefusingExecutor;
impl GuestSyscallExecutor<AdapterTool> for RefusingExecutor {
    fn read_clock(&self) -> Result<u64> {
        Err(Error::GuestClock(
            "signal test adapter has no guest counter".into(),
        ))
    }

    fn execute(&mut self, _: &SyscallRequest, _: &GuestMemory) -> i64 {
        panic!("terminal cancellation executed a syscall")
    }
    fn tail_injection_allowed(&self) -> bool {
        false
    }
    fn complete_injection<'a>(
        &'a mut self,
        _: ToolContext<'a, AdapterTool>,
    ) -> Pin<Box<dyn Future<Output = Result<InjectionCompletion>> + Send + 'a>>
    where
        AdapterTool: 'a,
    {
        panic!("terminal cancellation completed an injection")
    }
}

#[test]
fn terminal_cancellation_and_into_guest_forwarding_do_not_inject_or_start_children() {
    for adapted in [false, true] {
        for thread in [false, true] {
            let memory = GuestMemory::new(0, STACK_CAPACITY).unwrap();
            let mut state = Box::new(());
            let mut executor = RefusingExecutor;
            let subscriptions = Subscription::none();
            let signal = Arc::new(Mutex::new(None));
            let starts = Arc::new(Mutex::new(Vec::new()));
            let (sender, receiver) = std::sync::mpsc::channel();
            let gate = ChildStartGate::new(sender);
            starts.lock().unwrap().push(if thread {
                PendingChildStart::tool_thread(2, gate)
            } else {
                PendingChildStart::fork_process(2, gate)
            });
            let mut guest = KvmGuest::<AdapterTool>::new(
                Pid::from_raw(1),
                Pid::from_raw(2),
                Arc::new(AdapterTool::default()),
                memory,
                &[],
                // SAFETY: this test never reads the register fields.
                unsafe { std::mem::zeroed() },
                &mut state,
                &mut executor,
                &(),
                None,
                &(),
                &subscriptions,
                signal.clone(),
                starts.clone(),
                crate::bootstrap::TOOL_STACK_TOP,
                Arc::new(AtomicBool::new(false)),
            );
            let outcome = futures::executor::block_on(drive_handler(
                async {
                    if adapted {
                        <_ as Guest<LowerTool>>::cancel_current_thread(&mut guest.into_guest())
                            .await
                    } else {
                        guest.cancel_current_thread().await
                    }
                },
                signal,
                starts.clone(),
            ));
            assert!(
                matches!(outcome, HandlerOutcome::ThreadCancelled),
                "adapted={adapted}"
            );
            assert_eq!(
                receiver.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            );
            assert_eq!(starts.lock().unwrap().len(), 1);
            // The owning caller, after dropping the callback, releases successful starts.
            start_pending_children(&starts).unwrap();
            assert_eq!(receiver.recv().unwrap(), ChildStartCommand::Start);
            assert!(starts.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn terminal_exit_retires_only_current_identity_and_preserves_existing_status() {
    let cwd = std::env::current_dir().unwrap();
    let memory = GuestMemory::new(0, 4096).unwrap();
    for established in [
        None,
        Some((libc::SYS_exit, 17, false)),
        Some((libc::SYS_exit_group, 37, true)),
    ] {
        let mut leader = ElfExecutor::new(crate::executor::test_loaded_state_for_vm(&cwd), false);
        let mut retired = leader.thread_child(7).unwrap();
        let _peer = leader.thread_child(8).unwrap();
        let probe = |tid| SyscallRequest::new(libc::SYS_tgkill as u64, [1, tid, 0, 0, 0, 0]);
        assert_eq!(leader.execute(&probe(7), &memory), 0);
        if let Some((number, status, _)) = established {
            assert_eq!(
                retired.execute(
                    &SyscallRequest::new(number as u64, [status, 0, 0, 0, 0, 0]),
                    &memory
                ),
                0
            );
        }
        let exit = retired.cancel_current_thread();
        assert_eq!(
            exit.status,
            established.map_or(ExitStatus::SUCCESS, |(_, status, _)| ExitStatus::Exited(
                status as i32
            ))
        );
        assert_eq!(exit.group, established.is_some_and(|(_, _, group)| group));
        assert!(
            retired.take_exit().is_none(),
            "terminal state is consumed once"
        );
        assert_eq!(leader.execute(&probe(7), &memory), -i64::from(libc::ESRCH));
        assert_eq!(
            leader.execute(&probe(8), &memory),
            0,
            "a live peer stays registered"
        );
        let _replacement = leader.thread_child(7).unwrap();
        drop(retired);
        assert_eq!(
            leader.execute(&probe(7), &memory),
            0,
            "old generation drop must not retire reused TID"
        );
    }
}
