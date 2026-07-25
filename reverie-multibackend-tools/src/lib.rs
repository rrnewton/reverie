/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(impl-multi-backend-tools): New multi-backend tool build
// infrastructure authored by an autonomous bot. No guest-visible syscall
// behavior changes; the DBI adapter only references existing reverie-dbi driver
// entry points to prove link-level compatibility. Replace the task id with the
// PR id when published.

//! Per-backend adapters that run a shared Reverie [`Tool`](reverie::Tool)
//! against a chosen backend.
//!
//! The tools themselves (`reverie-tool-sysctr`, `reverie-tool-riptrace`) are
//! backend-agnostic. These adapters are the thin, backend-specific glue that
//! each binary in `src/bin/` calls, so the tool code is written once and the
//! only thing that varies per binary is which adapter (and therefore which
//! backend) it links against.
//!
//! Each adapter is feature-gated to match its backend feature; see the crate
//! `Cargo.toml` for why DBI is not on by default.

/// Run a shared tool under the **ptrace** backend against the guest command
/// given on the command line, returning the guest exit status and the tool's
/// final global state.
///
/// This is the mature, general-purpose path: it can trace any guest command.
#[cfg(feature = "ptrace")]
pub async fn run_ptrace<T>(
    config: <T::GlobalState as reverie::GlobalTool>::Config,
) -> Result<(reverie::ExitStatus, T::GlobalState), reverie::Error>
where
    T: reverie::Tool + 'static,
{
    use clap::Parser;
    use reverie_util::CommonToolArguments;

    let args = CommonToolArguments::parse();
    let log_guard = args.init_tracing();
    let tracer = reverie_ptrace::TracerBuilder::<T>::new(args.into())
        .config(config)
        .spawn()
        .await?;
    let result = tracer.wait().await?;
    drop(log_guard); // Flush logs before exiting.
    Ok(result)
}

/// Run a shared tool under the **KVM** backend against a single static ELF given
/// on the command line, returning the tool's final global state and the guest
/// exit code.
///
/// The KVM backend is a bounded prototype: it runs one fixed-address static ELF
/// process, not an arbitrary guest command tree. `argv[0]` must be a readable
/// static ELF; any further arguments are passed to the guest.
#[cfg(feature = "kvm")]
pub fn run_kvm_static_elf<T>(
    config: <T::GlobalState as reverie::GlobalTool>::Config,
) -> anyhow::Result<(T::GlobalState, i32)>
where
    T: reverie::Tool,
{
    use anyhow::Context;

    // 256 MiB matches the KVM crate's own "real program" test sizing.
    const MEMORY_SIZE: usize = 256 * 1024 * 1024;

    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() {
        anyhow::bail!("usage: <binary> <static-elf> [guest-args...]");
    }
    let image = std::fs::read(&argv[0])
        .with_context(|| format!("reading guest static ELF {:?}", argv[0]))?;

    let mut backend = reverie_kvm::KvmBackend::new(MEMORY_SIZE)
        .map_err(|e| anyhow::anyhow!("KvmBackend::new failed: {e}"))?;
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    backend
        .install_static_elf_with_args(&image, &argv_refs, &[])
        .map_err(|e| anyhow::anyhow!("install_static_elf failed: {e}"))?;

    let (global, code, _stdout, _stderr) =
        futures::executor::block_on(backend.run_static_elf_with_tool::<T>(config, false))
            .map_err(|e| anyhow::anyhow!("run_static_elf_with_tool failed: {e}"))?;
    Ok((global, code))
}

/// Launch a guest command under the **DBI** (DynamoRIO) backend, returning the
/// guest process exit status.
///
/// Unlike ptrace and KVM, the DBI backend has no runtime tool selection: the
/// tool is compiled into a separately-built DynamoRIO native client
/// (`REVERIE_DBI_CLIENT`, built by `reverie-dbi/scripts/build-client.sh`), which
/// the DynamoRIO launcher loads into the guest. This adapter therefore does two
/// things:
///
/// 1. It statically references the generic DBI driver entry points instantiated
///    at `T`, which forces the compiler to type-check and instantiate them for
///    our shared tool. That is the compile/link-level proof that the exact same
///    tool code is compatible with the DBI backend's [`Tool`](reverie::Tool)
///    contract.
/// 2. It drives the real [`reverie_dbi::DbiRunner`] to launch the guest under
///    the DynamoRIO client.
///
/// Embedding a *specific* tool into a per-tool native client (so this launcher's
/// `T` and the client's baked-in tool are guaranteed to match) is owned by the
/// DBI native-client work and is tracked separately.
#[cfg(feature = "dbi")]
pub fn run_dbi<T>() -> anyhow::Result<std::process::ExitStatus>
where
    T: reverie::Tool,
{
    // (1) Compile/link-level proof that the shared tool satisfies the DBI
    // backend's Tool driver contract. Naming these generic entry points at `T`
    // forces the compiler to instantiate and type-check them for our tool.
    let dbi_driver_entry_points = (
        reverie_dbi::run_tool_thread_start::<T>,
        reverie_dbi::run_tool_post_exec::<T>,
        reverie_dbi::run_tool_thread_exit::<T>,
        reverie_dbi::run_tool_syscall::<T>,
    );
    std::hint::black_box(&dbi_driver_entry_points);

    // (2) Drive the real DynamoRIO launcher on the guest command.
    let mut args = std::env::args_os().skip(1);
    let program = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: <binary> <guest-program> [guest-args...]"))?;
    let mut guest = std::process::Command::new(program);
    guest.args(args);

    let runner = reverie_dbi::DbiRunner::from_env().map_err(|e| {
        anyhow::anyhow!(
            "DbiRunner::from_env failed: {e}. Set DYNAMORIO_HOME and REVERIE_DBI_CLIENT; \
             the native client must embed this tool."
        )
    })?;
    let status = runner
        .status(&guest)
        .map_err(|e| anyhow::anyhow!("DBI guest launch failed: {e}"))?;
    Ok(status)
}
