use std::future::poll_fn;
use std::io;
use std::sync::Arc;
use std::task::Poll;
use std::task::Waker;

use super::SpinMutex;

struct State {
    active: usize,
    closing: bool,
    failure: Option<i32>,
    waiter: Option<Waker>,
}

#[derive(Clone)]
pub(crate) struct Resources(Arc<SpinMutex<State>>);

pub(crate) struct Lease {
    resources: Resources,
}

impl Resources {
    pub(crate) fn new() -> Self {
        Self(Arc::new(SpinMutex::new(State {
            active: 0,
            closing: false,
            failure: None,
            waiter: None,
        })))
    }

    pub(crate) fn acquire(&self) -> io::Result<Lease> {
        let mut state = self.0.lock();
        if state.closing || state.failure.is_some() {
            drop(state);
            return Err(io::Error::other(
                "RPC resource lifetime is closing or failed",
            ));
        }
        let Some(active) = state.active.checked_add(1) else {
            drop(state);
            return Err(io::Error::other("RPC resource count exhausted"));
        };
        state.active = active;
        Ok(Lease {
            resources: self.clone(),
        })
    }

    pub(crate) fn close(&self) {
        self.0.lock().closing = true;
    }

    pub(crate) async fn drain(&self) -> io::Result<()> {
        poll_fn(|context| {
            let replacement = context.waker().clone();
            let mut state = self.0.lock();
            if !state.closing {
                drop(state);
                return Poll::Ready(Err(io::Error::other("RPC resource admission is open")));
            }
            if state.active == 0 {
                return Poll::Ready(match state.failure {
                    Some(errno) => Err(io::Error::from_raw_os_error(errno)),
                    None => Ok(()),
                });
            }
            let old = state.waiter.replace(replacement);
            drop(state);
            drop(old);
            Poll::Pending
        })
        .await
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> usize {
        self.0.lock().active
    }
}

impl Lease {
    pub(super) fn failed(&self, errno: i32) {
        let mut state = self.resources.0.lock();
        state.failure.get_or_insert(errno);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let waiter = {
            let mut state = self.resources.0.lock();
            state.active = state.active.checked_sub(1).expect("RPC resource underflow");
            if state.active == 0 {
                state.waiter.take()
            } else {
                None
            }
        };
        if let Some(waiter) = waiter {
            waiter.wake();
        }
    }
}
