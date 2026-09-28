/// Construct one future inside a catch, then catch its polling and destruction.
///
/// A Tool method may execute synchronous code before returning its future. Keep
/// that call in `build`; evaluating it before calling this helper is too late.
/// A construction panic has no returned future or output to poll or destroy.
/// As with `catch_owned_future`, a double panic during unwinding can abort.
pub(crate) async fn catch_owned_future_from<B, F>(build: B) -> CaughtFuture<F::Output>
where
    B: FnOnce() -> F,
    F: Future,
{
    match catch_unwind(AssertUnwindSafe(build)) {
        Ok(future) => catch_owned_future(future).await,
        Err(payload) => CaughtFuture {
            output: None,
            panics: vec![payload],
        },
    }
}

