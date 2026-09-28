
// These methods panic before returning a BoxFuture. Keep this fixture separate
// so the original polling/destruction controls and assertions stay unchanged.
#[derive(Default)]
struct ConstructorTool {
    global: Option<Arc<HookGlobal>>,
    thread_constructor_panic: Mutex<Option<PanicPayload>>,
    process_constructor_panic: Mutex<Option<PanicPayload>>,
}

struct ProcessAfterConstructorFuture {
    global: Arc<HookGlobal>,
}

impl Future for ProcessAfterConstructorFuture {
    type Output = std::result::Result<(), reverie::Error>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        assert_eq!(self.global.process_counts.polls.fetch_add(1, Ordering::SeqCst), 0);
        let failures = self.global.failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].phase, "thread exit hook");
        Poll::Ready(Ok(()))
    }
}

impl Drop for ProcessAfterConstructorFuture {
    fn drop(&mut self) {
        assert_eq!(self.global.process_counts.polls.load(Ordering::SeqCst), 1);
        assert_eq!(self.global.process_counts.drops.fetch_add(1, Ordering::SeqCst), 0);
    }
}

impl Tool for ConstructorTool {
    type GlobalState = HookGlobal;
    type ThreadState = u64;

    fn on_exit_thread<'life0, 'life1, 'async_trait, G>(
        &'life0 self,
        tid: Pid,
        _global: &'life1 G,
        thread_state: u64,
        status: ExitStatus,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), reverie::Error>> + Send + 'async_trait>>
    where
        G: GlobalRPC<HookGlobal> + 'async_trait,
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        assert_eq!(tid, Pid::from_raw(19));
        assert_eq!(thread_state, 41);
        assert_eq!(status, ExitStatus::Exited(13));
        let global = self.global.as_ref().unwrap().clone();
        assert_eq!(global.thread_counts.constructed.fetch_add(1, Ordering::SeqCst), 0);
        assert!(global.failures.lock().unwrap().is_empty());
        let panic = self.thread_constructor_panic.lock().unwrap().take();
        if let Some(payload) = panic {
            resume_unwind(payload);
        }
        let mut state = global.thread.lock().unwrap().take().unwrap();
        state.thread_state = Some(thread_state);
        Box::pin(HookFuture { global, process: false, state })
    }

    fn on_exit_process<'life0, 'async_trait, G>(
        self,
        pid: Pid,
        _global: &'life0 G,
        status: ExitStatus,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), reverie::Error>> + Send + 'async_trait>>
    where
        G: GlobalRPC<HookGlobal> + 'async_trait,
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        assert_eq!(pid, Pid::from_raw(19));
        assert_eq!(status, ExitStatus::Exited(13));
        let global = self.global.unwrap();
        assert_eq!(global.process_counts.constructed.fetch_add(1, Ordering::SeqCst), 0);
        {
            let failures = global.failures.lock().unwrap();
            assert_eq!(failures.len(), 1, "process construction preceded thread failure publication");
            assert_eq!(failures[0].phase, "thread exit hook");
        }
        let panic = self.process_constructor_panic.lock().unwrap().take();
        if let Some(payload) = panic {
            assert_eq!(global.thread_counts.polls.load(Ordering::SeqCst), 1);
            assert_eq!(global.thread_counts.drops.load(Ordering::SeqCst), 1);
            resume_unwind(payload);
        }
        assert_eq!(global.thread_counts.polls.load(Ordering::SeqCst), 0);
        assert_eq!(global.thread_counts.drops.load(Ordering::SeqCst), 0);
        Box::pin(ProcessAfterConstructorFuture { global })
    }
}

fn check_constructor_panic(thread_constructor: bool, with_failure: bool) {
    let global = Arc::new(HookGlobal::default());
    let mut expected_errors = Vec::new();
    let mut expected_payloads = Vec::new();
    let mut make_payload = |label: &'static str| {
        let (payload, address, drops) = payload(label);
        expected_payloads.push((address, label, drops));
        payload
    };
    let (thread_panic, process_panic) = if thread_constructor {
        (Some(make_payload("thread constructor")), None)
    } else {
        let (error, address) = typed_error("thread error before process constructor");
        expected_errors.push((address, "thread error before process constructor"));
        *global.thread.lock().unwrap() = Some(HookState {
            outcome: Some(Err(error)),
            release: None,
            poll_panic: None,
            drop_panic: Some(make_payload("thread drop before process constructor")),
            thread_state: None,
        });
        (None, Some(make_payload("process constructor")))
    };
    let run = RunFailure::new(&global);
    let failure = FailureContext::new(run.clone(), Pid::from_raw(19), Pid::from_raw(19));
    let panics = ToolPanics::default();
    let mut notify = Box::pin(notify_tool_exit_with_panics(
        Arc::new(ConstructorTool {
            global: Some(global.clone()),
            thread_constructor_panic: Mutex::new(thread_panic),
            process_constructor_panic: Mutex::new(process_panic),
        }),
        Pid::from_raw(19),
        Pid::from_raw(19),
        global.as_ref(),
        &(),
        41,
        ToolExit { status: ExitStatus::Exited(13), process_exited: true },
        with_failure.then_some(&failure),
        &panics,
    ));
    fn require_send<T: Send>(_: &T) {}
    require_send(&notify);
    let mut cx = Context::from_waker(Waker::noop());
    let Poll::Ready(result) = notify.as_mut().poll(&mut cx) else {
        panic!("finite constructor controls stayed pending");
    };
    drop(notify);
    let error = result.unwrap_err();
    assert_eq!(global.thread_counts.constructed.load(Ordering::SeqCst), 1);
    assert_eq!(global.process_counts.constructed.load(Ordering::SeqCst), 1);
    let (thread_futures, process_futures) = if thread_constructor { (0, 1) } else { (1, 0) };
    assert_eq!(global.thread_counts.polls.load(Ordering::SeqCst), thread_futures);
    assert_eq!(global.thread_counts.drops.load(Ordering::SeqCst), thread_futures);
    assert_eq!(global.process_counts.polls.load(Ordering::SeqCst), process_futures);
    assert_eq!(global.process_counts.drops.load(Ordering::SeqCst), process_futures);
    assert!(global.thread.lock().unwrap().is_none());
    assert!(global.process.lock().unwrap().is_none());

    let expected_phases = if thread_constructor {
        vec!["thread exit hook"]
    } else {
        vec!["thread exit hook", "process exit hook"]
    };
    let failures = global.failures.lock().unwrap();
    assert_eq!(failures.iter().map(|event| event.phase).collect::<Vec<_>>(), expected_phases);
    for event in failures.iter() {
        assert_eq!(event.pid, Pid::from_raw(19));
        assert_eq!(event.tid, Pid::from_raw(19));
    }
    let mut actual_errors = Vec::new();
    let mut actual_phases = Vec::new();
    inspect(&error, &mut actual_errors, &mut actual_phases);
    assert_eq!(actual_errors, expected_errors);
    assert_eq!(actual_phases, expected_phases);
    if with_failure {
        let first = run.published_primary().unwrap();
        assert!(error.retains_primary(&first));
        let mut first_errors = Vec::new();
        let mut first_phases = Vec::new();
        inspect(&first, &mut first_errors, &mut first_phases);
        assert_eq!(first_errors, expected_errors);
        assert_eq!(first_phases, vec!["thread exit hook"]);
    } else {
        assert!(run.published_primary().is_none());
    }

    let payloads = panics.take();
    assert_eq!(payloads.len(), expected_payloads.len());
    assert!(panics.take().is_empty());
    for (actual, (address, label, drops)) in payloads.iter().zip(&expected_payloads) {
        let actual = actual.downcast_ref::<Payload>().unwrap();
        assert_eq!(std::ptr::from_ref(actual) as usize, *address);
        assert_eq!(actual.label, *label);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    }
    drop(payloads);
    for (_, _, drops) in expected_payloads { assert_eq!(drops.load(Ordering::SeqCst), 1); }
}

#[test]
fn thread_constructor_panic_is_published_before_process_hook_runs() {
    for with_failure in [false, true] { check_constructor_panic(true, with_failure); }
}

#[test]
fn process_constructor_panic_retains_prior_thread_error_and_drop_payload() {
    for with_failure in [false, true] { check_constructor_panic(false, with_failure); }
}
