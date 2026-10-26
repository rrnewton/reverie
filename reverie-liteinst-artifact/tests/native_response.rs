// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Constructed serializer controls, not captured successful native build data.
//! No command, compiler, native build or qualification factory is invoked.

use std::path::PathBuf;

use reverie_liteinst_artifact::native_response::BuildResponse;
use reverie_liteinst_artifact::native_response::ExpectedTarget;
use reverie_liteinst_artifact::native_response::ResponseExpectation;
use reverie_liteinst_artifact::native_response::embedded_build_report;
use reverie_liteinst_artifact::native_response::join_build_response;
use reverie_liteinst_artifact::native_response::parse_build_response;
use serde_json::Value;
use serde_json::json;

fn wire() -> Value {
    json!({"Result": {"result": {"build_response": {
        "build_targets": [{
            "target": "opaque-response-target[component]",
            "run_args": [],
            "outputs": [{"path": "out/runtime.so", "providers": {
                "default_info": true, "run_info": false,
                "other": false, "test_info": false
            }}],
            "configuration": "opaque-response-configuration",
            "configured_graph_size": null,
            "target_rule_type_name": null
        }],
        "project_root": "/synthetic/project",
        "serialized_build_report": null,
        "errors": []
    }}}})
}

fn body(value: &mut Value) -> &mut Value {
    &mut value["Result"]["result"]["build_response"]
}

fn parse(value: &Value) -> Result<BuildResponse, String> {
    parse_build_response(&serde_json::to_vec(value).unwrap())
}

fn expected(response: &BuildResponse) -> ResponseExpectation {
    ResponseExpectation {
        project_root: PathBuf::from("/synthetic/project"),
        targets: vec![ExpectedTarget {
            target: "opaque-response-target[component]".into(),
            configuration: "opaque-response-configuration".into(),
            outputs: response.build_targets[0].outputs.clone(),
        }],
    }
}

#[test]
fn exact_confirmed_wrapper_retains_raw_fields_without_cargo_authority() {
    let value = wire();
    let response = parse(&value).unwrap();
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        value["Result"]["result"]["build_response"]
    );
    let selected = join_build_response(&response, &expected(&response)).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].target, "opaque-response-target[component]");
    assert_eq!(selected[0].configuration, "opaque-response-configuration");
    assert_eq!(
        selected[0].absolute_path,
        PathBuf::from("/synthetic/project/out/runtime.so")
    );
    assert!(selected[0].providers.default_info);
    assert!(embedded_build_report(&response).unwrap().is_none());
    assert!(
        serde_json::from_value::<reverie_liteinst_artifact::CompiledSources>(
            serde_json::to_value(response).unwrap()
        )
        .is_err()
    );
}

#[test]
fn wrong_wrapper_case_null_result_and_command_error_are_not_build_responses() {
    let value = wire();
    let response = &value["Result"]["result"]["build_response"];
    for bad in [
        json!({"result": {"result": {"build_response": response}}}),
        json!({"Result": {"build_response": response}}),
        json!({"Result": {"result": {"BuildResponse": response}}}),
        json!({"Result": {"result": null}}),
        json!({"Result": {}}),
        json!({"Result": {"result": {"error": {"message": "synthetic failure"}}}}),
        json!({"Event": {"result": {"build_response": response}}}),
        json!({"PartialResult": {"result": {"build_response": response}}}),
    ] {
        assert!(parse(&bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn required_observation_fields_are_not_defaulted_from_absence() {
    for pointer in [
        "/Result/result/build_response",
        "/Result/result/build_response/build_targets/0",
        "/Result/result/build_response/build_targets/0/outputs/0",
        "/Result/result/build_response/build_targets/0/outputs/0/providers",
    ] {
        let original = wire();
        for key in original
            .pointer(pointer)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
        {
            let mut bad = original.clone();
            bad.pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(parse(&bad).is_err(), "defaulted missing {pointer}/{key}");
        }
    }
}

#[test]
fn error_payloads_parse_as_data_but_cannot_join_selected_success() {
    let good = parse(&wire()).unwrap();
    let mut value = wire();
    body(&mut value)["errors"] = json!([{"message": "an unselected target failed"}]);
    let failed = parse(&value).unwrap();
    assert!(
        join_build_response(&failed, &expected(&good))
            .unwrap_err()
            .contains("errors")
    );
}

#[test]
fn target_configuration_provider_flags_and_complete_paths_are_exact() {
    let good = parse(&wire()).unwrap();
    let expectation = expected(&good);
    for mutation in 0..7 {
        let mut response = good.clone();
        let target = &mut response.build_targets[0];
        match mutation {
            0 => target.target = "opaque-response-target".into(),
            1 => target.configuration = "other configuration".into(),
            2 => target.outputs[0].path = "different/runtime.so".into(),
            3 => target.outputs[0].providers.as_mut().unwrap().default_info = false,
            4 => target.outputs[0].providers.as_mut().unwrap().other = true,
            5 => target.outputs[0].providers = None,
            6 => {
                let mut extra = target.outputs[0].clone();
                extra.path = "out/runtime.rmeta".into();
                target.outputs.push(extra);
            }
            _ => unreachable!(),
        }
        assert!(
            join_build_response(&response, &expectation).is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn duplicates_unknown_fields_and_trailing_json_are_refused() {
    let good = serde_json::to_string(&wire()).unwrap();
    for bad in [
        good.replacen(
            "\"project_root\":",
            "\"project_root\":\"/other\",\"project_root\":",
            1,
        ),
        good.replacen(
            "\"default_info\":",
            "\"default_info\":false,\"default_info\":",
            1,
        ),
        good.replacen(
            "\"errors\":[]",
            "\"errors\":[{\"opaque\":0,\"opaque\":1}]",
            1,
        ),
        format!("{good} {{}}"),
    ] {
        assert_ne!(bad, good);
        assert!(
            parse_build_response(bad.as_bytes()).is_err(),
            "accepted {bad}"
        );
    }
    let mut bad = wire();
    body(&mut bad)["success"] = json!(true);
    assert!(parse(&bad).is_err());
    let mut bad = wire();
    bad["Result"]["result"]["unexpected"] = json!({});
    assert!(parse(&bad).is_err());
}

#[test]
fn embedded_report_is_separate_json_text_and_retains_failures() {
    let report = json!({
        "trace_id": "synthetic-trace", "project_root": "/synthetic/project",
        "success": false, "truncated": false, "results": {},
        "failures": {"target": "synthetic failure"}, "strings": {}
    });
    let mut value = wire();
    body(&mut value)["serialized_build_report"] = json!(serde_json::to_string(&report).unwrap());
    let response = parse(&value).unwrap();
    let parsed = embedded_build_report(&response).unwrap().unwrap();
    assert!(!parsed.success);
    assert_eq!(parsed.failures["target"], "synthetic failure");
    assert!(join_build_response(&response, &expected(&response)).is_err());
    body(&mut value)["serialized_build_report"] = report;
    assert!(parse(&value).is_err());
    body(&mut value)["serialized_build_report"] = json!("not JSON");
    let response = parse(&value).unwrap();
    assert!(embedded_build_report(&response).is_err());
    assert!(join_build_response(&response, &expected(&response)).is_err());
}

#[test]
fn path_aliases_duplicate_groups_and_incomplete_selection_are_refused() {
    let good = parse(&wire()).unwrap();
    for path in [
        "",
        "/out/runtime.so",
        "out/../runtime.so",
        "out//runtime.so",
        "./out/runtime.so",
        "out/runtime.so\0",
    ] {
        let mut response = good.clone();
        response.build_targets[0].outputs[0].path = path.into();
        let expectation = expected(&response);
        assert!(
            join_build_response(&response, &expectation).is_err(),
            "accepted {path:?}"
        );
    }
    let mut response = good.clone();
    response
        .build_targets
        .push(response.build_targets[0].clone());
    assert!(join_build_response(&response, &expected(&good)).is_err());
    let mut response = good.clone();
    let duplicate = response.build_targets[0].outputs[0].clone();
    response.build_targets[0].outputs.push(duplicate);
    assert!(join_build_response(&response, &expected(&good)).is_err());
    let mut expectation = expected(&good);
    expectation.targets.clear();
    assert!(join_build_response(&good, &expectation).is_err());
    let mut expectation = expected(&good);
    expectation.project_root = "/different".into();
    assert!(join_build_response(&good, &expectation).is_err());
}

#[test]
fn distinct_unexpected_target_or_configuration_is_not_a_complete_join() {
    let good = parse(&wire()).unwrap();
    assert_eq!(
        join_build_response(&good, &expected(&good)).unwrap().len(),
        1
    );
    for change_configuration in [false, true] {
        let mut response = good.clone();
        let mut extra = response.build_targets[0].clone();
        if change_configuration {
            extra.configuration = "independent-second-configuration".into();
        } else {
            extra.target = "independent-second-target".into();
        }
        extra.outputs[0].path = "out/second.so".into();
        response.build_targets.push(extra);
        let mut expectation = expected(&good);
        assert_eq!(
            join_build_response(&response, &expectation).unwrap_err(),
            "native response complete target/configuration set differs from expectation"
        );
        let mut outputs = good.build_targets[0].outputs.clone();
        outputs[0].path = "out/second.so".into();
        expectation.targets.push(ExpectedTarget {
            target: if change_configuration {
                "opaque-response-target[component]".into()
            } else {
                "independent-second-target".into()
            },
            configuration: if change_configuration {
                "independent-second-configuration".into()
            } else {
                "opaque-response-configuration".into()
            },
            outputs,
        });
        let selected = join_build_response(&response, &expectation).unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[1].target, expectation.targets[1].target);
        assert_eq!(
            selected[1].configuration,
            expectation.targets[1].configuration
        );
        assert_eq!(selected[1].path, "out/second.so");
    }
}
