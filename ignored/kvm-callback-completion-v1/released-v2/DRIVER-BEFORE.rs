async fn drive_handler<T>(
    future: impl Future<Output = T>,
    handler_signal: SharedHandlerSignal,
    pending_child_starts: SharedChildStarts,
    failure: impl Future<Output = ()>,
) -> HandlerOutcome<T> {
    // The callback may already have moved irreversible effects into this
    // terminal error. Failure still wins over ordinary values/child starts,
    // but must not replace the owned error with an empty cancellation marker.
    let terminal = || match handler_signal
        .lock()
        .expect("KVM handler signal lock poisoned")
        .take()
    {
        Some(HandlerSignal::RuntimeError(error)) => HandlerOutcome::RuntimeError(error),
        _ => HandlerOutcome::RunFailed,
    };
    // Own the pinned allocation: dropping a Pin<&mut F> would only end a
    // borrow. Any caller's callback receipt must follow destruction of this
    // future, including an inline RPC that was pending when failure arrived.
    let mut future = Box::pin(future);
    let mut failure = pin!(failure);
    let outcome = poll_fn(|context| {
        if failure.as_mut().poll(context).is_ready() {
            return Poll::Ready(terminal());
        }
        let result = future.as_mut().poll(context);
        if failure.as_mut().poll(context).is_ready() {
            return Poll::Ready(terminal());
        }
        // A callback can select another ready future after an operation records
        // a nonlocal result. Consume that result before accepting either a
        // returned callback value or an ordinary suspension.
        let handler_signal = handler_signal
            .lock()
            .expect("KVM handler signal lock poisoned")
            .take();
        match handler_signal {
            Some(HandlerSignal::ParkedFatal(selection)) => {
                return Poll::Ready(HandlerOutcome::ParkedFatal(selection));
            }
            Some(HandlerSignal::ParkedCancelled(context)) => {
                return Poll::Ready(HandlerOutcome::ParkedCancelled(context));
            }
            Some(HandlerSignal::ParkedRetired(context)) => {
                return Poll::Ready(HandlerOutcome::ParkedRetired(context));
            }
            Some(HandlerSignal::ThreadCancelled) => {
                return Poll::Ready(HandlerOutcome::ThreadCancelled);
            }
            Some(HandlerSignal::ThreadRetired) => {
                return Poll::Ready(HandlerOutcome::ThreadRetired);
            }
            Some(HandlerSignal::TailInjected {
                result,
                image_replaced,
                process_exited,
            }) => {
                return Poll::Ready(HandlerOutcome::TailInjected {
                    result,
                    image_replaced,
                    process_exited,
                });
            }
            Some(HandlerSignal::RuntimeError(error)) => {
                return Poll::Ready(HandlerOutcome::RuntimeError(error));
            }
            None => {}
        }
        match result {
            Poll::Ready(result) => Poll::Ready(HandlerOutcome::Returned(result)),
            Poll::Pending => {
                let mut starts = pending_child_starts
                    .lock()
                    .expect("KVM child-start lock poisoned");
                if starts.iter().any(|start| start.start().is_err()) {
                    return Poll::Ready(HandlerOutcome::RuntimeError(Error::UnexpectedVcpuExit(
                        "KVM child exited before its parent suspended registration".to_owned(),
                    )));
                }
                starts.clear();
                Poll::Pending
            }
        }
    })
    .await;
    drop(future);
    outcome
}

