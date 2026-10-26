// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Synthetic JSON shapes derived from source and the separately confirmed
//! materialization-end tag. These tests claim no successful compiler or
//! qualification evidence.
//! Nullable command status is intentional: parsing it must not imply success.

use reverie_liteinst_artifact::native_events::ActionExecutionEnd;
use reverie_liteinst_artifact::native_events::ActionOwner;
use reverie_liteinst_artifact::native_events::Command;
use reverie_liteinst_artifact::native_events::MAX_EVENT_BYTES;
use reverie_liteinst_artifact::native_events::parse_action_execution_end;
use reverie_liteinst_artifact::native_events::parse_materialization_end;
use reverie_liteinst_artifact::native_events::parse_materialization_start;
use reverie_liteinst_artifact::native_events::recorded_terminal_local_command;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

fn key() -> Value {
    json!({
        "id": "0102000000000000",
        "key": "opaque-deferred-owner-key_27",
        "owner": {"TargetLabel": {
            "label": {"package": "cell//full/package", "name": "runtime"},
            "configuration": {"full_name": "platform//:target (full-config-hash)"},
            "execution_configuration": {"full_name": "platform//:exec (exec-config-hash)"}
        }}
    })
}

fn local_command() -> Value {
    json!({
        "argv": ["wrapper", "argument, with comma", "@nested response.args", "", "--last"],
        "env": [{"key": "SETTING", "value": "first"}, {"key": "SETTING", "value": "last"}],
        "action_digest": "opaque-action-digest",
        "working_directory": "cell/working directory",
        "environment_rule": 1
    })
}

fn action() -> Value {
    json!({"Event": {
        "timestamp": [123456, 789],
        "trace_id": "synthetic-source-shape-trace",
        "span_id": 81,
        "parent_id": 17,
        "data": {"SpanEnd": {
            "stats": null,
            "duration_us": 42,
            "data": {"ActionExecution": {
                "key": key(), "kind": "Run",
                "name": {"category": "rustc", "identifier": "full-codegen-identifier"},
                "failed": false, "error": null,
                "execution_kind": 1,
                "commands": [{
                    "details": {
                        "signed_exit_code": null, "stdout": "", "stderr": "diagnostic",
                        "command_kind": {"command": {"LocalCommand": local_command()}},
                        "metadata": null, "additional_message": null
                    },
                    "status": null,
                    "inline_environment_metadata": null
                }],
                "outputs": [{"tiny_digest": "short-digest-only"}],
                "graph_producers": null,
                "wall_time_us": 42
            }}
        }}
    }})
}

fn action_fields(value: &mut Value) -> &mut Map<String, Value> {
    value
        .pointer_mut("/Event/data/SpanEnd/data/ActionExecution")
        .unwrap()
        .as_object_mut()
        .unwrap()
}

fn parse(value: &Value) -> Result<ActionExecutionEnd, String> {
    parse_action_execution_end(&serde_json::to_vec(value).unwrap())
}

#[test]
fn full_identity_and_ordered_local_recipe_survive_as_untrusted_data() {
    let observed = parse(&action()).unwrap();
    assert_eq!(observed.event.trace_id, "synthetic-source-shape-trace");
    assert_eq!((observed.event.span_id, observed.event.parent_id), (81, 17));
    assert_eq!(observed.event.timestamp, Some((123456, 789)));
    assert_eq!(observed.key.id, "0102000000000000");
    assert_eq!(observed.key.key, "opaque-deferred-owner-key_27");
    let ActionOwner::TargetLabel(owner) = &observed.key.owner else {
        panic!("full configured owner was lost")
    };
    assert_eq!(owner.label.package, "cell//full/package");
    assert_eq!(owner.label.name, "runtime");
    assert_eq!(
        owner.configuration.full_name,
        "platform//:target (full-config-hash)"
    );
    assert_eq!(
        owner.execution_configuration.as_ref().unwrap().full_name,
        "platform//:exec (exec-config-hash)"
    );
    let attempt = &observed.commands[0];
    assert!(attempt.status.is_null());
    let details = attempt.details.as_ref().unwrap();
    assert_eq!(details.signed_exit_code, None);
    let Some(Command::LocalCommand(local)) = &details.command_kind.as_ref().unwrap().command else {
        panic!("confirmed LocalCommand was not retained")
    };
    assert_eq!(
        local.argv,
        [
            "wrapper",
            "argument, with comma",
            "@nested response.args",
            "",
            "--last"
        ]
    );
    assert_eq!(local.env.len(), 2);
    assert_eq!(local.env[0].key, local.env[1].key);
    assert_eq!(local.env[0].value, "first");
    assert_eq!(local.env[1].value, "last");
    assert_eq!(
        local.working_directory.as_deref(),
        Some("cell/working directory")
    );
    assert_eq!(local.environment_rule, 1);
    assert_eq!(local.action_digest, "opaque-action-digest");
    // A recipe can be read even though this fixture has null status/exit. The
    // accessor grants no terminal-success or complete-environment assertion.
    assert_eq!(recorded_terminal_local_command(&observed).unwrap(), local);
    assert_eq!(
        observed.extra["outputs"],
        json!([{"tiny_digest": "short-digest-only"}])
    );
    assert_eq!(observed.span_fields["duration_us"], 42);
    let serialized = serde_json::to_value(&observed).unwrap();
    assert_eq!(
        serde_json::from_value::<ActionExecutionEnd>(serialized.clone()).unwrap(),
        observed
    );
    assert!(
        serde_json::from_value::<reverie_liteinst_artifact::CompiledSources>(serialized).is_err()
    );
}

#[test]
fn wrong_envelope_span_action_or_kind_spelling_is_refused() {
    for replacement in ["event", "Result", "PartialResult"] {
        let mut value = action();
        let payload = value.as_object_mut().unwrap().remove("Event").unwrap();
        value[replacement] = payload;
        assert!(parse(&value).is_err(), "{replacement}");
    }
    for replacement in ["span_end", "SpanStart", "ActionExecutionEnd"] {
        let mut value = action();
        let data = value["Event"]["data"].as_object_mut().unwrap();
        let payload = data.remove("SpanEnd").unwrap();
        data.insert(replacement.into(), payload);
        assert!(parse(&value).is_err(), "{replacement}");
    }
    for replacement in [
        "action_execution",
        "ActionExecutionEnd",
        "FinalMaterialization",
    ] {
        let mut value = action();
        let data = value["Event"]["data"]["SpanEnd"]["data"]
            .as_object_mut()
            .unwrap();
        let payload = data.remove("ActionExecution").unwrap();
        data.insert(replacement.into(), payload);
        assert!(parse(&value).is_err(), "{replacement}");
    }
    for kind in [json!("RUN"), json!("run"), json!(3), Value::Null] {
        let mut value = action();
        action_fields(&mut value).insert("kind".into(), kind);
        assert!(parse(&value).is_err());
    }
}

#[test]
fn key_encoding_and_full_owner_fields_cannot_be_inferred() {
    for id in [
        json!([1, 2, 0, 0]),
        json!("AQ=="),
        json!("0"),
        json!("0g"),
        Value::Null,
    ] {
        let mut value = action();
        action_fields(&mut value)["key"]["id"] = id;
        assert!(parse(&value).is_err());
    }
    for field in ["id", "key", "owner"] {
        let mut value = action();
        action_fields(&mut value)["key"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(parse(&value).is_err(), "{field}");
    }
    let mut value = action();
    action_fields(&mut value)["key"]["owner"] = json!({"target_label": {}});
    assert!(parse(&value).is_err());
    for field in ["label", "configuration", "execution_configuration"] {
        let mut value = action();
        action_fields(&mut value)["key"]["owner"]["TargetLabel"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(parse(&value).is_err(), "{field}");
    }
    // hex::decode accepts both cases; preserve the exact input spelling.
    let mut value = action();
    action_fields(&mut value)["key"]["id"] = json!("aBcD");
    assert_eq!(parse(&value).unwrap().key.id, "aBcD");
}

#[test]
fn confirmed_non_target_owners_stay_distinct_raw_payloads() {
    for (tag, payload) in [
        (
            "BxlKey",
            json!({"label": {"bxl_path": "cell//entry.bxl", "name": "build"}}),
        ),
        (
            "AnonTarget",
            json!({"name": null, "execution_configuration": null, "hash": "opaque"}),
        ),
    ] {
        let mut value = action();
        action_fields(&mut value)["key"]["owner"] = json!({(tag): payload.clone()});
        let owner = parse(&value).unwrap().key.owner;
        match owner {
            ActionOwner::BxlKey(actual) | ActionOwner::AnonTarget(actual) => {
                assert_eq!(actual, payload)
            }
            ActionOwner::TargetLabel(_) => panic!("non-target owner was coerced"),
        }
    }
}

#[test]
fn failed_and_error_are_explicit_and_cannot_disagree() {
    let mut absent = action();
    action_fields(&mut absent).remove("failed");
    assert!(parse(&absent).is_err());
    for failed in [Value::Null, json!("false"), json!(0)] {
        let mut value = action();
        action_fields(&mut value).insert("failed".into(), failed);
        assert!(parse(&value).is_err());
    }
    let mut value = action();
    action_fields(&mut value).remove("error");
    assert!(parse(&value).is_err());
    let mut value = action();
    action_fields(&mut value)["failed"] = json!(true);
    assert!(parse(&value).is_err());
    // No meaning is assigned to this deliberately unmodeled error tag.
    let error = json!({"UninterpretedSyntheticError": {"message": "retained"}});
    action_fields(&mut value)["error"] = error.clone();
    let observed = parse(&value).unwrap();
    assert!(observed.failed);
    assert_eq!(observed.error, error);
    action_fields(&mut value)["failed"] = json!(false);
    assert!(parse(&value).is_err());
}

#[test]
fn attempt_order_raw_status_and_signed_exit_are_preserved() {
    let mut value = action();
    let mut first = action_fields(&mut value)["commands"][0].clone();
    // This exact Failure spelling and omitted-false payload are documented by
    // the custom serde attribute in buck2_data/build.rs. No success is inferred.
    first["status"] = json!({"Failure": {}});
    first["details"]["signed_exit_code"] = json!(-9);
    let mut second = first.clone();
    second["status"] = json!({"Failure": {"missing_materialized_inputs": true}});
    second["details"]["signed_exit_code"] = json!(7);
    let third = json!({"details": null, "status": null, "inline_environment_metadata": null});
    action_fields(&mut value)["commands"] = json!([first, second, third]);
    let observed = parse(&value).unwrap();
    assert_eq!(observed.commands.len(), 3);
    assert_eq!(observed.commands[0].status, json!({"Failure": {}}));
    assert_eq!(
        observed.commands[0]
            .details
            .as_ref()
            .unwrap()
            .signed_exit_code,
        Some(-9)
    );
    assert_eq!(
        observed.commands[1].status,
        json!({"Failure": {"missing_materialized_inputs": true}})
    );
    assert_eq!(
        observed.commands[1]
            .details
            .as_ref()
            .unwrap()
            .signed_exit_code,
        Some(7)
    );
    assert!(observed.commands[2].details.is_none());
    assert!(observed.commands[2].status.is_null());
}

#[test]
fn absent_attempt_fields_are_refused_and_explicit_absence_remains_visible() {
    for field in ["commands", "execution_kind"] {
        let mut value = action();
        action_fields(&mut value).remove(field);
        assert!(parse(&value).is_err(), "{field}");
    }
    for field in ["details", "status"] {
        let mut value = action();
        action_fields(&mut value)["commands"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(parse(&value).is_err(), "{field}");
    }
    let mut value = action();
    action_fields(&mut value)["commands"] = json!([]);
    assert!(parse(&value).unwrap().commands.is_empty());
    action_fields(&mut value)["commands"] = Value::Null;
    assert!(parse(&value).is_err());
    for kind in [Value::Null, json!({"command": null})] {
        let mut value = action();
        action_fields(&mut value)["commands"][0]["details"]["command_kind"] = kind.clone();
        let observed = parse(&value).unwrap();
        let observed_kind = &observed.commands[0].details.as_ref().unwrap().command_kind;
        if kind.is_null() {
            assert!(observed_kind.is_none());
        } else {
            assert!(observed_kind.as_ref().unwrap().command.is_none());
        }
    }
}

#[test]
fn local_recipe_requires_each_field_without_environment_or_cwd_defaults() {
    for field in [
        "argv",
        "env",
        "action_digest",
        "working_directory",
        "environment_rule",
    ] {
        let mut value = action();
        action_fields(&mut value)["commands"][0]["details"]["command_kind"]["command"]["LocalCommand"]
            .as_object_mut().unwrap().remove(field);
        assert!(parse(&value).is_err(), "{field}");
    }
    let mut value = action();
    action_fields(&mut value)["commands"][0]["details"]["command_kind"]["command"]["LocalCommand"]
        ["argv"] = json!("[wrapper, argument, with comma]");
    assert!(parse(&value).is_err());
    let mut value = action();
    action_fields(&mut value)["commands"][0]["details"]["command_kind"]["command"]["LocalCommand"]
        ["working_directory"] = Value::Null;
    let observed = parse(&value).unwrap();
    let Some(Command::LocalCommand(local)) = &observed.commands[0]
        .details
        .as_ref()
        .unwrap()
        .command_kind
        .as_ref()
        .unwrap()
        .command
    else {
        panic!("local command lost")
    };
    assert!(local.working_directory.is_none());
}

#[test]
fn remote_omitted_worker_and_unknown_payloads_never_become_local_recipes() {
    for tag in [
        "RemoteCommand",
        "OmittedLocalCommand",
        "WorkerCommand",
        "local_command",
        "UnmodeledCommand",
    ] {
        let mut value = action();
        let payload =
            json!({"action_digest": "retained-only", "cache_hit_type": 3, "details": null});
        action_fields(&mut value)["commands"][0]["details"]["command_kind"]["command"] =
            json!({(tag): payload.clone()});
        let observed = parse(&value).unwrap();
        let command = &observed.commands[0]
            .details
            .as_ref()
            .unwrap()
            .command_kind
            .as_ref()
            .unwrap()
            .command;
        assert_eq!(
            command,
            &Some(Command::Uninterpreted {
                tag: tag.into(),
                payload
            })
        );
    }
    for execution_kind in [0, 2, 3, 7, 9, 10, 12, 13, i32::MAX] {
        let mut value = action();
        action_fields(&mut value)["execution_kind"] = json!(execution_kind);
        assert_eq!(parse(&value).unwrap().execution_kind, execution_kind);
    }
    let mut value = action();
    action_fields(&mut value)["execution_kind"] = json!("Local");
    assert!(parse(&value).is_err());
}

#[test]
fn duplicate_keys_in_typed_and_raw_fields_are_refused() {
    let bytes = serde_json::to_string(&action()).unwrap();
    for (from, to) in [
        ("\"failed\":false", "\"failed\":false,\"failed\":false"),
        (
            "\"id\":\"0102000000000000\"",
            "\"id\":\"0102000000000000\",\"id\":\"0102000000000000\"",
        ),
        (
            "\"metadata\":null",
            "\"metadata\":{\"nested\":{\"x\":1,\"x\":1}}",
        ),
    ] {
        assert!(bytes.contains(from));
        let duplicate = bytes.replace(from, to);
        assert!(parse_action_execution_end(duplicate.as_bytes()).is_err());
    }
    let mut multiple = action();
    action_fields(&mut multiple)["commands"][0]["status"] =
        json!({"Failure": {}, "UnmodeledStatus": {}});
    assert!(parse(&multiple).is_err());
}

#[test]
fn event_identity_and_one_record_byte_bound_are_enforced() {
    for field in ["timestamp", "trace_id", "span_id", "parent_id"] {
        let mut value = action();
        value["Event"].as_object_mut().unwrap().remove(field);
        assert!(parse(&value).is_err(), "{field}");
    }
    let mut value = action();
    value["Event"]["span_id"] = json!(0);
    assert!(parse(&value).is_err());
    let mut value = action();
    value["Event"]["trace_id"] = json!("");
    assert!(parse(&value).is_err());
    let mut concatenated = serde_json::to_vec(&action()).unwrap();
    concatenated.extend_from_slice(b"\n{}");
    assert!(parse_action_execution_end(&concatenated).is_err());
    assert!(parse_action_execution_end(&vec![b' '; MAX_EVENT_BYTES + 1]).is_err());
}

#[test]
fn terminal_recipe_accessor_refuses_absence_without_borrowing_an_earlier_recipe() {
    let mut value = action();
    action_fields(&mut value)["commands"] = json!([]);
    assert!(recorded_terminal_local_command(&parse(&value).unwrap()).is_err());
    for terminal in [
        json!({"details": null, "status": null}),
        json!({"details": {
            "signed_exit_code": null, "stdout": "", "stderr": "", "command_kind": null,
            "metadata": null, "additional_message": null
        }, "status": null}),
        json!({"details": {
            "signed_exit_code": null, "stdout": "", "stderr": "", "command_kind": {"command": null},
            "metadata": null, "additional_message": null
        }, "status": null}),
    ] {
        let mut value = action();
        action_fields(&mut value)["commands"]
            .as_array_mut()
            .unwrap()
            .push(terminal);
        let observed = parse(&value).unwrap();
        assert_eq!(observed.commands.len(), 2);
        assert!(recorded_terminal_local_command(&observed).is_err());
    }
    for tag in [
        "RemoteCommand",
        "OmittedLocalCommand",
        "WorkerCommand",
        "UnmodeledCommand",
    ] {
        let mut value = action();
        let mut terminal = action_fields(&mut value)["commands"][0].clone();
        terminal["details"]["command_kind"]["command"] =
            json!({(tag): {"action_digest": "only-digest"}});
        action_fields(&mut value)["commands"]
            .as_array_mut()
            .unwrap()
            .push(terminal);
        assert!(
            recorded_terminal_local_command(&parse(&value).unwrap()).is_err(),
            "{tag}"
        );
    }
    for argv in [json!([]), json!([""])] {
        let mut value = action();
        action_fields(&mut value)["commands"][0]["details"]["command_kind"]["command"]["LocalCommand"]
            ["argv"] = argv;
        assert!(recorded_terminal_local_command(&parse(&value).unwrap()).is_err());
    }
}

#[test]
fn confirmed_materialization_records_keep_logical_path_without_resolving_it() {
    let value = json!({"Event": {
        "timestamp": [123456, 900], "trace_id": "synthetic-source-shape-trace",
        "span_id": 82, "parent_id": 17,
        "data": {"SpanStart": {"data": {"FinalMaterialization": {
            "artifact": {"key": key(), "path": "owner-relative/deep/runtime.so"}
        }}}}
    }});
    let observed = parse_materialization_start(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(observed.artifact.key, parse(&action()).unwrap().key);
    assert_eq!(observed.artifact.path, "owner-relative/deep/runtime.so");
    assert_eq!(observed.event.span_id, 82);
    assert!(parse(&value).is_err());
    let mut end_value = value;
    let data = end_value["Event"]["data"].as_object_mut().unwrap();
    let mut end = data.remove("SpanStart").unwrap();
    end["stats"] = json!({"max_poll_time_us": 21, "total_poll_time_us": 28});
    end["duration_us"] = json!(60);
    data.insert("SpanEnd".into(), end);
    let bytes = serde_json::to_vec(&end_value).unwrap();
    let materialized = parse_materialization_end(&bytes).unwrap();
    assert_eq!(materialized.event, observed.event);
    assert_eq!(materialized.artifact, observed.artifact);
    assert_eq!(materialized.span_fields["duration_us"], 60);
    assert!(parse_action_execution_end(&bytes).is_err());
    assert!(parse_materialization_start(&bytes).is_err());
    for wrong in [
        "final_materialization",
        "MaterializeRequestedArtifactEnd",
        "Materialization",
    ] {
        let mut wrong_value = end_value.clone();
        let data = wrong_value["Event"]["data"]["SpanEnd"]["data"]
            .as_object_mut()
            .unwrap();
        let payload = data.remove("FinalMaterialization").unwrap();
        data.insert(wrong.into(), payload);
        assert!(parse_materialization_end(&serde_json::to_vec(&wrong_value).unwrap()).is_err());
    }
}

#[test]
fn empty_action_id_and_key_are_refused_without_losing_hex_validation() {
    for (field, message) in [
        ("id", "native event action key id must not be empty"),
        ("key", "native event action key key must not be empty"),
    ] {
        let mut value = action();
        action_fields(&mut value)["key"][field] = json!("");
        assert!(parse(&value).unwrap_err().contains(message), "{field}");
    }
    for id in ["0", "xz", "00 0"] {
        let mut value = action();
        action_fields(&mut value)["key"]["id"] = json!(id);
        assert!(
            parse(&value)
                .unwrap_err()
                .contains("id must be a hex string"),
            "{id}"
        );
    }
    let mut value = action();
    action_fields(&mut value)["key"]["id"] = json!("00");
    action_fields(&mut value)["key"]["key"] = json!("x");
    let observed = parse(&value).unwrap();
    assert_eq!(observed.key.id, "00");
    assert_eq!(observed.key.key, "x");
}
