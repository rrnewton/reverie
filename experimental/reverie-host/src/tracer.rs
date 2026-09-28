/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

use std::env;
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use reverie_process::Child;
use reverie_process::Command;
use reverie_rpc::Service;

use super::server::Server;

pub struct TracerBuilder<S> {
    command: Command,
    sabre: Option<PathBuf>,
    plugin: Option<PathBuf>,
    service: S,
}

impl TracerBuilder<()> {
    pub fn new(command: Command) -> Self {
        Self {
            command,
            sabre: None,
            plugin: None,
            service: (),
        }
    }
}

impl<S> TracerBuilder<S> {
    /// Sets the path to the plugin's DSO. If this is not set, the
    /// `SABRE_PLUGIN` environment variable is used instead.
    pub fn plugin<P: Into<Option<PathBuf>>>(mut self, path: P) -> Self {
        self.plugin = path.into();
        self
    }

    /// Sets the path to the sabre binary. If this is not set, the
    /// `SABRE_BINARY` environment variable is used instead.
    pub fn sabre<P: Into<Option<PathBuf>>>(mut self, path: P) -> Self {
        self.sabre = path.into();
        self
    }

    /// Set the global state service. The service is started when the child
    /// process is spawned.
    pub fn global_state<T>(self, service: T) -> TracerBuilder<T> {
        TracerBuilder {
            command: self.command,
            sabre: self.sabre,
            plugin: self.plugin,
            service,
        }
    }

    /// Spawns the root guest process.
    pub fn spawn(self) -> Result<Child>
    where
        S: Service + Clone + Send + Sync + 'static,
    {
        self.command.require_pathname_execution("SaBRe")?;
        let sabre = self
            .sabre
            .or_else(|| std::env::var_os("SABRE_BINARY").map(PathBuf::from))
            .map_or_else(find_sabre, |x| Ok(Some(x)))?;
        let sabre = sabre.ok_or_else(|| anyhow!("Could not find sabre executable"))?;

        let plugin = self
            .plugin
            .or_else(|| std::env::var_os("SABRE_PLUGIN").map(PathBuf::from))
            .map_or_else(find_plugin, |x| Ok(Some(x)))?;
        let plugin = plugin.ok_or_else(|| anyhow!("Could not find SaBRe plugin"))?;

        let mut command = into_sabre(self.command, sabre.as_ref(), plugin.as_ref())?;

        let server = Server::new()?;

        command.env("REVERIE_SOCK", server.sock_path());

        let service = self.service;

        tokio::spawn(async move { server.serve(service).await });

        let child = command
            .spawn()
            .with_context(|| format!("Failed to spawn: {:?}", command.get_program()))?;

        Ok(child)
    }
}

fn into_sabre(mut command: Command, sabre: &OsStr, plugin: &OsStr) -> Result<Command> {
    command.require_pathname_execution("SaBRe")?;
    let program = command
        .find_program()
        .with_context(|| format!("Could not find program: {:?}", command.get_program()))?;

    command.prepend_args([plugin, "--".as_ref(), program.as_ref()]);

    // Change the program that we're launching. This also changes arg0 to match.
    command.program(sabre);

    // Ensure that SABRE_BINARY and SABRE_PLUGIN are not inherited by the child
    // process.
    command.env_remove("SABRE_BINARY");
    command.env_remove("SABRE_PLUGIN");

    Ok(command)
}

/// Tries to find the path to the `sabre` executable based on the path to the
/// current executablbe. This should be the case when using dotslash.
fn find_sabre() -> Result<Option<PathBuf>, io::Error> {
    let mut path = env::current_exe()?;

    path.pop();
    path.push("sabre");

    if path.is_file() {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

/// Tries to find the plugin based on the path to the current executable. This
/// should be the case when using dotslash.
fn find_plugin() -> Result<Option<PathBuf>, io::Error> {
    let mut path = env::current_exe()?;

    if let Some(exe_name) = path.file_name() {
        let mut name = exe_name.to_os_string();
        name.push("_plugin.so");

        // Search for the plugin in the same directory.
        path.set_file_name(name);

        if path.is_file() {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use reverie_process::DescriptorExecutionUnsupported;

    use super::*;

    #[test]
    fn sabre_refuses_mismatched_diagnostic_path_and_descriptor_before_wrapping() {
        let mut command = Command::new("/diagnostic/path/must-not-be-resolved");
        command
            .executable(File::open("/bin/true").unwrap().into())
            .unwrap();

        let error = match into_sabre(
            command,
            OsStr::new("/unused/sabre"),
            OsStr::new("/unused/plugin"),
        ) {
            Ok(_) => panic!("SaBRe accepted descriptor execution"),
            Err(error) => error,
        };
        let io_error = error
            .downcast_ref::<io::Error>()
            .expect("SaBRe refusal must remain an I/O error");
        assert_eq!(io_error.kind(), io::ErrorKind::Unsupported);
        let typed = io_error
            .get_ref()
            .and_then(|error| error.downcast_ref::<DescriptorExecutionUnsupported>())
            .expect("SaBRe refusal lost its typed descriptor reason");
        assert_eq!(typed.consumer(), "SaBRe");
        assert_eq!(
            io_error.to_string(),
            "SaBRe does not support descriptor-based execution"
        );
    }

    #[test]
    fn sabre_spawn_refuses_descriptor_before_binary_plugin_and_server_setup() {
        let mut command = Command::new("/diagnostic/path/must-not-be-resolved");
        command
            .executable(File::open("/bin/true").unwrap().into())
            .unwrap();

        let error = match TracerBuilder::new(command)
            .sabre(PathBuf::from("/sabre/path/must-not-be-resolved"))
            .plugin(PathBuf::from("/plugin/path/must-not-be-resolved"))
            .spawn()
        {
            Ok(_) => panic!("SaBRe spawn accepted descriptor execution"),
            Err(error) => error,
        };
        let io_error = error
            .downcast_ref::<io::Error>()
            .expect("SaBRe spawn refusal must remain an I/O error");
        assert_eq!(io_error.kind(), io::ErrorKind::Unsupported);
        let typed = io_error
            .get_ref()
            .and_then(|error| error.downcast_ref::<DescriptorExecutionUnsupported>())
            .expect("SaBRe spawn refusal lost its typed descriptor reason");
        assert_eq!(typed.consumer(), "SaBRe");
    }
}
