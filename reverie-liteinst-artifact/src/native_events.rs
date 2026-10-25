// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Untrusted observations from the ordinary local JSON event stream.
//!
//! Wire spellings are bound to source at
//! `13f223c4320d5a78da497e7dde6c25ab2a728eec`: StreamValue in
//! buck2_event_log_format, buck2_data's build.rs/custom serializers, and the
//! what_ran, action_key_owner and span_tracker consumers. The requested-
//! materialization end tag is separately confirmed by a retained ordinary
//! event stream from Buck `f41e47335a0719274872e02761f283b64c335477`.
//! Caller-constructible data cannot grant source, compilation or qualification
//! authority; the recorded action in that capture has no command attempts.
//!
//! Only the confirmed Run action-end and requested-materialization records are
//! interpreted. Commands retain their attempt order; nullable details and raw
//! status/error payloads are never replaced with successful defaults. No output-path resolution,
//! inherited environment reconstruction or aquery argument splitting occurs.

use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Map;
use serde_json::Value;

use super::Result;

/// A bound on one event, not on a complete event log.
pub const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventIdentity {
    pub trace_id: String,
    pub span_id: u64,
    pub parent_id: u64,
    /// Custom serde represents a present timestamp as [seconds, nanos].
    pub timestamp: Option<(i64, i32)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetLabel {
    pub package: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Configuration {
    pub full_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfiguredTargetLabel {
    pub label: TargetLabel,
    pub configuration: Configuration,
    pub execution_configuration: Option<Configuration>,
}

/// Non-target owners remain distinct and are not coerced into target labels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionOwner {
    TargetLabel(ConfiguredTargetLabel),
    BxlKey(Value),
    AnonTarget(Value),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionKey {
    /// Original hex spelling, validated like serialize_bytes' hex decoder.
    /// This is opaque identity, never a portable action counter.
    pub id: String,
    pub key: String,
    pub owner: ActionOwner,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionName {
    pub category: String,
    pub identifier: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentEntry {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalCommand {
    pub argv: Vec<String>,
    /// Ordered overlay entries, not a complete inherited environment.
    pub env: Vec<EnvironmentEntry>,
    pub action_digest: String,
    /// Source says project-relative; retained without resolving or normalizing.
    pub working_directory: Option<String>,
    /// Protobuf enum field, serialized as i32 without the ActionKind override.
    pub environment_rule: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    LocalCommand(LocalCommand),
    /// Includes remote, worker, omitted and unrecognized commands. This is not
    /// a recipe and cannot supply missing local argv, environment or cwd.
    Uninterpreted {
        tag: String,
        payload: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandExecutionKind {
    pub command: Option<Command>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandExecutionDetails {
    pub signed_exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub command_kind: Option<CommandExecutionKind>,
    pub metadata: Value,
    pub additional_message: Option<String>,
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandExecution {
    /// Explicit null remains absent; a missing JSON field is refused.
    pub details: Option<CommandExecutionDetails>,
    /// Explicit null or the complete oneof object, never interpreted as success.
    /// No status spelling is guessed from a protobuf message name.
    pub status: Value,
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionExecutionEnd {
    pub event: EventIdentity,
    pub key: ActionKey,
    pub name: ActionName,
    /// This parser accepts the confirmed wire spelling "Run" only.
    pub kind: String,
    pub failed: bool,
    /// Explicit null or the original error oneof; never inferred from omission.
    pub error: Value,
    /// Retain numeric cache/remote/reuse distinctions without classifying them.
    pub execution_kind: i32,
    pub commands: Vec<CommandExecution>,
    /// Includes output tiny digests, graph producers and diagnostic fields.
    /// These raw fields are not a source/output manifest or execution proof.
    pub extra: Map<String, Value>,
    pub span_fields: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildArtifact {
    pub key: ActionKey,
    /// Logical path relative to the owner's output base, not a resolved file.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationStart {
    pub event: EventIdentity,
    pub artifact: BuildArtifact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationEnd {
    pub event: EventIdentity,
    pub artifact: BuildArtifact,
    pub span_fields: Map<String, Value>,
}

fn object(value: Value, context: &str) -> Result<Map<String, Value>> {
    match value {
        Value::Object(fields) => Ok(fields),
        _ => Err(format!("native event {context} must be an object")),
    }
}

fn take(fields: &mut Map<String, Value>, name: &str) -> Result<Value> {
    fields
        .remove(name)
        .ok_or_else(|| format!("native event missing field {name}"))
}

fn typed<T: DeserializeOwned>(fields: &mut Map<String, Value>, name: &str) -> Result<T> {
    serde_json::from_value(take(fields, name)?)
        .map_err(|error| format!("native event {name}: {error}"))
}

fn complete(fields: Map<String, Value>, context: &str) -> Result<()> {
    if fields.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "native event unexpected {context} fields: {:?}",
            fields.keys()
        ))
    }
}

fn oneof(value: Value, context: &str) -> Result<(String, Value)> {
    let fields = object(value, context)?;
    if fields.len() != 1 {
        return Err(format!(
            "native event {context} must contain exactly one tag"
        ));
    }
    Ok(fields.into_iter().next().unwrap())
}

fn tagged(value: Value, expected: &str) -> Result<Value> {
    let (tag, payload) = oneof(value, expected)?;
    if tag != expected {
        return Err(format!("native event expected tag {expected}, found {tag}"));
    }
    Ok(payload)
}

fn raw_optional_oneof(value: Value, context: &str) -> Result<Value> {
    if !value.is_null() {
        oneof(value.clone(), context)?;
    }
    Ok(value)
}

fn configuration(value: Value) -> Result<Configuration> {
    let mut fields = object(value, "configuration")?;
    let full_name = typed(&mut fields, "full_name")?;
    complete(fields, "configuration")?;
    Ok(Configuration { full_name })
}

fn target(value: Value) -> Result<ConfiguredTargetLabel> {
    let mut fields = object(value, "configured target")?;
    let mut label = object(take(&mut fields, "label")?, "target label")?;
    let label_value = TargetLabel {
        package: typed(&mut label, "package")?,
        name: typed(&mut label, "name")?,
    };
    complete(label, "target label")?;
    let configuration = configuration(take(&mut fields, "configuration")?)?;
    let execution = take(&mut fields, "execution_configuration")?;
    let execution_configuration = if execution.is_null() {
        None
    } else {
        Some(self::configuration(execution)?)
    };
    complete(fields, "configured target")?;
    Ok(ConfiguredTargetLabel {
        label: label_value,
        configuration,
        execution_configuration,
    })
}

fn action_key(value: Value) -> Result<ActionKey> {
    let mut fields = object(value, "action key")?;
    let id: String = typed(&mut fields, "id")?;
    if id.is_empty() {
        return Err("native event action key id must not be empty".into());
    }
    if !id.len().is_multiple_of(2) || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("native event action key id must be a hex string".into());
    }
    let key: String = typed(&mut fields, "key")?;
    if key.is_empty() {
        return Err("native event action key key must not be empty".into());
    }
    let (tag, payload) = oneof(take(&mut fields, "owner")?, "action owner")?;
    let owner = match tag.as_str() {
        "TargetLabel" => ActionOwner::TargetLabel(target(payload)?),
        // These spellings are explicitly referenced in action_key_owner.rs.
        "BxlKey" => ActionOwner::BxlKey(Value::Object(object(payload, "BxlKey")?)),
        "AnonTarget" => ActionOwner::AnonTarget(Value::Object(object(payload, "AnonTarget")?)),
        _ => return Err(format!("native event unsupported action owner tag {tag}")),
    };
    complete(fields, "action key")?;
    Ok(ActionKey { id, key, owner })
}

fn build_artifact(value: Value) -> Result<BuildArtifact> {
    let mut fields = object(value, "build artifact")?;
    let artifact = BuildArtifact {
        key: action_key(take(&mut fields, "key")?)?,
        path: typed(&mut fields, "path")?,
    };
    complete(fields, "build artifact")?;
    Ok(artifact)
}

fn local_command(value: Value) -> Result<LocalCommand> {
    let mut fields = object(value, "LocalCommand")?;
    let argv = typed(&mut fields, "argv")?;
    let env: Vec<Value> = typed(&mut fields, "env")?;
    let env = env
        .into_iter()
        .map(|entry| {
            let mut entry = object(entry, "environment entry")?;
            let value = EnvironmentEntry {
                key: typed(&mut entry, "key")?,
                value: typed(&mut entry, "value")?,
            };
            complete(entry, "environment entry")?;
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    let action_digest = typed(&mut fields, "action_digest")?;
    let working_directory = typed(&mut fields, "working_directory")?;
    let environment_rule = typed(&mut fields, "environment_rule")?;
    complete(fields, "LocalCommand")?;
    Ok(LocalCommand {
        argv,
        env,
        action_digest,
        working_directory,
        environment_rule,
    })
}

fn command_kind(value: Value) -> Result<Option<CommandExecutionKind>> {
    if value.is_null() {
        return Ok(None);
    }
    let mut fields = object(value, "command kind")?;
    let command = take(&mut fields, "command")?;
    let command = if command.is_null() {
        None
    } else {
        let (tag, payload) = oneof(command, "command")?;
        Some(if tag == "LocalCommand" {
            // Explicit generated enum use: buck2_data/src/lib.rs, get_action_digest.
            Command::LocalCommand(local_command(payload)?)
        } else {
            Command::Uninterpreted { tag, payload }
        })
    };
    complete(fields, "command kind")?;
    Ok(Some(CommandExecutionKind { command }))
}

fn details(value: Value) -> Result<Option<CommandExecutionDetails>> {
    if value.is_null() {
        return Ok(None);
    }
    let mut fields = object(value, "command details")?;
    Ok(Some(CommandExecutionDetails {
        signed_exit_code: typed(&mut fields, "signed_exit_code")?,
        stdout: typed(&mut fields, "stdout")?,
        stderr: typed(&mut fields, "stderr")?,
        command_kind: command_kind(take(&mut fields, "command_kind")?)?,
        metadata: take(&mut fields, "metadata")?,
        additional_message: typed(&mut fields, "additional_message")?,
        extra: fields,
    }))
}

fn command(value: Value) -> Result<CommandExecution> {
    let mut fields = object(value, "command attempt")?;
    Ok(CommandExecution {
        details: details(take(&mut fields, "details")?)?,
        status: raw_optional_oneof(take(&mut fields, "status")?, "command status")?,
        extra: fields,
    })
}

fn event(bytes: &[u8], span: &str) -> Result<(EventIdentity, Map<String, Value>)> {
    let value = super::native_wire::parse(bytes, MAX_EVENT_BYTES)?;
    let mut fields = object(tagged(value, "Event")?, "event")?;
    let identity = EventIdentity {
        timestamp: typed(&mut fields, "timestamp")?,
        trace_id: typed(&mut fields, "trace_id")?,
        span_id: typed(&mut fields, "span_id")?,
        parent_id: typed(&mut fields, "parent_id")?,
    };
    if identity.trace_id.is_empty() || identity.span_id == 0 {
        return Err("native event requires trace and nonzero span identity".into());
    }
    let payload = object(tagged(take(&mut fields, "data")?, span)?, span)?;
    complete(fields, "event")?;
    Ok((identity, payload))
}

/// Parse one confirmed Run action-end event. Parsing is not a success check:
/// failed actions, empty attempt lists, absent details/status and cached or
/// unhandled command kinds remain distinguishable in the returned data.
pub fn parse_action_execution_end(bytes: &[u8]) -> Result<ActionExecutionEnd> {
    let (event, mut span_fields) = event(bytes, "SpanEnd")?;
    let mut fields = object(
        tagged(take(&mut span_fields, "data")?, "ActionExecution")?,
        "ActionExecution",
    )?;
    let key = action_key(take(&mut fields, "key")?)?;
    let kind: String = typed(&mut fields, "kind")?;
    if kind != "Run" {
        return Err("native event action kind must be the confirmed Run spelling".into());
    }
    let mut name = object(take(&mut fields, "name")?, "action name")?;
    let name_value = ActionName {
        category: typed(&mut name, "category")?,
        identifier: typed(&mut name, "identifier")?,
    };
    complete(name, "action name")?;
    let failed: bool = typed(&mut fields, "failed")?;
    let error = raw_optional_oneof(take(&mut fields, "error")?, "action error")?;
    // The normal producer explicitly sets failed = error.is_some().
    if failed == error.is_null() {
        return Err("native event failed/error fields disagree".into());
    }
    let execution_kind = typed(&mut fields, "execution_kind")?;
    let commands: Vec<Value> = typed(&mut fields, "commands")?;
    let commands = commands
        .into_iter()
        .map(command)
        .collect::<Result<Vec<_>>>()?;
    Ok(ActionExecutionEnd {
        event,
        key,
        name: name_value,
        kind,
        failed,
        error,
        execution_kind,
        commands,
        extra: fields,
        span_fields,
    })
}

/// Return only a local recipe explicitly recorded in the last command attempt.
/// Earlier attempts never fill a missing terminal recipe. This accessor does
/// not interpret status, exit code, failed/error or execution_kind; even a
/// recorded command from a failed action may be inspected. The environment is
/// still an overlay, and no execution, successful result or authority is granted.
pub fn recorded_terminal_local_command(action: &ActionExecutionEnd) -> Result<&LocalCommand> {
    let attempt = action
        .commands
        .last()
        .ok_or("native action has no recorded command attempts")?;
    let details = attempt
        .details
        .as_ref()
        .ok_or("native terminal command details are absent")?;
    let kind = details
        .command_kind
        .as_ref()
        .ok_or("native terminal command kind is absent")?;
    match kind.command.as_ref() {
        Some(Command::LocalCommand(local)) => {
            if local.argv.first().is_none_or(String::is_empty) {
                return Err("native terminal local command has no executable argument".into());
            }
            Ok(local)
        }
        Some(Command::Uninterpreted { .. }) => {
            Err("native terminal command does not contain an interpreted local recipe".into())
        }
        None => Err("native terminal command is absent".into()),
    }
}

/// Parse only the confirmed requested-materialization start. The logical
/// artifact path and action key do not establish a resolved output or success.
pub fn parse_materialization_start(bytes: &[u8]) -> Result<MaterializationStart> {
    let (event, mut span) = event(bytes, "SpanStart")?;
    let mut fields = object(
        tagged(take(&mut span, "data")?, "FinalMaterialization")?,
        "FinalMaterialization",
    )?;
    complete(span, "SpanStart")?;
    let artifact = build_artifact(take(&mut fields, "artifact")?)?;
    complete(fields, "FinalMaterialization")?;
    Ok(MaterializationStart { event, artifact })
}

/// Parse the requested-materialization end tag confirmed by the retained
/// ordinary stream. There is no success flag in this record. Matching its key
/// and logical path to a start does not resolve a physical output or prove the
/// enclosing operation succeeded.
pub fn parse_materialization_end(bytes: &[u8]) -> Result<MaterializationEnd> {
    let (event, mut span_fields) = event(bytes, "SpanEnd")?;
    let mut fields = object(
        tagged(take(&mut span_fields, "data")?, "FinalMaterialization")?,
        "FinalMaterialization",
    )?;
    let artifact = build_artifact(take(&mut fields, "artifact")?)?;
    complete(fields, "FinalMaterialization")?;
    Ok(MaterializationEnd {
        event,
        artifact,
        span_fields,
    })
}
