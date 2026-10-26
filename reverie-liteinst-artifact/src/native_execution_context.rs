// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! One private process-entry context for an owned normal wrapper invocation.
//! Serializable observations are data only; they cannot reacquire this context
//! or authorize source/tool inputs. Retained observations never open old scratch.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::Result;
use super::native_build::DirectoryGuard;
use super::require;

/// Closed dynamic environment slots. These references belong only in expected
/// environment values, never source, tool, argv, cwd or configured-role fields.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize
)]
#[serde(rename_all = "snake_case")]
pub enum ContextSlot {
    Tmpdir,
    Pwd,
    ExecutionId,
    SessionId,
    ActionDigest,
    BuckScratchPath,
}

const SLOTS: [ContextSlot; 6] = [
    ContextSlot::Tmpdir,
    ContextSlot::Pwd,
    ContextSlot::ExecutionId,
    ContextSlot::SessionId,
    ContextSlot::ActionDigest,
    ContextSlot::BuckScratchPath,
];

impl ContextSlot {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Tmpdir => "TMPDIR",
            Self::Pwd => "PWD",
            Self::ExecutionId => "EXECUTION_ID",
            Self::SessionId => "SESSION_ID",
            Self::ActionDigest => "ACTION_DIGEST",
            Self::BuckScratchPath => "BUCK_SCRATCH_PATH",
        }
    }
}

/// Exact original values, including absence. Cwd is observed independently
/// from PWD. Deserializing this record grants no live execution context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContextObservation {
    pub cwd: Vec<u8>,
    pub slots: BTreeMap<ContextSlot, Option<Vec<u8>>>,
}

impl ContextObservation {
    pub(super) fn validate(&self) -> Result<()> {
        require(
            self.cwd.first() == Some(&b'/') && !self.cwd.contains(&0),
            "execution context cwd is not an absolute POSIX path",
        )?;
        require(
            self.slots.keys().copied().collect::<BTreeSet<_>>() == BTreeSet::from(SLOTS),
            "execution context lacks the exact closed slot set",
        )?;
        for value in self.slots.values().flatten() {
            require(!value.contains(&0), "execution context value contains NUL")?;
        }
        require(
            self.slots[&ContextSlot::Tmpdir]
                .as_ref()
                .is_some_and(|value| !value.is_empty()),
            "TMPDIR must name an owned temporary directory",
        )?;
        require(
            self.slots[&ContextSlot::BuckScratchPath]
                .as_ref()
                .is_none_or(|value| !value.is_empty()),
            "execution context BUCK_SCRATCH_PATH is empty",
        )
    }

    /// Pure validation/resolution. In particular, retained original directory
    /// spellings are not opened, canonicalized or compared to a new action.
    pub(super) fn resolve(&self, slot: ContextSlot) -> Result<Option<&[u8]>> {
        self.validate()?;
        Ok(self.slots[&slot].as_deref())
    }
}

// No Deserialize, Clone, public constructor or caller-supplied capture hook.
pub(super) struct ExecutionContext {
    environment: BTreeMap<String, Vec<u8>>,
    observation: ContextObservation,
    cwd: PathBuf,
    temporary: PathBuf,
    guards: Vec<(&'static str, DirectoryGuard)>,
}

fn verify_guards(guards: &[(&str, DirectoryGuard)]) -> Result<()> {
    let errors: Vec<_> = guards
        .iter()
        .filter_map(|(label, guard)| {
            guard
                .verify()
                .err()
                .map(|error| format!("{label}: {error}"))
        })
        .collect();
    require(errors.is_empty(), errors.join("; "))
}

impl ExecutionContext {
    /// Acquire once inside the normal owned process before logs or wrapper
    /// launch. Snapshot values are passed to the actual spawn, not reread there.
    pub(super) fn capture() -> Result<Self> {
        let mut environment = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            let name = name
                .into_string()
                .map_err(|_| "execution environment name is not UTF-8")?;
            require(
                environment.insert(name, value.into_vec()).is_none(),
                "duplicate execution environment name",
            )?;
        }
        let cwd = std::env::current_dir().map_err(|e| format!("execution context cwd: {e}"))?;
        Self::acquire(environment, cwd)
    }

    // Kept private: production calls this only with values captured above.
    // Unit controls supply local maps without mutating the process environment.
    fn acquire(mut environment: BTreeMap<String, Vec<u8>>, cwd: PathBuf) -> Result<Self> {
        for (name, value) in &environment {
            require(
                !name.is_empty()
                    && !name.contains('=')
                    && !name.contains('\0')
                    && !value.contains(&0),
                "execution environment contains an invalid name or value",
            )?;
        }
        let observation = ContextObservation {
            cwd: cwd.as_os_str().as_bytes().to_vec(),
            slots: SLOTS
                .into_iter()
                .map(|slot| (slot, environment.get(slot.name()).cloned()))
                .collect(),
        };
        observation.validate()?;
        let temporary = cwd.join(std::ffi::OsString::from_vec(
            observation.slots[&ContextSlot::Tmpdir].clone().unwrap(),
        ));
        let scratch = observation.slots[&ContextSlot::BuckScratchPath]
            .as_ref()
            .map(|value| cwd.join(std::ffi::OsString::from_vec(value.clone())));
        let mut guards = Vec::new();
        let primary = (|| {
            guards.push(("cwd", DirectoryGuard::capture(&cwd)?));
            guards.push(("TMPDIR", DirectoryGuard::capture(&temporary)?));
            if let Some(scratch) = &scratch {
                guards.push(("BUCK_SCRATCH_PATH", DirectoryGuard::capture(scratch)?));
            }
            Ok(())
        })();
        if let Err(error) = primary {
            return match verify_guards(&guards) {
                Ok(()) => Err(error),
                Err(close) => Err(format!("{error}; execution context close: {close}")),
            };
        }
        for name in super::RUNNER_REMOVED_ENVIRONMENT {
            environment.remove(name);
        }
        environment.insert("LC_ALL".into(), b"C".to_vec());
        Ok(Self {
            environment,
            observation,
            cwd,
            temporary,
            guards,
        })
    }

    pub(super) fn effective_environment(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.environment
    }

    /// Choose the complete child map from independent expectations, after
    /// checking every modeled consumer and every selected fixed value against
    /// this capture. Unselected owner variables never become child variables.
    pub(super) fn select_environment(
        &self,
        expected: &BTreeMap<String, Vec<u8>>,
        required: &BTreeSet<String>,
        consumed: &BTreeSet<String>,
        prefixes: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        require(
            expected.get("LC_ALL").map(Vec::as_slice) == Some(b"C"),
            "wrapper child must preserve LC_ALL=C",
        )?;
        for name in required {
            require(
                expected.contains_key(name),
                format!("required wrapper child environment key absent: {name}"),
            )?;
        }
        for (name, value) in &self.environment {
            if consumed.contains(name) || prefixes.iter().any(|prefix| name.starts_with(prefix)) {
                require(
                    expected.get(name) == Some(value),
                    format!("consumed owner environment differs from wrapper child: {name}"),
                )?;
            }
        }
        for (name, value) in expected {
            require(
                self.environment.get(name) == Some(value),
                format!("fixed wrapper child environment differs from captured source: {name}"),
            )?;
        }
        for (slot, value) in &self.observation.slots {
            require(
                expected.get(slot.name()).map(Vec::as_slice) == value.as_deref(),
                "execution context differs from selected wrapper child environment",
            )?;
        }
        Ok(expected.clone())
    }

    pub(super) fn observation(&self) -> ContextObservation {
        self.observation.clone()
    }

    pub(super) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(super) fn temporary(&self) -> &Path {
        &self.temporary
    }

    pub(super) fn close(&self) -> Result<()> {
        verify_guards(&self.guards)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use super::*;

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let temporary =
                std::env::var_os("TMPDIR").expect("context controls require owned TMPDIR");
            let root = PathBuf::from(temporary).join(format!(
                "native-context-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            for name in ["cwd", "tmp", "scratch"] {
                fs::create_dir(root.join(name)).unwrap();
            }
            Self { root }
        }
        fn environment(&self) -> BTreeMap<String, Vec<u8>> {
            BTreeMap::from([
                (
                    "TMPDIR".into(),
                    self.root.join("tmp").into_os_string().into_vec(),
                ),
                (
                    "BUCK_SCRATCH_PATH".into(),
                    self.root.join("scratch").into_os_string().into_vec(),
                ),
                ("PWD".into(), b"/original/logical/pwd".to_vec()),
                ("EXECUTION_ID".into(), b"execution-original".to_vec()),
                ("SESSION_ID".into(), b"session-original".to_vec()),
                ("ACTION_DIGEST".into(), b"opaque-action-id".to_vec()),
                (
                    "FIXED_TOOL_SETTING".into(),
                    b"independent-fixed-value".to_vec(),
                ),
            ])
        }
        fn acquire(&self, environment: BTreeMap<String, Vec<u8>>) -> ExecutionContext {
            ExecutionContext::acquire(environment, self.root.join("cwd")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn real_runner_uses_only_the_captured_effective_map() {
        let fixture = Fixture::new();
        let mut later = fixture.environment();
        for name in super::super::RUNNER_REMOVED_ENVIRONMENT {
            later.insert(name.into(), b"removed-before-owned-spawn".to_vec());
        }
        later.insert("LC_ALL".into(), b"original-locale".to_vec());
        let context = fixture.acquire(later.clone());
        later.insert("EXECUTION_ID".into(), b"later-mutated-id".to_vec());
        later.insert("UNEXPECTED_LATER_KEY".into(), b"not-in-snapshot".to_vec());
        let environment = context.effective_environment();
        assert_eq!(environment["EXECUTION_ID"], b"execution-original");
        assert_eq!(environment["LC_ALL"], b"C");
        assert!(!environment.contains_key("UNEXPECTED_LATER_KEY"));
        for name in super::super::RUNNER_REMOVED_ENVIRONMENT {
            assert!(!environment.contains_key(name));
        }
        let mut runner = super::super::Runner::new(
            context.temporary().join("runner"),
            Duration::from_secs(10),
            64 * 1024,
        )
        .unwrap();
        let output = runner
            .run_with_environment("/usr/bin/env", &[], context.cwd(), environment)
            .unwrap();
        let actual = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let split = line.iter().position(|byte| *byte == b'=').unwrap();
                (
                    String::from_utf8(line[..split].to_vec()).unwrap(),
                    line[split + 1..].to_vec(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(&actual, environment);
        assert!(
            !actual.contains_key("PATH"),
            "ambient PATH entered an explicit spawn"
        );
        assert_eq!(runner.commands.len(), 1);
        assert_eq!(runner.commands[0].status, 0);
        context.close().unwrap();
    }

    #[test]
    fn selected_child_uses_independent_fixed_values_and_preserves_all_consumers() {
        let fixture = Fixture::new();
        let mut expected = fixture.environment();
        expected.extend([
            ("LC_ALL".into(), b"C".to_vec()),
            ("LANG".into(), b"C.UTF-8".to_vec()),
            ("PYTHONUNBUFFERED".into(), b"1".to_vec()),
            ("PATH".into(), b"/independent/tool/bin".to_vec()),
            ("HOME".into(), b"/independent/tool/home".to_vec()),
        ]);
        let required = BTreeSet::from(["LANG".into(), "PYTHONUNBUFFERED".into()]);
        let consumed = BTreeSet::from(["PATH".into(), "HOME".into(), "RUSTC_LOG".into()]);
        let prefixes = BTreeSet::from([
            "PYTHON".into(),
            "LD_".into(),
            "NIX_CC_WRAPPER_TARGET_HOST_".into(),
        ]);
        let mut owner = expected.clone();
        owner.insert(
            "UNRELATED_OWNER_SETTING".into(),
            b"never-child-input".to_vec(),
        );
        for name in super::super::RUNNER_REMOVED_ENVIRONMENT {
            owner.insert(name.into(), b"removed-before-owned-spawn".to_vec());
        }
        let context = fixture.acquire(owner.clone());
        // Mutating the caller's local map cannot alter a captured source or the
        // exact map later supplied to Runner; no process-global env mutation.
        owner.insert("PATH".into(), b"/changed/after/capture".to_vec());
        let selected = context
            .select_environment(&expected, &required, &consumed, &prefixes)
            .unwrap();
        assert_eq!(selected, expected);
        assert!(
            context
                .effective_environment()
                .contains_key("UNRELATED_OWNER_SETTING")
        );
        let mut runner = super::super::Runner::new(
            context.temporary().join("selected-runner"),
            Duration::from_secs(10),
            64 * 1024,
        )
        .unwrap();
        let output = runner
            .run_with_environment("/usr/bin/env", &[], context.cwd(), &selected)
            .unwrap();
        let actual: BTreeMap<_, _> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let at = line.iter().position(|byte| *byte == b'=').unwrap();
                (
                    String::from_utf8(line[..at].to_vec()).unwrap(),
                    line[at + 1..].to_vec(),
                )
            })
            .collect();
        assert_eq!(actual, expected);
        assert!(!actual.contains_key("UNRELATED_OWNER_SETTING"));
        for name in super::super::RUNNER_REMOVED_ENVIRONMENT {
            assert!(!actual.contains_key(name));
            let mut forbidden = expected.clone();
            forbidden.insert(name.into(), b"removed-before-owned-spawn".to_vec());
            assert!(
                context
                    .select_environment(&forbidden, &required, &consumed, &prefixes)
                    .is_err()
            );
        }
        for name in [
            "PATH",
            "HOME",
            "LANG",
            "PYTHONUNBUFFERED",
            "LC_ALL",
            "TMPDIR",
            "EXECUTION_ID",
        ] {
            let mut wrong = expected.clone();
            wrong.insert(name.into(), b"wrong".to_vec());
            assert!(
                context
                    .select_environment(&wrong, &required, &consumed, &prefixes)
                    .is_err()
            );
            wrong.remove(name);
            assert!(
                context
                    .select_environment(&wrong, &required, &consumed, &prefixes)
                    .is_err()
            );
        }
        let mut extra = expected.clone();
        extra.insert("NOT_PRESENT_IN_SOURCE".into(), b"invented".to_vec());
        assert!(
            context
                .select_environment(&extra, &required, &consumed, &prefixes)
                .is_err()
        );
        context.close().unwrap();
        for name in [
            "RUSTC_LOG",
            "NIX_CC_WRAPPER_TARGET_HOST_extra",
            "PYTHONPATH",
            "LD_LIBRARY_PATH",
        ] {
            let mut changed = expected.clone();
            changed.insert(name.into(), b"unmodeled-consumer".to_vec());
            let changed = fixture.acquire(changed);
            assert!(
                changed
                    .select_environment(&expected, &required, &consumed, &prefixes)
                    .is_err()
            );
            changed.close().unwrap();
        }
    }

    #[test]
    fn optional_slots_pwd_and_retained_context_do_not_reopen_original_directories() {
        let fixture = Fixture::new();
        let mut environment = fixture.environment();
        for name in [
            "EXECUTION_ID",
            "SESSION_ID",
            "ACTION_DIGEST",
            "BUCK_SCRATCH_PATH",
        ] {
            environment.remove(name);
        }
        let context = fixture.acquire(environment);
        let observed = context.observation();
        assert_eq!(
            observed.resolve(ContextSlot::Pwd).unwrap(),
            Some(b"/original/logical/pwd".as_slice())
        );
        assert_ne!(
            observed.cwd,
            observed.resolve(ContextSlot::Pwd).unwrap().unwrap()
        );
        for slot in [
            ContextSlot::ExecutionId,
            ContextSlot::SessionId,
            ContextSlot::ActionDigest,
            ContextSlot::BuckScratchPath,
        ] {
            assert_eq!(observed.resolve(slot).unwrap(), None);
        }
        context.close().unwrap();
        drop(context);
        fs::remove_dir(fixture.root.join("tmp")).unwrap();
        fs::remove_dir(fixture.root.join("cwd")).unwrap();
        let retained: ContextObservation =
            serde_json::from_slice(&serde_json::to_vec(&observed).unwrap()).unwrap();
        retained.validate().unwrap();
        assert_eq!(retained, observed);
        let mut invalid = retained.clone();
        invalid.slots.remove(&ContextSlot::Pwd);
        assert!(invalid.validate().is_err());
        invalid = retained;
        invalid.slots.insert(ContextSlot::Tmpdir, None);
        assert!(invalid.validate().is_err());
        assert!(serde_json::from_str::<ContextSlot>("\"arbitrary_environment_name\"").is_err());
    }

    #[test]
    fn temporary_contents_may_change_but_all_replaced_directory_identities_refuse() {
        let fixture = Fixture::new();
        let context = fixture.acquire(fixture.environment());
        fs::write(context.temporary().join("new-log"), b"owned content").unwrap();
        fs::write(
            fixture.root.join("scratch/response"),
            b"normalized response",
        )
        .unwrap();
        context.close().unwrap();
        for name in ["cwd", "tmp", "scratch"] {
            fs::rename(
                fixture.root.join(name),
                fixture.root.join(format!("old-{name}")),
            )
            .unwrap();
            fs::create_dir(fixture.root.join(name)).unwrap();
        }
        let error = context.close().unwrap_err();
        for label in ["cwd:", "TMPDIR:", "BUCK_SCRATCH_PATH:"] {
            assert!(error.contains(label), "{error}");
        }
    }

    #[test]
    fn same_target_replaced_scratch_link_and_absent_required_tmpdir_refuse() {
        let fixture = Fixture::new();
        let link = fixture.root.join("scratch-link");
        symlink(fixture.root.join("scratch"), &link).unwrap();
        let mut environment = fixture.environment();
        environment.insert(
            "BUCK_SCRATCH_PATH".into(),
            link.clone().into_os_string().into_vec(),
        );
        let context = fixture.acquire(environment.clone());
        context.close().unwrap();
        fs::rename(&link, fixture.root.join("old-link")).unwrap();
        symlink(fixture.root.join("scratch"), &link).unwrap();
        assert!(context.close().is_err());
        environment.remove("TMPDIR");
        assert!(ExecutionContext::acquire(environment.clone(), fixture.root.join("cwd")).is_err());
        environment.insert("TMPDIR".into(), Vec::new());
        assert!(ExecutionContext::acquire(environment, fixture.root.join("cwd")).is_err());
    }
}
