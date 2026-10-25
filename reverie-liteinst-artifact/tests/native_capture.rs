// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Normalized real wire fixtures. See fixtures/native-result/README.md for the
//! exact capture, transformations and limits. No build or file custody is tested.

use std::path::PathBuf;

use reverie_liteinst_artifact::native_events::ActionOwner;
use reverie_liteinst_artifact::native_events::parse_action_execution_end;
use reverie_liteinst_artifact::native_events::parse_materialization_end;
use reverie_liteinst_artifact::native_events::parse_materialization_start;
use reverie_liteinst_artifact::native_events::recorded_terminal_local_command;
use reverie_liteinst_artifact::native_response;
use reverie_liteinst_artifact::native_results;

const ACTION: &[u8] = include_bytes!("fixtures/native-result/action-end.json");
const START: &[u8] = include_bytes!("fixtures/native-result/materialization-start.json");
const END: &[u8] = include_bytes!("fixtures/native-result/materialization-end.json");
const RESPONSE: &[u8] = include_bytes!("fixtures/native-result/result.json");
const REPORT: &[u8] = include_bytes!("fixtures/native-result/build-report.json");
const TRACE: &str = "79c4be3c-8b00-4c84-9ee0-b34a20978d41";
const OUTPUT: &str = "buck-out/v2/art/example/product/demo/__program__/program";

#[test]
fn recorded_materialization_pair_and_terminal_action_keep_full_identity() {
    let action = parse_action_execution_end(ACTION).unwrap();
    let start = parse_materialization_start(START).unwrap();
    let end = parse_materialization_end(END).unwrap();
    assert_eq!(action.event.trace_id, TRACE);
    assert_eq!(start.event.trace_id, TRACE);
    assert_eq!(end.event.trace_id, TRACE);
    assert_eq!(start.event.span_id, 705649);
    assert_eq!(end.event.span_id, start.event.span_id);
    assert_eq!(end.event.parent_id, start.event.parent_id);
    assert_eq!(end.artifact, start.artifact);
    assert_eq!(action.key, end.artifact.key);
    assert_eq!(end.artifact.path, "program");
    let ActionOwner::TargetLabel(owner) = &action.key.owner else {
        panic!("recorded owner changed");
    };
    assert_eq!(owner.label.package, "example//demo");
    assert_eq!(owner.label.name, "program");
    assert_eq!(owner.configuration.full_name, "cfg:example#product");
    assert!(owner.execution_configuration.is_none());
    assert_ne!(action.event.span_id, start.event.span_id);
}

#[test]
fn recorded_success_without_command_recipe_is_still_refused() {
    let action = parse_action_execution_end(ACTION).unwrap();
    assert!(!action.failed);
    assert!(action.error.is_null());
    assert_eq!(action.execution_kind, 10); // Numeric data, no enum interpretation.
    assert_eq!(action.extra["wall_time_us"], 0);
    assert!(action.commands.is_empty());
    assert!(
        recorded_terminal_local_command(&action)
            .unwrap_err()
            .contains("no recorded command attempts")
    );
    // Neither an observed event nor a final response is a historical Cargo
    // source envelope. Deserialization cannot acquire source authority.
    assert!(serde_json::from_slice::<reverie_liteinst_artifact::CompiledSources>(ACTION).is_err());
    assert!(
        serde_json::from_slice::<reverie_liteinst_artifact::CompiledSources>(RESPONSE).is_err()
    );
}

#[test]
fn captured_report_and_response_keep_request_config_separate_from_producer() {
    let report = native_results::parse_build_report(REPORT).unwrap();
    let response = native_response::parse_build_response(RESPONSE).unwrap();
    let request_configuration = "cfg:example#request";
    let target = "example//demo:program-export";
    let report_outputs = native_results::join_build_report(
        &report,
        &native_results::ReportExpectation {
            trace_id: TRACE.into(),
            project_root: PathBuf::from("/work/project"),
            outputs: vec![native_results::ExpectedOutput {
                role: "program".into(),
                target: target.into(),
                configuration: request_configuration.into(),
                provider: "DEFAULT".into(),
                paths: vec![OUTPUT.into()],
            }],
        },
    )
    .unwrap();
    let response_outputs = native_response::join_build_response(
        &response,
        &native_response::ResponseExpectation {
            project_root: PathBuf::from("/work/project"),
            targets: vec![native_response::ExpectedTarget {
                target: target.into(),
                configuration: request_configuration.into(),
                outputs: vec![native_response::BuildOutput {
                    path: OUTPUT.into(),
                    providers: Some(native_response::OutputProviders {
                        default_info: true,
                        run_info: false,
                        other: false,
                        test_info: false,
                    }),
                }],
            }],
        },
    )
    .unwrap();
    assert_eq!(report_outputs[0].paths, vec![OUTPUT]);
    assert_eq!(response_outputs[0].path, OUTPUT);
    assert_eq!(
        report_outputs[0].absolute_paths,
        vec![response_outputs[0].absolute_path.clone()]
    );
    let action = parse_action_execution_end(ACTION).unwrap();
    let ActionOwner::TargetLabel(owner) = action.key.owner else {
        panic!("recorded owner changed");
    };
    assert_ne!(owner.configuration.full_name, request_configuration);
    assert!(response.serialized_build_report.is_none());
}
