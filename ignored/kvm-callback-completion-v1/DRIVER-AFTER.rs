async fn drive_handler_completion<T>(
    future: impl Future<Output = T>,
    handler_signal: SharedHandlerSignal,
    pending_child_starts: SharedChildStarts,
    failure: impl Future<Output = ()>,
) -> crate::failure::owned_future::CaughtFuture<HandlerOutcome<T>> {
    use std::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;

    use crate::failure::owned_future::catch_owned_future;

    let into_outcome = |signal| match signal {
        HandlerSignal::ParkedFatal(selection) => HandlerOutcome::ParkedFatal(selection),
        HandlerSignal::ParkedCancelled(context) => HandlerOutcome::ParkedCancelled(context),
        HandlerSignal::ParkedRetired(context) => HandlerOutcome::ParkedRetired(context),
        HandlerSignal::ThreadCancelled => HandlerOutcome::ThreadCancelled,
        HandlerSignal::ThreadRetired => HandlerOutcome::ThreadRetired,
        HandlerSignal::TailInjected {
            result,
            image_replaced,
            process_exited,
        } => HandlerOutcome::TailInjected {
            result,
            image_replaced,
            process_exited,
        },
        HandlerSignal::RuntimeError(error) => HandlerOutcome::RuntimeError(error),
    };
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
    let mut failure = Box::pin(failure);
    // This driver only borrows the callback. Its caught poll cannot destroy the
    // callback allocation or an outcome already moved out of the signal slot.
    let mut completion = catch_owned_future(poll_fn(|context| {
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
        if let Some(signal) = handler_signal {
            return Poll::Ready(into_outcome(signal));
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
    }))
    .await;
    if completion.output.is_none() {
        // Polling may have stored the original error and then panicked while
        // holding this mutex. Keep that owned result before callback Drop can
        // panic too; lock poison must not replace the already-caught payload.
        completion.output = handler_signal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .map(into_outcome);
    }
    // Keep the selected outcome outside both destruction catches. Neither a
    // callback panic nor destruction of the failure future may discard it.
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(future))) {
        completion.panics.push(payload);
    }
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(failure))) {
        completion.panics.push(payload);
    }
    completion
}

/// Compatibility for the old non-panicking controls. Production callers must
/// retain both fields of the completion instead of using this wrapper.
#[cfg(test)]
async fn drive_handler<T>(
    future: impl Future<Output = T>,
    handler_signal: SharedHandlerSignal,
    pending_child_starts: SharedChildStarts,
    failure: impl Future<Output = ()>,
) -> HandlerOutcome<T> {
    let mut completion =
        drive_handler_completion(future, handler_signal, pending_child_starts, failure).await;
    if !completion.panics.is_empty() {
        std::panic::resume_unwind(completion.panics.remove(0));
    }
    completion
        .output
        .expect("non-panicking callback driver returned no outcome")
}

