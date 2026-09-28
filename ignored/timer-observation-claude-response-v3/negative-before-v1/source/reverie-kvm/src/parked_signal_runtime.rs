// This file is included in runtime.rs to use the one real KvmGuest reborrow.

impl StaticElfSyscallExecutor<'_> {
    fn signal_injection_allowed(&self, request: &SyscallRequest) -> bool {
        if self.signal_guard == SignalGuard::DequeueNotification {
            return false;
        }
        if (self.signal_guard == SignalGuard::Observation || self.executor.has_prepared_signal())
            && (injection_can_be_nonreturning(request)
                || matches!(
                    request.number() as libc::c_long,
                    libc::SYS_fork | libc::SYS_vfork | libc::SYS_clone | libc::SYS_clone3
                ))
        {
            return false;
        }
        if request.number() == libc::SYS_rt_sigaction as u64
            && request.args()[1] != 0
            && self
                .executor
                .prepared_signal_number()
                .is_some_and(|signal| request.args()[0] == signal as u64)
        {
            return false;
        }
        self.process_context.ordinary_injection_allowed(request)
    }

    fn current_parked_site(&self) -> Option<reverie::CallbackSignalSite> {
        (self.executor.signal_dequeues_enabled()
            && matches!(
                self.process_context,
                ProcessExecutionContext::SyscallBoundary(_)
            )
            && !*self.process_completed
            && !self.executor.has_prepared_signal()
            && self.executor.sole_signal_receiver())
        .then_some(self.callback_site)
        .flatten()
    }
}

impl<T: Tool> KvmGuest<'_, T> {
    async fn complete_signal_effects(
        &mut self,
        raw: Option<i64>,
    ) -> Result<Option<reverie::SignalDequeue>> {
        let mut last = None;
        if self.executor.signal_dequeue_front().is_some() {
            // Stored outside this future: cancellation during the notification
            // must retain the real copyout errno along with the removal.
            self.executor.retain_signal_effect_result(raw);
        }
        loop {
            let next = futures::future::poll_fn(|cx| self.executor.poll_signal_dequeue(cx)).await;
            let effect = match next {
                Ok(Some(effect)) => effect,
                Ok(None) => break,
                Err(error) => return Err(self.executor.with_signal_effects(error, raw)),
            };
            self.executor.retain_signal_dequeue(effect);
            let previous = self
                .executor
                .set_signal_guard(SignalGuard::DequeueNotification);
            self.notifying_dequeue = true;
            let tool = self.process_state.clone();
            let result = tool.handle_signal_dequeue(self, effect).await;
            self.notifying_dequeue = false;
            self.executor.set_signal_guard(previous);
            if let Err(errno) = result {
                return Err(self
                    .executor
                    .with_signal_effects(Error::Reverie(errno.into()), raw));
            }
            if let Err(errno) = self.executor.acknowledge_signal_dequeue(effect) {
                return Err(self
                    .executor
                    .with_signal_effects(Error::Reverie(errno.into()), raw));
            }
            last = Some(effect);
        }
        Ok(last)
    }

    async fn observe_parked_signal_impl(
        &mut self,
        site: reverie::CallbackSignalSite,
        lease: reverie::ParkedObservationLease,
    ) -> std::result::Result<reverie::ParkedSignalObservation, reverie::SignalObservationFailure>
    {
        use reverie::SignalObservationFailure as Failure;
        use reverie::SignalObservationStep as Step;
        use reverie::SignalObservationStop as Stop;
        if self.executor.parked_signal_site() != Some(site)
            || self.observation_lease.is_some()
            || self.notifying_dequeue
            || self.stack_checked_out.load(Ordering::Acquire)
        {
            return Err(Failure::RejectedBeforeRemoval {
                errno: Errno::ENOSYS,
            });
        }
        let mut steps = Vec::new();
        steps
            .try_reserve_exact(64)
            .map_err(|_| Failure::RejectedBeforeRemoval {
                errno: Errno::ENOMEM,
            })?;
        self.executor
            .admit_signal_observation(site, lease)
            .map_err(|errno| Failure::RejectedBeforeRemoval { errno })?;
        self.observation_lease = Some(lease);
        let previous = self.executor.set_signal_guard(SignalGuard::Observation);
        let result: Result<reverie::ParkedSignalObservation> = async {
            for _ in 0..64 {
                let pending = self.executor.take_signal_for_observation();
                // Even readiness failure after removal must flush before propagating.
                let effect = self.complete_signal_effects(None).await?;
                let pending = pending.map_err(|errno| Error::Reverie(errno.into()))?;
                let Some(pending) = pending else {
                    return Ok(reverie::ParkedSignalObservation {
                        steps,
                        stop: Stop::NoEligibleSignal,
                    });
                };
                let effect = effect.ok_or_else(|| {
                    Error::UnexpectedVcpuExit(
                        "parked removal had no dequeue journal entry".to_owned(),
                    )
                })?;
                if effect.event != pending.event
                    || effect.domain
                        != match pending.domain {
                            crate::executor::PendingSignalDomain::Thread => {
                                reverie::PendingDomain::Thread
                            }
                            crate::executor::PendingSignalDomain::Process => {
                                reverie::PendingDomain::Process
                            }
                        }
                    || effect.consumer != reverie::SignalConsumer::ReturnToUser
                {
                    return Err(Error::UnexpectedVcpuExit(
                        "parked dequeue identity did not match selected event".to_owned(),
                    ));
                }
                let id = effect.id();
                let tool = self.process_state.clone();
                let replacement = tool
                    .handle_structured_signal_event(self, pending.event)
                    .await
                    .map_err(|errno| Error::Reverie(errno.into()))?;
                let Some(event) = replacement else {
                    steps.push(Step::Suppressed { dequeue: id });
                    continue;
                };
                let selected = self
                    .executor
                    .filter_signal_replacement(event, pending.domain)
                    .map_err(|errno| Error::Reverie(errno.into()))?;
                let Some(selected) = selected else {
                    steps.push(Step::Reblocked {
                        dequeue: id,
                        domain: effect.domain,
                    });
                    continue;
                };
                let fatal = match self.executor.signal_disposition(selected.event.signal()) {
                    crate::executor::SignalDisposition::Ignore => {
                        steps.push(Step::Ignored { dequeue: id });
                        continue;
                    }
                    crate::executor::SignalDisposition::Handled => false,
                    crate::executor::SignalDisposition::Terminate => true,
                    crate::executor::SignalDisposition::Stop => {
                        return Err(Error::Reverie(Errno::ENOSYS.into()));
                    }
                };
                let selection = self
                    .executor
                    .reserve_signal_delivery(selected, id, fatal)
                    .map_err(|errno| Error::Reverie(errno.into()))?;
                steps.push(if fatal {
                    Step::Fatal { dequeue: id }
                } else {
                    Step::Caught { dequeue: id }
                });
                return Ok(reverie::ParkedSignalObservation {
                    steps,
                    stop: if fatal {
                        Stop::Fatal(selection)
                    } else {
                        Stop::Caught(selection)
                    },
                });
            }
            Err(Error::UnexpectedVcpuExit(
                "parked signal observation exhausted 64 actual selections".to_owned(),
            ))
        }
        .await;
        self.executor.set_signal_guard(previous);
        self.observation_lease = None;
        match result {
            Ok(observation) => Ok(observation),
            Err(error) => {
                // Dropping the outer future cannot erase acknowledged removals.
                let error = self.executor.with_signal_effects(error, None);
                self.signal_handler(HandlerSignal::RuntimeError(error));
                std::future::pending().await
            }
        }
    }
}

// Drive the real consuming cleanup path separately from ordinary callbacks.
async fn drive_signal_cleanup<T>(
    future: impl Future<Output = T>,
    handler_signal: SharedHandlerSignal,
    pending_children: SharedChildStarts,
    _failure: impl Future<Output = ()>,
) -> HandlerOutcome<T> {
    drive_handler(future, handler_signal, pending_children, std::future::pending()).await
}

#[allow(clippy::too_many_arguments)]
async fn flush_pending_signal_effects_with_tool<T>(
    backend: &mut KvmBackend,
    executor: &mut ElfExecutor,
    pid: Pid,
    tid: Pid,
    tool: &Arc<T>,
    memory: &GuestMemory,
    auxv: &[(libc::c_ulong, libc::c_ulong)],
    registers: libc::user_regs_struct,
    thread_state: &mut T::ThreadState,
    global_state: &Arc<T::GlobalState>,
    config: &<T::GlobalState as GlobalTool>::Config,
    subscriptions: &Subscription,
    stack_checked_out: &Arc<AtomicBool>,
    raw: Option<i64>,
) -> Result<()>
where
    T: Tool + 'static,
    T::ThreadState: 'static,
    T::GlobalState: 'static,
    <T::GlobalState as GlobalTool>::Config: 'static,
{
    if executor.signal_dequeue_front().is_none() {
        return Ok(());
    }
    let failure = backend.failure_subscription(executor.is_traced_tree_root());
    let handler_signal = Arc::new(Mutex::new(None));
    let pending_children = Arc::new(Mutex::new(Vec::new()));
    let mut process_completed = false;
    let tool_stack_top = backend.tool_stack_top();
    let outcome = {
        let mut adapter = StaticElfSyscallExecutor {
            backend,
            executor,
            memory: memory.clone(),
            process_context: ProcessExecutionContext::Lifecycle,
            last_result: raw,
            process_completed: &mut process_completed,
            callback_site: None,
            signal_guard: SignalGuard::Ordinary,
        };
        let mut guest = KvmGuest::new(
            pid,
            tid,
            tool.clone(),
            memory.clone(),
            auxv,
            registers,
            thread_state,
            &mut adapter,
            global_state.as_ref(),
            Some(global_state.clone()),
            config,
            subscriptions,
            handler_signal.clone(),
            pending_children.clone(),
            tool_stack_top,
            stack_checked_out.clone(),
        );
        // Consuming cleanup must flush even when the run has already failed.
        drive_signal_cleanup(
            guest.complete_signal_effects(raw),
            handler_signal,
            pending_children,
            wait_for_failure(global_state.as_ref(), failure),
        )
        .await
    };
    match outcome {
        HandlerOutcome::Returned(result) => result.map(|_| ()),
        HandlerOutcome::RuntimeError(error) => Err(error),
        HandlerOutcome::ParkedCancelled(_) | HandlerOutcome::RunFailed => {
            Err(executor.with_signal_effects(Error::RunAborted, raw))
        }
        _ => Err(executor.with_signal_effects(
            Error::UnexpectedVcpuExit(
                "nonlocal operation during dequeue acknowledgment".to_owned(),
            ),
            raw,
        )),
    }
}

#[cfg(test)]
mod signal_cleanup_tests {
    use super::*;
    use futures::task::noop_waker;
    use std::task::Context;

    #[test]
    fn signal_cleanup_pending_callback_observes_published_peer_failure() {
        let global = Arc::new(());
        let run = RunFailure::new(&global);
        let context = FailureContext::new(run.clone(), Pid::from_raw(1), Pid::from_raw(2));
        let polled = Arc::new(AtomicBool::new(false));
        let pending = polled.clone();
        let future = futures::future::poll_fn(move |_| {
            pending.store(true, Ordering::Release);
            Poll::<Result<()>>::Pending
        });
        let mut cleanup = pin!(drive_signal_cleanup(
            future,
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(Vec::new())),
            wait_for_failure(global.as_ref(), Some(run.subscribe())),
        ));
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(cleanup.as_mut().poll(&mut cx).is_pending());
        assert!(polled.load(Ordering::Acquire));
        context.publish("lost dequeue owner", Error::GuestWorkerPanic);
        assert!(matches!(
            cleanup.as_mut().poll(&mut cx),
            Poll::Ready(HandlerOutcome::RunFailed)
        ), "a published lost owner must terminate consuming cleanup, not leave it parked");
        assert!(matches!(run.primary().unwrap().primary(), Error::GuestWorkerPanic));
    }

    #[test]
    fn signal_cleanup_ready_result_precedes_terminal_cancellation() {
        for result in [Ok(()), Err(Error::Reverie(Errno::EFAULT.into()))] {
            let was_error = result.is_err();
            let outcome = futures::executor::block_on(drive_signal_cleanup(
                std::future::ready(result),
                Arc::new(Mutex::new(None)),
                Arc::new(Mutex::new(Vec::new())),
                std::future::ready(()),
            ));
            match outcome {
                HandlerOutcome::Returned(result) => assert_eq!(result.is_err(), was_error),
                _ => panic!("already-ready cleanup/result was discarded"),
            }
        }
    }
}
