/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Retains the Reverie example-tool selector surface while generic LiteInst
//! launch refuses before starting a guest.

use std::path::PathBuf;

use anyhow::bail;
use clap::Parser;
use reverie::process::Command;
#[path = "src/host.rs"]
mod example_tools;

// TODO-HUMAN-REVIEW(PR-139): Review crate-local strace source reuse.
pub(crate) use example_tools::config;
pub(crate) use example_tools::filter;
pub(crate) use example_tools::global_state;

#[derive(Debug, Default, clap::Args)]
struct ChaosCliOptions {
    /// Skips the first N syscalls before doing any intervention.
    #[clap(long, value_name = "N")]
    skip: Option<u64>,

    /// Does not modify read-like system calls.
    #[clap(long)]
    no_read: bool,

    /// Does not modify recv-like system calls.
    #[clap(long)]
    no_recv: bool,

    /// Does not inject interrupted-read errors.
    #[clap(long)]
    no_interrupt: bool,
}

impl ChaosCliOptions {
    fn was_supplied(&self) -> bool {
        self.skip.is_some() || self.no_read || self.no_recv || self.no_interrupt
    }

    fn into_config(self) -> example_tools::ChaosOpts {
        example_tools::ChaosOpts::for_liteinst(
            self.skip,
            self.no_read,
            self.no_recv,
            self.no_interrupt,
        )
    }
}

#[derive(Debug, Parser)]
#[clap(trailing_var_arg = true)]
struct Args {
    #[clap(long, value_enum)]
    tool: example_tools::ToolKind,

    #[clap(long)]
    preload: Option<PathBuf>,

    #[clap(long = "trace")]
    filters: Vec<String>,

    /// Reserved debug port.
    #[clap(long)]
    port: Option<u16>,

    /// The path to write the Chrome trace artifact.
    // TODO-HUMAN-REVIEW(PR-159): Review the ChromeTrace artifact option.
    #[clap(long)]
    out: Option<PathBuf>,

    // TODO-HUMAN-REVIEW(PR-157): Review the production chaos option surface.
    #[clap(flatten)]
    chaos_options: ChaosCliOptions,

    #[clap(required = true, num_args = 1.., allow_hyphen_values = true)]
    command: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.tool != example_tools::ToolKind::Strace && !args.filters.is_empty() {
        bail!("--trace is only valid with --tool strace");
    }
    if args.tool != example_tools::ToolKind::ChromeTrace && args.out.is_some() {
        bail!("--out is only valid with --tool chrome-trace");
    }
    if args.tool != example_tools::ToolKind::Chaos && args.chaos_options.was_supplied() {
        bail!("chaos options are only valid with --tool chaos");
    }
    if args.tool != example_tools::ToolKind::Debug && args.port.is_some() {
        bail!("--port is only valid with --tool debug");
    }
    let chaos_options = args.chaos_options.into_config();

    let _ = args.preload;
    let mut command = Command::new(&args.command[0]);
    command.args(&args.command[1..]);
    example_tools::run(args.tool, command, args.filters, chaos_options).await?;
    Ok(())
}
