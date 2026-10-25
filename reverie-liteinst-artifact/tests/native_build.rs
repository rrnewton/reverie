// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Synthetic command fixtures exercise owned invocation and file-transaction
//! mechanics. They never compile a runtime, emit native action evidence, or
//! create a qualification token. No Buck executable is invoked by these tests.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reverie_liteinst_artifact::FileIdentity;
use reverie_liteinst_artifact::Runner;
use reverie_liteinst_artifact::native_build::BuildRequest;
use reverie_liteinst_artifact::native_build::ObservationKind;
use reverie_liteinst_artifact::native_build::with_owned_build_report;
use reverie_liteinst_artifact::native_inputs::Configuration;
use reverie_liteinst_artifact::native_inputs::Input;
use reverie_liteinst_artifact::native_inputs::Inputs;
use reverie_liteinst_artifact::native_results::ExpectedOutput;
use reverie_liteinst_artifact::sha256;
use serde_json::json;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    directory: PathBuf,
    root: PathBuf,
    inputs: Inputs,
    request: BuildRequest,
    runner: Runner,
}

impl Fixture {
    fn new(exit: u8) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "native-owned-build-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let root = directory.join("root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("out")).unwrap();
        fs::write(root.join("source.rs"), b"independent source").unwrap();
        let report = json!({
            "trace_id": "synthetic-owned-command-trace",
            "project_root": root,
            "success": true,
            "truncated": false,
            "results": {"test//:runtime": {
                "errors": [],
                "configured": {"configuration-full": {
                    "success": "SUCCESS", "errors": [],
                    "outputs": {"cdylib": ["out/runtime.so"]},
                    "other_outputs": {}, "configured_graph_size": null
                }}
            }},
            "failures": {}, "strings": {}
        });
        fs::write(
            root.join("report-fixture.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        let script = format!(
            "#!/bin/sh\nset -eu\nreport=\nfor arg in \"$@\"; do\n case \"$arg\" in --build-report=*) report=${{arg#--build-report=}};; esac\ndone\ncp report-fixture.json \"$report\"\nprintf 'synthetic product bytes' > out/runtime.so\nexit {exit}\n"
        );
        let executable = root.join("synthetic-build-program");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let inputs = Inputs {
            configuration: Configuration {
                producer: "test//:runtime".into(),
                identity: "configuration-full".into(),
                arguments: vec!["fixture transaction only".into()],
                environment: BTreeMap::new(),
            },
            inputs: [
                "source.rs",
                "report-fixture.json",
                "synthetic-build-program",
            ]
            .into_iter()
            .map(|name| Input {
                role: name.into(),
                path: name.into(),
                sha256: sha256(&fs::read(root.join(name)).unwrap()),
            })
            .collect(),
            dependencies: Vec::new(),
            outputs: Vec::new(),
        };
        let report_path = root.join("owned-report.json");
        let request = BuildRequest {
            executable: FileIdentity::read(&executable).unwrap(),
            arguments: vec![
                "build".into(),
                format!("--build-report={}", report_path.display()),
                "--materializations=all".into(),
                "test//:runtime[cdylib]".into(),
            ],
            working_directory: root.clone(),
            report_path,
            outputs: vec![ExpectedOutput {
                role: "runtime".into(),
                target: "test//:runtime".into(),
                configuration: "configuration-full".into(),
                provider: "cdylib".into(),
                paths: vec!["out/runtime.so".into()],
            }],
        };
        let runner = Runner::new(directory.join("logs"), Duration::from_secs(10), 65536).unwrap();
        Self {
            directory,
            root,
            inputs,
            request,
            runner,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn owns_command_and_reopens_exact_selected_bytes_without_qualification_authority() {
    let mut f = Fixture::new(0);
    let observation = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |data, _| {
            assert_eq!(data.kind, ObservationKind::OwnedBuildReport);
            assert_eq!(data.command.status, 0);
            assert_eq!(data.selected_files.len(), 1);
            assert_eq!(data.selected_files[0].role, "runtime");
            assert_eq!(
                data.selected_files[0].reported_path,
                PathBuf::from("out/runtime.so")
            );
            assert_eq!(
                data.selected_files[0].file.sha256,
                sha256(b"synthetic product bytes")
            );
            Ok(data.clone())
        },
    )
    .unwrap();
    assert_eq!(f.runner.commands.len(), 1);
    assert_eq!(observation.command, f.runner.commands[0]);
    // The returned value is expressly raw observation data, not an opaque source.
    let encoded = serde_json::to_value(&observation).unwrap();
    assert_eq!(encoded["kind"], "reverie-native-owned-build-observation/v1");
    assert!(serde_json::from_value::<reverie_liteinst_artifact::CompiledSources>(encoded).is_err());
}

#[test]
fn preexisting_report_and_materialization_override_refuse_before_command() {
    for mutation in 0..4 {
        let mut f = Fixture::new(0);
        match mutation {
            0 => fs::write(&f.request.report_path, b"saved successful report").unwrap(),
            1 => f.request.arguments.push("--materializations=none".into()),
            2 => f.request.arguments.push("--build-report=other.json".into()),
            3 => f.request.arguments[0] = "log".into(),
            _ => unreachable!(),
        }
        let called = Cell::new(false);
        assert!(
            with_owned_build_report(
                &f.root,
                &f.inputs,
                &f.inputs,
                &f.request,
                &mut f.runner,
                |_, _| {
                    called.set(true);
                    Ok(())
                },
            )
            .is_err()
        );
        assert!(!called.get());
        assert!(f.runner.commands.is_empty());
    }
}

#[test]
fn successful_report_cannot_hide_failed_owned_command() {
    let mut f = Fixture::new(7);
    let called = Cell::new(false);
    let error = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |_, _| {
            called.set(true);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.contains("failed"), "{error}");
    assert!(!called.get());
    assert_eq!(f.runner.commands.len(), 1);
    assert_eq!(f.runner.commands[0].status, 7);
    assert!(f.request.report_path.is_file());
}

#[test]
fn wrong_configured_role_or_complete_path_never_enters_callback() {
    for mutation in 0..3 {
        let mut f = Fixture::new(0);
        match mutation {
            0 => f.request.outputs[0].configuration = "other-configuration".into(),
            1 => f.request.outputs[0].provider = "DEFAULT".into(),
            2 => f.request.outputs[0].paths = vec!["elsewhere/runtime.so".into()],
            _ => unreachable!(),
        }
        let called = Cell::new(false);
        assert!(
            with_owned_build_report(
                &f.root,
                &f.inputs,
                &f.inputs,
                &f.request,
                &mut f.runner,
                |_, _| {
                    called.set(true);
                    Ok(())
                },
            )
            .is_err()
        );
        assert!(!called.get());
        assert_eq!(f.runner.commands.len(), 1);
    }
}

#[test]
fn primary_output_and_source_close_errors_are_all_preserved() {
    let mut f = Fixture::new(0);
    let root = f.root.clone();
    let error = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |_, _| {
            fs::write(root.join("out/runtime.so"), b"changed output").unwrap();
            fs::write(root.join("source.rs"), b"changed source").unwrap();
            Err::<(), _>("primary qualifier refusal".into())
        },
    )
    .unwrap_err();
    for expected in [
        "primary qualifier refusal",
        "out/runtime.so",
        "source.rs",
        "input close failed",
    ] {
        assert!(error.contains(expected), "missing {expected}: {error}");
    }
}

#[test]
fn successful_callback_still_requires_report_and_output_close() {
    for mutate_report in [false, true] {
        let mut f = Fixture::new(0);
        let path = if mutate_report {
            f.request.report_path.clone()
        } else {
            f.root.join("out/runtime.so")
        };
        let error = with_owned_build_report(
            &f.root,
            &f.inputs,
            &f.inputs,
            &f.request,
            &mut f.runner,
            |_, _| {
                fs::write(&path, b"changed bytes").unwrap();
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("close failed"), "{error}");
    }
}

fn set_report_destination(fixture: &mut Fixture, path: PathBuf) {
    fixture.request.arguments[1] = format!("--build-report={}", path.display());
    fixture.request.report_path = path;
}

fn insert_before_synthetic_exit(fixture: &mut Fixture, exit: u8, extra: &str) {
    let path = &fixture.request.executable.path;
    let script = fs::read_to_string(path).unwrap();
    let ending = format!("exit {exit}\n");
    let before_exit = script.strip_suffix(&ending).unwrap();
    fs::write(path, format!("{before_exit}{extra}\n{ending}")).unwrap();
    fixture.request.executable = FileIdentity::read(path).unwrap();
    for input in &mut fixture.inputs.inputs {
        if input.role == "synthetic-build-program" {
            input.sha256 = fixture.request.executable.sha256.clone();
        }
    }
}

fn replace_with_identical_bytes(path: &Path) {
    let before = FileIdentity::read(path).unwrap();
    let metadata = fs::metadata(path).unwrap();
    let replacement = path.parent().unwrap().join(format!(
        "identical-replacement-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(path, &replacement).unwrap();
    let replacement_metadata = fs::metadata(&replacement).unwrap();
    assert_ne!(
        (metadata.dev(), metadata.ino()),
        (replacement_metadata.dev(), replacement_metadata.ino())
    );
    fs::rename(replacement, path).unwrap();
    // The historical path/hash identity deliberately cannot see this change.
    assert_eq!(FileIdentity::read(path).unwrap(), before);
}

#[test]
fn same_byte_report_tool_and_command_log_replacements_fail_close() {
    for selected in ["report", "tool", "stdout", "stderr"] {
        let mut f = Fixture::new(0);
        // Exercise the build's tool guard, independently of source inputs.
        f.inputs
            .inputs
            .retain(|input| input.role != "synthetic-build-program");
        let called = Cell::new(false);
        let error = with_owned_build_report(
            &f.root,
            &f.inputs,
            &f.inputs,
            &f.request,
            &mut f.runner,
            |data, _| {
                called.set(true);
                let path = match selected {
                    "report" => &data.report_file.path,
                    "tool" => &data.executable.path,
                    "stdout" => &data.command.stdout.path,
                    "stderr" => &data.command.stderr.path,
                    _ => unreachable!(),
                };
                replace_with_identical_bytes(path);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(called.get(), "{selected}: {error}");
        assert!(error.contains("owned build close failed"), "{error}");
        let path = match selected {
            "report" => &f.request.report_path,
            "tool" => &f.request.executable.path,
            "stdout" => &f.runner.commands[0].stdout.path,
            "stderr" => &f.runner.commands[0].stderr.path,
            _ => unreachable!(),
        };
        assert!(
            error.contains(path.to_str().unwrap()),
            "{selected}: {error}"
        );
    }
}

#[test]
fn report_ancestor_retarget_fails_with_old_canonical_file_intact() {
    let mut f = Fixture::new(0);
    let original_parent = f.directory.join("report-original");
    let other_parent = f.directory.join("report-other");
    let alias = f.directory.join("report-alias");
    fs::create_dir(&original_parent).unwrap();
    fs::create_dir(&other_parent).unwrap();
    symlink(&original_parent, &alias).unwrap();
    set_report_destination(&mut f, alias.join("owned-report.json"));
    let called = Cell::new(false);
    let error = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |data, _| {
            called.set(true);
            let other_report = other_parent.join("owned-report.json");
            fs::copy(&data.report_file.path, &other_report).unwrap();
            fs::remove_file(&alias).unwrap();
            symlink(&other_parent, &alias).unwrap();
            data.report_file.verify().unwrap();
            assert_eq!(
                fs::read(&data.report_file.path).unwrap(),
                fs::read(alias.join("owned-report.json")).unwrap()
            );
            assert_ne!(
                fs::canonicalize(alias.join("owned-report.json")).unwrap(),
                data.report_file.path
            );
            Ok(())
        },
    )
    .unwrap_err();
    assert!(called.get(), "{error}");
    assert!(error.contains("owned build close failed"), "{error}");
    assert!(error.contains("report-alias"), "{error}");
    assert!(original_parent.join("owned-report.json").is_file());
}

#[test]
fn hardlinked_report_cannot_hide_parent_directory_replacement() {
    let mut f = Fixture::new(0);
    let report_parent = f.directory.join("report-parent");
    let replacement_parent = f.directory.join("report-replacement");
    let retained_parent = f.directory.join("report-retained");
    fs::create_dir(&report_parent).unwrap();
    fs::create_dir(&replacement_parent).unwrap();
    set_report_destination(&mut f, report_parent.join("owned-report.json"));
    // Make the hardlink before report observation, so the callback changes no
    // report inode, link count, timestamps or bytes. Only its parent changes.
    insert_before_synthetic_exit(
        &mut f,
        0,
        "ln \"$report\" ../report-replacement/owned-report.json",
    );
    let called = Cell::new(false);
    let error = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |data, _| {
            called.set(true);
            let before = fs::metadata(&data.report_file.path).unwrap();
            let parent_before = fs::metadata(&report_parent).unwrap();
            fs::rename(&report_parent, &retained_parent).unwrap();
            fs::rename(&replacement_parent, &report_parent).unwrap();
            let after = fs::metadata(&data.report_file.path).unwrap();
            let parent_after = fs::metadata(&report_parent).unwrap();
            let file_state = |metadata: &fs::Metadata| {
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.len(),
                    metadata.nlink(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            };
            assert_eq!(file_state(&before), file_state(&after));
            assert_ne!(
                (parent_before.dev(), parent_before.ino()),
                (parent_after.dev(), parent_after.ino())
            );
            data.report_file.verify().unwrap();
            Ok(())
        },
    )
    .unwrap_err();
    assert!(called.get(), "{error}");
    assert!(error.contains("owned build close failed"), "{error}");
    assert!(error.contains("report-parent"), "{error}");
}

#[test]
fn primary_error_keeps_every_independent_file_close_failure() {
    let mut f = Fixture::new(0);
    f.inputs
        .inputs
        .retain(|input| input.role != "synthetic-build-program");
    let error = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |data, _| {
            for file in [
                &data.executable,
                &data.report_file,
                &data.command.stdout,
                &data.command.stderr,
            ] {
                replace_with_identical_bytes(&file.path);
            }
            Err::<(), _>("primary synthetic callback refusal".into())
        },
    )
    .unwrap_err();
    assert!(
        error.contains("primary synthetic callback refusal"),
        "{error}"
    );
    assert!(error.contains("owned build close failed"), "{error}");
    for path in [
        &f.request.executable.path,
        &f.request.report_path,
        &f.runner.commands[0].stdout.path,
        &f.runner.commands[0].stderr.path,
    ] {
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }
}

#[test]
fn unchanged_direct_parent_symlink_allows_unrelated_sibling_creation() {
    let mut f = Fixture::new(0);
    let report_parent = f.directory.join("report-parent");
    let alias = f.directory.join("report-alias");
    let source_parent = f.root.clone();
    fs::create_dir(&report_parent).unwrap();
    symlink(&report_parent, &alias).unwrap();
    set_report_destination(&mut f, alias.join("owned-report.json"));
    let value = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |data, runner| {
            assert_eq!(
                data.report_file.path,
                report_parent.join("owned-report.json")
            );
            fs::write(report_parent.join("unrelated-sibling"), b"report sibling").unwrap();
            fs::create_dir(report_parent.join("unrelated-directory")).unwrap();
            fs::write(source_parent.join("unrelated-sibling"), b"tool sibling").unwrap();
            fs::write(runner.logs.join("unrelated-sibling"), b"log sibling").unwrap();
            Ok(73)
        },
    )
    .unwrap();
    assert_eq!(value, 73);
    assert_eq!(f.runner.commands.len(), 1);
    assert_eq!(fs::read_link(&alias).unwrap(), report_parent);
}

#[test]
fn failed_command_and_same_byte_tool_replacement_both_survive_close() {
    let mut f = Fixture::new(7);
    f.inputs
        .inputs
        .retain(|input| input.role != "synthetic-build-program");
    insert_before_synthetic_exit(
        &mut f,
        7,
        "cp \"$0\" \"$0.replacement\"\nmv \"$0.replacement\" \"$0\"",
    );
    let before = fs::metadata(&f.request.executable.path).unwrap();
    let called = Cell::new(false);
    let error = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |_, _| {
            called.set(true);
            Ok(())
        },
    )
    .unwrap_err();
    assert!(!called.get());
    assert_eq!(f.runner.commands.len(), 1);
    assert_eq!(f.runner.commands[0].status, 7);
    let after = fs::metadata(&f.request.executable.path).unwrap();
    assert_ne!((before.dev(), before.ino()), (after.dev(), after.ino()));
    f.request.executable.verify().unwrap();
    assert!(error.contains("failed"), "{error}");
    assert!(error.contains("owned build close failed"), "{error}");
    assert!(
        error.contains(f.request.executable.path.to_str().unwrap()),
        "{error}"
    );
}

#[test]
fn relative_runner_log_directory_preserves_owned_build_success() {
    let mut f = Fixture::new(0);
    let canonical_logs = fs::canonicalize(&f.runner.logs).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let mut relative_logs = PathBuf::new();
    for component in cwd.components() {
        if matches!(component, std::path::Component::Normal(_)) {
            relative_logs.push("..");
        }
    }
    relative_logs.push(canonical_logs.strip_prefix(Path::new("/")).unwrap());
    assert!(relative_logs.is_relative());
    assert_eq!(fs::canonicalize(&relative_logs).unwrap(), canonical_logs);
    // Runner::new creates its directory. Recreate only this fixture's empty
    // log directory, retaining a relative spelling without changing process cwd.
    fs::remove_dir(&f.runner.logs).unwrap();
    f.runner = Runner::new(relative_logs.clone(), Duration::from_secs(10), 65536).unwrap();
    assert_eq!(fs::canonicalize(&relative_logs).unwrap(), canonical_logs);
    let observation = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |data, _| Ok(data.clone()),
    )
    .unwrap();
    assert_eq!(f.runner.commands.len(), 1);
    assert_eq!(observation.command.status, 0);
    assert_eq!(
        observation.command.stdout.path,
        canonical_logs.join("0001.stdout")
    );
    assert_eq!(
        observation.command.stderr.path,
        canonical_logs.join("0001.stderr")
    );
}

#[test]
fn owned_capture_keeps_the_reported_trace_without_independent_trace_authority() {
    let mut f = Fixture::new(0);
    let path = f.root.join("report-fixture.json");
    let mut report: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    report["trace_id"] = json!("different-unbound-report-trace");
    let bytes = serde_json::to_vec(&report).unwrap();
    fs::write(&path, &bytes).unwrap();
    f.inputs
        .inputs
        .iter_mut()
        .find(|input| input.role == "report-fixture.json")
        .unwrap()
        .sha256 = sha256(&bytes);
    let trace = with_owned_build_report(
        &f.root,
        &f.inputs,
        &f.inputs,
        &f.request,
        &mut f.runner,
        |observation, _| Ok(observation.trace_id.clone()),
    )
    .unwrap();
    assert_eq!(trace, "different-unbound-report-trace");
    assert_eq!(f.runner.commands.len(), 1);
}
