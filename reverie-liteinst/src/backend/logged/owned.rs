use super::*;

#[cfg(test)]
thread_local! {
    pub(super) static TEST_RUNTIME: std::cell::RefCell<Option<tokio::runtime::Runtime>> = const { std::cell::RefCell::new(None) };
}

fn worker_runtime(
    #[cfg(test)] runtime: Option<tokio::runtime::Runtime>,
) -> io::Result<tokio::runtime::Runtime> {
    #[cfg(test)]
    if let Some(runtime) = runtime {
        return Ok(runtime);
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

type SpawnCheck<O> = Box<dyn FnOnce(&O, &mut std::process::Command) -> Result<(), Error> + Send>;
type Cleanup<O> = Box<dyn FnOnce(&mut O) -> Result<(), Error> + Send>;

pub struct PreparedCommand<O: Send + 'static> {
    command: std::process::Command,
    owner: O,
    check: Option<SpawnCheck<O>>,
    cleanup: Option<Cleanup<O>>,
}

impl<O: Send + 'static> PreparedCommand<O> {
    pub fn new(command: std::process::Command, owner: O) -> Self {
        Self {
            command,
            owner,
            check: None,
            cleanup: None,
        }
    }

    pub fn with_spawn_check(
        mut self,
        check: impl FnOnce(&O, &mut std::process::Command) -> Result<(), Error> + Send + 'static,
    ) -> Self {
        self.check = Some(Box::new(check));
        self
    }

    pub fn with_cleanup(
        mut self,
        cleanup: impl FnOnce(&mut O) -> Result<(), Error> + Send + 'static,
    ) -> Self {
        self.cleanup = Some(Box::new(cleanup));
        self
    }
}

pub(super) trait LaunchLifetime: Send {
    fn before_spawn(&mut self, command: &mut std::process::Command) -> Result<(), Error>;
    fn spawning(&mut self);
    fn spawn_failed(&mut self);
    fn reaped(&mut self);
}

struct Lifetime<O: Send + 'static> {
    owner: Option<O>,
    check: Option<SpawnCheck<O>>,
    pending: bool,
    handle: LogHandle,
}

impl<O: Send + 'static> LaunchLifetime for Lifetime<O> {
    fn before_spawn(&mut self, command: &mut std::process::Command) -> Result<(), Error> {
        if let Some(check) = self.check.take() {
            check(self.owner.as_ref().expect("retained launch owner"), command)?;
        }
        Ok(())
    }

    fn spawning(&mut self) {
        self.pending = true;
    }

    fn spawn_failed(&mut self) {
        self.pending = false;
    }

    fn reaped(&mut self) {
        self.pending = false;
    }
}

impl<O: Send + 'static> Drop for Lifetime<O> {
    fn drop(&mut self) {
        if !self.pending {
            return;
        }
        self.handle.issue(
            IssueKind::Cleanup,
            "launch resources retained: child reap is unconfirmed",
        );
        std::mem::forget(self.owner.take());
    }
}

pub(in crate::backend) fn prepare<T: Tool + 'static, O: Send + 'static>(
    prepared: PreparedCommand<O>,
    config: <T::GlobalState as GlobalTool>::Config,
    tool_data: Vec<u8>,
    sink: LogSink,
    mode: StdioMode,
) -> (
    RunObserver,
    impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
) {
    prepare_configured::<T, O, _>(prepared, config, tool_data, sink, mode, |_, _, _| Ok(()))
}

pub(in crate::backend) fn prepare_configured<T, O, F>(
    prepared: PreparedCommand<O>,
    mut config: <T::GlobalState as GlobalTool>::Config,
    tool_data: Vec<u8>,
    sink: LogSink,
    mode: StdioMode,
    configure: F,
) -> (
    RunObserver,
    impl Future<Output = Result<(Output, T::GlobalState), LoggedRunError>>,
)
where
    T: Tool + 'static,
    O: Send + 'static,
    F: FnOnce(
            &mut O,
            &std::process::Command,
            &mut <T::GlobalState as GlobalTool>::Config,
        ) -> Result<(), Error>
        + Send
        + 'static,
{
    #[cfg(test)]
    let test_runtime = TEST_RUNTIME.with(|runtime| runtime.borrow_mut().take());
    let handle = sink.handle();
    let observer = RunObserver::new(mode);
    let retained = observer.clone();
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    let parent_span = tracing::Span::current();
    let mut caller = Owner {
        handle: handle.clone(),
        evidence: observer.clone(),
        completed: false,
        caller: true,
    };
    let future = async move {
        observer.polled();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("liteinst-launch".into())
            .spawn(move || {
                let dispatch_guard = tracing::dispatcher::set_default(&dispatch);
                let span_guard = parent_span.entered();
                let mut worker = Owner {
                    handle: handle.clone(),
                    evidence: observer.clone(),
                    completed: false,
                    caller: false,
                };
                let mut evidence = Evidence {
                    handle: handle.clone(),
                    rpc: None,
                    run: observer.clone(),
                };
                let PreparedCommand {
                    mut command,
                    owner,
                    check,
                    mut cleanup,
                } = prepared;
                let mut lifetime = Lifetime {
                    owner: Some(owner),
                    check,
                    pending: false,
                    handle: handle.clone(),
                };
                let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if handle.stopped() {
                        return Err(cancelled());
                    }
                    match mode {
                        StdioMode::Captured => {
                            command
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped());
                        }
                        StdioMode::Inherited => {
                            command
                                .stdout(std::process::Stdio::inherit())
                                .stderr(std::process::Stdio::inherit());
                        }
                    }
                    configure_owned_in_guest_address_space(&mut command);
                    configure(
                        lifetime.owner.as_mut().expect("retained launch owner"),
                        &command,
                        &mut config,
                    )?;
                    if handle.stopped() {
                        return Err(cancelled());
                    }
                    let runtime = worker_runtime(
                        #[cfg(test)]
                        test_runtime,
                    )?;
                    runtime.block_on(execute_with_launch::<T>(
                        command,
                        config,
                        tool_data,
                        sink,
                        &mut evidence,
                        &mut lifetime,
                    ))
                }))
                .unwrap_or_else(|_| Err(io::Error::other("owned logged launch unwound").into()));
                if !lifetime.pending {
                    if let Some(cleanup) = cleanup.take() {
                        let cleaned =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                cleanup(lifetime.owner.as_mut().expect("retained launch owner"))
                            }))
                            .unwrap_or_else(|_| {
                                Err(io::Error::other("owned cleanup callback unwound").into())
                            });
                        if let Err(error) = cleaned {
                            evidence.run.cleanup_unwound(&error);
                            handle.issue(IssueKind::Cleanup, &error);
                            if result.is_ok() {
                                result = Err(error);
                            }
                        }
                    }
                } else {
                    std::mem::forget(cleanup.take());
                }
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(lifetime)))
                    .is_err()
                {
                    let error = io::Error::other("owned launch cleanup unwound");
                    evidence.run.cleanup_unwound(&error);
                    handle.issue(IssueKind::Cleanup, &error);
                    if result.is_ok() {
                        result = Err(error.into());
                    }
                }
                evidence.run.finished(result.as_ref().err());
                if let Err(error) = &result {
                    handle.run_state(RunState::Failed);
                    handle.stop(IssueKind::Child, error);
                } else {
                    handle.run_state(RunState::Succeeded);
                }
                worker.complete();
                let result = result.map_err(|error| evidence.error(error));
                drop(span_guard);
                let _ = sender.send(result);
                drop(dispatch_guard);
            });
        let result = match thread {
            Ok(thread) => {
                drop(thread);
                caller.evidence.worker_submitted();
                receiver
                    .await
                    .map_err(|error| io::Error::other(error).into())
            }
            Err(error) => Err(error.into()),
        };
        caller.complete();
        result.unwrap_or_else(|error| {
            caller.evidence.finished(Some(&error));
            caller.handle.run_state(RunState::Failed);
            caller.handle.stop(IssueKind::Startup, &error);
            Err(Evidence {
                handle: caller.handle.clone(),
                rpc: None,
                run: caller.evidence.clone(),
            }
            .error(error))
        })
    };
    (retained, future)
}

#[cfg(test)]
mod tests;
