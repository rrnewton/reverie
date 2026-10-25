// Copyright (c) Meta Platforms, Inc. and affiliates.
// SPDX-License-Identifier: BSD-2-Clause

//! Untrusted final responses from the ordinary build-result stream.
//!
//! The confirmed wire path is `Result.result.build_response`: the CLI oneof
//! has snake_case serde names, unlike data-crate event enums. Fields follow
//! the CLI proto and serializer definitions at source revision
//! `13f223c4320d5a78da497e7dde6c25ab2a728eec`. These source-derived readers do
//! not authenticate a command, trace, successful codegen, artifact or recipe.
//! Target/configuration strings are retained exactly. No projection from a
//! build-report target or provider name into a response target is assumed.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde_json::Value;

use super::Result;
use super::native_results;
use super::native_wire;
use super::require;

pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

// Ordinary serialization emits these Option fields, including explicit null.
// Require their presence so a missing observation does not silently become None.
fn explicit_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputProviders {
    pub default_info: bool,
    pub run_info: bool,
    pub other: bool,
    pub test_info: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildOutput {
    pub path: String,
    #[serde(deserialize_with = "explicit_option")]
    pub providers: Option<OutputProviders>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildTarget {
    pub target: String,
    pub run_args: Vec<String>,
    pub outputs: Vec<BuildOutput>,
    pub configuration: String,
    #[serde(deserialize_with = "explicit_option")]
    pub configured_graph_size: Option<u64>,
    #[serde(deserialize_with = "explicit_option")]
    pub target_rule_type_name: Option<String>,
}

/// Plain response data. In particular, an empty errors list is not authority.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildResponse {
    pub build_targets: Vec<BuildTarget>,
    pub project_root: String,
    /// JSON text inside a string, never an embedded object on the wire.
    #[serde(deserialize_with = "explicit_option")]
    pub serialized_build_report: Option<String>,
    pub errors: Vec<Value>,
}

#[derive(Deserialize)]
enum StreamResult {
    Result(CommandResult),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandResult {
    result: ResultValue,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResultValue {
    BuildResponse(BuildResponse),
    Error(Value),
}

/// Parse one complete final stream record, rejecting duplicate keys everywhere.
/// Invocation headers, events, null results and command errors are not responses.
pub fn parse_build_response(bytes: &[u8]) -> Result<BuildResponse> {
    let value = native_wire::parse(bytes, MAX_RESPONSE_BYTES)?;
    let StreamResult::Result(result) =
        serde_json::from_value(value).map_err(|e| format!("native final build response: {e}"))?;
    match result.result {
        ResultValue::BuildResponse(response) => Ok(response),
        ResultValue::Error(error) => Err(format!("native final command error: {error}")),
    }
}

/// Parse a present embedded report as its distinct report form. A missing
/// embedded report stays absent; no successful default report is synthesized.
pub fn embedded_build_report(
    response: &BuildResponse,
) -> Result<Option<native_results::BuildReport>> {
    response
        .serialized_build_report
        .as_ref()
        .map(|report| {
            native_wire::parse(report.as_bytes(), native_results::MAX_REPORT_BYTES)?;
            native_results::parse_build_report(report.as_bytes())
        })
        .transpose()
}

/// Independently expected full response identity. Provider booleans are not
/// build-report provider names or a declaration of Rust codegen product kind.
pub struct ExpectedTarget {
    pub target: String,
    pub configuration: String,
    pub outputs: Vec<BuildOutput>,
}

pub struct ResponseExpectation {
    pub project_root: PathBuf,
    pub targets: Vec<ExpectedTarget>,
}

/// Selected lexical paths only, not resolved files or materialization evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedOutput {
    pub target: String,
    pub configuration: String,
    pub path: String,
    pub providers: OutputProviders,
    pub absolute_path: PathBuf,
}

fn named(value: &str) -> Result<()> {
    require(
        !value.trim().is_empty() && !value.contains('\0'),
        "native response identity is empty or contains NUL",
    )
}

fn normalized_path(value: &str, absolute: bool) -> Result<()> {
    if absolute && value == "/" {
        return Ok(());
    }
    let relative = if absolute {
        value
            .strip_prefix('/')
            .ok_or("native response root is not absolute")?
    } else {
        value
    };
    require(
        !relative.contains('\0')
            && relative
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "native response path is not normalized",
    )
}

fn output_map(outputs: &[BuildOutput]) -> Result<BTreeMap<&str, &Option<OutputProviders>>> {
    let mut paths = BTreeMap::new();
    for output in outputs {
        normalized_path(&output.path, false)?;
        require(
            paths
                .insert(output.path.as_str(), &output.providers)
                .is_none(),
            "duplicate native response output path",
        )?;
    }
    Ok(paths)
}

/// Join raw response fields to independent exact expectations. This does not
/// establish trace association (the response has no trace_id), action ownership,
/// provider-name projection, codegen, recipe/source joins or materialized bytes.
/// The configured-target key set and every target's output/provider set must
/// match completely. All response errors refuse the join.
pub fn join_build_response(
    response: &BuildResponse,
    expected: &ResponseExpectation,
) -> Result<Vec<SelectedOutput>> {
    require(
        response.errors.is_empty(),
        "native final build response contains errors",
    )?;
    normalized_path(&response.project_root, true)?;
    require(
        expected.project_root.to_str() == Some(response.project_root.as_str()),
        "native final response project root differs from independent expectation",
    )?;
    if let Some(report) = embedded_build_report(response)? {
        report.require_complete_success()?;
        require(
            report.project_root == response.project_root,
            "embedded report project root differs from final response",
        )?;
    }
    require(
        !expected.targets.is_empty(),
        "no expected native response targets",
    )?;
    let mut targets = BTreeMap::new();
    for target in &response.build_targets {
        named(&target.target)?;
        named(&target.configuration)?;
        output_map(&target.outputs)?;
        require(
            targets
                .insert(
                    (target.target.as_str(), target.configuration.as_str()),
                    target,
                )
                .is_none(),
            "duplicate configured target in native final response",
        )?;
    }
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    for target in &expected.targets {
        named(&target.target)?;
        named(&target.configuration)?;
        let key = (target.target.as_str(), target.configuration.as_str());
        require(
            seen.insert(key),
            "duplicate expected native response target",
        )?;
        let actual = targets
            .get(&key)
            .ok_or("expected native response target/configuration absent")?;
        require(
            !target.outputs.is_empty(),
            "expected native response target has no outputs",
        )?;
        require(
            output_map(&actual.outputs)? == output_map(&target.outputs)?,
            "native response complete output/provider set differs from expectation",
        )?;
        for output in &target.outputs {
            let providers = output
                .providers
                .clone()
                .ok_or("native selected output lacks providers")?;
            selected.push(SelectedOutput {
                target: target.target.clone(),
                configuration: target.configuration.clone(),
                path: output.path.clone(),
                providers,
                absolute_path: expected.project_root.join(&output.path),
            });
        }
    }
    require(
        targets.keys().eq(seen.iter()),
        "native response complete target/configuration set differs from expectation",
    )?;
    Ok(selected)
}
