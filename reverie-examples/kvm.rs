/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Runs Reverie's example Tool implementations over a real program with KVM.

use std::ffi::OsStr;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use reverie::GlobalTool;
use reverie::Tool;
use reverie_examples::chaos::ChaosOpts;
use reverie_examples::chaos::ChaosTool;
use reverie_examples::counter1;
use reverie_examples::counter2;
use reverie_examples::noop::NoopTool;
use reverie_examples::strace;
use reverie_examples::strace_minimal;
use reverie_kvm::KvmBackend;

type RunnerResult<T> = Result<T, Box<dyn std::error::Error>>;
type ToolOutput<T> = (<T as Tool>::GlobalState, i32, Vec<u8>, Vec<u8>);

const GUEST_MEMORY_BYTES: usize = 256 * 1024 * 1024;

struct GuestCommand {
    image: Vec<u8>,
    argv: Vec<String>,
    envp: Vec<String>,
    cwd: PathBuf,
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("kvm_examples: {error}");
            std::process::exit(1);
        }
    }
}

// TODO-HUMAN-REVIEW(#123): Review the backend-neutral example Tool runner CLI.
fn run() -> RunnerResult<i32> {
    let mut args = std::env::args_os().skip(1);
    let mode = args
        .next()
        .and_then(|mode| mode.into_string().ok())
        .ok_or_else(usage_error)?;
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
    let command = GuestCommand {
        image: std::fs::read(resolved_program)?,
        argv,
        envp: std::env::vars()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        cwd: std::fs::canonicalize(std::env::current_dir()?)?,
    };

    match mode.as_str() {
        "counter1" => {
            let (state, code, stdout, stderr) = execute::<counter1::CounterLocal>(&command, ())?;
            write_guest_output(&stdout, &stderr)?;
            eprintln!("[kvm-counter1-summary] total={}", state.num_syscalls());
            Ok(code)
        }
        "counter2" => {
            let (state, code, stdout, stderr) = execute::<counter2::CounterLocal>(&command, ())?;
            write_guest_output(&stdout, &stderr)?;
            let inner = state
                .inner
                .lock()
                .map_err(|_| io::Error::other("counter2 state lock poisoned"))?;
            eprintln!(
                "[kvm-counter2-summary] total={} processes={} threads={}",
                inner.total_syscalls, inner.exited_procs, inner.exited_threads
            );
            Ok(code)
        }
        "strace" => {
            let (_, code, stdout, stderr) =
                execute::<strace::tool::Strace>(&command, strace::config::Config::default())?;
            write_guest_output(&stdout, &stderr)?;
            eprintln!("[kvm-strace-summary] completed");
            Ok(code)
        }
        "strace_minimal" => {
            let (_, code, stdout, stderr) = execute::<strace_minimal::StraceTool>(&command, ())?;
            write_guest_output(&stdout, &stderr)?;
            eprintln!("[kvm-strace-minimal-summary] completed");
            Ok(code)
        }
        "chaos" => {
            let (_, code, stdout, stderr) = execute::<ChaosTool>(&command, ChaosOpts::default())?;
            write_guest_output(&stdout, &stderr)?;
            eprintln!("[kvm-chaos-summary] completed");
            Ok(code)
        }
        "noop" => {
            let (_, code, stdout, stderr) = execute::<NoopTool>(&command, ())?;
            write_guest_output(&stdout, &stderr)?;
            eprintln!("[kvm-noop-summary] completed");
            Ok(code)
        }
        _ => Err(usage_error().into()),
    }
}

fn execute<T>(
    command: &GuestCommand,
    config: <<T as Tool>::GlobalState as GlobalTool>::Config,
) -> RunnerResult<ToolOutput<T>>
where
    T: Tool,
{
    let argv = command.argv.iter().map(String::as_str).collect::<Vec<_>>();
    let envp = command.envp.iter().map(String::as_str).collect::<Vec<_>>();
    let mut backend = KvmBackend::new(GUEST_MEMORY_BYTES)?;
    backend.install_static_elf_with_context(&command.image, &argv, &envp, &command.cwd)?;
    Ok(block_on(
        backend.run_static_elf_with_tool::<T>(config, true),
    )?)
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
        "usage: kvm_examples <counter1|counter2|strace|strace_minimal|chaos|noop> -- <program> [args...]",
    )
}
