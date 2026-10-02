/* Copyright (c) Meta Platforms, Inc. and affiliates.
 * Licensed under the BSD-style license in the root LICENSE file. */

use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

const BRIDGE: &str = "COHORT_BRIDGE_FIXTURE";

fn native_name(variable: &str) -> Result<Option<&'static str>, String> {
    Ok(Some(match variable {
        BRIDGE => return Ok(None),
        "REVERIE_SOURCE_OBSERVATION_FIXTURE" => "source-observation",
        "REVERIE_CLONE3_JOIN_FIXTURE" => "clone3-join",
        "REVERIE_PRIVATE_SIGNAL_FIXTURE" => "private-signal",
        "REVERIE_PRIVATE_REPLAY_FIXTURE" => "private-replay",
        "REVERIE_PRIVATE_CONTINUATION_FIXTURE" => "private-continuation",
        "REVERIE_PRIVATE_TIMER_FIXTURE" => "private-timer",
        _ => return Err(format!("unknown fixture variable {variable}")),
    }))
}

// The explicit inputs let unit controls exercise all decisions without racing
// other libtests through process-global environment or working-directory edits.
fn resolve(
    variable: &str,
    explicit: Option<&OsStr>,
    native_dir: Option<&Path>,
    test_executable: Option<&Path>,
    inspect: impl FnOnce(&Path) -> io::Result<(bool, u32)>,
) -> Result<PathBuf, String> {
    let native = native_name(variable)?;
    let path = if let Some(explicit) = explicit {
        PathBuf::from(explicit)
    } else if let Some(name) = native {
        native_dir
            .ok_or_else(|| format!("no Cargo native fixture directory; set {variable}"))?
            .join(name)
    } else {
        let deps = test_executable
            .and_then(Path::parent)
            .filter(|path| path.file_name() == Some(OsStr::new("deps")))
            .ok_or_else(|| format!("nonstandard libtest layout; set {BRIDGE}"))?;
        deps.parent()
            .ok_or_else(|| format!("no Cargo profile directory; set {BRIDGE}"))?
            .join("examples/cohort_bridge_fixture")
    };
    if !path.is_absolute() {
        return Err(format!(
            "{variable} fixture must be an absolute path: {path:?}"
        ));
    }
    let (is_file, mode) = inspect(&path).map_err(|error| {
        format!("{variable} fixture {path:?}: {error}; build native fixtures and the cohort_bridge_fixture example, or set the absolute override")
    })?;
    if !is_file || mode & 0o111 == 0 {
        return Err(format!(
            "{variable} fixture is not an executable file: {path:?}"
        ));
    }
    Ok(path)
}

pub(crate) fn fixture_path(variable: &str) -> OsString {
    let explicit = std::env::var_os(variable);
    let executable = if explicit.is_none() && variable == BRIDGE {
        Some(std::env::current_exe().expect("cannot locate the current libtest executable"))
    } else {
        None
    };
    resolve(
        variable,
        explicit.as_deref(),
        option_env!("OUT_DIR").map(Path::new),
        executable.as_deref(),
        |path| {
            let metadata = std::fs::metadata(path)?;
            Ok((metadata.is_file(), metadata.permissions().mode()))
        },
    )
    .unwrap_or_else(|error| panic!("{error}"))
    .into_os_string()
}

#[test]
fn explicit_fixture_path_wins_without_default_layout() {
    for variable in [BRIDGE, "REVERIE_PRIVATE_SIGNAL_FIXTURE"] {
        let actual = resolve(
            variable,
            Some(OsStr::new("/owned/fixture")),
            None,
            None,
            |path| {
                assert_eq!(path, Path::new("/owned/fixture"));
                Ok((true, 0o100755))
            },
        )
        .unwrap();
        assert_eq!(actual, Path::new("/owned/fixture"));
    }
}

#[test]
fn all_native_defaults_and_standard_example_path_are_exact() {
    for (variable, name) in [
        ("REVERIE_SOURCE_OBSERVATION_FIXTURE", "source-observation"),
        ("REVERIE_CLONE3_JOIN_FIXTURE", "clone3-join"),
        ("REVERIE_PRIVATE_SIGNAL_FIXTURE", "private-signal"),
        ("REVERIE_PRIVATE_REPLAY_FIXTURE", "private-replay"),
        (
            "REVERIE_PRIVATE_CONTINUATION_FIXTURE",
            "private-continuation",
        ),
        ("REVERIE_PRIVATE_TIMER_FIXTURE", "private-timer"),
    ] {
        let actual = resolve(
            variable,
            None,
            Some(Path::new("/cargo/out")),
            None,
            |path| {
                assert_eq!(path, Path::new("/cargo/out").join(name));
                Ok((true, 0o100755))
            },
        )
        .unwrap();
        assert_eq!(actual, Path::new("/cargo/out").join(name));
    }
    let actual = resolve(
        BRIDGE,
        None,
        None,
        Some(Path::new("/custom/target/debug/deps/libtest")),
        |path| {
            assert_eq!(
                path,
                Path::new("/custom/target/debug/examples/cohort_bridge_fixture")
            );
            Ok((true, 0o100755))
        },
    )
    .unwrap();
    assert_eq!(
        actual,
        Path::new("/custom/target/debug/examples/cohort_bridge_fixture")
    );
}

#[test]
fn invalid_overrides_never_fall_back() {
    for explicit in ["", "relative/fixture"] {
        assert!(
            resolve(
                BRIDGE,
                Some(OsStr::new(explicit)),
                None,
                Some(Path::new("/cargo/debug/deps/libtest")),
                |_| panic!("must reject before inspection")
            )
            .unwrap_err()
            .contains("absolute path")
        );
    }
    for facts in [(false, 0o40755), (true, 0o100644)] {
        assert!(
            resolve(
                BRIDGE,
                Some(OsStr::new("/bad/fixture")),
                None,
                Some(Path::new("/cargo/debug/deps/libtest")),
                |path| {
                    assert_eq!(path, Path::new("/bad/fixture"));
                    Ok(facts)
                }
            )
            .unwrap_err()
            .contains("not an executable file")
        );
    }
    assert!(
        resolve(
            BRIDGE,
            Some(OsStr::new("/missing/fixture")),
            None,
            Some(Path::new("/cargo/debug/deps/libtest")),
            |path| {
                assert_eq!(path, Path::new("/missing/fixture"));
                Err(io::Error::from(io::ErrorKind::NotFound))
            }
        )
        .unwrap_err()
        .contains("/missing/fixture")
    );
}

#[test]
fn missing_defaults_and_unknown_fixture_refuse() {
    assert!(
        resolve(
            "REVERIE_PRIVATE_SIGNAL_FIXTURE",
            None,
            None,
            None,
            |_| panic!("no native default to inspect")
        )
        .unwrap_err()
        .contains("no Cargo native fixture directory")
    );
    assert!(
        resolve(
            BRIDGE,
            None,
            None,
            Some(Path::new("/relocated/libtest")),
            |_| panic!("no standard example path")
        )
        .unwrap_err()
        .contains("nonstandard libtest layout")
    );
    assert!(
        resolve(
            BRIDGE,
            None,
            None,
            Some(Path::new("/cargo/debug/deps/libtest")),
            |_| Err(io::Error::from(io::ErrorKind::NotFound))
        )
        .unwrap_err()
        .contains("build native fixtures and the cohort_bridge_fixture example")
    );
    assert!(
        resolve(
            "UNRECOGNIZED_FIXTURE",
            Some(OsStr::new("/owned/fixture")),
            None,
            None,
            |_| panic!("unknown names must not inspect a file")
        )
        .unwrap_err()
        .contains("unknown fixture variable")
    );
}
