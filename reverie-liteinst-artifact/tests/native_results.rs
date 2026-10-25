// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

// Synthetic source-schema fixtures only. No actual invocation, action, file or
// successful compilation is established, and this module grants no authority.
use native_results::BuildOutcome;
use native_results::ExpectedOutput;
use native_results::MAX_REPORT_BYTES;
use native_results::ReportExpectation;
use native_results::join_build_report;
use native_results::parse_build_report;
use reverie_liteinst_artifact::native_results;
use serde_json::Value;
use serde_json::json;

const TARGET: &str = "root//example:runtime";
const CONFIG: &str = "root//platform:linux#0123456789abcdef";
const OUTPUT: &str = "buck-out/v2/gen/root/0123456789abcdef/example/__runtime__/runtime.so";

fn fixture() -> Value {
    json!({
        "trace_id": "owned-trace-example",
        "project_root": "/project",
        "success": true,
        "truncated": false,
        "failures": {},
        "strings": {},
        "results": {
            (TARGET): {
                "errors": [],
                "configured": {
                    (CONFIG): {
                        "success": "SUCCESS", "errors": [],
                        "outputs": {"cdylib": [OUTPUT]},
                        "other_outputs": {}, "configured_graph_size": null,
                        "artifact_info": {"cdylib": {
                            "kind": "file", "digest": "opaque-cas-digest:17", "is_exec": false
                        }}
                    }
                }
            }
        }
    })
}

fn expectation() -> ReportExpectation {
    ReportExpectation {
        trace_id: "owned-trace-example".into(),
        project_root: "/project".into(),
        outputs: vec![ExpectedOutput {
            role: "runtime".into(),
            target: TARGET.into(),
            configuration: CONFIG.into(),
            provider: "cdylib".into(),
            paths: vec![OUTPUT.into()],
        }],
    }
}

fn configured(value: &mut Value) -> &mut Value {
    &mut value["results"][TARGET]["configured"][CONFIG]
}

fn report(value: &Value) -> native_results::BuildReport {
    parse_build_report(&serde_json::to_vec(value).unwrap()).unwrap()
}

#[test]
fn source_schema_fixture_preserves_exact_configured_output_description() {
    let parsed = report(&fixture());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), fixture());
    assert_eq!(parsed.trace_id, "owned-trace-example");
    assert_eq!(
        parsed.results[TARGET].configured[CONFIG].success,
        BuildOutcome::Success
    );
    let selected = join_build_report(&parsed, &expectation()).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].role, "runtime");
    assert_eq!(selected[0].target, TARGET);
    assert_eq!(selected[0].configuration, CONFIG);
    assert_eq!(selected[0].provider, "cdylib");
    assert_eq!(selected[0].paths, [OUTPUT]);
    assert_eq!(
        selected[0].absolute_paths,
        [std::path::Path::new("/project").join(OUTPUT)]
    );
    assert_eq!(
        parsed.results[TARGET].configured[CONFIG].artifact_info["cdylib"]["digest"],
        "opaque-cas-digest:17"
    );
}

#[test]
fn failed_and_streaming_outcomes_remain_data_but_cannot_join() {
    for outcome in ["FAIL", "CANCELED", "IN_PROGRESS"] {
        let mut value = fixture();
        configured(&mut value)["success"] = json!(outcome);
        assert!(
            join_build_report(&report(&value), &expectation()).is_err(),
            "{outcome}"
        );
    }
    for outcome in ["success", "Success", "CANCELLED", "INPROGRESS", "UNKNOWN"] {
        let mut value = fixture();
        configured(&mut value)["success"] = json!(outcome);
        assert!(
            parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{outcome}"
        );
    }
}

#[test]
fn report_failures_and_both_error_levels_are_refused() {
    for case in 0..6 {
        let mut value = fixture();
        match case {
            0 => value["success"] = json!(false),
            1 => value["truncated"] = json!(true),
            2 => value["failures"] = json!({TARGET: "retained failure"}),
            3 => value["results"][TARGET]["errors"] = json!([{"message_content": "failure"}]),
            4 => configured(&mut value)["errors"] = json!([{"message_content": "failure"}]),
            5 => value["error_category"] = json!("INFRA"),
            _ => unreachable!(),
        }
        assert!(
            join_build_report(&report(&value), &expectation()).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn no_unconfigured_or_metadata_provider_fallback() {
    let mut value = fixture();
    let entry = &mut value["results"][TARGET];
    entry["success"] = json!("SUCCESS");
    entry["outputs"] = json!({"cdylib": [OUTPUT]});
    entry["configured"] = json!({});
    assert!(join_build_report(&report(&value), &expectation()).is_err());

    let mut value = fixture();
    configured(&mut value)["outputs"] = json!({"DEFAULT": ["buck-out/runtime.rmeta"]});
    assert!(join_build_report(&report(&value), &expectation()).is_err());
    // A DEFAULT metadata group can be joined if explicitly requested. That
    // success is a data association, not a claim of executable/codegen output.
    let mut expected = expectation();
    expected.outputs[0].provider = "DEFAULT".into();
    expected.outputs[0].paths = vec!["buck-out/runtime.rmeta".into()];
    assert_eq!(
        join_build_report(&report(&value), &expected).unwrap()[0].provider,
        "DEFAULT"
    );
    expected.outputs[0].provider.clear();
    assert!(join_build_report(&report(&value), &expected).is_err());
}

#[test]
fn exact_trace_root_target_configuration_provider_and_full_path_are_required() {
    let parsed = report(&fixture());
    for case in 0..6 {
        let mut expected = expectation();
        match case {
            0 => expected.trace_id = "different-trace".into(),
            1 => expected.project_root = "/other-project".into(),
            2 => expected.outputs[0].target = "other//example:runtime".into(),
            3 => expected.outputs[0].configuration = "root//platform:linux#fedcba9876543210".into(),
            4 => expected.outputs[0].provider = "staticlib".into(),
            5 => expected.outputs[0].paths = vec!["other/runtime.so".into()],
            _ => unreachable!(),
        }
        assert!(
            join_build_report(&parsed, &expected).is_err(),
            "case {case}"
        );
    }
}

#[test]
fn whole_provider_group_is_required_and_artifact_info_is_not_a_path_digest() {
    let mut value = fixture();
    configured(&mut value)["outputs"] =
        json!({"cdylib|unstripped": [OUTPUT, "buck-out/debug/runtime.so"]});
    // The single provider artifact_info row cannot authenticate both outputs.
    configured(&mut value)["artifact_info"] = json!({"cdylib|unstripped": {
        "kind": "file", "digest": "single-overwritten-value", "is_exec": true
    }});
    let parsed = report(&value);
    let mut expected = expectation();
    expected.outputs[0].provider = "cdylib|unstripped".into();
    assert!(join_build_report(&parsed, &expected).is_err());
    expected.outputs[0].paths = vec!["buck-out/debug/runtime.so".into(), OUTPUT.into()];
    let joined = join_build_report(&parsed, &expected).unwrap();
    assert_eq!(joined[0].paths, [OUTPUT, "buck-out/debug/runtime.so"]);
    assert_eq!(joined[0].absolute_paths.len(), 2);
    expected.outputs[0]
        .paths
        .push("unreported/runtime.so".into());
    assert!(join_build_report(&parsed, &expected).is_err());
}

#[test]
fn missing_fields_unknown_fields_and_cargo_envelopes_are_refused() {
    for field in [
        "trace_id",
        "project_root",
        "success",
        "truncated",
        "results",
        "failures",
        "strings",
    ] {
        let mut value = fixture();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{field}"
        );
    }
    for field in ["success", "errors", "outputs", "other_outputs"] {
        let mut value = fixture();
        configured(&mut value)
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{field}"
        );
    }
    let mut value = fixture();
    configured(&mut value)["native_authority"] = json!(true);
    assert!(parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err());
    assert!(parse_build_report(br#"{"reason":"build-finished","success":true}"#).is_err());
}

#[test]
fn duplicate_target_configuration_provider_and_struct_keys_are_refused() {
    let original = serde_json::to_string(&fixture()).unwrap();
    for (needle, replacement) in [
        (
            format!("\"{TARGET}\":"),
            format!("\"{TARGET}\":{{\"configured\":{{}},\"errors\":[]}},\"{TARGET}\":"),
        ),
        (
            format!("\"{CONFIG}\":"),
            format!(
                "\"{CONFIG}\":{{\"success\":\"SUCCESS\",\"errors\":[],\"outputs\":{{}},\"other_outputs\":{{}}}},\"{CONFIG}\":"
            ),
        ),
        (
            format!("\"cdylib\":[\"{OUTPUT}\"]"),
            format!("\"cdylib\":[],\"cdylib\":[\"{OUTPUT}\"]"),
        ),
        (
            "\"trace_id\":".into(),
            "\"trace_id\":\"forged\",\"trace_id\":".into(),
        ),
    ] {
        assert_eq!(original.matches(&needle).count(), 1);
        let duplicate = original.replace(&needle, &replacement);
        assert!(
            parse_build_report(duplicate.as_bytes()).is_err(),
            "{needle}"
        );
    }
}

#[test]
fn malformed_and_duplicate_paths_are_not_normalized_into_matches() {
    for path in [
        "",
        "/runtime.so",
        "../runtime.so",
        "out/../runtime.so",
        "out/./runtime.so",
        "out//runtime.so",
        "out/runtime.so/",
        "out/\0runtime.so",
    ] {
        let mut value = fixture();
        configured(&mut value)["outputs"] = json!({"cdylib": [path]});
        assert!(
            parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{path:?}"
        );
    }
    for root in [
        "project",
        "/project/",
        "/project//nested",
        "/project/../other",
    ] {
        let mut value = fixture();
        value["project_root"] = json!(root);
        assert!(
            parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err(),
            "{root}"
        );
    }
    let mut value = fixture();
    configured(&mut value)["outputs"] = json!({"cdylib": [OUTPUT, OUTPUT]});
    assert!(parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn duplicate_and_empty_expected_role_mappings_are_refused() {
    let parsed = report(&fixture());
    let mut expected = expectation();
    expected.outputs.clear();
    assert!(join_build_report(&parsed, &expected).is_err());
    let mut expected = expectation();
    expected.outputs.push(expected.outputs[0].clone());
    assert!(join_build_report(&parsed, &expected).is_err());
    expected.outputs[1].role = "different-role-same-provider".into();
    assert!(join_build_report(&parsed, &expected).is_err());
    let mut expected = expectation();
    expected.outputs[0].paths.clear();
    assert!(join_build_report(&parsed, &expected).is_err());
    expected.outputs[0].paths = vec![OUTPUT.into(), OUTPUT.into()];
    assert!(join_build_report(&parsed, &expected).is_err());
}

#[test]
fn report_byte_limit_and_trailing_data_are_enforced() {
    assert!(
        parse_build_report(&vec![b' '; MAX_REPORT_BYTES + 1])
            .unwrap_err()
            .contains("byte bound")
    );
    let mut bytes = serde_json::to_vec(&fixture()).unwrap();
    bytes.extend_from_slice(b"{}");
    assert!(parse_build_report(&bytes).is_err());
}

#[test]
fn final_response_wrapper_is_not_guessed_as_a_report() {
    for value in [
        json!({"Result": {"result": {"BuildResponse": {
            "build_targets": [], "project_root": "/project", "errors": []
        }}}}),
        json!({"build_targets": [], "project_root": "/project", "errors": []}),
    ] {
        assert!(parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn unrelated_failure_and_empty_provider_do_not_hide_behind_selected_success() {
    let mut value = fixture();
    let mut unrelated = value["results"][TARGET].clone();
    unrelated["configured"][CONFIG]["success"] = json!("FAIL");
    value["results"]["root//example:unrelated"] = unrelated;
    assert!(join_build_report(&report(&value), &expectation()).is_err());
    let mut value = fixture();
    configured(&mut value)["outputs"] = json!({"": [OUTPUT]});
    assert!(parse_build_report(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn review_f6_duplicate_opaque_report_fields_are_refused_probe() {
    let mut accepted = Vec::new();
    for field in ["build_metrics", "artifact_info"] {
        let mut value = fixture();
        if field == "build_metrics" {
            value["build_metrics"] = json!({"opaque_probe": 0});
        } else {
            configured(&mut value)["artifact_info"]["cdylib"] = json!({"opaque_probe": 0});
        }
        let original = serde_json::to_string(&value).unwrap();
        let needle = "\"opaque_probe\":0";
        assert_eq!(original.matches(needle).count(), 1);
        let duplicate = original.replace(needle, "\"opaque_probe\":1,\"opaque_probe\":2");
        eprintln!("F6_DUPLICATE_PROBE field={field} raw_input={duplicate}");
        match parse_build_report(duplicate.as_bytes()) {
            Ok(report) => {
                eprintln!(
                    "F6_DUPLICATE_PROBE field={field} accepted=true retained={}",
                    serde_json::to_string(&report).unwrap()
                );
                accepted.push(field);
            }
            Err(error) => {
                eprintln!("F6_DUPLICATE_PROBE field={field} accepted=false error={error}")
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "duplicate opaque report fields accepted: {accepted:?}"
    );
}
