/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Ptrace and KVM launcher for the shared chaos tool.

mod chaos_tool;

use chaos_tool::ChaosOpts;
use chaos_tool::ChaosTool;
use clap::Parser;
use reverie_util::CommonToolArguments;

#[path = "src/kvm_runner.rs"]
mod kvm_runner;

/// A tool to introduce inject "chaos" into a running process. A pathological
/// kernel is simulated by forcing reads to only return one byte a time.
#[derive(Debug, Parser)]
struct Args {
    // TODO-HUMAN-REVIEW(PR-195): Review chaos runner selection.
    /// Execution runner; KVM selects the prototype KvmGuest host.
    #[clap(long, value_enum, default_value = "ptrace")]
    runner: kvm_runner::Runner,

    #[clap(flatten)]
    common_opts: CommonToolArguments,

    #[clap(flatten)]
    chaos_opts: ChaosArgs,
}

// The command line that fills in `ChaosOpts`.
#[derive(Parser, Debug)]
struct ChaosArgs {
    /// Skips the first N syscalls of a process before doing any intervention.
    /// This is useful when you need to skip past an error caused by the tool.
    #[clap(long, value_name = "N", default_value = "0")]
    skip: u64,

    /// If set, does not intercept `read`-like system calls and modify them.
    #[clap(long)]
    no_read: bool,

    /// If set, does not intercept `recv`-like system calls and modify them.
    #[clap(long)]
    no_recv: bool,

    /// If set, does not inject random `EINTR` errors.
    #[clap(long)]
    no_interrupt: bool,
}

impl From<ChaosArgs> for ChaosOpts {
    fn from(args: ChaosArgs) -> Self {
        Self {
            skip: args.skip,
            no_read: args.no_read,
            no_recv: args.no_recv,
            no_interrupt: args.no_interrupt,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let stdin = match args.runner {
        kvm_runner::Runner::Ptrace => None,
        kvm_runner::Runner::Kvm => kvm_runner::reserve_stdin()?,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_main(args, stdin))
}

async fn run_main(args: Args, stdin: Option<std::fs::File>) -> anyhow::Result<()> {
    let log_guard = args.common_opts.init_tracing();
    let (status, _) = match args.runner {
        kvm_runner::Runner::Ptrace => {
            let tracer =
                reverie_ptrace::TracerBuilder::<ChaosTool>::new(args.common_opts.clone().into())
                    .config(args.chaos_opts.into())
                    .spawn()
                    .await?;
            tracer.wait().await?
        }
        kvm_runner::Runner::Kvm => {
            let result =
                kvm_runner::run::<ChaosTool>(&args.common_opts, args.chaos_opts.into(), stdin)
                    .await?;
            (
                reverie::ExitStatus::Exited(result.exit_code),
                result.global_state,
            )
        }
    };
    drop(log_guard); // Flush logs before exiting.
    status.raise_or_exit()
}
