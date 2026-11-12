/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The `chrome_trace` binary: the tool in `main.rs`, run on any Reverie backend
//! through the shared runner (`backend.rs`). The tool's file holds only the
//! tool, so the SaBRe plugin and the example preload can include it.

#[path = "../backend.rs"]
mod backend;
#[path = "main.rs"]
mod chrome_trace;

use std::fs;
use std::io;
use std::path::PathBuf;

use anyhow::Context;
use chrome_trace::*;
use clap::Parser;
use reverie_util::CommonToolArguments;

/// A tool to render a summary of the process tree.
#[derive(Debug, Parser)]
struct Args {
    // TODO-HUMAN-REVIEW(PR-195): Review Chrome trace runner selection.
    /// The Reverie backend to run the program on (`--runner` is accepted).
    #[clap(long, value_enum, default_value = "ptrace", alias = "runner")]
    backend: backend::BackendKind,

    /// The example preload the in-guest backends host the tool in; by
    /// default `libreverie_examples.so` beside this binary.
    #[clap(long)]
    preload: Option<std::path::PathBuf>,

    /// Verified caller artifact manifest for DBT. Defaults to
    /// reverie-dbt-callers/chrome-trace/manifest.json beside this binary.
    #[clap(long, env = "REVERIE_DBT_CALLER_ARTIFACT")]
    dbt_artifact: Option<PathBuf>,

    #[clap(flatten)]
    common: CommonToolArguments,

    /// The path to write out Chrome trace file. This can be loaded with
    /// `chrome://tracing`.
    #[clap(long)]
    out: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    run_main(Args::parse())
}

fn run_main(args: Args) -> anyhow::Result<()> {
    let caller = if args.backend == backend::BackendKind::Dbt {
        let path = match &args.dbt_artifact {
            Some(path) => path.clone(),
            None => std::env::current_exe()?
                .parent()
                .ok_or_else(|| anyhow::anyhow!("chrome_trace executable has no profile directory"))?
                .join("reverie-dbt-callers/chrome-trace/manifest.json"),
        };
        Some(reverie_dbt::CallerArtifact::load(&path).map_err(|error| {
            anyhow::anyhow!("cannot load DBT caller artifact {}: {error}; run reverie-dbt/scripts/build-client.sh --caller-manifest reverie-examples/chrome-trace-dbt-runtime/Cargo.toml --profile <this binary's Cargo profile>", path.display())
        })?)
    } else {
        None
    };
    let log_guard = args.common.init_tracing();
    let run = backend::run_blocking_with_caller::<ChromeTrace>(
        args.backend,
        &args.common,
        (),
        "chrome-trace",
        args.preload.clone(),
        caller,
    )?;
    let (status, global_state) = (run.status, run.global_state);

    if let Some(path) = args.out {
        let mut f = io::BufWriter::new(fs::File::create(path)?);
        global_state
            .chrome_trace(&mut f)
            .context("failed to generate Chrome trace")?;
    }

    // Flush logs before exiting.
    drop(log_guard);
    status.raise_or_exit()
}
