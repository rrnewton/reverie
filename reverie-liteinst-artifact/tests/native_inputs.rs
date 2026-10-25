// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

// These tests check input joins and transaction close, not qualification authority.
#[path = "../src/native_inputs.rs"]
mod native_inputs;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use native_inputs::Configuration;
use native_inputs::Dependency;
use native_inputs::Input;
use native_inputs::Inputs;
use native_inputs::Output;
use native_inputs::with_inputs;
use sha2::Digest;
use sha2::Sha256;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    expected: Inputs,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "native-inputs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let root = directory.join("root");
        fs::create_dir(&root).unwrap();
        let source = b"source input";
        let generated = b"generated input";
        let dependency = b"dependency bytes";
        let output = b"selected product bytes";
        for (path, bytes) in [
            ("source.rs", source.as_slice()),
            ("generated.rs", generated.as_slice()),
            ("deps/libdependency.rlib", dependency.as_slice()),
            ("build/runtime.so", output.as_slice()),
            ("stage/runtime.so", output.as_slice()),
        ] {
            write(&root, path, bytes);
        }
        let expected = Inputs {
            configuration: Configuration {
                producer: "//example:runtime".into(),
                identity: "configuration-a".into(),
                arguments: vec!["--setting=first".into(), "--setting=last".into()],
                environment: BTreeMap::from([("SETTING".into(), "value".into())]),
            },
            inputs: vec![
                Input {
                    role: "source".into(),
                    path: "source.rs".into(),
                    sha256: digest(source),
                },
                Input {
                    role: "generated".into(),
                    path: "generated.rs".into(),
                    sha256: digest(generated),
                },
                Input {
                    role: "dependency".into(),
                    path: "deps/libdependency.rlib".into(),
                    sha256: digest(dependency),
                },
            ],
            dependencies: vec![Dependency {
                input_role: "dependency".into(),
                producer: "//example:dependency".into(),
                configuration: "dependency-configuration-a".into(),
                output_role: "library".into(),
            }],
            outputs: vec![Output {
                role: "runtime".into(),
                selected: "build/runtime.so".into(),
                staged: Some("stage/runtime.so".into()),
                sha256: digest(output),
            }],
        };
        Self {
            directory,
            root,
            expected,
        }
    }

    fn refused(&self, expected: &Inputs, observed: &Inputs, reason: &str) {
        let called = Cell::new(false);
        let error = with_inputs(&self.root, expected, observed, || {
            called.set(true);
            Ok(())
        })
        .unwrap_err();
        assert!(!called.get(), "action ran before refusing {reason}");
        assert!(
            error.contains(reason),
            "{error:?} did not contain {reason:?}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn write(root: &Path, path: &str, bytes: &[u8]) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn matching_parsed_descriptions_and_real_files_close_successfully() {
    let fixture = Fixture::new();
    let json = serde_json::to_vec(&fixture.expected).unwrap();
    let mut observed: Inputs = serde_json::from_slice(&json).unwrap();
    observed.inputs.reverse(); // Roles, not list positions, join the files.
    let result = with_inputs(&fixture.root, &fixture.expected, &observed, || Ok(37)).unwrap();
    assert_eq!(result, 37); // The only returned value is the caller's own value.
}

#[test]
fn parsing_does_not_validate_files_or_supply_authority() {
    let fixture = Fixture::new();
    let json = serde_json::to_vec(&fixture.expected).unwrap();
    fs::remove_file(fixture.root.join("source.rs")).unwrap();
    let observed: Inputs = serde_json::from_slice(&json).unwrap();
    assert_eq!(observed, fixture.expected);
    fixture.refused(&fixture.expected, &observed, "resolve");
}

#[test]
fn missing_and_duplicate_roles_are_refused() {
    let fixture = Fixture::new();
    for field in 0..3 {
        let mut missing = fixture.expected.clone();
        match field {
            0 => {
                missing.inputs.remove(0);
            }
            1 => missing.outputs.clear(),
            2 => missing.dependencies.clear(),
            _ => unreachable!(),
        }
        fixture.refused(&fixture.expected, &missing, "differ");
        let mut duplicate = fixture.expected.clone();
        match field {
            0 => duplicate.inputs.push(duplicate.inputs[0].clone()),
            1 => duplicate.outputs.push(duplicate.outputs[0].clone()),
            2 => duplicate
                .dependencies
                .push(duplicate.dependencies[0].clone()),
            _ => unreachable!(),
        }
        fixture.refused(&fixture.expected, &duplicate, "duplicate role");
    }
}

#[test]
fn duplicate_missing_and_unmapped_paths_are_refused() {
    let fixture = Fixture::new();
    let mut duplicate = fixture.expected.clone();
    duplicate.inputs[1].path = duplicate.inputs[0].path.clone();
    fixture.refused(&duplicate, &duplicate, "duplicate file mapping");
    let mut duplicate_output = fixture.expected.clone();
    duplicate_output.outputs[0].staged = Some(duplicate_output.outputs[0].selected.clone());
    fixture.refused(
        &duplicate_output,
        &duplicate_output,
        "duplicate file mapping",
    );
    let mut missing = fixture.expected.clone();
    missing.inputs[0].path = "absent.rs".into();
    fixture.refused(&missing, &missing, "resolve");
    let mut unmapped = fixture.expected.clone();
    unmapped.dependencies[0].input_role = "absent-role".into();
    fixture.refused(&unmapped, &unmapped, "missing dependency input role");
}

#[test]
fn independently_expected_source_digest_is_checked_against_bytes() {
    let fixture = Fixture::new();
    let mut wrong = fixture.expected.clone();
    wrong.inputs[0].sha256 = digest(b"different source");
    fixture.refused(&wrong, &fixture.expected, "input roles/mappings differ");
    // Agreement between two records cannot substitute for checking the file.
    fixture.refused(&wrong, &wrong, "file digest differs: source.rs");
}

#[test]
fn invalid_paths_and_digests_are_refused() {
    let fixture = Fixture::new();
    for path in ["", "../source.rs", "/source.rs"] {
        let mut wrong = fixture.expected.clone();
        wrong.inputs[0].path = path.into();
        fixture.refused(&wrong, &wrong, "relative path without traversal");
    }
    let mut wrong = fixture.expected.clone();
    wrong.inputs[0].sha256 = "not a digest".into();
    fixture.refused(&wrong, &wrong, "invalid SHA256");
    wrong = fixture.expected.clone();
    wrong.inputs[0].role.clear();
    fixture.refused(&wrong, &wrong, "empty role");
}

#[test]
fn escaping_leaf_and_directory_symlinks_are_refused() {
    for directory_link in [false, true] {
        let fixture = Fixture::new();
        let outside = fixture.directory.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::copy(fixture.root.join("source.rs"), outside.join("source.rs")).unwrap();
        fs::remove_file(fixture.root.join("source.rs")).unwrap();
        let mut expected = fixture.expected.clone();
        if directory_link {
            symlink(&outside, fixture.root.join("link")).unwrap();
            expected.inputs[0].path = "link/source.rs".into();
        } else {
            symlink(outside.join("source.rs"), fixture.root.join("source.rs")).unwrap();
        }
        fixture.refused(&expected, &expected, "escapes root");
    }
}

#[test]
fn swapping_an_internal_symlink_to_identical_bytes_fails_close() {
    let fixture = Fixture::new();
    fs::rename(
        fixture.root.join("source.rs"),
        fixture.root.join("first.rs"),
    )
    .unwrap();
    fs::copy(
        fixture.root.join("first.rs"),
        fixture.root.join("second.rs"),
    )
    .unwrap();
    symlink("first.rs", fixture.root.join("source.rs")).unwrap();
    assert!(
        with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || Ok(
            ()
        ))
        .is_ok()
    );
    let error = with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || {
        fs::remove_file(fixture.root.join("source.rs")).unwrap();
        symlink("second.rs", fixture.root.join("source.rs")).unwrap();
        Ok(())
    })
    .unwrap_err();
    assert!(
        error.contains("input close failed: file identity changed at close: source.rs"),
        "{error}"
    );
}

#[test]
fn aliased_file_mappings_are_refused() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join("generated.rs")).unwrap();
    symlink("source.rs", fixture.root.join("generated.rs")).unwrap();
    let mut expected = fixture.expected.clone();
    expected.inputs[1].sha256 = expected.inputs[0].sha256.clone();
    fixture.refused(&expected, &expected, "duplicate resolved file mapping");
}

#[test]
fn indirect_symlink_targets_are_refused_before_action() {
    let fixture = Fixture::new();
    fs::rename(
        fixture.root.join("source.rs"),
        fixture.root.join("actual.rs"),
    )
    .unwrap();
    symlink("actual.rs", fixture.root.join("alias.rs")).unwrap();
    symlink("alias.rs", fixture.root.join("source.rs")).unwrap();
    // Replacing alias.rs could preserve the final bytes and source.rs link.
    // This unsupported chain must be refused, rather than silently untracked.
    fixture.refused(
        &fixture.expected,
        &fixture.expected,
        "indirect symlink target",
    );
}

#[test]
fn introducing_an_indirect_symlink_fails_close() {
    let fixture = Fixture::new();
    fs::rename(
        fixture.root.join("source.rs"),
        fixture.root.join("alias.rs"),
    )
    .unwrap();
    symlink("alias.rs", fixture.root.join("source.rs")).unwrap();
    let error = with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || {
        fs::rename(
            fixture.root.join("alias.rs"),
            fixture.root.join("actual.rs"),
        )
        .unwrap();
        symlink("actual.rs", fixture.root.join("alias.rs")).unwrap();
        Ok(())
    })
    .unwrap_err();
    assert!(
        error.contains("input close failed: indirect symlink target"),
        "{error}"
    );
}

#[test]
fn source_and_generated_mutation_each_fail_close() {
    for path in ["source.rs", "generated.rs"] {
        let fixture = Fixture::new();
        let error = with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || {
            fs::write(fixture.root.join(path), b"changed after opening").unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.contains(&format!("input close failed: file digest differs: {path}")),
            "{error}"
        );
    }
}

#[test]
fn same_basename_output_substitutions_are_refused_for_selected_and_staged() {
    for path in ["build/runtime.so", "stage/runtime.so"] {
        let fixture = Fixture::new();
        write(&fixture.root, "unrelated/runtime.so", b"different product");
        let mut observed = fixture.expected.clone();
        if path.starts_with("build/") {
            observed.outputs[0].selected = "unrelated/runtime.so".into();
        } else {
            observed.outputs[0].staged = Some("unrelated/runtime.so".into());
        }
        fixture.refused(&fixture.expected, &observed, "output roles/mappings differ");
        fs::copy(
            fixture.root.join("unrelated/runtime.so"),
            fixture.root.join(path),
        )
        .unwrap();
        fixture.refused(
            &fixture.expected,
            &fixture.expected,
            &format!("file digest differs: {path}"),
        );
    }
}

#[test]
fn selected_and_staged_replacement_with_identical_bytes_each_fail_close() {
    for path in ["build/runtime.so", "stage/runtime.so"] {
        let fixture = Fixture::new();
        fs::copy(fixture.root.join(path), fixture.root.join("replacement.so")).unwrap();
        let error = with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || {
            fs::rename(fixture.root.join("replacement.so"), fixture.root.join(path)).unwrap();
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.contains(&format!(
                "input close failed: file identity changed at close: {path}"
            )),
            "{error}"
        );
    }
}

#[test]
fn configured_producer_settings_and_dependency_joins_must_match() {
    let fixture = Fixture::new();
    for field in 0..4 {
        let mut observed = fixture.expected.clone();
        match field {
            0 => observed.configuration.producer = "//other:producer".into(),
            1 => observed.configuration.identity = "other-configuration".into(),
            2 => observed.configuration.arguments.reverse(),
            3 => {
                observed
                    .configuration
                    .environment
                    .insert("SETTING".into(), "other".into());
            }
            _ => unreachable!(),
        }
        fixture.refused(
            &fixture.expected,
            &observed,
            "configured producer/settings differ",
        );
    }
    for field in 0..4 {
        let mut observed = fixture.expected.clone();
        match field {
            0 => observed.dependencies[0].producer = "//other:dependency".into(),
            1 => observed.dependencies[0].configuration = "other-configuration".into(),
            2 => observed.dependencies[0].output_role = "other-product".into(),
            3 => observed.dependencies[0].input_role = "source".into(),
            _ => unreachable!(),
        }
        fixture.refused(&fixture.expected, &observed, "dependency join differs");
    }
}

#[test]
fn primary_and_close_errors_are_both_preserved() {
    let fixture = Fixture::new();
    let primary: Result<(), String> =
        with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || {
            Err("primary failure".into())
        });
    assert_eq!(primary.unwrap_err(), "primary failure");
    let both: Result<(), String> =
        with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || {
            fs::write(fixture.root.join("generated.rs"), b"changed").unwrap();
            Err("primary failure".into())
        });
    assert_eq!(
        both.unwrap_err(),
        "primary failure; input close failed: file digest differs: generated.rs"
    );
}

#[test]
fn all_changed_files_are_reported_with_and_without_primary_failure() {
    for primary_failure in [false, true] {
        let fixture = Fixture::new();
        let result: Result<(), String> =
            with_inputs(&fixture.root, &fixture.expected, &fixture.expected, || {
                // The first input and final staged output must both be checked.
                fs::write(fixture.root.join("source.rs"), b"changed source").unwrap();
                fs::write(fixture.root.join("stage/runtime.so"), b"changed product").unwrap();
                if primary_failure {
                    Err("primary failure".into())
                } else {
                    Ok(())
                }
            });
        let close = "input close failed: file digest differs: source.rs; file digest differs: stage/runtime.so";
        let expected = if primary_failure {
            format!("primary failure; {close}")
        } else {
            close.to_owned()
        };
        assert_eq!(result.unwrap_err(), expected);
    }
}
