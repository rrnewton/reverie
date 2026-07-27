/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Run a simple Reverie syscall tool over a real program with the KVM backend.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

use reverie::ExitStatus;
use reverie::GlobalRPC;
use reverie::GlobalTool;
use reverie::Guest;
use reverie::Pid;
use reverie::Tool;
use reverie::syscalls::Syscall;
use reverie::syscalls::SyscallInfo;
use reverie_kvm::KvmBackend;
use reverie_kvm::StraceTool;

const GUEST_MEMORY_BYTES: usize = 256 * 1024 * 1024;

#[derive(Default)]
struct CounterState {
    counts: Mutex<BTreeMap<String, u64>>,
}

impl CounterState {
    fn counts(&self) -> BTreeMap<String, u64> {
        self.counts
            .lock()
            .expect("syscall counter lock poisoned")
            .clone()
    }
}

#[reverie::global_tool]
impl GlobalTool for CounterState {
    type Request = String;
    type Response = ();
    type Config = ();

    async fn receive_rpc(&self, _from: Pid, name: String) {
        *self
            .counts
            .lock()
            .expect("syscall counter lock poisoned")
            .entry(name)
            .or_default() += 1;
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SyscallCounterTool;

#[reverie::tool]
impl Tool for SyscallCounterTool {
    type GlobalState = CounterState;
    type ThreadState = ();

    async fn handle_syscall_event<G: Guest<Self>>(
        &self,
        guest: &mut G,
        syscall: Syscall,
    ) -> Result<i64, reverie::Error> {
        guest.send_rpc(syscall.name().to_owned()).await;
        guest.tail_inject(syscall).await
    }

    async fn on_exit_thread<G: GlobalRPC<Self::GlobalState>>(
        &self,
        _tid: Pid,
        _global: &G,
        _thread_state: Self::ThreadState,
        _status: ExitStatus,
    ) -> Result<(), reverie::Error> {
        Ok(())
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("kvm-tool: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().ok_or_else(usage_error)?;
    if args.next().as_deref() != Some(OsStr::new("--")) {
        return Err(usage_error().into());
    }
    let program = args.next().ok_or_else(usage_error)?;
    let program = program
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "program is not UTF-8"))?;
    let resolved_program = resolve_program(program)?;
    let mut argv = vec![program.to_owned()];
    argv.extend(
        args.map(|argument| {
            argument.into_string().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "guest argument is not UTF-8")
            })
        })
        .collect::<Result<Vec<_>, _>>()?,
    );
    let argv = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let envp = std::env::vars()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    let envp = envp.iter().map(String::as_str).collect::<Vec<_>>();
    let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
    let image = std::fs::read(&resolved_program)?;

    let mut backend = KvmBackend::new(GUEST_MEMORY_BYTES)?;
    backend.install_static_elf_with_context(&image, &argv, &envp, &cwd)?;

    match mode.to_str() {
        Some("counter") => {
            let (state, code, stdout, stderr) =
                block_on(backend.run_static_elf_with_tool::<SyscallCounterTool>((), true))?;
            write_guest_output(&stdout, &stderr)?;
            let counts = state.counts();
            for (name, count) in &counts {
                eprintln!("[kvm-counter] syscall={name} count={count}");
            }
            eprintln!(
                "[kvm-counter-summary] total={} distinct={}",
                counts.values().sum::<u64>(),
                counts.len(),
            );
            Ok(code)
        }
        Some("strace") => {
            let (state, code, stdout, stderr) =
                block_on(backend.run_static_elf_with_tool::<StraceTool>((), true))?;
            write_guest_output(&stdout, &stderr)?;
            eprintln!("[kvm-strace-summary] total={}", state.syscalls().len());
            Ok(code)
        }
        _ => Err(usage_error().into()),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct ThreadWake(std::thread::Thread);

    impl std::task::Wake for ThreadWake {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = std::task::Waker::from(std::sync::Arc::new(ThreadWake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match std::future::Future::poll(future.as_mut(), &mut context) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

fn resolve_program(program: &str) -> io::Result<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return std::fs::canonicalize(path);
    }

    let search_path = std::env::var_os("PATH")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "PATH is not set"))?;
    std::env::split_paths(&search_path)
        .map(|directory| directory.join(path))
        .find(|candidate| candidate.is_file())
        .map(std::fs::canonicalize)
        .transpose()?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("program {program:?} was not found in PATH"),
            )
        })
}

fn write_guest_output(stdout: &[u8], stderr: &[u8]) -> io::Result<()> {
    io::stdout().write_all(stdout)?;
    io::stderr().write_all(stderr)?;
    Ok(())
}

fn usage_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "usage: kvm-tool <counter|strace> -- <program> [args...]",
    )
}
