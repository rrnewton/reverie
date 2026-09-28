#[allow(clippy::too_many_arguments)]
async fn notify_tool_exit_with_panics<T: Tool>(
    tool: Arc<T>,
    pid: Pid,
    tid: Pid,
    global_state: &T::GlobalState,
    config: &<T::GlobalState as GlobalTool>::Config,
    thread_state: T::ThreadState,
    exit: ToolExit,
    failure: Option<&FailureContext>,
    panics: &crate::failure::tool_panics::ToolPanics,
) -> Result<()> {
    use crate::failure::owned_future::CaughtFuture;
    use crate::failure::owned_future::catch_owned_future;

    // on_exit_thread deregisters this thread from the scheduler, so its RPCs
    // must be attributed to the exiting thread's tid.
    let thread_global = KvmGlobal {
        tid,
        state: global_state,
        config,
    };
    let caught = catch_owned_future(tool.on_exit_thread(
        tid,
        &thread_global,
        thread_state,
        exit.status,
    ))
    .await;
    let thread_result = panics
        .finish(
            CaughtFuture {
                output: caught.output.map(|result| result.map_err(Error::Reverie)),
                panics: caught.panics,
            },
            "thread exit hook",
        )
        .map_err(|error| match failure {
            Some(failure) => failure.publish("thread exit hook", error),
            None => {
                global_state.report_backend_failure(reverie::BackendFailure {
                    pid,
                    tid,
                    phase: "thread exit hook",
                });
                error
            }
        });
    if !exit.process_exited {
        return thread_result;
    }
    // The process-exit hook belongs to the thread-group leader (tid == pid).
    let process_global = KvmGlobal {
        tid: pid,
        state: global_state,
        config,
    };
    // A failed thread hook has still consumed ThreadState. Attempt the
    // process hook as well, after every worker has dropped its process Arc.
    let process_result = match Arc::try_unwrap(tool) {
        Ok(tool) => {
            let caught = catch_owned_future(tool.on_exit_process(
                pid,
                &process_global,
                exit.status,
            ))
            .await;
            panics.finish(
                CaughtFuture {
                    output: caught.output.map(|result| result.map_err(Error::Reverie)),
                    panics: caught.panics,
                },
                "process exit hook",
            )
        }
        Err(_) => Err(Error::UnexpectedVcpuExit(
            "KVM worker retained process Tool state after exit".to_owned(),
        )),
    };
    let process_result = process_result.map_err(|error| match failure {
        Some(failure) => failure.publish("process exit hook", error),
        None => {
            global_state.report_backend_failure(reverie::BackendFailure {
                pid,
                tid,
                phase: "process exit hook",
            });
            error
        }
    });
    match (thread_result, process_result) {
        (Ok(()), process) => process,
        (Err(error), Ok(())) => Err(error),
        (Err(thread), Err(process)) => Err(thread.with_cleanup(vec![process])),
    }
}

