#![forbid(unsafe_op_in_unsafe_fn)]
#![feature(linux_pidfd)]

use std::process::Command;
mod backend;
#[cfg(test)]
mod package_tests;

pub use backend::LiteinstBackend;
pub use backend::LoggedRunError;
pub use backend::PreparedCommand;
pub use backend::run_evidence;
pub use reverie_liteinst_runtime::*;

// AUTONOMOUS-BOT-IMPLEMENTED
// TODO-HUMAN-REVIEW(PR-254): Review launcher-side shared RuntimeConfig alt-stack selector.
/// Selects the shared `reverie-preload` `RuntimeConfig` alt-stack knob for a guest.
///
/// Sets [`ALT_STACK_ENV`] so the in-guest runtime installs its `SIGSYS` handler
/// with or without an alternate signal stack (`RuntimeConfig::use_alt_stack`).
/// The `RuntimeConfig` and the controller honoring it are shared with e9patch in
/// `reverie-preload`; only the env-var spelling is LiteInst's. Leaving this
/// unset preserves the shared default (alt stack on). The written value
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
