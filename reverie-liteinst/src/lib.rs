#![forbid(unsafe_op_in_unsafe_fn)]
#![feature(linux_pidfd)]

use std::env;
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::process::Command;
mod backend;
#[cfg(test)]
mod package_tests;

pub use backend::LiteinstBackend;
pub use backend::LoggedRunError;
pub use backend::PreparedCommand;
pub use backend::run_evidence;
pub use reverie_liteinst_runtime::*;

/// Built-in synchronous tool executed by the preload runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreloadTool {
    /// Emit one detailed line for every trapped syscall.
    Strace,
    /// Emit stable syscall-number markers for external comparison.
    Compatibility,
}

impl PreloadTool {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Strace => "strace",
            Self::Compatibility => "compat",
        }
    }
}

/// Locates the preload runtime produced beside the current executable.
pub fn preload_library_path() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("REVERIE_LITEINST_PRELOAD") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "REVERIE_LITEINST_PRELOAD does not name a file",
            )
        });
    }

    let executable = env::current_exe()?;
    let parent = executable.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "current executable has no parent")
    })?;
    [
        parent.join("libreverie_liteinst.so"),
        parent.join("deps/libreverie_liteinst.so"),
        parent
            .parent()
            .unwrap_or(parent)
            .join("libreverie_liteinst.so"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "cannot find libreverie_liteinst.so beside {}",
                executable.display()
            ),
        )
    })
}

/// Configures a guest command to load the runtime and select a built-in tool.
pub fn configure_command(command: &mut Command, tool: PreloadTool) -> io::Result<()> {
    let mut preload = preload_library_path()?.into_os_string();
    if let Some(existing) = env::var_os("LD_PRELOAD").filter(|value| !value.is_empty()) {
        preload.push(OsStr::new(":"));
        preload.push(existing);
    }
    command
        .env("LD_PRELOAD", preload)
        .env("REVERIE_LITEINST_TOOL", tool.as_str());
    Ok(())
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-252): Review shared built-in command configuration.
/// Maps a shared [`BuiltinTool`] to its `REVERIE_LITEINST_TOOL` selector value.
fn builtin_tool_env_value(tool: BuiltinTool) -> &'static str {
    match tool {
        BuiltinTool::Passthrough => TOOL_PASSTHROUGH,
        BuiltinTool::SpoofGetpid => TOOL_SPOOF_GETPID,
    }
}

/// Configures a guest command to load the runtime and select a shared built-in.
///
/// This is the built-in analog of [`configure_command`]: it sets `LD_PRELOAD`
/// and `REVERIE_LITEINST_TOOL` to a shared `reverie-preload` [`BuiltinTool`]
/// selector, so the runtime installs the built-in verbatim through
/// `reverie_preload::install_builtin` (no LiteInst patching). It mirrors
/// e9patch's launcher-side built-in configuration.
pub fn configure_command_builtin(command: &mut Command, tool: BuiltinTool) -> io::Result<()> {
    let mut preload = preload_library_path()?.into_os_string();
    if let Some(existing) = env::var_os("LD_PRELOAD").filter(|value| !value.is_empty()) {
        preload.push(OsStr::new(":"));
        preload.push(existing);
    }
    command
        .env("LD_PRELOAD", preload)
        .env("REVERIE_LITEINST_TOOL", builtin_tool_env_value(tool));
    Ok(())
}

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-254): Review launcher-side shared RuntimeConfig alt-stack selector.
/// Selects the shared `reverie-preload` `RuntimeConfig` alt-stack knob for a guest.
///
/// Sets [`ALT_STACK_ENV`] so the in-guest runtime installs its `SIGSYS` handler
/// with or without an alternate signal stack (`RuntimeConfig::use_alt_stack`).
/// The `RuntimeConfig` and the controller honoring it are shared with e9patch in
/// `reverie-preload`; only the env-var spelling is LiteInst's. Leaving this
/// unset preserves the shared default (alt stack on). It composes with
/// [`configure_command`] and [`configure_command_builtin`]; the written value
/// round-trips through [`alt_stack_from_env_value`].
pub fn set_guest_alt_stack(command: &mut Command, use_alt_stack: bool) {
    command.env(ALT_STACK_ENV, if use_alt_stack { "1" } else { "0" });
}

/// Select whether the direct runtime may forward process-creation syscalls.
///
/// This does not add support for thread-style clone. It lets a consumer retain
/// the existing fail-closed boundary until its Tool lifecycle has a positive
/// fork bracket.
pub fn set_guest_process_forks(command: &mut Command, allowed: bool) {
    command.env(PROCESS_FORK_ENV, if allowed { "1" } else { "0" });
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::process::Command;

    use super::ALT_STACK_ENV;
    use super::alt_stack_from_env_value;
    use super::set_guest_alt_stack;

    /// The value the launcher writes must parse back to the same boolean it
    /// selected, for both polarities. This closes the loop between the setter
    /// (`set_guest_alt_stack`) and the runtime-side parser
    /// (`alt_stack_from_env_value`).
    #[test]
    fn alt_stack_setter_round_trips_through_the_parser() {
        for use_alt_stack in [true, false] {
            let mut command = Command::new("/bin/true");
            set_guest_alt_stack(&mut command, use_alt_stack);
            let written = command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(ALT_STACK_ENV))
                .and_then(|(_, value)| value)
                .expect("set_guest_alt_stack must set ALT_STACK_ENV")
                .to_owned();
            assert_eq!(
                alt_stack_from_env_value(Some(written.as_os_str())).unwrap(),
                use_alt_stack,
                "written value must parse back to the selected boolean"
            );
        }
    }
}
