from pathlib import Path
p=Path('reverie-kvm/src/signal.rs');s=p.read_text();at='/// Signal state shared by all threads in one guest process.'
s=s.replace(at,'''/// Stable per-task notification wakeup, also retained after lifecycle retirement.
#[derive(Clone, Debug, Default)]
pub(crate) struct SignalDequeueWake(pub(crate) Arc<futures::task::AtomicWaker>);
#[cfg(test)]
impl PartialEq for SignalDequeueWake {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}
#[cfg(test)]
impl Eq for SignalDequeueWake {}

#[derive(Clone, Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub(crate) struct OwnedSignalDequeue {
    pub(crate) owner: reverie::SignalTaskIdentity,
    pub(crate) effect: reverie::SignalDequeue,
    pub(crate) wake: SignalDequeueWake,
}

'''+at)
s=s.replace('VecDeque<reverie::SignalDequeue>', 'VecDeque<OwnedSignalDequeue>')
s=s.replace('    pub(crate) dequeue_acknowledged: Option<reverie::SignalDequeue>,','''    pub(crate) dequeue_acknowledged: Option<reverie::SignalDequeue>,
    pub(crate) dequeue_acknowledged_owner: Option<reverie::SignalTaskIdentity>,
    pub(crate) dequeue_failed: bool,''')
s=s.replace('            dequeue_acknowledged: None,','''            dequeue_acknowledged: None,
            dequeue_acknowledged_owner: None,
            dequeue_failed: false,''')
s=s.replace('            dequeue_acknowledged: self.dequeue_acknowledged,','''            dequeue_acknowledged: self.dequeue_acknowledged,
            dequeue_acknowledged_owner: self.dequeue_acknowledged_owner,
            dequeue_failed: self.dequeue_failed,''')
s=s.replace('    pub(crate) dequeue_identity: Option<reverie::SignalTaskIdentity>,','''    pub(crate) dequeue_identity: Option<reverie::SignalTaskIdentity>,
    pub(crate) dequeue_wake: SignalDequeueWake,''')
s=s.replace('            dequeue_identity: None,','''            dequeue_identity: None,
            dequeue_wake: SignalDequeueWake::default(),''')
s=s.replace('            dequeue_identity: self.dequeue_identity,','''            dequeue_identity: self.dequeue_identity,
            dequeue_wake: self.dequeue_wake.clone(),''');p.write_text(s)
p=Path('reverie-kvm/src/executor.rs');s=p.read_text()
s=s.replace('''        if self
            .parked_signals
            .as_ref()
            .is_some_and(|state| state.site != site)''','''        if !self.signal_site_is_current(site) || self
            .parked_signals
            .as_ref()
            .is_some_and(|state| state.site != site)''',1)
a=s.index('        let (pending, acknowledged) = {',s.index('    pub(crate) fn with_signal_effects('));b=s.index('        let raw_result =',a)
s=s[:a]+'''        let owner = self.admitted_signal_identity();
        let mut process = self.state.process_signals.lock().unwrap_or_else(|p| p.into_inner());
        // Terminal ownership transfer does not acknowledge or roll back any
        // removal. Stop later notification and wake all queued owners into
        // their own consuming cleanup; never steal a sibling's journal.
        process.dequeue_failed = true;
        for entry in &process.dequeue_journal { entry.wake.0.wake(); }
        let acknowledged = process.dequeue_acknowledged.map_or(0, |effect| effect.sequence);
        let has_pending = process.dequeue_journal.iter().any(|entry| entry.owner == owner);
'''+s[b:]
s=s.replace('if pending.is_empty() && state.is_none() && completed.is_empty() {','if !has_pending && state.is_none() && completed.is_empty() {',1)
a=s.index('        for effect in pending {',s.index('    pub(crate) fn with_signal_effects('));b=s.index('        crate::Error::SignalEffects {',a)
s=s[:a]+'''        process.dequeue_journal.retain(|entry| {
            if entry.owner != owner { return true; }
            if !dequeues.iter().any(|old| old.id() == entry.effect.id()) {
                dequeues.push(entry.effect);
            }
            false
        });
        drop(process);
'''+s[b:]
a=s.index('    pub(crate) fn signal_dequeue_front(&self)');b=s.index('    /// Selects one event',a)
s=s[:a]+'''    fn admitted_signal_identity(&self) -> reverie::SignalTaskIdentity {
        reverie::SignalTaskIdentity {
            process: reverie::SignalProcessId { tgid: reverie::Pid::from_raw(self.state.pid), generation: self.process_generation },
            tid: reverie::Pid::from_raw(self.state.tid), task_generation: self.task_generation,
        }
    }

    /// First removal owned by this executor, even when a predecessor must ack.
    pub(crate) fn signal_dequeue_front(&self) -> Option<reverie::SignalDequeue> {
        let owner = self.admitted_signal_identity();
        self.state.process_signals.lock().unwrap_or_else(|p| p.into_inner())
            .dequeue_journal.iter().find(|entry| entry.owner == owner).map(|entry| entry.effect)
    }

    pub(crate) fn poll_signal_dequeue(&self, cx: &mut std::task::Context<'_>)
        -> std::task::Poll<crate::Result<Option<reverie::SignalDequeue>>>
    {
        use std::task::Poll;
        let owner = self.admitted_signal_identity();
        let process = self.state.process_signals.lock().unwrap_or_else(|p| p.into_inner());
        if process.dequeue_failed { return Poll::Ready(Err(crate::Error::RunAborted)); }
        let Some(entry) = process.dequeue_journal.iter().find(|entry| entry.owner == owner) else {
            return Poll::Ready(Ok(None));
        };
        if process.dequeue_journal.front().is_some_and(|front| front.owner == owner) {
            Poll::Ready(Ok(Some(entry.effect)))
        } else {
            // Registration and the predicate share the journal lock, avoiding a
            // lost ack wakeup. No lock survives the pending return or Tool await.
            entry.wake.0.register(cx.waker());
            Poll::Pending
        }
    }

    pub(crate) fn acknowledge_signal_dequeue(&self, effect: reverie::SignalDequeue)
        -> Result<(), reverie::syscalls::Errno>
    {
        let owner = self.admitted_signal_identity();
        let mut process = self.state.process_signals.lock().unwrap_or_else(|p| p.into_inner());
        if process.dequeue_failed { return Err(reverie::syscalls::Errno::EINVAL); }
        if process.dequeue_acknowledged == Some(effect) && process.dequeue_acknowledged_owner == Some(owner) {
            return Ok(());
        }
        if !process.dequeue_journal.front().is_some_and(|entry| entry.owner == owner && entry.effect == effect) {
            return Err(reverie::syscalls::Errno::EINVAL);
        }
        process.dequeue_journal.pop_front();
        process.dequeue_acknowledged = Some(effect);
        process.dequeue_acknowledged_owner = Some(owner);
        let wake = process.dequeue_journal.front().map(|entry| entry.wake.clone());
        drop(process);
        if let Some(wake) = wake { wake.0.wake(); }
        Ok(())
    }

'''+s[b:]
s=s.replace('''        if let Err(errno) = self.reserve_signal_effects(64) {
            return -(i64::from(errno.into_raw()));
        }
''','',1)
s=s.replace('        Some(admitted.process)\n','        Some(admitted)\n',1)
s=s.replace('''        process.dequeue_journal.push_back(reverie::SignalDequeue {
            process: identity,''','''        process.dequeue_journal.push_back(crate::signal::OwnedSignalDequeue {
            owner: identity,
            wake: thread.dequeue_wake.clone(),
            effect: reverie::SignalDequeue {
            process: identity.process,''',1)
s=s.replace('''            event: pending.event,
        });
    }
    Ok(Some(pending))''','''            event: pending.event,
        }});
    }
    Ok(Some(pending))''',1)
p.write_text(s)
p=Path('reverie-kvm/src/runtime.rs');s=p.read_text()
pos='trait GuestSyscallExecutor<T: Tool>: Send + Sync {';a=s.index(pos);b=s.index('    fn failure_subscription',a)
s=s[:b]+'''    /// Reserve backend bookkeeping before executing an injected syscall.
    /// Refusal is terminal backend failure, never an emulated syscall errno.
    fn prepare_signal_effects(&mut self) -> std::result::Result<(), Errno> { Ok(()) }

'''+s[b:]
a=s.index('    fn signal_dequeue_front(&self)',a);b=s.index('    fn acknowledge_signal_dequeue(',a)
s=s[:b]+'''    fn poll_signal_dequeue(&self, _cx: &mut std::task::Context<'_>)
        -> std::task::Poll<Result<Option<reverie::SignalDequeue>>>
    { std::task::Poll::Ready(Ok(self.signal_dequeue_front())) }
'''+s[b:]
s=s.replace('''        if let Err(errno) = self.executor.reserve_signal_effects(64) {
            return -(i64::from(errno.into_raw()));
        }
''','',1)
a=s.index('    fn defer_signal_delivery',s.index('impl<T: Tool> GuestSyscallExecutor<T> for StaticElfSyscallExecutor')) if 'impl<T: Tool> GuestSyscallExecutor<T> for StaticElfSyscallExecutor' in s else s.index('    fn defer_signal_delivery',s.index('let result = self.executor.execute'))
s=s[:a]+'''    fn prepare_signal_effects(&mut self) -> std::result::Result<(), Errno> {
        self.executor.reserve_signal_effects(64)
    }

'''+s[a:]
a=s.index('    fn signal_dequeue_front(&self)',s.index('let result = self.executor.execute'));b=s.index('    fn retain_signal_dequeue',a)
s=s[:b]+'''    fn poll_signal_dequeue(&self, cx: &mut std::task::Context<'_>)
        -> std::task::Poll<Result<Option<reverie::SignalDequeue>>>
    { self.executor.poll_signal_dequeue(cx) }
'''+s[b:]
s=s.replace('''        let raw = self.executor.execute(&request, &self.memory);''','''        if let Err(errno) = self.executor.prepare_signal_effects() {
            let error = self.executor.with_signal_effects(Error::Reverie(errno.into()), None);
            self.signal_handler(HandlerSignal::RuntimeError(error));
            return std::future::pending().await;
        }
        let raw = self.executor.execute(&request, &self.memory);''',1)
s=s.replace('''                } else {
                    let raw = executor.execute(&request, &memory);''','''                } else {
                    executor.reserve_signal_effects(64).map_err(|errno|
                        executor.with_signal_effects(Error::Reverie(errno.into()), None))?;
                    let raw = executor.execute(&request, &memory);''',1)
p.write_text(s)
p=Path('reverie-kvm/src/parked_signal_runtime.rs');s=p.read_text();s=s.replace('''        while let Some(effect) = self.executor.signal_dequeue_front() {''','''        loop {
            let next = futures::future::poll_fn(|cx| self.executor.poll_signal_dequeue(cx)).await;
            let effect = match next {
                Ok(Some(effect)) => effect,
                Ok(None) => break,
                Err(error) => return Err(self.executor.with_signal_effects(error, raw)),
            };''',1);p.write_text(s)
