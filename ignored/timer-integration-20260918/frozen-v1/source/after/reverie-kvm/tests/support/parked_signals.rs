// Actual initialized KvmGuest/Tool/frame controls. This component has no Hermit
// scheduler or timer: repeated publications are explicit fixture stimuli.
use reverie::CallbackSignalSite;
use reverie::DequeueId;
use reverie::ParkedObservationLease;
use reverie::PendingDomain;
use reverie::ProcessAlarmSignalOutcome;
use reverie::SignalConsumer;
use reverie::SignalDequeue;
use reverie::SignalObservationStep;
use reverie::SignalObservationStop;
use reverie::SignalTaskIdentity;

use super::*;

#[derive(Clone, Debug)]
enum Seen {
    Start(SignalTaskIdentity),
    Dequeue(SignalDequeue),
    Hook(DequeueId),
    Finish(CallbackSignalSite),
    Exit(ExitStatus),
}
static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());
fn seen(value: Seen) {
    SEEN.lock().unwrap().push(value);
}
fn alarm(pid: Pid) -> SignalEvent {
    let mut info = [0; reverie::SIGNAL_INFO_SIZE];
    info[..4].copy_from_slice(&libc::SIGALRM.to_ne_bytes());
    info[8..12].copy_from_slice(&libc::SI_KERNEL.to_ne_bytes());
    SignalEvent::new(libc::SIGALRM, info, SignalTarget::Process { pid }).unwrap()
}
#[derive(Default, Debug)]
struct Global;
#[reverie::global_tool]
impl GlobalTool for Global {
    type Request = u64;
    type Response = u64;
    type Config = u8;
    async fn receive_rpc(&self, _: Pid, value: u64) -> u64 {
        let mut first = true;
        futures::future::poll_fn(|cx| {
            if std::mem::take(&mut first) {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(value)
            }
        })
        .await
    }
}
#[derive(Default)]
struct ParkedTool {
    mode: u8,
    removed: AtomicU64,
    hooks: AtomicU64,
}
#[reverie::tool]
impl Tool for ParkedTool {
    type GlobalState = Global;
    type ThreadState = ();
    fn new(_: Pid, mode: &u8) -> Self {
        Self {
            mode: *mode,
            ..Default::default()
        }
    }
    fn observe_signal_dequeues(_: &u8) -> bool {
        true
    }
    fn subscriptions(mode: &u8) -> Subscription {
        let mut subscription = Subscription::all();
        if *mode == 8 {
            subscription.disable_syscall(Sysno::read);
        }
        if *mode == 9 {
            subscription.disable_syscall(Sysno::rt_sigtimedwait);
        }
        subscription
    }
    async fn handle_thread_start<G: Guest<Self>>(
        &self,
        guest: &mut G,
    ) -> Result<(), reverie::Error> {
        let identity = guest
            .signal_task_identity()
            .expect("identity before first guest instruction");
        assert_eq!(identity.tid, guest.tid());
        assert_eq!(identity.process.tgid, guest.pid());
        assert!(identity.task_generation > 0 && identity.process.generation > 0);
        assert!(guest.parked_signal_site().is_none());
        seen(Seen::Start(identity));
        Ok(())
    }
    async fn handle_signal_dequeue<G: Guest<Self>>(
        &self,
        guest: &mut G,
        effect: SignalDequeue,
    ) -> Result<(), Errno> {
        let ordinal = self.removed.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(effect.sequence, ordinal);
        assert_eq!(
            effect.process,
            guest.signal_task_identity().unwrap().process
        );
        assert_eq!(effect.domain, PendingDomain::Process);
        assert_eq!(effect.event, alarm(guest.pid()));
        assert_eq!(
            guest.inject(reverie::syscalls::Getpid::new()).await,
            Err(Errno::ENOSYS)
        );
        assert_eq!(guest.send_rpc(ordinal).await, ordinal);
        seen(Seen::Dequeue(effect));
        if self.mode == 13 {
            guest.cancel_current_thread().await;
        }
        if self.mode == 10 {
            Err(Errno::EIO)
        } else {
            Ok(())
        }
    }
    async fn handle_structured_signal_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        event: SignalEvent,
    ) -> Result<Option<SignalEvent>, Errno> {
        let ordinal = self.hooks.fetch_add(1, Ordering::SeqCst);
        let identity = guest.signal_task_identity().unwrap();
        let id = DequeueId {
            process: identity.process,
            sequence: self.removed.load(Ordering::SeqCst),
        };
        assert!(id.sequence > 0);
        assert_eq!(event, alarm(guest.pid()));
        assert!(guest.signal_observation_lease().is_some());
        assert_eq!(guest.send_rpc(0x100 + ordinal).await, 0x100 + ordinal);
        assert_eq!(
            guest.inject(reverie::syscalls::Getpid::new()).await?,
            i64::from(guest.pid().as_raw())
        );
        assert_eq!(guest.inject(Fork::new()).await, Err(Errno::ENOSYS));
        assert_eq!(
            guest.inject(ExitGroup::new().with_status(99)).await,
            Err(Errno::ENOSYS)
        );
        seen(Seen::Hook(id));
        if self.mode == 11 {
            guest.tail_inject(reverie::syscalls::Getpid::new()).await;
        }
        if self.mode == 1 {
            return Ok(None);
        }
        if self.mode == 2 && ordinal == 0 {
            let mut stack = guest.stack().await;
            let mask = stack.push(1_u64 << (libc::SIGALRM - 1)).as_raw() as usize;
            let _guard = stack.commit()?;
            let call = Syscall::from_raw(
                Sysno::rt_sigprocmask,
                SyscallArgs::new(libc::SIG_BLOCK as usize, mask, 0, 8, 0, 0),
            );
            assert_eq!(guest.inject(call).await?, 0);
        }
        Ok(Some(event))
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        let (_, args) = syscall.into_parts();
        if syscall.number() != Sysno::getpid || args.arg0 != 0x7061726b {
            guest.tail_inject(syscall).await;
        }
        assert_eq!(args.arg1 as u8, self.mode);
        let site = guest
            .parked_signal_site()
            .expect("actual original syscall boundary");
        let cycles = if self.mode <= 1 { 3 } else { 1 };
        for cycle in 0..cycles {
            let outcome = guest.queue_process_alarm_signal(alarm(guest.pid())).await;
            assert!(
                matches!(outcome, ProcessAlarmSignalOutcome::Accepted(_)),
                "{outcome:?}"
            );
            if (6..=10).contains(&self.mode) || self.mode == 13 {
                if self.mode == 8 || self.mode == 9 || self.mode == 13 {
                    return Ok(123);
                }
                let call = if self.mode == 7 {
                    Syscall::from_raw(
                        Sysno::rt_sigtimedwait,
                        SyscallArgs::new(args.arg2, 1, 0, 8, 0, 0),
                    )
                } else {
                    Syscall::from_raw(Sysno::read, SyscallArgs::new(args.arg3, 1, 128, 0, 0, 0))
                };
                assert_eq!(guest.inject(call).await, Err(Errno::EFAULT));
                assert_eq!(
                    self.removed.load(Ordering::SeqCst),
                    1,
                    "effect acknowledgment preceded errno return"
                );
                return Ok(123);
            }
            let mut observation = guest
                .observe_parked_signal(site, ParkedObservationLease { nonce: cycle + 1 })
                .await
                .unwrap();
            assert!(guest.signal_observation_lease().is_none());
            if self.mode == 2 {
                assert!(matches!(
                    observation.steps.as_slice(),
                    [SignalObservationStep::Reblocked {
                        domain: PendingDomain::Process,
                        ..
                    }]
                ));
                assert_eq!(observation.stop, SignalObservationStop::NoEligibleSignal);
                let call = Syscall::from_raw(
                    Sysno::rt_sigprocmask,
                    SyscallArgs::new(libc::SIG_UNBLOCK as usize, args.arg2, 0, 8, 0, 0),
                );
                assert_eq!(guest.inject(call).await?, 0);
                observation = guest
                    .observe_parked_signal(site, ParkedObservationLease { nonce: 2 })
                    .await
                    .unwrap();
            }
            match observation.stop {
                SignalObservationStop::NoEligibleSignal => {
                    assert!(self.mode <= 1);
                    assert_eq!(observation.steps.len(), 1);
                    assert!(matches!(
                        observation.steps[0],
                        SignalObservationStep::Ignored { .. }
                            | SignalObservationStep::Suppressed { .. }
                    ));
                }
                SignalObservationStop::Caught(_) => {
                    assert!(matches!(self.mode, 2 | 3 | 5 | 12));
                    let context = guest
                        .parked_signal_failure_context()
                        .expect("ledger survives caught handoff");
                    assert_eq!(context.callback_nonce, site.callback_nonce);
                    if self.mode == 5 {
                        guest.cancel_parked_signal(context).await.unwrap();
                    }
                    assert_eq!(guest.inject(Fork::new()).await, Err(Errno::ENOSYS));
                    assert_eq!(
                        guest.inject(ExitGroup::new().with_status(99)).await,
                        Err(Errno::ENOSYS)
                    );
                    // A real posthook RPC/injection runs while the original callback is alive.
                    assert_eq!(guest.send_rpc(0x200).await, 0x200);
                    assert_eq!(
                        guest.inject(reverie::syscalls::Getpid::new()).await?,
                        i64::from(guest.pid().as_raw())
                    );
                    seen(Seen::Finish(site));
                    return Err(if self.mode == 12 {
                        Errno::EFAULT
                    } else {
                        Errno::EINTR
                    }
                    .into());
                }
                SignalObservationStop::Fatal(token) => {
                    assert_eq!(self.mode, 4);
                    guest.terminate_from_parked_signal(token).await.unwrap();
                }
            }
        }
        seen(Seen::Finish(site));
        Ok(123)
    }
    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        _: Pid,
        _: &G,
        _: (),
        status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        seen(Seen::Exit(status));
        Ok(())
    }
}

type EffectEvidence<'a> = (
    &'a [SignalDequeue],
    u64,
    &'a [ProcessAlarmSignalOutcome],
    Option<i64>,
);

fn effect_error(error: &Error) -> Option<EffectEvidence<'_>> {
    match error {
        Error::SignalEffects {
            dequeues,
            acknowledged_through,
            publications,
            raw_result,
            ..
        } => Some((dequeues, *acknowledged_through, publications, *raw_result)),
        Error::SharedFailure(error) | Error::WorkerFailure { error, .. } => effect_error(error),
        Error::WithCleanup { primary, cleanup } => {
            effect_error(primary).or_else(|| cleanup.iter().find_map(|error| effect_error(error)))
        }
        _ => None,
    }
}

#[test]
fn parked_signal_actual_callback_frame_and_dequeue_contract() {
    const TEST: &str = "parked_signals::parked_signal_actual_callback_frame_and_dequeue_contract";
    if !leader_self_exec_bounded(TEST) {
        return;
    }
    let directory = TestDirectory::new();
    let executable = compile_c_program(
        &directory.0,
        "parked-signal",
        include_str!("../fixtures/parked_signal.c"),
    );
    for mode in 0..=13_u8 {
        SEEN.lock().unwrap().clear();
        let mut backend = KvmBackend::new(256 * 1024 * 1024).unwrap();
        backend
            .install_static_elf_file_with_context(
                std::fs::File::open(&executable).unwrap(),
                &[executable.to_str().unwrap(), &mode.to_string()],
                &[],
                &directory.0,
            )
            .unwrap();
        let result =
            futures::executor::block_on(backend.run_static_elf_with_tool::<ParkedTool>(mode, true));
        let seen = SEEN.lock().unwrap().clone();
        eprintln!("parked-signal mode={mode} result={result:?} seen={seen:?}");
        let starts = seen
            .iter()
            .filter_map(|s| {
                if let Seen::Start(id) = s {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), 1);
        let dequeues = seen
            .iter()
            .filter_map(|s| {
                if let Seen::Dequeue(d) = s {
                    Some(*d)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            dequeues.len(),
            if mode <= 1 {
                3
            } else if mode == 2 {
                2
            } else {
                1
            }
        );
        for (index, effect) in dequeues.iter().enumerate() {
            assert_eq!(effect.sequence, index as u64 + 1);
            assert_eq!(effect.process, starts[0].process);
        }
        let hooks = seen
            .iter()
            .filter_map(|s| {
                if let Seen::Hook(id) = s {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            hooks.len(),
            if (6..=10).contains(&mode) || mode == 13 {
                0
            } else {
                dequeues.len()
            }
        );
        for id in hooks {
            assert!(dequeues.iter().any(|effect| effect.id() == id));
        }
        if matches!(mode, 5 | 10 | 11 | 13) {
            let error = result.unwrap_err();
            let (retained, ack, receipts, raw) =
                effect_error(&error).expect("committed effects attached to original failure");
            assert_eq!(retained, dequeues);
            assert_eq!(receipts.len(), usize::from(mode != 13));
            assert_eq!(ack, if mode == 10 || mode == 13 { 0 } else { 1 });
            if mode == 13 {
                assert!(matches!(error.primary(), Error::RunAborted));
            }
            assert_eq!(
                raw,
                if mode == 10 || mode == 13 {
                    Some(-(libc::EFAULT as i64))
                } else {
                    None
                }
            );
        } else {
            let (_, code, stdout, stderr) = result.unwrap();
            assert!(stderr.is_empty());
            if mode == 4 {
                assert_eq!(code, 128 + libc::SIGALRM);
                assert!(stdout.is_empty());
            } else {
                assert_eq!(code, 0, "mode {mode}");
                assert_eq!(stdout, b"parked-signal-checked\n");
            }
            let exits = seen
                .iter()
                .filter_map(|s| {
                    if let Seen::Exit(status) = s {
                        Some(*status)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(
                exits,
                [if mode == 4 {
                    ExitStatus::Signaled(reverie::Signal::SIGALRM, false)
                } else {
                    ExitStatus::Exited(0)
                }]
            );
        }
        for finish in seen.iter().filter_map(|s| {
            if let Seen::Finish(site) = s {
                Some(site)
            } else {
                None
            }
        }) {
            assert_eq!(finish.process, starts[0].process);
        }
        for effect in dequeues {
            assert_eq!(
                effect.consumer,
                if matches!(mode, 6 | 8 | 10 | 13) {
                    SignalConsumer::SignalFd
                } else if matches!(mode, 7 | 9) {
                    SignalConsumer::SignalTimedWait
                } else {
                    SignalConsumer::ReturnToUser
                }
            );
        }
    }
}
