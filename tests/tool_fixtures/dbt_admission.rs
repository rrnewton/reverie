// Copyright (c) Meta Platforms, Inc. and affiliates.
// Licensed under the BSD-style license in the repository LICENSE file.

use std::sync::Mutex;

use reverie::Error;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Subscription;
use reverie::Tool;
use reverie::syscalls::Errno;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallArgs;
use reverie::syscalls::Sysno;
use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Restart,
    Inject(i64),
    Tail(i64),
    Error,
    FailStart,
    FailPostExec,
}

#[derive(Default)]
pub struct AdmissionGlobal(pub Mutex<Vec<u64>>);
#[reverie::global_tool]
impl GlobalTool for AdmissionGlobal {
    type Request = u64;
    type Response = u64;
    type Config = Mode;
    async fn receive_rpc(&self, _from: Pid, request: u64) -> u64 {
        self.0.lock().unwrap().push(request);
        request
    }
}

#[derive(Default)]
pub struct AdmissionTool(Mode);
#[reverie::tool]
impl Tool for AdmissionTool {
    type GlobalState = AdmissionGlobal;
    type ThreadState = u64;
    fn new(_pid: Pid, config: &Mode) -> Self {
        Self(*config)
    }
    fn subscriptions(_config: &Mode) -> Subscription {
        {
            let mut subscription = Subscription::none();
            subscription.syscall(Sysno::getpid);
            subscription
        }
    }
    async fn handle_thread_start<G: Guest<Self>>(&self, _guest: &mut G) -> Result<(), Error> {
        if matches!(self.0, Mode::FailStart) {
            return Err(std::io::Error::other("fixture thread-start failure").into());
        }
        Ok(())
    }
    async fn handle_post_exec<G: Guest<Self>>(&self, _guest: &mut G) -> Result<(), Errno> {
        if matches!(self.0, Mode::FailPostExec) {
            return Err(Errno::EIO);
        }
        Ok(())
    }
    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        original: Syscall,
    ) -> Result<i64, Error> {
        match self.0 {
            Mode::Inject(number) | Mode::Tail(number) => {
                let operation = Syscall::from_raw(
                    Sysno::new(number as usize).unwrap(),
                    SyscallArgs::new(0, 0, 0, 0, 0, 0),
                );
                if matches!(self.0, Mode::Tail(_)) {
                    guest.tail_inject(operation).await
                }
                let _ = guest.inject(operation).await;
                // Refusal must remain fatal even if a Tool swallows the errno.
                Ok(999)
            }
            Mode::Error => Err(std::io::Error::other("fixture non-errno Tool error").into()),
            Mode::Restart => {
                *guest.thread_state_mut() += 1;
                let call = *guest.thread_state();
                assert_eq!(guest.send_rpc(call).await, call);
                if call == 1 {
                    return Err(Errno::ERESTARTSYS.into());
                }
                guest.inject(original).await.map_err(Error::from)
            }
            Mode::FailStart | Mode::FailPostExec => {
                unreachable!("startup failure reached application")
            }
        }
    }
}
