/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! The `chaos` binary: the tool in `chaos.rs`, run on any Reverie backend
//! through the shared runner (`backend.rs`). The tool's file holds only the
//! tool, so the SaBRe plugin and the example preload can include it.

#[path = "backend.rs"]
mod backend;
#[path = "chaos.rs"]
mod chaos;

use chaos::*;
use clap::Parser;
use reverie_util::CommonToolArguments;

/// A tool to introduce inject "chaos" into a running process. A pathological
/// kernel is simulated by forcing reads to only return one byte a time.
#[derive(Debug, Parser)]
struct Args {
    // TODO-HUMAN-REVIEW(PR-195): Review chaos runner selection.
    /// The Reverie backend to run the program on (`--runner` is accepted).
    #[clap(long, value_enum, default_value = "ptrace", alias = "runner")]
    backend: backend::BackendKind,

    /// The example preload the in-guest backends host the tool in; by
    /// default `libreverie_examples.so` beside this binary.
    #[clap(long)]
    preload: Option<std::path::PathBuf>,

    /// Verified caller artifact manifest for DBT. Defaults to
    /// reverie-dbt-callers/chaos/manifest.json beside this binary.
    #[clap(long, env = "REVERIE_DBT_CALLER_ARTIFACT")]
    dbt_artifact: Option<std::path::PathBuf>,

    #[clap(flatten)]
    common_opts: CommonToolArguments,

    #[clap(flatten)]
    chaos_opts: ChaosOpts,
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
                .ok_or_else(|| anyhow::anyhow!("chaos executable has no profile directory"))?
                .join("reverie-dbt-callers/chaos/manifest.json"),
        };
        Some(reverie_dbt::CallerArtifact::load(&path).map_err(|error| {
            anyhow::anyhow!("cannot load DBT caller artifact {}: {error}; run reverie-dbt/scripts/build-client.sh --caller-manifest reverie-examples/chaos-dbt-runtime/Cargo.toml --profile <this binary's Cargo profile>", path.display())
        })?)
    } else {
        None
    };
    let log_guard = args.common_opts.init_tracing();
    let run = backend::run_blocking_with_caller::<ChaosTool>(
        args.backend,
        &args.common_opts,
        args.chaos_opts,
        "chaos",
        args.preload.clone(),
        caller,
    )?;
    let (status, _) = (run.status, run.global_state);
    drop(log_guard); // Flush logs before exiting.
    status.raise_or_exit()
}
