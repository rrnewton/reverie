
#[test]
fn constructor_panic_recovers_owned_signal_before_failure_future_destruction() {
    struct ConstructionFailure {
        signal: SharedHandlerSignal,
        weak: Weak<Error>,
        counts: Arc<Counts>,
        polls: Arc<AtomicUsize>,
        drop_panic: Option<PanicPayload>,
    }

    impl Future for ConstructionFailure {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            panic!("construction failure must not poll the failure future");
        }
    }

    impl Drop for ConstructionFailure {
        fn drop(&mut self) {
            assert_eq!(self.counts.polls.load(Ordering::SeqCst), 0);
            assert_eq!(self.counts.drops.load(Ordering::SeqCst), 0);
            assert_eq!(self.polls.load(Ordering::SeqCst), 0);
            assert_eq!(self.counts.failure_drops.fetch_add(1, Ordering::SeqCst), 0);
            assert!(self.signal.is_poisoned());
            assert!(
                self.signal
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_none(),
                "constructor outcome was not recovered before failure destruction"
            );
            let retained = self.weak.upgrade().expect("constructor error was discarded");
            assert_eq!(Arc::strong_count(&retained), 2);
            drop(retained);
            if let Some(payload) = self.drop_panic.take() {
                resume_unwind(payload);
            }
        }
    }

    for failure_drop_panics in [false, true] {
        let (error, expected) = owned_error();
        let signal = Arc::new(Mutex::new(None));
        let starts = Arc::new(Mutex::new(Vec::new()));
        let (sender, receiver) = std::sync::mpsc::channel();
        let gate = ChildStartGate::new(sender);
        starts
            .lock()
            .unwrap()
            .push(PendingChildStart::tool_thread(77, gate.clone()));
        let counts = Arc::new(Counts::default());
        let constructions = Arc::new(AtomicUsize::new(0));
        let failure_polls = Arc::new(AtomicUsize::new(0));
        let (constructor_payload, constructor_address) = payload("callback constructor");
        let mut expected_payloads = vec![(constructor_address, "callback constructor")];
        let failure_payload = failure_drop_panics.then(|| {
            let (payload, address) = payload("failure drop after constructor");
            expected_payloads.push((address, "failure drop after constructor"));
            payload
        });
        let failure = ConstructionFailure {
            signal: signal.clone(),
            weak: expected.weak.clone(),
            counts: counts.clone(),
            polls: failure_polls.clone(),
            drop_panic: failure_payload,
        };
        let callback_signal = signal.clone();
        let constructor_calls = constructions.clone();
        // This is synchronous construction, not an async block whose first
        // poll panics. No Callback allocation is created or returned.
        let build = move || -> Callback {
            assert_eq!(constructor_calls.fetch_add(1, Ordering::SeqCst), 0);
            let mut slot = callback_signal.lock().unwrap();
            assert!(slot.is_none());
            *slot = Some(HandlerSignal::RuntimeError(error));
            // Transfer the only strong error owner, then poison the real slot.
            resume_unwind(constructor_payload);
        };
        let mut driven = Box::pin(drive_handler_completion_from(
            build,
            signal.clone(),
            starts.clone(),
            failure,
        ));
        fn require_send<T: Send>(_: &T) {}
        require_send(&driven);
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(completion) = driven.as_mut().poll(&mut cx) else {
            panic!("failed callback construction stayed pending");
        };
        drop(driven);
        assert_eq!(constructions.load(Ordering::SeqCst), 1);
        assert_eq!(counts.polls.load(Ordering::SeqCst), 0);
        assert_eq!(counts.drops.load(Ordering::SeqCst), 0);
        assert_eq!(failure_polls.load(Ordering::SeqCst), 0);
        assert_eq!(counts.failure_drops.load(Ordering::SeqCst), 1);
        assert!(signal.is_poisoned());
        assert!(
            signal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none()
        );
        assert_eq!(starts.lock().unwrap().len(), 1);
        assert!(gate.is_pending(), "constructor panic started a child");
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        let Some(HandlerOutcome::RuntimeError(ref error)) = completion.output else {
            panic!("construction panic replaced the transferred RuntimeError");
        };
        assert_error(error, &expected);
        assert_eq!(completion.panics.len(), expected_payloads.len());
        for (actual, (address, label)) in completion.panics.iter().zip(expected_payloads) {
            let actual = actual.downcast_ref::<Payload>().unwrap();
            assert_eq!(std::ptr::from_ref(actual) as usize, address);
            assert_eq!(actual.label, label);
        }
        drop(completion);
        assert!(expected.weak.upgrade().is_none());
    }
}
