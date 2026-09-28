/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */
//! Common production connection/birth drain and failure driver. Both the real
//! supervisor and the existing generic-server lifecycle controls call this code.
use std::future::Future;
use std::io;
use std::time::Duration;

use reverie_rpc_transport::ConnectionMonitor;
use reverie_rpc_transport::RpcError;

pub(crate) const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) enum Lifetime {
    Rpc(ConnectionMonitor),
    Birth(super::supervisor::Monitor),
}
impl Lifetime {
    fn active_connections(&self) -> usize {
        match self {
            Self::Rpc(value) => value.active_connections(),
            Self::Birth(value) => value.active(),
        }
    }
    async fn wait_for_idle(&self) {
        match self {
            Self::Rpc(value) => value.wait_for_idle().await,
            Self::Birth(value) => value.idle().await,
        }
    }
}

fn rpc_server_stopped(
    result: Option<Result<Result<(), RpcError>, tokio::task::JoinError>>,
) -> io::Error {
    let message = match result {
        Some(Ok(Ok(()))) => "LiteInst coordinator stopped unexpectedly".to_owned(),
        Some(Ok(Err(error))) => error.to_string(),
        Some(Err(error)) => error.to_string(),
        None => "LiteInst coordinator task disappeared".to_owned(),
    };
    io::Error::other(message)
}

pub(crate) struct Completion<T> {
    pub(crate) result: io::Result<T>,
    pub(crate) cleanup_deadline: tokio::time::Instant,
}

pub(crate) async fn run_tasks_until<F, T>(
    mut serving: tokio::task::JoinSet<Result<(), RpcError>>,
    connection_monitors: Vec<Lifetime>,
    completion: F,
    drain_timeout: Duration,
) -> Completion<T>
where
    F: Future<Output = io::Result<T>>,
{
    // Return/error selection is scoped so every path reaches owned task
    // cancellation and joins below, including an early failed server.
    let mut cleanup_deadline = None;
    let mut result = async {
        tokio::pin!(completion);

        let mut result = tokio::select! {
            biased;
            result = &mut completion => result,
            result = serving.join_next() => return Err(rpc_server_stopped(result)),
        };

        let deadline = tokio::time::Instant::now() + drain_timeout;
        cleanup_deadline = Some(deadline);

        // Generic streams retain their ordinary last-close monitor. The production
        // supervisor additionally holds a birth lease before delivering a pre-fork
        // endpoint and through child reconnection. Neither lifetime can transiently
        // disappear while its owner is still active. Wait for notifications with
        // the original finite drain bound, including failed or unclaimed births.
        let drain = async {
            for monitor in &connection_monitors {
                monitor.wait_for_idle().await;
            }
        };
        tokio::pin!(drain);
        let drain_result = tokio::select! {
            biased;
            result = serving.join_next() => return Err(rpc_server_stopped(result)),
            result = tokio::time::timeout_at(deadline, &mut drain) => result,
        };
        if drain_result.is_err() {
            let active_connections = connection_monitors
                .iter()
                .map(Lifetime::active_connections)
                .sum::<usize>();
            let timeout_message = format!(
                "LiteInst coordinator retained {active_connections} active RPC connection(s) for {}ms after guest exit",
                drain_timeout.as_millis()
            );
            if result.is_ok() {
                result = Err(io::Error::new(io::ErrorKind::TimedOut, timeout_message));
            } else {
                tracing::warn!("{timeout_message}; preserving guest completion error");
            }
        }

        result
    }.await;

    let cleanup_deadline =
        cleanup_deadline.unwrap_or_else(|| tokio::time::Instant::now() + drain_timeout);
    serving.abort_all();
    let mut shutdown_error = None;
    while let Some(server_result) = serving.join_next().await {
        let error = match server_result {
            Err(error) if error.is_cancelled() => None,
            Ok(Ok(())) => Some(io::Error::other(
                "LiteInst coordinator stopped unexpectedly",
            )),
            Ok(Err(error)) => Some(io::Error::other(error.to_string())),
            Err(error) => Some(io::Error::other(error.to_string())),
        };
        if shutdown_error.is_none() {
            shutdown_error = error;
        } else if let Some(error) = error {
            tracing::warn!("additional coordinator shutdown error: {error}");
        }
    }
    if let Some(error) = shutdown_error {
        if result.is_err() {
            tracing::warn!(
                "coordinator shutdown error: {error}; preserving selected completion/service error"
            );
        } else {
            result = Err(error);
        }
    }
    Completion {
        result,
        cleanup_deadline,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use super::*;

    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[tokio::test]
    async fn early_server_error_joins_other_owned_tasks_before_return() {
        let dropped = Arc::new(AtomicBool::new(false));
        let held = Dropped(dropped.clone());
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            let _held = held;
            started.send(()).unwrap();
            std::future::pending::<Result<(), RpcError>>().await
        });
        tasks.spawn(async move {
            ready.await.unwrap();
            Err(io::Error::other("selected server failure").into())
        });
        let result = run_tasks_until(
            tasks,
            Vec::new(),
            std::future::pending::<io::Result<()>>(),
            Duration::from_secs(1),
        )
        .await;
        assert!(
            result
                .result
                .unwrap_err()
                .to_string()
                .contains("selected server failure")
        );
        assert!(
            dropped.load(Ordering::Acquire),
            "return preceded actual owned future destruction"
        );
    }
}
