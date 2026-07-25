/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Run an ELF under KVM and report its intercepted syscall count.

use std::future::Future;
use std::io::Write;
use std::pin::pin;
use std::process;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

use reverie::Error;
use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie_kvm::KvmBackend;

const GUEST_MEMORY_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug, Default)]
struct CounterGlobal {
    syscalls: AtomicU64,
    threads: AtomicU64,
}

impl CounterGlobal {
    fn syscalls(&self) -> u64 {
        self.syscalls.load(Ordering::Relaxed)
    }

    fn threads(&self) -> u64 {
        self.threads.load(Ordering::Relaxed)
    }
}

#[reverie::global_tool]
impl GlobalTool for CounterGlobal {
    type Request = u64;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, thread_syscalls: u64) {
        self.syscalls.fetch_add(thread_syscalls, Ordering::Relaxed);
        self.threads.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CounterTool;

#[reverie::tool]
impl Tool for CounterTool {
    type GlobalState = CounterGlobal;
    type ThreadState = u64;

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, Error> {
        *guest.thread_state_mut() += 1;
        guest.tail_inject(syscall).await
    }

    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        _tid: Pid,
        global: &G,
        thread_syscalls: u64,
        _status: ExitStatus,
    ) -> Result<(), Error> {
        global.send_rpc(thread_syscalls).await;
        Ok(())
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut args = std::env::args();
    let executable = args.next().unwrap_or_else(|| "reverie-kvm-counter".into());
    let first = args.next();
    let program = match first.as_deref() {
        Some("--") => args.next(),
        _ => first,
    }
    .ok_or_else(|| format!("usage: {executable} -- PROGRAM [ARG ...]"))?;

    let mut argv = vec![program.clone()];
    argv.extend(args);
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let environment: Vec<String> = std::env::vars()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let environment_refs: Vec<&str> = environment.iter().map(String::as_str).collect();

    let image = std::fs::read(&program)?;
    let mut backend = KvmBackend::new(GUEST_MEMORY_SIZE)?;
    backend.install_static_elf_with_args(&image, &argv_refs, &environment_refs)?;
    let (counts, status, stdout, stderr) =
        block_on(backend.run_static_elf_with_tool::<CounterTool>((), true))?;

    std::io::stdout().write_all(&stdout)?;
    std::io::stderr().write_all(&stderr)?;
    eprintln!(
        "reverie-counter: syscalls={} threads={}",
        counts.syscalls(),
        counts.threads()
    );
    Ok(status)
}

fn main() {
    match run() {
        Ok(status) => process::exit(status),
        Err(error) => {
            eprintln!("reverie-kvm-counter: {error}");
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_counter_aggregates_thread_totals() {
        let counter = CounterGlobal::default();
        block_on(counter.receive_rpc(Pid::from_raw(10), 7));
        block_on(counter.receive_rpc(Pid::from_raw(11), 5));
        assert_eq!(counter.syscalls(), 12);
        assert_eq!(counter.threads(), 2);
    }
}
